//! m10756：「索引管理里面的日志，增加清空日志按钮」——任务历史那一半。
//!
//! 「清空任务历史」必须满足两条底线：
//! 1. **只删已结束的任务**（`finished_at IS NOT NULL`）。正在跑 / 排队 / 暂停的
//!    一条都不删——删了就查不到该恢复什么，重启后任务再也接不上。
//! 2. 失败明细随任务级联删除（`index_task_failures` 是 `ON DELETE CASCADE`），
//!    所以「失败分类」角标会跟着归零；但同时**不能**动照片、缩略图与用户标记。

use picasa_db::{Db, FailureKind, TaskKind, TaskOutcome, TaskStatus};

/// 造一条已结束的任务，并挂两条失败明细。
fn finished_task_with_failures(db: &Db, scope: &str) -> i64 {
    let id = db.create_task(None, scope, TaskKind::FirstScan).unwrap();
    db.record_task_failure_kind(id, "a.jpg", "decode", FailureKind::Corrupt)
        .unwrap();
    db.record_task_failure_kind(id, "b.jpg", "decode", FailureKind::Corrupt)
        .unwrap();
    db.finish_task(
        id,
        TaskStatus::PartialFailed,
        TaskOutcome::PartialFailed,
        2,
        2,
        None,
    )
    .unwrap();
    id
}

#[test]
fn clearing_history_removes_finished_tasks_and_their_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("lib.sqlite")).unwrap();
    let a = finished_task_with_failures(&db, "/photos/a");
    let b = finished_task_with_failures(&db, "/photos/b");

    assert_eq!(db.finished_tasks(50).unwrap().len(), 2);
    let before: u64 = db.failure_groups().unwrap().iter().map(|g| g.count).sum();
    assert_eq!(before, 4, "两条任务各两条失败");

    let removed = db.clear_task_history().unwrap();
    assert_eq!(removed, 2, "两条已结束任务都要删掉");

    assert!(db.finished_tasks(50).unwrap().is_empty());
    assert!(
        db.task(a).unwrap().is_none() && db.task(b).unwrap().is_none(),
        "任务行本身必须删掉，否则任务页还会显示"
    );
    let after: u64 = db.failure_groups().unwrap().iter().map(|g| g.count).sum();
    assert_eq!(after, 0, "失败明细随任务级联删除，分类角标要归零");
}

#[test]
fn clearing_history_never_touches_unfinished_tasks() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("lib.sqlite")).unwrap();

    let done = finished_task_with_failures(&db, "/photos/done");
    // 三条活任务：等待中 / 运行中 / 用户暂停。
    let waiting = db.create_task(None, "/photos/waiting", TaskKind::FirstScan).unwrap();
    let running = db.create_task(None, "/photos/running", TaskKind::FirstScan).unwrap();
    db.set_task_status(running, TaskStatus::Scanning, None).unwrap();
    let paused = db.create_task(None, "/photos/paused", TaskKind::FirstScan).unwrap();
    db.set_task_user_paused(paused, true).unwrap();

    let removed = db.clear_task_history().unwrap();
    assert_eq!(removed, 1, "只有那条已结束的被删");
    assert!(db.task(done).unwrap().is_none());
    for id in [waiting, running, paused] {
        assert!(
            db.task(id).unwrap().is_some(),
            "未结束的任务一条都不能删（task_id={id}）"
        );
    }
    assert_eq!(db.unfinished_tasks().unwrap().len(), 3);
}

#[test]
fn clearing_history_does_not_touch_photos_thumbs_or_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("lib.sqlite")).unwrap();
    let root = db.upsert_root("/media/a", 0, false).unwrap();
    let fid = db.upsert_folder("/media/a", "/media/a", None).unwrap();
    db.set_folder_root(fid, root, None).unwrap();
    let input = |name: &str| picasa_db::PhotoInput {
        name: name.to_string(),
        name_key: name.to_ascii_lowercase(),
        size_bytes: 1000,
        mtime_ns: 1_000_000_000,
        width: Some(800),
        height: Some(600),
        orientation: Some(1),
        taken_at: Some(100),
        taken_at_src: 0,
        format: 1,
    };
    db.apply_scan(fid, "/media/a", &[input("a.jpg"), input("b.jpg")], 0)
        .unwrap();
    db.put_thumb(
        1,
        0,
        &picasa_db::ThumbBuf {
            w: 2,
            h: 2,
            format: 0,
            data: vec![0xFF, 0xD8, 0xFF, 0xD9],
        },
    )
    .unwrap();
    db.set_starred(&[1], true).unwrap();
    db.add_tag(&[1], "旅行").unwrap();
    db.set_caption(1, "海边").unwrap();

    finished_task_with_failures(&db, "/photos/a");
    let removed = db.clear_task_history().unwrap();
    assert_eq!(removed, 1);

    assert_eq!(db.total_photos().unwrap(), 2, "照片一张不能少");
    assert_eq!(db.cache_stats().unwrap().thumb_count, 1, "缩略图不能少");
    assert!(db.starred_ids().unwrap().contains(&1), "收藏不能丢");
    assert!(db.tags_of(1).unwrap().iter().any(|t| t == "旅行"), "标签不能丢");
    assert_eq!(db.caption_of(1).unwrap(), "海边", "说明不能丢");
}

#[test]
fn clearing_an_empty_history_reports_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("lib.sqlite")).unwrap();
    assert_eq!(db.clear_task_history().unwrap(), 0);
    // 幂等：再清一次还是 0，不报错。
    assert_eq!(db.clear_task_history().unwrap(), 0);
}
