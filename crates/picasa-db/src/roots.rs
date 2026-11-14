//! 关注根目录与排除项（`roots` / `exclusions`）。
//!
//! ## 单一真相源
//!
//! 迁入之后 `roots` 表说了算。`Preferences.roots` 降级为**只读兼容字段**：
//! 只在迁移时当种子读一次，之后任何「加/关/重开根目录」都走这里。
//! 两处都写就是双真相源，`docs/04` §3.2 按 P0 处理。
//!
//! ## 关闭 ≠ 删除
//!
//! `state = 1 (closed)` 只是「树里不显示」，照片、缩略图、`direct_count` 原样保留
//! （P-07：`direct_count` 是物理事实）。重新添加同一路径时匹配到 closed 记录就置回
//! active、复用 `root_id`，排除记录也原样沿用——这就是「再次添加秒开」。

use crate::pathutil::{compare_key, nfc};
use crate::{Db, ExclusionRow, Result, RootRow, RootWatchRow};
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

impl Db {
    /// 添加（或重开）一个根目录，返回 `root_id`。
    ///
    /// 同路径已存在时**只置回 active**，不动 `added_at`（那是首次加入的时间）。
    pub fn upsert_root(&self, path: &str, added_at: i64, case_insensitive: bool) -> Result<i64> {
        let path = nfc(path);
        self.conn.execute(
            "INSERT INTO roots (path, path_key, state, added_at, case_insensitive)
             VALUES (?1, ?2, 0, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                 state = 0,
                 closed_at = NULL",
            params![
                path,
                compare_key(&path),
                added_at,
                if case_insensitive { 1 } else { 0 }
            ],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM roots WHERE path = ?1",
            params![path],
            |r| r.get(0),
        )?;
        // 重新添加同路径 = 重新建立一个可索引的根，之前「索引已移除」的
        // 呈现标记必须一起清掉（`mark_root_index_removed` 的注释解释了它为什么
        // 是独立于 `state` 的一位）。放这里是为了让「加根」只有一条路径可信，
        // 不依赖每个调用点各自记得清。
        let _ = self.clear_root_index_removed(id);
        Ok(id)
    }

    /// 根目录列表。`include_closed = false` 时只返回 active（树用这个）。
    pub fn roots(&self, include_closed: bool) -> Result<Vec<RootRow>> {
        let sql = if include_closed {
            "SELECT id, path, path_key, state, added_at, closed_at, case_insensitive,
                    origin, import_source_id
             FROM roots ORDER BY path"
        } else {
            "SELECT id, path, path_key, state, added_at, closed_at, case_insensitive,
                    origin, import_source_id
             FROM roots WHERE state = 0 ORDER BY path"
        };
        let mut st = self.conn.prepare(sql)?;
        let rows = st.query_map([], map_root)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn root(&self, id: i64) -> Result<Option<RootRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, path, path_key, state, added_at, closed_at, case_insensitive,
                        origin, import_source_id
                 FROM roots WHERE id = ?1",
                params![id],
                map_root,
            )
            .optional()?)
    }

    pub fn root_by_path(&self, path: &str) -> Result<Option<RootRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, path, path_key, state, added_at, closed_at, case_insensitive,
                        origin, import_source_id
                 FROM roots WHERE path = ?1",
                params![nfc(path)],
                map_root,
            )
            .optional()?)
    }

    /// 关闭 / 重开一个根目录。**不删任何数据**（照片、缩略图、排除项都留着）。
    pub fn set_root_closed(&self, id: i64, closed: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE roots SET state = ?2,
                              closed_at = CASE WHEN ?2 = 1 THEN strftime('%s','now') ELSE NULL END
             WHERE id = ?1",
            params![id, if closed { 1 } else { 0 }],
        )?;
        Ok(())
    }

    /// 该卷不区分大小写时，排除项的比较键要小写；否则保留大小写。
    pub fn match_key_for(&self, root_id: i64, relative_path: &str) -> Result<String> {
        let ci: i64 = self
            .conn
            .query_row(
                "SELECT case_insensitive FROM roots WHERE id = ?1",
                params![root_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(1);
        let key = nfc(relative_path);
        Ok(if ci != 0 { key.to_lowercase() } else { key })
    }

    /// 加一条排除项（`relative_path` 相对根目录）。重复添加不报错，返回已有 id。
    pub fn add_exclusion(&self, root_id: i64, relative_path: &str) -> Result<i64> {
        let rel = nfc(relative_path);
        let key = self.match_key_for(root_id, &rel)?;
        self.conn.execute(
            "INSERT INTO exclusions (root_id, relative_path, match_key, created_at, stale)
             VALUES (?1, ?2, ?3, strftime('%s','now'), 0)
             ON CONFLICT(root_id, match_key) DO UPDATE SET
                 relative_path = excluded.relative_path,
                 stale = 0",
            params![root_id, rel, key],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM exclusions WHERE root_id = ?1 AND match_key = ?2",
            params![root_id, key],
            |r| r.get(0),
        )?)
    }

    pub fn exclusions(&self, root_id: i64) -> Result<Vec<ExclusionRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, root_id, relative_path, match_key, created_at, stale
             FROM exclusions WHERE root_id = ?1 ORDER BY relative_path",
        )?;
        let rows = st.query_map(params![root_id], |r| {
            Ok(ExclusionRow {
                id: r.get(0)?,
                root_id: r.get(1)?,
                relative_path: r.get(2)?,
                match_key: r.get(3)?,
                created_at: r.get(4)?,
                stale: r.get::<_, i64>(5)? != 0,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 全量排除项（跨所有根，M2.2 §4.1「排除项」标签用）。
    ///
    /// `stale` 是**库里记的**上次判定结果；M2.2 §4.4 要求的「路径已不存在」是
    /// **当下**的判定（`path_missing`），由 core 的 [`crate::MgmtSnapshot`] 生成时
    /// 现算——库里的值可能过期（盘拔了又插回来），不能拿它当角标依据。
    pub fn all_exclusions(&self) -> Result<Vec<ExclusionRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, root_id, relative_path, match_key, created_at, stale
             FROM exclusions ORDER BY root_id, relative_path",
        )?;
        let rows = st.query_map([], |r| {
            Ok(ExclusionRow {
                id: r.get(0)?,
                root_id: r.get(1)?,
                relative_path: r.get(2)?,
                match_key: r.get(3)?,
                created_at: r.get(4)?,
                stale: r.get::<_, i64>(5)? != 0,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 恢复（删除）一条排除项。
    pub fn remove_exclusion(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM exclusions WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// 扫描时用的内存集合：进入目录时判定一次，命中就跳过整棵子树
    /// （`docs/05` §1.5：不逐文件判断）。
    pub fn exclusion_keys(&self, root_id: i64) -> Result<HashSet<String>> {
        let mut st = self
            .conn
            .prepare("SELECT match_key FROM exclusions WHERE root_id = ?1")?;
        let rows = st.query_map(params![root_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<HashSet<_>, _>>()?)
    }
}

fn map_root(r: &rusqlite::Row<'_>) -> rusqlite::Result<RootRow> {
    Ok(RootRow {
        id: r.get(0)?,
        path: r.get(1)?,
        path_key: r.get(2)?,
        state: r.get(3)?,
        added_at: r.get(4)?,
        closed_at: r.get(5)?,
        case_insensitive: r.get::<_, i64>(6)? != 0,
        origin: r.get(7)?,
        import_source_id: r.get(8)?,
    })
}

impl Db {
    /// 每个关注根的监视配置（`07` §8.1 的 `roots.watch_enabled` / `poll_interval_s`）。
    ///
    /// **只读用户意图**：运行时状态（装没装上、是不是网络卷）不入库。
    pub fn root_watch_configs(&self) -> Result<Vec<RootWatchRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, path, watch_enabled, poll_interval_s
               FROM roots WHERE state = 0 ORDER BY path",
        )?;
        let rows = st.query_map([], |r| {
            Ok(RootWatchRow {
                root_id: r.get::<_, i64>(0)?,
                path: r.get::<_, String>(1)?,
                enabled: r.get::<_, i64>(2)? != 0,
                poll_interval_s: r.get::<_, Option<i64>>(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 设置某个根的实时监视开关（WATCH-12）。
    pub fn set_root_watch_enabled(&self, root_id: i64, enabled: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE roots SET watch_enabled = ?2 WHERE id = ?1",
            rusqlite::params![root_id, if enabled { 1 } else { 0 }],
        )?;
        Ok(())
    }

    /// 设置某个根的轮询间隔（秒）。`None` = 用默认值。
    pub fn set_root_poll_interval(&self, root_id: i64, seconds: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE roots SET poll_interval_s = ?2 WHERE id = ?1",
            rusqlite::params![root_id, seconds],
        )?;
        Ok(())
    }

    /// 彻底删掉一个关注根（IDX-21 ⑤：导入来源整组移除时用）。
    ///
    /// 只删 `roots` 行本身——`folders`/`photos` 必须已经由
    /// `clear_index_for_root` 清干净（它负责标记快照与作废任务意图）。
    /// **不碰磁盘**：只动自有 DB。
    pub fn delete_root_row(&self, id: i64) -> Result<()> {
        let _ = self.set_setting_raw(&index_removed_key(id), None);
        self.conn
            .execute("DELETE FROM exclusions WHERE root_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM roots WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// 标记「这个根的索引已被用户移除」（IDX-10 / IDX-11）。
    ///
    /// ## 为什么另开一个标记，而不是复用 `state`
    ///
    /// `roots.state` 只有 0/1 两态，且 1 = **用户主动关闭**（`close_root`，
    /// 索引原样保留，随时可重开）。移除索引是另一件事：索引真的没了，
    /// 用户要能一眼看出「这行是删过索引的空壳」而不是「我不小心关掉了」。
    /// 复用同一位会让这两种状态在界面上不可区分，也会让「关闭但保留索引」
    /// 从此无法自证。
    ///
    /// 用 `settings["m43_index_removed_<id>"]` 而不是加列：这一位是
    /// **纯 UI 呈现辅助**，查询侧一条都不依赖它；为它做一次 v9→v10 迁移
    /// （全库 rebuild 表）不划算，也与既有 `m43_import_hidden_<id>` 的
    /// 范式一致。
    pub fn mark_root_index_removed(&self, id: i64) -> Result<()> {
        self.set_setting_raw(&index_removed_key(id), Some("1"))
    }

    /// 清掉「索引已移除」标记（重新索引 / 重新添加同路径时调）。
    pub fn clear_root_index_removed(&self, id: i64) -> Result<()> {
        self.set_setting_raw(&index_removed_key(id), None)
    }

    /// 该根是否处于「索引已移除」状态。
    pub fn root_index_removed(&self, id: i64) -> Result<bool> {
        self.setting_bool(&index_removed_key(id), false)
    }

    /// 所有「索引已移除」的根 id（快照构建时一次读出来，别每行查一次）。
    pub fn index_removed_root_ids(&self) -> Result<Vec<i64>> {
        let mut st = self
            .conn
            .prepare("SELECT key FROM settings WHERE key LIKE 'm43_index_removed_%' AND value = '1'")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for key in rows {
            let key = key?;
            if let Some(rest) = key.strip_prefix("m43_index_removed_")
                && let Ok(id) = rest.parse::<i64>()
            {
                out.push(id);
            }
        }
        Ok(out)
    }
}

/// `settings` 里「该根索引已移除」的键名（与 `m43_import_hidden_<id>` 同范式）。
fn index_removed_key(root_id: i64) -> String {
    format!("m43_index_removed_{root_id}")
}
