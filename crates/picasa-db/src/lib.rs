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
mod export;
mod identity;
mod import_sources;
mod m4;
mod pathutil;
mod roots;
mod schema;
mod task_intent;
mod tasks;
mod types;
mod upgrade;

pub use schema::{
    DATA_V2_ROOTS_FOLDERS, DATA_V5_FAILURE_KINDS, DATA_V6_FORMATS_L1, MIGRATIONS, SCHEMA_VERSION,
    TaskKind, TaskStatus,
};
pub use types::{
    aggregate_totals,
    CacheStats, CachedMeta, CountSnapshot, DimsStats, ExclusionRow, FailureGroup, FailureKind,
    PhotoFilter, SizeClass,
    FolderFingerprint, FolderRow, FolderTreeRow, PhotoFormat, PhotoInput, PhotoRow, RootRow,
    RootSeed, RootStat, RootWatchRow, ScanApply, TakenFrom, TaskOutcome, TaskRow, ThumbBuf, ThumbState,
    ThumbStats, UPSERT_UNCHANGED, UpgradeReport, V2DataReport, classify_failure, PageCursor,
};
pub use import_sources::{
    ImportBatchRow, ImportSourceRow, import_batch_photo_ids, prune_orphan_import_sources,
    record_import, remove_import_source,
};
pub use task_intent::{TaskIntent, TaskScopeKind};
pub use m4::{
    AlbumRow, ContentFlags, FolderTree, FolderTreeNode, FolderTreeRoot, ImportReport,
    ImportStrategy, IndexManifest, ManifestCounts, ManifestRoot, PhotoExif, RecentSearch,
    TreeSource,
};
pub use export::{
    FORMAT_VERSION, ExportReport, ImportPreview, check_export_dir, cleanup_export_tmp, cleanup_staging,
    execute_import, export_index, inspect_index, inspect_index_cancellable,
    rewrite_manifest_for_test,
};

use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
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
    /// 业务校验失败（重名、空名、不兼容的索引文件等），文案可直接展示给用户。
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, DbError>;

/// 当前 Unix 秒。
///
/// `clippy.toml` 把 `SystemTime` 列为禁用类型（跨平台序列化语义不稳），
/// 但**这一处是唯一合理的用法**：只是取一个「现在几点」的数字写进
/// `last_verified_at`，不比较、不序列化、不跨平台传。集中到这一个函数里，
/// 别处就不必再破例。
#[expect(clippy::disallowed_types)]
fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

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
    /// photos 是否已有 starred 列（v7+）。降级库 / 回退库为 false，查询走常量 0。
    has_starred_col: bool,
    /// v9 身份骨架表是否可用（标记守恒，见 `identity.rs`）。
    ///
    /// 探测 `photo_identities` 表而不是看 `schema_version()`：迁移失败回滚后版本号
    /// 可能停在 8 但代码按 9 写，直接探测表才是可靠判据（与 `has_eff_col` 同理）。
    has_identity: bool,
}

/// `photos_filtered` 与 `count_photos_filtered` 共用的 WHERE 形态。
enum FilterWhere {
    /// 没有筛选条件，走无筛选查询。
    Inactive,
    /// 条件恒假（例如格式 slug 一个都认不出）。
    Empty,
    /// 含前导空格的 ` WHERE ...`。
    Sql(String),
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
            has_starred_col: false,
            has_identity: false,
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
        db.has_starred_col = Self::detect_starred_col(&db.conn);
        db.has_identity = db.table_exists("photo_identities")?;
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
            has_starred_col: false,
            has_identity: false,
        };
        db.migrate()?;
        db.has_eff_col = Self::detect_eff_col(&db.conn);
        db.has_starred_col = Self::detect_starred_col(&db.conn);
        db.has_identity = db.table_exists("photo_identities")?;
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
    fn detect_starred_col(conn: &Connection) -> bool {
        let Ok(mut st) = conn.prepare("PRAGMA table_xinfo(photos)") else {
            return false;
        };
        let Ok(rows) = st.query_map([], |r| r.get::<_, String>(1)) else {
            return false;
        };
        rows.flatten().any(|name| name == "starred")
    }

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
             p.thumb_state, p.decoder_ver, {} AS eff_taken_at, {}",
            self.eff_col(),
            // v7 之前的库（迁移失败回退场景）没有 starred 列：给常量 0，
            // 让降级路径「能用」而不是一点开就 no such column 崩掉。
            if self.has_starred_col { "p.starred" } else { "0 AS starred" }
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
        self.apply_v6_data()?;
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

    /// v6 数据步骤（`07` §8.2 + `07-A` §6「迁移补数」）：**幂等**，跑第二次返回 0。
    ///
    /// 做三件事，都是「必须现在做，否则升级后一段时间内数据是错的」：
    ///
    /// 1. **`photos.format` 按扩展名预填**。不预填的话，升级后所有老照片都是
    ///    `Unknown`，而 M4 的类型筛选会显示「未知 259000 张」——用户会以为数据丢了。
    ///    嗅探（魔数）留给 L2：`format` 是「实际格式」，预填只是按扩展名**猜**，
    ///    真正解码时若魔数与扩展名不符，那时会改写它（FORMAT-01）。
    /// 2. **`photos.taken_from` 按 EXIF 源回填**。`taken_at_src` 早就在列里
    ///    （0=EXIF / 1=文件名 / 2=mtime），但语义和新列不完全一样：
    ///    `taken_at_src` 是「拍摄时间取自哪」，`taken_from` 是「日期可信吗」。
    ///    这里按 `taken_at_src` 映射，**不重新读盘**。
    /// 3. **`folders.total_count` 一次递归聚合**。用递归 CTE 自底向上累加，
    ///    4330 个目录在毫秒级；之后由落库线程维护（`07` §4.1）。
    ///
    /// ⚠️ `folders.dir_mtime` **故意不填**（`07` §8.2 第一条）：空 = 「还没量过」，
    /// 于是升级后的首次启动仍走 M2.2 的完整 L2，把 `dir_mtime` 建立起来，
    /// 之后才是 L1 快筛。这比「升级时顺手 stat 全树」好：升级不该卡住启动。
    pub fn apply_v6_data(&self) -> Result<u64> {
        if self.data_migration_done(DATA_V6_FORMATS_L1)? {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let n = self.backfill_v6_data_inner(&tx)?;
        tx.execute(
            "INSERT OR REPLACE INTO data_migrations (name, applied_at)
             VALUES (?1, strftime('%s','now'))",
            params![DATA_V6_FORMATS_L1],
        )?;
        tx.commit()?;
        Ok(n)
    }

    fn backfill_v6_data_inner(&self, tx: &rusqlite::Transaction<'_>) -> Result<u64> {
        let mut touched = 0u64;

        // ---- 1. format：按扩展名预填 -------------------------------------
        // 只动 `format = 0` 的行：跑过一次之后再跑（或用户升级到更新版本）不会覆盖
        // 已经由嗅探确定的值。
        //
        // SQL 里做「取最后一个点之后的部分转小写」：
        // `lower(substr(name, length(rtrim(name, replace(name,'.','')||name)) + 2))`
        // 太绕且遇到含点目录名会错，所以这里读一次 name 在 Rust 侧算——
        // 26 万行读一遍约 0.2 s，比一条谁都看不懂的 SQL 值。
        {
            let mut st = tx.prepare("SELECT id, name FROM photos WHERE format = 0")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            let mut upd = tx.prepare_cached("UPDATE photos SET format = ?2 WHERE id = ?1")?;
            for row in rows {
                let (id, name) = row?;
                let ext = std::path::Path::new(&name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let Some(fmt) = crate::PhotoFormat::from_extension(ext) else {
                    continue;
                };
                upd.execute(params![id, fmt.code()])?;
                touched += 1;
            }
        }

        // ---- 2. taken_from：按 taken_at_src 映射 -------------------------
        // 0 → EXIF，其余（1 文件名 / 2 mtime）→ Mtime。**不碰已是 1/2 的行**。
        self.conn.execute(
            "UPDATE photos SET taken_from =
                 CASE WHEN taken_at_src = 0 THEN 1 ELSE 2 END
             WHERE taken_from = 0",
            [],
        )?;

        // ---- 3. total_count：递归聚合 -----------------------------------
        // `07` §4.1 的口径：含失败项、含离线项；**不含**已关闭子目录子树。
        // 关闭是 `roots.state = 1`（根级），子目录没有「关闭」概念（那是排除项，
        // 排除项根本不会进 folders 表），所以这里只需按 parent 关系累加。
        //
        // 递归 CTE 的用法：以每个叶子（没有子目录的目录）为起点向上累加，
        // 用 `UNION ALL` 而不是 `UNION`（同一个祖先会从多条路径被累加多次，
        // 那正是我们要的）。`direct_count` 已经是「本层张数」。
        self.conn.execute(
            "WITH RECURSIVE up(folder, amount) AS (
                 SELECT id, direct_count FROM folders
                  UNION ALL
                 SELECT f.parent_id, up.amount
                   FROM folders f
                   JOIN up ON f.id = up.folder
                  WHERE f.parent_id IS NOT NULL
             )
             UPDATE folders SET total_count = COALESCE(
                 (SELECT SUM(amount) FROM up WHERE up.folder = folders.id), 0
             )",
            [],
        )?;

        Ok(touched)
    }

    // ------------------------------------------------------------ 诊断辅助（测试用）

    /// 某张表的所有列名（`PRAGMA table_info`）。迁移测试用它断言「列真的加了」。
    pub fn column_names(&self, table: &str) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = st.query_map([], |r| r.get::<_, String>(1))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 库里的全部索引名。迁移测试用它断言「索引真的建了」——
    /// 光建列不建索引，键集分页会退化成全表扫描，而这一点**没有任何报错**。
    pub fn index_names(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name NOT LIKE 'sqlite_%'",
        )?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ---------------------------------------------------------------- settings

    /// 读一个设置项的**值**。`None` 的两个来源（行不存在 / 值是 SQL NULL）在这里
    /// 被合并——想让它们可区分时用 [`Db::setting_row_present`]。
    ///
    /// 为什么不给 `get(key) -> String` 加默认值：`grid_cell_px` 的 NULL 就是
    /// 「用默认刻度」这个语义，归一成某个具体数字后，「用户拖过滑块又拖回来」
    /// 与「从没拖过」就分不出来了。
    pub fn setting_raw(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// 这一行设置**存在吗**（值可能是 NULL）。
    ///
    /// 存在的意义：迁移会预置 `grid_cell_px = NULL`（`07-A` §6）。若只看
    /// [`Db::setting_raw`]，预置成功的库与「v6 迁移没跑」的库长得一模一样，
    /// 于是「设置页读不到默认刻度」这类缺陷会被误诊成读取方的问题。
    pub fn setting_row_present(&self, key: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM settings WHERE key = ?1",
                params![key],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
    }

    /// `settings.grid_cell_px`（`07-A` ZOOM-06）。`None` = 用默认刻度。
    pub fn grid_cell_px(&self) -> Result<Option<i64>> {
        self.setting_i64("grid_cell_px")
    }

    /// `settings.recurse_subfolders`（`07-A` §6 预置为 `"0"`）。
    pub fn recurse_subfolders(&self) -> Result<bool> {
        self.setting_bool("recurse_subfolders", false)
    }

    /// 写 `settings.recurse_subfolders`。
    pub fn set_recurse_subfolders(&self, on: bool) -> Result<()> {
        self.set_setting_raw("recurse_subfolders", Some(if on { "1" } else { "0" }))
    }

    /// 写 `settings.grid_cell_px`。
    pub fn set_grid_cell_px(&self, px: i64) -> Result<()> {
        self.set_setting_raw("grid_cell_px", Some(&px.to_string()))
    }

    /// 整数设置项。`None` = 用默认值（行不存在或值为 NULL 都算）。
    pub fn setting_i64(&self, key: &str) -> Result<Option<i64>> {
        Ok(self
            .setting_raw(key)?
            .and_then(|v| v.trim().parse::<i64>().ok()))
    }

    /// 布尔设置项（`"1"` / `"0"`）。认不出的值按 `fallback` 处理而不是报错：
    /// 设置项坏掉不该让应用起不来。
    pub fn setting_bool(&self, key: &str, fallback: bool) -> Result<bool> {
        Ok(match self.setting_raw(key)?.as_deref() {
            Some("1") => true,
            Some("0") => false,
            _ => fallback,
        })
    }

    /// 写一个设置项（`None` 写成 SQL NULL）。
    pub fn set_setting_raw(&self, key: &str, value: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ------------------------------------------ v6 · 递归计数 / L1 / 格式（只读侧）

    // ------------------------------------------------- v6/M3 · 键集分页与聚合

    /// 键集分页取行（`07` §5.2 SCALE-03）。
    ///
    /// ## 为什么需要 `cursor` 参数
    ///
    /// 纯 `OFFSET` 的代价是 O(offset)：100 万张滚到底，每补一页要跳 90 万行。
    /// 键集分页是 O(log n)：`WHERE (sort_key, id) < (?, ?)` 直接走索引定位。
    ///
    /// `cursor = Some((sort_value, id))` 表示「从这一行**之后**开始」；
    /// `None` 表示从 `start` 位置起（用 OFFSET，只在跳转 / 首屏时发生）。
    ///
    /// ## ⚠️ 三个必须一起做对的事
    ///
    /// 1. **比较方向与排序方向一致**：`DESC` 排序要 `(k, id) < (?, ?)`，
    ///    `ASC` 要 `>`。写反了会从错误的方向开始翻页。
    /// 2. **元组比较必须带 id**：同一天拍的照片 `taken_at` 完全相同，
    ///    只比 `taken_at` 会跳过整批。
    /// 3. **名字排序的比较键是 `name_key` 不是 `name`**：与 `ORDER BY` 逐字一致。
    pub fn range_keyset(
        &self,
        folder_ids: Option<&[i64]>,
        sort: SortSpec,
        start: usize,
        n: usize,
        cursor: Option<(i64, i64)>,
    ) -> Result<Vec<PhotoRow>> {
        // ⚠️ **只有「按拍摄时间」走键集**，其余排序老实走 OFFSET。
        //
        // 判据是「有没有一条以排序键打头的复合索引」：
        //
        // | 排序键 | 可用索引 | 键集可走？ |
        // | --- | --- | --- |
        // | `TakenAt` | `idx_photos_taken_id (taken_at, id)` | ✅ |
        // | `Size` | 只有 `idx_photos_size_mtime (folder_id, …)` —— 前导列是 folder_id | ❌ |
        // | `ModifiedAt` | 只有 `mtime_ns DESC` 单列，没有 `(mtime_ns, id)` | ❌ |
        // | `Name` / `Dimensions` | 键是 TEXT / 表达式 | ❌ |
        //
        // 硬走键集但索引不匹配的后果是 SQLite 退化成「全表扫描 + 临时 B 树排序」，
        // 那比 OFFSET 更慢，而且**结果依然完全正确**——这种退化只有执行计划能发现
        // （`TC-M3-SCALE-003` 就是为此存在的）。
        //
        // 补齐其余排序键的索引是 M4 排序功能的事（`08` §3.5 的 `idx_photos_size_id` 等），
        // 本版只承诺「所有照片」与「按日期」这条主路径。
        let use_keyset = cursor.is_some() && matches!(sort.key, SortKey::TakenAt);

        let mut sql = format!(
            "SELECT {} FROM photos p JOIN folders f ON f.id = p.folder_id",
            self.photo_cols()
        );
        let mut where_sql: Vec<String> = Vec::new();
        if let Some(ids) = folder_ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let list = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            where_sql.push(format!("p.folder_id IN ({list})"));
        }
        where_sql.push(self.active_root_clause().to_string());

        if use_keyset {
            let (k, id) = cursor.expect("use_keyset 时 cursor 必然存在");
            let col = self.keyset_col(sort.key);
            let op = if sort.descending { "<" } else { ">" };
            // ⚠️ **平局决胜必须与 `ORDER BY` 逐字一致**。
            //
            // `SortSpec::order_by(TakenAt)` 是 `<eff> DESC, name_key ASC`
            // （见 `order_by` 的实现）——也就是说同一时刻的照片按**文件名**排，
            // 而主排序键相同时的游标必须用 `name_key`，**不能用 `id`**。
            //
            // 细节：这仍然是「正确但可能重复/遗漏」的经典陷阱——
            // 若平局条件写成 `eff = ? AND id < ?`，而实际顺序是 `eff, name_key`，
            // 那么一次翻页会跳过所有「同一时刻且 name_key 更小但 id 更大」的照片。
            // 它不会报错，只会让用户滚到底少几张（`keyset_tiebreak_matches_the_order_by_clause` 守着）。
            //
            // 取游标那一行的 `name_key` 作为平局键。
            let tie: (Option<String>, i64) = self
                .conn
                .query_row(
                    "SELECT name_key, id FROM photos p WHERE p.id = ?1",
                    params![id],
                    |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()?
                .unwrap_or((None, id));
            // 注意 `sort.descending` 只管**主键**；`order_by` 里平局键永远是 `name_key ASC`
            // （见 `SortSpec::order_by`）。所以平局比较一律用 `>`（「名字比游标大」才排在后面）。
            match tie.0 {
                Some(name_key) => {
                    let escaped = sql_text_literal(&name_key);
                    where_sql.push(format!(
                        "({col} {op} {k} OR ({col} = {k} AND p.name_key > '{escaped}'))"
                    ));
                }
                None => {
                    where_sql.push(format!(
                        "({col} {op} {k} OR ({col} = {k} AND p.id {op} {id}))"
                    ));
                }
            }
        }

        sql.push_str(&format!(" WHERE {}", where_sql.join(" AND ")));
        sql.push_str(" ORDER BY ");
        sql.push_str(&sort.order_by(self.eff_col()));
        sql.push_str(&format!(" LIMIT {n}"));
        if !use_keyset && start > 0 {
            sql.push_str(&format!(" OFFSET {start}"));
        }

        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 键集分页用的「排序键列」表达式（必须与 [`SortSpec::order_by`] 一致）。
    fn keyset_col(&self, key: SortKey) -> String {
        match key {
            SortKey::TakenAt => self.eff_col().to_string(),
            SortKey::ModifiedAt => "p.mtime_ns".to_string(),
            SortKey::Size => "p.size_bytes".to_string(),
            // 其余走 OFFSET（见 `range_keyset` 的注释），这里给个不会用到的占位
            SortKey::Name | SortKey::Dimensions => "p.id".to_string(),
        }
    }

    /// 一张照片在整列里的下标（锚点恢复，`07` §9.3）。
    ///
    /// 用 `COUNT(*)` 而不是「取全表在 Rust 里数」：走索引的计数是 O(log n)，
    /// 而数行要读全表——100 万张时前者亚毫秒、后者几秒。
    pub fn index_of_photo(
        &self,
        folder_ids: Option<&[i64]>,
        sort: SortSpec,
        photo_id: i64,
    ) -> Result<Option<usize>> {
        if matches!(sort.key, SortKey::Name) {
            // 文件名排序：键是 TEXT，复合索引 `(folder_id, name_key)` 已能接受 OFFSET；
            // 这里用一条计数查询，比较键与 ORDER BY 逐字一致。
            let target: Option<(String, i64)> = self
                .conn
                .query_row(
                    "SELECT name_key, id FROM photos WHERE id = ?1",
                    params![photo_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()?;
            let Some((name_key, id)) = target else {
                return Ok(None);
            };
            let mut where_sql = vec![self.active_root_clause().to_string()];
            if let Some(ids) = folder_ids {
                if ids.is_empty() {
                    return Ok(None);
                }
                let list = ids
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                where_sql.push(format!("p.folder_id IN ({list})"));
            }
            let op = if sort.descending { ">" } else { "<" };
            let sql = format!(
                "SELECT COUNT(*) FROM photos p JOIN folders f ON f.id = p.folder_id
                  WHERE {} AND (p.name_key {op} '{}' OR (p.name_key = '{}' AND p.id {op} {}))",
                where_sql.join(" AND "),
                escape_like(&name_key),
                escape_like(&name_key),
                id
            );
            let before: i64 = self.conn.query_row(&sql, [], |r| r.get(0))?;
            return Ok(Some(before.max(0) as usize));
        }

        let col = self.keyset_col(sort.key);
        let target: Option<(i64, i64)> = self
            .conn
            .query_row(
                &format!("SELECT {col}, p.id FROM photos p WHERE p.id = ?1"),
                params![photo_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((k, id)) = target else {
            return Ok(None);
        };

        let mut where_sql = vec![self.active_root_clause().to_string()];
        if let Some(ids) = folder_ids {
            if ids.is_empty() {
                return Ok(None);
            }
            let list = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            where_sql.push(format!("p.folder_id IN ({list})"));
        }
        let op = if sort.descending { ">" } else { "<" };
        let sql = format!(
            "SELECT COUNT(*) FROM photos p JOIN folders f ON f.id = p.folder_id
              WHERE {} AND ({col} {op} {k} OR ({col} = {k} AND p.id {op} {id}))",
            where_sql.join(" AND ")
        );
        let before: i64 = self.conn.query_row(&sql, [], |r| r.get(0))?;
        Ok(Some(before.max(0) as usize))
    }

    /// 按天分组的行数（`07` §5.2 SCALE-02）。
    ///
    /// 返回 `(当天 00:00 的 Unix 秒, 张数)`，按日期**倒序**。
    ///
    /// ⚠️ 时区口径必须与 UI 的分组一致（`chrono::Local`）。SQL 里用 `'localtime'`
    /// 让 SQLite 按本机时区切天——若这里用 UTC 而 UI 用本地，
    /// 组头会与栅格错开一整天（跨时区用户尤其明显）。
    pub fn date_group_counts(&self, folder_ids: Option<&[i64]>) -> Result<Vec<(i64, u64)>> {
        let mut where_sql = vec![self.active_root_clause().to_string()];
        if let Some(ids) = folder_ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let list = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            where_sql.push(format!("p.folder_id IN ({list})"));
        }
        // `date(x, 'unixepoch', 'localtime')` 把「墙上时间」还原成用户看到的那一天；
        // 再 `strftime('%s', ...)` 折回 Unix 秒（本地时区零点），供 UI 直接格式化。
        // ⚠️ `strftime('%s', …)` 返回的是**文本**（SQLite 里 `%s` 是字符串格式），
        // 直接 `get::<i64>` 会得到 `InvalidColumnType`。
        // 用 `CAST(... AS INTEGER)` 才拿到整数——这条在测试里撞到过一次，
        // 表现是「建数据源就报错」，很容易被误判成迁移问题。
        let sql = format!(
            "SELECT CAST(strftime('%s', date({eff}, 'unixepoch', 'localtime')) AS INTEGER) AS day,
                    COUNT(*)
               FROM photos p JOIN folders f ON f.id = p.folder_id
              WHERE {where_clause}
              GROUP BY day
              ORDER BY day DESC",
            eff = self.eff_col(),
            where_clause = where_sql.join(" AND ")
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (day, n) = row?;
            out.push((day, n.max(0) as u64));
        }
        Ok(out)
    }

    /// 目录 id → 规范化路径（组头文案与诊断用）。找不到返回 `Err`。
    pub fn folder_path(&self, folder_id: i64) -> Result<String> {
        self.conn
            .query_row(
                "SELECT path FROM folders WHERE id = ?1",
                params![folder_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| DbError::Sqlite(rusqlite::Error::QueryReturnedNoRows))
    }

    /// 一次取 `thumbs` 的 BLOB（`07` §5.2 SCALE-05：首屏只取可见的）。
    ///
    /// `ids` 由调用方限长在 [`crate::datasource::IN_LIST_MAX`]。
    pub fn thumbs_for(&self, ids: &[i64], tier: i64) -> Result<std::collections::HashMap<i64, ThumbBuf>> {
        let mut out = std::collections::HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let list = ids
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT photo_id, w, h, format, data FROM thumbs
              WHERE tier = {tier} AND photo_id IN ({list})"
        );
        let mut st = self.conn.prepare(&sql)?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            out.insert(
                id,
                ThumbBuf {
                    w: r.get(1)?,
                    h: r.get(2)?,
                    format: r.get(3)?,
                    data: r.get(4)?,
                },
            );
        }
        Ok(out)
    }

    /// 键集查询的执行计划（`07` §12.2 `TC-M3-SCALE-003`：**必须无 `SCAN TABLE photos`**）。
    ///
    /// 把「查询能走索引」变成可断言的字符串，而不是靠跑基准看耗时——
    /// 耗时随机器负载飘，执行计划不飘。
    pub fn explain_keyset_plan(&self, sort: SortSpec) -> Result<Vec<String>> {
        let col = self.keyset_col(sort.key);
        let op = if sort.descending { "<" } else { ">" };
        let sql = format!(
            "EXPLAIN QUERY PLAN
             SELECT p.id FROM photos p JOIN folders f ON f.id = p.folder_id
              WHERE {active} AND ({col} {op} 0 OR ({col} = 0 AND p.id {op} 0))
              ORDER BY {order} LIMIT 200",
            active = self.active_root_clause(),
            order = sort.order_by(self.eff_col())
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], |r| r.get::<_, String>(3))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 每个目录的**递归**照片数（`folders.total_count`）。
    ///
    /// ⚠️ UI **不该读这个**（`07` §4.1：这一列只写不读，界面用快照里的
    /// `folder_totals`，那份与 `folder_counts` 同事务查出，不会与别处差 1）。
    /// 这里暴露给三条路径：① 迁移测试；② 诊断页；③ M4 之后**没有**快照的场合。
    pub fn folder_totals(&self) -> Result<std::collections::HashMap<i64, u32>> {
        let mut st = self.conn.prepare("SELECT id, total_count FROM folders")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (id, n) = row?;
            out.insert(id, n.max(0) as u32);
        }
        Ok(out)
    }

    /// 单个目录的递归数（`folder_totals` 的单点查询版）。
    pub fn folder_total(&self, folder_id: i64) -> Result<u32> {
        let n: i64 = self.conn.query_row(
            "SELECT total_count FROM folders WHERE id = ?1",
            params![folder_id],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u32)
    }

    /// 目录的 `dir_mtime`（**纳秒**，自 epoch 起）。`None` = 还没量过 → 走 L2（`07` §3）。
    ///
    /// 存纳秒不存秒：L1 要的是「有没有变」这个二值判断，秒级精度在
    /// 「同一秒内拷进 1000 张」这种场景下会把变化吞掉（见 `verify.rs` 的对照表）。
    /// 列名仍叫 `dir_mtime`（`07` §8.1 的 DDL 口径），精度差异记在 `docs/06` §12.1。
    pub fn folder_dir_mtime(&self, folder_id: i64) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT dir_mtime FROM folders WHERE id = ?1",
                params![folder_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }

    /// 落库线程在**目录遍历完成时**写 `dir_mtime`（`07` §3.2 VERIFY-02：
    /// 与扫描结果同一事务，不是事后补一条）。
    pub fn set_folder_dir_mtime(&self, folder_id: i64, mtime_s: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET dir_mtime = ?2 WHERE id = ?1",
            params![folder_id, mtime_s],
        )?;
        Ok(())
    }

    /// 按名字找一张照片（迁移/诊断用；`name_key` 已索引）。
    pub fn photo_by_name(&self, name: &str) -> Result<Option<PhotoRow>> {
        let key = crate::pathutil::compare_key(name);
        let sql = format!(
            "SELECT {} FROM photos p JOIN folders f ON f.id = p.folder_id
              WHERE p.name_key = ?1 LIMIT 1",
            self.photo_cols()
        );
        Ok(self
            .conn
            .query_row(&sql, params![key], map_photo)
            .optional()?)
    }

    /// 全库按格式分组的张数（迁移验收与诊断用）。
    pub fn format_histogram(&self) -> Result<Vec<(i64, u64)>> {
        let mut st = self
            .conn
            .prepare("SELECT format, COUNT(*) FROM photos GROUP BY format ORDER BY 2 DESC")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (f, n) = row?;
            out.push((f, n.max(0) as u64));
        }
        Ok(out)
    }

    /// 一张照片的 `format` 编码。
    pub fn photo_format(&self, photo_id: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT format FROM photos WHERE id = ?1",
            params![photo_id],
            |r| r.get(0),
        )?)
    }

    /// 一张照片的 `taken_from` 编码。
    pub fn photo_taken_from(&self, photo_id: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT taken_from FROM photos WHERE id = ?1",
            params![photo_id],
            |r| r.get(0),
        )?)
    }

    /// 按名字取 `taken_from`（迁移测试的便捷入口）。
    pub fn photo_taken_from_by_name(&self, name: &str) -> Result<Option<i64>> {
        let key = crate::pathutil::compare_key(name);
        Ok(self
            .conn
            .query_row(
                "SELECT taken_from FROM photos WHERE name_key = ?1 LIMIT 1",
                params![key],
                |r| r.get::<_, i64>(0),
            )
            .optional()?)
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

    /// 把目录挂到某个关注根下（`folders.root_id`）。
    ///
    /// 生产路径上这一列由 `derive_hierarchy()` 统一推导（迁移时一次算完）；
    /// 但**测试与诊断**需要能手工挂一个目录，否则每个造夹具的地方都要复制一遍
    /// 「写 roots 行 + 推导层级」的仪式。公开它是安全的：它只写一列，不推导父子关系。
    pub fn set_folder_root(
        &self,
        folder_id: i64,
        root_id: i64,
        parent: Option<i64>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET root_id = ?2, parent_id = ?3 WHERE id = ?1",
            params![folder_id, root_id, parent],
        )?;
        Ok(())
    }

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

    /// 删除一个目录（及其照片，靠 `photos.folder_id` 的 CASCADE）。
    ///
    /// M4.3 / R-M43-02：删之前把该目录下每张照片的用户标记快照进身份骨架，
    /// 这样 `remove_root` / `exclude_folder` 的删除也不会丢星标/标签/说明/相册；
    /// 重新扫描（`apply_scan`）会按身份把标记挂回新 `photo_id`。
    pub fn delete_folder(&self, folder_id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        if self.has_identity {
            let path: Option<String> = tx
                .query_row(
                    "SELECT path FROM folders WHERE id = ?1",
                    params![folder_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(path) = path {
                identity::snapshot_for_folder(&tx, &path)?;
            }
        }
        // 自引用 parent_id 先断开，避免删除顺序触发外键问题。
        let _ = tx.execute(
            "UPDATE folders SET parent_id = NULL WHERE parent_id = ?1",
            params![folder_id],
        );
        tx.execute("DELETE FROM folders WHERE id = ?1", params![folder_id])?;
        tx.commit()?;
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
        let res = self.apply_scan_inner(folder_id, folder_path, items, decoder_ver, false, &mut out);
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

    /// 全量重建（M4.3 IDX-14）：与 [`apply_scan`] 相同事务边界，但强制绕过 L1 缓存命中，
    /// 让每张已存在照片都进 `touched` 重新生成缩略图；且**不删旧缩略图 BLOB、不把
    /// `thumb_state` 归零**（旧图保留到被 `put_thumb` 原子替换为止）。
    pub fn apply_scan_force(
        &self,
        folder_id: i64,
        folder_path: &str,
        items: &[PhotoInput],
        decoder_ver: i64,
    ) -> Result<ScanApply> {
        let mut out = ScanApply::default();
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let res = self.apply_scan_inner(folder_id, folder_path, items, decoder_ver, true, &mut out);
        match res {
            Ok(()) => { self.conn.execute_batch("COMMIT")?; Ok(out) }
            Err(e) => { let _ = self.conn.execute_batch("ROLLBACK"); Err(e) }
        }
    }

    fn apply_scan_inner(
        &self,
        folder_id: i64,
        folder_path: &str,
        items: &[PhotoInput],
        decoder_ver: i64,
        force: bool,
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
                    if *size == it.size_bytes && *mtime == it.mtime_ns && *ver == decoder_ver && !force =>
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
                                           -- `format = 0` 表示「这次没判」→ 保留库里的值。
                                           -- 缓存命中的路径不重读文件头，它不该把已知格式抹掉。
                                           format = CASE WHEN ?7 = 0 THEN format ELSE ?7 END,
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
                            it.taken_at_src,
                            it.format
                        ],
                    )?;
                    out.unchanged += 1;
                }
                Some((id, ..)) => {
                    // 缓存键变了：元数据更新 + 缩略图作废（thumb_state 归零、BLOB 删掉）
                    self.conn.execute(
                        "UPDATE photos SET size_bytes = ?2, mtime_ns = ?3, width = ?4,
                                           height = ?5, orientation = ?6, taken_at = ?7,
                                           taken_at_src = ?8, decoder_ver = ?9,
                                           thumb_state = CASE WHEN ?11 = 1 THEN thumb_state ELSE 0 END,
                                           format = CASE WHEN ?10 = 0 THEN format ELSE ?10 END,
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
                            decoder_ver,
                            it.format,
                            i32::from(force)
                        ],
                    )?;
                    if !force {
                        self.conn
                            .execute("DELETE FROM thumbs WHERE photo_id = ?1", params![id])?;
                    }
                    out.updated += 1;
                    out.touched.push(*id);
                }
                None => {
                    self.conn.execute(
                        "INSERT INTO photos
                           (folder_id, name, name_key, size_bytes, mtime_ns, width, height,
                            orientation, taken_at, taken_at_src, thumb_state, decoder_ver,
                            dims_status, format)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0,?11,
                                 CASE WHEN ?6 IS NOT NULL AND ?7 IS NOT NULL THEN 1 ELSE 0 END,
                                 ?12)",
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
                            decoder_ver,
                            it.format
                        ],
                    )?;
                    out.added += 1;
                    let new_id = self.conn.last_insert_rowid();
                    out.touched.push(new_id);
                    // M4.3 / R-M43-02：新建的照片行按身份把先前快照的标记挂回来
                    // （重建/重加后 photo_id 变了也能恢复），无骨架表时为空操作。
                    if self.has_identity {
                        identity::restore_photo(
                            &self.conn,
                            folder_path,
                            &it.name_key,
                            new_id,
                        )?;
                    }
                }
            }
        }

        // 3) 磁盘上已消失的 → 删索引（cascade 会带走 thumbs）
        let stale: Vec<(i64, String)> = existing
            .iter()
            .filter(|(k, _)| !seen.contains(k.as_str()))
            .map(|(k, v)| (v.0, k.clone()))
            .collect();
        for (id, name_key) in stale {
            // M4.3 / R-M43-02：删 live 行前先快照用户标记，避免 CASCADE 静默带走。
            if self.has_identity {
                identity::snapshot_photo(&self.conn, folder_path, &name_key, id)?;
            }
            self.conn
                .execute("DELETE FROM photos WHERE id = ?1", params![id])?;
            out.removed += 1;
        }

        // ---- `dir_mtime` 与扫描结果**同一事务**写入（`07` §3.2 VERIFY-02）
        //
        // 这是 L1 快筛能生效的**唯一**前提：没有它，`dir_mtime` 永远是 NULL，
        // 于是 L1 每次都说「还没量过 → 走 L2」，两级校验退化成 M2.2 的老行为，
        // 而这一点**不会报错**——只是启动一直慢 20 分钟。
        //
        // 为什么在这里 stat 而不是让调用方传进来：`apply_scan` 是「这个目录的
        // 遍历结果落库」的**唯一入口**，把 `dir_mtime` 的写入点绑在这里，
        // 就不可能出现「某条扫描路径忘了写」。而 `apply_scan` 本身已经过了
        // `read_dir`，多一次目录 `stat` 在噪声里（一次 stat ≈ 2 µs）。
        let dir_mtime = std::fs::metadata(folder_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_nanos()).ok());

        self.conn.execute(
            "UPDATE folders SET last_scan_at = strftime('%s','now'), is_online = 1,
                                direct_count = (SELECT COUNT(*) FROM photos WHERE folder_id = ?1),
                                dir_mtime = COALESCE(?2, dir_mtime)
             WHERE id = ?1",
            params![folder_id, dir_mtime],
        )?;

        // ---- 递归计数缓存（`07` §4.1：同一事务维护 `total_count`）
        //
        // 只沿 parent 链往上走：一个目录的增删只影响它的祖先，不影响兄弟或子孙。
        // 4330 个目录、平均深 4 层 → 约 4 次 UPDATE，可以忽略。
        //
        // ⚠️ 与 `folder_counts()` 的关系：后者读 `direct_count`（本层），
        // 快照里的 `folder_totals` 由**同事务内存聚合**算出（见 `count_snapshot`）。
        // 这一列只是缓存，给 M4+ 的查询用；**UI 不读它**。
        self.conn.execute(
            "WITH RECURSIVE up(id) AS (
                 SELECT parent_id FROM folders WHERE id = ?1 AND parent_id IS NOT NULL
                  UNION ALL
                 SELECT f.parent_id FROM folders f JOIN up ON f.id = up.id
                  WHERE f.parent_id IS NOT NULL
             )
             UPDATE folders SET total_count = COALESCE(
                 (SELECT SUM(direct_count) FROM folders c
                   WHERE c.id = folders.id
                      OR c.parent_id = folders.id
                      OR c.parent_id IN (SELECT id FROM folders WHERE parent_id = folders.id))
             , 0)
             WHERE id IN (SELECT id FROM up)",
            params![folder_id],
        )?;
        Ok(())
    }

    // ----------------------------------------------------------------- photos

    /// 「关闭的关注文件夹」在 SQL 侧的过滤条件。
    ///
    /// `folders.root_id` 指向 `roots.id`，而关闭根目录只改 `roots.state`（不删索引），
    /// 所以「用户还能不能看到这张照片」完全由 `roots.state` 决定：
    ///
    /// ```text
    /// EXISTS (SELECT 1 FROM roots r WHERE r.id = f.root_id AND r.state = 0)
    /// ```
    ///
    /// ## 为什么是 EXISTS 而不是 JOIN
    ///
    /// 与 `docs/06` §6.5 的 `root_stats` 同一个理由：`JOIN roots` 会让优化器物化整行，
    /// 而 `EXISTS` 是半连接，且 `roots` 是小表（个位数行）、`idx_roots_key` 之外
    /// 还有主键索引可用，代价可忽略。`thumbs` 那种 1.26 GB 的 BLOB 表才真正怕 JOIN，
    /// 这里的 `roots` 怕的是「把一条能走索引的排序查询变成要建临时 B-tree」。
    ///
    /// ## 为什么 root_id 为 NULL 的行也算「可见」
    ///
    /// `folders.root_id` 由 `derive_hierarchy()` 按**全部** roots（含已关闭的）推导，
    /// 正常库不会为 NULL。但两种情况下它是 NULL：① 层级还没推导（迁移失败降级运行）；
    /// ② 用户手动改过库。这时候**判成不可见会让整个图库空掉**——比「多显示几张」
    /// 糟得多。所以 `COALESCE(f.root_id, 0)` 落到「没有归属根」分支，按可见处理。
    ///
    /// ⚠️ 这条过滤**只认 `roots.state`**，不看 `folders.is_online`，
    /// 也不做任何 `stat`。**离线根（外接盘没插）的照片必须仍然看得到**——
    /// 那是 P-17 的裁决（「所有照片」包含离线根），与「用户主动关闭」是两件事。
    fn active_root_clause(&self) -> &'static str {
        // `f.root_id IS NULL` 必须**判成可见**（见上）。写成裸 `EXISTS` 会让
        // 所有「还没推导层级」的库整库空白——那比多显示几张糟得多。
        "(f.root_id IS NULL
          OR EXISTS (SELECT 1 FROM roots r WHERE r.id = f.root_id AND r.state = 0))"
    }

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
        self.photos_from_scoped(folder_ids, sort, limit, offset, false)
    }

    /// 同 [`Db::photos_from`]，但**排除已关闭关注文件夹下的照片**。
    ///
    /// 这是栅格真正该走的那条：用户关掉一个关注文件夹，就是不想再看到它的照片。
    /// 与 `photos_from`（物理全库）分开而不是改后者，原因是后者有两个**必须保留
    /// 物理口径**的使用者：① `m21_nav` 断言「关闭不删索引」（照片行必须还在）；
    /// ② `c_bench` 的性能基准要压整库。
    pub fn photos_from_visible(
        &self,
        folder_ids: Option<&[i64]>,
        sort: SortSpec,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<PhotoRow>> {
        self.photos_from_scoped(folder_ids, sort, limit, offset, true)
    }

    /// 键集（seek）翻页：从 `cursor` 那一行**之后**取 `limit` 行。
    ///
    /// ## 为什么必须补这条路径
    ///
    /// `OFFSET` 的代价是「先跳过 offset 行」，25 万张实测深页 **103 ms**
    /// （`docs/18` TC-M43-12 的预算是 p99 ≤ 25 ms）。同一条查询改成
    /// 「`WHERE (排序键, name_key) > 游标` + `LIMIT`」后走
    /// `idx_photos_eff_taken` 直接 seek，同一页实测 **2.3 ms**——快 45 倍。
    ///
    /// ## 只有「按拍摄时间」能用
    ///
    /// 键集要求「排序键有索引且单调」。`TakenAt` 有 `idx_photos_eff_taken
    /// (eff_taken_at DESC, name_key ASC)`，正是 `SortSpec::order_by` 的一条；
    /// 其余排序键（名称是 TEXT、尺寸是表达式、修改时间只有单列索引）
    /// 硬走键集会让 SQLite 退化成全表 + 临时 B 树排序，比 OFFSET 更慢。
    /// 因此 `cursor` 只对 `SortKey::TakenAt` 生效，其余情况退化为首屏语义
    /// （调用方本来也只在 `TakenAt` 下传游标）。
    ///
    /// ⚠️ **平局决胜必须与 `ORDER BY` 逐字一致**：`order_by(TakenAt)` 是
    /// `eff DESC, name_key ASC`，同一秒的照片按文件名排，所以比较键是
    /// `name_key` 而不是 `id`（与 [`Db::range_keyset`] 同一个坑，那里有更长的说明）。
    pub fn photos_from_visible_after(
        &self,
        folder_ids: Option<&[i64]>,
        sort: SortSpec,
        limit: usize,
        cursor: Option<PageCursor>,
    ) -> Result<Vec<PhotoRow>> {
        let conds = self.scope_conds(folder_ids, true);
        self.page_after(conds, sort, limit, cursor)
    }

    /// 一行的键集游标（`(eff_taken_at, name_key)`）。查不到返回 `None`。
    ///
    /// 单独抽出来是因为 `PhotoRow` 不带 `name_key`（界面用 `name` 就够），
    /// 而游标必须用**归一化后的**比较键，否则大小写 / NFC 差异会让翻页错行。
    pub fn page_cursor(&self, photo_id: i64) -> Result<Option<PageCursor>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {}, p.name_key FROM photos p WHERE p.id = ?1", self.eff_col()),
                params![photo_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?)
    }

    /// 拼「`WHERE <scope> AND <keyset>` + ORDER BY + LIMIT」的公用实现。
    fn page_after(
        &self,
        conds: Option<Vec<String>>,
        sort: SortSpec,
        limit: usize,
        cursor: Option<PageCursor>,
    ) -> Result<Vec<PhotoRow>> {
        let mut all: Vec<String> = conds.unwrap_or_default();
        if let Some((k, name_key)) = cursor
            && sort.key == SortKey::TakenAt
        {
            let eff = self.eff_col();
            let op = if sort.descending { "<" } else { ">" };
            let escaped = sql_text_literal(&name_key);
            all.push(format!(
                "({eff} {op} {k} OR ({eff} = {k} AND p.name_key > '{escaped}'))"
            ));
        }
        let mut sql = format!(
            "SELECT {} FROM photos p JOIN folders f ON f.id = p.folder_id",
            self.photo_cols()
        );
        if !all.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&all.join(" AND "));
        }
        sql.push_str(" ORDER BY ");
        sql.push_str(&sort.order_by(self.eff_col()));
        sql.push_str(&format!(" LIMIT {limit}"));

        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    fn photos_from_scoped(
        &self,
        folder_ids: Option<&[i64]>,
        sort: SortSpec,
        limit: usize,
        offset: usize,
        visible_only: bool,
    ) -> Result<Vec<PhotoRow>> {
        let mut sql = format!(
            "SELECT {} FROM photos p JOIN folders f ON f.id = p.folder_id",
            self.photo_cols()
        );
        let where_sql = self.build_scope(folder_ids, visible_only);
        if let Some(w) = where_sql {
            sql.push_str(&w);
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

    /// 时间轴（用户功能 #7）：按「有效拍摄时间」的年份分组计数。
    /// 只统计可见根；年份取 `strftime('%Y', eff, 'unixepoch')`，eff 是秒。
    pub fn timeline_years(&self) -> Result<Vec<(i32, i64)>> {
        let eff = self.eff_col();
        let sql = format!(
            "SELECT CAST(strftime('%Y', {eff}, 'unixepoch') AS INTEGER) AS y, COUNT(*) AS n
             FROM photos p JOIN folders f ON f.id = p.folder_id
             {}
             GROUP BY y ORDER BY y DESC",
            self.active_root_clause(),
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i32>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 筛选 + 排序 + 分页的一站式查询（用户功能 #5/#9/#10）。
    ///
    /// `filter` 为空（全 None / 空集）时等价于 [`Db::photos_from_visible`]；
    /// 带 `filter` 时按「与」关系拼条件：日期范围、格式集、尺寸类、收藏、标签。
    /// `folder_ids` 沿用 `photos_from_visible` 的口径（`None` = 全库）。
    pub fn photos_filtered(
        &self,
        folder_ids: Option<&[i64]>,
        filter: &PhotoFilter,
        sort: SortSpec,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<PhotoRow>> {
        let where_sql = match self.filter_where(folder_ids, filter) {
            FilterWhere::Inactive => {
                return self.photos_from_visible(folder_ids, sort, limit, offset);
            }
            // 未知格式名：条件恒假，返回空（防拼出非法 SQL）
            FilterWhere::Empty => return Ok(Vec::new()),
            FilterWhere::Sql(sql) => sql,
        };

        let mut sql = format!(
            "SELECT {} FROM photos p JOIN folders f ON f.id = p.folder_id{where_sql}",
            self.photo_cols(),
        );
        sql.push_str(" ORDER BY ");
        sql.push_str(&sort.order_by(self.eff_col()));
        sql.push_str(&format!(" LIMIT {limit}"));
        if offset > 0 {
            sql.push_str(&format!(" OFFSET {offset}"));
        }

        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 把栅格分成「有缩略图」「没有缩略图」两桶再排序（用户要求：有图的往前放）。
    ///
    /// 桶键**只看 `photos.thumb_state = 1`**，不去查 `thumbs` 表：
    /// 真实库 5.6 万张实测，`ORDER BY CASE WHEN EXISTS(SELECT 1 FROM thumbs ...)`
    /// 要 **5.6 s**（每行一次相关子查询），而只看整数列是 **0.04 s**。
    /// 「库里其实有 BLOB 但 `thumb_state != 1`」这种情况在 `apply_scan` 里不会出现
    /// （写 BLOB 与置状态是同一批操作），值这个取舍。
    ///
    /// 实现是**两条独立查询**（先第一桶、取不满再补第二桶），不是一条带
    /// `CASE WHEN ... THEN 0 ELSE 1 END` 的排序：两条查询各自走
    /// `idx_photos_eff_taken` 直接截断，25 万张实测 0.01 s，而带 CASE 的
    /// 单一排序会退化成临时 B 树（25 万张 0.14 s）。
    ///
    /// 代价：偏移量是**两桶合并后的全局偏移**，第二桶内部的深偏移仍是
    /// O(offset)。栅格分页是 1500 首屏 + 4000 一页，实测可接受（5.6 万张
    /// `OFFSET 50000` 0.05 s）。
    pub fn photos_bucketed(
        &self,
        folder_ids: Option<&[i64]>,
        filter: Option<&PhotoFilter>,
        sort: SortSpec,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<PhotoRow>> {
        let conds = match filter {
            Some(f) => match self.filter_where(folder_ids, f) {
                FilterWhere::Inactive => self.scope_conds(folder_ids, true),
                FilterWhere::Empty => return Ok(Vec::new()),
                FilterWhere::Sql(w) => Some(vec![w
                    .trim_start()
                    .trim_start_matches("WHERE ")
                    .to_string()]),
            },
            None => self.scope_conds(folder_ids, true),
        };

        // 全局偏移必须先减掉第一桶的行数，否则 `offset` 越过分桶边界后会整页错位
        // （第一桶返回空、第二桶又从 0 开始取，用户滑到「没有缩略图」那一段时
        // 会看到一堆已经看过的图重复出现）。
        let first_len = self.bucket_count(conds.clone(), true)?;
        if offset >= first_len {
            return self.bucket_rows(conds, sort, limit, offset - first_len, false);
        }
        // 这一页落在第一桶内。取满 `limit` 就说明第二桶还轮不到。
        let first = self.bucket_rows(conds.clone(), sort, limit, offset, true)?;
        if first.len() >= limit {
            return Ok(first);
        }
        // 第一桶到底了，第二桶从 0 开始把这一页补满。
        let need = limit - first.len();
        let mut rows = first;
        rows.extend(self.bucket_rows(conds, sort, need, 0, false)?);
        Ok(rows)
    }

    /// 单桶行数。分页要把全局偏移换算成桶内偏移，必须知道第一桶有多大。
    /// 只数 `photos.thumb_state`，走 `idx_photos_thumb`，5.6 万张约 1 ms。
    fn bucket_count(&self, conds: Option<Vec<String>>, with_thumbs: bool) -> Result<usize> {
        let mut sql = String::from("SELECT COUNT(*) FROM photos p JOIN folders f ON f.id = p.folder_id");
        let mut all = conds.unwrap_or_default();
        all.push(if with_thumbs {
            format!("p.thumb_state = {}", ThumbState::Ok as i64)
        } else {
            format!("p.thumb_state <> {}", ThumbState::Ok as i64)
        });
        sql.push_str(" WHERE ");
        sql.push_str(&all.join(" AND "));
        let n: i64 = self.conn.query_row(&sql, [], |r| r.get(0))?;
        Ok(n.max(0) as usize)
    }

    /// `photos_bucketed` 的单桶实现：`with_thumbs` 决定取 `thumb_state = 1`
    /// 还是 `thumb_state <> 1`（含 0=未生成 / 2=失败 / 3=损坏 / 4=云端未下载，
    /// 这些在界面上都没有图，属于同一个「没有缩略图」桶）。
    fn bucket_rows(
        &self,
        conds: Option<Vec<String>>,
        sort: SortSpec,
        limit: usize,
        offset: usize,
        with_thumbs: bool,
    ) -> Result<Vec<PhotoRow>> {
        let mut sql = format!(
            "SELECT {} FROM photos p JOIN folders f ON f.id = p.folder_id",
            self.photo_cols()
        );
        let mut all = conds.unwrap_or_default();
        all.push(if with_thumbs {
            format!("p.thumb_state = {}", ThumbState::Ok as i64)
        } else {
            format!("p.thumb_state <> {}", ThumbState::Ok as i64)
        });
        sql.push_str(" WHERE ");
        sql.push_str(&all.join(" AND "));
        sql.push_str(" ORDER BY ");
        sql.push_str(&sort.order_by(self.eff_col()));
        sql.push_str(&format!(" LIMIT {limit}"));
        if offset > 0 {
            sql.push_str(&format!(" OFFSET {offset}"));
        }
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 带筛选的键集翻页：语义同 [`Db::photos_from_visible_after`]，
    /// 只是 WHERE 多了筛选条件（复用 [`Db::filter_where`]，保证与首屏同一套条件）。
    pub fn photos_filtered_after(
        &self,
        folder_ids: Option<&[i64]>,
        filter: &PhotoFilter,
        sort: SortSpec,
        limit: usize,
        cursor: Option<PageCursor>,
    ) -> Result<Vec<PhotoRow>> {
        let where_sql = match self.filter_where(folder_ids, filter) {
            FilterWhere::Inactive => return self.photos_from_visible_after(folder_ids, sort, limit, cursor),
            FilterWhere::Empty => return Ok(Vec::new()),
            FilterWhere::Sql(sql) => sql,
        };

        let mut sql = format!(
            "SELECT {} FROM photos p JOIN folders f ON f.id = p.folder_id{where_sql}",
            self.photo_cols(),
        );
        if let Some((k, name_key)) = cursor
            && sort.key == SortKey::TakenAt
        {
            let eff = self.eff_col();
            let op = if sort.descending { "<" } else { ">" };
            let escaped = sql_text_literal(&name_key);
            sql.push_str(&format!(
                " AND ({eff} {op} {k} OR ({eff} = {k} AND p.name_key > '{escaped}'))"
            ));
        }
        sql.push_str(" ORDER BY ");
        sql.push_str(&sort.order_by(self.eff_col()));
        sql.push_str(&format!(" LIMIT {limit}"));

        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 与 [`Db::photos_filtered`] **同一套 WHERE** 的行数。
    ///
    /// 栅格标题用这个数，不能用「可见全库」的 `count_photos_visible`：
    /// 两者不一致时，首屏虽然是筛过的，分页补齐却会按无筛选的 OFFSET
    /// 把其余照片再拼进来。
    pub fn count_photos_filtered(
        &self,
        folder_ids: Option<&[i64]>,
        filter: &PhotoFilter,
    ) -> Result<u64> {
        match self.filter_where(folder_ids, filter) {
            FilterWhere::Inactive => self.count_photos_visible(folder_ids),
            FilterWhere::Empty => Ok(0),
            FilterWhere::Sql(where_sql) => {
                let sql = format!(
                    "SELECT COUNT(*) FROM photos p JOIN folders f ON f.id = p.folder_id{where_sql}"
                );
                let n: i64 = self.conn.query_row(&sql, [], |r| r.get(0))?;
                Ok(n.max(0) as u64)
            }
        }
    }

    /// 筛选 WHERE（含前导空格与 `WHERE`）。`photos_filtered` 与计数共用，避免两套条件漂开。
    fn filter_where(&self, folder_ids: Option<&[i64]>, filter: &PhotoFilter) -> FilterWhere {
        if !filter.is_active() {
            return FilterWhere::Inactive;
        }
        let mut where_sql = self.build_scope(folder_ids, true).unwrap_or_default();
        if !where_sql.contains("WHERE") {
            where_sql.push_str(" WHERE 1=1");
        }
        if let Some((from, to)) = filter.taken_range {
            // 走「有效拍摄时间」（缺失回退 mtime），与排序同口径
            where_sql.push_str(&format!(" AND {} BETWEEN {from} AND {to}", self.eff_col()));
        }
        if !filter.formats.is_empty() {
            // 格式名 → 数字编码。UI 的 slug 是 `jpeg` 不是扩展名 `jpg`；
            // 库里存的是 `photos.format` 的整数（JPEG = 1），两边在 `from_slug` 汇合。
            let codes: Vec<i64> = filter
                .formats
                .iter()
                .filter_map(|f| PhotoFormat::from_slug(f))
                .map(|pf| pf as i64)
                .collect();
            if codes.is_empty() {
                return FilterWhere::Empty;
            }
            let list = codes.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",");
            where_sql.push_str(&format!(" AND p.format IN ({list})"));
        }
        if let Some(sc) = filter.size_class {
            where_sql.push_str(&format!(" AND {}", sc.sql_cond()));
        }
        if filter.starred_only {
            where_sql.push_str(" AND p.starred = 1");
        }
        if let Some(tag) = &filter.tag {
            where_sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM tags t WHERE t.photo_id = p.id AND t.label = '{}')",
                sql_text_literal(tag)
            ));
        }
        if let Some(ids) = &filter.ids {
            // IDX-19「本次导入 · N 张」：空集是**明确「没有照片」**，
            // 不能退化成「不过滤」——否则空批次会把整个图库显示出来。
            if ids.is_empty() {
                return FilterWhere::Empty;
            }
            let list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
            where_sql.push_str(&format!(" AND p.id IN ({list})"));
        }
        // ---- M4 组合筛选新增维度（docs/11 §4.1）----
        if let Some((lo, hi)) = filter.size_bytes {
            where_sql.push_str(&format!(" AND p.size_bytes BETWEEN {lo} AND {hi}"));
        }
        if let Some((lo, hi)) = filter.dimension_range {
            where_sql.push_str(&format!(
                " AND MAX(COALESCE(p.width,0), COALESCE(p.height,0)) BETWEEN {lo} AND {hi}"
            ));
        }
        if let Some(cam) = &filter.camera {
            where_sql.push_str(&format!(
                " AND (COALESCE(p.camera_make,'') || ' ' || COALESCE(p.camera_model,'')) LIKE '%{}%'",
                sql_text_literal(cam)
            ));
        }
        if let Some(lens) = &filter.lens {
            where_sql.push_str(&format!(
                " AND COALESCE(p.lens_model,'') LIKE '%{}%'",
                sql_text_literal(lens)
            ));
        }
        if filter.missing_date {
            where_sql.push_str(" AND (p.taken_at IS NULL OR p.taken_from = 2)");
        }
        if filter.missing_exif {
            where_sql.push_str(" AND p.exif_status <> 1");
        }
        if let Some(album) = &filter.album {
            where_sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM album_photos ap JOIN albums a ON a.id = ap.album_id
                              WHERE ap.photo_id = p.id AND a.name = '{}')",
                sql_text_literal(album)
            ));
        }
        FilterWhere::Sql(where_sql)
    }

    /// 拼 `WHERE` 子句（含前导 ` WHERE `）。返回 `None` 表示不加条件（物理全库）。
    ///
    /// `folder_ids == Some(&[])` 是「视图里一个目录都没有」，必须**短路成恒假**，
    /// 而不是拼出 `IN ()`（那是 SQL 语法错误）。这个短路在这里统一做，
    /// 免得每个调用点各写一遍、漏一个就 panic。
    fn build_scope(&self, folder_ids: Option<&[i64]>, visible_only: bool) -> Option<String> {
        self.scope_conds(folder_ids, visible_only)
            .map(|conds| format!(" WHERE {}", conds.join(" AND ")))
    }

    /// 与 [`Db::build_scope`] 同一套条件的**未拼接**形式。
    ///
    /// 键集翻页要把 scope 与游标条件用 `AND` 连起来，先拼成一整条 `WHERE`
    /// 再往里塞字符串很容易拼出 `WHERE a WHERE b`；这里返回条件向量。
    /// 空文件夹列表返回 `vec!["0"]`（`IN ()` 不合法，`0` 让结果恒空）。
    fn scope_conds(&self, folder_ids: Option<&[i64]>, visible_only: bool) -> Option<Vec<String>> {
        let mut conds: Vec<String> = Vec::new();
        if let Some(ids) = folder_ids {
            if ids.is_empty() {
                return Some(vec!["0".to_string()]);
            }
            let list = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            conds.push(format!("p.folder_id IN ({list})"));
        }
        if visible_only {
            conds.push(self.active_root_clause().to_string());
        }
        if conds.is_empty() {
            None
        } else {
            Some(conds)
        }
    }

    /// 视图内照片总数（C1 首屏先拿总数，用来判断「要不要继续补」与显示进度）。
    /// 走 `EXPLAIN QUERY PLAN` 验证过：有 `folder_id` 条件时用 `idx_photos_folder_taken`，
    /// 无条件时是 `SCAN photos` 的计数优化（SQLite 对无 WHERE 的 `COUNT(*)` 直接读页数）。
    pub fn count_photos(&self, folder_ids: Option<&[i64]>) -> Result<u64> {
        self.count_photos_scoped(folder_ids, false)
    }

    /// 同 [`Db::count_photos`]，但排除已关闭关注文件夹。
    ///
    /// ⚠️ **必须与 `photos_from_visible` 同口径**，否则会出现「状态栏说 13 张、
    /// 栅格只画出 1 张」——这正是 `docs/04` §3.2 定为 P0 的那类偏差。
    pub fn count_photos_visible(&self, folder_ids: Option<&[i64]>) -> Result<u64> {
        self.count_photos_scoped(folder_ids, true)
    }

    fn count_photos_scoped(
        &self,
        folder_ids: Option<&[i64]>,
        visible_only: bool,
    ) -> Result<u64> {
        let mut sql = String::from("SELECT COUNT(*) FROM photos p");
        if visible_only {
            // `COUNT` 的可见口径要 join 到 folders 才拿得到 root_id
            sql.push_str(" JOIN folders f ON f.id = p.folder_id");
        }
        if let Some(w) = self.build_scope(folder_ids, visible_only) {
            sql.push_str(&w);
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
        self.explain_photos_plan_scoped(sort, false)
    }

    /// 同 [`Db::explain_photos_plan`]，但带**可见作用域**（栅格真正走的那条）。
    ///
    /// 必须单独有这一条：`explain_photos_plan` 的 SQL 里没有 `active_root` 过滤，
    /// 它变绿不能证明栅格用的那条没退化——两边的 WHERE 结构不同，
    /// SQLite 选择索引的方式也就可能不同。`tests/plan_visible_scope.rs` 用它守着。
    pub fn explain_visible_plan(&self, sort: SortSpec) -> Result<Vec<String>> {
        self.explain_photos_plan_scoped(sort, true)
    }

    fn explain_photos_plan_scoped(&self, sort: SortSpec, visible_only: bool) -> Result<Vec<String>> {
        let where_sql = self
            .build_scope(None, visible_only)
            .unwrap_or_default();
        let sql = format!(
            "EXPLAIN QUERY PLAN
             SELECT p.id FROM photos p JOIN folders f ON f.id = p.folder_id{where_sql}
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
        // 搜索结果同样不许越过「已关闭的关注文件夹」——否则搜一下就能把
        // 用户明确关掉的目录的照片又翻出来。
        sql.push_str(" AND ");
        sql.push_str(self.active_root_clause());
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
    ///
    /// 只返回**活动关注文件夹**下的照片：关闭一个根目录后，它的照片不该继续
    /// 出现在「最近更改」里（与「所有照片」同口径）。
    pub fn recently_changed(&self, limit: usize) -> Result<Vec<PhotoRow>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {}
             FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE {}
             ORDER BY p.mtime_ns DESC LIMIT ?1",
            self.photo_cols(),
            self.active_root_clause()
        ))?;
        let rows = st.query_map(params![limit as i64], map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 从索引里移除一张（「从图库移除」，**不删文件**，F2-08 的独立命令）。
    ///
    /// M4.3 / R-M43-02：裸 `DELETE FROM photos` 会借 CASCADE 静默带走星标/标签/
    /// 说明/相册，和 `clear_index` / `delete_folder` / `apply_scan` 的 stale 清理是
    /// 同一条红线。删前按 `(folder_path, name_key)` 快照到 v9 身份骨架，重新扫描
    /// 加回同一路径时由 `apply_scan_inner` 的 restore 分支挂回新 id。
    /// 与 `delete_folder` 一样用显式事务保证「快照 + 删除」原子。
    pub fn forget_photo(&self, id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        if self.has_identity {
            let info: Option<(String, String)> = tx
                .query_row(
                    "SELECT f.path, p.name_key FROM photos p
                       JOIN folders f ON f.id = p.folder_id
                      WHERE p.id = ?1",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((folder_path, name_key)) = info {
                identity::snapshot_photo(&tx, &folder_path, &name_key, id)?;
            }
        }
        tx.execute("DELETE FROM photos WHERE id = ?1", params![id])?;
        tx.commit()?;
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

    /// 把一个目录里指定的这批照片标成「云端未下载」（`ThumbState::CloudOnly`）。
    ///
    /// 只按 `name_key` 定位（扫描器手里就是这个键，不必再回查 id），一次 `UPDATE`
    /// 处理整批。返回实际改动的行数。
    ///
    /// 为什么单独一个方法而不是复用 [`Db::set_thumb_state`]：那个按 id 逐张写，
    /// 一个目录几百上千张就是几百上千条语句；而这里是一次性、幂等的批量标记。
    pub fn mark_photos_cloud_only(&self, folder_id: i64, name_keys: &[String]) -> Result<u64> {
        if name_keys.is_empty() {
            return Ok(0);
        }
        let mut total = 0u64;
        for chunk in name_keys.chunks(200) {
            // `thumb_state <> 1`：已经出过图的**不降级**。占位文件的缩略图可能是
            // 之前下载期间生成的，那种情况下库里还有合法 BLOB，把它标成「云端未下载」
            // 等于凭空把一张能看的图变成灰格。
            let mut sql = String::from(
                "UPDATE photos SET thumb_state = ?1 \
                 WHERE folder_id = ?2 AND thumb_state <> ?4 AND name_key IN (",
            );
            for i in 0..chunk.len() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push('?');
                sql.push_str(&(i + 5).to_string());
            }
            sql.push(')');
            let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() + 3);
            let state = ThumbState::CloudOnly as i64;
            let ok = ThumbState::Ok as i64;
            params.push(&state);
            params.push(&folder_id);
            params.push(&state);
            params.push(&ok);
            for k in chunk {
                params.push(k);
            }
            total += self.conn.execute(&sql, params.as_slice())? as u64;
        }
        Ok(total)
    }

    /// 把一个目录里所有「云端未下载」的照片放回「待生成」。
    ///
    /// 与 [`Db::mark_photos_cloud_only`] 配对使用：扫描时先复位、再把仍无内容的
    /// 重新标上。这样「文件终于下载到本地了」这件事不需要额外的 watcher ——
    /// 下一轮扫描自然会把它放回队列。
    pub fn clear_cloud_only(&self, folder_id: i64) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE photos SET thumb_state = ?1 WHERE folder_id = ?2 AND thumb_state = ?3",
            params![
                ThumbState::Pending as i64,
                folder_id,
                ThumbState::CloudOnly as i64
            ],
        )?;
        Ok(n as u64)
    }

    /// 一个「关注文件夹」下的所有子目录 id（含它自己）。
    ///
    /// 用 `substr(path, 1, n) = 'root/'` 而不是 `LIKE`：路径里合法地可以出现 `%` 和 `_`，
    /// 用 `LIKE` 就必须转义，转错一次就会把别的目录算进来。
    /// 按 `roots.path` 的路径前缀取子树 folder id（含根自己）。
    ///
    /// 与 [`Db::subtree_folder_ids`] 同样的 `substr` 前缀算法，但入口是 **root 路径**
    /// 而不是 folder id——作用域删除拿到的是 `roots.id`（`MgrAction::DeleteIndexRoot`）。
    fn subtree_folder_ids_for_path(&self, root_path: &str) -> Result<Vec<i64>> {
        let cut = root_path.chars().count() as i64 + 1;
        let prefix = format!("{root_path}/");
        let mut st = self.conn.prepare(
            "SELECT id FROM folders
              WHERE path = ?1 OR substr(path, 1, ?2) = ?3
              ORDER BY path",
        )?;
        let rows = st.query_map(params![root_path, cut, prefix], |r| {
            r.get::<_, i64>(0)
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 一个 root 下已建索引的照片数（作用域删除确认框显示用）。
    pub fn photo_count_for_root(&self, root_id: i64) -> Result<i64> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM roots WHERE id = ?1",
                params![root_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(path) = path else {
            return Ok(0);
        };
        let ids = self.subtree_folder_ids_for_path(&path)?;
        if ids.is_empty() {
            return Ok(0);
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT COUNT(*) FROM photos WHERE folder_id IN ({placeholders})"
        );
        let mut st = self.conn.prepare(&sql)?;
        let count = st.query_row(
            rusqlite::params_from_iter(ids.iter()),
            |r| r.get::<_, i64>(0),
        )?;
        Ok(count)
    }

    /// M4.3 IDX-11：一个 root 下「带用户标记」的照片数（作用域确认框显示用）。
    ///
    /// 「有标记」= 星标 / 说明 / 标签 / 相册任一即可，按照片去重（不是按标记条数）。
    /// 返回 `None` 表示该 root 不存在或统计失败 —— UI 必须显示「待计算」，
    /// **绝不能当成 0**（§2 M43-IDX-11：未知数量显示待计算，不能填 0）。
    /// 子树范围沿用 [`Self::subtree_folder_ids_for_path`] 的路径前缀口径。
    pub fn marker_count_for_root(&self, root_id: i64) -> Result<Option<i64>> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM roots WHERE id = ?1",
                params![root_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(path) = path else {
            return Ok(None);
        };
        let ids = self.subtree_folder_ids_for_path(&path)?;
        if ids.is_empty() {
            return Ok(Some(0));
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT COUNT(*) FROM photos p WHERE p.folder_id IN ({placeholders}) \
             AND (p.starred = 1 \
                  OR EXISTS (SELECT 1 FROM captions c WHERE c.photo_id = p.id) \
                  OR EXISTS (SELECT 1 FROM tags t WHERE t.photo_id = p.id) \
                  OR EXISTS (SELECT 1 FROM album_photos ap WHERE ap.photo_id = p.id))"
        );
        let mut st = self.conn.prepare(&sql)?;
        let count = st.query_row(
            rusqlite::params_from_iter(ids.iter()),
            |r| r.get::<_, i64>(0),
        )?;
        Ok(Some(count))
    }

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
                    COALESCE(SUM(CASE WHEN p.thumb_state = 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN p.thumb_state = 4 THEN 1 ELSE 0 END), 0)
             FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE f.path = ?1 OR substr(f.path, 1, ?2) = ?3",
        )?;
        let row = st.query_row(params![path, cut, prefix], |r| {
            Ok(ThumbStats {
                total: r.get::<_, i64>(0)? as u64,
                ready: r.get::<_, i64>(1)? as u64,
                failed: r.get::<_, i64>(2)? as u64,
                pending: r.get::<_, i64>(3)? as u64,
                cloud_only: r.get::<_, i64>(4)? as u64,
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
                    COALESCE(SUM(CASE WHEN thumb_state = 0 THEN 1 ELSE 0 END), 0) AS pending,
                    COALESCE(SUM(CASE WHEN thumb_state = 4 THEN 1 ELSE 0 END), 0) AS cloud_only
             FROM photos WHERE folder_id = ?1",
        )?;
        let row = st.query_row(params![folder_id], |r| {
            Ok(ThumbStats {
                total: r.get::<_, i64>(0)? as u64,
                ready: r.get::<_, i64>(1)? as u64,
                failed: r.get::<_, i64>(2)? as u64,
                pending: r.get::<_, i64>(3)? as u64,
                cloud_only: r.get::<_, i64>(4)? as u64,
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
                    COALESCE(SUM(CASE WHEN thumb_state = 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN thumb_state = 4 THEN 1 ELSE 0 END), 0)
             FROM photos",
        )?;
        let row = st.query_row([], |r| {
            Ok(ThumbStats {
                total: r.get::<_, i64>(0)? as u64,
                ready: r.get::<_, i64>(1)? as u64,
                failed: r.get::<_, i64>(2)? as u64,
                pending: r.get::<_, i64>(3)? as u64,
                cloud_only: r.get::<_, i64>(4)? as u64,
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

    /// 是否存在「用旧版解码器解过」或「一直没人补图」的照片行（`thumb_state=0`）。
    ///
    /// M4 启动补全的判据。两种情况都要补：
    /// ① `decoder_ver < 当前`（版本 bump 后旧失败/旧缓存必须重做）；
    /// ② ver 相同但仍 `thumb_state=0`（上一轮补图被中断 / 启动扫描只补新增的历史欠账）。
    pub fn has_rows_with_decoder_ver_before(&self, ver: i64) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM photos WHERE decoder_ver < ?1 OR thumb_state = 0)",
            [ver],
            |r| r.get(0),
        )?;
        Ok(n != 0)
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

    /// 全量重建（IDX-14）用：按 id 取待重建照片行，**不过滤 thumb_state**。
    /// force 路径故意保留旧 `thumb_state`（通常 1=Ok），所以不能走
    /// `pending_thumbs_by_ids`（它带 `WHERE p.thumb_state = 0`）。
    pub fn photos_by_ids_for_force(&self, ids: &[i64], limit: usize) -> Result<Vec<PhotoRow>> {
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
             WHERE p.id IN ({list})
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
                   FROM folders WHERE root_id IS NOT NULL GROUP BY root_id",
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
                  WHERE p.thumb_state IN (2, 3) AND f.root_id IS NOT NULL
                  GROUP BY f.root_id",
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
        let now = unix_now_secs();
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
        let now = unix_now_secs();
        self.conn.execute(
            "UPDATE folders SET last_verified_at = ?1 WHERE id = ?2",
            params![now, folder_id],
        )?;
        Ok(())
    }

    /// 取一批「最久没校验过」的目录（`07` §3.2 VERIFY-03）。
    ///
    /// ## 为什么按 `last_verified_at` 升序
    ///
    /// L2 全校的预算是「每轮最多 N 个目录」，而目录总数可能远大于 N。
    /// 升序取最久没校验的，等价于**轮转**：跑完一轮之后，最早被校验的那些
    /// 又变成最久的，自然轮到它们。用 `ORDER BY random()` 或「按 id」都会
    /// 让某些目录永远轮不到。
    ///
    /// `NULL`（从未校验）排最前：它们的日期最不可信。
    ///
    /// ⚠️ 走 `idx_folders_verified`（v6 建的 `(last_verified_at)` 索引），
    /// 否则 10 万目录的排序会把 UI 线程卡住几百毫秒。
    pub fn folders_due_for_verify(
        &self,
        older_than_secs: i64,
        limit: usize,
    ) -> Result<Vec<(i64, String)>> {
        let mut st = self.conn.prepare(
            "SELECT id, path FROM folders
              WHERE last_verified_at IS NULL OR last_verified_at < ?1
              -- ⚠️ 不能写成 `ORDER BY last_verified_at IS NOT NULL, last_verified_at`：
              -- 那个表达式会让 SQLite 放弃索引走 `USE TEMP B-TREE FOR ORDER BY`
              -- （10 万目录上就是几百毫秒的 UI 卡顿）。而 SQLite 的 ASC 排序
              -- **本来就把 NULL 排在最前**，正是我们要的语义。
              ORDER BY last_verified_at ASC, id ASC
              LIMIT ?2",
        )?;
        let rows = st.query_map(params![older_than_secs, limit as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// `folders_due_for_verify` 的执行计划（诊断与回归用）。
    pub fn explain_due_for_verify_plan(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "EXPLAIN QUERY PLAN
             SELECT id, path FROM folders
              WHERE last_verified_at IS NULL OR last_verified_at < ?1
              ORDER BY last_verified_at ASC, id ASC
              LIMIT 200",
        )?;
        let rows = st.query_map(params![0i64], |r| r.get::<_, String>(3))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 有多少目录「该校验了」（任务行显示「还有 N 个目录待校验」用）。
    pub fn count_folders_due_for_verify(&self, older_than_secs: i64) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM folders
              WHERE last_verified_at IS NULL OR last_verified_at < ?1",
            params![older_than_secs],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
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
    pub fn mgmt_counts(&self) -> Result<CountSnapshot> {
        self.count_snapshot()
    }

    /// **同一事务**取出「本层数 + 递归数 + 组边界来源 + 所有照片」。
    ///
    /// `07` §4.1 的要求：`folder_totals` 与 `folder_counts` **同一事务**查出，
    /// 在内存里一次自底向上聚合。为什么不直接读 `folders.total_count`（那列明明存在）：
    ///
    /// 1. 缓存列要靠落库线程维护，**任何一次漏维护**都会让界面显示错数，
    ///    而且错得毫无征兆（数字只是少了，不会报错）；
    /// 2. 4330 个目录在内存里聚合一次是**亚毫秒**级（Map 查 + 加法），
    ///    100 万张 / 10 万目录也就几毫秒，比多读一列还便宜；
    /// 3. 同一事务意味着它天然与 `direct_count` 是**同一个世界线**，
    ///    `TC-M22-IDX-011` 那种「连续取 100 个 seq 断言五处一致」才可能过。
    ///
    /// `total_count` 列仍然写（`07` §4.1：供 M4+ 的其他查询用），只是**不读**。
    ///
    /// ⚠️ 只做**只读**查询，别往这个事务里塞写操作（`DEFERRED` 会升级成写锁，
    /// 在 WAL 下与落库线程抢锁，把流水线拖住）。
    pub fn count_snapshot(&self) -> Result<CountSnapshot> {
        let tx = self.conn.unchecked_transaction()?;

        // 一次查询同时拿 id / 父 / 本层数：聚合需要 parent 关系，
        // 单独再查一次 folders 就等于把 10 万行读两遍。
        let mut counts = std::collections::HashMap::new();
        let mut parents: std::collections::HashMap<i64, Option<i64>> =
            std::collections::HashMap::new();
        {
            let mut st = tx.prepare("SELECT id, parent_id, direct_count FROM folders")?;
        // ⚠️ 这条是全表读：4000 个目录 ≈ 4000 行，实测 6 ms 左右。
        // 目录数到 10 万时（07 §5.1 的 100 万张假设）会到 150 ms 级 —— 超 PERF-20。
        // 那是**已知的下一道坎**，见 `docs/07-M3-实现状态与冲突登记.md` §2.13。
        // 解法是把 `total_count` 的聚合挪到落库线程（那时它已经是 O(depth) 的增量维护），
        // 快照只读缓存列。现在不做：`total_count` 的增量维护还没验证过正确性。
            let mut rows = st.query(params![])?;
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                let parent: Option<i64> = r.get(1)?;
                let n: i64 = r.get(2)?;
                counts.insert(id, n.max(0) as u32);
                parents.insert(id, parent);
            }
        }

        // ---- 「所有照片」= **按目录求和**，不是「逐张照片数」
        //
        // ⚠️ 这条以前写的是 `COUNT(*) FROM photos JOIN folders JOIN roots`。
        // 它要**遍历每一张照片行**并逐行回查 `roots`——100 万张实测 **57 ms**，
        // 而 `07` §6.1 PERF-20 的上限是 20 ms。1 万目录 / 100 万张的库上必挂。
        //
        // 两边等价性：`folders.direct_count` 就是「这个目录有几张照片」，
        // 而 `apply_scan` 每次落库都在同一事务里重算它（见那里的 UPDATE）。
        // 所以 `Σ direct_count over 活跃根` 恒等于「活跃根下的照片总数」。
        //
        // 代价从 O(照片数) 降到 O(目录数)：1000 个目录实测 **0.06 ms**（快 900 倍）。
        //
        // ⚠️ 唯一的前提是 `direct_count` 与 `photos` 行数**不能漂移**。
        // 这不是新引入的依赖——`mgmt_counts` 早就用 `direct_count` 当「本层数」，
        // 而 `TC-M22-IDX-011` 一直要求它与「所有照片」逐字段相等。
        let all: i64 = tx.query_row(
            "SELECT COALESCE(SUM(f.direct_count), 0)
               FROM folders f
               JOIN roots   r ON r.id = f.root_id
              WHERE r.state = 0",
            [],
            |r| r.get(0),
        )?;
        tx.commit()?;

        let totals = aggregate_totals(&counts, &parents);
        Ok(CountSnapshot {
            direct: counts,
            totals,
            all_photos: all.max(0) as u64,
        })
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
    ///
    /// M4.3 / R-M43-02：`DELETE FROM photos` 会经 CASCADE 带走 `starred` /
    /// `captions` / `tags` / `album_photos`。删前整库快照到 v9 身份骨架，
    /// 重新扫描时按身份恢复（见 `identity.rs`）。
    pub fn clear_index(&self) -> Result<(u64, u64)> {
        let photos: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM photos", [], |r| r.get(0))?;
        let thumbs: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM thumbs", [], |r| r.get(0))
            .unwrap_or(0);

        let tx = self.conn.unchecked_transaction()?;
        if self.has_identity {
            identity::snapshot_all(&tx)?;
        }
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

    /// 作废某个 root 作用域里**所有还活着**的持久任务意图（M4.3 IDX-11 / G11.3）。
    ///
    /// 旧实现在作用域删除时直接 `DELETE FROM index_tasks WHERE root_id=?1`：行没了之后
    /// `read_intent` 查不到会降级 Run，内存队列里还没取出的旧 Job 会把已经移除的范围
    /// 重新扫回来。改成落 Cancelled 终态 + 抬 generation：既让目录边界的意图检查把旧
    /// Job 让出去，也保留「结果跨重启可追溯」（IDX-17）。
    ///
    /// 收 `&Transaction`（调用方已开事务）——`cancel_index_task` 会另开一层事务，
    /// SQLite 不允许嵌套，所以这里直接用同一条 UPDATE 作用于调用方的事务。
    /// `root_id` 可能为 NULL（阶段 1 `root_id_for_path` 返回 Option），故同时按
    /// `scope_path` 精确/前缀匹配收窄到本 root，既不漏也不误伤其它 root。
    fn invalidate_scope_intents(
        &self,
        tx: &rusqlite::Transaction<'_>,
        root_id: i64,
        root_path: &str,
    ) -> Result<()> {
        let cut = root_path.chars().count() as i64 + 1;
        let prefix = format!("{root_path}/");
        tx.execute(
            "UPDATE index_tasks
              SET cancel_reason = ?2,
                  outcome       = 3,
                  status        = 3,
                  finished_at   = strftime('%s','now'),
                  error_code    = ?2,
                  generation    = generation + 1
              WHERE (root_id = ?1 OR scope_path = ?3
                     OR substr(scope_path, 1, ?4) = ?5)
                AND cancel_reason IS NULL
                AND finished_at IS NULL
                AND kind IN (0,1,2,3)
                AND scope_kind = 0",
            params![root_id, "索引已移除", root_path, cut, prefix],
        )?;
        Ok(())
    }

    /// 删除**某一个关注文件夹**（root）下的索引，其他 root 完全不动。
    ///
    /// M4.3 §2 / §7 P-M43-04「单根 + 全部」：作用域删除必须带稳定 `root_id`，
    /// 不准像旧接线那样丢弃 id 转全库 `delete_index()`。与 `clear_index` 一样
    /// 保留 roots / exclusions，并在删前按路径前缀快照标记（R-M43-02）。
    pub fn clear_index_for_root(&self, root_id: i64) -> Result<(u64, u64)> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM roots WHERE id = ?1",
                params![root_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(root_path) = path else {
            return Ok((0, 0));
        };
        let folder_ids = self.subtree_folder_ids_for_path(&root_path)?;

        let tx = self.conn.unchecked_transaction()?;
        // 0) 作废该 scope 的持久任务意图必须在「有没有已建索引」之前做：
        //    刚入队、还没写任何 folders 的任务同样要失效，否则重启会把已移除的
        //    范围重新扫回来（IDX-11 / G11.3）。
        self.invalidate_scope_intents(&tx, root_id, &root_path)?;
        if folder_ids.is_empty() {
            tx.commit()?;
            // 即便一行索引都没建过，也要落「已移除」标记：用户点了删除，
            // 界面必须能区分「删过」与「从没索引过」（IDX-10 的「隐藏范围」）。
            self.mark_root_index_removed(root_id)?;
            return Ok((0, 0));
        }
        let placeholders = vec!["?"; folder_ids.len()].join(",");
        let in_ids = format!("({placeholders})");

        // 1) 标记快照：整棵子树一把抓（集合式，5 条语句）。
        //    早先版本逐个 folders.path 调 snapshot_for_folder，而那个函数又逐张照片
        //    跑 6 条 SQL——9.6 万张照片 ≈ 58 万条语句挤在一个事务里，期间一直持有
        //    DB 锁，UI 每帧抢锁就冻结五十秒（实机复现，见 v0.7-dev20）。
        if self.has_identity {
            identity::snapshot_for_folder_ids(&tx, &folder_ids)?;
        }
        // 2) 计数 + 删子表 photos（thumbs / captions / tags / album_photos 靠 CASCADE）
        let photos: i64 = tx.query_row(
            &format!("SELECT COUNT(*) FROM photos WHERE folder_id IN {in_ids}"),
            rusqlite::params_from_iter(folder_ids.iter()),
            |r| r.get(0),
        )?;
        let thumbs: i64 = tx
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM thumbs t JOIN photos p ON p.id = t.photo_id
                      WHERE p.folder_id IN {in_ids}"
                ),
                rusqlite::params_from_iter(folder_ids.iter()),
                |r| r.get(0),
            )
            .unwrap_or(0);
        tx.execute(
            &format!("DELETE FROM photos WHERE folder_id IN {in_ids}"),
            rusqlite::params_from_iter(folder_ids.iter()),
        )?;
        // 3) 断开子树内/外指向这些 folder 的 parent_id，再删 folder 行
        tx.execute(
            &format!("UPDATE folders SET parent_id = NULL WHERE parent_id IN {in_ids}"),
            rusqlite::params_from_iter(folder_ids.iter()),
        )?;
        tx.execute(
            &format!("UPDATE folders SET parent_id = NULL WHERE id IN {in_ids}"),
            rusqlite::params_from_iter(folder_ids.iter()),
        )?;
        tx.execute(
            &format!("DELETE FROM folders WHERE id IN {in_ids}"),
            rusqlite::params_from_iter(folder_ids.iter()),
        )?;
        // 4) 该 scope 的持久任务意图已在步骤 0（同一事务、且在「有没有已建索引」
        //    的早退之前）统一作废，这里不必也不能重复开事务再写一遍。
        // 失败明细属于被移除的范围，一并清掉（其它 root 的任务/失败不受影响）。
        tx.execute(
            "DELETE FROM index_task_failures WHERE task_id IN \
                (SELECT task_id FROM index_tasks WHERE root_id = ?1)",
            params![root_id],
        )?;
        tx.commit()?;
        // IDX-10：移除索引后该根进入「已移除」态（持久、跨重启），
        // 界面据此显示「索引已移除」而不是照旧报「正常」。
        self.mark_root_index_removed(root_id)?;

        Ok((photos.max(0) as u64, thumbs.max(0) as u64))
    }

    /// **彻底**删掉一个关注根的索引（m10218）：连 `roots` 行一起删。
    ///
    /// ## 与 `clear_index_for_root` 的区别（两条路都必须存在）
    ///
    /// `clear_index_for_root` 是 IDX-10 的语义：清掉派生数据，但**保留 roots 行**
    /// 并落「索引已移除」标记，于是索引管理页还留着一行空壳，可以「重新索引（恢复）」。
    /// 用户点了「删除索引」却发现列表里那行还在，观感就是「没删干净」。
    ///
    /// 彻底删除则是：索引数据、缩略图、`roots` 行、`exclusions`、`m43_index_removed_<id>`
    /// 设置键，全部消失；界面上该根及其子树连同缩略图一起不见。
    ///
    /// 两步顺序不能反：
    /// - 只 `delete_root_row` → 留下孤儿的 folders/photos（`folders.root_id` 是
    ///   `ON DELETE SET NULL` 之外的外键，没有级联），索引数据永远搜不到删不掉；
    /// - 只 `clear_index_for_root` → 留下「已移除空壳」行，正是用户抱怨的现象。
    ///
    /// 标记不丢：`clear_index_for_root` 内部先按路径前缀做身份快照（R-M43-02），
    /// 所以删完之后重新添加同一个目录并扫描，收藏/标签/说明/相册会按身份挂回。
    /// **不碰磁盘**：只动自有 DB，原图一张都不删。
    pub fn purge_index_for_root(&self, root_id: i64) -> Result<(u64, u64)> {
        // 1) 先清派生数据：标记快照 + 作废任务意图 + 删 photos/folders/thumbs。
        let counts = self.clear_index_for_root(root_id)?;
        // 2) 再删根行。`index_tasks.root_id` 是 ON DELETE CASCADE，
        //    该根的历史任务与失败明细会随这一行一起消失（正是「彻底」的含义）。
        self.delete_root_row(root_id)?;
        // 3) m10756：这个根若是导入带进来的，且它是来源下**最后一个**根，
        //    来源行必须一起收掉。否则左栏「导入的索引」里会留下一个点开
        //    什么都没有的空节点——用户报的「删除后导入的索引还在文件夹树上」
        //    就是它（来源节点是照 `import_sources` 画的，不按根画）。
        //    来源还有别的根时保持不变：那说明用户只删了其中一组。
        let _ = prune_orphan_import_sources(self);
        Ok(counts)
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
        starred: r.get::<_, Option<i64>>(14)?.map(|v| v != 0).unwrap_or(false),
    })
}

// ============================================================ 收藏 / 标签 / 说明 (v7)
//
// 数据边界（产品定义）：只写应用数据库，不写照片文件 / IPTC / XMP。

impl Db {
    /// 设置 / 取消一批照片的收藏。批量走一个事务：多选 1000 张也是一次落盘。
    pub fn set_starred(&self, photo_ids: &[i64], on: bool) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare("UPDATE photos SET starred = ?1 WHERE id = ?2")?;
            for id in photo_ids {
                st.execute(rusqlite::params![if on { 1 } else { 0 }, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 给一批照片打一个标签（幂等：已有的不会重复插）。
    pub fn add_tag(&self, photo_ids: &[i64], label: &str) -> Result<()> {
        let label = label.trim();
        if label.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare("INSERT OR IGNORE INTO tags(photo_id, label) VALUES (?1, ?2)")?;
            for id in photo_ids {
                st.execute(rusqlite::params![id, label])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 从一批照片上摘掉一个标签。
    pub fn remove_tag(&self, photo_ids: &[i64], label: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare("DELETE FROM tags WHERE photo_id = ?1 AND label = ?2")?;
            for id in photo_ids {
                st.execute(rusqlite::params![id, label])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 一批照片各自的标签（多选面板展示用）。只查命中的 id。
    pub fn tags_of(&self, photo_id: i64) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare("SELECT label FROM tags WHERE photo_id = ?1 ORDER BY label")?;
        let rows = st.query_map([photo_id], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 全库标签清单（筛选面板用）：标签名 + 用它多少次，按次数降序。
    pub fn all_tags(&self) -> Result<Vec<(String, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT label, COUNT(*) AS n FROM tags GROUP BY label ORDER BY n DESC, label",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 当前视图范围内的标签计数（M4.3 `IDX-22` P1 的「本视图 N」）。
    ///
    /// 与 [`Db::all_tags`] 的区别只在**范围**：这里只数落在 `folder_ids` 里、
    /// 且 root 未被关闭的照片（与栅格同一 `active_root_clause` 口径）——
    /// 左栏两个数字必须是「同一批照片」的两种切法，否则用户会看到
    /// 「全库 12、本视图 5、可网格只画出 3 张」这种三方对不上的局面。
    ///
    /// `folder_ids` 为 `None` 表示全库（此时结果与 `all_tags` 的集合一致，
    /// 只是排序不同、且不含 0 计数）。
    pub fn tag_counts_visible(&self, folder_ids: Option<&[i64]>) -> Result<Vec<(String, i64)>> {
        // 两条路径，按「要不要触到 folders / roots」分流。
        //
        // ⚠️ 这里去掉过 `COUNT(DISTINCT t.photo_id)`：`tags` 的主键是
        // `(photo_id, label)`，一行 tag 只对应一张照片，`COUNT(*)` 与
        // 「有几张照片带这个标签」严格等价。旧写法的 DISTINCT 让 SQLite 必须
        // 建临时去重结构，25 万张 / 35 万行 tag 实测 171 ms，换成 `COUNT(*)` 后
        // 降到几十毫秒——**这不是「差不多」的差别，是能不能进 25 ms 预算的差别**。
        //
        // 全库口径（`folder_ids == None`）且所有根都可见时，可见性条件恒真，
        // 直接在主键覆盖索引上做一次分组即可；否则才 join photos / folders。
        let all_roots_visible = self
            .conn
            .query_row("SELECT COUNT(*) FROM roots WHERE state <> 0", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n == 0)
            .unwrap_or(false);
        if folder_ids.is_none() && all_roots_visible {
            let mut st = self
                .conn
                .prepare("SELECT label, COUNT(*) AS n FROM tags GROUP BY label ORDER BY label")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            return Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?);
        }

        // 通用路径：从 photos 驱动（`idx_photos_folder_taken` 或主键扫描），
        // 再按 `tags` 主键回查——比「先扫全部 tag 行再逐行判可见」省掉大量回表。
        let mut sql = String::from(
            "SELECT t.label, COUNT(*) AS n \
             FROM photos p \
             JOIN tags t ON t.photo_id = p.id \
             JOIN folders f ON f.id = p.folder_id",
        );
        if let Some(w) = self.build_scope(folder_ids, true) {
            sql.push_str(&w);
        }
        sql.push_str(" GROUP BY t.label ORDER BY t.label");
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 写说明（一张照片一段；空串 = 删除说明行，不留空壳）。
    pub fn set_caption(&self, photo_id: i64, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            self.conn
                .execute("DELETE FROM captions WHERE photo_id = ?1", [photo_id])?;
        } else {
            self.conn.execute(
                "INSERT INTO captions(photo_id, text) VALUES (?1, ?2)
                 ON CONFLICT(photo_id) DO UPDATE SET text = excluded.text",
                rusqlite::params![photo_id, text],
            )?;
        }
        Ok(())
    }

    /// 读说明。没有说明行 = 空串。
    pub fn caption_of(&self, photo_id: i64) -> Result<String> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT text FROM captions WHERE photo_id = ?1", [photo_id], |r| {
                r.get(0)
            })
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(v.unwrap_or_default())
    }

    /// 筛选：只收藏。返回收藏的 photo_id 集合（查询侧用它拼 IN 子句太重，
    /// 直接返回 id 让调用方走 `photos_page` 的 folder_ids 通道不合适——
    /// 这里提供独立分页查询，见 `starred_page`）。
    pub fn starred_ids(&self) -> Result<Vec<i64>> {
        let mut st = self
            .conn
            .prepare("SELECT id FROM photos WHERE starred = 1 ORDER BY id")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 筛选：打某标签的照片 id。
    pub fn ids_with_tag(&self, label: &str) -> Result<Vec<i64>> {
        let mut st = self
            .conn
            .prepare("SELECT photo_id FROM tags WHERE label = ?1 ORDER BY photo_id")?;
        let rows = st.query_map([label], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// M4.2 `M42-UI-06`：一次查库拿到「选中集里每张照片各自的标签」。
    ///
    /// 批量标签浮层算三态时，M4.1 的写法是**对每个标签各查一次**
    /// `ids_with_tag`（O(标签数) 次查库、且每帧重复）。这里改成一条
    /// `IN (...)` 把整个选中集的 `tags` 行一次取回，在内存里按 photo_id 聚合，
    /// 查库次数与标签数量无关（始终 1 次；选中集为空时 0 次）。
    ///
    /// 约束：只查 `tags` 关联表、不读原图、不新增表、不改 schema（R-M42-2）。
    pub fn tags_for_ids(&self, photo_ids: &[i64]) -> Result<HashMap<i64, Vec<String>>> {
        let mut out: HashMap<i64, Vec<String>> = HashMap::new();
        // 选中集为空必须短路：否则会拼出 `IN ()` 触发 SQL 语法错误
        // （与 build_scope :1568 的 `folder_ids == Some(&[])` 同类边界）。
        if photo_ids.is_empty() {
            return Ok(out);
        }
        let holes = std::iter::repeat_n("?", photo_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT photo_id, label FROM tags WHERE photo_id IN ({holes}) ORDER BY photo_id, label"
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(photo_ids.iter().copied()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, label) = row?;
            out.entry(id).or_default().push(label);
        }
        Ok(out)
    }
}

/// `LIKE` 转义，避免用户输入的 `%` / `_` 变成通配符。
/// 把字符串转义成 SQL 单引号字面量的内容（只处理 `'` 与 `\0`）。
///
/// ⚠️ 这是**唯一**该用来拼字符串字面量的函数。`escape_like` 是给 `LIKE` 用的
/// （它转义 `%` `_` `\`），两者不能混用——把 LIKE 转义喂给普通等值比较，
/// 会让文件名里的 `%` 被当成字面量反斜杠加百分号，于是「查不到」。
fn sql_text_literal(s: &str) -> String {
    s.replace('\'', "''").replace('\0', "")
}

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
            format: 0,
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

    /// M42-UI-06 TC-M42-B-06-01：`tags_for_ids` 一次取回选中集全部关联，
    /// 空选中集短路返回空 map（不会拼出 `IN ()`），未命中的 id 不出现在结果里。
    #[test]
    fn tags_for_ids_batches_and_short_circuits_empty() {
        let (db, fid) = db_with_folder();
        db.apply_scan(
            fid,
            "/tmp/a",
            &[
                mk("a.jpg", 1, 1, None),
                mk("b.jpg", 2, 2, None),
                mk("c.jpg", 3, 3, None),
            ],
            0,
        )
        .unwrap();
        let all = db.photos(Some(&[fid]), SortSpec::default(), 10).unwrap();
        let id = |n: &str| all.iter().find(|p| p.name == n).unwrap().id;
        let (a, b, c) = (id("a.jpg"), id("b.jpg"), id("c.jpg"));

        // 空选中集：0 次查库、返回空 map
        assert!(db.tags_for_ids(&[]).unwrap().is_empty());

        db.add_tag(&[a, b], "旅行").unwrap();
        db.add_tag(&[b], "人物").unwrap();

        let got = db.tags_for_ids(&[a, b, c]).unwrap();
        assert_eq!(got.get(&a).unwrap(), &vec!["旅行".to_string()]);
        // 同一张照片的两个标签按 label 排好序
        assert_eq!(got.get(&b).unwrap(), &vec!["人物".to_string(), "旅行".to_string()]);
        // 没有任何标签的 c 不出现在 map 里（用 .get().unwrap_or(&0) 显示 ☐）
        assert!(!got.contains_key(&c));
        assert_eq!(got.len(), 2);

        // 只查子集时也只返回子集
        let sub = db.tags_for_ids(&[c]).unwrap();
        assert!(sub.is_empty());
    }
}
