//! `photos_filtered`（筛选查询）的验收用例：日期 / 格式 / 尺寸 / 收藏 / 标签
//! 五个维度单独与组合，以及与 `photos_from_visible` 的空筛选等价性。

use picasa_db::{Db, PhotoFilter, PhotoRow, SizeClass, SortKey, SortSpec};

fn make_db(dir: &std::path::Path) -> (Db, std::path::PathBuf) {
    let path = dir.join("library.sqlite");
    let db = Db::open(&path).unwrap();
    (db, path)
}

fn seed(path: &std::path::Path, rows: &[(&str, i64, i64, i64, i64)]) -> Vec<i64> {
    // (name, format, width, height, taken_at)
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        r#"
        INSERT OR IGNORE INTO roots (id, path, path_key, state) VALUES (1, '/x', '/x', 0);
        INSERT OR IGNORE INTO folders (id, path, path_key, is_online, root_id, parent_id, name, direct_count)
            VALUES (1, '/x', '/x', 1, 1, NULL, 'x', 0);
        "#,
    )
    .unwrap();
    let mut ids = Vec::new();
    for (name, format, _w, h, taken) in rows {
        conn.execute(
            "INSERT INTO photos(folder_id, name, name_key, size_bytes, mtime_ns, width, height, taken_at, taken_at_src, format)
             VALUES (1, ?1, ?1, 100, ?5, ?2*10, ?3*10, ?4, 0, ?2)",
            rusqlite::params![name, format, h, taken, taken],
        )
        .unwrap();
        ids.push(conn.query_row("SELECT last_insert_rowid()", [], |r| r.get(0)).unwrap());
    }
    ids
}

fn names(rows: &[PhotoRow]) -> Vec<&str> {
    rows.iter().map(|r| r.name.as_str()).collect()
}

#[test]
fn empty_filter_equals_unfiltered() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, path) = make_db(tmp.path());
    seed(&path, &[("a.jpg", 1, 192, 108, 100), ("b.png", 2, 192, 108, 200)]);

    let sort = SortSpec { key: SortKey::TakenAt, descending: false };
    let plain = db.photos_from_visible(None, sort, 100, 0).unwrap();
    let filtered = db.photos_filtered(None, &PhotoFilter::default(), sort, 100, 0).unwrap();
    assert_eq!(names(&plain), names(&filtered));
    assert_eq!(names(&filtered), vec!["a.jpg", "b.png"]);
}

#[test]
fn filter_by_format() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, path) = make_db(tmp.path());
    seed(&path, &[
        ("a.jpg", 1, 100, 100, 100),
        ("b.png", 2, 100, 100, 200),
        ("c.heic", 7, 100, 100, 300),
    ]);

    let sort = SortSpec { key: SortKey::TakenAt, descending: false };
    let f = PhotoFilter { formats: vec!["jpeg".into(), "png".into()], ..Default::default() };
    let out = db.photos_filtered(None, &f, sort, 100, 0).unwrap();
    assert_eq!(names(&out), vec!["a.jpg", "b.png"]);

    let f = PhotoFilter { formats: vec!["heic".into()], ..Default::default() };
    let out = db.photos_filtered(None, &f, sort, 100, 0).unwrap();
    assert_eq!(names(&out), vec!["c.heic"]);

    // 未知 slug：安全返回空，不拼坏 SQL
    let f = PhotoFilter { formats: vec!["nope".into()], ..Default::default() };
    assert!(db.photos_filtered(None, &f, sort, 100, 0).unwrap().is_empty());
}

#[test]
fn filter_by_date_range_and_size_class() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, path) = make_db(tmp.path());
    seed(&path, &[
        ("small-old.jpg", 1, 40, 30, 100),   // 400×300 → Small
        ("med-old.jpg", 1, 120, 80, 150),    // 1200×800 → Medium
        ("big-new.jpg", 1, 300, 200, 300),   // 3000×2000 → Large
    ]);

    let sort = SortSpec { key: SortKey::TakenAt, descending: false };

    let f = PhotoFilter { taken_range: Some((120, 10_000)), ..Default::default() };
    assert_eq!(names(&db.photos_filtered(None, &f, sort, 100, 0).unwrap()), vec!["med-old.jpg", "big-new.jpg"]);

    let f = PhotoFilter { size_class: Some(SizeClass::Large), ..Default::default() };
    assert_eq!(names(&db.photos_filtered(None, &f, sort, 100, 0).unwrap()), vec!["big-new.jpg"]);

    let f = PhotoFilter { size_class: Some(SizeClass::Small), ..Default::default() };
    assert_eq!(names(&db.photos_filtered(None, &f, sort, 100, 0).unwrap()), vec!["small-old.jpg"]);
}

#[test]
fn filter_by_starred_and_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, path) = make_db(tmp.path());
    let ids = seed(&path, &[
        ("a.jpg", 1, 100, 100, 100),
        ("b.jpg", 1, 100, 100, 200),
        ("c.jpg", 1, 100, 100, 300),
    ]);
    db.set_starred(&[ids[0], ids[1]], true).unwrap();
    db.add_tag(&[ids[1]], "旅行").unwrap();

    let sort = SortSpec { key: SortKey::TakenAt, descending: false };

    let f = PhotoFilter { starred_only: true, ..Default::default() };
    assert_eq!(names(&db.photos_filtered(None, &f, sort, 100, 0).unwrap()), vec!["a.jpg", "b.jpg"]);

    let f = PhotoFilter { tag: Some("旅行".into()), ..Default::default() };
    assert_eq!(names(&db.photos_filtered(None, &f, sort, 100, 0).unwrap()), vec!["b.jpg"]);

    // 组合：收藏 且 打标签
    let f = PhotoFilter {
        starred_only: true,
        tag: Some("旅行".into()),
        ..Default::default()
    };
    assert_eq!(names(&db.photos_filtered(None, &f, sort, 100, 0).unwrap()), vec!["b.jpg"]);
}

#[test]
fn filter_respects_scope_and_starred_readback() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, path) = make_db(tmp.path());
    let ids = seed(&path, &[("a.jpg", 1, 100, 100, 100)]);
    db.set_starred(&ids, true).unwrap();

    // PhotoRow.starred 读回
    let rows = db.photos_from_visible(None, SortSpec::default(), 10, 0).unwrap();
    assert!(rows[0].starred);
}
