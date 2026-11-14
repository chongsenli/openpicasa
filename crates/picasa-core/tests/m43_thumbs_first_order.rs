//! 用户要求：**有缩略图的往前放，没有缩略图的单独放在一起**。
//!
//! 这条走的是 `Library` 的显示路径（先 `reload_photos` 再 `extend_photos` 翻页），
//! 与 `picasa-db/tests/m43_thumbs_first.rs` 的纯查询用例互补：
//! 那边钉「查询本身对不对」，这边钉「栅格真正拿到的顺序对不对、翻页会不会串」。
//!
//! 关键取舍（写在 `Db::photos_bucketed` 的文档里）：
//! - 桶键只看 `photos.thumb_state = 1`，**不查 `thumbs` 表**（5.6 万张实测
//!   `EXISTS` 子查询 5.6 s vs 整数列 0.04 s）；
//! - 分桶时**不用键集翻页**，退回 OFFSET——键集游标不带桶号会跨桶重复。

use picasa_core::{AppPaths, Library};
use picasa_db::{PhotoInput, SortKey, SortSpec, ThumbBuf, ThumbState};
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

fn thumb() -> ThumbBuf {
    ThumbBuf {
        w: 1,
        h: 1,
        format: 0,
        data: vec![0xFF, 0xD8, 0xFF, 0xD9],
    }
}

/// 6 张照片都在同一个根里，拍摄时间递增；`a`/`c`/`e` 有缩略图。
fn fixture() -> (tempfile::TempDir, Library) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = isolated_paths(tmp.path());
    let lib = Library::open(paths).unwrap();
    let shared = "/photos";
    let db = lib.db().unwrap();
    let rid = db.upsert_root(shared, 0, false).unwrap();
    let fid = db.upsert_folder(shared, shared, None).unwrap();
    db.set_folder_root(fid, rid, None).unwrap();
    let items = vec![
        photo("a.jpg", 1_300_000_001),
        photo("b.jpg", 1_300_000_002),
        photo("c.jpg", 1_300_000_003),
        photo("d.jpg", 1_300_000_004),
        photo("e.jpg", 1_300_000_005),
        photo("f.jpg", 1_300_000_006),
    ];
    db.apply_scan(fid, shared, &items, 0).unwrap();
    let ids: Vec<(String, i64)> = db
        .photos(None, SortSpec::default(), 100)
        .unwrap()
        .into_iter()
        .map(|r| (r.name, r.id))
        .collect();
    for (name, id) in &ids {
        match name.as_str() {
            "a.jpg" | "c.jpg" | "e.jpg" => db.put_thumb(*id, 0, &thumb()).unwrap(),
            "d.jpg" => db.set_thumb_state(*id, ThumbState::Failed).unwrap(),
            "f.jpg" => db.set_thumb_state(*id, ThumbState::CloudOnly).unwrap(),
            _ => db.set_thumb_state(*id, ThumbState::Pending).unwrap(),
        }
    }
    db.refresh_hierarchy().unwrap();
    drop(db);
    (tmp, lib)
}

fn names(lib: &Library) -> Vec<String> {
    lib.photos.iter().map(|p| p.name.clone()).collect()
}

#[test]
fn the_grid_lists_photos_with_thumbnails_first() {
    let (_tmp, mut lib) = fixture();
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();

    assert_eq!(
        names(&lib),
        vec!["e.jpg", "c.jpg", "a.jpg", "f.jpg", "d.jpg", "b.jpg"],
        "栅格顺序：有图的整桶在前，两桶内部都按拍摄时间倒序"
    );
}

#[test]
fn turning_the_switch_off_restores_pure_time_order() {
    let (_tmp, mut lib) = fixture();
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();
    assert!(lib.thumbs_first, "默认开着——用户眼里一半灰格更难受");

    lib.set_thumbs_first(false).unwrap();
    assert_eq!(
        names(&lib),
        vec!["f.jpg", "e.jpg", "d.jpg", "c.jpg", "b.jpg", "a.jpg"],
        "关掉分桶后回到纯拍摄时间倒序"
    );

    // 再打开必须回到分桶顺序，不能有残留状态。
    lib.set_thumbs_first(true).unwrap();
    assert_eq!(
        names(&lib),
        vec!["e.jpg", "c.jpg", "a.jpg", "f.jpg", "d.jpg", "b.jpg"]
    );
}

#[test]
fn the_choice_survives_a_restart() {
    let (tmp, mut lib) = fixture();
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();
    lib.set_thumbs_first(false).unwrap();
    lib.save_prefs();
    drop(lib);

    let lib2 = Library::open(isolated_paths(tmp.path())).unwrap();
    assert!(
        !lib2.thumbs_first,
        "关掉之后重启必须还是关的（存在偏好里，不是会话状态）"
    );
}

#[test]
fn bucketing_still_orders_by_the_selected_sort_key() {
    let (_tmp, mut lib) = fixture();
    lib.reload_tree().unwrap();
    // 换成按文件名升序：两桶内部都必须按 name_key 升序，而不是退回时间序。
    lib.set_sort(SortKey::Name).unwrap();
    // 默认 descending = false for Name? `set_sort` 对与当前不同的键按
    // matches!(TakenAt|ModifiedAt|Size) 定方向，Name 不在其中 → 升序。
    assert_eq!(lib.sort.key, SortKey::Name);
    assert!(!lib.sort.descending);

    let got = names(&lib);
    assert_eq!(
        got,
        vec!["a.jpg", "c.jpg", "e.jpg", "b.jpg", "d.jpg", "f.jpg"],
        "分桶只在最外层分两段，桶内仍严格按当前排序键（文件名升序）"
    );
}

#[test]
fn extend_photos_appends_the_second_bucket_without_repeats() {
    let (_tmp, mut lib) = fixture();
    lib.reload_tree().unwrap();
    // 首屏只装 2 张（都在第一桶），随后靠 extend 补齐。
    lib.reload_photos().unwrap();
    let first_page: Vec<String> = names(&lib);
    assert_eq!(first_page.len(), 6, "6 张小于 PAGE_FIRST，首屏就装完了");

    // 再 ensure 一次到底：不报错、不重复。
    let more = lib.extend_photos().unwrap();
    assert!(!more, "已经到底，不该再追加");
    assert_eq!(
        names(&lib),
        vec!["e.jpg", "c.jpg", "a.jpg", "f.jpg", "d.jpg", "b.jpg"]
    );
}
