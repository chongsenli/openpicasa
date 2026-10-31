//! C 工作包的性能基线（无窗口，纯数据层与布局层）。
//!
//! 用法：
//!
//! ```text
//! cargo run --release --example c_bench            # 默认 5 万张
//! cargo run --release --example c_bench -- 200000  # 20 万张
//! ```
//!
//! **为什么要有这个**：C 的出口标准是「5 万张下首屏与 55 FPS 达标」，但
//! 沙箱里没有 `/dev/dri`、窗口起不来，FPS 没法直接测。能测准的是**每帧工作量的主体**：
//! 分页查询、日期分组、布局重建、可见区间计算。这几项一旦超预算，任何 GPU 都救不回来
//! ——它们跑在 UI 线程上，和帧率是同一根时间轴。
//!
//! 测不出来的部分（真实 GPU 上传、字体光栅化）明确不算在这个工具里，
//! 交付说明里也不会拿它冒充"FPS 已达标"。

use std::time::{Duration, Instant};

use picasa_core::Tier;
use picasa_core::library::group_by_day;
use picasa_db::{Db, PhotoInput, SortSpec};
use picasa_ui::grid::GridLayout;

/// 一帧的预算（60fps）。55fps 的门槛是 18.2ms，这里按 16.6ms 更严地要求。
const FRAME_BUDGET: Duration = Duration::from_micros(16_600);

fn timed<T>(label: &str, mut f: impl FnMut() -> T) -> (T, Duration) {
    let t0 = Instant::now();
    let v = f();
    let d = t0.elapsed();
    let verdict = if d <= FRAME_BUDGET { "OK" } else { "超预算" };
    println!("  {label:<34} {:>9.2} ms   {verdict}", d.as_secs_f64() * 1000.0);
    (v, d)
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50_000);

    println!("== C 基线：{n} 张照片 ==");

    let tmp = tempdir();
    let db_path = tmp.join("bench.sqlite");
    let db = Db::open(&db_path).expect("建库失败");

    // ---- 造数据：8 个目录，时间跨 5 年（每天约 27 张，分组数 ~1800）----
    let t_fill = Instant::now();
    let folders = 8usize;
    let per = n / folders;
    let mut folder_ids = Vec::new();
    for i in 0..folders {
        let path = format!("/bench/volume/photos/album-{i:02}/roll");
        let id = db.upsert_folder(&path, &path.to_lowercase(), None).unwrap();
        folder_ids.push(id);
    }
    let base_ts: i64 = 1_300_000_000; // 2011-03
    for (fi, fid) in folder_ids.iter().enumerate() {
        let mut items = Vec::with_capacity(per);
        for k in 0..per {
            let global = fi * per + k;
            let name = format!("IMG_{global:06}_DSCF{}.jpg", global % 9973);
            items.push(PhotoInput {
                name_key: name.to_lowercase(),
                name,
                size_bytes: 2_000_000 + (global as i64 % 700) * 1000,
                mtime_ns: 1_600_000_000_000_000_000 + global as i64,
                width: Some(4000),
                height: Some(3000),
                orientation: Some(1),
                // 每天 27 张，均匀铺开
                taken_at: Some(base_ts + (global as i64 / 27) * 86_400),
                taken_at_src: 0,
            });
        }
        db.apply_scan(*fid, &format!("/bench/album-{fi}"), &items, 0)
            .unwrap();
    }
    println!(
        "  造数据 {:>31} {:>9.2} s（不计入基线）\n",
        "",
        t_fill.elapsed().as_secs_f64()
    );

    // ---- 1. 首屏：把整个视图读进 Vec<PhotoRow> ----
    println!("[1] 首屏查询（冷）");
    let (photos, _) = timed("photos(limit=200_000) 全量", || {
        db.photos(None, SortSpec::default(), 200_000).unwrap()
    });
    assert_eq!(photos.len(), per * folders);

    // ---- 2. 每帧：日期分组 ----
    println!("\n[2] 每帧开销（这些在 M2 里是每帧都跑一遍的）");
    let (groups, _) = timed("group_by_day", || group_by_day(&photos));
    println!("  {:>36} {} 个分组", "", groups.len());

    let (layout, _) = timed("GridLayout::build", || {
        GridLayout::build(&photos, 1280.0, Tier::Medium, true)
    });
    println!(
        "  {:>36} {} 列 / 内容高 {:.0}px",
        "", layout.cols, layout.total_h
    );

    let viewport = eframe::egui::Rect::from_min_size(
        eframe::egui::pos2(0.0, 40_000.0),
        eframe::egui::vec2(1280.0, 800.0),
    );
    let ((first, last), _) = timed("visible_index_range（滚到 4 万 px）", || {
        layout.visible_index_range(viewport)
    });
    println!(
        "  {:>36} {} 个单元格可见（虚拟化后只画这些）",
        "",
        last - first
    );

    // ---- 3. 分页：只取第一页 vs 全量 ----
    println!("\n[3] 分页（C1 的目标形态）");
    let (page, _) = timed("photos(limit=2000) 首屏页", || {
        db.photos(None, SortSpec::default(), 2000).unwrap()
    });
    println!("  {:>36} {} 行", "", page.len());
    let (_, _) = timed("group_by_day(仅首屏页)", || group_by_day(&page));
    let (_, _) = timed("GridLayout::build(仅首屏页)", || {
        GridLayout::build(&page, 1280.0, Tier::Medium, true)
    });

    println!("\n== 判据 ==");
    println!("  · 首屏全量查询 < 60 ms（用户按下文件夹到看见第一格的时间预算）");
    println!("  · 每帧三件（分组 + 布局 + 可见区间）合计 < 4 ms（留给绘制与上传 12 ms）");
    println!("  · 分页后的每帧开销应与页大小相关，不与总张数相关");
}

/// 最小临时目录。
///
/// ⚠️ **不要用 `std::env::temp_dir()`**：这台开发机的 `/tmp` 是 **10MB 的 tmpfs**，
/// 5 万行索引库加上 WAL 会直接 `SQLITE_FULL`（实测踩到，报错是 "database or disk is full"，
/// 看起来像磁盘满了，实际是 /tmp 太小）。放在 target 目录旁边，跟着 `cargo clean` 一起清掉。
#[allow(clippy::disallowed_methods)] // 基准工具刻意清理上一次的产物，这里需要 remove_dir_all
fn tempdir() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/picasa-ui → crates → 仓库根
    p.pop();
    p.push("target");
    p.push("c-bench");
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}
