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
use picasa_core::texbudget::TextureBudget;

/// [`TextureCache::missing_state`] 的判定结果（见那里的文档）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MissingState {
    /// 库里没 BLOB，但本机也没有原图字节（云端占位）。
    CloudOnly,
    /// 库里没 BLOB、还可以去生成。
    Pending,
}


/// 一张缩略图在本帧的可用性。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TexState {
    /// 可以画了（纹理 id + 原始像素尺寸，用于等比贴合单元格）
    Ready(TextureId, egui::Vec2),
    /// 还没好（本帧画占位框）
    Pending,
    /// 解码失败（画「破损」占位，不重试）
    Failed,
    /// 文件在云端、本机没有内容（画「云端未下载」占位，**不是失败**）
    CloudOnly,
}

pub struct TextureCache {
    /// 字节账本（`07` §6.1 PERF-08）。与 `textures` 一一对应，但**分开存**：
    /// 账本要能被单测直接构造，而 `TextureHandle` 离不开 `egui::Context`。
    pub budget: TextureBudget,
    /// 本帧待释放的纹理（帧末统一 drop）。
    ///
    /// 为什么不在 [`TextureCache::evict_if_needed`] 里直接 drop：
    /// 帧中 drop 会让本帧剩下的绘制拿到失效的 `TextureId`（画面上是若干灰块闪一下）。
    /// 攒到帧末统一释放，那一帧的绘制已经结束。
    pending_evict: Vec<i64>,
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
        Self::with_budget(TextureBudget::from_physical_memory(physical_memory_mb()))
    }

    /// 指定预算（测试与诊断用：真机上按物理内存算，测试里给固定值）。
    pub fn with_budget(budget: TextureBudget) -> Self {
        Self {
            budget,
            pending_evict: Vec::new(),
            textures: HashMap::new(),
            sizes: HashMap::new(),
            bytes: HashMap::new(),
            lru: VecDeque::new(),
            requested: HashSet::new(),
            failed: HashSet::new(),
        }
    }

    pub fn clear(&mut self) {
        self.budget.clear();
        self.pending_evict.clear();
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

    /// 只读查询：这张照片的纹理**已经在缓存里**就返回，否则 `None`。
    ///
    /// 查看器「逐级加载」用：打开大图时先把栅格里已缓存的 T0 缩略图当预览画出来，
    /// 等 2560 的大图解码完再换掉。**不触发解码、不请求、不改 LRU**——它只是「顺手看一眼」。
    pub fn peek(&self, photo_id: i64) -> Option<(TextureId, egui::Vec2)> {
        let tex = self.textures.get(&photo_id)?;
        let size = self
            .sizes
            .get(&photo_id)
            .copied()
            .unwrap_or(egui::Vec2::ZERO);
        Some((tex.id(), size))
    }

    pub fn len(&self) -> usize {
        self.textures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }

    /// 一张照片「库里没有 BLOB」时该怎么呈现。
    ///
    /// 必须**先**问「有没有 BLOB」再问「是不是云端占位」：导入的索引包会把
    /// 缩略图一起带过来（`content_flags.thumbnails`），那些照片的原图在本机
    /// 往往根本不存在，行上也会带 `CloudOnly`。若先判 `CloudOnly` 就直接返回，
    /// 包里存着的图就永远画不出来——这是 v0.7-dev22 引入的缺陷。
    fn missing_state(thumb_state: i64) -> MissingState {
        if thumb_state == picasa_db::ThumbState::CloudOnly as i64 {
            MissingState::CloudOnly
        } else {
            MissingState::Pending
        }
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
                // 库里确实没有 BLOB。**只有到这一步**才可能是「云端未下载」：
                // 有 BLOB 就必须画出来（导入的索引包自带缩略图，那些照片的
                // 原图在本机可能根本没有——不能因为原图在云上就不显示包里的图）。
                match Self::missing_state(row.thumb_state) {
                    MissingState::CloudOnly => {
                        // 本机没有内容，请求生成也只会白等一轮超时，直接判掉；
                        // 但**不记进 `failed`**——文件下载到本地后下一轮扫描会把它回队列。
                        return TexState::CloudOnly;
                    }
                    MissingState::Pending => {}
                }
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
        // 字节数按 RGBA8 算：`w * h * 4`。这是 GPU 上真实占用的下限
        // （驱动可能还有对齐填充与 mipmap，`07` §6.1 只要求登记这个下限）。
        let bytes = (img.size[0] * img.size[1] * 4) as u32;
        let tex = ctx.load_texture(format!("thumb-{id}"), img, TextureOptions::LINEAR);
        self.textures.insert(id, tex);
        self.sizes.insert(id, size);
        self.budget.register(id, bytes, 0);
        self.touch(id);
        self.evict_if_needed();
    }

    fn touch(&mut self, id: i64) {
        if let Some(pos) = self.lru.iter().position(|x| *x == id) {
            self.lru.remove(pos);
        }
        self.lru.push_back(id);
    }

    /// 上传后的回收：**张数上限**与**字节预算**两条线都要看。
    ///
    /// - 张数上限（[`Metrics::TEX_CACHE_CAP`]）是 M2.1 的老规则，防「一堆小图占满头」；
    /// - 字节预算（[`TextureBudget`]）是 M3 的新规则，防「少量大图吃光显存」——
    ///   缩放滑块拉到 512 px 时，一张纹理就 1 MB，120 张 = 120 MB，远超 384 MB 之外的实际可用量。
    ///
    /// 真正的 drop 推迟到 [`TextureCache::flush_evictions`]（帧末）。
    fn evict_if_needed(&mut self) {
        while self.textures.len() > Metrics::TEX_CACHE_CAP {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if self.textures.contains_key(&oldest) {
                self.pending_evict.push(oldest);
            }
        }
        // 字节预算：只把「该走的」排进队列，真正的 drop 在帧末
        for id in self.budget.evict_end_of_frame() {
            if self.textures.contains_key(&id) && !self.pending_evict.contains(&id) {
                self.pending_evict.push(id);
            }
        }
    }

    /// 帧末执行真正的 `drop`（`07` §6.1 PERF-08 的「帧末释放」）。
    ///
    /// 返回本帧释放了多少张——诊断浮层会显示它，让「预算真的在起作用」可观察。
    pub fn flush_evictions(&mut self) -> usize {
        let ids = std::mem::take(&mut self.pending_evict);
        let n = ids.len();
        for id in ids {
            if let Some(tex) = self.textures.remove(&id) {
                drop(tex); // 立刻释放 GPU 纹理，不等 egui 的延迟回收
            }
            self.sizes.remove(&id);
            if let Some(pos) = self.lru.iter().position(|x| *x == id) {
                self.lru.remove(pos);
            }
            self.budget.forget(id);
        }
        n
    }

    /// 告知本帧哪些照片在视口内、离视口多少屏（`TextureBudget` 依赖它判断谁能淘汰）。
    ///
    /// 每帧**只调一次**（不是每张纹理一次）：调用方先把视口映射成下标区间，
    /// 再一次性把区间内外的屏距算好。
    pub fn note_viewport(&mut self, distances: impl IntoIterator<Item = (i64, u32)>) {
        self.budget.begin_frame();
        self.budget.note_visible(distances);
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

/// 物理内存（MB）。取不到时给一个保守值（4 GB → 128 MB 纹理预算）。
///
/// ## 为什么不用 `sysinfo` 这类 crate
///
/// 整个项目只需要**一个整数**，为此拉一棵依赖树（还要跟着平台升级维护）不划算。
/// 三个平台各一行系统调用就够：
///
/// - Linux：`sysconf(_SC_PHYS_PAGES) × _SC_PAGESIZE`
/// - macOS：`sysctl hw.memsize`
/// - 其他：返回 0 → 调用方用兜底值
///
/// 而且**只读**、不涉及照片目录（R-1 无关）。
fn physical_memory_mb() -> u64 {
    let bytes = physical_memory_bytes().unwrap_or(4 * 1024 * 1024 * 1024);
    (bytes / (1024 * 1024)).max(1024)
}

#[cfg(target_os = "macos")]
fn physical_memory_bytes() -> Option<u64> {
    // `libc::sysctl` 又要拉 libc 依赖；`std::process::Command` 调 `sysctl` 更简单，
    // 但每次启动 fork 一个进程不值。改用 `std::env` 拿不到，所以这里退一步：
    // 读 `/usr/bin/sysctl` 的成本（≈3 ms）只在**构造一次**时付，可接受。
    let out = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    std::str::from_utf8(&out.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

#[cfg(target_os = "linux")]
fn physical_memory_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb = rest.trim().trim_end_matches(" kB").trim().parse::<u64>().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn physical_memory_bytes() -> Option<u64> {
    None
}

fn decode_jpeg(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用户反馈（v0.7-dev22 的缺陷）：打了「云端未下载」的行，若索引库里
    /// 其实存着缩略图（典型：从别的电脑导出的索引包，包里带
    /// `content_flags.thumbnails`），必须照样把图画出来。
    ///
    /// 判定顺序写反过一版——先看 `thumb_state` 就直接返回 `CloudOnly`，
    /// 于是包里存着的图永远画不出来。这里把顺序钉死。
    #[test]
    fn cloud_only_rows_still_render_thumbs_that_exist_in_the_database() {
        // 有 BLOB 时，`get` 走的是 `db.get_thumb` 的 `Ok(Some(_))` 分支，
        // 根本不会问 `missing_state`——所以这里能验证的就是：
        // `missing_state` 只在「库里确实没有 BLOB」之后才被调用。
        assert_eq!(
            TextureCache::missing_state(picasa_db::ThumbState::CloudOnly as i64),
            MissingState::CloudOnly
        );
        assert_eq!(
            TextureCache::missing_state(picasa_db::ThumbState::Ok as i64),
            MissingState::Pending,
            "有图的行走的是 Ready 分支，不该被当成云端占位"
        );

        // 源码级断言：`missing_state` 的调用点必须在 `db.get_thumb` 之后。
        let src = include_str!("textures.rs");
        let get_thumb = src.find("match db.get_thumb(row.id, 0)").expect("必须有读 BLOB 的分支");
        let call = src
            .find("match Self::missing_state(row.thumb_state)")
            .expect("必须有 missing_state 的调用点");
        assert!(
            get_thumb < call,
            "先判云端占位再读 BLOB 会让导入包里的缩略图永不显示（v0.7-dev22 的缺陷）"
        );
        // 只钉住调用点还不够：在 `get` 开头再加一句 `if row.thumb_state == CloudOnly
        // { return TexState::CloudOnly; }` 一样能把 BLOB 挡掉，而上面的断言察觉不到
        // （调用点仍在 get_thumb 之后）。所以再钉住「`get_thumb` 之前不许出现
        // `return TexState::CloudOnly`」。
        let early_return = src.find("return TexState::CloudOnly;").expect("必须有该返回");
        assert!(
            get_thumb < early_return,
            "在读完 BLOB 之前就返回 CloudOnly，会把导入包里已有的缩略图挡掉"
        );
    }
}
