//! M5-R5 整理回归：收藏（布尔）/ 说明 / 标签三态 / 相册 / 托盘 / 选择语义。
//!
//! 本轮**不新增能力**，只把「R1–R4 改壳时容易碰坏」的既有行为用测试钉住：
//!
//! - 收藏是布尔，不是 1–5 星（`M5-OR-01`、`M5-GR-07`）
//! - 说明只写索引、多选不预填（`M5-OR-03`）
//! - 批量标签三态、取消勾选即移除、无选中不落库（`M5-OR-05`、`M5-OR-07`）
//! - 相册是独立集合、删除只解关联、不删照片（`M5-OR-08`～`M5-OR-10`）

use picasa_core::{AppPaths, Library, View};
use picasa_db::{PhotoInput, PhotoRow};
use std::collections::HashSet;
use std::path::Path;

const ROOT: &str = "/m5r5/albums";

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

/// 6 张照片，全部落在同一个根 / 一个文件夹。
fn fixture() -> (tempfile::TempDir, Library) {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    {
        let db = lib.db().unwrap();
        let root = db.upsert_root(ROOT, 0, false).unwrap();
        let fid = db.upsert_folder(ROOT, ROOT, None).unwrap();
        db.set_folder_root(fid, root, None).unwrap();
        let items: Vec<PhotoInput> = (1..=6)
            .map(|i| photo(&format!("p{i}.jpg"), 1_300_000_000 + i))
            .collect();
        db.apply_scan(fid, ROOT, &items, 0).unwrap();
    }
    lib.reload_photos().unwrap();
    lib.set_view(View::All).unwrap();
    (tmp, lib)
}

fn ids(lib: &Library) -> Vec<i64> {
    lib.photos.iter().map(|p| p.id).collect()
}

fn row(lib: &Library, id: i64) -> &PhotoRow {
    lib.photos.iter().find(|p| p.id == id).expect("照片在栅格里")
}

/// `M5-OR-01` / `AC-OR-01`：收藏是**布尔**。批量设 true / false 往返，
/// 只有两种状态，没有「第几颗星」。
#[test]
fn favorites_are_a_boolean_not_a_one_to_five_star_rating() {
    let (_tmp, mut lib) = fixture();
    let all = ids(&lib);
    lib.selection = all.iter().copied().take(3).collect();

    assert_eq!(lib.batch_set_starred(true).unwrap(), 3);
    for id in all.iter().take(3) {
        assert!(row(&lib, *id).starred, "已收藏的应显示星标");
    }
    for id in all.iter().skip(3) {
        assert!(!row(&lib, *id).starred, "未选中的不该被连带收藏");
    }

    // 混合态下再点一次 = 全部收藏（不是「加一颗星」）
    lib.selection = all.iter().copied().collect();
    assert_eq!(lib.batch_set_starred(true).unwrap(), 6);
    assert!(all.iter().all(|id| row(&lib, *id).starred));

    // 全收藏再点 = 全部取消
    assert_eq!(lib.batch_set_starred(false).unwrap(), 6);
    assert!(all.iter().all(|id| !row(&lib, *id).starred));
}

/// `M5-OR-03` / `AC-OR-01`：说明只写索引；多选保存给每张都写同一段文字，
/// `caption_of` 读回一致；「多选不预填」是 UI 侧规则，这里锁住数据侧的可写可读。
#[test]
fn captions_write_to_the_index_for_every_selected_photo() {
    let (_tmp, mut lib) = fixture();
    let all = ids(&lib);
    let picked: Vec<i64> = all.iter().copied().take(3).collect();

    for id in &picked {
        lib.set_caption(*id, "同一段说明").unwrap();
    }
    for id in &picked {
        assert_eq!(lib.caption_of(*id).unwrap(), "同一段说明");
    }
    for id in all.iter().skip(3) {
        assert_eq!(lib.caption_of(*id).unwrap(), "", "没选的不能被写入");
    }

    // 清空说明 = 写空串（不是留一条脏记录）
    lib.set_caption(picked[0], "").unwrap();
    assert_eq!(lib.caption_of(picked[0]).unwrap(), "");
}

/// `M5-OR-05` / `M5-OR-07` / `AC-OR-TRISTATE`：批量标签三态的数据侧。
///
/// 三态是「选中集里有多少张已带此标签」（`tri_state_counts`）：
/// 全部 = ☑、部分 = ▣、没有 = ☐。再点一次已全选的标签是**移除**。
#[test]
fn batch_tags_are_tri_state_and_unticking_removes_them() {
    let (_tmp, mut lib) = fixture();
    let all = ids(&lib);
    let picked: Vec<i64> = all.iter().copied().take(3).collect();

    // 无选中：新建标签必须被拒绝（一次关联都不写）
    lib.selection.clear();
    assert_eq!(lib.batch_add_tag("旅行").unwrap(), 0);
    assert!(
        lib.all_tags().unwrap().iter().all(|(t, _)| t != "旅行"),
        "无选中时不该落任何一种标签关联"
    );

    // 选中 3 张，只给其中 2 张打标签 → 部分选中（▣）
    lib.selection = picked.iter().copied().collect();
    assert_eq!(lib.batch_add_tag("旅行").unwrap(), 3);
    lib.batch_remove_tag("旅行").unwrap();
    lib.selection = picked.iter().copied().take(2).collect();
    assert_eq!(lib.batch_add_tag("旅行").unwrap(), 2);

    lib.selection = picked.iter().copied().collect();
    let by_photo = lib.tags_for_ids(&picked).unwrap();
    let labels = lib.all_tags().unwrap();
    let counts = Library::tri_state_counts(&picked, &by_photo, &labels);
    assert_eq!(counts.get("旅行"), Some(&2), "2/3 → 部分选中（▣）");

    // 全选这一批都已带的标签 → 再点 = 全部移除
    lib.selection = picked.iter().copied().take(2).collect();
    assert_eq!(lib.batch_remove_tag("旅行").unwrap(), 2);
    let by_photo = lib.tags_for_ids(&picked).unwrap();
    let counts = Library::tri_state_counts(&picked, &by_photo, &labels);
    assert_eq!(counts.get("旅行"), Some(&0), "全部移除 → ☐");

    // 空选中集：所有标签都是 0，不 panic
    let empty = Library::tri_state_counts(&[], &by_photo, &labels);
    assert!(empty.values().all(|&v| v == 0));
}

/// `M5-OR-08`～`M5-OR-10` / `AC-OR-01`：相册是独立集合，删除只解除关联，
/// 照片一张不少、原文件不动。
#[test]
fn albums_are_independent_collections_and_deleting_one_keeps_the_photos() {
    let (_tmp, lib) = fixture();
    let all = ids(&lib);
    let picked: Vec<i64> = all.iter().copied().take(4).collect();

    let album = lib.create_album("2024 夏", 1_700_000_000).unwrap();
    let added = lib.add_to_album(album, &picked, 1_700_000_000).unwrap();
    assert_eq!(added, 4, "4 张全部加入");
    assert_eq!(lib.album_photo_ids(album).unwrap().len(), 4);

    // 加入相册不改变视图、不移动文件：栅格仍是 6 张
    assert_eq!(lib.photos.len(), 6);

    // 删除相册只解关联
    lib.delete_album(album).unwrap();
    assert_eq!(lib.photos.len(), 6, "删相册不许动照片索引");
    assert_eq!(
        lib.photos.iter().map(|p| p.id).collect::<HashSet<_>>(),
        all.iter().copied().collect::<HashSet<_>>()
    );

    // 相册名不再出现在侧栏清单里
    assert!(
        lib.all_albums()
            .unwrap()
            .iter()
            .all(|a| a.name != "2024 夏")
    );
}

/// `AC-GR-01` / `M5-GR-01`～`M5-GR-04`：选择语义（单击 / ⌘ 加减 / Shift 范围 /
/// 全选 / 取消 / 反选 / Home / End）。占位格不参与由栅格层保证，
/// 这里锁住数据侧的下标 → id 映射不会串行。
#[test]
fn selection_modifiers_and_edges_follow_the_grid_order() {
    let (_tmp, mut lib) = fixture();
    let all = ids(&lib);
    assert_eq!(all.len(), 6);

    // 单击
    lib.click_select(2, false, false);
    assert_eq!(lib.selection, HashSet::from([all[2]]));
    assert_eq!(lib.anchor, Some(2));

    // ⌘ 单击加减：已选中的再点一次是取消
    lib.click_select(4, true, false);
    assert_eq!(lib.selection, HashSet::from([all[2], all[4]]));
    lib.click_select(2, true, false);
    assert_eq!(lib.selection, HashSet::from([all[4]]));

    // Shift 范围（从锚点 2 到 4，含端点）
    lib.click_select(2, false, false);
    lib.click_select(4, false, true);
    assert_eq!(
        lib.selection,
        HashSet::from([all[2], all[3], all[4]]),
        "Shift 范围必须含端点"
    );

    // 全选 / 取消 / 反选
    lib.select_all();
    assert_eq!(lib.selection.len(), 6);
    lib.select_none();
    assert!(lib.selection.is_empty());
    lib.selection = HashSet::from([all[0], all[3]]);
    lib.select_invert();
    assert_eq!(
        lib.selection,
        HashSet::from([all[1], all[2], all[4], all[5]]),
        "反选 = 全量 − 当前选中"
    );

    // Home / End
    lib.select_edge(false);
    assert_eq!(lib.selection, HashSet::from([all[0]]));
    lib.select_edge(true);
    assert_eq!(lib.selection, HashSet::from([all[5]]));
}

/// `M5-SB-20` / `AC-SB-01`：托盘加入 / 移出 / 清空，且托盘内容随偏好持久化。
#[test]
fn tray_add_remove_and_clear_are_all_idempotent() {
    let (_tmp, mut lib) = fixture();
    let all = ids(&lib);
    lib.selection = all.iter().copied().take(2).collect();

    assert_eq!(lib.to_tray(), 2, "首次加入 2 张");
    assert_eq!(lib.to_tray(), 0, "重复加入不重复计数");
    assert_eq!(lib.tray.len(), 2);

    lib.tray_remove(all[0]);
    assert_eq!(lib.tray, vec![all[1]]);
    lib.tray_remove(all[0]); // 幂等
    assert_eq!(lib.tray, vec![all[1]]);

    lib.tray_clear();
    assert!(lib.tray.is_empty());
}
