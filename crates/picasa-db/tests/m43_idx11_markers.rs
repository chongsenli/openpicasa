//! M4.3 IDX-11：作用域删除确认框的「标记数」必须准确且**未知时不填 0**。
//!
//! 验收要点（docs/18 §2 M43-IDX-11）：确认框显示照片数 / 标记数；
//! 未知数量显示「待计算」，不能填 0。本文件覆盖 `Db::marker_count_for_root`：
//! - 按「带任一标记的照片」去重计数，不按标记条数累加；
//! - 只数目标 root 子树，不串到别的 root；
//! - root 不存在返回 `None`（供 UI 显示「待计算」），空库返回 `Some(0)`。

use picasa_db::{Db, PhotoInput, SortSpec};

fn input(name: &str) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_lowercase(),
        size_bytes: 10,
        mtime_ns: 1,
        width: Some(100),
        height: Some(80),
        orientation: Some(1),
        taken_at: Some(1_300_000_000),
        taken_at_src: 0,
        format: 0,
    }
}

fn photo_ids(db: &Db) -> Vec<i64> {
    db.photos(None, SortSpec::default(), 100)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect()
}

#[test]
fn unknown_root_is_none_not_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    assert_eq!(
        db.marker_count_for_root(999).unwrap(),
        None,
        "root 不存在必须返回 None，UI 才显示「待计算」而不是 0"
    );
}

#[test]
fn empty_root_is_some_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let rid = db.upsert_root("/x/photos", 1, false).unwrap();
    // root 行存在但没有 folders/photos
    assert_eq!(db.marker_count_for_root(rid).unwrap(), Some(0));
}

#[test]
fn counts_distinct_marked_photos_not_marker_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let rid = db.upsert_root("/x/photos", 1, false).unwrap();
    let fid = db.upsert_folder("/x/photos", "/x/photos", None).unwrap();
    db.apply_scan(
        fid,
        "/x/photos",
        &[input("a.jpg"), input("b.jpg"), input("c.jpg")],
        0,
    )
    .unwrap();
    let ids = photo_ids(&db);
    let (a, b, _c) = (ids[0], ids[1], ids[2]);

    // 无标记时 0
    assert_eq!(db.marker_count_for_root(rid).unwrap(), Some(0));

    // a 打满四类标记：星标 + 说明 + 两个标签 + 相册 → 仍只算 1 张
    db.set_starred(&[a], true).unwrap();
    db.set_caption(a, "海边").unwrap();
    db.add_tag(&[a], "旅行").unwrap();
    db.add_tag(&[a], "精选").unwrap();
    let album = db.create_album("年度最佳", 1).unwrap();
    db.add_to_album(album, &[a], 1).unwrap();
    assert_eq!(
        db.marker_count_for_root(rid).unwrap(),
        Some(1),
        "同一张照片的多个标记不能重复计数"
    );

    // b 只有标签 → 2 张带标记；c 仍无标记
    db.add_tag(&[b], "旅行").unwrap();
    assert_eq!(db.marker_count_for_root(rid).unwrap(), Some(2));
}

#[test]
fn markers_from_other_roots_do_not_leak() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let ra = db.upsert_root("/x/A", 1, false).unwrap();
    let rb = db.upsert_root("/x/B", 1, false).unwrap();
    let fa = db.upsert_folder("/x/A", "/x/A", None).unwrap();
    let fb = db.upsert_folder("/x/B", "/x/B", None).unwrap();
    db.apply_scan(fa, "/x/A", &[input("a.jpg")], 0).unwrap();
    db.apply_scan(fb, "/x/B", &[input("b.jpg")], 0).unwrap();
    let ids = photo_ids(&db);
    // 只给 B 的照片打星标
    let b_id = *ids.iter().max().unwrap();
    db.set_starred(&[b_id], true).unwrap();

    assert_eq!(db.marker_count_for_root(rb).unwrap(), Some(1));
    assert_eq!(
        db.marker_count_for_root(ra).unwrap(),
        Some(0),
        "A 根不能把 B 根的标记算进来（前缀边界 + 去重）"
    );
}
