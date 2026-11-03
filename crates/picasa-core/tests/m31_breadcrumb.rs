//! 顶栏双语境路径面包屑（`09` §7.3）的验收测试。
//!
//! 双语境 = 文件夹语境（`图库 / 祖先… / 当前`，祖先可点击跳转）与
//! 根语境（弱化成单段 `图库`）。三层树保证祖先链真的被逐级收集，
//! 而不是只拼出「根 + 当前」两段。

use picasa_core::library::View;
use picasa_core::{AppPaths, Library};
use picasa_db::{Db, PhotoInput};

struct Fixture {
    _tmp: tempfile::TempDir,
    lib: Library,
    root: i64,
    a: i64,
    b: i64,
}

/// 建一棵三层树：root(1 张) ├ a(2 张) ─ b(3 张)；└ c(0 张)。与 m3_recurse 同款。
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let mut paths = AppPaths::resolve();
    paths.support = tmp.path().join("support");
    paths.preferences = tmp.path().join("prefs");
    paths.caches = tmp.path().join("caches");
    paths.logs = tmp.path().join("logs");
    paths.ensure().unwrap();

    let photos_dir = tmp.path().join("photos");
    std::fs::create_dir_all(&photos_dir).unwrap();

    let db = Db::open(paths.db_file()).unwrap();
    let root_str = photos_dir.to_string_lossy().to_string();
    let root_id = db.upsert_root(&root_str, 0, false).unwrap();

    let mk = |rel: &str, n: usize, parent: Option<i64>| -> i64 {
        // ⚠️ `Path::join("")` 会产出带尾斜杠的路径（`photos/`），与真实扫描器
        // （`paths::canonical_string` 去尾斜杠）落库的 path 不一致，树里就会
        // 出现两个「同名根节点」。这里显式避开，保持与扫描器同一条口径。
        let dir = if rel.is_empty() {
            photos_dir.clone()
        } else {
            photos_dir.join(rel)
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_string_lossy().to_string();
        let fid = db.upsert_folder(&path, &path.to_lowercase(), None).unwrap();
        db.set_folder_root(fid, root_id, parent).unwrap();
        let items: Vec<PhotoInput> = (0..n)
            .map(|i| PhotoInput {
                name: format!("{rel}-{i}.jpg"),
                name_key: format!("{rel}-{i}.jpg"),
                size_bytes: 100,
                mtime_ns: 1_600_000_000_000_000_000,
                width: Some(800),
                height: Some(600),
                orientation: Some(1),
                taken_at: Some(1_600_000_000 + i as i64),
                taken_at_src: 0,
                format: 0,
            })
            .collect();
        db.apply_scan(fid, &path, &items, 0).unwrap();
        fid
    };

    let root_fid = mk("", 1, None);
    let a = mk("a", 2, Some(root_fid));
    let b = mk("a/b", 3, Some(a));
    mk("c", 0, Some(root_fid));
    drop(db);

    let lib = Library::open(paths).unwrap();
    Fixture {
        _tmp: tmp,
        lib,
        root: root_fid,
        a,
        b,
    }
}

#[test]
fn breadcrumb_folder_view_lists_gallery_root_and_ancestors() {
    let mut f = fixture();
    f.lib.set_view(View::Folder(f.b)).unwrap();
    let crumbs = f.lib.view_breadcrumb();
    assert_eq!(crumbs.len(), 4, "图库 + root + a + b");
    assert_eq!(crumbs[0], ("图库".to_string(), None), "根段是语境前缀，不可点");
    assert_eq!(crumbs[1].1, Some(f.root), "祖先段带 folder_id（可跳转）");
    assert_eq!(crumbs[2].1, Some(f.a));
    assert_eq!(crumbs[3].1, Some(f.b), "最后一段 = 当前文件夹");
}

#[test]
fn breadcrumb_mid_level_folder_has_no_extra_segments() {
    let mut f = fixture();
    f.lib.set_view(View::Folder(f.a)).unwrap();
    let crumbs = f.lib.view_breadcrumb();
    assert_eq!(crumbs.len(), 3, "图库 + root + a");
    assert_eq!(crumbs[2].1, Some(f.a));
}

#[test]
fn breadcrumb_root_folder_is_two_segments() {
    let mut f = fixture();
    f.lib.set_view(View::Folder(f.root)).unwrap();
    let crumbs = f.lib.view_breadcrumb();
    assert_eq!(crumbs.len(), 2, "图库 + 根目录本身");
}

#[test]
fn breadcrumb_root_context_is_weakened_to_gallery_only() {
    let mut f = fixture();
    f.lib.set_view(View::All).unwrap();
    let crumbs = f.lib.view_breadcrumb();
    assert_eq!(crumbs, vec![("图库".to_string(), None)]);
    f.lib.set_view(View::Recent).unwrap();
    assert_eq!(f.lib.view_breadcrumb(), vec![("图库".to_string(), None)]);
}
