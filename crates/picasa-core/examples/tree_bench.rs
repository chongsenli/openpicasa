//! 左栏树 + 首屏重载的规模基准：证明（或证伪）「目录数一多就卡住 UI」。
//!
//! 用法：`cargo run --release -p picasa-core --example tree_bench`
//!
//! 造 N 个相册目录（每目录 K 张照片），然后分别计时
//! `reload_tree()` 与 `reload_photos()`——这两个函数**都跑在 UI 线程上**，
//! 而且每扫完一个目录就会被调用一次（见 `Library::apply_event` 的 `FolderScanned` 分支）。
//! 所以单次耗时 × 目录数 ≈ 用户实际承受的总卡顿时长。

use picasa_core::{AppPaths, Library};
use picasa_db::PhotoInput;
use std::time::Instant;

fn app_paths(root: &std::path::Path) -> AppPaths {
    AppPaths {
        support: root.join("Support"),
        preferences: root.join("Preferences"),
        caches: root.join("Caches"),
        logs: root.join("Logs"),
    }
}

/// 一次规模实验：N 个目录 × K 张照片，测单次 reload_tree / reload_photos。
fn run_case(n_dirs: usize, per_dir: usize) -> (f64, f64) {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    std::fs::create_dir_all(&media).unwrap();

    let mut lib = Library::open(app_paths(tmp.path())).unwrap();
    lib.add_root(&media);

    let root_str = picasa_core::paths::canonical_string(&media);
    {
        let db = lib.db().unwrap();
        for i in 0..n_dirs {
            let dir = format!("{root_str}/album{i:05}");
            let key = picasa_core::paths::compare_key(&dir);
            let fid = db.upsert_folder(&dir, &key, None).unwrap();
            let items: Vec<PhotoInput> = (0..per_dir)
                .map(|j| PhotoInput {
                    name: format!("p{j:04}.jpg"),
                    name_key: format!("p{j:04}.jpg"),
                    size_bytes: 1024,
                    mtime_ns: 1_700_000_000_000_000_000 + (i * per_dir + j) as i64,
                    width: Some(1000),
                    height: Some(800),
                    orientation: Some(1),
                    taken_at: Some(1_700_000_000 + (i * per_dir + j) as i64),
                    taken_at_src: 0,
                })
                .collect();
            db.apply_scan(fid, &dir, &items, 1).unwrap();
        }
    }

    // 预热一次，排除首次分配的影响
    lib.reload_tree().unwrap();
    lib.reload_photos().unwrap();

    let t = Instant::now();
    lib.reload_tree().unwrap();
    let tree_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    lib.reload_photos().unwrap();
    let photos_ms = t.elapsed().as_secs_f64() * 1000.0;

    (tree_ms, photos_ms)
}

fn main() {
    println!("左栏树与首屏重载的规模基准（UI 线程执行，每目录触发一次）\n");
    println!(
        "{:>8} {:>7} {:>10} {:>12} {:>12} {:>14}",
        "目录数", "每目录", "总照片", "reload_tree", "reload_photos", "扫描累计卡顿"
    );
    println!("{}", "-".repeat(72));

    for &n in &[500usize, 1000, 2000, 4000, 8000] {
        let per_dir = 20;
        let (tree_ms, photos_ms) = run_case(n, per_dir);
        // 每个目录落库后都会触发一次 (reload_tree + reload_photos)
        let total = (tree_ms + photos_ms) * n as f64 / 1000.0;
        println!(
            "{n:>8} {:>7} {:>10} {:>10.2}ms {:>10.2}ms {:>11.1}s",
            per_dir,
            n * per_dir,
            tree_ms,
            photos_ms,
            total
        );
    }

    println!("\n判读：");
    println!("  · reload_tree 若随目录数**平方增长**，说明 §1 的 entries 去重用了线性 find");
    println!("  · 最后一列 = 用户从添加文件夹到扫完期间，UI 线程花在重载上的累计时间");
}
