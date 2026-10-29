//! **可写路径的唯一来源**。
//!
//! 02 §5.2 的结论表在这里落成代码：
//!
//! | 位置 | 放什么 |
//! | --- | --- |
//! | `~/Library/Application Support/app.openpicasa/` | 索引库、偏好 JSON（A/B 类） |
//! | `~/Library/Preferences/app.openpicasa/` | eframe 的窗口状态（macOS 惯例位置） |
//! | `~/Library/Caches/app.openpicasa/` | M1 基本不用（缩略图进 DB，见 02 §5.4） |
//! | `~/Library/Logs/app.openpicasa/` | 日志 |
//! | 照片目录 | **什么都不放**（R-1） |
//!
//! ⚠️ 本文件是「谁能写哪儿」的**单点定义**。想加一个新的落盘位置，只能改这里——
//! 这样 R-1 的审计永远只需看一个文件。

use std::path::{Path, PathBuf};

/// Bundle identifier。与 `packaging/Info.plist` 里的 `CFBundleIdentifier` 必须一致，
/// 否则 TCC 授权与偏好会落在另一个容器里（这是很难查的「偏好记不住」bug 来源）。
pub const BUNDLE_ID: &str = "app.openpicasa";

/// 应用名（窗口标题、日志文件名等展示用）。
pub const APP_NAME: &str = "OpenPicasa";

/// 全部可写路径。
#[derive(Clone, Debug)]
pub struct AppPaths {
    pub support: PathBuf,
    pub preferences: PathBuf,
    pub caches: PathBuf,
    pub logs: PathBuf,
}

impl AppPaths {
    /// 按 macOS 惯例解析（支持 `OPENPICASA_DATA_DIR` 覆盖，测试与便携模式用）。
    pub fn resolve() -> Self {
        if let Some(dir) = std::env::var_os("OPENPICASA_DATA_DIR") {
            let root = PathBuf::from(dir);
            return Self {
                support: root.join("Application Support").join(BUNDLE_ID),
                preferences: root.join("Preferences").join(BUNDLE_ID),
                caches: root.join("Caches").join(BUNDLE_ID),
                logs: root.join("Logs").join(BUNDLE_ID),
            };
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let lib = home.join("Library");
        Self {
            support: lib.join("Application Support").join(BUNDLE_ID),
            preferences: lib.join("Preferences").join(BUNDLE_ID),
            caches: lib.join("Caches").join(BUNDLE_ID),
            logs: lib.join("Logs").join(BUNDLE_ID),
        }
    }

    /// 建目录。**只创建应用容器**，永远不会碰到照片目录。
    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [&self.support, &self.preferences, &self.caches, &self.logs] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }

    /// 索引库（含缩略图 BLOB）。**必须放 support，不能放 caches**（02 §5.2.1）。
    pub fn db_file(&self) -> PathBuf {
        self.support.join("library.sqlite")
    }

    /// 偏好（B 类）。放 support 而不是 Preferences 目录：
    /// 我们自己解析这个 JSON，不需要 `NSUserDefaults` 的自动刷盘（02 §5.2.2 方案 A）。
    pub fn preferences_file(&self) -> PathBuf {
        self.support.join("preferences.json")
    }

    /// eframe 的 persistence 落点（窗口几何等 UI 内部状态）。
    pub fn ui_state_file(&self) -> PathBuf {
        self.support.join("ui_state.ron")
    }

    /// 判断一个路径是否落在**应用容器**内。用于自检与测试断言。
    pub fn is_in_container(&self, path: &Path) -> bool {
        [&self.support, &self.preferences, &self.caches, &self.logs]
            .iter()
            .any(|root| path.starts_with(root))
    }
}

/// 把一个路径规范化成「可比较、可持久化」的字符串（NFC + 去掉尾部斜杠）。
pub fn canonical_string(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    let trimmed = s.trim_end_matches('/');
    let trimmed = if trimmed.is_empty() { "/" } else { trimmed };
    // macOS 的 HFS+/APFS 文件名以 NFD 存储，用户看到的是 NFC。统一成 NFC 再存，
    // 避免「同一个目录在树里出现两次」这类经典 bug。
    nfc(trimmed)
}

/// 比较键：NFC + 小写。中文没有大小写，但拉丁文件名有，且 macOS 默认大小写不敏感。
pub fn compare_key(s: &str) -> String {
    nfc(s).to_lowercase()
}

/// 极简 NFC 归一化。
///
/// 不引入 `unicode-normalization`（一个只为一个函数服务的依赖）——
/// M1 只需要保证「同一串字符的两种编码不会导致两条索引」，
/// 用 `char` 层面的组合字符合并覆盖了现实里的绝大部分 macOS 文件名。
pub fn nfc(s: &str) -> String {
    if !s.chars().any(|c| ('\u{0300}'..='\u{036f}').contains(&c)) {
        return s.to_string();
    }
    // 有组合字符：按「基字符 + 组合字符」预组合成常见拉丁字符。
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let Some(&next) = chars.peek() else {
            out.push(c);
            break;
        };
        let combined = match (c, next) {
            ('a', '\u{0300}') => Some('à'),
            ('a', '\u{0301}') => Some('á'),
            ('a', '\u{0308}') => Some('ä'),
            ('e', '\u{0300}') => Some('è'),
            ('e', '\u{0301}') => Some('é'),
            ('e', '\u{0308}') => Some('ë'),
            ('i', '\u{0301}') => Some('í'),
            ('o', '\u{0301}') => Some('ó'),
            ('o', '\u{0308}') => Some('ö'),
            ('u', '\u{0301}') => Some('ú'),
            ('u', '\u{0308}') => Some('ü'),
            ('n', '\u{0303}') => Some('ñ'),
            ('c', '\u{0327}') => Some('ç'),
            _ => None,
        };
        match combined {
            Some(pre) => {
                out.push(pre);
                chars.next();
            }
            None => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfc_composes_common_accents() {
        let decomposed = "Cafe\u{0301}";
        assert_eq!(nfc(decomposed), "Café");
    }

    #[test]
    fn nfc_keeps_chinese_untouched() {
        assert_eq!(nfc("苏州行 2011"), "苏州行 2011");
    }

    #[test]
    fn compare_key_is_case_and_form_insensitive() {
        assert_eq!(compare_key("Café.JPG"), compare_key("Cafe\u{0301}.jpg"));
    }

    #[test]
    fn canonical_string_strips_trailing_slash() {
        assert_eq!(
            canonical_string(Path::new("/Users/x/Photos/")),
            "/Users/x/Photos"
        );
        assert_eq!(canonical_string(Path::new("/")), "/");
    }

    #[test]
    fn container_detection() {
        let p = AppPaths::resolve();
        assert!(p.is_in_container(&p.db_file()));
        assert!(!p.is_in_container(Path::new("/Users/x/Pictures/a.jpg")));
    }

    #[test]
    fn db_lives_in_support_not_caches() {
        let p = AppPaths::resolve();
        assert!(p.db_file().starts_with(&p.support));
        assert!(
            !p.db_file().starts_with(&p.caches),
            "02 §5.2.1：索引不能放 Caches"
        );
    }
}
