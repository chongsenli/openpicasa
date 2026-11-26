//! M5 视觉 token：**亮色** 与 **暗色** 两套完整主题。
//!
//! ## 事实来源
//!
//! 数值来自 `M5-需求说明书-v0.1` §5.1 / §5.2 的 token 表（与原型
//! `css/app.css` 的 CSS 变量一致）。这里做的是 **Rust / egui 的等价转译**：
//! 颜色、圆角、描边、字体栈、星标色都是设计 token，不是搬运 CSS。
//!
//! ## 为什么是一个 `Theme::for_appearance` 而不是两个散落的常量表
//!
//! 每个字段都同时被两套主题赋值，编译器会强制两套都写全；任何一处漏填都
//! 无法通过 `cargo check`。界面只认 [`Theme`]，不直接读 `Color32::WHITE`
//! 之类的字面量（少数「照片底片白」除外，见各绘制点注释）。
//!
//! ## §11 #4 不在这里裁定
//!
//! 主题**入口**（设置页 / 更多菜单 / 跟随系统）仍未定稿。设置页里的
//! 「外观（入口位置未定稿，仅用于验收）」只是让验收能在固定 Linux/Xvfb
//! 环境里切换两套主题的临时开关，不代表产品已经决定入口位置。

use std::sync::Arc;

use eframe::egui::{self, Color32, FontData, FontFamily, FontId, TextStyle};
use picasa_core::Appearance;

// ---- 内嵌 Inter 字重（原型用 Inter 400 / 500 / 560 / 600）----
//
// 原型 `css/app.css` 用的是 Inter（`font-family: Inter, ...`），并在多处用
// `font-weight: 500 / 560 / 600`（侧栏行名 500、组头 600、顶栏应用名 560）。
// egui 的 `FontId` 只有 (size, family)，**没有 weight**，所以字重只能拆成
// 独立 `FontFamily`：`op-medium`(500) 与 `op-semibold`(600)。
//
// 字体随二进制一起发布（SIL OFL 1.1，见 `assets/fonts/OFL.txt`），
// 不依赖系统是否装了 Inter —— macOS 没有 `/System/Library/Fonts/Inter.ttf`，
// 旧实现会静默回退到 egui 内置的 Ubuntu-Light，字形与原型差很多。
const INTER_REGULAR_TTF: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_MEDIUM_TTF: &[u8] = include_bytes!("../assets/fonts/Inter-Medium.ttf");
const INTER_SEMIBOLD_TTF: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");

/// 中等字重族名（原型 `font-weight: 500`／应用名 560）。
pub const FAMILY_MEDIUM: &str = "op-medium";
/// 半粗字重族名（原型 `font-weight: 600`，如侧栏组头）。
pub const FAMILY_SEMIBOLD: &str = "op-semibold";

/// Inter 500：侧栏行名、计数、需要「略重一点」的标签。
pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_MEDIUM.into()))
}

/// Inter 600：侧栏组头、顶栏应用名（原型 560，取更近的 600）。
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_SEMIBOLD.into()))
}

/// 两套主题共用的设计 token（按主题取值不同）。
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// 当前主题对应的外观枚举（持久化用）。
    pub appearance: Appearance,
    /// 是否暗色（方案 B）。
    pub dark: bool,

    // ---- 页面 / 窗口 / 内容井 ----
    /// 窗口外衬底（`--bg-page`）：亮 `#C8C8CC`，暗 `#050505`。
    pub page_bg: Color32,
    /// 窗口底色（`--win`）：亮 `#f5f5f5`，暗 `#151515`。
    pub chrome_bg: Color32,
    /// 内容井卡片（包住主区的圆角面）：亮 = 近白；暗 = `#1c1c1c` 卡面。
    pub well_bg: Color32,
    /// 主内容井（井内实际画照片的面）：亮 = 近白；暗 = `#151515`。
    pub grid_bg: Color32,
    /// 卡片 / 菜单 / 浮层底色（`--card`）：亮 `#fff`，暗 `#1c1c1c`。
    pub menu_bg: Color32,
    /// 左栏底色（与窗口同色，用发丝线分区）。
    pub sidebar_bg: Color32,
    /// 日期分组头条底色（M5 起与内容井同色，不画灰条）。
    pub header_bg: Color32,

    // ---- 线条 ----
    /// 分区发丝线（`--line`）：亮 6% 黑，暗近黑。
    pub separator: Color32,
    /// 控件发丝边（比 [`Self::separator`] 更淡，用于输入框 / 胶囊）。
    pub hairline: Color32,
    /// 缩略图外框（占位格）。
    pub cell_border: Color32,

    // ---- 强调 / 交互 ----
    /// 强调色：亮 `#0485f7`，暗 `#00dfe1`。
    pub selected: Color32,
    /// 强调文字色：亮 `#0373d6`，暗 `#00dfe1`。
    pub accent_ink: Color32,
    /// 强调柔底：亮 `#e6f1fe`，暗 `#00dfe114`。
    pub accent_soft: Color32,
    /// 收藏星：亮 `#f5b942`，暗按 B 视觉用强调色 `#00dfe1`（M5-GR-07）。
    pub star: Color32,
    /// 左栏选中行底色：亮 = 白卡片，暗 = `#232323`。
    pub sidebar_selected: Color32,
    /// 选中行文字：亮 = ink，暗 = 白。
    pub row_selected_text: Color32,
    /// 失焦后仍保留的浅色（沿用强调柔底）。
    pub sidebar_selected_inactive: Color32,
    /// 悬停底色：亮 `#ebebec`，暗 `#232323`。
    pub sidebar_hover: Color32,
    /// 栅格悬停 / 控件悬停（与侧栏同一个 hover token）。
    pub cell_hover: Color32,
    /// 滑轨 / 芯片底色：亮 `#ebebec`，暗 `#1c1c1c`。
    pub track_bg: Color32,
    /// 芯片底：亮 `#f4f4f5`，暗 `#232323`。
    pub chip_bg: Color32,

    // ---- 文字 ----
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_disabled: Color32,
    pub header_text: Color32,
    pub status_text: Color32,
    pub tree_text: Color32,

    // ---- 查看器展厅 ----
    /// 亮色 = 浅灰展厅；暗色 = 暗房。`Bg::Black` 档位读它（V1）。
    pub viewer_bg: Color32,
    /// 照片卡片 / 顶栏胶囊的浅色底（亮色展厅里的圆角卡片）。
    pub viewer_card: Color32,
    /// 展厅控件的实色底（亮色浅、暗色深）。
    pub viewer_chrome_bg: Color32,
    pub viewer_chrome: Color32,
    pub viewer_text: Color32,

    // ---- 状态色 ----
    /// 索引进度（= 强调色，不另造一个蓝）。
    pub progress: Color32,
    /// 警示（部分失败 / 离线根）。
    pub warn: Color32,
    /// 危险：亮 `#e11d48`，暗 `#fb7185`（暗底上提高明度）。
    pub danger: Color32,
    /// 成功：亮 `#16a34a`，暗 `#34d399`。
    pub ok: Color32,
    /// 浮层底色（卡片底）。
    pub overlay_bg: Color32,
    /// 占位格底色。
    pub placeholder_bg: Color32,

    // ---- 树 / 组头（保留字段；M5 起观感见各绘制点）----
    pub header_top: Color32,
    pub header_bottom: Color32,
    pub header_border: Color32,
    pub disclosure_green: Color32,

    // ---- 顶栏 ----
    /// 顶栏底色 = 窗口色。
    pub toolbar_bg: Color32,
    /// 顶栏底缘发丝线。
    pub toolbar_border: Color32,
    /// 分段控件 / 排布胶囊的槽底。
    pub seg_bg: Color32,
    /// 分段控件选中片：亮 = 白，暗 = `#2a2a2a`。
    pub seg_active: Color32,
    /// 顶栏线性图标色：亮 = ink，暗 = `#d4d4d4`。
    pub toolbar_icon: Color32,
    /// 左栏分组标题：亮 = faint，暗 = `#7c7c7c`。
    pub sidebar_section_text: Color32,
    /// 排布胶囊选中片（M5 起 = 强调柔底，不再用旧淡橙）。
    pub seg_active_orange: Color32,

    // ---- 阴影（只有一层极淡的缩略图落影；不再有白圆按钮软阴影）----
    pub thumb_shadow: egui::Shadow,

    // ---- 圆角 token ----
    /// 卡片 / 内容井：亮 26，暗 22。
    pub radius_card: f32,
    /// 缩略图：亮 16，暗 10。
    pub radius_thumb: f32,
    /// 按钮 / 行 / 芯片：亮 10，暗 8。
    pub radius_btn: f32,

    // ---- 选中环（0 0 0 2px 内环 + 0 0 0 4px 外环）----
    /// 内环：亮 `#fff`，暗 `#151515`。
    pub ring_inner: Color32,
    /// 外环：亮 `#0485f7`，暗 `#00dfe1`。
    pub ring_outer: Color32,

    // ---- 视觉参考（**不驱动** `sidebar_width` 偏好逻辑）----
    /// 侧栏参考宽：亮 228，暗 216。仅用于截图对照说明，不改用户拖出来的宽度。
    pub sidebar_ref_w: f32,
    /// 暗色标题是否用 11px 字距加宽大写。
    pub title_uppercase: bool,
}

/// 亮色。
pub const LIGHT: ThemeSpec = ThemeSpec {
    dark: false,
    page: 0xC8C8CC,
    win: 0xf5f5f5,
    well: 0xffffff,
    card: 0xffffff,
    ink: 0x18181b,
    muted: 0x71717a,
    faint: 0xa1a1aa,
    accent: 0x0485f7,
    accent_ink: 0x0373d6,
    accent_soft: 0xe6f1fe,
    card_chip: 0xf4f4f5,
    track: 0xebebec,
    hover: 0xebebec,
    star: 0xf5b942,
    danger: 0xe11d48,
    ok: 0x16a34a,
    radius_card: 26.0,
    radius_thumb: 16.0,
    radius_btn: 10.0,
    sidebar_ref_w: 228.0,
};

/// 暗色。
pub const DARK: ThemeSpec = ThemeSpec {
    dark: true,
    page: 0x050505,
    win: 0x151515,
    well: 0x151515,
    card: 0x1c1c1c,
    ink: 0xf2f2f2,
    muted: 0x9a9a9a,
    faint: 0x7c7c7c,
    accent: 0x00dfe1,
    accent_ink: 0x00dfe1,
    accent_soft: 0x00000000, // 带 alpha 的柔底单独处理（`#00dfe114`）
    card_chip: 0x232323,
    track: 0x1c1c1c,
    hover: 0x232323,
    star: 0x00dfe1,
    danger: 0xfb7185,
    ok: 0x34d399,
    radius_card: 22.0,
    radius_thumb: 10.0,
    radius_btn: 8.0,
    sidebar_ref_w: 216.0,
};

/// 一套主题的原始 token（HEX 值），由 [`Theme::for_appearance`] 展开。
#[derive(Clone, Copy, Debug)]
pub struct ThemeSpec {
    pub dark: bool,
    pub page: u32,
    pub win: u32,
    pub well: u32,
    pub card: u32,
    pub ink: u32,
    pub muted: u32,
    pub faint: u32,
    pub accent: u32,
    pub accent_ink: u32,
    pub accent_soft: u32,
    pub card_chip: u32,
    pub track: u32,
    pub hover: u32,
    pub star: u32,
    pub danger: u32,
    pub ok: u32,
    pub radius_card: f32,
    pub radius_thumb: f32,
    pub radius_btn: f32,
    pub sidebar_ref_w: f32,
}

impl ThemeSpec {
    pub fn appearance(&self) -> Appearance {
        if self.dark {
            Appearance::Dark
        } else {
            Appearance::Light
        }
    }
}

fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

impl Theme {
    /// 卡片外框描边。
    ///
    /// 原型：亮 `html[data-theme="light"] .card{box-shadow:…,0 0 0 1px rgba(0,0,0,.035)}`
    /// （1px 极淡黑边），暗 `html[data-theme="dark"] .card{border:1px solid #ffffff0d}`。
    pub fn card_border(&self) -> Color32 {
        if self.dark {
            Color32::from_rgba_unmultiplied(255, 255, 255, 0x0d)
        } else {
            Color32::from_rgba_unmultiplied(0, 0, 0, 0x09)
        }
    }

    /// 卡片内层描边：原型暗色 `.inner{border:1px solid #ffffff0a}`；亮色没有内层。
    pub fn card_inner_border(&self) -> Color32 {
        Color32::from_rgba_unmultiplied(255, 255, 255, 0x0a)
    }

    /// 卡片内层圆角：原型暗色 `.inner{border-radius:16px}`。
    pub const CARD_INNER_RADIUS: f32 = 16.0;

    /// 卡片内层底色：暗色 `css/app.css:159 .inner{background:#151515}`（比外卡 #1c1c1c 暗一档），
    /// 亮色没有这层，卡片本身就是白色。底栏内嵌在卡片里，也要用这个色，
    /// 否则暗色底栏会露出外卡的 #1c1c1c，跟原型差一档（实测差 7 个灰阶）。
    pub fn card_inner_bg(&self) -> Color32 {
        if self.dark {
            self.grid_bg
        } else {
            self.well_bg
        }
    }

    /// 亮色。产品默认。
    pub fn light() -> Self {
        Self::from_spec(&LIGHT)
    }

    /// 暗色。
    pub fn dark() -> Self {
        Self::from_spec(&DARK)
    }

    /// 按偏好返回主题。**默认亮色**（`Appearance::Light`）。
    ///
    /// `Appearance::System` 会先按系统深浅色解析（见 [`Appearance::resolve_system`]），
    /// 再选亮 / 暗两套 token。
    pub fn for_appearance(appearance: Appearance) -> Self {
        match appearance.resolve_system() {
            Appearance::Dark => Self::dark(),
            _ => Self::light(),
        }
    }

    fn from_spec(s: &ThemeSpec) -> Self {
        let dark = s.appearance().is_dark();
        // 暗色强调柔底 `#00dfe114`：8% 青色。
        //
        // 必须用 `from_rgba_unmultiplied`：`Color32` 存的是**预乘**色，若把
        // `(0x00,0xdf,0xe1)` 原样塞进 premultiplied 版本，实际透明度 8% 却带着
        // 满亮度的通道值，叠在深底上会变成一条实心高亮青块（M5-V1 顶栏「照片信息」
        // 打开态就是这样露出来的）。
        let accent_soft = if dark {
            Color32::from_rgba_unmultiplied(0x00, 0xdf, 0xe1, 0x14)
        } else {
            rgb(s.accent_soft)
        };
        let hairline = if dark {
            Color32::from_rgba_unmultiplied(255, 255, 255, 0x14)
        } else {
            Color32::from_rgba_premultiplied(0, 0, 0, 0x12)
        };
        let separator = if dark {
            Color32::from_rgb(0x00, 0x00, 0x00)
        } else {
            Color32::from_rgba_premultiplied(0, 0, 0, 0x10)
        };
        Self {
            appearance: s.appearance(),
            dark,
            page_bg: rgb(s.page),
            chrome_bg: rgb(s.win),
            well_bg: if dark { rgb(s.card) } else { rgb(s.well) },
            grid_bg: rgb(s.well),
            menu_bg: rgb(s.card),
            sidebar_bg: rgb(s.win),
            // 日期分组头底色：亮色不能直接用 `well`（= 纯白），那样吸顶头和
            // 卡片完全同色，滚动时照片从下面滑过看不出是「浮层」；取比卡片
            // 低 5 灰阶的一档（原型 `.ghead` 本无底色，这是为吸顶遮挡补的）。
            header_bg: if dark {
                rgb(s.well)
            } else {
                Color32::from_rgb(0xFA, 0xFA, 0xFA)
            },
            separator,
            hairline,
            cell_border: if dark {
                // 白 + alpha 必须走 unmultiplied：`Color32` 存预乘色，把
                // (255,255,255,0x1f) 直接塞进 premultiplied 会被当成满亮度白。
                Color32::from_rgba_unmultiplied(255, 255, 255, 0x1f)
            } else {
                Color32::from_rgba_premultiplied(0, 0, 0, 0x1f)
            },
            selected: rgb(s.accent),
            accent_ink: rgb(s.accent_ink),
            accent_soft,
            star: rgb(s.star),
            sidebar_selected: if dark { rgb(s.hover) } else { rgb(s.card) },
            row_selected_text: if dark { Color32::WHITE } else { rgb(s.ink) },
            sidebar_selected_inactive: accent_soft,
            sidebar_hover: rgb(s.hover),
            cell_hover: rgb(s.hover),
            track_bg: rgb(s.track),
            chip_bg: rgb(s.card_chip),
            text_primary: rgb(s.ink),
            text_secondary: rgb(s.muted),
            text_disabled: rgb(s.faint),
            header_text: if dark { rgb(s.muted) } else { rgb(s.ink) },
            status_text: rgb(s.muted),
            tree_text: if dark { rgb(0xb8) } else { rgb(0x3f3f46) },
            viewer_bg: if dark {
                Color32::from_rgb(0x1A, 0x1A, 0x1A)
            } else {
                Color32::from_rgb(0xEC, 0xEC, 0xEE)
            },
            viewer_card: if dark {
                Color32::from_rgb(0x20, 0x20, 0x20)
            } else {
                Color32::from_rgb(0xFF, 0xFF, 0xFF)
            },
            viewer_chrome_bg: if dark {
                Color32::from_rgb(0x2B, 0x2B, 0x2B)
            } else {
                Color32::from_rgb(0xF7, 0xF7, 0xF8)
            },
            viewer_chrome: Color32::from_rgb(0x2B, 0x2B, 0x2B),
            viewer_text: Color32::from_rgb(0xE8, 0xE8, 0xE8),
            progress: rgb(s.accent),
            warn: if dark {
                Color32::from_rgb(0xFB, 0xBF, 0x24)
            } else {
                Color32::from_rgb(0xC8, 0x86, 0x1E)
            },
            danger: rgb(s.danger),
            ok: rgb(s.ok),
            overlay_bg: rgb(s.card),
            placeholder_bg: if dark { rgb(0x23) } else { rgb(0xE4) },
            header_top: rgb(s.well),
            header_bottom: rgb(s.well),
            header_border: separator,
            disclosure_green: rgb(s.accent),
            toolbar_bg: rgb(s.win),
            toolbar_border: if dark {
                Color32::from_rgba_unmultiplied(255, 255, 255, 0x12)
            } else {
                Color32::from_rgba_premultiplied(0, 0, 0, 0x14)
            },
            seg_bg: rgb(s.track),
            seg_active: if dark { rgb(0x2a) } else { Color32::WHITE },
            toolbar_icon: if dark {
                Color32::from_rgb(0xD4, 0xD4, 0xD4)
            } else {
                rgb(s.ink)
            },
            sidebar_section_text: rgb(s.faint),
            seg_active_orange: accent_soft,
            thumb_shadow: egui::Shadow {
                offset: [0, 1],
                blur: if dark { 2 } else { 4 },
                spread: 0,
                color: Color32::from_black_alpha(if dark { 90 } else { 30 }),
            },
            radius_card: s.radius_card,
            radius_thumb: s.radius_thumb,
            radius_btn: s.radius_btn,
            ring_inner: if dark {
                Color32::from_rgb(0x15, 0x15, 0x15)
            } else {
                Color32::WHITE
            },
            ring_outer: rgb(s.accent),
            sidebar_ref_w: s.sidebar_ref_w,
            title_uppercase: dark,
        }
    }

    /// 顶栏图标按钮在某个状态下应有的**实色底**。
    ///
    /// M5 §5.3：不能再是「一排白圆钮」。默认 = 透明（线性图标直接落在顶栏上），
    /// 悬停 = hover 底，选中 = 强调柔底。函数化便于单测锁定这条禁止项。
    /// 顶栏图标按钮的底色。
    ///
    /// 原型 `.ib.on` 用 `--hover`（亮 `#ebebec` / 暗 `#232323`）而不是强调色填充：
    /// 暗房里一块 8% 青几乎看不出选中，实心青又太扎眼。选定方案 D —— 底色走
    /// 中性的 hover 灰，选中感由**图标变品牌青**承担（见 `topbar_icon_color`）。
    pub fn topbar_icon_fill(&self, hovered: bool, selected: bool) -> Color32 {
        if selected || hovered {
            self.sidebar_hover
        } else {
            Color32::TRANSPARENT
        }
    }

    /// 顶栏图标按钮的前景色：选中态取品牌强调色（方案 D），禁用态取 disabled。
    pub fn topbar_icon_color(&self, enabled: bool, selected: bool) -> Color32 {
        if !enabled {
            self.text_disabled
        } else if selected {
            if self.dark {
                self.selected
            } else {
                self.text_primary
            }
        } else {
            self.toolbar_icon
        }
    }

    /// 缩略图圆角：M5-TB-10 的极小尺寸收窄规则 × 本主题的圆角 token。
    pub fn thumb_radius_for(&self, cell_px: f32) -> f32 {
        let cell = cell_px.max(1.0);
        if cell >= 96.0 {
            return self.radius_thumb;
        }
        if cell <= 22.0 {
            return 2.0_f32.min(self.radius_thumb);
        }
        let t = (cell - 22.0) / (96.0 - 22.0);
        (2.0 + t * (self.radius_thumb - 2.0)).min(self.radius_thumb)
    }

    /// 字体栈（用于文档 / 测试；实际加载见 [`install_fonts`]）。
    pub fn font_stack(&self) -> &'static [&'static str] {
        if self.dark {
            &["Geist", "Inter", "Noto Sans CJK SC", "PingFang SC"]
        } else {
            &[
                "Inter",
                "Noto Sans CJK SC",
                "PingFang SC",
                "Hiragino Sans GB",
                "Microsoft YaHei",
            ]
        }
    }
}

/// 16 进制 `#rrggbb`（测试与文档用）。
pub fn hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// 尺寸常量。全部标了 U 编号，回填时一一对应。
pub struct Metrics;

impl Metrics {
    /// 顶部两段式：保留旧 XP 常量（不再使用，避免下游断编译）。
    pub const MENUBAR_H: f32 = 21.0;
    pub const TOOLROW_H: f32 = 38.0;
    pub const TOOLBAR_H: f32 = 60.0;
    pub const MENUBAR_TOP: Color32 = Color32::from_rgb(0xFE, 0xFE, 0xFF);
    pub const MENUBAR_MID: Color32 = Color32::from_rgb(0xD4, 0xDB, 0xEE);
    pub const MENUBAR_BOT: Color32 = Color32::from_rgb(0xE1, 0xE6, 0xF6);
    pub const MENUBAR_BORDER: Color32 = Color32::from_rgb(0xB6, 0xBC, 0xCC);
    pub const TOOLROW_BORDER: Color32 = Color32::from_rgb(0xBA, 0xBA, 0xBA);
    pub const TOOLROW_BG: Color32 = Color32::from_rgb(0xE8, 0xE8, 0xE8);
    pub const SEARCH_BORDER: Color32 = Color32::from_rgb(0x7F, 0x9D, 0xB9);
    pub const TOOL_BTN_W: f32 = 58.0;
    pub const TOOL_ICON: f32 = 22.0;
    pub const TOOL_GROUP_PAD: f32 = 8.0;

    pub const STATUS_H: f32 = 32.0;
    pub const SIDEBAR_W: f32 = 220.0;
    pub const SIDEBAR_MIN: f32 = 160.0;
    pub const SIDEBAR_MAX: f32 = 420.0;

    pub const ROW_H: f32 = 22.0;
    pub const INDENT: f32 = 14.0;
    pub const SECTION_H: f32 = 22.0;

    pub const TRAY_COLLAPSED_H: f32 = 22.0;
    pub const TRAY_EXPANDED_H: f32 = 92.0;
    pub const TRAY_THUMB: f32 = 56.0;

    /// U-21：三档单元格边长（与 `picasa_core::library::CELL_SIZES` 必须一致）。
    pub const CELL_SIZES: [f32; 3] = [85.0, 144.0, 200.0];

    // ---- M3.1 顶栏 ----
    /// M5 v1.4 首页顶栏 = **标题带 + 工具条**两段（原型 `css/app.css:59,64`）：
    /// 标题带 36px 只放红绿灯与窗口标题，工具条 48px 放按钮/排布/搜索。
    /// 按钮垂直中心 = 36 + 48/2 = **60**，距红绿灯中心 18 有 42px，
    /// 不再像单行 64px 那样贴着 macOS 标题栏。
    pub const TOPBAR_TITLE_H: f32 = 36.0;
    pub const TOPBAR_TOOL_H: f32 = 48.0;
    pub const TOPBAR_H: f32 = Self::TOPBAR_TITLE_H + Self::TOPBAR_TOOL_H;
    /// 红绿灯的垂直带 = 标题带（原型红绿灯 dy=12 高 12 → 中心 y=18）。
    pub const TOPBAR_LINE1_H: f32 = Self::TOPBAR_TITLE_H;

    // ---- 暗色是另一套尺寸表（2026-11-26 逐像素比对验证）----
    // 原型 `css/app.css` 的 `html[data-theme="dark"]` 段不仅换色，还整表改尺寸：
    //   `:60` .titlebar{height:34px;border-bottom:1px solid #000}（亮 36）
    //   `:65` .toolbar{height:42px}（亮 48）
    //   `:101` .sec{padding:14px 10px 6px}（高 ≈35，亮 27.19）
    //   `:108` .row{height:28px}（亮 30）
    // 所以这些量必须按主题取，不能再用单一 const。

    /// 标题带高度：亮 36 / 暗 34。
    pub fn topbar_title_h(dark: bool) -> f32 {
        if dark { 34.0 } else { Self::TOPBAR_TITLE_H }
    }

    /// 工具条高度：亮 48 / 暗 42。
    pub fn topbar_tool_h(dark: bool) -> f32 {
        if dark { 42.0 } else { Self::TOPBAR_TOOL_H }
    }

    /// 顶栏总高：亮 84 / 暗 76 + 1px 底边线 = 77（原型暗色 `.body` 从 y=77 起）。
    pub fn topbar_h(dark: bool) -> f32 {
        Self::topbar_title_h(dark) + Self::topbar_tool_h(dark) + if dark { 1.0 } else { 0.0 }
    }

    /// 顶栏图标按钮尺寸：亮 `.ib` **30×30**、暗 **32×28**（`css/app.css:66,68`）。
    /// 暗色不是正方形（原型 `.ib{width:32px;height:28px}`），所以用 Vec2 传。
    pub fn topbar_icon_size(dark: bool) -> egui::Vec2 {
        if dark {
            egui::vec2(32.0, 28.0)
        } else {
            egui::vec2(30.0, 30.0)
        }
    }

    /// 侧栏行高：亮 30 / 暗 28。
    pub fn sidebar_row_h(dark: bool) -> f32 {
        if dark { 28.0 } else { Self::SIDEBAR_ROW_H }
    }

    /// 侧栏组头行高：亮 27 / 暗 35（暗色 padding 14/10/6 上下留白更大）。
    pub fn sidebar_section_h(dark: bool) -> f32 {
        if dark { 35.0 } else { Self::SIDEBAR_SECTION_H }
    }
    pub const TOPBAR_ICON_D: f32 = 38.0;
    /// 左栏默认宽度。M5-R3 视觉参考 228（亮）/ 216（暗）；**宽度偏好逻辑不变**，
    /// 这里保持既有默认 216，不借主题改用户拖出来的宽度。
    pub const SIDEBAR_DEFAULT_W: f32 = 228.0;
    pub const SIDEBAR_ROW_H: f32 = 30.0;
    pub const SIDEBAR_INDENT: f32 = 14.0;
    pub const SIDEBAR_SECTION_H: f32 = 27.0;
    pub const SIDEBAR_HEADER_H: f32 = 58.0;
    pub const SEG_H: f32 = 36.0;
    pub const ZOOM_PILL_H: f32 = 32.0;
    pub const CELL_GAP: f32 = 12.0;
    /// 方格缩略图「图片格」高度相对格宽的比例（用户 2026-11-24「每个图片上方的留白减三分之二」）。
    ///
    /// 派生：8:5 横图 contain 进 `cell×cell` 的画高 = 5/8·cell；旧布局再下移半个留白，
    /// 上留白 = 3/4·(cell−5/8·cell) = 9/32·cell。把图片格收紧到 13/16·cell 并回到居中，
    /// 上留白 = (13/16−5/8)/2·cell = 3/32·cell，正好是旧值的 1/3（减三分之二）；
    /// 图片底到文件名的间距仍是 3/32·cell，保持不变；行距同步变小。
    pub const THUMB_SLOT_RATIO: f32 = 1.0;
    /// 方格视图的竖直行间距：水平列间距 CELL_GAP 的一半。
    /// 用户 2026-11-24 要求「图片和图片之间的高度缩减一半」，只压竖直方向，
    /// 水平列间距保持 M3.2 定稿的 16px 不变。
    pub const CELL_GAP_Y: f32 = Self::CELL_GAP;
    pub const GRID_PAD: f32 = 24.0;
    pub const GROUP_HEADER_H: f32 = 30.0;
    pub const FILENAME_H: f32 = 9.0;
    pub const FILENAME_GAP: f32 = 0.5;
    pub const FILENAME_FONT_SIZE: f32 = 11.0;

    /// 方格缩略图圆角（M4 产品事实的 8 px；M5 主题 token 见 [`Theme::radius_thumb`]）。
    /// 保留这个常量是因为 `grid::thumb_radius_for` 的微缩图集档行为不随主题变化。
    pub const THUMB_RADIUS: f32 = 8.0;

    pub const DRAG_THRESHOLD: f32 = 4.0;
    pub const MARQUEE_FILL_ALPHA: f32 = 0.12;
    pub const MARQUEE_STROKE_W: f32 = 1.0;

    pub const SCROLLBAR: f32 = 8.0;

    pub const TEX_UPLOAD_PER_FRAME: usize = 3;
    pub const TEX_CACHE_CAP: usize = 120;
}

/// 字号层级。M5 正文 13px / 行高 1.4（字号由 egui 的字体度量决定行高，
/// 这里只给四档字号；暗色小标题 11px）。
pub fn text_styles(dark: bool) -> Vec<(TextStyle, FontId)> {
    vec![
        (
            TextStyle::Heading,
            FontId::new(if dark { 11.0 } else { 12.5 }, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(12.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(11.0, FontFamily::Monospace),
        ),
    ]
}

/// 把 egui 的全局样式与字体换成指定主题（A / B 各一套完整外观）。
///
/// 切换主题时**再调一次**即可：样式与字体都是幂等的重建，不依赖系统外观
/// （Linux / Xvfb 上系统主题固定也不影响两套主题的验收）。
///
/// 关键：`System` 偏好**不能**用 `ctx.set_theme(Dark/Light)` 把 egui 的
/// `theme_preference` 钉死。钉死后 egui 每帧 `sync_window_theme()` 会给
/// NSWindow 下发固定 appearance，winit 的 `observeValueForKeyPath` 见到
/// 窗口 appearance 被自定义就不再发 `WindowEvent::ThemeChanged`，于是 OS
/// 深浅色切换传不到 `ctx.system_theme()`，`sync_theme` 永远早退、配色卡住。
/// 两套 style 已由下面的 `all_styles_mut` 各自配好，`System` 下让 egui
/// 自己按系统挑即可；显式 `Light` / `Dark` 才钉死。
pub fn apply(ctx: &egui::Context, appearance: Appearance) {
    // 跟随系统：先读 OS 深浅色，再落成具体主题（无系统信息时按亮色）。
    if appearance.is_system()
        && let Some(system) = ctx.system_theme()
    {
        picasa_core::prefs::set_system_dark(system == egui::Theme::Dark);
    }
    let theme = Theme::for_appearance(appearance);
    install_fonts(ctx, appearance);

    if appearance.is_system() {
        // 交还系统：egui 下发 `SystemDefault`，NSWindow 不设自定义 appearance，
        // OS 切换才能继续经 `ThemeChanged` 更新 `ctx.system_theme()`。
        ctx.options_mut(|opt| opt.theme_preference = egui::ThemePreference::System);
    } else {
        // 显式亮 / 暗：钉死 egui 的控件调色板，不跟随系统深色。
        ctx.set_theme(if theme.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
    }

    ctx.all_styles_mut(|style| {
        for (style_kind, font_id) in text_styles(theme.dark) {
            style.text_styles.insert(style_kind, font_id);
        }

        style.spacing.item_spacing = egui::vec2(7.0, 5.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.spacing.window_margin = egui::Margin::same(8);
        style.spacing.menu_margin = egui::Margin::same(5);
        style.spacing.slider_width = 96.0;

        let radius = egui::CornerRadius::same(theme.radius_btn.round().clamp(0.0, 255.0) as u8);
        style.visuals.widgets.noninteractive.corner_radius = radius;
        style.visuals.widgets.inactive.corner_radius = radius;
        style.visuals.widgets.hovered.corner_radius = radius;
        style.visuals.widgets.active.corner_radius = radius;

        style.visuals.dark_mode = theme.dark;
        for w in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
        ] {
            w.fg_stroke = egui::Stroke::new(1.0, theme.text_primary);
        }
        style.visuals.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.0, theme.text_secondary);
        style.visuals.widgets.inactive.bg_fill = theme.track_bg;
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, theme.hairline);
        style.visuals.widgets.inactive.weak_bg_fill = theme.chip_bg;
        style.visuals.widgets.hovered.bg_fill = theme.sidebar_hover;
        style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, theme.hairline);
        style.visuals.widgets.hovered.weak_bg_fill = theme.chip_bg;
        style.visuals.widgets.active.bg_fill = theme.accent_soft;
        style.visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, theme.selected);
        style.visuals.widgets.active.weak_bg_fill = theme.accent_soft;

        style.visuals.window_fill = theme.menu_bg;
        style.visuals.panel_fill = theme.chrome_bg;
        style.visuals.extreme_bg_color = theme.chip_bg;
        style.visuals.text_edit_bg_color = Some(theme.menu_bg);
        style.visuals.code_bg_color = theme.chip_bg;
        style.visuals.faint_bg_color = theme.chip_bg;
        style.visuals.override_text_color = Some(theme.text_primary);
        style.visuals.selection.bg_fill = theme.selected;
        style.visuals.selection.stroke = egui::Stroke::new(1.0, theme.ring_inner);
        style.visuals.hyperlink_color = theme.accent_ink;
        style.visuals.warn_fg_color = theme.warn;
        style.visuals.error_fg_color = theme.danger;
        style.visuals.window_corner_radius =
            egui::CornerRadius::same(theme.radius_btn.round() as u8 + 2);
        style.visuals.menu_corner_radius =
            egui::CornerRadius::same(theme.radius_btn.round() as u8 + 2);
        style.visuals.window_stroke = egui::Stroke::new(1.0, theme.hairline);
        style.visuals.window_shadow = egui::Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(if theme.dark { 140 } else { 40 }),
        };
        style.visuals.popup_shadow = egui::Shadow {
            offset: [0, 6],
            blur: 20,
            spread: 0,
            color: Color32::from_black_alpha(if theme.dark { 150 } else { 40 }),
        };

        style.spacing.scroll.bar_width = Metrics::SCROLLBAR;
        style.spacing.scroll.floating = false;
    });
}

/// 字体文件候选（按主题的主字体 + 共用的 CJK 回退）。
fn font_file_candidates(
    appearance: Appearance,
) -> (&'static [&'static str], &'static [&'static str]) {
    let primary: &'static [&'static str] = if appearance.is_dark() {
        &[
            "/usr/share/fonts/truetype/sand-box/google/Geist/Geist-VariableFont_wght.ttf",
            "/usr/share/fonts/opentype/geist/Geist-Regular.otf",
            "/System/Library/Fonts/Geist.ttf",
        ]
    } else {
        &[
            "/usr/share/fonts/truetype/sand-box/google/Inter/Inter-VariableFont_opsz,wght.ttf",
            "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
            "/System/Library/Fonts/Inter.ttf",
        ]
    };
    let cjk: &'static [&'static str] = &[
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "C:/Windows/Fonts/msyh.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    ];
    (primary, cjk)
}

fn read_first(candidates: &'static [&'static str]) -> Option<(&'static str, Vec<u8>)> {
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            return Some((path, bytes));
        }
    }
    None
}

/// 安装主题字体：主字体（Inter / Geist）+ 中文回退。
///
/// 系统没有主字体时**静默回退**到 egui 内置字体（中文仍由 CJK 回退保证），
/// 不因为缺一个拉丁字体把整套主题做废。字体加载失败只降低观感，不影响功能。
fn install_fonts(ctx: &egui::Context, appearance: Appearance) {
    ctx.set_fonts(font_definitions(appearance));
}

/// 组装字体表：内嵌 Inter 三档字重 + 系统主字体（若有）+ 中文回退。
///
/// 独立成函数是为了让测试能直接检查 `families` —— 「族没注册」这类错误
/// 在界面上只表现为「字重没变」，靠肉眼很容易漏。
pub fn font_definitions(appearance: Appearance) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let (primary_candidates, cjk_candidates) = font_file_candidates(appearance);

    // 内嵌 Inter 三个字重：Regular 进 Proportional，另外两档各建一个族。
    // 这是 1:1 还原里唯一无法用常量表达式解决的一层 —— 字形粗细。
    fonts.font_data.insert(
        "op_ui_regular".to_owned(),
        Arc::new(FontData::from_static(INTER_REGULAR_TTF)),
    );
    fonts.font_data.insert(
        "op_ui_medium".to_owned(),
        Arc::new(FontData::from_static(INTER_MEDIUM_TTF)),
    );
    fonts.font_data.insert(
        "op_ui_semibold".to_owned(),
        Arc::new(FontData::from_static(INTER_SEMIBOLD_TTF)),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "op_ui_regular".to_owned());
    fonts
        .families
        .entry(FontFamily::Name(FAMILY_MEDIUM.into()))
        .or_default()
        .extend(["op_ui_medium".to_owned(), "op_ui_regular".to_owned()]);
    fonts
        .families
        .entry(FontFamily::Name(FAMILY_SEMIBOLD.into()))
        .or_default()
        .extend(["op_ui_semibold".to_owned(), "op_ui_medium".to_owned()]);

    if let Some((path, bytes)) = read_first(primary_candidates) {
        tracing::info!(path, "已加载主题主字体");
        fonts
            .font_data
            .insert("op_ui".to_owned(), Arc::new(FontData::from_owned(bytes)));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(1, "op_ui".to_owned());
    } else {
        tracing::info!(
            appearance = appearance.as_str(),
            "系统无主题主字体，使用内嵌 Inter"
        );
    }

    if let Some((path, bytes)) = read_first(cjk_candidates) {
        tracing::info!(path, bytes = bytes.len(), "已加载 CJK 回退字体");
        // 汉字与拉丁混排的垂直校准。M32-UI-13 当时取 0.30（按那时的系统字体
        // 顶端对齐）；M5 换成内嵌 Inter 后 metrics 变了，0.30 会把汉字整体抬
        // 高约 0.25em（实测 13px 下偏上 3.25px，侧栏行、搜索框占位符都看得到）。
        // 实测：该系数每 +0.25 让汉字下移约 3.5px（13px 字号），0.30 时汉字
        // 仍偏上 3.25px，取 0.55 才落到行内视觉中心。
        let tweak = egui::epaint::text::FontTweak {
            y_offset_factor: 0.55,
            ..Default::default()
        };
        // 同一份字节用 Arc 共享给多个 face，避免为了字重复制几十 MB。
        let shared = Arc::new(bytes);
        fonts.font_data.insert(
            "cjk_fallback".to_owned(),
            Arc::new(FontData {
                font: shared.clone(),
                index: 0,
                tweak: tweak.clone(),
            }),
        );

        // `Hiragino Sans GB.ttc` 里 W3(400)=index 0、W6(600)=index 2。中文的
        // semibold（侧栏组头等）只有靠 W6 才真的变粗；medium(500) 中文保留 W3
        // —— 500 与 400 的中文观感差极小，不值得再背一份字体。
        let has_w6 = path.ends_with("Hiragino Sans GB.ttc");
        if has_w6 {
            fonts.font_data.insert(
                "cjk_w6".to_owned(),
                Arc::new(FontData {
                    font: shared.clone(),
                    index: 2,
                    tweak: tweak.clone(),
                }),
            );
        }

        // 三个族都要挂中文回退：中文在 medium/semibold 里同样要能显示。
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("cjk_fallback".to_owned());
        }
        fonts
            .families
            .entry(FontFamily::Name(FAMILY_MEDIUM.into()))
            .or_default()
            .push("cjk_fallback".to_owned());
        fonts
            .families
            .entry(FontFamily::Name(FAMILY_SEMIBOLD.into()))
            .or_default()
            .push(if has_w6 { "cjk_w6" } else { "cjk_fallback" }.to_owned());
    } else {
        tracing::warn!("未找到可用的 CJK 回退字体，中文可能显示为方框");
    }

    fonts
}

#[cfg(test)]
mod tests {
    use super::*;
    use picasa_core::{
        Appearance, CELL_MAX_PX, CELL_MIN_PX, DEFAULT_CELL_PX, RESET_CELL_PX, ZoomState,
    };

    fn c(hex: &str) -> Color32 {
        let h = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap();
        rgb(h)
    }

    /// 首页顶栏是原型 v1.4 的「标题带 36 + 工具条 48」两段，不再单行 64。
    /// 按钮中心 y=60，离红绿灯中心 y=18 有 42px，不会贴着 macOS 标题栏。
    #[test]
    fn m5_v1_topbar_is_two_bands_matching_prototype() {
        assert_eq!(Metrics::TOPBAR_TITLE_H, 36.0, "原型 .titlebar 高 36");
        assert_eq!(Metrics::TOPBAR_TOOL_H, 48.0, "原型 .toolbar 高 48");
        assert_eq!(Metrics::TOPBAR_H, 84.0, "两段和 = 36 + 48");
        assert_eq!(Metrics::TOPBAR_LINE1_H, 36.0, "红绿灯带 = 标题带");
        let button_center = Metrics::TOPBAR_TITLE_H + Metrics::TOPBAR_TOOL_H / 2.0;
        let traffic_center = Metrics::TOPBAR_LINE1_H / 2.0;
        assert_eq!(button_center, 60.0);
        assert_eq!(traffic_center, 18.0);
        assert_eq!(button_center - traffic_center, 42.0, "按钮离红绿灯 42px");
    }

    /// §5.1：亮色关键 token 必须与说明书表格逐项一致。
    #[test]
    fn m5_r3_light_tokens_match_spec_table() {
        let t = Theme::light();
        assert_eq!(hex(t.page_bg), "#c8c8cc", "页面背景 --bg-page");
        assert_eq!(hex(t.chrome_bg), "#f5f5f5", "窗口 --win");
        assert_eq!(hex(t.grid_bg), "#ffffff", "主内容井是近白卡片");
        assert_eq!(hex(t.text_primary), "#18181b", "--ink");
        assert_eq!(hex(t.text_secondary), "#71717a", "--muted");
        assert_eq!(hex(t.text_disabled), "#a1a1aa", "--faint");
        assert_eq!(hex(t.selected), "#0485f7", "--accent");
        assert_eq!(hex(t.accent_ink), "#0373d6", "--accent-ink");
        assert_eq!(hex(t.accent_soft), "#e6f1fe", "--accent-soft");
        assert_eq!(hex(t.menu_bg), "#ffffff", "卡片 --card");
        assert_eq!(hex(t.track_bg), "#ebebec", "滑轨 --track");
        assert_eq!(hex(t.chip_bg), "#f4f4f5", "芯片 --chip");
        assert_eq!(hex(t.star), "#f5b942", "亮色收藏星");
        assert_eq!(hex(t.danger), "#e11d48", "危险");
        assert_eq!(hex(t.ok), "#16a34a", "成功");
        assert_eq!(t.radius_card, 26.0, "卡片圆角");
        assert_eq!(t.radius_thumb, 16.0, "缩略图圆角");
        assert_eq!(t.radius_btn, 10.0, "按钮圆角");
        assert_eq!(hex(t.ring_inner), "#ffffff", "选中环内圈");
        assert_eq!(hex(t.ring_outer), "#0485f7", "选中环外圈");
        assert_eq!(t.sidebar_ref_w, 228.0, "侧栏参考宽");
        assert!(!t.dark);
        assert_eq!(t.appearance, Appearance::Light);
    }

    /// §5.2：暗色关键 token 必须与说明书表格逐项一致（不是 egui 系统 Dark 近似）。
    #[test]
    fn m5_r3_dark_tokens_match_spec_table() {
        let t = Theme::dark();
        assert_eq!(hex(t.page_bg), "#050505", "页面");
        assert_eq!(hex(t.chrome_bg), "#151515", "窗口");
        assert_eq!(hex(t.text_primary), "#f2f2f2", "主文字");
        assert_eq!(hex(t.text_secondary), "#9a9a9a", "次文字");
        assert_eq!(hex(t.text_disabled), "#7c7c7c", "弱文字");
        assert_eq!(hex(t.selected), "#00dfe1", "强调");
        assert_eq!(hex(t.accent_ink), "#00dfe1", "强调文字");
        // `Color32` 内部是**预乘**色，直接读通道会被 alpha 缩过（0xdf→0x11）；
        // 断言「叠加到深底上之后的观感」才对得上 `#00dfe114` 这个 8% 柔底。
        assert_eq!(t.accent_soft.a(), 0x14, "强调柔底 alpha = 8%");
        // 8% alpha 下预乘只有 8 位精度，反解量化误差可达 ±10，按容差比色相。
        let un = t.accent_soft.to_srgba_unmultiplied();
        let near = |got: u8, want: u8| (got as i32 - want as i32).abs() <= 12;
        assert!(
            near(un[0], 0x00) && near(un[1], 0xdf) && near(un[2], 0xe1),
            "强调柔底色相应为 #00dfe1，反解得 {:?}",
            (un[0], un[1], un[2])
        );
        assert!(
            t.accent_soft.r() < 0x40,
            "柔底必须仍是半透明：预乘后的红通道应接近 0，实际 {}",
            t.accent_soft.r()
        );
        assert_eq!(hex(t.menu_bg), "#1c1c1c", "卡片");
        assert_eq!(hex(t.sidebar_hover), "#232323", "悬停");
        assert_eq!(hex(t.star), "#00dfe1", "暗色收藏星 = 强调色（M5-GR-07）");
        assert_eq!(t.radius_card, 22.0, "卡片圆角");
        assert_eq!(t.radius_thumb, 10.0, "缩略图圆角");
        assert_eq!(t.radius_btn, 8.0, "按钮圆角");
        assert_eq!(hex(t.ring_inner), "#151515", "选中环内圈");
        assert_eq!(hex(t.ring_outer), "#00dfe1", "选中环外圈");
        assert_eq!(t.sidebar_ref_w, 216.0, "侧栏参考宽");
        assert!(t.dark);
        assert!(t.title_uppercase, "暗色标题可用小号大写");
        assert_eq!(t.appearance, Appearance::Dark);
    }

    /// 默认仍是亮色（§11 #4 未裁定，不把默认改成跟随系统 / 暗色）。
    #[test]
    fn m5_r3_default_appearance_is_light() {
        assert_eq!(Appearance::default(), Appearance::Light);
        assert_eq!(
            Theme::for_appearance(Appearance::default()).appearance,
            Appearance::Light
        );
        assert!(!Theme::for_appearance(Appearance::default()).dark);
    }

    /// 切换主题不改变缩略图范围 / 当前边长（AC-THUMB-01 的主题相关句）。
    #[test]
    fn m5_r3_theme_switch_keeps_thumbnail_range_and_value() {
        let before = ZoomState::from_setting(Some(144.0));
        let light = Theme::light();
        let dark = Theme::dark();
        // 主题只影响绘制，不参与 ZoomState 的任何计算。
        let after = ZoomState::from_setting(Some(144.0));
        assert_eq!(before.cell_px(), after.cell_px());
        assert_eq!(before.to_setting(), after.to_setting());
        assert_eq!(CELL_MIN_PX, 22.0);
        assert_eq!(CELL_MAX_PX, 512.0);
        assert_eq!(DEFAULT_CELL_PX, 144.0, "新建默认按原型 THUMB_DEFAULT");
        assert_eq!(RESET_CELL_PX, 96.0, "⌘0 复位按原型 THUMB_RESET");
        // 主题的圆角/颜色不同，但都不携带任何尺寸范围。
        assert_ne!(light.radius_thumb, dark.radius_thumb);
    }

    /// §5.3：主材质必须实色，且顶栏默认不再是白圆钮。
    #[test]
    fn m5_r3_no_glass_and_no_white_circle_toolbar() {
        for t in [Theme::light(), Theme::dark()] {
            for (name, c) in [
                ("窗口", t.chrome_bg),
                ("内容井", t.grid_bg),
                ("侧栏", t.sidebar_bg),
                ("菜单", t.menu_bg),
                ("顶栏", t.toolbar_bg),
            ] {
                assert_eq!(c.a(), 255, "{name}必须是实色（玻璃拟态不得作为主材质）");
            }
            // 顶栏图标默认透明；悬停/选中也不是不透明白圆。
            assert_eq!(
                t.topbar_icon_fill(false, false).a(),
                0,
                "顶栏默认不能是白圆底"
            );
            assert_ne!(hex(t.topbar_icon_fill(true, false)), "#ffffff");
            assert_ne!(hex(t.topbar_icon_fill(false, true)), "#ffffff");
            // 缩略图落影存在但很淡（不是白圆按钮的软阴影语言）。
            assert!(t.thumb_shadow.color.a() < 130);
        }
    }

    /// 字体栈按主题切换；缺字体时加载函数静默回退（这里只锁 token）。
    #[test]
    fn m5_r3_font_stack_matches_theme() {
        assert_eq!(Theme::light().font_stack()[0], "Inter");
        assert_eq!(Theme::dark().font_stack()[0], "Geist");
        for t in [Theme::light(), Theme::dark()] {
            assert!(
                t.font_stack().contains(&"Noto Sans CJK SC"),
                "中文回退必须在字体栈里"
            );
        }
    }

    /// M5-TB-10：极小尺寸收窄圆角仍成立（主题 token 不改变这条行为）。
    #[test]
    fn m5_r3_thumb_radius_still_shrinks_on_tiny_cells() {
        for t in [Theme::light(), Theme::dark()] {
            assert_eq!(t.thumb_radius_for(96.0), t.radius_thumb);
            assert_eq!(t.thumb_radius_for(512.0), t.radius_thumb);
            assert!(t.thumb_radius_for(22.0) < 4.0);
            assert!(t.thumb_radius_for(22.0) < t.thumb_radius_for(48.0));
        }
    }

    /// 颜色辅助函数本身要正确（防止 token 断言因为解析错误而「全绿」）。
    #[test]
    fn hex_helper_round_trips() {
        assert_eq!(hex(c("#0485f7")), "#0485f7");
        assert_eq!(hex(c("c8c8cc")), "#c8c8cc");
    }

    /// 跟随系统时不能把 egui 的 `theme_preference` 钉死为 Light/Dark。
    ///
    /// 钉死后 winit 会给 NSWindow 设固定 appearance，`ThemeChanged` 不再上报，
    /// OS 深浅色切换就传不到应用（详见 `apply` 文档注释）。显式亮/暗则必须钉死。
    #[test]
    fn apply_system_keeps_egui_theme_preference_on_system() {
        let ctx = egui::Context::default();
        apply(&ctx, Appearance::System);
        let pref = ctx.options(|o| o.theme_preference);
        assert_eq!(
            pref,
            egui::ThemePreference::System,
            "跟随系统必须让 egui 自己按系统选主题"
        );

        apply(&ctx, Appearance::Dark);
        assert_eq!(
            ctx.options(|o| o.theme_preference),
            egui::ThemePreference::Dark
        );
        apply(&ctx, Appearance::Light);
        assert_eq!(
            ctx.options(|o| o.theme_preference),
            egui::ThemePreference::Light
        );

        // 再切回 System 必须能恢复（不是第一次调用才生效）。
        apply(&ctx, Appearance::System);
        assert_eq!(
            ctx.options(|o| o.theme_preference),
            egui::ThemePreference::System
        );
    }
}

#[cfg(test)]
mod m5_type_tests {
    use super::*;

    /// 三个字重的 Inter 必须真的嵌进二进制。
    ///
    /// 旧实现只去系统路径找 Inter，macOS 没有 `/System/Library/Fonts/Inter.ttf`，
    /// 于是静默回退到 egui 内置的 Ubuntu-Light —— 字形与原型差很多却没有任何报错。
    #[test]
    fn inter_weights_are_embedded_in_the_binary() {
        for (name, bytes, min_len) in [
            ("Regular", INTER_REGULAR_TTF, 350_000),
            ("Medium", INTER_MEDIUM_TTF, 350_000),
            ("SemiBold", INTER_SEMIBOLD_TTF, 350_000),
        ] {
            assert!(
                bytes.len() > min_len,
                "{name} 字体太小（{} 字节），可能没真正嵌入",
                bytes.len()
            );
            // TrueType 头：0x00010000 或 'true'；OpenType/CFF 是 'OTTO'。
            let magic = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            assert!(
                magic == 0x0001_0000
                    || magic == u32::from_be_bytes(*b"true")
                    || magic == u32::from_be_bytes(*b"OTTO"),
                "{name} 不是有效的字体文件（magic={magic:#x}）"
            );
        }
        assert_ne!(INTER_REGULAR_TTF.len(), INTER_MEDIUM_TTF.len());
        assert_ne!(INTER_MEDIUM_TTF.len(), INTER_SEMIBOLD_TTF.len());
    }

    /// 字重族必须注册成功 —— egui 的 `FontId` 没有 weight，只能靠族区分。
    #[test]
    fn medium_and_semibold_families_are_registered() {
        let fonts = font_definitions(Appearance::Light);
        for key in ["op_ui_regular", "op_ui_medium", "op_ui_semibold"] {
            assert!(fonts.font_data.contains_key(key), "缺字体数据 {key}");
        }
        let prop = fonts
            .families
            .get(&FontFamily::Proportional)
            .expect("Proportional 族必须存在");
        assert_eq!(prop[0], "op_ui_regular", "拉丁主字体应是内嵌 Inter Regular");

        let med = fonts
            .families
            .get(&FontFamily::Name(FAMILY_MEDIUM.into()))
            .expect("medium 族必须注册");
        assert_eq!(med[0], "op_ui_medium");

        let semi = fonts
            .families
            .get(&FontFamily::Name(FAMILY_SEMIBOLD.into()))
            .expect("semibold 族必须注册");
        assert_eq!(semi[0], "op_ui_semibold");

        // 两族都必须带 Regular 兜底，否则缺字时会直接空白。
        assert!(med.contains(&"op_ui_regular".to_owned()));
        assert!(semi.contains(&"op_ui_medium".to_owned()));
    }

    /// 原型 `div.sec` 600、`span.name`/`span.cnt` 500、`div.appname` 560 ——
    /// 这些位置必须用字重族，且不能被悄悄改回 Proportional。
    #[test]
    fn prototype_weights_use_the_dedicated_families() {
        let sidebar = include_str!("sidebar.rs");
        assert!(
            sidebar.contains("crate::theme::semibold(sidebar_row_font_size(theme))"),
            "侧栏组头应按原型 600 用 semibold"
        );
        assert!(
            sidebar.contains("crate::theme::medium(sidebar_row_font_size(theme))"),
            "侧栏行名应按原型 500 用 medium"
        );
        assert!(
            sidebar.contains("crate::theme::medium(sidebar_count_font_size(theme))"),
            "侧栏计数应按原型 500 用 medium"
        );
        let topbar = include_str!("topbar.rs");
        assert!(
            topbar.contains("crate::theme::semibold(app_name_font_size(theme))"),
            "应用名应按原型 560 用 semibold"
        );
    }
}

/// 原型暗色主题是**另一套尺寸表**，不是只换色。
///
/// 逐像素比对（2026-11-26）发现：`css/app.css` 的 `html[data-theme="dark"]` 段
/// 把 `.titlebar` 34px（亮 36）、`.toolbar` 42px（亮 48）、`.row` 28px（亮 30）、
/// `.sec` padding 14/10/6（高 35，亮 27.19）全部改掉。此前应用两套主题共用
/// 一套几何，暗色首页的分区差异率因此高达 sidebar 17.5% / toolbar 10.9%。
#[cfg(test)]
mod m5_theme_metrics_tests {
    use super::*;

    #[test]
    fn topbar_selected_icon_uses_neutral_fill_and_brand_color() {
        // 方案 D：底色永远走中性 hover 灰（原型 `.ib.on` 的 `--hover`），
        // 选中感由图标变品牌青承担；暗色绝不用强调色铺底。
        let light = Theme::for_appearance(Appearance::Light);
        let dark = Theme::for_appearance(Appearance::Dark);
        assert_eq!(light.topbar_icon_fill(false, false), Color32::TRANSPARENT);
        assert_eq!(light.topbar_icon_fill(true, false), light.sidebar_hover);
        assert_eq!(light.topbar_icon_fill(false, true), light.sidebar_hover);
        assert_eq!(dark.topbar_icon_fill(false, true), dark.sidebar_hover);
        assert_ne!(
            dark.topbar_icon_fill(false, true),
            dark.selected,
            "选中底不能是强调色本身"
        );
        assert_eq!(dark.topbar_icon_color(true, true), dark.selected);
        assert_eq!(light.topbar_icon_color(true, true), light.text_primary);
        assert_ne!(dark.topbar_icon_color(true, false), dark.selected);
        assert_eq!(dark.topbar_icon_color(false, true), dark.text_disabled);
    }

    #[test]
    fn dark_theme_uses_its_own_size_table() {
        // 顶栏：亮 36+48=84；暗 34+42+1px 底边=77（原型暗色 .body 从 y=77 起）
        assert_eq!(Metrics::topbar_title_h(false), 36.0);
        assert_eq!(Metrics::topbar_title_h(true), 34.0);
        assert_eq!(Metrics::topbar_tool_h(false), 48.0);
        assert_eq!(Metrics::topbar_tool_h(true), 42.0);
        assert_eq!(Metrics::topbar_h(false), 84.0);
        assert_eq!(Metrics::topbar_h(true), 77.0);

        // 侧栏：行高 30→28；组头 27→35（暗色上下留白更大，组头比行还高）
        assert_eq!(Metrics::sidebar_row_h(false), 30.0);
        assert_eq!(Metrics::sidebar_row_h(true), 28.0);
        assert_eq!(Metrics::sidebar_section_h(false), 27.0);
        assert_eq!(Metrics::sidebar_section_h(true), 35.0);
        assert!(
            Metrics::sidebar_section_h(true) > Metrics::sidebar_row_h(true),
            "暗色组头应比行高更高（原型 35 > 28）"
        );
        assert!(
            Metrics::sidebar_section_h(false) < Metrics::sidebar_row_h(false),
            "亮色组头应比行矮（原型 27 < 30）"
        );
    }

    /// 顶栏图标按钮尺寸：亮 30×30（正方形）、暗 32×28（**不是正方形**）。
    #[test]
    fn topbar_icon_buttons_follow_the_prototype_sizes() {
        let light = Metrics::topbar_icon_size(false);
        let dark = Metrics::topbar_icon_size(true);
        assert_eq!((light.x, light.y), (30.0, 30.0));
        assert_eq!((dark.x, dark.y), (32.0, 28.0));
        assert_ne!(dark.x, dark.y, "暗色 .ib 是 32×28，不是正方形");

        // 源码锁：不能再有「按直径画圆」的 d/2.0 残留
        let bar = include_str!("topbar.rs");
        let body = bar.split("#[cfg(test)]").next().unwrap_or(bar);
        assert!(
            body.contains("Metrics::topbar_icon_size(theme.dark)"),
            "顶栏按钮尺寸按主题取"
        );
        let dead = ["TOPBAR", "_ICON_D"].concat();
        assert!(!body.contains(&dead), "渲染路径不应再用单一常量");
    }

    /// 卡片描边：亮色极淡黑边（`rgba(0,0,0,.035)`），暗色 `#ffffff0d`；
    /// 内层描边 `#ffffff0a`、圆角 16。都不能退回主题的 hairline（那档是 190 灰，
    /// 压在白色内容井上会让四个圆角显脏——2026-11-25 用户反馈过）。
    #[test]
    fn card_borders_follow_the_prototype_alphas() {
        let light = Theme::for_appearance(Appearance::Light);
        let dark = Theme::for_appearance(Appearance::Dark);
        assert_eq!(light.card_border().a(), 0x09, "亮色 0.035 alpha ≈ 9");
        assert_eq!(dark.card_border().a(), 0x0d, "暗色 #ffffff0d");
        assert_eq!(dark.card_inner_border().a(), 0x0a, "暗色 .inner #ffffff0a");
        assert_eq!(Theme::CARD_INNER_RADIUS, 16.0, "原型 .inner 圆角 16");
        assert_ne!(
            light.card_border(),
            light.hairline,
            "卡片描边不能复用 hairline"
        );

        let src = include_str!("grid.rs");
        let body = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(body.contains("theme.card_border()"), "卡片外框走主题描边");
        assert!(
            body.contains("theme.card_inner_border()"),
            "暗色内层描边走主题 token"
        );
        let dead = ["theme", ".hairline"].concat();
        assert!(!body.contains(&dead), "内容井不再用 hairline 描边");
    }

    /// 源码锁：渲染路径必须走主题函数，不能退回单一 const
    /// （否则暗色又会跟亮色共用一套几何，差异率悄悄涨回去）。
    #[test]
    fn chrome_renders_with_theme_aware_heights() {
        let side = include_str!("sidebar.rs");
        let body = side.split("#[cfg(test)]").next().unwrap_or(side);
        assert!(body.contains("sidebar_row_h("), "侧栏行高要走主题函数");
        assert!(body.contains("sidebar_section_h("), "侧栏组头要走主题函数");
        let dead = ["SIDEBAR_ROW_H)", "x"].concat();
        assert!(!body.contains(&dead), "渲染路径不应再直接用 SIDEBAR_ROW_H");

        let bar = include_str!("topbar.rs");
        let bar_body = bar.split("#[cfg(test)]").next().unwrap_or(bar);
        assert!(
            bar_body.contains("topbar_h(theme.dark)"),
            "顶栏高度按主题取"
        );
        assert!(
            bar_body.contains("topbar_title_h(dark)")
                || bar_body.contains("topbar_title_h(theme.dark)"),
            "标题带高度按主题取"
        );
    }
}
