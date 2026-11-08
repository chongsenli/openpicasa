//! OpenPicasa 的可执行入口。
//!
//! 真正的逻辑在 `picasa_ui::run()`——这里只留一行，
//! 这样以后加「诊断模式 / 命令行开关」时不用动应用结构。

// Windows 上声明 GUI 子系统（PE Optional Header Subsystem = 2）。
// 不声明时链接器默认产出 Subsystem = 3 (CUI)，双击启动 Windows 会额外分配一个
// conhost 控制台窗口，GUI 窗口旁边多出一个黑框。日志写应用数据目录的
// openpicasa.log（见 init_tracing），不依赖 stdout；需要实时日志时从终端
// 运行并重定向：`OpenPicasa.exe > log.txt 2>&1`。
// cfg_attr 保证非 Windows 目标不会触发 unused attribute 警告。
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() -> eframe::Result {
    picasa_ui::run()
}
