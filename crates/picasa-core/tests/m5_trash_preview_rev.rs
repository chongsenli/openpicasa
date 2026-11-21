//! M5-V1-r8 · 确认框路径预览缓存的**失效键**在 core 侧的可观测契约。
//!
//! UI 每帧复用预览的前提是 `Library::photos_rev` 真的是一个 O(1) 版本号：
//! - 换列表 / 补页 / 清空 → +1（路径可能变了，缓存必须失效）；
//! - 只改选择集 → **不变**（选中集合由 `TrashScope` 自己逐项比较，不需要
//!   core 为它做任何每帧哈希，更不能拿「图库没变」去掩盖「集合变了」）。
//!
//! 这里只读内存状态，不移动任何文件。

use picasa_core::{AppPaths, Library, paths};
use picasa_db::{PhotoInput, SortSpec};
use std::path::Path;

fn isolated_paths(root: &Path) -> AppPaths {
    AppPaths {
        support: root.join("Library/Application Support/app.openpicasa"),
        preferences: root.join("Library/Preferences/app.openpicasa"),
        caches: root.join("Library/Caches/app.openpicasa"),
        logs: root.join("Library/Logs/app.openpicasa"),
    }
}

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

fn fixture(tmp: &Path) -> Library {
    let root = tmp.join("Pictures");
    std::fs::create_dir_all(&root).unwrap();
    let names = ["a.jpg", "b.jpg", "c.jpg"];
    for n in names {
        std::fs::write(root.join(n), b"x").unwrap();
    }
    let mut lib = Library::open(isolated_paths(tmp)).unwrap();
    lib.add_root(&root);
    let scope = paths::canonical_string(&root);
    {
        let db = lib.db().unwrap();
        let fid = db.upsert_folder(&scope, &scope, None).unwrap();
        let rows: Vec<PhotoInput> = names.iter().map(|n| input(n)).collect();
        db.apply_scan(fid, &scope, &rows, 0).unwrap();
    }
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();
    lib
}

#[test]
fn photos_rev_bumps_on_list_reload_but_not_on_selection_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = fixture(tmp.path());
    let before = lib.photos_rev;

    // 选择集变化不触碰图库数据版本：集合变化由 `TrashScope` 逐项比较兜住。
    lib.select_all();
    assert_eq!(lib.photos_rev, before, "只改选择集不应让图库版本变化");
    lib.select_none();
    assert_eq!(lib.photos_rev, before);

    // 重载换了一份列表 → +1。
    lib.reload_photos().unwrap();
    assert!(
        lib.photos_rev > before,
        "换列表必须让预览缓存失效：{} -> {}",
        before,
        lib.photos_rev
    );

    // 一份新列表上的空补页不该制造无意义的失效。
    let after_reload = lib.photos_rev;
    assert!(!lib.extend_photos().unwrap(), "3 张已到底");
    assert_eq!(lib.photos_rev, after_reload, "没有新行就不该 bump");
}

#[test]
fn reload_changes_the_rows_visible_to_the_preview() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Pictures");
    let mut lib = fixture(tmp.path());
    assert_eq!(lib.photos.len(), 3);

    // 再落一张进库并重载：预览缓存靠 photos_rev 感知到「第 4 行出现了」。
    // `apply_scan` 是「该目录的完整快照」，所以要连同原有 3 张一起传。
    std::fs::write(root.join("d.jpg"), b"x").unwrap();
    let scope = paths::canonical_string(&root);
    {
        let db = lib.db().unwrap();
        let fid = db.upsert_folder(&scope, &scope, None).unwrap();
        let rows: Vec<PhotoInput> = ["a.jpg", "b.jpg", "c.jpg", "d.jpg"]
            .iter()
            .map(|n| input(n))
            .collect();
        db.apply_scan(fid, &scope, &rows, 0).unwrap();
    }
    let before = lib.photos_rev;
    lib.reload_photos().unwrap();
    assert_eq!(lib.photos.len(), 4);
    assert!(lib.photos_rev > before, "新行出现必须让预览缓存失效");
}

#[test]
fn trash_ids_still_takes_the_whole_passed_set() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = fixture(tmp.path());
    let ids: Vec<i64> = lib
        .db()
        .unwrap()
        .photos(None, SortSpec::default(), 100)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids.len(), 3, "夹具 3 张");
    lib.selection = ids.iter().copied().collect();
    assert_eq!(lib.selection_ids(), ids, "执行集合来自完整选择集");
    // 这里不断言执行结果（会移动原文件）：只钉住「传入什么就是什么」的入口口径。
    assert_eq!(lib.selection_ids().len(), 3);
}
