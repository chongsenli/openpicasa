//! M4.3 `IDX-22`：左栏标签区的排序、可见性、计数口径与视图标题。
//!
//! 规格（docs/18 §IDX-22，P-M43-11 已裁决）：
//! - ④ 默认**按名称**（不区分大小写）；可切「按数量」= 降序、同数按名称；选择持久化。
//! - ⑥ **0 计数默认隐藏**，可切「显示空标签」。
//! - ③ 计数是**全库口径**：含离线根与导入索引的照片，按 `photo_id` 去重只算一份，
//!   不含已从索引移除的范围。
//! - ⑤ 点标签 → 网格显示该标签的全部照片，网格数 = 左栏计数；标题「标签：X · N 张」。
//!
//! 排序 / 过滤本身是 `picasa_core::tag_counts` 的纯函数（那里有单测），
//! 这里验的是它与 `Library` / `Db` 接通后的**口径**——尤其是「同一张照片挂了
//! 两次标签只算一次」这种只有真库才能证伪的事。

use picasa_core::{AppPaths, Library, TagsSort, View};
use picasa_db::{PhotoFilter, PhotoInput};
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

/// 两根：`/a` 2 张、`/b` 2 张；`/b` 的第二根被「已关闭」用来验离线口径。
fn fixture() -> (tempfile::TempDir, Library) {
    let tmp = tempfile::tempdir().unwrap();
    let ps = isolated_paths(tmp.path());
    let mut lib = Library::open(ps).unwrap();
    {
        let db = lib.db().unwrap();
        // 每个根**只扫一次**：`apply_scan` 会把本次没出现的行当 stale 删掉，
        // 分成两次扫同一目录会把前一张照片删掉（造数据时踩过）。
        for (root, items) in [
            ("/a", vec![photo("a1.jpg", 100), photo("a2.jpg", 200)]),
            ("/b", vec![photo("b1.jpg", 300), photo("b2.jpg", 400)]),
        ] {
            let rid = db.upsert_root(root, 0, false).unwrap();
            let fid = db.upsert_folder(root, root, None).unwrap();
            db.set_folder_root(fid, rid, None).unwrap();
            db.apply_scan(fid, root, &items, 0).unwrap();
        }
        let rows = db
            .photos_from_visible(None, picasa_db::SortSpec::default(), 20, 0)
            .unwrap();
        let id = |name: &str| rows.iter().find(|p| p.name == name).unwrap().id;
        // 「旅行」3 张、「京都」1 张、「空的」0 张（一条也没挂）。
        db.add_tag(&[id("a1.jpg"), id("b1.jpg"), id("b2.jpg")], "旅行")
            .unwrap();
        db.add_tag(&[id("a2.jpg")], "京都").unwrap();
        // 同一张照片挂两次同名标签：`tags` 的 PK 是 (photo_id,label)，
        // 但 `apply_scan` 重扫后库里可能残留重复行——这里显式验一次去重口径。
        db.add_tag(&[id("a1.jpg")], "旅行").unwrap();
        // P-M43-11：**离线根**的照片仍要算进全库口径。
        // 注意用「离线」（`foleders.is_online=0`，外置盘没挂载）而不是「已关闭」
        // ——「已关闭」是用户主动把根收起来，它本来就不该出现在网格里，
        // 用它验口径会把「计数含离线根」和「网格不含已关闭根」混成一件事。
        let b_fid = db.folder_id("/b").unwrap().unwrap();
        db.set_folder_online(b_fid, false).unwrap();
    }
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();
    (tmp, lib)
}

fn map(lib: &Library) -> Vec<(String, i64)> {
    lib.sidebar_tags().unwrap()
}

#[test]
fn default_order_is_by_name_and_zero_counts_hidden() {
    let (_tmp, lib) = fixture();
    assert_eq!(lib.tags_sort(), TagsSort::Name, "IDX-22 ④ 默认按名称");
    assert!(!lib.show_zero_tags(), "IDX-22 ⑥ 默认隐藏空标签");

    let tags = map(&lib);
    let names: Vec<&str> = tags.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(names, vec!["京都", "旅行"], "按名称排序，0 计数不出现");
    // ③ 全库口径：/b 已被关闭（离线根），它的 2 张仍计入「旅行」。
    assert_eq!(
        tags.iter().find(|(l, _)| l == "旅行").map(|(_, n)| *n),
        Some(3),
        "离线/已关闭根的照片仍计入全库口径（P-M43-11）"
    );
}

#[test]
fn count_sort_and_show_zero_toggle_round_trip_through_prefs() {
    let (tmp, mut lib) = fixture();
    lib.set_tags_sort(TagsSort::Count);
    assert_eq!(lib.tags_sort(), TagsSort::Count);
    let tags = map(&lib);
    assert_eq!(
        tags.iter().map(|(l, n)| (l.as_str(), *n)).collect::<Vec<_>>(),
        vec![("旅行", 3), ("京都", 1)],
        "按数量降序"
    );

    assert!(lib.toggle_show_zero_tags(), "切换后为「显示」");
    lib.save_prefs();

    // 重新打开同一容器：排序与可见性都要还在（IDX-22 ④ 选择持久化）。
    let reopened = Library::open(isolated_paths(tmp.path())).unwrap();
    assert_eq!(reopened.tags_sort(), TagsSort::Count, "排序选择重启保留");
    assert!(reopened.show_zero_tags(), "显示空标签的选择重启保留");
    assert_eq!(reopened.sidebar_tags().unwrap().len(), 2);
}

#[test]
fn clicking_a_tag_switches_to_library_wide_and_titles_the_scope() {
    let (_tmp, mut lib) = fixture();
    // 先停在某个文件夹视图里，模拟「在文件夹里点标签」。
    // /b 在本 fixture 里被标成「已关闭」，不会出现在可见树里，所以拿 /a。
    let folder = lib
        .roots
        .iter()
        .find(|n| n.path == "/a")
        .and_then(|n| n.folder_id)
        .expect("/a 必须是可见根");
    lib.set_view(View::Folder(folder)).unwrap();

    // app 侧点标签做的事：切回全库 + 叠加 tag。
    lib.set_view(View::All).unwrap();
    let mut f: PhotoFilter = lib.filter.clone().unwrap_or_default();
    f.tag = Some("旅行".into());
    lib.set_filter(Some(f)).unwrap();
    lib.reload_photos().unwrap();

    assert_eq!(lib.view, View::All, "⑤ 点标签必须切回全库视图");
    let left_count = lib
        .sidebar_tags()
        .unwrap()
        .into_iter()
        .find(|(l, _)| l == "旅行")
        .unwrap()
        .1;
    assert_eq!(
        lib.photos_total as i64, left_count,
        "⑤ 网格张数必须等于左栏计数（{left_count}）"
    );
    assert_eq!(
        lib.view_title(),
        format!("标签：旅行 · {left_count} 张"),
        "⑤ 标题必须自报标签范围"
    );

    // 再点一次 = 取消筛选。
    let mut f: PhotoFilter = lib.filter.clone().unwrap_or_default();
    f.tag = None;
    lib.set_filter(Some(f).filter(|f| f.is_active())).unwrap();
    assert_eq!(lib.view_title(), "所有照片");
}

#[test]
fn tag_title_wins_over_starred_but_not_over_import_scope() {
    let (_tmp, mut lib) = fixture();
    // 标签 + 星标叠加：标题先报标签（更具体的用户动作）。
    let mut f: PhotoFilter = lib.filter.clone().unwrap_or_default();
    f.tag = Some("京都".into());
    f.starred_only = true;
    lib.set_filter(Some(f)).unwrap();
    lib.reload_photos().unwrap();
    assert!(
        lib.view_title().starts_with("标签：京都"),
        "标签叠加星标时标题报标签，实际得到：{}",
        lib.view_title()
    );

    // IDX-19 的临时范围更窄，优先于标签。
    let ids = lib.photos.iter().take(1).map(|p| p.id).collect::<Vec<_>>();
    let mut f: PhotoFilter = lib.filter.clone().unwrap_or_default();
    f.ids = Some(ids);
    lib.set_filter(Some(f)).unwrap();
    assert!(
        lib.view_title().starts_with("本次导入"),
        "一次性范围优先于标签，实际得到：{}",
        lib.view_title()
    );
}

#[test]
fn clearing_a_hidden_root_drops_its_tag_counts() {
    let (_tmp, mut lib) = fixture();
    {
        let db = lib.db().unwrap();
        let root = db.root_by_path("/b").unwrap().unwrap();
        // IDX-21 的移除路径：整子树快照 + 作用域删（保标记），计数应当少掉 /b 的 2 张。
        db.clear_index_for_root(root.id).unwrap();
    }
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();
    let tags = map(&lib);
    assert_eq!(
        tags.iter().map(|(l, n)| (l.as_str(), *n)).collect::<Vec<_>>(),
        vec![("京都", 1), ("旅行", 1)],
        "③ 已从索引移除的范围不计入左栏计数"
    );
}

#[test]
fn view_scope_tag_counts_follow_the_grid() {
    // IDX-22 P1「本视图 N」：第二数字必须与栅格同口径——只在当前视图的
    // 文件夹子树内、且 root 未关闭的照片。这条只有真库能证伪（`active_root_clause`
    // 与 `folder_id IN` 必须同时生效）。
    let tmp = tempfile::tempdir().unwrap();
    let (mut lib, a_fid) = {
        let ps = isolated_paths(tmp.path());
        let mut lib = Library::open(ps).unwrap();
        let a_fid;
        {
            let db = lib.db().unwrap();
            for (root, items) in [
                ("/a", vec![photo("a1.jpg", 100), photo("a2.jpg", 200)]),
                ("/b", vec![photo("b1.jpg", 300)]),
            ] {
                let rid = db.upsert_root(root, 0, false).unwrap();
                let fid = db.upsert_folder(root, root, None).unwrap();
                db.set_folder_root(fid, rid, None).unwrap();
                db.apply_scan(fid, root, &items, 0).unwrap();
            }
            let rows = db.photos(None, picasa_db::SortSpec::default(), 10).unwrap();
            let id = |n: &str| rows.iter().find(|r| r.name == n).unwrap().id;
            // 三张都挂「旅行」；只有 /a 的两张挂「京都」。
            db.add_tag(&[id("a1.jpg"), id("a2.jpg"), id("b1.jpg")], "旅行")
                .unwrap();
            db.add_tag(&[id("a1.jpg"), id("a2.jpg")], "京都").unwrap();
            a_fid = db.folder_id("/a").unwrap().unwrap();
        }
        lib.reload_tree().unwrap();
        lib.reload_photos().unwrap();
        (lib, a_fid)
    };

    // 全库口径：旅行 3、京都 2。
    let all = lib.all_tags().unwrap();
    let count_of = |rows: &[(String, i64)], label: &str| {
        rows.iter().find(|(l, _)| l == label).map(|(_, n)| *n)
    };
    assert_eq!(count_of(&all, "旅行"), Some(3));
    assert_eq!(count_of(&all, "京都"), Some(2));

    // 切到 /a：本视图口径「旅行 2、京都 2」——b1 不在范围里。
    lib.set_view(View::Folder(a_fid)).unwrap();
    let in_view = lib.view_tag_counts().unwrap();
    assert_eq!(
        count_of(&in_view, "旅行"),
        Some(2),
        "本视图计数必须只数视图范围内的照片：{in_view:?}"
    );
    assert_eq!(count_of(&in_view, "京都"), Some(2));
    assert_eq!(
        lib.photos_total, 2,
        "网格张数与本视图计数同口径（IDX-22 ⑤ 的「网格数 = 左栏数字」）"
    );

    // 全库视图下两者一致（UI 因此不画第二数字）。
    lib.set_view(View::All).unwrap();
    let back = lib.view_tag_counts().unwrap();
    assert_eq!(count_of(&back, "旅行"), Some(3));

    // 关闭 /a 的根之后，本视图口径里它整支都不算——与栅格一致。
    {
        let db = lib.db().unwrap();
        let root_id = db.roots(false).unwrap().iter().find(|r| r.path == "/a").unwrap().id;
        db.set_root_closed(root_id, true).unwrap();
    }
    lib.set_view(View::Folder(a_fid)).unwrap();
    let closed = lib.view_tag_counts().unwrap();
    assert_eq!(
        count_of(&closed, "旅行"),
        None,
        "已关闭根的照片不该再出现在本视图计数里：{closed:?}"
    );
}
