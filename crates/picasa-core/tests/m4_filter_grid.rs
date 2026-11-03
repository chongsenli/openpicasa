//! B16 / B17：筛选必须改栅格，不能只改芯片。
//!
//! 以前 `reload_photos` 把「可见全库」的张数写进 `photos_total`。首屏查询
//! 其实是筛过的，但 `photos_complete` 为 false，下一帧 `extend_photos`
//! 按无筛选的 OFFSET 把其余行拼回来——JPEG 里出现 gif/png，格子还会重复。
//! 侧栏「星标」和「只看收藏」是同一个 `starred_only`；已经停在「所有照片」
//! 上再点一次时，`set_view` 不能因为视图没变就把这个筛选留在原地。

use picasa_core::{AppPaths, Library, View};
use picasa_db::{PhotoFilter, PhotoInput};
use std::collections::HashSet;
use std::path::Path;

fn isolated_paths(root: &Path) -> AppPaths {
    AppPaths {
        support: root.join("Library/Application Support/app.openpicasa"),
        preferences: root.join("Library/Preferences/app.openpicasa"),
        caches: root.join("Library/Caches/app.openpicasa"),
        logs: root.join("Library/Logs/app.openpicasa"),
    }
}

fn photo(name: &str, format: i64, taken: i64) -> PhotoInput {
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
        format,
    }
}

/// 9 张，和 QA 样本同形：jpeg / gif / bmp / png 混在一起，其中 3 张收藏。
fn fixture() -> (tempfile::TempDir, Library) {
    let tmp = tempfile::tempdir().unwrap();
    let ps = isolated_paths(tmp.path());
    let mut lib = Library::open(ps).unwrap();
    {
        let db = lib.db().unwrap();
        let root = db.upsert_root("/albums", 0, false).unwrap();
        let fid = db.upsert_folder("/albums", "/albums", None).unwrap();
        db.set_folder_root(fid, root, None).unwrap();
        let items = vec![
            photo("deep.jpg", 1, 100),
            photo("other.jpg", 1, 200),
            photo("tiny.gif", 3, 300),
            photo("a.png", 2, 400),
            photo("b.bmp", 4, 500),
            photo("c.webp", 6, 600),
            photo("d.tiff", 5, 700),
            photo("e.heic", 7, 800),
            photo("f.jpg", 1, 900),
        ];
        db.apply_scan(fid, "/albums", &items, 0).unwrap();
        let rows = db
            .photos_from_visible(None, picasa_db::SortSpec::default(), 20, 0)
            .unwrap();
        let id = |name: &str| rows.iter().find(|p| p.name == name).unwrap().id;
        db.set_starred(&[id("deep.jpg"), id("tiny.gif"), id("f.jpg")], true)
            .unwrap();
    }
    lib.reload_photos().unwrap();
    (tmp, lib)
}

fn names(lib: &Library) -> HashSet<String> {
    lib.photos.iter().map(|p| p.name.clone()).collect()
}

fn assert_unique(lib: &Library) {
    let ids: HashSet<i64> = lib.photos.iter().map(|p| p.id).collect();
    assert_eq!(ids.len(), lib.photos.len(), "栅格出现了重复 id");
    assert_eq!(
        lib.photos_total,
        lib.photos.len(),
        "标题张数 {} 和栅格 {} 不一致",
        lib.photos_total,
        lib.photos.len()
    );
}

#[test]
fn jpeg_filter_drops_other_formats_and_does_not_duplicate_on_extend() {
    let (_tmp, mut lib) = fixture();
    assert_eq!(lib.photos_total, 9);

    lib.set_filter(Some(PhotoFilter {
        formats: vec!["jpeg".into()],
        ..PhotoFilter::default()
    }))
    .unwrap();

    assert_eq!(
        names(&lib),
        HashSet::from(["deep.jpg".into(), "other.jpg".into(), "f.jpg".into()])
    );
    assert_eq!(lib.view_title(), "所有照片");
    assert!(
        lib.photos_complete,
        "筛完首屏就该到底，否则下一帧会把没筛中的行补回来"
    );
    assert_unique(&lib);

    // 模拟「首屏没装完」：补页必须仍走筛选，不能按无筛选 OFFSET 把 gif 捞回来。
    lib.photos.truncate(1);
    lib.photos_complete = false;
    assert!(lib.extend_photos().unwrap());
    assert_eq!(
        names(&lib),
        HashSet::from(["deep.jpg".into(), "other.jpg".into(), "f.jpg".into()])
    );
    assert_unique(&lib);
    assert!(!lib.extend_photos().unwrap());
}

#[test]
fn starred_filter_updates_title_and_all_photos_clears_it() {
    let (_tmp, mut lib) = fixture();

    lib.set_filter(Some(PhotoFilter {
        starred_only: true,
        ..PhotoFilter::default()
    }))
    .unwrap();

    assert_eq!(
        names(&lib),
        HashSet::from(["deep.jpg".into(), "tiny.gif".into(), "f.jpg".into()])
    );
    assert!(lib.photos.iter().all(|p| p.starred));
    assert_eq!(lib.view_title(), "星标");
    assert_eq!(lib.photos_total, 3);
    assert_unique(&lib);
    assert!(lib.photos_complete);
    assert!(!lib.extend_photos().unwrap());

    // 已经在「所有照片」上。再点一次必须退出收藏，不能因为 view 没变而粘住。
    assert_eq!(lib.view, View::All);
    lib.set_view(View::All).unwrap();
    assert!(lib.filter.is_none());
    assert_eq!(lib.view_title(), "所有照片");
    assert_eq!(lib.photos_total, 9);
    assert_eq!(names(&lib).len(), 9);
    assert_unique(&lib);

    // 再点一次是空操作。
    lib.set_view(View::All).unwrap();
    assert_eq!(lib.photos_total, 9);
}

#[test]
fn starred_plus_jpeg_keeps_format_when_leaving_star_view() {
    let (_tmp, mut lib) = fixture();
    lib.set_filter(Some(PhotoFilter {
        formats: vec!["jpeg".into()],
        starred_only: true,
        ..PhotoFilter::default()
    }))
    .unwrap();
    assert_eq!(
        names(&lib),
        HashSet::from(["deep.jpg".into(), "f.jpg".into()])
    );
    assert_eq!(lib.view_title(), "星标");

    lib.set_view(View::All).unwrap();
    let left = lib.filter.clone().expect("格式筛选还在");
    assert!(!left.starred_only);
    assert_eq!(left.formats, vec!["jpeg".to_string()]);
    assert_eq!(
        names(&lib),
        HashSet::from(["deep.jpg".into(), "other.jpg".into(), "f.jpg".into()])
    );
    assert_eq!(lib.view_title(), "所有照片");
    assert_unique(&lib);

    lib.set_filter(None).unwrap();
    assert_eq!(names(&lib).len(), 9);
    assert_eq!(lib.view_title(), "所有照片");
    assert_unique(&lib);
}

#[test]
fn recent_view_does_not_clear_star_until_all_photos() {
    let (_tmp, mut lib) = fixture();
    lib.set_filter(Some(PhotoFilter {
        starred_only: true,
        ..PhotoFilter::default()
    }))
    .unwrap();
    lib.set_view(View::Recent).unwrap();
    assert!(lib.filter.as_ref().unwrap().starred_only);
    assert_eq!(lib.view_title(), "星标");
    assert!(names(&lib).is_subset(&HashSet::from([
        "deep.jpg".into(),
        "tiny.gif".into(),
        "f.jpg".into(),
    ])));

    lib.set_view(View::All).unwrap();
    assert!(lib.filter.is_none());
    assert_eq!(lib.photos_total, 9);
    assert_eq!(lib.view_title(), "所有照片");
}
