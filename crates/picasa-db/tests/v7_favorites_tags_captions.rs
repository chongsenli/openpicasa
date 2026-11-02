//! v7 迁移 + 收藏/标签/说明 API 的验收用例。
//!
//! 数据边界（产品定义）：这些数据**只存应用数据库**，不写照片文件 / IPTC / XMP。
//! 用例同时守住：批量幂等、空标签忽略、说明空串删行、照片删除级联清理。

use picasa_db::{Db, SCHEMA_VERSION};

fn open_db(dir: &std::path::Path) -> (Db, std::path::PathBuf) {
    let path = dir.join("library.sqlite");
    let db = Db::open(&path).expect("建库");
    (db, path)
}

/// 直接造一张照片行（不跑扫描器——这些用例只测元数据 API）。
/// 用独立的 rusqlite 连接插数据：`Db.conn` 是私有的，集成测试在外部；
/// WAL 模式下两个连接一读一写不会互锁。
fn seed_photo(path: &std::path::Path, name: &str) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    // 先补最小外键链（roots / folders），幂等：已存在则跳过
    conn.execute_batch(
        r#"
        INSERT OR IGNORE INTO roots (id, path, path_key, state) VALUES (1, '/x', '/x', 0);
        INSERT OR IGNORE INTO folders (id, path, path_key, is_online, root_id, parent_id, name, direct_count)
            VALUES (1, '/x', '/x', 1, 1, NULL, 'x', 1);
        "#,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO photos(folder_id, name, name_key, size_bytes, mtime_ns, format)
         VALUES (1, ?1, ?1, 100, 0, 1)",
        [name],
    )
    .unwrap();
    conn.query_row("SELECT last_insert_rowid()", [], |r| r.get(0)).unwrap()
}

#[test]
fn v7_creates_metadata_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_db(tmp.path());
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 7);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let have: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name IN ('captions','tags')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(have.contains(&"captions".into()));
    assert!(have.contains(&"tags".into()));
}

#[test]
fn starred_roundtrip_and_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_db(tmp.path());
    let a = seed_photo(&db_path, "a.jpg");
    let b = seed_photo(&db_path, "b.jpg");
    let c = seed_photo(&db_path, "c.jpg");

    db.set_starred(&[a, b], true).unwrap();
    let mut ids = db.starred_ids().unwrap();
    ids.sort();
    assert_eq!(ids, vec![a, b]);

    // 取消一张
    db.set_starred(&[b], false).unwrap();
    assert_eq!(db.starred_ids().unwrap(), vec![a]);

    // 批量取消不存在的 id 不报错（幂等）
    db.set_starred(&[999_999], false).unwrap();
    // 再全量批量设置（多选场景）
    db.set_starred(&[a, b, c], true).unwrap();
    assert_eq!(db.starred_ids().unwrap().len(), 3);
}

#[test]
fn tags_are_idempotent_and_queryable() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_db(tmp.path());
    let a = seed_photo(&db_path, "a.jpg");
    let b = seed_photo(&db_path, "b.jpg");

    db.add_tag(&[a, b], "旅行").unwrap();
    // 重复打同一标签：幂等
    db.add_tag(&[a], "旅行").unwrap();
    db.add_tag(&[a], "风景").unwrap();

    // ORDER BY label 按 Unicode 码点（无 Collate），「旅」(U+65C5) > 「风」(U+98CE)
    assert_eq!(db.tags_of(a).unwrap(), vec!["旅行".to_string(), "风景".to_string()]);
    assert_eq!(db.tags_of(b).unwrap(), vec!["旅行".to_string()]);

    let all = db.all_tags().unwrap();
    assert_eq!(all[0], ("旅行".to_string(), 2)); // 按次数降序
    assert_eq!(all[1], ("风景".to_string(), 1));

    assert_eq!(db.ids_with_tag("旅行").unwrap(), vec![a, b]);

    // 摘标签
    db.remove_tag(&[a], "旅行").unwrap();
    assert_eq!(db.ids_with_tag("旅行").unwrap(), vec![b]);
    assert_eq!(db.tags_of(a).unwrap(), vec!["风景".to_string()]);

    // 空标签是空操作（不报错、不插入）
    db.add_tag(&[a], "   ").unwrap();
    assert_eq!(db.tags_of(a).unwrap(), vec!["风景".to_string()]);
}

#[test]
fn caption_upsert_and_empty_deletes() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_db(tmp.path());
    let a = seed_photo(&db_path, "a.jpg");

    assert_eq!(db.caption_of(a).unwrap(), "");
    db.set_caption(a, "颐和园的落日").unwrap();
    assert_eq!(db.caption_of(a).unwrap(), "颐和园的落日");
    // 覆盖写
    db.set_caption(a, "改为：颐和园日落").unwrap();
    assert_eq!(db.caption_of(a).unwrap(), "改为：颐和园日落");
    // 空串删除行（不留空壳）
    db.set_caption(a, "  ").unwrap();
    assert_eq!(db.caption_of(a).unwrap(), "");
}

#[test]
fn deleting_photo_cascades_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_db(tmp.path());
    let a = seed_photo(&db_path, "a.jpg");
    db.add_tag(&[a], "t1").unwrap();
    db.set_caption(a, "note").unwrap();
    db.set_starred(&[a], true).unwrap();

    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute("DELETE FROM photos WHERE id=?1", [a])
        .unwrap();

    assert_eq!(db.tags_of(a).unwrap(), Vec::<String>::new());
    assert_eq!(db.caption_of(a).unwrap(), "");
    assert!(db.starred_ids().unwrap().is_empty());
}
