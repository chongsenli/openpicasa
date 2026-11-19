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
    /// `C`：查看器并排比较（M4 #11）
    ToggleCompare,
    /// 切换「包含子文件夹」（`07-A` RECURSE-01：`⌘⇧R`）
    ToggleRecurse,
    /// `⌘0`：缩略图滑块回默认刻度 96 px（`M5-KB-08`；产品事实，不随 §11 #1 改）
    ZoomReset,
    /// `⌘,`：打开设置页（`M5-IA-04`，与更多菜单「设置…」同一入口）
    OpenSettings,
    /// `⌘F`：展开并聚焦顶栏搜索（§7.2 搜索紧接在 + 之后）
    FocusSearch,
}

/// 收集本帧的按键。`in_viewer` 决定用哪一套映射。
///
/// 焦点在输入框里时**一律不处理**（否则用户打「A」会全选照片）。
pub fn collect(ctx: &Context, in_viewer: bool) -> Vec<KeyAction> {
    collect_ex(ctx, in_viewer, false)
}

/// 同 [`collect`]，但额外知道**上一帧结束时焦点是否停在输入框**。
///
/// ## 为什么需要这个参数（`M5-SF-01` / `AC-KB-FOCUS`）
///
/// egui 在每一帧开头的 `begin_pass` 里就把「按了 Escape」当作失焦动作，
/// 直接清掉 `focused_widget`。于是用户在搜索框里按 Esc 的那一帧，
/// `ctx.memory(|m| m.focused())` 已经是 `None`——只这一条判据会把 Esc 当成
/// 「关闭查看器」，用户想失焦却被踢出查看器，正是验收里明令禁止的行为。
///
/// 调用方在上一帧末尾记下焦点状态传进来：那一帧所有快捷键都停用，
/// Esc 只由 egui 完成失焦，不产生任何 `KeyAction`。
pub fn collect_ex(ctx: &Context, in_viewer: bool, input_focus_prev: bool) -> Vec<KeyAction> {
    if input_focus_prev || ctx.memory(|m| m.focused()).is_some() {
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
            // `⌘0`：缩略图尺寸回默认刻度（`07-A` ZOOM-03）
            if cmd && i.key_pressed(Key::Num0) {
                out.push(KeyAction::ZoomReset);
            }
            // M5-TB-10：`⌘1`/`⌘2`/`⌘+`/`⌘−` 与底栏滑块共用同一状态，
            // 按**线性 16 px** 一档（不再是对数档）。
            // `⌘0` 回 96 px（产品事实；§11 #1 的默认 96 vs 144 未裁定，不擅自改）。
            if cmd && (i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals)) {
                out.push(KeyAction::TierLarge);
            }
            if cmd && i.key_pressed(Key::Minus) {
                out.push(KeyAction::TierSmall);
            }
            // `⌘F`：展开顶栏搜索框并聚焦（M3.1 既有快捷键，§7 版式下仍适用）
            if cmd && i.key_pressed(Key::F) {
                out.push(KeyAction::FocusSearch);
            }
            // `⌘,`：设置页（与「更多 → 设置…」同一入口，AC-IA-MORE）
            if cmd && i.key_pressed(Key::Comma) {
                out.push(KeyAction::OpenSettings);
            }
            // `⌘⇧R`：包含子文件夹（`07-A` RECURSE-01）。
            // 用 `shift` 而不是另设 `⌘R`——后者在不少应用里是「刷新」，
            // 而本视图里「刷新」是右键菜单的语义，冲突会让用户按错。
            if cmd && shift && i.key_pressed(Key::R) {
                out.push(KeyAction::ToggleRecurse);
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
            if i.key_pressed(Key::C) {
                out.push(KeyAction::ToggleCompare);
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
            // `M5-VW-15` / `M5-KB-14`：查看器里的 Delete / Backspace 同样走废纸篓确认。
            //
            // 这一支原先**漏了**——`apply_viewer_key` 里有处理，但收集阶段没往
            // `out` 里塞，于是查看器按 Delete 毫无反应（能力形同不存在）。
            // 焦点在输入框时 `collect` 在函数开头就整体返回空，所以这里不需要再判。
            if i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace) {
                out.push(KeyAction::Delete);
            }
            let _ = shift;
        }
    });

    out
}

#[cfg(test)]
mod m5r4_viewer_tests {
    use super::*;
    use eframe::egui::{Event, Modifiers, Pos2, RawInput, Rect, vec2};

    /// 造一帧：只按下指定键。
    fn press(ctx: &Context, key: Key, modifiers: Modifiers) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 820.0))),
            max_texture_side: Some(2048),
            // `collect` 读的是 InputState 的当前修饰键：先发 ModifiersChanged
            events: vec![
                Event::ModifiersChanged(modifiers),
                Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
            ],
            ..Default::default()
        };
        crate::theme::apply(ctx, picasa_core::Appearance::Light);
        // egui 自己把 ⌘0 / ⌘+ / ⌘− 当「界面缩放」并在**帧末**消费掉这些快捷键。
        // 应用里的 `collect` 跑在帧内（`handle_keys`），所以不受影响；而这里的
        // 测试助手是在整帧跑完才回读，不关掉它就会误判成「⌘0 没有映射」。
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let out = ctx.run_ui(input, |ui| {
            let _ = ui;
        });
        out.drop_without_applying_deltas();
    }

    /// 查看器缩放键：PageUp 放大、PageDown 适合、数字 1 = 100%（不打分）。
    #[test]
    fn viewer_zoom_keys_map_to_expected_actions() {
        let ctx = Context::default();
        press(&ctx, Key::PageUp, Modifiers::NONE);
        assert_eq!(collect(&ctx, true), vec![KeyAction::ZoomIn]);
        press(&ctx, Key::PageDown, Modifiers::NONE);
        assert_eq!(collect(&ctx, true), vec![KeyAction::ZoomFit]);
        press(&ctx, Key::Num1, Modifiers::NONE);
        assert_eq!(
            collect(&ctx, true),
            vec![KeyAction::ZoomActual],
            "数字 1 是 100%，不是评分"
        );
    }

    /// 查看器 +/-、↑/↓、I、Space、C 映射保持。
    #[test]
    fn viewer_retained_keys_still_map() {
        let ctx = Context::default();
        for (key, expected) in [
            (Key::Plus, KeyAction::ZoomIn),
            (Key::Equals, KeyAction::ZoomIn),
            (Key::ArrowUp, KeyAction::ZoomIn),
            (Key::Minus, KeyAction::ZoomOut),
            (Key::ArrowDown, KeyAction::ZoomOut),
            (Key::I, KeyAction::ToggleInfo),
            (Key::Space, KeyAction::ToggleBackground),
            (Key::C, KeyAction::ToggleCompare),
            (Key::Escape, KeyAction::CloseViewer),
            (Key::ArrowLeft, KeyAction::Prev),
            (Key::ArrowRight, KeyAction::Next),
        ] {
            press(&ctx, key, Modifiers::NONE);
            assert_eq!(collect(&ctx, true), vec![expected], "{key:?}");
        }
    }

    /// `AC-KB-FOCUS`：焦点在输入框时**整张快捷键表停用**。
    ///
    /// 还要覆盖「Esc 那一帧」：egui 在帧首把 Esc 当失焦动作、已清掉 `focused()`，
    /// 只看本帧会把 Esc 当成「关闭查看器」。调用方传入上一帧的焦点状态即可挡住。
    #[test]
    fn input_focus_disables_the_whole_shortcut_table_including_escape() {
        let ctx = Context::default();
        // 上一帧焦点在输入框：这一帧任何键都不产生动作（含 Esc）。
        for key in [Key::Escape, Key::Delete, Key::Num1, Key::J, Key::A] {
            press(&ctx, key, Modifiers::NONE);
            assert!(
                collect_ex(&ctx, true, true).is_empty(),
                "输入框聚焦时 {key:?} 不该生效"
            );
            assert!(collect_ex(&ctx, false, true).is_empty(), "图库同样停用");
        }
        // 没有输入框焦点时，Esc 恢复为「关闭查看器」。
        press(&ctx, Key::Escape, Modifiers::NONE);
        assert_eq!(collect_ex(&ctx, true, false), vec![KeyAction::CloseViewer]);
    }

    /// `AC-KB-01`：图库侧的整张表都在，且每一键都能映射到动作（不是空转）。
    #[test]
    fn library_shortcut_table_covers_every_documented_key() {
        let ctx = Context::default();
        for (key, mods, expected) in [
            (Key::A, Modifiers::COMMAND, KeyAction::SelectAll),
            (Key::D, Modifiers::COMMAND, KeyAction::SelectNone),
            (Key::I, Modifiers::COMMAND, KeyAction::Invert),
            (Key::H, Modifiers::COMMAND, KeyAction::ToTray),
            (Key::Num1, Modifiers::COMMAND, KeyAction::TierSmall),
            (Key::Num2, Modifiers::COMMAND, KeyAction::TierLarge),
            (Key::Num0, Modifiers::COMMAND, KeyAction::ZoomReset),
            (Key::Minus, Modifiers::COMMAND, KeyAction::TierSmall),
            (Key::Plus, Modifiers::COMMAND, KeyAction::TierLarge),
            (Key::F, Modifiers::COMMAND, KeyAction::FocusSearch),
            (Key::R, Modifiers::COMMAND | Modifiers::SHIFT, KeyAction::ToggleRecurse),
            (Key::F11, Modifiers::NONE, KeyAction::ToggleFullscreen),
            (Key::Enter, Modifiers::COMMAND, KeyAction::Reveal),
            (Key::Enter, Modifiers::NONE, KeyAction::OpenViewer),
            (Key::Home, Modifiers::NONE, KeyAction::ToStart),
            (Key::End, Modifiers::NONE, KeyAction::ToEnd),
        ] {
            press(&ctx, key, mods);
            assert_eq!(collect(&ctx, false), vec![expected], "{key:?} + {mods:?}");
        }
        // 图库里 Enter 与 ⌘Enter 是两件事：一个进查看器、一个显示位置。
        // 同时按下时 `OpenViewer` 的判据带了 `!cmd`，所以只出 Reveal。
        press(&ctx, Key::Enter, Modifiers::COMMAND);
        assert_eq!(collect(&ctx, false), vec![KeyAction::Reveal]);
    }

    /// `AC-OUT-C` / `M5-OR-13`：数字键 2–5 在查看器里**什么都不做**。
    /// 原型用 0–5 打分，本产品没有评分，不能留一个能改状态的半成品入口。
    #[test]
    fn viewer_digit_keys_other_than_one_are_inert() {
        let ctx = Context::default();
        for key in [Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num0] {
            press(&ctx, key, Modifiers::NONE);
            assert!(
                collect(&ctx, true).is_empty(),
                "{key:?} 在查看器里不该有任何动作（没有评分）"
            );
        }
    }

    /// 查看器放大键不会落到「给照片打分」的动作上（没有那样的 KeyAction）。
    #[test]
    fn viewer_numeric_one_is_not_a_rating_action() {
        let ctx = Context::default();
        press(&ctx, Key::Num1, Modifiers::NONE);
        assert_eq!(collect(&ctx, true), vec![KeyAction::ZoomActual]);
        // 图库语境里 ⌘1 才是缩略图档位（与查看器无关）
        press(&ctx, Key::Num1, Modifiers::COMMAND);
        assert_eq!(collect(&ctx, false), vec![KeyAction::TierSmall]);
    }
}
