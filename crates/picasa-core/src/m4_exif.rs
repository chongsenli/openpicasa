//! M4 · EXIF 摘要回填（docs/11 §3、`M4-META-01～08`）。
//!
//! ## 为什么不改 `PhotoInput` / scanner
//!
//! `PhotoInput` 在扫描器 3 处构造、测试里十几处字面量，加 9 个字段会牵动整条链路，
//! 而且**缓存命中路径根本不读文件**（`scanner.rs:884-887` 只比对 size/mtime/解码器版本），
//! 加字段也拿不到值。所以 M4 采用**独立回填**：扫描落库后跑一遍 `exif_status = 0`
//! 的照片，把 `ExifSummary` 写进 v8 列。
//!
//! ## ⚠️ 锁粒度是性能红线（2026-11-06 实机反馈「索引速度太慢」的根因）
//!
//! `probe()` 每个文件都要 `File::open` + 解析 EXIF，慢且受磁盘影响。早期版本
//! 在**持有 `Mutex<Db>` 锁期间**做这一步，把索引 / 缩略图写入线程全部堵死。
//! 现在拆成三段，调用方（app.rs 的后台线程）只在 ①/③ 持毫秒级短锁：
//!
//! ```text
//! ① collect_pending(db)  → 持锁取 (id, abs_path)，立刻放锁
//! ② probe_paths(...)     → 完全无锁，最贵的文件 I/O
//! ③ write_batch(db, ..)  → 持锁单事务批量写回，立刻放锁
//! ```
//!
//! 全程只读原图、只写自有数据库（R-M2-1 / `M4-META-06`）；失败降级为
//! `exif_status = 2` + 原因短语，不让单张坏图阻塞整个回填。

use picasa_db::{Db, PhotoExif, Result};
use std::path::PathBuf;

/// ① 持锁查询：取一批待回填照片的 (id, 绝对路径)。调用方拿到结果后**立刻放锁**。
pub fn collect_pending(db: &Db, limit: usize) -> Result<Vec<(i64, PathBuf)>> {
    let ids = db.photos_pending_exif(limit)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Ok(Some(row)) = db.photo(id) {
            out.push((id, row.abs_path()));
        }
    }
    Ok(out)
}

/// ② 完全无锁：逐张 `probe()` 只读原图。返回 (待写回项, 失败数)。
pub fn probe_paths(pending: Vec<(i64, PathBuf)>) -> (Vec<(i64, PhotoExif)>, usize) {
    let mut items = Vec::with_capacity(pending.len());
    let mut failed = 0;
    for (id, path) in pending {
        let e = match picasa_imaging::probe(&path) {
            Ok(p) => {
                let s = p.summary;
                PhotoExif {
                    camera_make: s.make,
                    camera_model: s.model,
                    lens_model: s.lens,
                    focal_length: s.focal_length,
                    f_number: s.f_number,
                    exposure_time: s.exposure_time,
                    iso: s.iso,
                    status: 1,
                    error: None,
                }
            }
            Err(err) => {
                failed += 1;
                PhotoExif {
                    status: 2,
                    error: Some(format!("EXIF 读取失败：{err}")),
                    ..Default::default()
                }
            }
        };
        items.push((id, e));
    }
    (items, failed)
}

/// ③ 持锁写入：单事务批量写回（一次 BEGIN/COMMIT，而不是 N 次）。
pub fn write_batch(db: &Db, items: &[(i64, PhotoExif)]) -> Result<usize> {
    db.set_photo_exif_batch(items)
}

/// 便捷入口（测试 / 单线程场合用）：三段连跑。
/// ⚠️ 生产路径不要用这个 —— 它会在调用方的锁内做 `probe()`。
pub fn backfill_batch(db: &Db, limit: usize) -> Result<(usize, usize)> {
    let pending = collect_pending(db, limit)?;
    if pending.is_empty() {
        return Ok((0, 0));
    }
    let (items, failed) = probe_paths(pending);
    let ok = write_batch(db, &items)?;
    Ok((ok, failed))
}

/// 供「文件变化后缓存失效重读」使用（`M4-META-05`）：把一张照片标记回未读，
/// 下次回填会重新 probe。
pub fn invalidate_exif(db: &Db, photo_id: i64) -> Result<()> {
    db.set_photo_exif(
        photo_id,
        &PhotoExif {
            status: 0,
            ..Default::default()
        },
    )
}
