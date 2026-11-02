//! 文件系统监视（`07` §2，P-12 落定）。
//!
//! ## 定位：监视**只回答「哪个目录可能变了」**
//!
//! 它不解析文件、不读 DB、不写 DB（R-M3-8）。事件去抖合并后投递一个
//! 「目录级刷新」，复用 M2.1 的右键刷新语义——**事件处理路径 = 已验证路径**，
//! 不引入第二套发现逻辑。
//!
//! ## 为什么「降级可见」是红线（R-M3-7）
//!
//! 监视会因为一堆与用户无关的原因失效：网络卷不支持事件、inotify 配额用完、
//! 权限不足、事件队列溢出。**这些都不是 bug，是环境**。但静默退化成轮询是缺陷——
//! 用户会以为「软件知道我改了什么」，实际上它只每 5 分钟看一眼。
//!
//! 所以每个根都有一个 [`WatchState`]，界面逐行可见、角标有分量。
//!
//! ## macOS 的 FSEvents 语义
//!
//! 「可能重复、可能合并、重命名表现为 delete+create」——所以这里**不解析事件类型**，
//! 只把「哪个目录变了」交给去抖器。猜事件类型反而容易产生幽灵条目。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// 去抖窗口：同一目录在这个窗口内的事件合并成一个 `Refresh`。
pub const WATCH_DEBOUNCE_MS: u64 = 500;

/// 最长推迟：窗口内**持续**有事件（正在拷 1000 张）时，最多推迟这么久就强制投递一次。
///
/// 没有它的话，「持续有事件」= 「永远不投递」，用户拷完一个文件夹却发现软件毫无反应。
pub const WATCH_MAX_DEFER_MS: u64 = 5_000;

/// 事件通道容量。满了 → 记 `Overflow`，该根降级为「等待全量刷新」。
pub const WATCH_EVENT_CHAN: usize = 8_192;

/// 单个根 1 秒内投递的 `Refresh` 超过这么多 → 折叠为一次根级全量刷新。
pub const WATCH_STORM_THRESHOLD: usize = 200;

/// 监视后端（`07` §2.2 WATCH-05）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchBackend {
    /// macOS：FSEvents（`latency` 0.3 s，**不用** kqueue）
    FsEvents,
    /// Linux：inotify 递归
    Inotify,
    /// 其余平台 / 兜底：限速轮询
    Poll,
}

impl WatchBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::FsEvents => "FSEvents",
            Self::Inotify => "inotify",
            Self::Poll => "轮询",
        }
    }

    /// 当前平台的首选后端。
    pub fn for_platform() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::FsEvents
        }
        #[cfg(target_os = "linux")]
        {
            Self::Inotify
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Self::Poll
        }
    }
}

/// 为什么在轮询（`07` §2.3）。
///
/// ⚠️ `UserDisabled` **不进角标**：用户自己关掉的东西不该变成「需要你处理」。
/// 其余四条都是「环境导致的能力降级」，必须可见。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PollReason {
    /// 网络卷（SMB / AFP / NFS / WebDAV …）：事件不可靠或根本不支持
    NetworkVolume,
    /// inotify 配额不够（`need` > `have` × 0.9）
    InotifyLimit { need: u32, have: u32 },
    /// 没有读权限（监视装不上）
    NoPermission,
    /// 后端报错（创建 watcher 失败、root 句柄丢失）
    BackendError(String),
    /// 用户主动关掉实时监视
    UserDisabled,
}

impl PollReason {
    /// 进角标吗（`07` §2.3 的公式）。
    pub fn counts_for_badge(&self) -> bool {
        !matches!(self, Self::UserDisabled)
    }

    /// 界面文案的「原因」部分。
    pub fn label(&self) -> &'static str {
        match self {
            Self::NetworkVolume => "网络卷",
            Self::InotifyLimit { .. } => "监视句柄不足",
            Self::NoPermission => "权限不足",
            Self::BackendError(_) => "监视器错误",
            Self::UserDisabled => "已关闭",
        }
    }

    /// 修复建议（`07` §2.2 WATCH-06：页面显示修复命令，**软件自己不改系统参数**）。
    pub fn advice(&self) -> Option<String> {
        match self {
            Self::NetworkVolume => Some(
                "网络卷（SMB / AFP / NFS）通常不支持文件系统事件，软件会按固定间隔检查。\
                 想要实时生效，可以把照片拷到本地磁盘再关注。"
                    .into(),
            ),
            Self::InotifyLimit { need, have } => Some(format!(
                "该目录树需要 {need} 个监视句柄，当前上限 {have}。\
                 可以在系统层面调高（软件不会自动修改系统设置）：\n\
                 sudo sysctl fs.inotify.max_user_watches={}",
                next_reasonable_watch_limit(*need)
            )),
            Self::NoPermission => Some(
                "没有读取该目录的权限。请在「系统设置 → 隐私与安全性 → 完全磁盘访问」里授权。"
                    .into(),
            ),
            Self::BackendError(e) => Some(format!("监视器启动失败：{e}。已退回定时检查。")),
            Self::UserDisabled => None,
        }
    }
}

/// 建议的 `max_user_watches`：留 20% 余量并向上取整到 1024 的倍数。
fn next_reasonable_watch_limit(need: u32) -> u32 {
    let padded = (u64::from(need) * 12 / 10).max(1024);
    ((padded.div_ceil(1024)) * 1024) as u32
}

/// 一个根目录的监视状态（`07` §2.3，**UI 不另造词**）。
///
/// `Default = Off`：还没装监视时**如实**显示「未监视」，不假装在实时。
/// 这条看起来琐碎，但决定了「装监视失败」会不会被显示成一个正常状态。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WatchState {
    /// 实时监视中
    Active { backend: WatchBackend },
    /// 轮询中（降级）
    Polling { interval_s: u32, reason: PollReason },
    /// 事件过多，等待一次全量刷新（瞬态）
    Overflow,
    /// 未监视（离线 / 根已关闭 / 还没装）
    #[default]
    Off,
}

impl WatchState {
    /// 界面文案（**唯一措辞来源**）。
    pub fn label(&self) -> String {
        match self {
            Self::Active { .. } => "实时".into(),
            Self::Polling { interval_s, reason } => {
                let mins = (*interval_s as f32 / 60.0).round().max(1.0) as u32;
                format!("每 {mins} 分钟检查 · {}", reason.label())
            }
            Self::Overflow => "事件过多，等待全量刷新".into(),
            Self::Off => "未监视".into(),
        }
    }

    /// 进角标吗（`07` §2.3 的公式：`Polling(reason≠UserDisabled)` 与 `Overflow` 进）。
    pub fn counts_for_badge(&self) -> bool {
        match self {
            Self::Active { .. } | Self::Off => false,
            Self::Overflow => true,
            Self::Polling { reason, .. } => reason.counts_for_badge(),
        }
    }

    /// 树上根节点要不要加 S-05 黄色三角叠层（`07` UI-02）。
    pub fn shows_degraded_overlay(&self) -> bool {
        self.counts_for_badge()
    }

    /// 需要「为什么？」链接展开修复建议吗。
    pub fn advice(&self) -> Option<String> {
        match self {
            Self::Polling { reason, .. } => reason.advice(),
            Self::Overflow => Some(
                "短时间内有太多文件变化，已改为一次性重新检查这个文件夹。完成后会自动恢复实时监视。"
                    .into(),
            ),
            _ => None,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active { .. })
    }
}

/// 卷类型探测（`07` §2.2 WATCH-07）。
///
/// 测试可注入：真机走 `df`，用例给假的网络卷 / 配额值——
/// 这样降级路径能被**直接**覆盖，不必真的挂一个 SMB 共享。
pub trait VolumeProbe {
    /// 是不是网络卷（SMB / AFP / NFS / WebDAV / CIFS / FUSE）。
    fn is_network(&self, path: &Path) -> bool;
    /// inotify 剩余配额 `(需要, 可用)`。非 Linux 返回 `None`（不受此限）。
    fn inotify_budget(&self, dir_count: u32) -> Option<(u32, u32)>;
    /// 能不能读到这个目录（权限）。
    fn readable(&self, path: &Path) -> bool;
}

/// 真机探测。
pub struct SystemProbe;

impl VolumeProbe for SystemProbe {
    fn is_network(&self, path: &Path) -> bool {
        probe_network_fs(path)
    }

    fn inotify_budget(&self, dir_count: u32) -> Option<(u32, u32)> {
        read_inotify_budget(dir_count)
    }

    fn readable(&self, path: &Path) -> bool {
        std::fs::read_dir(path).is_ok()
    }
}

/// `statfs` 的 `f_fstypename` 是否属于网络文件系统。
///
/// ## 为什么用 `df` 而不是直接调 `statfs`
///
/// 本项目没有 `libc` 依赖（刻意保持依赖树小）。`df -P` 的输出解析在 macOS 与
/// Linux 上都够用，而且**只在装监视时跑一次**（不是热路径）。
///
/// 失败时保守地返回 `false`：宁可试着装监视（装不上会自己降级成 `BackendError`），
/// 也不要因为探测失败就把本地盘当成网络卷、白白退化成轮询。
#[cfg(unix)]
fn probe_network_fs(path: &Path) -> bool {
    const NETWORK_FS: &[&str] = &[
        "smbfs", "afpfs", "nfs", "nfs4", "webdav", "cifs", "fuse", "fuseblk", "osxfuse",
        "macfuse", "sshfs", "davfs",
    ];
    let Ok(out) = std::process::Command::new("df").arg("-P").arg(path).output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(line) = text.lines().nth(1) else {
        return false;
    };
    let fs = line
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    NETWORK_FS.iter().any(|n| fs.contains(n))
}

#[cfg(not(unix))]
fn probe_network_fs(_path: &Path) -> bool {
    false
}

/// 读 inotify 配额。**只在 Linux 上有意义**，其余返回 `None`。
///
/// `07` §2.2 WATCH-06：该根目录数 > 剩余配额 × 0.9 → 不安装，直接降级。
/// 软件**绝不**自己 `sysctl -w`（那是改系统参数）。
fn read_inotify_budget(dir_count: u32) -> Option<(u32, u32)> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches").ok()?;
        let have: u32 = text.trim().parse().ok()?;
        Some((dir_count, have))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dir_count;
        None
    }
}

/// 安装前的能力判定（`07` §2.2 WATCH-06/07/12）。
///
/// 给定「用户想不想开」「这个根有多少目录」，返回应该用哪种状态。
/// **纯函数**，所以降级路径能被用例直接覆盖，不必真的挂一个网络卷。
pub fn decide_state(
    watch_enabled: bool,
    dir_count: u32,
    path: &Path,
    probe: &dyn VolumeProbe,
    poll_interval_s: u32,
) -> WatchState {
    if !watch_enabled {
        return WatchState::Polling {
            interval_s: poll_interval_s,
            reason: PollReason::UserDisabled,
        };
    }
    if !probe.readable(path) {
        return WatchState::Polling {
            interval_s: poll_interval_s,
            reason: PollReason::NoPermission,
        };
    }
    if probe.is_network(path) {
        return WatchState::Polling {
            interval_s: poll_interval_s,
            reason: PollReason::NetworkVolume,
        };
    }
    // inotify 配额：需要 dir_count 个句柄，只用 90% 的头寸（`watch.inotify_headroom`）
    if let Some((need, have)) = probe.inotify_budget(dir_count) {
        let usable = (u64::from(have) * 9) / 10;
        if u64::from(need) > usable {
            return WatchState::Polling {
                interval_s: poll_interval_s,
                reason: PollReason::InotifyLimit { need, have },
            };
        }
    }
    WatchState::Active {
        backend: WatchBackend::for_platform(),
    }
}

// ---------------------------------------------------------------- 去抖器

/// 一个待投递的目录。
#[derive(Clone, Debug)]
struct Pending {
    /// 第一次事件到达的时刻（算「最长推迟」用）
    first: Instant,
    /// 最后一次事件到达的时刻（算「窗口」用）
    last: Instant,
}

/// 目录级去抖 + 风暴折叠（`07` §2.2 WATCH-02/03/11）。
///
/// ## 为什么是「目录级」而不是「事件级」
///
/// 一次 Finder 操作会连发十几个事件（创建、写、改属性、关闭、改目录 mtime），
/// 逐个处理等于把同一件事做十遍。而**目录级刷新**本身就是幂等的
/// （重扫一个目录的结果与扫几次无关），所以合并是安全的。
///
/// ## 剪枝要在**去抖前**做
///
/// `07` §2.2 WATCH-03：已关闭子目录的事件必须在去抖前丢弃。
/// 若先合并再剪枝，关闭的目录仍然占着去抖表（内存），而且会刷出一次无用的扫描。
#[derive(Debug, Default)]
pub struct Debouncer {
    /// 目录 → 待投递
    pending: HashMap<PathBuf, Pending>,
    /// 最近 1 秒内**新进入待投递表**的目录数（风暴判定）
    ///
    /// ⚠️ 判据是「1 秒内有多少个**不同目录**开始变化」，不是「drain 被调了几次」。
    /// 早先按 drain 次数算，一次 drain 带 300 个目录就漏判了——而那正是拷完整个
    /// 相册目录树时发生的事（`TC-M3-WATCH-011` 抓到过）。
    recent: Vec<Instant>,
    /// 这次 drain 该不该折叠成根级刷新（由 push 阶段的计数决定）
    storm_pending: bool,
    /// 累积的刷新数（诊断用）
    pub refreshes_sent: u64,
    /// 累积的剪枝丢弃数（诊断用）
    pub pruned: u64,
}

/// 去抖器的一次产出。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebounceOut {
    /// 刷新这些目录（正常路径）
    Dirs(Vec<PathBuf>),
    /// 风暴折叠：整根全量刷新一次（`07` WATCH-11）
    RootStorm,
}

impl Debouncer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 投递一个原始事件（`07` §9.1 的 `push`）。
    ///
    /// `excluded` 是剪枝判定：返回 `true` 表示「这个目录在关闭的子目录里，直接丢」。
    pub fn push_at(&mut self, dir: PathBuf, now: Instant, excluded: impl Fn(&Path) -> bool) {
        if excluded(&dir) {
            self.pruned += 1;
            return;
        }
        match self.pending.entry(dir) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.get_mut().last = now;
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(Pending {
                    first: now,
                    last: now,
                });
                // 新目录进表 → 记一次「1 秒内的变化目录数」
                self.recent.push(now);
                self.recent
                    .retain(|t| now.saturating_duration_since(*t) < Duration::from_secs(1));
                if self.recent.len() > WATCH_STORM_THRESHOLD {
                    self.storm_pending = true;
                }
            }
        }
    }

    /// 取出到期的（`07` §9.1 的 `drain_due`）。
    ///
    /// 两种到期条件，取先到的那个：
    /// 1. **静默够久**：距最后一次事件 ≥ [`WATCH_DEBOUNCE_MS`]
    /// 2. **推迟够久**：距第一次事件 ≥ [`WATCH_MAX_DEFER_MS`]（持续拷贝时防止永不投递）
    pub fn drain_due(&mut self, now: Instant) -> Option<DebounceOut> {
        if self.pending.is_empty() {
            return None;
        }
        let mut due: Vec<PathBuf> = Vec::new();
        for (dir, p) in &self.pending {
            let quiet = now.saturating_duration_since(p.last)
                >= Duration::from_millis(WATCH_DEBOUNCE_MS);
            let overdue = now.saturating_duration_since(p.first)
                >= Duration::from_millis(WATCH_MAX_DEFER_MS);
            if quiet || overdue {
                due.push(dir.clone());
            }
        }
        if due.is_empty() {
            return None;
        }

        // 风暴折叠：1 秒内变化的目录数超阈值 → 这一次直接发根级刷新。
        // 清空待投递表，因为根级刷新会覆盖它们全部（逐个发是纯浪费）。
        if self.storm_pending {
            self.pending.clear();
            self.recent.clear();
            self.storm_pending = false;
            self.refreshes_sent += 1;
            return Some(DebounceOut::RootStorm);
        }

        for d in &due {
            self.pending.remove(d);
        }
        self.refreshes_sent += due.len() as u64;
        Some(DebounceOut::Dirs(due))
    }

    /// 还有多少目录在等（界面「等待刷新」文案用）。
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// 清空（根关闭 / 卸载监视时）。
    pub fn clear(&mut self) {
        self.pending.clear();
        self.recent.clear();
    }
}

// ---------------------------------------------------------------- 运行中的监视

/// 一个根的监视句柄（`07` WATCH-01/05 的生命周期靠 RAII 守：drop 即卸载）。
pub struct WatchHandle {
    /// `Active` 时持有 OS 句柄；`Polling` / `Off` 时为 `None`
    _watcher: Option<RecommendedWatcher>,
    /// 事件缓冲（满了丢最旧，丢掉的计数会变成 `Overflow`）
    queue: std::sync::Arc<BoundedQueue>,
    pub state: WatchState,
    pub root: PathBuf,
}

impl WatchHandle {
    /// 装监视。`state` 由 [`decide_state`] 先算好：
    /// **先判定能力，再决定装不装**。直接装再处理失败会出现「装了一半、状态没更新」的中间态。
    pub fn install(root: &Path, state: WatchState, chan_cap: usize) -> Option<Self> {
        let queue = BoundedQueue::new(chan_cap);
        if !state.is_active() {
            // `Polling` / `Off`：不装 OS 句柄，由上层按 `interval_s` 触发快筛
            // （`07` §2.2 WATCH-08：轮询复用 L1，不做文件级 stat）。
            return Some(Self {
                _watcher: None,
                queue,
                state,
                root: root.to_path_buf(),
            });
        }

        let q = queue.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            for p in ev.paths {
                // 事件给的是「文件」或「目录」：统一换算到目录级
                let dir = if p.is_dir() {
                    p
                } else {
                    p.parent().map(|d| d.to_path_buf()).unwrap_or(p)
                };
                q.push(dir);
            }
        })
        .ok()?;

        if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
            tracing::warn!(root = %root.display(), error = %e, "无法监视该目录");
            // 装不上 → `BackendError`，**不静默**
            return Some(Self {
                _watcher: None,
                queue,
                state: WatchState::Polling {
                    interval_s: 600,
                    reason: PollReason::BackendError(e.to_string()),
                },
                root: root.to_path_buf(),
            });
        }
        Some(Self {
            _watcher: Some(watcher),
            queue,
            state,
            root: root.to_path_buf(),
        })
    }

    /// 取一批已到达的原始事件（非阻塞）。
    ///
    /// 通道满时**丢弃并记 `Overflow`**（`07` §2.1：不丢事件地假装没事）——
    /// 由调用方把 `state` 改成 `WatchState::Overflow` 并安排一次根级全量刷新。
    pub fn drain_raw(&self) -> DrainResult {
        // 一次最多取 4096：避免一个热目录把这一帧吃满
        let (dirs, dropped, more_pending) = self.queue.drain(4096);
        DrainResult {
            dirs,
            // 丢过事件 → 溢出，上层转 `Overflow` + 安排一次根级全量刷新。
            // **不能**当没发生过（R-M3-7：静默丢事件就是缺陷）。
            dropped,
            more_pending,
        }
    }

    pub fn watched_root(&self) -> &Path {
        &self.root
    }
}

/// [`WatchHandle::drain_raw`] 的结果。
pub struct DrainResult {
    pub dirs: Vec<PathBuf>,
    /// 因为缓冲满而**丢掉**的事件数 → 上层应转 `Overflow`（`07` §2.1）
    pub dropped: u64,
    /// 是否还有积压（下一帧继续取）
    pub more_pending: bool,
}

impl DrainResult {
    /// 需要转 `WatchState::Overflow` 吗。
    pub fn overflow(&self) -> bool {
        self.dropped > 0
    }
}

/// 有界事件缓冲。
///
/// ## 为什么不用 `sync_channel`
///
/// `watch.event_chan = 8192` 要的是**容量**，而 `std::sync::mpsc::sync_channel(n)`
/// 满了会让**发送方阻塞**。发送方是 notify 的 callback（跑在它的内部线程上），
/// 阻塞在那里等于把整个 FSEvents 事件循环卡住——那时连「丢弃」都做不到。
///
/// 所以这里是「满了就**丢最旧的**」：新事件比旧事件有用（旧目录马上会被
/// 下一次刷新覆盖），而且丢弃这个动作本身要能被观测到（`dropped` 计数 →
/// 上层转 `WatchState::Overflow`）。
struct BoundedQueue {
    inner: std::sync::Mutex<BoundedInner>,
}

struct BoundedInner {
    items: std::collections::VecDeque<PathBuf>,
    cap: usize,
    /// 因为满了而丢掉的条数（`07` §2.1：不丢事件地假装没事）
    dropped: u64,
}

impl BoundedQueue {
    fn new(cap: usize) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            inner: std::sync::Mutex::new(BoundedInner {
                items: std::collections::VecDeque::with_capacity(cap.min(1024)),
                cap: cap.max(1),
                dropped: 0,
            }),
        })
    }

    fn push(&self, p: PathBuf) {
        let Ok(mut g) = self.inner.lock() else { return };
        if g.items.len() >= g.cap {
            g.items.pop_front();
            g.dropped += 1;
        }
        g.items.push_back(p);
    }

    fn drain(&self, max: usize) -> (Vec<PathBuf>, u64, bool) {
        let Ok(mut g) = self.inner.lock() else {
            return (Vec::new(), 0, false);
        };
        let take = g.items.len().min(max);
        let out: Vec<PathBuf> = g.items.drain(..take).collect();
        let dropped = std::mem::take(&mut g.dropped);
        // 还有剩的说明这一轮没取完，上层应当继续取（但不该在同一帧里无限取）
        (out, dropped, !g.items.is_empty())
    }
}

// ---------------------------------------------------------------- 旧接口（兼容）

/// M2 的 `spawn_watch`：**保留**给仍在使用 `CoreEvent::FsChanged` 的路径。
///
/// ⚠️ M3 起新代码走 [`WatchHandle`] + [`Debouncer`]（带状态、带去抖、带降级可见）。
/// 这个函数保留只是为了 R-M22-5（零功能丢失）：删掉它会让 M2 的既有行为消失。
pub struct LegacyWatchHandle {
    _watcher: RecommendedWatcher,
    _thread: std::thread::JoinHandle<()>,
}

impl LegacyWatchHandle {
    pub fn spawn(roots: Vec<PathBuf>, tx: Sender<crate::scanner::CoreEvent>) -> Option<Self> {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel::<PathBuf>();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    for p in ev.paths {
                        let dir = if p.is_dir() {
                            p
                        } else {
                            p.parent().map(|d| d.to_path_buf()).unwrap_or(p)
                        };
                        let _ = raw_tx.send(dir);
                    }
                }
            })
            .ok()?;
        for root in &roots {
            if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
                tracing::warn!(root = %root.display(), error = %e, "无法监视该目录");
            }
        }
        let debounce = Duration::from_millis(WATCH_DEBOUNCE_MS);
        let thread = std::thread::Builder::new()
            .name("op-watch".into())
            .spawn(move || {
                let mut pending: HashSet<PathBuf> = HashSet::new();
                let mut last: Option<Instant> = None;
                loop {
                    match raw_rx.recv_timeout(debounce) {
                        Ok(dir) => {
                            pending.insert(dir);
                            last = Some(Instant::now());
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            if pending.is_empty() {
                                continue;
                            }
                            let quiet = last.map(|t| t.elapsed() >= debounce).unwrap_or(true);
                            if !quiet {
                                continue;
                            }
                            let paths: Vec<String> = pending
                                .drain()
                                .map(|p| crate::paths::canonical_string(&p))
                                .collect();
                            if tx
                                .send(crate::scanner::CoreEvent::FsChanged { paths })
                                .is_err()
                            {
                                break;
                            }
                            last = None;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .ok()?;
        Some(Self {
            _watcher: watcher,
            _thread: thread,
        })
    }
}

/// 兼容旧调用点。
pub fn spawn_watch(
    roots: Vec<PathBuf>,
    tx: Sender<crate::scanner::CoreEvent>,
) -> Option<LegacyWatchHandle> {
    LegacyWatchHandle::spawn(roots, tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定基准的「时刻」。用 `Instant::now()` 反复取会让用例对真实时间敏感。
    fn t(ms: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        base + Duration::from_millis(ms)
    }

    struct FakeProbe {
        network: bool,
        budget: Option<(u32, u32)>,
        readable: bool,
    }

    impl VolumeProbe for FakeProbe {
        fn is_network(&self, _p: &Path) -> bool {
            self.network
        }
        fn inotify_budget(&self, _n: u32) -> Option<(u32, u32)> {
            self.budget
        }
        fn readable(&self, _p: &Path) -> bool {
            self.readable
        }
    }

    fn probe(network: bool, budget: Option<(u32, u32)>, readable: bool) -> FakeProbe {
        FakeProbe {
            network,
            budget,
            readable,
        }
    }

    #[test]
    fn debounce_merges_events_in_the_same_directory() {
        // WATCH-02：同一目录窗口内的事件合并成一次 Refresh。
        let mut d = Debouncer::new();
        let p = PathBuf::from("/p/a");
        d.push_at(p.clone(), t(0), |_| false);
        d.push_at(p.clone(), t(100), |_| false);
        d.push_at(p.clone(), t(200), |_| false);
        assert_eq!(d.drain_due(t(400)), None, "还没静默够");
        let out = d.drain_due(t(800)).expect("该到期了");
        assert_eq!(out, DebounceOut::Dirs(vec![p]));
        assert_eq!(d.pending_len(), 0);
    }

    #[test]
    fn debounce_keeps_different_directories_apart() {
        let mut d = Debouncer::new();
        d.push_at(PathBuf::from("/p/a"), t(0), |_| false);
        d.push_at(PathBuf::from("/p/b"), t(0), |_| false);
        let out = d.drain_due(t(800)).unwrap();
        let DebounceOut::Dirs(mut v) = out else {
            panic!("不该是风暴")
        };
        v.sort();
        assert_eq!(v, vec![PathBuf::from("/p/a"), PathBuf::from("/p/b")]);
    }

    #[test]
    fn continuous_copy_still_gets_flushed_by_max_defer() {
        // WATCH-02 后半句：窗口内**持续**有事件时最长推迟 5 s。
        // 没有这条，「拷 1000 张」的过程中事件永远不会停 → 一次都不投递。
        let mut d = Debouncer::new();
        let p = PathBuf::from("/p/a");
        for i in 0..20 {
            d.push_at(p.clone(), t(i * 400), |_| false);
        }
        let out = d.drain_due(t(7600));
        assert_eq!(out, Some(DebounceOut::Dirs(vec![p])));
    }

    #[test]
    fn excluded_directories_are_pruned_before_debouncing() {
        // WATCH-03：已关闭子目录的事件在**去抖前**丢弃。
        // 顺序错了会让关闭的目录占着去抖表，还会刷出一次无用扫描。
        let mut d = Debouncer::new();
        d.push_at(PathBuf::from("/p/closed"), t(0), |p| {
            p.to_string_lossy().contains("closed")
        });
        assert_eq!(d.pruned, 1);
        assert_eq!(d.pending_len(), 0, "剪掉的不该进待投递表");
        assert_eq!(d.drain_due(t(800)), None);
    }

    #[test]
    fn storm_collapses_to_one_root_refresh() {
        // WATCH-11：1 秒内 300 个目录变化 → 折叠为 1 次根级刷新。
        let mut d = Debouncer::new();
        for i in 0..300 {
            d.push_at(PathBuf::from(format!("/p/d{i}")), t(0), |_| false);
        }
        let out = d.drain_due(t(800));
        assert_eq!(out, Some(DebounceOut::RootStorm));
        assert_eq!(
            d.pending_len(),
            0,
            "风暴后待投递必须清空（根级刷新已覆盖）"
        );
    }

    #[test]
    fn user_disabled_does_not_count_for_the_badge() {
        // `07` §2.3：用户自己关掉的东西不该变成「需要你处理」。
        let s = WatchState::Polling {
            interval_s: 600,
            reason: PollReason::UserDisabled,
        };
        assert!(!s.counts_for_badge());
        assert!(!s.shows_degraded_overlay());
    }

    #[test]
    fn environmental_degradations_do_count_for_the_badge() {
        // R-M3-7：其余四条必须可见。
        for reason in [
            PollReason::NetworkVolume,
            PollReason::InotifyLimit { need: 900, have: 800 },
            PollReason::NoPermission,
            PollReason::BackendError("boom".into()),
        ] {
            let s = WatchState::Polling {
                interval_s: 300,
                reason: reason.clone(),
            };
            assert!(s.counts_for_badge(), "{reason:?} 必须进角标");
            assert!(s.shows_degraded_overlay(), "{reason:?} 树上要加 S-05");
            assert!(s.advice().is_some(), "{reason:?} 要给修复建议");
        }
        assert!(WatchState::Overflow.counts_for_badge());
        assert!(
            !WatchState::Active {
                backend: WatchBackend::FsEvents
            }
            .counts_for_badge()
        );
        assert!(!WatchState::Off.counts_for_badge());
    }

    #[test]
    fn network_volume_degrades_to_polling() {
        // WATCH-07
        let p = probe(true, None, true);
        let s = decide_state(true, 100, Path::new("/Volumes/smb"), &p, 300);
        assert_eq!(
            s,
            WatchState::Polling {
                interval_s: 300,
                reason: PollReason::NetworkVolume
            }
        );
    }

    #[test]
    fn inotify_limit_degrades_before_installing() {
        // WATCH-06：需要 5000，配额 4000 → 4000×0.9 = 3600 < 5000 → 不装
        let p = probe(false, Some((5000, 4000)), true);
        let s = decide_state(true, 5000, Path::new("/big"), &p, 600);
        assert!(matches!(
            s,
            WatchState::Polling {
                reason: PollReason::InotifyLimit { .. },
                ..
            }
        ));
        // 配额充足 → Active（留 90% 头寸的判断也要对）
        let p2 = probe(false, Some((5000, 10_000)), true);
        assert!(decide_state(true, 5000, Path::new("/big"), &p2, 600).is_active());
        // 刚好卡在 90% 边界上：需要 9000、配额 10000 → 9000 > 9000? 否 → Active
        let p3 = probe(false, Some((9000, 10_000)), true);
        assert!(decide_state(true, 9000, Path::new("/big"), &p3, 600).is_active());
        // 再多 1 个就超
        let p4 = probe(false, Some((9001, 10_000)), true);
        assert!(!decide_state(true, 9001, Path::new("/big"), &p4, 600).is_active());
    }

    #[test]
    fn no_permission_degrades() {
        let p = probe(false, None, false);
        let s = decide_state(true, 10, Path::new("/root"), &p, 300);
        assert_eq!(
            s,
            WatchState::Polling {
                interval_s: 300,
                reason: PollReason::NoPermission
            }
        );
    }

    #[test]
    fn user_switch_off_wins_over_everything() {
        // WATCH-12：总开关关掉 → 直接 UserDisabled，不再探测环境。
        let p = probe(true, Some((999_999, 1)), false);
        let s = decide_state(false, 10_000, Path::new("/x"), &p, 600);
        assert_eq!(
            s,
            WatchState::Polling {
                interval_s: 600,
                reason: PollReason::UserDisabled
            }
        );
    }

    #[test]
    fn inotify_advice_contains_a_command_but_we_never_run_it() {
        // WATCH-06：页面显示修复命令文本，**软件自己不改系统参数**。
        let s = WatchState::Polling {
            interval_s: 300,
            reason: PollReason::InotifyLimit {
                need: 50_000,
                have: 8192,
            },
        };
        let advice = s.advice().unwrap();
        assert!(advice.contains("sysctl"), "必须给出命令：{advice}");
        assert!(advice.contains("fs.inotify.max_user_watches"));
        // 建议值要够用且带余量：50000 × 1.2 = 60000 → 向上取整到 1024 → 60416
        assert!(advice.contains("60416"), "建议值该是 60416：{advice}");
        // 而且**只是一段文本**：本文件里不该有真正执行它的调用点。
        //
        // ⚠️ 这条断言不能直接 `include_str!` 后搜 "sysctl"——建议文本本身
        // 就含这个词，注释里也会提到它，会自己匹配自己。
        // 要看的是「可执行的语句」：去掉注释与字符串字面量之后，还剩不剩 `sysctl`。
        let src = include_str!("watcher.rs");
        let code_only: String = src
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let offenders: Vec<&str> = code_only
            .lines()
            .filter(|l| l.contains("sysctl") && !l.contains('"'))
            .collect();
        assert!(
            offenders.is_empty(),
            "R-M3：软件不许自己改系统参数，但发现了非字符串里的 sysctl：{offenders:?}"
        );
    }

    #[test]
    fn label_is_readable_and_has_no_debug_braces() {
        // 文案是给用户看的：不能漏出 `{` `}` 这种 Debug 格式的残渣。
        let cases = [
            WatchState::Active {
                backend: WatchBackend::FsEvents,
            },
            WatchState::Polling {
                interval_s: 300,
                reason: PollReason::NetworkVolume,
            },
            WatchState::Polling {
                interval_s: 600,
                reason: PollReason::UserDisabled,
            },
            WatchState::Overflow,
            WatchState::Off,
        ];
        for c in cases {
            let l = c.label();
            assert!(
                !l.contains('{') && !l.contains('}'),
                "文案漏了格式字符：{l}"
            );
            assert!(!l.is_empty());
        }
        assert_eq!(
            WatchState::Polling {
                interval_s: 300,
                reason: PollReason::NetworkVolume
            }
            .label(),
            "每 5 分钟检查 · 网络卷"
        );
    }

    #[test]
    fn backend_selection_follows_the_platform() {
        // WATCH-05：三后端各一条
        let b = WatchBackend::for_platform();
        #[cfg(target_os = "macos")]
        assert_eq!(b, WatchBackend::FsEvents);
        #[cfg(target_os = "linux")]
        assert_eq!(b, WatchBackend::Inotify);
        assert!(!b.label().is_empty());
    }

    #[test]
    fn installing_on_a_real_directory_yields_a_live_handle() {
        // 端到端：本地临时目录应当装得上（环境不支持时跳过，不让 CI 红）
        let tmp = tempfile::tempdir().unwrap();
        let p = probe(false, None, true);
        let state = decide_state(true, 1, tmp.path(), &p, 300);
        assert!(state.is_active(), "本地目录应当判定为可实时监视");
        let Some(h) = WatchHandle::install(tmp.path(), state, WATCH_EVENT_CHAN) else {
            return;
        };
        assert!(h.state.is_active());
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(tmp.path().join("new.jpg"), b"x").unwrap();
        // 给 FSEvents 一点时间（latency 0.3 s）
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !h.drain_raw().dirs.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("5 秒内没有收到文件变化事件");
    }

    #[test]
    fn polling_state_does_not_hold_os_handles() {
        // WATCH-08 / R-1：轮询根不装 OS 句柄。
        let tmp = tempfile::tempdir().unwrap();
        let state = WatchState::Polling {
            interval_s: 300,
            reason: PollReason::NetworkVolume,
        };
        let h = WatchHandle::install(tmp.path(), state.clone(), 16).unwrap();
        assert_eq!(h.state, state);
        assert!(h.drain_raw().dirs.is_empty(), "轮询根不该产生事件");
    }
}
