//! M5 本轮必须改准的**文案与危险确认标题**（集中一处，好测也好对齐设计）。
//!
//! 为什么抽成模块：这几句每句都对应一条验收（`AC-ABOUT`、`AC-XX-COPY`、
//! `AC-FS-INDEXONLY`），而它们原本散在 `app.rs` 的几个 `match` 分支里——
//! 散着写就没法用单元测试钉住「不许再出现旧全称承诺」这种要求。
//!
//! 这里只放**已经定死的**文案。§11 #8（关于入口的最终字句）还没裁定，
//! 所以 [`about_text`] 只表达 M5-MN-05 规定的准确含义，不冒充品牌口号。

/// 「关于 OpenPicasa」的说明（toast 形式，`M5-MN-05` / `AC-ABOUT`）。
///
/// 三件事必须能区分开，缺一件就又变回误导：
/// 1. 组织动作（收藏 / 说明 / 标签 / 相册 / 索引管理）**只写独立索引**；
/// 2. 这些动作**不向照片目录写辅助文件、不移动原图**；
/// 3. 用户显式「移到废纸篓」**会移动原图**（到系统废纸篓，可还原）。
pub fn about_text(version: &str) -> String {
    format!(
        "OpenPicasa {version} · 收藏、说明、标签、相册与索引管理只写独立索引，\
         不向照片目录写辅助文件、不移动原图；\
         只有你显式「移到废纸篓」才会把原图移到系统废纸篓。"
    )
}

/// 关闭子文件夹后的 toast（`M5-SB-12` / `M5-XX-07`）。
///
/// 恢复入口是「索引管理 → 排除项」。旧句「可在诊断里恢复」是死路：
/// 诊断窗在 M5 是 UNREACHABLE，用户照它去找只会扑空。
pub const CLOSE_FOLDER_TOAST: &str = "已关闭，不再显示这个文件夹。\
                                      需要恢复时去「索引管理 → 排除项」点「恢复」。";

/// 在根目录上点「关闭此文件夹」时的 toast（行为不变，文案保留）。
pub const CLOSE_ROOT_REFUSED_TOAST: &str = "根目录不能这样关闭";

/// ① 单根彻底删除的确认框标题与确认钮（`M5-IDX-06` ①）。
pub const PURGE_ROOT_TITLE: &str = "确认删除索引";
pub const PURGE_ROOT_BUTTON: &str = "确认";

/// ② 索引进度里全库「删除索引」的确认框标题与确认钮（`M5-IDX-06` ②）。
///
/// 按钮字必须与标题一致，**不能**也用「确认」——那会和 ① 长得一样，
/// 用户就分不清自己删的是「一个根」还是「全库」。
pub const PURGE_ALL_TITLE: &str = "删除索引";
pub const PURGE_ALL_BUTTON: &str = "删除索引";

/// ③ 左栏「从图库移除」是**无确认**的只删索引路径（`M5-IDX-06` ③ / `M5-SB-11`）。
pub const REMOVE_ROOT_LABEL: &str = "从图库移除（删索引，不删文件）";

/// 导入来源移除确认的按钮字（`M5-SB-15`：确认钮「移除」，默认焦点在取消）。
pub const IMPORT_SOURCE_BUTTON: &str = "移除";

/// 三套只删索引的确认 UI 必须**互不相同**——这条规则本身也要可判定，
/// 于是把「标题 + 按钮」组合抽成一个值来比。
pub fn index_delete_dialog(kind: IndexDeleteKind) -> (&'static str, &'static str) {
    match kind {
        IndexDeleteKind::PurgeRoot => (PURGE_ROOT_TITLE, PURGE_ROOT_BUTTON),
        IndexDeleteKind::PurgeAll => (PURGE_ALL_TITLE, PURGE_ALL_BUTTON),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexDeleteKind {
    PurgeRoot,
    PurgeAll,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `AC-ABOUT`：不得再出现旧的「只读不改原文件」全称承诺，
    /// 且必须能区分「组织动作」与「废纸篓」。
    #[test]
    fn about_copy_no_longer_promises_read_only_never_touches_originals() {
        let t = about_text("0.4.2-beta.1");
        for banned in [
            "只读不改原文件",
            "所有操作都不改原图",
            "所有操作都不动原图",
            "不改原图",
            "只读，不会修改你的文件",
            "只读",
        ] {
            assert!(!t.contains(banned), "关于文案仍在承诺「{banned}」：{t}");
        }
        // 三条准确含义都要说出来。
        assert!(t.contains("只写独立索引"), "{t}");
        assert!(t.contains("不向照片目录写辅助文件"), "{t}");
        assert!(t.contains("不移动原图"), "{t}");
        assert!(t.contains("移到废纸篓"), "{t}");
        assert!(t.contains("原图移到系统废纸篓"), "{t}");
    }

    /// `AC-XX-COPY`：关闭子文件夹的恢复路径指向索引管理「排除项」。
    #[test]
    fn close_folder_toast_points_at_index_manager_exclusions_not_diagnostics() {
        assert!(CLOSE_FOLDER_TOAST.contains("排除项"), "{CLOSE_FOLDER_TOAST}");
        assert!(CLOSE_FOLDER_TOAST.contains("索引管理"), "{CLOSE_FOLDER_TOAST}");
        assert!(
            !CLOSE_FOLDER_TOAST.contains("诊断"),
            "不得再指向诊断窗（M5 不可达）：{CLOSE_FOLDER_TOAST}"
        );
        assert_eq!(CLOSE_ROOT_REFUSED_TOAST, "根目录不能这样关闭");
    }

    /// `AC-FS-INDEXONLY`：三套只删索引的标题/按钮字互不相同，且都不叫「移到废纸篓」。
    #[test]
    fn the_index_only_deletes_all_have_distinct_dialog_wording() {
        let (root_title, root_btn) = index_delete_dialog(IndexDeleteKind::PurgeRoot);
        let (all_title, all_btn) = index_delete_dialog(IndexDeleteKind::PurgeAll);
        assert_ne!(root_title, all_title);
        assert_ne!(root_btn, all_btn);
        assert_eq!(root_btn, "确认");
        assert_eq!(all_btn, "删除索引");
        for s in [root_title, root_btn, all_title, all_btn, IMPORT_SOURCE_BUTTON] {
            assert_ne!(s, "移到废纸篓", "只删索引不能与废纸篓共用按钮字");
        }
        // ③ 左栏移除是无确认路径，其入口文案必须写明「删索引，不删文件」。
        assert!(REMOVE_ROOT_LABEL.contains("删索引"));
        assert!(REMOVE_ROOT_LABEL.contains("不删文件"));
        // ① 彻底删除后不能再说「可重新索引恢复」。
        assert!(!PURGE_ROOT_TITLE.contains("恢复"));
    }
}
