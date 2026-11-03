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

    // ---- 索引进度（M2，`docs/04`）----
    /// 进度条与"进行中"指示（沿用选中蓝，不另造一个蓝）
    pub progress: Color32,
    /// 警示（部分失败 / 失败角标），暖色
    pub warn: Color32,
    /// 不可访问 / 硬错误
    pub danger: Color32,
    /// 已完成状态点
    pub ok: Color32,
    /// 浮层底色（比工具栏更亮，形成"压在内容上"的层级）
    pub overlay_bg: Color32,
    /// 占位格底色（灰底 +「待生成」，`04` §3.2.1）
    pub placeholder_bg: Color32,

    // ---- 树 / 组头（量自 09-library-zh-full.png，见 ui-baseline.toml）----
    /// 树行文字与计数（Picasa 用 #4D4D4D，计数与标签同色）
    pub tree_text: Color32,
    /// 组头渐变顶 / 底与 1px 边框
    pub header_top: Color32,
    pub header_bottom: Color32,
    pub header_border: Color32,
    /// 组头折叠钮的 Picasa 绿
    pub disclosure_green: Color32,

    // ---- M3.1：macOS「照片」风格（量自参考截图 2400×1440 ÷2）----
    /// 顶栏底色（截图上没有独立顶栏底，就是窗口白）
    pub toolbar_bg: Color32,
    /// 顶栏底边发丝线（半透明灰）
    pub toolbar_border: Color32,
    /// 分段控件胶囊底（灰白）
    pub seg_bg: Color32,
    /// 分段控件选中片白底
    pub seg_active: Color32,
    /// 顶栏图标主色（近黑）
    pub toolbar_icon: Color32,
    /// 左栏分组标题（"相簿"/"固定"，灰色小字）
    pub sidebar_section_text: Color32,
    /// 左栏悬停底色（浅灰）
    pub sidebar_hover: Color32,
}

impl Theme {
    /// Picasa 是浅色界面。这里**不跟随系统深色模式**——
    /// 界面还原度是第一优先级（01 §11.5.5），跟随系统会直接破坏「像不像」。
    pub fn picasa() -> Self {
        Self {
            // TODO(U-43-①): 待取色。目测 ≈ #F0F0F0
            chrome_bg: Color32::from_rgb(0xF0, 0xF0, 0xF0),
            // M3.1：栅格背景 = 窗口白（截图的内容区就是白底）
            grid_bg: Color32::WHITE,
            // TODO(U-43-④): 待取色。目测 ≈ #D6D6D6
            header_bg: Color32::from_rgb(0xD8, 0xD8, 0xD8),
            // M3.1：侧栏底（截图半透灰的实色近似，比窗口白深一档）
            sidebar_bg: Color32::from_rgb(0xF5, 0xF4, 0xF2),

            separator: Color32::from_rgb(0xC8, 0xC8, 0xC8),
            cell_border: Color32::from_rgb(0xCF, 0xCF, 0xCF),

            // TODO(U-43-③): 待取色。目测 ≈ #4A90D9
            selected: Color32::from_rgb(0x4A, 0x90, 0xD9),
            // 树选中行：量自 09-library-zh-full.png（131,167,189）——钢青蓝，**纯色无渐变**，
            // 上面配白字。之前目测的 #3875D7 偏亮偏饱和，量出来才知道是灰蓝。
            sidebar_selected: Color32::from_rgb(0x83, 0xA7, 0xBD),
            sidebar_selected_inactive: Color32::from_rgb(0xD6, 0xE4, 0xF7),
            cell_hover: Color32::from_rgb(0xDC, 0xE6, 0xF2),

            // 树文字/计数：量自参考图（最深像素 77,77,77）——Picasa 的计数「(1)」
            // **与标签同色**，不是更浅的灰；只是字号更小。
            tree_text: Color32::from_rgb(0x4D, 0x4D, 0x4D),
            // 组头（「人物 (1)」那种带边框的白盒）：
            // 底是 (241,243,246)→(220,221,225) 的竖直渐变，上下各 1px (155,155,155) 边框；
            // 左端折叠钮是 Picasa 绿 (10,144,0)。
            header_top: Color32::from_rgb(0xF1, 0xF3, 0xF6),
            header_bottom: Color32::from_rgb(0xDC, 0xDD, 0xE1),
            header_border: Color32::from_rgb(0x9B, 0x9B, 0x9B),
            disclosure_green: Color32::from_rgb(0x0A, 0x90, 0x00),

            text_primary: Color32::from_rgb(0x20, 0x20, 0x20),
            text_secondary: Color32::from_rgb(0x66, 0x66, 0x66),
            text_disabled: Color32::from_rgb(0xAA, 0xAA, 0xAA),
            header_text: Color32::from_rgb(0x33, 0x33, 0x33),
            status_text: Color32::from_rgb(0x5A, 0x5A, 0x5A),

            viewer_bg: Color32::from_rgb(0x1A, 0x1A, 0x1A),
            viewer_chrome: Color32::from_rgb(0x2B, 0x2B, 0x2B),
            viewer_text: Color32::from_rgb(0xE8, 0xE8, 0xE8),

            progress: Color32::from_rgb(0x4A, 0x90, 0xD9),
            warn: Color32::from_rgb(0xC8, 0x86, 0x1E),
            danger: Color32::from_rgb(0xC0, 0x39, 0x2B),
            ok: Color32::from_rgb(0x3E, 0x8E, 0x3E),
            overlay_bg: Color32::from_rgb(0xFB, 0xFB, 0xFB),
            placeholder_bg: Color32::from_rgb(0xE4, 0xE4, 0xE4),

            // M3.1（量自参考截图）：窗口白、发丝线、灰白胶囊
            toolbar_bg: Color32::WHITE,
            toolbar_border: Color32::from_rgba_premultiplied(0, 0, 0, 26),
            seg_bg: Color32::from_rgb(0xEB, 0xE9, 0xE5),
            seg_active: Color32::WHITE,
            toolbar_icon: Color32::from_rgb(0x2A, 0x2A, 0x2A),
            sidebar_section_text: Color32::from_rgb(0x8A, 0x8A, 0x8A),
            sidebar_hover: Color32::from_rgb(0xE9, 0xE7, 0xE3),
        }
    }
}

/// 尺寸常量。全部标了 U 编号，回填时一一对应。
pub struct Metrics;

impl Metrics {
    /// 顶部两段式（量自 09-library-zh-full.png x=650 逐行扫描）：
    /// 标题栏(y0..19)是 OS 的，我们不画；**菜单栏** 21px + 1px 分隔 + **工具行** 38px。
    /// 总高 = 21 + 1 + 38 = 60（旧的 68「图标上文下」版式已废弃，PM 2026-11-01 拍板照抄参考图）。
    pub const MENUBAR_H: f32 = 21.0;
    pub const TOOLROW_H: f32 = 38.0;
    pub const TOOLBAR_H: f32 = 60.0;
    /// 菜单栏：白→淡蓝渐变的三个采样点（y22 #FEFEFF / y30 #D4DBEE / y39 #E1E6F6）
    pub const MENUBAR_TOP: Color32 = Color32::from_rgb(0xFE, 0xFE, 0xFF);
    pub const MENUBAR_MID: Color32 = Color32::from_rgb(0xD4, 0xDB, 0xEE);
    pub const MENUBAR_BOT: Color32 = Color32::from_rgb(0xE1, 0xE6, 0xF6);
    /// 菜单栏底边 1px（y40）与工具行底边 1px（y80）
    pub const MENUBAR_BORDER: Color32 = Color32::from_rgb(0xB6, 0xBC, 0xCC);
    pub const TOOLROW_BORDER: Color32 = Color32::from_rgb(0xBA, 0xBA, 0xBA);
    /// 工具行底色（y41..79）
    pub const TOOLROW_BG: Color32 = Color32::from_rgb(0xE8, 0xE8, 0xE8);
    /// 搜索框边框（x=1250 处 #7F9DB9 蓝灰边 + 白芯，XP 经典风格）
    pub const SEARCH_BORDER: Color32 = Color32::from_rgb(0x7F, 0x9D, 0xB9);
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

    /// 左栏行高：量自 09-library-zh-full.png —— 相邻文件夹图标行顶距恒为 **22px**。
    pub const ROW_H: f32 = 22.0;
    /// U-14：每级缩进。目测 12~16，取 14。
    /// ⚠️ 参考图里所有可见文件夹都在同一层级，**量不出**每级缩进；沿用 14，
    /// 已登记进 `ui-baseline.toml` 的「本图不可测」清单。
    pub const INDENT: f32 = 14.0;
    /// 组头行高：量自参考图 —— 「人物 (1)」白盒上下边框外沿 82..102，高 21px，取 22 对齐行高。
    pub const SECTION_H: f32 = 22.0;

    /// U-36/U-37：左栏底部照片托盘（收起高度 / 展开高度）
    pub const TRAY_COLLAPSED_H: f32 = 22.0;
    pub const TRAY_EXPANDED_H: f32 = 92.0;
    /// 托盘里一个缩略图方块的边长
    pub const TRAY_THUMB: f32 = 56.0;

    /// U-21：三档单元格边长（与 `picasa_core::library::CELL_SIZES` 必须一致）
    pub const CELL_SIZES: [f32; 3] = [85.0, 144.0, 200.0];

    // ---- M3.1：macOS「照片」风格顶栏（量自参考截图 2400×1440 ÷2）----
    /// 顶栏总高：截图 104px ÷ 2 ≈ 52。
    pub const TOPBAR_H: f32 = 52.0;
    /// 顶栏图标（圆形热区）直径：截图 76px ÷ 2 = 38。
    pub const TOPBAR_ICON_D: f32 = 38.0;
    /// 左栏默认宽度：截图 432px ÷ 2 = 216（连同最小/最大一起放宽）。
    pub const SIDEBAR_DEFAULT_W: f32 = 216.0;
    /// 左栏行高：截图 72px ÷ 2 = 36。
    pub const SIDEBAR_ROW_H: f32 = 36.0;
    /// 左栏行缩进每级步长。
    pub const SIDEBAR_INDENT: f32 = 16.0;
    /// 左栏分组标题（"相簿"）行高：截图上标题行约 30px ÷ 2 = 15 → 取 22 留呼吸。
    pub const SIDEBAR_SECTION_H: f32 = 30.0;
    /// 分段控件高度：截图 72px ÷ 2 = 36。
    pub const SEG_H: f32 = 36.0;
    /// −/+ 胶囊高：与分段控件同高。
    pub const ZOOM_PILL_H: f32 = 36.0;
    /// U-22：单元格间距（水平/垂直同一个值，**不是均分剩余空间**）
    pub const CELL_GAP: f32 = 10.0;
    /// U-23：栅格四周留白
    pub const GRID_PAD: f32 = 12.0;
    /// U-30：日期分组头高度
    pub const GROUP_HEADER_H: f32 = 26.0;
    /// U-26：文件名行高（三档都显示，见 `Tier::shows_filename`）
    pub const FILENAME_H: f32 = 16.0;
    /// U-26：文件名字号。
    ///
    /// 11px 对中档（144px 格）偏大——16px 行高里塞 11px 字只剩 5px 余量，
    /// Windows 上原版 Picasa 的文件名行也明显小于图标下方留白。取 11 是与
    /// 侧栏行高一致；这里提成常量是为了让「字号」和「行高」能分开调，
    /// 回填 U-26 时不必再去 grid.rs 里挖魔法数字。
    pub const FILENAME_FONT_SIZE: f32 = 11.0;

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
    install_cjk_fallback(ctx);

    // 🔒 锁定浅色（2026-11-01 用户报告「索引管理里按钮文字与按钮颜色接近，无法识别」）：
    // egui 默认**跟随系统**主题偏好，GNOME 深色模式下 egui 走 dark 调色板——
    // 控件底色变成深灰，而我们下面的 window_fill / 文字色仍按 Picasa 浅色系硬编码，
    // 结果就是深灰按钮上叠深灰文字。Picasa 是浅色应用（查看器是暗的，但那是
    // `viewer_bg` 手绘的，不走 egui 控件），所以这里直接钉死 Light。
    ctx.set_theme(egui::Theme::Light);

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

        // 控件配色**显式**给出，不依赖 egui 默认调色板（同上：防止主题跟随把
        // 底色/前景色拆到两个世界里）。量值来源见 `docs/ui-baseline/ui-baseline.toml`。
        for w in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
        ] {
            w.fg_stroke = egui::Stroke::new(1.0, theme.text_primary);
        }
        // 非交互文字（分组标签等）用次级灰
        style.visuals.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.0, theme.text_secondary);
        // 按钮：近白底 + 灰边，压在 #F0F0F0 的窗口底上可辨（参考图「分享」按钮同款）
        style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(0xFB, 0xFB, 0xFB);
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, theme.separator);

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

/// 安装中日韩字体回退（2026-11-02 用户报告：界面中文全部乱码）。
///
/// ## 为什么不用 `system_fonts` feature
///
/// 那个 feature 在本机（macOS 26）上会让 egui 的系统字体枚举卡死主线程
/// （见根 `Cargo.toml` 里 eframe features 的注释）。所以这里**手动**加载一个
/// 已知路径的系统 CJK 字体，绕开枚举，也不走 CoreText fallback 探测。
///
/// ## 为什么选 Hiragino Sans GB 而不是 PingFang
///
/// 都在 `/System/Library/Fonts` 下、所有 macOS 自带；PingFang.ttc 的第一个 face
/// 就是中文主 face，而 Hiragino Sans GB.ttc 第一个 face 覆盖拉丁、后面 face 才是
/// 中文字形——作为 **fallback**（`FontPriority::Lowest`）后者更合适：
/// 拉丁字符继续用 egui 内置字体（保持现有观感与度量），只有内置字体缺的字形
/// （全部汉字）才落到 Hiragino 上。
///
/// 加载失败静默跳过：缺字体的后果只是中文乱码（可诊断），不该让应用打不开。
fn install_cjk_fallback(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        // macOS 自带（第一个含简体中文字形的系统字体，23 MB，比 PingFang 小）
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        // 备选：Windows / 其他发行方式下可能存在的路径
        "C:/Windows/Fonts/msyh.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    ];
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        tracing::info!(path, bytes = bytes.len(), "已加载 CJK 回退字体");
        ctx.add_font(egui::FontInsert::new(
            "cjk_fallback",
            egui::FontData::from_owned(bytes),
            vec![
                egui::InsertFontFamily {
                    family: egui::FontFamily::Proportional,
                    priority: egui::FontPriority::Lowest,
                },
                egui::InsertFontFamily {
                    family: egui::FontFamily::Monospace,
                    priority: egui::FontPriority::Lowest,
                },
            ],
        ));
        return;
    }
    tracing::warn!("未找到可用的 CJK 回退字体，中文可能显示为方框");
}
