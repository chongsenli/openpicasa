//! M4.3 Phase 1B · IDX-17 终态摘要验收。
//!
//! 三条用例各自开一个**全新的 v9 库**（`Db::open` 跑完整迁移），互不干扰：
//! 1. 失败按「文件」计，不按「重试次数」计（重复失败只累加 attempts）；
//! 2. 「取消」与「部分成功」是两种可区分的终态，且跨重启可追溯；
//! 3. `TaskRow` 历史行带齐 v9 意图字段（cancel_reason / user_paused /
//!    scope_kind / generation），默认值与写入值都能读回。
//!
//! 风格对齐 `tests/maintenance.rs` / `tests/m43_task_intent.rs`：
//! `tempfile::tempdir()` + `Db::open(tmp.path().join("lib.sqlite"))`，全部用 `picasa_db::{...}`。

use picasa_db::{Db, FailureKind, TaskKind, TaskOutcome, TaskStatus};

/// ① 同一失败条目重试三次，仍只算**一个失败文件**。
///
/// 陷阱：`index_task_failures` 主键是 `(task_id, relative_path)`，重复失败靠
/// `attempts` 累加而不是插新行。若按尝试次数统计，面板会把「重试 3 次」
/// 显示成「3 个失败文件」——即契约里「不以重试次数冒充失败文件数」。
#[test]
fn repeated_failures_count_as_one_file_not_attempts() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("lib.sqlite")).unwrap();

    let id = db
        .create_task(None, "/photos", TaskKind::FirstScan)
        .unwrap();

    // 对同一个文件以同一分类重复记三次失败。
    for _ in 0..3 {
        db.record_task_failure_kind(id, "2020/a.jpg", "decode", FailureKind::Corrupt)
            .unwrap();
    }

    // task_failures 只有一行，且 attempts == 3。
    let failures = db.task_failures(id).unwrap();
    assert_eq!(failures.len(), 1, "重复失败必须收敛成一行，不是三行");
    assert_eq!(failures[0].2, 3, "同一条目的 attempts 累加到 3");

    // failure_groups() 下标即 kind-1：Corrupt(2) -> index 1，count 应为 1（不是 3）。
    let groups = db.failure_groups().unwrap();
    let corrupt_idx = FailureKind::Corrupt.code() as usize - 1;
    assert_eq!(
        groups[corrupt_idx].count, 1,
        "失败分组按文件数聚合，不能因重试三次变成 3"
    );

    // failed_task_count 数的是任务（COUNT DISTINCT task_id），也是 1。
    assert_eq!(db.failed_task_count().unwrap(), 1);
}

/// ② 取消 vs 部分成功：两种终态可区分，且**跨重启**仍可追溯。
///
/// 关键在重新 `Db::open` 同一个路径后再取 `TaskRow`：v9 的 cancel_reason /
/// outcome 是落库列，不是内存态，重开必须原样读回。
#[test]
fn cancelled_and_partial_failed_are_distinguishable_across_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("lib.sqlite");

    let db = Db::open(&path).unwrap();
    let t1 = db
        .create_task(None, "/photos/a", TaskKind::FirstScan)
        .unwrap();
    let t2 = db
        .create_task(None, "/photos/b", TaskKind::FirstScan)
        .unwrap();

    // t1：部分失败（跑完了，但有 2 张失败）。
    db.finish_task(
        t1,
        TaskStatus::PartialFailed,
        TaskOutcome::PartialFailed,
        10,
        2,
        Some("decode"),
    )
    .unwrap();
    // t2：被用户取消（cancel_index_task 落 outcome=Cancelled + cancel_reason）。
    db.cancel_index_task(t2, "用户取消").unwrap();

    // 模拟进程重启：丢弃旧连接，用同一路径重开。
    drop(db);
    let db = Db::open(&path).unwrap();

    let r1 = db.task(t1).unwrap().expect("t1 跨重启仍存在");
    let r2 = db.task(t2).unwrap().expect("t2 跨重启仍存在");

    // ---- t1：部分成功
    assert_eq!(r1.outcome(), Some(TaskOutcome::PartialFailed));
    assert_eq!(r1.status, TaskStatus::PartialFailed.code());
    assert_eq!(r1.failed, 2);
    assert_eq!(r1.processed, 10);
    assert!(
        r1.cancel_reason.is_none(),
        "部分成功不是取消，cancel_reason 必须为空"
    );

    // ---- t2：取消
    assert_eq!(r2.outcome(), Some(TaskOutcome::Cancelled));
    assert_eq!(
        r2.cancel_reason.as_deref(),
        Some("用户取消"),
        "取消原因跨重启可追溯"
    );

    // ---- 两者结局必须不同（这才是 IDX-17 终态摘要要分辨的东西）
    assert_ne!(
        r2.outcome(),
        r1.outcome(),
        "取消与部分成功必须是两种可区分的终态"
    );
}

/// ③ 历史行（TaskRow）带齐 v9 意图字段：默认值合理，写入值能读回。
#[test]
fn history_rows_carry_v9_intent_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("lib.sqlite")).unwrap();

    // 先建一条并终态化，检查四个 v9 字段的默认值（0 / false / None）。
    let id1 = db
        .create_task(None, "/photos", TaskKind::FirstScan)
        .unwrap();
    db.finish_task(
        id1,
        TaskStatus::Completed,
        TaskOutcome::Success,
        1,
        0,
        None,
    )
    .unwrap();

    let rows = db.recent_tasks(10).unwrap();
    let row1 = rows
        .iter()
        .find(|r| r.task_id == id1)
        .expect("recent_tasks 应返回刚结束的任务");
    assert_eq!(row1.scope_kind, 0, "默认 scope_kind = 0 (Root)");
    assert!(!row1.user_paused, "默认 user_paused = false");
    assert_eq!(row1.generation, 0, "默认 generation = 0");
    assert!(row1.cancel_reason.is_none(), "默认 cancel_reason = None");

    // 另一条：写入用户暂停意图与作废世代，再读回。
    let id2 = db
        .create_task(None, "/photos2", TaskKind::Refresh)
        .unwrap();
    db.set_task_user_paused(id2, true).unwrap();
    db.bump_task_generation(id2).unwrap();

    let row2 = db.task(id2).unwrap().expect("id2 存在");
    assert!(row2.user_paused, "set_task_user_paused(true) 落库可读回");
    assert!(row2.generation >= 1, "bump_task_generation 使 generation >= 1");
}
