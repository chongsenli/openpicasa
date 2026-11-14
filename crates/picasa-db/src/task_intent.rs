//! M4.3 §2 / §3.1：索引任务的**持久意图**。
//!
//! 阶段 0 的 v9 迁移给 `index_tasks` 加了 `user_paused` / `generation` /
//! `checkpoint` / `scope_kind` / `cancel_reason` 五列；本模块是这些列的读写面，
//! 服务 IDX-07/08/09/12（暂停/继续/取消/崩溃恢复/全局暂停）。
//!
//! 与 `tasks.rs` 的分工：那边写「历史账目」（何时跑完、结局、processed/failed），
//! 这边写「用户意图」（暂停、取消、代次、续跑位置）。实时进度仍在
//! `picasa-core` 的内存 `IndexBoard`，DB 不参与百分比计算。

use crate::{Db, Result, TaskKind, TaskOutcome, TaskStatus};
use rusqlite::{OptionalExtension, params};

/// 任务范围类型（`index_tasks.scope_kind` 编码）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum TaskScopeKind {
    /// 单个关注根目录（可续跑 / 可暂停 / 可被全局取消）。
    Root = 0,
    /// 全库范围（由「全部暂停」之类入口产生，不进单根恢复队列）。
    All = 1,
}

impl TaskScopeKind {
    pub fn code(self) -> i64 {
        self as i64
    }

    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::Root,
            1 => Self::All,
            _ => return None,
        })
    }
}

/// 一条任务的持久意图快照（不依赖实时 IndexBoard）。
#[derive(Clone, Debug)]
pub struct TaskIntent {
    pub task_id: i64,
    pub root_id: Option<i64>,
    pub scope_path: String,
    /// `TaskKind` 编码
    pub kind: i64,
    /// `TaskStatus` 编码
    pub status: i64,
    pub user_paused: bool,
    pub generation: i64,
    pub checkpoint: Option<String>,
    pub scope_kind: TaskScopeKind,
    pub cancel_reason: Option<String>,
    /// `TaskOutcome` 编码；未终态为 `None`
    pub outcome: Option<i64>,
    pub processed: i64,
    pub failed: i64,
}

/// `TaskIntent` 的列清单 —— 与 [`map_intent`] 的下标一一对应。
const INTENT_COLS: &str = "task_id, root_id, scope_path, kind, status, user_paused, generation, checkpoint, scope_kind, cancel_reason, outcome, processed, failed";

fn map_intent(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskIntent> {
    Ok(TaskIntent {
        task_id: r.get(0)?,
        root_id: r.get(1)?,
        scope_path: r.get(2)?,
        kind: r.get(3)?,
        status: r.get(4)?,
        user_paused: r.get::<_, i64>(5)? != 0,
        generation: r.get(6)?,
        checkpoint: r.get(7)?,
        scope_kind: TaskScopeKind::from_code(r.get(8)?).unwrap_or(TaskScopeKind::Root),
        cancel_reason: r.get(9)?,
        outcome: r.get(10)?,
        processed: r.get(11)?,
        failed: r.get(12)?,
    })
}

impl Db {
    /// 开始（或复用）一条索引任务，返回 `task_id`。
    ///
    /// 同一 `scope_path` 已有「活着」的任务（未取消、未结束、kind ∈ 首次/刷新/重建/恢复）
    /// 时**原样复用**，不重置 `checkpoint` / `user_paused` / `status`：崩溃恢复要靠
    /// 检查点续跑，重复入队不能把它抹掉。已取消（有 `cancel_reason`）或已结束的任务
    /// 不会被复用，于是调用方拿到的是一条**新**任务。
    pub fn begin_index_task(
        &self,
        root_id: Option<i64>,
        scope_path: &str,
        kind: TaskKind,
        scope_kind: TaskScopeKind,
    ) -> Result<i64> {
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT task_id FROM index_tasks
                 WHERE scope_path = ?1
                   AND cancel_reason IS NULL
                   AND finished_at IS NULL
                   AND kind IN (0,1,2,3)
                 ORDER BY task_id DESC LIMIT 1",
                params![scope_path],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        let id = self.create_task(root_id, scope_path, kind)?;
        self.conn.execute(
            "UPDATE index_tasks SET scope_kind = ?2 WHERE task_id = ?1",
            params![id, scope_kind.code()],
        )?;
        Ok(id)
    }

    /// 把一条任务行标记为「全量重建」（IDX-14 的 force 意图持久化）。
    /// 复用 `TaskKind::Reindex`(code=2) 而非新增 schema 列；扫描器入队/续跑时读回
    /// kind==Reindex 即知道该 Job 必须走 force 路径。
    pub fn mark_task_reindex(&self, task_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE index_tasks SET kind = ?2 WHERE task_id = ?1",
            params![task_id, TaskKind::Reindex.code()],
        )?;
        Ok(())
    }

    /// 读一条任务的持久意图。
    pub fn task_intent(&self, task_id: i64) -> Result<Option<TaskIntent>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {INTENT_COLS} FROM index_tasks WHERE task_id = ?1"),
                params![task_id],
                map_intent,
            )
            .optional()?)
    }

    /// 某个范围下的全部任务意图（按 `task_id` 倒序）。
    pub fn task_intents_for_scope(&self, scope_path: &str) -> Result<Vec<TaskIntent>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {INTENT_COLS} FROM index_tasks
             WHERE scope_path = ?1 ORDER BY task_id DESC"
        ))?;
        let rows = st.query_map(params![scope_path], map_intent)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 写续跑检查点。
    pub fn set_task_checkpoint(&self, task_id: i64, checkpoint: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE index_tasks SET checkpoint = ?2 WHERE task_id = ?1",
            params![task_id, checkpoint],
        )?;
        Ok(())
    }

    /// 标记任务正在运行：清掉用户暂停位与结束时间。
    pub fn set_task_running(&self, task_id: i64, status: TaskStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE index_tasks SET status = ?2, user_paused = 0, finished_at = NULL
             WHERE task_id = ?1",
            params![task_id, status.code()],
        )?;
        Ok(())
    }

    /// 用户显式暂停 / 继续。**幂等**：行不存在或已被取消（0 行变化）也返回 `Ok(())`，
    /// 且 `WHERE cancel_reason IS NULL` 保证「已取消」不会被重新激活。
    pub fn set_task_user_paused(&self, task_id: i64, on: bool) -> Result<()> {
        let status = if on {
            TaskStatus::Paused
        } else {
            TaskStatus::Scanning
        };
        self.conn.execute(
            "UPDATE index_tasks SET user_paused = ?2, status = ?3
             WHERE task_id = ?1 AND cancel_reason IS NULL",
            params![task_id, if on { 1 } else { 0 }, status.code()],
        )?;
        Ok(())
    }

    /// 取消一条索引任务，**幂等**。返回是否本次真的执行了取消。
    ///
    /// 读 + 写在 `unchecked_transaction` 内：并发下两次取消只能有一次把
    /// `generation` 加一、写 `cancel_reason`。取消同时落终态（status=Completed /
    /// outcome=Cancelled），这样老 API 的 `unfinished_tasks()` 也自然查不到它。
    pub fn cancel_index_task(&self, task_id: i64, reason: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let existing: Option<Option<String>> = tx
            .query_row(
                "SELECT cancel_reason FROM index_tasks WHERE task_id = ?1",
                params![task_id],
                |r| r.get(0),
            )
            .optional()?;
        match existing {
            // 行不存在，或已经有取消原因：不再改动（generation 也不再动）。
            None | Some(Some(_)) => {
                tx.commit()?;
                return Ok(false);
            }
            Some(None) => {}
        }
        tx.execute(
            "UPDATE index_tasks
             SET cancel_reason = ?2,
                 outcome       = 3,
                 status        = 3,
                 finished_at   = strftime('%s','now'),
                 error_code    = ?2,
                 generation    = generation + 1
             WHERE task_id = ?1",
            params![task_id, reason],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// 让任务的 `generation` 加一并返回新值；行不存在时返回 0。
    pub fn bump_task_generation(&self, task_id: i64) -> Result<i64> {
        self.conn.execute(
            "UPDATE index_tasks SET generation = generation + 1 WHERE task_id = ?1",
            params![task_id],
        )?;
        Ok(self
            .conn
            .query_row(
                "SELECT generation FROM index_tasks WHERE task_id = ?1",
                params![task_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// 崩溃恢复入口：未结束、未取消、**未被用户暂停**的单根任务。
    pub fn resumable_index_tasks(&self) -> Result<Vec<TaskIntent>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {INTENT_COLS} FROM index_tasks
             WHERE status IN (0,1,2,5)
               AND cancel_reason IS NULL
               AND user_paused = 0
               AND kind IN (0,1,2,3)
               AND scope_kind = 0
             ORDER BY task_id"
        ))?;
        let rows = st.query_map([], map_intent)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 用户显式暂停、等待「继续」的单根任务。
    pub fn paused_index_tasks(&self) -> Result<Vec<TaskIntent>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {INTENT_COLS} FROM index_tasks
             WHERE status IN (0,1,2,5)
               AND cancel_reason IS NULL
               AND user_paused = 1
               AND kind IN (0,1,2,3)
               AND scope_kind = 0
             ORDER BY task_id"
        ))?;
        let rows = st.query_map([], map_intent)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 面板快照用的「全部未完成单根任务」：暂停的 + 可续跑的，但不含已取消。
    pub fn unfinished_index_tasks(&self) -> Result<Vec<TaskIntent>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {INTENT_COLS} FROM index_tasks
             WHERE status IN (0,1,2,5)
               AND cancel_reason IS NULL
               AND kind IN (0,1,2,3)
               AND scope_kind = 0
             ORDER BY task_id"
        ))?;
        let rows = st.query_map([], map_intent)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 任务终态一次性写全（委托给 `tasks.rs::finish_task`）。
    pub fn finish_index_task(
        &self,
        task_id: i64,
        status: TaskStatus,
        outcome: TaskOutcome,
        processed: u64,
        failed: u64,
        error_code: Option<&str>,
    ) -> Result<()> {
        self.finish_task(task_id, status, outcome, processed, failed, error_code)
    }

    /// 全局取消所有「活着」的单根索引任务。**幂等**：第二次返回 0。
    pub fn cancel_all_index_tasks(&self, reason: &str) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE index_tasks
             SET cancel_reason = ?1,
                 outcome       = 3,
                 status        = 3,
                 finished_at   = strftime('%s','now'),
                 error_code    = ?1,
                 generation    = generation + 1
             WHERE cancel_reason IS NULL
               AND finished_at IS NULL
               AND kind IN (0,1,2,3)
               AND scope_kind = 0",
            params![reason],
        )?;
        Ok(n as u64)
    }

    /// 全局「暂停全部索引」开关（默认关）。
    pub fn index_pause_all(&self) -> Result<bool> {
        self.setting_bool("m43_index_pause_all", false)
    }

    /// 写全局「暂停全部索引」开关。
    pub fn set_index_pause_all(&self, on: bool) -> Result<()> {
        self.set_setting_raw("m43_index_pause_all", Some(if on { "1" } else { "0" }))
    }

    /// 按原始路径或比较键（小写）找一个 `root_id`。找不到返回 `None`。
    pub fn root_id_for_path(&self, path: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM roots
                 WHERE path = ?1 OR path_key = ?2
                 ORDER BY id LIMIT 1",
                params![path, path.to_lowercase()],
                |r| r.get(0),
            )
            .optional()?)
    }
}
