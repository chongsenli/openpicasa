//! # 索引管理的**唯一数据源**（M2.2 §6.1）
//!
//! 这个模块只做一件事：**把库里的状态读成一份不可变的快照**，交给 UI 只读渲染。
//!
//! ## 为什么必须有这一层
//!
//! M2.1 里「树上的数字」「栅格组头的 N」「所有照片」「索引管理页的照片数」是
//! **四条各自去查库的路径**。它们在同一时刻给出不同的答案——不是 bug，是四条查询
//! 各自落在了不同的事务点上。M2.2 的红线 R-M22-6 把这件事定为 P0：四处必须逐字段相等。
//!
//! 修法只有一种：**数字只有一个出处**。于是有了 [`MgmtSnapshot`]：
//! 一次事务内算出全部计数，带上单调递增的 `seq`，四处都读这一份。
//! UI 层**不许**再自己 `SELECT`（§1.2 非目标最后一条）。
//!
//! ## 快照刷新不能顺手重排布局
//!
//! PERF-09：快照以 ≤ 4 次/秒刷新，而 `Layout` 只在 {档位, 窗口宽度, 数据集版本号}
//! 变化时重算。**刷新快照不改变数据集版本号**——[`MgmtSnapshot::dataset_version`]
//! 只在照片集合真的变了（增删）时才自增，计数变化不算。
//!
//! ## 无窗口可测
//!
//! 本模块**不碰** egui，也不 `read_dir`（离线判定除外，且注入可替）。
//! 所有逻辑都能在 `cargo test` 里跑：构造一个内存库 → 灌数据 → 断言快照字段。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use picasa_db::{self as db, ExclusionRow, FailureGroup, FailureKind, RootStat, TaskRow};

use crate::Result;
use crate::index::IndexState;

// ---------------------------------------------------------------- 常量（附录 A）

/// 任务历史保留条数（§4.7 / 附录 A `index.history_keep`）。
pub const HISTORY_KEEP: usize = 50;

/// 首屏有图之后多久开始启动校验（§4.8 / `index.startup_verify_delay_ms`）。
pub const STARTUP_VERIFY_DELAY_MS: u64 = 2000;

/// 启动校验每检查完一个目录歇多久（§4.8 / `index.verify_dir_interval_ms`）。
pub const VERIFY_DIR_INTERVAL_MS: u64 = 50;

/// 快照发布频率上限（§6.1 / `perf.snapshot_hz_max`）。
pub const SNAPSHOT_HZ_MAX: u32 = 4;

/// 单张解码超时（§4.5 `Timeout` 的判据 / `index.decode_timeout_ms`）。
pub const DECODE_TIMEOUT_MS: u64 = 15_000;

// ---------------------------------------------------------------- 视图类型

/// 文件夹树节点的状态叠层（§6.1 `FolderState`）。
///
/// 与 `photos.thumb_state` 不是一回事：那是**照片**的，这是**目录**的，
/// 一个目录的状态是它下面所有照片状态的聚合（有失败就算 `Failed`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FolderState {
    /// 没事
    #[default]
    Idle,
    /// 正在索引（有 `Running` 任务覆盖它）
    Indexing,
    /// 有失败照片（R-M2-4：必须有可见落点，不许静默）
    Failed,
    /// 被系统暂停（P-03：无「继续」按钮）
    Paused,
    /// 有历史欠账（缩略图从未生成过，`thumb_state = 0`）
    Pending,
}

impl FolderState {
    /// 树上的状态点文案（与 `FailureKind::label` 一样，是**唯一措辞来源**）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "",
            Self::Indexing => "正在索引",
            Self::Failed => "有失败项",
            Self::Paused => "已暂停",
            Self::Pending => "待生成缩略图",
        }
    }

    /// 优先级：一个目录同时满足多条时显示哪个。
    ///
    /// `Failed` 最高——R-M2-4「失败不静默」压过「正在索引」的进度感：
    /// 用户更该知道有东西坏了，而不是知道有东西在跑。
    fn rank(self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Pending => 1,
            Self::Paused => 2,
            Self::Indexing => 3,
            Self::Failed => 4,
        }
    }

    /// 取更「值得显示」的那一个。
    pub fn merge(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// 「任务」标签里的一行（§4.1 第一行的字段表）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskView {
    pub task_id: i64,
    pub root_id: Option<i64>,
    /// 范围：根路径或子目录路径
    pub scope: String,
    /// 类型文案（`TaskKind::label`，UI 不另造词）
    pub kind: String,
    /// 状态文案（`TaskStatus::label`）
    pub status: String,
    /// 是否处于运行态
    pub running: bool,
    pub discovered: u64,
    pub completed: u64,
    pub failed: u64,
    /// 阶段文案（扫描中 / 生成缩略图 / —）
    pub stage: String,
    /// 批次号（同一批一起发起的任务共用一个）
    pub batch: Option<u64>,
    pub started_at: Option<i64>,
    /// 已用时长文案 `mm:ss`。
    ///
    /// ⚠️ **没有 ETA 字段**，也不许加（`TC-IDX-003`）：分母在扫描期间一直在漂，
    /// 任何一个 ETA 都是在撒谎。
    pub elapsed_text: String,
    /// 结局（仅终态有）
    pub outcome: Option<String>,
}

/// 「关注文件夹」标签里的一行（§4.1 第二行 / §6.1 `RootView`）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootView {
    pub id: i64,
    pub path: String,
    /// 用户在界面上主动关闭（`roots.state = 1`）
    pub closed: bool,
    /// 卷不在（§4.6，**不落库**，每次快照现算）
    pub offline: bool,
    pub photo_count: u64,
    pub thumb_bytes: u64,
    pub failed: u64,
    pub last_indexed_at: Option<i64>,
}

impl RootView {
    /// 「正常 / 已关闭 / 离线」三态文案（§4.1）。
    ///
    /// 顺序有讲究：离线**优先**于已关闭。盘拔了还显示「已关闭」，用户会以为是
    /// 自己关的，然后去找一个根本不存在的开关。
    pub fn status_text(&self) -> &'static str {
        if self.offline {
            "离线"
        } else if self.closed {
            "已关闭"
        } else {
            "正常"
        }
    }

    /// 该行是否还能发起索引任务（离线根点「刷新」只会立刻失败）。
    pub fn can_index(&self) -> bool {
        !self.offline
    }
}

/// 「排除项」标签里的一行（§4.1 第三行 / §6.1 `ExclusionView`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExclusionView {
    pub id: i64,
    pub root_id: i64,
    /// 完整可显示路径（根 + 相对路径）
    pub path: String,
    /// **当下**路径不存在（§4.4 角标的分量之一）
    pub path_missing: bool,
}

/// 「存储与诊断」标签的一屏（§4.1 第四行 / §6.1 `StorageView`）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StorageView {
    /// 主库文件字节（`-wal` 不计）
    pub db_bytes: u64,
    pub wal_bytes: u64,
    pub thumb_bytes: u64,
    pub thumb_count: u64,
    pub photo_count: u64,
    /// 缩略图占用按根分布（`(根路径, 字节)`）
    pub by_root: Vec<(String, u64)>,
    /// `thumb_count` / `thumb_bytes` / `by_root` 是否已由后台线程填上。
    ///
    /// `false` 时界面必须显示「计算中…」，**不能显示 0**——显示 0 会被读成
    /// 「你没有缩略图」，而真相只是还没算完。
    pub thumb_stats_known: bool,
}

impl StorageView {
    /// 库文件 + WAL 的总占用。
    pub fn total_bytes(&self) -> u64 {
        self.db_bytes + self.wal_bytes
    }
}

/// 缩略图占用统计。**读 BLOB，慢**——只能由后台线程产出（[`collect_storage`]）。
///
/// 单独成一个结构而不是塞进 [`StorageView`]，就是为了让「快的部分」和「慢的部分」
/// 在类型上分开：快的部分可以每秒刷，慢的部分只能异步算、算完再合并。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThumbStorage {
    pub thumb_count: u64,
    pub thumb_bytes: u64,
    /// `根 id → 字节`
    pub by_root: Vec<(i64, u64)>,
}

/// 后台线程入口：把 [`ThumbStorage`] 算出来。
///
/// ⚠️ 调这个会锁住 `Db` 一段时间（冷缓存首次可达 ~1 s），**不要**在 UI 线程调。
pub fn collect_storage(d: &db::Db, root_ids: &[i64]) -> Result<ThumbStorage> {
    let by_root = d.root_thumb_bytes(root_ids)?;
    let cache = d.thumb_cache_stats()?;
    Ok(ThumbStorage {
        thumb_count: cache.0,
        thumb_bytes: cache.1,
        by_root: root_ids
            .iter()
            .map(|&id| (id, by_root.get(&id).copied().unwrap_or(0)))
            .collect(),
    })
}

/// 把后台算出的 [`ThumbStorage`] 合并进快照（UI 每帧刷新后调一次即可，开销为零）。
pub fn apply_storage(snap: &mut MgmtSnapshot, st: &ThumbStorage) {
    snap.storage.thumb_count = st.thumb_count;
    snap.storage.thumb_bytes = st.thumb_bytes;
    snap.storage.thumb_stats_known = true;
    snap.storage.by_root = snap
        .roots
        .iter()
        .map(|r| {
            let bytes = st
                .by_root
                .iter()
                .find(|(id, _)| *id == r.id)
                .map(|(_, b)| *b)
                .unwrap_or(0);
            (r.path.clone(), bytes)
        })
        .collect();
    for r in &mut snap.roots {
        r.thumb_bytes = st
            .by_root
            .iter()
            .find(|(id, _)| *id == r.id)
            .map(|(_, b)| *b)
            .unwrap_or(0);
    }
}

/// 索引管理页的四个标签（§4.1）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MgmtTab {
    #[default]
    Tasks,
    Folders,
    Exclusions,
    Storage,
}

impl MgmtTab {
    pub const ALL: [Self; 4] = [Self::Tasks, Self::Folders, Self::Exclusions, Self::Storage];

    pub fn label(self) -> &'static str {
        match self {
            Self::Tasks => "任务",
            Self::Folders => "关注文件夹",
            Self::Exclusions => "排除项",
            Self::Storage => "存储与诊断",
        }
    }
}

/// §6.1 的 `MgmtSnapshot`——索引管理页、状态栏入口、右下角弹框的**同一份**数据源。
///
/// ⚠️ 字段名与文档对齐，语义不可改；字段名在文档里标注了「可改」的除外。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MgmtSnapshot {
    /// 单调递增序号。`TC-M22-IDX-011` 拿它判定「同一刷新周期」。
    pub seq: u64,
    /// 进行中 + 排队 + 最近 [`HISTORY_KEEP`] 条历史
    pub tasks: Vec<TaskView>,
    pub roots: Vec<RootView>,
    /// 树的状态叠层来源
    pub folder_states: HashMap<i64, FolderState>,
    /// `direct_count`；树与组头**都**用这个
    pub folder_counts: HashMap<i64, u32>,
    /// 「所有照片」（只数 active 根，含失败项与离线项，不含已关闭子目录）
    pub all_photos: u32,
    /// 五类失败，下标 = `kind - 1`
    pub failures_by_kind: [FailureGroup; 5],
    pub exclusions: Vec<ExclusionView>,
    /// §4.4 算好的角标
    pub badge: u32,
    /// 角标的「有未忽略失败项的任务数」分量（悬停文案用）。
    ///
    /// 存进快照而不是让 UI 从总数反推：反推在「角标封顶 99+」时会算错，
    /// 而悬停文案说错一个数，用户就会怀疑整个角标是假的。
    pub failed_task_count: u64,
    pub storage: StorageView,
    /// 系统暂停态（P-03：只读，不给「继续」）
    pub paused_by_system: bool,
    /// 数据集版本号：只在照片集合**增删**时自增。
    ///
    /// PERF-09 的判据——`Layout` 只在它变化时才重算，快照刷新（每 ≤ 4 次/秒）
    /// 不算变化。
    pub dataset_version: u64,
}

impl MgmtSnapshot {
    /// §4.4 角标：未忽略失败的任务数 + 失效排除项数 + 离线根数。
    ///
    /// ⚠️ 第一个分量是**任务数**不是照片数（文档原文 `count_tasks()`）。
    /// 一个目录下 970 张失败只算 1——角标是「有几件事要你处理」，
    /// 不是「有几张照片坏了」，后者进失败分组明细。
    pub fn compute_badge(&mut self, failed_task_count: u64, missing_exclusions: u64) {
        let offline_roots = self.roots.iter().filter(|r| r.offline && !r.closed).count() as u64;
        self.badge =
            (failed_task_count + missing_exclusions + offline_roots).min(u32::MAX as u64) as u32;
    }

    /// 角标显示文案：≥ 100 显示「99+」（§4.4）。
    pub fn badge_text(&self) -> String {
        if self.badge == 0 {
            String::new()
        } else if self.badge >= 100 {
            "99+".to_string()
        } else {
            self.badge.to_string()
        }
    }

    /// 悬停文案：三行，为 0 的分量省略（§4.4）。
    pub fn badge_tooltip(&self, failed_task_count: u64) -> Vec<String> {
        let mut out = Vec::new();
        if failed_task_count > 0 {
            out.push(format!("{} 个任务有失败项", failed_task_count));
        }
        let missing = self.exclusions.iter().filter(|e| e.path_missing).count();
        if missing > 0 {
            out.push(format!("{} 条排除项已失效", missing));
        }
        let offline = self.roots.iter().filter(|r| r.offline && !r.closed).count();
        if offline > 0 {
            out.push(format!("{} 个根目录离线", offline));
        }
        out
    }

    /// 点开默认定位到哪个标签：「有问题的那个」（§4.2 M22-IDX-02）。
    ///
    /// 优先级按用户处理动线排：先看是不是有任务挂了 → 再看盘是不是没插 →
    /// 再看排除项失效 → 都没有就停在任务页。
    pub fn default_tab(&self, failed_task_count: u64) -> MgmtTab {
        if failed_task_count > 0 {
            return MgmtTab::Tasks;
        }
        if self.roots.iter().any(|r| r.offline && !r.closed) {
            return MgmtTab::Folders;
        }
        if self.exclusions.iter().any(|e| e.path_missing) {
            return MgmtTab::Exclusions;
        }
        MgmtTab::Tasks
    }

    /// 未忽略的失败条数（五类合计）。
    pub fn open_failures(&self) -> u64 {
        self.failures_by_kind.iter().map(|g| g.count).sum()
    }

    /// 已忽略的失败条数。**仍在列表可见**（R-M2-4），只是不进角标。
    pub fn ignored_failures(&self) -> u64 {
        self.failures_by_kind.iter().map(|g| g.ignored).sum()
    }

    /// 某个分类是否值得给「重试全部」按钮：`Unsupported` 是唯一的永久失败。
    pub fn group_retryable(&self, kind: FailureKind) -> bool {
        kind.retryable() && self.failures_by_kind[(kind.code() - 1) as usize].count > 0
    }

    pub fn root_by_id(&self, id: i64) -> Option<&RootView> {
        self.roots.iter().find(|r| r.id == id)
    }

    /// 状态栏入口的四态之一（§2.6）。四态互斥。
    pub fn entry_state(&self) -> IndexEntryState {
        if self.badge > 0 {
            // R-M2-4：失败优先于进度
            IndexEntryState::Attention
        } else if self.paused_by_system {
            IndexEntryState::Paused
        } else if self.tasks.iter().any(|t| t.running) {
            IndexEntryState::Running
        } else {
            IndexEntryState::Idle
        }
    }
}

/// 状态栏索引入口的四态（§2.6，图标 I-12）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexEntryState {
    Idle,
    Running,
    Paused,
    Attention,
}

impl IndexEntryState {
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Idle => "index-idle",
            Self::Running => "index-running",
            Self::Paused => "index-paused",
            Self::Attention => "index-attention",
        }
    }

    /// 是否要画环形指示器（只有 `Running` 要；**无百分比数字**）。
    pub fn shows_spinner(self) -> bool {
        matches!(self, Self::Running)
    }

    /// 是否要画红色角标数字。
    pub fn shows_badge(self) -> bool {
        matches!(self, Self::Attention)
    }
}

// ---------------------------------------------------------------- 离线判定（§4.6）

/// 一个根路径现在**在不在**。
///
/// ## 判据
///
/// §4.6 原文是「卷不存在（macOS 下 `/Volumes/<x>` 不存在或 `statfs` 失败）」。
/// 落到跨平台代码上就是两步：
///
/// 1. 路径**存不存在**（`Path::exists` 走 `stat`，拔掉的挂载点返回 false）；
/// 2. 存在但**读不进去**（`read_dir` 失败）——覆盖「挂载点还在但卷已经死了」这种
///    macOS 上很常见的半死状态。
///
/// 两步任一失败即离线。判成离线的代价只是「树节点变灰」，
/// 而漏判的代价是「用户看到一堆永远转圈的任务」——**宁可误判为离线**。
///
/// ⚠️ **不落库**：这是每次快照现算的字段。落库就会出现「盘拔了又插回来，
/// 库里还写着离线」的脏状态。
pub fn path_is_offline(path: &Path) -> bool {
    if !path.exists() {
        return true;
    }
    // 挂载点还在但卷已死：`read_dir` 会失败（ENOTCONN / EIO / EACCES…）
    std::fs::read_dir(path).is_err()
}

/// 可注入的离线判定（测试用：不想真的拔盘）。
pub type OfflineProbe = Box<dyn Fn(&Path) -> bool + Send + Sync>;

/// 默认探针：真的去看文件系统。
pub fn default_offline_probe() -> OfflineProbe {
    Box::new(path_is_offline)
}

// ---------------------------------------------------------------- 快照构建

/// 快照构建器的输入：除了库，还有**不在库里**的运行时状态。
///
/// 之所以把这些做成参数而不是从库里读：任务板（`IndexBoard`）的进度是内存态
/// （M2.1 的设计），库里只有终态历史；离线判定要碰文件系统，不该混进 DB 查询。
pub struct SnapshotInput<'a> {
    pub seq: u64,
    /// 当前会话内的运行/排队任务（由 `IndexBoard` 提供）
    pub live: &'a [TaskView],
    /// 系统暂停态
    pub paused_by_system: bool,
    /// 离线探针（测试可注入）
    pub offline: &'a OfflineProbe,
    /// 上一份快照的数据集版本号（用于判定是否自增）
    pub prev_dataset_version: u64,
    /// 上一份快照的 `photos` 行数。
    ///
    /// 数据集版本号的**唯一**自增判据（PERF-09）：行数变了才说明照片集合真的增删过。
    /// 只比较「所有照片」是不够的——关闭/重开一个根会让那个数变化，
    /// 但栅格的行集合没变，不该触发 `Layout` 重算。
    pub prev_photo_count: u64,
}

/// 从库里读出一整份快照。
///
/// ⚠️ **UI 线程不许直接调**：这是一次十几个查询的批量读，属于 `ui_guard!`
/// 要审计的范畴。正常用法是快照线程以 ≤ [`SNAPSHOT_HZ_MAX`] 次/秒调用它，
/// UI 通过 watch channel 订阅结果。
pub fn build_snapshot(d: &db::Db, input: &SnapshotInput<'_>) -> Result<MgmtSnapshot> {
    // ---- 计数：**同一事务**（§6.1 约束 / R-M22-6）
    let (folder_counts, all_photos_all) = d.mgmt_counts()?;

    let root_stats = d.root_stats()?;
    let mut roots = Vec::with_capacity(root_stats.len());
    for rs in root_stats {
        roots.push(root_view(&rs, input.offline));
    }

    // ---- 失败分类（§4.5 五类）
    let groups = d.failure_groups()?;
    let mut failures_by_kind = [FailureGroup::default(); 5];
    for g in groups {
        if let Some(kind) = g.kind {
            let i = (kind - 1) as usize;
            if i < 5 {
                failures_by_kind[i] = g;
            }
        }
    }

    // ---- 排除项：`path_missing` 现算
    let rows = d.all_exclusions()?;
    let root_path: HashMap<i64, String> = roots.iter().map(|r| (r.id, r.path.clone())).collect();
    let mut exclusions = Vec::with_capacity(rows.len());
    for row in rows {
        exclusions.push(exclusion_view(
            &row,
            root_path.get(&row.root_id).map(|s| s.as_str()),
        ));
    }
    let missing_exclusions = exclusions.iter().filter(|e| e.path_missing).count() as u64;

    // ---- 存储（**只放不碰 BLOB 的那部分**）
    //
    // ⚠️ `thumb_count` / `thumb_bytes` / `by_root` 一律留空：它们要读缩略图 BLOB，
    // 冷缓存下是 90 秒级（见 `Db::root_thumb_bytes` 的实测表）。真正的死机就是这么来的——
    // 每点一次索引按钮，UI 线程同步跑一次全表 BLOB 扫描，管理页开着还按 4 Hz 反复跑。
    //
    // 这三个数由 [`collect_storage`] 在**后台线程**算，再用 [`apply_storage`]
    // 合并进来；UI 拿到 [StorageView::thumb_stats_known] = false 时显示「计算中…」。
    let storage = StorageView {
        db_bytes: d.db_bytes(),
        wal_bytes: d.wal_bytes(),
        thumb_bytes: 0,
        thumb_count: 0,
        photo_count: d.photo_count_total()?,
        by_root: Vec::new(),
        thumb_stats_known: false,
    };

    // ---- 任务：库里的历史 + 会话内的活任务
    let history = d.finished_tasks(HISTORY_KEEP)?;
    let mut tasks: Vec<TaskView> = input.live.to_vec();
    for t in history {
        tasks.push(task_view(&t, None));
    }

    // ---- 目录状态叠层：失败 + 欠账
    let mut folder_states = HashMap::new();
    let failed_dirs = d.folders_with_failures()?;
    for (folder_id, n) in failed_dirs {
        if n > 0 {
            folder_states.insert(folder_id, FolderState::Failed);
        }
    }
    for folder_id in folder_counts.keys() {
        folder_states.entry(*folder_id).or_insert(FolderState::Idle);
    }

    let mut snap = MgmtSnapshot {
        seq: input.seq,
        tasks,
        roots,
        folder_states,
        folder_counts,
        all_photos: all_photos_all.min(u32::MAX as u64) as u32,
        failures_by_kind,
        exclusions,
        badge: 0,
        failed_task_count: 0,
        storage,
        paused_by_system: input.paused_by_system,
        dataset_version: input.prev_dataset_version,
    };

    // 有未忽略失败的任务数（§4.4 的第一个分量）
    let failed_task_count = d.failed_task_count()?;
    snap.failed_task_count = failed_task_count;
    snap.compute_badge(failed_task_count, missing_exclusions);

    // 数据集版本号只在照片集合真的变了才动（PERF-09）：
    // 计数分布变化（A 目录 +1、B 目录 -1）不动，只有 photos 行数变了才自增，
    // 否则每 4 次/秒的快照刷新都会触发一次全量重排，5 万张库必卡。
    if snap.storage.photo_count != input.prev_photo_count {
        snap.dataset_version = input.prev_dataset_version + 1;
    } else {
        snap.dataset_version = input.prev_dataset_version;
    }

    Ok(snap)
}

fn root_view(rs: &RootStat, offline: &OfflineProbe) -> RootView {
    let path = PathBuf::from(&rs.path);
    RootView {
        id: rs.id,
        path: rs.path.clone(),
        closed: rs.state != 0,
        offline: offline(&path),
        photo_count: rs.photo_count,
        // 字节数慢，由 `apply_storage` 异步补；快照本身不带它。
        thumb_bytes: 0,
        failed: rs.failed,
        last_indexed_at: rs.last_indexed_at,
    }
}

fn exclusion_view(row: &ExclusionRow, root: Option<&str>) -> ExclusionView {
    let path = match root {
        Some(r) => {
            let mut p = PathBuf::from(r);
            p.push(&row.relative_path);
            p.to_string_lossy().into_owned()
        }
        None => row.relative_path.clone(),
    };
    ExclusionView {
        id: row.id,
        root_id: row.root_id,
        path_missing: !Path::new(&path).exists(),
        path,
    }
}

/// 把任务板（M2.1 的内存态进度）里的活任务翻成界面行。
///
/// ## 为什么要这一层
///
/// 库里只有**终态**历史（`index_tasks`），正在跑的任务活在 `IndexBoard` 的内存里
/// （M2.1 的设计：进度每帧变，不该每帧写库）。所以「任务」标签要同时看两个地方，
/// 而两边的字段形状不同——这个函数就是那个接缝。
///
/// ⚠️ 板上的任务**没有类型**（`TaskSnapshot` 不记 kind），所以这里写「索引」。
/// 类型精确化要等流水线（M2.2 迭代 2 / M3）把建任务与入板合并，本版不猜。
pub fn live_task_views(board: &crate::index::IndexSnapshot) -> Vec<TaskView> {
    let now = crate::index::now_secs();
    board
        .tasks
        .iter()
        .filter(|t| t.state.is_running())
        .map(|t| TaskView {
            // 板上没有 task_id（它按 path 认任务），给 0 表示「会话内任务」。
            // 界面据此知道：它不能走「按 id 重试」那条路。
            task_id: 0,
            root_id: None,
            scope: t.path.clone(),
            kind: "索引".to_string(),
            status: t.state.label().to_string(),
            running: true,
            discovered: t.discovered,
            completed: t.completed,
            failed: t.failed,
            stage: match t.state {
                IndexState::Scanning => "扫描中",
                IndexState::Thumbnailing => "生成缩略图",
                IndexState::Waiting => "排队中",
                _ => "—",
            }
            .to_string(),
            batch: Some(t.batch),
            started_at: Some(board.started_at as i64),
            elapsed_text: format_mmss(((now - t.last_update).max(0.0) * 1000.0) as i64),
            outcome: None,
        })
        .collect()
}

/// 把库里的一条任务行翻成界面行。
///
/// `elapsed_override`：活任务由调用方给（会话内算的已用时长），
/// 历史任务用 `duration_ms`。
fn task_view(t: &TaskRow, elapsed_override: Option<i64>) -> TaskView {
    let elapsed_ms = elapsed_override.or(t.duration_ms).unwrap_or(0).max(0);
    let state = IndexState::from_code(t.status);
    TaskView {
        task_id: t.task_id,
        root_id: t.root_id,
        scope: t.scope_path.clone(),
        kind: db::TaskKind::from_code(t.kind)
            .map(|k| k.label().to_string())
            .unwrap_or_else(|| "未知".to_string()),
        status: state
            .map(|s| s.label().to_string())
            .unwrap_or_else(|| "未知".to_string()),
        running: state.map(|s| s.is_running()).unwrap_or(false),
        discovered: t.processed.max(0) as u64,
        completed: t.processed.saturating_sub(t.failed).max(0) as u64,
        failed: t.failed.max(0) as u64,
        // 阶段由状态推：扫描态与生成态分别对应两个阶段，终态没有阶段
        stage: state
            .map(|s| match s {
                IndexState::Scanning => "扫描中",
                IndexState::Thumbnailing => "生成缩略图",
                _ => "—",
            })
            .unwrap_or("—")
            .to_string(),
        batch: None,
        started_at: t.started_at,
        elapsed_text: format_mmss(elapsed_ms),
        outcome: t.outcome().map(|o| o.label().to_string()),
    }
}

/// `mm:ss`。超过一小时显示 `h:mm:ss`（不截断——索引跑几小时是常事）。
fn format_mmss(ms: i64) -> String {
    let total = ms / 1000;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

// ---------------------------------------------------------------- 启动校验（§4.8）

/// 启动增量校验的一次运行计划。
///
/// §4.8：首屏有图之后 [`STARTUP_VERIFY_DELAY_MS`] 开始，对每个 active root
/// 以**每目录 [`VERIFY_DIR_INTERVAL_MS`]** 的限速做 `read_dir` + `stat`，
/// 与 `folder_cached_meta` 比对 `(size, mtime)`。
///
/// 之所以「限速」而不是全速跑完：M2.1 实测 4330 个目录全树遍历约 20 min。
/// 全速跑会长时间占着 I/O 线程，与用户正在做的事抢盘。限速后**更慢**，
/// 但文档明说「可接受，因为它不阻塞任何操作」。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VerifyPlan {
    pub roots: Vec<PathBuf>,
    pub delay_ms: u64,
    pub dir_interval_ms: u64,
}

impl VerifyPlan {
    /// 只为**活跃且在线**的根排计划：离线根校验必然全失败，
    /// 而失败会进角标——用户插着盘时凭空多一条「需要处理」。
    pub fn new(snap: &MgmtSnapshot) -> Self {
        Self {
            roots: snap
                .roots
                .iter()
                .filter(|r| !r.closed && !r.offline)
                .map(|r| PathBuf::from(&r.path))
                .collect(),
            delay_ms: STARTUP_VERIFY_DELAY_MS,
            dir_interval_ms: VERIFY_DIR_INTERVAL_MS,
        }
    }

    /// 直接按路径排计划（启动阶段用：那时还没有 `MgmtSnapshot`，
    /// 而快照本身要等管理页打开才刷）。
    pub fn from_root_paths(paths: impl IntoIterator<Item = std::path::PathBuf>) -> Self {
        Self {
            roots: paths.into_iter().collect(),
            delay_ms: STARTUP_VERIFY_DELAY_MS,
            dir_interval_ms: VERIFY_DIR_INTERVAL_MS,
        }
    }

    /// 要校验的目录数（= 计划里的根数，真正的目录展开在遍历时做）。
    pub fn root_count(&self) -> usize {
        self.roots.len()
    }

    /// 是否无事可做（全关或全离线）。
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

/// 校验进度（任务标签里显示「已检查 N 个目录」）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VerifyProgress {
    pub dirs_checked: u64,
    pub changed_files: u64,
    pub new_dirs: u64,
    pub done: bool,
}

impl VerifyProgress {
    /// 「已检查 N 个目录」的文案（§4.8 可见性要求）。
    pub fn text(&self) -> String {
        if self.done {
            format!(
                "已检查 {} 个目录，{} 个文件有变化",
                self.dirs_checked, self.changed_files
            )
        } else {
            format!("已检查 {} 个目录", self.dirs_checked)
        }
    }
}

// ---------------------------------------------------------------- 诊断日志导出（§4.9）

/// 导出诊断日志时的路径脱敏（§4.9「默认开」）。
///
/// 用 `DefaultHasher` 的前 8 位十六进制替换路径。不是加密，
/// 只是让「把日志发给别人」时不至于连自家目录结构一起发走。
pub fn anonymize_path(path: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    format!("{:08x}", h.finish() & 0xffff_ffff)
}

/// 导出目标目录的合法性检查（§4.9 第三条）。
///
/// ⚠️ 守的是 R-1：用户选的目录若落在某个 active root 之下，**拒绝**。
/// 听起来是多管闲事——用户自己选的目录凭什么不让写？因为那是**照片目录**，
/// 而在照片目录里出现任何新文件都算红线违规，`TC-M22-GUARD-003` 会红。
/// 与其事后解释「那是你自己选的」，不如事前拦下。
///
/// 返回 `Err` 时带的是可直接显示的文案。
pub fn check_export_dir(dir: &Path, snap: &MgmtSnapshot) -> std::result::Result<(), String> {
    for root in &snap.roots {
        if root.closed || root.offline {
            continue;
        }
        let rp = Path::new(&root.path);
        if dir.starts_with(rp) {
            return Err(format!(
                "不能导出到「{}」：该目录位于已索引的照片目录之下，\
                 在那里新建文件会违反「照片目录零写入」约束。请另选一个目录。",
                dir.display()
            ));
        }
    }
    Ok(())
}

/// 日志导出的文件名（§4.9）：`openpicasa-diag-yyyyMMdd-HHmmss.zip`。
pub fn diag_filename(unix_secs: i64) -> String {
    // 不引 chrono：只需要一个 UTC 时间戳格式化，手写避免多一个依赖。
    let (y, m, d, hh, mm, ss) = civil_from_unix(unix_secs);
    format!("openpicasa-diag-{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}.zip")
}

/// Unix 秒 → (年, 月, 日, 时, 分, 秒)（UTC）。Howard Hinnant 的 civil_from_days 算法。
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let hh = (rem / 3600) as u32;
    let mm = ((rem % 3600) / 60) as u32;
    let ss = (rem % 60) as u32;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, hh, mm, ss)
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个「永远在线」的探针（测试里不想碰真实文件系统）。
    #[allow(dead_code)] // 供后续用例与集成测试直接取用，不因单条用例未引用而告警
    fn always_online() -> OfflineProbe {
        Box::new(|_| false)
    }

    fn snap_roots(roots: Vec<RootView>) -> MgmtSnapshot {
        MgmtSnapshot {
            roots,
            ..Default::default()
        }
    }

    fn root(id: i64, path: &str, closed: bool, offline: bool) -> RootView {
        RootView {
            id,
            path: path.to_string(),
            closed,
            offline,
            ..Default::default()
        }
    }

    // ---- §4.4 角标

    #[test]
    fn badge_is_the_sum_of_three_components() {
        let mut s = snap_roots(vec![
            root(1, "/a", false, false),
            root(2, "/b", false, true), // 离线
            root(3, "/c", true, true),  // 已关闭 + 离线 → **不算**
        ]);
        s.exclusions = vec![
            ExclusionView {
                id: 1,
                root_id: 1,
                path: "/a/x".into(),
                path_missing: true,
            },
            ExclusionView {
                id: 2,
                root_id: 1,
                path: "/a/y".into(),
                path_missing: false,
            },
        ];
        s.compute_badge(3, 1);
        // 3 个失败任务 + 1 条失效排除项 + 1 个离线根（已关闭的不算）
        assert_eq!(
            s.badge, 5,
            "角标 = 失败任务 + 失效排除项 + 离线且未关闭的根"
        );
    }

    #[test]
    fn badge_text_caps_at_99_plus() {
        let mut s = MgmtSnapshot::default();
        assert_eq!(s.badge_text(), "", "0 不显示");
        s.badge = 42;
        assert_eq!(s.badge_text(), "42");
        s.badge = 99;
        assert_eq!(s.badge_text(), "99");
        s.badge = 100;
        assert_eq!(s.badge_text(), "99+", "≥100 显示 99+");
        s.badge = 5000;
        assert_eq!(s.badge_text(), "99+");
    }

    #[test]
    fn badge_tooltip_omits_zero_components() {
        let mut s = snap_roots(vec![root(1, "/a", false, true)]);
        s.exclusions = vec![ExclusionView {
            id: 1,
            root_id: 1,
            path: "/a/x".into(),
            path_missing: true,
        }];
        let t = s.badge_tooltip(0);
        assert_eq!(t.len(), 2, "失败任务为 0 → 那一行省略");
        assert!(t[0].contains("排除项"));
        assert!(t[1].contains("离线"));
    }

    #[test]
    fn default_tab_lands_on_the_broken_one() {
        let mut s = MgmtSnapshot::default();
        assert_eq!(s.default_tab(0), MgmtTab::Tasks, "都没问题 → 任务页");

        // 有离线根 → 关注文件夹
        s.roots = vec![root(1, "/a", false, true)];
        assert_eq!(s.default_tab(0), MgmtTab::Folders);

        // 失败任务优先级更高
        assert_eq!(s.default_tab(2), MgmtTab::Tasks);

        // 只有失效排除项 → 排除项页
        let s2 = MgmtSnapshot {
            exclusions: vec![ExclusionView {
                id: 1,
                root_id: 1,
                path: "/a/x".into(),
                path_missing: true,
            }],
            ..Default::default()
        };
        assert_eq!(s2.default_tab(0), MgmtTab::Exclusions);
    }

    // ---- §2.6 状态栏四态

    #[test]
    fn entry_state_is_mutually_exclusive_and_failure_wins() {
        let mut s = MgmtSnapshot::default();
        assert_eq!(s.entry_state(), IndexEntryState::Idle);

        s.tasks = vec![TaskView {
            task_id: 1,
            running: true,
            ..task_view_blank()
        }];
        assert_eq!(s.entry_state(), IndexEntryState::Running);

        // 暂停 + 正在跑 → 暂停（没有角标时）
        s.paused_by_system = true;
        assert_eq!(s.entry_state(), IndexEntryState::Paused);

        // R-M2-4：有失败 → 「需要处理」压过一切
        s.badge = 1;
        assert_eq!(s.entry_state(), IndexEntryState::Attention);

        assert!(IndexEntryState::Running.shows_spinner());
        assert!(!IndexEntryState::Attention.shows_spinner(), "角标态不转圈");
        assert!(IndexEntryState::Attention.shows_badge());
        assert!(!IndexEntryState::Running.shows_badge());
    }

    fn task_view_blank() -> TaskView {
        TaskView {
            task_id: 0,
            root_id: None,
            scope: String::new(),
            kind: String::new(),
            status: String::new(),
            running: false,
            discovered: 0,
            completed: 0,
            failed: 0,
            stage: String::new(),
            batch: None,
            started_at: None,
            elapsed_text: String::new(),
            outcome: None,
        }
    }

    // ---- FolderState

    #[test]
    fn folder_state_merge_prefers_failure_over_progress() {
        assert_eq!(
            FolderState::Indexing.merge(FolderState::Failed),
            FolderState::Failed,
            "R-M2-4：失败压过「正在索引」"
        );
        assert_eq!(
            FolderState::Failed.merge(FolderState::Paused),
            FolderState::Failed
        );
        assert_eq!(
            FolderState::Idle.merge(FolderState::Pending),
            FolderState::Pending
        );
        assert_eq!(FolderState::Failed.label(), "有失败项");
    }

    // ---- 离线判定

    #[test]
    fn offline_probe_is_injectable() {
        let offline: OfflineProbe = Box::new(|p| p.to_string_lossy().contains("/Volumes"));
        let rs = RootStat {
            id: 1,
            path: "/Volumes/Ext".into(),
            state: 0,
            photo_count: 10,
            failed: 0,
            last_indexed_at: None,
        };
        let v = root_view(&rs, &offline);
        assert!(v.offline);
        assert!(!v.can_index(), "离线根不该能发起任务");
        assert_eq!(v.status_text(), "离线");

        let rs2 = RootStat {
            path: "/Users/me/Pictures".into(),
            ..rs
        };
        let v2 = root_view(&rs2, &offline);
        assert!(!v2.offline);
        assert_eq!(v2.status_text(), "正常");
    }

    #[test]
    fn real_offline_probe_says_missing_path_is_offline() {
        // 一个几乎不可能存在的路径
        assert!(path_is_offline(Path::new(
            "/__openpicasa_definitely_not_here__/x"
        )));
        // 一定存在的路径
        assert!(!path_is_offline(Path::new("/")));
    }

    #[test]
    fn root_status_prefers_offline_over_closed() {
        let v = root(1, "/x", true, true);
        assert_eq!(
            v.status_text(),
            "离线",
            "拔了盘还显示「已关闭」会让用户去找不存在的开关"
        );
    }

    // ---- §4.8 启动校验

    #[test]
    fn verify_plan_skips_closed_and_offline_roots() {
        let s = snap_roots(vec![
            root(1, "/a", false, false), // 要
            root(2, "/b", true, false),  // 已关闭：不要
            root(3, "/c", false, true),  // 离线：不要（校验必然全失败，还污染角标）
            root(4, "/d", false, false), // 要
        ]);
        let plan = VerifyPlan::new(&s);
        assert_eq!(plan.root_count(), 2);
        assert_eq!(plan.roots[0], PathBuf::from("/a"));
        assert_eq!(plan.roots[1], PathBuf::from("/d"));
        assert_eq!(plan.delay_ms, STARTUP_VERIFY_DELAY_MS);
        assert_eq!(plan.dir_interval_ms, VERIFY_DIR_INTERVAL_MS);
    }

    #[test]
    fn verify_plan_can_be_empty() {
        let s = snap_roots(vec![root(1, "/a", true, false)]);
        assert!(VerifyPlan::new(&s).is_empty());
    }

    #[test]
    fn verify_progress_text_has_no_eta() {
        let p = VerifyProgress {
            dirs_checked: 128,
            changed_files: 3,
            new_dirs: 1,
            done: false,
        };
        let t = p.text();
        assert!(t.contains("128"), "进行中显示已检查目录数");
        assert!(!t.contains("预计"), "§4.8 沿用 ETA 禁令");
        let d = VerifyProgress { done: true, ..p };
        assert!(d.text().contains("3"));
    }

    // ---- §4.9 导出

    #[test]
    fn export_dir_under_an_active_root_is_rejected() {
        let s = snap_roots(vec![
            root(1, "/Photos", false, false),
            root(2, "/Volumes/Ext", false, true), // 离线：不拦
        ]);
        assert!(check_export_dir(Path::new("/Photos/sub"), &s).is_err());
        assert!(check_export_dir(Path::new("/Photos"), &s).is_err());
        // 离线根之下放行（盘都不在，写了也不落在照片目录的可观测快照里）
        assert!(check_export_dir(Path::new("/Volumes/Ext/x"), &s).is_ok());
        assert!(check_export_dir(Path::new("/Downloads"), &s).is_ok());
    }

    #[test]
    fn diag_filename_shape() {
        // 2026-11-01 12:34:56 UTC（`date -u -d "2026-11-01 12:34:56" +%s` 得来，别手算）
        let secs = 1_793_536_496;
        assert_eq!(diag_filename(secs), "openpicasa-diag-20261101-123456.zip");
    }

    #[test]
    fn anonymize_is_stable_and_short() {
        let a = anonymize_path("/Users/me/Photos/2019/img.jpg");
        assert_eq!(a.len(), 8);
        assert_eq!(a, anonymize_path("/Users/me/Photos/2019/img.jpg"));
        assert_ne!(a, anonymize_path("/Users/me/Photos/2019/img2.jpg"));
    }

    // ---- 时长格式

    #[test]
    fn elapsed_text_formats_mmss_and_hours() {
        assert_eq!(format_mmss(0), "00:00");
        assert_eq!(format_mmss(65_000), "01:05");
        assert_eq!(format_mmss(3_725_000), "1:02:05");
        assert_eq!(format_mmss(-5), "00:00", "负数不该显示成 -1:-1");
    }
}
