//! 关闭关注文件夹后，它的照片**必须从所有浏览入口消失**（回归用例）。
//!
//! ## 这条 bug 的形态
//!
//! 树按 `roots.state = 0` 只显示活动根，所以关掉一个根之后左栏立刻干净了；
//! 但照片查询走的是 `folder_ids: Option<&[i64]>`，`View::All` 传 `None`，
//! 而 DB 把 `None` 解释成「物理全库」——于是栅格、状态栏、搜索、
//! 「最近更改」全都还在显示已关闭根的照片。用户看到的正是
//! 「关了，但照片还在」。
//!
//! ## 为什么不能只修「所有照片」一处
//!
//! 四个入口各有自己的路径（`photos_from` / `count_photos` / `search` /
//! `recently_changed`），漏任何一条都是同一个症状换个地方出现。
//! 所以这里逐条断言。
//!
//! ## 与「离线根」的区别（不要改反了）
//!
//! `docs/06` P-17 裁决：「所有照片」**包含离线根**的照片——外接盘没插不代表
//! 用户不要那些照片。本用例只覆盖 `roots.state = 1`（用户主动关闭），
//! **不涉及** `folders.is_online`。两者混为一谈会把 P-17 改反。

use picasa_core::{AppPaths, CoreEvent, IndexBoard, IndexService, Library, paths};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

fn isolated_paths(root: &Path) -> AppPaths {
    AppPaths {
        support: root.join("Library/Application Support/app.openpicasa"),
        preferences: root.join("Library/Preferences/app.openpicasa"),
        caches: root.join("Library/Caches/app.openpicasa"),
        logs: root.join("Library/Logs/app.openpicasa"),
    }
}

fn touch_jpeg(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), b"not-a-real-jpeg").unwrap();
}

fn scan_blocking(lib: &Library, root: &Path) {
    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);
    service.enqueue_scan(&[root.to_path_buf()]);

    let key = paths::canonical_string(root);
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        let snap = board.snapshot();
        if !snap.running() {
            match snap.task_by_path(&key) {
                Some(t) => {
                    if t.state.is_terminal() {
                        return;
                    }
                }
                None => return,
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    panic!("扫描超时：{key}");
}

/// 两个根：`keep`（一直开着）+ `gone`（稍后关掉），各带一个子目录。
fn fixture() -> (tempfile::TempDir, Library, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let ps = isolated_paths(tmp.path());
    ps.ensure().unwrap();

    let keep = tmp.path().join("keep");
    let gone = tmp.path().join("gone");
    touch_jpeg(&keep.join("a"), "1.jpg");
    touch_jpeg(&keep.join("a"), "2.jpg");
    touch_jpeg(&gone.join("b"), "3.jpg");

    let mut lib = Library::open(ps).unwrap();
    lib.add_root(&keep);
    lib.add_root(&gone);
    scan_blocking(&lib, &keep);
    scan_blocking(&lib, &gone);
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();
    (tmp, lib, keep, gone)
}

#[test]
fn closing_a_root_hides_its_photos_from_every_entry() {
    let (_tmp, mut lib, keep, gone) = fixture();
    assert_eq!(lib.photos_total, 3, "两个根共 3 张（keep 2 + gone 1）");

    lib.close_root(&gone);
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();

    // ① 「所有照片」栅格：只剩 keep 的 2 张
    assert_eq!(lib.photos_total, 2, "关闭 gone 后「所有照片」应只剩 2 张");
    let paths_in_grid: Vec<String> = lib.photos.iter().map(|p| p.folder_path.clone()).collect();
    assert!(
        !paths_in_grid.iter().any(|p| p.contains("gone")),
        "栅格里仍有已关闭根的照片：{paths_in_grid:?}"
    );

    // ② 左栏「所有照片」旁的数字（total_in_db）：必须与栅格同口径
    assert_eq!(
        lib.total_in_db, 2,
        "左栏「所有照片」的数字与栅格不一致（那是 P0 级的四处不一致）"
    );

    // ③ 搜索：按已关闭目录的文件名搜，不能命中
    lib.set_search("3.jpg".into()).unwrap();
    assert!(
        lib.photos.is_empty(),
        "搜索把已关闭根的照片翻出来了：{:?}",
        lib.photos.iter().map(|p| p.name.clone()).collect::<Vec<_>>()
    );
    lib.set_search(String::new()).unwrap();

    // ④ 「最近更改」：同样不许出现
    lib.set_view(picasa_core::View::Recent).unwrap();
    let recent: Vec<String> = lib.photos.iter().map(|p| p.folder_path.clone()).collect();
    assert!(
        !recent.iter().any(|p| p.contains("gone")),
        "「最近更改」里仍有已关闭根的照片：{recent:?}"
    );
    lib.set_view(picasa_core::View::All).unwrap();

    // ⑤ 关闭**不删索引**：物理全库口径仍然是 3 行（这是 M2.1 NAV-001 的要求，
    //    与上面「看不见」并不矛盾——一个是用户视图，一个是库里的物理事实）
    assert_eq!(
        lib.db().unwrap().count_photos(None).unwrap(),
        3,
        "关闭根目录不能删索引（与「从图库移除」的区别）"
    );

    // ⑥ 重新打开 → 照片立刻回来（复用 root_id，不重扫）
    lib.add_root(&gone);
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();
    assert_eq!(lib.photos_total, 3, "重开 gone 后应立刻恢复 3 张");
    let _ = keep;
}

/// 关闭的是**子目录**（排除规则）时，`View::Folder` 不该再返回它的照片。
///
/// 走的是同一个「用户明确不要看」的语义，但落在那条 `p.folder_id IN (...)` 路径上，
/// 与根目录开关是两套机制——所以单独断言一次。
#[test]
fn excluded_subfolder_photos_disappear_from_the_parent_view() {
    let (_tmp, mut lib, keep, _gone) = fixture();
    let before = lib.photos_total;
    assert!(before >= 2);

    lib.exclude_folder(&keep.join("a")).unwrap();
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();

    assert_eq!(
        lib.photos_total, 1,
        "排除 keep/a 之后「所有照片」应只剩 gone 的 1 张，实际 {}",
        lib.photos_total
    );
    assert!(
        !lib.photos.iter().any(|p| p.folder_path.contains("/keep/a")),
        "被排除子目录的照片仍在栅格里"
    );
}
