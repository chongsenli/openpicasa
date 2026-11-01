//! 缩略图单张耗时基准。
//!
//! ```text
//! cargo run --example thumb_bench -- <目录或文件>... [--runs N] [--limit N]
//! ```
//!
//! 为什么要单独一个工具：25.9 万张的库，单张缩略图 1.4 秒和 20 毫秒的差别是
//! 「永远补不完」和「一晚上补完」。这个数字必须能随时量出来，而不是靠感觉。
//!
//! 打点口径：**每个文件重复 `runs` 次取最小值**（本机 5W 无风扇，跑久了会降频，
//! 取平均会把降频算进成本里）。

use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let mut targets: Vec<PathBuf> = Vec::new();
    let mut runs = 3usize;
    let mut limit = 20usize;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--runs" => {
                if let Some(v) = args.next() {
                    runs = v.parse().unwrap_or(runs);
                }
            }
            "--limit" => {
                if let Some(v) = args.next() {
                    limit = v.parse().unwrap_or(limit);
                }
            }
            other => targets.push(PathBuf::from(other)),
        }
    }
    if targets.is_empty() {
        eprintln!("用法: thumb_bench <目录或文件>... [--runs N] [--limit N]");
        std::process::exit(2);
    }

    // 展开目录，只收图片
    let mut files: Vec<PathBuf> = Vec::new();
    for t in &targets {
        collect(t, &mut files, limit);
    }
    files.truncate(limit.saturating_mul(targets.len().max(1)));
    if files.is_empty() {
        eprintln!("没找到图片文件");
        std::process::exit(2);
    }

    println!(
        "样本 {} 个文件，每张重复 {} 次取最小值（debug 判定 {}）",
        files.len(),
        runs,
        if cfg!(debug_assertions) {
            "DEBUG 构建"
        } else {
            "release 构建"
        }
    );

    let mut total_min_ms = 0.0f64;
    let mut per_image: Vec<f64> = Vec::with_capacity(files.len());
    let mut decode_ms: Vec<f64> = Vec::with_capacity(files.len());
    let mut encode_ms: Vec<f64> = Vec::with_capacity(files.len());
    let mut bytes_out = 0usize;
    let mut pixels = 0u64;

    for f in &files {
        let mut best = f64::MAX;
        let mut best_decode = f64::MAX;
        let mut best_encode = f64::MAX;
        let mut ok = true;
        for _ in 0..runs {
            // 只看「解码 + 摆正」这一段，用来判断瓶颈在解码还是在缩放/编码
            let t0 = Instant::now();
            let decoded = picasa_imaging::decode_oriented(f);
            let dms = t0.elapsed().as_secs_f64() * 1000.0;

            let t = Instant::now();
            match picasa_imaging::thumb_t0(f) {
                Ok((bytes, _w, _h)) => {
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    best = best.min(ms);
                    best_decode = best_decode.min(dms);
                    if let Ok(ref img) = decoded {
                        pixels = img.width() as u64 * img.height() as u64;
                    }
                    best_encode = best_encode.min((ms - dms).max(0.0));
                    bytes_out = bytes.len();
                }
                Err(e) => {
                    println!("  [失败] {} → {e}", f.display());
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            total_min_ms += best;
            per_image.push(best);
            decode_ms.push(best_decode);
            encode_ms.push(best_encode);
        } else {
            total_min_ms += 0.0;
            per_image.push(f64::NEG_INFINITY);
        }
    }

    per_image.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let valid: Vec<f64> = per_image
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    if valid.is_empty() {
        println!("全部失败");
        std::process::exit(1);
    }
    let avg = valid.iter().sum::<f64>() / valid.len() as f64;
    let avg_decode = decode_ms.iter().sum::<f64>() / decode_ms.len().max(1) as f64;
    let avg_encode = encode_ms.iter().sum::<f64>() / encode_ms.len().max(1) as f64;
    println!(
        "单张：最快 {:.1} ms / 中位 {:.1} ms / 平均 {:.1} ms / 最慢 {:.1} ms",
        valid.first().copied().unwrap_or(0.0),
        valid[valid.len() / 2],
        avg,
        valid.last().copied().unwrap_or(0.0)
    );
    println!(
        "拆解：解码+摆正 {:.1} ms（{:.1}%），缩放+编码+合白底 {:.1} ms（{:.1}%），原图 {:.1} 万像素",
        avg_decode,
        avg_decode / avg.max(0.001) * 100.0,
        avg_encode,
        avg_encode / avg.max(0.001) * 100.0,
        pixels as f64 / 10_000.0
    );
    println!(
        "合计（最小值累加）{:.0} ms → 按此速度补 22.4 万张约 {:.1} 小时（单线程）",
        total_min_ms,
        avg * 224_000.0 / 1000.0 / 3600.0
    );
    println!("最后一张缩略图 {} 字节", bytes_out);
}

fn collect(p: &Path, out: &mut Vec<PathBuf>, limit: usize) {
    if p.is_file() {
        out.push(p.to_path_buf());
        return;
    }
    let Ok(rd) = std::fs::read_dir(p) else {
        return;
    };
    let mut subs: Vec<PathBuf> = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.is_dir() {
            subs.push(path);
        } else if is_image(&path) {
            out.push(path);
            if out.len() >= limit {
                return;
            }
        }
    }
    for s in subs {
        collect(&s, out, limit);
        if out.len() >= limit {
            return;
        }
    }
}

fn is_image(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff")
    )
}
