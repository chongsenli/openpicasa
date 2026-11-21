//! 「移到废纸篓」确认框的**决策表**（`M5-FS-01` / `M5-VW-15` / `M5-KB-14`）。
//!
//! `app.rs` 里的弹框每帧只做一件事：把「用户这一帧做了什么」和「此刻的真实范围」
//! 交给 [`decide`]，然后照返回的动作执行。把这张表抽出来的唯一理由是——
//! 「取消零执行」「范围变化作废」必须能被单元测试钉住，而它们写在 egui 的
//! 闭包里就只能靠实机点击验证。

use picasa_core::TrashScope;

use crate::theme::Theme;

/// T2：废纸篓确认框的两个按钮。
///
/// 「移到废纸篓」是**破坏性主按钮**：实心危险红 + 白字；
/// 「取消」保持普通（次）按钮样式，并且仍是默认焦点——
/// 回车 / 空格落在取消上，移动原文件必须再主动点一次。
pub fn confirm_buttons(theme: &Theme) -> (egui::Button<'static>, egui::Button<'static>) {
    // 次按钮：卡片底 + 明确可见的描边（原型里「取消」是描边按钮），
    // 视觉上明确弱于红色主按钮。
    let cancel = egui::Button::new(
        egui::RichText::new(picasa_core::trash_plan::TRASH_CANCEL).color(theme.text_primary),
    )
    .fill(theme.viewer_card)
    .stroke(egui::Stroke::new(1.2, theme.text_secondary))
    .corner_radius(theme.radius_btn);
    // 主按钮：实心危险红 + 白字。
    let confirm = egui::Button::new(
        egui::RichText::new(picasa_core::trash_plan::TRASH_ACTION).color(egui::Color32::WHITE),
    )
    .fill(theme.danger)
    .corner_radius(theme.radius_btn);
    (confirm, cancel)
}

/// 对话框宽度：按屏幕**逻辑像素**取比例的居中卡片。
///
/// 原型的 1280 截图是物理像素（2560 宽、DPR 2），不能直接塞进 1024 逻辑
/// 坐标；原型里卡片约占窗口逻辑宽的 33%（约 417 / 1280），这里取 33%
/// （下限 380 / 上限 520 逻辑像素），所以同一窗口在任意 DPI 下都同样居中、
/// 同样比例。
pub fn dialog_width(screen_w: f32) -> f32 {
    (screen_w * 0.33).clamp(380.0, 520.0)
}

/// 确认框一帧的返回值：动作用户点击/按键决定，`open` 仅用于「叉掉」语义
/// （本原型没有叉，保留给 Escape / 关闭窗口路径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialogResponse {
    pub action: UserAction,
}

impl Default for DialogResponse {
    fn default() -> Self {
        Self {
            action: UserAction::None,
        }
    }
}

/// 按原型 v1.4 画「移到废纸篓」确认框（居中、较大圆角对话框 + 背景遮罩）。
///
/// * 尺寸按**当前屏幕逻辑像素**取比例（宽 min 520 / 46%），不把 1280 截图的
///   物理像素硬塞进 1024 逻辑坐标；同一图窗在两种 DPI 下都是居中的对话框。
/// * 标题左上；正文分段（会发生什么 / 如何还原）＋ 真实文件路径；按钮
///   右对齐：描边「取消」+ 危险红主按钮「移到废纸篓」。
/// * 遮罩铺满窗口并吃掉点击（后面的删除 / 选择等操作点不到）；点遮罩 =
///   取消（零执行）。默认焦点在「取消」，Escape = 取消。
/// * 只画真实张数/路径；不写「仅模拟 / 不移动真实照片」之类演示文案。
pub fn show_dialog(
    ctx: &egui::Context,
    theme: &Theme,
    count: usize,
    paths: &[String],
) -> DialogResponse {
    let screen = ctx.content_rect();
    let width = dialog_width(screen.width());
    let mut action = UserAction::None;

    // 遮罩：铺满窗口，吃掉落在后面的所有点击。
    egui::Area::new(egui::Id::unique("trash-confirm-scrim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(96));
            if resp.clicked() {
                action = UserAction::Dismiss;
            }
        });

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        action = UserAction::Dismiss;
    }

    let card_fill = if theme.dark {
        theme.menu_bg
    } else {
        theme.viewer_card
    };
    let mut clicked = UserAction::None;
    egui::Area::new(egui::Id::unique("trash-confirm"))
        .order(egui::Order::Tooltip)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(card_fill)
                .corner_radius(16)
                .inner_margin(egui::Margin::same(24))
                .stroke(egui::Stroke::new(1.0, theme.hairline))
                .show(ui, |ui| {
                    ui.set_width(width - 48.0);
                    ui.label(
                        egui::RichText::new(picasa_core::trash_plan::TRASH_TITLE)
                            .size(16.0)
                            .strong()
                            .color(theme.text_primary),
                    );
                    ui.add_space(10.0);
                    for para in picasa_core::trash_plan::confirm_body(count).split("\n\n") {
                        ui.label(
                            egui::RichText::new(para)
                                .size(12.5)
                                .color(theme.text_secondary),
                        );
                        ui.add_space(8.0);
                    }
                    if !paths.is_empty() {
                        let shown: Vec<&String> = paths.iter().take(3).collect();
                        for p in &shown {
                            ui.label(
                                egui::RichText::new(p.as_str())
                                    .size(11.5)
                                    .color(theme.text_primary),
                            );
                        }
                        if paths.len() > shown.len() {
                            ui.label(
                                egui::RichText::new(format!(
                                    "… 另有 {} 个文件",
                                    paths.len() - shown.len()
                                ))
                                .size(11.0)
                                .color(theme.text_secondary),
                            );
                        }
                        ui.add_space(4.0);
                    }
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (confirm, cancel) = confirm_buttons(theme);
                        let confirm_resp = ui.add(confirm);
                        if confirm_resp.clicked() {
                            clicked = UserAction::Confirm;
                        }
                        let cancel_resp = ui.add(cancel);
                        // 默认焦点在「取消」：回车 / 空格落在取消上。
                        if ui.memory(|m| m.focused().is_none()) {
                            cancel_resp.request_focus();
                        }
                        if cancel_resp.clicked() {
                            clicked = UserAction::Cancel;
                        }
                    });
                });
        });

    if action == UserAction::None {
        action = clicked;
    }
    DialogResponse { action }
}

/// 用户在确认框上的动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserAction {
    /// 什么都没点（框只是开着）。
    None,
    /// 点「移到废纸篓」。
    Confirm,
    /// 点「取消」。
    Cancel,
    /// Escape 或点窗口的关闭叉。
    Dismiss,
}

/// 这一帧应该发生什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// 继续等用户：范围没变，框留着。
    Keep,
    /// 范围变了：作废并**重新确认**（用新范围重画），不得用旧张数执行。
    Rearm(TrashScope),
    /// 执行这一组（并且只执行这一组）。空集永不出现。
    Execute(Vec<i64>),
    /// 关掉框，零执行。
    Close,
}

/// 决策表。
///
/// `confirmed` = 弹框那一刻固化的集合；`current` = 此刻的真实范围。
pub fn decide(action: UserAction, confirmed: &TrashScope, current: &TrashScope) -> Decision {
    // 取消 / Escape / 关窗：无条件零执行，跟范围有没有变无关。
    if matches!(action, UserAction::Cancel | UserAction::Dismiss) {
        return Decision::Close;
    }

    let still_same = confirmed.matches(current);

    if action == UserAction::Confirm {
        // 只有「范围仍是确认过的那一组、且非空」才允许执行。
        if still_same && !current.is_empty() {
            return Decision::Execute(current.ids().to_vec());
        }
        // 否则作废：重新确认，或者（范围已经空了）直接关掉。
        return if current.is_empty() {
            Decision::Close
        } else {
            Decision::Rearm(current.clone())
        };
    }

    // 还没点：范围一变就立刻作废重画，别让用户对着一个已经不成立的张数点确认。
    if still_same {
        Decision::Keep
    } else if current.is_empty() {
        Decision::Close
    } else {
        Decision::Rearm(current.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(v: &[i64]) -> TrashScope {
        TrashScope::new(v.iter().copied())
    }

    /// 无窗口跑一帧确认框：返回（响应、所有文字及其矩形、所有矩形）。
    fn run_dialog(
        ctx: &egui::Context,
        theme: &Theme,
        screen: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> (DialogResponse, Vec<(String, egui::Rect)>, Vec<egui::Rect>) {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            max_texture_side: Some(2048),
            ..Default::default()
        };
        input.events = events;
        let mut resp = DialogResponse::default();
        // Area 的首帧只做布局、不产出形状；先跑一帧预热再用同一输入跑正式帧。
        let _warm = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
                max_texture_side: Some(2048),
                ..Default::default()
            },
            |_ui| {
                let _ = show_dialog(
                    ctx,
                    theme,
                    1,
                    &["/home/me/Pictures/2024/5/07-city.jpg".to_string()],
                );
            },
        );
        _warm.drop_without_applying_deltas();
        let out = ctx.run_ui(input, |_ui| {
            resp = show_dialog(
                ctx,
                theme,
                1,
                &["/home/me/Pictures/2024/5/07-city.jpg".to_string()],
            );
        });
        let mut texts = Vec::new();
        let mut rects = Vec::new();
        fn walk(
            shape: &egui::Shape,
            texts: &mut Vec<(String, egui::Rect)>,
            rects: &mut Vec<egui::Rect>,
        ) {
            match shape {
                egui::Shape::Vec(v) => {
                    for s in v {
                        walk(s, texts, rects);
                    }
                }
                egui::Shape::Text(t) => texts.push((
                    t.galley.job.text.clone(),
                    egui::Rect::from_min_size(t.pos, t.galley.size()),
                )),
                egui::Shape::Rect(r) => rects.push(r.rect),
                _ => {}
            }
        }
        for c in &out.shapes {
            walk(&c.shape, &mut texts, &mut rects);
        }
        out.drop_without_applying_deltas();
        (resp, texts, rects)
    }

    fn dialog_ctx(appearance: picasa_core::Appearance) -> (egui::Context, Theme) {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx, appearance);
        (ctx, Theme::for_appearance(appearance))
    }

    fn press_release(rect: egui::Rect, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: rect.center(),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }

    /// 原型 v1.4：确认框是按**逻辑像素比例**取宽的居中卡片，不是 420x168 横条。
    #[test]
    fn dialog_width_follows_logical_pixels_not_screenshot_pixels() {
        // 原型窗口是 1280 逻辑宽（2560 物理 / DPR 2），卡片实测约 417 逻辑宽
        // （约 33%）。这里 1280 → 422.4，与原型同一比例。
        assert!(
            (dialog_width(1280.0) - 422.4).abs() < 0.5,
            "{}",
            dialog_width(1280.0)
        );
        assert_eq!(dialog_width(1024.0), 380.0, "1024 逻辑宽应取到下限");
        assert_eq!(dialog_width(1600.0), 520.0, "超宽窗口封顶");
        assert_eq!(dialog_width(800.0), 380.0, "窄窗仍不小于下限");
        // 逻辑像素比例，不是把 1280 截图的物理像素塞进 1024 逻辑坐标：
        // 若照搬 417 物理像素宽，1024 窗里比例会明显偏离原型。
        assert!(
            dialog_width(1024.0) / 1024.0 < 0.45,
            "卡片不应占掉近半窗口：{}",
            dialog_width(1024.0)
        );
    }

    /// 对话框结构：居中大圆角卡片、标题在左上、正文分段 + 真实路径、
    /// 按钮在底部右对齐（描边取消在左、危险红主按钮在右），且有背景遮罩。
    #[test]
    fn dialog_is_centered_with_title_left_and_buttons_right_aligned() {
        for appearance in [
            picasa_core::Appearance::Light,
            picasa_core::Appearance::Dark,
        ] {
            let (ctx, theme) = dialog_ctx(appearance);
            let screen = egui::vec2(1280.0, 800.0);
            let (_r, texts, rects) = run_dialog(&ctx, &theme, screen, Vec::new());
            let find = |needle: &str| {
                texts
                    .iter()
                    .find(|(t, _)| t == needle)
                    .map(|(_, r)| *r)
                    .unwrap_or_else(|| panic!("{appearance:?}：缺「{needle}」"))
            };
            let title = find(picasa_core::trash_plan::TRASH_TITLE);
            let cancel = find(picasa_core::trash_plan::TRASH_CANCEL);
            let confirm = find(picasa_core::trash_plan::TRASH_ACTION);
            // 标题在正文之上；标题左对齐（不在卡片中线上）
            assert!(title.top() < cancel.top(), "标题应在按钮之上");
            // 正文分段：还原说明与真实路径都画出来了
            let all: String = texts
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(all.contains("张照片移到"), "缺少正文：{all}");
            assert!(all.contains("还原"), "缺少还原说明：{all}");
            assert!(
                all.contains("/home/me/Pictures/2024/5/07-city.jpg"),
                "缺少真实路径：{all}"
            );
            // 不复制演示文案
            assert!(!all.contains("仅模拟"), "{all}");
            assert!(!all.contains("不移动真实照片"), "{all}");
            // 按钮右对齐：两枚都在卡片右半，取消在确认左侧，且互不重叠。
            assert!(cancel.right() < confirm.left(), "取消应在确认左侧");
            assert!(
                confirm.right() > screen.x * 0.5,
                "按钮组应在右半区：{confirm:?}"
            );
            // 有大背景遮罩：存在一个几乎铺满屏幕的矩形。
            assert!(
                rects
                    .iter()
                    .any(|r| r.width() >= screen.x - 1.0 && r.height() >= screen.y - 1.0),
                "{appearance:?}：应有铺满窗口的遮罩"
            );
        }
    }

    /// 取消 / Escape / 点遮罩都零执行；点确认才是 Confirm。
    #[test]
    fn dialog_escape_and_scrim_dismiss_without_side_effects() {
        let screen = egui::vec2(1024.0, 800.0);
        for appearance in [
            picasa_core::Appearance::Light,
            picasa_core::Appearance::Dark,
        ] {
            let (ctx, theme) = dialog_ctx(appearance);
            // Escape
            let (r, _, _) = run_dialog(
                &ctx,
                &theme,
                screen,
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
            );
            assert_eq!(
                r.action,
                UserAction::Dismiss,
                "{appearance:?} Escape 应取消"
            );

            // 点遮罩左上角（对话框之外）
            let (ctx, theme) = dialog_ctx(appearance);
            let (_, _, _) = run_dialog(&ctx, &theme, screen, Vec::new());
            let scrim_pos = egui::pos2(4.0, 4.0);
            let mut events = vec![egui::Event::PointerMoved(scrim_pos)];
            events.push(egui::Event::PointerButton {
                pos: scrim_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            });
            let (_, _, _) = run_dialog(&ctx, &theme, screen, events);
            let (r, _, _) = run_dialog(
                &ctx,
                &theme,
                screen,
                vec![egui::Event::PointerButton {
                    pos: scrim_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            );
            assert_eq!(r.action, UserAction::Dismiss, "{appearance:?} 点遮罩应取消");
        }
    }

    /// 默认焦点在「取消」：框一打开就按回车 / 空格，绝不误触删除。
    ///
    /// 这是 `M5-FS-01` 的安全默认：移动原文件必须再主动点一次确认按钮。
    #[test]
    fn dialog_defaults_focus_to_cancel_so_enter_never_trashes() {
        for appearance in [
            picasa_core::Appearance::Light,
            picasa_core::Appearance::Dark,
        ] {
            let (ctx, theme) = dialog_ctx(appearance);
            let screen = egui::vec2(1024.0, 800.0);
            let theme_paths = ["/home/me/Pictures/2024/5/07-city.jpg".to_string()];
            // 多跑几帧让 request_focus 生效（egui 的焦点在下一个 pass 应用）。
            let mut last = UserAction::None;
            for _ in 0..3 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
                    max_texture_side: Some(2048),
                    ..Default::default()
                };
                let out = ctx.run_ui(input, |_ui| {
                    last = show_dialog(&ctx, &theme, 1, &theme_paths).action;
                });
                out.drop_without_applying_deltas();
            }
            assert_eq!(last, UserAction::None, "{appearance:?}：静置时不应有动作");
            // 现在按回车：焦点在「取消」→ Cancel，而不是 Confirm。
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
                max_texture_side: Some(2048),
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
                ..Default::default()
            };
            let mut action = UserAction::None;
            let out = ctx.run_ui(input, |_ui| {
                action = show_dialog(&ctx, &theme, 1, &theme_paths).action;
            });
            out.drop_without_applying_deltas();
            assert_eq!(
                action,
                UserAction::Cancel,
                "{appearance:?}：默认焦点应在「取消」，回车不得确认删除"
            );
        }
    }

    /// 点「移到废纸篓」→ Confirm；点「取消」→ Cancel。
    #[test]
    fn dialog_buttons_emit_confirm_and_cancel() {
        for appearance in [
            picasa_core::Appearance::Light,
            picasa_core::Appearance::Dark,
        ] {
            for (label, expect) in [
                (picasa_core::trash_plan::TRASH_ACTION, UserAction::Confirm),
                (picasa_core::trash_plan::TRASH_CANCEL, UserAction::Cancel),
            ] {
                let (ctx, theme) = dialog_ctx(appearance);
                let screen = egui::vec2(1024.0, 800.0);
                let (_r, texts, _) = run_dialog(&ctx, &theme, screen, Vec::new());
                let target = texts
                    .iter()
                    .find(|(t, _)| t == label)
                    .map(|(_, r)| *r)
                    .unwrap_or_else(|| panic!("缺「{label}」"));
                let mut events = vec![egui::Event::PointerMoved(target.center())];
                events.push(press_release(target, true));
                let (_, _, _) = run_dialog(&ctx, &theme, screen, events);
                let (r, _, _) =
                    run_dialog(&ctx, &theme, screen, vec![press_release(target, false)]);
                assert_eq!(r.action, expect, "{appearance:?} 点「{label}」");
            }
        }
    }

    /// T2：确认框里「移到废纸篓」是红色主按钮（实心危险红 + 白字），
    /// 「取消」是普通次按钮（默认底、不是危险色）。
    #[test]
    fn t2_confirm_is_danger_primary_and_cancel_is_secondary() {
        use crate::theme::{Theme, apply};
        let ctx = egui::Context::default();
        apply(&ctx, picasa_core::Appearance::Light);
        let theme = Theme::for_appearance(picasa_core::Appearance::Light);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(420.0, 168.0),
            )),
            max_texture_side: Some(2048),
            ..Default::default()
        };
        let out = ctx.run_ui(input, |ui| {
            let (confirm, cancel) = confirm_buttons(&theme);
            ui.add(confirm);
            ui.add(cancel);
        });
        // 收集（按钮底矩形, 文字位置）两类形状。
        let mut rects: Vec<(egui::Rect, egui::Color32)> = Vec::new();
        let mut strokes: Vec<(egui::Rect, egui::Stroke)> = Vec::new();
        let mut texts: Vec<(String, egui::Rect)> = Vec::new();
        fn walk(
            shape: &egui::Shape,
            rects: &mut Vec<(egui::Rect, egui::Color32)>,
            strokes: &mut Vec<(egui::Rect, egui::Stroke)>,
            texts: &mut Vec<(String, egui::Rect)>,
        ) {
            match shape {
                egui::Shape::Vec(v) => {
                    for s in v {
                        walk(s, rects, strokes, texts);
                    }
                }
                egui::Shape::Rect(r) => {
                    if r.fill != egui::Color32::TRANSPARENT {
                        rects.push((r.rect, r.fill));
                    }
                    if r.stroke.width > 0.0 {
                        strokes.push((r.rect, r.stroke));
                    }
                }
                egui::Shape::Text(t) => {
                    texts.push((
                        t.galley.job.text.clone(),
                        egui::Rect::from_min_size(t.pos, t.galley.size()),
                    ));
                }
                _ => {}
            }
        }
        for c in &out.shapes {
            walk(&c.shape, &mut rects, &mut strokes, &mut texts);
        }
        out.drop_without_applying_deltas();

        let confirm_text = texts
            .iter()
            .find(|(t, _)| t == picasa_core::trash_plan::TRASH_ACTION)
            .expect("确认框要有「移到废纸篓」");
        let cancel_text = texts
            .iter()
            .find(|(t, _)| t == picasa_core::trash_plan::TRASH_CANCEL)
            .expect("确认框要有「取消」");
        let danger_rect = rects
            .iter()
            .find(|(_, fill)| *fill == theme.danger)
            .expect("「移到废纸篓」应有危险红实心底");
        assert!(
            danger_rect.0.contains_rect(confirm_text.1),
            "危险红底应包住「移到废纸篓」文字：{danger_rect:?} vs {:?}",
            confirm_text.1
        );
        // 取消按钮：次按钮 = 卡片底 + 可见描边，且绝不是危险红
        let cancel_bg = rects
            .iter()
            .find(|(r, _)| r.contains_rect(cancel_text.1))
            .map(|(_, f)| *f);
        assert_ne!(cancel_bg, Some(theme.danger), "「取消」不能是危险红主按钮");
        assert_eq!(
            cancel_bg,
            Some(theme.viewer_card),
            "「取消」应是卡片底 + 描边"
        );
        let cancel_strokes: Vec<egui::Stroke> = strokes
            .iter()
            .filter(|(r, _)| r.contains_rect(cancel_text.1))
            .map(|(_, s)| *s)
            .collect();
        // 原型里的「取消」是明确可见的描边按钮（不是发丝线，也不是实心主按钮）。
        assert!(
            cancel_strokes
                .iter()
                .any(|s| s.color == theme.text_secondary && s.width >= 1.0),
            "「取消」应有可见描边（次按钮）：{cancel_strokes:?}"
        );
    }

    /// `AC-FS-CONFIRM`：取消不执行；Escape / 关闭同样零执行。
    #[test]
    fn cancel_escape_and_dismiss_never_execute_anything() {
        let confirmed = scope(&[1, 2, 3]);
        let current = scope(&[1, 2, 3]);
        for a in [UserAction::Cancel, UserAction::Dismiss] {
            assert_eq!(decide(a, &confirmed, &current), Decision::Close, "{a:?}");
        }
        // 范围已经变了也照样零执行（取消优先于一切）。
        let changed = scope(&[9]);
        for a in [UserAction::Cancel, UserAction::Dismiss] {
            assert_eq!(decide(a, &confirmed, &changed), Decision::Close, "{a:?}");
        }
    }

    /// `AC-FS-CONFIRM`：确认后只执行确认过的那一组。
    #[test]
    fn confirm_executes_exactly_the_confirmed_set() {
        let confirmed = scope(&[4, 5, 6]);
        assert_eq!(
            decide(UserAction::Confirm, &confirmed, &scope(&[4, 5, 6])),
            Decision::Execute(vec![4, 5, 6])
        );
    }

    /// `AC-FS-CONFIRM`：确认框打开后范围变化，必须作废并重新确认。
    #[test]
    fn changing_the_scope_while_open_voids_the_old_confirmation() {
        let confirmed = scope(&[1, 2, 3]);
        // 加一张
        assert_eq!(
            decide(UserAction::None, &confirmed, &scope(&[1, 2, 3, 4])),
            Decision::Rearm(scope(&[1, 2, 3, 4]))
        );
        // 换一张（张数相同也必须作废）
        assert_eq!(
            decide(UserAction::None, &confirmed, &scope(&[1, 2, 9])),
            Decision::Rearm(scope(&[1, 2, 9]))
        );
        // 已经变了还敢点确认：不得用旧张数执行，改为重新确认。
        assert_eq!(
            decide(UserAction::Confirm, &confirmed, &scope(&[7])),
            Decision::Rearm(scope(&[7]))
        );
    }

    /// 范围被清空（例如切视图）时确认框直接关掉，不执行空集。
    #[test]
    fn emptying_the_scope_closes_without_executing() {
        let confirmed = scope(&[1, 2]);
        assert_eq!(
            decide(UserAction::None, &confirmed, &scope(&[])),
            Decision::Close
        );
        assert_eq!(
            decide(UserAction::Confirm, &confirmed, &scope(&[])),
            Decision::Close
        );
    }

    /// 张数没变、范围没变 → 什么都不做，框留着。
    #[test]
    fn unchanged_scope_keeps_the_dialog_open() {
        let confirmed = scope(&[10, 11]);
        assert_eq!(
            decide(UserAction::None, &confirmed, &scope(&[10, 11])),
            Decision::Keep
        );
    }
}
