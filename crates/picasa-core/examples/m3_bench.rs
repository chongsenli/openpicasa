//! M3 的规模与实时性基准（`07` §12.1 `perf_bench` 的新参数）。
//!
//! 用法：
//!
//! ```text
//! cargo run --release -p picasa-core --example m3_bench -- --all-photos 1000000
//! cargo run --release -p picasa-core --example m3_bench -- --watch-copy 1000
//! cargo run --release -p picasa-core --example m3_bench -- --scroll 1000000
//! cargo run --release -p picasa-core --example m3_bench -- --rows 40000
//! ```
//!
//! ## 这个工具测什么、不测什么
//!
//! | 测 | 不测 |
//! | --- | --- |
//! | 快照构建（`count_snapshot` 的聚合） | 真实 GPU 上传 |
//! | 键集分页的 p50/p95/p99 | 帧率（无 `/dev/dri`） |
//! | 组边界构造 | 字体光栅化 |
//! | 逐行窗口 `range()` 的命中/miss 成本 | 磁盘 IO 的真实延迟（这里用内存库） |
//!
//! 明确不拿它冒充「FPS 已达标」——与 `c_bench` 的分工一致。
//!
//! ## 为什么可以造 100 万行
//!
//! 用内存库 + `apply_scan` 批量插，100 万行在 release 下约 20～40 秒、占 1 GB 左右。
//! **不写任何文件到照片目录**（R-1）：所有"照片"只是数据库里的行，
//! 没有对应的真实文件——这足够测「数据源与布局」这一层的代价，
//! 而那正是 `07` §5.1 的 100 万张门禁要看的。

use std::time::Instant;

use picasa_core::datasource::{DataSource, Scope};
use picasa_db::{CountSnapshot, Db, PhotoInput, SortKey, SortSpec};
use std::sync::{Arc, Mutex};

/// 一个「虚拟库」：内存库 + N 行照片 + 若干目录。
struct VirtualLib {
    db: Arc<Mutex<Db>>,
    folder_id: i64,
    counts: CountSnapshot,
}

/// 造一个含 `rows` 张照片的虚拟库。
///
/// `dirs` 个目录平均分摊。照片名与时间戳都确定性生成（便于复现）。
fn build_virtual(rows: usize, dirs: usize) -> VirtualLib {
    let db = Db::open_in_memory().expect("内存库");
    let root_id = db.upsert_root("/virtual", 0, false).unwrap();
    let mut fids = Vec::with_capacity(dirs);
    for d in 0..dirs {
        let path = format!("/virtual/d{d:04}");
        let fid = db.upsert_folder(&path, &path, None).unwrap();
        db.set_folder_root(fid, root_id, None).unwrap();
        fids.push(fid);
    }

    // 分批插，避免一次构造 100 万条 PhotoInput 占满内存
    const BATCH: usize = 20_000;
    let mut inserted = 0usize;
    while inserted < rows {
        let n = BATCH.min(rows - inserted);
        // 每批落到第 (inserted / BATCH) % dirs 个目录
        let fid = fids[(inserted / BATCH) % fids.len()];
        let path = format!("/virtual/d{:04}", (inserted / BATCH) % fids.len());
        let items: Vec<PhotoInput> = (0..n)
            .map(|i| {
                let g = inserted + i;
                // 时间戳按天递增（每 100 张一天），这样日期分组有 ~rows/100 组
                let taken = 1_500_000_000i64 + (g as i64 / 100) * 86_400;
                PhotoInput {
                    name: format!("p{g:07}.jpg"),
                    name_key: format!("p{g:07}.jpg"),
                    size_bytes: 100_000 + (g as i64 % 1000),
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
        db.apply_scan(fid, &path, &items, 0).unwrap();
        inserted += n;
    }

    let counts = db.count_snapshot().unwrap();
    VirtualLib {
        db: Arc::new(Mutex::new(db)),
        folder_id: fids[0],
        counts,
    }
}

/// 百分位（`sorted` 必须已排序）。
fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn report(label: &str, mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.total_cmp(b));
    let p50 = pct(&samples, 0.50);
    let p95 = pct(&samples, 0.95);
    let p99 = pct(&samples, 0.99);
    let max = samples.last().copied().unwrap_or(0.0);
    println!(
        "{label:<34} n={n:<6} p50={p50:>7.2}ms p95={p95:>7.2}ms p99={p99:>7.2}ms max={max:>7.2}ms",
        n = samples.len()
    );
    p99
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get_num = |flag: &str| -> Option<usize> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
    };
    let has = |flag: &str| args.iter().any(|a| a == flag);

    if has("--help") || args.is_empty() {
        println!(
            "用法：\n\
             \x20 --all-photos <N>   建 N 行的虚拟库，测「所有照片」首屏与组边界\n\
             \x20 --scroll <N>       建 N 行，滚动采样（每 40 行取一次 range）\n\
             \x20 --rows <N>         只测快照构建与窗口缓存（N 行）\n\
             \x20 --watch-copy <N>   模拟拷 N 张期间的去抖与刷新投递\n\
             \x20 --dirs <N>         目录数（默认 100）"
        );
        return;
    }
    let dirs = get_num("--dirs").unwrap_or(100);
    let rows = get_num("--all-photos")
        .or_else(|| get_num("--scroll"))
        .or_else(|| get_num("--rows"))
        .unwrap_or(100_000);

    println!("== M3 规模基准 == ");
    println!("规模 {rows} 行 / {dirs} 个目录（内存库，无真实文件）");

    // ---- 建库
    let t = Instant::now();
    let lib = build_virtual(rows, dirs);
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("建库耗时 {build_ms:.0} ms");

    // ---- 快照构建（PERF-20：100 万张 / 10 万目录 ≤ 20 ms）
    {
        let mut samples = Vec::new();
        {
            let db = lib.db.lock().unwrap();
            for _ in 0..20 {
                let t = Instant::now();
                let _ = db.count_snapshot().unwrap();
                samples.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        let p99 = report("count_snapshot（计数聚合）", samples);
        let limit = 20.0;
        println!(
            "  {} PERF-20 上限 {limit} ms（p99 {p99:.2}）",
            if p99 <= limit { "✅" } else { "❌" }
        );
    }

    // ---- 数据源构造（含组边界聚合）
    {
        let mut samples = Vec::new();
        for _ in 0..10 {
            let t = Instant::now();
            let _ds = DataSource::new(
                lib.db.clone(),
                Scope::Folder(lib.folder_id),
                SortSpec::default(),
                1,
                &lib.counts,
            )
            .unwrap();
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        report("DataSource::new（组边界聚合）", samples);
    }

    let mut ds = DataSource::new(
        lib.db.clone(),
        Scope::Folder(lib.folder_id),
        SortSpec::default(),
        1,
        &lib.counts,
    )
    .unwrap();
    println!("组边界 {} 组 / 整列 {} 行", ds.groups().len(), ds.len());

    // ---- --all-photos：全库 scope 的首屏与组边界
    if has("--all-photos") {
        let t = Instant::now();
        let mut all = DataSource::new(
            lib.db.clone(),
            Scope::AllPhotos,
            SortSpec::default(),
            1,
            &lib.counts,
        )
        .unwrap();
        let construct_ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("「所有照片」DataSource 构造 {construct_ms:.1} ms（{} 组 / {} 行）", all.groups().len(), all.len());

        // 首屏：第 0 行起取 4 屏
        let t = Instant::now();
        let _ = all.range(0, 240).unwrap();
        let first_ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "  首屏 240 行 {first_ms:.2} ms  {} 07 §5.1 的 1.0 s",
            if first_ms <= 1000.0 { "✅" } else { "❌" }
        );
    }

    // ---- --scroll：滚动采样
    if has("--scroll") || has("--all-photos") {
        let mut samples = Vec::new();
        let mut start = 0usize;
        let step = 40;
        let mut guard = 0;
        while start < ds.len() && guard < 2000 {
            guard += 1;
            let t = Instant::now();
            let _ = ds.range(start, 60).unwrap();
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
            start += step;
        }
        let p99 = report("range(60) 顺序滚动", samples);
        // 07 §5.1：25 万张 p99 ≤ 25 ms；100 万张只要求 max ≤ 500 ms（不假死）
        let is_million = rows >= 1_000_000;
        let limit = if is_million { 500.0 } else { 25.0 };
        println!(
            "  {} 07 §5.1 {} 门禁 {limit} ms（p99 {p99:.2}）",
            if p99 <= limit { "✅" } else { "❌" },
            if is_million { "100 万张不假死" } else { "25 万张" }
        );

        // 随机跳转（07 §5.1 的 --jump）
        let mut samples = Vec::new();
        let mut rng = 0x1234_5678u64;
        for _ in 0..40 {
            rng ^= rng >> 12;
            rng ^= rng << 25;
            rng ^= rng >> 27;
            let start = (rng % ds.len().max(1) as u64) as usize;
            let t = Instant::now();
            let _ = ds.range(start, 60).unwrap();
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let p95 = {
            let mut s = samples.clone();
            s.sort_by(|a, b| a.total_cmp(b));
            pct(&s, 0.95)
        };
        report("range(60) 随机跳转", samples);
        let limit = if is_million { 2000.0 } else { 500.0 };
        println!(
            "  {} 07 §5.1 跳转门禁 {limit} ms（p95 {p95:.2}）",
            if p95 <= limit { "✅" } else { "❌" }
        );
    }

    // ---- --watch-copy：模拟拷贝期间的刷新投递
    if let Some(n) = get_num("--watch-copy") {
        use picasa_core::watcher::{DebounceOut, Debouncer, WATCH_MAX_DEFER_MS};
        let mut d = Debouncer::new();
        let base = Instant::now();
        let mut deliveries = 0usize;
        let mut storm = 0usize;
        // 1000 张拷进同一个目录：每 10 ms 一个事件，共 n 个
        for i in 0..n {
            let at = base + std::time::Duration::from_millis((i as u64) * 10);
            d.push_at(std::path::PathBuf::from("/virtual/d0000"), at, |_| false);
            if let Some(out) = d.drain_due(at) {
                deliveries += 1;
                if out == DebounceOut::RootStorm {
                    storm += 1;
                }
            }
        }
        let elapsed_ms = base.elapsed().as_secs_f64() * 1000.0;
        println!("\n--watch-copy {n}：投递 {deliveries} 次（风暴折叠 {storm} 次）/ 模拟耗时 {elapsed_ms:.0} ms");
        // 07 §2.2 WATCH-02 的上界：投递数 ≤ n 事件 / 窗口数 + 1
        let upper = (elapsed_ms / 500.0).ceil() as usize + 1;
        println!(
            "  {} 去抖上界 {upper} 次（实际 {deliveries}）",
            if deliveries <= upper { "✅" } else { "❌" }
        );
        println!(
            "  {} max_defer {WATCH_MAX_DEFER_MS} ms 内有投递",
            if deliveries > 0 { "✅" } else { "❌" }
        );
    }

    // ---- 顺带报一下键集分页的执行计划（走没走索引）
    {
        let db = lib.db.lock().unwrap();
        let plan = db.explain_keyset_plan(SortSpec::default()).unwrap_or_default();
        let text = plan.join(" | ");
        println!(
            "\n键集计划：{} {}",
            if text.contains("SCAN p") { "❌ 全表扫" } else { "✅ 走索引" },
            text
        );
        let _ = (SortKey::TakenAt,);
    }
}
