//! M4.3 阶段 1 · 持久任务意图验收（IDX-07/08/09/12）。
//!
//! 每个用例都开一个**全新的 v9 库**（`Db::open` 跑完整迁移），互不干扰。
//! 覆盖：入队复用 / 检查点与暂停续跑往返 / 恢复与暂停队列 / 取消幂等 /
//! 全局取消 / 全局暂停开关 / 取消后不复活 / 按路径查 root。

use picasa_db::{Db, TaskKind, TaskOutcome, TaskScopeKind, TaskStatus};

/// 全新 v9 库。返回 `(TempDir, Db)`：TempDir 必须活着，否则目录被删、WAL 句柄失效。
fn fresh_db() -> (tempfile::TempDir, Db) {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let db = Db::open(tmp.path().join("t.db")).expect("open fresh v9 db");
    (tmp, db)
}

/// `begin_index_task` 只建一行；同范围二次入队**复用同一个 task_id**，
/// 且中途写入的 `checkpoint` / `user_paused` / `status` / `kind` / `scope_kind`
/// 全部保持不变（崩溃恢复的续跑位置不能被重复入队抹掉）。
#[test]
fn begin_index_task_creates_one_row_and_reuses_it_unchanged() {
    let (_tmp, db) = fresh_db();

    let id1 = db
        .begin_index_task(None, "/photos", TaskKind::FirstScan, TaskScopeKind::Root)
        .expect("first begin_index_task");
    let rows = db.task_intents_for_scope("/photos").expect("scope intents");
    assert_eq!(
        rows.len(),
        1,
        "first begin_index_task must create exactly one row"
    );
    assert_eq!(rows[0].task_id, id1, "the single row is the returned id");

    // 在两次调用之间写入用户意图，模拟「跑到一半」。
    db.set_task_checkpoint(id1, "cursor-42")
        .expect("set checkpoint");
    db.set_task_user_paused(id1, true)
        .expect("pause before reuse");

    // 故意换一个 kind 再入队：复用分支不应改写既有行。
    let id2 = db
        .begin_index_task(None, "/photos", TaskKind::Refresh, TaskScopeKind::All)
        .expect("second begin_index_task");
    assert_eq!(
        id2, id1,
        "second call on the same live scope must REUSE the same task_id"
    );
    let rows = db.task_intents_for_scope("/photos").expect("scope intents");
    assert_eq!(rows.len(), 1, "reuse must not insert a duplicate row");

    let t = db.task_intent(id1).expect("task_intent").expect("row exists");
    assert_eq!(
        t.checkpoint.as_deref(),
        Some("cursor-42"),
        "reuse must preserve the checkpoint"
    );
    assert!(t.user_paused, "reuse must preserve user_paused=true");
    assert_eq!(
        t.status,
        TaskStatus::Paused.code(),
        "reuse must preserve status (Paused)"
    );
    assert_eq!(
        t.kind,
        TaskKind::FirstScan.code(),
        "reuse must NOT reset kind to the newly passed Refresh"
    );
    assert_eq!(
        t.scope_kind,
        TaskScopeKind::Root,
        "reuse must NOT reset scope_kind to the newly passed All"
    );
}

/// `set_task_running` / `set_task_checkpoint` / `set_task_user_paused` 的往返，
/// 以及「已取消/不存在的行」上的幂等行为。
#[test]
fn running_checkpoint_and_user_paused_roundtrip() {
    let (_tmp, db) = fresh_db();
    let id = db
        .begin_index_task(None, "/p", TaskKind::FirstScan, TaskScopeKind::Root)
        .unwrap();

    // 先制造一个终态，验证 set_task_running 会把 finished_at 清回 NULL。
    db.finish_index_task(id, TaskStatus::Completed, TaskOutcome::Success, 5, 0, None)
        .expect("finish to set finished_at");
    assert!(
        db.task(id).unwrap().unwrap().finished_at.is_some(),
        "finish_index_task must set finished_at"
    );

    db.set_task_running(id, TaskStatus::Thumbnailing)
        .expect("set_task_running");
    let t = db.task_intent(id).unwrap().unwrap();
    assert_eq!(
        t.status,
        TaskStatus::Thumbnailing.code(),
        "set_task_running(Thumbnailing) status"
    );
    assert!(!t.user_paused, "set_task_running clears user_paused");
    assert!(
        db.task(id).unwrap().unwrap().finished_at.is_none(),
        "set_task_running clears finished_at"
    );

    db.set_task_checkpoint(id, "dir-0007")
        .expect("set_task_checkpoint");
    assert_eq!(
        db.task_intent(id).unwrap().unwrap().checkpoint.as_deref(),
        Some("dir-0007"),
        "checkpoint round-trip"
    );

    db.set_task_user_paused(id, true).expect("pause");
    let t = db.task_intent(id).unwrap().unwrap();
    assert!(t.user_paused, "set_task_user_paused(true) -> true");
    assert_eq!(
        t.status,
        TaskStatus::Paused.code(),
        "pausing sets status=Paused"
    );

    db.set_task_user_paused(id, false).expect("resume");
    let t = db.task_intent(id).unwrap().unwrap();
    assert!(!t.user_paused, "set_task_user_paused(false) -> false");
    assert_eq!(
        t.status,
        TaskStatus::Scanning.code(),
        "resuming sets status=Scanning"
    );

    // 不存在的 id：0 行变化也算成功（幂等）。
    db.set_task_user_paused(999_999, true)
        .expect("set_task_user_paused on missing id is Ok");
    assert!(
        db.task_intent(999_999).unwrap().is_none(),
        "task_intent on unknown id -> None"
    );

    // 取消之后，WHERE cancel_reason IS NULL 必须阻止「取消的任务被重新激活」。
    assert!(
        db.cancel_index_task(id, "user_cancel").unwrap(),
        "cancel this task"
    );
    db.set_task_user_paused(id, false)
        .expect("set_task_user_paused on cancelled id is Ok");
    let t = db.task_intent(id).unwrap().unwrap();
    assert_eq!(
        t.status,
        TaskStatus::Completed.code(),
        "cancelled task must not be revived to Scanning"
    );
    assert!(
        t.cancel_reason.is_some(),
        "cancelled task keeps its cancel_reason"
    );
}

/// 恢复队列 / 暂停队列 / 面板未完成队列 三者口径，以及老 `unfinished_tasks()`
/// 不返回已取消任务。
#[test]
fn resumable_paused_unfinished_and_old_api_partition() {
    let (_tmp, db) = fresh_db();
    let a = db
        .begin_index_task(None, "/a", TaskKind::FirstScan, TaskScopeKind::Root)
        .unwrap();
    let b = db
        .begin_index_task(None, "/b", TaskKind::Refresh, TaskScopeKind::Root)
        .unwrap();
    let c = db
        .begin_index_task(None, "/c", TaskKind::Reindex, TaskScopeKind::Root)
        .unwrap();

    db.set_task_user_paused(b, true).expect("pause b");
    assert!(db.cancel_index_task(c, "user_cancel").unwrap(), "cancel c");

    let resumable = db.resumable_index_tasks().unwrap();
    assert_eq!(
        resumable.iter().map(|t| t.task_id).collect::<Vec<_>>(),
        vec![a],
        "resumable must exclude paused(b) and cancelled(c)"
    );

    let paused = db.paused_index_tasks().unwrap();
    assert_eq!(
        paused.iter().map(|t| t.task_id).collect::<Vec<_>>(),
        vec![b],
        "paused must contain only the user-paused task"
    );

    let unfinished = db.unfinished_index_tasks().unwrap();
    assert_eq!(
        unfinished.iter().map(|t| t.task_id).collect::<Vec<_>>(),
        vec![a, b],
        "unfinished_index_tasks = resumable + paused, still excludes cancelled(c)"
    );

    let old = db.unfinished_tasks().unwrap();
    let old_ids = old.iter().map(|t| t.task_id).collect::<Vec<_>>();
    assert!(
        !old_ids.contains(&c),
        "old unfinished_tasks() must exclude the cancelled task"
    );
    assert!(
        old_ids.contains(&a) && old_ids.contains(&b),
        "old unfinished_tasks() still returns the live a and pausd b"
    );
}

/// `cancel_index_task` 幂等：第二次返回 false，`generation` 不再增长，
/// `cancel_reason` 不被覆盖；并落终态（outcome=Cancelled / finished_at / status）。
#[test]
fn cancel_index_task_is_idempotent_and_sets_terminal_state() {
    let (_tmp, db) = fresh_db();
    let id = db
        .begin_index_task(None, "/x", TaskKind::FirstScan, TaskScopeKind::Root)
        .unwrap();
    assert_eq!(
        db.task_intent(id).unwrap().unwrap().generation,
        0,
        "fresh task generation starts at 0"
    );

    assert!(
        db.cancel_index_task(id, "user_requested").unwrap(),
        "first cancel returns true"
    );
    let t = db.task_intent(id).unwrap().unwrap();
    assert_eq!(
        t.outcome,
        Some(TaskOutcome::Cancelled.code()),
        "cancel sets outcome=Cancelled"
    );
    assert_eq!(
        t.cancel_reason.as_deref(),
        Some("user_requested"),
        "cancel stores the reason"
    );
    assert_eq!(t.generation, 1, "cancel bumps generation by exactly 1");
    assert_eq!(
        t.status,
        TaskStatus::Completed.code(),
        "cancel moves status to a terminal state"
    );
    let row = db.task(id).unwrap().unwrap();
    assert!(row.finished_at.is_some(), "cancel sets finished_at");
    assert_eq!(
        row.error_code.as_deref(),
        Some("user_requested"),
        "cancel also writes error_code = reason"
    );

    assert!(
        !db.cancel_index_task(id, "another_reason").unwrap(),
        "second cancel must return false (idempotent)"
    );
    let t = db.task_intent(id).unwrap().unwrap();
    assert_eq!(
        t.generation, 1,
        "second cancel must NOT bump generation again"
    );
    assert_eq!(
        t.cancel_reason.as_deref(),
        Some("user_requested"),
        "second cancel must NOT overwrite cancel_reason"
    );

    assert!(
        !db.cancel_index_task(999_999, "nope").unwrap(),
        "cancel on a missing task returns false"
    );
}

/// `cancel_all_index_tasks` 只取消「活着」的**单根**任务，幂等（第二次 = 0）；
/// 已结束的与 `scope_kind = All` 的都不动。
#[test]
fn cancel_all_index_tasks_cancels_live_root_tasks_and_is_idempotent() {
    let (_tmp, db) = fresh_db();
    let t1 = db
        .begin_index_task(None, "/a", TaskKind::FirstScan, TaskScopeKind::Root)
        .unwrap();
    let t2 = db
        .begin_index_task(None, "/b", TaskKind::Refresh, TaskScopeKind::Root)
        .unwrap();
    let t3 = db
        .begin_index_task(None, "/c", TaskKind::Reindex, TaskScopeKind::Root)
        .unwrap();
    let all_scope = db
        .begin_index_task(None, "/all", TaskKind::FirstScan, TaskScopeKind::All)
        .unwrap();
    let finished = db
        .begin_index_task(None, "/d", TaskKind::Verify, TaskScopeKind::Root)
        .unwrap();
    db.finish_index_task(
        finished,
        TaskStatus::Completed,
        TaskOutcome::Success,
        1,
        0,
        None,
    )
    .unwrap();

    let n = db.cancel_all_index_tasks("shutdown").unwrap();
    assert_eq!(n, 3, "cancel_all must touch exactly the 3 live Root tasks");

    for id in [t1, t2, t3] {
        let t = db.task_intent(id).unwrap().unwrap();
        assert_eq!(
            t.cancel_reason.as_deref(),
            Some("shutdown"),
            "task {id} is cancelled with the reason"
        );
        assert_eq!(
            t.outcome,
            Some(TaskOutcome::Cancelled.code()),
            "task {id} outcome=Cancelled"
        );
        assert_eq!(t.generation, 1, "task {id} generation bumped to 1");
        assert_eq!(
            t.status,
            TaskStatus::Completed.code(),
            "task {id} status=Completed"
        );
    }
    assert!(
        db.task_intent(all_scope)
            .unwrap()
            .unwrap()
            .cancel_reason
            .is_none(),
        "scope_kind=All task must NOT be cancelled by cancel_all"
    );
    assert!(
        db.task_intent(finished)
            .unwrap()
            .unwrap()
            .cancel_reason
            .is_none(),
        "already-finished task must NOT be cancelled again"
    );

    assert_eq!(
        db.cancel_all_index_tasks("shutdown").unwrap(),
        0,
        "second cancel_all is idempotent and returns 0"
    );
}

/// 全局暂停开关默认关，写 true / false 都能往返。
#[test]
fn index_pause_all_defaults_false_and_roundtrips() {
    let (_tmp, db) = fresh_db();
    assert!(
        !db.index_pause_all().unwrap(),
        "m43_index_pause_all defaults to false"
    );
    db.set_index_pause_all(true)
        .expect("set_index_pause_all(true)");
    assert!(db.index_pause_all().unwrap(), "true round-trips");
    db.set_index_pause_all(false)
        .expect("set_index_pause_all(false)");
    assert!(!db.index_pause_all().unwrap(), "false round-trips");
    db.set_index_pause_all(true)
        .expect("set_index_pause_all(true) again");
    assert!(db.index_pause_all().unwrap(), "true round-trips again");
}

/// 已取消的任务不会被 `begin_index_task` 复活：同范围再入队拿到**新的 task_id**，
/// 且新行是干净的（无 checkpoint / 无暂停 / 无取消原因）。
#[test]
fn cancelled_task_is_not_resurrected_by_begin_index_task() {
    let (_tmp, db) = fresh_db();
    let id1 = db
        .begin_index_task(None, "/photos", TaskKind::FirstScan, TaskScopeKind::Root)
        .unwrap();
    db.set_task_checkpoint(id1, "half-way").unwrap();
    assert!(db.cancel_index_task(id1, "user_cancel").unwrap());

    let id2 = db
        .begin_index_task(None, "/photos", TaskKind::FirstScan, TaskScopeKind::Root)
        .unwrap();
    assert_ne!(
        id2, id1,
        "begin_index_task after cancel must create a NEW task_id, not reuse the cancelled one"
    );

    let t = db.task_intent(id2).unwrap().unwrap();
    assert!(t.checkpoint.is_none(), "new task has no checkpoint");
    assert!(!t.user_paused, "new task is not user-paused");
    assert!(t.cancel_reason.is_none(), "new task is not cancelled");
    assert_eq!(t.generation, 0, "new task generation starts at 0");
    assert_eq!(t.scope_kind, TaskScopeKind::Root, "new task is Root-scoped");

    let rows = db.task_intents_for_scope("/photos").unwrap();
    assert_eq!(rows.len(), 2, "scope now holds the cancelled + new task");
    assert_eq!(
        rows[0].task_id, id2,
        "task_intents_for_scope orders by task_id DESC (newest first)"
    );
}

/// `root_id_for_path` 命中原始路径与小写比较键，未知路径返回 None。
#[test]
fn root_id_for_path_finds_real_and_lowercased_paths() {
    let (_tmp, db) = fresh_db();
    let rid = db.upsert_root("/photos", 1, true).expect("upsert_root");

    assert_eq!(
        db.root_id_for_path("/photos").unwrap(),
        Some(rid),
        "exact path resolves to root_id"
    );
    assert_eq!(
        db.root_id_for_path("/PHOTOS").unwrap(),
        Some(rid),
        "lowercased path_key also resolves to root_id"
    );
    assert_eq!(
        db.root_id_for_path("/unknown").unwrap(),
        None,
        "unknown path resolves to None"
    );
}
