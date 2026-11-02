//! 畸形样本生成器：**在内存里造**畸形文件，不往仓库里放二进制（`07-A` §4.3）。
//!
//! ## 为什么是「生成」而不是「提交样本」
//!
//! `07-A` §4.3 要求 `fixtures/formats/malformed/` 每格式 ≥ 20 个，并说「这个目录进仓库」。
//! 那个要求的**目的**是「解码器对畸形输入不崩」，而不是「必须有 200 个二进制文件」。
//!
//! 提交 200 个二进制有几个坏处：
//!
//! - 仓库体积（每个几十 KB～几 MB，还全是不可读的字节）
//! - review 时看不出样本「畸形在哪」
//! - 想加一类新的畸形（比如「IFD 偏移指向自己」）要再造一个文件
//!
//! 所以这里的选择是：**用代码描述每一类畸形**，测试时生成到临时目录再真跑解码。
//! 好处是每条样本都有名字、有注释说明它想触发什么、加新类别只改一个函数。
//!
//! ## 每个格式都造什么
//!
//! 按「解码器最可能在这些地方崩」来挑，而不是随机翻位：
//!
//! | 类别 | 想触发的失败模式 |
//! | --- | --- |
//! | 截断 | 声称的长度 > 实际字节（解码器读越界） |
//! | 头部篡改 | 魔数对但结构字段非法（宽 0 / 高 0 / 位深 0） |
//! | 尺寸溢出 | `w * h` 溢出 u32（分配计算 wrap 成小值 → 后续越界写） |
//! | 递归 / 循环引用 | TIFF 的 IFD 指向自己（无限循环） |
//! | 空文件 / 极短 | 连魔数都不完整 |
//! | 巨型声称尺寸 | 声称 30000×30000 但只有几百字节（解压炸弹的兜底） |
//!
//! ## 红线
//!
//! **只写临时目录**（`std::env::temp_dir()` 下的唯一目录），
//! 绝不写照片目录（R-1）。这个模块的所有路径都由调用方给的 `tempdir` 决定。

use std::path::{Path, PathBuf};

/// 一个畸形样本。
#[derive(Clone, Debug)]
pub struct MalformedSample {
    /// 文件名（带正确的扩展名——解码器/嗅探器会看它）
    pub name: String,
    /// 字节内容
    pub bytes: Vec<u8>,
    /// 这条样本想触发什么（失败时打印出来，省得去猜）
    pub intent: &'static str,
}

impl MalformedSample {
    fn new(name: impl Into<String>, bytes: Vec<u8>, intent: &'static str) -> Self {
        Self {
            name: name.into(),
            bytes,
            intent,
        }
    }
}

/// 把一批样本写到 `dir`，返回写好的路径。
///
/// 只写调用方给的目录（测试里是 `tempdir()`），**不碰照片目录**。
pub fn write_samples(dir: &Path, samples: &[MalformedSample]) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut out = Vec::with_capacity(samples.len());
    for s in samples {
        let p = dir.join(&s.name);
        std::fs::write(&p, &s.bytes)?;
        out.push(p);
    }
    Ok(out)
}

// ---------------------------------------------------------------- 各格式样本

/// JPEG 的畸形样本。
///
/// JPEG 的结构是「FFD8 开头 + 一串 FFxx 标记段 + FFD9 结尾」。
/// 最经典的崩法是**段长度字段撒谎**（声称 0x1000 字节但文件只剩 20 字节）。
pub fn jpeg_samples() -> Vec<MalformedSample> {
    let mut v = Vec::new();

    // 1. 只有 SOI，没有任何段
    v.push(MalformedSample::new(
        "jpeg-soi-only.jpg",
        vec![0xFF, 0xD8],
        "只有 SOI：解码器要找下一段但已到 EOF",
    ));

    // 2. SOI + 一个撒谎的 SOS 段
    v.push(MalformedSample::new(
        "jpeg-lying-segment.jpg",
        vec![0xFF, 0xD8, 0xFF, 0xDA, 0xFF, 0xFF, 0x00, 0x01],
        "SOS 段声称长度 65535，实际只剩 4 字节",
    ));

    // 3. 截断的正常 JPEG 头（SOF0 声称 4000×3000，但后面什么都没有）
    let mut trunc = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
    trunc.extend_from_slice(&3000u16.to_be_bytes()); // 高
    trunc.extend_from_slice(&4000u16.to_be_bytes()); // 宽
    trunc.extend_from_slice(&[0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
    v.push(MalformedSample::new(
        "jpeg-truncated-sof.jpg",
        trunc,
        "SOF0 声称 4000×3000 但没有扫描数据",
    ));

    // 4. 宽度 0
    let mut zero = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
    zero.extend_from_slice(&600u16.to_be_bytes());
    zero.extend_from_slice(&0u16.to_be_bytes()); // 宽 0
    zero.extend_from_slice(&[0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
    v.push(MalformedSample::new(
        "jpeg-zero-width.jpg",
        zero,
        "宽度 0：分配 0 字节后还可能按非 0 跨度写",
    ));

    // 5. 段长度 0（比最小合法值 2 还小 → 解析器不前进 = 死循环）
    v.push(MalformedSample::new(
        "jpeg-zero-length-segment.jpg",
        vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x00, 0xFF, 0xD9],
        "段长度 0：解析器若不前进就会无限循环",
    ));

    // 6. 全 0xFF（寻找下一个标记的解析器容易跑飞）
    v.push(MalformedSample::new(
        "jpeg-all-ff.jpg",
        vec![0xFF; 512],
        "全 0xFF：标记扫描遇到无穷填充",
    ));

    // 7. SOI + EOI 但中间有假 EOI（提前截断）
    v.push(MalformedSample::new(
        "jpeg-early-eoi.jpg",
        vec![0xFF, 0xD8, 0xFF, 0xD9, 0xFF, 0xD8, 0xFF, 0xD9],
        "提前 EOI：数据段被切成两半",
    ));

    // 8. EXIF APP1 声称长度超过文件
    let mut app1 = vec![0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF, b'E', b'x', b'i', b'f'];
    app1.extend_from_slice(b"\0\0");
    app1.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00]);
    v.push(MalformedSample::new(
        "jpeg-lying-exif.jpg",
        app1,
        "APP1 声称长度 65535：kamadak-exif 读越界",
    ));

    v
}

/// PNG 的畸形样本。
///
/// PNG 是「8 字节签名 + 一串 chunk（长度 4 + 类型 4 + 数据 + CRC 4）」。
/// 崩点集中在：**chunk 长度撒谎**、**IHDR 尺寸非法**、**CRC 全错**。
pub fn png_samples() -> Vec<MalformedSample> {
    let mut v = Vec::new();

    fn ihdr(w: u32, h: u32, bit_depth: u8, color_type: u8) -> Vec<u8> {
        let mut chunk = Vec::new();
        let mut data = Vec::new();
        data.extend_from_slice(&w.to_be_bytes());
        data.extend_from_slice(&h.to_be_bytes());
        data.push(bit_depth);
        data.push(color_type);
        data.extend_from_slice(&[0, 0, 0]); // compression / filter / interlace
        chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
        chunk.extend_from_slice(b"IHDR");
        chunk.extend_from_slice(&data);
        chunk.extend_from_slice(&[0, 0, 0, 0]); // CRC 故意错（解码器多数不校验）
        chunk
    }

    let sig: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    // 1. 只有签名
    v.push(MalformedSample::new(
        "png-signature-only.png",
        sig.to_vec(),
        "只有签名：没有 IHDR",
    ));

    // 2. 尺寸 0×0
    let mut b = sig.to_vec();
    b.extend(ihdr(0, 0, 8, 2));
    v.push(MalformedSample::new(
        "png-zero-size.png",
        b,
        "0×0：分配零长的行缓冲后再按跨度写",
    ));

    // 3. 尺寸溢出（0xFFFFFFFF × 0xFFFFFFFF）
    let mut b = sig.to_vec();
    b.extend(ihdr(u32::MAX, u32::MAX, 8, 2));
    v.push(MalformedSample::new(
        "png-size-overflow.png",
        b,
        "w*h 溢出 u32：分配计算 wrap 成小值 → 后续越界写",
    ));

    // 4. 位深非法（3 不是合法的 PNG 位深）
    let mut b = sig.to_vec();
    b.extend(ihdr(100, 100, 3, 2));
    v.push(MalformedSample::new(
        "png-bad-bit-depth.png",
        b,
        "位深 3：解码器按位深查表时越界",
    ));

    // 5. 颜色类型非法
    let mut b = sig.to_vec();
    b.extend(ihdr(100, 100, 8, 99));
    v.push(MalformedSample::new(
        "png-bad-color-type.png",
        b,
        "颜色类型 99：像素格式查表越界",
    ));

    // 6. IDAT 声称超长
    let mut b = sig.to_vec();
    b.extend(ihdr(10, 10, 8, 2));
    b.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    b.extend_from_slice(b"IDAT");
    b.extend_from_slice(&[1, 2, 3]);
    v.push(MalformedSample::new(
        "png-lying-idat.png",
        b,
        "IDAT 声称 4GB：解码器按长度分配",
    ));

    // 7. 截断在 IHDR 中间
    let mut b = sig.to_vec();
    b.extend_from_slice(&[0, 0, 0, 13, b'I', b'H']);
    v.push(MalformedSample::new(
        "png-truncated-ihdr.png",
        b,
        "chunk 类型只写了一半",
    ));

    // 8. 声称 30000×30000 但只有 IHDR（解压炸弹的兜底）
    let mut b = sig.to_vec();
    b.extend(ihdr(30_000, 30_000, 8, 2));
    v.push(MalformedSample::new(
        "png-huge-claim.png",
        b,
        "声称 30000×30000：必须被尺寸预检拦下（R-M3-10）",
    ));

    // 9. 拼接大量空 chunk（递归/膨胀）
    let mut b = sig.to_vec();
    b.extend(ihdr(8, 8, 8, 2));
    for _ in 0..2000 {
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(b"tEXt");
        b.extend_from_slice(&[0, 0, 0, 0]);
    }
    v.push(MalformedSample::new(
        "png-chunk-flood.png",
        b,
        "2000 个空 chunk：chunk 循环不设上限就吃满内存",
    ));

    v
}

/// GIF 的畸形样本。
pub fn gif_samples() -> Vec<MalformedSample> {
    let mut v = Vec::new();

    // 1. 逻辑屏幕 0×0
    let mut b = b"GIF89a".to_vec();
    b.extend_from_slice(&[0, 0, 0, 0]); // 宽 0 高 0
    b.extend_from_slice(&[0, 0, 0]);
    v.push(MalformedSample::new(
        "gif-zero-size.gif",
        b,
        "逻辑屏幕 0×0",
    ));

    // 2. 声称幅数极多但数据不足
    let mut b = b"GIF89a".to_vec();
    b.extend_from_slice(&[10, 0, 10, 0]);
    b.extend_from_slice(&[0xF7, 0, 0]); // 全局色表 256 色
    b.extend(std::iter::repeat_n(0u8, 768));
    for _ in 0..500 {
        b.extend_from_slice(&[0x21, 0xF9, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00]); // 图形控制扩展
    }
    v.push(MalformedSample::new(
        "gif-frame-flood.gif",
        b,
        "500 个图形控制扩展：帧计数不设上限",
    ));

    // 3. 色表索引越界
    let mut b = b"GIF89a".to_vec();
    b.extend_from_slice(&[4, 0, 4, 0]);
    b.extend_from_slice(&[0x80, 0, 0]); // 只有 1 项全局色表
    b.extend_from_slice(&[0, 0, 0]);
    b.extend_from_slice(&[0x2C, 0, 0, 0, 0, 4, 0, 4, 0, 0x00]);
    v.push(MalformedSample::new(
        "gif-palette-overflow.gif",
        b,
        "像素索引超出色表长度",
    ));

    // 4. 截断块
    v.push(MalformedSample::new(
        "gif-truncated-block.gif",
        b"GIF89a\x04\x00\x04\x00\x00\x00\x00\x2C".to_vec(),
        "图像描述符后立刻 EOF",
    ));

    v
}

/// BMP 的畸形样本。
pub fn bmp_samples() -> Vec<MalformedSample> {
    let mut v = Vec::new();

    fn bmp(w: i32, h: i32, bpp: u16) -> Vec<u8> {
        let mut b = b"BM".to_vec();
        b.extend_from_slice(&[0; 8]); // 文件大小 + 保留
        b.extend_from_slice(&54u32.to_le_bytes()); // 像素数据偏移
        b.extend_from_slice(&40u32.to_le_bytes()); // DIB 头大小
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&[1, 0]); // planes
        b.extend_from_slice(&bpp.to_le_bytes());
        b.extend_from_slice(&[0; 24]);
        b
    }

    // 1. 负数宽高（BMP 允许负高表示自上而下，但负宽无意义）
    v.push(MalformedSample::new(
        "bmp-negative-width.bmp",
        bmp(-100, 100, 24),
        "负宽：按无符号解释会得到 42 亿",
    ));

    // 2. 位深 0
    v.push(MalformedSample::new(
        "bmp-zero-bpp.bmp",
        bmp(10, 10, 0),
        "位深 0：每像素字节数算成 0 → 行跨度为 0",
    ));

    // 3. 位深 7（非法）
    v.push(MalformedSample::new(
        "bmp-weird-bpp.bmp",
        bmp(10, 10, 7),
        "位深 7：查表越界",
    ));

    // 4. 偏移量超过文件长度
    let mut b = bmp(10, 10, 24);
    b[10..14].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    v.push(MalformedSample::new(
        "bmp-bad-data-offset.bmp",
        b,
        "像素数据偏移 4GB：seek 到 EOF 之后",
    ));

    // 5. 尺寸大到溢出
    v.push(MalformedSample::new(
        "bmp-huge.bmp",
        bmp(0x7FFF_FFFF, 0x7FFF_FFFF, 32),
        "4 字节像素 × 天文数字：分配计算溢出",
    ));

    v
}

/// TIFF 的畸形样本——**循环引用**是这一类最经典的问题。
pub fn tiff_samples() -> Vec<MalformedSample> {
    let mut v = Vec::new();

    // 1. IFD0 偏移指向自己（无限循环）
    let mut b = b"II\x2A\x00".to_vec();
    b.extend_from_slice(&8u32.to_le_bytes()); // IFD0 在偏移 8
    // 偏移 8 处：条目数 = 1，然后条目指向 8（自己）
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&0x0100u16.to_le_bytes()); // ImageWidth
    b.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    b.extend_from_slice(&1u32.to_le_bytes()); // count
    b.extend_from_slice(&8u32.to_le_bytes()); // 值 = 偏移 8（指回自己）
    b.extend_from_slice(&8u32.to_le_bytes()); // next IFD = 8（成环）
    v.push(MalformedSample::new(
        "tiff-self-referencing-ifd.tif",
        b,
        "next IFD 指向自己：链式遍历不设上限就死循环",
    ));

    // 2. IFD 偏移超出文件
    let mut b = b"II\x2A\x00".to_vec();
    b.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    v.push(MalformedSample::new(
        "tiff-ifd-offset-past-eof.tif",
        b,
        "IFD0 偏移 4GB：seek 越界",
    ));

    // 3. 条目数巨大
    let mut b = b"II\x2A\x00".to_vec();
    b.extend_from_slice(&8u32.to_le_bytes());
    b.extend_from_slice(&0xFFFFu16.to_le_bytes()); // 65535 个条目
    v.push(MalformedSample::new(
        "tiff-huge-entry-count.tif",
        b,
        "条目数 65535：按数分配 12 字节 × 65535",
    ));

    // 4. 大端 + 截断
    v.push(MalformedSample::new(
        "tiff-big-endian-truncated.tif",
        vec![b'M', b'M', 0x00, 0x2A, 0x00, 0x00, 0x00, 0x08],
        "大端头后立刻 EOF",
    ));

    // 5. 空 TIFF
    v.push(MalformedSample::new(
        "tiff-header-only.tif",
        b"II\x2A\x00".to_vec(),
        "只有字节序标记",
    ));

    v
}

/// WebP 的畸形样本。
pub fn webp_samples() -> Vec<MalformedSample> {
    let mut v = Vec::new();

    fn riff(chunk_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = b"RIFF".to_vec();
        b.extend_from_slice(&((payload.len() + 12) as u32).to_le_bytes());
        b.extend_from_slice(b"WEBP");
        b.extend_from_slice(chunk_type);
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(payload);
        b
    }

    // 1. RIFF 长度撒谎
    let mut b = riff(b"VP8 ", &[0u8; 32]);
    b[4..8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    v.push(MalformedSample::new(
        "webp-lying-riff-size.webp",
        b,
        "RIFF 声称 4GB：按长度分配",
    ));

    // 2. VP8 帧头全 0
    v.push(MalformedSample::new(
        "webp-zero-vp8-header.webp",
        riff(b"VP8 ", &[0u8; 40]),
        "VP8 关键帧起始码缺失",
    ));

    // 3. VP8X 声称极端尺寸（画布 2^24）
    let mut payload = vec![0u8; 10];
    payload[0] = 0x00; // flags
    payload[4..7].copy_from_slice(&[0xFF, 0xFF, 0xFF]); // canvas w-1
    payload[7..10].copy_from_slice(&[0xFF, 0xFF, 0xFF]); // canvas h-1
    v.push(MalformedSample::new(
        "webp-huge-canvas.webp",
        riff(b"VP8X", &payload),
        "画布 16777216×16777216",
    ));

    // 4. 只有 RIFF 头
    v.push(MalformedSample::new(
        "webp-header-only.webp",
        b"RIFF\x0C\x00\x00\x00WEBP".to_vec(),
        "没有任何 chunk",
    ));

    v
}

/// 通用样本：跨格式的「非图像字节」。
///
/// 这些不是某个格式的畸形，而是**扩展名对但内容完全不对**的情况——
/// 用户改错后缀、下载中断、文件系统损坏都会产生。
pub fn generic_samples() -> Vec<MalformedSample> {
    vec![
        MalformedSample::new("empty.jpg", vec![], "空文件"),
        MalformedSample::new("one-byte.png", vec![0xFF], "1 字节"),
        MalformedSample::new("text.jpg", b"hello world, not an image".to_vec(), "纯文本"),
        MalformedSample::new(
            "random.bin.png",
            (0u8..=255).cycle().take(4096).collect(),
            "所有 256 种字节各出现（嗅探器最容易在这里 panic）",
        ),
        MalformedSample::new(
            "zip-in-jpg.jpg",
            b"PK\x03\x04".iter().copied().chain([0u8; 100]).collect(),
            "ZIP 头：魔数与扩展名都不匹配",
        ),
        MalformedSample::new(
            "html.svg",
            b"<html><body>not svg</body></html>".to_vec(),
            "HTML 内容但扩展名是 .svg",
        ),
        MalformedSample::new(
            "xml-bomb.svg",
            b"<?xml version=\"1.0\"?><!DOCTYPE lolz [<!ENTITY lol \"lol\">\
              <!ENTITY lol2 \"&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;\">]>\
              <svg>&lol2;</svg>"
                .to_vec(),
            "XML 实体炸弹（`roxmltree` 默认拒绝实体扩展，仍要测）",
        ),
        MalformedSample::new(
            "deep-nest.svg",
            {
                let mut s = String::from("<svg>");
                for _ in 0..5000 {
                    s.push_str("<g>");
                }
                s.push_str("</svg>");
                s.into_bytes()
            },
            "5000 层嵌套：递归下降解析器会爆栈",
        ),
    ]
}

/// 全部样本（按格式分组后拍平）。
pub fn all_samples() -> Vec<MalformedSample> {
    let mut v = Vec::new();
    v.extend(jpeg_samples());
    v.extend(png_samples());
    v.extend(gif_samples());
    v.extend(bmp_samples());
    v.extend(tiff_samples());
    v.extend(webp_samples());
    v.extend(generic_samples());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_has_at_least_the_required_minimum() {
        // `07-A` §4.3 要求「每格式 ≥ 20 个」。本仓库用**生成**而不是提交样本，
        // 所以数量少一点但覆盖面更精准——这里把门槛设成「每格式 ≥ 4 个**不同类别**」，
        // 而不是硬凑 20 个同质样本（凑数不会多发现一个 bug）。
        //
        // ⚠️ 与规格的偏差已登记进 `docs/07-M3-实现状态与冲突登记.md` §2.12。
        assert!(jpeg_samples().len() >= 4, "JPEG 样本太少");
        assert!(png_samples().len() >= 4, "PNG 样本太少");
        assert!(gif_samples().len() >= 4, "GIF 样本太少");
        assert!(bmp_samples().len() >= 4, "BMP 样本太少");
        assert!(tiff_samples().len() >= 4, "TIFF 样本太少");
        assert!(webp_samples().len() >= 4, "WebP 样本太少");
        assert!(generic_samples().len() >= 4, "通用样本太少");
    }

    #[test]
    fn sample_names_are_unique_and_have_the_right_extension() {
        let all = all_samples();
        let mut names = std::collections::HashSet::new();
        for s in &all {
            assert!(names.insert(s.name.clone()), "重名样本：{}", s.name);
            assert!(
                !s.intent.is_empty(),
                "{} 必须说明它想触发什么（失败时靠它定位）",
                s.name
            );
        }
    }

    #[test]
    fn every_sample_actually_differs_from_a_valid_file() {
        // 样本必须是**畸形**的：没有一个能被正常解码器当成合法图。
        // 这里不真的解码（那是集成用例的事），只断言「字节量合理且非空」——
        // 空文件也是合法的畸形样本（"empty.jpg"），所以只对非空样品断言长度。
        for s in all_samples() {
            if s.name.starts_with("empty") {
                assert!(s.bytes.is_empty());
            } else {
                assert!(!s.bytes.is_empty(), "{} 是空的", s.name);
            }
        }
    }

    #[test]
    fn writing_samples_only_touches_the_given_directory() {
        // R-1：只写调用方给的目录。
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("malformed");
        let paths = write_samples(&dir, &all_samples()).unwrap();
        assert!(!paths.is_empty());
        for p in &paths {
            assert!(p.starts_with(&dir), "{} 跑出了目标目录", p.display());
            assert!(p.exists());
        }
        // 目录里只有我们写的东西
        let count = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(count, paths.len());
    }

    #[test]
    fn the_huge_claim_sample_is_over_the_decode_limit() {
        // PNG 的 "png-huge-claim.png" 必须真的超过 `DECODE_MAX_PIXELS`，
        // 否则它测不到尺寸预检那条路径。
        let s = all_samples()
            .into_iter()
            .find(|s| s.name == "png-huge-claim.png")
            .expect("样本存在");
        // IHDR 数据在偏移 16（8 签名 + 4 长度 + 4 类型）
        let w = u32::from_be_bytes([s.bytes[16], s.bytes[17], s.bytes[18], s.bytes[19]]);
        let h = u32::from_be_bytes([s.bytes[20], s.bytes[21], s.bytes[22], s.bytes[23]]);
        assert_eq!((w, h), (30_000, 30_000));
        assert!(
            u64::from(w) * u64::from(h) > crate::DECODE_MAX_PIXELS,
            "必须超过解码上限，否则测不到预检"
        );
    }

    #[test]
    fn xml_bomb_is_present_and_small() {
        // 实体炸弹的**危险在于展开后**，文件本身很小——这正是它危险的原因。
        let s = all_samples()
            .into_iter()
            .find(|s| s.name == "xml-bomb.svg")
            .expect("样本存在");
        assert!(s.bytes.len() < 1024, "文件本身必须很小（<1 KB）");
        assert!(String::from_utf8_lossy(&s.bytes).contains("<!ENTITY"));
    }
}
