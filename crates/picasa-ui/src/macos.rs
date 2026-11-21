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

/// 选目录的结果。**必须**区分三种，不能把环境失败说成用户取消。
///
/// `rfd` 只回 `Option<PathBuf>`：`None` 既可能是用户按了取消，也可能是
/// XDG Desktop Portal / zenity 根本不存在（本仓库的无桌面冒烟机就是后者）。
/// 把两者混成一句「已取消」等于把功能缺失伪装成用户操作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickFolderOutcome {
    /// 用户选了目录
    Picked(PathBuf),
    /// 系统选择器弹出来了，用户取消
    Cancelled,
    /// 本环境**没能弹出**选择器（无桌面门户 / 无 zenity 等），附原因
    Unavailable(&'static str),
}

impl PickFolderOutcome {
    /// 给用户看的一句话说明。`Picked` 没有提示（正常路径）。
    ///
    /// * `Cancelled`：直说用户取消；
    /// * `Unavailable`：**明确承认选择器没弹出来**，并指出这是环境问题，
    ///   不把责任推给用户，也不假装功能已验收。
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Picked(_) => None,
            Self::Cancelled => Some("已取消选择文件夹".to_string()),
            Self::Unavailable(why) => Some(format!(
                "本环境未能弹出系统文件夹选择器（{why}）；没有添加任何文件夹"
            )),
        }
    }
}

/// 本机是否具备弹出系统选目录面板的条件（不含「用户会不会取消」）。
///
/// macOS 的 `NSOpenPanel` 一定在；Linux 上 `rfd` 走 XDG Desktop Portal，
/// 没有门户服务时会退到 `zenity`，两者都没有就只能静默返回 `None`——
/// 这正是需要被识别出来的失败。
pub fn picker_env_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(target_os = "linux")]
    {
        // 没有会话总线时门户与 zenity 都起不来（无桌面 / 纯 SSH 的典型情况）。
        // 先看这一条，能挡住「装了门户包但根本没有桌面会话」的假可用。
        let has_bus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|v| !v.is_empty());
        has_bus && (portal_service_present() || which("zenity"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

/// 环境不可用时的原因短语（可用时为 `None`）。
pub fn picker_unavailable_reason() -> Option<&'static str> {
    if picker_env_available() {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            Some("没有桌面会话总线（无桌面环境）")
        } else {
            Some("未找到 xdg-desktop-portal，也没有 zenity")
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Some("当前平台没有可用的系统目录选择器")
    }
}

/// 系统「文件选择器」服务是否在 D-Bus 上注册（只看服务文件与可执行文件，
/// **不**发起 D-Bus 调用——UI 线程上不能阻塞）。
#[cfg(target_os = "linux")]
fn portal_service_present() -> bool {
    const FILES: [&str; 3] = [
        "/usr/share/dbus-1/services/org.freedesktop.portal.Desktop.service",
        "/usr/local/share/dbus-1/services/org.freedesktop.portal.Desktop.service",
        "/usr/lib/systemd/user/org.freedesktop.portal.Desktop.service",
    ];
    if FILES.iter().any(|p| Path::new(p).exists()) {
        return true;
    }
    // 有的发行版把服务装在 $XDG_DATA_DIRS 下；再看一次可执行文件名。
    which("xdg-desktop-portal")
}

/// `PATH` 里有没有这个可执行文件（不执行它）。
#[cfg(target_os = "linux")]
fn which(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    path.split(':')
        .filter(|d| !d.is_empty())
        .any(|d| Path::new(d).join(name).is_file())
}

/// 弹出系统「选择文件夹」面板，并区分「取消 / 环境不可用」。
///
/// 仍然**只有一条实现**（`rfd`）：本函数只是在它返回 `None` 之后补一次
/// 环境判定，不引入第二套选目录。
pub fn pick_folder_outcome(start: Option<&Path>) -> PickFolderOutcome {
    if let Some(dir) = pick_folder(start) {
        return PickFolderOutcome::Picked(dir);
    }
    match picker_unavailable_reason() {
        Some(why) => PickFolderOutcome::Unavailable(why),
        None => PickFolderOutcome::Cancelled,
    }
}

/// 弹出系统「选择文件夹」面板。没有选到路径时返回 `None`。
///
/// ⚠️ 必须在主线程调用（egui 的 UI 线程就是主线程，所以没问题）。
///
/// `None` **不能**区分「用户取消」和「选择器根本没弹出来」。`rfd` 把两种
/// 情况都收成 `None`（Linux 上 XDG Desktop Portal 失败、又没有 zenity 时
/// 尤其安静）。调用方必须给出可见提示，文案用 [`pick_folder_none_toast`]；
/// 需要区分时用 [`pick_folder_outcome`]。
pub fn pick_folder(start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("选择照片文件夹");
    if let Some(dir) = start {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_folder()
}

/// `pick_folder` 返回 `None` 时给用户看的提示，以及停留秒数。
///
/// 这是**不分平台**的兜底文案；能分清「取消 / 环境不可用」的地方
/// （`add_folder`）应改用 [`pick_folder_none_toast_for`]。
///
/// macOS 的后端是 `NSOpenPanel`，`None` 就是用户取消，可以直说。
/// 其他平台（内部冒烟的 Linux）无法分辨取消与门户/zenity 不可用，
/// 文案同时覆盖两种情况，不把环境故障说成用户的错。
pub fn pick_folder_none_toast() -> (&'static str, f64) {
    #[cfg(target_os = "macos")]
    {
        ("已取消选择文件夹", 2.5)
    }
    #[cfg(not(target_os = "macos"))]
    {
        ("未选择文件夹：已取消，或系统文件夹选择器未能弹出", 4.5)
    }
}

/// `PickFolderOutcome` → `(文案, 停留秒数)`，给 `toast` 用。
pub fn pick_folder_none_toast_for(outcome: &PickFolderOutcome) -> (String, f64) {
    match outcome {
        PickFolderOutcome::Picked(_) => (String::new(), 0.0),
        PickFolderOutcome::Cancelled => ("已取消选择文件夹".to_string(), 2.5),
        // 环境失败比「取消」更需要被读到：停久一点。
        PickFolderOutcome::Unavailable(_) => (outcome.message().unwrap_or_default(), 6.0),
    }
}

/// M4 导出：系统「保存文件」面板（`.opicasaidx` / `.opicasaidx.zip`）。
pub fn pick_export_index(start: Option<&Path>, with_thumbs: bool) -> Option<PathBuf> {
    let ext = if with_thumbs {
        "opicasaidx.zip"
    } else {
        "opicasaidx"
    };
    let mut dialog = rfd::FileDialog::new()
        .set_title("导出索引文件（不含原始照片）")
        .set_file_name(format!("photo-index.{ext}"))
        .add_filter("OpenPicasa 索引文件", &[ext]);
    if let Some(dir) = start {
        dialog = dialog.set_directory(dir);
    }
    dialog.save_file()
}

/// M4 导入：系统「打开文件」面板。
pub fn pick_import_index(start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("选择 OpenPicasa 索引文件")
        .add_filter("OpenPicasa 索引文件", &["opicasaidx", "opicasaidx.zip"]);
    if let Some(dir) = start {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_file()
}

/// M4 重新匹配：选一个本机目录作为新的图库根。
pub fn pick_remap_folder(start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("重新匹配图库根目录");
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

    #[cfg(target_os = "windows")]
    {
        // `explorer /select,<path>` 在资源管理器中定位并选中该文件。
        // 注意参数必须作为一个整体传（逗号是 explorer 的语法，不是分隔符）。
        let arg = format!("/select,{}", path.display());
        std::process::Command::new("explorer")
            .arg(arg)
            .status()
            .map_err(|e| format!("无法调用 explorer：{e}"))?;
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
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

    #[cfg(target_os = "windows")]
    let program = "explorer";

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
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

#[cfg(test)]
mod tests {
    use super::pick_folder_none_toast;

    #[test]
    fn none_toast_is_visible_and_does_not_only_blame_the_user() {
        let (msg, ttl) = pick_folder_none_toast();
        assert!(!msg.is_empty());
        assert!(ttl > 0.0);
        // 两种平台都要让人看见「没选到」。macOS 可以直说取消；
        // 其他平台必须同时承认选择器可能根本没弹出来。
        assert!(msg.contains("文件夹"));
        #[cfg(target_os = "macos")]
        assert!(msg.contains("取消"));
        #[cfg(not(target_os = "macos"))]
        {
            assert!(msg.contains("取消"));
            assert!(
                msg.contains("未能弹出"),
                "非 macOS 不能把门户/zenity 缺失说成单纯的用户取消"
            );
        }
    }
    /// M5-R1 closeout：三种结果必须区分，且**不能**把环境失败说成用户取消。
    #[test]
    fn picker_outcome_distinguishes_cancel_from_environment_failure() {
        use super::{PickFolderOutcome as O, pick_folder_none_toast_for};

        let picked = O::Picked(std::path::PathBuf::from("/photos"));
        assert_eq!(picked.message(), None, "选到目录不该有提示");

        let cancelled = O::Cancelled;
        let msg = cancelled.message().unwrap();
        assert!(msg.contains("取消"), "{msg}");
        assert!(!msg.contains("未能弹出"), "用户取消不是环境失败：{msg}");

        let unavailable = O::Unavailable("未找到 xdg-desktop-portal");
        let msg = unavailable.message().unwrap();
        assert!(msg.contains("未能弹出"), "{msg}");
        assert!(msg.contains("未找到 xdg-desktop-portal"), "{msg}");
        assert!(
            !msg.starts_with("已取消"),
            "环境失败不能被写成「用户取消」：{msg}"
        );
        // 环境失败必须给出原因，而不是一句含糊的「没选」。
        assert!(msg.contains("没有添加任何文件夹"), "{msg}");

        // toast 停留时间：环境失败比取消更久（更需要被读到）。
        let (_, t_cancel) = pick_folder_none_toast_for(&O::Cancelled);
        let (_, t_unavail) = pick_folder_none_toast_for(&unavailable);
        assert!(
            t_unavail > t_cancel,
            "环境失败要停更久：{t_unavail} vs {t_cancel}"
        );
        let (m, _) = pick_folder_none_toast_for(&picked);
        assert!(m.is_empty(), "成功路径不该产生 toast");
    }

    /// 本机条件与原因短语必须自洽：说得出原因 = 一定不可用。
    #[test]
    fn unavailable_reason_matches_environment_probe() {
        use super::{picker_env_available, picker_unavailable_reason};
        match picker_unavailable_reason() {
            Some(why) => {
                assert!(!picker_env_available(), "有原因就应当判定为不可用");
                assert!(!why.is_empty());
            }
            None => assert!(picker_env_available(), "没原因就应当判定为可用"),
        }
    }
}
