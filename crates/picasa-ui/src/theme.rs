//! 配色与尺寸常量——**界面还原度的单点配置**（02 §8.3.1 / §8.2.4）。
//!
//! ## 为什么全部集中在一个文件
//!
//! 01 §11.5.6 的 44 项界面还原清单里，有 24 项标着 `⚠️待核对`——它们全都是**数值**
//! （尺寸、间距、HEX）。等 PM 用原版 3.9 的真实截图取色/量像素之后，只需要改这里，
//! 不用碰任何布局算法。**这是把「待回填」的技术成本降到最低的方式。**
//!
//! 每个常量后面都标了它对应的 U 编号，回填时按编号对照 01 文档即可。
//!
//! ## 现值是怎么来的（诚实说明）
//!
//! 下面带 `TODO(U-xx)` 的值是 02 §8.3.1 的**目测近似值**，**不是**取色结果。
//! 它们足以让界面「看起来像 Picasa」，但**不能作为验收依据**——
//! 验收必须用真实截图量出来的数字（01 §11.5.6 的处置规则）。

use eframe::egui::{self, Color32, FontFamily, FontId, TextStyle};

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    // ---- 背景与分区（U-43 的四个关键色之一：①②④）----
    /// ① 工具栏 / 状态栏底色
    pub chrome_bg: Color32,
    /// ② 栅格背景（比工具栏更浅）
    pub grid_bg: Color32,
    /// ④ 日期分组头灰条
    pub header_bg: Color32,
    /// 左栏背景（比工具栏略深一点点，形成分区）
    pub sidebar_bg: Color32,

    // ---- 线条 ----
    /// 分区之间的 1px 分隔线（**不是投影**，U-02）
    pub separator: Color32,
    /// 缩略图外框（U-24：1px 浅灰描边，让白底照片也有边界）
    pub cell_border: Color32,

    // ---- 交互态 ----
    /// ③ 选中蓝
    pub selected: Color32,
    /// 左栏选中行背景（U-16）
    pub sidebar_selected: Color32,
    /// 失去焦点后仍保留的浅色（U-16）
    pub sidebar_selected_inactive: Color32,
    pub cell_hover: Color32,

    // ---- 文字 ----
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_disabled: Color32,
    pub header_text: Color32,
    /// 状态栏文字（U-35 估 11px，灰色）
    pub status_text: Color32,

    // ---- 查看器 ----
    /// U-39：深灰/近黑，**不是纯黑**（纯黑照片仍要能看出边界）
    pub viewer_bg: Color32,
    pub viewer_chrome: Color32,
    pub viewer_text: Color32,
}

impl Theme {
    /// Picasa 是浅色界面。这里**不跟随系统深色模式**——
    /// 界面还原度是第一优先级（01 §11.5.5），跟随系统会直接破坏「像不像」。
    pub fn picasa() -> Self {
        Self {
            // TODO(U-43-①): 待取色。目测 ≈ #F0F0F0
            chrome_bg: Color32::from_rgb(0xF0, 0xF0, 0xF0),
            // TODO(U-43-②): 待取色。目测 ≈ #EBEBEB
            grid_bg: Color32::from_rgb(0xEB, 0xEB, 0xEB),
            // TODO(U-43-④): 待取色。目测 ≈ #D6D6D6
            header_bg: Color32::from_rgb(0xD8, 0xD8, 0xD8),
            sidebar_bg: Color32::from_rgb(0xF5, 0xF5, 0xF5),

            separator: Color32::from_rgb(0xC8, 0xC8, 0xC8),
            cell_border: Color32::from_rgb(0xCF, 0xCF, 0xCF),

            // TODO(U-43-③): 待取色。目测 ≈ #4A90D9
            selected: Color32::from_rgb(0x4A, 0x90, 0xD9),
            sidebar_selected: Color32::from_rgb(0x38, 0x75, 0xD7),
            sidebar_selected_inactive: Color32::from_rgb(0xD6, 0xE4, 0xF7),
            cell_hover: Color32::from_rgb(0xDC, 0xE6, 0xF2),

            text_primary: Color32::from_rgb(0x20, 0x20, 0x20),
            text_secondary: Color32::from_rgb(0x66, 0x66, 0x66),
            text_disabled: Color32::from_rgb(0xAA, 0xAA, 0xAA),
            header_text: Color32::from_rgb(0x33, 0x33, 0x33),
            status_text: Color32::from_rgb(0x5A, 0x5A, 0x5A),

            viewer_bg: Color32::from_rgb(0x1A, 0x1A, 0x1A),
            viewer_chrome: Color32::from_rgb(0x2B, 0x2B, 0x2B),
            viewer_text: Color32::from_rgb(0xE8, 0xE8, 0xE8),
        }
    }
}

/// 尺寸常量。全部标了 U 编号，回填时一一对应。
pub struct Metrics;

impl Metrics {
    /// U-06：工具栏高度。目测 64~72，取 68（单行大图标 + 下方文案）。
    pub const TOOLBAR_H: f32 = 68.0;
    /// 工具栏里一个按钮的宽度（图标在上、文案在下）
    pub const TOOL_BTN_W: f32 = 58.0;
    /// 工具栏按钮的图标边长
    pub const TOOL_ICON: f32 = 22.0;
    /// U-08：分组之间的竖线两侧留白
    pub const TOOL_GROUP_PAD: f32 = 8.0;

    /// U-33：状态栏高度。目测 22~26，取 24。
    pub const STATUS_H: f32 = 24.0;
    /// U-03：左栏默认宽度。目测 200~240，取 220。
    pub const SIDEBAR_W: f32 = 220.0;
    pub const SIDEBAR_MIN: f32 = 160.0;
    pub const SIDEBAR_MAX: f32 = 420.0;

    /// U-18：左栏行高。目测 18~22，取 20（比 macOS 侧栏更密）。
    pub const ROW_H: f32 = 20.0;
    /// U-14：每级缩进。目测 12~16，取 14。
    pub const INDENT: f32 = 14.0;
    /// U-13：分组标题行高
    pub const SECTION_H: f32 = 22.0;

    /// U-36/U-37：左栏底部照片托盘（收起高度 / 展开高度）
    pub const TRAY_COLLAPSED_H: f32 = 22.0;
    pub const TRAY_EXPANDED_H: f32 = 92.0;
    /// 托盘里一个缩略图方块的边长
    pub const TRAY_THUMB: f32 = 56.0;

    /// U-21：三档单元格边长（与 `picasa_core::library::CELL_SIZES` 必须一致）
    pub const CELL_SIZES: [f32; 3] = [85.0, 144.0, 200.0];
    /// U-22：单元格间距（水平/垂直同一个值，**不是均分剩余空间**）
    pub const CELL_GAP: f32 = 10.0;
    /// U-23：栅格四周留白
    pub const GRID_PAD: f32 = 12.0;
    /// U-30：日期分组头高度
    pub const GROUP_HEADER_H: f32 = 26.0;
    /// U-26：文件名行高（大档才显示）
    pub const FILENAME_H: f32 = 16.0;

    /// 滚动条宽度（U-20：细滚动条，不占内容宽度）
    pub const SCROLLBAR: f32 = 8.0;

    /// 每帧最多上传几张缩略图纹理（02 §8.2.5 的「纹理上传 < 4ms」预算）。
    /// JPEG 解码 + 上传一张 512px 约 1.2ms，3 张 ≈ 3.6ms，正好卡在预算内。
    pub const TEX_UPLOAD_PER_FRAME: usize = 3;
    /// 纹理缓存上限（张）。512×384×4 ≈ 786KB/张 → 120 张 ≈ 92MB。
    pub const TEX_CACHE_CAP: usize = 120;
}

/// 字号层级（U-44 的「四档」）。用比例而非磅值对齐原版观感。
pub fn text_styles() -> Vec<(TextStyle, FontId)> {
    vec![
        // 分区标题（左栏「文件夹」「收藏」）
        (
            TextStyle::Heading,
            FontId::new(11.5, FontFamily::Proportional),
        ),
        // 正文（左栏条目、按钮文案）
        (TextStyle::Body, FontId::new(12.5, FontFamily::Proportional)),
        // 按钮文案
        (
            TextStyle::Button,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        // 状态栏 / 文件名（小一号）
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
        // 等宽（调试信息）
        (
            TextStyle::Monospace,
            FontId::new(11.0, FontFamily::Monospace),
        ),
    ]
}

/// 把 egui 的全局样式调成 Picasa 的观感。
pub fn apply(ctx: &egui::Context) {
    let theme = Theme::picasa();

    ctx.all_styles_mut(|style| {
        for (style_kind, font_id) in text_styles() {
            style.text_styles.insert(style_kind, font_id);
        }

        // 紧凑：Picasa 的密度明显高于 macOS 默认控件
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(6.0, 2.0);
        style.spacing.window_margin = egui::Margin::same(6);
        style.spacing.menu_margin = egui::Margin::same(4);

        // 圆角：Picasa 时代几乎没有圆角，全部压到 2px
        style.visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(2);
        style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(2);
        style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(2);
        style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(2);

        style.visuals.window_fill = theme.chrome_bg;
        style.visuals.panel_fill = theme.chrome_bg;
        style.visuals.extreme_bg_color = Color32::WHITE;
        style.visuals.override_text_color = Some(theme.text_primary);
        style.visuals.selection.bg_fill = theme.selected;
        style.visuals.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);
        style.visuals.widgets.hovered.bg_fill = theme.cell_hover;
        style.visuals.widgets.active.bg_fill = theme.cell_hover;

        // 滚动条：细、半透明、悬停加粗（U-20）
        style.spacing.scroll.bar_width = Metrics::SCROLLBAR;
        style.spacing.scroll.floating = false;
    });
}
