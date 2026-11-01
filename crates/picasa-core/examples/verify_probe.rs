//! GUARD-002 的**负载发生器**：驱动 §4.8 启动增量校验跑完整一轮。
//!
//! 用法（由 `tools/ro_guard.sh` 调用）：
//! ```sh
//! RO_GUARD_PROBE=target/release/examples/verify_probe \
//!   RO_GUARD_SKIP_BUILD=1 ./tools/ro_guard.sh /照片目录
//! ```
//!
//! 断言的不是「它跑得快」，而是**它只读**：校验全程对照片目录只能有 `read_dir`/`stat`，
//! 任何 `write`/`rename`/`unlink` 都是 R-1 违规。

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let db_path = args.next().expect("参数1：library.sqlite 的副本路径");
    // 第二个参数（照片目录）由 ro_guard.sh 统一传，这里只用来确认被测目录在线
    let _photo_dir = args.next();

    let t0 = Instant::now();
    let db = picasa_db::Db::open(std::path::Path::new(&db_path)).expect("打开库副本");
    let roots = db.roots(false).expect("读 roots");
    let paths: Vec<PathBuf> = roots.into_iter().map(|r| PathBuf::from(r.path)).collect();
    if paths.is_empty() {
        eprintln!("[verify-probe] 没有活跃根，无法造负载");
        std::process::exit(2);
    }
    println!("[verify-probe] 活跃根 {} 个", paths.len());

    let mut runner = picasa_core::verify::VerifyRunner::new(
        picasa_core::VerifyPlan::from_root_paths(paths),
    );
    // 基准模式：不等首屏延迟、不限速，量的是纯 IO 成本（产品路径仍是 2s/50ms）
    runner.set_delay_ms(0);
    runner.set_dir_interval_ms(0);

    let t_run = Instant::now();
    let mut ticks = 0u64;
    loop {
        ticks += 1;
        match runner.tick(&db) {
            picasa_core::verify::VerifyTick::Finished => break,
            _ => {}
        }
        if ticks > 2_000_000 {
            eprintln!("[verify-probe] tick 次数超上限，退出");
            std::process::exit(3);
        }
    }
    let secs = t_run.elapsed().as_secs_f64();
    let p = runner.progress();
    let changed = runner.take_changed();
    let new_dirs = runner.take_new_dirs();
    println!("[verify-probe] 检查 {} 个目录 · 耗时 {:.2}s", p.dirs_checked, secs);
    println!(
        "[verify-probe] 有变化 {} 个目录 · 新建目录 {} 个",
        changed.len(),
        new_dirs.len()
    );
    println!("[verify-probe] tick {} 次 · 总耗时 {:.2}s", ticks, t0.elapsed().as_secs_f64());
    if p.dirs_checked == 0 {
        eprintln!("[verify-probe] 一个目录都没检查——负载没造成，GUARD 断言无意义");
        std::process::exit(3);
    }
}
