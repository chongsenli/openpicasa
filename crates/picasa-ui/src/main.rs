//! OpenPicasa 的可执行入口。
//!
//! 真正的逻辑在 `picasa_ui::run()`——这里只留一行，
//! 这样以后加「诊断模式 / 命令行开关」时不用动应用结构。

fn main() -> eframe::Result {
    picasa_ui::run()
}
