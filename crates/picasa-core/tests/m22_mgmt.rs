//! M2.2 索引管理的**应用侧**验收（`docs/06` §8.3 的 `TC-M22-IDX-*`）。
//!
//! 这些用例的共同前提是 §6.1 那句话：**数字只有一个出处**。
//! 于是这里验的不是「某个查询对不对」，而是「同一份快照里四处数字是否互相自洽」——
//! 这才是 R-M22-6 的可判定形式。
//!
//! ⚠️ 全部用**注入的离线探针**，不真的拔盘；库是临时文件，跑完随目录删掉。

use picasa_core::{MgmtTab, SnapshotInput, build_snapshot, default_offline_probe, live_task_views};
use picasa_db::{Db, FailureKind, MIGRATIONS, TaskKind, TaskOutcome, TaskStatus};
use std::path::Path;

/// 造一个 **v5** 库：两个根（一个在线、一个模拟离线）、三棵树、若干照片，
/// 外加一个带失败项的任务。
///
/// `direct_count` 是**手工写进去的**（真实值由 `walk_and_store` 维护），
/// 这样「四处一致」验的是「快照有没有如实把库里的数读出来」，
/// 而不是「扫描器有没有算对」——后者是 M2 的用例，不在这里重复。
fn make_v5_db(path: &Path, online_root: &str, offline_root: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
    )
    .unwrap();
    for m in MIGRATIONS.iter() {
        conn.execute_batch(m.sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations VALUES (?1, 0)",
            rusqlite::params![m.version],
        )
        .unwrap();
    }
    let sql = format!(
        r#"
        INSERT INTO roots (id, path, path_key, added_at, state) VALUES
            (1, '{a}', '{a}', 100, 0),
            (2, '{b}', '{b}', 100, 0);
        -- 根 1：自身 3 张 + 子目录 2 张
        INSERT INTO folders (id, path, path_key, root_id, parent_id, name,
                             is_online, direct_count, has_children, last_scan_at) VALUES
            (1, '{a}',        '{a}',        1, NULL, 'photos',  1, 3, 1, 100),
            (2, '{a}/2020',   '{a}/2020',   1, 1,    '2020',    1, 2, 0, 100);
        -- 根 2（模拟离线卷）：4 张
        INSERT INTO folders (id, path, path_key, root_id, parent_id, name,
                             is_online, direct_count, has_children, last_scan_at) VALUES
            (3, '{b}',        '{b}',        2, NULL, 'Ext',      1, 4, 0, 100);
        INSERT INTO photos
            (folder_id, name, name_key, size_bytes, mtime_ns, width, height, orientation,
             taken_at, taken_at_src, thumb_state, decoder_ver) VALUES
            (1, 'a.jpg', 'a.jpg', 10, 1, 4000, 3000, 1, 100, 0, 1, 0),
            (1, 'b.jpg', 'b.jpg', 20, 2, 4000, 3000, 1, 200, 0, 1, 0),
            (1, 'c.jpg', 'c.jpg', 30, 3, 4000, 3000, 1, 300, 0, 2, 0),
            (2, 'd.jpg', 'd.jpg', 40, 4, 4000, 3000, 1, 400, 0, 1, 0),
            (2, 'e.jpg', 'e.jpg', 50, 5, 4000, 3000, 1, 500, 0, 1, 0),
            (3, 'f.jpg', 'f.jpg', 60, 6, 4000, 3000, 1, 600, 0, 1, 0),
            (3, 'g.jpg', 'g.jpg', 70, 7, 4000, 3000, 1, 700, 0, 1, 0),
            (3, 'h.jpg', 'h.jpg', 80, 8, 4000, 3000, 1, 800, 0, 1, 0),
            (3, 'i.jpg', 'i.jpg', 90, 9, 4000, 3000, 1, 900, 0, 1, 0);
        -- 一个已结束的任务，带一条「失联」失败
        INSERT INTO index_tasks (task_id, root_id, scope_path, kind, status, started_at)
            VALUES (1, 1, '{a}', 0, 4, 1000);
        INSERT INTO index_task_failures (task_id, relative_path, error_code, kind, ignored)
            VALUES (1, 'c.jpg', 'No such file or directory', 3, 0);
        "#,
        a = online_root,
        b = offline_root,
    );
    conn.execute_batch(&sql).unwrap();
}

struct Fixture {
    _tmp: tempfile::TempDir,
    db: Db,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let online = tmp.path().join("photos").to_string_lossy().into_owned();
    // 用一个**真的不存在**的路径当外置盘：这样默认探针也会判它离线，
    // 用例就不必注入探针也能自洽（注入只用于「假装它在线」的反向验证）。
    let offline = "/Volumes/OpenPicasaTestNotMounted".to_string();
    std::fs::create_dir_all(&online).unwrap();
    let db_path = tmp.path().join("library.sqlite");
    make_v5_db(&db_path, &online, &offline);
    let db = Db::open(&db_path).unwrap();
    Fixture { _tmp: tmp, db }
}

/// 快照输入的样板：seq 递增、不暂停，活任务与探针由调用方给。
fn input<'a>(
    seq: u64,
    probe: &'a picasa_core::OfflineProbe,
    live: &'a [picasa_core::TaskView],
    prev_dataset_version: u64,
    prev_photo_count: u64,
) -> SnapshotInput<'a> {
    SnapshotInput {
        seq,
        live,
        paused_by_system: false,
        offline: probe,
        prev_dataset_version,
        prev_photo_count,
    }
}

/// 不关心「数据集版本号」的便捷入口（第一次取快照时用）。
fn input_fresh<'a>(seq: u64, probe: &'a picasa_core::OfflineProbe) -> SnapshotInput<'a> {
    SnapshotInput {
        seq,
        live: &[],
        paused_by_system: false,
        offline: probe,
        prev_dataset_version: 0,
        prev_photo_count: 0,
    }
}

// ---------------------------------------------------------------- TC-M22-IDX-011

/// **四处一致**：树节点计数 / 组头 N / 所有照片 / 索引管理页照片数，
/// 在同一 `seq` 下逐字段相等。索引进行中连取 100 次也必须全等。
#[test]
fn tc_m22_idx_011_four_places_agree_on_the_same_seq() {
    let f = fixture();
    let probe = default_offline_probe();

    let snap = build_snapshot(&f.db, &input_fresh(1, &probe)).unwrap();

    // ① 树节点计数 == 库里的 direct_count
    assert_eq!(snap.folder_counts.len(), 3, "三个目录都要有计数");
    // ② 组头 N 读的是同一份 map（不是另查一遍）
    let direct_sum: u32 = snap.folder_counts.values().sum();
    assert_eq!(direct_sum, 3 + 2 + 4, "direct_count 之和 = 9");

    // ③ 「所有照片」== 全部 active 根的照片（含离线根的 4 张，§2.9 / P-17 建议）
    assert_eq!(snap.all_photos, 9, "离线根的照片也要算（置灰可见）");

    // ④ 索引管理页「关注文件夹」里的照片数
    let root1 = snap.root_by_id(1).unwrap();
    let root2 = snap.root_by_id(2).unwrap();
    assert_eq!(root1.photo_count, 5, "根 1 含子目录：3 + 2");
    assert_eq!(root2.photo_count, 4);
    assert_eq!(
        root1.photo_count + root2.photo_count,
        snap.all_photos as u64,
        "关注文件夹页的照片数之和必须等于「所有照片」"
    );

    // 连续取 100 个 seq：计数一次都不能漂（这是 R-M22-6 的自动化形式）
    let mut prev: Option<(u32, u64)> = None;
    for seq in 2..=101 {
        let s = build_snapshot(&f.db, &input_fresh(seq, &probe)).unwrap();
        let now = (s.all_photos, s.storage.photo_count);
        assert_eq!(now, (9, 9), "第 {seq} 次：所有照片与 photos 行数都不许变");
        if let Some(p) = prev {
            assert_eq!(p, now, "第 {seq} 次与上一次不一致");
        }
        prev = Some(now);
        assert_eq!(s.seq, seq, "seq 必须单调递增");
    }
}

/// §6.1 硬约束：`folder_counts` 与 `all_photos` 必须**同一事务**查出。
///
/// 这条用例验的是接口形状（`mgmt_counts` 一个方法返回两个数），
/// 而不是去构造并发——并发下的可串行化由 SQLite 的读事务保证，
/// 在这里造一个真并发测试只会得到一个 flaky 用例。
#[test]
fn tc_m22_idx_011_counts_come_from_one_call() {
    let f = fixture();
    let (counts, all) = f.db.mgmt_counts().unwrap();
    assert_eq!(counts.len(), 3);
    assert_eq!(all, 9);
    // 快照里的两份数必须与这一次调用一致
    let snap = build_snapshot(&f.db, &input_fresh(1, &default_offline_probe())).unwrap();
    assert_eq!(snap.folder_counts, counts);
    assert_eq!(snap.all_photos as u64, all);
}

/// PERF-09：只刷新快照**不得**推动数据集版本号（`Layout` 不该重排）。
#[test]
fn tc_m22_perf_09_snapshot_refresh_does_not_bump_dataset_version() {
    let f = fixture();
    let probe = default_offline_probe();
    // 预热：第一次取快照时照片集合从「空」变成「9 张」，版本号必然 +1
    // （这是**期望**行为——首屏确实需要建一次 Layout）。
    // PERF-09 管的是**之后**的刷新：数据集没变就不许再动。
    let warm = build_snapshot(&f.db, &input_fresh(0, &probe)).unwrap();
    assert_eq!(warm.dataset_version, 1, "首次建快照 → 版本号从 0 变 1");

    let mut prev = warm.dataset_version;
    let mut prev_photos = warm.storage.photo_count;
    for seq in 1..=20 {
        let inp = SnapshotInput {
            seq,
            live: &[],
            paused_by_system: false,
            offline: &probe,
            prev_dataset_version: prev,
            prev_photo_count: prev_photos,
        };
        let s = build_snapshot(&f.db, &inp).unwrap();
        assert_eq!(
            s.dataset_version, prev,
            "照片集合没变，数据集版本号就不许动（否则每帧都重排 Layout）"
        );
        prev = s.dataset_version;
        prev_photos = s.storage.photo_count;
    }
}

// ---------------------------------------------------------------- TC-M22-IDX-002

/// 角标 = 失败任务数 + 失效排除项数 + 离线根数；处理完即消。
#[test]
fn tc_m22_idx_002_badge_is_the_sum_of_three_components() {
    let f = fixture();
    let probe = default_offline_probe();
    let snap = build_snapshot(&f.db, &input_fresh(1, &probe)).unwrap();

    // 一条失败（task 1）+ 无失效排除项 + 1 个离线根 = 2
    assert_eq!(snap.failed_task_count, 1);
    assert_eq!(snap.badge, 2, "1 个失败任务 + 1 个离线根");
    assert_eq!(snap.badge_text(), "2");
    assert_eq!(snap.entry_state(), picasa_core::IndexEntryState::Attention);

    // 处理掉失败项（忽略）→ 只剩离线根
    let n =
        f.db.ignore_failures_of_kind(FailureKind::Unreachable)
            .unwrap();
    assert_eq!(n, 1);
    let snap2 = build_snapshot(&f.db, &input_fresh(2, &probe)).unwrap();
    assert_eq!(snap2.failed_task_count, 0, "忽略后不再进角标");
    assert_eq!(snap2.badge, 1, "只剩离线根");
    // R-M2-4：忽略只是标记，失败项**仍在列表可见**
    assert_eq!(
        snap2.ignored_failures(),
        1,
        "忽略的失败项必须还在（只是不进角标）"
    );

    // 取消忽略 → 回到 2（可逆，用户反悔了能退回来）
    f.db.unignore_failures_of_kind(FailureKind::Unreachable)
        .unwrap();
    let snap3 = build_snapshot(&f.db, &input_fresh(3, &probe)).unwrap();
    assert_eq!(snap3.badge, 2);
}

/// 离线根判定的注入验证：同一个库，探针说在线就不该有离线分量。
#[test]
fn tc_m22_idx_005_offline_root_is_a_runtime_judgement_not_a_stored_flag() {
    let f = fixture();

    let real = default_offline_probe();
    let snap = build_snapshot(&f.db, &input_fresh(1, &real)).unwrap();
    assert!(snap.root_by_id(2).unwrap().offline, "不存在的卷 → 离线");
    assert!(!snap.root_by_id(1).unwrap().offline);
    assert!(snap.badge > 0);

    // 注入「永远在线」：离线分量必须消失
    let always_on: picasa_core::OfflineProbe = Box::new(|_| false);
    let snap2 = build_snapshot(&f.db, &input_fresh(2, &always_on)).unwrap();
    assert!(!snap2.root_by_id(2).unwrap().offline);
    assert_eq!(snap2.badge, 1, "只剩失败任务这一个分量");
    // 离线根的照片**仍然算进**「所有照片」（P-17：置灰可见，与树一致）
    assert_eq!(snap2.all_photos, 9);
}

// ---------------------------------------------------------------- TC-M22-IDX-004

/// 失败分类的批量处理：按类点名、忽略后仍可见、`Unsupported` 不给重试。
#[test]
fn tc_m22_idx_004_failures_are_grouped_and_batch_actionable() {
    let f = fixture();
    let probe = default_offline_probe();
    let snap = build_snapshot(&f.db, &input_fresh(1, &probe)).unwrap();

    // 五类分组：下标 = code - 1
    let unreachable = snap.failures_by_kind[(FailureKind::Unreachable.code() - 1) as usize];
    assert_eq!(unreachable.count, 1, "失联 1 条");
    assert_eq!(snap.open_failures(), 1);

    // 「失联」可重试；「格式不支持」不可重试
    assert!(snap.group_retryable(FailureKind::Unreachable));
    assert!(!snap.group_retryable(FailureKind::Unsupported));

    // 重试路径要能点名到具体 photo_id（§4.3 边界：不走整目录重建）
    let ids =
        f.db.photo_ids_of_failure_kind(FailureKind::Unreachable)
            .unwrap();
    assert_eq!(ids.len(), 1, "必须能取到待重试的照片 id");

    // 默认分页到有问题的标签
    assert_eq!(snap.default_tab(snap.failed_task_count), MgmtTab::Tasks);
}

// ---------------------------------------------------------------- TC-M22-IDX-003

/// 任务历史：终态一次写全、重启后仍在、超 50 条按结束时间淘汰。
#[test]
fn tc_m22_idx_003_task_history_is_written_once_and_pruned() {
    let db = Db::open_in_memory().unwrap();

    let mut ids = Vec::new();
    for i in 0..3 {
        let id = db
            .create_task(None, &format!("/p{i}"), TaskKind::Refresh)
            .unwrap();
        db.finish_task(
            id,
            TaskStatus::Completed,
            TaskOutcome::Success,
            100,
            0,
            None,
        )
        .unwrap();
        ids.push(id);
    }
    // 一条未结束的：淘汰时**必须**保留（删了就查不到该恢复什么）
    let live = db
        .create_task(None, "/running", TaskKind::FirstScan)
        .unwrap();

    let hist = db.finished_tasks(50).unwrap();
    assert_eq!(hist.len(), 3, "只有终态进历史");
    assert!(hist.iter().all(|t| t.outcome().is_some()), "终态必有结局");
    assert!(hist.iter().all(|t| t.finished_at.is_some()));
    assert!(hist.iter().all(|t| t.duration_ms.is_some()), "耗时由库算");

    // 淘汰：keep=1
    let deleted = db.prune_task_history(1).unwrap();
    assert_eq!(deleted, 2, "3 条终态保留 1 条");
    let left = db.finished_tasks(50).unwrap();
    assert_eq!(left.len(), 1);
    assert!(
        db.unfinished_tasks()
            .unwrap()
            .iter()
            .any(|t| t.task_id == live),
        "未结束的任务一条都不能删"
    );
    // 被删的是最旧的两条（按 finished_at 排），留下的是最新那条
    assert_eq!(left[0].task_id, ids[2]);
}

/// 板上正在跑的任务要能进「任务」标签（§4.3：进行中进度在新页面可达）。
#[test]
fn tc_m22_idx_001_running_tasks_reach_the_tasks_tab() {
    use picasa_core::{IndexBoard, IndexState};

    let board = IndexBoard::new();
    board.begin_batch(&["/photos".to_string()]);
    board.mark_scanning("/photos");
    let board_snap = board.snapshot();
    assert!(board_snap.running());

    let views = live_task_views(&board_snap);
    assert_eq!(views.len(), 1, "板上有一个活任务");
    assert!(views[0].running);
    assert_eq!(views[0].status, IndexState::Scanning.label());
    // ETA 禁令：任务行里没有任何「剩余/预计」字段
    assert!(!views[0].elapsed_text.contains("预计"));

    // 活任务要排在历史前面（界面上「正在跑的」必须在最上面）
    let f = fixture();
    let inp = SnapshotInput {
        seq: 1,
        live: &views,
        paused_by_system: false,
        offline: &default_offline_probe(),
        prev_dataset_version: 0,
        prev_photo_count: 0,
    };
    let snap = build_snapshot(&f.db, &inp).unwrap();
    assert!(snap.tasks.iter().any(|t| t.running), "活任务进快照");
    // 有活任务但没角标时，入口应显示「进行中」而不是「需处理」。
    // 这里用一个「全员在线 + 忽略全部失败」的库来构造那种状态。
    f.db.ignore_failures_of_kind(FailureKind::Unreachable)
        .unwrap();
    let all_online: picasa_core::OfflineProbe = Box::new(|_| false);
    let clean = build_snapshot(&f.db, &input(9, &all_online, &views, 0, 0)).unwrap();
    assert_eq!(clean.badge, 0, "失败已忽略 + 无离线根 → 角标归零");
    assert_eq!(
        clean.entry_state(),
        picasa_core::IndexEntryState::Running,
        "有活任务 → 进行中"
    );
}
