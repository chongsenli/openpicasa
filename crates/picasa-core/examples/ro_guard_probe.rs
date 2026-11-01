//! GUARD-001~003 的**负载发生器**：用真实照片 + 真实库副本跑满三段流水线。
//!
//! ## 为什么要有这个东西
//!
//! 规格 §3.4 末段警告：三段流水线 + DCT 缩放解码 + mmap 是本版最大的**新写入风险**。
//! 红线 R-1「照片目录零写入」的现有自动化断言（`tests/zero_write.rs`）只覆盖 M2.1 的
//! 导航路径，**没有**覆盖流水线。而流水线目前尚未接入 GUI 生产路径，所以需要一个
//! 能被外部工具驱动的程序来造负载——这就是它。
//!
//! ## 用法（由 `tools/ro_guard.sh` 调用，一般不手动跑）
//!
//! ```sh
//! cargo run --release --example ro_guard_probe -p picasa-core -- \
//!     <库副本.sqlite> <照片根目录> [张数上限]
//! ```
//!
//! ⚠️ 参数 2 是**被测的只读目录**。本程序只 `open(2)`/`read(2)` 它；
//! 任何 `write`/`unlink`/`rename` 都会被引线工具抓到。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use picasa_core::pipeline::{DecodeItem, Pipeline, PipelineState, StoredItem, ThumbReason};
use picasa_db::ThumbBuf;

/// 目标边长（T0 档 85px 的 2 倍，强制走 DCT 缩放而不是"解码后整图缩小"）
const MAX_EDGE: u32 = 160;
/// 遍历段每批交付多少条
const SCAN_BATCH: usize = 64;

fn main() {
    let mut args = std::env::args().skip(1);
    let db_path = args
        .next()
        .expect("参数1：library.sqlite 的**副本**路径（可写，必须放在应用容器里）");
    let photo_root = args.next().expect("参数2：照片根目录（只读！GUARD 的断言对象）");
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(2000);

    let t0 = Instant::now();
    let db = picasa_db::Db::open(Path::new(&db_path)).expect("打开库副本");
    let root = Path::new(&photo_root);

    // ---- 备料：从库里取照片行，只保留**落在被测目录内**且真实存在的
    // `Path::join` 对绝对路径参数会返回参数本身，所以这里两种存储方式都兼容。
    let rows = db
        .photos(None, Default::default(), limit * 4)
        .expect("查询照片");
    let mut jobs: Vec<(i64, i64, PathBuf)> = Vec::new();
    for r in &rows {
        let p = root.join(&r.folder_path).join(&r.name);
        if p.starts_with(root) && p.is_file() {
            jobs.push((r.id, r.folder_id, p));
            if jobs.len() >= limit {
                break;
            }
        }
    }
    drop(db);
    println!("[probe] 候选 {} 张（上限 {limit}）", jobs.len());
    if jobs.is_empty() {
        eprintln!("[probe] 被测目录内没有可用照片，无法造负载");
        std::process::exit(2);
    }

    // ---- 落库连接：store 段单线程持有（`Connection` 不是 `Sync`，包一层 Mutex）
    let db = Arc::new(Mutex::new(
        picasa_db::Db::open(Path::new(&db_path)).expect("打开库副本（写侧）"),
    ));

    // ---- ① 遍历段：只**读**照片目录，按批交付
    let cursor = Arc::new(AtomicUsize::new(0));
    let jobs = Arc::new(jobs);
    let scan_jobs = Arc::clone(&jobs);
    let scan_cursor = Arc::clone(&cursor);
    let scan = Box::new(move || -> Vec<DecodeItem> {
        let start = scan_cursor.fetch_add(SCAN_BATCH, Ordering::Relaxed);
        if start >= scan_jobs.len() {
            return Vec::new();
        }
        let end = (start + SCAN_BATCH).min(scan_jobs.len());
        scan_jobs[start..end]
            .iter()
            .map(|(photo_id, folder_id, _)| DecodeItem {
                photo_id: *photo_id,
                folder_id: *folder_id,
                tier: 0,
                reason: ThumbReason::Discovered,
            })
            .collect()
    });

    // ---- ② 解码段：读字节 → `thumb_from_bytes`（内含 DCT 缩放快路径）
    let decode_jobs = Arc::clone(&jobs);
    let decoded = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let d_ok = Arc::clone(&decoded);
    let d_err = Arc::clone(&failed);
    let decode = Box::new(move |photo_id: i64, _tier: usize| -> Option<(Vec<u8>, u32, u32)> {
        let path = decode_jobs
            .iter()
            .find(|(id, _, _)| *id == photo_id)
            .map(|(_, _, p)| p.clone());
        let Some(path) = path else {
            d_err.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        // ⚠️ 唯一接触照片目录的地方：**只读** open + read，绝不写
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                d_err.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(path = %path.display(), error = %e, "读取失败");
                return None;
            }
        };
        match picasa_imaging::thumb_from_bytes(&bytes, MAX_EDGE) {
            Ok((out, w, h)) => {
                d_ok.fetch_add(1, Ordering::Relaxed);
                Some((out, w, h))
            }
            Err(e) => {
                d_err.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(path = %path.display(), error = %e, "解码失败");
                None
            }
        }
    });

    // ---- ③ 落库段：批量写**容器库**，不碰照片目录
    let stored_n = Arc::new(AtomicUsize::new(0));
    let s_n = Arc::clone(&stored_n);
    let db_store = Arc::clone(&db);
    let store = Box::new(move |batch: &[StoredItem]| {
        let Ok(d) = db_store.lock() else {
            return;
        };
        for it in batch {
            let buf = ThumbBuf {
                w: it.w as i64,
                h: it.h as i64,
                format: 0, // jpeg
                data: it.bytes.clone(),
            };
            if d.put_thumb(it.photo_id, it.tier as i64, &buf).is_ok() {
                s_n.fetch_add(1, Ordering::Relaxed);
            }
        }
    });

    let state = Arc::new(PipelineState::new());
    let target = jobs.len();
    let total = jobs.len();
    let t_start = Instant::now();
    let pipe = Pipeline::start(Arc::clone(&state), scan, decode, store);

    // ---- 等它跑完（或超时）。`shutdown` 会 join 三段线程。
    let deadline = Duration::from_secs(300);
    while stored_n.load(Ordering::Relaxed) + failed.load(Ordering::Relaxed) < total
        && t_start.elapsed() < deadline
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    state.stop();
    pipe.shutdown();

    let secs = t_start.elapsed().as_secs_f64();
    let ok = stored_n.load(Ordering::Relaxed);
    let err = failed.load(Ordering::Relaxed);
    println!("[probe] 目标 {target} 张 · 落库 {ok} · 失败 {err}");
    println!(
        "[probe] 耗时 {:.2}s · 吞吐 {:.1} 张/秒（解码 {} 次）",
        secs,
        ok as f64 / secs.max(0.001),
        decoded.load(Ordering::Relaxed)
    );
    println!("[probe] 总耗时 {:.2}s", t0.elapsed().as_secs_f64());
    if ok == 0 {
        eprintln!("[probe] 一张都没落库——负载没造成，GUARD 断言无意义");
        std::process::exit(3);
    }
}
