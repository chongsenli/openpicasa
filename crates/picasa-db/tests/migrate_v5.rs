//! M2.2 数据层迁移 v4 → v5 的验收测试（`docs/06` §5）。
//!
//! 规格里这条叫 `tests/migrate_v3.rs`（它成文时代码停在 v2）；本仓库的 v4 已经落地，
//! 所以这里的文件名与断言都按**代码里的真实版本号 5** 写，偏差已登记进 §12.1。
//!
//! 覆盖 §5.5 列的四件事：升级成功、幂等、备份在应用容器、中途失败可回滚。
//! 另外顺带守住 §4.5/§4.7 的行为：失败分类补数、忽略只是标记、任务历史淘汰。

use picasa_db::{
    Db, FailureKind, MIGRATIONS, SCHEMA_VERSION, TaskKind, TaskOutcome, TaskStatus,
    classify_failure,
};
use std::path::{Path, PathBuf};

/// 造一个 **v4** 库：建到 v4 为止，写 `schema_migrations = 1..4`，
/// 塞进目录/照片/一个任务/三条历史失败（error_code 各不相同，供分类补数验证）。
fn make_v4_db(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (
             version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
    )
    .unwrap();
    for m in MIGRATIONS.iter().take(4) {
        conn.execute_batch(m.sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations VALUES (?1, 0)",
            rusqlite::params![m.version],
        )
        .unwrap();
    }
    conn.execute_batch(
        r#"
        INSERT INTO folders (id, path, path_key, is_online, last_scan_at) VALUES
            (1, '/photos',          '/photos',          1, 100),
            (2, '/photos/2020',     '/photos/2020',     1, 100);
        INSERT INTO photos
            (folder_id, name, name_key, size_bytes, mtime_ns, width, height, orientation,
             taken_at, taken_at_src, thumb_state, decoder_ver) VALUES
            (1, 'a.jpg', 'a.jpg', 10, 1, 4000, 3000, 1, 100, 0, 1, 0),
            (1, 'b.jpg', 'b.jpg', 20, 2, 4000, 3000, 1, 200, 0, 2, 0),
            (2, 'c.jpg', 'c.jpg', 30, 3, 4000, 3000, 1, 300, 0, 3, 0);
        INSERT INTO index_tasks (task_id, root_id, scope_path, kind, status, started_at)
            VALUES (1, NULL, '/photos', 0, 2, 1000);
        INSERT INTO index_task_failures (task_id, relative_path, error_code, attempts, updated_at)
            VALUES (1, 'b.jpg', 'permission denied',        1, 100),
                   (1, 'c.jpg', 'unsupported format',       1, 100),
                   (1, 'gone.jpg', 'No such file or directory', 1, 100);
        "#,
    )
    .unwrap();
}

fn open_fixture(dir: &Path) -> (Db, PathBuf) {
    let db_path = dir.join("library.sqlite");
    make_v4_db(&db_path);
    let (db, _report) = Db::open_with_report(&db_path).unwrap();
    (db, db_path)
}

#[test]
fn v4_library_upgrades_to_v5_with_rows_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_fixture(tmp.path());

    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 5);

    // 行数一条都不能变（只增列，不动数据）
    let n: i64 = db
        .total_photos()
        .unwrap()
        .try_into()
        .expect("照片数不应溢出");
    assert_eq!(n, 3);

    let report = db.last_upgrade_note().expect("升级现场应被记录");
    assert_eq!(report.from, 4);
    assert_eq!(report.to, SCHEMA_VERSION);
    assert!(report.error.is_none());

    let backup = report.backup.clone().expect("升级前必须先备份");
    assert!(backup.exists());
    assert!(
        backup.starts_with(Db::backup_dir(&db_path)),
        "备份必须落在应用容器，绝不能落在照片目录（R-1）"
    );
    // 备份是**自洽的 v4 库**：打开后版本还是 4 —— 降级要用它覆盖
    let b = rusqlite::Connection::open(&backup).unwrap();
    let v: i64 = b
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(v, 4, "备份必须是升级前的版本");
}

#[test]
fn migration_is_idempotent_when_run_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_fixture(tmp.path());

    // 第二次打开同一个文件：migrate 应变成空操作
    let (db2, report2) = Db::open_with_report(&db_path).unwrap();
    assert_eq!(db2.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(report2.from, SCHEMA_VERSION);
    assert!(report2.backup.is_none(), "已是最新就不该再备份一次");

    // 数据步骤跑两次结果相同（补数是幂等的）
    let first = db.backfill_failure_kinds().unwrap();
    let second = db.backfill_failure_kinds().unwrap();
    assert_eq!(
        second, 0,
        "第二次补数必须什么都没改（第一次改了 {first} 条）"
    );
    let groups = db.failure_groups().unwrap();
    let sum: u64 = groups.iter().map(|g| g.count + g.ignored).sum();
    assert_eq!(sum, 3, "分类不得丢失或凭空增加失败条目");
}

#[test]
fn failure_kinds_are_backfilled_from_legacy_error_text() {
    // §5.3：历史 `error_code` 文本 → `kind`。推断不出才回落 Unreachable。
    assert_eq!(
        classify_failure("permission denied"),
        FailureKind::NoPermission
    );
    assert_eq!(classify_failure("EACCES"), FailureKind::NoPermission);
    assert_eq!(
        classify_failure("unsupported format"),
        FailureKind::Unsupported
    );
    assert_eq!(classify_failure("decode timeout"), FailureKind::Timeout);
    assert_eq!(classify_failure("corrupt jpeg"), FailureKind::Corrupt);
    // ⚠️ 关键：认不出来的**不许**标成 Corrupt（那等于把好照片判死）
    assert_eq!(
        classify_failure("No such file or directory"),
        FailureKind::Unreachable
    );
    assert_eq!(classify_failure(""), FailureKind::Unreachable);

    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());
    let groups = db.failure_groups().unwrap();
    let of = |k: FailureKind| -> u64 {
        groups
            .iter()
            .find(|g| g.kind == Some(k.code()))
            .map(|g| g.count)
            .unwrap_or(0)
    };
    assert_eq!(of(FailureKind::NoPermission), 1);
    assert_eq!(of(FailureKind::Unsupported), 1);
    assert_eq!(of(FailureKind::Unreachable), 1);
}

#[test]
fn ignoring_a_kind_is_a_marker_not_a_deletion() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    assert_eq!(
        db.ignore_failures_of_kind(FailureKind::Unsupported)
            .unwrap(),
        1
    );

    let groups = db.failure_groups().unwrap();
    let g = groups
        .iter()
        .find(|g| g.kind == Some(FailureKind::Unsupported.code()))
        .unwrap();
    // 角标只算 count → 归零
    assert_eq!(g.count, 0);
    // 但条目**还在**（R-M2-4：忽略只是标记，不是隐藏）
    assert_eq!(g.ignored, 1);
    let all = db
        .failures_of_kind(FailureKind::Unsupported, true, 10)
        .unwrap();
    assert_eq!(all.len(), 1, "已忽略的失败项必须仍可列出");

    // 取消忽略 → 回到角标
    assert_eq!(
        db.unignore_failures_of_kind(FailureKind::Unsupported)
            .unwrap(),
        1
    );
    let groups = db.failure_groups().unwrap();
    let g = groups
        .iter()
        .find(|g| g.kind == Some(FailureKind::Unsupported.code()))
        .unwrap();
    assert_eq!((g.count, g.ignored), (1, 0));
}

#[test]
fn retry_targets_only_the_named_photo_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    // b.jpg 是 NoPermission。解析出来的 id 必须**只有它自己**，
    // 不能把同目录其它 thumb_state=0 的历史欠账也算进来（§4.3 边界）。
    let ids = db
        .photo_ids_of_failure_kind(FailureKind::NoPermission)
        .unwrap();
    assert_eq!(ids.len(), 1, "只有点名的那一条能进重试队列");
    let row = db.photo(ids[0]).unwrap().unwrap();
    assert_eq!(row.name, "b.jpg");

    // 忽略之后就不再被点名
    let _ = db
        .ignore_failures_of_kind(FailureKind::NoPermission)
        .unwrap();
    assert!(
        db.photo_ids_of_failure_kind(FailureKind::NoPermission)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn task_history_keeps_only_the_newest_fifty() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    // 造 60 个已结束任务
    for i in 0..60 {
        let id = db
            .create_task(None, &format!("/p/{i}"), TaskKind::Refresh)
            .unwrap();
        db.finish_task(id, TaskStatus::Completed, TaskOutcome::Success, 10, 0, None)
            .unwrap();
    }
    let _ = db.prune_task_history(50).unwrap();
    let history = db.finished_tasks(200).unwrap();
    assert_eq!(history.len(), 50, "历史必须恰好保留 50 条");
    // 按结束时间倒序：第一条是最后结束的
    let mut sorted = history.clone();
    sorted.sort_by_key(|t| std::cmp::Reverse((t.finished_at.unwrap_or(0), t.task_id)));
    assert_eq!(history[0].task_id, sorted[0].task_id);

    // 未结束的任务**一条都不能被淘汰**
    let running = db
        .create_task(None, "/p/running", TaskKind::FirstScan)
        .unwrap();
    let _ = db.prune_task_history(50).unwrap();
    assert!(
        db.task(running).unwrap().is_some(),
        "进行中的任务不能被淘汰"
    );
}

#[test]
fn finish_task_writes_outcome_and_duration_in_one_step() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    let id = db.create_task(None, "/photos", TaskKind::Reindex).unwrap();
    db.finish_task(
        id,
        TaskStatus::PartialFailed,
        TaskOutcome::PartialFailed,
        42,
        3,
        Some("部分失败"),
    )
    .unwrap();

    let t = db.task(id).unwrap().unwrap();
    assert_eq!(t.outcome(), Some(TaskOutcome::PartialFailed));
    assert_eq!(t.outcome_label(), "部分失败");
    assert_eq!((t.processed, t.failed), (42, 3));
    assert!(t.finished_at.is_some());
    assert_eq!(
        t.duration_ms,
        Some(0),
        "started_at 与 now 同秒时为 0，不为空"
    );
    // 「已用 mm:ss」不显示 ETA（§4.2 M22-IDX-08）
    assert_eq!(t.elapsed_text(), "00:00");
}

#[test]
fn a_failed_migration_leaves_the_library_at_v4() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("library.sqlite");
    make_v4_db(&db_path);

    // 在 v5 的 DDL 里插一条必然失败的语句，模拟「迁移跑一半炸了」。
    // 做法：手工把 v4→v5 的 DDL 拆开，先跑一条合法的，再跑一条非法的。
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    conn.execute_batch("ALTER TABLE index_tasks ADD COLUMN outcome INTEGER;")
        .unwrap();
    let broken = conn.execute_batch("ALTER TABLE index_tasks ADD COLUMN nope INTEGER NOT NULL DEFAULT 0 REFERENCES nonexistent(id);");
    assert!(broken.is_err(), "这条 DDL 必须失败，否则本测试没有意义");
    conn.execute_batch("ROLLBACK").unwrap();
    drop(conn);

    // 回滚之后：库仍在 v4，数据完好
    let check = rusqlite::Connection::open(&db_path).unwrap();
    let v: i64 = check
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(v, 4, "失败的 DDL 必须整体回滚，版本号停在 4");
    let n: i64 = check
        .query_row("SELECT COUNT(*) FROM photos", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3, "数据一条都不能少");
    drop(check);

    // 重开：从 v4 正常升到 v5（不会留下半截 schema）
    let (db, report) = Db::open_with_report(&db_path).unwrap();
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    assert!(report.error.is_none());
    assert_eq!(db.total_photos().unwrap(), 3);
}
