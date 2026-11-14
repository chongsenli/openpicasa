//! M4.3 阶段 2 · 第三批 —— IDX-13（单根强制重扫）与 IDX-16（离线内核）的端到端契约测试。
//!
//! docs/18 §2（IDX-13/16）+ §3.1 + §5（TC-M43-09 / TC-M43-10）：
//!   · IDX-13：强制重扫必须**跳过 L1 快筛**，直接做文件级 L2 比对；变化目录由
//!     `VerifyRunner::take_changed()` 交给 `IndexService::enqueue_refresh`。这里锁的是
//!     「`VerifyRunner::new_forced_full` → changed → 刷新队列」这条接线，不是 L1/L2
//!     算法本身（后者由 `m3_verify_l1l2.rs` 覆盖）。
//!   · IDX-16：扫描发现根目录已不在盘上（外置盘拔出）时，必须发射
//!     `CoreEvent::FolderOnline { online: false }`，由 `Library::apply_event` 把
//!     `folders.is_online` 置灰并刷新面板；任务用现成的 `Unreachable`（code 6）承载
//!     blocked，**不写持久终态**，这样重连/重启后任务能自动续跑（IDX-12）。
//!     不新增第 8 态（index.rs `codes_match_db_status` 七态双射钉子）。

use picasa_core::verify::{VerifyLevel, VerifyRunner, VerifyTick};
use picasa_core::{
    AppPaths, CoreEvent, IndexBoard, IndexService, IndexState, Library, VerifyPlan, paths,
};
use picasa_db::{Db, PhotoInput, TaskStatus};
use std::path::Path;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn isolated_paths(root: &Path) -> AppPaths {
    AppPaths {
        support: root.join("Library/Application Support/app.openpicasa"),
        preferences: root.join("Library/Preferences/app.openpicasa"),
        caches: root.join("Library/Caches/app.openpicasa"),
        logs: root.join("Library/Logs/app.openpicasa"),
    }
}

/// 与 `m3_verify_l1l2.rs::fixture` 同款：造一个「库与磁盘不完全一致」的小目录树。
///
/// 照片行的 `mtime_ns` 故意用常量而磁盘文件是当下时间 —— 于是强制 L2 一定会把
/// 这个目录判成「有变化」，`take_changed()` 才有东西可断言。返回 (目录 id, Db)。
fn seeded_db(root: &Path) -> (i64, Db) {
    let db = Db::open(root.join("library.sqlite")).unwrap();
    let root_path = root.to_string_lossy().to_string();
    let root_id = db.upsert_root(&root_path, 0, false).unwrap();

    let dir = root.join("a");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.to_string_lossy().to_string();
    let fid = db.upsert_folder(&path, &path.to_lowercase(), None).unwrap();
    db.set_folder_root(fid, root_id, None).unwrap();

    let mut items = Vec::new();
    for i in 0..2 {
        let name = format!("p{i}.jpg");
        std::fs::write(dir.join(&name), vec![0u8; 64]).unwrap();
        items.push(PhotoInput {
            name: name.clone(),
            name_key: name.to_lowercase(),
            size_bytes: 64,
            mtime_ns: 1_600_000_000_000_000_000,
            width: Some(800),
            height: Some(600),
            orientation: Some(1),
            taken_at: Some(1_600_000_000),
            taken_at_src: 0,
            format: 0,
        });
    }
    db.apply_scan(fid, &path, &items, 0).unwrap();
    // 模拟「升级后第一次启动」：`dir_mtime` 还没量过。
    db.set_folder_dir_mtime(fid, None).unwrap();
    (fid, db)
}

fn run_to_completion(runner: &mut VerifyRunner, db: &Db) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 10_000, "校验跑了 1 万次 tick 还没结束，像死循环");
        if matches!(runner.tick(db), VerifyTick::Finished) {
            return;
        }
    }
}

/// ① IDX-13：强制重扫跳过 L1（dir_stats 恒 0、file_stats > 0），变化目录全部进入
///    刷新队列。端到端锁住 `new_forced_full → take_changed → enqueue_refresh` 接线。
#[test]
fn forced_full_rescan_skips_l1_and_feeds_changed_dirs_to_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let root_path = root.to_string_lossy().to_string();
    let (_fid, db) = seeded_db(&root);
    let arc = Arc::new(Mutex::new(db));

    let plan = VerifyPlan::from_root_paths([root.clone()]);
    assert!(!plan.is_empty());
    let mut runner = VerifyRunner::new_forced_full(plan);
    runner.set_delay_ms(0);
    runner.set_dir_interval_ms(0);
    assert_eq!(
        runner.level(),
        VerifyLevel::L2,
        "强制重扫必须一上来就是 L2（跳过 L1 快筛）"
    );

    {
        let guard = arc.lock().unwrap();
        run_to_completion(&mut runner, &guard);
    }
    assert_eq!(runner.dir_stats, 0, "强制重扫不该跑任何一次 L1 目录快筛");
    assert!(runner.file_stats > 0, "强制重扫必须真正逐文件 stat 过");

    let changed = runner.take_changed();
    assert!(
        !changed.is_empty(),
        "库里 mtime 与磁盘不一致，强制 L2 必须报出变化目录"
    );
    for (_folder_id, path) in &changed {
        assert!(
            path.starts_with(&root_path),
            "变化目录必须落在被重扫的根下：{path}"
        );
    }

    // 「变化目录 → 刷新队列」这一跳：UI 侧 `tick_startup_verify` 完成块就是这么做的。
    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(arc.clone(), board, tx);
    let mut queued = 0usize;
    for (_folder_id, path) in &changed {
        if service.enqueue_refresh(Path::new(path)) {
            queued += 1;
        }
    }
    assert!(
        queued > 0,
        "至少要有变化目录被送进刷新队列（IDX-13 端到端接线）"
    );
}

/// ② IDX-16：根目录从盘上消失时发射 `FolderOnline{false}`，消费者把 `is_online`
///    置灰并刷新面板；blocked 用 `Unreachable`（code 6）承载、reason 带盘名；
///    **持久任务行不写终态**，仍在「可续跑」队列里（IDX-12 自动续跑语义）。
#[test]
fn offline_root_emits_folder_online_and_marks_unreachable_without_terminal_state() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    std::fs::create_dir_all(&media).unwrap();

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);
    let scope = paths::canonical_string(&media);

    // 模拟「曾经索引过、现在外置盘被拔出」：folders 里得先有这一行，
    // `db_folder_id` 才能解析出 folder_id 并发出离线事件。
    let fid = lib
        .db()
        .unwrap()
        .upsert_folder(&scope, &scope.to_lowercase(), None)
        .unwrap();
    assert!(lib.db().unwrap().all_folders().unwrap().iter().any(|f| f.id == fid));

    // 拔盘：把目录整体「移走」（R-1 禁止递归删目录，rename 同样让该路径消失，
    // 且更贴近外置盘卸载——路径不再存在，但库里的 folders 行还在）。
    let unplugged = tmp.path().join("unplugged-volume");
    std::fs::rename(&media, &unplugged).unwrap();
    assert!(!media.is_dir());

    let (tx, rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);
    service.enqueue_scan(std::slice::from_ref(&media));

    // ScanStarted 会先到，循环里挑出 FolderOnline。
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut offline: Option<(i64, bool)> = None;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(CoreEvent::FolderOnline { folder_id, online }) => {
                offline = Some((folder_id, online));
                break;
            }
            Ok(_) => continue,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let (folder_id, online) = offline.expect("离线扫描必须发射 FolderOnline 事件");
    assert_eq!(folder_id, fid, "事件必须带稳定的 folder_id");
    assert!(!online, "目录消失是「离线」而不是「在线」");

    // 消费者：更新 is_online 且返回 true（面板据此刷新）。
    assert!(
        lib.apply_event(CoreEvent::FolderOnline {
            folder_id,
            online: false,
        }),
        "FolderOnline 必须让面板刷新"
    );
    let online_now = lib
        .db()
        .unwrap()
        .all_folders()
        .unwrap()
        .into_iter()
        .find(|f| f.id == fid)
        .map(|f| f.is_online);
    assert_eq!(online_now, Some(false), "folders.is_online 应被置灰");

    // blocked 用现成的 Unreachable（code 6）承载，reason 带盘名，不新增第 8 态。
    let snap = board.snapshot();
    let task = snap
        .task_by_path(&scope)
        .expect("离线任务必须留在面板上（不可访问）");
    assert_eq!(task.state, IndexState::Unreachable);
    assert!(
        task.reason.as_deref().unwrap_or("").contains("卷："),
        "不可访问原因要带卷名：{:?}",
        task.reason
    );

    // 关键：没有写持久终态 —— 任务行仍是非终态、仍在可续跑队列里。
    let intents = lib.db().unwrap().task_intents_for_scope(&scope).unwrap();
    assert_eq!(intents.len(), 1, "这次扫描只该有一条持久任务行");
    let intent = &intents[0];
    assert!(
        intent.cancel_reason.is_none(),
        "离线不是取消，不能落 cancel_reason"
    );
    assert_ne!(
        intent.status,
        TaskStatus::Completed.code(),
        "离线分支不能把任务写成 Completed 终态"
    );
    assert!(
        lib.db()
            .unwrap()
            .resumable_index_tasks()
            .unwrap()
            .iter()
            .any(|t| t.task_id == intent.task_id),
        "离线任务必须留在可续跑队列，重连/重启后自动续跑（IDX-12）"
    );
}
