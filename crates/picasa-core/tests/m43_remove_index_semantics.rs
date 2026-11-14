//! M4.3 · IDX-10「移除根/子树索引」的完整语义契约测试。
//!
//! docs/18 §2:87（IDX-10 P0）明文：
//!   「移除根/子树索引（**停止监视 / 排除扫描并隐藏范围**、清派生缓存、
//!     保留身份骨架与全部标记）」
//!
//! 用户实机反馈「删除索引后，索引列表没有更新」正是只做了第三件事的结果：
//! `clear_index_for_root` 清干净了派生数据，但 `roots.state` 仍是 0（active），
//! 于是 ① 左栏树照旧列着 ② 管理面板照旧报「正常」③ 下次启动 `scan_roots()`
//! 又把它扫回来。本文件把这三条钉成可判定的断言。

use picasa_core::{AppPaths, Library, SnapshotInput, build_snapshot, default_offline_probe, paths};
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

/// 起一个库：A（2 张，带星标 + 标签）、B（1 张），返回 (lib, A 的 root_id)。
fn fixture(tmp: &Path) -> (Library, i64) {
    let root_a = tmp.join("A");
    let root_b = tmp.join("B");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    std::fs::write(root_a.join("a1.jpg"), b"x").unwrap();
    std::fs::write(root_b.join("b1.jpg"), b"x").unwrap();

    let mut lib = Library::open(isolated_paths(tmp)).unwrap();
    lib.add_root(&root_a);
    lib.add_root(&root_b);
    let scope_a = paths::canonical_string(&root_a);
    let scope_b = paths::canonical_string(&root_b);

    let root_a_id = {
        let db = lib.db().unwrap();
        let fid_a = db.upsert_folder(&scope_a, &scope_a, None).unwrap();
        let fid_b = db.upsert_folder(&scope_b, &scope_b, None).unwrap();
        db.apply_scan(fid_a, &scope_a, &[input("a1.jpg"), input("a2.jpg")], 0)
            .unwrap();
        db.apply_scan(fid_b, &scope_b, &[input("b1.jpg")], 0).unwrap();

        let photos: Vec<i64> = db
            .photos(None, SortSpec::default(), 100)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        db.set_starred(&photos[..1], true).unwrap();
        db.add_tag(&photos[..1], "旅行").unwrap();
        db.root_by_path(&scope_a).unwrap().unwrap().id
    };

    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();
    (lib, root_a_id)
}

fn snapshot_of(lib: &Library) -> picasa_core::MgmtSnapshot {
    let db = lib.db().unwrap();
    let probe = default_offline_probe();
    let input = SnapshotInput {
        seq: 1,
        live: &[],
        paused_by_system: false,
        offline: &probe,
        prev_dataset_version: 0,
        watch: &std::collections::HashMap::new(),
        prev_photo_count: 0,
    };
    build_snapshot(&db, &input).unwrap()
}

/// ① 只调 `clear_index_for_root` 时，库层如实落下「已移除」标记，
///    但**根仍是 active**——所以 `Library` 还把它当根列出（当前实现的行为）。
///
/// 这条用例的作用是把「库层只负责清数据 + 打标记」与「UI 层负责停止监视 /
/// 隐藏范围」的分工钉死，避免以后有人把两件事又揉回 `clear_index_for_root`。
#[test]
fn clearing_index_marks_removed_but_leaves_the_root_active() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, root_a_id) = fixture(tmp.path());

    {
        let db = lib.db().unwrap();
        let (photos, _) = db.clear_index_for_root(root_a_id).unwrap();
        assert_eq!(photos, 2, "A 的两张照片应当被清掉");
        assert!(
            db.root_index_removed(root_a_id).unwrap(),
            "删除索引后必须落下「已移除」标记，否则界面无从区分「删过」与「从没索引过」"
        );
        assert!(
            db.root(root_a_id).unwrap().unwrap().is_active(),
            "`clear_index_for_root` 只管清数据与打标记；关闭根是 UI / Library 的事"
        );
        assert!(
            db.index_removed_root_ids().unwrap().contains(&root_a_id),
            "快照要用的一次性查询必须能列出该根"
        );
    }

    // 面板：状态文案必须是「索引已移除」，而不是「正常」。
    let snap = snapshot_of(&lib);
    let row = snap.roots.iter().find(|r| r.id == root_a_id).unwrap();
    assert!(row.index_removed, "RootView 必须如实反映「已移除」");
    assert_eq!(row.status_text(), "索引已移除");
    assert_eq!(row.photo_count, 0);
}

/// ② `Library::remove_root` 的完整语义：停止监视 + 排除扫描 + 隐藏范围。
///
/// 这是 `delete_index_root` 走的路径（IDX-10）。三条都要断言，因为
/// 缺任何一条都会让用户看到「删了还在那儿」：
///  - `roots(false)` 不再返回 → 左栏树不列、监视配置不装；
///  - `scan_roots()` 为空 → 下次启动不会重新扫回来；
///  - 面板状态是「索引已移除」而不是「正常」。
#[test]
fn removing_a_root_hides_it_from_tree_scan_and_watch() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut lib, root_a_id) = fixture(tmp.path());
    let scope_a = {
        let db = lib.db().unwrap();
        db.root(root_a_id).unwrap().unwrap().path
    };

    lib.remove_root(Path::new(&scope_a));

    {
        let db = lib.db().unwrap();
        assert!(
            db.roots(false).unwrap().iter().all(|r| r.id != root_a_id),
            "移除后 `roots(false)` 不许再返回它——左栏树与监视配置都读这一份"
        );
        assert!(
            db.root_watch_configs()
                .unwrap()
                .iter()
                .all(|w| w.root_id != root_a_id),
            "监视配置必须随之停掉（IDX-10 的「停止监视」）"
        );
        assert!(
            db.root_index_removed(root_a_id).unwrap(),
            "`remove_root` 也要落下「已移除」标记"
        );
        // B 完全不受影响
        assert_eq!(db.total_photos().unwrap(), 1, "B 的那张必须还在");
    }

    assert!(
        !lib.root_paths().contains(&std::path::PathBuf::from(&scope_a)),
        "Library 的根列表里不许再有 A"
    );
    assert!(
        !lib.scan_roots().contains(&std::path::PathBuf::from(&scope_a)),
        "启动扫描的根集合必须为空——否则下次开应用又把它扫回来（用户看到的就是「没更新」）"
    );

    let snap = snapshot_of(&lib);
    let row = snap.roots.iter().find(|r| r.id == root_a_id).unwrap();
    assert!(
        row.index_removed && row.closed,
        "已移除的根在快照里应当同时是 closed（隐藏范围）与 index_removed（呈现）"
    );
    assert_eq!(row.status_text(), "索引已移除");
}

/// ③ 重新添加同路径 = 恢复：标记清掉、根回到 active、树里重新出现，
///    而**用户标记按身份自动挂回**（R-M43-02）。
#[test]
fn readding_the_same_path_restores_the_root_and_its_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut lib, root_a_id) = fixture(tmp.path());
    let scope_a = {
        let db = lib.db().unwrap();
        db.root(root_a_id).unwrap().unwrap().path
    };

    lib.remove_root(Path::new(&scope_a));
    // 模拟「重新添加同一路径」（UI 的 ReindexRoot / 添加文件夹都走 add_root）
    lib.add_root(Path::new(&scope_a));
    lib.reload_tree().unwrap();

    {
        let db = lib.db().unwrap();
        assert!(
            !db.root_index_removed(root_a_id).unwrap(),
            "重新添加必须清掉「已移除」标记（`upsert_root` 负责）"
        );
        let row = db.root(root_a_id).unwrap().unwrap();
        assert!(row.is_active(), "根必须回到 active，否则树里还是不显示");
        assert_eq!(row.id, root_a_id, "同路径必须复用 root id（排除项也一并复用）");

        // 重扫后标记挂回
        let fid = db.upsert_folder(&scope_a, &scope_a, None).unwrap();
        db.apply_scan(fid, &scope_a, &[input("a1.jpg"), input("a2.jpg")], 0)
            .unwrap();
        // `apply_scan` 只维护 direct_count；`root_stats` 按 `folders.root_id`
        // 求和，所以必须再跑一次层级推导把 root_id 挂上（真实路径里由
        // scanner 的 `refresh_hierarchy` 负责）。
        db.refresh_hierarchy().unwrap();
    }

    let snap = snapshot_of(&lib);
    let row = snap.roots.iter().find(|r| r.id == root_a_id).unwrap();
    assert!(!row.index_removed);
    assert_eq!(row.status_text(), "正常");
    assert_eq!(row.photo_count, 2);

    let db = lib.db().unwrap();
    assert_eq!(
        db.starred_ids().unwrap().len(),
        1,
        "星标必须按身份挂回，且只有原来那 1 张"
    );
    assert!(
        db.tag_counts_visible(None)
            .unwrap()
            .iter()
            .any(|(t, n)| t == "旅行" && *n == 1),
        "标签「旅行」必须挂回 1 张"
    );
}
