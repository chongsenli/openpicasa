//! 索引进度快照（M2 A 线，见 `docs/04` §3）。
//!
//! ## 这个模块只做一件事：**让四个地方读同一份数**
//!
//! 弹框、进度面板、左栏状态点、栅格计数如果各自累加事件，**必然出现「两处差 1」**
//! （`04` §3.2 把这种偏差按 P0 处理）。所以这里只维护一份内部状态，
//! 对外只暴露**不可变快照**（[`IndexSnapshot`] / [`TaskSnapshot`]）：
//! UI 拿到的是同一次 `snapshot()` 的切片，汇总与单文件夹视图都从它切，不另存计数。
//!
//! ## 分母冻结 + 上包络（`04` §3.3，P-04 裁决）
//!
//! 1. 扫描阶段分母未知 → `percent()` 为 `None`（界面只能显示「已发现 N 张」）。
//! 2. 进入生成阶段时**冻结分母**，之后新发现的文件排入下一批，不改当前分母。
//! 3. 显示值取上包络 `displayed = max(displayed, raw)`，同一批次内只前进不回退。
//! 4. 单调性**只在同一批次内成立**：换批允许从 0 重新计，但批次号必须被界面显示出来
//!    （`IndexSnapshot::batch_label()`），否则就是给用户看一个来路不明的回落。
//!
//! ## 批次（`04` §1.2.1，P-02 裁决）
//!
//! 一个批次 = 一次任务入队。只有三类动作产生新批次：首次扫描、重试、重启后恢复。
//! 关闭记忆**不在这里**——它属于 UI 会话状态（进程退出即清空，不落盘）。

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// 当前时间（Unix 秒，浮点）。只用于「最近更新」这类展示，不参与逻辑判定。
///
/// 用 `chrono` 而不是 `SystemTime`：`clippy.toml` 明确禁用后者
/// （跨平台序列化语义不稳），时间统一落成 i64/f64 数值。
pub fn now_secs() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
}

/// 任务状态（`04` §3.1 的七态，**一个都不能省**）。
///
/// 省略 `暂停` 的后果写在这里，防止后来者"顺手简化"：
/// 系统休眠后线程池被挂起，界面会停在「生成缩略图中」却一动不动——比没有状态更糟。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexState {
    /// 已入队，还没轮到
    Waiting,
    /// 枚举目录中（分母未知）
    Scanning,
    /// 生成缩略图中（分母已冻结）
    Thumbnailing,
    /// 全部成功
    Completed,
    /// 存在失败项（不可视为"一切正常"）
    PartialFailed,
    /// 系统事件（休眠/待机/挂起）导致的暂停，恢复由系统事件自动发生
    Paused,
    /// 目录不存在、无权限、外置盘失联
    Unreachable,
}

impl IndexState {
    /// 界面文案。**四处（弹框/面板/左栏/栅格）必须逐字用这个**（`04` §3.1）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "等待",
            Self::Scanning => "扫描中",
            Self::Thumbnailing => "生成缩略图中",
            Self::Completed => "已完成",
            Self::PartialFailed => "部分失败",
            Self::Paused => "暂停",
            Self::Unreachable => "不可访问",
        }
    }

    /// 终态：不会再自己动了。
    ///
    /// 注意 `部分失败` 也算终态——它在等用户决定重试还是忽略；
    /// 但**弹框不能因为它是终态就收起**（`04` §1.4：有失败项时降级为异常摘要）。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::PartialFailed | Self::Unreachable)
    }

    /// 还在跑（含排队与系统暂停）。
    pub fn is_running(self) -> bool {
        matches!(
            self,
            Self::Waiting | Self::Scanning | Self::Thumbnailing | Self::Paused
        )
    }
}

/// 一条失败记录。**按条目计，不按尝试次数计**（`04` §2.4 第 3 条）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureItem {
    pub photo_id: i64,
    pub name: String,
    pub reason: String,
}

/// 单个关注文件夹的只读快照。
#[derive(Clone, Debug)]
pub struct TaskSnapshot {
    /// 规范化路径（任务身份）
    pub path: String,
    /// 路径尾段（界面显示用）
    pub name: String,
    pub state: IndexState,
    /// 该任务所属批次
    pub batch: u64,
    /// 已发现（进入生成阶段后即本批次冻结的分母）
    pub discovered: u64,
    /// 已完成（**只数真图**，占位格不算）
    pub completed: u64,
    /// 失败条目数（= `failures.len()`，不重复累加）
    pub failed: u64,
    /// 显示百分比：扫描阶段为 `None`（不确定态），生成阶段为**上包络后的**值
    pub percent: Option<u32>,
    /// 异常原因短语（失败/不可访问/暂停）
    pub reason: Option<String>,
    /// 最近更新时间（Unix 秒）
    pub last_update: f64,
    /// 失败项明细（给面板展开用；内部已限长，见 [`MAX_FAILURES`]）
    pub failures: Vec<FailureItem>,
}

impl TaskSnapshot {
    /// `已完成 / 已发现` 的文案。分母未知时是「X / 已发现 Y」。
    pub fn counts_text(&self) -> String {
        if self.percent.is_none() && self.state == IndexState::Scanning {
            format!("已发现 {} 张", self.discovered)
        } else {
            format!("{} / {}", self.completed, self.discovered)
        }
    }
}

/// 失败明细最多保留多少条。10 万张里坏 500 张已经足够说明问题，
/// 全留着只会让快照克隆变贵（UI 每 250 ms 拿一次）。
pub const MAX_FAILURES: usize = 500;

/// 整块索引进度的只读快照（**唯一数据源**）。
#[derive(Clone, Debug, Default)]
pub struct IndexSnapshot {
    /// 当前（最新）批次号；批次从 1 开始
    pub batch: u64,
    pub tasks: Vec<TaskSnapshot>,
    /// 当前批次入队时刻（Unix 秒）
    pub started_at: f64,
    /// 全部任务结束时刻（无任务或未结束为 `None`）
    pub finished_at: Option<f64>,
    /// 汇总显示百分比（上包络后；扫描阶段或分母全为 0 时为 `None`）
    pub percent: Option<u32>,
}

impl IndexSnapshot {
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// 是否还有任务没结束（含 `暂停`）。
    pub fn running(&self) -> bool {
        self.tasks.iter().any(|t| t.state.is_running())
    }

    /// 汇总 `(已完成, 已发现, 失败)`。**分项之和**，不是另算一遍。
    pub fn totals(&self) -> (u64, u64, u64) {
        self.tasks.iter().fold((0, 0, 0), |(c, d, f), t| {
            (c + t.completed, d + t.discovered, f + t.failed)
        })
    }

    pub fn failed(&self) -> u64 {
        self.tasks.iter().map(|t| t.failed).sum()
    }

    pub fn has_failures(&self) -> bool {
        self.failed() > 0
    }

    /// 有不可访问的目录（需要用户处理，弹框不得自动消失）。
    pub fn has_unreachable(&self) -> bool {
        self.tasks.iter().any(|t| t.state == IndexState::Unreachable)
    }

    /// 存在系统暂停的任务。
    pub fn paused(&self) -> bool {
        self.tasks.iter().any(|t| t.state == IndexState::Paused)
    }

    /// 汇总阶段文案。多任务时以「最近活跃」的那个为准，避免出现"汇总说扫描中、实际在生成"。
    pub fn stage_label(&self) -> &'static str {
        if self.tasks.iter().any(|t| t.state == IndexState::Paused) {
            return IndexState::Paused.label();
        }
        if self
            .tasks
            .iter()
            .any(|t| t.state == IndexState::Thumbnailing)
        {
            return IndexState::Thumbnailing.label();
        }
        if self.tasks.iter().any(|t| t.state == IndexState::Scanning) {
            return IndexState::Scanning.label();
        }
        if self.tasks.iter().any(|t| t.state == IndexState::Waiting) {
            return IndexState::Waiting.label();
        }
        if self.has_failures() {
            return IndexState::PartialFailed.label();
        }
        if self.has_unreachable() {
            return IndexState::Unreachable.label();
        }
        IndexState::Completed.label()
    }

    /// 批次标注。**`第 1 批` 不显示**（首个批次没有"回落"的可能）；
    /// 从第 2 批起必须显示，否则跨批重置就是无来路的数字（`04` §3.3，P-04）。
    pub fn batch_label(&self) -> Option<String> {
        (self.batch > 1).then(|| format!("第 {} 批", self.batch))
    }

    /// 当前正在处理的文件夹（多任务时取最近更新的那个，`04` §1.3）。
    pub fn current_folder(&self) -> Option<&TaskSnapshot> {
        self.tasks
            .iter()
            .filter(|t| t.state.is_running())
            .max_by(|a, b| a.last_update.total_cmp(&b.last_update))
            .or_else(|| self.tasks.first())
    }

    /// 按路径找任务（左栏状态点、栅格占位判断都用它）。
    pub fn task_by_path(&self, path: &str) -> Option<&TaskSnapshot> {
        self.tasks.iter().find(|t| t.path == path)
    }
}

// ---------------------------------------------------------------- 内部状态

#[derive(Clone, Debug)]
struct Task {
    path: String,
    name: String,
    state: IndexState,
    batch: u64,
    discovered: u64,
    completed: u64,
    failures: Vec<FailureItem>,
    failed: u64,
    percent_displayed: u32,
    /// 分母是否已冻结（进入生成阶段）
    frozen: bool,
    reason: Option<String>,
    last_update: f64,
    /// 进入 `暂停` 前的状态，恢复时原样回去
    resume_to: Option<IndexState>,
}

impl Task {
    fn snapshot(&self) -> TaskSnapshot {
        TaskSnapshot {
            path: self.path.clone(),
            name: self.name.clone(),
            state: self.state,
            batch: self.batch,
            discovered: self.discovered,
            completed: self.completed,
            failed: self.failed,
            percent: if self.frozen && self.discovered > 0 {
                Some(self.percent_displayed)
            } else {
                None
            },
            reason: self.reason.clone(),
            last_update: self.last_update,
            failures: self.failures.clone(),
        }
    }

    fn raw_percent(&self) -> u32 {
        if self.discovered == 0 {
            return 0;
        }
        // 向下取整（`04` §3.3：取整方式固定）
        ((self.completed.min(self.discovered) * 100) / self.discovered) as u32
    }

    /// 上包络：同一批次内只允许持平或前进。
    fn bump_percent(&mut self) {
        let raw = self.raw_percent();
        if raw > self.percent_displayed {
            self.percent_displayed = raw;
        }
    }
}

struct Inner {
    tasks: Vec<Task>,
    next_batch: u64,
    last_batch: u64,
    last_started: f64,
    last_finished: Option<f64>,
    /// 汇总百分比的上包络（换批时重置）
    agg_displayed: u32,
}

impl Inner {
    fn find_mut(&mut self, path: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.path == path)
    }

    fn recompute_agg(&mut self) {
        if self.tasks.iter().any(|t| !t.frozen) {
            // 还有任务在扫描：汇总不造百分比
            self.agg_displayed = 0;
            return;
        }
        let discovered: u64 = self.tasks.iter().map(|t| t.discovered).sum();
        let completed: u64 = self.tasks.iter().map(|t| t.completed).sum();
        if discovered == 0 {
            self.agg_displayed = 0;
            return;
        }
        let raw = ((completed.min(discovered) * 100) / discovered) as u32;
        if raw > self.agg_displayed {
            self.agg_displayed = raw;
        }
    }
}

/// 索引进度板：**唯一数据源**（UI 只读它的快照）。
pub struct IndexBoard {
    inner: Mutex<Inner>,
    revision: AtomicU64,
    dirs: AtomicU64,
}

impl Default for IndexBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexBoard {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                tasks: Vec::new(),
                next_batch: 0,
                last_batch: 0,
                last_started: 0.0,
                last_finished: None,
                agg_displayed: 0,
            }),
            revision: AtomicU64::new(0),
            dirs: AtomicU64::new(0),
        }
    }

    /// 版本号：UI 用它决定「要不要重新取快照」——数值没变就不重绘（`04` §3.4）。
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    fn bump(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub fn note_dir(&self) {
        self.dirs.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dirs_seen(&self) -> u64 {
        self.dirs.load(Ordering::Relaxed)
    }

    /// 是否还有任务在跑（心跳线程每 2 s 问一次，**不要**在这里克隆快照）。
    pub fn running(&self) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.tasks.iter().any(|t| t.state.is_running())
    }

    /// 开一个新批次：一批任务（各关注文件夹）入队。
    ///
    /// 同一路径若上次的任务还留着（已完成/部分失败），**替换**它——
    /// 面板只显示每个文件夹的最新状态，不做历史任务列表（`04` §6 非目标）。
    pub fn begin_batch(&self, paths: &[String]) -> u64 {
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.next_batch += 1;
        let batch = inner.next_batch;
        inner.last_batch = batch;
        inner.last_started = now;
        inner.last_finished = None;
        inner.agg_displayed = 0;
        for path in paths {
            let name = display_name(path);
            if let Some(t) = inner.find_mut(path) {
                *t = new_task(path.clone(), name, batch, now);
            } else {
                inner.tasks.push(new_task(path.clone(), name, batch, now));
            }
        }
        drop(inner);
        self.bump();
        batch
    }

    /// 重试批次：**只针对失败项**，已完成数作为基线继承（`04` §2.4：已完成不回退）。
    ///
    /// 分母沿用原批次冻结的值，于是重试成功后百分比只会前进，
    /// 不会出现「重试一下进度条归零」这种假象。
    pub fn begin_retry(&self, path: &str, discovered: u64, completed: u64, failed: u64) -> u64 {
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.next_batch += 1;
        let batch = inner.next_batch;
        inner.last_batch = batch;
        inner.last_started = now;
        inner.last_finished = None;
        inner.agg_displayed = 0;
        let name = display_name(path);
        let mut task = new_task(path.to_string(), name, batch, now);
        task.state = IndexState::Scanning;
        task.reason = Some("重试失败项".into());
        task.discovered = discovered;
        task.completed = completed;
        task.failed = failed;
        task.frozen = true; // 分母不变（沿用上一批的冻结值）
        task.bump_percent();
        if let Some(t) = inner.find_mut(path) {
            *t = task;
        } else {
            inner.tasks.push(task);
        }
        drop(inner);
        self.bump();
        batch
    }

    pub fn mark_scanning(&self, path: &str) {
        self.transition(path, IndexState::Scanning, None);
    }

    pub fn mark_thumbnailing(&self, path: &str) {
        self.transition(path, IndexState::Thumbnailing, None);
    }

    fn transition(&self, path: &str, state: IndexState, reason: Option<String>) {
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = inner.find_mut(path) {
            t.state = state;
            t.reason = reason;
            t.last_update = now;
        }
        drop(inner);
        self.bump();
    }

    /// 扫描阶段：已发现数增长（**分母未知，不出百分比**）。
    pub fn note_discovered(&self, path: &str, discovered: u64) {
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(t) = inner.find_mut(path) else {
            return;
        };
        // 分母已冻结：新发现的文件排下一批，**绝不并入当前分母**
        if t.frozen {
            return;
        }
        if discovered <= t.discovered {
            return;
        }
        t.discovered = discovered;
        t.last_update = now;
        inner.recompute_agg();
        drop(inner);
        self.bump();
    }

    /// 扫描结束 → **冻结分母**，进入生成阶段。
    ///
    /// `discovered` 取「本任务扫到的图片数」与「库里该目录最后的总数」的较大者：
    /// 一个是本轮扫到的，一个是实际落库的，正常情况下相等；取大避免抖动。
    pub fn freeze_denominator(&self, path: &str, discovered: u64, completed: u64) {
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(t) = inner.find_mut(path) else {
            return;
        };
        t.discovered = t.discovered.max(discovered);
        // 已完成以库里实际就绪数为准，但不能超过分母（否则百分比没有意义）
        t.completed = completed.min(t.discovered);
        t.frozen = true;
        t.state = if t.discovered == 0 {
            IndexState::Completed
        } else {
            IndexState::Thumbnailing
        };
        t.reason = None;
        t.last_update = now;
        if t.state == IndexState::Completed && t.failed == 0 {
            t.percent_displayed = 100;
        } else {
            t.bump_percent();
        }
        inner.recompute_agg();
        drop(inner);
        self.bump();
    }

    /// 一张缩略图落库成功。
    pub fn note_thumb_ok(&self, path: &str) {
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(t) = inner.find_mut(path) else {
            return;
        };
        t.completed = t.completed.saturating_add(1);
        if t.frozen {
            t.bump_percent();
        }
        t.last_update = now;
        inner.recompute_agg();
        drop(inner);
        self.bump();
    }

    /// 一条失败（**同一条目重复失败不重复计数**）。
    pub fn note_thumb_failed(&self, path: &str, item: FailureItem) {
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(t) = inner.find_mut(path) else {
            return;
        };
        if t.failures.iter().any(|f| f.photo_id == item.photo_id) {
            return;
        }
        t.failures.push(item);
        t.failed = t.failures.len() as u64;
        if t.failures.len() > MAX_FAILURES {
            t.failures.truncate(MAX_FAILURES);
        }
        t.last_update = now;
        drop(inner);
        self.bump();
    }

    /// 目录不可访问（不存在 / 无权限 / 外置盘拔出）。
    pub fn mark_unreachable(&self, path: &str, reason: impl Into<String>) {
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = inner.find_mut(path) {
            t.state = IndexState::Unreachable;
            t.reason = Some(reason.into());
            t.last_update = now;
            inner.last_finished = Some(now);
        }
        drop(inner);
        self.bump();
    }

    /// 本任务结束：有失败 → `部分失败`，否则 `已完成`。
    ///
    /// 收尾时若「全部任务终态 + 零失败 + 无不可访问」，把任务清空：
    /// 面板随即进入空态（`04` M2-FD-44），不留一堆"已完成"残行。
    /// 有失败的任务**必须保留**，否则失败就被"完成"吞掉了（`04` §8 风险表）。
    pub fn finish(&self, path: &str) {
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = inner.find_mut(path) {
            t.state = if t.failed > 0 {
                IndexState::PartialFailed
            } else {
                IndexState::Completed
            };
            t.reason = None;
            t.last_update = now;
            if t.failed == 0 && t.frozen {
                t.percent_displayed = 100;
            }
        }
        let all_done = inner.tasks.iter().all(|t| t.state.is_terminal());
        if all_done {
            inner.last_finished = Some(now);
            let clean = inner
                .tasks
                .iter()
                .all(|t| t.failed == 0 && t.state != IndexState::Unreachable);
            if clean {
                inner.tasks.clear();
            }
        }
        inner.recompute_agg();
        drop(inner);
        self.bump();
    }

    /// 系统事件（休眠/挂起）导致的暂停：只对还在跑的任务生效。
    pub fn pause_all(&self, reason: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = false;
        for t in inner.tasks.iter_mut() {
            if t.state.is_running() && t.state != IndexState::Paused {
                t.resume_to = Some(t.state);
                t.state = IndexState::Paused;
                t.reason = Some(reason.to_string());
                changed = true;
            }
        }
        drop(inner);
        if changed {
            self.bump();
        }
    }

    /// 系统恢复：回到进入暂停前的状态，数字从冻结值继续（不回退）。
    pub fn resume_all(&self) {
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = false;
        for t in inner.tasks.iter_mut() {
            if t.state == IndexState::Paused {
                t.state = t.resume_to.take().unwrap_or(IndexState::Thumbnailing);
                t.reason = None;
                t.last_update = now;
                changed = true;
            }
        }
        drop(inner);
        if changed {
            self.bump();
        }
    }

    /// 从图库移除关注文件夹时，连它的任务一起清掉。
    pub fn forget(&self, path: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.tasks.retain(|t| t.path != path);
        inner.recompute_agg();
        drop(inner);
        self.bump();
    }

    /// 取一份快照。**UI 唯一的读入口**。
    pub fn snapshot(&self) -> IndexSnapshot {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let tasks: Vec<TaskSnapshot> = inner.tasks.iter().map(|t| t.snapshot()).collect();
        let percent = if tasks.is_empty()
            || tasks.iter().any(|t| t.percent.is_none() || t.discovered == 0)
        {
            None
        } else {
            Some(inner.agg_displayed)
        };
        IndexSnapshot {
            batch: inner.last_batch,
            tasks,
            started_at: inner.last_started,
            finished_at: inner.last_finished,
            percent,
        }
    }
}

fn new_task(path: String, name: String, batch: u64, now: f64) -> Task {
    Task {
        path,
        name,
        state: IndexState::Waiting,
        batch,
        discovered: 0,
        completed: 0,
        failures: Vec::new(),
        failed: 0,
        percent_displayed: 0,
        frozen: false,
        reason: None,
        last_update: now,
        resume_to: None,
    }
}

fn display_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fail(id: i64) -> FailureItem {
        FailureItem {
            photo_id: id,
            name: format!("IMG_{id}.jpg"),
            reason: "解码失败".into(),
        }
    }

    #[test]
    fn scanning_has_no_percentage() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into()]);
        b.mark_scanning("/p/a");
        b.note_discovered("/p/a", 42);
        let s = b.snapshot();
        assert!(s.percent.is_none(), "扫描阶段不得给百分比");
        assert!(s.tasks[0].percent.is_none());
        assert_eq!(s.tasks[0].counts_text(), "已发现 42 张");
        assert_eq!(s.tasks[0].state.label(), "扫描中");
    }

    #[test]
    fn denominator_freezes_and_percent_never_goes_back() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into()]);
        b.mark_scanning("/p/a");
        b.note_discovered("/p/a", 10);
        b.freeze_denominator("/p/a", 10, 0);
        for _ in 0..5 {
            b.note_thumb_ok("/p/a");
        }
        assert_eq!(b.snapshot().tasks[0].percent, Some(50));

        // 生成阶段又发现新文件：本批分母不变，新文件排下一批
        b.note_discovered("/p/a", 30);
        let t = &b.snapshot().tasks[0];
        assert_eq!(t.discovered, 10, "分母必须在批次内冻结");
        assert_eq!(t.percent, Some(50), "分母漂移不得让显示值回退");
    }

    #[test]
    fn upper_envelope_survives_recount() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into()]);
        b.mark_scanning("/p/a");
        b.freeze_denominator("/p/a", 4, 4); // 4 张早已就绪
        assert_eq!(b.snapshot().tasks[0].percent, Some(100));
        b.note_thumb_failed("/p/a", fail(1));
        assert_eq!(b.snapshot().tasks[0].percent, Some(100), "已显示的值不能倒退");
    }

    #[test]
    fn completed_requires_100_percent_when_all_ready() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into()]);
        b.mark_scanning("/p/a");
        b.freeze_denominator("/p/a", 3, 0);
        for _ in 0..3 {
            b.note_thumb_ok("/p/a");
        }
        b.finish("/p/a");
        let s = b.snapshot();
        // 无失败 → 任务被清空，批次仍可查
        assert!(s.tasks.is_empty());
        assert_eq!(s.batch, 1);
        assert!(s.finished_at.is_some());
        assert!(!s.running());
    }

    #[test]
    fn failures_survive_finish_and_block_auto_cleanup() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into()]);
        b.mark_scanning("/p/a");
        b.freeze_denominator("/p/a", 10, 0);
        b.note_thumb_failed("/p/a", fail(1));
        b.note_thumb_failed("/p/a", fail(1)); // 同一条目第二次失败不重复计数
        b.note_thumb_failed("/p/a", fail(2));
        b.finish("/p/a");
        let s = b.snapshot();
        assert_eq!(s.tasks.len(), 1, "有失败的任务不能被清理掉");
        assert_eq!(s.tasks[0].state, IndexState::PartialFailed);
        assert_eq!(s.failed(), 2, "失败数是条目数，不是尝试次数");
        assert!(s.has_failures());
    }

    #[test]
    fn retry_keeps_completed_base_and_starts_new_batch() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into()]);
        b.mark_scanning("/p/a");
        b.freeze_denominator("/p/a", 10, 0);
        for _ in 0..8 {
            b.note_thumb_ok("/p/a");
        }
        b.note_thumb_failed("/p/a", fail(1));
        b.note_thumb_failed("/p/a", fail(2));
        b.finish("/p/a");
        assert_eq!(b.snapshot().tasks[0].percent, Some(80));

        let batch = b.begin_retry("/p/a", 10, 8, 2);
        assert_eq!(batch, 2);
        let t = &b.snapshot().tasks[0];
        assert_eq!(t.batch, 2);
        assert_eq!(t.completed, 8, "重试不得让已完成回退");
        assert_eq!(t.discovered, 10, "分母沿用冻结值");
        assert_eq!(t.percent, Some(80));
        assert_eq!(t.state, IndexState::Scanning, "重试视为新任务：先扫描");
        assert_eq!(t.reason.as_deref(), Some("重试失败项"));
    }

    #[test]
    fn batch_label_only_after_first_batch() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into()]);
        assert!(b.snapshot().batch_label().is_none(), "首批不标注");
        b.begin_retry("/p/a", 3, 1, 2);
        assert_eq!(b.snapshot().batch_label().as_deref(), Some("第 2 批"));
    }

    #[test]
    fn pause_freezes_and_resume_goes_back_to_previous_state() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into()]);
        b.mark_scanning("/p/a");
        b.freeze_denominator("/p/a", 10, 0);
        b.note_thumb_ok("/p/a");
        b.pause_all("系统休眠");
        let s = b.snapshot();
        assert_eq!(s.tasks[0].state, IndexState::Paused);
        assert_eq!(s.tasks[0].percent, Some(10), "暂停期间数字冻结");
        assert_eq!(s.stage_label(), "暂停");

        b.resume_all();
        let s = b.snapshot();
        assert_eq!(s.tasks[0].state, IndexState::Thumbnailing);
        assert_eq!(s.tasks[0].percent, Some(10), "恢复后不得回退");
        assert!(s.tasks[0].reason.is_none());
    }

    #[test]
    fn unreachable_is_reported_and_kept() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/offline".into()]);
        b.mark_scanning("/p/offline");
        b.mark_unreachable("/p/offline", "目录已失联");
        let s = b.snapshot();
        assert_eq!(s.tasks[0].state, IndexState::Unreachable);
        assert_eq!(s.tasks[0].state.label(), "不可访问");
        assert!(s.has_unreachable());
    }

    #[test]
    fn totals_are_sums_of_parts() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into(), "/p/b".into()]);
        b.mark_scanning("/p/a");
        b.freeze_denominator("/p/a", 4, 0);
        b.note_thumb_ok("/p/a");
        b.mark_scanning("/p/b");
        b.freeze_denominator("/p/b", 6, 2);
        let s = b.snapshot();
        assert_eq!(s.totals(), (3, 10, 0));
        assert_eq!(s.tasks.len(), 2);
        // 分项之和 = 汇总
        let sum_c: u64 = s.tasks.iter().map(|t| t.completed).sum();
        assert_eq!(s.totals().0, sum_c);
    }

    #[test]
    fn aggregate_percent_is_none_while_any_task_scans() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into(), "/p/b".into()]);
        b.mark_scanning("/p/a");
        b.freeze_denominator("/p/a", 4, 2);
        b.mark_scanning("/p/b");
        assert!(b.snapshot().percent.is_none(), "有一个还在扫描就不能给汇总百分比");
        b.freeze_denominator("/p/b", 4, 0);
        assert_eq!(b.snapshot().percent, Some(25));
    }

    #[test]
    fn state_labels_are_the_seven_required_ones() {
        let labels = [
            IndexState::Waiting,
            IndexState::Scanning,
            IndexState::Thumbnailing,
            IndexState::Completed,
            IndexState::PartialFailed,
            IndexState::Paused,
            IndexState::Unreachable,
        ]
        .map(|s| s.label());
        assert_eq!(
            labels,
            ["等待", "扫描中", "生成缩略图中", "已完成", "部分失败", "暂停", "不可访问"]
        );
    }
}
