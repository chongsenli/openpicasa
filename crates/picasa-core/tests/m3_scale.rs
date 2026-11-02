//! 虚拟数据源的验收测试（`07` §12.2 `TC-M3-SCALE-001～005`）。
//!
//! ## 这些用例要证明的三件事
//!
//! 1. **窗口有界**：内存里的行数永远 ≤ 4 屏（`TC-M3-SCALE-004`）。这是整个
//!    「100 万张不崩溃」的地基——一旦某处偷偷把全列读进内存，内存门禁立刻失守，
//!    而这个错误**不会报错**，只会让内存慢慢涨上去。
//! 2. **键集分页真的走索引**（`TC-M3-SCALE-003`）：`EXPLAIN QUERY PLAN` 里不许出现
//!    `SCAN photos`。走 OFFSET 的查询能给出**完全正确**的结果，只是慢——
//!    所以正确性用例拦不住它，只有执行计划能。
//! 3. **翻页不重不漏**：键集分页最经典的坑是漏掉「同一秒拍摄」的那批照片
//!    （比较时少写了 id 兜底）。这里专门造一批相同 `taken_at` 的行去撞它。

use picasa_core::datasource::{
    DataSource, GroupBound, IN_LIST_MAX, RANGE_MAX, Scope, group_at_offset, group_of,
};
use picasa_core::library::group_by_day;
use picasa_db::{CountSnapshot, Db, PhotoInput, SortKey, SortSpec};
use std::sync::{Arc, Mutex};

/// 造一个目录 + n 张照片。`taken_at` 按 `secs_per_photo` 递增，
/// 传 0 就得到「全部同一秒」的连拍场景。
fn fixture(
    n: usize,
    secs_per_photo: i64,
) -> (tempfile::TempDir, Arc<Mutex<Db>>, i64, CountSnapshot) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("scale.sqlite")).unwrap();
    let dir = tmp.path().join("photos");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.to_string_lossy().to_string();
    let root_id = db.upsert_root(&path, 0, false).unwrap();
    let fid = db.upsert_folder(&path, &path.to_lowercase(), None).unwrap();
    db.set_folder_root(fid, root_id, None).unwrap();

    let base = 1_600_000_000i64;
    let items: Vec<PhotoInput> = (0..n)
        .map(|i| {
            let taken = base + (i as i64) * secs_per_photo;
            PhotoInput {
                name: format!("p{i:06}.jpg"),
                name_key: format!("p{i:06}.jpg"),
                size_bytes: 1000 + i as i64,
                mtime_ns: taken * 1_000_000_000,
                width: Some(4000),
                height: Some(3000),
                orientation: Some(1),
                taken_at: Some(taken),
                taken_at_src: 0,
                format: 0,
            }
        })
        .collect();
    db.apply_scan(fid, &path, &items, 0).unwrap();
    let counts = db.count_snapshot().unwrap();
    (tmp, Arc::new(Mutex::new(db)), fid, counts)
}

fn source(db: &Arc<Mutex<Db>>, fid: i64, counts: &CountSnapshot) -> DataSource {
    DataSource::new(db.clone(), Scope::Folder(fid), SortSpec::default(), 1, counts).unwrap()
}

#[test]
fn tc_m3_scale_001_ui_never_holds_the_whole_column() {
    // 造 5000 行（远超 4 屏），模拟「用户滚到任意位置」。
    let (_tmp, db, fid, counts) = fixture(5000, 60);
    let mut ds = source(&db, fid, &counts);

    assert_eq!(ds.len(), 5000);
    // 全列永远不驻留：窗口上限 = RANGE_MAX 与 4 屏的较大者
    let four_screens = 4 * picasa_core::datasource::ROWS_PER_SCREEN_ESTIMATE;
    for start in [0usize, 100, 1000, 2500, 4990] {
        let rows = ds.range(start, 60).unwrap();
        assert_eq!(rows.len(), 60.min(5000 - start));
        assert!(
            ds.cached_rows() <= RANGE_MAX.max(four_screens),
            "缓存了 {} 行，超过窗口上限",
            ds.cached_rows()
        );
    }
    // 窗口是**复用**的：在刚落脚的窗口内部挪动不该重查。
    // ⚠️ 前面那一串跳转都会重建窗口，所以必须紧接一次「滚一小步」来验命中。
    let _ = ds.range(4000, 60).unwrap();
    let misses_before = ds.misses;
    let _ = ds.range(4010, 60).unwrap();
    assert_eq!(
        ds.misses, misses_before,
        "窗口内部滚动必须命中缓存（否则每帧一次 DB 往返）"
    );
}

#[test]
fn tc_m3_scale_002_group_bounds_cover_every_row_without_gaps() {
    // 组边界必须**无缝无重叠**地铺满整列：组头 N 之和 = len()。
    // 差一行的表现是「滚到某天最后一张，组头提前切换」。
    let (_tmp, db, fid, counts) = fixture(300, 86_400); // 每张隔一天
    let mut ds = source(&db, fid, &counts);

    // 先克隆一份组边界：`groups()` 借的是 `&ds`，而下面 `range()` 要 `&mut ds`
    let groups = ds.groups().to_vec();
    assert!(!groups.is_empty(), "按日期分组必须产出组");
    assert_eq!(groups[0].start, 0, "第一组从 0 开始");
    let mut expect = 0usize;
    for g in &groups {
        assert_eq!(g.start, expect, "组起点必须接上一组的终点（组 {}）", g.label);
        assert!(g.len > 0, "空组不该出现");
        expect += g.len;
    }
    assert_eq!(expect, ds.len(), "组边界之和必须等于整列长度");

    // 组头文案与 UI 的 `group_by_day` 必须一致（同一时区口径）
    let total = ds.len();
    let rows_slice = ds.range(0, total).expect("取不到行").to_vec();
    let ui_groups = group_by_day(&rows_slice);
    assert_eq!(ui_groups.len(), groups.len(), "组数必须与 UI 侧一致");
    for (ui, dsg) in ui_groups.iter().zip(groups.iter()) {
        assert_eq!(ui.0, dsg.label, "组头文案必须逐字一致");
        assert_eq!(ui.2 - ui.1, dsg.len);
    }
}

#[test]
fn tc_m3_scale_003_keyset_plan_uses_an_index() {
    // 这条是**唯一**能证明「不是 OFFSET」的用例：结果对不对证明不了性能。
    let (_tmp, db, fid, counts) = fixture(100, 60);
    let ds = source(&db, fid, &counts);
    let _ = ds;

    let guard = db.lock().unwrap();
    // ⚠️ 只断言「按拍摄时间」——那是 v6 唯一补齐了 `(taken_at, id)` 复合索引的排序键。
    // 其余排序键（大小 / 修改时间 / 文件名）在 M4 补索引之前走 OFFSET，
    // 把它们也塞进这条用例等于**制造一条注定红的断言**，而不是发现问题。
    for sort in [
        SortSpec {
            key: SortKey::TakenAt,
            descending: true,
        },
        SortSpec {
            key: SortKey::TakenAt,
            descending: false,
        },
    ] {
        let plan = guard.explain_keyset_plan(sort).unwrap();
        let text = plan.join(" | ");
        assert!(
            !text.contains("SCAN p"),
            "★ 键集查询退化成全表扫描了（sort={:?}）：{text}",
            sort.key
        );
        assert!(
            !plan.iter().any(|l| l.contains("USE TEMP B-TREE FOR ORDER BY")),
            "★ 排序没能走索引（sort={:?}）：{text}",
            sort.key
        );
    }
}

#[test]
fn tc_m3_scale_003b_paging_does_not_skip_photos_in_the_same_second() {
    // 键集分页最经典的坑：同一秒拍 500 张时，只按 `taken_at` 比较会跳过整批。
    // 这里 `secs_per_photo = 0` → 全部同一秒。
    let (_tmp, db, fid, counts) = fixture(500, 0);
    let mut ds = source(&db, fid, &counts);
    assert_eq!(ds.len(), 500);

    // 逐页往后翻（每页 37，故意不是整倍数，会切在中间）
    let mut seen = Vec::new();
    let mut start = 0usize;
    while start < ds.len() {
        let n = 37.min(ds.len() - start);
        let rows = ds.range(start, n).unwrap();
        seen.extend(rows.iter().map(|p| p.id));
        start += n;
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 500, "翻页必须不重不漏（实际 {} 张）", seen.len());
}

#[test]
fn tc_m3_scale_004_window_stays_bounded_while_dragging_the_scrollbar() {
    // 「快速拖动滚动条」是最坏情况：每一步都 miss。
    // 即便这样，驻留行数也不能随拖动次数增长。
    let (_tmp, db, fid, counts) = fixture(3000, 60);
    let mut ds = source(&db, fid, &counts);

    let mut peak = 0usize;
    for i in 0..40 {
        let start = (i * 73) % 2900;
        let _ = ds.range(start, 60).unwrap();
        peak = peak.max(ds.cached_rows());
    }
    assert!(
        peak <= RANGE_MAX,
        "拖动 40 次后缓存峰值 {peak} 行，超过单次上限 {RANGE_MAX}"
    );
    // 拖动的目标是「驻留行数不随拖动次数增长」，不是「每次必 miss」——
    // 相邻位置偶尔落在同一窗口里是好事（说明缓存有用）。
    assert!(ds.misses > 0, "这个场景必然有 miss，否则说明缓存范围大得可疑");
}

#[test]
fn tc_m3_scale_004b_index_of_recovers_the_anchor() {
    // 切档 / 切视图要按 `photo_id` 找回滚动位置（`07` ZOOM-05 / SCALE-01）。
    let (_tmp, db, fid, counts) = fixture(200, 60);
    let ds = source(&db, fid, &counts);

    // 默认按拍摄时间倒序：最后一张 p000000 在最前面？
    // `taken_at` 递增，倒序 → 下标 0 是最新的（p000199）。
    let rows = {
        let mut d = source(&db, fid, &counts);
        d.range(0, 10).unwrap().to_vec()
    };
    let target = rows[3].id;
    let idx = ds.index_of(target).unwrap();
    assert_eq!(idx, Some(3), "前 10 行里第 4 张的下标必须是 3");
}

#[test]
fn tc_m3_scale_005_thumb_in_list_is_capped() {
    // SCALE-05：`IN` 列表 ≤ 200。这里通过公开常量把它钉住——
    // 有人把 200 改成 20000 时，这条会立刻红。
    assert_eq!(IN_LIST_MAX, 200);
}

#[test]
fn group_helpers_agree_with_the_bounds() {
    // UI-04 的滚动条日期气泡只靠 `group_bounds()`，零额外查询。
    let bounds = vec![
        GroupBound {
            start: 0,
            len: 4,
            label: "2024年01月01日".into(),
            date: Some(1),
        },
        GroupBound {
            start: 4,
            len: 2,
            label: "2023年12月31日".into(),
            date: Some(0),
        },
    ];
    assert_eq!(group_of(&bounds, 0), Some(0));
    assert_eq!(group_of(&bounds, 3), Some(0));
    assert_eq!(group_of(&bounds, 4), Some(1));
    assert_eq!(
        group_at_offset(&bounds, 5).map(|g| g.label.as_str()),
        Some("2023年12月31日")
    );
}

#[test]
fn subtree_scope_lists_its_ordered_folders() {
    // RECURSE 的 `ordered_folders` 是 DFS 序的数组（07-A §3.3）。
    // 这里验接口形状：按文件夹分组的组边界要能直接由它拼出来。
    let ordered: Arc<[i64]> =
        Arc::from(vec![10i64, 11, 12].into_boxed_slice());
    let scope = Scope::Subtree {
        root_folder: 10,
        ordered_folders: ordered.clone(),
    };
    assert_eq!(scope.folders(), Some(&ordered[..]));
    assert_eq!(Scope::Folder(7).folders(), Some(&[7i64][..]));
    assert_eq!(Scope::AllPhotos.folders(), None);
}
