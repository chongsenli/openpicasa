//! M2.1 迁移（v1 → v2）的验收测试。
//!
//! 覆盖 `docs/05` §1.4 的七步：判定 → 备份 → 单事务 DDL → 迁 roots → 推导 folders →
//! 置待回填 → 失败回滚。**夹具是手工造的 v1 库**（用 `MIGRATIONS[0].sql` 建表并插数据），
//! 因为「用新代码建一个 v1 库」这件事本身已经不可能了。
//!
//! 另有一个**真实库样本**测试：设 `OPENPICASA_M21_SAMPLE=<v1 库副本路径>` 才跑，
//! 不设就跳过（CI 与其他机器上没有这个样本）。

use picasa_db::{Db, FolderTreeRow, MIGRATIONS, RootSeed, SCHEMA_VERSION, TaskKind, TaskStatus};
use std::path::{Path, PathBuf};

/// 造一个 v1 库：两个根目录下的目录，其中一个缺中间层（`2020/01`），
/// 一张照片 orientation 为 NULL、一张 6（旋转 90°）、一张宽高缺失。
fn make_v1_db(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (
             version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
    )
    .unwrap();
    conn.execute_batch(MIGRATIONS[0].sql).unwrap();
    conn.execute("INSERT INTO schema_migrations VALUES (1, 0)", [])
        .unwrap();
    conn.execute_batch(
        r#"
        INSERT INTO folders (id, path, path_key, is_online, last_scan_at) VALUES
            (1, '/photos',              '/photos',              1, 100),
            (2, '/photos/2020/01/02',   '/photos/2020/01/02',   1, 100),
            (3, '/photos/未分组',        '/photos/未分组',        1, 100),
            (4, '/music',               '/music',               1, 100);
        INSERT INTO photos
            (folder_id, name, name_key, size_bytes, mtime_ns, width, height, orientation,
             taken_at, taken_at_src, thumb_state, decoder_ver) VALUES
            (1, 'a.jpg', 'a.jpg', 10, 1, 4000, 3000, NULL, 100, 0, 0, 0),
            (2, 'b.jpg', 'b.jpg', 20, 2, 2000, 3000, 6,    200, 0, 0, 0),
            (3, 'c.jpg', 'c.jpg', 30, 3, NULL, NULL, NULL, 300, 0, 0, 0);
        "#,
    )
    .unwrap();
}

fn seed(path: &str) -> RootSeed {
    RootSeed {
        path: path.to_string(),
        path_key: path.to_lowercase(),
        added_at: 1_700_000_000,
        case_insensitive: Some(false),
    }
}

fn open_fixture(dir: &Path) -> (Db, PathBuf) {
    let db_path = dir.join("library.sqlite");
    make_v1_db(&db_path);
    let (db, _report) = Db::open_with_report(&db_path).unwrap();
    (db, db_path)
}

#[test]
fn upgrade_to_v2_keeps_a_brand_new_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_fixture(tmp.path());

    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    // v2 是数据层迁移（roots/层级/尺寸状态），v3 是排序生成列，
    // v4 是「取待补缩略图」的复合索引——对「v1 库升级」来说都该一口气跑完，
    // 最终落在 SCHEMA_VERSION。
    assert_eq!(SCHEMA_VERSION, 4, "v4 加待补缩略图索引后版本必须是 4");

    let report = db.last_upgrade_note().expect("升级现场应被记录");
    assert_eq!(report.from, 1);
    assert_eq!(report.to, SCHEMA_VERSION);
    assert!(report.error.is_none());

    let backup = report.backup.clone().expect("v1 → v2 必须先备份");
    assert!(backup.exists(), "备份文件应真实存在");
    assert!(
        backup.starts_with(Db::backup_dir(&db_path)),
        "备份必须落在应用容器的 Backups/ 里"
    );
    // 备份是**自洽的 v1 库**：能打开、版本还是 1、数据在
    let b = rusqlite::Connection::open(&backup).unwrap();
    let v: i64 = b
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(v, 1, "备份必须是升级前的版本（降级要用它覆盖）");
    let n: i64 = b
        .query_row("SELECT COUNT(*) FROM photos", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3, "备份必须包含数据，不能只是个空骨架");
}

#[test]
fn roots_are_migrated_and_folder_hierarchy_is_derived() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    // 还没跑数据步骤
    assert!(db.needs_v2_data().unwrap());

    let rep = db
        .apply_v2_data(&[seed("/photos"), seed("/music")])
        .unwrap();
    assert!(rep.applied);
    assert_eq!(rep.roots, 2);
    assert!(
        rep.folders_created >= 1,
        "`/photos/2020/01` 没有照片行，必须补出中间层，否则树断了"
    );

    // ---- roots
    let roots = db.roots(false).unwrap();
    assert_eq!(roots.len(), 2);
    let photos_root = roots.iter().find(|r| r.path == "/photos").unwrap();
    assert!(photos_root.is_active());
    assert!(!photos_root.case_insensitive, "种子里明确写了区分大小写");

    // ---- 中间层补出来了，父链连得上
    // 夹具里 `/photos/2020` 与 `/photos/2020/01` 都没有行，两层都要补出来
    let all = db.folders_of_root(photos_root.id).unwrap();
    let by_path = |p: &str| -> &FolderTreeRow { all.iter().find(|f| f.path == p).unwrap() };
    let year = by_path("/photos/2020");
    assert_eq!(year.parent_id, Some(1), "2020 的父目录应是 /photos（id=1）");
    let mid = by_path("/photos/2020/01");
    assert_eq!(mid.parent_id, Some(year.id), "缺一层就会断链");
    let deep = by_path("/photos/2020/01/02");
    assert_eq!(deep.parent_id, Some(mid.id));
    assert_eq!(deep.name.as_deref(), Some("02"));
    assert!(by_path("/photos").has_children);
    assert_eq!(by_path("/photos").direct_count, 1, "根目录只直接含 a.jpg");

    // 两个根各管一段：`/music` 归第二个根，`/photos` 那段不串到它名下
    let music_root = roots.iter().find(|r| r.path == "/music").unwrap();
    let music_nodes = db.folders_of_root(music_root.id).unwrap();
    assert_eq!(music_nodes.len(), 1, "/music 下没有子目录");
    let tops = db.children_of(None).unwrap();
    assert_eq!(
        tops.iter().find(|f| f.path == "/music").unwrap().root_id,
        Some(music_root.id)
    );

    // ---- orientation 空值归一（唯一一处改写已有行）
    let a = db.photo(1).unwrap().unwrap();
    assert_eq!(a.orientation, Some(1), "NULL 方向必须归一为 1");
    // b.jpg 是 6（旋转 90°），生成列应交换宽高
    let (dw, dh) = db.display_dims(2).unwrap().unwrap();
    assert_eq!(
        (dw, dh),
        (Some(3000), Some(2000)),
        "orientation=6 要交换宽高"
    );

    // ---- 尺寸状态：有宽高的算完成，没有的才待回填
    let stats = db.dims_stats().unwrap();
    assert_eq!((stats.total, stats.done, stats.pending), (3, 2, 1));
    let pending = db.photos_pending_dims(None, 10).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].name, "c.jpg");
}

#[test]
fn apply_v2_data_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());
    let first = db.apply_v2_data(&[seed("/photos")]).unwrap();
    assert!(first.applied);

    let second = db.apply_v2_data(&[seed("/photos")]).unwrap();
    assert!(!second.applied, "第二次必须是空操作");
    assert_eq!(second.roots, 0);
    assert!(!db.needs_v2_data().unwrap());
    assert_eq!(db.roots(false).unwrap().len(), 1);
}

#[test]
fn failed_migration_rolls_back_and_restores_the_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("library.sqlite");
    make_v1_db(&db_path);
    // 故意制造 DDL 冲突：v2 要建 roots 表，这里先占位
    {
        let c = rusqlite::Connection::open(&db_path).unwrap();
        c.execute_batch("CREATE TABLE roots (id INTEGER PRIMARY KEY);")
            .unwrap();
    }

    let (db, report) = Db::open_with_report(&db_path).unwrap();
    assert!(report.error.is_some(), "冲突必须被报出来，不能静默跳过");
    assert!(report.restored_from_backup, "失败后应已用备份覆盖");
    assert_eq!(report.to, 1, "失败时版本停在 1");
    assert_eq!(
        db.schema_version().unwrap(),
        1,
        "应用要以 M2 数据继续运行，不是拒绝启动"
    );
    // 数据还在（这就是「迁移失败可回退」的可判定性）
    assert_eq!(db.total_photos().unwrap(), 3);
    // 没有生成列时 dims_status 查不了——但旧列仍可读
    let photos = db
        .photos(Some(&[1]), picasa_db::SortSpec::default(), 10)
        .unwrap();
    assert_eq!(photos.len(), 1);
}

#[test]
fn schema_too_new_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("library.sqlite");
    {
        let c = rusqlite::Connection::open(&db_path).unwrap();
        c.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
             INSERT INTO schema_migrations VALUES (99, 0);",
        )
        .unwrap();
    }
    let err = match Db::open(&db_path) {
        Ok(_) => panic!("版本 99 的库必须被拒绝"),
        Err(e) => e,
    };
    assert!(
        matches!(err, picasa_db::DbError::SchemaTooNew { found: 99, .. }),
        "降级不支持的判据是 SchemaTooNew，不能放宽：{err}"
    );
}

#[test]
fn dims_backfill_updates_generated_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());
    db.apply_v2_data(&[seed("/photos")]).unwrap();

    // c.jpg 回填成功
    db.set_photo_dims(3, Some(800), Some(600), Some(8), true)
        .unwrap();
    let (dw, dh) = db.display_dims(3).unwrap().unwrap();
    assert_eq!((dw, dh), (Some(600), Some(800)), "orientation=8 也要交换");
    assert_eq!(db.dims_stats().unwrap().done, 3);

    // 失败只标状态，不清已有值
    db.set_photo_dims(3, None, None, None, false).unwrap();
    assert_eq!(db.dims_stats().unwrap().failed, 1);
    let p = db.photo(3).unwrap().unwrap();
    assert_eq!(
        (p.width, p.height),
        (Some(800), Some(600)),
        "失败不该抹掉旧值"
    );
}

#[test]
fn roots_close_and_reopen_reuses_the_same_id() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());
    db.apply_v2_data(&[seed("/photos")]).unwrap();
    let id = db.root_by_path("/photos").unwrap().unwrap().id;

    db.set_root_closed(id, true).unwrap();
    assert!(db.roots(false).unwrap().is_empty(), "关闭后不应出现在树里");
    assert_eq!(db.roots(true).unwrap().len(), 1, "但记录与数据都在");
    assert_eq!(
        db.total_photos().unwrap(),
        3,
        "关闭根目录绝不删照片（P-07：direct_count 是物理事实）"
    );

    // 再次添加同一路径 → 复用 root_id
    let again = db.upsert_root("/photos", 1_800_000_000, false).unwrap();
    assert_eq!(again, id, "再次添加必须复用原 root_id（秒开的实现基础）");
    assert_eq!(db.roots(false).unwrap().len(), 1);
}

#[test]
fn exclusions_are_case_rules_aware() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());
    // /photos 区分大小写（seed 里 case_insensitive = false）
    db.apply_v2_data(&[seed("/photos")]).unwrap();
    let rid = db.root_by_path("/photos").unwrap().unwrap().id;

    // 区分大小写的卷：match_key 保留原样（`Photos` 与 `photos` 是两个目录）
    assert_eq!(db.match_key_for(rid, "Private/Sub").unwrap(), "Private/Sub");

    // 不区分大小写的卷：match_key 必须小写，否则关掉 `Photos` 对 `photos` 不生效
    let mac = db.upsert_root("/Volumes/Mac", 0, true).unwrap();
    assert_eq!(db.match_key_for(mac, "Private/Sub").unwrap(), "private/sub");

    db.add_exclusion(rid, "Private/Sub").unwrap();
    db.add_exclusion(rid, "Private/Sub").unwrap();
    assert_eq!(db.exclusions(rid).unwrap().len(), 1, "同路径不允许重复排除");
    let keys = db.exclusion_keys(rid).unwrap();
    assert!(keys.contains("Private/Sub"));

    // NFC 归一：分解形式与预组合形式是同一个排除项
    db.add_exclusion(rid, "Cafe\u{0301}").unwrap();
    assert_eq!(db.exclusions(rid).unwrap().len(), 2);
    assert!(db.exclusion_keys(rid).unwrap().contains("Café"));
}

#[test]
fn task_history_records_failures_per_entry() {
    let db = Db::open_in_memory().unwrap();
    let root = db.upsert_root("/photos", 0, true).unwrap();
    let task = db
        .create_task(Some(root), "/photos", TaskKind::FirstScan)
        .unwrap();
    db.set_task_status(task, TaskStatus::Thumbnailing, None)
        .unwrap();
    db.record_task_failure(task, "bad.jpg", "decode").unwrap();
    db.record_task_failure(task, "bad.jpg", "decode").unwrap();
    db.set_task_status(task, TaskStatus::PartialFailed, Some("partial"))
        .unwrap();

    let fails = db.task_failures(task).unwrap();
    assert_eq!(fails.len(), 1, "同一条目重试多次仍然只有一行");
    assert_eq!(fails[0].2, 2, "attempts 累加");
    let row = db.task(task).unwrap().unwrap();
    assert_eq!(row.status, TaskStatus::PartialFailed.code());
    assert!(row.finished_at.is_some(), "终态要补 finished_at");
}

/// 真实库样本：`OPENPICASA_M21_SAMPLE=/path/to/v1-copy.sqlite cargo test -p picasa-db -- --nocapture`
///
/// 样本是**副本**，测试会原地升级它，所以每次跑之前要重新拷一份。
/// 关注目录用 `OPENPICASA_M21_ROOTS=/a,/b` 指定（不指定就只验层级与尺寸状态）。
#[test]
fn real_m2_library_sample_upgrades() {
    let Ok(sample) = std::env::var("OPENPICASA_M21_SAMPLE") else {
        eprintln!("跳过：未设置 OPENPICASA_M21_SAMPLE");
        return;
    };
    let db_path = PathBuf::from(&sample);
    assert!(db_path.exists());

    let seeds: Vec<RootSeed> = std::env::var("OPENPICASA_M21_ROOTS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| RootSeed {
            path: s.to_string(),
            path_key: s.to_lowercase(),
            added_at: 1_700_000_000,
            case_insensitive: Some(true),
        })
        .collect();

    let (db, report) = Db::open_with_report(&db_path).unwrap();
    assert!(
        report.error.is_none(),
        "真实样本不该升不上去：{:?}",
        report.error
    );
    assert!(report.backup.is_some());
    let rep = db.apply_v2_data(&seeds).unwrap();
    eprintln!("真实样本：{rep:?}");

    let stats = db.dims_stats().unwrap();
    let total = db.total_photos().unwrap();
    assert_eq!(stats.total, total);
    assert!(stats.done > 0, "真实库里大部分照片本来就有宽高");
    assert_eq!(
        stats.done + stats.pending + stats.failed,
        total,
        "三种状态必须刚好覆盖全表"
    );

    // 每个目录都要有名字与 path_key；给了根目录的还要挂上 root_id
    let mut checked = 0;
    for r in db.roots(true).unwrap() {
        for f in db.folders_of_root(r.id).unwrap() {
            assert!(f.name.is_some(), "{} 没有推导出显示名", f.path);
            checked += 1;
        }
    }
    if !seeds.is_empty() {
        assert!(checked > 0, "给了根目录却一个节点都没挂上");
    }
    // 全库不再有 NULL 方向
    let pending = db.photos_pending_dims(None, 100_000).unwrap();
    assert!(
        pending.iter().all(|p| p.orientation.is_some()),
        "待回填的照片也不该再有 NULL 方向"
    );
}
