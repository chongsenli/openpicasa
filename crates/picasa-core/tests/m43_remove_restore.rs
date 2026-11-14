//! M4.3 阶段 2 · R-M43-02「重建/删除不丢用户标记」端到端契约测试。
//!
//! docs/18 §2（IDX-10/11）+ §3.1 + §5（TC-M43-GUARD-01）：
//!   · `Library::remove_root` 只删根夹、不快照子树标记，会让整棵子树 folders+photos
//!     成孤儿且子目录标记静默丢失（G10.1）→ 改走 `Db::clear_index_for_root`；
//!   · `Db::forget_photo` 是裸 `DELETE FROM photos`，CASCADE 带走全部标记（G10.2）
//!     → 删前按身份快照；
//!   · 作用域删除必须**作废持久任务意图**而非删行，否则 `read_intent` 查不到会降级
//!     Run，旧 Job 把已移除范围重新扫回来（IDX-11 / G11.3）。刚入队、还没写任何
//!     folders 的任务同样要失效——这正是 `clear_index_for_root` 早退分支的漏洞。
//!
//! 全部走 DB API 直接搭数据（不启 scanner 线程），确定性、无竞态；标记恢复路径
//! `apply_scan_inner → identity::restore_photo` 是 `Db::apply_scan` 的直接调用，
//! 不需要 worker。

use picasa_core::{AppPaths, Library, paths};
use picasa_db::{Db, PhotoInput, SortSpec, TaskKind, TaskOutcome, TaskScopeKind, TaskStatus};
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

/// ① `remove_root` 必须：整棵子树（含子目录）连索引一起删、B 完全不动、
///    roots 行保留且 state=closed；重新加回并扫描后，**子目录**照片的全部标记
///    按身份挂回新 id（旧实现的 bug 正好在「只快照根夹」）。
#[test]
fn remove_root_deletes_whole_subtree_and_restores_all_markers_on_readd() {
    let tmp = tempfile::tempdir().unwrap();
    let root_a = tmp.path().join("A");
    let root_b = tmp.path().join("B");
    std::fs::create_dir_all(root_a.join("sub")).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    // 文件只作占位（本测试不启 scanner，靠 apply_scan 直接落库）
    std::fs::write(root_a.join("top.jpg"), b"x").unwrap();
    std::fs::write(root_a.join("sub").join("deep.jpg"), b"x").unwrap();
    std::fs::write(root_b.join("keep.jpg"), b"x").unwrap();

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&root_a);
    lib.add_root(&root_b);
    let scope_a = paths::canonical_string(&root_a);
    let scope_b = paths::canonical_string(&root_b);
    let sub_a = format!("{scope_a}/sub");

    // 直接搭索引：A 根夹 + A 子目录 + B 根夹，各一张照片。
    {
        let db = lib.db().unwrap();
        let fid_a = db.upsert_folder(&scope_a, &scope_a, None).unwrap();
        let fid_sub = db.upsert_folder(&sub_a, &sub_a, None).unwrap();
        let fid_b = db.upsert_folder(&scope_b, &scope_b, None).unwrap();
        db.apply_scan(fid_a, &scope_a, &[input("top.jpg")], 0).unwrap();
        db.apply_scan(fid_sub, &sub_a, &[input("deep.jpg")], 0).unwrap();
        db.apply_scan(fid_b, &scope_b, &[input("keep.jpg")], 0).unwrap();
        assert_eq!(db.count_photos(None).unwrap(), 3);

        // 给**子目录**的 deep.jpg 打满四类标记。旧实现只快照根夹，这张必丢。
        let rows = db.photos(None, SortSpec::default(), 10).unwrap();
        let deep = rows.iter().find(|r| r.name == "deep.jpg").unwrap().id;
        db.set_starred(&[deep], true).unwrap();
        db.add_tag(&[deep], "旅行").unwrap();
        db.set_caption(deep, "2020 年夏天").unwrap();
        let album = db.create_album("精选", 1).unwrap();
        db.add_to_album(album, &[deep], 1).unwrap();
    }

    lib.remove_root(&root_a);

    // A 的整棵子树（根夹 + 子目录）连照片一起没了，B 一根不动。
    {
        let db = lib.db().unwrap();
        assert_eq!(
            db.count_photos(None).unwrap(),
            1,
            "移除 A 后只剩 B 的 keep.jpg（子目录不能成孤儿）"
        );
        let folders = db.all_folders().unwrap();
        assert!(
            folders.iter().all(|f| !f.path.starts_with(&scope_a)),
            "A 的整棵子树 folders 必须删除：{folders:?}"
        );
        assert!(
            folders.iter().any(|f| f.path == scope_b),
            "B 的 folder 必须保留"
        );
        let roots = db.roots(true).unwrap();
        let ra = roots.iter().find(|r| r.path == scope_a).unwrap();
        assert!(!ra.is_active(), "roots 行保留且 state=closed，便于重加复用 id");
        assert!(
            !lib_root_paths_contains(&lib, &scope_a),
            "树里不应再显示已移除的 A"
        );
    }

    // 重新加回（`upsert_root` 复用 id、state 置回 active），模拟重新扫描：
    // 重新 upsert_folder 得到**新 id**，apply_scan 应把标记按身份挂回去。
    lib.add_root(&root_a);
    {
        let db = lib.db().unwrap();
        let fid_a2 = db.upsert_folder(&scope_a, &scope_a, None).unwrap();
        let fid_sub2 = db.upsert_folder(&sub_a, &sub_a, None).unwrap();
        db.apply_scan(fid_a2, &scope_a, &[input("top.jpg")], 0).unwrap();
        db.apply_scan(fid_sub2, &sub_a, &[input("deep.jpg")], 0).unwrap();
        assert_eq!(db.count_photos(None).unwrap(), 3);

        let rows = db.photos(None, SortSpec::default(), 10).unwrap();
        let deep2 = rows.iter().find(|r| r.name == "deep.jpg").unwrap();
        // id 必须变化（否则无法区分「按身份恢复」与「旧的没删」）。
        assert!(
            db.starred_ids().unwrap().contains(&deep2.id),
            "星标必须按身份挂回新 id"
        );
        assert!(
            db.tags_of(deep2.id).unwrap().iter().any(|t| t == "旅行"),
            "标签必须按身份挂回新 id"
        );
        assert_eq!(
            db.caption_of(deep2.id).unwrap(),
            "2020 年夏天",
            "说明必须按身份挂回新 id"
        );
        assert!(
            db.albums_of(deep2.id).unwrap().iter().any(|a| a.name == "精选"),
            "相册必须按身份挂回新 id"
        );
        // 标记不能串到同目录/根夹的其它照片。
        let top2 = rows.iter().find(|r| r.name == "top.jpg").unwrap();
        assert!(
            !db.starred_ids().unwrap().contains(&top2.id),
            "没打过标记的照片不能被误挂"
        );
        assert!(db.tags_of(top2.id).unwrap().is_empty());
    }
}

/// ② `Db::forget_photo` 删前必须快照标记（G10.2）：删掉再重新 apply_scan 同一
///    身份的照片，标记必须回来。旧实现是裸 DELETE，标记会被 CASCADE 静默带走。
#[test]
fn forget_photo_snapshots_markers_before_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let scope = "/x/photos".to_string();
    let fid = db.upsert_folder(&scope, &scope, None).unwrap();
    db.apply_scan(fid, &scope, &[input("a.jpg")], 0).unwrap();
    let id = db.photos(None, SortSpec::default(), 10).unwrap()[0].id;

    db.set_starred(&[id], true).unwrap();
    db.add_tag(&[id], "旅行").unwrap();
    db.set_caption(id, "海边").unwrap();

    db.forget_photo(id).unwrap();
    assert_eq!(db.count_photos(None).unwrap(), 0, "照片行应已删除");

    // 重新扫描同一身份（新 id）——标记应挂回。
    db.apply_scan(fid, &scope, &[input("a.jpg")], 0).unwrap();
    let rows = db.photos(None, SortSpec::default(), 10).unwrap();
    let new_id = rows[0].id;
    assert!(
        db.starred_ids().unwrap().contains(&new_id),
        "forget_photo 后星标必须按身份恢复"
    );
    assert!(db.tags_of(new_id).unwrap().iter().any(|t| t == "旅行"));
    assert_eq!(db.caption_of(new_id).unwrap(), "海边");
}

/// ③ IDX-11 / G11.3：作用域删除必须作废该 scope 的持久任务意图，且**即使还没有
///    任何已建索引的 folder**（刚入队、还没写 folders），也不能让旧任务复活。
///    这正是 `clear_index_for_root` 原来 `folder_ids.is_empty()` 早退分支的漏洞。
#[test]
fn clear_index_for_root_invalidates_pending_task_even_without_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let scope = "/media/A".to_string();
    let root_id = db.upsert_root(&scope, 1, false).unwrap();

    // 一条「刚入队、还没扫」的活任务行。
    let id = db
        .begin_index_task(Some(root_id), &scope, TaskKind::FirstScan, TaskScopeKind::Root)
        .unwrap();
    assert!(
        db.resumable_index_tasks().unwrap().iter().any(|t| t.task_id == id),
        "前置条件：任务处于可续跑状态"
    );

    // 没有任何 folders：走 `folder_ids.is_empty()` 早退，但意图仍必须被作废。
    assert_eq!(db.clear_index_for_root(root_id).unwrap(), (0, 0));

    let t = db.task_intent(id).unwrap().unwrap();
    assert_eq!(
        t.cancel_reason.as_deref(),
        Some("索引已移除"),
        "作用域删除必须落 Cancelled 终态，而不是删行让 read_intent 降级 Run"
    );
    assert_eq!(t.outcome, Some(TaskOutcome::Cancelled.code()));
    assert_eq!(t.status, TaskStatus::Completed.code());
    assert!(t.generation >= 1, "作废必须抬升 generation（IDX-11）");
    assert!(
        db.unfinished_index_tasks().unwrap().iter().all(|r| r.task_id != id),
        "已作废任务不得留在未完成/可续跑队列里（重启不复活）"
    );
    // roots 行仍在（作用域删除不删根）。
    assert!(db.root(root_id).unwrap().is_some());
}

/// 小助手：`root_paths()` 里是否还含某路径（不能在这里持有 db 锁再调 `remove_root`）。
fn lib_root_paths_contains(lib: &Library, scope: &str) -> bool {
    lib.root_paths()
        .iter()
        .any(|p| p.to_string_lossy() == scope)
}
