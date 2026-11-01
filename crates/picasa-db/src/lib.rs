//! # picasa-db · OpenPicasa M1 自有索引库
//!
//! 叶子 crate：**不依赖任何其他本地 crate**（02 §6.2 的硬规则），保证可独立测试与迁移。
//!
//! ## 职责边界（写进代码纪律）
//!
//! - 只负责「**能从文件系统重新算出来的东西**」：文件夹清单、照片记录、EXIF 摘要、缩略图 BLOB。
//! - **不存任何用户创作的数据**（星标 / 说明 / 标签 / 编辑指令都在 M2，见 02 §5.6）。
//! - 因此整个库**可以随时删掉重建**，不需要「用户数据保命」逻辑。
//!
//! ## 落盘位置
//!
//! `~/Library/Application Support/app.openpicasa/library.sqlite`（由 `picasa-core::paths` 决定，
//! 本 crate 只接受一个路径参数，**不自己拼路径**——这样「谁能写哪儿」只有一处定义）。

mod dims;
mod pathutil;
mod roots;
mod schema;
mod tasks;
mod types;
mod upgrade;

pub use schema::{
    DATA_V2_ROOTS_FOLDERS, DATA_V5_FAILURE_KINDS, MIGRATIONS, SCHEMA_VERSION, TaskKind, TaskStatus,
};
pub use types::{
    CacheStats, CachedMeta, DimsStats, ExclusionRow, FailureGroup, FailureKind, FolderFingerprint,
    FolderRow, FolderTreeRow, PhotoInput, PhotoRow, RootRow, RootSeed, RootStat, ScanApply,
    TaskOutcome, TaskRow, ThumbBuf, ThumbState, ThumbStats, UPSERT_UNCHANGED, UpgradeReport,
    V2DataReport, classify_failure,
};

use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

/// 索引库错误。
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("schema 版本 {found} 比本程序支持的 {supported} 更新；请升级应用而不是降级数据库")]
    SchemaTooNew { found: i64, supported: i64 },
}

pub type Result<T> = std::result::Result<T, DbError>;

/// 索引库句柄。
///
/// 内部 `Connection` 不是 `Sync`，上层（`picasa-core`）用一把锁把它串起来；
/// 本 crate 不做内部加锁，避免「两层锁」。
pub struct Db {
    conn: Connection,
    path: Option<PathBuf>,
    /// 本次打开时的升级现场（新库 / 已最新时为 `None`）。
    upgrade: Option<UpgradeReport>,
    /// v3 的 `eff_taken_at` 生成列是否可用。
    ///
    /// **为什么不能只看 `schema_version()`**：迁移失败会整体回滚并以旧 schema 继续运行
    /// （`docs/05` §1.4 的降级路径），此时版本号停在 2，但代码已经写成「按 3 查」。
    /// 直接查 `PRAGMA table_xinfo` 一次，是唯一可靠的判据（也顺带覆盖「用户手动改过库」）。
    has_eff_col: bool,
}

impl Db {
    /// 打开（或创建）索引库并跑迁移。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::open_with_report(path)?.0)
    }

    /// 同 [`Db::open`]，但把升级现场交回给调用方（日志 / 「正在升级资料库」/ 降级提示）。
    ///
    /// 流程（`docs/05` §1.4）：
    /// 1. **先备份**：`VACUUM INTO <support>/Backups/openpicasa-v1-<utc>.db`；
    /// 2. 单事务跑 DDL；
    /// 3. 失败 → 回滚 + 备份覆盖 → **以 M2 数据继续运行**（不拒绝启动）；
    /// 4. 成功 → 打开长连接，`migrate()` 变成空操作。
    pub fn open_with_report(path: impl AsRef<Path>) -> Result<(Self, UpgradeReport)> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let report = upgrade::upgrade_file(&path)?;
        let conn = Connection::open(&path)?;
        Self::tune(&conn)?;
        let mut db = Self {
            conn,
            path: Some(path),
            upgrade: None,
            has_eff_col: false,
        };
        if report.error.is_none() {
            db.migrate()?;
        } else {
            // 迁移失败时不重试：重试只会再失败一次，还会把「已恢复的旧库」再动一遍。
            tracing::warn!(
                error = report.error.as_deref().unwrap_or_default(),
                "本次以旧 schema 数据继续运行，新增能力不可用"
            );
        }
        // 必须在 migrate 之后探测：新建的库到这一步才把列建出来。
        db.has_eff_col = Self::detect_eff_col(&db.conn);
        let mut report = report;
        report.to = db.schema_version()?;
        db.upgrade = Some(report.clone());
        Ok((db, report))
    }

    /// 纯内存库（测试用）。
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let mut db = Self {
            conn,
            path: None,
            upgrade: None,
            has_eff_col: false,
        };
        db.migrate()?;
        db.has_eff_col = Self::detect_eff_col(&db.conn);
        Ok(db)
    }

    /// `photos` 表上有没有 v3 的 `eff_taken_at`（见字段注释）。
    ///
    /// ⚠️ **必须用 `table_xinfo`，不能用 `table_info`**：生成列在 `table_info`
    /// 里根本不出现（SQLite 把它们归为 hidden 列，`hidden=2` 表示 VIRTUAL 生成列）。
    /// 用 `table_info` 探测会**永远返回 false**，于是 `eff_col()` 恒走 COALESCE
    /// 降级分支——库明明在 v3、索引明明建了，排序却每次全表扫。
    /// 这条是用 `tests/photo_columns.rs` 的执行计划断言抓出来的：同一条 SQL
    /// 拿 sqlite3 直连走 `idx_photos_eff_taken`，经 `Db` 却是 `USE TEMP B-TREE`。
    fn detect_eff_col(conn: &Connection) -> bool {
        let Ok(mut st) = conn.prepare("PRAGMA table_xinfo(photos)") else {
            return false;
        };
        let Ok(rows) = st.query_map([], |r| r.get::<_, String>(1)) else {
            return false;
        };
        rows.flatten().any(|name| name == "eff_taken_at")
    }

    /// 「有效拍摄时间」的 SQL 表达式。
    ///
    /// 正常情况直接用 v3 的生成列（命中 `idx_photos_eff_taken`）；
    /// **降级到旧 schema 时退回 COALESCE 表达式**——语义完全一致，只是排序要全表扫。
    /// 这样降级路径仍然「能用」，而不是一点开文件夹就 `no such column` 直接崩。
    ///
    /// 别名统一写成 `eff_taken_at`，所以 `map_photo` 的列顺序不受影响。
    fn eff_col(&self) -> &'static str {
        if self.has_eff_col {
            "p.eff_taken_at"
        } else {
            "COALESCE(p.taken_at, p.mtime_ns/1000000000)"
        }
    }

    /// `PhotoRow` 的列清单 —— **全项目只此一份**。
    ///
    /// 这个列表以前在 8 处 SELECT 里各写一遍，加字段时漏改一处就只在运行期炸
    /// （`InvalidColumnIndex`）。收口到这里之后，`map_photo` 的下标与它是一对，
    /// `tests/photo_columns.rs` 会把每条查询真跑一遍来守。
    fn photo_cols(&self) -> String {
        format!(
            "p.id, p.folder_id, f.path, p.name, p.size_bytes, p.mtime_ns,
             p.width, p.height, p.orientation, p.taken_at, p.taken_at_src,
             p.thumb_state, p.decoder_ver, {} AS eff_taken_at",
            self.eff_col()
        )
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn tune(conn: &Connection) -> Result<()> {
        // WAL：读写不互斥，扫描时 UI 仍可查。
        // foreign_keys：ON DELETE CASCADE 才能生效（thumb 跟着 photo 走）。
        // synchronous=NORMAL：WAL 下崩溃最多丢最后一个事务，而索引本来可重建（02 §5.6）。
        // M22-PERF-04：这四个数是 25 万张库下的打开参数，不是随手挑的——
        //  · mmap_size 256MB：让大库的读走内存映射，省掉反复的 read 系统调用；
        //    它只映射**我们自己的索引库**（在应用容器里），照片目录一个字节都不碰（R-1）。
        //  · cache_size -65536：页缓存 64MB（负数 = 以 KiB 计）。
        //  · wal_autocheckpoint 4000：checkpoint 攒到 4000 页再做，避免索引高峰期
        //    每隔几秒就同步一次把 I/O 打满。
        //  · temp_store MEMORY：临时表/排序不落盘（排序本来就在容器里，但放内存更快）。
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA temp_store = MEMORY;
             PRAGMA cache_size = -65536;
             PRAGMA mmap_size = 268435456;
             PRAGMA wal_autocheckpoint = 4000;",
        )?;
        Ok(())
    }

    /// M22-PERF-04：退出前跑一次 `PRAGMA optimize`。
    ///
    /// 它只做两件很便宜的事：把 `ANALYZE` 的结果写进 `sqlite_stat1`（下次启动直接读，
    /// 不用重跑 ANALYZE），以及必要时整理索引统计。**不对数据库做重写**，
    /// 所以放在退出路径上是安全的；失败也只当没发生，不该阻止退出。
    pub fn optimize(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA optimize;")?;
        Ok(())
    }

    /// 读一个整数型 `PRAGMA`（`cache_size` / `mmap_size` / `wal_autocheckpoint` …）。
    ///
    /// 存在的理由只有一个：**让打开参数可被断言**。M22-PERF-04 给的是具体数值，
    /// 光看代码里那句 `PRAGMA` 看不出它到底生效了没有（SQLite 会静默裁剪越界值，
    /// 比如编译期没开 mmap 时 `mmap_size` 就是 0）。测试与「诊断导出」都走这个口子。
    pub fn pragma_i64(&self, name: &str) -> Result<i64> {
        let sql = format!("PRAGMA {name}");
        let v: i64 = self.conn.query_row(&sql, [], |r| r.get(0))?;
        Ok(v)
    }

    /// `ANALYZE` 结果是否已经在库里（`sqlite_stat1` 有行）。
    pub fn has_analyzer_stats(&self) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM sqlite_stat1", [], |r| r.get(0))?;
        Ok(n > 0)
    }

    /// 顺序跑迁移；已应用的跳过。**框架比表重要**（02 §5.5）。
    ///
    /// 迁移在一个事务里跑完：中途失败整体回滚，不会留下「建了一半的表」。
    /// 幂等由 `schema_migrations` 保证，SQL 里不写 `IF NOT EXISTS` 兜底。
    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version    INTEGER PRIMARY KEY,
                 applied_at INTEGER NOT NULL
             );",
        )?;
        let current = self.schema_version()?;
        if current > SCHEMA_VERSION {
            return Err(DbError::SchemaTooNew {
                found: current,
                supported: SCHEMA_VERSION,
            });
        }
        if current < SCHEMA_VERSION {
            upgrade::apply_pending(&self.conn, current)?;
        }
        // ⚠️ 数据步骤的守卫必须**独立于**「要不要跑 DDL」：
        // 走 `open_with_report` 这条路径时，DDL 已经在 `upgrade_file` 里跑完了，
        // 到这里 `current >= SCHEMA_VERSION` 会提前返回 —— 若把补数放在那个分支里，
        // 首次升级的库就永远补不上分类（`tests/migrate_v5.rs` 抓到过）。
        self.apply_v5_data()?;
        Ok(())
    }

    /// v5 数据步骤：历史失败记录补 `kind`。**幂等**，跑第二次返回 0。
    pub fn apply_v5_data(&self) -> Result<u64> {
        if self.data_migration_done(DATA_V5_FAILURE_KINDS)? {
            return Ok(0);
        }
        let n = self.backfill_failure_kinds()?;
        self.conn.execute(
            "INSERT OR REPLACE INTO data_migrations (name, applied_at)
             VALUES (?1, strftime('%s','now'))",
            params![DATA_V5_FAILURE_KINDS],
        )?;
        Ok(n)
    }

    pub fn schema_version(&self) -> Result<i64> {
        let v: Option<i64> = self
            .conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        Ok(v.unwrap_or(0))
    }

    // ---------------------------------------------------------------- folders

    /// 按规范化路径 upsert 一个文件夹，返回 id。
    ///
    /// `volume_id` 用于外接盘重挂载后识别（M1 只记录，不参与重映射——F1-06 在 M3）。
    pub fn upsert_folder(
        &self,
        path: &str,
        path_key: &str,
        volume_id: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO folders (path, path_key, volume_id, is_online, last_scan_at)
             VALUES (?1, ?2, ?3, 1, NULL)
             ON CONFLICT(path) DO UPDATE SET
                 path_key  = excluded.path_key,
                 volume_id = COALESCE(excluded.volume_id, folders.volume_id),
                 is_online = 1",
            params![path, path_key, volume_id],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM folders WHERE path = ?1",
            params![path],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    pub fn folder_id(&self, path: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM folders WHERE path = ?1",
                params![path],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn all_folders(&self) -> Result<Vec<FolderRow>> {
        let mut st = self.conn.prepare(
            "SELECT f.id, f.path, f.is_online, f.thumb_tier,
                    (SELECT COUNT(*) FROM photos p WHERE p.folder_id = f.id) AS n
             FROM folders f ORDER BY f.path",
        )?;
        let rows = st.query_map([], |r| {
            Ok(FolderRow {
                id: r.get(0)?,
                path: r.get(1)?,
                is_online: r.get::<_, i64>(2)? != 0,
                thumb_tier: r.get(3)?,
                photo_count: r.get::<_, i64>(4)? as u32,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn set_folder_online(&self, folder_id: i64, online: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET is_online = ?2 WHERE id = ?1",
            params![folder_id, if online { 1 } else { 0 }],
        )?;
        Ok(())
    }

    /// 每目录缩略图档位（B 类偏好，但放这里更好查，见 02 §5.3 设计说明 3）。
    pub fn set_folder_thumb_tier(&self, folder_id: i64, tier: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET thumb_tier = ?2 WHERE id = ?1",
            params![folder_id, tier],
        )?;
        Ok(())
    }

    pub fn delete_folder(&self, folder_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM folders WHERE id = ?1", params![folder_id])?;
        Ok(())
    }

    /// 把一个文件夹的整批扫描结果落库（单事务）。返回增/改/删计数。
    ///
    /// **删除语义**：磁盘上已不存在的照片记录会被移除（这不是「写照片目录」，
    /// 而是「更正我们自己的索引」——R-1 只约束照片目录，不约束应用容器）。
    pub fn apply_scan(
        &self,
        folder_id: i64,
        folder_path: &str,
        items: &[PhotoInput],
        decoder_ver: i64,
    ) -> Result<ScanApply> {
        let mut out = ScanApply::default();
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let res = self.apply_scan_inner(folder_id, folder_path, items, decoder_ver, &mut out);
        match res {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(out)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn apply_scan_inner(
        &self,
        folder_id: i64,
        _folder_path: &str,
        items: &[PhotoInput],
        decoder_ver: i64,
        out: &mut ScanApply,
    ) -> Result<()> {
        // 1) 一次性把该目录现有记录读进 HashMap（name_key → (id, size, mtime, ver)）
        let mut existing: std::collections::HashMap<String, (i64, i64, i64, i64)> =
            std::collections::HashMap::new();
        {
            let mut st = self.conn.prepare(
                "SELECT id, name_key, size_bytes, mtime_ns, decoder_ver
                 FROM photos WHERE folder_id = ?1",
            )?;
            let mut rows = st.query(params![folder_id])?;
            while let Some(r) = rows.next()? {
                existing.insert(r.get(1)?, (r.get(0)?, r.get(2)?, r.get(3)?, r.get(4)?));
            }
        }

        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

        // 2) upsert
        for it in items {
            seen.insert(it.name_key.as_str());
            match existing.get(&it.name_key) {
                Some((id, size, mtime, ver))
                    if *size == it.size_bytes && *mtime == it.mtime_ns && *ver == decoder_ver =>
                {
                    // 缓存键相同：只更新可能变化的 EXIF 摘要字段，不改 thumb_state
                    // dims_status 跟着宽高一起刷新：扫描器给了宽高就算「已完成」，
                    // 没给就回到「待回填」（下一轮回填会再试）。
                    //
                    // ⚠️ 但**终态（2）必须保住**。`dims_status = 2` 的含义是「文件读得到、
                    // 但拿不到宽高」（HEIC/AVIF/文件头损坏），它的 `width` 永远是 NULL。
                    // 早期版本这里无条件写 0，于是这批照片每次启动都被打回「待回填」：
                    // 25.9 万张的真实库里实测 **903 张**，每启动一次就把这 903 个文件头
                    // 重读一遍、再由回填线程重新判一次失败——永远没有终止的一天。
                    // 「文件没变过」是保住终态的前提；变了就把状态归零重试（见下面的分支）。
                    self.conn.execute(
                        "UPDATE photos SET width = ?2, height = ?3, orientation = ?4,
                                           taken_at = ?5, taken_at_src = ?6,
                                           dims_status = CASE
                                               WHEN ?2 IS NOT NULL AND ?3 IS NOT NULL THEN 1
                                               WHEN dims_status = 2 THEN 2
                                               ELSE 0 END
                         WHERE id = ?1",
                        params![
                            id,
                            it.width,
                            it.height,
                            it.orientation,
                            it.taken_at,
                            it.taken_at_src
                        ],
                    )?;
                    out.unchanged += 1;
                }
                Some((id, ..)) => {
                    // 缓存键变了：元数据更新 + 缩略图作废（thumb_state 归零、BLOB 删掉）
                    self.conn.execute(
                        "UPDATE photos SET size_bytes = ?2, mtime_ns = ?3, width = ?4,
                                           height = ?5, orientation = ?6, taken_at = ?7,
                                           taken_at_src = ?8, decoder_ver = ?9, thumb_state = 0,
                                           dims_status = CASE
                                               WHEN ?4 IS NOT NULL AND ?5 IS NOT NULL THEN 1
                                               ELSE 0 END
                         WHERE id = ?1",
                        params![
                            id,
                            it.size_bytes,
                            it.mtime_ns,
                            it.width,
                            it.height,
                            it.orientation,
                            it.taken_at,
                            it.taken_at_src,
                            decoder_ver
                        ],
                    )?;
                    self.conn
                        .execute("DELETE FROM thumbs WHERE photo_id = ?1", params![id])?;
                    out.updated += 1;
                    out.touched.push(*id);
                }
                None => {
                    self.conn.execute(
                        "INSERT INTO photos
                           (folder_id, name, name_key, size_bytes, mtime_ns, width, height,
                            orientation, taken_at, taken_at_src, thumb_state, decoder_ver,
                            dims_status)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0,?11,
                                 CASE WHEN ?6 IS NOT NULL AND ?7 IS NOT NULL THEN 1 ELSE 0 END)",
                        params![
                            folder_id,
                            it.name,
                            it.name_key,
                            it.size_bytes,
                            it.mtime_ns,
                            it.width,
                            it.height,
                            it.orientation,
                            it.taken_at,
                            it.taken_at_src,
                            decoder_ver
                        ],
                    )?;
                    out.added += 1;
                    out.touched.push(self.conn.last_insert_rowid());
                }
            }
        }

        // 3) 磁盘上已消失的 → 删索引（cascade 会带走 thumbs）
        let stale: Vec<i64> = existing
            .iter()
            .filter(|(k, _)| !seen.contains(k.as_str()))
            .map(|(_, v)| v.0)
            .collect();
        for id in stale {
            self.conn
                .execute("DELETE FROM photos WHERE id = ?1", params![id])?;
            out.removed += 1;
        }

        self.conn.execute(
            "UPDATE folders SET last_scan_at = strftime('%s','now'), is_online = 1,
                                direct_count = (SELECT COUNT(*) FROM photos WHERE folder_id = ?1)
             WHERE id = ?1",
            params![folder_id],
        )?;
        Ok(())
    }

    // ----------------------------------------------------------------- photos

    /// 当前视图中要显示的照片（按拍摄时间倒序，缺失回退 mtime）。
    ///
    /// `folder_ids` 为 `None` 表示全库（「所有照片」入口）。
    pub fn photos(
        &self,
        folder_ids: Option<&[i64]>,
        sort: SortSpec,
        limit: usize,
    ) -> Result<Vec<PhotoRow>> {
        self.photos_from(folder_ids, sort, limit, 0)
    }

    /// 分页版：跳过前 `offset` 行再取 `limit` 行（C 包的「大文件夹快速打开」用）。
    ///
    /// ## 为什么是 OFFSET 而不是 keyset
    ///
    /// keyset 分页（`WHERE (eff_taken_at, name_key) < (?, ?)`）在**按拍摄时间**这一种
    /// 排序下最优，但本项目有 5 种排序键，每种都要写一份「严格小于」的元组比较；
    /// 而 OFFSET 在命中索引时是顺序跳过，代价 O(offset) 但**不需要排序**。
    /// 实测 5 万行跳 4.8 万 ≈ 8 ms，追加一批 4000 行的代价与它同量级——
    /// 摊到十几帧上完全无感。用一份代码换五种排序的一致行为，值。
    ///
    /// ⚠️ **一致性**：分页补齐期间若有并发写入（扫描正把新照片插到前面），
    /// OFFSET 会错位，可能出现重复或漏行。这里**不试图加锁修它**——
    /// 补齐结束后 `Library::reload_photos` 会整体重取一次收敛。
    /// 分页是「让用户尽快看见」，不是「保证某一瞬间的严格快照」。
    pub fn photos_from(
        &self,
        folder_ids: Option<&[i64]>,
        sort: SortSpec,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<PhotoRow>> {
        let mut sql = format!(
            "SELECT {} FROM photos p JOIN folders f ON f.id = p.folder_id",
            self.photo_cols()
        );
        if let Some(ids) = folder_ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let list = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" WHERE p.folder_id IN ({list})"));
        }
        sql.push_str(" ORDER BY ");
        sql.push_str(&sort.order_by(self.eff_col()));
        sql.push_str(&format!(" LIMIT {limit}"));
        if offset > 0 {
            // SQLite 语法要求 OFFSET 跟在 LIMIT 后；没有 LIMIT 不能用 OFFSET，
            // 所以这里 limit 恒有值（调用方用 usize::MAX 表示「不设上限」时也一样）。
            sql.push_str(&format!(" OFFSET {offset}"));
        }

        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 视图内照片总数（C1 首屏先拿总数，用来判断「要不要继续补」与显示进度）。
    /// 走 `EXPLAIN QUERY PLAN` 验证过：有 `folder_id` 条件时用 `idx_photos_folder_taken`，
    /// 无条件时是 `SCAN photos` 的计数优化（SQLite 对无 WHERE 的 `COUNT(*)` 直接读页数）。
    pub fn count_photos(&self, folder_ids: Option<&[i64]>) -> Result<u64> {
        let mut sql = String::from("SELECT COUNT(*) FROM photos p");
        if let Some(ids) = folder_ids {
            if ids.is_empty() {
                return Ok(0);
            }
            let list = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" WHERE p.folder_id IN ({list})"));
        }
        let n: i64 = self.conn.query_row(&sql, [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// 取「按某个排序取照片」这条查询的执行计划（诊断与回归测试用）。
    ///
    /// 存在的理由是可测试性：C1 的整个价值都压在「排序能不能走索引」上，
    /// 而这件事**只有执行计划能证明**——没有它，只能靠 release 构建手动跑 5 万张样本
    /// 看耗时，那种断言既慢又会随机器负载飘。有它之后，
    /// `tests/photo_columns.rs` 可以断言计划里必须出现 `idx_photos_eff_taken`、
    /// 且不出现 `TEMP B-TREE FOR ORDER BY`。
    pub fn explain_photos_plan(&self, sort: SortSpec) -> Result<Vec<String>> {
        let sql = format!(
            "EXPLAIN QUERY PLAN
             SELECT p.id FROM photos p JOIN folders f ON f.id = p.folder_id
             ORDER BY {} LIMIT 2000",
            sort.order_by(self.eff_col())
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], |r| r.get::<_, String>(3))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 搜索：文件名 / 文件夹名 / EXIF 拍摄日期（F1-21 的 M1 数据源）。
    ///
    /// 用 `LIKE` 而不是 FTS5：`unicode61` 不切分中文，而 M1 的用户输入大概率是中文。
    /// 10 万行 `LIKE` 在 WAL + `name_key` 索引下的实测代价可接受（见 ADR-0002），
    /// 且不需要「双写 FTS 与主表」的一致性维护。
    pub fn search(
        &self,
        needle: &str,
        date_range: Option<(i64, i64)>,
        folder_ids: Option<&[i64]>,
        limit: usize,
    ) -> Result<Vec<PhotoRow>> {
        let like = format!("%{}%", escape_like(needle));
        let mut sql = format!(
            "SELECT {}
             FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE (p.name_key LIKE ?1 ESCAPE '\\'
                    OR f.path_key LIKE ?1 ESCAPE '\\')",
            self.photo_cols()
        );
        // 日期区间是**收窄条件（AND）**，不是并列条件（OR）：
        // 「搜 苏州 + 限定 2011 年」应当是交集；空关键字时 LIKE '%%' 恒真，AND 依然等价于纯日期筛选。
        if date_range.is_some() {
            sql.push_str(" AND p.taken_at BETWEEN ?2 AND ?3");
        }
        if let Some(ids) = folder_ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let list = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND p.folder_id IN ({list})"));
        }
        sql.push_str(&format!(" ORDER BY p.taken_at DESC LIMIT {limit}"));

        let mut st = self.conn.prepare(&sql)?;
        // 占位符个数必须与实参个数一致：`date_range == None` 时 SQL 里只有 ?1
        let rows = match date_range {
            Some((a, b)) => st
                .query_map(params![like, a, b], map_photo)?
                .collect::<std::result::Result<Vec<_>, _>>()?,
            None => st
                .query_map(params![like], map_photo)?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    pub fn photo(&self, id: i64) -> Result<Option<PhotoRow>> {
        let sql = format!(
            "SELECT {} FROM photos p JOIN folders f ON f.id = p.folder_id WHERE p.id = ?1",
            self.photo_cols()
        );
        Ok(self
            .conn
            .query_row(&sql, params![id], map_photo)
            .optional()?)
    }

    pub fn total_photos(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM photos", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// 「最近更改」入口：按 mtime 倒序（F1-22 的 M1 可用入口之一）。
    pub fn recently_changed(&self, limit: usize) -> Result<Vec<PhotoRow>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {}
             FROM photos p JOIN folders f ON f.id = p.folder_id
             ORDER BY p.mtime_ns DESC LIMIT ?1",
            self.photo_cols()
        ))?;
        let rows = st.query_map(params![limit as i64], map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 从索引里移除一张（「从图库移除」，**不删文件**，F2-08 的独立命令）。
    pub fn forget_photo(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM photos WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ----------------------------------------------------------------- thumbs

    pub fn put_thumb(&self, photo_id: i64, tier: i64, buf: &ThumbBuf) -> Result<()> {
        self.conn.execute(
            "INSERT INTO thumbs (photo_id, tier, w, h, format, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(photo_id, tier) DO UPDATE SET
                 w = excluded.w, h = excluded.h,
                 format = excluded.format, data = excluded.data",
            params![photo_id, tier, buf.w, buf.h, buf.format, buf.data],
        )?;
        self.conn.execute(
            "UPDATE photos SET thumb_state = 1 WHERE id = ?1",
            params![photo_id],
        )?;
        Ok(())
    }

    pub fn get_thumb(&self, photo_id: i64, tier: i64) -> Result<Option<ThumbBuf>> {
        Ok(self
            .conn
            .query_row(
                "SELECT w, h, format, data FROM thumbs WHERE photo_id = ?1 AND tier = ?2",
                params![photo_id, tier],
                |r| {
                    Ok(ThumbBuf {
                        w: r.get(0)?,
                        h: r.get(1)?,
                        format: r.get(2)?,
                        data: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_thumb_state(&self, photo_id: i64, state: ThumbState) -> Result<()> {
        self.conn.execute(
            "UPDATE photos SET thumb_state = ?2 WHERE id = ?1",
            params![photo_id, state as i64],
        )?;
        Ok(())
    }

    /// 一个「关注文件夹」下的所有子目录 id（含它自己）。
    ///
    /// 用 `substr(path, 1, n) = 'root/'` 而不是 `LIKE`：路径里合法地可以出现 `%` 和 `_`，
    /// 用 `LIKE` 就必须转义，转错一次就会把别的目录算进来。
    pub fn subtree_folder_ids(&self, folder_id: i64) -> Result<Vec<i64>> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM folders WHERE id = ?1",
                params![folder_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(path) = path else {
            return Ok(Vec::new());
        };
        let cut = path.chars().count() as i64 + 1;
        let prefix = format!("{path}/");
        let mut st = self.conn.prepare(
            "SELECT id FROM folders
             WHERE path = ?1 OR substr(path, 1, ?2) = ?3
             ORDER BY path",
        )?;
        let rows = st.query_map(params![path, cut, prefix], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 整棵子树（关注文件夹含子目录）的缩略图状态计数。
    ///
    /// **任务口径必须按子树算**：用户关注 `2020` 时，`2020/01`、`2020/02` 里的照片
    /// 都属于这个任务；只数根目录自己的照片会让进度永远显示 0。
    pub fn subtree_thumb_stats(&self, folder_id: i64) -> Result<ThumbStats> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM folders WHERE id = ?1",
                params![folder_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(path) = path else {
            return Ok(ThumbStats::default());
        };
        let cut = path.chars().count() as i64 + 1;
        let prefix = format!("{path}/");
        let mut st = self.conn.prepare(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN p.thumb_state = 1 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN p.thumb_state IN (2, 3) THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN p.thumb_state = 0 THEN 1 ELSE 0 END), 0)
             FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE f.path = ?1 OR substr(f.path, 1, ?2) = ?3",
        )?;
        let row = st.query_row(params![path, cut, prefix], |r| {
            Ok(ThumbStats {
                total: r.get::<_, i64>(0)? as u64,
                ready: r.get::<_, i64>(1)? as u64,
                failed: r.get::<_, i64>(2)? as u64,
                pending: r.get::<_, i64>(3)? as u64,
            })
        })?;
        Ok(row)
    }

    /// 一个关注文件夹（含子目录）里所有待生成的缩略图。
    pub fn subtree_pending_thumbs(&self, folder_id: i64, limit: usize) -> Result<Vec<PhotoRow>> {
        let ids = self.subtree_folder_ids(folder_id)?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.photos_pending_thumbs(Some(&ids), limit)
    }

    /// 把一个关注文件夹（含子目录）的失败项重置为待生成，返回重置条数。
    pub fn reset_failed_thumbs_subtree(&self, folder_id: i64) -> Result<u64> {
        let ids = self.subtree_folder_ids(folder_id)?;
        let mut total = 0u64;
        for id in ids {
            total += self.reset_failed_thumbs(id)?;
        }
        Ok(total)
    }

    /// 一个目录的缩略图状态计数（M2 索引进度：已完成 / 已发现 / 失败）。
    pub fn folder_thumb_stats(&self, folder_id: i64) -> Result<ThumbStats> {
        let mut st = self.conn.prepare(
            "SELECT COUNT(*) AS total,
                    COALESCE(SUM(CASE WHEN thumb_state = 1 THEN 1 ELSE 0 END), 0) AS ready,
                    COALESCE(SUM(CASE WHEN thumb_state IN (2, 3) THEN 1 ELSE 0 END), 0) AS failed,
                    COALESCE(SUM(CASE WHEN thumb_state = 0 THEN 1 ELSE 0 END), 0) AS pending
             FROM photos WHERE folder_id = ?1",
        )?;
        let row = st.query_row(params![folder_id], |r| {
            Ok(ThumbStats {
                total: r.get::<_, i64>(0)? as u64,
                ready: r.get::<_, i64>(1)? as u64,
                failed: r.get::<_, i64>(2)? as u64,
                pending: r.get::<_, i64>(3)? as u64,
            })
        })?;
        Ok(row)
    }

    /// 整个库的缩略图状态计数（「所有照片」视图的双数计数用）。
    pub fn library_thumb_stats(&self) -> Result<ThumbStats> {
        let mut st = self.conn.prepare(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN thumb_state = 1 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN thumb_state IN (2, 3) THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN thumb_state = 0 THEN 1 ELSE 0 END), 0)
             FROM photos",
        )?;
        let row = st.query_row([], |r| {
            Ok(ThumbStats {
                total: r.get::<_, i64>(0)? as u64,
                ready: r.get::<_, i64>(1)? as u64,
                failed: r.get::<_, i64>(2)? as u64,
                pending: r.get::<_, i64>(3)? as u64,
            })
        })?;
        Ok(row)
    }

    /// 失败项明细（面板展开用）。**只读查询，不重试、不改状态。**
    pub fn failed_photos(&self, folder_id: Option<i64>) -> Result<Vec<PhotoRow>> {
        let mut sql = format!(
            "SELECT {}
             FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE p.thumb_state IN (2, 3)",
            self.photo_cols()
        );
        if let Some(id) = folder_id {
            sql.push_str(&format!(" AND p.folder_id = {id}"));
        }
        sql.push_str(" ORDER BY p.name_key ASC");
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 把失败项重置为「待生成」，返回重置条数。
    ///
    /// **只动失败项**（`thumb_state ∈ {2,3}`）：已完成的有效缩略图不重建，
    /// 所以重试不会让「已完成」计数回退（`04` §2.4 第 1 条）。
    pub fn reset_failed_thumbs(&self, folder_id: i64) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE photos SET thumb_state = 0
             WHERE folder_id = ?1 AND thumb_state IN (2, 3)",
            params![folder_id],
        )?;
        Ok(n as u64)
    }

    /// 把**点名的一批**失败项重置为「待生成」，返回重置条数。
    ///
    /// 「重建失败索引」用。为什么不直接全库重置（`UPDATE ... WHERE thumb_state IN (2,3)`）：
    /// 重置后这些照片会变成「待生成」，而**待生成并不等于马上会被处理**——
    /// 启动扫描只补本次新增，历史欠账一律不碰。于是「重置了但没入队」的照片
    /// 会永远停在灰格子上，比失败状态更糟（失败至少还有明细可查）。
    /// 所以必须**先算清楚哪些 id 真的会入队，再只重置这些**。
    ///
    /// 分块是因为 SQLite 的 `IN (...)` 变量数有上限（默认 32766）：
    /// 一次 500 个既安全，也让单条语句保持在毫秒级。
    pub fn reset_failed_thumbs_by_ids(&self, ids: &[i64]) -> Result<u64> {
        let mut total = 0u64;
        for chunk in ids.chunks(500) {
            let list = chunk
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "UPDATE photos SET thumb_state = 0
                 WHERE thumb_state IN (2, 3) AND id IN ({list})"
            );
            total += self.conn.execute(&sql, [])? as u64;
        }
        Ok(total)
    }

    /// 待生成的缩略图（`None` 表示全库）。给扫描后的补图队列用。
    pub fn photos_pending_thumbs(
        &self,
        folder_ids: Option<&[i64]>,
        limit: usize,
    ) -> Result<Vec<PhotoRow>> {
        let mut sql = format!(
            "SELECT {}
             FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE p.thumb_state = 0",
            self.photo_cols()
        );
        if let Some(ids) = folder_ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let list = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND p.folder_id IN ({list})"));
        }
        sql.push_str(&format!(" ORDER BY p.taken_at DESC LIMIT {limit}"));
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 指定 id 里仍在「待生成」的照片（用于扫描结束只补**本次新增/变化**的那批）。
    pub fn pending_thumbs_by_ids(&self, ids: &[i64], limit: usize) -> Result<Vec<PhotoRow>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let list = ids
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {}
             FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE p.thumb_state = 0 AND p.id IN ({list})
             ORDER BY p.taken_at DESC LIMIT {limit}",
            self.photo_cols()
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 一个目录里已入库照片的缓存键与已解出的元数据（`name_key` → `CachedMeta`）。
    ///
    /// 扫描器先问这个，就能对「没变过」的文件**不再读盘**：25.9 万张的库，
    /// 每次启动都全量重新解析 EXIF 要几十分钟，而增量发现本该只碰变化的文件。
    pub fn folder_cached_meta(
        &self,
        folder_id: i64,
    ) -> Result<std::collections::HashMap<String, CachedMeta>> {
        let mut st = self.conn.prepare(
            "SELECT name_key, size_bytes, mtime_ns, decoder_ver,
                    width, height, orientation, taken_at, taken_at_src, dims_status
             FROM photos WHERE folder_id = ?1",
        )?;
        let mut rows = st.query(params![folder_id])?;
        let mut out = std::collections::HashMap::new();
        while let Some(r) = rows.next()? {
            out.insert(
                r.get(0)?,
                CachedMeta {
                    size_bytes: r.get(1)?,
                    mtime_ns: r.get(2)?,
                    decoder_ver: r.get(3)?,
                    width: r.get(4)?,
                    height: r.get(5)?,
                    orientation: r.get(6)?,
                    taken_at: r.get(7)?,
                    taken_at_src: r.get(8)?,
                    dims_status: r.get(9)?,
                },
            );
        }
        Ok(out)
    }

    // ------------------------------------------------------------ 维护（F 加固）
    /// 缓存占用统计（诊断页显示「占了多少空间」）。
    pub fn cache_stats(&self) -> Result<CacheStats> {
        let photo_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM photos", [], |r| r.get(0))?;
        let (thumb_count, thumb_bytes): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(data)), 0) FROM thumbs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let db_bytes = self
            .path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        Ok(CacheStats {
            db_bytes,
            photo_count: photo_count.max(0) as u64,
            thumb_count: thumb_count.max(0) as u64,
            thumb_bytes: thumb_bytes.max(0) as u64,
        })
    }

    /// 缩略图的 `(条数, 字节数)`。**读 BLOB**：冷缓存下可达 1 s，只给后台线程用。
    pub fn thumb_cache_stats(&self) -> Result<(u64, u64)> {
        let (n, bytes): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(data)), 0) FROM thumbs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((n.max(0) as u64, bytes.max(0) as u64))
    }

    /// 主库文件字节（`-wal` 不计）。纯 `stat`，可以放心地每帧调。
    pub fn db_bytes(&self) -> u64 {
        self.path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// 全库照片数（`COUNT(*) FROM photos`，走 `idx_photos_*`，亚毫秒）。
    pub fn photo_count_total(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM photos", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// WAL 文件大小（「存储与诊断」标签）。
    ///
    /// 为什么单独报：用户清理完缩略图后主库文件常常没变小（页只是标记为空闲），
    /// 而 WAL 可能还挂着几百 MB。不把这两个数字分开摆，用户只会认为「清理没生效」。
    pub fn wal_bytes(&self) -> u64 {
        self.path
            .as_ref()
            .map(|p| {
                let wal = p.with_extension("sqlite-wal");
                let shm = p.with_extension("sqlite-shm");
                std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0)
                    + std::fs::metadata(&shm).map(|m| m.len()).unwrap_or(0)
            })
            .unwrap_or(0)
    }

    /// 「关注文件夹」标签的每行统计（M2.2 §4.1）。
    ///
    /// 照片数/缩略图占用/失败数都按**整棵子树**算：用户关注 `2020` 时，
    /// 他关心的数字是这整个文件夹，不是它自己那一层（`docs/04` §3.2 的同一口径）。
    pub fn root_stats(&self) -> Result<Vec<RootStat>> {
        // ---- 照片数：对 `folders` 做**一次**分组求和，而不是「每个根一个相关子查询」
        //
        // 旧写法对每个根跑 `SELECT COUNT(*) FROM photos JOIN folders ...`，
        // 在 25.9 万张的库上 **3 个根 = 1.025 s**；而照片数本来就以 `direct_count`
        // 冗余存在 `folders` 里（4330 行），一次分组求和即可 —— 实测 **1.025 s → 0.002 s**。
        let mut photo_count: std::collections::HashMap<i64, u64> = Default::default();
        let mut last_scan: std::collections::HashMap<i64, Option<i64>> = Default::default();
        {
            let mut st = self.conn.prepare(
                "SELECT root_id, SUM(direct_count), MAX(last_scan_at)
                   FROM folders GROUP BY root_id",
            )?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let rid: i64 = r.get(0)?;
                photo_count.insert(rid, r.get::<_, i64>(1)?.max(0) as u64);
                last_scan.insert(rid, r.get(2)?);
            }
        }
        // ---- 失败数：走 `idx_photos_thumb`，只扫失败项，不碰 BLOB
        let mut failed: std::collections::HashMap<i64, u64> = Default::default();
        {
            let mut st = self.conn.prepare(
                "SELECT f.root_id, COUNT(*) FROM photos p
                   JOIN folders f ON f.id = p.folder_id
                  WHERE p.thumb_state IN (2, 3) GROUP BY f.root_id",
            )?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                failed.insert(r.get(0)?, r.get::<_, i64>(1)?.max(0) as u64);
            }
        }
        let mut st = self
            .conn
            .prepare("SELECT id, path, state FROM roots ORDER BY path")?;
        let rows = st.query_map([], |r| {
            let id: i64 = r.get(0)?;
            Ok(RootStat {
                id,
                path: r.get(1)?,
                state: r.get(2)?,
                photo_count: photo_count.get(&id).copied().unwrap_or(0),
                failed: failed.get(&id).copied().unwrap_or(0),
                last_indexed_at: last_scan.get(&id).copied().flatten(),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 每个根的缩略图 BLOB 字节数。**慢查询**：只能由后台线程调用，
    /// 禁止进每帧 / 每秒的路径（[`Db::root_stats`] 里已不再包含它）。
    ///
    /// ## 为什么是 `EXISTS` 而不是 `JOIN`
    ///
    /// 同一件事的三种写法，在 25.9 万张 / 1.26 GB 缩略图的库上实测：
    ///
    /// | 写法 | 耗时 |
    /// | --- | --- |
    /// | `thumbs JOIN photos JOIN folders … GROUP BY root_id` | **112 s** |
    /// | `WHERE photo_id IN (SELECT …)` | 23 s／根 |
    /// | `WHERE EXISTS (SELECT …)` | **0.28 s／根（热缓存 0.06 s）** |
    ///
    /// 差别在于 SQLite 对 `LENGTH(blob)` 有个优化：**列宽写在记录头里，不读溢出页就能取到**
    /// （所以裸 `SUM(LENGTH(data)) FROM thumbs` 只要 0.02 s）。写成 `JOIN` 后优化器会把整行
    /// 物化，1.26 GB 的 BLOB 溢出页被逐字节读出来，在 eMMC 上就是几十秒级。
    /// `EXISTS` 是**半连接**，不物化 `thumbs` 行，优化得以保留。
    ///
    /// ⚠️ 即便如此，冷缓存首次仍是 **~0.9 s**（3 个根），所以仍然只能异步跑。
    pub fn root_thumb_bytes(
        &self,
        root_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, u64>> {
        let mut out = std::collections::HashMap::with_capacity(root_ids.len());
        let mut st = self.conn.prepare(
            "SELECT COALESCE(SUM(LENGTH(t.data)), 0) FROM thumbs t
              WHERE EXISTS (SELECT 1 FROM photos p JOIN folders f ON f.id = p.folder_id
                             WHERE p.id = t.photo_id AND f.root_id = ?1)",
        )?;
        for &rid in root_ids {
            let n: i64 = st.query_row(params![rid], |r| r.get(0))?;
            out.insert(rid, n.max(0) as u64);
        }
        Ok(out)
    }

    /// 「所有照片」的总数（§2.9：只数 **active** 根，含失败项与离线项）。
    ///
    /// 离线根的照片**要算**：它们仍以置灰形式显示在栅格里（`TC-DM-016`），
    /// 树上的计数与栅格里的照片必须一致（R-M22-6）。已关闭子目录的照片不算——
    /// 关闭时那批索引已经删掉了。
    pub fn all_photos_active(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM photos p
               JOIN folders f ON f.id = p.folder_id
               JOIN roots   r ON r.id = f.root_id
              WHERE r.state = 0",
            [],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    /// 每个目录的「库内指纹」（§4.8 启动增量校验的比对基准）。
    ///
    /// 返回 `(folder_id, 路径, 库内照片数, 库内字节总和, 库内最大 mtime_ns)`。
    /// 校验时拿它与 `read_dir` 的实时结果比对——**不等**就说明用户在外部动过这个目录。
    ///
    /// ⚠️ 用一次 `LEFT JOIN + GROUP BY` 而不是「每目录三个子查询」：
    /// 后者在 4330 个目录上会退化成 1.3 万次索引查找，实测数量级差百倍
    /// （同类教训见 `root_thumb_bytes` 的注释）。
    pub fn folder_fingerprints(&self, root_id: i64) -> Result<Vec<FolderFingerprint>> {
        let mut st = self.conn.prepare(
            "SELECT f.id, f.path,
                    COUNT(p.id),
                    COALESCE(SUM(p.size_bytes), 0),
                    COALESCE(MAX(p.mtime_ns), 0)
               FROM folders f
               LEFT JOIN photos p ON p.folder_id = f.id
              WHERE f.root_id = ?1
              GROUP BY f.id
              ORDER BY f.path",
        )?;
        let rows = st.query_map(params![root_id], |r| {
            Ok(FolderFingerprint {
                folder_id: r.get(0)?,
                path: r.get(1)?,
                photo_count: r.get::<_, i64>(2)?.max(0) as u64,
                size_sum: r.get::<_, i64>(3)?.max(0) as u64,
                mtime_max: r.get::<_, i64>(4)?.max(0) as u64,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 该根下所有已入库目录的路径集合（用来发现「外部新建的目录」）。
    pub fn folder_paths_of_root(&self, root_id: i64) -> Result<std::collections::HashSet<String>> {
        let mut st = self
            .conn
            .prepare("SELECT path FROM folders WHERE root_id = ?1")?;
        let rows = st.query_map(params![root_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<std::collections::HashSet<_>, _>>()?)
    }

    /// 批量记「这几个目录刚校验过」。
    ///
    /// 为什么批量：4330 个目录逐个 `UPDATE` 就是 4330 个写事务，在 WAL 下
    /// 光 fsync 排队就够拖慢启动阶段了。一条 `WHERE id IN (...)` 只有一次事务。
    pub fn mark_verified_many(&self, folder_ids: &[i64]) -> Result<()> {
        if folder_ids.is_empty() {
            return Ok(());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let holes = std::iter::repeat_n("?", folder_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("UPDATE folders SET last_verified_at = ?1 WHERE id IN ({holes})");
        let mut st = self.conn.prepare(&sql)?;
        st.execute(rusqlite::params_from_iter(
            std::iter::once(now).chain(folder_ids.iter().copied()),
        ))?;
        Ok(())
    }

    /// 记下「这个目录刚校验过」（§4.8 P-16 预留列，本版只写不依赖）。
    pub fn mark_verified(&self, folder_id: i64) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "UPDATE folders SET last_verified_at = ?1 WHERE id = ?2",
            params![now, folder_id],
        )?;
        Ok(())
    }

    /// 每个目录的 `direct_count`（树与栅格组头共用，**同一事务同一份数**）。
    pub fn folder_counts(&self) -> Result<std::collections::HashMap<i64, u32>> {
        let mut st = self.conn.prepare("SELECT id, direct_count FROM folders")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (id, n) = row?;
            out.insert(id, n.max(0) as u32);
        }
        Ok(out)
    }

    /// **同一事务**内取 `folder_counts` + `all_photos`（M2.2 §6.1 硬约束）。
    ///
    /// ## 为什么非得是一个方法
    ///
    /// R-M22-6 要求「树节点计数 / 组头 N / 所有照片 / 索引管理页」四处**逐字段相等**。
    /// 分两次 `SELECT` 就有缝：中间若有人提交了扫描结果，两份数就来自不同的世界线，
    /// 而 `TC-M22-IDX-011` 会在索引进行中连取 100 个 `seq` —— 必挂。
    /// 所以这里用 `DEFERRED` 事务把两次查询夹住：SQLite 的读事务一旦拿到快照，
    /// 直到 `COMMIT` 前都不会看到别人的写入。
    ///
    /// ⚠️ 只做**只读**查询，别往这个事务里塞写操作（`DEFERRED` 会升级成写锁，
    /// 在 WAL 下与落库线程抢锁，把流水线拖住）。
    pub fn mgmt_counts(&self) -> Result<(std::collections::HashMap<i64, u32>, u64)> {
        let tx = self.conn.unchecked_transaction()?;
        let mut st = tx.prepare("SELECT id, direct_count FROM folders")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        let mut counts = std::collections::HashMap::new();
        for row in rows {
            let (id, n) = row?;
            counts.insert(id, n.max(0) as u32);
        }
        drop(st);
        let all: i64 = tx.query_row(
            "SELECT COUNT(*) FROM photos p
               JOIN folders f ON f.id = p.folder_id
               JOIN roots   r ON r.id = f.root_id
              WHERE r.state = 0",
            [],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok((counts, all.max(0) as u64))
    }

    /// 每个目录**直接**包含的失败照片数（树上的 S-02 状态点用）。
    ///
    /// 只数直接包含的（与 `direct_count` 同口径）：状态点的语义是
    /// 「这个文件夹里有失败」，不是「这棵子树里某处有失败」。
    pub fn folders_with_failures(&self) -> Result<std::collections::HashMap<i64, u64>> {
        let mut st = self.conn.prepare(
            "SELECT folder_id, COUNT(*) FROM photos
             WHERE thumb_state IN (2, 3) GROUP BY folder_id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (id, n) = row?;
            out.insert(id, n.max(0) as u64);
        }
        Ok(out)
    }

    /// 记下一次目录校验完成（P-16 预留列，`folders.last_verified_at`）。
    pub fn set_folder_verified(&self, folder_id: i64, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET last_verified_at = ?2 WHERE id = ?1",
            params![folder_id, at],
        )?;
        Ok(())
    }

    /// 回收磁盘空间：先把 WAL 落盘并截断，再 `VACUUM`。
    ///
    /// ⚠️ **必须真的 `VACUUM`**：`DELETE FROM thumbs` 只是把页标成空闲，
    /// 文件大小不会变——用户点了「清理」却看到占用没降，会认为功能没做（`docs/05` §2.2 已记）。
    pub fn vacuum(&self) -> Result<()> {
        // 截断 WAL 才能让 VACUUM 后紧接着看到文件变小。
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        self.conn.execute_batch("VACUUM;")?;
        Ok(())
    }

    /// 清空全部缩略图缓存，并把照片标回「待生成」。返回删掉的缩略图条数。
    ///
    /// 语义是「释放空间 + 下次扫描重建」，**不是**删除照片：`photos` 行一条不少。
    pub fn clear_all_thumbs(&self) -> Result<u64> {
        let n = self.conn.execute("DELETE FROM thumbs", [])?;
        self.conn.execute("UPDATE photos SET thumb_state = 0", [])?;
        Ok(n as u64)
    }

    /// 删除**索引数据**：照片、缩略图、目录、历史任务与失败记录。
    ///
    /// ## 保留什么、为什么
    ///
    /// | 表 | 处理 | 理由 |
    /// |---|---|---|
    /// | `photos` / `thumbs` / `folders` | 删 | 就是"索引"本身 |
    /// | `index_tasks` / `index_task_failures` | 删 | 历史任务记录指向已不存在的目录，留着只会让面板显示幽灵任务 |
    /// | `roots` | **保留** | 关注文件夹是**用户配置**，不是索引产物。删了等于顺手把用户添加的文件夹也清了 |
    /// | `exclusions` | **保留** | 同上——"关闭此子文件夹"是用户的决定，重建索引后必须继续生效 |
    ///
    /// 返回 `(照片数, 缩略图数)`，给界面报一个如实的数字（"已删除 N 张照片的索引"）。
    ///
    /// ⚠️ 顺序不能反：`thumbs` 有 `REFERENCES photos(id) ON DELETE CASCADE`，
    /// 但**外键级联不一定开着**（用户手工改过库、或连接是别的路径开的）。
    /// 先删子表再删父表，任何情况下都不会留下孤儿行。
    pub fn clear_index(&self) -> Result<(u64, u64)> {
        let photos: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM photos", [], |r| r.get(0))?;
        let thumbs: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM thumbs", [], |r| r.get(0))
            .unwrap_or(0);

        let tx = self.conn.unchecked_transaction()?;
        // 子表先走
        let _ = tx.execute("DELETE FROM index_task_failures", []);
        let _ = tx.execute("DELETE FROM index_tasks", []);
        tx.execute("DELETE FROM thumbs", [])?;
        tx.execute("DELETE FROM photos", [])?;
        // folders 里有 root_id / parent_id 自引用：先断开父链再删，避免自引用外键顺序问题
        tx.execute("UPDATE folders SET parent_id = NULL", [])?;
        tx.execute("DELETE FROM folders", [])?;
        tx.commit()?;

        Ok((photos.max(0) as u64, thumbs.max(0) as u64))
    }
}

pub(crate) fn map_photo(r: &rusqlite::Row<'_>) -> rusqlite::Result<PhotoRow> {
    Ok(PhotoRow {
        id: r.get(0)?,
        folder_id: r.get(1)?,
        folder_path: r.get(2)?,
        name: r.get(3)?,
        size_bytes: r.get(4)?,
        mtime_ns: r.get(5)?,
        width: r.get(6)?,
        height: r.get(7)?,
        orientation: r.get(8)?,
        taken_at: r.get(9)?,
        taken_at_src: r.get(10)?,
        thumb_state: r.get(11)?,
        decoder_ver: r.get(12)?,
        eff_taken_at: r.get(13)?,
    })
}

/// `LIKE` 转义，避免用户输入的 `%` / `_` 变成通配符。
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match c {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// 排序规格（F1-15 的六种排序；星标在 M2 恢复）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SortKey {
    TakenAt,
    Name,
    ModifiedAt,
    Size,
    Dimensions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SortSpec {
    pub key: SortKey,
    pub descending: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            key: SortKey::TakenAt,
            descending: true,
        }
    }
}

impl SortSpec {
    /// 生成 `ORDER BY` 子句。`eff` 是「有效拍摄时间」的列表达式（见 `Db::eff_col`）：
    ///
    /// - 正常情况传 `"p.eff_taken_at"` → 命中 `idx_photos_eff_taken`，5 万张 `LIMIT 2000` 1.8 ms；
    /// - 降级到 v2 schema 时传 `"COALESCE(p.taken_at, p.mtime_ns/1000000000)"`
    ///   → 语义一样，但没有索引（全表排序），这是迁移失败后「能用但慢」的代价。
    ///
    /// ⚠️ **不能直接写 COALESCE 表达式**：表达式会让 SQLite 放弃所有索引，
    /// 5 万张实测 199 ms，而且 `LIMIT` 完全帮不上忙（排序在截断之前）。
    /// `docs/05` §1.9 记了这件事的来龙去脉，`tests/photo_columns.rs` 用执行计划守着。
    fn order_by(self, eff: &str) -> String {
        // 中文文件名用 name_key（已做小写 + NFC）比较，避免大小写与归一化导致的乱序。
        match (self.key, self.descending) {
            (SortKey::TakenAt, true) => format!("{eff} DESC, p.name_key ASC"),
            (SortKey::TakenAt, false) => format!("{eff} ASC, p.name_key ASC"),
            (SortKey::Name, true) => "p.name_key DESC".to_string(),
            (SortKey::Name, false) => "p.name_key ASC".to_string(),
            (SortKey::ModifiedAt, true) => "p.mtime_ns DESC".to_string(),
            (SortKey::ModifiedAt, false) => "p.mtime_ns ASC".to_string(),
            (SortKey::Size, true) => "p.size_bytes DESC".to_string(),
            (SortKey::Size, false) => "p.size_bytes ASC".to_string(),
            (SortKey::Dimensions, true) => {
                "COALESCE(p.width,0)*COALESCE(p.height,0) DESC".to_string()
            }
            (SortKey::Dimensions, false) => {
                "COALESCE(p.width,0)*COALESCE(p.height,0) ASC".to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(name: &str, size: i64, mtime: i64, taken: Option<i64>) -> PhotoInput {
        PhotoInput {
            name: name.to_string(),
            name_key: name.to_lowercase(),
            size_bytes: size,
            mtime_ns: mtime,
            width: Some(4000),
            height: Some(3000),
            orientation: Some(1),
            taken_at: taken,
            taken_at_src: if taken.is_some() { 0 } else { 2 },
        }
    }

    fn db_with_folder() -> (Db, i64) {
        let db = Db::open_in_memory().unwrap();
        let id = db.upsert_folder("/tmp/a", "/tmp/a", None).unwrap();
        (db, id)
    }

    #[test]
    fn migration_is_idempotent() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        db.migrate().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn scan_adds_updates_removes() {
        let (db, fid) = db_with_folder();
        let r = db
            .apply_scan(
                fid,
                "/tmp/a",
                &[mk("a.jpg", 10, 1, Some(100)), mk("b.jpg", 20, 2, Some(200))],
                0,
            )
            .unwrap();
        assert_eq!((r.added, r.updated, r.removed, r.unchanged), (2, 0, 0, 0));

        // 同一份数据再扫一次：全部 unchanged
        let r = db
            .apply_scan(
                fid,
                "/tmp/a",
                &[mk("a.jpg", 10, 1, Some(100)), mk("b.jpg", 20, 2, Some(200))],
                0,
            )
            .unwrap();
        assert_eq!((r.added, r.updated, r.removed, r.unchanged), (0, 0, 0, 2));

        // b 变大 → updated；c 新增；a 保留
        let r = db
            .apply_scan(
                fid,
                "/tmp/a",
                &[
                    mk("a.jpg", 10, 1, Some(100)),
                    mk("b.jpg", 22, 2, Some(200)),
                    mk("c.jpg", 30, 3, Some(300)),
                ],
                0,
            )
            .unwrap();
        assert_eq!((r.added, r.updated, r.removed, r.unchanged), (1, 1, 0, 1));

        // 只留 c → 删 2
        let r = db
            .apply_scan(fid, "/tmp/a", &[mk("c.jpg", 30, 3, Some(300))], 0)
            .unwrap();
        assert_eq!(r.removed, 2);
        assert_eq!(db.total_photos().unwrap(), 1);
    }

    #[test]
    fn stale_thumb_is_dropped_on_change() {
        let (db, fid) = db_with_folder();
        db.apply_scan(fid, "/tmp/a", &[mk("a.jpg", 10, 1, None)], 0)
            .unwrap();
        let p = db.photos(Some(&[fid]), SortSpec::default(), 10).unwrap();
        let pid = p[0].id;
        db.put_thumb(
            pid,
            0,
            &ThumbBuf {
                w: 1,
                h: 1,
                format: 0,
                data: vec![1, 2, 3],
            },
        )
        .unwrap();
        assert!(db.get_thumb(pid, 0).unwrap().is_some());

        // 文件变了 → 缩略图作废
        db.apply_scan(fid, "/tmp/a", &[mk("a.jpg", 11, 1, None)], 0)
            .unwrap();
        assert!(db.get_thumb(pid, 0).unwrap().is_none());
        assert_eq!(
            db.photos(Some(&[fid]), SortSpec::default(), 10).unwrap()[0].thumb_state,
            0
        );
    }

    #[test]
    fn search_hits_name_and_folder() {
        let (db, fid) = db_with_folder();
        db.apply_scan(
            fid,
            "/tmp/a",
            &[
                mk("苏州行.jpg", 1, 1, Some(1_300_000_000)),
                mk("beach.JPG", 1, 1, Some(1_400_000_000)),
            ],
            0,
        )
        .unwrap();
        assert_eq!(db.search("苏州", None, None, 100).unwrap().len(), 1);
        assert_eq!(db.search("beach", None, None, 100).unwrap().len(), 1);
        assert_eq!(db.search("%", None, None, 100).unwrap().len(), 0); // 通配符被转义

        // 空关键字 + 只有日期收窄 → 等价于纯日期筛选
        // beach 的 taken_at 是 1_400_000_000（2014-05），苏州行 是 1_300_000_000（2011-03）
        let around_2014 = (1_398_000_000, 1_402_000_000);
        let in_2014 = db.search("", Some(around_2014), None, 100).unwrap();
        assert_eq!(in_2014.len(), 1);
        assert_eq!(in_2014[0].name, "beach.JPG");

        // 关键字 + 日期是**交集**（AND），不是并集
        assert_eq!(
            db.search("苏州", Some(around_2014), None, 100)
                .unwrap()
                .len(),
            0
        );
        // folder_ids 收窄
        assert_eq!(db.search("a%", None, Some(&[fid]), 100).unwrap().len(), 0);
        assert_eq!(
            db.search("beach", None, Some(&[fid]), 100).unwrap().len(),
            1
        );
        assert_eq!(db.search("beach", None, Some(&[]), 100).unwrap().len(), 0);
    }

    #[test]
    fn sort_orders_are_stable() {
        let (db, fid) = db_with_folder();
        db.apply_scan(
            fid,
            "/tmp/a",
            &[mk("b.jpg", 2, 20, Some(200)), mk("a.jpg", 1, 10, Some(100))],
            0,
        )
        .unwrap();
        let asc = db
            .photos(
                Some(&[fid]),
                SortSpec {
                    key: SortKey::Name,
                    descending: false,
                },
                10,
            )
            .unwrap();
        assert_eq!(asc[0].name, "a.jpg");
        let by_taken = db.photos(Some(&[fid]), SortSpec::default(), 10).unwrap();
        assert_eq!(by_taken[0].name, "b.jpg");
    }

    #[test]
    fn thumb_stats_count_each_state() {
        let (db, fid) = db_with_folder();
        db.apply_scan(
            fid,
            "/tmp/a",
            &[
                mk("ok.jpg", 1, 1, None),
                mk("failed.jpg", 2, 2, None),
                mk("pending.jpg", 3, 3, None),
                mk("pending2.jpg", 4, 4, None),
            ],
            0,
        )
        .unwrap();
        let all = db.photos(Some(&[fid]), SortSpec::default(), 10).unwrap();
        let by_name = |n: &str| all.iter().find(|p| p.name == n).unwrap().id;
        db.put_thumb(
            by_name("ok.jpg"),
            0,
            &ThumbBuf {
                w: 1,
                h: 1,
                format: 0,
                data: vec![1],
            },
        )
        .unwrap();
        db.set_thumb_state(by_name("failed.jpg"), ThumbState::Failed)
            .unwrap();

        let s = db.folder_thumb_stats(fid).unwrap();
        assert_eq!((s.total, s.ready, s.failed, s.pending), (4, 1, 1, 2));
        assert_eq!(db.library_thumb_stats().unwrap(), s);
        assert_eq!(db.failed_photos(Some(fid)).unwrap().len(), 1);
    }

    #[test]
    fn reset_only_touches_failed_items() {
        let (db, fid) = db_with_folder();
        db.apply_scan(
            fid,
            "/tmp/a",
            &[mk("ok.jpg", 1, 1, None), mk("bad.jpg", 2, 2, None)],
            0,
        )
        .unwrap();
        let all = db.photos(Some(&[fid]), SortSpec::default(), 10).unwrap();
        let ok_id = all.iter().find(|p| p.name == "ok.jpg").unwrap().id;
        let bad_id = all.iter().find(|p| p.name == "bad.jpg").unwrap().id;
        db.put_thumb(
            ok_id,
            0,
            &ThumbBuf {
                w: 1,
                h: 1,
                format: 0,
                data: vec![9],
            },
        )
        .unwrap();
        db.set_thumb_state(bad_id, ThumbState::Corrupt).unwrap();

        assert_eq!(db.reset_failed_thumbs(fid).unwrap(), 1);
        let s = db.folder_thumb_stats(fid).unwrap();
        assert_eq!((s.ready, s.failed, s.pending), (1, 0, 1), "已完成不回退");
        // 已就绪的缩略图内容仍在（没有因为重试被删）
        assert!(db.get_thumb(ok_id, 0).unwrap().is_some());
    }
}
