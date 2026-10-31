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
use crate::{Db, ExclusionRow, Result, RootRow};
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
        Ok(self
            .conn
            .query_row("SELECT id FROM roots WHERE path = ?1", params![path], |r| {
                r.get(0)
            })?)
    }

    /// 根目录列表。`include_closed = false` 时只返回 active（树用这个）。
    pub fn roots(&self, include_closed: bool) -> Result<Vec<RootRow>> {
        let sql = if include_closed {
            "SELECT id, path, path_key, state, added_at, closed_at, case_insensitive
             FROM roots ORDER BY path"
        } else {
            "SELECT id, path, path_key, state, added_at, closed_at, case_insensitive
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
                "SELECT id, path, path_key, state, added_at, closed_at, case_insensitive
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
                "SELECT id, path, path_key, state, added_at, closed_at, case_insensitive
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
    })
}
