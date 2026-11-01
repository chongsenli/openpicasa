//! 缩略图：等比缩放 → 白底合成 → JPEG 编码。
//!
//! ## 三条硬规则（对齐 01 U-25 / U-24）
//!
//! 1. **等比缩放，不裁切不拉伸**：长边贴合目标，短边留白。
//! 2. **半透明 PNG 合到白底**：Picasa 的栅格是白底，透明图直接编码 JPEG 会变黑。
//! 3. **缩略图里不含文件名、不含星标**——那些是 UI 层画的，不是烘焙进图片的。

use crate::{Result, THUMB_T0, decode, orient};
use image::{RgbaImage, codecs::jpeg::JpegEncoder, imageops};
use std::path::Path;

/// 缩放后的内存位图（还带着 alpha，编码前才合白底）。
pub struct ThumbImage {
    pub img: RgbaImage,
}

impl ThumbImage {
    pub fn size(&self) -> (u32, u32) {
        (self.img.width(), self.img.height())
    }
}

/// 等比缩放到「长边 ≤ max_edge」。已经更小的图**不放大**（避免糊）。
pub fn resize_to_fit(img: &RgbaImage, max_edge: u32) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return img.clone();
    }
    if w <= max_edge && h <= max_edge {
        return img.clone();
    }
    let scale = max_edge as f32 / w.max(h) as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    // Triangle 比 Lanczos3 快 3-4 倍，缩略图尺寸下肉眼无差；成本花在解码上更值。
    imageops::resize(img, nw, nh, imageops::FilterType::Triangle)
}

/// 把 alpha 合到白底，输出 RGB8（JPEG 没有 alpha 通道）。
///
/// 实现上**不用 `put_pixel`**：那是「每像素一次边界检查 + 一次元组解构」，
/// 在 debug 构建里比线性扫一遍慢一个数量级，而缩略图每条像素都要过一次。
/// 这里直接走原始切片，全不透明（绝大多数 JPEG）时连混合都不用算。
pub fn flatten_on_white(img: &RgbaImage) -> image::RgbImage {
    let (w, h) = (img.width(), img.height());
    let src = img.as_raw();
    let mut out: Vec<u8> = Vec::with_capacity(src.len() / 4 * 3);
    let (pxs, _) = src.as_chunks::<4>();
    let opaque = pxs.iter().all(|px| px[3] == 255);
    if opaque {
        for px in pxs {
            out.extend_from_slice(&px[..3]);
        }
    } else {
        for px in pxs {
            let a = px[3] as u32;
            if a == 255 {
                out.extend_from_slice(&px[..3]);
            } else {
                for c in &px[..3] {
                    let v = ((*c as u32) * a + 255 * (255 - a)) / 255;
                    out.push(v as u8);
                }
            }
        }
    }
    // 尺寸与容量都对齐了，`from_raw` 不会返回 None
    image::RgbImage::from_raw(w, h, out).unwrap_or_else(|| image::RgbImage::new(w, h))
}

/// JPEG 编码（质量 82：缩略图看的是密度与速度，不是无损）。
pub fn encode_jpeg(img: &RgbaImage, quality: u8) -> Result<Vec<u8>> {
    let rgb = flatten_on_white(img);
    let mut buf = Vec::with_capacity(32 * 1024);
    let mut enc = JpegEncoder::new_with_quality(&mut buf, quality);
    enc.encode(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(buf)
}

/// 从文件直接产出 T0 缩略图字节（扫描/补图的主路径）。
///
/// 走「解码 → 摆正 → 缩放 → 合白底 → 编码」五步，**中间不落盘**；
/// 文件只读一次（解码与 EXIF Orientation 共用同一份字节）。
pub fn thumb_from_file(path: &Path, max_edge: u32) -> Result<(Vec<u8>, u32, u32)> {
    let bytes = std::fs::read(path)?;
    thumb_from_bytes(&bytes, max_edge)
}

/// 同 [`thumb_from_file`]，但字节已在内存里。
///
/// ## M2.2 PERF-13：JPEG 先走 DCT 缩放解码
///
/// 流程变成「**缩放解码** → 摆正 → 精确 resize → 合白底 → 编码」。
/// 缩放解码失败（非 JPEG / CMYK / 16bit 灰度 / 坏文件）时**静默回落**到 M2.1 的
/// 「全解码 → 摆正」——宁可慢，不可缺图。`SCALED_STATS` 记录两条路径各走了多少次。
pub fn thumb_from_bytes(bytes: &[u8], max_edge: u32) -> Result<(Vec<u8>, u32, u32)> {
    let img = match crate::scaled::decode_jpeg_scaled(bytes, max_edge)? {
        Some(fast) => fast,
        None => decode::decode_oriented_bytes(bytes)?,
    };
    let small = resize_to_fit(&img, max_edge);
    let (w, h) = (small.width(), small.height());
    let out = encode_jpeg(&small, 82)?;
    Ok((out, w, h))
}

/// T0 档的便捷入口。
pub fn thumb_t0(path: &Path) -> Result<(Vec<u8>, u32, u32)> {
    thumb_from_file(path, THUMB_T0)
}

/// 只按 Orientation 算「摆正后的尺寸」，不解全图（用于占位框的行高预估）。
pub fn oriented_size(w: u32, h: u32, orientation: u32) -> (u32, u32) {
    if orient::orientation_swaps_axes(orientation) {
        (h, w)
    } else {
        (w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(px))
    }

    #[test]
    fn resize_keeps_aspect_ratio() {
        let img = solid(4000, 3000, [10, 20, 30, 255]);
        let out = resize_to_fit(&img, 512);
        assert_eq!(out.width(), 512);
        assert_eq!(out.height(), 384); // 4000:3000 = 4:3
    }

    #[test]
    fn resize_never_upscales() {
        let img = solid(100, 80, [1, 2, 3, 255]);
        let out = resize_to_fit(&img, 512);
        assert_eq!((out.width(), out.height()), (100, 80));
    }

    #[test]
    fn portrait_scales_by_long_edge() {
        let img = solid(300, 900, [0, 0, 0, 255]);
        let out = resize_to_fit(&img, 512);
        assert_eq!((out.width(), out.height()), (171, 512));
    }

    #[test]
    fn transparent_becomes_white_not_black() {
        let img = solid(4, 4, [0, 0, 0, 0]); // 全透明
        let rgb = flatten_on_white(&img);
        assert_eq!(rgb.get_pixel(0, 0).0, [255, 255, 255]);
    }

    #[test]
    fn opaque_pixels_pass_through() {
        let img = solid(2, 2, [12, 34, 56, 255]);
        let rgb = flatten_on_white(&img);
        assert_eq!(rgb.get_pixel(1, 1).0, [12, 34, 56]);
    }

    #[test]
    fn encodes_valid_jpeg() {
        let img = solid(64, 48, [200, 100, 50, 255]);
        let bytes = encode_jpeg(&img, 82).unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "JPEG magic");
        // 能被解回来，且尺寸一致
        let back = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!((back.width(), back.height()), (64, 48));
    }

    /// PERF-13 的**接线**测试：主路径真的走了缩放解码，而不是只在 `scaled` 模块里自嗨。
    ///
    /// 这条用例存在的理由：`decode_jpeg_scaled` 全绿但没人调用，是这类优化最常见的
    /// 落地失败方式——「更快的代码写好了，主路径还在走老路」。
    #[test]
    fn thumb_pipeline_takes_the_scaled_decode_path() {
        use crate::scaled::SCALED_STATS;
        let mut src = image::RgbImage::new(1600, 1200);
        for (x, y, px) in src.enumerate_pixels_mut() {
            px.0 = [((x + y) % 256) as u8, (x % 256) as u8, (y % 256) as u8];
        }
        let mut bytes = Vec::new();
        let mut enc = JpegEncoder::new_with_quality(&mut bytes, 90);
        enc.encode(
            src.as_raw(),
            src.width(),
            src.height(),
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();

        let before = SCALED_STATS.scaled_count();
        let (out, w, h) = thumb_from_bytes(&bytes, 512).unwrap();
        assert!(
            SCALED_STATS.scaled_count() > before,
            "JPEG 主路径必须走 DCT 缩放解码"
        );
        assert_eq!(&out[..2], &[0xFF, 0xD8], "产出仍是合法 JPEG");
        assert!(w <= 512 && h <= 512, "长边贴合目标：{w}x{h}");

        // 非 JPEG 必须回落，且**仍然出图**（不能因为快路径不可用就缺缩略图）
        let fb = SCALED_STATS.fallback_count();
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(64, 48, image::Rgb([1, 2, 3])))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let (out2, w2, h2) = thumb_from_bytes(&png, 512).unwrap();
        assert!(SCALED_STATS.fallback_count() > fb, "非 JPEG 记一次回落");
        assert_eq!(&out2[..2], &[0xFF, 0xD8]);
        assert_eq!((w2, h2), (64, 48), "小图不放大");
    }

    #[test]
    fn oriented_size_swaps_for_rotations() {
        assert_eq!(oriented_size(4000, 3000, 1), (4000, 3000));
        assert_eq!(oriented_size(4000, 3000, 6), (3000, 4000));
        assert_eq!(oriented_size(4000, 3000, 8), (3000, 4000));
        assert_eq!(oriented_size(4000, 3000, 3), (4000, 3000));
    }
}
