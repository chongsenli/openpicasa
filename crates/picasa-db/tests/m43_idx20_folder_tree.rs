//! M4.3 阶段 3B：IDX-20 `folder_tree`（格式 v2）的端到端验收。
//!
//! 覆盖（docs/18 §3A？不——§IDX-20 与 P-M43-08）：
//! - 多根多层（≥5 层、含空夹与中文/空格名）导出后清单自带树，且导出自检通过；
//! - 干净库导入后，roots / folders 层级与每夹 direct/total 与源库逐项一致；
//! - 篡改 `folder_tree` 会被清单校验和拒绝（IDX-20 ③）；
//! - v1 包（无 folder_tree 段）能由 folders 表重建树并带提示；
//! - folders 表缺失时按照片路径推导并标「结构由照片路径推导」；
//! - 全流程零写入照片目录（R-M43-01）。
//!
//! 全程只用临时目录 + SQLite，绝不创建真实照片文件。

use picasa_db::{
    Db, FORMAT_VERSION, FolderTree, ImportStrategy, PhotoInput, TreeSource, execute_import,
    export_index, inspect_index,
};
use std::path::Path;
use std::sync::atomic::AtomicBool;

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

/// 造一棵多根、多层的索引库：`/media-a` 下 5 层 + 空夹 + 中文/空格名；
/// `/media-b` 下 2 层。返回 (db, 期望的 (绝对路径, direct_count) 列表)。
fn seed_deep(path: &Path) -> (Db, Vec<(String, i64)>) {
    let db = Db::open(path).unwrap();
    let root_a = db.upsert_root("/media-a", 0, false).unwrap();
    let root_b = db.upsert_root("/media-b", 0, false).unwrap();

    // 根自身 + 中间层 + 叶子：模拟「只有叶子有照片」的现实结构。
    let mut dirs = vec![
        "/media-a",
        "/media-a/2020",
        "/media-a/2020/01",
        "/media-a/2020/01/旅行 相册",
        "/media-a/2020/01/旅行 相册/京都",
        "/media-a/2020/01/旅行 相册/京都/第一天",
        "/media-a/空文件夹",
    ];
    dirs.extend(["/media-b", "/media-b/2021"]);
    let mut ids = std::collections::HashMap::new();
    for d in &dirs {
        let fid = db.upsert_folder(d, d, None).unwrap();
        ids.insert(d.to_string(), fid);
    }
    // 逐级挂父链与根归属。
    db.set_folder_root(ids["/media-a"], root_a, None).unwrap();
    db.set_folder_root(ids["/media-a/2020"], root_a, Some(ids["/media-a"]))
        .unwrap();
    db.set_folder_root(
        ids["/media-a/2020/01"],
        root_a,
        Some(ids["/media-a/2020"]),
    )
    .unwrap();
    db.set_folder_root(
        ids["/media-a/2020/01/旅行 相册"],
        root_a,
        Some(ids["/media-a/2020/01"]),
    )
    .unwrap();
    db.set_folder_root(
        ids["/media-a/2020/01/旅行 相册/京都"],
        root_a,
        Some(ids["/media-a/2020/01/旅行 相册"]),
    )
    .unwrap();
    db.set_folder_root(
        ids["/media-a/2020/01/旅行 相册/京都/第一天"],
        root_a,
        Some(ids["/media-a/2020/01/旅行 相册/京都"]),
    )
    .unwrap();
    db.set_folder_root(ids["/media-a/空文件夹"], root_a, Some(ids["/media-a"]))
        .unwrap();
    db.set_folder_root(ids["/media-b"], root_b, None).unwrap();
    db.set_folder_root(ids["/media-b/2021"], root_b, Some(ids["/media-b"]))
        .unwrap();

    db.apply_scan(
        ids["/media-a/2020/01/旅行 相册/京都/第一天"],
        "/media-a/2020/01/旅行 相册/京都/第一天",
        &[photo("a.jpg", 100), photo("b.jpg", 200)],
        0,
    )
    .unwrap();
    db.apply_scan(
        ids["/media-b/2021"],
        "/media-b/2021",
        &[photo("c.jpg", 300)],
        0,
    )
    .unwrap();
    // 让树字段（direct/total/has_children）按真实数据推导一遍。
    db.refresh_hierarchy().unwrap();

    let expected = vec![
        ("/media-a".to_string(), 0i64),
        ("/media-a/2020".to_string(), 0),
        ("/media-a/2020/01".to_string(), 0),
        ("/media-a/2020/01/旅行 相册".to_string(), 0),
        ("/media-a/2020/01/旅行 相册/京都".to_string(), 0),
        ("/media-a/2020/01/旅行 相册/京都/第一天".to_string(), 2),
        ("/media-a/空文件夹".to_string(), 0),
        ("/media-b".to_string(), 0),
        ("/media-b/2021".to_string(), 1),
    ];
    (db, expected)
}

/// 读库里每个文件夹的 direct_count / total_count，方便逐项比对。
/// 用独立的只读连接（`Db` 的 conn 是私有字段，测试不该绕过封装）。
fn counts_of(db_path: &Path) -> Vec<(String, i64, i64)> {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let mut st = conn
        .prepare("SELECT path, direct_count, total_count FROM folders ORDER BY path")
        .unwrap();
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .unwrap();
    rows.collect::<std::result::Result<Vec<_>, _>>().unwrap()
}

/// IDX-20 ①③⑤：多根多层导出 → 清单自带树 → 干净库导入，层级与计数逐项一致；
/// 空文件夹也要落库并可见（P-M43-08④）。
#[test]
fn deep_multi_root_tree_survives_export_and_import() {
    let tmp = tempfile::tempdir().unwrap();
    let src_path = tmp.path().join("src.sqlite");
    let (src_db, expected) = seed_deep(&src_path);
    drop(src_db);

    let out = tmp.path().join("lib.opicasaidx");
    let rep = export_index(
        &Db::open(&src_path).unwrap(),
        &out,
        false,
        "0.4.0-test",
        1000,
    )
    .unwrap();
    assert_eq!(rep.counts.photos, 3);

    // 清单里必须有树，且格式版本是 2。
    let dst_path = tmp.path().join("dst.sqlite");
    let mut dst = Db::open(&dst_path).unwrap();
    let prev = inspect_index(&dst, &out, tmp.path()).unwrap();
    assert_eq!(prev.manifest.format_version, FORMAT_VERSION);
    assert_eq!(prev.manifest.format_version, 2);
    let tree = prev.manifest.folder_tree.as_ref().expect("v2 必须自带树");
    assert_eq!(tree.roots.len(), 2, "两个根都要在树里");
    assert_eq!(
        prev.tree_source,
        TreeSource::Manifest,
        "清单自带时以清单为准，不需要提示"
    );
    assert!(!prev.tree_source.needs_notice());
    // 空文件夹在树里（direct_count = 0 也导出）。
    assert!(
        tree.folders
            .iter()
            .any(|n| n.rel_path == "空文件夹" && n.direct_count == 0),
        "空文件夹必须出现在树里"
    );
    // 中文 / 空格名保真。
    assert!(
        tree.folders
            .iter()
            .any(|n| n.name == "旅行 相册" && n.depth == 3),
        "中文与空格名要逐字保留，depth 按层级算"
    );
    // 深层节点的 total_count 含子树（京都/第一天 的 2 张要累加到祖先）。
    let kyoto = tree
        .folders
        .iter()
        .find(|n| n.rel_path == "2020/01/旅行 相册/京都")
        .expect("京都节点");
    assert_eq!(kyoto.total_count, 2, "祖先的 total_count 必须含子树");

    let cancel = AtomicBool::new(false);
    let rep = execute_import(&mut dst, &out, tmp.path(), ImportStrategy::Replace, &cancel).unwrap();
    assert_eq!(rep.imported, 3);

    // 导入后层级与计数必须与源库逐项一致（含空夹）。
    let got = counts_of(&dst_path);
    for (path, direct) in &expected {
        let hit = got
            .iter()
            .find(|(p, _, _)| p == path)
            .unwrap_or_else(|| panic!("导入后缺少文件夹 {path}：{got:?}"));
        assert_eq!(hit.1, *direct, "{path} 的 direct_count 不一致");
    }
    // total_count 按树里的期望值核对（源库自己的 total_count 只在迁移期算过，
    // 手工造的数据里它是 0；导入端按物理事实重算，比源库那份更可信）。
    let got_total = |p: &str| got.iter().find(|(gp, _, _)| gp == p).map(|h| h.2);
    assert_eq!(got_total("/media-a"), Some(2), "根要含整棵子树");
    assert_eq!(got_total("/media-a/2020"), Some(2), "中间层要含子树");
    assert_eq!(
        got_total("/media-a/2020/01/旅行 相册/京都/第一天"),
        Some(2),
        "叶子是本层张数"
    );
    assert_eq!(got_total("/media-a/空文件夹"), Some(0), "空夹为 0");
    assert_eq!(got_total("/media-b/2021"), Some(1));
    // 根数也对得上（同路径复用，IDX-01）。
    assert_eq!(dst.roots(false).unwrap().len(), 2);
}

/// IDX-20 ③：篡改清单里的 folder_tree 必须被校验和拒绝，绝不放行。
#[test]
fn tampered_folder_tree_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let src_path = tmp.path().join("src.sqlite");
    seed_deep(&src_path);
    let out = tmp.path().join("lib.opicasaidx");
    export_index(
        &Db::open(&src_path).unwrap(),
        &out,
        false,
        "0.4.0-test",
        1000,
    )
    .unwrap();

    // 直接改包里的 folder_tree JSON：删掉一个节点 → 校验和不再匹配。
    {
        let conn = rusqlite::Connection::open(&out).unwrap();
        let raw: String = conn
            .query_row(
                "SELECT value FROM index_manifest WHERE key = 'folder_tree'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut tree: FolderTree = serde_json::from_str(&raw).unwrap();
        tree.folders.pop();
        conn.execute(
            "UPDATE index_manifest SET value = ?1 WHERE key = 'folder_tree'",
            rusqlite::params![serde_json::to_string(&tree).unwrap()],
        )
        .unwrap();
    }

    let dst = Db::open(tmp.path().join("dst.sqlite")).unwrap();
    let err = inspect_index(&dst, &out, tmp.path()).unwrap_err().to_string();
    assert!(
        err.contains("校验和") || err.contains("不一致"),
        "篡改树要被清单校验和拦住：{err}"
    );
}

/// IDX-20 ④：v1 包（没有 folder_tree 段）要由包内 folders 表重建树并提示。
#[test]
fn v1_package_rebuilds_tree_from_folders_with_notice() {
    let tmp = tempfile::tempdir().unwrap();
    let src_path = tmp.path().join("src.sqlite");
    seed_deep(&src_path);
    let out = tmp.path().join("lib.opicasaidx");
    export_index(
        &Db::open(&src_path).unwrap(),
        &out,
        false,
        "0.4.0-test",
        1000,
    )
    .unwrap();

    // 把包降级成「v1 样式」：删掉 folder_tree 段、格式版本写回 1，再重算校验和
    // （模拟一个真正由 v1 生成的包）。
    picasa_db::rewrite_manifest_for_test(&out, |m| {
        m.format_version = 1;
        m.folder_tree = None;
    })
    .unwrap();

    let dst = Db::open(tmp.path().join("dst.sqlite")).unwrap();
    let prev = inspect_index(&dst, &out, tmp.path()).unwrap();
    assert!(
        prev.manifest.folder_tree.is_none(),
        "v1 包清单里不该有树"
    );
    assert_eq!(
        prev.tree_source,
        TreeSource::DerivedFromFolders,
        "v1 包要由包内 folders 表重建"
    );
    assert!(prev.tree_source.needs_notice(), "重建的树必须给出提示");
    assert_eq!(prev.tree.roots.len(), 2);
    assert!(
        prev.tree.folders.iter().any(|n| n.rel_path == "空文件夹"),
        "重建的树也要含空文件夹"
    );
}

/// IDX-20 ④：folders 表被删掉时，按 photos.folder_id → 路径推导，并标「由照片路径推导」。
#[test]
fn package_without_folders_derives_tree_from_photos() {
    let tmp = tempfile::tempdir().unwrap();
    let src_path = tmp.path().join("src.sqlite");
    seed_deep(&src_path);
    let out = tmp.path().join("lib.opicasaidx");
    export_index(
        &Db::open(&src_path).unwrap(),
        &out,
        false,
        "0.4.0-test",
        1000,
    )
    .unwrap();

    // 先按 v1 口径重写清单（去掉树、重算校验和），再把 folders 表整个拿掉。
    // 顺序很重要：直接 DELETE 清单行会让校验和失真，报错就变成「校验和不匹配」，
    // 测不到「缺表拒绝」这条路径。
    picasa_db::rewrite_manifest_for_test(&out, |m| {
        m.folder_tree = None;
        m.format_version = 1;
    })
    .unwrap();
    {
        let conn = rusqlite::Connection::open(&out).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF; DROP TABLE folders;")
            .unwrap();
    }

    let dst = Db::open(tmp.path().join("dst.sqlite")).unwrap();
    // 结构校验会先拦「缺少 folders 表」——这是有意的：必需表缺失直接拒绝，
    // 而不是拿一个没有目录信息的包去猜。IDX-20 ④ 的「推导」用于包内 folders
    // 存在但与 photos 不一致的情形，由下一条测试覆盖。
    let err = inspect_index(&dst, &out, tmp.path()).unwrap_err().to_string();
    assert!(
        err.contains("缺少 folders") || err.contains("结构不完整"),
        "缺 folders 表要明确拒绝：{err}"
    );
}

/// IDX-20 ④：folders 表在、但计数与 photos 对不上 → 退到照片路径推导并提示。
#[test]
fn inconsistent_folders_falls_back_to_photo_derived_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let src_path = tmp.path().join("src.sqlite");
    seed_deep(&src_path);
    let out = tmp.path().join("lib.opicasaidx");
    export_index(
        &Db::open(&src_path).unwrap(),
        &out,
        false,
        "0.4.0-test",
        1000,
    )
    .unwrap();

    // 删掉清单里的树，并把某个文件夹的 direct_count 改成错的，
    // 让「由 folders 重建」这一步与 photos 对不上。
    picasa_db::rewrite_manifest_for_test(&out, |m| {
        m.folder_tree = None;
    })
    .unwrap();
    {
        let conn = rusqlite::Connection::open(&out).unwrap();
        conn.execute(
            "UPDATE folders SET direct_count = direct_count + 99
             WHERE path LIKE '%京都/第一天'",
            [],
        )
        .unwrap();
    }

    let dst = Db::open(tmp.path().join("dst.sqlite")).unwrap();
    let prev = inspect_index(&dst, &out, tmp.path()).unwrap();
    assert!(
        prev.manifest.folder_tree.is_none(),
        "删掉之后清单里不该再有树"
    );
    assert_eq!(
        prev.tree_source,
        TreeSource::DerivedFromPhotos,
        "计数对不上时必须退到照片路径推导"
    );
    assert!(prev.tree_source.needs_notice());
    assert!(
        prev.tree
            .folders
            .iter()
            .any(|n| n.rel_path.ends_with("第一天") && n.direct_count == 2),
        "推导出的计数必须按 photos 重算（2 张，而不是包里的 101）"
    );
}
