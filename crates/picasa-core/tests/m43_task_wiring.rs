//! M4.3 阶段 1 · 「持久任务意图 ↔ scanner 接线」的端到端契约测试。
//!
//! docs/18 §2（IDX-07/08/09/12）+ §3.1：扫描任务不再只是内存里的 `IndexBoard`
//! 条目，而要有持久的 `index_tasks` 行，支持：
//!   · 可持久暂停 / 继续（暂停 A 不影响 B，重启仍暂停）；
//!   · 按检查点续跑（不重置 0、不从根重复全扫）；
//!   · 真取消（Cancelled 终态、重启不自动恢复）；
//!   · 全局「暂停索引」跨进程重启。
//!
//! 这里锁的是 **scanner 与 DB 意图层的接线**，不是 DB 列本身（后者由
//! `picasa-db/tests/m43_task_intent.rs` 覆盖）。

use picasa_core::{AppPaths, CoreEvent, IndexBoard, IndexService, Library, paths};
use picasa_db::{TaskKind, TaskOutcome, TaskScopeKind, TaskStatus};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

fn isolated_paths(root: &Path) -> AppPaths {
    AppPaths {
        support: root.join("Library/Application Support/app.openpicasa"),
        preferences: root.join("Library/Preferences/app.openpicasa"),
        caches: root.join("Library/Caches/app.openpicasa"),
        logs: root.join("Library/Logs/app.openpicasa"),
    }
}

/// 写一张**能真正解码出缩略图**的 JPEG。
///
/// M4.3 IDX-17 起，收尾终态按真实缩略图失败数判定（G1）：若沿用旧的
/// `b"not-a-real-jpeg"` 假字节，扫描会如实落 `PartialFailed`，把本文件里
/// 验证「成功 / 续跑接线」的用例误伤。这里用工作区已依赖的 `image` 编码一张
/// 4×4 纯色图，保证 `picasa_imaging::thumb_t0` 走通、无失败项。
fn touch_jpeg(dir: &Path, name: &str) {
    use image::{Rgba, RgbaImage};
    std::fs::create_dir_all(dir).unwrap();
    let img = RgbaImage::from_pixel(4, 4, Rgba([10, 20, 30, 255]));
    std::fs::write(dir.join(name), picasa_imaging::encode_jpeg(&img, 82).unwrap()).unwrap();
}

/// 轮询等一个条件成立；超时后返回最后一次判断的值。
fn wait_until<F: Fn() -> bool>(cond: F, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

/// ① `enqueue_scan` 必须建持久任务行，扫描完成后落 Completed/Success 终态。
///    这是「DB 任务行 ↔ scanner」最基础的一条接线。
#[test]
fn scan_lands_a_persistent_task_row_with_success_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    for i in 0..4 {
        touch_jpeg(&media.join(format!("d{i}")), "p.jpg");
    }

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);
    let scope = paths::canonical_string(&media);

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);
    service.enqueue_scan(std::slice::from_ref(&media));

    assert!(
        wait_until(|| lib.db().unwrap().count_photos(None).unwrap() == 4, 30),
        "扫描应把 4 张照片全部入库"
    );

    // 终态在 run_job 收尾的 finish_intent 里写，给它一个落定窗口。
    let done = wait_until(
        || {
            lib.db()
                .unwrap()
                .task_intents_for_scope(&scope)
                .ok()
                .and_then(|v| v.first().cloned())
                .is_some_and(|t| t.status == TaskStatus::Completed.code())
        },
        30,
    );
    assert!(done, "扫描完成后任务行必须落 Completed 终态");

    let intents = lib.db().unwrap().task_intents_for_scope(&scope).unwrap();
    assert_eq!(intents.len(), 1, "一次 enqueue_scan 只该有一条任务行");
    let t = &intents[0];
    assert_eq!(t.kind, TaskKind::FirstScan.code());
    assert_eq!(t.scope_kind, TaskScopeKind::Root);
    assert_eq!(t.outcome, Some(TaskOutcome::Success.code()));
    // G1：成功收尾必须如实写「0 失败」（不是永远 Success/failed=0 的假账）。
    assert_eq!(t.failed, 0, "有效 JPEG 不应产生失败项");
    assert!(
        t.checkpoint.is_some(),
        "扫描过程中必须写过检查点（IDX-08 / TC-M43-08）"
    );
    assert!(t.cancel_reason.is_none());
    assert!(!t.user_paused);
    // 终态任务不该再出现在「可续跑」队列里。
    assert!(
        lib.db()
            .unwrap()
            .resumable_index_tasks()
            .unwrap()
            .iter()
            .all(|r| r.task_id != t.task_id)
    );
}

/// ② 用户暂停的任务必须：被 `enqueue_scan` 复用（不新建行）、在目录边界让出
///    （不写任何照片）、保持持久 `user_paused`、且重启不自动续跑。
#[test]
fn user_paused_task_yields_without_writing_and_stays_persisted() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    for i in 0..4 {
        touch_jpeg(&media.join(format!("d{i}")), "p.jpg");
    }

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);
    let scope = paths::canonical_string(&media);

    // 预先造一条「上次会话被用户暂停」的任务行：确定性，不依赖线程竞态。
    let id = {
        let db = lib.db().unwrap();
        let id = db
            .begin_index_task(None, &scope, TaskKind::FirstScan, TaskScopeKind::Root)
            .unwrap();
        db.set_task_user_paused(id, true).unwrap();
        id
    };

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);
    service.enqueue_scan(std::slice::from_ref(&media));

    // 给 worker 充分时间取到 Job；若接线正确，它在动任何写之前就会 yield。
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        lib.db().unwrap().count_photos(None).unwrap(),
        0,
        "被用户暂停的任务不该往库里写任何照片（暂停必须真的停写）"
    );

    let intents = lib.db().unwrap().task_intents_for_scope(&scope).unwrap();
    assert_eq!(
        intents.len(),
        1,
        "enqueue_scan 必须复用已有暂停行，绝不能新建一条让它偷偷跑起来"
    );
    assert_eq!(intents[0].task_id, id);
    assert!(intents[0].user_paused, "user_paused 必须持久保留");
    assert!(
        lib.db()
            .unwrap()
            .resumable_index_tasks()
            .unwrap()
            .iter()
            .all(|r| r.task_id != id),
        "暂停任务不能进自动续跑队列（TC-M43-05）"
    );
    assert!(
        lib.db()
            .unwrap()
            .paused_index_tasks()
            .unwrap()
            .iter()
            .any(|r| r.task_id == id)
    );
    assert_eq!(
        service.resumable_task_count(),
        0,
        "可续跑任务数不应包含已暂停任务"
    );
}

/// ③ 真取消：落 Cancelled 终态、幂等、且不会被任何恢复队列复活。
#[test]
fn cancel_task_marks_cancelled_terminal_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    touch_jpeg(&media, "p.jpg");

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);
    let scope = paths::canonical_string(&media);

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);

    let id = lib
        .db()
        .unwrap()
        .begin_index_task(None, &scope, TaskKind::FirstScan, TaskScopeKind::Root)
        .unwrap();

    assert!(service.cancel_task(id, "用户取消"), "首次取消必须生效");
    assert!(
        !service.cancel_task(id, "用户取消"),
        "取消必须幂等：第二次不应再改动"
    );

    let t = lib.db().unwrap().task_intent(id).unwrap().unwrap();
    assert_eq!(t.cancel_reason.as_deref(), Some("用户取消"));
    assert_eq!(t.outcome, Some(TaskOutcome::Cancelled.code()));
    assert_eq!(t.status, TaskStatus::Completed.code());
    assert!(t.generation >= 1, "取消应抬升 generation");
    assert!(
        lib.db()
            .unwrap()
            .unfinished_index_tasks()
            .unwrap()
            .iter()
            .all(|r| r.task_id != id),
        "已取消任务不得留在未完成队列里（重启不复活，TC-M43-06）"
    );
}

/// ④ 全局「暂停索引」必须持久化，且新 `IndexService`（模拟重启）恢复暂停态。
#[test]
fn global_pause_persists_across_service_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    touch_jpeg(&media, "p.jpg");

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx.clone());
    assert!(!service.is_paused());

    service.pause();
    assert!(service.is_paused());
    assert!(lib.db().unwrap().index_pause_all().unwrap());

    // 模拟进程重启：重新构造 service，持久标志要在 new() 里被读回。
    let board2 = Arc::new(IndexBoard::new());
    let service2 = IndexService::new(lib.db.clone(), board2, tx);
    assert!(
        service2.is_paused(),
        "退出/kill 后重开必须仍是暂停（IDX-07 / TC-M43-05）"
    );

    service2.resume();
    assert!(!service2.is_paused());
    assert!(!lib.db().unwrap().index_pause_all().unwrap());
    drop(service);
}

/// ⑤ 检查点续跑（IDX-08 / TC-M43-08）：复用同一任务行，从检查点之后接着走，
///    不重置为 0、不重复全扫。检查点之前已落库的目录必须被跳过。
#[test]
fn checkpoint_resume_reuses_row_and_skips_already_done_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    touch_jpeg(&media, "root.jpg"); // root 自身（dirs[0]，检查点指向它 → 应跳过）
    for i in 0..4 {
        touch_jpeg(&media.join(format!("d{i}")), "p.jpg"); // 4 个子目录
    }

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);
    let scope = paths::canonical_string(&media);

    // 造一条「扫到 root 后进程中断」的任务行：检查点 = root 目录。
    let id = {
        let db = lib.db().unwrap();
        let id = db
            .begin_index_task(None, &scope, TaskKind::FirstScan, TaskScopeKind::Root)
            .unwrap();
        db.set_task_checkpoint(id, &scope).unwrap();
        id
    };

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);
    service.enqueue_scan(std::slice::from_ref(&media));

    assert!(
        wait_until(|| lib.db().unwrap().count_photos(None).unwrap() >= 4, 30),
        "检查点之后的 4 个子目录应继续入库"
    );
    let done = wait_until(
        || {
            lib.db()
                .unwrap()
                .task_intent(id)
                .ok()
                .flatten()
                .is_some_and(|t| t.status == TaskStatus::Completed.code())
        },
        30,
    );
    assert!(done, "续跑完成后任务行必须落终态");

    assert_eq!(
        lib.db().unwrap().count_photos(None).unwrap(),
        4,
        "检查点之前的 root 目录不该被重复扫描入库（共 5 张，跳过 root 的 1 张）"
    );
    let intents = lib.db().unwrap().task_intents_for_scope(&scope).unwrap();
    assert_eq!(intents.len(), 1, "续跑必须复用同一行，不能新建");
    assert_eq!(intents[0].task_id, id);
}
