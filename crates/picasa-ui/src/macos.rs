//! 系统原生交互：选目录面板 + 在文件管理器中显示 / 打开目录。
//!
//! 文件名沿用 `macos`（M1 只跑 macOS），但从 2026-10 起补了 Linux 分支，
//! 这里仍是**唯一**存放「调外部程序打开东西」的地方。
//!
//! ## 为什么这两件事必须走原生
//!
//! - **选目录**：F10-25 / R-3 要求走系统授权流程。`rfd` 背后在 macOS 是 `NSOpenPanel`，
//!   在 Linux 是 XDG Desktop Portal。用户在这里点「允许」才会产生 TCC 授权；
//!   自己手拼路径输入框**拿不到权限**。
//! - **在文件管理器中显示**：macOS 用 `open -R`，Linux 用 FileManager1 的 `ShowItems`。
//!
//! **注意**：这里不会、也不能往照片目录写任何东西——面板只读授权。

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

/// 在文件管理器中显示（并选中）。失败返回 `Err(说明)`。
pub fn reveal_in_finder(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("文件不存在：{}", path.display()));
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .status()
            .map_err(|e| format!("无法调用 open：{e}"))?;
    }

    #[cfg(target_os = "linux")]
    {
        // Linux 没有 `open -R` 的等价命令。先试 freedesktop 的 FileManager1.ShowItems
        // （Nautilus / Dolphin / Thunar 都实现），它才是「定位到该文件」；
        // 失败再退回「打开父目录」，至少让用户看到东西。
        let uri_arg = format!("array:string:file://{}", path.display());
        let shown = std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &uri_arg,
                "string:",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !shown {
            let dir = path.parent().unwrap_or(path);
            open_folder(dir)?;
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        return Err("当前平台暂不支持「在文件管理器中显示」".to_string());
    }

    Ok(())
}

/// 打开一个目录（用于「在文件夹中打开文件夹」）。
pub fn open_folder(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let program = "open";

    #[cfg(target_os = "linux")]
    let program = "xdg-open";

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let program = "";

    if program.is_empty() {
        return Err("当前平台暂不支持打开目录".to_string());
    }

    std::process::Command::new(program)
        .arg(path)
        .status()
        .map_err(|e| format!("无法调用 {program}：{e}"))?;
    Ok(())
}
