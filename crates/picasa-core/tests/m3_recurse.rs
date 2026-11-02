//! 「包含子文件夹」的验收测试（`07-A` §12.2 `TC-M3-RECURSE-001～006`）。
//!
//! ## 这个开关最容易错的地方
//!
//! 1. **范围算错**：开了之后仍然只显示直接照片（或反过来，关了还显示子目录）。
//!    这两种错在界面上都「看起来正常」——只是数字不对，而数字对不对要靠对账。
//! 2. **计数不同源**：标题栏用 `total`、组头用 `direct`，于是「标题 13 张、
//!    滚到底只有 12 格」。这是六处一致的直接破口。
//! 3. **已关闭子目录**：`total` 的口径明确**不含**已关闭的子树（`07` §4.1）。
//!    「包含子文件夹」也必须守住这一条，否则一开就把用户关掉的目录翻出来。
//!
//! ## 为什么要造一棵真的三层树
//!
//! 平坦的树里「含不含子目录」算出来的结果一样，测不出任何东西。
//! 只有 `root → a → b` 这种多层结构才能暴露「只算了一层」的实现错误。

use picasa_core::library::View;
use picasa_core::{AppPaths, Library};
use picasa_db::{Db, PhotoInput};

struct Fixture {
    _tmp: tempfile::TempDir,
    lib: Library,
    ids: Ids,
}

struct Ids {
    root: i64,
    a: i64,
    b: i64,
    c: i64,
}

/// 建一棵三层的树：root(1 张) ├ a(2 张) ─ b(3 张)；└ c(0 张)。共 6 张。
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

    // 先用裸 Db 建库与数据，再用 Library 打开（这样能直接控制父子关系）
    let db = Db::open(paths.db_file()).unwrap();
    let root_str = photos_dir.to_string_lossy().to_string();
    let root_id = db.upsert_root(&root_str, 0, false).unwrap();

    let mk = |rel: &str, n: usize, parent: Option<i64>| -> i64 {
        let dir = photos_dir.join(rel);
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
            })
            .collect();
        db.apply_scan(fid, &path, &items, 0).unwrap();
        fid
    };

    let root_fid = mk("", 1, None);
    let a = mk("a", 2, Some(root_fid));
    let b = mk("a/b", 3, Some(a));
    let c = mk("c", 0, Some(root_fid));
    drop(db);

    let lib = Library::open(paths).unwrap();
    Fixture {
        _tmp: tmp,
        lib,
        ids: Ids {
            root: root_fid,
            a,
            b,
            c,
        },
    }
}

#[test]
fn tc_m3_recurse_001_default_is_off_and_shows_only_direct_photos() {
    // RECURSE-01 / 原版行为：关（默认）= 只显示 F 的直接照片。
    let mut f = fixture();
    assert!(!f.lib.recurse, "默认必须关（P-28：原版行为）");
    f.lib.set_view(View::Folder(f.ids.a)).unwrap();
    assert_eq!(f.lib.photos_total, 2, "a 只有 2 张直接照片");
    assert_eq!(f.lib.photos.len(), 2);
}

#[test]
fn tc_m3_recurse_002_turning_it_on_includes_the_whole_subtree() {
    // 开 → a 及全部后代 = 2 + 3 = 5 张。
    let mut f = fixture();
    f.lib.set_view(View::Folder(f.ids.a)).unwrap();
    assert_eq!(f.lib.photos_total, 2);

    assert!(f.lib.set_recurse(true).unwrap(), "切换应当生效");
    assert_eq!(f.lib.photos_total, 5, "a(2) + b(3)");
    // 必须是**真的**多出 3 行，而不是只把总数改了
    assert_eq!(f.lib.photos.len(), 5);

    // 关回去
    assert!(f.lib.set_recurse(false).unwrap());
    assert_eq!(f.lib.photos_total, 2);
}

#[test]
fn tc_m3_recurse_003_root_with_recurse_equals_all_photos_scale() {
    // RECURSE-03：根目录开启 = 「所有照片」规模。
    let mut f = fixture();
    f.lib.set_view(View::Folder(f.ids.root)).unwrap();
    assert_eq!(f.lib.photos_total, 1, "关：只有根自己的 1 张");
    f.lib.set_recurse(true).unwrap();
    assert_eq!(f.lib.photos_total, 6, "开：整棵树 = 1 + 2 + 3");
    // 与「所有照片」一致
    f.lib.set_view(View::All).unwrap();
    assert_eq!(f.lib.photos_total, 6, "根开启子文件夹 = 所有照片");
}

#[test]
fn tc_m3_recurse_004_counts_are_consistent_with_the_tree_totals() {
    // RECURSE-04 / 六处一致：开启态的视图计数必须等于 `folder_totals`。
    //
    // ⚠️ `total_count` 是 v6 数据步骤一次性算出来的，而夹具**先建行、后挂父子关系**
    // （`set_folder_root`），所以那一步跑的时候父子链还不存在，算出来全是 0。
    // 生产路径上 `derive_hierarchy` 会先把层级推完再算数，不存在这个顺序问题。
    // 这里用 `count_snapshot` 的**同事务内存聚合**做对照——它正是 UI 实际读的那份。
    let mut f = fixture();
    let totals = f.lib.db().unwrap().count_snapshot().unwrap().totals;
    assert_eq!(totals[&f.ids.a], 5, "a 的递归数是 5");

    f.lib.set_view(View::Folder(f.ids.a)).unwrap();
    f.lib.set_recurse(true).unwrap();
    assert_eq!(
        f.lib.photos_total as u32, totals[&f.ids.a],
        "★ 开启态的视图张数必须等于树上的递归数（否则标题与滚到底对不上）"
    );
}

#[test]
fn tc_m3_recurse_005_scope_ids_are_defensive_when_the_node_is_missing() {
    // 关闭的关注文件夹、尚未落库的新目录：树里找不到节点时
    // **必须返回 `vec![id]` 而不是空**——返回空会让栅格直接空掉，
    // 用户看到「这个文件夹没有照片」，而实际上只是树没同步。
    let mut f = fixture();
    f.lib.set_view(View::Folder(999_999)).unwrap();
    f.lib.set_recurse(true).unwrap();
    let ids = f.lib.view_folder_ids();
    assert_eq!(ids, Some(vec![999_999]), "找不到节点时也要给出这个 id");
}

#[test]
fn tc_m3_recurse_006_switch_is_global_not_per_folder() {
    // P-28：全局一份。在 a 打开后切到 c，c 也是「含子目录」状态。
    let mut f = fixture();
    f.lib.set_view(View::Folder(f.ids.a)).unwrap();
    f.lib.set_recurse(true).unwrap();
    assert!(f.lib.recurse);

    f.lib.set_view(View::Folder(f.ids.c)).unwrap();
    assert!(f.lib.recurse, "★ 开关是全局的，换文件夹不会重置");
    // c 自己有 0 张，也没有子目录 → 0
    assert_eq!(f.lib.photos_total, 0);

    f.lib.set_view(View::Folder(f.ids.root)).unwrap();
    assert_eq!(f.lib.photos_total, 6, "根在开启态仍是整棵树");
}

#[test]
fn tc_m3_recurse_009_switch_is_disabled_on_all_photos() {
    // RECURSE-09：「所有照片」视图下开关置灰（语义重叠）。
    let mut f = fixture();
    f.lib.set_view(View::All).unwrap();
    assert!(!f.lib.recurse_available(), "所有照片下不可用");
    // 即使强行改状态，也不该改变范围
    let before = f.lib.photos_total;
    assert!(!f.lib.set_recurse(true).unwrap(), "切了也没意义（返回 false）");
    assert_eq!(f.lib.photos_total, before);

    f.lib.set_view(View::Folder(f.ids.a)).unwrap();
    assert!(f.lib.recurse_available(), "文件夹视图下可用");
}

#[test]
fn tc_m3_recurse_002b_count_both_scopes_matches_the_two_views() {
    // `count_both_scopes` 是 RECURSE-02 锚点判断的依据：
    // 「切过去之后顶部那张还在不在」取决于两个范围各有多少张。
    let mut f = fixture();
    f.lib.set_view(View::Folder(f.ids.a)).unwrap();
    let (direct, subtree) = f.lib.count_both_scopes().unwrap();
    assert_eq!((direct, subtree), (2, 5));
}

#[test]
fn tc_m3_recurse_004b_no_extra_queries_when_building_the_scope() {
    // RECURSE-04：按文件夹分组时组边界**零额外查询**。
    //
    // 这条测的是「范围来自内存里的树」：`scope_folder_ids` 不管多少次都返回同样结果，
    // 且不查库（它只读 `self.roots`）。所以乘 1000 次也不该变慢。
    let f = fixture();
    let start = std::time::Instant::now();
    for _ in 0..1000 {
        let _ = f.lib.scope_folder_ids(true);
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "1000 次范围计算用了 {elapsed:?}——说明在查库而不是读内存树"
    );
}

#[test]
fn tc_m3_recurse_005b_closing_a_subfolder_removes_it_from_the_subtree() {
    // RECURSE-05：关闭某子目录 → 开启状态下视图即时减少。
    //
    // 关闭在本仓库表现为「从 folders 表删行」（子目录级没有独立开关，
    // 用户屏蔽子目录走排除项，那条路径根本不入库）。
    let mut f = fixture();
    f.lib.set_view(View::Folder(f.ids.a)).unwrap();
    f.lib.set_recurse(true).unwrap();
    assert_eq!(f.lib.photos_total, 5);

    // 关掉 b（3 张）
    f.lib.db().unwrap().delete_folder(f.ids.b).unwrap();
    f.lib.reload_tree().unwrap();
    f.lib.reload_photos().unwrap();
    assert_eq!(f.lib.photos_total, 2, "b 被关掉后只剩 a 自己的 2 张");

    // 恢复（重新入库）
    let b_path = {
        let folders = f.lib.db().unwrap().all_folders().unwrap();
        folders
            .iter()
            .map(|x| x.path.clone())
            .find(|p| p.ends_with("/a/b"))
            .unwrap_or_default()
    };
    let _ = b_path; // 恢复路径在本用例里不重要（重建索引属于别的用例）
}
