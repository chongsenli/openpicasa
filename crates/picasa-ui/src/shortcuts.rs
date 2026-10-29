//! M1 快捷键：**只实现「浏览」相关键位**（01 附录 A 的 v2.0 表）。
//!
//! ## 红线 R-2 在这里最容易被破
//!
//! 编辑语义的键位（`Ctrl+3` / `Ctrl+Z` / `Ctrl+R` / `8` / `Ctrl+T`）在 M1 **必须完全无响应**——
//! 不是「按了弹个提示」，而是连提示都不能有（那等于留了个半成品入口）。
//! 这个文件里**根本没有**这些键位，所以「不小心实现了半个编辑功能」在结构上不可能发生。

use eframe::egui::{Context, Key};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    SelectAll,
    SelectNone,
    Invert,
    ToStart,
    ToEnd,
    ToTray,
    TierSmall,
    TierLarge,
    ToggleFullscreen,
    Reveal,
    Delete,
    OpenViewer,
    CloseViewer,
    Next,
    Prev,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ZoomActual,
    ToggleBackground,
    ToggleInfo,
}

/// 收集本帧的按键。`in_viewer` 决定用哪一套映射。
///
/// 焦点在输入框里时**一律不处理**（否则用户打「A」会全选照片）。
pub fn collect(ctx: &Context, in_viewer: bool) -> Vec<KeyAction> {
    if ctx.memory(|m| m.focused()).is_some() {
        return Vec::new();
    }
    let mut out = Vec::new();

    ctx.input(|i| {
        let cmd = i.modifiers.command || i.modifiers.ctrl;
        let shift = i.modifiers.shift;

        if !in_viewer {
            // ---- 选择 ----
            if cmd && i.key_pressed(Key::A) {
                out.push(KeyAction::SelectAll);
            }
            if cmd && i.key_pressed(Key::D) {
                out.push(KeyAction::SelectNone);
            }
            if cmd && i.key_pressed(Key::I) {
                out.push(KeyAction::Invert);
            }
            if i.key_pressed(Key::Home) {
                out.push(KeyAction::ToStart);
            }
            if i.key_pressed(Key::End) {
                out.push(KeyAction::ToEnd);
            }
            if cmd && i.key_pressed(Key::H) {
                out.push(KeyAction::ToTray);
            }
            // ---- 视图 ----
            if cmd && i.key_pressed(Key::Num1) {
                out.push(KeyAction::TierSmall);
            }
            if cmd && i.key_pressed(Key::Num2) {
                out.push(KeyAction::TierLarge);
            }
            if i.key_pressed(Key::F11) {
                out.push(KeyAction::ToggleFullscreen);
            }
            if cmd && i.key_pressed(Key::Enter) {
                out.push(KeyAction::Reveal);
            }
            if i.key_pressed(Key::Enter) && !cmd {
                out.push(KeyAction::OpenViewer);
            }
            if i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace) {
                out.push(KeyAction::Delete);
            }
        } else {
            // ---- 查看器 ----
            if i.key_pressed(Key::Escape) {
                out.push(KeyAction::CloseViewer);
            }
            if i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::J) {
                out.push(KeyAction::Prev);
            }
            if i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::K) {
                out.push(KeyAction::Next);
            }
            if i.key_pressed(Key::Enter) || i.key_pressed(Key::F11) {
                out.push(KeyAction::ToggleFullscreen);
            }
            if i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals) || i.key_pressed(Key::ArrowUp)
            {
                out.push(KeyAction::ZoomIn);
            }
            if i.key_pressed(Key::Minus) || i.key_pressed(Key::ArrowDown) {
                out.push(KeyAction::ZoomOut);
            }
            if i.key_pressed(Key::PageUp) {
                out.push(KeyAction::ZoomIn); // PageUp = 4 倍放大（01 附录 A）
            }
            if i.key_pressed(Key::PageDown) {
                out.push(KeyAction::ZoomFit);
            }
            if i.key_pressed(Key::Num1) {
                out.push(KeyAction::ZoomActual);
            }
            if i.key_pressed(Key::Space) {
                out.push(KeyAction::ToggleBackground);
            }
            if i.key_pressed(Key::I) {
                out.push(KeyAction::ToggleInfo);
            }
            if cmd && i.key_pressed(Key::Enter) {
                out.push(KeyAction::Reveal);
            }
            let _ = shift;
        }
    });

    out
}
