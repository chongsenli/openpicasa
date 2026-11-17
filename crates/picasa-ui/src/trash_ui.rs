//! 「移到废纸篓」确认框的**决策表**（`M5-FS-01` / `M5-VW-15` / `M5-KB-14`）。
//!
//! `app.rs` 里的弹框每帧只做一件事：把「用户这一帧做了什么」和「此刻的真实范围」
//! 交给 [`decide`]，然后照返回的动作执行。把这张表抽出来的唯一理由是——
//! 「取消零执行」「范围变化作废」必须能被单元测试钉住，而它们写在 egui 的
//! 闭包里就只能靠实机点击验证。

use picasa_core::TrashScope;

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
