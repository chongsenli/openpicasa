//! 「文件在云上、本机没有内容」的识别。
//!
//! ## 为什么需要这一层
//!
//! OneDrive / iCloud「按需下载」的占位文件在 macOS 上是**dataless** 的：
//! 目录项存在、`len()` 报着原始大小、`mtime` 也在，但本地磁盘上**没有数据**。
//! 读它会触发一次云下载；**云服务客户端没在跑时那次下载永远等不到**，
//! 实测 `read()` 直接返回 `ETIMEDOUT`（errno 60），耗时 0.7～27 s 不等。
//!
//! 后果有两处，都很用户可见：
//!
//! 1. 扫描线程卡在 `read()` 上（`sample` 里整个 `op-index` 线程停在
//!    `read (in libsystem_kernel.dylib)`），一个目录几百张就是几十分钟，
//!    用户以为程序死了；
//! 2. 缩略图生成把 `ETIMEDOUT` 归成 `Io(_)` → `Unreachable` → `thumb_state = 2`
//!    「失败」。可这些文件**没有坏**，只是没下载下来——把它们算进「失败数」、
//!    让用户去点「重试失败项」，是让用户白费力气。
//!
//! 所以：**读内容之前先问一句「这本机有内容吗」**，没有就跳过读取、单独记一个状态。
//!
//! ## 判据是文件系统标志位，不是猜测
//!
//! macOS 的 `st_flags` 里有 `SF_DATALESS`（`0x4000_0000`），只要文件是「仅有
//! 元数据、内容在云端」就会被置上。实测 OneDrive 的占位文件 `st_flags =
//! 0x4000_0060`（`0x40` = `UF_COMPRESSED`，`0x20` = `UF_DATAVAULT`）。
//!
//! 用标准库的 `std::os::macos::fs::MetadataExt::st_flags()` 就能读到，
//! **不需要引入 `libc` 依赖**（本 workspace 刻意没有它）。
//!
//! 其他平台一律返回 `false`：Windows 的云占位文件走 reparse point，
//! Linux 没有这套机制。宁可漏判（回到旧行为：读超时→算失败），也不要误判
//! （把正常文件当占位符跳过，用户会看到「照片在但永远没有缩略图」）。

use std::path::Path;

/// `SF_DATALESS`：内容不在本机。见 `sys/stat.h`。
#[cfg(target_os = "macos")]
const SF_DATALESS: u32 = 0x4000_0000;

/// 这个文件在本机有内容吗？`false` = 内容在云端，**不要直接读**。
///
/// 路径不存在、无权限时返回 `true`（「不是云占位符」）——那种情况由调用方
/// 按原有的「读失败」路径处理，不要在这里假装成功，否则真正的错误会被吞掉。
/// 「云端未下载」的统一文案。UI 与后台任务都用它，避免各处拼出不同说法。
pub const CLOUD_ONLY_REASON: &str = "文件在云端，本机未下载";

pub fn has_local_content(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta_has_local_content(&meta),
        Err(_) => true,
    }
}

/// 同 [`has_local_content`]，但复用已取到的元数据——扫描器在目录遍历时
/// 已经 `entry.metadata()` 过一次，再 `stat` 一遍是白花一次系统调用。
pub fn meta_has_local_content(meta: &std::fs::Metadata) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        meta.st_flags() & SF_DATALESS == 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = meta;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_files_count_as_local() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, b"hello").unwrap();
        assert!(has_local_content(&p), "普通文件必须被当成『本机有内容』");
    }

    #[test]
    fn missing_files_are_not_claimed_to_be_cloud_placeholders() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("nope.txt");
        // 关键：返回 true（不是云占位符），让调用方走「读失败」的既有路径。
        assert!(
            has_local_content(&gone),
            "路径不存在不该被误判成云占位符——那会把真错误藏起来"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dataless_flag_is_read_from_metadata() {
        use std::os::macos::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("b.txt");
        std::fs::write(&p, b"x").unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        // 本地文件的 flags 不含 SF_DATALESS（不同机器上其余位可能不同，只断这一位）。
        assert_eq!(
            meta.st_flags() & SF_DATALESS,
            0,
            "本地文件不该带 SF_DATALESS"
        );
        assert!(meta_has_local_content(&meta));
    }
}
