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

pub use schema::{DATA_V2_ROOTS_FOLDERS, MIGRATIONS, SCHEMA_VERSION, TaskKind, TaskStatus};
pub use types::{
    DimsStats, ExclusionRow, FolderRow, FolderTreeRow, PhotoInput, PhotoRow, RootRow, RootSeed,
    ScanApply, TaskRow, ThumbBuf, ThumbState, ThumbStats, UPSERT_UNCHANGED, UpgradeReport,
    V2DataReport,
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
        let mut report = report;
        report.to = db.schema_version()?;
        db.upgrade = Some(report.clone());
        Ok((db, report))
    }

    /// 纯内存库（测试用）。
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn,
            path: None,
            upgrade: None,
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn tune(conn: &Connection) -> Result<()> {
        // WAL：读写不互斥，扫描时 UI 仍可查。
        // foreign_keys：ON DELETE CASCADE 才能生效（thumb 跟着 photo 走）。
        // synchronous=NORMAL：WAL 下崩溃最多丢最后一个事务，而索引本来可重建（02 §5.6）。
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA temp_store = MEMORY;
             PRAGMA cache_size = -16384;",
        )?;
        Ok(())
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
        if current >= SCHEMA_VERSION {
            return Ok(());
        }
        upgrade::apply_pending(&self.conn, current)?;
        Ok(())
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
                    // 没给就回到「待回填」（下一轮回填会再试）
                    self.conn.execute(
                        "UPDATE photos SET width = ?2, height = ?3, orientation = ?4,
                                           taken_at = ?5, taken_at_src = ?6,
                                           dims_status = CASE
                                               WHEN ?2 IS NOT NULL AND ?3 IS NOT NULL THEN 1
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
        let mut sql = String::from(
            "SELECT p.id, p.folder_id, f.path, p.name, p.size_bytes, p.mtime_ns,
                    p.width, p.height, p.orientation, p.taken_at, p.taken_at_src,
                    p.thumb_state, p.decoder_ver
             FROM photos p JOIN folders f ON f.id = p.folder_id",
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
        sql.push_str(sort.order_by());
        sql.push_str(&format!(" LIMIT {limit}"));

        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], map_photo)?;
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
        let mut sql = String::from(
            "SELECT p.id, p.folder_id, f.path, p.name, p.size_bytes, p.mtime_ns,
                    p.width, p.height, p.orientation, p.taken_at, p.taken_at_src,
                    p.thumb_state, p.decoder_ver
             FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE (p.name_key LIKE ?1 ESCAPE '\\'
                    OR f.path_key LIKE ?1 ESCAPE '\\')",
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
        Ok(self
            .conn
            .query_row(
                "SELECT p.id, p.folder_id, f.path, p.name, p.size_bytes, p.mtime_ns,
                        p.width, p.height, p.orientation, p.taken_at, p.taken_at_src,
                        p.thumb_state, p.decoder_ver
                 FROM photos p JOIN folders f ON f.id = p.folder_id WHERE p.id = ?1",
                params![id],
                map_photo,
            )
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
        let mut st = self.conn.prepare(
            "SELECT p.id, p.folder_id, f.path, p.name, p.size_bytes, p.mtime_ns,
                    p.width, p.height, p.orientation, p.taken_at, p.taken_at_src,
                    p.thumb_state, p.decoder_ver
             FROM photos p JOIN folders f ON f.id = p.folder_id
             ORDER BY p.mtime_ns DESC LIMIT ?1",
        )?;
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
        let mut sql = String::from(
            "SELECT p.id, p.folder_id, f.path, p.name, p.size_bytes, p.mtime_ns,
                    p.width, p.height, p.orientation, p.taken_at, p.taken_at_src,
                    p.thumb_state, p.decoder_ver
             FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE p.thumb_state IN (2, 3)",
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

    /// 待生成的缩略图（`None` 表示全库）。给扫描后的补图队列用。
    pub fn photos_pending_thumbs(
        &self,
        folder_ids: Option<&[i64]>,
        limit: usize,
    ) -> Result<Vec<PhotoRow>> {
        let mut sql = String::from(
            "SELECT p.id, p.folder_id, f.path, p.name, p.size_bytes, p.mtime_ns,
                    p.width, p.height, p.orientation, p.taken_at, p.taken_at_src,
                    p.thumb_state, p.decoder_ver
             FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE p.thumb_state = 0",
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
    fn order_by(self) -> &'static str {
        // taken_at 可能为 NULL（EXIF 缺失且未回退），统一用 COALESCE 保证排序稳定。
        // 中文文件名用 name_key（已做小写 + NFC）比较，避免大小写与归一化导致的乱序。
        match (self.key, self.descending) {
            (SortKey::TakenAt, true) => {
                "COALESCE(p.taken_at, p.mtime_ns/1000000000) DESC, p.name_key ASC"
            }
            (SortKey::TakenAt, false) => {
                "COALESCE(p.taken_at, p.mtime_ns/1000000000) ASC, p.name_key ASC"
            }
            (SortKey::Name, true) => "p.name_key DESC",
            (SortKey::Name, false) => "p.name_key ASC",
            (SortKey::ModifiedAt, true) => "p.mtime_ns DESC",
            (SortKey::ModifiedAt, false) => "p.mtime_ns ASC",
            (SortKey::Size, true) => "p.size_bytes DESC",
            (SortKey::Size, false) => "p.size_bytes ASC",
            (SortKey::Dimensions, true) => "COALESCE(p.width,0)*COALESCE(p.height,0) DESC",
            (SortKey::Dimensions, false) => "COALESCE(p.width,0)*COALESCE(p.height,0) ASC",
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
