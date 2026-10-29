//! macOS 专有交互：原生选目录面板（TCC 入口）+ 在 Finder 中显示。
//!
//! ## 为什么这两件事必须走原生
//!
//! - **选目录**：F10-25 / R-3 要求走系统授权流程。`rfd` 背后是 `NSOpenPanel`，
//!   用户在这里点「允许」才会产生 TCC 授权；自己手拼路径输入框**拿不到权限**。
//! - **在 Finder 中显示**：`open -R` 是系统自带命令，与 `NSWorkspace` 等价且零依赖。
//!
//! **注意**：这里不会、也不能往照片目录写任何东西——`NSOpenPanel` 只读授权。

use std::path::{Path, PathBuf};

/// 弹出系统「选择文件夹」面板。用户取消返回 `None`。
///
/// ⚠️ 必须在主线程调用（egui 的 UI 线程就是主线程，所以没问题）。
pub fn pick_folder(start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("选择照片文件夹（只读，不会修改你的文件）");
    if let Some(dir) = start {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_folder()
}

/// 在 Finder 中显示（并选中）。失败返回 `Err(说明)`。
pub fn reveal_in_finder(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("文件不存在：{}", path.display()));
    }
    std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .status()
        .map_err(|e| format!("无法调用 open：{e}"))?;
    Ok(())
}

/// 打开一个目录（用于「在 Finder 中打开文件夹」）。
pub fn open_folder(path: &Path) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(path)
        .status()
        .map_err(|e| format!("无法调用 open：{e}"))?;
    Ok(())
}
