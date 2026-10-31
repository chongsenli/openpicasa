//! 轻量尺寸探测：**只读文件头**，不建 `ExifSummary`，不做全解码（M2.1 B 工作包）。
//!
//! ## 为什么不复用 [`crate::probe`]
//!
//! `probe()` 是给**扫描**用的：它要 EXIF 的相机型号、镜头、光圈、GPS 等一整套字符串，
//! 因为那批字段要落进查看器的信息面板。回填宽高只要三个数字（宽、高、方向），
//! 为了三个数字去 `format!` 十来个字符串，在 5 万张的量级上是纯粹的浪费。
//!
//! 两者的**读取顺序**是一致的（EXIF → 文件头 → 全解码），差别只在最后一步：
//! 回填**不做全解码**。理由写在下面 [`DimsError::Unknown`] 旁边。
//!
//! ## 一次 open，两次读
//!
//! 先按 EXIF 读一遍（`kamadak-exif` 只扫 APP1 段），拿不到尺寸就把句柄 seek 回起点
//! 交给 `image` 猜格式读头。**同一个 `File` 句柄**——回填是后台批量操作，
//! 每张多一次 `open` 在 5 万张上是 5 万次多余的 syscall，还会和索引线程抢目录项缓存。

use exif::{In, Tag};
use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::Path;

/// 探测结果：原始像素尺寸 + EXIF 方向。
///
/// `width` / `height` 是**未摆正**的原始尺寸——方向折算由 `photos.display_*`
/// 生成列负责，这里再算一遍就必然有一天和生成列打架（`docs/05` §1.3.1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dims {
    pub width: u32,
    pub height: u32,
    /// EXIF Orientation 1..8；缺失记 1（`docs/05` §1.3.1 的空值归一规则）
    pub orientation: u32,
}

/// 为什么没探到。
///
/// **这两类的处理方式完全相反**，所以不能合并成一个 `Result<(u32,u32)>`：
/// 合并之后调用方只能把两种情况一起判死，而真实库里最常见的恰恰是第一类。
#[derive(Debug)]
pub enum DimsError {
    /// 文件打不开 / 读不了：盘没挂载、权限不足、文件被移走。
    ///
    /// ⚠️ **这不是终态**。实测样本里 1150 张有 61 张待回填，全部位于一块**当时没插的
    /// 外置盘**上（`/run/media/.../202606`）。若把这类判成「读取失败」写进
    /// `dims_status = 2`，用户把盘插回来的那一刻，这 61 张也永远不会再被处理。
    Unreachable(std::io::Error),
    /// 文件读得到，但拿不到宽高：格式不支持（HEIC / AVIF）或文件头损坏。
    ///
    /// **这是终态**，记 `dims_status = 2`。**故意不做全解码兜底**：`probe()` 在扫描时
    /// 会兜底（那是每张只做一次的一次性成本），而回填是后台批量操作，
    /// 为了一张可能根本解不出来的图去解码 24MP，会让「后台不打扰用户」这个前提失效。
    /// 真能解码的文件，其文件头也一定能给出尺寸。
    Unknown,
}

impl DimsError {
    /// 是否值得在「盘回来之后」重试。
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Unreachable(_))
    }
}

/// 日志里要能直接读懂是「盘不在」还是「格式不认」——这两种在用户眼里是
/// 完全不同的两件事，日志不该都写成 `Unknown`。
impl std::fmt::Display for DimsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "无法读取（盘未挂载/权限/已移走）：{e}"),
            Self::Unknown => write!(f, "拿不到宽高（格式不支持或文件头损坏）"),
        }
    }
}

/// 只读探测一张图的宽高与方向。
pub fn probe_dims(path: &Path) -> Result<Dims, DimsError> {
    let file = File::open(path).map_err(DimsError::Unreachable)?;
    let mut reader = BufReader::new(file);

    let exif = exif::Reader::new()
        .read_from_container(&mut reader)
        .ok();

    let mut orientation = 1u32;
    if let Some(exif) = exif.as_ref() {
        orientation = exif
            .get_field(Tag::Orientation, In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
            .filter(|o| (1..=8).contains(o))
            .unwrap_or(1);

        // EXIF 的 PixelXDimension / PixelYDimension：绝大多数相机与手机都会写，
        // 读到就不必再碰文件头。
        let exif_dims = exif.get_field(Tag::PixelXDimension, In::PRIMARY).and_then(|f| {
            let w = f.value.get_uint(0)?;
            let h = exif
                .get_field(Tag::PixelYDimension, In::PRIMARY)?
                .value
                .get_uint(0)?;
            (w > 0 && h > 0).then_some((w, h))
        });
        if let Some((width, height)) = exif_dims {
            return Ok(Dims {
                width,
                height,
                orientation,
            });
        }
    }

    // 回到文件起点，交给 image 猜格式读头（`with_guessed_format` 按魔数判定，
    // 不信扩展名——回填对象里就有扩展名和真实格式不符的）。
    reader
        .seek(SeekFrom::Start(0))
        .map_err(DimsError::Unreachable)?;
    let reader = image::ImageReader::new(reader)
        .with_guessed_format()
        .map_err(|_| DimsError::Unknown)?;
    match reader.into_dimensions() {
        Ok((w, h)) if w > 0 && h > 0 => Ok(Dims {
            width: w,
            height: h,
            orientation,
        }),
        _ => Err(DimsError::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_plain_jpeg(path: &Path, w: u32, h: u32) {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 255) as u8, (y % 255) as u8, 64])
        });
        let mut buf = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new(&mut buf);
        enc.encode(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        File::create(path).unwrap().write_all(&buf).unwrap();
    }

    /// 造一张带最小 EXIF 的 JPEG：IFD0 里 Orientation=6，Exif 子 IFD 里是像素宽高。
    ///
    /// **结构必须与真实文件一致**：`PixelXDimension` / `PixelYDimension` 属于
    /// Exif 子 IFD（由 IFD0 的 `0x8769` 指针指向），放进 IFD0 的话
    /// `kamadak-exif` 只会把它当成一个匿名的 Tiff 标签，`get_field(Tag::PixelXDimension)`
    /// 拿不到——这正是第一版夹具写错的地方（写错了会「测试通过但产品路径没被覆盖」）。
    ///
    /// 手搓字节而不是引入写 EXIF 的依赖（`kamadak-exif` 只读）。M1 的红线也要求
    /// 「不许写照片」——哪怕是测试用的假照片，能不加写路径就不加。
    fn exif_jpeg(w: u32, h: u32, orientation: u16) -> Vec<u8> {
        const IFD0_AT: u32 = 8;
        let ifd0_len = 2 + 2 * 12 + 4; // 2 个条目
        let sub_at = IFD0_AT + ifd0_len as u32;

        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&IFD0_AT.to_le_bytes());

        // ---- IFD0：Orientation + Exif 子 IFD 指针
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
        tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0x8769u16.to_le_bytes()); // ExifIFDPointer
        tiff.extend_from_slice(&4u16.to_le_bytes()); // LONG
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&sub_at.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes()); // 无下一个 IFD

        // ---- Exif 子 IFD：PixelXDimension + PixelYDimension
        tiff.extend_from_slice(&2u16.to_le_bytes());
        for (tag, val) in [(0xA002u16, w), (0xA003u16, h)] {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&4u16.to_le_bytes()); // LONG
            tiff.extend_from_slice(&1u32.to_le_bytes());
            tiff.extend_from_slice(&val.to_le_bytes());
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());

        let mut app1 = Vec::new();
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&tiff);

        let mut out = Vec::new();
        out.extend_from_slice(&[0xFF, 0xD8]); // SOI
        out.extend_from_slice(&[0xFF, 0xE1]); // APP1
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&[0xFF, 0xD9]); // EOI
        out
    }

    #[test]
    fn reads_dimensions_from_header_without_decoding() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("plain.jpg");
        write_plain_jpeg(&p, 320, 200);

        let d = probe_dims(&p).unwrap();
        assert_eq!((d.width, d.height), (320, 200));
        assert_eq!(d.orientation, 1, "没有 EXIF 时方向记 1，不是 0");
    }

    #[test]
    fn reads_exif_pixel_dims_and_orientation() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("rotated.jpg");
        // 故意只写 EXIF 没有真实图像数据：能读出来就证明「EXIF 优先、不必解码」
        std::fs::write(&p, exif_jpeg(4000, 3000, 6)).unwrap();

        let d = probe_dims(&p).unwrap();
        assert_eq!((d.width, d.height), (4000, 3000), "返回原始尺寸，不折算");
        assert_eq!(d.orientation, 6, "方向要读出来，折算交给生成列");
    }

    #[test]
    fn missing_file_is_retryable_not_permanent() {
        let tmp = tempfile::tempdir().unwrap();
        let err = probe_dims(&tmp.path().join("not-here.jpg")).unwrap_err();
        assert!(err.is_retryable(), "文件不在 ≠ 文件坏了：盘插回来要能重试");
        assert!(matches!(err, DimsError::Unreachable(_)));
    }

    #[test]
    fn garbage_file_is_permanent_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("broken.jpg");
        std::fs::write(&p, b"not an image at all").unwrap();
        let err = probe_dims(&p).unwrap_err();
        assert!(!err.is_retryable(), "文件读得到但认不出格式 → 终态");
        assert!(matches!(err, DimsError::Unknown));
    }
}
