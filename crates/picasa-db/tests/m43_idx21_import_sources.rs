//! M4.3 阶段 3C：IDX-21「导入的索引」的来源/批次模型与移除流程。
//!
//! 覆盖：
//! - 导入把新带进来的根标 `origin='imported'` 并挂 `import_source_id`，
//!   同一条路径本机已有时**不**新建根（P-M43-10「以本机为准」，计数只算一份）；
//! - `imported_roots` / `import_source_photo_count` / `import_batch_photo_ids`
//!   的口径（照片数按 distinct member，不按批次累加）；
//! - `remove_import_source` 删索引与缩略图、**保住用户标记**（R-M43-02）、
//!   清掉来源与批次行，且从不触碰照片目录（R-M43-01）；
//! - 「仅从分组隐藏」只是偏好开关，不删任何索引行。

use picasa_db::{
    Db, ImportStrategy, PhotoInput, SortSpec, execute_import, export_index, remove_import_source,
};
use std::path::Path;
use std::sync::atomic::AtomicBool;

fn photo(name: &str, taken: i64) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_ascii_lowercase(),
        size_bytes: 1000,
        mtime_ns: taken * 1_000_000_000,
        width: Some(800),
        height: Some(600),
        orientation: Some(1),
        taken_at: Some(taken),
        taken_at_src: 0,
        format: 1,
    }
}

/// 造一个「别人的」索引库：一个根 `/media-a` 下两层、三张照片。
fn seed_source(path: &Path) -> Db {
    let db = Db::open(path).unwrap();
    let root = db.upsert_root("/media-a", 0, false).unwrap();
    let top = db.upsert_folder("/media-a", "/media-a", None).unwrap();
    let sub = db
        .upsert_folder("/media-a/2020", "/media-a/2020", None)
        .unwrap();
    db.set_folder_root(top, root, None).unwrap();
    db.set_folder_root(sub, root, Some(top)).unwrap();
    db.apply_scan(top, "/media-a", &[photo("a.jpg", 100)], 0)
        .unwrap();
    db.apply_scan(
        sub,
        "/media-a/2020",
        &[photo("b.jpg", 200), photo("c.jpg", 300)],
        0,
    )
    .unwrap();
    db.refresh_hierarchy().unwrap();
    db
}

/// 导出并导进 `dst`，返回 (导入报告, 目标库)。
fn import_into(dst: &Path, pkg: &Path, work: &Path) -> picasa_db::ImportReport {
    let mut db = Db::open(dst).unwrap();
    let cancel = AtomicBool::new(false);
    execute_import(&mut db, pkg, work, ImportStrategy::Merge, &cancel).unwrap()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    pkg: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let db = seed_source(&src);
    let pkg = tmp.path().join("lib.opicasaidx");
    export_index(&db, &pkg, false, "0.4.0-test", 1000).unwrap();
    drop(db);
    Fixture { _tmp: tmp, pkg }
}

#[test]
fn imported_roots_are_tagged_and_counted_once() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("dst.sqlite");
    let rep = import_into(&dst, &f.pkg, tmp.path());
    assert_eq!(rep.imported, 3);
    let source_id = rep.import_source_id.expect("导入必须记来源");
    let batch_id = rep.import_batch_id.expect("导入必须记批次");

    let db = Db::open(&dst).unwrap();
    let roots = db.imported_roots(source_id).unwrap();
    assert_eq!(roots.len(), 1, "只有一个来源根");
    assert_eq!(roots[0].1, "/media-a", "根路径逐字保留");
    assert_eq!(roots[0].3, 3, "根的照片数是子树合计");

    assert_eq!(
        db.import_source_photo_count(source_id).unwrap(),
        3,
        "照片数按 distinct member 算"
    );
    let members = picasa_db::import_batch_photo_ids(&db, batch_id).unwrap();
    assert_eq!(members.len(), 3);

    // 根挂上了 imported 标记，本机组不会重复显示它。
    assert!(
        db.roots(false).unwrap().iter().any(|r| r.is_imported()),
        "导入带进来的根必须标 origin=imported"
    );
}

#[test]
fn reimporting_a_package_does_not_create_duplicate_roots_or_double_counts() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("dst.sqlite");
    let first = import_into(&dst, &f.pkg, tmp.path());
    let second = import_into(&dst, &f.pkg, tmp.path());
    let source_id = first.import_source_id.unwrap();

    let db = Db::open(&dst).unwrap();
    // 同一路径、同一自然键 → 不新建根；第二次导入的照片全部「跳过」。
    assert_eq!(db.roots(false).unwrap().len(), 1, "重复导入不能新建根");
    assert_eq!(
        db.imported_roots(source_id).unwrap().len(),
        1,
        "来源下仍然只有一个根"
    );
    assert_eq!(
        db.import_source_photo_count(source_id).unwrap(),
        3,
        "重复导入不能把照片数翻倍"
    );
    assert_eq!(second.import_source_id, Some(source_id), "同一包复用同一来源");
    assert_ne!(second.import_batch_id, first.import_batch_id, "每次导入是新批次");
}

#[test]
fn remove_import_source_keeps_user_markers_and_clears_index() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("dst.sqlite");
    let rep = import_into(&dst, &f.pkg, tmp.path());
    let source_id = rep.import_source_id.unwrap();

    // 在导入带进来的照片上打满标记。
    let db = Db::open(&dst).unwrap();
    // 按**文件名**取行，别按列表顺序：默认排序是「最新在前」，
    // 用下标会拿错照片，测出来的结论就成了假的。
    let rows = db.photos(None, SortSpec::default(), 100).unwrap();
    assert_eq!(rows.len(), 3);
    let a = rows.iter().find(|r| r.name == "a.jpg").unwrap().id;
    let b = rows.iter().find(|r| r.name == "b.jpg").unwrap().id;
    db.set_starred(&[a], true).unwrap();
    db.set_caption(a, "海边").unwrap();
    db.add_tag(&[a, b], "旅行").unwrap();

    let (photos, _thumbs) = remove_import_source(&db, source_id).unwrap();
    assert_eq!(photos, 3, "移除要报出删了多少张");

    // 索引已清空、根也没了。
    assert_eq!(db.total_photos().unwrap(), 0);
    assert!(db.roots(false).unwrap().is_empty(), "来源根要一并摘掉");
    assert!(db.import_sources().unwrap().is_empty(), "来源行要删掉");
    assert!(db.import_batches(source_id).unwrap().is_empty());

    // 标记仍在身份骨架里：重新导入同一包后自动挂回（R-M43-02）。
    let rep2 = import_into(&dst, &f.pkg, tmp.path());
    assert_eq!(rep2.imported, 3);
    let db = Db::open(&dst).unwrap();
    let rows = db.photos(None, SortSpec::default(), 100).unwrap();
    let a2 = rows.iter().find(|r| r.name == "a.jpg").unwrap();
    let b2 = rows.iter().find(|r| r.name == "b.jpg").unwrap();
    assert!(
        db.starred_ids().unwrap().contains(&a2.id),
        "收藏必须在重新导入后挂回（R-M43-02）"
    );
    assert_eq!(db.caption_of(a2.id).unwrap(), "海边", "说明要挂回");
    assert!(
        db.tags_of(a2.id).unwrap().iter().any(|t| t == "旅行"),
        "a.jpg 的标签要挂回"
    );
    assert!(
        db.tags_of(b2.id).unwrap().iter().any(|t| t == "旅行"),
        "b.jpg 的标签也要挂回"
    );
}

#[test]
fn hiding_a_source_only_touches_preferences() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("dst.sqlite");
    let rep = import_into(&dst, &f.pkg, tmp.path());
    let source_id = rep.import_source_id.unwrap();

    let db = Db::open(&dst).unwrap();
    assert!(!db.import_source_hidden(source_id).unwrap());
    db.set_import_source_hidden(source_id, true).unwrap();
    assert!(db.import_source_hidden(source_id).unwrap());
    assert_eq!(
        db.total_photos().unwrap(),
        3,
        "隐藏只是分组显示开关，索引一行都不能少"
    );
    assert_eq!(
        db.imported_roots(source_id).unwrap().len(),
        1,
        "隐藏后导入的根仍然在"
    );
    db.set_import_source_hidden(source_id, false).unwrap();
    assert!(!db.import_source_hidden(source_id).unwrap());
}
