//! 与路径字符串有关的小工具。
//!
//! ## 为什么不用 `str::starts_with`
//!
//! `/a/bc` 的前缀是 `/a/b`，但它是另一个目录。`folders` 的子树查询在 SQL 侧
//! 用 `substr` 而不是 `LIKE`（`%`/`_` 要转义），Rust 侧同样必须**按组件**比较。
//!
//! ## 为什么这里用完整的 NFC
//!
//! `picasa-core::paths::nfc` 是 M1 的手写近似版，只预组合了十来个拉丁字符对——
//! 它够用来避免「同一串字符的两种编码产生两条索引」，但**不够用来改全库的存量数据**。
//! 迁移（`docs/05` §1.4.2）是一次性改写，错一个字符就是永久错，所以走
//! `unicode-normalization`（已在依赖里，见 workspace `Cargo.toml`）。

use unicode_normalization::UnicodeNormalization;

/// NFC 归一化。
pub(crate) fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// 比较键：NFC + 小写。
///
/// ⚠️ **是否小写由卷决定，不由这里决定**：`exclusions.match_key` 只在
/// `roots.case_insensitive = 1` 时才该小写。这里给的是「最严格口径」的键，
/// 用于 `folders.path_key`（V1 起就是这个语义）。
pub(crate) fn compare_key(s: &str) -> String {
    nfc(s).to_lowercase()
}

/// 严格祖先判定：`child` 是否在 `parent` 目录下（**不含**自身）。
pub(crate) fn is_under(child: &str, parent: &str) -> bool {
    if parent == "/" {
        return child.starts_with('/') && child != "/";
    }
    match child.strip_prefix(parent) {
        Some(rest) => rest.starts_with('/') && rest.len() > 1,
        None => false,
    }
}

/// 父目录。`/a/b` → `/a`；`/a` → `/`；`/` → `None`。
pub(crate) fn parent_of(path: &str) -> Option<String> {
    if path == "/" || path.is_empty() {
        return None;
    }
    match path.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(i) => Some(path[..i].to_string()),
        None => None,
    }
}

/// 展示名：末段；根目录用 `/`。
pub(crate) fn base_name(path: &str) -> &str {
    if path == "/" {
        return "/";
    }
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_wise_ancestry() {
        assert!(is_under("/a/b/c", "/a/b"));
        assert!(!is_under("/a/bc", "/a/b"), "组件边界不能只看前缀");
        assert!(!is_under("/a/b", "/a/b"), "自身不算自己的祖先");
        assert!(is_under("/a/b", "/"));
        assert!(!is_under("/", "/"));
    }

    #[test]
    fn parent_and_base() {
        assert_eq!(parent_of("/a/b").as_deref(), Some("/a"));
        assert_eq!(parent_of("/a").as_deref(), Some("/"));
        assert_eq!(parent_of("/"), None);
        assert_eq!(base_name("/run/media/x/Photos"), "Photos");
        assert_eq!(base_name("/"), "/");
    }

    #[test]
    fn nfc_handles_decomposed_latin_and_keeps_cjk() {
        assert_eq!(nfc("Cafe\u{0301}.JPG"), "Café.JPG");
        assert_eq!(nfc("苏州行"), "苏州行");
        assert_eq!(compare_key("Café.JPG"), "café.jpg");
    }
}
