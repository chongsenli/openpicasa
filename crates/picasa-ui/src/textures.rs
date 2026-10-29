//! 缩略图纹理缓存：**每帧限量上传**（02 §8.2.5 的 4ms 预算）+ LRU 淘汰。
//!
//! 三层数据流：
//!
//! ```text
//! SQLite BLOB (jpeg)  ──decode──>  ColorImage  ──upload──>  TextureHandle (GPU)
//!        ▲                                                      │
//!        │ 没有就请求后台生成                                     │ 超过上限按 LRU 淘汰
//!   ThumbService (rayon)                                        ▼
//! ```
//!
//! **关键约束**：解码与上传都在 UI 线程，所以必须限量。一帧 120 个可见单元格
//! 里可能有 30 个是新的，全解一遍就是 40ms——直接掉到 25fps。
//! 这就是 `TEX_UPLOAD_PER_FRAME` 存在的理由。

use eframe::egui::{self, Context, TextureHandle, TextureId, TextureOptions};
use picasa_core::{ThumbJob, ThumbService, thumbs::JobKind};
use picasa_db::{Db, PhotoRow};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::theme::Metrics;

/// 一张缩略图在本帧的可用性。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TexState {
    /// 可以画了（纹理 id + 原始像素尺寸，用于等比贴合单元格）
    Ready(TextureId, egui::Vec2),
    /// 还没好（本帧画占位框）
    Pending,
    /// 解码失败（画「破损」占位，不重试）
    Failed,
}

pub struct TextureCache {
    textures: HashMap<i64, TextureHandle>,
    /// 每张纹理的像素尺寸（egui 没有反查接口，只能自己记）
    sizes: HashMap<i64, egui::Vec2>,
    /// 已到达但还没解码的 JPEG（省掉「一到达就解码」的 CPU 尖峰）
    bytes: HashMap<i64, Vec<u8>>,
    lru: VecDeque<i64>,
    requested: HashSet<i64>,
    failed: HashSet<i64>,
}

impl Default for TextureCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TextureCache {
    pub fn new() -> Self {
        Self {
            textures: HashMap::new(),
            sizes: HashMap::new(),
            bytes: HashMap::new(),
            lru: VecDeque::new(),
            requested: HashSet::new(),
            failed: HashSet::new(),
        }
    }

    pub fn clear(&mut self) {
        self.textures.clear();
        self.sizes.clear();
        self.bytes.clear();
        self.lru.clear();
        self.requested.clear();
        self.failed.clear();
    }

    /// 后台生成的缩略图到了。
    pub fn insert_jpeg(&mut self, photo_id: i64, jpeg: Vec<u8>) {
        self.failed.remove(&photo_id);
        self.bytes.insert(photo_id, jpeg);
        self.requested.remove(&photo_id);
    }

    pub fn mark_failed(&mut self, photo_id: i64) {
        self.failed.insert(photo_id);
        self.bytes.remove(&photo_id);
        self.requested.remove(&photo_id);
        if let Some(t) = self.textures.remove(&photo_id) {
            drop(t);
        }
        self.sizes.remove(&photo_id);
    }

    pub fn is_failed(&self, photo_id: i64) -> bool {
        self.failed.contains(&photo_id)
    }

    pub fn len(&self) -> usize {
        self.textures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }

    /// 拿一张纹理；拿不到就在预算允许时现场解码/上传，或请求后台生成。
    pub fn get(
        &mut self,
        ctx: &Context,
        db: &Db,
        service: &ThumbService,
        row: &PhotoRow,
        budget: &mut usize,
    ) -> TexState {
        if self.textures.contains_key(&row.id) {
            let tex_id = self.textures[&row.id].id();
            let size = self.sizes.get(&row.id).copied().unwrap_or(egui::Vec2::ZERO);
            self.touch(row.id);
            return TexState::Ready(tex_id, size);
        }
        if self.failed.contains(&row.id) || row.thumb_state == 2 || row.thumb_state == 3 {
            return TexState::Failed;
        }
        if *budget == 0 {
            // 预算用完：本帧只画占位，下一帧继续。**不做无预算的解码**。
            return TexState::Pending;
        }

        // ① 内存里已有 JPEG：解码 + 上传
        if let Some(bytes) = self.bytes.remove(&row.id) {
            match decode_jpeg(&bytes) {
                Some(img) => {
                    *budget -= 1;
                    self.upload(ctx, row.id, img);
                    let tex = self.textures[&row.id].id();
                    let size = self.sizes[&row.id];
                    return TexState::Ready(tex, size);
                }
                None => {
                    self.mark_failed(row.id);
                    return TexState::Failed;
                }
            }
        }

        // ② DB 里有 BLOB：读出来解码
        match db.get_thumb(row.id, 0) {
            Ok(Some(buf)) => match decode_jpeg(&buf.data) {
                Some(img) => {
                    *budget -= 1;
                    self.upload(ctx, row.id, img);
                    let tex = self.textures[&row.id].id();
                    let size = self.sizes[&row.id];
                    TexState::Ready(tex, size)
                }
                None => {
                    self.mark_failed(row.id);
                    TexState::Failed
                }
            },
            Ok(None) => {
                // ③ 还没有：请求后台生成（同一张只请求一次）
                if self.requested.insert(row.id) {
                    service.request(ThumbJob {
                        photo_id: row.id,
                        path: row.abs_path(),
                        kind: JobKind::Thumb,
                    });
                }
                TexState::Pending
            }
            Err(_) => TexState::Pending,
        }
    }

    fn upload(&mut self, ctx: &Context, id: i64, img: egui::ColorImage) {
        let size = egui::vec2(img.size[0] as f32, img.size[1] as f32);
        let tex = ctx.load_texture(format!("thumb-{id}"), img, TextureOptions::LINEAR);
        self.textures.insert(id, tex);
        self.sizes.insert(id, size);
        self.touch(id);
        self.evict_if_needed();
    }

    fn touch(&mut self, id: i64) {
        if let Some(pos) = self.lru.iter().position(|x| *x == id) {
            self.lru.remove(pos);
        }
        self.lru.push_back(id);
    }

    fn evict_if_needed(&mut self) {
        while self.textures.len() > Metrics::TEX_CACHE_CAP {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if let Some(tex) = self.textures.remove(&oldest) {
                drop(tex); // 立刻释放 GPU 纹理，不等 egui 的延迟回收
            }
            self.sizes.remove(&oldest);
        }
    }

    /// 预取：把即将进入视口的照片提前请求（扫描后补图用）。
    pub fn prefetch(&mut self, service: &ThumbService, rows: &[&PhotoRow]) {
        for row in rows {
            if self.textures.contains_key(&row.id) || self.bytes.contains_key(&row.id) {
                continue;
            }
            if self.requested.insert(row.id) {
                service.request(ThumbJob {
                    photo_id: row.id,
                    path: row.abs_path(),
                    kind: JobKind::Thumb,
                });
            }
        }
    }
}

fn decode_jpeg(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}
