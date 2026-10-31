//! M2.1 迁移的**应用侧**验收：`Preferences.roots` → `roots` 表 → 左栏树。
//!
//! 这条链路是「单一真相源」的可判定证据：M2 的库里根目录只存在于 `preferences.json`，
//! 升级后必须出现在 `roots` 表里，而**树要读表不读文件**。

use picasa_core::{AppPaths, Library, paths};
use std::path::Path;

fn isolated_paths(root: &Path) -> AppPaths {
    AppPaths {
        support: root.join("Library/Application Support/app.openpicasa"),
        preferences: root.join("Library/Preferences/app.openpicasa"),
        caches: root.join("Library/Caches/app.openpicasa"),
        logs: root.join("Library/Logs/app.openpicasa"),
    }
}

/// 造一个 M2（schema v1）的库：两个目录，其中子目录的中间层缺失。
fn make_m2_db(db_path: &Path, root_path: &str) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
    )
    .unwrap();
    conn.execute_batch(picasa_db::MIGRATIONS[0].sql).unwrap();
    conn.execute("INSERT INTO schema_migrations VALUES (1, 0)", [])
        .unwrap();
    let sub = format!("{root_path}/2011/05");
    conn.execute(
        "INSERT INTO folders (id, path, path_key, is_online) VALUES (1, ?1, ?1, 1), (2, ?2, ?2, 1)",
        rusqlite::params![root_path, sub],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO photos
           (folder_id, name, name_key, size_bytes, mtime_ns, width, height, orientation,
            taken_at, taken_at_src, thumb_state, decoder_ver)
         VALUES (1, 'a.jpg', 'a.jpg', 1, 1, 4000, 3000, NULL, 100, 0, 0, 0),
                (2, 'b.jpg', 'b.jpg', 2, 2, 1000, 2000, 6,    200, 0, 0, 0);",
    )
    .unwrap();
}

#[test]
fn opening_an_m2_library_moves_roots_into_the_database() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = isolated_paths(tmp.path());
    paths.ensure().unwrap();

    let photos = tmp.path().join("photos");
    std::fs::create_dir_all(photos.join("2011/05")).unwrap();
    // 放一个带大小写差异的条目，好让 case_insensitive 探测得出确定结论
    // （全是数字/中文的目录探测不了，会按默认取「不区分大小写」）
    std::fs::write(photos.join("Best.JPG"), b"not-a-real-jpeg").unwrap();
    let root = paths::canonical_string(&photos);

    make_m2_db(&paths.db_file(), &root);
    // M2 的偏好：根目录在 JSON 里
    std::fs::write(
        paths.preferences_file(),
        serde_json::json!({ "version": 1, "roots": [root], "tier": "Large" }).to_string(),
    )
    .unwrap();

    let lib = Library::open(paths.clone()).unwrap();

    // ---- DB 侧：roots 表里有它，且探测结果已固化
    let db_roots = lib.db().unwrap().roots(false).unwrap();
    assert_eq!(db_roots.len(), 1);
    assert_eq!(db_roots[0].path, root);
    assert!(
        !db_roots[0].case_insensitive,
        "Linux 上的 tmp 目录区分大小写"
    );

    // ---- 数据步骤只跑一次
    assert!(!lib.db().unwrap().needs_v2_data().unwrap());

    // ---- 树侧：读表读出来的节点，中间层 `2011` 被补出来
    assert_eq!(lib.root_paths(), vec![photos.clone()]);
    assert_eq!(lib.scan_roots(), vec![photos.clone()]);
    let flat: Vec<&str> = lib
        .visible_folders
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert!(flat.contains(&root.as_str()));
    assert!(
        flat.contains(&format!("{root}/2011").as_str()),
        "2011 没有照片行，但必须补出来，否则 05 挂不上去：{flat:?}"
    );
    assert!(flat.iter().any(|p| p.ends_with("/2011/05")));

    // ---- 层级字段：direct_count 是物理事实
    let all = lib.db().unwrap().folders_of_root(db_roots[0].id).unwrap();
    let by_path = |suffix: &str| all.iter().find(|f| f.path.ends_with(suffix)).unwrap();
    assert_eq!(by_path("/2011/05").direct_count, 1);
    assert_eq!(by_path("/2011/05").name.as_deref(), Some("05"));
    assert!(by_path("/photos").has_children);

    // ---- 生成列：orientation=6 的照片显示宽高要交换
    let (dw, dh) = lib.db().unwrap().display_dims(2).unwrap().unwrap();
    assert_eq!((dw, dh), (Some(2000), Some(1000)));

    // ---- 偏好里的 roots 保留（只读兼容），但不再是真相源
    assert_eq!(lib.prefs.roots, vec![root.clone()]);
}

#[test]
fn adding_a_root_writes_the_database_not_the_preferences() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = isolated_paths(tmp.path());
    paths.ensure().unwrap();
    let photos = tmp.path().join("photos");
    std::fs::create_dir_all(&photos).unwrap();

    let mut lib = Library::open(paths.clone()).unwrap();
    lib.add_root(&photos);
    lib.save_prefs();

    let root = paths::canonical_string(&photos);
    let db_roots = lib.db().unwrap().roots(false).unwrap();
    assert_eq!(db_roots.len(), 1);
    assert_eq!(db_roots[0].path, root);

    // 偏好文件里**不该**再出现这个路径：两处都写就是双真相源
    let prefs_json = std::fs::read_to_string(paths.preferences_file()).unwrap();
    assert!(
        !prefs_json.contains(&root),
        "roots 只能有一个真相源（DB）：{prefs_json}"
    );

    // 关闭等价于「从图库移除」：状态置 closed，记录还在
    lib.remove_root(&photos);
    let all = lib.db().unwrap().roots(true).unwrap();
    assert_eq!(all.len(), 1, "记录保留，便于再次添加时复用 id");
    assert!(!all[0].is_active());
    assert!(lib.root_paths().is_empty(), "树里不应再显示");
}
