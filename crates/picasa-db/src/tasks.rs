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

use crate::pathutil::compare_key;
use crate::schema::{TaskKind, TaskStatus};
use crate::types::{FailureKind, classify_failure};
use crate::{Db, FailureGroup, Result, TaskOutcome, TaskRow};
use rusqlite::{OptionalExtension, params};

/// `TaskRow` 的列清单 —— **全项目只此一份**。
///
/// 与 `lib.rs::photo_cols()` 同样的道理：`recent_tasks` / `task` / `unfinished_tasks`
/// 三处 SELECT 各写一遍列，加字段时漏改一处就只在运行期炸。收口之后
/// `map_task` 的下标与它是一对。
const TASK_COLS: &str = "task_id, root_id, scope_path, kind, status, started_at, finished_at,
                         error_code, outcome, processed, failed, duration_ms";

fn map_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        task_id: r.get(0)?,
        root_id: r.get(1)?,
        scope_path: r.get(2)?,
        kind: r.get(3)?,
        status: r.get(4)?,
        started_at: r.get(5)?,
        finished_at: r.get(6)?,
        error_code: r.get(7)?,
        outcome: r.get(8)?,
        processed: r.get(9)?,
        failed: r.get(10)?,
        duration_ms: r.get(11)?,
    })
}

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

    /// 任务终态**一次性**写全（M2.2 §4.7）：状态 + 结局 + 账目 + 耗时。
    ///
    /// ## 为什么必须是一个方法而不是四个
    ///
    /// 规格写明「终态由落库线程一并写（同一事务）」。分四次写的话，
    /// 进程在中间被杀就会留下「已完成但 processed=0」这种自相矛盾的历史行，
    /// 而历史列表是给用户看的——他没法分辨哪一行是残缺的。
    ///
    /// `duration_ms` 由 `started_at` 算出，不靠调用方传：调用方手算会各算一套，
    /// 而 `started_at` 是库里的事实。
    pub fn finish_task(
        &self,
        task_id: i64,
        status: TaskStatus,
        outcome: TaskOutcome,
        processed: u64,
        failed: u64,
        error_code: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE index_tasks
             SET status      = ?2,
                 outcome     = ?3,
                 processed   = ?4,
                 failed      = ?5,
                 error_code  = ?6,
                 finished_at = strftime('%s','now'),
                 duration_ms = (strftime('%s','now') - COALESCE(started_at, strftime('%s','now'))) * 1000
             WHERE task_id = ?1",
            params![
                task_id,
                status.code(),
                outcome.code(),
                processed as i64,
                failed as i64,
                error_code
            ],
        )?;
        Ok(())
    }

    /// 历史淘汰：只保留最近 `keep` 条**已结束**的任务（§4.7）。
    ///
    /// 用 `finished_at` 排序而不是 `task_id`：重启恢复的 `Resume` 任务会被重新插入，
    /// 其 `task_id` 可能比一批已完成的旧任务更大，按 id 排会误删新近的。
    /// 未结束的任务（正在跑/排队/暂停）**一条都不删**——删了就查不到该恢复什么。
    pub fn prune_task_history(&self, keep: usize) -> Result<u64> {
        let n = self.conn.execute(
            "DELETE FROM index_tasks
             WHERE finished_at IS NOT NULL
               AND task_id NOT IN (
                   SELECT task_id FROM index_tasks
                    WHERE finished_at IS NOT NULL
                    ORDER BY finished_at DESC, task_id DESC
                    LIMIT ?1
               )",
            params![keep as i64],
        )?;
        Ok(n as u64)
    }

    /// 已结束任务的历史（§4.2 M22-IDX-03：最近 50 条，按结束时间倒序）。
    pub fn finished_tasks(&self, limit: usize) -> Result<Vec<TaskRow>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {TASK_COLS} FROM index_tasks
             WHERE finished_at IS NOT NULL
             ORDER BY finished_at DESC, task_id DESC
             LIMIT ?1"
        ))?;
        let rows = st.query_map(params![limit as i64], map_task)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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
        let mut st = self.conn.prepare(&format!(
            "SELECT {TASK_COLS} FROM index_tasks ORDER BY task_id DESC LIMIT ?1"
        ))?;
        let rows = st.query_map(params![limit as i64], map_task)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 某个任务的任务行（测试与「恢复未完成任务」用）。
    pub fn task(&self, task_id: i64) -> Result<Option<TaskRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {TASK_COLS} FROM index_tasks WHERE task_id = ?1"),
                params![task_id],
                map_task,
            )
            .optional()?)
    }

    /// 上次退出时还没跑完的任务（`kind = Resume` 的恢复入口，见 `docs/04` P-02 第 3 类批次）。
    pub fn unfinished_tasks(&self) -> Result<Vec<TaskRow>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {TASK_COLS} FROM index_tasks
             WHERE status IN (0, 1, 2, 5)
             ORDER BY task_id"
        ))?;
        let rows = st.query_map([], map_task)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ------------------------------------------------ 失败分类（M2.2 §4.5）

    /// 记一条失败，带分类。同条目重复失败只累加 `attempts`（**不改 kind**）。
    ///
    /// ⚠️ 重复失败时不覆盖 `kind`：后一次失败可能是「卷被拔了」（Unreachable），
    /// 而文件本身是格式不支持（Unsupported）。覆盖成前者会让「忽略」失效——
    /// 用户忽略了 Unsupported，下次重扫又变成 Unreachable 冒出来。
    pub fn record_task_failure_kind(
        &self,
        task_id: i64,
        relative_path: &str,
        error_code: &str,
        kind: FailureKind,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO index_task_failures
                 (task_id, relative_path, error_code, attempts, updated_at, kind, ignored)
             VALUES (?1, ?2, ?3, 1, strftime('%s','now'), ?4, 0)
             ON CONFLICT(task_id, relative_path) DO UPDATE SET
                 error_code = excluded.error_code,
                 attempts   = index_task_failures.attempts + 1,
                 updated_at = excluded.updated_at",
            params![task_id, relative_path, error_code, kind.code()],
        )?;
        Ok(())
    }

    /// v5 数据步骤：给历史失败记录补 `kind`（§5.3）。**幂等**。
    ///
    /// 只动 `kind = 3`（默认值）的行：用户后来手动改过的分类不能被覆盖。
    /// 判据是 `error_code` 文本（见 [`classify_failure`]），推断不出来就保持 3。
    pub fn backfill_failure_kinds(&self) -> Result<u64> {
        let rows: Vec<(i64, String, String)> = {
            let mut st = self.conn.prepare(
                "SELECT task_id, relative_path, error_code
                 FROM index_task_failures WHERE kind = 3",
            )?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut n = 0u64;
        for (task_id, rel, code) in rows {
            let kind = classify_failure(&code);
            n += self.conn.execute(
                // `AND kind <> ?3` 是幂等的关键：分类本来就对的行（比如推断结果仍是
                // Unreachable）不该被算进"改了几条"，否则第二次补数会报一个非零的
                // 假数字，让调用方以为又动过数据。
                "UPDATE index_task_failures SET kind = ?3
                 WHERE task_id = ?1 AND relative_path = ?2 AND kind = 3 AND kind <> ?3",
                params![task_id, rel, kind.code()],
            )? as u64;
        }
        Ok(n)
    }

    /// 有**未忽略**失败项的任务数（§4.4 角标的第一分量）。
    ///
    /// ⚠️ 数的是**任务**不是照片（`COUNT(DISTINCT task_id)`）：
    /// 一个目录下 970 张失败只算 1 件事。角标回答的是「有几件事要你处理」，
    /// 不是「有几张照片坏了」——后者在失败分组明细里看。
    pub fn failed_task_count(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT task_id) FROM index_task_failures WHERE ignored = 0",
            [],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    /// 五类失败的汇总（§6.1 `failures_by_kind`）。下标即 `kind - 1`。
    ///
    /// 角标只算 `count`（未忽略）；`ignored` 单独给出来，
    /// 因为**忽略的失败项必须仍在列表里可见**（R-M2-4）——界面要能显示「已忽略 N」。
    pub fn failure_groups(&self) -> Result<Vec<FailureGroup>> {
        let mut out = vec![FailureGroup::default(); FailureKind::ALL.len()];
        let mut st = self.conn.prepare(
            "SELECT kind, ignored, COUNT(*) FROM index_task_failures GROUP BY kind, ignored",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (kind, ignored, count) = row?;
            let Some(idx) = FailureKind::from_code(kind).map(|k| k.code() as usize - 1) else {
                continue;
            };
            if idx >= out.len() {
                continue;
            }
            if ignored != 0 {
                out[idx].ignored += count.max(0) as u64;
            } else {
                out[idx].count += count.max(0) as u64;
            }
            out[idx].kind = Some(kind);
        }
        Ok(out)
    }

    /// 某一类失败的明细（面板展开用）。`include_ignored` 为真时连已忽略的一起给。
    pub fn failures_of_kind(
        &self,
        kind: FailureKind,
        include_ignored: bool,
        limit: usize,
    ) -> Result<Vec<(i64, String, String)>> {
        let sql = format!(
            "SELECT f.task_id, f.relative_path, f.error_code
             FROM index_task_failures f
             WHERE f.kind = ?1 {} 
             ORDER BY f.updated_at DESC, f.relative_path ASC
             LIMIT ?2",
            if include_ignored {
                ""
            } else {
                "AND f.ignored = 0"
            }
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map(params![kind.code(), limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 「忽略全部」：把一整类标成已忽略，返回受影响条数。
    ///
    /// ⚠️ **只是标记，不是删除**（R-M2-4）。删了就等于"失败不存在了"，
    /// 用户再也无法知道这批照片为什么没有缩略图。
    pub fn ignore_failures_of_kind(&self, kind: FailureKind) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE index_task_failures SET ignored = 1 WHERE kind = ?1 AND ignored = 0",
            params![kind.code()],
        )?;
        Ok(n as u64)
    }

    /// 取消忽略（让一类失败重新进角标）。
    pub fn unignore_failures_of_kind(&self, kind: FailureKind) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE index_task_failures SET ignored = 0 WHERE kind = ?1 AND ignored = 1",
            params![kind.code()],
        )?;
        Ok(n as u64)
    }

    /// 「重试全部」：把一类的失败项解析成**照片 id**（§4.3 边界：只处理点名的 id）。
    ///
    /// ## 为什么要解析成 id
    ///
    /// 规格 §4.3 明确「重试失败项（跨会话）」要走 `generate_thumbs_for_ids`，
    /// **不能**退化成 `generate_thumbs(folder_id)`——后者会把该目录里所有
    /// `thumb_state = 0` 的历史欠账也一起做掉（实测 22 万张），一次点按变成几小时。
    ///
    /// 解析方式：`index_task_failures.relative_path` 是相对任务范围的，
    /// 拼回 `scope_path` 得到完整路径，拆出目录与文件名后按 `(folder_path, name_key)` 查。
    /// 查不到的（照片已被删、或已移出关注范围）**静默跳过**：那是索引已经更正了，
    /// 不是失败。
    pub fn photo_ids_of_failure_kind(&self, kind: FailureKind) -> Result<Vec<i64>> {
        let rows: Vec<(String, String)> = {
            let mut st = self.conn.prepare(
                "SELECT t.scope_path, f.relative_path
                 FROM index_task_failures f
                 JOIN index_tasks t ON t.task_id = f.task_id
                 WHERE f.kind = ?1 AND f.ignored = 0",
            )?;
            st.query_map(params![kind.code()], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        {
            let mut st = self.conn.prepare(
                "SELECT p.id FROM photos p JOIN folders fo ON fo.id = p.folder_id
                 WHERE fo.path = ?1 AND p.name_key = ?2",
            )?;
            for (scope, rel) in rows {
                let full = join_scope(&scope, &rel);
                let Some((folder, name)) = split_last(full) else {
                    continue;
                };
                let key = compare_key(&name);
                let hit: Option<i64> = st
                    .query_row(params![folder, key], |r| r.get(0))
                    .optional()?;
                if let Some(id) = hit
                    && seen.insert(id)
                {
                    out.push(id);
                }
            }
        }
        Ok(out)
    }
}

/// `scope_path` + `relative_path` → 完整路径（两侧都可能带/不带斜杠）。
fn join_scope(scope: &str, rel: &str) -> String {
    let s = scope.trim_end_matches('/');
    let r = rel.trim_start_matches('/');
    if s.is_empty() {
        r.to_string()
    } else {
        format!("{s}/{r}")
    }
}

/// 拆成 `(目录, 文件名)`。没有分隔符（相对路径就是文件名）时目录为 `scope`。
fn split_last(full: String) -> Option<(String, String)> {
    full.rfind('/')
        .map(|i| (full[..i].to_string(), full[i + 1..].to_string()))
}
