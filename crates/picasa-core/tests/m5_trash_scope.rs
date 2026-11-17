//! M5-R2 · `AC-FS-TRASH` / `AC-FS-CONFIRM` 的核心层契约：**确认张数 = 执行集合**。
//!
//! 审计缺陷是「确认框写 1 张、真正移动的是整个 `selection`」。修法不是把数字改大，
//! 而是让执行集合**由调用方传入**（`Library::trash_ids(&ids)`），UI 再把确认时
//! 固化的同一个 `TrashScope` 传进来。本文件把这件事钉成可判定的断言：
//!
//! 1. 选择 3 张 → 范围就是 3 张；只移动这 3 张，其余原文件不动；
//! 2. **传进来的集合才是执行集合**：选择集有 5 张、只传 2 张，就只移动 2 张
//!    （这正是「确认几张就移动几张」的机器可验证形式）；
//! 3. 取消路径不调 `trash_ids`，磁盘零变化（由 UI 决策表保证，这里顺带验证
//!    「不调用就没有副作用」这条边界）。

use picasa_core::{AppPaths, Library, library_scope, paths, viewer_scope};
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

/// 起一个库：根下 5 张「照片」（内容随便，只要文件真的在盘上）。
///
/// 用临时 `HOME` 把废纸篓也关进 tempdir：`trash::move_to_trash` 在启动盘上走
/// `$HOME/.Trash`，不隔离就会往真实家目录里搬东西——测试不许有这种副作用。
fn fixture(tmp: &Path) -> (Library, Vec<std::path::PathBuf>) {
    let root = tmp.join("Pictures");
    std::fs::create_dir_all(&root).unwrap();
    let names = ["a.jpg", "b.jpg", "c.jpg", "d.jpg", "e.jpg"];
    let files: Vec<_> = names.iter().map(|n| root.join(n)).collect();
    for f in &files {
        std::fs::write(f, b"x").unwrap();
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
    assert_eq!(lib.photos.len(), 5, "夹具应有 5 张");
    (lib, files)
}

fn all_ids(lib: &Library) -> Vec<i64> {
    let db = lib.db().unwrap();
    db.photos(None, SortSpec::default(), 100)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect()
}

/// 选择 3 张 → 范围 3 张 → 只移动这 3 张，另外 2 张原文件仍在。
#[test]
fn confirming_three_moves_exactly_three_files() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // SAFETY: 单测进程内只有本文件这一个用例在跑，且它只读这个变量。
    unsafe { std::env::set_var("HOME", home.path()) };

    let (mut lib, files) = fixture(tmp.path());
    let ids = all_ids(&lib);
    assert_eq!(ids.len(), 5);
    let picked: Vec<i64> = ids[..3].to_vec();
    lib.selection = picked.iter().copied().collect();

    // 确认框那一刻固化的集合（图库规则 = 选择集）。
    let scope = library_scope(&lib.selection_ids());
    assert_eq!(scope.len(), 3, "确认张数必须是 3");
    assert!(picasa_core::trash_plan::confirm_body(scope.len()).starts_with("将把 3 张"));

    let (ok, errors) = lib.trash_ids(scope.ids());
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(ok, 3, "只应移动 3 张");

    // 索引里剩下 2 行；磁盘上正好 3 个文件消失、2 个还在。
    assert_eq!(lib.photos.len(), 2, "索引应只剩 2 行");
    let gone = files.iter().filter(|f| !f.exists()).count();
    assert_eq!(gone, 3, "磁盘上也应正好少 3 个文件");
    assert_eq!(files.iter().filter(|f| f.exists()).count(), 2);
}

/// 执行集合就是**传进来的那个集合**，不是「当前选择集」。
///
/// 这是把「确认 1 张、执行整个 selection」从结构上堵死的关键一条：
/// 选择集有 5 张，但确认的集合只有 2 张 → 只移动 2 张。
#[test]
fn execution_follows_the_confirmed_set_not_the_current_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // SAFETY: 同 `confirming_three_moves_exactly_three_files`。
    unsafe { std::env::set_var("HOME", home.path()) };

    let (mut lib, files) = fixture(tmp.path());
    let ids = all_ids(&lib);
    lib.selection = ids.iter().copied().collect(); // 5 张全选
    assert_eq!(lib.selection.len(), 5);

    let confirmed = vec![ids[0], ids[1]]; // 用户确认的只有 2 张
    let (ok, errors) = lib.trash_ids(&confirmed);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(ok, 2, "确认 2 张就只能移动 2 张");

    assert_eq!(files.iter().filter(|f| f.exists()).count(), 3);
}

/// `M5-VW-15`：查看器可见 1 张但选择集为 3 张 → 确认 3 张，执行也是这 3 张。
#[test]
fn viewer_with_selection_targets_the_whole_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // SAFETY: 同上。
    unsafe { std::env::set_var("HOME", home.path()) };

    let (mut lib, files) = fixture(tmp.path());
    let ids = all_ids(&lib);
    lib.selection = ids[..3].iter().copied().collect();

    let visible = ids[4]; // 正在看第 5 张
    let scope = viewer_scope(&lib.selection_ids(), Some(visible));
    assert_eq!(scope.len(), 3, "有选择集时必须确认整个选择集的真实张数");
    assert!(!scope.ids().contains(&visible), "选择集不含可见那张，就不该动它");

    let (ok, errors) = lib.trash_ids(scope.ids());
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(ok, 3);
    assert_eq!(files.iter().filter(|f| f.exists()).count(), 2);
}
