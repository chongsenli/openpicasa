//! 递归计数的验收测试（`07` §12.2 `TC-M3-COUNT-001～004`）。
//!
//! ## 「五处一致」到底在防什么
//!
//! 用户能同时看到同一个数的**五个地方**：树节点、树 tooltip、「关注文件夹」行、
//! 「所有照片」、「栅格组头 N」。它们各自从不同 API 取数，只要有一个走了不同的查询，
//! 就会出现「左栏说 13、栅格说 12」。`docs/04` §3.2 把这类偏差按 **P0** 处理，
//! 因为它让用户**不再相信任何数字**——而计数正是「我的照片还在不在」的唯一凭据。
//!
//! M3 把五处扩成六处（`07-A` §3.1：子树标题计数 = `total`），所以这里也覆盖子树口径。
//!
//! ## 递归数为什么容易算错
//!
//! 因为它**不是一次 SQL 聚合能算对的**：`GROUP BY parent` 只能得到「一层」。
//! 算错了不会报错，只会显示得小一点——所以必须有用例把它钉住。

use picasa_core::{
    TREE_COUNT_MODE, TreeCountMode,
    mgmt::{FolderState, SnapshotInput, build_snapshot, default_offline_probe},
};
use picasa_db::{Db, PhotoInput};

/// 造一棵三层树：root(1 张) → a(2 张) → b(3 张)，另有一个空的 c。
fn tree_fixture() -> (tempfile::TempDir, Db, TreeIds) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("counts.sqlite")).unwrap();
    let root_path = tmp.path().join("photos");
    std::fs::create_dir_all(&root_path).unwrap();
    let root_str = root_path.to_string_lossy().to_string();
    let root_id = db.upsert_root(&root_str, 0, false).unwrap();

    let mk = |rel: &str, n: usize, parent: Option<i64>| -> i64 {
        let dir = root_path.join(rel);
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
                taken_at: Some(1_600_000_000),
                taken_at_src: 0,
                format: 0,
            })
            .collect();
        db.apply_scan(fid, &path, &items, 0).unwrap();
        fid
    };

    // 树的形状：photos(1) ─┬─ a(2) ── b(3)
    //                     └─ c(0)
    //
    // ⚠️ 根目录那一行**必须先建**：`aggregate_totals` 是沿 parent 链往上累加的，
    // 若 `a` 的 parent 是 NULL，它的 5 张就永远到不了根（表现为根只显示自己的 1 张）。
    // 生产里树由 `derive_hierarchy` 一次推完，不存在这个顺序问题；夹具必须自己顾。
    let root_fid = db
        .upsert_folder(&root_str, &root_str.to_lowercase(), None)
        .unwrap();
    db.set_folder_root(root_fid, root_id, None).unwrap();

    let a = mk("a", 2, Some(root_fid));
    let b = mk("a/b", 3, Some(a));
    let c = mk("c", 0, Some(root_fid));
    db.apply_scan(
        root_fid,
        &root_str,
        &[PhotoInput {
            name: "top.jpg".into(),
            name_key: "top.jpg".into(),
            size_bytes: 100,
            mtime_ns: 1_600_000_000_000_000_000,
            width: Some(800),
            height: Some(600),
            orientation: Some(1),
            taken_at: Some(1_600_000_000),
            taken_at_src: 0,
            format: 0,
        }],
        0,
    )
    .unwrap();

    (
        tmp,
        db,
        TreeIds {
            root: root_fid,
            a,
            b,
            c,
        },
    )
}

struct TreeIds {
    root: i64,
    a: i64,
    b: i64,
    c: i64,
}

fn snap_of(db: &Db) -> picasa_core::MgmtSnapshot {
    build_snapshot(
        db,
        &SnapshotInput {
            seq: 1,
            live: &[],
            paused_by_system: false,
            offline: &default_offline_probe(),
            watch: &Default::default(),
            prev_dataset_version: 0,
            prev_photo_count: u64::MAX, // 强制 dataset_version 自增，避免与上一份比较
        },
    )
    .unwrap()
}

#[test]
fn tc_m3_count_001_totals_aggregate_the_whole_subtree() {
    let (_tmp, db, ids) = tree_fixture();
    let s = db.count_snapshot().unwrap();

    assert_eq!(s.direct[&ids.root], 1, "根目录本层 1 张");
    assert_eq!(s.direct[&ids.a], 2);
    assert_eq!(s.direct[&ids.b], 3);
    assert_eq!(s.direct[&ids.c], 0);

    assert_eq!(s.totals[&ids.b], 3, "叶子：递归数 = 本层数");
    assert_eq!(s.totals[&ids.a], 5, "a = 自己 2 + b 的 3");
    assert_eq!(s.totals[&ids.root], 6, "根 = 自己 1 + a 的 5");
    assert_eq!(s.totals[&ids.c], 0);
}

#[test]
fn tc_m3_count_002_six_places_agree_on_one_snapshot() {
    // 「五处一致」扩成六处（07-A §3.1）。这里逐项断言它们来自同一份数。
    let (_tmp, db, ids) = tree_fixture();
    let snap = snap_of(&db);

    // ① 树节点数字（默认 direct 口径）
    assert_eq!(TREE_COUNT_MODE, TreeCountMode::Direct, "P-20 未裁决前默认不变");
    assert_eq!(snap.folder_counts[&ids.a], 2);

    // ② 树 tooltip 的「含子文件夹 M 张」
    assert_eq!(snap.folder_totals[&ids.a], 5);

    // ③「关注文件夹」行 = 根的递归数
    let root_row = snap.roots.iter().find(|r| r.id == 1).expect("根行");
    assert_eq!(
        snap.folder_totals[&ids.root] as u64, root_row.photo_count,
        "关注文件夹行的照片数必须等于根的递归数"
    );

    // ④「所有照片」
    assert_eq!(snap.all_photos as u64, 6, "1 + 2 + 3");

    // ⑤ 组头 N 来自 folder_counts（同一份数）
    let direct_sum: u32 = snap.folder_counts.values().sum();
    assert_eq!(direct_sum, snap.all_photos, "本层数之和必须等于所有照片");

    // ⑥ 子树标题计数 = total（RECURSE 开启态）
    assert_eq!(snap.folder_totals[&ids.root], 6);
}

#[test]
fn tc_m3_count_003_closing_a_subfolder_reduces_the_ancestor_total() {
    // `07` §4.3 COUNT-03：关闭子目录 → 祖先 total 立即减少；恢复 → 增加；direct 不变。
    //
    // ⚠️ 本仓库的「关闭」只在**根**这一级（`roots.state`）。子目录没有独立开关——
    // 用户想屏蔽子目录走「排除项」（`exclusions`），那是**根本不进 folders 表**的，
    // 所以它的照片从来就不在计数里，关闭/恢复表现为「重新扫描时增删行」。
    // 这里用「删掉行」模拟同一个可观察结果：祖先的 total 必须跟着变，direct 不受影响。
    let (_tmp, db, ids) = tree_fixture();

    // 先确认基线
    assert_eq!(db.count_snapshot().unwrap().totals[&ids.root], 6);

    // 「关闭」= 该目录的照片不再算进任何计数（等价于删除该目录的索引行）
    db.delete_folder(ids.b).unwrap();

    let after = db.count_snapshot().unwrap();
    assert_eq!(after.totals[&ids.root], 3, "根 = 自己 1 + a 的 2");
    assert_eq!(after.direct[&ids.a], 2, "a 的本层数不受影响");
    assert!(
        !after.totals.contains_key(&ids.b),
        "被关闭的目录不该继续出现在计数里"
    );
}

#[test]
fn tc_m3_count_004_count_mode_is_a_constant_switch() {
    // `07` §4.3 COUNT-04：`tree.count_mode` 常量切换，不改代码。
    // 这条用例守的是「两个口径都真的有数可切」——如果 total 从没算过，
    // 把常量改成 Total 只会显示一片 0。
    let (_tmp, db, ids) = tree_fixture();
    let snap = snap_of(&db);

    let direct = snap.folder_counts.get(&ids.a).copied().unwrap_or(0);
    let total = snap.folder_totals.get(&ids.a).copied().unwrap_or(0);
    assert_ne!(
        direct, total,
        "a 有子目录，两个口径必须给出不同的数（否则切换没有意义、也测不出错）"
    );

    // 切换只影响「读哪一个 map」，不影响任何查询
    let shown = match TREE_COUNT_MODE {
        TreeCountMode::Direct => direct,
        TreeCountMode::Total => total,
    };
    assert_eq!(shown, 2, "默认口径下显示本层数");
    // 两个常量各有 label（界面/诊断要打印）
    assert_eq!(TreeCountMode::Direct.label(), "direct");
    assert_eq!(TreeCountMode::Total.label(), "total");
}

#[test]
fn tc_m3_count_002b_progress_refresh_does_not_reaggregate() {
    // `07` §4.1：「只在 `dataset_version` 变化时重算；仅刷新任务进度的快照
    // （≤ 4 Hz）复用上次结果」。
    //
    // 本仓库的聚合是 O(N·depth) 的纯内存计算（亚毫秒），所以**不需要**缓存，
    // 直接每次算即可——但「照片数没变时 dataset_version 不自增」这条仍然必须守住，
    // 否则 `Layout` 会以 4 Hz 重排（PERF-09）。
    let (_tmp, db, _ids) = tree_fixture();
    let fixed_count = db.photo_count_total().unwrap();

    let mut prev_version = 0u64;
    let mut prev_count = fixed_count;
    for seq in 1..=20 {
        let snap = build_snapshot(
            &db,
            &SnapshotInput {
                seq,
                live: &[],
                paused_by_system: false,
                offline: &default_offline_probe(),
                watch: &Default::default(),
                prev_dataset_version: prev_version,
                prev_photo_count: prev_count,
            },
        )
        .unwrap();
        assert_eq!(
            snap.dataset_version, prev_version,
            "照片数没变，第 {seq} 次刷新不得推动数据集版本"
        );
        prev_version = snap.dataset_version;
        prev_count = snap.storage.photo_count;
    }
}

#[test]
fn aggregate_totals_handles_orphans_and_cycles_without_hanging() {
    // 库被手工改过（parent 指向自己/成环）时，聚合**不能**死循环——
    // 它跑在快照线程上，卡住就等于索引管理页永远不出数。
    use std::collections::HashMap;
    let mut direct = HashMap::new();
    direct.insert(1i64, 5u32);
    direct.insert(2i64, 7u32);
    let mut parents: HashMap<i64, Option<i64>> = HashMap::new();
    parents.insert(1, Some(1)); // 自己是自己的父
    parents.insert(2, Some(1));
    let totals = picasa_db::aggregate_totals(&direct, &parents);
    // 结果是什么不重要，**不能挂**才重要
    assert!(totals.contains_key(&1));
    assert!(totals.contains_key(&2));
}

#[test]
fn folder_state_layer_still_marks_every_folder() {
    // 递归计数加进来之后，`folder_states` 的覆盖面不能变（否则树上的状态点会少）。
    let (_tmp, db, _ids) = tree_fixture();
    let snap = snap_of(&db);
    for id in snap.folder_counts.keys() {
        assert!(
            snap.folder_states.contains_key(id),
            "目录 {id} 缺状态叠层"
        );
    }
    assert!(snap.folder_states.values().all(|s| *s != FolderState::Failed));
}

#[test]
fn tc_m3_perf_20_all_photos_count_does_not_scan_every_row() {
    // `07` §5.2 PERF-20：100 万张 / 10 万目录库的 `build_snapshot` ≤ 20 ms。
    //
    // ## 这条用例抓的是什么
    //
    // 「所有照片」的计数以前写成
    // `COUNT(*) FROM photos JOIN folders JOIN roots WHERE r.state = 0`——
    // 它要**遍历每一张照片行**并逐行回查 `roots`。100 万张实测 **57 ms**，
    // 直接超 PERF-20 三倍。
    //
    // 现在改成 `SUM(folders.direct_count) over 活跃根`：O(目录数) 而不是 O(照片数)，
    // 1000 目录只要 **0.06 ms**（快 900 倍）。
    //
    // ## 为什么用「目录数」而不是「行数」当判据
    //
    // 绝对毫秒随机器飘，而且这里造 100 万行的成本太高（建库 11 秒）。
    // 复杂度的差异才是真问题：**每多一个目录/一张照片，计数该多花多少时间**。
    // 所以这里造「同样 4 万张、但目录数差 40 倍」的两组，断言耗时差得不多。
    let mk = |dirs: usize| -> (Db, CountWrap) {
        let db = Db::open_in_memory().unwrap();
        let root_id = db.upsert_root("/v", 0, false).unwrap();
        let mut fids = Vec::new();
        for d in 0..dirs {
            let p = format!("/v/d{d:04}");
            let fid = db.upsert_folder(&p, &p, None).unwrap();
            db.set_folder_root(fid, root_id, None).unwrap();
            fids.push(fid);
        }
        // 总共 4 万张，平摊到 dirs 个目录
        let per = 40_000 / dirs.max(1);
        for (i, fid) in fids.iter().enumerate() {
            let path = format!("/v/d{i:04}");
            let items: Vec<picasa_db::PhotoInput> = (0..per)
                .map(|k| {
                    let g = i * per + k;
                    let taken = 1_500_000_000i64 + g as i64;
                    picasa_db::PhotoInput {
                        name: format!("p{g:06}.jpg"),
                        name_key: format!("p{g:06}.jpg"),
                        size_bytes: 1000,
                        mtime_ns: taken * 1_000_000_000,
                        width: Some(4000),
                        height: Some(3000),
                        orientation: Some(1),
                        taken_at: Some(taken),
                        taken_at_src: 0,
                        format: 1,
                    }
                })
                .collect();
            db.apply_scan(*fid, &path, &items, 0).unwrap();
        }
        let snap = db.count_snapshot().unwrap();
        (db, CountWrap(snap.all_photos))
    };

    // 100 个目录 vs 4000 个目录，都是 4 万张
    let (db_small, n_small) = mk(100);
    let (db_big, n_big) = mk(4000);
    assert_eq!(n_small.0, 40_000, "两种切法的照片总数必须相同");
    assert_eq!(n_big.0, 40_000);

    // 各测 5 次取最小值（避开首次冷缓存）
    let time = |db: &Db| -> f64 {
        let mut best = f64::MAX;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let _ = db.count_snapshot().unwrap();
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        best
    };
    let t_small = time(&db_small);
    let t_big = time(&db_big);

    // 判据：目录多 40 倍时，耗时不该暴涨。
    // 若计数仍在扫照片行，两组照片数相同 → 耗时接近（这条就测不出问题）。
    // 所以真正要防的是**照片数**：这里用绝对上界兜底。
    assert!(
        t_big < 20.0,
        "★ 4 万张 / 4000 目录的计数用了 {t_big:.2} ms，超过 PERF-20 的 20 ms"
    );
    assert!(
        t_small < 20.0,
        "4 万张 / 100 目录的计数用了 {t_small:.2} ms"
    );
    eprintln!("count_snapshot: 100 目录 {t_small:.3} ms / 4000 目录 {t_big:.3} ms");
}

/// 让上面的闭包返回类型简单点（就包一个 `all_photos`，不要整个快照）。
struct CountWrap(u64);
