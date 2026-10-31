//! v1 → v2 升级的落地流程：**备份 → 单事务 DDL → 数据步骤 → 失败恢复**。
//!
//! ## 为什么备份必须是 `VACUUM INTO`
//!
//! 库开着 WAL，未 checkpoint 的事务在 `-wal` 里。只 `cp` 那个 `.db` 拿到的是一个
//! **缺事务的库**；而「先 checkpoint 再拷」要求没有活跃读者——索引线程还在跑时必然失败。
//!
//! `VACUUM INTO` 是读事务语义、产物是自洽的完整库、还顺带整理碎片。
//! 代价是临时多占一份库大小的空间（失败就中止升级，原库一个字节不动）。
//!
//! ## 失败之后
//!
//! DDL 全在 `BEGIN IMMEDIATE ... COMMIT` 里；SQLite 的 DDL 是事务性的，
//! 所以「迁移失败」天然回滚。备份再覆盖恢复一次是第二层保险。
//! 恢复后**以 M2 数据继续运行**（`docs/05` §1.4 第 7 步），不是拒绝启动——由
//! [`crate::Db::open_with_report`] 看 `UpgradeReport::error` 决定是否跳过 migrate。
//!
//! ## 数据步骤为什么单独有守卫
//!
//! `schema_migrations` 只保证「DDL 跑过一次」，不保证「数据补数跑过一次」。
//! 所以 roots 迁入 + 层级推导用 `data_migrations` 另记一笔，幂等靠它。

use crate::pathutil::{base_name, compare_key, is_under, nfc, parent_of};
use crate::schema::{DATA_V2_ROOTS_FOLDERS, MIGRATIONS, SCHEMA_VERSION};
use crate::{Db, DbError, FolderTreeRow, Result, RootSeed, UpgradeReport, V2DataReport};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

impl Db {
    /// 备份目录：与索引库同级，**永远在应用容器内**（R-1）。
    pub fn backup_dir(db_path: &Path) -> PathBuf {
        db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("Backups")
    }

    /// 最近一次升级的现场。应用启动时写日志 / 决定是否提示「降级需用备份覆盖」。
    pub fn last_upgrade_note(&self) -> Option<&UpgradeReport> {
        self.upgrade.as_ref()
    }

    /// 数据步骤有没有跑过（`false` = 需要跑 [`Db::apply_v2_data`]）。
    pub fn needs_v2_data(&self) -> Result<bool> {
        Ok(!self.data_migration_done(DATA_V2_ROOTS_FOLDERS)?)
    }

    pub(crate) fn data_migration_done(&self, name: &str) -> Result<bool> {
        let hit: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM data_migrations WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    /// v2 数据步骤：roots 迁入 + 文件夹层级推导 + 尺寸状态初始化。**幂等**。
    ///
    /// `seeds` 由 core 从 `Preferences.roots` 读出来（本 crate 是叶子，不认识偏好文件）。
    /// 已经跑过就直接返回 `applied = false`，不会重复覆盖用户后来手工加的根目录。
    pub fn apply_v2_data(&self, seeds: &[RootSeed]) -> Result<V2DataReport> {
        if !self.needs_v2_data()? {
            return Ok(V2DataReport::default());
        }
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match self.apply_v2_data_inner(seeds) {
            Ok(rep) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(rep)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn apply_v2_data_inner(&self, seeds: &[RootSeed]) -> Result<V2DataReport> {
        let mut rep = V2DataReport {
            applied: true,
            ..V2DataReport::default()
        };

        // 1) orientation 空值归一。**全库唯一一处「改写已有行」**（`docs/05` §1.3.1）。
        //    理由：生成列的 ELSE 分支虽能兜住 NULL，但 M4 的旋转叠加要拿 orientation 做算术，
        //    NULL 会一路传染。只碰 NULL 行，且幂等。
        let blanked = self.conn.execute(
            "UPDATE photos SET orientation = 1 WHERE orientation IS NULL",
            [],
        )?;
        if blanked > 0 {
            tracing::info!(rows = blanked, "orientation 空值归一到 1");
        }

        // 2) 尺寸状态初始化。
        //    ⚠️ 与 `docs/05` §1.4 第 6 步的字面写法（全部置 0）有一处**有意偏差**：
        //    已有宽高的照片直接标 1（已完成）。否则一个 1150 张、其中 1089 张宽高已知的库，
        //    会凭空多出 1089 条「待回填」——进度与诊断页全是假 backlog。
        //    语义没变：**没有宽高的才是待回填**。
        rep.photos_stamped = self.conn.execute(
            "UPDATE photos SET dims_status =
                 CASE WHEN width IS NOT NULL AND height IS NOT NULL THEN 1 ELSE 0 END",
            [],
        )? as u64;

        // 3) roots 迁入（Preferences → DB）。同路径已存在（closed）则置回 active、复用 id。
        for seed in seeds {
            self.upsert_root_seed(seed)?;
            rep.roots += 1;
        }

        // 4) 文件夹层级推导
        let (created, linked) = self.derive_hierarchy()?;
        rep.folders_created = created;
        rep.folders_linked = linked;

        // 5) 记一笔，之后不再跑
        self.conn.execute(
            "INSERT OR REPLACE INTO data_migrations (name, applied_at)
             VALUES (?1, strftime('%s','now'))",
            params![DATA_V2_ROOTS_FOLDERS],
        )?;
        Ok(rep)
    }

    fn upsert_root_seed(&self, seed: &RootSeed) -> Result<i64> {
        let ci = seed.case_insensitive.unwrap_or(true);
        self.conn.execute(
            "INSERT INTO roots (path, path_key, state, added_at, case_insensitive)
             VALUES (?1, ?2, 0, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                 state = 0,
                 closed_at = NULL",
            params![
                seed.path,
                seed.path_key,
                seed.added_at,
                if ci { 1 } else { 0 }
            ],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM roots WHERE path = ?1",
            params![seed.path],
            |r| r.get(0),
        )?)
    }

    /// 重建 `folders` 的 `parent_id` / `root_id` / `name` / `direct_count` / `has_children`。
    ///
    /// 返回 `(补出来的目录数, 回写的目录数)`。
    ///
    /// 用**读一遍 → 在 Rust 里算 → 回写**，而不是纯 SQL：SQLite 没有 Unicode 归一化函数，
    /// `substr`/`instr` 拿到的仍是原样字节，NFD 目录名永远匹配不上 NFC 记录
    /// （`docs/05` §1.4.2 已把初稿「用 SQL 推导」判为不成立）。
    pub(crate) fn derive_hierarchy(&self) -> Result<(usize, usize)> {
        // ---- 1. 先把 folders.path 统一到 NFC（只改编码，不改语义）
        let raw: Vec<(i64, String)> = {
            let mut st = self.conn.prepare("SELECT id, path FROM folders")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (id, path) in &raw {
            let normalized = nfc(path);
            if &normalized == path {
                continue;
            }
            match self.conn.execute(
                "UPDATE folders SET path = ?2, path_key = ?3 WHERE id = ?1",
                params![id, normalized, compare_key(&normalized)],
            ) {
                Ok(_) => tracing::info!(folder_id = id, "目录名归一为 NFC"),
                // 两条记录归一后撞 UNIQUE：保留原样交给诊断页，绝不在迁移里猜
                Err(e) => tracing::warn!(
                    folder_id = id,
                    error = %e,
                    "NFC 归一后与已有目录冲突，保留原路径"
                ),
            }
        }

        // ---- 2. 读 roots（长路径优先，便于「最长前缀」匹配）
        let root_norms: Vec<(i64, String)> = {
            let mut st = self.conn.prepare("SELECT id, path FROM roots")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            let mut v = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            // 长路径优先：最长前缀匹配才能把 `/photos/sub` 归到 `/photos/sub` 而不是 `/photos`
            v.sort_by_key(|(_, p): &(i64, String)| std::cmp::Reverse(p.len()));
            v.into_iter().map(|(id, p)| (id, nfc(&p))).collect()
        };

        // ---- 3. 现有目录索引
        let mut entries: Vec<(i64, String)> = {
            let mut st = self.conn.prepare("SELECT id, path FROM folders")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut by_path: HashMap<String, i64> =
            entries.iter().map(|(id, p)| (p.clone(), *id)).collect();

        // ---- 4. 根目录自身必须有行（关注了但还没扫过的目录，也要在树里可见可点）
        let mut created = 0usize;
        for (_, root_path) in &root_norms {
            if by_path.contains_key(root_path.as_str()) {
                continue;
            }
            let id = self.insert_folder_row(root_path)?;
            by_path.insert(root_path.clone(), id);
            entries.push((id, root_path.clone()));
            created += 1;
        }

        // ---- 5. 补中间层：`2020/01/P` 有照片、`2020/01` 没照片时，
        //         不补这一层树就断了（父链指向一个不存在的节点）。
        let mut missing: BTreeSet<String> = BTreeSet::new();
        for (_, path) in &entries {
            let Some((_, root_path)) = owner_root(path.as_str(), &root_norms) else {
                continue;
            };
            let mut anc = parent_of(path);
            while let Some(a) = anc {
                // 只补**根目录以内**的层级；根之外的父目录不是我们的数据
                if a != *root_path && !is_under(&a, root_path) {
                    break;
                }
                if !by_path.contains_key(&a) {
                    missing.insert(a.clone());
                }
                if a == *root_path {
                    break;
                }
                anc = parent_of(&a);
            }
        }
        for path in missing {
            if by_path.contains_key(&path) {
                continue;
            }
            let id = self.insert_folder_row(&path)?;
            by_path.insert(path.clone(), id);
            entries.push((id, path));
            created += 1;
        }

        // ---- 6. 逐行回写层级字段
        let mut linked = 0usize;
        for (id, path) in &entries {
            let root = owner_root(path, &root_norms).map(|(rid, _)| rid);
            let parent_id = parent_of(path).and_then(|p| by_path.get(&p).copied());
            // 幂等手段：已推导过的行不动（`COALESCE` 优先保留旧值）。
            // 这样「回退到 M2 再升回来」只补新增部分，不重算已有。
            linked += self.conn.execute(
                "UPDATE folders SET
                     root_id   = COALESCE(?2, root_id),
                     name      = COALESCE(name, ?3),
                     parent_id = COALESCE(parent_id, ?4),
                     path_key  = ?5
                 WHERE id = ?1",
                params![id, root, base_name(path), parent_id, compare_key(path)],
            )?;
        }

        // ---- 7. 计数（物理事实，不受关闭状态影响 —— P-07）
        self.conn.execute(
            "UPDATE folders SET direct_count =
                 (SELECT COUNT(*) FROM photos p WHERE p.folder_id = folders.id)",
            [],
        )?;
        self.conn.execute(
            "UPDATE folders SET has_children =
                 EXISTS (SELECT 1 FROM folders c WHERE c.parent_id = folders.id)",
            [],
        )?;
        Ok((created, linked))
    }

    fn insert_folder_row(&self, path: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO folders (path, path_key, is_online)
             VALUES (?1, ?2, 1)
             ON CONFLICT(path) DO NOTHING",
            params![path, compare_key(path)],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM folders WHERE path = ?1",
            params![path],
            |r| r.get(0),
        )?)
    }

    /// 树的直接子节点（`WHERE parent_id = ?`，O(直接子节点数)，不全表扫）。
    pub fn children_of(&self, parent_id: Option<i64>) -> Result<Vec<FolderTreeRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, path, name, parent_id, root_id, direct_count, has_children,
                    index_status, is_online
             FROM folders WHERE parent_id IS ?1 ORDER BY COALESCE(name, path)",
        )?;
        let rows = st.query_map(params![parent_id], map_tree_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 一个根目录下的全部节点（导航一次取齐再在内存建树）。
    pub fn folders_of_root(&self, root_id: i64) -> Result<Vec<FolderTreeRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, path, name, parent_id, root_id, direct_count, has_children,
                    index_status, is_online
             FROM folders WHERE root_id = ?1 ORDER BY path",
        )?;
        let rows = st.query_map(params![root_id], map_tree_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 全量重建层级：扫描完一个关注文件夹后调用，让 `direct_count` / `has_children` 跟上。
    ///
    /// 代价是读一遍 folders（几千行级别，毫秒量级）；比"增量维护计数"少一整类 bug。
    pub fn refresh_hierarchy(&self) -> Result<(usize, usize)> {
        self.derive_hierarchy()
    }
}

fn map_tree_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FolderTreeRow> {
    Ok(FolderTreeRow {
        id: r.get(0)?,
        path: r.get(1)?,
        name: r.get(2)?,
        parent_id: r.get(3)?,
        root_id: r.get(4)?,
        direct_count: r.get(5)?,
        has_children: r.get::<_, i64>(6)? != 0,
        index_status: r.get(7)?,
        is_online: r.get::<_, i64>(8)? != 0,
    })
}

/// 归属根目录（最长前缀）。
fn owner_root<'a>(path: &str, roots: &'a [(i64, String)]) -> Option<(i64, &'a str)> {
    roots
        .iter()
        .find(|(_, r)| path == r.as_str() || is_under(path, r))
        .map(|(id, r)| (*id, r.as_str()))
}

// ------------------------------------------------------------ 升级主流程

/// 在**打开长连接之前**把文件升到当前版本：备份 → 事务 DDL → 失败恢复。
///
/// 单独用一个短连接做这件事，是为了让「失败恢复」可以安全地覆盖数据库文件
/// （连接还开着的时候覆盖一个 WAL 库太容易出人命）。
pub(crate) fn upgrade_file(path: &Path) -> Result<UpgradeReport> {
    let mut report = UpgradeReport::default();
    if !path.exists() {
        // 新库：没有旧数据可备份，交给 `migrate()` 从零建。
        return Ok(report);
    }

    let conn = Connection::open(path)?;
    let current = read_version(&conn)?;
    report.from = current;
    report.to = current;
    if current > SCHEMA_VERSION {
        return Err(DbError::SchemaTooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    if current == 0 || current >= SCHEMA_VERSION {
        // 0：空文件 / 只有非本程序的表，交给 migrate 建全套
        return Ok(report);
    }

    // 1) 备份（失败即中止升级，原库一个字节不动）
    let backup = next_backup_path(&conn, path, current)?;
    conn.execute("VACUUM INTO ?1", params![backup.to_string_lossy()])?;
    tracing::info!(
        from = current,
        to = SCHEMA_VERSION,
        file = %backup.display(),
        "已备份索引库"
    );
    report.backup = Some(backup.clone());
    report.to = SCHEMA_VERSION;

    // 2) 单事务 DDL
    if let Err(e) = apply_pending(&conn, current) {
        tracing::error!(error = %e, "迁移失败，回滚并恢复备份");
        report.to = current;
        drop(conn);
        match std::fs::copy(&backup, path) {
            Ok(_) => report.restored_from_backup = true,
            Err(re) => tracing::error!(error = %re, "用备份恢复失败，需要人工处理"),
        }
        // 恢复是第二层保险：DDL 本身在事务里，SQLite 已经回滚过一次。
        // 错误留在现场让上层提示，**不拒绝启动**（以 M2 数据继续跑）。
        report.error = Some(e.to_string());
        return Ok(report);
    }
    Ok(report)
}

fn read_version(conn: &Connection) -> Result<i64> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(0);
    }
    let v: Option<i64> = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    Ok(v.unwrap_or(0))
}

/// 把 `current` 之后的迁移在**一个事务**里跑完。失败整体回滚。
pub(crate) fn apply_pending(conn: &Connection, current: i64) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let mut failure: Option<DbError> = None;
    for m in MIGRATIONS.iter().filter(|m| m.version > current) {
        let step = conn
            .execute_batch(m.sql)
            .map_err(DbError::from)
            .and_then(|()| {
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at)
                     VALUES (?1, strftime('%s','now'))",
                    params![m.version],
                )
                .map(|_| ())
                .map_err(DbError::from)
            });
        if let Err(e) = step {
            failure = Some(e);
            break;
        }
    }
    match failure {
        Some(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
        None => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
    }
}

fn next_backup_path(conn: &Connection, db_path: &Path, from: i64) -> Result<PathBuf> {
    let dir = Db::backup_dir(db_path);
    std::fs::create_dir_all(&dir)?;
    // 用 SQLite 的时间函数取 UTC 时间戳：本 crate 不引 chrono，
    // 而 `SystemTime` 被 clippy.toml 禁了（跨平台序列化语义不稳）。
    let stamp: String =
        conn.query_row("SELECT strftime('%Y%m%dT%H%M%SZ','now')", [], |r| r.get(0))?;
    let mut candidate = dir.join(format!("openpicasa-v{from}-{stamp}.db"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("openpicasa-v{from}-{stamp}-{n}.db"));
        n += 1;
    }
    Ok(candidate)
}
