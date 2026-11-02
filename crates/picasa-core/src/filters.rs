//! 收录规则：格式白名单 + 隐藏/系统目录策略（F1-24 / F1-28）。
//!
//! 这些规则直接决定「栅格里出现什么」，也决定「不出现什么」——
//! 后者同样重要：**扫描时要尽量少碰文件**（每次 stat 都是一次 IO）。

use std::path::Path;

/// 支持解码的扩展名（**小写比较**，`.jpeg` 与 `.jpg` 等价）。
///
/// ## M3 FORMAT：从「只认 JPEG + 少数」扩到 `07-A` §4.1 / `08` §1 的全集
///
/// 注意这是**候选**，不是结论：真正按什么解码由魔数嗅探决定
/// （`picasa_imaging::sniff`），一个叫 `.jpg` 的 PNG 会按 PNG 解（FORMAT-01）。
///
/// 分级（`08` §1 终版）：
///
/// | 级 | 扩展名 |
/// | --- | --- |
/// | 已有 | `jpg jpeg jpe jfif` |
/// | A | `png gif bmp dib tif tiff webp` |
/// | A+ | `svg svgz` |
/// | B | `heic heif hif avif`（需 `heif` feature，未编译时进可见失败） |
/// | C | `dng cr2 cr3 nef arw raf rwl rw2`（只提取内嵌预览） |
///
/// ⚠️ **扩展名白名单放宽不等于「都能解」**：B/C 级没编译进来时，
/// 文件照样入库（用户能看见「有这个文件」），缩略图进**可见失败**队列——
/// 静默丢弃才是缺陷（R-M2-4）。
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    // 已有 / A 级：真正能解码
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "dib", "tif", "tiff", "webp",
    // A+ 级：SVG（矢量，按档位栅格化）
    "svg", "svgz",
    // B 级：HEIC / AVIF
    "heic", "heif", "hif", "avif",
    // C 级：RAW（只取内嵌预览）
    "dng", "cr2", "cr3", "nef", "arw", "raf", "rwl", "rw2",
];

/// 系统/构建产物目录：即使不以 `.` 开头也要跳过。
const SKIP_DIR_NAMES: &[&str] = &[
    "System Volume Information",
    "$RECYCLE.BIN",
    ".Trash",
    ".Trashes",
    ".Spotlight-V100",
    ".fseventsd",
    ".DocumentRevisions-V100",
    ".TemporaryItems",
    ".git",
    "node_modules",
    "target",
];

/// 该文件是否在格式白名单内（按扩展名，大小写不敏感）。
pub fn is_supported_image(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let ext = ext.to_ascii_lowercase();
            SUPPORTED_EXTENSIONS.iter().any(|e| *e == ext)
        }
        None => false,
    }
}

pub fn is_supported_extension(ext: &str) -> bool {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    SUPPORTED_EXTENSIONS.iter().any(|e| *e == ext)
}

/// 该目录是否应当整棵跳过。
///
/// 规则：以 `.` 开头的一律跳过（含 `.git`、`.Trash`、`.hidden`），
/// 加上 [`SKIP_DIR_NAMES`] 里的系统目录名。**跳过意味着「不索引」，而不是「删除」**。
pub fn should_skip_dir(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    SKIP_DIR_NAMES.iter().any(|s| s.eq_ignore_ascii_case(name))
}

/// 该文件是否应当跳过（隐藏文件不收录——`.DS_Store` 之类的噪音）。
pub fn should_skip_file(name: &str) -> bool {
    name.starts_with('.')
}

/// 扫描深度上限。`None` = 无限。
///
/// 设一个上限的意义：用户误选 `/` 或整个家目录时不至于扫到天亮（P0 风险）。
pub const DEFAULT_MAX_DEPTH: usize = 12;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn jpeg_variants_are_supported() {
        for ext in ["a.jpg", "b.JPG", "c.jpeg", "d.JPeG"] {
            assert!(is_supported_image(&PathBuf::from(ext)), "{ext}");
        }
    }

    #[test]
    fn video_and_psd_stay_out_of_scope() {
        // `07-A` §10：视频要 ffmpeg，PSD/TGA/ICO/JXL 不做。
        for ext in ["a.mp4", "a.mov", "a.psd", "a.tga", "a.ico", "a.jxl"] {
            assert!(!is_supported_image(&PathBuf::from(ext)), "{ext}");
        }
    }

    #[test]
    fn m3_adds_every_format_in_the_decided_list() {
        // `08` §1 终版清单：一个都不能漏（漏了就是「用户看到文件但软件不认」）。
        for ext in [
            "a.jpg", "a.jpeg", "a.jpe", "a.jfif", // 已有
            "a.png", "a.gif", "a.bmp", "a.dib", "a.tif", "a.tiff", "a.webp", // A
            "a.svg", "a.svgz", // A+
            "a.heic", "a.heif", "a.hif", "a.avif", // B
            "a.dng", "a.cr2", "a.cr3", "a.nef", "a.arw", "a.raf", "a.rwl", "a.rw2", // C
        ] {
            assert!(is_supported_image(&PathBuf::from(ext)), "{ext} 应当在清单里");
        }
    }

    #[test]
    fn no_extension_is_not_supported() {
        assert!(!is_supported_image(Path::new("README")));
    }

    #[test]
    fn hidden_and_system_dirs_are_skipped() {
        for name in [
            ".git",
            ".hidden",
            ".Trash",
            "System Volume Information",
            "$RECYCLE.BIN",
            "node_modules",
        ] {
            assert!(should_skip_dir(name), "{name}");
        }
        assert!(!should_skip_dir("Photos"));
        assert!(!should_skip_dir("2011-05-13"));
    }

    #[test]
    fn ds_store_is_skipped() {
        assert!(should_skip_file(".DS_Store"));
        assert!(!should_skip_file("IMG_0001.JPG"));
    }

    #[test]
    fn chinese_folder_names_are_kept() {
        assert!(!should_skip_dir("苏州行"));
        assert!(!should_skip_dir("家庭照片 2011"));
    }
}
