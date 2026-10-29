//! 按需缩略图/大图服务（后台线程池 + 去重）。
//!
//! 为什么需要它：扫描是**一次性**的，但 UI 会在两个时刻临时需要图片——
//! ① 栅格滚到一个「扫描时失败/新增」的格子；② 查看器打开一张还没补图的照片。
//! 这两条路径不能阻塞 UI 线程，也不能重复排队同一张。

use crate::scanner::CoreEvent;
use picasa_imaging::{THUMB_T0, THUMB_T1};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// 工作线程数。图像解码是 CPU 密集但短任务，取核数一半：
/// 留一半给 UI 与系统，避免「滚动时风扇狂转」。
pub const THUMB_WORKERS: usize = 4;

/// 一份工作：要么补缩略图，要么取查看器大图。
#[derive(Clone, Debug)]
pub struct ThumbJob {
    pub photo_id: i64,
    pub path: PathBuf,
    pub kind: JobKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    /// T0 栅格缩略图（512）
    Thumb,
    /// 查看器大图（2048，已按 Orientation 摆正）
    Viewer,
}

/// 后台图片服务。
pub struct ThumbService {
    tx: Sender<CoreEvent>,
    pool: rayon::ThreadPool,
    inflight: Arc<Mutex<HashSet<(i64, u8)>>>,
}

impl ThumbService {
    pub fn new(tx: Sender<CoreEvent>) -> Self {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(THUMB_WORKERS)
            .thread_name(|i| format!("op-img-{i}"))
            .build()
            .expect("无法创建图片线程池");
        Self {
            tx,
            pool,
            inflight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// 提交一个任务（同一张照片的同一种任务自动去重）。
    pub fn request(&self, job: ThumbJob) {
        let key = (job.photo_id, job.kind as u8);
        {
            let Ok(mut set) = self.inflight.lock() else {
                return;
            };
            if !set.insert(key) {
                return; // 已在队列里
            }
        }
        let tx = self.tx.clone();
        let inflight = self.inflight.clone();
        self.pool.spawn(move || {
            let result = match job.kind {
                JobKind::Thumb => {
                    picasa_imaging::thumb_from_file(&job.path, THUMB_T0).map(|(jpeg, w, h)| {
                        CoreEvent::ThumbReady {
                            photo_id: job.photo_id,
                            w,
                            h,
                            jpeg,
                        }
                    })
                }
                JobKind::Viewer => picasa_imaging::decode_oriented(&job.path).map(|img| {
                    CoreEvent::FullImageReady {
                        photo_id: job.photo_id,
                        w: img.width(),
                        h: img.height(),
                        rgba: img.into_raw(),
                    }
                }),
            };
            match result {
                Ok(ev) => {
                    let _ = tx.send(ev);
                }
                Err(e) => {
                    tracing::debug!(path = %job.path.display(), error = %e, "图片任务失败");
                    let _ = tx.send(CoreEvent::ThumbFailed {
                        photo_id: job.photo_id,
                    });
                }
            }
            if let Ok(mut set) = inflight.lock() {
                set.remove(&key);
            }
        });
    }

    /// 查看器用的大图长边（供 UI 侧做纹理上限判断）。
    pub fn viewer_max_edge() -> u32 {
        THUMB_T1
    }
}
