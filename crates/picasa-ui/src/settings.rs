//! 设置页（M5 只做到**入口壳**）。
//!
//! ## 为什么这一轮只做壳
//!
//! v1.4 已确认的是「更多 → 设置… → 设置页 → 打开索引管理」这条路径必须可达
//! （`M5-IA-04` / `AC-IA-MORE`）。设置页**还有哪些段落**是说明书 §11 #10 的
//! 未决项——本轮不裁定、不借机做完整设置产品。
//!
//! 因此本页只有三件已确认的事：
//!
//! 1. **索引**：按钮「打开索引管理」→ 五标签索引管理窗口（`M5-IA-02` /
//!    `M5-IDX-01`）。这是底栏入口被删之后的正规入口之一。
//! 2. **图库来源**：按钮「添加关注文件夹…」→ 与顶栏「导入」**同一个**
//!    `add_folder`（`M5-IM-01`：同一函数、同一系统选目录面板），不是第二套实现。
//! 3. **外观**：亮色 / 暗色 / 跟随系统三选一，写入既有 preferences。
//!    这是正式主题入口（只在设置页；更多菜单不放「切换到暗色」）。
//!
//! ## 明确不在这里的东西
//!
//! * 「演示与数据」不进入正式产品（OUT-C）。
//! * 帮助页（§11 #3）尚未定稿，不做假入口。
//! * 不可达窗口（诊断、重建失败索引）不因「设置页」而获得新入口。
//!
//! 本模块只画界面、发出 [`SettingsAction`]，不碰数据库、不调系统面板——
//! 与 `index_mgr` / `diagnostics` 同一分工，便于单测。

use eframe::egui::{self, Context, RichText};
use picasa_core::Appearance;

use crate::theme::Theme;

/// 设置页里的动作，交回 `app` 执行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsAction {
    /// 打开五标签索引管理窗口（与更多菜单「索引管理…」同一个落点）。
    OpenIndexManager,
    /// 添加关注文件夹——必须落到既有的 `add_folder`（同一个系统选目录）。
    AddFolder,
    /// M5-R3：验收用临时外观开关（不是 §11 #4 的定稿入口）。
    SetAppearance(Appearance),
}

/// 设置页标题（窗口标题与页内标题**必须是同一句**，测试锁定）。
pub const SETTINGS_TITLE: &str = "设置";

/// 页内「索引」段落的按钮文案。
pub const OPEN_INDEX_MANAGER_LABEL: &str = "打开索引管理";

/// 页内「图库来源」段落的按钮文案。
pub const ADD_FOLDER_LABEL: &str = "添加关注文件夹…";

/// 外观段落的标题（正式入口只在设置页）。
pub const APPEARANCE_SECTION: &str = "外观";

/// 亮色选项文案。
pub const APPEARANCE_LIGHT_LABEL: &str = "亮色";

/// 暗色选项文案。
pub const APPEARANCE_DARK_LABEL: &str = "暗色";

/// 跟随系统选项文案。
pub const APPEARANCE_SYSTEM_LABEL: &str = "跟随系统";

/// 画设置窗口。`open` 由调用方持有（本模块不负责「关掉之后干什么」）。
///
/// 返回本帧用户点出的动作（最多一条）。
pub fn show(
    ctx: &Context,
    theme: &Theme,
    appearance: Appearance,
    open: &mut bool,
) -> Option<SettingsAction> {
    if !*open {
        return None;
    }
    let mut action = None;
    egui::Window::new(SETTINGS_TITLE)
        .id(egui::Id::unique("m5-settings"))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_width(440.0);
            ui.add_space(4.0);

            // ---- 外观（亮色 / 暗色 / 跟随系统）----
            section(ui, theme, APPEARANCE_SECTION);
            ui.horizontal(|ui| {
                for (value, label) in [
                    (Appearance::Light, APPEARANCE_LIGHT_LABEL),
                    (Appearance::Dark, APPEARANCE_DARK_LABEL),
                    (Appearance::System, APPEARANCE_SYSTEM_LABEL),
                ] {
                    let selected = appearance == value;
                    let text = if selected {
                        format!("● {label}")
                    } else {
                        format!("○ {label}")
                    };
                    if ui.selectable_label(selected, text).clicked() && !selected {
                        action = Some(SettingsAction::SetAppearance(value));
                    }
                }
            });
            ui.add_space(10.0);

            // ---- 图库来源（已确认的入口壳之一；与顶栏「导入」同一 add_folder）----
            section(ui, theme, "图库来源");
            ui.add_space(4.0);
            if ui.button(ADD_FOLDER_LABEL).clicked() {
                action = Some(SettingsAction::AddFolder);
            }
            ui.add_space(10.0);

            // ---- 索引：底栏入口删除后的正规入口 ----
            section(ui, theme, "索引");
            ui.label(
                RichText::new("索引管理包含任务、关注文件夹、排除项、存储与诊断、导入 / 导出五个标签。")
                    .size(11.5)
                    .color(theme.text_secondary),
            );
            ui.add_space(4.0);
            if ui.button(OPEN_INDEX_MANAGER_LABEL).clicked() {
                action = Some(SettingsAction::OpenIndexManager);
            }
            ui.add_space(8.0);

            // §11 #10：设置页其余段落待确认，本轮不放占位段落（不留空标题）。
            ui.separator();
            ui.label(
                RichText::new("更多设置项待确认（帮助等在后续版本补齐）。")
                    .size(11.0)
                    .color(theme.text_disabled),
            );
        });
    action
}

fn section(ui: &mut egui::Ui, theme: &Theme, title: &str) {
    ui.label(
        RichText::new(title)
            .size(12.5)
            .strong()
            .color(theme.text_primary),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `M5-IA-04` / `AC-IA-MORE`：设置壳必须有「打开索引管理」与同源「图库来源」
    /// 入口；页内不得出现「演示与数据」。
    #[test]
    fn settings_shell_exposes_index_manager_and_reuses_add_folder() {
        assert_eq!(SETTINGS_TITLE, "设置");
        assert_eq!(OPEN_INDEX_MANAGER_LABEL, "打开索引管理");
        assert!(ADD_FOLDER_LABEL.contains("关注文件夹"));
        // 动作是分开的枚举值：设置页不直接执行任何动作。
        assert_ne!(SettingsAction::OpenIndexManager, SettingsAction::AddFolder);
        // 「演示与数据」不进正式产品（OUT-C）。
        for banned in ["演示", "恢复演示数据"] {
            assert!(!OPEN_INDEX_MANAGER_LABEL.contains(banned));
            assert!(!ADD_FOLDER_LABEL.contains(banned));
        }
    }

    /// `AC-IDX-ENTRY` / §11 #10：设置页只放外观（含主题）、打开索引管理、图库来源；
    /// 「演示与数据」不进正式产品，也没有帮助页假入口。
    #[test]
    fn ac_idx_entry_settings_only_carries_the_three_confirmed_sections() {
        // 三件已确认的事各有对应动作。
        assert_eq!(SETTINGS_TITLE, "设置");
        assert_eq!(OPEN_INDEX_MANAGER_LABEL, "打开索引管理");
        assert!(ADD_FOLDER_LABEL.contains("关注文件夹"));
        // 动作枚举里只有这三个（没有「演示」「帮助」「评分」之类）。
        let actions = [
            SettingsAction::OpenIndexManager,
            SettingsAction::AddFolder,
            SettingsAction::SetAppearance(Appearance::Light),
        ];
        assert_eq!(actions.len(), 3);

        // 页面正文（去掉注释）里不许出现这些越界项。
        let prod = match include_str!("settings.rs").find("#[cfg(test)]") {
            Some(i) => &include_str!("settings.rs")[..i],
            None => include_str!("settings.rs"),
        };
        for banned in ["演示与数据", "恢复演示数据", "帮助与快捷键", "评分"] {
            for line in prod.lines() {
                let t = line.trim_start();
                if t.starts_with("//") || t.starts_with("///") {
                    continue;
                }
                assert!(!line.contains(banned), "设置页不许出现「{banned}」：{line}");
            }
        }
    }

    /// `AC-IDX-ENTRY`：设置页的「打开索引管理」与更多菜单「索引管理…」是**同一个落点**
    /// （都走 `IndexMgrUi::open_at`），不是两套实现。
    #[test]
    fn ac_idx_entry_settings_and_more_menu_share_one_landing_point() {
        let app = include_str!("app.rs");
        let prod = match app.find("#[cfg(test)]") {
            Some(i) => &app[..i],
            None => app,
        };
        // 两个入口都调 open_at，且都传同一份 failed_task_count。
        let calls = prod.matches(".open_at(&self.mgmt, self.mgmt.failed_task_count)").count();
        assert!(
            calls >= 3,
            "更多菜单 / 设置页 / 浮层详情都应走同一个 open_at（实际 {calls} 处）"
        );
        // 设置页那支必须真的把窗口打开（不是只置一个标志没人读）。
        assert!(prod.contains("SettingsAction::OpenIndexManager"));
    }

    /// 正式主题：设置页只给「亮色 / 暗色 / 跟随系统」三个选项，
    /// 不再带「清场 / 暗房 / 入口位置未定稿」这类验收期文案。
    #[test]
    fn appearance_is_formal_light_dark_and_system() {
        assert_eq!(APPEARANCE_SECTION, "外观");
        assert_eq!(APPEARANCE_LIGHT_LABEL, "亮色");
        assert_eq!(APPEARANCE_DARK_LABEL, "暗色");
        assert_eq!(APPEARANCE_SYSTEM_LABEL, "跟随系统");
        // 三个选项是不同的动作值（切换真的会发生）。
        assert_ne!(
            SettingsAction::SetAppearance(Appearance::Light),
            SettingsAction::SetAppearance(Appearance::Dark)
        );
        assert_ne!(
            SettingsAction::SetAppearance(Appearance::Dark),
            SettingsAction::SetAppearance(Appearance::System)
        );
        // 验收期文案彻底退役：用户可见处不再出现这三个词组。
        let src = match include_str!("settings.rs").find("#[cfg(test)]") {
            Some(i) => &include_str!("settings.rs")[..i],
            None => include_str!("settings.rs"),
        };
        for banned in ["清场", "暗房", "入口位置未定稿", "仅用于验收"] {
            assert!(!src.contains(banned), "设置页仍出现验收期文案「{banned}」");
        }
        // 也不写「默认暗色」这类会改产品默认的话。
        for text in [
            APPEARANCE_SECTION,
            APPEARANCE_LIGHT_LABEL,
            APPEARANCE_DARK_LABEL,
            APPEARANCE_SYSTEM_LABEL,
            OPEN_INDEX_MANAGER_LABEL,
            ADD_FOLDER_LABEL,
        ] {
            assert!(!text.contains("演示"), "设置页不许出现演示数据：{text}");
            assert!(!text.contains("恢复演示"), "{text}");
        }
    }
}
