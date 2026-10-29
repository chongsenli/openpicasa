//! 收录规则：格式白名单 + 隐藏/系统目录策略（F1-24 / F1-28）。
//!
//! 这些规则直接决定「栅格里出现什么」，也决定「不出现什么」——
//! 后者同样重要：**扫描时要尽量少碰文件**（每次 stat 都是一次 IO）。

use std::path::Path;

/// 支持解码的扩展名（**小写比较**，`.jpeg` 与 `.jpg` 等价）。
///
/// M1 不含 RAW（`rawler` 是 LGPL-2.1，M2 才引入；见 02 §11）。
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "gif", "bmp", "tif", "tiff", "webp",
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
    fn raw_and_video_are_not_supported_in_m1() {
        for ext in ["a.cr2", "a.nef", "a.mp4", "a.mov", "a.psd"] {
            assert!(!is_supported_image(&PathBuf::from(ext)), "{ext}");
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
