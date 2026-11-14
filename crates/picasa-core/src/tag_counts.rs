//! M4.3 / IDX-22：左栏标签区的排序与可见性（纯函数，**不碰数据库**）。
//!
//! 规格（docs/18 §IDX-22 ④⑥，P-M43-11 已裁决）：
//! - 默认**按名称**排序（不区分大小写；中文按 Unicode 码位，拼音排序可行性未核实）；
//! - 组头菜单可切「按数量」：数量降序，同数再按名称；
//! - **0 计数默认隐藏**，「显示空标签」可切换。
//!
//! 放在这里而不是塞进 UI：这三件事都是纯数据变换，能单测、能在后台线程算，
//! UI 帧只负责画。计数本身仍来自 `Db::all_tags()`（单条聚合）。

use serde::{Deserialize, Serialize};

/// 标签列表的排序方式（IDX-22 ④，默认按名称）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TagsSort {
    /// 按名称（不区分大小写）。默认值——P-M43-11 把旧行为（按数量降序）改掉了。
    #[default]
    Name,
    /// 按数量降序，同数按名称。
    Count,
}

impl TagsSort {
    pub fn as_pref_string(self) -> &'static str {
        match self {
            TagsSort::Name => "name",
            TagsSort::Count => "count",
        }
    }

    pub fn from_pref_string(s: &str) -> Option<Self> {
        match s {
            "name" => Some(TagsSort::Name),
            "count" => Some(TagsSort::Count),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TagsSort::Name => "按名称",
            TagsSort::Count => "按数量",
        }
    }

    pub fn flip(self) -> Self {
        match self {
            TagsSort::Name => TagsSort::Count,
            TagsSort::Count => TagsSort::Name,
        }
    }
}

/// 名称比较键：不区分大小写。中文按 Unicode 码位（`to_lowercase` 对中文是恒等）。
fn name_key(label: &str) -> String {
    label.to_lowercase()
}

/// 把 `(label, count)` 列表按 `sort` 排序并（默认）滤掉 0 计数。
///
/// `items` 可以是任意顺序——这个函数**不改调用方数据**，返回新 `Vec`。
/// 同名的多条会保留（理论上 `all_tags()` 已按 label 分组，这里不合并，
/// 免得把「计数口径」偷偷改掉）。
pub fn sort_tag_counts(
    items: &[(String, i64)],
    sort: TagsSort,
    show_zero: bool,
) -> Vec<(String, i64)> {
    let mut out: Vec<(String, i64)> = items
        .iter()
        .filter(|(_, n)| show_zero || *n > 0)
        .cloned()
        .collect();
    match sort {
        // `sort_by_cached_key`：比较时只算一次小写键（标签数量级不大，但别每比较都分配）
        TagsSort::Name => out.sort_by_cached_key(|(label, _)| name_key(label)),
        TagsSort::Count => out.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| name_key(&a.0).cmp(&name_key(&b.0)))
        }),
    }
    out
}

/// 名称在左栏一行里放不下时的截断：按**字符**（不是字节）截，末尾留 `…`。
///
/// 悬停要给全名，所以这里只处理「画出来的那一段」。
pub fn truncate_label(label: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut it = label.chars();
    let head: String = it.by_ref().take(max_chars).collect();
    if it.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, n: i64) -> (String, i64) {
        (label.to_string(), n)
    }

    #[test]
    fn default_sort_is_by_name_case_insensitive() {
        assert_eq!(TagsSort::default(), TagsSort::Name);
        let items = vec![item("beta", 9), item("Alpha", 2), item("gamma", 5)];
        let sorted = sort_tag_counts(&items, TagsSort::Name, true);
        assert_eq!(
            sorted.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
            vec!["Alpha", "beta", "gamma"],
            "默认必须按名称、且不区分大小写"
        );
    }

    #[test]
    fn count_sort_is_descending_then_by_name() {
        let items = vec![
            item("b", 3),
            item("a", 3),
            item("c", 10),
            item("A", 1),
        ];
        let sorted = sort_tag_counts(&items, TagsSort::Count, true);
        assert_eq!(
            sorted.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
            vec!["c", "a", "b", "A"],
            "数量降序；同数按名称（不区分大小写）"
        );
    }

    #[test]
    fn zero_counts_hidden_by_default_but_kept_when_asked() {
        let items = vec![item("used", 4), item("empty", 0), item("negative", -1)];
        let hidden = sort_tag_counts(&items, TagsSort::Name, false);
        assert_eq!(
            hidden.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
            vec!["used"],
            "0 与负计数默认隐藏"
        );
        let shown = sort_tag_counts(&items, TagsSort::Name, true);
        assert_eq!(shown.len(), 3, "打开「显示空标签」后一条不丢");
    }

    #[test]
    fn sort_is_stable_on_empty_input() {
        assert!(sort_tag_counts(&[], TagsSort::Name, true).is_empty());
        assert!(sort_tag_counts(&[], TagsSort::Count, false).is_empty());
    }

    #[test]
    fn truncation_counts_chars_not_bytes() {
        assert_eq!(truncate_label("京都旅行", 4), "京都旅行", "刚好放下不加省略号");
        assert_eq!(truncate_label("京都旅行", 3), "京都旅…");
        // 字符边界：4 个汉字 = 12 字节，按字节截会切坏 UTF-8。
        assert_eq!(truncate_label("一二三四", 2), "一二…");
        assert_eq!(truncate_label("abc", 0), "");
    }

    #[test]
    fn pref_string_roundtrips_and_flip_toggles() {
        for s in [TagsSort::Name, TagsSort::Count] {
            assert_eq!(TagsSort::from_pref_string(s.as_pref_string()), Some(s));
        }
        assert_eq!(TagsSort::from_pref_string("pinyin"), None);
        assert_eq!(TagsSort::Name.flip(), TagsSort::Count);
        assert_eq!(TagsSort::Count.flip(), TagsSort::Name);
    }
}
