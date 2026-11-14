//! 「导入的索引」来源与批次（M4.3 IDX-21 / §3.1）。
//!
//! ## 为什么要有这一层
//!
//! 导入进来的照片索引和本机扫描出来的索引，在 `folders` / `photos` 里长得一样。
//! 但侧边栏要把它们**分成两个组**（P-M43-09：每个导入来源一个节点），移除流程也
//! 只能作用于「这个来源带进来的 roots/folders」，不能碰用户本机自己加的目录。
//! 所以需要一条从「来源」到「根目录」的稳定链：
//!
//! ```text
//! import_sources 1 ──< import_batches ──< import_batch_members (photo_id)
//!       1 ──────────────< roots.import_source_id
//! ```
//!
//! ## 与本机目录的关系（P-M43-10 建议默认）
//!
//! 同一路径在本机也存在时**以本机为准**：`roots` 按自然键（path）复用既有行，
//! `origin` 保持 `local`，不挂 `import_source_id`——UI 上标「本机已有」，计数只算一份。
//! 只有「本次导入新带进来的根」才写成 `origin = 'imported'`。
//!
//! ## 红线
//!
//! 本模块只读写自有 DB，**绝不建目录、不碰原图**（R-M43-01）。

use crate::{Db, Result};
use rusqlite::{OptionalExtension, Transaction, params};

/// 一个导入来源（`import_sources` 的一行）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportSourceRow {
    pub id: i64,
    pub name: String,
    /// 导出包所在路径（合并导入次数多了也只有一个来源）。
    pub source_path: Option<String>,
    pub created_at: i64,
}

/// 一次导入（`import_batches` 的一行）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportBatchRow {
    pub id: i64,
    pub source_id: Option<i64>,
    pub label: String,
    pub created_at: i64,
    /// `merge` / `replace`（`ImportStrategy` 的字符串形式）。
    pub strategy: String,
}

fn map_source(r: &rusqlite::Row<'_>) -> rusqlite::Result<ImportSourceRow> {
    Ok(ImportSourceRow {
        id: r.get(0)?,
        name: r.get(1)?,
        source_path: r.get(2)?,
        created_at: r.get(3)?,
    })
}

fn map_batch(r: &rusqlite::Row<'_>) -> rusqlite::Result<ImportBatchRow> {
    Ok(ImportBatchRow {
        id: r.get(0)?,
        source_id: r.get(1)?,
        label: r.get(2)?,
        created_at: r.get(3)?,
        strategy: r.get(4)?,
    })
}

impl Db {
    /// 全部导入来源，按导入时间倒序（侧边栏从上往下 = 从新到旧）。
    pub fn import_sources(&self) -> Result<Vec<ImportSourceRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, name, source_path, created_at FROM import_sources
              ORDER BY created_at DESC, id DESC",
        )?;
        let rows = st.query_map([], map_source)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn import_source(&self, id: i64) -> Result<Option<ImportSourceRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name, source_path, created_at FROM import_sources WHERE id = ?1",
                params![id],
                map_source,
            )
            .optional()?)
    }

    /// 某个来源的全部批次（新的在前）。
    pub fn import_batches(&self, source_id: i64) -> Result<Vec<ImportBatchRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, source_id, label, created_at, strategy FROM import_batches
              WHERE source_id = ?1 ORDER BY created_at DESC, id DESC",
        )?;
        let rows = st.query_map(params![source_id], map_batch)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 某个来源带进来的根目录（`origin = 'imported'` 且挂在该来源下）。
    ///
    /// 返回 `(root_id, path, 是否在线, 照片数)`；`是否在线` 取该根所有文件夹的
    /// `is_online` 全真，照片数按 `direct_count` 求和（与 `root_stats` 同口径）。
    pub fn imported_roots(&self, source_id: i64) -> Result<Vec<(i64, String, bool, u64)>> {
        let mut st = self.conn.prepare(
            "SELECT r.id, r.path,
                    COALESCE((SELECT MIN(f.is_online) FROM folders f WHERE f.root_id = r.id), 1),
                    COALESCE((SELECT SUM(f.direct_count) FROM folders f WHERE f.root_id = r.id), 0)
               FROM roots r
              WHERE r.import_source_id = ?1 AND r.origin = 'imported' AND r.state = 0
              ORDER BY r.path",
        )?;
        let rows = st.query_map(params![source_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? != 0,
                r.get::<_, i64>(3)?.max(0) as u64,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 某来源在库里的照片总数（按 batch 成员去重，跨批次只算一份）。
    pub fn import_source_photo_count(&self, source_id: i64) -> Result<u64> {
        Ok(self
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT m.photo_id) FROM import_batch_members m
                   JOIN import_batches b ON b.id = m.batch_id
                  WHERE b.source_id = ?1",
                params![source_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            .max(0) as u64)
    }

    /// 某来源带进来的根目录 id（移除流程用）。
    pub fn imported_root_ids(&self, source_id: i64) -> Result<Vec<i64>> {
        let mut st = self.conn.prepare(
            "SELECT id FROM roots WHERE import_source_id = ?1 AND origin = 'imported'",
        )?;
        let rows = st.query_map(params![source_id], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 「仅从分组隐藏」：把来源节点藏起来，**不动任何索引与标记**。
    pub fn set_import_source_hidden(&self, source_id: i64, hidden: bool) -> Result<()> {
        // 用批次表记不可行（批次是复数）；直接用一个 `label` 前缀标记太脏。
        // 走 settings：每个来源一行 `m43_import_hidden_<id>`。
        self.set_setting_raw(
            &format!("m43_import_hidden_{source_id}"),
            if hidden { Some("1") } else { None },
        )
    }

    pub fn import_source_hidden(&self, source_id: i64) -> Result<bool> {
        Ok(self
            .setting_raw(&format!("m43_import_hidden_{source_id}"))?
            .as_deref()
            == Some("1"))
    }
}

/// 在一次导入事务里记录来源 / 批次 / 成员，并把新带进来的根标成 `origin = 'imported'`。
///
/// * `roots_added`：本次导入**新增**的 root_id（调用方在复制前后比 id 集合得到）。
/// * `photo_ids`：本次导入真正落库的新照片 id（IDX-19 用它划「本次导入」scope）。
///
/// 返回 `(source_id, batch_id)`。
#[allow(clippy::too_many_arguments)]
pub fn record_import(
    tx: &Transaction<'_>,
    name: &str,
    source_path: Option<&str>,
    strategy: &str,
    label: &str,
    created_at: i64,
    roots_added: &[i64],
    photo_ids: &[i64],
) -> Result<(i64, i64)> {
    // 来源按 (name, source_path) 去重：同一个包重复导入不该长出一串同名节点
    //（P-M43-09：每个导入来源一个节点）。`source_path` 为 NULL 时按名字认。
    let existing: Option<i64> = match source_path {
        Some(sp) => tx
            .query_row(
                "SELECT id FROM import_sources WHERE source_path = ?1 ORDER BY id LIMIT 1",
                params![sp],
                |r| r.get(0),
            )
            .optional()?,
        None => tx
            .query_row(
                "SELECT id FROM import_sources WHERE source_path IS NULL AND name = ?1
                  ORDER BY id LIMIT 1",
                params![name],
                |r| r.get(0),
            )
            .optional()?,
    };
    let source_id = match existing {
        Some(id) => id,
        None => {
            tx.execute(
                "INSERT INTO import_sources (name, source_path, created_at) VALUES (?1, ?2, ?3)",
                params![name, source_path, created_at],
            )?;
            tx.last_insert_rowid()
        }
    };

    tx.execute(
        "INSERT INTO import_batches (source_id, label, created_at, strategy)
         VALUES (?1, ?2, ?3, ?4)",
        params![source_id, label, created_at, strategy],
    )?;
    let batch_id = tx.last_insert_rowid();

    {
        let mut st = tx.prepare(
            "INSERT OR IGNORE INTO import_batch_members (batch_id, photo_id) VALUES (?1, ?2)",
        )?;
        for id in photo_ids {
            st.execute(params![batch_id, id])?;
        }
    }

    for rid in roots_added {
        tx.execute(
            "UPDATE roots SET origin = 'imported', import_source_id = ?2 WHERE id = ?1",
            params![rid, source_id],
        )?;
    }

    Ok((source_id, batch_id))
}

/// IDX-21 ⑤：把一个导入来源整组移除。
///
/// 分两步，刻意**不在一个事务里包住全部**：
/// 1. 逐根走 `clear_index_for_root`（整子树标记快照 + 作用域删 + 作废任务意图），
///    这是保住 R-M43-02 的唯一路径；
/// 2. 再清来源/批次/成员与「仅从分组隐藏」的偏好键。
///
/// 只读写自有 DB，**从不触碰原文件、不建/删任何目录**（红线 R-M43-01）。
pub fn remove_import_source(db: &Db, source_id: i64) -> crate::Result<(u64, u64)> {
    let mut photos = 0u64;
    let mut thumbs = 0u64;
    for root_id in db.imported_root_ids(source_id)? {
        let (p, t) = db.clear_index_for_root(root_id)?;
        photos += p;
        thumbs += t;
        // 根行本身也要走「移除」而不是「保留空壳」：`clear_index_for_root`
        // 只清索引、保留 roots 行（关闭关注那套语义），这里把导入带进来的根
        // 彻底从关注列表拿掉，避免重扫时又把它们扫回来。
        let _ = db.delete_root_row(root_id);
    }

    let tx = db.conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM import_batch_members WHERE batch_id IN \
            (SELECT id FROM import_batches WHERE source_id = ?1)",
        rusqlite::params![source_id],
    )?;
    tx.execute(
        "DELETE FROM import_batches WHERE source_id = ?1",
        rusqlite::params![source_id],
    )?;
    tx.execute(
        "DELETE FROM import_sources WHERE id = ?1",
        rusqlite::params![source_id],
    )?;
    tx.commit()?;
    let _ = db.set_setting_raw(&format!("m43_import_hidden_{source_id}"), None);
    Ok((photos, thumbs))
}

/// 收掉「已经没有任何根」的导入来源，返回被收掉的来源 id。
///
/// ## 为什么需要它（m10756 实机反馈）
///
/// 用户彻底删除一个**导入根**之后，左栏「导入的索引」里那个来源节点还在。
/// 原因不是界面的问题：来源节点是照 `import_sources` 画的，那张表里从没记过
/// 「这个来源还剩几个根」。`delete_root_row` 只删 `roots` 行，来源行原样留着，
/// 于是节点成了一个点开什么都没有的空壳——用户说的「导入的索引还在文件夹树上，
/// 没有删除，需要联动」正是这个。
///
/// 判据只看 `roots` 是否还引用它：还有根（多根来源只删了一个）就保留，
/// 一个都没了才删。绝不按「照片数」判——照片可能已被别的根复用。
///
/// 删来源时连带批次与成员：它们是同一个来源的元数据，留着只会让
/// `import_source_photo_count` 与侧栏计数对不上。**不碰磁盘**（R-M43-01）。
pub fn prune_orphan_import_sources(db: &Db) -> crate::Result<Vec<i64>> {
    let orphans: Vec<i64> = {
        let mut st = db.conn.prepare(
            "SELECT s.id FROM import_sources s
              WHERE NOT EXISTS (SELECT 1 FROM roots r WHERE r.import_source_id = s.id)",
        )?;
        let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    if orphans.is_empty() {
        return Ok(orphans);
    }
    let tx = db.conn.unchecked_transaction()?;
    for &id in &orphans {
        tx.execute(
            "DELETE FROM import_batch_members WHERE batch_id IN \
                (SELECT id FROM import_batches WHERE source_id = ?1)",
            rusqlite::params![id],
        )?;
        tx.execute(
            "DELETE FROM import_batches WHERE source_id = ?1",
            rusqlite::params![id],
        )?;
        tx.execute(
            "DELETE FROM import_sources WHERE id = ?1",
            rusqlite::params![id],
        )?;
    }
    tx.commit()?;
    // 「仅从分组隐藏」的偏好键也得清：来源都没了，键留着会成为永久的死配置。
    for &id in &orphans {
        let _ = db.set_setting_raw(&format!("m43_import_hidden_{id}"), None);
    }
    Ok(orphans)
}

/// IDX-19：「本次导入 · N 张」这个临时 scope 的照片 id 集。
///
/// 只读 `import_batch_members`；批次不存在时返回空集，调用方据此显示
/// 「这批没有照片」，**不要**退化成「不过滤」把整个图库露出来。
pub fn import_batch_photo_ids(db: &Db, batch_id: i64) -> crate::Result<Vec<i64>> {
    let mut st = db.conn.prepare(
        "SELECT photo_id FROM import_batch_members WHERE batch_id = ?1 ORDER BY photo_id",
    )?;
    let rows = st.query_map(rusqlite::params![batch_id], |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
