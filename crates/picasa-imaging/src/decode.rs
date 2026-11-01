//! 解码：只读打开文件，**从不以写方式打开**（R-1 的第一道结构性保证）。

use crate::{ImageError, Result, exif, orient};
use image::{DynamicImage, ImageReader, RgbaImage};
use std::path::Path;

/// 只读探测图片像素尺寸（不解全图）。
///
/// 优先用 `into_dimensions()`（只读文件头），失败再退回全解码。
pub fn probe_dimensions(path: &Path) -> Result<(u32, u32)> {
    let reader = ImageReader::open(path)
        .map_err(|e| ImageError::Unsupported(format!("{}: {e}", path.display())))?
        .with_guessed_format()?;
    match reader.into_dimensions() {
        Ok((w, h)) => Ok((w, h)),
        Err(_) => {
            let img = decode(path)?;
            Ok((img.width(), img.height()))
        }
    }
}

/// 解码为 RGBA8（**不**摆正方向）。
pub fn decode(path: &Path) -> Result<RgbaImage> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let img: DynamicImage = reader.decode()?;
    Ok(img.to_rgba8())
}

/// 从内存字节解码为 RGBA8。
///
/// 存在的理由很实际：缩略图要**同一份文件字节**用两次（解码 + 读 EXIF Orientation）。
/// 老实现 `decode_oriented` 是「`ImageReader::open` 读一遍 + `read_orientation`
/// 再 open 一遍」，在 USB 移动硬盘上等于把 I/O 翻倍——25.9 万张的库，这个翻倍是小时级的。
pub fn decode_bytes(bytes: &[u8]) -> Result<RgbaImage> {
    let reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let img: DynamicImage = reader.decode()?;
    Ok(img.to_rgba8())
}

/// 解码并按 EXIF Orientation 摆正（F2-17 的落地实现）。
///
/// M1 的照片方向**只由 EXIF Orientation 决定**——没有旋转指令、没有用户状态，
/// 所以这里没有任何「叠加逻辑」，也就没有「方向错乱」的经典 bug 面。
pub fn decode_oriented(path: &Path) -> Result<RgbaImage> {
    let bytes = std::fs::read(path)?;
    decode_oriented_bytes(&bytes)
}

/// 同 [`decode_oriented`]，但字节已经在内存里（扫描主路径：一次读盘，两处使用）。
pub fn decode_oriented_bytes(bytes: &[u8]) -> Result<RgbaImage> {
    let img = decode_bytes(bytes)?;
    let orientation = exif::orientation_from_bytes(bytes).unwrap_or(1);
    Ok(orient::apply_orientation(img, orientation))
}
