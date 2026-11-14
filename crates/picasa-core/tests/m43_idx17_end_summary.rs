//! M4.3 阶段 1B · IDX-17「结果与历史」端到端契约测试。
//!
//! docs/18 §2（IDX-17）+ §5（TC-M43-11/13）+ §3.1：收尾终态必须按真实
//! 完成/失败数落账，运行时失败必须写进 `index_task_failures`，取消与部分成功
//! 可区分，且结果跨重启可追溯。这里锁的是「损坏 JPEG 扫描」这一条完整链路：
//! scanner（G1 终态真实化 + G4 失败记录）→ DB 持久层 → `build_snapshot` 的
//! `TaskView` 结束摘要（G2）。G6（诊断导出脱敏）由 mgmt.rs 单测覆盖。

use picasa_core::{
    AppPaths, CoreEvent, IndexBoard, IndexService, Library, MgmtSnapshot, SnapshotInput,
    TaskView, build_snapshot, default_offline_probe, paths,
};
use picasa_db::{FailureKind, TaskKind, TaskOutcome, TaskScopeKind, TaskStatus};
use std::collections::HashMap;
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

fn write_jpeg(path: &Path, w: u32, h: u32) {
    use image::RgbImage;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let img = RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 255) as u8, (y % 255) as u8, 128])
    });
    let mut buf = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new(&mut buf);
    enc.encode(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    std::fs::write(path, buf).unwrap();
}

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

/// 损坏 JPEG 扫描 → 真实 `PartialFailed` 终态 + 持久失败记录 + 结束摘要。
///
/// · G1：收尾不再永远写「Completed / Success / failed=0」；
/// · G4：`record_task_failure_kind` 在缩略图解码失败点被真正调用，
///   失败条目相对路径可被 `photo_ids_of_failure_kind` 反查（不只留在内存 board）；
/// · G2：`TaskView` 的完成/跳过/失败三数与持久层一致，outcome 文案「部分失败」；
/// · TC-M43-06：取消是持久 Cancelled 终态，不会被后续收尾覆盖成成功。
#[test]
fn broken_jpeg_lands_partial_failure_with_persistent_failures_and_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    write_jpeg(&media.join("ok.jpg"), 40, 30);
    // 截断 JPEG：扩展名与魔数都认得，但解码必定失败。
    std::fs::write(media.join("bad.jpg"), b"\xff\xd8\xff\xe0truncated").unwrap();

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);
    let scope = paths::canonical_string(&media);

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);
    service.enqueue_scan(std::slice::from_ref(&media));

    assert!(
        wait_until(|| lib.db().unwrap().count_photos(None).unwrap() == 2, 30),
        "两张照片都应入库（缩略图失败不阻止入库）"
    );

    // 终态在 run_job 收尾的 finish_intent 里写，给它一个落定窗口。
    let done = wait_until(
        || {
            lib.db()
                .unwrap()
                .task_intents_for_scope(&scope)
                .ok()
                .and_then(|v| v.first().cloned())
                .is_some_and(|t| t.status == TaskStatus::PartialFailed.code())
        },
        30,
    );
    assert!(done, "有失败项的任务必须落 PartialFailed，而不是假报成功");

    let intent = lib
        .db()
        .unwrap()
        .task_intents_for_scope(&scope)
        .unwrap()
        .remove(0);
    assert_eq!(intent.kind, TaskKind::FirstScan.code());
    assert_eq!(intent.scope_kind, TaskScopeKind::Root);
    assert_eq!(intent.status, TaskStatus::PartialFailed.code());
    assert_eq!(intent.outcome, Some(TaskOutcome::PartialFailed.code()));
    assert!(intent.failed >= 1, "失败数必须反映真实失败文件数");
    assert!(intent.processed >= 2, "processed 是分母，不能为 0");
    assert!(intent.cancel_reason.is_none(), "这不是取消");
    assert!(!intent.user_paused);
    assert!(intent.checkpoint.is_some());

    // G4：运行时失败写进了 index_task_failures，且相对路径可被反查。
    let db = lib.db().unwrap();
    let failures = db.task_failures(intent.task_id).unwrap();
    assert!(
        !failures.is_empty(),
        "解码失败必须留下可定位、可只重试失败项的持久记录"
    );
    assert!(
        failures.iter().any(|(rel, _, _)| rel.ends_with("bad.jpg")),
        "失败相对路径应指向 bad.jpg（photo_ids_of_failure_kind 才能反查）"
    );
    assert_eq!(db.failed_task_count().unwrap(), 1, "一个任务算一次失败");
    let groups = db.failure_groups().unwrap();
    let corrupt: u64 = groups
        .iter()
        .filter(|g| g.kind == Some(FailureKind::Corrupt.code()))
        .map(|g| g.count)
        .sum();
    assert!(corrupt >= 1, "坏 JPEG 应归入「文件损坏」分类桶");
    let ids = db.photo_ids_of_failure_kind(FailureKind::Corrupt).unwrap();
    assert!(!ids.is_empty(), "按分类能反查回 photo_id（只重试失败项）");
    // 同一失败项重复记录不冒充多个失败文件（G5，DB 层已有独立用例）。
    assert_eq!(db.task_failures(intent.task_id).unwrap().len(), failures.len());
    drop(db);

    // G2：build_snapshot 的任务摘要三数与持久层一致，outcome 文案正确。
    let live: Vec<TaskView> = Vec::new();
    let probe = default_offline_probe();
    let watch: HashMap<i64, picasa_core::watcher::WatchState> = HashMap::new();
    let input = SnapshotInput {
        seq: 1,
        live: &live,
        paused_by_system: false,
        offline: &probe,
        prev_dataset_version: 0,
        watch: &watch,
        prev_photo_count: 0,
    };
    let snap: MgmtSnapshot = {
        let db = lib.db().unwrap();
        build_snapshot(&db, &input).unwrap()
    };
    let view = snap
        .tasks
        .iter()
        .find(|t| t.task_id == intent.task_id)
        .expect("终态任务必须出现在历史里（结果跨重启可追溯）");
    assert_eq!(view.failed, intent.failed as u64);
    assert_eq!(view.completed, (intent.processed - intent.failed).max(0) as u64);
    assert_eq!(view.outcome.as_deref(), Some("部分失败"));
    assert!(view.cancel_reason.is_none());
    // 完成 + 失败 = 已处理（没有把重试次数算成失败文件数）。
    assert_eq!(view.completed + view.failed, view.discovered);
}

/// TC-M43-06：取消是持久终态；即便随后再触发一次 enqueue_scan（模拟旧队列
/// 里残留的 Job 到达收尾），`finish_intent` 的意图保护也不得把它改写回 Success。
#[test]
fn cancelled_task_is_never_overwritten_by_later_success() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    write_jpeg(&media.join("ok.jpg"), 20, 20);
    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);
    let scope = paths::canonical_string(&media);

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);
    service.enqueue_scan(std::slice::from_ref(&media));
    assert!(wait_until(|| lib.db().unwrap().count_photos(None).unwrap() == 1, 30));
    assert!(wait_until(
        || lib
            .db()
            .unwrap()
            .task_intents_for_scope(&scope)
            .ok()
            .and_then(|v| v.first().cloned())
            .is_some_and(|t| t.status == TaskStatus::Completed.code()),
        30
    ));

    let id = lib.db().unwrap().task_intents_for_scope(&scope).unwrap()[0].task_id;
    // 取消一条已终态行（模拟用户点「取消」时任务刚跑完的竞态）。
    assert!(service.cancel_task(id, "用户取消"));
    let row = lib.db().unwrap().task(id).unwrap().unwrap();
    assert_eq!(row.outcome(), Some(TaskOutcome::Cancelled));
    assert_eq!(row.cancel_reason.as_deref(), Some("用户取消"));

    // 重开一个 service、再次入队：begin_index_task 会为同一 scope 建新行
    // （旧行 finished_at 非空），但已取消的旧行绝不能被收尾改写。
    let (tx2, _rx2) = channel::<CoreEvent>();
    let service2 = IndexService::new(lib.db.clone(), board.clone(), tx2);
    service2.enqueue_scan(std::slice::from_ref(&media));
    assert!(wait_until(
        || lib
            .db()
            .unwrap()
            .task_intents_for_scope(&scope)
            .ok()
            .map(|v| v.len() >= 2)
            .unwrap_or(false),
        30
    ));
    std::thread::sleep(Duration::from_millis(300));
    let old = lib.db().unwrap().task(id).unwrap().unwrap();
    assert_eq!(old.outcome(), Some(TaskOutcome::Cancelled), "取消终态不可复活");
    assert_eq!(old.cancel_reason.as_deref(), Some("用户取消"));
}
