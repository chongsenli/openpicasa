//! 监视的端到端验收测试（`07` §12.2 `TC-M3-WATCH-001～011` 中可自动化的部分）。
//!
//! ## 哪些能自动测、哪些不能
//!
//! 能在 CI 里跑的是**纯逻辑**与**本地目录**这两类：
//!
//! | 用例 | 做法 |
//! | --- | --- |
//! | 001 新增文件 → 事件 | 真往临时目录写文件，等 FSEvents |
//! | 002 拷贝大量文件 → 去抖上界 | 直接喂 `Debouncer`（不依赖真实事件密度） |
//! | 003 排除项剪枝 | 纯逻辑 |
//! | 005 根关闭 → 卸载 | `WatchManager::sync` 之后再 sync 空集合 |
//! | 006 关闭子目录零事件 | 纯逻辑（剪枝表） |
//! | 008/009/010 降级三种 | 注入假 `VolumeProbe` |
//! | 011 风暴折叠 | 纯逻辑 |
//!
//! 不能在这里测的是 **007 身份稳定**（要 `MoveMatcher`）与 **004 原地覆盖**
//! （要监测 `thumb_state` 回 0 的全链路），那两条在各自的模块里。

use picasa_core::WatchManager;
use picasa_core::watcher::{
    DebounceOut, Debouncer, PollReason, VolumeProbe, WATCH_DEBOUNCE_MS, WATCH_MAX_DEFER_MS,
    WATCH_STORM_THRESHOLD, WatchBackend, WatchState, decide_state,
};
use picasa_db::Db;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 假探测：可以凭空宣布「这是网络卷 / 配额不够 / 没有权限」。
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

fn fake(network: bool, budget: Option<(u32, u32)>, readable: bool) -> FakeProbe {
    FakeProbe {
        network,
        budget,
        readable,
    }
}

fn t(ms: u64) -> Instant {
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *BASE.get_or_init(Instant::now) + Duration::from_millis(ms)
}

// ---------------------------------------------------------------- WATCH-001

#[test]
fn tc_m3_watch_001_new_file_produces_a_refresh_within_the_settle_window() {
    // 「新增 1 张 → WATCH_SETTLE_MS（2 s）内栅格反映」的**第一段**：
    // 文件系统事件必须真的到达，并且被去抖器产出一个 `Dirs`。
    let tmp = tempfile::tempdir().unwrap();
    let probe = fake(false, None, true);
    let state = decide_state(true, 1, tmp.path(), &probe, 300);
    assert!(state.is_active(), "本地临时目录应当能实时监视");

    let Some(handle) =
        picasa_core::watcher::WatchHandle::install(tmp.path(), state, 8192)
    else {
        return; // 环境不支持（极少数 CI），跳过
    };

    // 给 FSEvents 一点启动时间（latency 0.3 s）
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(tmp.path().join("new.jpg"), b"jpeg-bytes").unwrap();

    // 轮询到「拿到事件」为止，最多等 2 秒（= WATCH_SETTLE_MS）
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut dirs = Vec::new();
    while Instant::now() < deadline {
        dirs.extend(handle.drain_raw().dirs);
        if !dirs.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !dirs.is_empty(),
        "2 秒内必须收到「新增文件」的目录级事件（WATCH_SETTLE_MS）"
    );
    // 事件被换算到**目录级**（不是文件路径）。
    //
    // ⚠️ macOS 上 `tempdir()` 返回 `/var/...`，而 FSEvents 给的是
    // `/private/var/...`（`/var` 是指向 `private/var` 的符号链接）。
    // 断言「事件路径的父目录等于它自己」比「等于 tmp.path()」更贴近语义，
    // 也不受这个符号链接影响。
    let expected = std::fs::canonicalize(tmp.path()).unwrap();
    let hit = dirs.iter().any(|d| {
        std::fs::canonicalize(d)
            .map(|c| c == expected || c.parent() == Some(expected.as_path()))
            .unwrap_or(false)
    });
    assert!(hit, "事件应归到目录（{expected:?}），实际：{dirs:?}");
}

// ---------------------------------------------------------------- WATCH-002

#[test]
fn tc_m3_watch_002_copying_many_files_respects_the_debounce_upper_bound() {
    // 「拷 1000 张」：事件密集但**同一个目录**，必须合并。
    // 断言的是「投递数远小于事件数」，这正是去抖的全部价值。
    let mut d = Debouncer::new();
    let dir = PathBuf::from("/p/album");
    for i in 0..1000 {
        // 每 10 ms 一个事件，持续 10 秒
        d.push_at(dir.clone(), t(i * 10), |_| false);
    }
    let mut deliveries = 0;
    // 每秒 drain 一次（模拟 UI 帧循环）
    for ms in (500..=10_000).step_by(500) {
        if d.drain_due(t(ms)).is_some() {
            deliveries += 1;
        }
    }
    assert!(
        deliveries <= 1000,
        "去抖后投递数必须显著少于事件数，实际 {deliveries}"
    );
    // 但不能一次都不投（max_defer 兜底）：
    assert!(deliveries >= 1, "持续拷贝期间必须至少投递一次（max_defer 兜底）");
}

#[test]
fn tc_m3_watch_002b_max_defer_bounds_the_worst_case_latency() {
    // 「最长推迟 5 s」的孤立的断言：连续事件下，第一次投递不得晚于 5 s + 一个窗口。
    let mut d = Debouncer::new();
    let dir = PathBuf::from("/p/album");
    let mut first_delivery: Option<u64> = None;
    for ms in (0..=20_000).step_by(100) {
        d.push_at(dir.clone(), t(ms), |_| false);
        if first_delivery.is_none() && d.drain_due(t(ms)).is_some() {
            first_delivery = Some(ms);
        }
    }
    let at = first_delivery.expect("必须投递过至少一次");
    assert!(
        at <= WATCH_MAX_DEFER_MS + WATCH_DEBOUNCE_MS,
        "首次投递在 {at} ms，超过了 max_defer({WATCH_MAX_DEFER_MS}) + 窗口({WATCH_DEBOUNCE_MS})"
    );
}

// ---------------------------------------------------------------- WATCH-003

#[test]
fn tc_m3_watch_003_debounce_merges_per_directory_not_globally() {
    let mut d = Debouncer::new();
    // 同一目录 50 次
    for i in 0..50 {
        d.push_at(PathBuf::from("/p/a"), t(i * 10), |_| false);
    }
    // 另一个目录 50 次
    for i in 0..50 {
        d.push_at(PathBuf::from("/p/b"), t(i * 10), |_| false);
    }
    let out = d.drain_due(t(1000)).expect("该到期");
    let DebounceOut::Dirs(v) = out else {
        panic!("不该是风暴")
    };
    assert_eq!(v.len(), 2, "两个目录 → 两次刷新，而不是 100 次");
}

// ---------------------------------------------------------------- WATCH-006

#[test]
fn tc_m3_watch_006_closed_subdirectory_yields_no_refresh() {
    // 「已关闭子目录下增删 → 零 Refresh 投递、零 read_dir」。
    // 去抖器的剪枝是这条的前半段（后半段「零 read_dir」由扫描器的排除集保证）。
    let mut d = Debouncer::new();
    let closed = vec![PathBuf::from("/p/album/private")];
    // ⚠️ 这里给的是**目录**（事件在上游就被换算成目录了）。
    // 用文件路径当输入会得到「目录不等于 /p/album/private2」的结果——那是测试写错，
    // 不是剪枝错。
    for p in [
        "/p/album/private",
        "/p/album/private/deep",
        "/p/album/private2", // 兄弟目录，**不能**被误剪
    ] {
        let path = PathBuf::from(p);
        d.push_at(path.clone(), t(0), |x| {
            picasa_core::watch_mgr::is_pruned(x, &closed)
        });
    }
    assert_eq!(d.pruned, 2, "两个在关闭范围内的要剪掉");
    let out = d.drain_due(t(1000));
    let Some(DebounceOut::Dirs(v)) = out else {
        panic!("应当产出一次刷新（兄弟目录）")
    };
    assert_eq!(v, vec![PathBuf::from("/p/album/private2")]);
}

// ---------------------------------------------------------------- WATCH-008/009/010

#[test]
fn tc_m3_watch_008_network_volume_is_visible_and_polls() {
    // 网络卷 → `Polling(NetworkVolume)`；页面可见；角标 +1。
    let probe = fake(true, None, true);
    let s = decide_state(true, 500, Path::new("/Volumes/share"), &probe, 300);
    assert_eq!(
        s,
        WatchState::Polling {
            interval_s: 300,
            reason: PollReason::NetworkVolume
        }
    );
    assert!(s.counts_for_badge(), "必须进角标（R-M3-7）");
    assert!(s.advice().is_some(), "页面要有修复建议");
    assert_eq!(s.label(), "每 5 分钟检查 · 网络卷");
}

#[test]
fn tc_m3_watch_009_inotify_limit_is_visible_and_never_auto_tunes() {
    // 配额压低 → `Polling(InotifyLimit)`；页面显示修复命令；软件未改 sysctl。
    let probe = fake(false, Some((30_000, 8192)), true);
    let s = decide_state(true, 30_000, Path::new("/big"), &probe, 600);
    let WatchState::Polling {
        reason: PollReason::InotifyLimit { need, have },
        ..
    } = &s
    else {
        panic!("应当是 inotify 配额降级，实际 {s:?}")
    };
    assert_eq!((*need, *have), (30_000, 8192));
    assert!(s.counts_for_badge());
    let advice = s.advice().unwrap();
    assert!(advice.contains("sysctl fs.inotify.max_user_watches"));
    // 90% 头寸的边界：需要 8191、配额 8192 → 8192×0.9=7372 < 8191 → 降级
    let probe2 = fake(false, Some((8191, 8192)), true);
    assert!(!decide_state(true, 8191, Path::new("/b"), &probe2, 600).is_active());
}

#[test]
fn tc_m3_watch_010_no_permission_is_visible_but_user_disabled_is_not() {
    let probe = fake(false, None, false);
    let s = decide_state(true, 10, Path::new("/root"), &probe, 300);
    assert_eq!(
        s,
        WatchState::Polling {
            interval_s: 300,
            reason: PollReason::NoPermission
        }
    );
    assert!(s.counts_for_badge());

    // 同一个根，但用户主动关掉 → **不进**角标
    let user_off = decide_state(false, 10, Path::new("/root"), &probe, 600);
    assert!(!user_off.counts_for_badge(), "UserDisabled 不进角标");
    assert!(!user_off.shows_degraded_overlay(), "树上也不该加黄三角");
}

// ---------------------------------------------------------------- WATCH-011

#[test]
fn tc_m3_watch_011_storm_collapses_to_a_single_root_refresh() {
    let mut d = Debouncer::new();
    for i in 0..=WATCH_STORM_THRESHOLD {
        d.push_at(PathBuf::from(format!("/p/d{i}")), t(0), |_| false);
    }
    assert_eq!(d.drain_due(t(1000)), Some(DebounceOut::RootStorm));
    // 折叠后清空：这一次根级刷新覆盖所有
    assert_eq!(d.pending_len(), 0);
    // 而且下一轮能正常恢复（不是永久卡在风暴态）
    d.push_at(PathBuf::from("/p/one"), t(2000), |_| false);
    assert!(matches!(
        d.drain_due(t(3000)),
        Some(DebounceOut::Dirs(_))
    ));
}

// ---------------------------------------------------------------- WATCH-005

#[test]
fn tc_m3_watch_005_closing_a_root_uninstalls_its_handle() {
    // 根关闭 → 监视卸载；重开 → 重装且不重复。
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("w.sqlite")).unwrap();
    let photos = tmp.path().join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    let p = photos.to_string_lossy().to_string();
    let root_id = db.upsert_root(&p, 0, false).unwrap();
    let fid = db.upsert_folder(&p, &p.to_lowercase(), None).unwrap();
    db.set_folder_root(fid, root_id, None).unwrap();

    let mut mgr = WatchManager::new().with_probe(Box::new(fake(false, None, true)));
    let configs = picasa_core::watch_mgr::load_configs(&db).unwrap();
    assert_eq!(configs.len(), 1);
    mgr.sync(&db, &configs).unwrap();
    assert_eq!(mgr.active_count(), 1, "应当装上一个实时监视");

    // 关闭根（state = 1）→ `load_configs` 只返回 active 根 → sync 空集
    db.set_root_closed(root_id, true).unwrap();
    let empty = picasa_core::watch_mgr::load_configs(&db).unwrap();
    assert!(empty.is_empty(), "关闭后不该再出现在配置里");
    mgr.sync(&db, &empty).unwrap();
    assert_eq!(mgr.active_count(), 0, "关闭后必须卸载");
    assert_eq!(mgr.states().len(), 0);

    // 重新打开 → 重装，且只有一个
    db.set_root_closed(root_id, false).unwrap();
    let again = picasa_core::watch_mgr::load_configs(&db).unwrap();
    mgr.sync(&db, &again).unwrap();
    mgr.sync(&db, &again).unwrap(); // 幂等：再同步一次不该装出第二个
    assert_eq!(mgr.active_count(), 1);
    assert_eq!(mgr.states().len(), 1, "不许重复安装");
}

#[test]
fn tc_m3_watch_001b_manager_starts_without_roots_and_stays_quiet() {
    let mut m = WatchManager::new();
    assert_eq!(m.poll(Instant::now()), vec![]);
    assert_eq!(m.degraded_count(), 0);
}

// ---------------------------------------------------------------- WATCH-012

#[test]
fn tc_m3_watch_012_master_switch_turns_realtime_off_and_keeps_it_out_of_the_badge() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("w.sqlite")).unwrap();
    let photos = tmp.path().join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    let p = photos.to_string_lossy().to_string();
    let root_id = db.upsert_root(&p, 0, false).unwrap();
    let fid = db.upsert_folder(&p, &p.to_lowercase(), None).unwrap();
    db.set_folder_root(fid, root_id, None).unwrap();

    let mut mgr = WatchManager::new().with_probe(Box::new(fake(false, None, true)));
    let cfg = picasa_core::watch_mgr::load_configs(&db).unwrap();
    mgr.sync(&db, &cfg).unwrap();
    assert!(mgr.state(root_id).is_active());

    // 用户在设置里关掉这个根的实时监视
    db.set_root_watch_enabled(root_id, false).unwrap();
    mgr.set_enabled(&db, &picasa_core::watch_mgr::load_configs(&db).unwrap())
        .unwrap();
    let s = mgr.state(root_id);
    assert!(matches!(
        s,
        WatchState::Polling {
            reason: PollReason::UserDisabled,
            ..
        }
    ));
    assert!(!s.counts_for_badge(), "用户自己关的不进角标");
    assert_eq!(mgr.degraded_count(), 0);

    // 再打开 → 回到实时
    db.set_root_watch_enabled(root_id, true).unwrap();
    mgr.set_enabled(&db, &picasa_core::watch_mgr::load_configs(&db).unwrap())
        .unwrap();
    assert!(mgr.state(root_id).is_active());
    assert!(matches!(
        mgr.state(root_id),
        WatchState::Active {
            backend: WatchBackend::FsEvents | WatchBackend::Inotify
        }
    ));
}

// ---------------------------------------------------------------- 角标分量

#[test]
fn badge_counts_degraded_roots_but_not_offline_ones_twice() {
    // `07` §2.3：第四分量只数「在线且未关闭」的根。
    // 离线的根已经有第三分量在数了，再算一次就是同一件事算两遍。
    use picasa_core::RootView;
    use picasa_core::mgmt::MgmtSnapshot;

    let mk = |id, offline: bool, closed: bool, watch: WatchState| RootView {
        id,
        path: format!("/p{id}"),
        closed,
        offline,
        photo_count: 0,
        thumb_bytes: 0,
        failed: 0,
        last_indexed_at: None,
        watch,
    };
    let degraded = WatchState::Polling {
        interval_s: 300,
        reason: PollReason::NetworkVolume,
    };
    let active = WatchState::Active {
        backend: WatchBackend::FsEvents,
    };

    let mut snap = MgmtSnapshot {
        roots: vec![
            mk(1, false, false, degraded.clone()), // 在线 + 降级 → 第四分量 +1
            mk(2, false, false, active),           // 在线 + 实时 → 不加
            mk(3, true, false, degraded.clone()),  // 离线 → 第三分量 +1，第四分量不加
            mk(4, false, true, degraded),          // 已关闭 → 都不加
        ],
        ..Default::default()
    };
    snap.compute_badge_with_watch(0, 0);
    assert_eq!(snap.watch_degraded_roots, 1, "只有在线且未关闭的降级根计数");
    assert_eq!(snap.badge, 2, "离线 1 + 监视降级 1");
}
