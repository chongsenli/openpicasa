//! 监视器管理：**每个活跃根一个 Watcher**，集中管理生命周期与状态（`07` §2.2）。
//!
//! ## 为什么要有这一层
//!
//! [`crate::watcher::WatchHandle`] 只管「一个根」：装、卸、取事件。
//! 但应用需要的是「一组根」的视角：
//!
//! - 根被关闭 / 删除 / 首次添加 → 对应句柄要装或卸（WATCH-01）
//! - 每根的事件要经各自的 [`Debouncer`] 合并（WATCH-02）
//! - 降级状态要能一次取出，填进快照（R-M3-7）
//! - 所有状态查询都在 UI 线程上发生，**不能**碰 DB（R-M3-8）
//!
//! ## 线程模型
//!
//! ```text
//! OS 事件回调（notify 内部线程）
//!      └─→ BoundedQueue（每根一个）
//!             └─→ [UI 线程每帧] WatchManager::poll()
//!                    └─→ Debouncer::push_at / drain_due
//!                           └─→ Vec<RefreshTarget>（交给 IndexService 排队）
//! ```
//!
//! 监视器自己**不扫描、不读库**。它只把「哪个目录可能变了」交出去，
//! 由既有的 `enqueue_refresh` 走已验收的扫描路径。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use picasa_db::Db;

use crate::Result;
use crate::watcher::{
    DebounceOut, Debouncer, DrainResult, PollReason, SystemProbe, VolumeProbe, WATCH_EVENT_CHAN,
    WatchHandle, WatchState, decide_state,
};

/// 一个根的监视配置（从 DB 的 `roots` 表读出来）。
#[derive(Clone, Debug)]
pub struct RootWatchConfig {
    pub root_id: i64,
    pub path: PathBuf,
    /// 用户是否想开实时监视（`roots.watch_enabled`）
    pub enabled: bool,
    /// 轮询间隔（`roots.poll_interval_s`，`None` = 用默认）
    pub poll_interval_s: Option<u32>,
}

/// 默认轮询间隔（网络卷 300 s ⚠️ P-24 待实测，本地 600 s）。
pub const POLL_INTERVAL_DEFAULT_S: u32 = 600;
/// 网络卷的轮询间隔（`07` 附录 A `watch.poll_interval_network_s`）。
pub const POLL_INTERVAL_NETWORK_S: u32 = 300;

/// 一次 poll 产出的「要刷新的目录」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchOut {
    /// 刷新这些目录（正常路径）
    Dirs(Vec<PathBuf>),
    /// 整根全量刷新（风暴折叠 / 溢出恢复，WATCH-11）
    Root { root_id: i64, path: PathBuf },
}

/// 监视器管理器。
pub struct WatchManager {
    handles: HashMap<i64, WatchHandle>,
    debouncers: HashMap<i64, Debouncer>,
    /// 上一次 poll 时刻（debounce 的判据）
    last_poll: Instant,
    /// 探测注入点（测试用假的网络卷 / 配额）
    probe: Box<dyn VolumeProbe + Send>,
    /// 待安排的根级全量刷新（溢出后）
    pending_root_refresh: Vec<i64>,
}

impl Default for WatchManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchManager {
    pub fn new() -> Self {
        Self {
            handles: HashMap::new(),
            debouncers: HashMap::new(),
            last_poll: Instant::now(),
            probe: Box::new(SystemProbe),
            pending_root_refresh: Vec::new(),
        }
    }

    /// 注入自定义探测（测试：模拟网络卷 / inotify 配额）。
    pub fn with_probe(mut self, probe: Box<dyn VolumeProbe + Send>) -> Self {
        self.probe = probe;
        self
    }

    /// 按当前根集合**同步**监视器（`07` WATCH-01：装 / 卸 / 重装不重复）。
    ///
    /// 幂等：同一批根调几次结果都一样。只处理「新增」与「消失」两类，
    /// 已装好且状态没变的**原样保留**——重装会让事件漏在缝里。
    pub fn sync(&mut self, db: &Db, configs: &[RootWatchConfig]) -> Result<()> {
        let want: HashMap<i64, &RootWatchConfig> =
            configs.iter().map(|c| (c.root_id, c)).collect();

        // 1) 卸掉不该有的
        let stale: Vec<i64> = self
            .handles
            .keys()
            .filter(|id| !want.contains_key(id))
            .copied()
            .collect();
        for id in stale {
            self.handles.remove(&id);
            self.debouncers.remove(&id);
        }

        // 2) 装上该有的
        for c in configs {
            if self.handles.contains_key(&c.root_id) {
                continue;
            }
            // 目录数决定 inotify 配额判断（WATCH-06）
            let dir_count = db
                .folder_counts()
                .map(|m| m.len() as u32)
                .unwrap_or(1)
                .max(1);
            let interval = c.poll_interval_s.unwrap_or_else(|| {
                if self.probe.is_network(&c.path) {
                    POLL_INTERVAL_NETWORK_S
                } else {
                    POLL_INTERVAL_DEFAULT_S
                }
            });
            let state = decide_state(
                c.enabled,
                dir_count,
                &c.path,
                self.probe.as_ref(),
                interval,
            );
            match WatchHandle::install(&c.path, state, WATCH_EVENT_CHAN) {
                Some(h) => {
                    tracing::info!(
                        root = %c.path.display(),
                        state = %h.state.label(),
                        "监视已安装"
                    );
                    self.debouncers.insert(c.root_id, Debouncer::new());
                    self.handles.insert(c.root_id, h);
                }
                None => {
                    // 装不上：留一个 `BackendError` 的壳，**不静默**
                    tracing::warn!(root = %c.path.display(), "监视安装失败，降级为轮询");
                    let fallback = WatchHandle::install(
                        &c.path,
                        WatchState::Polling {
                            interval_s: interval,
                            reason: PollReason::BackendError("安装失败".into()),
                        },
                        WATCH_EVENT_CHAN,
                    );
                    if let Some(h) = fallback {
                        self.debouncers.insert(c.root_id, Debouncer::new());
                        self.handles.insert(c.root_id, h);
                    }
                }
            }
        }
        Ok(())
    }

    /// 每个根的当前状态（填进快照用）。
    pub fn states(&self) -> HashMap<i64, WatchState> {
        self.handles
            .iter()
            .map(|(id, h)| (*id, h.state.clone()))
            .collect()
    }

    /// 某个根的状态。
    pub fn state(&self, root_id: i64) -> WatchState {
        self.handles
            .get(&root_id)
            .map(|h| h.state.clone())
            .unwrap_or(WatchState::Off)
    }

    /// 有多少根在实时监视（诊断/状态栏）。
    pub fn active_count(&self) -> usize {
        self.handles.values().filter(|h| h.state.is_active()).count()
    }

    /// 有多少根降级（R-M3-7 的角标分量）。
    pub fn degraded_count(&self) -> usize {
        self.handles
            .values()
            .filter(|h| h.state.counts_for_badge())
            .count()
    }

    /// 每帧调用一次：收取事件 → 去抖 → 产出要刷新的目录。
    ///
    /// `excluded` 决定某个目录是不是在「已关闭子目录」里（WATCH-03 剪枝）。
    ///
    /// ⚠️ 这个方法**不碰 DB**（R-M3-8 / WATCH-13）：`excluded` 由调用方用
    /// 内存里的排除集实现，不在这里查库。
    pub fn poll(&mut self, now: Instant) -> Vec<WatchOut> {
        let mut out = Vec::new();

        // 1) 收事件，喂进各自的去抖器
        for (root_id, handle) in &self.handles {
            let DrainResult {
                dirs,
                dropped,
                more_pending,
            } = handle.drain_raw();
            if dropped > 0 {
                // 丢事件 = 溢出。**不能**假装没事（`07` §2.1）：
                // 转 `Overflow` 并安排一次根级全量刷新。
                tracing::warn!(root_id, dropped, "监视事件溢出，安排全量刷新");
                self.pending_root_refresh.push(*root_id);
            }
            if dirs.is_empty() && !more_pending {
                continue;
            }
            if let Some(d) = self.debouncers.get_mut(root_id) {
                for dir in dirs {
                    d.push_at(dir, now, |_p| false);
                }
            }
        }

        // 2) 溢出 → `Overflow` 状态 + 根级刷新
        for root_id in std::mem::take(&mut self.pending_root_refresh) {
            if let Some(h) = self.handles.get_mut(&root_id) {
                h.state = WatchState::Overflow;
                out.push(WatchOut::Root {
                    root_id,
                    path: h.root.clone(),
                });
            }
        }

        // 3) 各去抖器取到期项
        for (root_id, d) in &mut self.debouncers {
            match d.drain_due(now) {
                Some(DebounceOut::Dirs(dirs)) => out.push(WatchOut::Dirs(dirs)),
                Some(DebounceOut::RootStorm) => {
                    if let Some(h) = self.handles.get_mut(root_id) {
                        h.state = WatchState::Overflow;
                        out.push(WatchOut::Root {
                            root_id: *root_id,
                            path: h.root.clone(),
                        });
                    }
                }
                None => {}
            }
        }

        self.last_poll = now;
        out
    }

    /// 根级刷新完成后调：把 `Overflow` 收回正常状态（`07` §2.3：
    /// 「瞬态，全量刷新完成后回到 0 或 1」）。
    pub fn note_root_refreshed(&mut self, root_id: i64, db: &Db, config: &RootWatchConfig) {
        let Some(h) = self.handles.get_mut(&root_id) else {
            return;
        };
        if h.state != WatchState::Overflow {
            return;
        }
        // 重新判定一次（环境可能在溢出期间变过，比如盘被拔了）
        let dir_count = db.folder_counts().map(|m| m.len() as u32).unwrap_or(1).max(1);
        let interval = config.poll_interval_s.unwrap_or(POLL_INTERVAL_DEFAULT_S);
        h.state = decide_state(
            config.enabled,
            dir_count,
            &config.path,
            self.probe.as_ref(),
            interval,
        );
        tracing::info!(root_id, state = %h.state.label(), "全量刷新完成，监视状态恢复");
    }

    /// 用户在设置页拨了总开关（WATCH-12）。
    pub fn set_enabled(&mut self, db: &Db, configs: &[RootWatchConfig]) -> Result<()> {
        // 直接重新同步：`decide_state` 会看出 `enabled == false` 并给出
        // `Polling{UserDisabled}`；反过来也会装回实时。
        for id in self.handles.keys().copied().collect::<Vec<_>>() {
            self.handles.remove(&id);
            self.debouncers.remove(&id);
        }
        self.sync(db, configs)
    }
}

/// 从 DB 读出每个根的监视配置（`roots.watch_enabled` / `poll_interval_s`）。
pub fn load_configs(db: &Db) -> Result<Vec<RootWatchConfig>> {
    let rows = db.root_watch_configs()?;
    Ok(rows
        .into_iter()
        .map(|r| RootWatchConfig {
            root_id: r.root_id,
            path: PathBuf::from(r.path),
            enabled: r.enabled,
            poll_interval_s: r.poll_interval_s.map(|v| v.max(1) as u32),
        })
        .collect())
}

/// 排除集剪枝（WATCH-03）：这个目录是否落在某个已关闭的子目录里。
///
/// 纯内存判断，不查库。`excluded_prefixes` 是「已关闭目录」的绝对路径前缀表。
pub fn is_pruned(dir: &Path, excluded_prefixes: &[PathBuf]) -> bool {
    excluded_prefixes.iter().any(|p| dir.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pruning_matches_descendants_not_siblings() {
        // WATCH-03：关闭 `a/b` 后，`a/b/c` 要剪掉，但 `a/bc` **不能**被误剪
        // （前缀匹配写成 `starts_with` 字符串版就会出这个错）。
        let closed = vec![PathBuf::from("/p/a/b")];
        assert!(is_pruned(Path::new("/p/a/b/c"), &closed));
        assert!(is_pruned(Path::new("/p/a/b"), &closed));
        assert!(!is_pruned(Path::new("/p/a/bc"), &closed), "兄弟目录不能误剪");
        assert!(!is_pruned(Path::new("/p/a"), &closed), "祖先不剪");
    }

    #[test]
    fn empty_exclusion_set_prunes_nothing() {
        assert!(!is_pruned(Path::new("/p/a"), &[]));
    }

    #[test]
    fn a_manager_with_no_roots_reports_nothing_degraded() {
        let m = WatchManager::new();
        assert_eq!(m.active_count(), 0);
        assert_eq!(m.degraded_count(), 0);
        assert_eq!(m.states().len(), 0);
        assert_eq!(m.state(42), WatchState::Off);
    }
}
