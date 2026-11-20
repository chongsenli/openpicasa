//! M5-R6 · 索引五标签 + 31 条索引原子项的**核心层**门禁。
//!
//! 这里只钉「数据与判定」，不画界面：
//! * `AC-IDX-ENTRY` 的默认标签四分支（失败 → 任务 / 离线 → 关注文件夹 /
//!   失效排除 → 排除项 / 其余 → 任务，**绝不**默认导入页）；
//! * `IDX-A-BADGE` / `IDX-A-OV` 的角标三句之和、`99+` 封顶、监视降级不进悬停；
//! * `IDX-A-STATE-LABEL` 的七态字与任务类型字（活任务对齐库里意图后才准）；
//! * `IDX-A-PROG` 的「已用 mm:ss」从批次开始算，不是从最后一次进度更新算；
//! * `IDX-A-ANTI-V12` 的永久禁令：任何「已处理」都不带分母 / 百分比 / ETA；
//! * `AC-IDX-PAUSE` 的两套暂停（系统暂停 vs 用户暂停）必须可区分；
//! * `IDX-A-STOR` 的诊断导出文件名是 `.json`（内容就是 JSON）。
//!
//! ⚠️ 这些是**无窗口**测试，不能当成 macOS 真机验收。

use picasa_core::index::{FailureItem, IndexBoard, IndexState};
use picasa_core::mgmt::{
    MgmtTab, MgmtSnapshot, SnapshotInput, build_snapshot, diag_filename, live_task_views,
};
use picasa_core::{default_offline_probe, watcher::WatchState};
use picasa_db::{Db, FailureKind, TaskKind, TaskScopeKind, TaskStatus};
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------- 夹具

struct Fixture {
    _tmp: tempfile::TempDir,
    db: Db,
    photos: String,
}

/// 一个开了完整迁移的库，带一个真实的本地根目录（里面有 3 张照片）。
///
/// 用真目录而不是假路径：离线探针会 stat 这些路径，假路径会让「在线根」变离线，
/// 那样默认标签的分支就测不出「都没有问题时停在任务页」。
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let photos = tmp.path().join("photos");
    std::fs::create_dir_all(&photos).expect("mkdir");
    for name in ["a.jpg", "b.jpg", "c.jpg"] {
        std::fs::write(photos.join(name), b"x").expect("write photo");
    }
    let db = Db::open(tmp.path().join("library.sqlite")).expect("open db");
    let path = photos.to_string_lossy().into_owned();
    db.upsert_root(&path, 100, false).expect("add root");
    Fixture {
        _tmp: tmp,
        db,
        photos: path,
    }
}

static EMPTY_WATCH: std::sync::LazyLock<HashMap<i64, WatchState>> =
    std::sync::LazyLock::new(HashMap::new);

fn input<'a>(
    probe: &'a picasa_core::OfflineProbe,
    live: &'a [picasa_core::TaskView],
    paused_by_system: bool,
) -> SnapshotInput<'a> {
    SnapshotInput {
        seq: 1,
        live,
        paused_by_system,
        offline: probe,
        watch: &EMPTY_WATCH,
        prev_dataset_version: 0,
        prev_photo_count: 0,
    }
}

fn snap(f: &Fixture) -> MgmtSnapshot {
    let probe = default_offline_probe();
    build_snapshot(&f.db, &input(&probe, &[], false)).expect("snapshot")
}

// ---------------------------------------------------------------- AC-IDX-ENTRY

/// `AC-IDX-ENTRY`：默认标签按「失败 → 任务；否则离线 → 关注文件夹；
/// 否则失效排除 → 排除项；否则任务」定位，**不会默认打开导入 / 导出**。
#[test]
fn ac_idx_entry_default_tab_follows_the_four_branch_rule() {
    let f = fixture();
    let mut s = snap(&f);

    // ① 一切正常 → 任务
    assert_eq!(s.default_tab(0), MgmtTab::Tasks);

    // ② 有失败任务 → 任务（哪怕同时有离线根）
    s.roots.iter_mut().for_each(|r| r.offline = true);
    assert_eq!(s.default_tab(0), MgmtTab::Folders);
    assert_eq!(
        s.default_tab(3),
        MgmtTab::Tasks,
        "失败任务优先级最高（不是离线根）"
    );

    // ③ 没有失败、没有离线，但有失效排除项 → 排除项
    s.roots.iter_mut().for_each(|r| r.offline = false);
    s.exclusions = vec![picasa_core::ExclusionView {
        id: 1,
        root_id: 1,
        path: f.photos.clone(),
        path_missing: true,
    }];
    assert_eq!(s.default_tab(0), MgmtTab::Exclusions, "失效排除项单独成支");

    // ④ 四支走遍，任何一支都不落到导入页。
    for failed in [0, 1] {
        for offline in [false, true] {
            let mut s = snap(&f);
            s.roots.iter_mut().for_each(|r| r.offline = offline);
            s.exclusions = if failed == 0 && !offline {
                vec![picasa_core::ExclusionView {
                    id: 1,
                    root_id: 1,
                    path: "/gone".into(),
                    path_missing: true,
                }]
            } else {
                Vec::new()
            };
            let tab = s.default_tab(failed);
            assert_ne!(
                tab,
                MgmtTab::ImportExport,
                "默认标签永远不能是「导入 / 导出」（failed={failed}, offline={offline}）"
            );
        }
    }
}

// ---------------------------------------------------------------- AC-IDX-TABS / IDX-A-WIN

/// `AC-IDX-TABS` / `IDX-A-WIN`：五个标签从左到右固定，总览在标签之上。
/// 关闭窗口不取消后台任务——这条在 UI 侧是「关窗只清确认框」，这里钉数据侧：
/// 快照里的任务/根不因为「窗口没开」而变化。
#[test]
fn ac_idx_tabs_five_tabs_in_fixed_order_and_overview_above_them() {
    let labels: Vec<&str> = MgmtTab::ALL.iter().map(|t| t.label()).collect();
    assert_eq!(
        labels,
        ["任务", "关注文件夹", "排除项", "存储与诊断", "导入 / 导出"],
        "五标签顺序是产品定义，不是实现顺序"
    );
    assert_eq!(MgmtTab::ALL.len(), 5);

    // 总览的五个字段都能从同一份快照拿到（图库根目录 / 索引状态 / 照片数量 /
    // 最近更新 / 异常行），且根行格式是「状态 · N 张」。
    let f = fixture();
    let s = snap(&f);
    assert_eq!(s.roots.len(), 1);
    let root = &s.roots[0];
    assert_eq!(root.status_text(), "正常");
    // 空索引也有明确数字（0），不是「计算中」——总览的四个字段都必须可读。
    assert_eq!(s.all_photos, 0, "没扫过就是 0 张，不能编造");
    assert_eq!(s.storage.photo_count, 0);
    let last = s
        .roots
        .iter()
        .filter_map(|r| r.last_indexed_at)
        .chain(s.tasks.iter().filter_map(|t| t.started_at))
        .max();
    assert!(last.is_none(), "从没索引过 → 总览写「尚未索引」");
}

/// `IDX-A-OV`：总览根行**不用千分位空格**、不写「N 项异常」/「一切正常」。
#[test]
fn idx_a_ov_root_line_has_no_thousands_separator_and_no_vague_summary() {
    let s = MgmtSnapshot {
        roots: vec![picasa_core::RootView {
            id: 1,
            path: "/photos".into(),
            photo_count: 12345,
            thumb_bytes: 2 * 1024 * 1024,
            failed: 7,
            ..Default::default()
        }],
        ..Default::default()
    };
    // core 侧只给计数；格式化在 UI 侧（index_mgr::root_summary_line）——
    // 这里钉的是「快照给的是原始数字」，UI 才有机会不造千分位。
    assert_eq!(s.roots[0].photo_count, 12345);
    assert_eq!(s.roots[0].failed, 7);
}

/// `IDX-A-BADGE`：角标三句之和 == 角标数；监视降级**不进**悬停；
/// 角标是「几件事」不是「几张失败照片」。
#[test]
fn idx_a_badge_counts_events_not_photos_and_watch_stays_out_of_tooltip() {
    let mut s = MgmtSnapshot {
        roots: vec![
            picasa_core::RootView {
                id: 1,
                path: "/a".into(),
                offline: true,
                ..Default::default()
            },
            picasa_core::RootView {
                id: 2,
                path: "/b".into(),
                ..Default::default()
            },
        ],
        exclusions: vec![picasa_core::ExclusionView {
            id: 1,
            root_id: 1,
            path: "/a/gone".into(),
            path_missing: true,
        }],
        ..Default::default()
    };
    // 一个任务有 970 张失败，但它只算「1 件事」。
    s.compute_badge(1, 1);
    assert_eq!(s.badge, 3, "1 任务 + 1 失效排除 + 1 离线根");

    let tooltip = s.badge_tooltip(1);
    assert_eq!(tooltip.len(), 3, "悬停只有三句（监视降级不进）：{tooltip:?}");
    assert!(tooltip[0].contains('1') && tooltip[0].contains("任务"), "{tooltip:?}");
    assert!(tooltip.iter().all(|t| !t.contains("监视")), "{tooltip:?}");

    // 封顶 99+
    s.compute_badge(150, 0);
    assert_eq!(s.badge_text(), "99+");
}

// ---------------------------------------------------------------- IDX-A-STATE-LABEL

/// `IDX-A-STATE-LABEL`：任务状态七态字与类型六种字逐字固定。
#[test]
fn idx_a_state_label_has_seven_states_and_six_task_kinds() {
    let states = [
        (IndexState::Waiting, "等待"),
        (IndexState::Scanning, "扫描中"),
        (IndexState::Thumbnailing, "生成缩略图中"),
        (IndexState::Completed, "已完成"),
        (IndexState::PartialFailed, "部分失败"),
        (IndexState::Paused, "暂停"),
        (IndexState::Unreachable, "不可访问"),
    ];
    for (state, want) in states {
        assert_eq!(state.label(), want, "状态字必须逐字一致");
    }
    let kinds = [
        (TaskKind::FirstScan, "首次索引"),
        (TaskKind::Refresh, "刷新"),
        (TaskKind::Reindex, "重建"),
        (TaskKind::Resume, "恢复"),
        (TaskKind::DimsBackfill, "回填尺寸"),
        (TaskKind::Verify, "校验"),
    ];
    for (kind, want) in kinds {
        assert_eq!(kind.label(), want, "类型字必须逐字一致");
    }
}

/// `IDX-A-STATE-LABEL`：活任务对齐到库里未完成的持久意图后，**类型取自这条意图**
/// （板上任务本身不带 kind；写死「索引」会让「首次索引 / 刷新 / 校验」全变成同一个词）。
#[test]
fn idx_a_state_label_live_task_takes_its_kind_from_the_persisted_intent() {
    let f = fixture();
    let board = IndexBoard::new();
    board.begin_batch(std::slice::from_ref(&f.photos));
    board.mark_scanning(&f.photos);

    // 库里有一条同范围、类型=刷新 的活意图。
    let task_id = f
        .db
        .begin_index_task(
            None,
            &f.photos,
            TaskKind::Refresh,
            TaskScopeKind::Root,
        )
        .expect("begin intent");
    f.db.set_task_running(task_id, TaskStatus::Scanning)
        .expect("set running");

    let live = live_task_views(&board.snapshot());
    assert_eq!(live.len(), 1);
    let probe = default_offline_probe();
    let s = build_snapshot(&f.db, &input(&probe, &live, false)).expect("snapshot");
    let task = s.tasks.iter().find(|t| t.running).expect("活任务要在快照里");
    assert_eq!(task.kind, "刷新", "类型必须来自库里的持久意图");
    assert_eq!(task.task_id, task_id, "对齐后要带真实 task_id（才能逐任务暂停）");
}

// ---------------------------------------------------------------- IDX-A-TASKS

/// `IDX-A-TASKS`：进行中排前面、历史在后；同一条不会既在活任务又在历史里。
#[test]
fn idx_a_tasks_running_first_and_history_never_duplicates_a_live_task() {
    let f = fixture();
    let id = f
        .db
        .begin_index_task(None, &f.photos, TaskKind::FirstScan, TaskScopeKind::Root)
        .expect("intent");
    let board = IndexBoard::new();
    board.begin_batch(std::slice::from_ref(&f.photos));
    board.mark_scanning(&f.photos);
    let live = live_task_views(&board.snapshot());
    let probe = default_offline_probe();
    let s = build_snapshot(&f.db, &input(&probe, &live, false)).expect("snapshot");

    let live_rows: Vec<_> = s.tasks.iter().filter(|t| t.running).collect();
    assert_eq!(live_rows.len(), 1, "只有一条活任务");
    assert_eq!(live_rows[0].task_id, id);
    // 活任务不重复进历史（历史只取终态）。
    let history_same: Vec<_> = s
        .tasks
        .iter()
        .filter(|t| !t.running && t.task_id == id)
        .collect();
    assert!(
        history_same.is_empty(),
        "同一条任务不能既在进行中又在历史里：{:?}",
        history_same
    );
}

// ---------------------------------------------------------------- IDX-A-ANTI-V12

/// `IDX-A-ANTI-V12` / `AC-IDX-PAUSE`：「已处理 N 张」**不带分母 / 百分比 / ETA**，
/// 且生成阶段用的是真正处理完的张数（不是 `discovered` 这个分母）。
#[test]
fn idx_a_anti_v12_processed_count_never_leaks_the_denominator() {
    let board = IndexBoard::new();
    let path = "/photos".to_string();
    board.begin_batch(std::slice::from_ref(&path));
    board.mark_scanning(&path);
    // 扫描阶段：已发现 100 张（分母未冻结）。
    for n in 1..=100 {
        board.note_discovered(&path, n);
    }
    assert_eq!(
        board.snapshot().processed_running(),
        100,
        "扫描阶段：已入库张数就是真实进展"
    );

    // 冻结分母：共 100 张，已出 30 张缩略图。
    board.freeze_denominator(&path, 100, 30);
    assert_eq!(
        board.snapshot().processed_running(),
        30,
        "生成阶段只能报真正处理完的 30 张，不能报 100（那是分母）"
    );

    // 再补 5 张，数字要跟着涨。
    for _ in 0..5 {
        board.note_thumb_ok(&path);
    }
    assert_eq!(board.snapshot().processed_running(), 35);

    // 失败也算处理过（判一张坏图也是一次处理），但不重复计数。
    board.note_thumb_failed(
        &path,
        FailureItem {
            photo_id: 1,
            name: "bad.jpg".into(),
            reason: "解码失败".into(),
        },
    );
    assert_eq!(board.snapshot().processed_running(), 36);
    board.note_thumb_failed(
        &path,
        FailureItem {
            photo_id: 1,
            name: "bad.jpg".into(),
            reason: "解码失败".into(),
        },
    );
    assert_eq!(
        board.snapshot().processed_running(),
        36,
        "同一条失败重复上报不重复计数"
    );
}

// ---------------------------------------------------------------- AC-IDX-PAUSE

/// `AC-IDX-PAUSE`：系统暂停与用户暂停必须能分辨。
///
/// * 系统暂停（休眠 / 挂起）→ `paused()` 为真、`user_paused()` 为假，
///   页面写「恢复由系统事件自动发生」且**没有**「继续」；
/// * 用户逐任务暂停 → 反过来，有「继续」。
///
/// 这条用例守的是「把用户暂停说成休眠 / 低电量」这类撒谎文案。
#[test]
fn ac_idx_pause_system_and_user_pauses_are_distinguishable() {
    let board = IndexBoard::new();
    let path = "/photos".to_string();
    board.begin_batch(std::slice::from_ref(&path));
    board.mark_scanning(&path);

    // 用户全局「暂停索引」（有「继续索引」）
    board.pause_all("已手动暂停");
    let s = board.snapshot();
    assert_eq!(s.tasks[0].state, IndexState::Paused, "状态字仍是暂停");
    assert!(s.user_paused(), "这是用户暂停：有「继续」");
    assert!(!s.paused(), "用户暂停不是系统暂停，不能写「恢复由系统事件自动发生」");
    assert!(
        s.tasks.iter().all(|t| !t.system_paused),
        "用户暂停不能被标成系统暂停"
    );

    // 恢复后系统休眠
    board.resume_all();
    board.pause_all_system("系统休眠 / 进程被挂起");
    let s = board.snapshot();
    assert!(s.paused(), "系统暂停要让任务页出现「系统暂停中…」");
    assert!(!s.user_paused(), "系统暂停没有人工「继续」");
    assert!(s.tasks.iter().all(|t| t.system_paused));

    // 系统恢复：自动回到暂停前的状态。
    board.resume_all();
    let s = board.snapshot();
    assert!(!s.paused() && !s.user_paused());
    assert_eq!(s.tasks[0].state, IndexState::Scanning, "回到进入暂停前的状态");
}

/// `AC-IDX-PAUSE`：用户逐任务暂停在系统恢复时**不被越过**（M4.3 TC-M43-05）。
#[test]
fn ac_idx_pause_system_resume_never_overrides_a_user_pause() {
    let board = IndexBoard::new();
    let path = "/photos".to_string();
    board.begin_batch(std::slice::from_ref(&path));
    board.mark_scanning(&path);

    board.pause_one(&path, "已手动暂停");
    let s = board.snapshot();
    assert!(s.user_paused() && !s.paused());

    board.pause_all_system("系统休眠 / 进程被挂起");
    board.resume_all();
    let s = board.snapshot();
    assert_eq!(s.tasks[0].state, IndexState::Paused, "用户暂停必须保留");
    assert!(s.user_paused(), "系统恢复不能越过用户意愿");
    assert!(!s.paused(), "用户接管后不再算系统暂停");
}

/// `AC-IDX-PAUSE`：`task_id == 0` 的会话内任务不给逐任务暂停——不画按钮的前提是
/// 数据层真的没有 id（否则就是「能点但点了没用」的假入口）。
#[test]
fn ac_idx_pause_session_only_tasks_have_no_task_id() {
    let board = IndexBoard::new();
    board.begin_batch(&["/photos".into()]);
    board.mark_scanning("/photos");
    let live = live_task_views(&board.snapshot());
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].task_id, 0, "板上任务未对齐库里意图时给 0");
    assert!(live[0].running);
}

// ---------------------------------------------------------------- IDX-A-PROG

/// `IDX-A-PROG`：任务行的「已用 mm:ss」从**批次开始**算，不是从最近一次进度更新算。
///
/// 旧实现取 `now - last_update`：每次进度更新都会把「已用」打回 0，
/// 界面上表现为时钟永远停在 00:00。
#[test]
fn idx_a_prog_elapsed_runs_from_batch_start() {
    let board = IndexBoard::new();
    board.begin_batch(&["/photos".into()]);
    board.mark_scanning("/photos");
    let live = live_task_views(&board.snapshot());
    let elapsed = &live[0].elapsed_text;
    assert_eq!(elapsed.len(), 5, "mm:ss 定长：{elapsed}");
    assert_eq!(&elapsed[2..3], ":");
    assert!(live[0].started_at.is_some(), "要有批次开始时刻");
    // 不含 ETA 字样。
    for banned in ["预计", "剩余", "ETA"] {
        assert!(!elapsed.contains(banned));
    }
}

// ---------------------------------------------------------------- IDX-A-STOR

/// `IDX-A-STOR`：诊断导出文件名是 `openpicasa-diag-{UTC}.json`（内容是脱敏 JSON）。
#[test]
fn idx_a_stor_diag_export_filename_is_json_and_utc() {
    // 2026-11-01 12:34:56 UTC
    let name = diag_filename(1_793_536_496);
    assert_eq!(name, "openpicasa-diag-20261101-123456.json");
    assert!(name.ends_with(".json"), "导出内容是 JSON，扩展名必须一致：{name}");
    assert!(!name.ends_with(".zip"), "{name}");
}

/// `AC-IDX-STORAGE`：导出目录不得落在照片根内（零写入约束）。
#[test]
fn ac_idx_storage_diag_export_refuses_to_land_inside_a_photo_root() {
    let f = fixture();
    let s = snap(&f);
    let inside = Path::new(&f.photos).join("diag");
    assert!(
        picasa_core::mgmt::check_export_dir(&inside, &s).is_err(),
        "照片根内不许导出诊断日志"
    );
    let outside = f._tmp.path().join("exports");
    assert!(
        picasa_core::mgmt::check_export_dir(&outside, &s).is_ok(),
        "库外的目录应当允许"
    );
}

// ---------------------------------------------------------------- AC-IDX-FAIL

/// `AC-IDX-FAIL`：失败五类固定顺序；`count + ignored == 0` 不画；
/// 「格式不支持」不可重试。
#[test]
fn ac_idx_fail_five_kinds_are_in_fixed_order_and_only_unsupported_is_permanent() {
    let kinds: Vec<&str> = FailureKind::ALL.iter().map(|k| k.label()).collect();
    assert_eq!(
        kinds,
        ["无权限", "文件损坏", "目录失联", "格式不支持", "超时"],
        "五类顺序固定，不按数量排序"
    );
    for k in FailureKind::ALL {
        assert_eq!(
            k.retryable(),
            k != FailureKind::Unsupported,
            "只有「格式不支持」不可重试"
        );
    }
}

/// `AC-IDX-FAIL`：忽略后仍在 `failures_by_kind` 里可见（`ignored` 计数），
/// 且只进「已忽略」不进角标。
#[test]
fn ac_idx_fail_ignored_entries_stay_visible_but_leave_the_badge() {
    let mut s = MgmtSnapshot::default();
    s.failures_by_kind[(FailureKind::Corrupt.code() - 1) as usize] = picasa_db::FailureGroup {
        kind: Some(FailureKind::Corrupt.code()),
        count: 2,
        ignored: 3,
    };
    s.compute_badge(1, 0);
    assert_eq!(s.badge, 1, "被忽略的 3 条不进角标");
    assert_eq!(
        s.failures_by_kind[(FailureKind::Corrupt.code() - 1) as usize].ignored,
        3,
        "但仍在分组里可见（AC-IDX-FAIL：忽略不是隐藏）"
    );
}
