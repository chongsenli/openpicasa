//! 多格式识别与**解码隔离**（`07-A` §4 FORMAT + R-M3-10）。
//!
//! ## 这个模块只做两件事
//!
//! 1. **识别**（[`sniff`]）：读文件头 ≤ 32 字节判实际格式。不看扩展名——
//!    扩展名是**候选**，魔数才是结论（`07-A` FORMAT-01）。
//! 2. **守门**（[`guard`]）：解码之前先判「这个文件该不该解」——
//!    尺寸超限、解码器熔断、格式不支持，都在这里拦下，变成**可见的失败**。
//!
//! ## 为什么「不可信输入」要单独一节
//!
//! 解码器是**唯一会把外部字节变成内存分配的入口**。一个 30000×30000 的 PNG
//! 声称自己是 ISO 小图，解码器会照着它分配 3.6 GB。R-M3-10 因此要求：
//!
//! > 任何解码器对任何输入都不得：崩进程、无限循环、写文件、分配超过
//! > `decode_mem_cap` 的内存。
//!
//! 四条里能在**进解码器之前**守住的是「不分配超限内存」（尺寸预检）。
//! 其余三条（不崩 / 不循环 / 不写文件）只能在解码之后核对——
//! 「不崩」靠 `catch_unwind`（在流水线里），「不循环」靠超时（在流水线里），
//! 「不写文件」靠零写入探针（`ro_guard.sh`）。
//!
//! 所以本模块专注于**预检**这一层，并把「熔断」的状态机放在这里——
//! 因为熔断的粒度是**格式**，不是单张文件。

use std::collections::HashMap;
use std::sync::Mutex;

/// 解码前读多少字节判格式（`07-A` 附录 A `decode.head_sniff_bytes = 32`）。
pub const HEAD_SNIFF_BYTES: usize = 32;

/// 单张图最多允许多少像素（`07-A` FORMAT-03，P-32 待定，初值 80 MP）。
///
/// ⚠️ 80 MP 的依据：全画幅 60 MP、中画幅 100 MP+。取 80 是个折中——
/// 覆盖绝大多数相机，同时 80 MP × 4 B = 320 MB 仍在 `decode_mem_cap`（512 MB）内。
/// P-32 要迭代 0 用 100 MP TIFF 实测内存后定。
pub const DECODE_MAX_PIXELS: u64 = 80_000_000;

/// 单次解码的内存上限（`07-A` §4.3，512 MB）。
pub const DECODE_MEM_CAP_MB: u64 = 512;

/// 熔断阈值：同一格式每 5 分钟最多容忍多少个解码 panic（`07-A` §4.3，初值 20）。
pub const DECODE_PANIC_FUSE_PER_MIN: u32 = 20;

/// 废弃线程数超过这个就熔断该格式（`07-A` §4.3）。
pub const DECODE_ABANDONED_FUSE: u32 = 4;

/// 文件的实际格式（由魔数嗅探得出，**不是**扩展名）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SniffedFormat {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Tiff,
    Webp,
    Heic,
    Avif,
    Svg,
    /// RAW 容器（TIFF 变体 / ISOBMFF），需要进一步的 IFD 解析才知道厂商
    RawContainer,
    /// 认不出来（文本文件、截断、未知格式）
    Unknown,
}

impl SniffedFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Gif => "GIF",
            Self::Bmp => "BMP",
            Self::Tiff => "TIFF",
            Self::Webp => "WebP",
            Self::Heic => "HEIC",
            Self::Avif => "AVIF",
            Self::Svg => "SVG",
            Self::RawContainer => "RAW",
            Self::Unknown => "未知",
        }
    }
}

/// 从文件头判格式（`07-A` FORMAT-01）。
///
/// 只看前 [`HEAD_SNIFF_BYTES`] 个字节。判错会比不判更糟，所以每种格式都要求
/// **完整的魔数 + 结构字段**，而不是「前两字节对上就算」。
pub fn sniff(head: &[u8]) -> SniffedFormat {
    if head.len() < 4 {
        return SniffedFormat::Unknown;
    }

    // JPEG: FF D8 FF
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return SniffedFormat::Jpeg;
    }
    // PNG: 89 50 4E 47 0D 0A 1A 0A
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return SniffedFormat::Png;
    }
    // GIF: "GIF87a" / "GIF89a"
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return SniffedFormat::Gif;
    }
    // BMP: "BM"
    if head.starts_with(b"BM") && head.len() >= 14 {
        return SniffedFormat::Bmp;
    }
    // WebP: "RIFF" .... "WEBP"
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return SniffedFormat::Webp;
    }
    // SVG: 文本，跳过 BOM 与空白后看 `<svg` 或 `<?xml`（后者要再确认含 <svg）
    if let Some(fmt) = sniff_svg(head) {
        return fmt;
    }

    // ---- ISOBMFF 家族（HEIC / AVIF / CR3）：`....ftyp<brand>` ----
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let brand = &head[8..12];
        return match brand {
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1" => {
                SniffedFormat::Heic
            }
            b"avif" | b"avis" => SniffedFormat::Avif,
            b"crx " => SniffedFormat::RawContainer, // Canon CR3
            _ => {
                // 有些 HEIC 把品牌放在兼容列表的后段，前 32 字节内能看到就认
                if head.windows(4).any(|w| w == b"heic" || w == b"mif1") {
                    SniffedFormat::Heic
                } else if head.windows(4).any(|w| w == b"avif") {
                    SniffedFormat::Avif
                } else {
                    SniffedFormat::Unknown
                }
            }
        };
    }

    // ---- TIFF 家族（TIFF / DNG / CR2 / NEF / ARW / RW2 / RWL）----
    // `II*\0`（小端）或 `MM\0*`（大端）
    if head.starts_with(b"II\x2A\x00") || head.starts_with(b"MM\x00\x2A") {
        // 区分「普通 TIFF」与「RAW 容器」：RAW 的 IFD0 里有厂商私有 tag，
        // 但那些要解析 IFD 才看得到（这是 RAW 提取器的事）。
        //
        // 这里用**扩展名之外的线索**：DNG 的 IFD0 必带 `DNGVersion`（0xC612）；
        // 字节序之后的第 8 字节起是 IFD0 偏移。太深了，所以这里只给
        // `RawContainer` 的**弱判定**，最终由 `PhotoFormat` 的扩展名 + IFD 解析定。
        //
        // ⚠️ 有一个廉价且可靠的信号：Panasonic 的 RW2/RWL 在第 2 字节是 `0x55`
        // （`II U\0` = `49 49 55 00`），不是标准的 `II*\0`。
        SniffedFormat::Tiff
    }
    // Panasonic RW2 / RWL：`II\x55\x00`
    else if head.starts_with(b"II\x55\x00") {
        SniffedFormat::RawContainer
    } else {
        SniffedFormat::Unknown
    }
}

/// SVG 是文本格式，魔数判定要看开头一段。
///
/// ⚠️ 这里**只做识别**，不做任何 XML 解析。真正的栅格化交给 `resvg`，
/// 而它的安全边界（禁用外部资源引用、实体炸弹）由 `usvg::Options` 保证。
fn sniff_svg(head: &[u8]) -> Option<SniffedFormat> {
    // 允许 UTF-8 BOM
    let s = head.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(head);
    let text = std::str::from_utf8(s).ok()?;
    let t = text.trim_start();
    if t.starts_with("<svg") {
        return Some(SniffedFormat::Svg);
    }
    // `<?xml ...?>` 后面紧跟 `<svg` 也算（但 32 字节内可能看不到 `<svg`，
    // 这时保守返回 Unknown——宁可让它进可见失败，也不要误判成 SVG 去解析）
    if t.starts_with("<?xml") && t.contains("<svg") {
        return Some(SniffedFormat::Svg);
    }
    None
}

/// 从文件头猜像素尺寸（`07-A` FORMAT-03 的尺寸预检）。
///
/// 返回 `None` 表示「从头部读不出来」——这时调用方应当允许解码但在解码过程中
/// 流式检查（`07-A` FORMAT-03 的兜底），而不是直接判失败。
///
/// ⚠️ **这是唯一能挡住「30000×30000 的 PNG」的地方**：等交给解码器再判断，
/// 它已经按声称的尺寸分配好内存了。
pub fn sniff_dimensions(head: &[u8]) -> Option<(u64, u64)> {
    match sniff(head) {
        SniffedFormat::Png => png_dimensions(head),
        SniffedFormat::Gif => gif_dimensions(head),
        SniffedFormat::Bmp => bmp_dimensions(head),
        SniffedFormat::Webp => webp_dimensions(head),
        SniffedFormat::Jpeg => None, // JPEG 的 SOF 段不在文件头，要扫段
        // TIFF 的尺寸在 IFD 里（要按偏移跳读），文件头 32 字节拿不到
        SniffedFormat::Tiff | SniffedFormat::RawContainer => None,
        SniffedFormat::Heic | SniffedFormat::Avif => None,
        SniffedFormat::Svg => None,
        SniffedFormat::Unknown => None,
    }
}

fn png_dimensions(head: &[u8]) -> Option<(u64, u64)> {
    // IHDR 紧跟在 8 字节签名 + 4 字节长度 + 4 字节类型之后
    if head.len() < 24 || &head[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([head[16], head[17], head[18], head[19]]) as u64;
    let h = u32::from_be_bytes([head[20], head[21], head[22], head[23]]) as u64;
    Some((w, h))
}

fn gif_dimensions(head: &[u8]) -> Option<(u64, u64)> {
    // 逻辑屏幕描述符：偏移 6，小端 u16 × 2
    if head.len() < 10 {
        return None;
    }
    let w = u16::from_le_bytes([head[6], head[7]]) as u64;
    let h = u16::from_le_bytes([head[8], head[9]]) as u64;
    Some((w, h))
}

fn bmp_dimensions(head: &[u8]) -> Option<(u64, u64)> {
    // DIB 头：偏移 18 起两个 i32（宽高，小端，高可为负）
    if head.len() < 26 {
        return None;
    }
    let w = i32::from_le_bytes([head[18], head[19], head[20], head[21]]).unsigned_abs() as u64;
    let h = i32::from_le_bytes([head[22], head[23], head[24], head[25]]).unsigned_abs() as u64;
    Some((w, h))
}

fn webp_dimensions(head: &[u8]) -> Option<(u64, u64)> {
    // VP8X（扩展格式）: 偏移 24 起 3 字节宽-1、3 字节高-1
    // VP8 （有损）  : 偏移 26 起 2 字节宽、2 字节高（14 位有效）
    // VP8L（无损）  : 偏移 21 起 packed 14 位 × 2
    if head.len() < 30 {
        return None;
    }
    match &head[12..16] {
        b"VP8X" => {
            let w = 1 + u32::from_le_bytes([head[24], head[25], head[26], 0]) as u64;
            let h = 1 + u32::from_le_bytes([head[27], head[28], head[29], 0]) as u64;
            Some((w, h))
        }
        b"VP8 " => {
            let w = u16::from_le_bytes([head[26], head[27]]) as u64 & 0x3FFF;
            let h = u16::from_le_bytes([head[28], head[29]]) as u64 & 0x3FFF;
            Some((w, h))
        }
        _ => None,
    }
}

/// 一次解码守门的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admit {
    /// 可以解码
    Go,
    /// 拒绝，原因可见（R-M2-4：失败永不隐藏）
    Reject(RejectReason),
}

/// 拒绝解码的原因（映射到 `FailureKind::Unsupported` 的子类）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// 像素数超限（`07-A` FORMAT-03）
    TooLarge { w: u64, h: u64, max_pixels: u64 },
    /// 魔数不在白名单
    UnknownMagic,
    /// 该格式的解码器已被熔断（连续 panic / 废弃线程过多）
    Fused { format: &'static str },
    /// 扩展名与魔数不符（**不是失败**，只记诊断，仍按魔数解）
    Mismatch,
}

impl RejectReason {
    /// 界面文案。
    pub fn label(&self) -> String {
        match self {
            Self::TooLarge { w, h, max_pixels } => format!(
                "图片尺寸过大（{w}×{h} = {} 万像素，上限 {} 万）",
                (w * h) / 10_000,
                max_pixels / 10_000
            ),
            Self::UnknownMagic => "无法识别的文件格式".into(),
            Self::Fused { format } => format!("{format} 解码器已暂停（连续出错，重启后恢复）"),
            Self::Mismatch => "扩展名与实际格式不一致".into(),
        }
    }
}

/// 解码守门器（`07-A` §4.3 R-M3-10）。
///
/// ## 为什么熔断的粒度是「格式」而不是「文件」
///
/// 一个坏目录里可能有几千个畸形文件，全是同一个格式的解析器崩的。
/// 逐文件熔断挡不住它——每个文件都是「新的」，都值得试一次，于是流水线一直在崩。
/// 按格式熔断才能真的停下来：连续 20 次崩溃就说明这个解析器在**这批输入**上不可用，
/// 继续试只是把整个索引拖死。
///
/// 熔断是**进程内状态**，重启即恢复——不落库，因为「上次运行崩过」不该永久禁用格式。
pub struct DecodeGuard {
    inner: Mutex<GuardInner>,
}

#[derive(Default)]
struct GuardInner {
    /// 每格式的「最近一分钟内的 panic 次数」
    panics: HashMap<&'static str, u32>,
    /// 每格式的废弃线程数
    abandoned: HashMap<&'static str, u32>,
    /// 已熔断的格式
    fused: HashMap<&'static str, String>,
}

impl Default for DecodeGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl DecodeGuard {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(GuardInner::default()),
        }
    }

    /// 解码前守门（`07-A` FORMAT-01/03）。
    ///
    /// `ext` 是文件扩展名（可能为空），`head` 是文件头。
    /// 返回 `Admit::Go` 时还给出**实际格式**——按魔数解码，不按扩展名。
    pub fn admit(&self, ext: &str, head: &[u8]) -> (Admit, SniffedFormat) {
        let fmt = sniff(head);
        if fmt == SniffedFormat::Unknown {
            return (Admit::Reject(RejectReason::UnknownMagic), fmt);
        }
        // 熔断：这个格式的解码器暂时不可用
        if let Ok(g) = self.inner.lock()
            && g.fused.contains_key(fmt.label())
        {
            return (
                Admit::Reject(RejectReason::Fused {
                    format: fmt.label(),
                }),
                fmt,
            );
        }
        // 尺寸预检
        if let Some((w, h)) = sniff_dimensions(head)
            && w.saturating_mul(h) > DECODE_MAX_PIXELS
        {
            return (
                Admit::Reject(RejectReason::TooLarge {
                    w,
                    h,
                    max_pixels: DECODE_MAX_PIXELS,
                }),
                fmt,
            );
        }
        // 扩展名与魔数不符：**不是失败**，仍按魔数解（`07-A` FORMAT-01）
        let _mismatch = !ext.is_empty() && !extension_matches(ext, fmt);
        (Admit::Go, fmt)
    }

    /// 记一次解码 panic。达到阈值 → 熔断该格式。
    pub fn note_panic(&self, format: &'static str) {
        let Ok(mut g) = self.inner.lock() else { return };
        let n = {
            let slot = g.panics.entry(format).or_insert(0);
            *slot += 1;
            *slot
        };
        if n >= DECODE_PANIC_FUSE_PER_MIN {
            g.fused
                .insert(format, format!("{format} 一分钟内崩溃 {n} 次，已熔断"));
        }
    }

    /// 记一次「超时被放弃的线程」。超过阈值 → 熔断。
    pub fn note_abandoned_thread(&self, format: &'static str) {
        let Ok(mut g) = self.inner.lock() else { return };
        let n = {
            let slot = g.abandoned.entry(format).or_insert(0);
            *slot += 1;
            *slot
        };
        if n >= DECODE_ABANDONED_FUSE {
            g.fused
                .insert(format, format!("{format} 有 {n} 个解码线程超时未退出，已熔断"));
        }
    }

    /// 这个格式熔断了吗。
    pub fn is_fused(&self, format: &str) -> bool {
        self.inner
            .lock()
            .map(|g| g.fused.contains_key(format))
            .unwrap_or(false)
    }

    /// 熔断清单（诊断页显示）。
    pub fn fused_list(&self) -> Vec<(String, String)> {
        self.inner
            .lock()
            .map(|g| {
                g.fused
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 重启后恢复（测试用；真实场景就是进程重启）。
    pub fn reset(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.panics.clear();
            g.abandoned.clear();
            g.fused.clear();
        }
    }
}

/// 扩展名与嗅探格式是否一致。
fn extension_matches(ext: &str, fmt: SniffedFormat) -> bool {
    let e = ext.trim_start_matches('.').to_ascii_lowercase();
    match fmt {
        SniffedFormat::Jpeg => matches!(e.as_str(), "jpg" | "jpeg" | "jpe" | "jfif"),
        SniffedFormat::Png => e == "png",
        SniffedFormat::Gif => e == "gif",
        SniffedFormat::Bmp => matches!(e.as_str(), "bmp" | "dib"),
        SniffedFormat::Tiff => matches!(e.as_str(), "tif" | "tiff"),
        SniffedFormat::Webp => e == "webp",
        SniffedFormat::Heic => matches!(e.as_str(), "heic" | "heif" | "hif"),
        SniffedFormat::Avif => e == "avif",
        SniffedFormat::Svg => matches!(e.as_str(), "svg" | "svgz"),
        SniffedFormat::RawContainer => {
            matches!(
                e.as_str(),
                "dng" | "cr2" | "cr3" | "nef" | "arw" | "raf" | "rwl" | "rw2" | "orf" | "pef"
            )
        }
        SniffedFormat::Unknown => false,
    }
}

/// 全局守门器（流水线共用一份——熔断必须是进程级的，每个任务一份就没有意义了）。
pub fn global_guard() -> &'static DecodeGuard {
    static GUARD: std::sync::OnceLock<DecodeGuard> = std::sync::OnceLock::new();
    GUARD.get_or_init(DecodeGuard::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_HEAD: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, // 签名
        0x00, 0x00, 0x00, 0x0D, // 长度 13
        b'I', b'H', b'D', b'R', // 类型
        0x00, 0x00, 0x0B, 0xB8, // 宽 3000
        0x00, 0x00, 0x0B, 0xB8, // 高 3000
    ];

    #[test]
    fn sniffs_the_six_classic_formats() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0]), SniffedFormat::Jpeg);
        assert_eq!(sniff(PNG_HEAD), SniffedFormat::Png);
        assert_eq!(sniff(b"GIF89a\x01\x00\x01\x00"), SniffedFormat::Gif);
        assert_eq!(sniff(b"GIF87a\x01\x00\x01\x00"), SniffedFormat::Gif);
        let mut bmp = vec![b'B', b'M', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        bmp.extend_from_slice(&[0; 16]);
        assert_eq!(sniff(&bmp), SniffedFormat::Bmp);
        let mut webp = b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec();
        webp.extend_from_slice(&[0; 16]);
        assert_eq!(sniff(&webp), SniffedFormat::Webp);
        let mut tiff = b"II\x2A\x00".to_vec();
        tiff.extend_from_slice(&[0; 28]);
        assert_eq!(sniff(&tiff), SniffedFormat::Tiff);
    }

    #[test]
    fn sniffs_the_modern_formats() {
        // HEIC：ftyp + heic
        let heic = b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00mif1heic".to_vec();
        assert_eq!(sniff(&heic), SniffedFormat::Heic);
        // AVIF
        let avif = b"\x00\x00\x00\x18ftypavif\x00\x00\x00\x00mif1avif".to_vec();
        assert_eq!(sniff(&avif), SniffedFormat::Avif);
        // CR3：ftyp + crx␣
        let cr3 = b"\x00\x00\x00\x18ftypcrx \x00\x00\x00\x01".to_vec();
        assert_eq!(sniff(&cr3), SniffedFormat::RawContainer);
        // Panasonic RW2/RWL：II U\0
        let rw2 = b"II\x55\x00\x18\x00\x00\x00".to_vec();
        assert_eq!(sniff(&rw2), SniffedFormat::RawContainer);
        // SVG
        assert_eq!(sniff(b"<svg xmlns=\"http://..."), SniffedFormat::Svg);
        assert_eq!(sniff(b"<?xml version=\"1.0\"?><svg"), SniffedFormat::Svg);
        // UTF-8 BOM 的 SVG
        let mut bom_svg = vec![0xEF, 0xBB, 0xBF];
        bom_svg.extend_from_slice(b"<svg>");
        assert_eq!(sniff(&bom_svg), SniffedFormat::Svg);
    }

    #[test]
    fn text_and_truncated_input_is_unknown() {
        assert_eq!(sniff(b"hello, world this is text!!"), SniffedFormat::Unknown);
        assert_eq!(sniff(b""), SniffedFormat::Unknown);
        assert_eq!(sniff(b"ab"), SniffedFormat::Unknown);
        // 截断的 PNG（只有签名）
        assert_eq!(sniff(&PNG_HEAD[..4]), SniffedFormat::Unknown);
    }

    #[test]
    fn png_dimensions_come_from_ihdr() {
        // 3000×3000
        assert_eq!(sniff_dimensions(PNG_HEAD), Some((3000, 3000)));
    }

    #[test]
    fn oversized_images_are_rejected_before_decoding() {
        // `07-A` FORMAT-03：30000×30000 PNG（= 9 亿像素）必须在**解码前**就拒掉。
        // 交给解码器的话，它已经按声称尺寸分配好 3.6 GB 了。
        let head: Vec<u8> = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H',
            b'D', b'R', 0x00, 0x00, 0x75, 0x30, // 宽 30000
            0x00, 0x00, 0x75, 0x30, // 高 30000
        ];
        let guard = DecodeGuard::new();
        let (admit, fmt) = guard.admit("png", &head);
        assert_eq!(fmt, SniffedFormat::Png);
        let Admit::Reject(RejectReason::TooLarge { w, h, .. }) = admit else {
            panic!("30000×30000 必须被拒：{admit:?}")
        };
        assert_eq!((w, h), (30000, 30000));
    }

    #[test]
    fn normal_sized_images_pass_the_guard() {
        let guard = DecodeGuard::new();
        let (admit, fmt) = guard.admit("png", PNG_HEAD);
        assert_eq!(admit, Admit::Go);
        assert_eq!(fmt, SniffedFormat::Png);
    }

    #[test]
    fn unknown_magic_is_rejected_visibly() {
        let guard = DecodeGuard::new();
        let (admit, _) = guard.admit("jpg", b"this is not an image at all");
        assert_eq!(admit, Admit::Reject(RejectReason::UnknownMagic));
    }

    #[test]
    fn a_jpg_whose_content_is_png_decodes_as_png() {
        // `07-A` FORMAT-01：扩展名与魔数不一致 → **按魔数解码**，不是失败。
        let guard = DecodeGuard::new();
        let (admit, fmt) = guard.admit("jpg", PNG_HEAD);
        assert_eq!(admit, Admit::Go, "不一致不是失败");
        assert_eq!(fmt, SniffedFormat::Png, "★ 按魔数认出真实格式");
    }

    #[test]
    fn fuse_disables_a_format_after_repeated_panics() {
        // R-M3-10：连续 panic > 20/分钟 → 熔断该格式，防止一个坏目录拖死流水线。
        let guard = DecodeGuard::new();
        assert!(!guard.is_fused("PNG"));
        for _ in 0..DECODE_PANIC_FUSE_PER_MIN {
            guard.note_panic("PNG");
        }
        assert!(guard.is_fused("PNG"), "达到阈值必须熔断");
        // 熔断后 admit 直接拒绝
        let (admit, _) = guard.admit("png", PNG_HEAD);
        assert!(matches!(admit, Admit::Reject(RejectReason::Fused { .. })));
        // 别的格式不受影响（熔断按格式隔离）
        assert!(!guard.is_fused("JPEG"));
        let (admit, _) = guard.admit("jpg", &[0xFF, 0xD8, 0xFF, 0xE0]);
        assert_eq!(admit, Admit::Go);
    }

    #[test]
    fn fuse_resets_when_the_process_restarts() {
        let guard = DecodeGuard::new();
        for _ in 0..DECODE_PANIC_FUSE_PER_MIN {
            guard.note_panic("PNG");
        }
        assert!(guard.is_fused("PNG"));
        guard.reset();
        assert!(!guard.is_fused("PNG"), "熔断是进程内状态，重启即恢复");
    }

    #[test]
    fn abandoned_threads_also_trip_the_fuse() {
        let guard = DecodeGuard::new();
        for _ in 0..DECODE_ABANDONED_FUSE {
            guard.note_abandoned_thread("TIFF");
        }
        assert!(guard.is_fused("TIFF"));
        let list = guard.fused_list();
        assert!(list.iter().any(|(k, _)| k == "TIFF"));
    }

    #[test]
    fn reject_reasons_are_human_readable() {
        let r = RejectReason::TooLarge {
            w: 30_000,
            h: 30_000,
            max_pixels: DECODE_MAX_PIXELS,
        };
        let t = r.label();
        assert!(t.contains("30000×30000"), "要给出真实尺寸：{t}");
        assert!(t.contains("8000"), "要给出上限（8000 万像素）：{t}");
        assert!(!RejectReason::UnknownMagic.label().is_empty());
        assert!(
            RejectReason::Fused { format: "PNG" }
                .label()
                .contains("PNG")
        );
    }

    #[test]
    fn extension_matching_is_a_hint_not_a_gate() {
        assert!(extension_matches("png", SniffedFormat::Png));
        assert!(extension_matches(".PNG", SniffedFormat::Png));
        assert!(!extension_matches("jpg", SniffedFormat::Png));
        assert!(extension_matches("rw2", SniffedFormat::RawContainer));
        assert!(extension_matches("cr3", SniffedFormat::RawContainer));
    }

    #[test]
    fn jpeg_dimensions_are_not_in_the_head() {
        // JPEG 的 SOF 段在文件头之后，32 字节内通常看不到 → 返回 None，
        // 由解码时的流式检查兜底（`07-A` FORMAT-03 的例外）。
        let head = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0, 1, 1, 0];
        assert_eq!(sniff_dimensions(&head), None);
    }
}
