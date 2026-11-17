//! 「移到废纸篓」的**确认范围**（M5-FS-01 / M5-VW-15 / M5-KB-14）。
//!
//! ## 为什么单独一个模块
//!
//! 审计缺陷是「确认框写 1 张、执行整个 `selection`」。根因不是某个按钮写错数字，
//! 而是**确认与执行之间没有共享同一个集合**：确认时读一次 `selection.len()`，
//! 执行时又照当时的 `selection` 重新算一遍。中间只要有一次范围变化（翻页、
//! 改视图、框选、⌘D），两者就会分叉，而分叉的表现是「用户确认了 1 张，删掉了 9 张」。
//!
//! 这里的做法是把「即将被移动的那组照片」在**弹框那一刻**固化成 [`TrashScope`]：
//!
//! 1. 确认文案里的张数 = `scope.len()`，只此一个来源；
//! 2. 执行时拿同一个 `scope` 去比对当前范围，**不一致就不执行**（作废重确认）；
//! 3. 执行集合就是 `scope.ids`，不再回头读 `selection`。
//!
//! 三条合起来，「确认张数 = 执行集合」在结构上成立，不靠调用方自觉。

/// 确认框标题（`M5-FS-01`：唯一会移动原文件的用户动作）。
pub const TRASH_TITLE: &str = "移到废纸篓？";
/// 取消按钮（默认焦点）。
pub const TRASH_CANCEL: &str = "取消";
/// 执行按钮。
pub const TRASH_ACTION: &str = "移到废纸篓";

/// 确认框正文。`n` 必须等于执行集合大小。
pub fn confirm_body(n: usize) -> String {
    format!(
        "将把 {n} 张照片移到系统废纸篓。\n\n可以在 Finder 的废纸篓里还原，我们不会直接删除文件。"
    )
}

/// 范围在执行前变了：当前确认作废，必须重新确认。
pub const SCOPE_CHANGED: &str =
    "选择或范围已变化，这次确认已作废（没有移动任何文件）。请重新确认。";

/// 一次「移到废纸篓」确认所固定的照片集合。
///
/// `ids` **有序去重**：两边的比较就是逐个 `id`，顺序与重复都会让比较失败——
/// 这正合适，「同一个集合」的充要条件就是排序去重后逐项相等。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrashScope {
    ids: Vec<i64>,
}

impl TrashScope {
    /// 从任意 id 流构造（自动去重 + 升序）。空集是合法的，但调用方不该弹确认框。
    pub fn new(ids: impl IntoIterator<Item = i64>) -> Self {
        let mut ids: Vec<i64> = ids.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        Self { ids }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn ids(&self) -> &[i64] {
        &self.ids
    }

    /// 当前范围是否仍与确认时**完全一致**。只有一致才允许执行。
    pub fn matches(&self, current: &Self) -> bool {
        self == current
    }
}

/// 查看器里的目标集合（`M5-VW-15`）。
///
/// 规则只有一条：确认张数 = 执行集合。
/// - 选择集非空 → 执行整个选择集，张数就是它的真实大小（**不是恒为 1**）；
/// - 选择集为空（只看不选）→ 只处理当前可见的那一张，张数为 1，也只移动那一张。
///
/// 两条分支都由这一个函数决定，UI 不再自己算数字。
pub fn viewer_scope(selection: &[i64], visible: Option<i64>) -> TrashScope {
    if !selection.is_empty() {
        return TrashScope::new(selection.iter().copied());
    }
    TrashScope::new(visible)
}

/// 图库里的目标集合：就是当前选择集（`M5-KB-14`：有选中才弹确认）。
pub fn library_scope(selection: &[i64]) -> TrashScope {
    TrashScope::new(selection.iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_uses_the_exact_count_and_says_files_are_recoverable() {
        let body = confirm_body(3);
        assert!(body.starts_with("将把 3 张照片移到系统废纸篓。"), "{body}");
        assert!(body.contains("可以在 Finder 的废纸篓里还原"), "{body}");
        assert!(body.contains("我们不会直接删除文件"), "{body}");
        // 不能出现「只删索引」一类说法——那是另一条路径（M5-FS-02）。
        assert!(!body.contains("索引"), "{body}");
        assert_eq!(TRASH_TITLE, "移到废纸篓？");
    }

    #[test]
    fn scope_is_sorted_and_deduped_so_comparison_is_set_equality() {
        let a = TrashScope::new([9, 3, 3, 7]);
        let b = TrashScope::new([3, 7, 9]);
        assert_eq!(a.ids(), &[3, 7, 9]);
        assert_eq!(a.len(), 3);
        assert!(a.matches(&b));
    }

    #[test]
    fn any_change_of_the_set_invalidates_the_confirmation() {
        let confirmed = TrashScope::new([1, 2, 3]);
        // 加一张
        assert!(!confirmed.matches(&TrashScope::new([1, 2, 3, 4])));
        // 少一张
        assert!(!confirmed.matches(&TrashScope::new([1, 2])));
        // 换一张（张数相同也必须作废）
        assert!(!confirmed.matches(&TrashScope::new([1, 2, 9])));
        // 清空
        assert!(!confirmed.matches(&TrashScope::new([])));
    }

    #[test]
    fn viewer_with_multi_selection_confirms_the_whole_selection_not_one() {
        // 可见第 5 张，但选择集有 3 张：确认 3 张，执行这 3 张（不是 1 张）。
        let scope = viewer_scope(&[11, 12, 13], Some(50));
        assert_eq!(scope.len(), 3);
        assert_eq!(scope.ids(), &[11, 12, 13]);
        // 确认文案的张数与执行集合同源。
        assert!(confirm_body(scope.len()).starts_with("将把 3 张"));
    }

    #[test]
    fn viewer_without_selection_targets_only_the_visible_photo() {
        let scope = viewer_scope(&[], Some(50));
        assert_eq!(scope.len(), 1);
        assert_eq!(scope.ids(), &[50]);
    }

    #[test]
    fn viewer_with_empty_selection_and_no_visible_photo_has_empty_scope() {
        // 空集：UI 不应弹确认框（无选中不弹废纸篓）。
        assert!(viewer_scope(&[], None).is_empty());
    }

    #[test]
    fn library_scope_is_the_selection_and_dedupes() {
        let scope = library_scope(&[5, 5, 2]);
        assert_eq!(scope.len(), 2);
        assert!(confirm_body(scope.len()).starts_with("将把 2 张"));
    }
}
