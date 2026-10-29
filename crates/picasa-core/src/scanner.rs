//! 扫描器：递归收录 → 落库 → 补缩略图。
//!
//! **R-1 在本模块的落地方式**：整条链路只有 `WalkDir`（`stat`）+ `File::open`（读），
//! 写操作全部发生在 `Db`（应用容器里的 SQLite）。没有 `fs::write` / `create` / `rename` /
//! `remove` 作用在照片目录上的任何调用点。

use crate::{CoreError, Result, filters, paths};
use picasa_db::{Db, PhotoInput};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc::Sender};
use std::time::Instant;

/// 扫描/后台任务推给 UI 的事件。全部是「值」，UI 侧不需要回调。
pub enum CoreEvent {
    /// 开始扫描（用于状态栏文案）
    ScanStarted {
        roots: usize,
    },
    /// 一个目录扫完（已落库）
    FolderScanned {
        folder_id: i64,
        path: String,
        added: u32,
        updated: u32,
        removed: u32,
        photos_in_folder: u32,
    },
    /// 进度（节流后，约每 200 个文件一次）
    Progress {
        dirs: u64,
        photos: u64,
        current: String,
    },
    /// 全部扫完
    ScanFinished {
        dirs: u64,
        photos: u64,
        secs: f32,
    },
    /// 一张缩略图就绪
    ThumbReady {
        photo_id: i64,
        w: u32,
        h: u32,
        jpeg: Vec<u8>,
    },
    ThumbFailed {
        photo_id: i64,
    },
    /// 查看器要的大图就绪（`rgba` 已按 Orientation 摆正）
    FullImageReady {
        photo_id: i64,
        w: u32,
        h: u32,
        rgba: Vec<u8>,
    },
    /// 文件系统有变化（已去抖）
    FsChanged {
        paths: Vec<String>,
    },
    /// 外部盘离线/在线状态变化
    FolderOnline {
        folder_id: i64,
        online: bool,
    },
    Error(String),
}

/// 扫描句柄（可取消）。
pub struct ScanHandle {
    pub cancel: Arc<AtomicBool>,
}

impl ScanHandle {
    /// 请求停止。已落库的部分保留（不从此零开始，见 F1-26 断点思想）。
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// 启动一次扫描（后台线程）。返回句柄，UI 通过 `tx` 收事件。
pub fn spawn_scan(db: Arc<Mutex<Db>>, roots: Vec<PathBuf>, tx: Sender<CoreEvent>) -> ScanHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let handle = ScanHandle {
        cancel: cancel.clone(),
    };
    std::thread::Builder::new()
        .name("op-scan".into())
        .spawn(move || {
            if let Err(e) = scan_roots(&db, &roots, &tx, &cancel) {
                tracing::error!(error = %e, "扫描失败");
                let _ = tx.send(CoreEvent::Error(format!("扫描失败：{e}")));
            }
        })
        .expect("无法创建扫描线程");
    handle
}

fn scan_roots(
    db: &Arc<Mutex<Db>>,
    roots: &[PathBuf],
    tx: &Sender<CoreEvent>,
    cancel: &AtomicBool,
) -> Result<()> {
    let started = Instant::now();
    let _ = tx.send(CoreEvent::ScanStarted { roots: roots.len() });

    let dirs_seen = AtomicU64::new(0);
    let photos_seen = AtomicU64::new(0);
    let mut folder_total = 0u64;

    for root in roots {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !root.is_dir() {
            tracing::warn!(root = %root.display(), "根目录不可用，跳过");
            continue;
        }

        // 按目录聚合：一个目录一次事务落库，比一张照片一次快一个数量级。
        let mut by_dir: BTreeMap<PathBuf, Vec<PhotoInput>> = BTreeMap::new();

        let walker = walkdir::WalkDir::new(root)
            .max_depth(filters::DEFAULT_MAX_DEPTH)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                // **根目录本身永不跳过**：用户已经明确选了它。
                // 否则一个以 `.` 开头的目录（例如 `~/Pictures/.staging`，或测试里的
                // `tempfile` 默认前缀 `.tmpXXXX`）会被整棵剪掉，扫出 0 张照片。
                if e.depth() == 0 {
                    return true;
                }
                if e.file_type().is_dir() {
                    let name = e.file_name().to_string_lossy();
                    !filters::should_skip_dir(&name)
                } else {
                    true
                }
            });

        for entry in walker.filter_map(|e| e.ok()) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if entry.file_type().is_dir() {
                dirs_seen.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if filters::should_skip_file(&name) || !filters::is_supported_image(entry.path()) {
                continue;
            }
            let Some(parent) = entry.path().parent() else {
                continue;
            };

            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(path = %entry.path().display(), error = %e, "读元数据失败");
                    continue;
                }
            };
            let size_bytes = meta.len() as i64;
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);

            let probe = picasa_imaging::probe(entry.path()).unwrap_or_default();
            let (taken_at, taken_at_src) = match probe.exif_taken_at {
                Some(ts) => (Some(ts), 0i64),
                None => {
                    // F1-15：日期缺失时回退文件修改时间，并且**标记来源**（UI 可见）
                    let fallback = if mtime_ns > 0 {
                        Some(mtime_ns / 1_000_000_000)
                    } else {
                        None
                    };
                    (fallback, 2)
                }
            };

            by_dir
                .entry(parent.to_path_buf())
                .or_default()
                .push(PhotoInput {
                    name_key: paths::compare_key(&name),
                    name,
                    size_bytes,
                    mtime_ns,
                    width: Some(probe.width as i64).filter(|w| *w > 0),
                    height: Some(probe.height as i64).filter(|h| *h > 0),
                    orientation: Some(probe.orientation as i64),
                    taken_at,
                    taken_at_src,
                });

            let n = photos_seen.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(200) {
                let _ = tx.send(CoreEvent::Progress {
                    dirs: dirs_seen.load(Ordering::Relaxed),
                    photos: n,
                    current: parent.display().to_string(),
                });
            }
        }

        // 落库 + 补缩略图（逐目录，UI 才能渐进填充）
        for (dir, items) in by_dir {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let dir_str = paths::canonical_string(&dir);
            let dir_key = paths::compare_key(&dir_str);
            let volume_id = volume_of(&dir);

            let outcome = {
                let guard = db
                    .lock()
                    .map_err(|_| CoreError::Other("db 锁中毒".into()))?;
                let folder_id = guard.upsert_folder(&dir_str, &dir_key, volume_id.as_deref())?;
                let apply =
                    guard.apply_scan(folder_id, &dir_str, &items, picasa_imaging::DECODER_VER)?;
                (folder_id, apply)
            };
            let (folder_id, apply) = outcome;
            folder_total += 1;

            let _ = tx.send(CoreEvent::FolderScanned {
                folder_id,
                path: dir_str.clone(),
                added: apply.added,
                updated: apply.updated,
                removed: apply.removed,
                photos_in_folder: items.len() as u32,
            });

            // 该目录里还没有缩略图的，用 rayon 并行补（一目录一批，避免长尾卡住 UI）
            generate_thumbs_for_folder(db, folder_id, tx, cancel);
        }
    }

    let _ = tx.send(CoreEvent::ScanFinished {
        dirs: dirs_seen.load(Ordering::Relaxed),
        photos: photos_seen.load(Ordering::Relaxed),
        secs: started.elapsed().as_secs_f32(),
    });
    tracing::info!(
        dirs = dirs_seen.load(Ordering::Relaxed),
        photos = photos_seen.load(Ordering::Relaxed),
        folders = folder_total,
        "扫描结束"
    );
    Ok(())
}

/// 补一个目录的缩略图（并行），逐张推事件。
pub fn generate_thumbs_for_folder(
    db: &Arc<Mutex<Db>>,
    folder_id: i64,
    tx: &Sender<CoreEvent>,
    cancel: &AtomicBool,
) {
    use rayon::prelude::*;

    let pending = {
        let Ok(guard) = db.lock() else { return };
        match guard.photos_pending_thumbs(Some(&[folder_id]), 20_000) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "查询待生成缩略图失败");
                return;
            }
        }
    };
    if pending.is_empty() {
        return;
    }

    pending.par_iter().for_each(|row| {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let path = row.abs_path();
        match picasa_imaging::thumb_t0(&path) {
            Ok((jpeg, w, h)) => {
                if let Ok(guard) = db.lock() {
                    let buf = picasa_db::ThumbBuf {
                        w: w as i64,
                        h: h as i64,
                        format: 0,
                        data: jpeg.clone(),
                    };
                    if let Err(e) = guard.put_thumb(row.id, 0, &buf) {
                        tracing::warn!(error = %e, "写缩略图失败");
                    }
                }
                let _ = tx.send(CoreEvent::ThumbReady {
                    photo_id: row.id,
                    w,
                    h,
                    jpeg,
                });
            }
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "缩略图生成失败");
                if let Ok(guard) = db.lock() {
                    let _ = guard.set_thumb_state(row.id, picasa_db::ThumbState::Failed);
                }
                let _ = tx.send(CoreEvent::ThumbFailed { photo_id: row.id });
            }
        }
    });
}

/// 卷标识：外接盘重挂载后用来识别「还是同一个盘」。
fn volume_of(path: &Path) -> Option<String> {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("/Volumes/") {
        let name = rest.split('/').next()?.to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    Some("/".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_jpeg(path: &Path, w: u32, h: u32) {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 255) as u8, (y % 255) as u8, 128])
        });
        let mut buf = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new(&mut buf);
        enc.encode(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        std::fs::File::create(path)
            .unwrap()
            .write_all(&buf)
            .unwrap();
    }

    #[test]
    fn scan_records_nested_folders_and_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("2011/05")).unwrap();
        write_jpeg(&root.join("a.jpg"), 40, 30);
        write_jpeg(&root.join("2011/b.jpeg"), 20, 20);
        write_jpeg(&root.join("2011/05/c.JPG"), 10, 10);
        std::fs::write(root.join("notes.txt"), b"ignored").unwrap();

        let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = spawn_scan(db.clone(), vec![root.to_path_buf()], tx);
        // 等扫描结束
        let mut finished = false;
        while let Ok(ev) = rx.recv_timeout(std::time::Duration::from_secs(20)) {
            if matches!(ev, CoreEvent::ScanFinished { .. }) {
                finished = true;
                break;
            }
        }
        drop(handle);
        assert!(finished, "扫描未结束");

        let guard = db.lock().unwrap();
        assert_eq!(guard.total_photos().unwrap(), 3);
        let folders = guard.all_folders().unwrap();
        assert_eq!(folders.len(), 3);
        let total: u32 = folders.iter().map(|f| f.photo_count).sum();
        assert_eq!(total, 3);
    }

    #[test]
    fn hidden_root_is_still_scanned() {
        // 用户显式选中一个以 `.` 开头的目录时，必须照扫（不能在根上套用隐藏目录规则）
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".staging");
        std::fs::create_dir_all(root.join(".inner")).unwrap();
        write_jpeg(&root.join("a.jpg"), 20, 10);
        // 但根**内部**的隐藏目录仍然跳过
        write_jpeg(&root.join(".inner/b.jpg"), 20, 10);

        let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = spawn_scan(db.clone(), vec![root.clone()], tx);
        while let Ok(ev) = rx.recv_timeout(std::time::Duration::from_secs(20)) {
            if matches!(ev, CoreEvent::ScanFinished { .. }) {
                break;
            }
        }
        drop(handle);

        let guard = db.lock().unwrap();
        assert_eq!(guard.total_photos().unwrap(), 1, "只应收录 .staging/a.jpg");
    }
}
