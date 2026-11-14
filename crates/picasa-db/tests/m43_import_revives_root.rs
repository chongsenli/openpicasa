//! 导入必须让照片**看得见**：复用一个被关闭／已移除索引的既有根时，把根恢复回来。
//!
//! 真实事故：用户导出一台机器上的索引（268565 张），其中一个源根是
//! `/Volumes/Expansion`；接收机上同路径的根处于「已关闭 + 索引已移除」态
//! （外置盘没插）。`apply_folder_tree` 按 P-M43-10「同路径以本机为准」复用了
//! 那个根，于是 26 万多张照片全挂进一个不可见范围——左栏不列、网格为空，
//! 用户看到的效果与「导入失败」一模一样。
//!
//! 这里钉住两条边界：
//! - 本次导入确实写进照片的根 → `state` 归 0、`m43_index_removed_*` 标记清掉；
//! - 与导入无关的已关闭根 → 保持原样（用户主动收起的盘不能因为一次导入被拉出来）。

use picasa_db::{Db, ImportStrategy, PhotoInput, execute_import, export_index};

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

/// 造一个「别人的」索引库：根 `/media-a` 一张照片。
fn seed_source(path: &std::path::Path) -> Db {
    let db = Db::open(path).unwrap();
    let root = db.upsert_root("/media-a", 0, false).unwrap();
    let top = db.upsert_folder("/media-a", "/media-a", None).unwrap();
    db.set_folder_root(top, root, None).unwrap();
    db.apply_scan(top, "/media-a", &[photo("a.jpg", 100)], 0)
        .unwrap();
    db.refresh_hierarchy().unwrap();
    db
}

fn package() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.sqlite");
    let db = seed_source(&src);
    let pkg = tmp.path().join("lib.opicasaidx");
    export_index(&db, &pkg, false, "0.4.0-test", 1000).unwrap();
    (tmp, pkg)
}

#[test]
fn importing_into_a_closed_and_removed_root_makes_its_photos_visible() {
    let (_src_tmp, pkg) = package();
    let tmp = tempfile::tempdir().unwrap();
    let dst_path = tmp.path().join("dst.sqlite");

    // 接收机：同路径的根存在，但用户先前把它关掉、并移除过索引。
    let existing_id = {
        let db = Db::open(&dst_path).unwrap();
        let id = db.upsert_root("/media-a", 0, false).unwrap();
        db.set_root_closed(id, true).unwrap();
        db.mark_root_index_removed(id).unwrap();
        assert!(db.root_index_removed(id).unwrap(), "前置：根已标移除");
        assert!(
            !db.roots(false).unwrap().iter().any(|r| r.id == id),
            "前置：已关闭的根不在可见根列表里"
        );
        id
    };

    let mut db = Db::open(&dst_path).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let rep = execute_import(&mut db, &pkg, tmp.path(), ImportStrategy::Merge, &cancel).unwrap();
    assert_eq!(rep.imported, 1, "照片确实导进来了");

    assert_eq!(
        db.total_photos().unwrap(),
        1,
        "照片挂在库里，总数不能是 0"
    );
    let visible = db.count_photos_visible(None).unwrap();
    assert_eq!(
        visible, 1,
        "导入的照片必须可见——挂进已关闭的根等于没导入"
    );

    let root = db.root(existing_id).unwrap().expect("根行仍在");
    assert!(root.is_active(), "导入写进了照片，根必须恢复为 active");
    assert!(
        !db.root_index_removed(existing_id).unwrap(),
        "「索引已移除」标记必须清掉，否则界面仍显示「索引已移除」"
    );
    assert_eq!(root.id, existing_id, "复用既有根，不新建第二个");
    assert_eq!(db.roots(false).unwrap().len(), 1, "只应有一个根");
}

#[test]
fn importing_leaves_unrelated_closed_roots_alone() {
    let (_src_tmp, pkg) = package();
    let tmp = tempfile::tempdir().unwrap();
    let dst_path = tmp.path().join("dst.sqlite");

    let other_id = {
        let db = Db::open(&dst_path).unwrap();
        let other = db.upsert_root("/elsewhere", 0, false).unwrap();
        db.set_root_closed(other, true).unwrap();
        db.mark_root_index_removed(other).unwrap();
        other
    };

    let mut db = Db::open(&dst_path).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    execute_import(&mut db, &pkg, tmp.path(), ImportStrategy::Merge, &cancel).unwrap();

    let other = db.root(other_id).unwrap().expect("根行仍在");
    assert!(
        !other.is_active(),
        "与本包无关的关闭根不能被一次导入悄悄拉出来"
    );
    assert!(
        db.root_index_removed(other_id).unwrap(),
        "无关根的「索引已移除」标记也不该被清"
    );
}

#[test]
fn importing_an_nfd_package_does_not_duplicate_nfc_folders() {
    // 真实事故：macOS 上日文目录名是 NFD（`ば` = `は` + 浊点），本产品的
    // `folders.path` 存 NFC。导入包里留着 NFD 时，SQL 的 `m.path = s.path`
    // 按字节比较认为「这是另一个目录」，而 `folders.path` 的 UNIQUE 也是字节层，
    // 于是 NFD 那行照样插得进去——同一张照片在左栏出现两次。
    // 实测一次真实导入产生 101 组重复目录、3470 张重复照片。
    let nfc = "/media-a/つばさ舞";
    let nfd = "/media-a/つは\u{3099}さ舞";
    assert_ne!(nfc, nfd, "前置：两种编码必须是不同的字节串");

    // 源库用 NFD 写路径（模拟 macOS 导出的包）。
    let src_tmp = tempfile::tempdir().unwrap();
    let src_path = src_tmp.path().join("src.sqlite");
    let pkg = src_tmp.path().join("lib.opicasaidx");
    {
        let db = Db::open(&src_path).unwrap();
        let root = db.upsert_root("/media-a", 0, false).unwrap();
        let top = db.upsert_folder("/media-a", "/media-a", None).unwrap();
        let sub = db.upsert_folder(nfd, nfd, None).unwrap();
        db.set_folder_root(top, root, None).unwrap();
        db.set_folder_root(sub, root, Some(top)).unwrap();
        db.apply_scan(top, "/media-a", &[photo("x.jpg", 100)], 0)
            .unwrap();
        db.apply_scan(sub, nfd, &[photo("y.jpg", 200)], 0).unwrap();
        // refresh_hierarchy 会把本库路径归一成 NFC——先归一，再用裸连接把源库
        // 改回 NFD，模拟「从 macOS 上导出的包」真实的样子。
        db.refresh_hierarchy().unwrap();
        drop(db);
        let raw = rusqlite::Connection::open(&src_path).unwrap();
        raw.execute(
            "UPDATE folders SET path = ?2, path_key = ?3 WHERE path = ?1",
            rusqlite::params![nfc, nfd, nfd.to_lowercase()],
        )
        .unwrap();
        drop(raw);
        let db = Db::open(&src_path).unwrap();
        assert!(
            db.all_folders().unwrap().iter().any(|f| f.path == nfd),
            "前置：包里必须留着 NFD 路径"
        );
        export_index(&db, &pkg, false, "0.4.0-test", 1000).unwrap();
    }

    // 目标库用 NFC 存同一批照片。
    let dst_tmp = tempfile::tempdir().unwrap();
    let dst_path = dst_tmp.path().join("dst.sqlite");
    {
        let db = Db::open(&dst_path).unwrap();
        let root = db.upsert_root("/media-a", 0, false).unwrap();
        let top = db.upsert_folder("/media-a", "/media-a", None).unwrap();
        let sub = db.upsert_folder(nfc, nfc, None).unwrap();
        db.set_folder_root(top, root, None).unwrap();
        db.set_folder_root(sub, root, Some(top)).unwrap();
        db.apply_scan(top, "/media-a", &[photo("x.jpg", 100)], 0)
            .unwrap();
        db.apply_scan(sub, nfc, &[photo("y.jpg", 200)], 0).unwrap();
        db.refresh_hierarchy().unwrap();
        assert_eq!(db.total_photos().unwrap(), 2);
    }

    let mut db = Db::open(&dst_path).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    execute_import(&mut db, &pkg, dst_tmp.path(), ImportStrategy::Merge, &cancel).unwrap();

    let folders = db.all_folders().unwrap();
    let same_dir: Vec<_> = folders
        .iter()
        .filter(|f| f.path.ends_with("つばさ舞") || f.path.ends_with("つは\u{3099}さ舞"))
        .collect();
    assert_eq!(
        same_dir.len(),
        1,
        "同一个目录只能有一行，实际：{:?}",
        same_dir.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    assert_eq!(
        db.total_photos().unwrap(),
        2,
        "重复导入同一批照片不能翻倍"
    );
}
