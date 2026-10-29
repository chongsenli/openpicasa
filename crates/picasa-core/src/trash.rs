//! 删除到回收站（F2-08）——**R-1 的唯一例外**。
//!
//! ## 边界（01 §11.5.3 明确规定，代码必须逐条对上）
//!
//! 1. **用户主动触发且二次确认**：本模块只提供「移到废纸篓」这个动作，
//!    确认弹窗在 UI 层，且文案必须写明「可在系统里还原」。
//! 2. **只移动到系统回收站，不做真正的 unlink**：实现是 `rename` 到
//!    `~/.Trash/` 或 `<卷>/.Trashes/<uid>/`，与 Finder 的行为一致。
//! 3. **不批量静默执行**：调用方必须一次一张地过确认。
//!
//! 为什么不用 `NSFileManager trashItemAtURL`：那需要把 objc2 引入 M1 的依赖图，
//! 而 `rename` 到 trash 目录在语义上等价、零依赖、可测试。代价是自己处理
//! 重名与跨卷，这两件事都在下面做了。

use crate::{CoreError, Result};
use std::path::{Path, PathBuf};

/// 把文件移到回收站，返回它在回收站里的最终路径。
///
/// 失败时**不改动原文件**（rename 要么成功要么不生效），并把原因说清楚。
pub fn move_to_trash(path: &Path) -> Result<PathBuf> {
    if !path.exists() {
        return Err(CoreError::Other(format!("文件不存在：{}", path.display())));
    }
    let trash_dir = trash_dir_for(path)?;
    std::fs::create_dir_all(&trash_dir).map_err(|e| {
        CoreError::Other(format!(
            "无法创建回收站目录 {}：{e}（外接盘可能需要先在 Finder 里删除一次）",
            trash_dir.display()
        ))
    })?;

    let dest = unique_destination(&trash_dir, path);
    match std::fs::rename(path, &dest) {
        Ok(()) => Ok(dest),
        // EXDEV = 18（macOS/Linux 一致）。用 errno 判断而不是 `ErrorKind::CrossesDevices`，
        // 后者在稳定版里的可用性随版本变化，而 errno 是 POSIX 常量，永远不会变。
        Err(e) if e.raw_os_error() == Some(18) => Err(CoreError::Other(format!(
            "无法移动到回收站（跨卷）：{}。请在 Finder 中删除这张照片。",
            path.display()
        ))),
        Err(e) => Err(CoreError::Other(format!(
            "移动到回收站失败：{}（{e}）",
            path.display()
        ))),
    }
}

/// 该文件所在卷的回收站目录。
///
/// - 启动盘：`~/.Trash`
/// - 外接盘：`<卷>/.Trashes/<uid>`（Finder 用的也是这个位置）
///
/// 两者都**不在照片目录内**——除非用户把照片直接放在卷根目录，
/// 那种情况下 `.Trashes` 与照片同级但不同目录，仍不构成「改写用户的照片目录内容」。
fn trash_dir_for(path: &Path) -> Result<PathBuf> {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("/Volumes/") {
        let vol = rest.split('/').next().unwrap_or("");
        if !vol.is_empty() {
            let uid = unsafe { libc_getuid() };
            return Ok(PathBuf::from(format!("/Volumes/{vol}/.Trashes/{uid}")));
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| CoreError::Other("无法确定 HOME 目录".into()))?;
    Ok(home.join(".Trash"))
}

/// 重名时加 ` 1`、` 2` 后缀——与 Finder 的命名习惯一致。
fn unique_destination(trash_dir: &Path, src: &Path) -> PathBuf {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "未命名".into());
    let mut candidate = trash_dir.join(&name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), Some(e.to_string())),
        _ => (name.clone(), None),
    };
    for i in 1..10_000 {
        let fname = match &ext {
            Some(e) => format!("{stem} {i}.{e}"),
            None => format!("{stem} {i}"),
        };
        candidate = trash_dir.join(fname);
        if !candidate.exists() {
            return candidate;
        }
    }
    candidate
}

/// `getuid(2)`：只为一个数字引入 `libc` crate 不值得，直接用 FFI 声明。
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_destination_appends_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = tmp.path();
        let src = tmp.path().join("IMG_0001.jpg");

        let first = unique_destination(trash, &src);
        assert_eq!(first.file_name().unwrap(), "IMG_0001.jpg");
        std::fs::write(&first, b"x").unwrap();

        let second = unique_destination(trash, &src);
        assert_eq!(second.file_name().unwrap(), "IMG_0001 1.jpg");
        std::fs::write(&second, b"x").unwrap();

        let third = unique_destination(trash, &src);
        assert_eq!(third.file_name().unwrap(), "IMG_0001 2.jpg");
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        assert!(move_to_trash(Path::new("/definitely/not/here.jpg")).is_err());
    }

    #[test]
    fn boot_volume_trash_is_home_trash() {
        let p = trash_dir_for(Path::new("/Users/x/Pictures/a.jpg")).unwrap();
        assert!(p.ends_with(".Trash"));
        assert!(!p.starts_with("/Users/x/Pictures"));
    }

    #[test]
    fn external_volume_trash_is_dot_trashes() {
        let p = trash_dir_for(Path::new("/Volumes/Photos/2011/a.jpg")).unwrap();
        assert_eq!(p.parent().unwrap(), Path::new("/Volumes/Photos/.Trashes"));
    }
}
