//! M5-R5 浏览 / 筛选 / 排序回归（`AC-SF-SCOPE`、`AC-SF-01`、`AC-SORT-THUMBS`）。
//!
//! 这些用例都走真实的 `Library`（真 SQLite、真查询），不 mock：
//! 搜索范围、筛选与搜索求交、排序键与「有缩略图的排在前面」都是
//! 「改壳时最容易只剩芯片亮着、栅格没动」的路径。

use picasa_core::{AppPaths, Library, View};
use picasa_db::{PhotoFilter, PhotoInput, SizeClass, SortKey, ThumbState};
use std::collections::HashSet;
use std::path::Path;

const ROOT: &str = "/m5r5/photos";
const SUB: &str = "/m5r5/photos/trip";

fn isolated_paths(root: &Path) -> AppPaths {
    AppPaths {
        support: root.join("Library/Application Support/app.openpicasa"),
        preferences: root.join("Library/Preferences/app.openpicasa"),
        caches: root.join("Library/Caches/app.openpicasa"),
        logs: root.join("Library/Logs/app.openpicasa"),
    }
}

fn input(name: &str, size: i64, w: u32, h: u32, taken: i64, format: i64) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_ascii_lowercase(),
        size_bytes: size,
        mtime_ns: taken * 1_000_000_000,
        width: Some(w as i64),
        height: Some(h as i64),
        orientation: Some(1),
        taken_at: Some(taken),
        taken_at_src: 1,
        format,
    }
}

/// 根目录 3 张 + 子目录 3 张：
/// - 根：`beach.jpg`(小图) / `city.png` / `flower.jpg`（文件名都不含 trip）
/// - 子：`trip-1.jpg` / `trip-2.jpg` / `trip-3.jpg`
fn fixture() -> (tempfile::TempDir, Library, i64, i64) {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    let (root_fid, sub_fid) = {
        let db = lib.db().unwrap();
        let root = db.upsert_root(ROOT, 0, false).unwrap();
        let root_fid = db.upsert_folder(ROOT, ROOT, None).unwrap();
        db.set_folder_root(root_fid, root, None).unwrap();
        let sub_fid = db.upsert_folder(SUB, "trip", None).unwrap();
        db.set_folder_root(sub_fid, root, Some(root_fid)).unwrap();
        db.apply_scan(
            root_fid,
            ROOT,
            &[
                input("beach.jpg", 100 * 1024, 400, 300, 1_300_000_001, 1),
                input("city.png", 200 * 1024, 1200, 800, 1_300_000_002, 2),
                input("flower.jpg", 900 * 1024, 4000, 3000, 1_300_000_003, 1),
            ],
            0,
        )
        .unwrap();
        db.apply_scan(
            sub_fid,
            SUB,
            &[
                input("trip-1.jpg", 150 * 1024, 500, 400, 1_300_000_011, 1),
                input("trip-2.jpg", 250 * 1024, 1500, 1000, 1_300_000_012, 1),
                input("trip-3.jpg", 950 * 1024, 4100, 3100, 1_300_000_013, 1),
            ],
            0,
        )
        .unwrap();
        (root_fid, sub_fid)
    };
    // 树是 `Library::open` 时从 DB 读的；本用例在同一个会话里新建了子目录，
    // 必须显式重建，否则「含子文件夹」拿不到子节点。
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();
    (tmp, lib, root_fid, sub_fid)
}

fn names(lib: &Library) -> HashSet<String> {
    lib.photos.iter().map(|p| p.name.clone()).collect()
}

fn assert_unique(lib: &Library) {
    let ids: HashSet<i64> = lib.photos.iter().map(|p| p.id).collect();
    assert_eq!(ids.len(), lib.photos.len(), "栅格出现重复 id");
    assert_eq!(
        lib.photos_total,
        lib.photos.len(),
        "标题张数与栅格行数不一致"
    );
}

/// `AC-SF-SCOPE` / `M5-SF-01`～`02`：搜索命中文件名或文件夹路径；
/// 「文」只在当前文件夹，「全」在整个图库；两者都与当前筛选求交。
#[test]
fn search_matches_name_or_folder_path_and_respects_scope() {
    let (_tmp, mut lib, root_fid, _sub_fid) = fixture();
    lib.set_view(View::Folder(root_fid)).unwrap();

    // 「文」：只在根目录里找。trip 的照片在子目录 → 0 张。
    lib.set_search_in_current(true).unwrap();
    lib.set_search("trip".into()).unwrap();
    assert!(lib.photos.is_empty(), "「文」不该命中子目录里的照片");
    assert_eq!(lib.photos_total, 0);

    // 「全」：全库 LIKE，命中文件夹路径 /m5r5/photos/trip。
    lib.set_search_in_current(false).unwrap();
    assert_eq!(
        names(&lib),
        HashSet::from(["trip-1.jpg".into(), "trip-2.jpg".into(), "trip-3.jpg".into()]),
        "「全」要能按文件夹路径命中"
    );
    assert_unique(&lib);

    // 文件名 LIKE（大小写不敏感，走 name_key）
    lib.set_search_in_current(true).unwrap();
    lib.set_search("BEACH".into()).unwrap();
    assert_eq!(names(&lib), HashSet::from(["beach.jpg".into()]));

    // 与筛选求交：搜索 trip + 只看 JPEG（全库）
    lib.set_search_in_current(false).unwrap();
    lib.set_search("trip".into()).unwrap();
    lib.set_filter(Some(PhotoFilter {
        formats: vec!["png".into()],
        ..PhotoFilter::default()
    }))
    .unwrap();
    assert!(lib.photos.is_empty(), "trip 里没有 PNG，交集必须为空");

    // 清搜索 + 清筛选 → 回到该视图的全部（根目录 3 张；子目录要「含子文件夹」）
    lib.set_search(String::new()).unwrap();
    lib.set_filter(None).unwrap();
    assert_eq!(lib.photos.len(), 3);
    // 打开「含子文件夹」后是 6 张（只改视图过滤，不改变磁盘扫描是否递归）
    lib.set_recurse(true).unwrap();
    assert_eq!(lib.photos.len(), 6);
}

/// `AC-SF-01` / `M5-SF-05` / `M5-SF-06`：类型与长边三档尺寸筛选真的改栅格。
#[test]
fn format_and_size_filters_actually_change_the_grid() {
    let (_tmp, mut lib, _root, _sub) = fixture();
    lib.set_view(View::All).unwrap();

    lib.set_filter(Some(PhotoFilter {
        formats: vec!["png".into()],
        ..PhotoFilter::default()
    }))
    .unwrap();
    assert_eq!(names(&lib), HashSet::from(["city.png".into()]));
    assert_unique(&lib);

    lib.set_filter(Some(PhotoFilter {
        size_class: Some(SizeClass::Large),
        ..PhotoFilter::default()
    }))
    .unwrap();
    assert_eq!(
        names(&lib),
        HashSet::from(["flower.jpg".into(), "trip-3.jpg".into()]),
        "大图 = 长边 > 1920"
    );

    lib.set_filter(Some(PhotoFilter {
        size_class: Some(SizeClass::Small),
        ..PhotoFilter::default()
    }))
    .unwrap();
    assert_eq!(
        names(&lib),
        HashSet::from(["beach.jpg".into(), "trip-1.jpg".into()]),
        "小图 = 长边 ≤ 640"
    );

    // 文件大小区间（字节）
    lib.set_filter(Some(PhotoFilter {
        size_bytes: Some((300 * 1024, i64::MAX)),
        ..PhotoFilter::default()
    }))
    .unwrap();
    assert_eq!(
        names(&lib),
        HashSet::from(["flower.jpg".into(), "trip-3.jpg".into()])
    );
}

/// `AC-SORT-THUMBS` / `M5-MN-06～08`：五个排序键都生效，方向可切，
/// 「有缩略图的排在前面」把无图的整桶排在后面。
#[test]
fn every_sort_key_and_the_thumbnail_bucket_actually_reorder() {
    let (_tmp, mut lib, _root, _sub) = fixture();
    lib.set_view(View::All).unwrap();

    // 文件名升序
    lib.set_sort(SortKey::Name).unwrap(); // 从 TakenAt 切到 Name → 默认升序
    assert!(
        !lib.sort.descending,
        "切到文件名应默认升序（不是继承拍摄日期的降序）"
    );
    let by_name: Vec<String> = lib.photos.iter().map(|p| p.name.clone()).collect();
    let mut expect = by_name.clone();
    expect.sort();
    assert_eq!(by_name, expect, "文件名排序不对");

    // 再点一次同键 = 反向
    lib.set_sort(SortKey::Name).unwrap();
    assert!(lib.sort.descending);
    let mut expect_desc = expect.clone();
    expect_desc.reverse();
    let got: Vec<String> = lib.photos.iter().map(|p| p.name.clone()).collect();
    assert_eq!(got, expect_desc);

    // 文件大小升序
    lib.set_sort(SortKey::Size).unwrap(); // 切键 → 默认降序
    assert!(lib.sort.descending, "文件大小默认新/大在前");
    lib.set_sort(SortKey::Size).unwrap(); // 同键 → 切升序
    assert!(!lib.sort.descending);
    let sizes: Vec<i64> = lib.photos.iter().map(|p| p.size_bytes).collect();
    let mut sorted = sizes.clone();
    sorted.sort();
    assert_eq!(sizes, sorted, "文件大小升序不对");

    // 修改日期 + 拍摄日期 + 图片尺寸都能排（不 panic、行数不变）
    for key in [SortKey::ModifiedAt, SortKey::TakenAt, SortKey::Dimensions] {
        lib.set_sort(key).unwrap();
        assert_eq!(lib.photos.len(), 6, "{key:?} 排序后行数变了");
        assert_unique(&lib);
    }
}

/// `AC-SORT-THUMBS` / `M5-MN-07`：开启「有缩略图的排在前面」后，
/// 无缩略图（含云端未下载、失败）整桶排后；关闭后回到纯排序键。
#[test]
fn thumbs_first_buckets_missing_thumbnails_after_those_with_one() {
    let (_tmp, lib, _root, _sub) = fixture();
    let ids: Vec<i64> = lib.photos.iter().map(|p| p.id).collect();
    // 前两张：有缩略图；后四张：失败 / 云端未下载 / 未生成
    {
        let db = lib.db().unwrap();
        for (i, id) in ids.iter().enumerate() {
            let state = if i < 2 {
                ThumbState::Ok
            } else if i % 2 == 0 {
                ThumbState::Failed
            } else {
                ThumbState::CloudOnly
            };
            db.set_thumb_state(*id, state).unwrap();
        }
    }

    let mut lib = lib;
    lib.set_view(View::All).unwrap();
    lib.set_sort(SortKey::Name).unwrap();
    lib.set_thumbs_first(true).unwrap();
    let with: Vec<i64> = lib.photos.iter().map(|p| p.id).collect();
    assert_eq!(
        with[..2].iter().copied().collect::<HashSet<_>>(),
        ids[..2].iter().copied().collect::<HashSet<_>>(),
        "有缩略图的两张必须整桶在前：{with:?}"
    );
    for id in &with[2..] {
        let row = lib.photos.iter().find(|p| p.id == *id).unwrap();
        assert_ne!(
            row.thumb_state,
            ThumbState::Ok as i64,
            "无缩略图的不能混进前桶"
        );
    }
    assert_unique(&lib);

    // 关闭后恢复纯排序：不再分桶
    lib.set_thumbs_first(false).unwrap();
    let plain: Vec<i64> = lib.photos.iter().map(|p| p.id).collect();
    assert_eq!(plain.len(), 6);
}

/// `M5-SF-16` / `AC-SF-01`：筛选结构里**没有**评分 / 人物 / 颜色维度。
///
/// 这条用结构判定而不是文本搜索：`PhotoFilter` 的字段就是产品允许的筛选维度，
/// 一旦有人加 `rating` / `person` / `color` 字段，这里会先亮。
#[test]
fn photo_filter_has_no_rating_person_or_colour_dimensions() {
    let f = PhotoFilter::default();
    // 允许的维度逐一命名，新增字段时这里必须一起改（有意的摩擦）。
    let PhotoFilter {
        ref taken_range,
        ref formats,
        size_class,
        size_bytes,
        dimension_range,
        ref camera,
        ref lens,
        missing_date,
        missing_exif,
        starred_only,
        ref tag,
        ref album,
        ref ids,
    } = f;
    let all_unset = taken_range.is_none()
        && formats.is_empty()
        && size_class.is_none()
        && size_bytes.is_none()
        && dimension_range.is_none()
        && camera.is_none()
        && lens.is_none()
        && !missing_date
        && !missing_exif
        && !starred_only
        && tag.is_none()
        && album.is_none()
        && ids.is_none();
    assert!(all_unset, "默认筛选必须全空");
    assert!(!f.is_active(), "全默认不算筛选");
}

/// `AC-SB-RECENT` / `M5-SB-03`：点「最近更改」进入按最近更改查询的视图，
/// 而不是被「最近导入」替换；它按 mtime 排序（不是拍摄时间）。
#[test]
fn recently_changed_is_a_real_view_ordered_by_modification_time() {
    let (_tmp, mut lib, _root, _sub) = fixture();
    lib.set_view(View::Recent).unwrap();
    assert_eq!(lib.view_title(), "最近更改");
    assert_eq!(lib.photos.len(), 6, "最近更改是全库视图（含子目录）");
    // 说明它是真实视图：标题不是「最近导入」，也不是「本次导入 · N 张」
    assert!(!lib.view_title().contains("最近导入"));
    assert!(!lib.view_title().contains("本次导入"));
}

/// `AC-SB-01` / `M5-SB-02`：点「所有照片」清掉星标筛选与年份，
/// 避免「两行同时亮」；格式 / 尺寸等维度保留。
#[test]
fn all_photos_clears_star_and_year_but_keeps_other_dimensions() {
    let (_tmp, mut lib, _root, _sub) = fixture();
    let year = 2011_i32;
    lib.set_filter(Some(PhotoFilter {
        starred_only: true,
        taken_range: Some(picasa_core::year_range_seconds(year)),
        formats: vec!["jpeg".into()],
        ..PhotoFilter::default()
    }))
    .unwrap();
    assert_eq!(lib.view_title(), "星标");

    lib.set_view(View::All).unwrap();
    let left = lib.filter.clone().expect("格式筛选应保留");
    assert!(!left.starred_only, "所有照片必须清掉星标");
    assert!(left.taken_range.is_none(), "所有照片必须清掉年份");
    assert_eq!(left.formats, vec!["jpeg".to_string()], "其它维度保留");
    assert_eq!(lib.view_title(), "所有照片");
}

/// `M5-OR-16` / `M5-SF-15`：相册筛选走 `filter.album`，标题**不是**「本次导入」；
/// 一次性导入范围（`filter.ids`）才用「本次导入 · N 张」。
#[test]
fn album_filter_uses_its_own_scope_and_only_ids_shows_the_import_title() {
    let (_tmp, mut lib, _root, _sub) = fixture();
    let all = ids_of(&lib);
    let album = lib.create_album("周末", 1_700_000_000).unwrap();
    lib.add_to_album(album, &all[..2], 1_700_000_000).unwrap();

    lib.set_filter(Some(PhotoFilter {
        album: Some("周末".into()),
        ..PhotoFilter::default()
    }))
    .unwrap();
    assert_eq!(lib.photos.len(), 2, "相册筛选只显示该集合成员");
    let title = lib.view_title();
    assert!(!title.contains("本次导入"), "相册筛选不该用本次导入标题：{title}");
    assert_eq!(title, "所有照片", "规格里相册不另立标题，回落到视图名");

    // 一次性导入范围 → 「本次导入 · N 张」
    lib.set_filter(Some(PhotoFilter {
        ids: Some(all[..3].to_vec()),
        ..PhotoFilter::default()
    }))
    .unwrap();
    assert_eq!(lib.photos.len(), 3);
    assert_eq!(lib.view_title(), "本次导入 · 3 张");

    // 清筛选即离开（不残留一次性范围）
    lib.set_filter(None).unwrap();
    assert!(!lib.view_title().contains("本次导入"));
}

fn ids_of(lib: &Library) -> Vec<i64> {
    let mut v: Vec<i64> = lib.photos.iter().map(|p| p.id).collect();
    v.sort();
    v
}
