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
    let cancel = egui::Button::new(picasa_core::trash_plan::TRASH_CANCEL);
    let confirm = egui::Button::new(
        egui::RichText::new(picasa_core::trash_plan::TRASH_ACTION).color(egui::Color32::WHITE),
    )
    .fill(theme.danger)
    .corner_radius(theme.radius_btn);
    (confirm, cancel)
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
        let mut texts: Vec<(String, egui::Rect)> = Vec::new();
        fn walk(shape: &egui::Shape, rects: &mut Vec<(egui::Rect, egui::Color32)>, texts: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Vec(v) => {
                    for s in v {
                        walk(s, rects, texts);
                    }
                }
                egui::Shape::Rect(r) => {
                    if r.fill != egui::Color32::TRANSPARENT {
                        rects.push((r.rect, r.fill));
                    }
                }
                egui::Shape::Text(t) => {
                    texts.push((t.galley.job.text.clone(), egui::Rect::from_min_size(t.pos, t.galley.size())));
                }
                _ => {}
            }
        }
        for c in &out.shapes {
            walk(&c.shape, &mut rects, &mut texts);
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
        // 取消按钮的底不是危险红（次按钮，沿用默认样式）
        let cancel_bg = rects
            .iter()
            .find(|(r, _)| r.contains_rect(cancel_text.1));
        assert!(
            cancel_bg.map(|(_, f)| *f) != Some(theme.danger),
            "「取消」不能是危险红主按钮"
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
