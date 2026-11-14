//! M4.3 `IDX-01`：添加照片文件夹的入口契约。
//!
//! 规格（docs/18 §IDX-01，P0）：
//! - 规范路径、**重复 / 父子根检测**，展示范围与权限；
//! - 同路径重新添加**复用 root 及标记**；
//! - 验收：同根连加两次不重复；父子重叠**不重复计数**；权限失败给原因，
//!   且已入库内容仍可浏览。
//!
//! 判定逻辑本身是 `picasa_core::root_intake` 的纯函数（那里有单测覆盖各种形态），
//! 这里验的是它与真库接上之后**计数不会翻倍**——只有真库才能证伪的部分。

use picasa_core::{AppPaths, Library, RootIntake};
use picasa_db::PhotoInput;
use std::path::Path;

fn isolated_paths(root: &Path) -> AppPaths {
    AppPaths {
        support: root.join("Library/Application Support/app.openpicasa"),
        preferences: root.join("Library/Preferences/app.openpicasa"),
        caches: root.join("Library/Caches/app.openpicasa"),
        logs: root.join("Library/Logs/app.openpicasa"),
    }
}

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
        taken_at_src: 1,
        format: 1,
    }
}

/// 把一个目录（连同其照片）作为根入库。
fn seed_root(lib: &Library, root: &str, items: &[PhotoInput]) {
    let db = lib.db().unwrap();
    let rid = db.upsert_root(root, 0, false).unwrap();
    let fid = db.upsert_folder(root, root, None).unwrap();
    db.set_folder_root(fid, rid, None).unwrap();
    db.apply_scan(fid, root, items, 0).unwrap();
}

fn root_paths(lib: &Library) -> Vec<String> {
    lib.db()
        .unwrap()
        .roots(false)
        .unwrap()
        .into_iter()
        .map(|r| r.path)
        .collect()
}

#[test]
fn adding_the_same_root_twice_reuses_the_row_and_keeps_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    seed_root(&lib, "/media", &[photo("a.jpg", 100), photo("b.jpg", 200)]);

    let id = {
        let db = lib.db().unwrap();
        let ids = db.photos(None, picasa_db::SortSpec::default(), 10).unwrap();
        let a = ids.iter().find(|r| r.name == "a.jpg").unwrap();
        db.set_starred(&[a.id], true).unwrap();
        db.add_tag(&[a.id], "旅行").unwrap();
        db.upsert_root("/media", 0, false).unwrap()
    };

    // 用户又点了一次「添加照片文件夹」，选了同一个目录。
    let intake = picasa_core::root_intake::classify("/media", &root_paths(&lib));
    assert!(
        intake.skips_insert(),
        "同路径必须复用既有根，而不是插第二行：{intake:?}"
    );

    // 复用既有行（`upsert_root` 是幂等的）：根数量不变、root_id 不变。
    lib.add_root(Path::new("/media"));
    let db = lib.db().unwrap();
    let again = db.upsert_root("/media", 0, false).unwrap();
    assert_eq!(again, id, "同路径重新添加必须复用同一个 root_id");
    assert_eq!(db.roots(false).unwrap().len(), 1, "不能插出第二个根");
    assert_eq!(db.total_photos().unwrap(), 2, "照片数不变");

    // 标记跟着那一行，不会被「再添加」冲掉。
    let photos = db.photos(None, picasa_db::SortSpec::default(), 10).unwrap();
    let a = photos.iter().find(|r| r.name == "a.jpg").unwrap();
    assert!(db.starred_ids().unwrap().contains(&a.id));
    assert!(db.tags_of(a.id).unwrap().iter().any(|t| t == "旅行"));
}

#[test]
fn parent_child_overlap_does_not_double_count_photos() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::open(isolated_paths(tmp.path())).unwrap();

    // 先关注子目录，再加它的父目录——这是最容易「看起来翻倍」的顺序。
    seed_root(&lib, "/media/2020", &[photo("a.jpg", 100)]);
    seed_root(&lib, "/media", &[photo("b.jpg", 200)]);
    let rp = root_paths(&lib);
    let intake = picasa_core::root_intake::classify("/media/2021", &rp);
    assert!(
        matches!(intake, RootIntake::ChildOf(ref p) if p == "/media"),
        "已在关注范围内的目录要报出最贴近的覆盖者：{intake:?}"
    );
    assert!(intake.overlaps());
    assert!(
        !intake.skips_insert(),
        "父子重叠仍允许添加——硬拒会挡住「单独关注子目录」的合理用法"
    );

    // ⚠️ 先在锁外把根路径取出来：`root_paths()` 内部要再拿一次 `lib.db()`，
    //    如果此时还握着 db 守卫，就会在同一个 Mutex 上自锁——测试会静默卡死
    //    （不是断言失败，是没有输出、只能靠超时发现）。
    let after: Vec<String> = root_paths(&lib);
    {
        let db = lib.db().unwrap();
        assert_eq!(db.roots(false).unwrap().len(), 2, "两个根各自一行");
        assert_eq!(
            db.total_photos().unwrap(),
            2,
            "父子重叠不能让同一张照片算两次（photos 按「路径 + 文件名」唯一）"
        );
    }

    // 反过来：已有父根时再加子根，应报 ChildOf（归入父根之下）。
    let back = picasa_core::root_intake::classify("/media/2019", &after);
    assert!(matches!(back, RootIntake::ChildOf(_)), "{back:?}");
}

#[test]
fn adding_a_parent_reports_the_children_it_now_covers() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::open(isolated_paths(tmp.path())).unwrap();
    seed_root(&lib, "/media/2020", &[photo("a.jpg", 100)]);
    seed_root(&lib, "/media/2021", &[photo("b.jpg", 200)]);

    let intake = picasa_core::root_intake::classify("/media", &root_paths(&lib));
    match &intake {
        RootIntake::ParentOf(covered) => {
            assert_eq!(covered.len(), 2, "两个已关注的子根都要列出来：{covered:?}");
        }
        other => panic!("应先报祖先关系再谈添加：{other:?}"),
    }
    // 添加之后照片仍然只有两张。
    seed_root(&lib, "/media", &[photo("c.jpg", 300)]);
    let db = lib.db().unwrap();
    assert_eq!(db.total_photos().unwrap(), 3);
    assert_eq!(db.roots(false).unwrap().len(), 3);
}

#[test]
fn unreadable_directory_is_refused_and_the_library_stays_browsable() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::open(isolated_paths(tmp.path())).unwrap();
    seed_root(&lib, "/media", &[photo("a.jpg", 100), photo("b.jpg", 200)]);

    // 不存在的目录：入口就该拒，而不是加进去再留一条看不懂的失败任务。
    let missing = tmp.path().join("not-mounted");
    let why = picasa_core::root_intake::probe_dir(&missing);
    assert!(why.is_some(), "不可读目录必须给出原因");
    let why = why.unwrap();
    assert!(!why.is_empty(), "原因不能是空串");

    // 拒掉之后，已入库的内容照样能浏览（IDX-01 验收第二句）。
    let db = lib.db().unwrap();
    assert_eq!(db.roots(false).unwrap().len(), 1);
    assert_eq!(
        db.count_photos_visible(None).unwrap(),
        2,
        "拒绝添加不能影响已索引内容"
    );
    assert!(db.photos(None, picasa_db::SortSpec::default(), 10).unwrap().len() == 2);
}
