//! 大子树删除的性能与标记守恒（M4.3 R-M43-02 / R-M43-04）。
//!
//! ## 为什么有这条测试
//!
//! 用户在真实库（9.7 万张照片）上点「删除索引」，界面冻结了约 50 秒。
//! 日志显示删除线程本身一秒内就跑完了，UI 事件循环却在 `MonitorSync` 之间
//! 断了 50 秒——因为 `clear_index_for_root` 拿着 DB 锁的时间太长，UI 每帧都要抢它。
//!
//! 根因是 `snapshot_for_folder` 逐张照片跑 6 条 SQL：9.6 万张 ≈ 58 万条语句
//! 挤在一个事务里。改成集合式 `INSERT ... SELECT` 后整批只有 5 条语句。
//!
//! 这条测试同时锁住两件事：
//! 1. **标记守恒**：整棵子树里每张带标记的照片在删除后都能按身份找回来
//!    （逐张快照改集合式时最容易写错的就是这里）；
//! 2. **规模**：删一个有大量照片的子树要够快，不能让 UI 等五十秒。
//!
//! 阈值取 8 秒：真机 9.6 万张改前约 50 秒、改后不到 1 秒；这里用 2 万张做样本，
//! 留足机器差异的余量，只用来拦「退回到逐张循环」这种量级回归。

use picasa_db::{Db, PhotoInput, SortSpec};
use std::time::Instant;

fn input(name: &str, taken: i64) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_lowercase(),
        size_bytes: 1000,
        mtime_ns: taken,
        width: Some(800),
        height: Some(600),
        orientation: Some(1),
        taken_at: Some(taken),
        taken_at_src: 0,
        format: 1,
    }
}

#[test]
fn clearing_a_large_subtree_is_fast_and_keeps_every_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("library.sqlite")).unwrap();

    // ---- 造一个 2 万张、200 个目录的根 -------------------------------------
    const DIRS: i64 = 200;
    const PER_DIR: i64 = 100;
    let root = "/big";
    let rid = db.upsert_root(root, 1, false).unwrap();
    for d in 0..DIRS {
        let path = format!("{root}/d{d:03}");
        let fid = db.upsert_folder(&path, &path, None).unwrap();
        let items: Vec<PhotoInput> = (0..PER_DIR)
            .map(|i| input(&format!("p{i:03}.jpg"), 1_300_000_000 + d * 1000 + i))
            .collect();
        db.apply_scan(fid, &path, &items, 0).unwrap();
    }

    // ---- 给一部分照片打上各类标记 ------------------------------------------
    // 挑 300 张：跨目录、跨标签、跨相册，确保集合式快照没有漏掉任何一种。
    let all: Vec<i64> = db
        .photos(None, SortSpec::default(), 100_000)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(all.len() as i64, DIRS * PER_DIR, "夹具应灌满 2 万张");
    // 注意 `step_by(67)` 在 2 万条上取不满 300 个（20000 / 67 = 298.5），
    // 后面的断言一律用 `starred.len()` 而不是写死 300，否则测的是算术不是代码。
    let starred: Vec<i64> = all.iter().copied().step_by(67).take(300).collect();
    db.set_starred(&starred, true).unwrap();
    db.add_tag(&starred, "旅行").unwrap();
    db.add_tag(&starred[..100], "京都").unwrap();
    db.set_caption(starred[0], "第一张").unwrap();
    let album = db.create_album("精选", 1).unwrap();
    db.add_to_album(album, &starred[..50], 1).unwrap();

    let markers_before = db.marker_count_for_root(rid).unwrap().unwrap();
    assert_eq!(markers_before, starred.len() as i64, "标记数应等于被打标的张数");

    // ---- 计时删除 ----------------------------------------------------------
    let t = Instant::now();
    let (photos, _thumbs) = db.clear_index_for_root(rid).unwrap();
    let elapsed = t.elapsed();
    println!(
        "清空 {photos} 张 / {DIRS} 个目录用时 {elapsed:.2?}（阈值 8s）"
    );
    assert_eq!(photos, (DIRS * PER_DIR) as u64);
    assert!(
        elapsed.as_secs() < 8,
        "删除 {photos} 张用了 {elapsed:.2?}：疑似退回到逐张快照的写法"
    );

    // ---- 索引确实清干净了 --------------------------------------------------
    assert_eq!(db.total_photos().unwrap(), 0);
    assert!(
        db.marker_count_for_root(rid).unwrap().unwrap() == 0,
        "子树删掉后该 root 已无照片"
    );

    // ---- 标记守恒：重新扫描同一批路径，标记必须按身份挂回来 -----------------
    // 重建时用不同的 mtime（模拟照片被重新导出过），只有 (folder_path, name_key)
    // 这一身份能对上，正好验证「不依赖旧 id」。
    for d in 0..DIRS {
        let path = format!("{root}/d{d:03}");
        let fid = db.upsert_folder(&path, &path, None).unwrap();
        let items: Vec<PhotoInput> = (0..PER_DIR)
            .map(|i| input(&format!("p{i:03}.jpg"), 1_700_000_000 + d * 1000 + i))
            .collect();
        db.apply_scan(fid, &path, &items, 0).unwrap();
    }
    assert_eq!(db.total_photos().unwrap(), (DIRS * PER_DIR) as u64);

    let restored_markers = db.marker_count_for_root(rid).unwrap().unwrap();
    assert_eq!(
        restored_markers, markers_before,
        "重扫后带标记的照片数必须与删除前一致（逐张快照改集合式最容易在这里漏）"
    );

    let travel = db.tag_counts_visible(None).unwrap();
    let travel = travel
        .iter()
        .find(|(label, _)| label == "旅行")
        .map(|(_, n)| *n)
        .unwrap_or(0);
    assert_eq!(
        travel,
        starred.len() as i64,
        "「旅行」标签的照片数必须全部恢复"
    );
    let kyoto = db
        .tag_counts_visible(None)
        .unwrap()
        .iter()
        .find(|(label, _)| label == "京都")
        .map(|(_, n)| *n)
        .unwrap_or(0);
    assert_eq!(kyoto, 100, "「京都」只打给前 100 张，恢复后也不能多");
    assert_eq!(
        db.starred_ids().unwrap().len(),
        starred.len(),
        "星标必须全部恢复"
    );
}

#[test]
fn clearing_a_root_without_markers_stays_cheap() {
    // 没有标记的大子树走的是同一条 SQL 路径，只是骨架表最终没有行。
    // 单独跑一遍，确保 `INSERT ... SELECT` 在空结果时不报错、也不写脏行。
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("library.sqlite")).unwrap();
    let rid = db.upsert_root("/plain", 1, false).unwrap();
    let path = "/plain/a";
    let fid = db.upsert_folder(path, path, None).unwrap();
    let items: Vec<PhotoInput> = (0..500)
        .map(|i| input(&format!("q{i:03}.jpg"), 1_300_000_000 + i))
        .collect();
    db.apply_scan(fid, path, &items, 0).unwrap();

    let (photos, _) = db.clear_index_for_root(rid).unwrap();
    assert_eq!(photos, 500);
    assert_eq!(db.total_photos().unwrap(), 0);
    // 骨架表里应当只有身份行、没有任何标签/相册行。
    assert_eq!(
        db.tag_counts_visible(None).unwrap(),
        Vec::<(String, i64)>::new(),
        "没有标签的子树不该凭空生出标签骨架"
    );
}

/// 真实量级的手工跑法（默认跳过）：用户库约 9.7 万张 / 941 个目录。
///
/// ```text
/// cargo test -p picasa-db --release --test m43_clear_scope_perf -- \
///   --ignored --nocapture clearing_a_hundred_thousand_photos
/// ```
#[test]
#[ignore = "10 万张要几十秒建库，手工跑"]
fn clearing_a_hundred_thousand_photos() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("library.sqlite")).unwrap();
    const DIRS: i64 = 950;
    const PER_DIR: i64 = 105;
    let rid = db.upsert_root("/huge", 1, false).unwrap();
    let t = Instant::now();
    for d in 0..DIRS {
        let path = format!("/huge/d{d:04}");
        let fid = db.upsert_folder(&path, &path, None).unwrap();
        let items: Vec<PhotoInput> = (0..PER_DIR)
            .map(|i| input(&format!("p{i:03}.jpg"), 1_300_000_000 + d * 1000 + i))
            .collect();
        db.apply_scan(fid, &path, &items, 0).unwrap();
    }
    let total = db.total_photos().unwrap();
    println!("建库 {total} 张用时 {:.1?}", t.elapsed());

    let all: Vec<i64> = db
        .photos(None, SortSpec::default(), 200_000)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    let starred: Vec<i64> = all.iter().copied().step_by(97).collect();
    db.set_starred(&starred, true).unwrap();
    db.add_tag(&starred, "旅行").unwrap();
    println!("打标 {} 张", starred.len());

    let t = Instant::now();
    let (photos, thumbs) = db.clear_index_for_root(rid).unwrap();
    let elapsed = t.elapsed();
    println!("清空 {photos} 张 / {thumbs} 张缩略图用时 {elapsed:.2?}");
    assert!(
        elapsed.as_secs() < 8,
        "{photos} 张用了 {elapsed:.2?}：疑似退回逐张快照"
    );
    assert_eq!(db.total_photos().unwrap(), 0);

    // 重扫后标记必须全回来
    for d in 0..DIRS {
        let path = format!("/huge/d{d:04}");
        let fid = db.upsert_folder(&path, &path, None).unwrap();
        let items: Vec<PhotoInput> = (0..PER_DIR)
            .map(|i| input(&format!("p{i:03}.jpg"), 1_700_000_000 + d * 1000 + i))
            .collect();
        db.apply_scan(fid, &path, &items, 0).unwrap();
    }
    assert_eq!(
        db.starred_ids().unwrap().len(),
        starred.len(),
        "10 万张规模下星标也必须全部恢复"
    );
}
