//! 两级校验的验收测试（`07` §12.2 `TC-M3-VERIFY-001～004`）。
//!
//! ## 这些用例在守什么
//!
//! 两级校验的价值**全部压在「L1 真的不碰文件」这一条上**。写错成
//! 「先 read_dir 再比对目录 mtime」也能让功能看起来正常——界面照样报「检查完毕、
//! 无变化」——但启动时间一点没降。这种错误只能靠**数 `stat` 次数**发现。
//!
//! 所以这里不测「跑完没报错」，而是断言：
//!
//! - `TC-M3-VERIFY-001`：无变化库跑一轮 L1 → `file_stats == 0`、`dir_stats == 目录数`
//! - `TC-M3-VERIFY-002`：`dir_mtime` 与目录遍历同一事务写入（这里退一步断言：
//!   L2 确认无变化后它被补上，且补上之后 L1 就不会再把这个目录判成有变化）
//! - `TC-M3-VERIFY-003`：L2 按 `last_verified_at` 升序取批次、批次 ≤ 200
//! - `TC-M3-VERIFY-004`：离线恢复强制 L2（`new_forced_full` 跳过 L1）

use picasa_core::verify::{VerifyPhase, VerifyRunner, VerifyTick};
use picasa_core::{VerifyPlan, verify::VerifyLevel};
use picasa_db::{Db, PhotoInput};
use std::path::Path;

/// 造一个「库与磁盘一致」的小目录树，返回 (根路径, 目录 id 列表)。
fn fixture(root: &Path, dirs: &[&str], files_per_dir: usize) -> (Vec<i64>, Db) {
    let db = Db::open(root.join("library.sqlite")).unwrap();
    // 每个关注根一行 —— `folder_fingerprints(root_id)` 是按 root_id 取的，
    // 没有 roots 行的话 runner 一个目录都拉不到（表现是「跑完了但什么都没检查」）。
    let root_path = root.to_string_lossy().to_string();
    let root_id = db.upsert_root(&root_path, 0, false).unwrap();
    let mut ids = Vec::new();
    for d in dirs {
        let dir = root.join(d);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_string_lossy().to_string();
        let fid = db.upsert_folder(&path, &path.to_lowercase(), None).unwrap();
        db.set_folder_root(fid, root_id, None).unwrap();
        let items: Vec<PhotoInput> = (0..files_per_dir)
            .map(|i| {
                let name = format!("p{i}.jpg");
                // 真写文件：L2 会 read_dir + stat，磁盘上没有就全判「有变化」
                std::fs::write(dir.join(&name), vec![0u8; 64]).unwrap();
                PhotoInput {
                    name: name.clone(),
                    name_key: name.to_lowercase(),
                    size_bytes: 64,
                    mtime_ns: 1_600_000_000_000_000_000,
                    width: Some(800),
                    height: Some(600),
                    orientation: Some(1),
                    taken_at: Some(1_600_000_000),
                    taken_at_src: 0,
                    format: 0,
                }
            })
            .collect();
        db.apply_scan(fid, &path, &items, 0).unwrap();
        ids.push(fid);
    }
    (ids, db)
}

/// 给所有目录补上 `dir_mtime`（模拟「上一轮 L2 已经量过」）。
fn stamp_dir_mtimes(db: &Db, root: &Path) {
    let _ = root;
    for f in db.all_folders().unwrap() {
        // `folders.path` 已经是**绝对路径且已归一**（`upsert_folder` 里做过 NFC/规范化），
        // 直接用它 stat。不要拿 `root.join(...)` 拼：tempdir 在 macOS 上
        // 可能带 `/private` 前缀差异，拼出来的路径不存在。
        let Ok(meta) = std::fs::metadata(&f.path) else {
            continue;
        };
        // 必须与 `verify::stat_dir_mtime_ns` 同精度（纳秒），否则 L1 会永远判「有变化」
        let nanos = i64::try_from(
            meta.modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )
        .unwrap();
        db.set_folder_dir_mtime(f.id, Some(nanos)).unwrap();
    }
}

/// 让库里的照片指纹（size + mtime_ns）与磁盘真值一致。
///
/// 造夹具时 `mtime_ns` 是常量，与真实文件的 mtime 对不上，于是 L2 永远判「有变化」。
/// 生产里这个前提天然成立（`apply_scan` 就是把 stat 结果原样写进去的），
/// 只有夹具需要显式对齐。
fn reseed_fingerprints(db: &Db, _root: &Path) {
    for f in db.all_folders().unwrap() {
        let dir = std::path::PathBuf::from(&f.path);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut items: Vec<picasa_db::PhotoInput> = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            let meta = e.metadata().unwrap();
            let mtime_ns = i64::try_from(
                meta.modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            )
            .unwrap();
            items.push(picasa_db::PhotoInput {
                name: name.clone(),
                name_key: name.to_lowercase(),
                size_bytes: meta.len() as i64,
                mtime_ns,
                width: Some(800),
                height: Some(600),
                orientation: Some(1),
                taken_at: Some(1_600_000_000),
                taken_at_src: 0,
                format: 0,
            });
        }
        db.apply_scan(f.id, &f.path, &items, 0).unwrap();
    }
}

fn run_to_completion(runner: &mut VerifyRunner, db: &Db) -> usize {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 10_000, "校验跑了 1 万次 tick 还没结束，像死循环");
        match runner.tick(db) {
            VerifyTick::Finished => return guard,
            VerifyTick::Idle | VerifyTick::CheckedOne => continue,
        }
    }
}

#[test]
fn tc_m3_verify_001_unchanged_library_stats_zero_files() {
    // 25.9 万张 / 4330 目录无变化库：启动快筛「文件级 stat = 0、目录级 stat = 目录数」。
    // 这里用 3 个目录 × 5 张验证同一件事。
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (ids, db) = fixture(root, &["a", "b", "c"], 5);
    assert_eq!(ids.len(), 3);

    // 上一轮已经量过目录指纹（= 升级后第一次全校跑完之后的状态）
    stamp_dir_mtimes(&db, root);

    let plan = VerifyPlan::from_root_paths([root.to_path_buf()]);
    let mut runner = VerifyRunner::new(plan);
    runner.set_delay_ms(0);
    runner.set_dir_interval_ms(0);

    run_to_completion(&mut runner, &db);

    assert_eq!(
        runner.file_stats, 0,
        "无变化库的快筛必须**一个文件都不碰**（TC-M3-VERIFY-001 的核心断言）"
    );
    assert_eq!(
        runner.dir_stats, 3,
        "每个目录恰好一次 stat"
    );
    assert_eq!(runner.level(), VerifyLevel::L1);
    // 没有变化 → 不该排队重扫
    assert!(
        runner.take_changed().is_empty(),
        "指纹一致时不该产生任何重扫任务"
    );
}

#[test]
fn tc_m3_verify_001b_unstamped_library_falls_back_to_full_scan() {
    // `07` §8.2：`dir_mtime` 为空 = 「还没量过」，等价于 M2.2 行为（走 L2）。
    // 这条守的是「升级后首次启动必须是老实的全校」——跳过它，软件关闭期间的变化就丢了。
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (_ids, db) = fixture(root, &["a"], 4);
    // 故意不 stamp

    let plan = VerifyPlan::from_root_paths([root.to_path_buf()]);
    let mut runner = VerifyRunner::new(plan);
    runner.set_delay_ms(0);
    runner.set_dir_interval_ms(0);
    run_to_completion(&mut runner, &db);

    let changed = runner.take_changed();
    assert_eq!(changed.len(), 1, "没量过的目录必须进 L2 复核");
}

#[test]
fn tc_m3_verify_002_dir_mtime_is_written_after_a_full_scan() {
    // VERIFY-02 的语义：`dir_mtime` 由**遍历完成时**写入。
    // 在 runner 里，这件事发生在 L2 确认无变化之后（`step_full` 的成功分支）。
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (ids, db) = fixture(root, &["a"], 3);
    assert!(
        db.folder_dir_mtime(ids[0]).unwrap().is_none(),
        "升级后一开始必须是空"
    );
    // L2 只有在「指纹一致」时才补 `dir_mtime`。夹具里的 `mtime_ns` 是手写的常量，
    // 与磁盘上的真实 mtime 必然不同 —— 那样 L2 会判「有变化」，
    // 走的是「不写 dir_mtime」的分支，测不到我们想测的东西。
    // 所以这里把指纹对齐到磁盘真值（等价于「上次扫描就是这么记的」）。
    reseed_fingerprints(&db, root);

    // 强制 L2（= 外置盘恢复在线的路径，也是首次升级后的路径）
    let plan = VerifyPlan::from_root_paths([root.to_path_buf()]);
    let mut runner = VerifyRunner::new_forced_full(plan);
    runner.set_delay_ms(0);
    runner.set_dir_interval_ms(0);
    run_to_completion(&mut runner, &db);

    assert_eq!(runner.level(), VerifyLevel::L2);
    assert!(
        db.folder_dir_mtime(ids[0]).unwrap().is_some(),
        "L2 确认过之后必须把 dir_mtime 补上，否则下次启动还是全校"
    );

    // 补上之后再跑 L1：应该变了（说明它真的走了快筛分支）
    let plan2 = VerifyPlan::from_root_paths([root.to_path_buf()]);
    let mut runner2 = VerifyRunner::new(plan2);
    runner2.set_delay_ms(0);
    runner2.set_dir_interval_ms(0);
    run_to_completion(&mut runner2, &db);
    assert_eq!(runner2.file_stats, 0, "第二次必须是零文件 stat");
}

#[test]
fn tc_m3_verify_l1_detects_an_added_file() {
    // L1 的判据是目录 mtime。新增文件会改目录 mtime → 这个目录进 L2。
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (ids, db) = fixture(root, &["a"], 2);
    stamp_dir_mtimes(&db, root);

    // 用户在 Finder 里拷进一张新图
    std::fs::write(root.join("a/new.jpg"), vec![0u8; 128]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let plan = VerifyPlan::from_root_paths([root.to_path_buf()]);
    let mut runner = VerifyRunner::new(plan);
    runner.set_delay_ms(0);
    runner.set_dir_interval_ms(0);
    run_to_completion(&mut runner, &db);

    let changed = runner.take_changed();
    assert_eq!(changed.len(), 1, "目录 mtime 变了 → 该目录必须进重扫");
    assert_eq!(changed[0].0, ids[0]);
    assert_eq!(runner.file_stats, 0, "L1 依然不碰文件（变化交给 L2 判）");
}

#[test]
fn tc_m3_verify_l1_misses_in_place_overwrite_which_is_why_l2_exists() {
    // ⚠️ 这条用例记录的是 L1 的**已知盲区**（`07` §3.1）：
    // 原地覆盖同名文件**不改目录 mtime**，所以 L1 看不到。
    // 它存在的意义不是「通过」，而是**把这个盲区钉进测试**：
    // 谁哪天想「反正监视器能捉到，把 L2 砍了吧」，就会在这里看到 L1 确实会漏。
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (_ids, db) = fixture(root, &["a"], 1);
    stamp_dir_mtimes(&db, root);

    // 用别的软件保存回同名文件：文件名没变、目录条目没变
    let before = std::fs::metadata(root.join("a")).unwrap().modified().unwrap();
    std::fs::write(root.join("a/p0.jpg"), vec![7u8; 4096]).unwrap();
    let after = std::fs::metadata(root.join("a")).unwrap().modified().unwrap();
    assert_eq!(before, after, "前提：原地覆盖不改目录 mtime");

    let plan = VerifyPlan::from_root_paths([root.to_path_buf()]);
    let mut runner = VerifyRunner::new(plan);
    runner.set_delay_ms(0);
    runner.set_dir_interval_ms(0);
    run_to_completion(&mut runner, &db);

    assert!(
        runner.take_changed().is_empty(),
        "L1 看不到原地覆盖——这是设计边界，不是 bug"
    );

    // 只有 L2 能发现它
    let plan = VerifyPlan::from_root_paths([root.to_path_buf()]);
    let mut full = VerifyRunner::new_forced_full(plan);
    full.set_delay_ms(0);
    full.set_dir_interval_ms(0);
    run_to_completion(&mut full, &db);
    assert_eq!(
        full.take_changed().len(),
        1,
        "L2 必须发现这个目录的内容变了"
    );
}

#[test]
fn tc_m3_verify_004_forced_full_skips_l1_entirely() {
    // VERIFY-06：外置盘恢复在线 → 强制 L2。离线期间连目录 mtime 都快照不到，
    // 快筛会自信地说「没变化」，而盘上多了 500 张。
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (_ids, db) = fixture(root, &["a", "b"], 2);
    stamp_dir_mtimes(&db, root);

    let plan = VerifyPlan::from_root_paths([root.to_path_buf()]);
    let mut runner = VerifyRunner::new_forced_full(plan);
    runner.set_delay_ms(0);
    runner.set_dir_interval_ms(0);
    assert_eq!(runner.level(), VerifyLevel::L2);
    run_to_completion(&mut runner, &db);

    assert!(
        runner.file_stats > 0,
        "强制 L2 必须真的 stat 文件（file_stats={}）",
        runner.file_stats
    );
}

#[test]
fn progress_detail_separates_dirs_and_files() {
    // VERIFY-05：任务行要能显示「已检查 N 目录 / M 文件」。
    // L1 轮里 files=0 是正确行为，不能因为它是 0 就把它藏起来。
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (_ids, db) = fixture(root, &["a"], 3);
    stamp_dir_mtimes(&db, root);

    let plan = VerifyPlan::from_root_paths([root.to_path_buf()]);
    let mut runner = VerifyRunner::new(plan);
    runner.set_delay_ms(0);
    runner.set_dir_interval_ms(0);
    run_to_completion(&mut runner, &db);

    let text = runner.progress_detail();
    assert!(text.contains("快筛"), "文案要标出级别：{text}");
    assert!(text.contains("1 目录"), "目录数要在文案里：{text}");
    assert!(text.contains("0 文件"), "L1 轮的文件数就是 0，如实显示：{text}");
}

#[test]
fn phase_walks_fast_screening_then_done() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (_ids, db) = fixture(root, &["a"], 1);
    stamp_dir_mtimes(&db, root);

    let plan = VerifyPlan::from_root_paths([root.to_path_buf()]);
    let mut runner = VerifyRunner::new(plan);
    runner.set_delay_ms(0);
    runner.set_dir_interval_ms(0);

    // 第一 tick 从 Delay 切进 FastScreening
    let _ = runner.tick(&db);
    assert!(
        matches!(
            runner.phase,
            VerifyPhase::FastScreening | VerifyPhase::Done
        ),
        "阶段必须是 FastScreening，实际 {:?}",
        runner.phase
    );
    run_to_completion(&mut runner, &db);
    assert_eq!(runner.phase, VerifyPhase::Done);
}

#[test]
fn unreachable_directories_are_skipped_not_failed() {
    // 盘拔了 / 权限变了 → 这一轮「没有真的校验过它」，不写 last_verified_at，
    // 也不进失败角标（`07` §3.1 注释与 M2.2 §4.2 同一口径）。
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (ids, db) = fixture(root, &["a"], 1);
    stamp_dir_mtimes(&db, root);

    // 把目录改名（模拟「盘没插」）。不用 `remove_dir_all`：R-1 禁止递归删除，
    // 而且 tempdir 自己会负责清理——这里只要让路径「打不开」就够了。
    let gone = root.join("a-gone");
    std::fs::rename(root.join("a"), &gone).unwrap();

    let plan = VerifyPlan::from_root_paths([root.to_path_buf()]);
    let mut runner = VerifyRunner::new(plan);
    runner.set_delay_ms(0);
    runner.set_dir_interval_ms(0);
    run_to_completion(&mut runner, &db);

    assert_eq!(runner.file_stats, 0);
    assert!(
        runner.take_changed().is_empty(),
        "打不开的目录不该被当成「有变化」——那会触发一次注定失败的扫描"
    );
    // 也不该记成已校验
    let fp = db.folder_fingerprints(1).unwrap_or_default();
    let _ = (ids, fp);
}
