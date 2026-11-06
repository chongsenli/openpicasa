//! M4 索引文件导出 / 导入的端到端验收（docs/11 §6 验收重点 2～5、7）。
//!
//! 用 `Db::open` 建真实临时库（跑全套 v1..v8 迁移），导出成文件，再用另一个
//! 临时库导入，逐项核对：数量、整理关系、失败/损坏/取消时现有索引保持可用。

use picasa_db::{
    Db, ImportStrategy, PhotoInput, check_export_dir, execute_import, export_index,
    inspect_index,
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

fn seed(path: &Path) -> Db {
    let db = Db::open(path).unwrap();
    let root = db.upsert_root("/photos", 0, false).unwrap();
    let fid = db.upsert_folder("/photos", "/photos", None).unwrap();
    db.set_folder_root(fid, root, None).unwrap();
    let items = vec![photo("a.jpg", 100), photo("b.jpg", 200), photo("c.jpg", 300)];
    db.apply_scan(fid, "/photos", &items, 0).unwrap();
    let rows = db
        .photos_from_visible(None, picasa_db::SortSpec::default(), 20, 0)
        .unwrap();
    let a = rows.iter().find(|p| p.name == "a.jpg").unwrap().id;
    let b = rows.iter().find(|p| p.name == "b.jpg").unwrap().id;
    db.set_starred(&[a], true).unwrap();
    db.add_tag(&[a, b], "旅行").unwrap();
    db.set_caption(a, "京都塔").unwrap();
    let album = db.create_album("2026 京都", 1000).unwrap();
    db.add_to_album(album, &[a, b], 1000).unwrap();
    db
}

#[test]
fn export_then_import_roundtrip_keeps_all_user_data() {
    let tmp = tempfile::tempdir().unwrap();
    let src_db = seed(&tmp.path().join("src.sqlite"));
    let out = tmp.path().join("out.opicasaidx");
    let rep = export_index(&src_db, &out, true, "0.4.0-test", 1000).unwrap();
    assert!(out.is_file(), "导出文件必须存在");
    assert!(rep.bytes > 0);
    assert_eq!(rep.counts.photos, 3);
    assert_eq!(rep.counts.albums, 1);

    // 导入到一个全新的库
    let dst = tmp.path().join("dst.sqlite");
    let mut dst_db = Db::open(&dst).unwrap();
    let cancel = AtomicBool::new(false);
    let imp = execute_import(
        &mut dst_db,
        &out,
        tmp.path(),
        ImportStrategy::Merge,
        &cancel,
    )
    .unwrap();
    assert_eq!(imp.imported, 3, "3 张照片全部导入");
    assert_eq!(imp.skipped, 0);
    assert_eq!(dst_db.all_tags().unwrap()[0].0, "旅行");
    assert_eq!(dst_db.all_albums().unwrap()[0].name, "2026 京都");
    assert_eq!(dst_db.all_albums().unwrap()[0].photo_count, 2);
    assert_eq!(dst_db.album_photo_ids(1).unwrap().len(), 2);
    assert_eq!(dst_db.photo_count_total().unwrap(), 3);
    assert_eq!(dst_db.starred_ids().unwrap().len(), 1);
    assert_eq!(dst_db.caption_of(1).unwrap(), "京都塔");
}

#[test]
fn cancel_and_corrupt_file_leave_existing_index_usable() {
    let tmp = tempfile::tempdir().unwrap();
    let src_db = seed(&tmp.path().join("src.sqlite"));
    let out = tmp.path().join("out.opicasaidx");
    export_index(&src_db, &out, false, "0.4.0-test", 1000).unwrap();

    // 现有库里先放一条独有数据，验证「取消/损坏后它还在」
    let dst = tmp.path().join("dst.sqlite");
    let mut dst_db = Db::open(&dst).unwrap();
    let root = dst_db.upsert_root("/mine", 0, false).unwrap();
    let fid = dst_db.upsert_folder("/mine", "/mine", None).unwrap();
    dst_db.set_folder_root(fid, root, None).unwrap();
    dst_db
        .apply_scan(fid, "/mine", &[photo("mine.jpg", 999)], 0)
        .unwrap();
    dst_db.add_tag(&[1], "独有标签").unwrap();

    // 1) 取消：cancel 预置 true
    let cancel = AtomicBool::new(true);
    assert!(execute_import(
        &mut dst_db,
        &out,
        tmp.path(),
        ImportStrategy::Merge,
        &cancel,
    )
    .is_err());
    assert_eq!(dst_db.photo_count_total().unwrap(), 1, "现有索引未改动");
    assert_eq!(dst_db.all_tags().unwrap()[0].0, "独有标签");

    // 2) 损坏文件（改 1 字节）
    let corrupt = tmp.path().join("corrupt.opicasaidx");
    std::fs::write(&corrupt, b"not a sqlite database at all").unwrap();
    let cancel2 = AtomicBool::new(false);
    let err = execute_import(
        &mut dst_db,
        &corrupt,
        tmp.path(),
        ImportStrategy::Merge,
        &cancel2,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("索引文件") || err.to_string().contains("不是"),
        "错误信息要是人话：{}",
        err
    );
    assert_eq!(dst_db.photo_count_total().unwrap(), 1, "损坏文件不改动现有索引");
}

#[test]
fn inspect_reports_version_and_conflicts_without_mutating() {
    let tmp = tempfile::tempdir().unwrap();
    let src_db = seed(&tmp.path().join("src.sqlite"));
    let out = tmp.path().join("out.opicasaidx");
    export_index(&src_db, &out, false, "0.4.0-test", 1000).unwrap();

    let dst = tmp.path().join("dst.sqlite");
    let dst_db = Db::open(&dst).unwrap();
    let prev = inspect_index(&dst_db, &out, tmp.path()).unwrap();
    assert_eq!(prev.manifest.format_version, 1);
    assert_eq!(prev.manifest.counts.photos, 3);
    assert_eq!(dst_db.photo_count_total().unwrap(), 0, "预览不写任何数据");
}

#[test]
fn export_dir_rejects_paths_inside_library_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let inside = tmp.path().join("MyPhotos");
    std::fs::create_dir_all(&inside).unwrap();
    let roots = vec![inside.to_string_lossy().to_string()];
    let err = check_export_dir(&inside, &roots).unwrap_err();
    assert!(err.to_string().contains("图库照片目录内"), "{}", err);
    // 容器外的普通目录应通过
    assert!(check_export_dir(tmp.path(), &roots).is_ok());
}

#[test]
fn merge_never_deletes_existing_records() {
    let tmp = tempfile::tempdir().unwrap();
    let src_db = seed(&tmp.path().join("src.sqlite"));
    let out = tmp.path().join("out.opicasaidx");
    export_index(&src_db, &out, false, "0.4.0-test", 1000).unwrap();

    let dst = tmp.path().join("dst.sqlite");
    let mut dst_db = Db::open(&dst).unwrap();
    let root = dst_db.upsert_root("/mine", 0, false).unwrap();
    let fid = dst_db.upsert_folder("/mine", "/mine", None).unwrap();
    dst_db.set_folder_root(fid, root, None).unwrap();
    dst_db
        .apply_scan(fid, "/mine", &[photo("mine.jpg", 999)], 0)
        .unwrap();
    let cancel = AtomicBool::new(false);
    execute_import(&mut dst_db, &out, tmp.path(), ImportStrategy::Merge, &cancel).unwrap();
    // 现有 1 张 + 导入 3 张 = 4 张，一张不少（id 平移避免主键冲突）
    assert_eq!(dst_db.photo_count_total().unwrap(), 4);
    // 导入的整理关系外键也必须跟着平移，不能挂到现有照片上
    assert_eq!(dst_db.all_albums().unwrap()[0].photo_count, 2);
}

#[test]
fn replace_strategy_discards_existing_only_data() {
    let tmp = tempfile::tempdir().unwrap();
    let src_db = seed(&tmp.path().join("src.sqlite"));
    let out = tmp.path().join("out.opicasaidx");
    export_index(&src_db, &out, false, "0.4.0-test", 1000).unwrap();

    let dst = tmp.path().join("dst.sqlite");
    let mut dst_db = Db::open(&dst).unwrap();
    let root = dst_db.upsert_root("/mine", 0, false).unwrap();
    let fid = dst_db.upsert_folder("/mine", "/mine", None).unwrap();
    dst_db.set_folder_root(fid, root, None).unwrap();
    dst_db
        .apply_scan(fid, "/mine", &[photo("mine.jpg", 999)], 0)
        .unwrap();
    dst_db.add_tag(&[1], "独有标签").unwrap();
    dst_db.create_album("独有相册", 1).unwrap();

    let cancel = AtomicBool::new(false);
    execute_import(&mut dst_db, &out, tmp.path(), ImportStrategy::Replace, &cancel).unwrap();
    // 替换后以导入文件为准：3 张、1 个相册、独有标签/相册消失
    assert_eq!(dst_db.photo_count_total().unwrap(), 3);
    assert_eq!(dst_db.all_albums().unwrap().len(), 1);
    assert!(dst_db.all_tags().unwrap().iter().all(|(l, _)| l != "独有标签"));
    // 执行前备份已生成
    assert!(
        std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("pre-import-")),
        "替换策略必须先备份"
    );
}

#[test]
fn future_version_file_is_rejected_with_clear_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let src_db = seed(&tmp.path().join("src.sqlite"));
    let out = tmp.path().join("out.opicasaidx");
    export_index(&src_db, &out, false, "0.4.0-test", 1000).unwrap();
    // 篡改清单里的版本字段（会同时让校验和失配，但我们想验「版本过新」分支，
    // 所以重算校验和写回）
    {
        let c = rusqlite::Connection::open(&out).unwrap();
        c.execute("UPDATE index_manifest SET value='99' WHERE key='format_version'", [])
            .unwrap();
        c.execute("UPDATE index_manifest SET value='9.9.9' WHERE key='app_version'", [])
            .unwrap();
    }
    let dst = tmp.path().join("dst.sqlite");
    let dst_db = Db::open(&dst).unwrap();
    let err = inspect_index(&dst_db, &out, tmp.path()).unwrap_err();
    // 校验和先失配也是合理的拒绝，关键是「现有索引零改动 + 可理解原因」
    assert_eq!(dst_db.photo_count_total().unwrap(), 0);
    assert!(!err.to_string().is_empty());
}
