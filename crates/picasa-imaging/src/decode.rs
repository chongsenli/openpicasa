//! 解码：只读打开文件，**从不以写方式打开**（R-1 的第一道结构性保证）。

use crate::{ImageError, Result, exif, orient};
use image::{DynamicImage, ImageReader, RgbaImage};
use std::path::Path;

/// 只读探测图片像素尺寸（不解全图）。
///
/// 优先用 `into_dimensions()`（只读文件头），失败再退回全解码。
pub fn probe_dimensions(path: &Path) -> Result<(u32, u32)> {
    // HEIF / RAW 的文件头 `image` 读不了：先嗅探，命中就全解码拿尺寸
    // （HEIC 的 heif 元数据盒里其实有 ispe 尺寸，但 rawler/heic 的 API 都以
    // 「解码即得」为主——首帧 0.5～1 s，宽高回填线程能接受）。
    let mut head = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open(path)
        && std::io::Read::read_exact(&mut f, &mut head).is_ok()
    {
        match crate::format::sniff(&head) {
            crate::format::SniffedFormat::Heic
            | crate::format::SniffedFormat::Avif
            | crate::format::SniffedFormat::RawContainer => {
                let img = decode(path)?;
                return Ok((img.width(), img.height()));
            }
            _ => {}
        }
    }
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
    // 统一走 `decode_bytes`：HEIF/RAW 的魔数分流只写一份（解码入口多一处漏分流的
    // 代价是「明明支持却报格式不支持」，M4 修 dims 回填时真实踩到）。
    let bytes = std::fs::read(path)?;
    decode_bytes(&bytes)
}

/// 从内存字节解码为 RGBA8。
///
/// 存在的理由很实际：缩略图要**同一份文件字节**用两次（解码 + 读 EXIF Orientation）。
/// 老实现 `decode_oriented` 是「`ImageReader::open` 读一遍 + `read_orientation`
/// 再 open 一遍」，在 USB 移动硬盘上等于把 I/O 翻倍——25.9 万张的库，这个翻倍是小时级的。
pub fn decode_bytes(bytes: &[u8]) -> Result<RgbaImage> {
    // `image` 覆盖不了的格式先按魔数分流（用户功能 #3：HEIC / AVIF / RAW）。
    // 分流在 `image` 之前：这些格式的字节喂给 `image` 只会得到无意义的
    // 「格式不支持」，而魔数嗅探一次 12 字节的代价可以忽略。
    match crate::format::sniff(bytes) {
        crate::format::SniffedFormat::Heic | crate::format::SniffedFormat::Avif => {
            return decode_heif_bytes(bytes);
        }
        crate::format::SniffedFormat::RawContainer => return decode_raw_bytes(bytes),
        _ => {}
    }
    let reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let img: DynamicImage = reader.decode()?;
    Ok(img.to_rgba8())
}

/// HEIC / HEIF / AVIF → RGBA8（`heic` crate，纯 Rust，无 C 依赖）。
fn decode_heif_bytes(bytes: &[u8]) -> Result<RgbaImage> {
    let out = heic::DecoderConfig::new()
        .decode(bytes, heic::PixelLayout::Rgba8)
        .map_err(|e| ImageError::Unsupported(format!("HEIF 解码失败: {e}")))?;
    let stride_check = out.data.len();
    let expected = out.width as usize * out.height as usize * 4;
    if stride_check != expected {
        return Err(ImageError::Unsupported(format!(
            "HEIF 输出尺寸不符：{stride_check} != {expected}"
        )));
    }
    image::RgbaImage::from_raw(out.width, out.height, out.data)
        .ok_or_else(|| ImageError::Unsupported("HEIF 像素缓冲尺寸不符".into()))
}

/// RAW（CR2/CR3/NEF/ARW/DNG…）→ RGBA8（`rawler`，纯 Rust）。
///
/// 缩略图走 full develop：`rawler` 的 `thumbnail_image` 只有部分格式支持，
/// 而 develop 出的图对 512² 的缩略图尺寸刚好（一步到位，避免两套代码）。
/// 性能上单张 RAW develop 约 0.3～2 s，后台线程可接受（M3 管线本来就不阻塞 UI）。
fn decode_raw_bytes(bytes: &[u8]) -> Result<RgbaImage> {
    use rawler::decoders::RawDecodeParams;
    use rawler::rawsource::RawSource;
    let source = RawSource::new_from_slice(bytes);
    // 优先取嵌入缩略图/预览（几十 ms，画质足够 512² 缩略图）；
    // 没有嵌入图时退 full develop（去马赛克 + 色彩矩阵，单张 0.3～2 s，后台线程可接受）。
    let decoder = rawler::get_decoder(&source)
        .map_err(|e| ImageError::Unsupported(format!("RAW 打不开: {e}")))?;
    if let Ok(Some(thumb)) = decoder.thumbnail_image(&source, &RawDecodeParams::default()) {
        let (w, h) = (thumb.width(), thumb.height());
        if w > 0 && h > 0 {
            return Ok(thumb.to_rgba8());
        }
    }
    // full develop 链（去马赛克/色彩矩阵/gamma）在 rawler 0.8 里要
    // `RawDevelop::default().develop_intermediate()` + 手工转 sRGB，
    // 且单张 0.3～2 s。M4 首发只承诺「能看见」：嵌入图没有就报错进失败队列，
    // full develop 留给后续（用户看大图时查看器可以走长路径）。
    Err(ImageError::Unsupported(
        "RAW 无嵌入缩略图，full develop 尚未启用".into(),
    ))
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
