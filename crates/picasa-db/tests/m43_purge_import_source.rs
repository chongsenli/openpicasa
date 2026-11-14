//! m10756：「删除后，导入的索引还在文件夹树上」——彻底删除根必须把空来源一起收掉。
//!
//! 左栏「导入的索引」分组是照 `import_sources` 表画的，**不按根画**。
//! 所以只删 `roots` 行时，来源行还在，分组里就会留一个点开什么都没有的空节点——
//! 用户看到的就是「删了，但导入的索引还在文件夹树上」。
//!
//! 这里钉住三件事：
//! 1. 删掉来源下**最后一个**根 → 来源、批次、批次成员、「仅从分组隐藏」的偏好键一起消失；
//! 2. 来源下**还有别的根** → 来源必须原样保留（只删用户点的那个）；
//! 3. 收尾不能误伤**别的**来源。

use picasa_db::{
    Db, ImportStrategy, PhotoInput, SortSpec, execute_import, export_index,
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

/// 造一个「别人的」索引库：`roots` 里每项一个根，根下各一张照片。
fn seed_source(path: &Path, roots: &[&str]) -> Db {
    let db = Db::open(path).unwrap();
    for (i, root) in roots.iter().enumerate() {
        let rid = db.upsert_root(root, 0, false).unwrap();
        let fid = db.upsert_folder(root, root, None).unwrap();
        db.set_folder_root(fid, rid, None).unwrap();
        db.apply_scan(fid, root, &[photo(&format!("p{i}.jpg"), 100 + i as i64)], 0)
            .unwrap();
    }
    db.refresh_hierarchy().unwrap();
    db
}

/// 导出源库成包，再导进 `dst`，返回导入报告。
fn import_into(dst: &Path, pkg: &Path, work: &Path) -> picasa_db::ImportReport {
    let mut db = Db::open(dst).unwrap();
    let cancel = AtomicBool::new(false);
    execute_import(&mut db, pkg, work, ImportStrategy::Merge, &cancel).unwrap()
}

/// 造包：`roots` 指定源库里的根路径。
fn package(tmp: &Path, name: &str, roots: &[&str]) -> std::path::PathBuf {
    let src = tmp.join(format!("{name}-src.sqlite"));
    let db = seed_source(&src, roots);
    let pkg = tmp.join(format!("{name}.opicasaidx"));
    export_index(&db, &pkg, false, "0.4.0-test", 1000).unwrap();
    drop(db);
    pkg
}

#[test]
fn purging_the_last_imported_root_removes_the_source_node() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = package(tmp.path(), "one", &["/media-a"]);

    let dst = tmp.path().join("dst.sqlite");
    let rep = import_into(&dst, &pkg, tmp.path());
    let source_id = rep.import_source_id.expect("导入必须记来源");
    assert_eq!(rep.imported, 1);

    let db = Db::open(&dst).unwrap();
    let root_ids = db.imported_root_ids(source_id).unwrap();
    assert_eq!(root_ids.len(), 1);
    let batch_id = rep.import_batch_id.expect("导入必须记批次");
    assert_eq!(picasa_db::import_batch_photo_ids(&db, batch_id).unwrap().len(), 1);
    // 用户把来源藏起来过：来源都没了，这个偏好键也该跟着走。
    db.set_import_source_hidden(source_id, true).unwrap();
    assert!(db.import_source_hidden(source_id).unwrap());

    // 用户点「彻底删除索引…」。
    let (photos, _) = db.purge_index_for_root(root_ids[0]).unwrap();
    assert_eq!(photos, 1, "删了 1 张");

    assert_eq!(db.total_photos().unwrap(), 0);
    assert!(db.roots(false).unwrap().is_empty(), "根行要删掉");
    assert!(
        db.import_sources().unwrap().is_empty(),
        "来源下最后一个根被删后，来源行必须一起收掉——否则左栏留一个空节点"
    );
    assert!(db.import_batches(source_id).unwrap().is_empty(), "批次行要清");
    assert!(
        picasa_db::import_batch_photo_ids(&db, batch_id).unwrap().is_empty(),
        "批次成员要清"
    );
    assert!(
        !db.import_source_hidden(source_id).unwrap(),
        "来源没了，「仅从分组隐藏」的偏好键不该留着变成死配置"
    );
}

#[test]
fn purging_one_of_several_imported_roots_keeps_the_source() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = package(tmp.path(), "two", &["/media-a", "/media-b"]);

    let dst = tmp.path().join("dst.sqlite");
    let rep = import_into(&dst, &pkg, tmp.path());
    let source_id = rep.import_source_id.expect("导入必须记来源");
    assert_eq!(rep.imported, 2);

    let db = Db::open(&dst).unwrap();
    let mut root_ids = db.imported_root_ids(source_id).unwrap();
    root_ids.sort();
    assert_eq!(root_ids.len(), 2);

    db.purge_index_for_root(root_ids[0]).unwrap();

    assert_eq!(
        db.import_sources().unwrap().len(),
        1,
        "来源下还有根时，来源节点必须保留（用户只删了其中一组）"
    );
    let left = db.imported_roots(source_id).unwrap();
    assert_eq!(left.len(), 1, "只剩一个根");
    assert_eq!(db.total_photos().unwrap(), 1, "另一个根的照片不受影响");
    // 剩下的那张照片还能按正常排序取出来（不是「行还在但查不到」）。
    let rows = db.photos(None, SortSpec::default(), 10).unwrap();
    assert_eq!(rows.len(), 1);
}

#[test]
fn purging_one_source_does_not_touch_another() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg_a = package(tmp.path(), "a", &["/media-a"]);
    let pkg_b = package(tmp.path(), "b", &["/media-b"]);

    let dst = tmp.path().join("dst.sqlite");
    let rep_a = import_into(&dst, &pkg_a, tmp.path());
    let rep_b = import_into(&dst, &pkg_b, tmp.path());
    let (sa, sb) = (
        rep_a.import_source_id.unwrap(),
        rep_b.import_source_id.unwrap(),
    );
    assert_ne!(sa, sb, "两个不同的包必须是两个来源");

    let db = Db::open(&dst).unwrap();
    let roots_a = db.imported_root_ids(sa).unwrap();
    db.purge_index_for_root(roots_a[0]).unwrap();

    let sources = db.import_sources().unwrap();
    assert_eq!(sources.len(), 1, "另一个来源必须原样保留");
    assert_eq!(sources[0].id, sb);
    assert_eq!(db.imported_roots(sb).unwrap().len(), 1);
    assert_eq!(db.total_photos().unwrap(), 1, "另一个来源的照片一张不少");
    assert!(
        db.import_source_photo_count(sb).unwrap() >= 1,
        "另一个来源的计数不能因为别人的删除被算没"
    );
}

#[test]
fn pruning_orphans_is_a_no_op_when_every_source_still_has_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = package(tmp.path(), "keep", &["/media-a"]);
    let dst = tmp.path().join("dst.sqlite");
    let rep = import_into(&dst, &pkg, tmp.path());
    let source_id = rep.import_source_id.unwrap();

    let db = Db::open(&dst).unwrap();
    let removed = picasa_db::prune_orphan_import_sources(&db).unwrap();
    assert!(removed.is_empty(), "没有孤儿来源时不该删任何东西");
    assert_eq!(db.import_sources().unwrap().len(), 1);
    assert_eq!(db.import_source_photo_count(source_id).unwrap(), 1);
}
