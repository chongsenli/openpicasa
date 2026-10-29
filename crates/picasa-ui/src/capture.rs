//! 窗口自截图：**给「界面还原」做可自动化、可重复的基线**。
//!
//! ## 为什么不用系统截屏
//!
//! 用 `screencapture` 截屏有三个不可控因素：需要「屏幕录制」授权、会把别的窗口一起拍进来、
//! 而且拿到的是**整屏像素**，得靠人眼反推窗口矩形——只要反推错一次，就可能把
//! 「栅格是单列」这种渲染 bug 误判成「窗口太窄」。
//!
//! 这里改成向 eframe 直接要**本窗口自己的帧缓冲**（`ViewportCommand::Screenshot`）：
//! 坐标系、DPI、窗口边界都由应用自己决定，截出来就是用户看到的这一帧，一个像素不多。
//!
//! ## 用法
//!
//! ```text
//! OPENPICASA_SCREENSHOT=/tmp/a.png OPENPICASA_SCREENSHOT_WARMUP=40 OpenPicasa
//! ```
//!
//! 跑到第 `warmup` 帧时请求截图 → 写 PNG（物理像素）→ 关窗退出。
//! `warmup` 存在的原因是：缩略图是**每帧限量上传**的（`TEX_UPLOAD_PER_FRAME = 3`），
//! 第一帧必然是大片灰色占位符，不能作为还原度基线。

use eframe::egui::{Context, ViewportCommand};
use std::path::PathBuf;
use std::sync::Arc;

/// 默认预热帧数：3 张/帧的上传节奏下，40 帧足够把一屏几十张缩略图铺满。
const DEFAULT_WARMUP: u32 = 40;

pub struct Capture {
    path: PathBuf,
    warmup: u32,
    frames: u32,
    requested: bool,
}

impl Capture {
    /// 从环境变量读配置；没设 `OPENPICASA_SCREENSHOT` 就是普通的交互运行。
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("OPENPICASA_SCREENSHOT")?;
        let warmup = std::env::var("OPENPICASA_SCREENSHOT_WARMUP")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(DEFAULT_WARMUP);
        Some(Self {
            path: PathBuf::from(path),
            warmup,
            frames: 0,
            requested: false,
        })
    }

    /// 每帧调用一次。返回 `true` 表示「还没到位，需要继续出帧」。
    pub fn tick(&mut self, ctx: &Context) -> bool {
        if self.requested {
            return false;
        }
        self.frames += 1;
        if self.frames < self.warmup {
            ctx.request_repaint();
            return true;
        }
        self.requested = true;
        let path = self.path.clone();
        let ctx2 = ctx.clone();
        ctx.request_screenshot(move |img: Arc<egui::ColorImage>| {
            let [w, h] = img.size;
            match save_png(&path, &img) {
                Ok(()) => tracing::info!(path = %path.display(), w, h, "已保存窗口截图"),
                Err(e) => tracing::error!(error = %e, path = %path.display(), "保存截图失败"),
            }
            // 截图模式是一次性的批处理：写完就关，方便脚本 `&& echo ok`
            ctx2.send_viewport_cmd(ViewportCommand::Close);
        });
        true
    }
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> Result<(), String> {
    let [w, h] = img.size;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for p in &img.pixels {
        rgba.extend_from_slice(&[p.r(), p.g(), p.b(), p.a()]);
    }
    image::save_buffer(path, &rgba, w as u32, h as u32, image::ColorType::Rgba8)
        .map_err(|e| e.to_string())
}
