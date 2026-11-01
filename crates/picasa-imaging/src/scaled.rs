//! # JPEG 缩放解码（M2.2 §3.4 PERF-13）
//!
//! ## 什么叫「缩放解码」，为什么它更快
//!
//! JPEG 存的是 **8×8 的 DCT 系数块**，不是像素。还原成像素要过一次 IDCT。
//! 而 IDCT 有个性质：**只取块的低频部分再做一次更小的 IDCT，就等价于先全解再缩小**。
//! 于是 1/8 缩放解码只做全尺寸 1/64 的 IDCT 工作量——解码是缩略图生成里最贵的一步，
//! 这一步省下来，索引吞吐就能翻倍。
//!
//! 关键前提是**它不比全解更糊**：DCT 缩放不是「跳采样」（那会有摩尔纹），
//! 它是**低通滤波后重采样**，理论上比「全解 → 双线性缩」的信噪比还高一点。
//! §3.4 因此给了量化判据：**与 M2.1 全解码再缩的差异 PSNR ≥ 40 dB**。
//! 这个判据是本模块存在的理由——没有它，「更快」很容易做成「更快但更糊」。
//!
//! ## 档位选择（§3.4 原文）
//!
//! 目标边长 ≤ 原图/8 → 1/8；≤ 原图/4 → 1/4；≤ 原图/2 → 1/2；否则 1（不缩放）。
//!
//! 之后**还要** `resize` 到精确档位：DCT 只有 1/8、1/4、1/2 三个离散档，
//! 而我们要的是「长边 ≤ 512」这种精确值，`resize_to_fit` 负责剩下的那点。
//!
//! ## 失败即回落
//!
//! 缩放解码**永远不许让缩略图变少**。非 JPEG、CMYK/灰度、坏文件、解码器报错——
//! 一律返回 `None`，调用方回落到 M2.1 的原路径（[`crate::decode::decode_oriented_bytes`]）。
//! 宁可慢，不可缺图。

use crate::{Result, exif, orient};
use image::{Rgba, RgbaImage};

/// JPEG 的魔数（`FF D8 FF`）。
///
/// 只认前三个字节：比这更复杂的嗅探（找 SOI/SOF）属于解码器的活，
/// 这里只需要「要不要走快路径」的是/否答案。
fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF
}

/// §3.4 的档位选择：返回 **除数**（1 / 2 / 4 / 8）。
///
/// `src_w/src_h` 是**未摆正**的原图尺寸，`target_edge` 是目标长边。
///
/// ⚠️ 用 `max(w, h)` 而不是分别判断宽高：EXIF Orientation 可能把图转 90°，
/// 摆正发生在**缩放之后**（见 [`decode_jpeg_scaled`]），此刻的「长边」该按最坏情况算。
/// 旋转不改变 `max`，所以用它是对的；分别判断宽高则会在竖图上选错档。
pub fn dct_scale_for(src_w: u32, src_h: u32, target_edge: u32) -> u32 {
    let long = src_w.max(src_h);
    if long == 0 || target_edge == 0 {
        return 1;
    }
    let long = long as u64;
    let t = target_edge as u64;
    if t * 8 <= long {
        8
    } else if t * 4 <= long {
        4
    } else if t * 2 <= long {
        2
    } else {
        1
    }
}

/// 缩放解码的统计（供 `TC-M22-PERF-008` 判定快路径真的被走到）。
///
/// 不用 `lazy_static`/`once_cell`：三个 `AtomicU64` 足够，
/// 而且要在 release 下也计数（「更快」这件事需要在真机上被举证，不能只在 debug 里）。
#[derive(Debug, Default)]
pub struct ScaledStats {
    /// 走了 DCT 缩放路径的次数
    pub scaled: std::sync::atomic::AtomicU64,
    /// 回落到全解码的次数（非 JPEG / 不支持的像素格式 / 解码器报错）
    pub fallback: std::sync::atomic::AtomicU64,
}

impl ScaledStats {
    pub fn note_scaled(&self) {
        self.scaled
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn note_fallback(&self) {
        self.fallback
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn scaled_count(&self) -> u64 {
        self.scaled.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn fallback_count(&self) -> u64 {
        self.fallback.load(std::sync::atomic::Ordering::Relaxed)
    }
}

pub static SCALED_STATS: ScaledStats = ScaledStats {
    scaled: std::sync::atomic::AtomicU64::new(0),
    fallback: std::sync::atomic::AtomicU64::new(0),
};

/// 用 DCT 缩放解码一张 JPEG，并按 EXIF Orientation 摆正。
///
/// 返回 `Ok(None)` 表示**不该走这条路**（调用方回落）；`Err` 只在真正的 IO/格式错误时出现
/// ——连回落都不该试的那种（比如字节流根本不是 JPEG）。
///
/// ## 为什么 Orientation 在这里处理而不是交给上层
///
/// 缩放解码绕过了 `image` 的 `ImageReader`，EXIF 得自己读。
/// 好在我们本来就有 [`exif::orientation_from_bytes`]，而且它读的是同一份字节
/// （不额外读盘），代价只是一次 APP1 段扫描。
pub fn decode_jpeg_scaled(bytes: &[u8], target_edge: u32) -> Result<Option<RgbaImage>> {
    if !is_jpeg(bytes) {
        SCALED_STATS.note_fallback();
        return Ok(None);
    }

    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    // 先读头拿尺寸——不知道原图多大就没法选档。
    if dec.read_info().is_err() {
        SCALED_STATS.note_fallback();
        return Ok(None);
    }
    let Some(info) = dec.info() else {
        SCALED_STATS.note_fallback();
        return Ok(None);
    };

    let divisor = dct_scale_for(info.width as u32, info.height as u32, target_edge);
    let req_w = ((info.width as u32 / divisor).max(1)).min(u16::MAX as u32) as u16;
    let req_h = ((info.height as u32 / divisor).max(1)).min(u16::MAX as u32) as u16;
    // `scale()` 会挑「产出 ≥ 请求尺寸」的最小缩放比；我们请求的正是 divisor 档的
    // 精确产出，所以它必然选中 divisor。返回值是**实际**产出尺寸。
    if dec.scale(req_w, req_h).is_err() {
        SCALED_STATS.note_fallback();
        return Ok(None);
    }

    let pixels = match dec.decode() {
        Ok(p) => p,
        Err(_) => {
            SCALED_STATS.note_fallback();
            return Ok(None);
        }
    };
    let (w, h) = match dec.info() {
        Some(i) => (i.width as u32, i.height as u32),
        None => {
            SCALED_STATS.note_fallback();
            return Ok(None);
        }
    };

    let Some(img) = to_rgba(info.pixel_format, pixels, w, h) else {
        // CMYK 之类：`image` 那边有成熟的颜色转换代码，别在这里重造。
        SCALED_STATS.note_fallback();
        return Ok(None);
    };

    SCALED_STATS.note_scaled();

    // 摆正方向与 M2.1 完全一致（同一份 orientation 取值逻辑，不另写一套）
    let orientation = exif::orientation_from_bytes(bytes).unwrap_or(1);
    Ok(Some(orient::apply_orientation(img, orientation)))
}

/// 把 `jpeg-decoder` 的裸像素转成 RGBA8。
///
/// 只处理 `RGB24` 与 `L8`（灰度）——这两种覆盖了照片库的绝大多数。
/// `CMYK32` 返回 `None` 让上层回落：CMYK 转 RGB 的色域映射 `image` 里已经写好，
/// 在这里重造一遍只会得到一个「看起来差不多但印出来不对」的实现。
fn to_rgba(
    format: jpeg_decoder::PixelFormat,
    pixels: Vec<u8>,
    w: u32,
    h: u32,
) -> Option<RgbaImage> {
    let need = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    let mut out = RgbaImage::from_pixel(w, h, Rgba([255, 255, 255, 255]));
    let dst = out.as_mut();
    if dst.len() != need {
        return None;
    }
    match format {
        jpeg_decoder::PixelFormat::RGB24 => {
            if pixels.len() < w as usize * h as usize * 3 {
                return None;
            }
            let n = w as usize * h as usize;
            for i in 0..n {
                let (d, s) = (i * 4, i * 3);
                dst[d] = pixels[s];
                dst[d + 1] = pixels[s + 1];
                dst[d + 2] = pixels[s + 2];
                dst[d + 3] = 255;
            }
        }
        jpeg_decoder::PixelFormat::L8 => {
            if pixels.len() < w as usize * h as usize {
                return None;
            }
            let n = w as usize * h as usize;
            for (i, &g) in pixels.iter().enumerate().take(n) {
                let d = i * 4;
                dst[d] = g;
                dst[d + 1] = g;
                dst[d + 2] = g;
                dst[d + 3] = 255;
            }
        }
        // L16（16bit 灰度 JPEG）与 CMYK32：回落。
        // `image` 那边有成熟的位深/色域转换代码，在这里重造只会得到
        // 「看起来差不多、但边界情况不对」的实现。
        jpeg_decoder::PixelFormat::L16 | jpeg_decoder::PixelFormat::CMYK32 => return None,
    }
    Some(out)
}

/// 两幅同尺寸图像的 PSNR（dB）。
///
/// §3.4 的验收判据就这一条。返回 `f64::INFINITY` 表示完全一致
/// （MSE = 0 时 PSNR 数学上是无穷，不能返回 0 让人以为"差到极点"）。
pub fn psnr(a: &RgbaImage, b: &RgbaImage) -> Option<f64> {
    if a.width() != b.width() || a.height() != b.height() {
        return None;
    }
    let (ra, rb) = (a.as_raw(), b.as_raw());
    let mut sum: u64 = 0;
    let n = ra.len();
    for i in 0..n {
        let d = (ra[i] as i64) - (rb[i] as i64);
        sum += (d * d) as u64;
    }
    let mse = sum as f64 / n as f64;
    if mse == 0.0 {
        return Some(f64::INFINITY);
    }
    // MAX = 255（8bit/channel）
    Some(10.0 * ((255.0f64 * 255.0) / mse).log10())
}

/// 平均绝对误差（0..255）。PSNR 是比值的对数，人不容易直觉理解；
/// 这个数直接回答「平均每个通道差多少级灰阶」，§3.4 的「≤ 2/255」就是它。
pub fn mean_abs_error(a: &RgbaImage, b: &RgbaImage) -> Option<f64> {
    if a.width() != b.width() || a.height() != b.height() {
        return None;
    }
    let (ra, rb) = (a.as_raw(), b.as_raw());
    let mut sum: u64 = 0;
    for i in 0..ra.len() {
        sum += ((ra[i] as i64) - (rb[i] as i64)).unsigned_abs();
    }
    Some(sum as f64 / ra.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode_oriented_bytes;
    use crate::thumb::resize_to_fit;
    use image::{RgbImage, codecs::jpeg::JpegEncoder};

    /// 造一张有真实高频内容的图（纯色图测不出缩放质量差异）。
    fn test_image(w: u32, h: u32) -> RgbImage {
        let mut img = RgbImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            // 棋盘 + 斜向渐变：既有奈奎斯特频率的内容，也有平滑区域
            let checker = if ((x / 4) + (y / 4)) % 2 == 0 { 40 } else { 0 };
            let grad = ((x + y) % 256) as u8;
            px.0 = [grad.saturating_add(checker), (255 - grad) / 2, grad / 3];
        }
        img
    }

    fn jpeg_bytes(img: &RgbImage) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = JpegEncoder::new_with_quality(&mut buf, 92);
        enc.encode(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
        buf
    }

    #[test]
    fn dct_scale_picks_the_coarsest_usable_factor() {
        // 原图 4000 长边
        assert_eq!(
            dct_scale_for(4000, 3000, 512),
            4,
            "512 ≤ 4000/4=1000 且 512 > 4000/8=500"
        );
        assert_eq!(dct_scale_for(4000, 3000, 400), 8, "400 ≤ 500");
        assert_eq!(dct_scale_for(4000, 3000, 1500), 2, "1500 ≤ 2000 且 > 1000");
        assert_eq!(dct_scale_for(4000, 3000, 3000), 1, "3000 > 2000，不缩放");
        assert_eq!(
            dct_scale_for(4000, 3000, 9999),
            1,
            "目标比原图还大 → 不缩放"
        );
        // 竖图：用 max(w,h) 判断，不会因为宽只有 3000 就选错档
        assert_eq!(dct_scale_for(3000, 4000, 512), 4);
        // 退化输入
        assert_eq!(dct_scale_for(0, 0, 512), 1);
        assert_eq!(dct_scale_for(4000, 3000, 0), 1);
    }

    #[test]
    fn scaled_decode_is_within_the_psnr_gate() {
        let src = test_image(1600, 1200);
        let bytes = jpeg_bytes(&src);
        let target = 512u32;

        // 快路径：DCT 缩放解码 → 精确 resize
        let scaled = decode_jpeg_scaled(&bytes, target)
            .unwrap()
            .expect("JPEG 必须走成快路径");
        let fast = resize_to_fit(&scaled, target);

        // 基线：M2.1 的全解码 → 摆正 → resize
        let full = decode_oriented_bytes(&bytes).unwrap();
        let slow = resize_to_fit(&full, target);

        assert_eq!(fast.width(), slow.width(), "两条路径必须产出同尺寸");
        assert_eq!(fast.height(), slow.height());

        let p = psnr(&fast, &slow).expect("同尺寸");
        let mae = mean_abs_error(&fast, &slow).expect("同尺寸");
        assert!(
            p >= 40.0,
            "PSNR 必须 ≥ 40 dB（§3.4 防「更快但更糊」），实得 {p:.2} dB，MAE {mae:.3}"
        );
        assert!(mae <= 2.0, "平均绝对误差 ≤ 2/255（§3.4），实得 {mae:.3}");
    }

    #[test]
    fn scaled_decode_actually_shrinks_work() {
        let src = test_image(1600, 1200);
        let bytes = jpeg_bytes(&src);
        let got = decode_jpeg_scaled(&bytes, 512).unwrap().unwrap();
        // 1600/2 = 800（1/4 档只有 400，比目标 512 小，不能用）
        assert_eq!(got.width(), 800, "1600 长边、目标 512 → 2 档 → 800");
        assert_eq!(got.height(), 600);
    }

    #[test]
    fn non_jpeg_and_garbage_fall_back_without_erroring() {
        // 不是 JPEG
        assert!(decode_jpeg_scaled(&[0u8; 64], 512).unwrap().is_none());
        // 是 JPEG 头但内容坏
        let mut bad = vec![0xFF, 0xD8, 0xFF];
        bad.extend_from_slice(&[0x00; 32]);
        assert!(decode_jpeg_scaled(&bad, 512).unwrap().is_none());
    }

    #[test]
    fn psnr_reports_infinity_for_identical_images() {
        let a = RgbaImage::from_pixel(4, 4, Rgba([1, 2, 3, 255]));
        let b = a.clone();
        assert!(psnr(&a, &b).unwrap().is_infinite());
        assert_eq!(mean_abs_error(&a, &b).unwrap(), 0.0);
        // 尺寸不同 → 不可比，返回 None 而不是瞎算
        let c = RgbaImage::from_pixel(8, 8, Rgba([1, 2, 3, 255]));
        assert!(psnr(&a, &c).is_none());
    }
}
