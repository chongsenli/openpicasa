//! 索引任务与历史失败记录（`index_tasks` / `index_task_failures`）。
//!
//! ## 真相源边界（必须遵守）
//!
//! **实时进度**以内存里的 `IndexBoard` 为准（`docs/04` §3.2：弹框、面板、左栏、栅格
//! 四处读同一份快照）。这里只存**历史**：某个任务什么时候跑、结局如何、失败了哪些文件。
//! DB 不参与实时百分比计算——跨过去就退化成「两个计数器」，那是 04 判定的 P0。
//!
//! ## 失败按条目计，不按尝试次数计
//!
//! `index_task_failures` 的主键是 `(task_id, relative_path)`：同一条目重试三次仍然只有一行，
//! `attempts` 累加。否则面板会把「重试 3 次」显示成「3 个失败文件」。

use crate::schema::{TaskKind, TaskStatus};
use crate::{Db, Result, TaskRow};
use rusqlite::{OptionalExtension, params};

impl Db {
    /// 建一条任务记录（入队时）。返回 `task_id`。
    pub fn create_task(
        &self,
        root_id: Option<i64>,
        scope_path: &str,
        kind: TaskKind,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO index_tasks (root_id, scope_path, kind, status, started_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%s','now'))",
            params![root_id, scope_path, kind.code(), TaskStatus::Waiting.code()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 状态推进。终态时补 `finished_at`。
    pub fn set_task_status(
        &self,
        task_id: i64,
        status: TaskStatus,
        error_code: Option<&str>,
    ) -> Result<()> {
        let finished = if status == TaskStatus::Completed
            || status == TaskStatus::PartialFailed
            || status == TaskStatus::Unreachable
        {
            "strftime('%s','now')"
        } else {
            "NULL"
        };
        self.conn.execute(
            &format!(
                "UPDATE index_tasks
                 SET status = ?2, error_code = ?3, finished_at = {finished}
                 WHERE task_id = ?1"
            ),
            params![task_id, status.code(), error_code],
        )?;
        Ok(())
    }

    /// 记一条失败。同条目重复失败只累加 `attempts`。
    pub fn record_task_failure(
        &self,
        task_id: i64,
        relative_path: &str,
        error_code: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO index_task_failures
                 (task_id, relative_path, error_code, attempts, updated_at)
             VALUES (?1, ?2, ?3, 1, strftime('%s','now'))
             ON CONFLICT(task_id, relative_path) DO UPDATE SET
                 error_code = excluded.error_code,
                 attempts   = index_task_failures.attempts + 1,
                 updated_at = excluded.updated_at",
            params![task_id, relative_path, error_code],
        )?;
        Ok(())
    }

    /// 一个任务的失败明细：`(relative_path, error_code, attempts)`。
    pub fn task_failures(&self, task_id: i64) -> Result<Vec<(String, String, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT relative_path, error_code, attempts
             FROM index_task_failures WHERE task_id = ?1
             ORDER BY relative_path",
        )?;
        let rows = st.query_map(params![task_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 最近的任务（诊断页用），按 `task_id` 倒序。
    pub fn recent_tasks(&self, limit: usize) -> Result<Vec<TaskRow>> {
        let mut st = self.conn.prepare(
            "SELECT task_id, root_id, scope_path, kind, status, started_at, finished_at, error_code
             FROM index_tasks ORDER BY task_id DESC LIMIT ?1",
        )?;
        let rows = st.query_map(params![limit as i64], |r| {
            Ok(TaskRow {
                task_id: r.get(0)?,
                root_id: r.get(1)?,
                scope_path: r.get(2)?,
                kind: r.get(3)?,
                status: r.get(4)?,
                started_at: r.get(5)?,
                finished_at: r.get(6)?,
                error_code: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 某个任务的任务行（测试与「恢复未完成任务」用）。
    pub fn task(&self, task_id: i64) -> Result<Option<TaskRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT task_id, root_id, scope_path, kind, status, started_at, finished_at,
                        error_code
                 FROM index_tasks WHERE task_id = ?1",
                params![task_id],
                |r| {
                    Ok(TaskRow {
                        task_id: r.get(0)?,
                        root_id: r.get(1)?,
                        scope_path: r.get(2)?,
                        kind: r.get(3)?,
                        status: r.get(4)?,
                        started_at: r.get(5)?,
                        finished_at: r.get(6)?,
                        error_code: r.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    /// 上次退出时还没跑完的任务（`kind = Resume` 的恢复入口，见 `docs/04` P-02 第 3 类批次）。
    pub fn unfinished_tasks(&self) -> Result<Vec<TaskRow>> {
        let mut st = self.conn.prepare(
            "SELECT task_id, root_id, scope_path, kind, status, started_at, finished_at, error_code
             FROM index_tasks
             WHERE status IN (0, 1, 2, 5)
             ORDER BY task_id",
        )?;
        let rows = st.query_map([], |r| {
            Ok(TaskRow {
                task_id: r.get(0)?,
                root_id: r.get(1)?,
                scope_path: r.get(2)?,
                kind: r.get(3)?,
                status: r.get(4)?,
                started_at: r.get(5)?,
                finished_at: r.get(6)?,
                error_code: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}
