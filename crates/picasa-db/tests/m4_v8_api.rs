//! v8 迁移 + M4 数据层 API 的验收用例（docs/11 §6 验收重点 7）。

use picasa_db::{
    Db, ImportStrategy, MIGRATIONS, PhotoExif, PhotoFilter, PhotoInput, SCHEMA_VERSION,
};
use std::path::Path;

fn photo(name: &str, size: i64) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_ascii_lowercase(),
        size_bytes: size,
        mtime_ns: 1_000_000_000,
        width: Some(800),
        height: Some(600),
        orientation: Some(1),
        taken_at: Some(1000),
        taken_at_src: 0,
        format: 1,
    }
}

fn open(path: &Path) -> Db {
    let db = Db::open(path).unwrap();
    let root = db.upsert_root("/photos", 0, false).unwrap();
    let fid = db.upsert_folder("/photos", "/photos", None).unwrap();
    db.set_folder_root(fid, root, None).unwrap();
    db.apply_scan(
        fid,
        "/photos",
        &[
            photo("a.jpg", 1000),
            photo("b.jpg", 500_000),
            photo("c.jpg", 2_000_000),
        ],
        0,
    )
    .unwrap();
    db
}

#[test]
fn v8_migration_is_last_and_adds_expected_tables() {
    // M4.3 阶段 0：v9 身份骨架/检查点迁移接在 v8 之后；v8 的 EXIF/相册列与表仍须存在。
    assert_eq!(SCHEMA_VERSION, 9);
    assert_eq!(MIGRATIONS.last().unwrap().version, 9);
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("lib.sqlite")).unwrap();
    assert_eq!(db.schema_version().unwrap(), 9);
    let cols = db.column_names("photos").unwrap();
    for c in [
        "camera_make", "camera_model", "lens_model", "focal_length", "f_number",
        "exposure_time", "iso", "exif_status", "exif_error",
    ] {
        assert!(cols.iter().any(|x| x == c), "缺少列 {c}");
    }
    for t in ["albums", "album_photos", "recent_searches", "index_manifest"] {
        assert!(db.table_exists(t).unwrap(), "缺少表 {t}");
    }
}

#[test]
fn album_crud_and_many_to_many() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open(&tmp.path().join("lib.sqlite"));
    let a = db.create_album("京都", 100).unwrap();
    let b = db.create_album("家人", 200).unwrap();
    assert!(db.create_album("京都", 300).is_err(), "重名必须拒绝");
    let rows = db
        .photos_from_visible(None, picasa_db::SortSpec::default(), 20, 0)
        .unwrap();
    let id1 = rows[0].id;
    let id2 = rows[1].id;
    assert_eq!(db.add_to_album(a, &[id1, id2], 100).unwrap(), 2);
    assert_eq!(db.add_to_album(b, &[id1], 100).unwrap(), 1);
    assert_eq!(db.add_to_album(a, &[id1], 100).unwrap(), 0, "重复加入去重");
    assert_eq!(db.albums_of(id1).unwrap().len(), 2, "一张照片可属多个相册");
    assert_eq!(
        db.all_albums()
            .unwrap()
            .iter()
            .find(|x| x.name == "京都")
            .unwrap()
            .photo_count,
        2,
        "京都相册有 2 张"
    );
    db.rename_album(a, "旅途").unwrap();
    assert!(db.all_albums().unwrap().iter().any(|x| x.name == "旅途"));
    db.remove_from_album(b, &[id1]).unwrap();
    assert_eq!(db.albums_of(id1).unwrap().len(), 1, "移出仅取消归属");
    let before = db.photo_count_total().unwrap();
    db.delete_album(a).unwrap();
    assert_eq!(db.photo_count_total().unwrap(), before, "删相册不删照片");
}

#[test]
fn tag_rename_and_delete_touch_only_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open(&tmp.path().join("lib.sqlite"));
    let rows = db
        .photos_from_visible(None, picasa_db::SortSpec::default(), 20, 0)
        .unwrap();
    let id = rows[0].id;
    db.add_tag(&[id], "旅行").unwrap();
    db.rename_tag("旅行", "旅途").unwrap();
    assert_eq!(db.tags_of(id).unwrap(), vec!["旅途"]);
    db.delete_tag("旅途").unwrap();
    assert!(db.tags_of(id).unwrap().is_empty());
    assert_eq!(db.photo_count_total().unwrap(), 3, "标签操作不动照片");
}

#[test]
fn exif_set_and_camera_options() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open(&tmp.path().join("lib.sqlite"));
    let rows = db
        .photos_from_visible(None, picasa_db::SortSpec::default(), 20, 0)
        .unwrap();
    let id = rows[0].id;
    db.set_photo_exif(
        id,
        &PhotoExif {
            camera_make: Some("Canon".into()),
            camera_model: Some("EOS 5D".into()),
            lens_model: Some("EF 24-70".into()),
            status: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let e = db.photo_exif(id).unwrap();
    assert_eq!(e.camera_make.as_deref(), Some("Canon"));
    assert_eq!(e.status, 1);
    assert_eq!(db.camera_options().unwrap().len(), 1);
    assert_eq!(db.lens_options().unwrap().len(), 1);
    assert_eq!(db.photos_pending_exif(10).unwrap().len(), 2, "其余仍待回填");
}

#[test]
fn recent_searches_keep_last_10_and_dedup() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open(&tmp.path().join("lib.sqlite"));
    for i in 0..15 {
        db.push_recent_search(&format!("{{\"q\":{i}}}"), i).unwrap();
    }
    assert_eq!(db.recent_searches().unwrap().len(), 10);
    db.push_recent_search("{\"q\":3}", 99).unwrap();
    let rs = db.recent_searches().unwrap();
    assert_eq!(rs[0].query_json, "{\"q\":3}", "重复项去重后回到最新");
    assert_eq!(rs.len(), 10);
    db.clear_recent_searches().unwrap();
    assert!(db.recent_searches().unwrap().is_empty());
}

#[test]
fn new_filter_dimensions_are_active_and_narrow_results() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open(&tmp.path().join("lib.sqlite"));

    // 文件大小区间：只留中间那张（500_000B）
    let f = PhotoFilter {
        size_bytes: Some((100_000, 1_000_000)),
        ..Default::default()
    };
    assert!(f.is_active());
    assert_eq!(db.count_photos_filtered(None, &f).unwrap(), 1);

    // 缺少元数据：v8 默认 exif_status=0，三张全中
    let f2 = PhotoFilter {
        missing_exif: true,
        ..Default::default()
    };
    assert_eq!(db.count_photos_filtered(None, &f2).unwrap(), 3);

    // 相册维度：建相册并加入一张后应只命中一张
    let rows = db
        .photos_from_visible(None, picasa_db::SortSpec::default(), 20, 0)
        .unwrap();
    let a = db.create_album("精选", 1).unwrap();
    db.add_to_album(a, &[rows[0].id], 1).unwrap();
    let f3 = PhotoFilter {
        album: Some("精选".into()),
        ..Default::default()
    };
    assert_eq!(db.count_photos_filtered(None, &f3).unwrap(), 1);
    assert!(f3.is_active());

    // 全默认不算筛选
    assert!(!PhotoFilter::default().is_active());
    let _ = ImportStrategy::Merge;
}
