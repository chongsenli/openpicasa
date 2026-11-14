//! M4.3 阶段 4 · `TC-M43-MIG-01` 的「失败回滚」一条（此前只有成功路径的用例）。
//!
//! ## 为什么需要这一条
//!
//! `migrate_v9.rs` 覆盖的是「v8 能升上来、数据守恒、幂等」；本文件覆盖的是
//! **升不上来的时候会发生什么**：v9 的 DDL 在单事务里执行，任何一步失败都必须
//! 整体回滚、用升级前备份覆盖、并把现场留在 `UpgradeReport.error` 里，同时
//! **旧库必须继续可用**（用户不能因为一次失败的迁移就打不开图库）。
//!
//! ## 怎么注入一次真实的失败
//!
//! 不 mock、不改生产代码：在一个**真正的 v8 库**上手工加一列 `user_paused`，
//! 于是 v9 的 `ALTER TABLE index_tasks ADD COLUMN user_paused ...` 会撞
//! `duplicate column name`。这种库在真实世界里对应「被外部工具动过 / 手工改过」
//! 的索引库，比构造一个语法错误的 SQL 更接近现实。
//!
//! 断言重点：
//! 1. 报告里 `from == to == 8`、`error.is_some()`、`restored_from_backup == true`；
//! 2. **v9 的对象一个都不许留下**（`photo_identities` 等表不存在）——
//!    否则下次启动会看到「半个 v9」；
//! 3. 用户数据（星标 / 标签 / 说明 / 相册）逐项还在，`foreign_key_check` 干净；
//! 4. 库仍然能读能写（`photos()` 正常、可以再写标记）——「旧库继续可用」；
//! 5. 备份文件真的落在 `Db::backup_dir`。

use picasa_db::{Db, MIGRATIONS};
use std::path::Path;

/// 造一个 **v8** 库：建到 v8 为止，塞进两层目录 + 3 张照片 + 用户标记。
fn make_v8_db(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (
             version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
    )
    .unwrap();
    for m in MIGRATIONS.iter().take(8) {
        conn.execute_batch(m.sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations VALUES (?1, 0)",
            rusqlite::params![m.version],
        )
        .unwrap();
    }
    conn.execute_batch(
        r#"
        INSERT INTO roots (id, path, path_key, state) VALUES (1, '/photos', '/photos', 0);
        INSERT INTO folders
            (id, path, path_key, is_online, root_id, parent_id, name, direct_count) VALUES
            (1, '/photos',      '/photos',      1, 1, NULL, 'photos', 1),
            (2, '/photos/2020', '/photos/2020', 1, 1, 1,    '2020',   2);
        INSERT INTO photos
            (id, folder_id, name, name_key, size_bytes, mtime_ns, width, height,
             orientation, taken_at, taken_at_src, thumb_state, decoder_ver) VALUES
            (1, 1, 'cover.JPG', 'cover.jpg', 10, 1, 4000, 3000, 1, 100, 0, 1, 0),
            (2, 2, 'a.png',     'a.png',     20, 2, 800,  600,  1, 200, 0, 1, 0),
            (3, 2, 'b.TIFF',    'b.tiff',    30, 3, 800,  600,  1, 300, 2, 1, 0);
        UPDATE photos SET starred = 1 WHERE id = 1;
        INSERT INTO captions (photo_id, text) VALUES (1, '封面');
        INSERT INTO tags (photo_id, label) VALUES (1, '旅行'), (1, '精选');
        INSERT INTO albums (id, name, created_at) VALUES (7, '年度最佳', 0);
        INSERT INTO album_photos (album_id, photo_id) VALUES (7, 1);
        "#,
    )
    .unwrap();
}

fn table_exists(path: &Path, table: &str) -> bool {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        rusqlite::params![table],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn scalar_i64(path: &Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn column_count(path: &Path, table: &str, column: &str) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    let mut st = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let names: Vec<String> = st
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    names.iter().filter(|n| n.as_str() == column).count() as i64
}

/// TC-M43-MIG-01「失败回滚」：v9 的 DDL 撞到已存在的列 → 整体回滚 + 备份恢复 +
/// 旧库继续可用，且**不留半个 v9**。
#[test]
fn failed_v9_migration_rolls_back_and_keeps_the_old_library_usable() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("library.sqlite");
    make_v8_db(&db_path);

    // 注入失败：v8 的 index_tasks 本来没有 user_paused，手工加一列让 v9 的
    // `ALTER TABLE ... ADD COLUMN user_paused` 撞 duplicate column name。
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("ALTER TABLE index_tasks ADD COLUMN user_paused INTEGER NOT NULL DEFAULT 0;")
            .unwrap();
        assert_eq!(column_count(&db_path, "index_tasks", "user_paused"), 1);
    }

    let (db, report) = Db::open_with_report(&db_path).unwrap();

    // 1) 报告如实：没升上去、已用备份恢复、现场留给人看。
    assert_eq!(report.from, 8, "升级前是 v8");
    assert_eq!(
        report.to, 8,
        "升级失败后版本必须回到 8（不能谎报成 9）：{report:?}"
    );
    assert!(
        report.error.is_some(),
        "失败现场必须留在 error 里而不是静默吞掉：{report:?}"
    );
    assert!(
        report.restored_from_backup,
        "单事务回滚之外还要用备份覆盖（第二层保险）：{report:?}"
    );

    // 2) 版本停在 8，且**v9 的对象一个都不许留**。
    assert_eq!(
        db.schema_version().unwrap(),
        8,
        "失败后 schema 必须停在 8"
    );
    for t in [
        "photo_identities",
        "photo_identity_tags",
        "photo_identity_albums",
        "import_sources",
        "import_batches",
        "import_batch_members",
    ] {
        assert!(
            !table_exists(&db_path, t),
            "迁移失败不该留下半个 v9：{t} 不应存在"
        );
    }
    assert_eq!(
        column_count(&db_path, "index_tasks", "user_paused"),
        1,
        "回滚后列数应回到「只有手工加的那一列」"
    );
    assert_eq!(
        column_count(&db_path, "roots", "origin"),
        0,
        "v9 给 roots 加的 origin 列不该留下"
    );

    // 3) 用户数据逐项守恒 + 外键干净。
    assert_eq!(db.total_photos().unwrap(), 3, "照片一行都不能少");
    assert_eq!(db.tags_of(1).unwrap().len(), 2, "标签守恒");
    assert_eq!(db.caption_of(1).unwrap(), "封面", "说明守恒");
    assert!(
        db.starred_ids().unwrap().contains(&1),
        "星标守恒（{report:?}）"
    );
    assert_eq!(db.albums_of(1).unwrap().len(), 1, "相册关系守恒");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let mut st = conn.prepare("PRAGMA foreign_key_check").unwrap();
        let bad = st
            .query_map([], |_| Ok(()))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(bad.is_empty(), "回滚后外键必须干净");
    }

    // 4) 旧库仍然可用：能读、能继续写标记（用户不该因为一次失败迁移就被锁死）。
    assert_eq!(db.photos(None, Default::default(), 10).unwrap().len(), 3);
    db.add_tag(&[2], "迁移失败后照常打标").unwrap();
    assert_eq!(db.tags_of(2).unwrap(), vec!["迁移失败后照常打标".to_string()]);

    // 5) 备份真的落在 backup_dir，且备份是升级前（= v8）的库。
    let backup = report
        .backup
        .as_ref()
        .expect("失败前必定先做了备份");
    assert!(backup.is_file(), "备份文件必须存在：{}", backup.display());
    assert_eq!(
        backup.parent().unwrap(),
        Db::backup_dir(&db_path),
        "备份必须落在约定的 Backups 目录"
    );
    assert_eq!(
        scalar_i64(backup, "SELECT MAX(version) FROM schema_migrations"),
        8,
        "备份应是升级前的 v8 库"
    );

    // 6) 再次打开仍然稳定（不因为「上次失败」反复折腾）。
    drop(db);
    let (db2, report2) = Db::open_with_report(&db_path).unwrap();
    assert_eq!(report2.from, 8, "第二次打开仍是 v8");
    assert_eq!(db2.schema_version().unwrap(), 8);
    assert_eq!(db2.total_photos().unwrap(), 3);
}
