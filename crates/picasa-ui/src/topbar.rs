//! M5 v1.4 首页顶栏 = **标题带 36 + 工具条 48** 两段（原型 `css/app.css:59,64`）：
//! 标题带只放红绿灯（中心 y=18），工具条放左段操作图标、中段路径 + 张数/拍摄时间、
//! 右簇排布/搜索/排序/筛选/…（按钮中心 y=60，离红绿灯 42px，不再贴 macOS 标题栏）。
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────────────┐
//! │ ●●●                                                                          │  标题带 36
//! ├──────────────────────────────────────────────────────────────────────────────┤
//! │ [侧栏][导入][幻灯片][编辑][♥][ⓘ][⌗][▣] │ 图库 / 旅行 / 京都 · 22 张 · 拍摄时间：… │ [方格|纵瀑|横瀑][搜索框][⇅][≡][…] │  工具条 48
//! └──────────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! 关键约定（`docs/10` M32-UI-01/02 v1.1，M5 v1.4 修订）：
//!
//! * 中段：**双语境面包屑**——文件夹语境画「图库 / 祖先… / 当前」
//!   （祖先段可点击跳转），根语境弱化为单段「图库」；其后接
//!   「N 张 · 拍摄时间：起—止」（H1）。
//! * 左段 = 侧栏开关 + 导入/幻灯片/编辑（对象操作），四枚对象钮紧随其后；
//! * 右簇（靠右、从左到右）= 排布胶囊 → **常驻搜索框** → 排序 → 筛选 → …（H2/H3）；
//! * 排布是**无文字三图标连体胶囊**，选中片为**淡橙圆**（定稿截图采样）；
//! * **M5-TB-10**：顶栏缩略图 −/+ 连同占位一起退役，缩略图尺寸改到底栏
//!   最右侧的线性滑块（22–512 px，step=1）。
//!
//! 排序 / 筛选 / 「…」保留为右侧图标组：它们是既有功能，不能在版式重排里丢。

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Response, Sense, Ui, Vec2, pos2, vec2,
};
use picasa_core::ViewMode;
use picasa_db::SortKey;

use crate::icons::{self, Icon};
use crate::theme::{Metrics, Theme};

/// 首页工具条里按钮/文本的**垂直中心**（窗口相对 y，egui 点）。
///
/// M5 v1.4 原型 `css/app.css:59,64` 把首页顶栏拆成「标题带 36 + 工具条 48」两段，
/// 红绿灯在标题带中心 y=18，按钮在工具条中心 y=36+24=60。两者相距 42px，
/// 不再像旧版单行 64px（按钮 y=32、离红绿灯仅 18px）那样贴着 macOS 标题栏。
/// 暗色是另一套尺寸表（标题带 34 + 工具条 42），所以按主题取，见 [`Metrics::topbar_title_h`]。
/// 查看器是另一套布局（`viewer::layout`，`TOP_H=44`），不走这里。
pub fn toolbar_center_y(rect: Rect, dark: bool) -> f32 {
    rect.top() + Metrics::topbar_title_h(dark) + Metrics::topbar_tool_h(dark) / 2.0
}

/// 首页标题带里 egui 自绘应用名 `OpenPicasa` 的中心点（窗口相对坐标）。
///
/// macOS 隐藏了 AppKit 原生标题（`viewport_builder().with_title_shown(false)`），
/// 应用名改由这里自绘——对齐原型 `css/app.css` 的 `.appname`（标题带居中、12px）。
/// 红绿灯在标题带左端（x≈0..78），故文字水平居中于整窗时不会与它重叠。
/// 查看器走 `viewer::layout`，不调用本函数，按原型不画应用名。
pub fn app_name_center(rect: Rect, dark: bool) -> Pos2 {
    pos2(
        rect.left() + rect.width() / 2.0,
        rect.top() + Metrics::topbar_title_h(dark) / 2.0,
    )
}

/// 在标题带自绘应用名（仅 macOS——其余平台 AppKit 原生标题仍在）。
/// 顶栏应用名的字号：原型 `.titlebar .appname` 亮色 12px / 暗色 11px。
pub fn app_name_font_size(theme: &Theme) -> f32 {
    if theme.dark { 11.0 } else { 12.0 }
}

fn draw_app_name(painter: &egui::Painter, rect: Rect, theme: &Theme) {
    painter.text(
        app_name_center(rect, theme.dark),
        Align2::CENTER_CENTER,
        "OpenPicasa",
        // 原型 `div.appname{font-weight:560}`，取更近的 600。
        crate::theme::semibold(app_name_font_size(theme)),
        theme.text_secondary,
    );
}

/// 顶栏产出的动作，由 App 执行（顶栏自己不碰状态）。
#[derive(Clone, Debug, PartialEq)]
pub enum TopbarAction {
    /// 侧栏开关
    ToggleSidebar,
    /// 导入 = 添加关注文件夹（既有 `add_folder` 流程）
    AddFolder,
    /// 占位入口（幻灯片 / 编辑）：App 用 toast 说明，不做假功能
    Placeholder(&'static str),
    /// 视图模式三选一（原「视图」菜单的三项）
    PickViewMode(ViewMode),
    /// 打开排序弹出菜单（由 App 画，位置取按钮 rect）
    ToggleSortMenu,
    /// 选择排序键（同键再点 = 翻转方向，App 处理）
    PickSortKey(SortKey),
    /// 翻转排序方向
    ToggleSortDirection,
    /// 切换「有缩略图的排在前面」（只影响显示顺序）
    ToggleThumbsFirst,
    /// 打开筛选面板（原「筛选」按钮）
    ToggleFilterMenu,
    /// 打开或关闭「…」弹出菜单
    ToggleMoreMenu,
    /// 「…」菜单里的动作（见 [`more_menu`]）
    More(MoreAction),
    /// 面包屑祖先段点击：跳到对应文件夹（顶栏自己不碰状态）
    PickFolder(i64),
    /// 展开/收起顶栏搜索框（搜索按钮紧接在 `+` 之后）
    ToggleSearch,
    /// 搜索框内容变化
    SearchChanged(String),
    /// 点了搜索框左侧的范围小按钮（当前文件夹 / 全部图库）
    ToggleSearchScope,
    // ---- M4.1 `M41-UI-01`：工具栏对象操作（作用于当前选中集）----
    /// 收藏 / 取消收藏（双态，由 App 按选中集判定）
    Favorite,
    /// 打开「照片说明」窗口（多选版）
    Caption,
    /// 打开「批量标签」浮层
    Tag,
    /// 加入 / 移出照片托盘（双态）
    Tray,
}

/// 「…」菜单里的真实功能项。
///
/// M5-MN-01：`AddFolder` 已删除——更多菜单不再提供「添加文件夹…」，
/// 添加关注文件夹只走顶栏「导入」按钮（索引进度空态与设置壳也走同一个
/// [`TopbarAction::AddFolder`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoreAction {
    ToggleRecurse,
    ToggleFullscreen,
    About,
    /// 方格模式下显示/隐藏文件名（§7.4）
    ToggleShowFilename,
    /// M5-IA-04：打开设置页（⌘,）。
    Settings,
    /// M5-IA-02：直达五标签索引管理窗口。
    IndexManager,
}

pub struct TopbarOutput {
    pub actions: Vec<TopbarAction>,
    /// 排序按钮的矩形（App 用它定位弹出菜单）
    pub sort_button: Option<Rect>,
    /// 筛选按钮的矩形（App 用它定位面板）
    pub filter_button: Option<Rect>,
    /// 「…」按钮的矩形（App 用它定位弹出菜单）
    pub more_button: Option<Rect>,
}

/// M4.1：四个对象操作按钮的**双态判定**（`docs/13` §1 M41-UI-01）。
///
/// 由 App 每帧从内存 `lib.selection` / `lib.tray` 计算，顶栏只负责画，不查库。
#[derive(Clone, Copy, Debug, Default)]
pub struct ObjectActionState {
    /// 有选中照片
    pub has_selection: bool,
    /// 选中集全部已收藏 → 收藏按钮为「选中态」，点击=取消收藏
    pub all_favorite: bool,
    /// 选中集全部已在托盘 → 托盘按钮为「选中态」，点击=移出托盘
    pub all_in_tray: bool,
}

/// M4.1：四枚对象操作钮的矩形（♥ 收藏 / ⓘ 说明 / ⌗ 标签 / ▣ 托盘）。
///
/// 放在「编辑」右侧，组间留白 12px、组内 6px，按钮尺寸恒为 `d`（不缩小）。
/// 窗口最窄 900 时由 `show` 把**中段右移**让位（左段优先），而不是压缩按钮。
/// 纯函数，便于单测。
fn object_button_rects(edit_right: f32, center_y: f32, size: Vec2) -> [Rect; 4] {
    const GROUP_GAP: f32 = 12.0;
    const INNER_GAP: f32 = 6.0;
    let first_center = edit_right + GROUP_GAP + size.x / 2.0;
    std::array::from_fn(|i| {
        Rect::from_center_size(
            pos2(first_center + i as f32 * (size.x + INNER_GAP), center_y),
            size,
        )
    })
}

/// 顶栏右簇（H3）：**排布胶囊 → 搜索框 → 排序 → 筛选 → 更多**。
///
/// 胶囊紧贴搜索框左侧、整簇靠右（v1.4 定稿）；搜索框常驻可见（H2），
/// 宽度随窗口收窄，但不会低于 170 px。
fn topbar_right_rects(rect: Rect, size: Vec2, dark: bool) -> (Rect, Rect, Rect, Rect, Rect) {
    let cy = toolbar_center_y(rect, dark);
    let margin = 14.0;
    let circle_at =
        |right_x: f32| -> Rect { Rect::from_center_size(pos2(right_x - size.x / 2.0, cy), size) };
    let more = circle_at(rect.right() - margin);
    let filter = circle_at(more.left() - 4.0);
    let sort = circle_at(filter.left() - 4.0);
    let search_w = (rect.width() * 0.20).clamp(170.0, 260.0);
    let search = Rect::from_min_size(
        pos2(sort.left() - 12.0 - search_w, cy - 16.0),
        vec2(search_w, 32.0),
    );
    let layout_w = 3.0 * 34.0;
    let layout = Rect::from_min_size(
        pos2(
            search.left() - 10.0 - layout_w,
            cy - Metrics::ZOOM_PILL_H / 2.0,
        ),
        vec2(layout_w, Metrics::ZOOM_PILL_H),
    );
    (layout, search, sort, filter, more)
}

/// 顶栏入参。
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    theme: &Theme,
    // 双语境面包屑：`(名称, folder_id)`；根段 id 为 None，最后一段是当前语境
    breadcrumb: &[(String, Option<i64>)],
    // 张数与拍摄时间起止（H1，如 `22 张 · 拍摄时间：2023-11-02—2024-05-21`）
    meta: &str,
    // 排布当前值
    view_mode: ViewMode,
    // 排序（按钮上要显示当前排序的语义）
    sort: picasa_db::SortSpec,
    // 筛选是否有激活条件（按钮高亮）
    filter_active: bool,
    // 搜索
    search: &mut String,
    _search_open: bool,
    search_in_current: bool,
    // 左栏当前宽度（M32-UI-01/02 v1.3：左段右缘与文件夹树右缘对齐）
    sidebar_w: f32,
    // M4.1：四个对象操作按钮的双态判定（App 从内存选中集算好传入）
    obj: ObjectActionState,
) -> TopbarOutput {
    let mut out = TopbarOutput {
        actions: Vec::new(),
        sort_button: None,
        filter_button: None,
        more_button: None,
    };
    let rect = Rect::from_min_size(
        ui.max_rect().left_top(),
        vec2(ui.max_rect().width(), Metrics::topbar_h(theme.dark)),
    );
    let _ = ui.allocate_space(rect.size());
    let painter = ui.painter_at(rect);
    // M4.1 v1.7：用户裁决「按钮样式回退到 M4」——玻璃整套（模糊底图/折射/
    // 色散/水波纹）移除，顶栏恢复 M4 的纯色底。
    painter.rect_filled(rect, 0.0, theme.toolbar_bg);
    // H8：macOS 隐藏了 AppKit 原生标题，应用名在标题带自绘（居中，避开红绿灯）。
    // 查看器不经过这里，按原型 .appname{display:none} 不画；其余平台保留原生标题。
    #[cfg(target_os = "macos")]
    draw_app_name(&painter, rect, theme);
    // 用户 2026-11-25：取消顶栏底边的灰色发丝线。
    // 顶栏与内容井之间已经由「窗底色 vs 白井」的明度差分开，再加一道线显得脏；
    // 原型 `.toolbar` 虽然有 `border-bottom`，但产品里内容井是浮起的圆角卡片，
    // 那道线会横穿卡片顶部两个圆角，是「圆角被遮挡」观感的主要来源。

    // 单行顶栏：左段图标 · 中段（路径 + 张数/拍摄时间）· 右簇（排布/搜索/排序/筛选/…）。
    // macOS 红绿灯嵌在这一行的左端，所以左段起点让出 `traffic_w`。
    #[cfg(target_os = "macos")]
    let traffic_w = 78.0;
    #[cfg(not(target_os = "macos"))]
    let traffic_w = 0.0;

    let size = Metrics::topbar_icon_size(theme.dark);
    let margin = 14.0;
    let cy = toolbar_center_y(rect, theme.dark);

    // ---- 右簇（从右往左）：… 筛选 排序 搜索 排布 ----
    let (layout_rect, search_rect, sort_r, filter_r, more_r) =
        topbar_right_rects(rect, size, theme.dark);
    out.more_button = Some(more_r);
    out.filter_button = Some(filter_r);
    out.sort_button = Some(sort_r);

    // 原型 `.toolbar{border-bottom:1px solid var(--line)}`：**整宽** 1px、`rgba(0,0,0,.06)`
    // （在 245 窗体上得 230，实测原型 y=83 整行 230，连侧栏上方那一段也有）。
    // 第十三轮曾把这条线整条删掉——那是因为当时画的是 190 灰的 `toolbar_border`，
    // 与原型这条极淡的界不是一回事；这里按原型的淡线还原。
    let line_color = if theme.dark {
        Color32::BLACK
    } else {
        Color32::from_rgba_unmultiplied(0, 0, 0, 15)
    };
    painter.line_segment(
        [
            pos2(rect.left(), rect.bottom() - 0.5),
            pos2(rect.right(), rect.bottom() - 0.5),
        ],
        egui::Stroke::new(1.0, line_color),
    );

    // ---- 左段（从左往右）：侧栏开关 导入 …… 幻灯片 编辑 ----
    // M32-UI-01/02 v1.4：
    // * 侧栏开关、导入贴窗口最左（红绿灯在上方的标题带，单行顶栏不必让位；
    //   `traffic_w` 只在把顶栏压到标题带时兜底）；
    // * 幻灯片、编辑两个按钮的**左沿**（幻灯片的 left）= 侧栏右缘，
    //   即与文件夹树的最右侧**始终保持对齐**——按钮组骑在侧栏边界右侧，
    //   侧栏拖宽时同步移动；侧栏隐藏时退回默认宽度。
    let left_x = rect.left() + traffic_w + margin;
    let side_r = Rect::from_center_size(pos2(left_x + size.x / 2.0, cy), size);
    let import_r = Rect::from_center_size(pos2(side_r.right() + 6.0 + size.x / 2.0, cy), size);
    // 幻灯片左沿 = 侧栏右缘（Clamp 防窄侧栏时与导入重叠）
    let seg_left = (rect.left() + sidebar_w).max(import_r.right() + 6.0);
    let slide_r = Rect::from_center_size(pos2(seg_left + size.x / 2.0, cy), size);
    let edit_r = Rect::from_center_size(pos2(slide_r.right() + 6.0 + size.x / 2.0, cy), size);
    // M4.1 `M41-UI-01`（用户 #4 定稿）：四枚对象操作钮放在「编辑」之后。
    // 纯函数算 rect，便于单测「不与右簇重叠」（窗口最窄 900 时也要守住）。
    let obj_rects = object_button_rects(edit_r.right(), cy, size);
    let [fav_r, cap_r, tag_r, tray_r] = obj_rects;

    // ---- ① 侧栏开关 ----
    if icon_btn(
        ui,
        &painter,
        side_r,
        Icon::SidebarToggle,
        theme,
        true,
        "显示/隐藏边栏",
    )
    .clicked()
    {
        out.actions.push(TopbarAction::ToggleSidebar);
    }

    // ---- ② 导入 / 幻灯片 / 编辑（无文字圆形图标）----
    // 导入是真实入口；幻灯片 / 编辑是**可见未接通**（`M5-TB-03`～04 /
    // `AC-XX-UNCONNECTED`），两者的悬停与点击反馈都由
    // [`unconnected_entries`] 提供，避免文案散落两处。
    let (slide_tip, slide_msg) = unconnected_entries()[0];
    let (edit_tip, edit_msg) = unconnected_entries()[1];
    let action_btns: &[(Rect, Icon, &'static str, Option<TopbarAction>)] = &[
        (
            import_r,
            Icon::Import,
            // M5-IM-01 / M5-TB-02：顶栏导入是添加关注文件夹的主入口。
            // 打开的是系统选目录面板。
            "导入：添加照片文件夹…",
            Some(TopbarAction::AddFolder),
        ),
        (
            slide_r,
            Icon::Slideshow,
            slide_tip,
            Some(TopbarAction::Placeholder(slide_msg)),
        ),
        (
            edit_r,
            Icon::Edit,
            edit_tip,
            Some(TopbarAction::Placeholder(edit_msg)),
        ),
    ];
    for (r, icon, tip, action) in action_btns {
        if icon_btn(ui, &painter, *r, *icon, theme, true, tip).clicked()
            && let Some(action) = action
        {
            out.actions.push(action.clone());
        }
    }

    // ---- ②′ M4.1 四枚对象操作钮（编辑之后）----
    // 双态判定由 App 传入 `obj`；`has_selection=false` 时按钮仍可点（App 弹 toast）。
    let fav_tip = if obj.all_favorite && obj.has_selection {
        "取消收藏"
    } else {
        "收藏"
    };
    let tray_tip = if obj.all_in_tray && obj.has_selection {
        "移出照片托盘"
    } else {
        "照片托盘 · ⌘H"
    };
    let obj_btns: &[(Rect, Icon, &str, TopbarAction, bool)] = &[
        (
            fav_r,
            Icon::Heart,
            fav_tip,
            TopbarAction::Favorite,
            obj.all_favorite && obj.has_selection,
        ),
        (cap_r, Icon::Caption, "说明", TopbarAction::Caption, false),
        (tag_r, Icon::Tag, "标签", TopbarAction::Tag, false),
        (
            tray_r,
            Icon::Tray,
            tray_tip,
            TopbarAction::Tray,
            obj.all_in_tray && obj.has_selection,
        ),
    ];
    for (r, icon, tip, action, selected) in obj_btns {
        if icon_btn_ex(ui, &painter, *r, *icon, theme, true, *selected, tip).clicked() {
            out.actions.push(action.clone());
        }
    }
    // 组分隔虚线（提示「对象操作组」结束，不影响交互）
    painter.line_segment(
        [
            pos2(tray_r.right() + 8.0, cy - 10.0),
            pos2(tray_r.right() + 8.0, cy + 10.0),
        ],
        egui::Stroke::new(1.0, Color32::from_black_alpha(24)),
    );

    // ---- ③ 中段：双语境路径 + 张数 / 拍摄时间起止（H1）----
    // 位置固定在「对象操作组」与右簇的排布胶囊之间，窄窗口下截断而不是压按钮。
    let crumb_left = tray_r.right() + 18.0;
    let crumb_right = layout_rect.left() - 16.0;
    if crumb_right > crumb_left + 24.0 {
        draw_breadcrumb(
            ui,
            &painter,
            Rect::from_min_max(pos2(crumb_left, cy - 12.0), pos2(crumb_right, cy + 12.0)),
            theme,
            breadcrumb,
            meta,
            &mut out,
        );
    }

    // ---- ④ 排布：无文字三图标连体胶囊（右簇最左，紧贴搜索）----
    draw_layout_pill(ui, &painter, layout_rect, theme, view_mode, &mut out);

    // ---- ⑤ 搜索：常驻可见的圆角输入框（H2），不依赖 search_open ----
    draw_search(
        ui,
        &painter,
        search_rect,
        theme,
        search,
        search_in_current,
        &mut out,
    );

    // ---- ⑥ 排序 / 筛选 / 更多 ----
    let sort_resp = icon_btn(
        ui,
        &painter,
        sort_r,
        Icon::SortArrows,
        theme,
        true,
        match sort.key {
            SortKey::TakenAt => {
                if sort.descending {
                    "排序：拍摄日期（新在前）"
                } else {
                    "排序：拍摄日期（旧在前）"
                }
            }
            SortKey::Name => {
                if sort.descending {
                    "排序：文件名（Z→A）"
                } else {
                    "排序：文件名（A→Z）"
                }
            }
            SortKey::ModifiedAt => "排序：修改时间",
            SortKey::Size => "排序：文件大小",
            SortKey::Dimensions => "排序：图片尺寸",
        },
    );
    if sort_resp.clicked() {
        out.actions.push(TopbarAction::ToggleSortMenu);
    }

    let f_resp = icon_btn(
        ui,
        &painter,
        filter_r,
        Icon::ListLines,
        theme,
        true,
        "筛选（日期 / 类型 / 尺寸 / 标签）",
    );
    if f_resp.clicked() {
        out.actions.push(TopbarAction::ToggleFilterMenu);
    }
    if filter_active {
        // 激活态：白圆上压一圈选中色描边（橙色留给排布选中片，不混用）
        painter.rect_stroke(
            filter_r,
            theme.radius_btn,
            egui::Stroke::new(1.5, theme.selected),
            egui::StrokeKind::Inside,
        );
    }

    if icon_btn(ui, &painter, more_r, Icon::Ellipsis, theme, true, "更多").clicked() {
        out.actions.push(TopbarAction::ToggleMoreMenu);
    }

    out
}

/// 双语境路径 + 张数 / 拍摄时间起止（H1）。
///
/// * 文件夹语境：`图库 / 祖先… / 当前`——根段与分隔符用次级灰，路径段用主色，
///   祖先段可点击（悬停变选中色 + 手型光标）；
/// * 路径后接 `· 22 张 · 拍摄时间：…`（次级灰，不参与点击）；
/// * 空间不足时先截断元信息，再截断路径段，绝不压到两侧按钮。
fn draw_breadcrumb(
    ui: &mut Ui,
    painter: &egui::Painter,
    area: Rect,
    theme: &Theme,
    breadcrumb: &[(String, Option<i64>)],
    meta: &str,
    out: &mut TopbarOutput,
) {
    let font = FontId::new(12.5, egui::FontFamily::Proportional);
    let meta_font = FontId::new(11.5, egui::FontFamily::Proportional);
    let mut x = area.left();
    let cy = area.center().y;
    // 主标题的**基线**，副标题要贴同一条线（galley 底边含 descent，不能拿来对齐）。
    let mut baseline = cy + font.size * crate::theme::BASELINE_RATIO_ASCENT;
    let n = breadcrumb.len();
    for (i, (name, folder_id)) in breadcrumb.iter().enumerate() {
        let current = i + 1 == n;
        let remaining = area.right() - x;
        if remaining < 12.0 {
            return;
        }
        let color = if current || folder_id.is_some() {
            theme.text_primary
        } else {
            theme.text_secondary
        };
        let text = fit_text(painter, name, &font, remaining);
        let galley = painter.layout_no_wrap(text, font.clone(), color);
        // 垂直居中按 galley 实际高度算，不再用 `cy - 10.0` 这种硬编码：
        // 汉字回退字形在 galley 里偏上，固定半高会让整段文字浮起来约 5px。
        let seg_top = cy - galley.size().y / 2.0;
        let seg_rect = Rect::from_min_size(pos2(x, seg_top), galley.size());
        // 主标题的基线：后面的「· 40 张 · 拍摄时间」要贴同一条线。
        baseline = seg_top + font.size * crate::theme::BASELINE_RATIO_ASCENT;
        painter.galley(pos2(x, seg_top), galley, color);

        // 祖先段（非当前、有 id）：整段可点
        if let Some(id) = folder_id
            && !current
        {
            let hot = seg_rect.expand(4.0);
            let resp = ui.interact(hot, ui.scope_id().with(("crumb", *id)), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                painter.rect_filled(hot, 6.0, theme.sidebar_hover);
                let hover_galley =
                    painter.layout_no_wrap(name.clone(), font.clone(), theme.selected);
                painter.galley(
                    pos2(x, cy - hover_galley.size().y / 2.0),
                    hover_galley,
                    theme.selected,
                );
            }
            if resp.clicked() {
                out.actions.push(TopbarAction::PickFolder(*id));
            }
            resp.clone().on_hover_text(format!("转到「{name}」"));
        }

        x = seg_rect.right() + 6.0;
        if !current {
            // 分隔符「/」
            painter.text(
                pos2(x, cy),
                Align2::LEFT_CENTER,
                "/",
                font.clone(),
                theme.text_disabled,
            );
            x += painter
                .layout_no_wrap("/".to_string(), font.clone(), theme.text_primary)
                .size()
                .x
                + 6.0;
        }
    }

    // 张数 / 拍摄时间起止（缺 EXIF 时 App 只给张数，不编造日期）。
    if meta.is_empty() || area.right() - x < 24.0 {
        return;
    }
    let sep = "  ·  ";
    let sep_w = painter
        .layout_no_wrap(sep.to_string(), meta_font.clone(), theme.text_secondary)
        .size()
        .x;
    if area.right() - x < sep_w + 24.0 {
        return;
    }
    painter.text(
        pos2(x, cy),
        Align2::LEFT_CENTER,
        sep,
        meta_font.clone(),
        theme.text_disabled,
    );
    let text = fit_text(painter, meta, &meta_font, area.right() - x - sep_w);
    // 副标题与主标题共用**基线**：两者字号不同（12.5 / 11.5），
    // 各自按中心对齐会让小字浮起，按 galley 底对齐则因 descent 不同而错开。
    let meta_top = baseline - meta_font.size * crate::theme::BASELINE_RATIO_ASCENT;
    painter.text(
        pos2(x + sep_w, meta_top),
        Align2::LEFT_TOP,
        text,
        meta_font,
        theme.text_secondary,
    );
}

/// 单行文本按可用宽度截断（超出补 `…`），保证不画到相邻控件上。
fn fit_text(painter: &egui::Painter, text: &str, font: &FontId, max_w: f32) -> String {
    let color = Color32::PLACEHOLDER;
    let full = painter.layout_no_wrap(text.to_string(), font.clone(), color);
    if full.size().x <= max_w {
        return text.to_string();
    }
    let ell = "…";
    let ell_w = painter
        .layout_no_wrap(ell.to_string(), font.clone(), color)
        .size()
        .x;
    let mut out = String::new();
    let mut w = 0.0_f32;
    for ch in text.chars() {
        let cw = painter
            .layout_no_wrap(ch.to_string(), font.clone(), color)
            .size()
            .x;
        if w + cw + ell_w > max_w {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push_str(ell);
    out
}

/// 圆形图标按钮（M5 §5.3）：**线性图标直接落在顶栏上**，不是一排白圆钮。
///
/// 默认底透明；悬停用主题 hover 底；选中用强调柔底。不再有软阴影 / 白圆底。
fn icon_btn(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    icon: Icon,
    theme: &Theme,
    enabled: bool,
    tooltip: &str,
) -> Response {
    icon_btn_ex(ui, painter, rect, icon, theme, enabled, false, tooltip)
}

/// 带「选中态」的图标按钮（♥ 收藏 / ▣ 托盘双态）。
///
/// M5：选中态用主题强调柔底（亮 `#e6f1fe` / 暗 `#00dfe114`），不再是白圆 + 淡橙。
#[allow(clippy::too_many_arguments)]
fn icon_btn_ex(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    icon: Icon,
    theme: &Theme,
    enabled: bool,
    selected: bool,
    tooltip: &str,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let resp = ui.interact(rect, ui.scope_id().with(("topbar-btn", tooltip)), sense);
    let radius = theme.radius_btn;
    let fill = theme.topbar_icon_fill(enabled && resp.hovered(), selected);
    if fill != Color32::TRANSPARENT {
        painter.rect_filled(rect, radius, fill);
    }
    if enabled && resp.hovered() {
        painter.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, theme.hairline),
            egui::StrokeKind::Inside,
        );
    }
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let color = theme.topbar_icon_color(enabled, selected);
    // 先取中心内接正方形再内缩：暗色按钮是 32×28，直接按宽度缩会让图标框变成
    // 14.1×10.1 —— Lucide 的 24×24 图形必然被压扁。
    let square = icons::icon_box(rect);
    let inner = square.shrink(square.width() * 0.28);
    // 收藏心形：选中（全部已收藏）时是实心（stroke 0），否则 1.4 描边 ——
    // 原型 `icon("heart", 15, allStarred ? 0 : 1.75, allStarred ? "currentColor" : "none")`。
    let icon_stroke = if selected && icon == Icon::Heart {
        0.0
    } else {
        1.4
    };
    icons::draw(painter, icon, inner, color, icon_stroke);
    if enabled {
        resp.clone().on_hover_text(tooltip);
    }
    resp
}

/// 排布胶囊：三枚无文字图标（方格 / 纵向瀑布 / 横向瀑布），**白底胶囊 + 软阴影**，
/// 选中片为**淡橙圆**（定稿效果图采样，`09` §7.2 像素校准）。
fn draw_layout_pill(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    theme: &Theme,
    current: ViewMode,
    out: &mut TopbarOutput,
) {
    // M5：排布胶囊 = 槽底 + 选中片，实色，无软阴影。
    let radius = rect.height() / 2.0;
    painter.rect_filled(rect, radius, theme.seg_bg);
    painter.rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, theme.hairline),
        egui::StrokeKind::Inside,
    );
    // 与 −|+ 同款中缝：未选中的两片之间有一条极淡的竖线。
    let w = rect.width() / 3.0;
    for i in 1..3 {
        let sx = rect.left() + i as f32 * w;
        painter.line_segment(
            [pos2(sx, rect.top() + 7.0), pos2(sx, rect.bottom() - 7.0)],
            egui::Stroke::new(1.0, theme.hairline),
        );
    }
    let modes = [
        (ViewMode::Grid, Icon::LayoutGrid, "方格"),
        (ViewMode::MasonryV, Icon::LayoutMasonryV, "纵向瀑布"),
        (ViewMode::MasonryH, Icon::LayoutMasonryH, "横向瀑布"),
    ];
    for (i, (mode, icon, label)) in modes.iter().enumerate() {
        let piece = Rect::from_min_size(
            pos2(rect.left() + i as f32 * w, rect.top()),
            vec2(w, rect.height()),
        );
        let active = *mode == current;
        if active {
            // M5 §5.1：选中片是主题的「白片 / 2a2a2a 片」，图标用强调色。
            let d = rect.height() - 6.0;
            let circle = Rect::from_center_size(piece.center(), vec2(d, d));
            painter.rect_filled(circle, d / 2.0, theme.seg_active);
        }
        let resp = ui.interact(piece, ui.scope_id().with(("layout", label)), Sense::click());
        let color = if active {
            theme.selected
        } else {
            theme.text_secondary
        };
        let inner = Rect::from_center_size(piece.center(), vec2(18.0, 18.0));
        icons::draw(painter, *icon, inner, color, 1.5);
        let resp = if resp.hovered() {
            resp.on_hover_text(*label)
        } else {
            resp
        };
        if resp.clicked() && !active {
            out.actions.push(TopbarAction::PickViewMode(*mode));
        }
    }
}

/// 顶栏搜索框：白底 + 细灰边 + 圆角；左端「文/全」范围钮，右端收起。
fn draw_search(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    theme: &Theme,
    search: &mut String,
    search_in_current: bool,
    out: &mut TopbarOutput,
) {
    // M5：搜索框 = 芯片底 + 发丝边（亮白 / 暗 #f4f4f5 对应的 chip 槽）。
    painter.rect_filled(rect, rect.height() / 2.0, theme.chip_bg);
    painter.rect_stroke(
        rect,
        rect.height() / 2.0,
        egui::Stroke::new(1.0, theme.hairline),
        egui::StrokeKind::Inside,
    );
    // 范围小字（当前文件夹 / 全库）：放在框内左端，点击切换
    let scope_w = 16.0;
    let scope_rect = Rect::from_min_size(
        pos2(rect.left() + 5.0, rect.top() + 3.0),
        vec2(scope_w, rect.height() - 6.0),
    );
    let scope_resp = ui.interact(
        scope_rect,
        ui.scope_id().with("search-scope"),
        Sense::click(),
    );
    painter.text(
        scope_rect.center(),
        Align2::CENTER_CENTER,
        if search_in_current { "文" } else { "全" },
        FontId::new(9.0, egui::FontFamily::Proportional),
        if scope_resp.hovered() {
            theme.selected
        } else {
            theme.text_secondary
        },
    );
    if scope_resp.clicked() {
        out.actions.push(TopbarAction::ToggleSearchScope);
    }
    scope_resp.clone().on_hover_text(if search_in_current {
        "搜索范围：当前文件夹。点击改为搜索全部图库"
    } else {
        "搜索范围：全部图库。点击改回当前文件夹"
    });

    // 左端放大镜（装饰；输入区从它右侧开始）
    let mag = Rect::from_center_size(
        pos2(scope_rect.right() + 10.0, rect.center().y),
        vec2(12.0, 12.0),
    );
    icons::draw(painter, Icon::Search, mag, theme.text_secondary, 1.2);

    let edit_rect = Rect::from_min_max(
        pos2(mag.right() + 4.0, rect.top() + 3.0),
        pos2(
            rect.right() - (if search.is_empty() { 8.0 } else { 24.0 }),
            rect.bottom() - 3.0,
        ),
    );
    let id = egui::Id::unique("topbar-search");
    let mut buf = search.clone();
    let resp = ui.put(
        edit_rect,
        egui::TextEdit::singleline(&mut buf)
            .id(id)
            .frame(egui::Frame::NONE)
            // 字形垂直位置由字体行框决定（`vertical_align` 已废弃且无效），
            // margin 只留水平内缩；汉字对齐靠 theme::TEXT_VISUAL_TWEAK 统一校准。
            .margin(egui::Margin::symmetric(2, 4))
            .hint_text("搜索")
            .text_color(theme.text_primary),
    );
    if resp.changed() {
        *search = buf.clone();
        out.actions.push(TopbarAction::SearchChanged(buf));
    }

    // 右端清空按钮（H2：搜索框常驻，这里不再「收起」，只清空搜索词）
    if !search.is_empty() {
        let clear =
            Rect::from_center_size(pos2(rect.right() - 12.0, rect.center().y), vec2(14.0, 14.0));
        let clear_resp = ui.interact(clear, ui.scope_id().with("search-clear"), Sense::click());
        if clear_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        icons::draw(
            painter,
            Icon::Close,
            clear.shrink(2.0),
            if clear_resp.hovered() {
                theme.toolbar_icon
            } else {
                theme.text_secondary
            },
            1.2,
        );
        if clear_resp.clicked() {
            search.clear();
            out.actions.push(TopbarAction::SearchChanged(String::new()));
        }
    }
}

/// 两个**可见但未接通**的顶栏入口：`(悬停文案, 点击后的 toast)`。
///
/// `M5-TB-03` / `M5-TB-04` / `M5-XX-01` / `AC-XX-UNCONNECTED`：幻灯片与编辑
/// 只能给「下一版开放」类反馈，**不得**变成可播放幻灯片或编辑台（两者都是
/// OUT-C / 非目标）。抽成纯函数是为了让这条口径能被单测直接钉住。
pub fn unconnected_entries() -> [(&'static str, &'static str); 2] {
    [
        ("幻灯片（下一版开放）", "幻灯片将在下一版开放"),
        ("编辑（下一版开放）", "编辑将在下一版开放"),
    ]
}

/// 「…」菜单的可见项（纯函数，便于单测锁定 M5 的入口集合）。
///
/// 顺序照 v1.4 壳：含子文件夹、显示文件名、设置…、索引管理…、全屏、关于。
/// **没有**「添加文件夹…」（M5-MN-01）；也没有「列表布局（演示扩展）」
/// 「恢复演示数据」「帮助与快捷键」「切换主题」——最后两项是 §11 #3 / #4 未决。
pub fn more_menu_items(
    recurse_on: bool,
    recurse_available: bool,
    show_filename: bool,
) -> Vec<(String, bool, MoreAction)> {
    vec![
        (
            if recurse_on {
                "✓ 只看本文件夹".to_string()
            } else {
                "含子文件夹".to_string()
            },
            recurse_available,
            MoreAction::ToggleRecurse,
        ),
        (
            if show_filename {
                "✓ 方格显示文件名".to_string()
            } else {
                "方格显示文件名".to_string()
            },
            true,
            MoreAction::ToggleShowFilename,
        ),
        ("设置…".to_string(), true, MoreAction::Settings),
        ("索引管理…".to_string(), true, MoreAction::IndexManager),
        ("全屏".to_string(), true, MoreAction::ToggleFullscreen),
        ("关于 OpenPicasa".to_string(), true, MoreAction::About),
    ]
}

/// 「更多」菜单的目标外框宽度（原型同菜单约 220 px）。
///
/// M5-R3 缺陷修复：这是**菜单自己的**宽度，不读 `ui.available_width()`——
/// 弹层不该跟着窗口/父容器剩余宽度走（旧实现被 `Area` 首帧的
/// `spacing.default_area_size` 600 px 撑成了 598 px 宽的白条）。
pub const MORE_MENU_WIDTH: f32 = 220.0;

/// 菜单 `Frame` 的内边距（四边，与绘制处 `Margin::same(6)` 同源）。
const MORE_MENU_INNER_MARGIN: f32 = 6.0;

/// 菜单一行的行高。
const MORE_MENU_ROW_H: f32 = 24.0;

/// `Area` 的固定 id：不随调用点嵌套层数变化，也便于测试/命中区回读实际矩形。
fn more_menu_area_id() -> egui::Id {
    egui::Id::unique("picasa.topbar-more-menu")
}

/// 已绘制「更多」菜单的实际矩形（未打开时 `None`）。
///
/// 供 `app.rs` 的「点空白处关闭」命中区使用：命中区必须等于菜单真实大小，
/// 不能写死一个比菜单窄的常量矩形。
pub fn more_menu_rect(ctx: &egui::Context) -> Option<Rect> {
    ctx.memory(|m| m.area_rect(more_menu_area_id()))
}

/// 按当前字体下**最长菜单项**算内容宽度；外框另加 [`MORE_MENU_INNER_MARGIN`]。
///
/// 下界是 [`MORE_MENU_WIDTH`] 对应的内容宽度（菜单观感不因文案短而发抖），
/// 上界由真实字形决定（字体缺失/回退变宽时宁可略宽，也不截断
/// 「方格显示文件名」「索引管理…」「关于 OpenPicasa」）。
fn more_menu_content_width(ui: &Ui, items: &[(String, bool, MoreAction)]) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let pad_x = ui.spacing().button_padding.x;
    let longest = items
        .iter()
        .map(|(label, _, _)| {
            ui.painter()
                .layout_no_wrap(label.clone(), font.clone(), Color32::PLACEHOLDER)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    (longest + 2.0 * pad_x).max(MORE_MENU_WIDTH - 2.0 * MORE_MENU_INNER_MARGIN)
}

/// 「…」弹出菜单：真实功能项（含子文件夹 / 文件名 / 设置 / 索引管理 / 全屏 / 关于）。
pub fn more_menu(
    ui: &mut Ui,
    at: Pos2,
    theme: &Theme,
    recurse_on: bool,
    recurse_available: bool,
    show_filename: bool,
) -> Vec<TopbarAction> {
    let mut actions = Vec::new();
    let items = more_menu_items(recurse_on, recurse_available, show_filename);
    let content_w = more_menu_content_width(ui, &items);
    egui::Area::new(more_menu_area_id())
        .fixed_pos(at)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            // M5：菜单 = 主题卡片实色 + 圆角 + 发丝边（亮白 / 暗 #1c1c1c）。
            egui::Frame::new()
                .fill(theme.menu_bg)
                .corner_radius(theme.radius_btn + 2.0)
                .stroke(egui::Stroke::new(1.0, theme.hairline))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 6],
                    blur: 20,
                    spread: 0,
                    color: Color32::from_black_alpha(if theme.dark { 150 } else { 40 }),
                })
                .inner_margin(egui::Margin::same(MORE_MENU_INNER_MARGIN as i8))
                .show(ui, |ui| {
                    // 固定内容宽度：按钮用 `available_width()` 时拿到的是这个值，
                    // 而不是 `Area` 首帧兜底的 600 px。
                    ui.set_width(content_w);
                    for (label, enabled, action) in items {
                        let btn = ui.add_enabled(
                            enabled,
                            egui::Button::new(label)
                                .frame(false)
                                .min_size(vec2(ui.available_width(), MORE_MENU_ROW_H)),
                        );
                        // 设置… / 索引管理… 是 M5 新增的两条入口，快捷键写在悬停提示里，
                        // 不额外占菜单宽度（v1.4 截图里显示在右侧）。
                        let btn = match action {
                            MoreAction::Settings => btn.on_hover_text("设置 · ⌘,"),
                            MoreAction::IndexManager => {
                                btn.on_hover_text("索引管理（五标签 · 后台任务不会因关窗取消）")
                            }
                            _ => btn,
                        };
                        if btn.clicked() {
                            actions.push(TopbarAction::More(action));
                            ui.close();
                        }
                    }
                });
        });
    actions
}

/// 排序弹出菜单：与截图观感一致的圆角白卡。返回选中的动作。
pub fn sort_menu(
    ui: &mut Ui,
    at: Pos2,
    theme: &Theme,
    current: picasa_db::SortSpec,
    thumbs_first: bool,
) -> Option<TopbarAction> {
    let mut action = None;
    egui::Area::new(ui.scope_id().with("topbar-sort-menu"))
        .fixed_pos(at)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(theme.menu_bg)
                .corner_radius(theme.radius_btn + 2.0)
                .stroke(egui::Stroke::new(1.0, theme.hairline))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 6],
                    blur: 20,
                    spread: 0,
                    color: Color32::from_black_alpha(if theme.dark { 150 } else { 40 }),
                })
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.set_min_width(170.0);
                    for (key, label) in [
                        (SortKey::TakenAt, "拍摄日期"),
                        (SortKey::Name, "文件名"),
                        (SortKey::ModifiedAt, "修改日期"),
                        (SortKey::Size, "文件大小"),
                        (SortKey::Dimensions, "图片尺寸"),
                    ] {
                        let mark = if current.key == key { "✓ " } else { "   " };
                        let dir_mark = if current.key == key && current.descending {
                            " ↓"
                        } else {
                            ""
                        };
                        let btn = ui.add(
                            egui::Button::new(format!("{mark}{label}{dir_mark}"))
                                .frame(false)
                                .min_size(vec2(ui.available_width(), 22.0)),
                        );
                        if btn.clicked() {
                            action = Some(TopbarAction::PickSortKey(key));
                            ui.close();
                        }
                    }
                    ui.separator();
                    // 用户要求：有缩略图的往前放，没有的单独排在一起。
                    // 这是「怎么排」之外的显示策略，所以单独一项，不混进排序键。
                    let mark = if thumbs_first { "✓ " } else { "   " };
                    if ui
                        .add(
                            egui::Button::new(format!("{mark}有缩略图的排在前面"))
                                .frame(false)
                                .min_size(vec2(ui.available_width(), 22.0)),
                        )
                        .on_hover_text("没有缩略图的（含云端未下载、生成失败）会集中排在后面")
                        .clicked()
                    {
                        action = Some(TopbarAction::ToggleThumbsFirst);
                        ui.close();
                    }
                    ui.separator();
                    let dir = if current.descending {
                        "改为升序"
                    } else {
                        "改为降序"
                    };
                    if ui
                        .add(
                            egui::Button::new(dir)
                                .frame(false)
                                .min_size(vec2(ui.available_width(), 22.0)),
                        )
                        .clicked()
                    {
                        action = Some(TopbarAction::ToggleSortDirection);
                        ui.close();
                    }
                });
        });
    action
}

#[cfg(test)]
mod m5_font_tests {
    use super::*;
    use crate::theme::Theme;
    use picasa_core::Appearance;

    /// 顶栏应用名字号按原型 `.titlebar .appname` 取（亮 12 / 暗 11）。
    ///
    /// 这条是「逐像素量字体」时发现的：应用原来两套都用 12，暗色比原型大一号。
    #[test]
    fn app_name_font_follows_the_prototype_scale() {
        assert_eq!(
            app_name_font_size(&Theme::for_appearance(Appearance::Light)),
            12.0
        );
        assert_eq!(
            app_name_font_size(&Theme::for_appearance(Appearance::Dark)),
            11.0
        );
    }
}

#[cfg(test)]
mod m41_tests {
    use super::*;

    /// `M41-UI-01`：四枚按钮都在「编辑」右侧、互不重叠、尺寸恒为 d。
    #[test]
    fn object_buttons_sit_after_edit_with_stable_size() {
        let size = Metrics::topbar_icon_size(false);
        let rects = object_button_rects(300.0, 60.0, size);
        assert_eq!(rects.len(), 4);
        for (i, r) in rects.iter().enumerate() {
            assert!(r.left() >= 300.0, "第 {i} 枚跑到编辑左侧了");
            assert_eq!(r.width(), size.x, "第 {i} 枚尺寸被压缩了");
            if i > 0 {
                assert!(r.left() >= rects[i - 1].right(), "第 {i} 枚与前一枚重叠");
            }
        }
    }

    /// `M41-UI-01`：窄窗口（900）时中段必须右移到四按钮组之后 —— 守
    /// `show` 里 `mid_left = centered.max(after_objects)` 这条让位逻辑。
    #[test]
    fn middle_group_yields_to_object_buttons_on_narrow_window() {
        let size = Metrics::topbar_icon_size(false);
        let rects = object_button_rects(300.0, 60.0, size);
        let after_objects = rects[3].right() + 18.0;
        for width in [1280.0_f32, 900.0_f32] {
            let mid_w = 3.0 * 34.0;
            let centered = (width - mid_w) / 2.0;
            let search_left = width - 14.0 - 4.0 * size.x - 2.0 * 4.0 - 14.0; // 右段近似左缘
            let mid_left = centered
                .max(after_objects)
                .min(search_left - mid_w - 12.0)
                .max(centered);
            assert!(
                mid_left >= rects[3].right(),
                "{width}px：中段 {mid_left} 仍与四按钮右缘 {} 重叠",
                rects[3].right()
            );
        }
    }

    /// `M5-MN-01` / `M5-IA-04`：更多菜单的第一项不再是「添加文件夹…」，
    /// 必须有「设置…」与「索引管理…」，且没有演示扩展 / 恢复演示数据。
    #[test]
    fn m5_more_menu_drops_add_folder_and_exposes_settings_and_index_manager() {
        let items = more_menu_items(false, true, true);
        let labels: Vec<&str> = items.iter().map(|(l, _, _)| l.as_str()).collect();
        assert!(
            !labels.iter().any(|l| l.contains("添加文件夹")),
            "更多菜单不许再有「添加文件夹…」：{labels:?}"
        );
        assert!(labels.contains(&"设置…"), "{labels:?}");
        assert!(labels.contains(&"索引管理…"), "{labels:?}");
        assert!(labels.contains(&"含子文件夹"), "{labels:?}");
        assert!(labels.contains(&"全屏"), "{labels:?}");
        assert!(labels.contains(&"关于 OpenPicasa"), "{labels:?}");
        // 「演示与数据」不得进入正式产品；「列表布局」「恢复演示数据」是原型 OUT-C。
        for banned in ["演示", "列表布局", "恢复演示数据", "帮助与快捷键"] {
            assert!(
                !labels.iter().any(|l| l.contains(banned)),
                "不该出现 {banned}：{labels:?}"
            );
        }
        let actions: Vec<MoreAction> = items.iter().map(|(_, _, a)| *a).collect();
        assert!(actions.contains(&MoreAction::Settings));
        assert!(actions.contains(&MoreAction::IndexManager));
    }

    /// `M5-MN-01`：文件夹语境才有「含子文件夹」；非文件夹语境置灰但仍在。
    #[test]
    fn m5_more_menu_recurse_is_context_gated() {
        let items = more_menu_items(false, false, false);
        let recurse = items
            .iter()
            .find(|(_, _, a)| *a == MoreAction::ToggleRecurse)
            .expect("含子文件夹这一项仍在");
        assert!(!recurse.1, "非文件夹语境应置灰");
        let items = more_menu_items(true, true, false);
        assert_eq!(items[0].0, "✓ 只看本文件夹", "开着时以勾选说明当前语义");
    }

    /// `AC-SORT-THUMBS` / `M5-MN-06～08`：排序菜单的可见项。
    ///
    /// 用无窗口后端跑真实 `sort_menu`，把本帧所有文本收集起来判定——
    /// 比另写一份「菜单应该有什么」的常量更能防止实现漂移。
    fn sort_menu_texts(current: picasa_db::SortSpec, thumbs_first: bool) -> Vec<String> {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx, picasa_core::Appearance::Light);
        let mut last = Vec::new();
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 820.0))),
                max_texture_side: Some(2048),
                ..Default::default()
            };
            let out = ctx.run_ui(input, |ui| {
                let theme = Theme::for_appearance(picasa_core::Appearance::Light);
                let _ = sort_menu(ui, pos2(681.0, 100.0), &theme, current, thumbs_first);
            });
            let mut texts = Vec::new();
            collect_texts(&out.shapes, &mut texts);
            last = texts.into_iter().map(|(t, _, _)| t).collect();
            out.drop_without_applying_deltas();
        }
        last
    }

    /// `AC-XX-UNCONNECTED` / `M5-TB-03`～`04`：幻灯片与编辑只有「下一版开放」反馈，
    /// 不是可运行功能。这里跑真实顶栏，逐条点这两个按钮并断言只产出一个 Placeholder 动作。
    #[test]
    fn m5_slideshow_and_edit_are_unconnected_only() {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx, picasa_core::Appearance::Light);
        let mut all = Vec::new();
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 820.0))),
                max_texture_side: Some(2048),
                ..Default::default()
            };
            let out = ctx.run_ui(input, |ui| {
                let theme = Theme::for_appearance(picasa_core::Appearance::Light);
                let out = show(
                    ui,
                    &theme,
                    &[("图库".to_string(), None)],
                    "",
                    ViewMode::Grid,
                    picasa_db::SortSpec::default(),
                    false,
                    &mut String::new(),
                    false,
                    true,
                    Metrics::SIDEBAR_DEFAULT_W,
                    ObjectActionState {
                        has_selection: false,
                        all_favorite: false,
                        all_in_tray: false,
                    },
                );
                all.extend(out.actions);
            });
            out.drop_without_applying_deltas();
        }
        // 没点按钮 → 不该自动产生占位动作
        assert!(
            all.iter()
                .all(|a| !matches!(a, TopbarAction::Placeholder(_))),
            "未点击时不应产生「下一版开放」：{all:?}"
        );
        // 两个可见未接通入口的**反馈文案**必须是「下一版开放」类，
        // 不能换成「已打开幻灯片 / 进入编辑」这种假装可用的句子。
        let entries = unconnected_entries();
        assert_eq!(entries.len(), 2, "只有幻灯片与编辑两个未接通入口");
        for (tip, msg) in entries {
            assert!(tip.contains("下一版开放"), "悬停文案不对：{tip}");
            assert!(msg.contains("下一版开放"), "点击反馈不对：{msg}");
            for banned in ["已打开", "进入编辑", "开始播放", "正在播放"] {
                assert!(!msg.contains(banned), "不许假装可用：{msg}");
            }
        }
    }

    /// `AC-SORT-THUMBS`：排序键齐全、当前键带「✓」、降序再加「↓」，
    /// 且有「有缩略图的排在前面」与「改为升序 / 改为降序」；**没有评分排序**。
    #[test]
    fn m5_sort_menu_lists_the_spec_keys_checks_current_and_never_offers_rating() {
        let texts = sort_menu_texts(
            picasa_db::SortSpec {
                key: SortKey::TakenAt,
                descending: true,
            },
            true,
        );
        for label in ["拍摄日期", "文件名", "修改日期", "文件大小", "图片尺寸"] {
            assert!(
                texts.iter().any(|t| t.contains(label)),
                "排序菜单缺「{label}」：{texts:?}"
            );
        }
        assert!(
            texts.iter().any(|t| t.contains("有缩略图的排在前面")),
            "缺「有缩略图的排在前面」：{texts:?}"
        );
        // 降序 → 「改为升序」；当前键 + 降序 → 「✓ … ↓」
        assert!(
            texts.iter().any(|t| t == "改为升序"),
            "降序时应该出现「改为升序」：{texts:?}"
        );
        let taken = texts
            .iter()
            .find(|t| t.contains("拍摄日期"))
            .expect("拍摄日期这一项");
        assert!(taken.starts_with('✓'), "当前键要带勾：{taken}");
        assert!(taken.ends_with('↓'), "降序要带 ↓：{taken}");
        // 没有「评分」排序（OR-13 / AC-OUT-C）
        assert!(
            !texts
                .iter()
                .any(|t| t.contains("评分") || t.contains("星级")),
            "排序菜单不许有评分：{texts:?}"
        );

        // 升序时应改为「改为降序」，且当前键不再带 ↓
        let texts = sort_menu_texts(
            picasa_db::SortSpec {
                key: SortKey::Name,
                descending: false,
            },
            false,
        );
        assert!(texts.iter().any(|t| t == "改为降序"), "{texts:?}");
        let name = texts
            .iter()
            .find(|t| t.contains("文件名"))
            .expect("文件名这一项");
        assert!(name.starts_with('✓'), "{name}");
        assert!(!name.contains('↓'), "升序不该带 ↓：{name}");
        // 未开启时，「有缩略图的排在前面」不带勾
        let thumbs = texts
            .iter()
            .find(|t| t.contains("有缩略图的排在前面"))
            .expect("缩略图优先这一项");
        assert!(!thumbs.starts_with('✓'), "{thumbs}");
    }

    // ---- M5-R3 缺陷修复：更多菜单宽度 -------------------------------------------------

    /// 无窗口后端跑一帧 egui（宽高即窗口逻辑尺寸），返回本帧绘制结果。
    fn run_frame(
        ctx: &egui::Context,
        appearance: picasa_core::Appearance,
        size: egui::Vec2,
        mut menu: impl FnMut(&mut Ui, &Theme),
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            max_texture_side: Some(2048),
            ..Default::default()
        };
        let theme = Theme::for_appearance(appearance);
        ctx.run_ui(input, |ui| {
            menu(ui, &theme);
        })
    }

    /// 打开菜单并返回 (菜单实际矩形, 本帧所有文本)。
    ///
    /// 菜单是 `Area`：首帧是 sizing pass（不可见、尺寸是占位值），所以跑 3 帧
    /// 再回读，拿到的是稳定后的真实矩形。
    fn open_menu(
        appearance: picasa_core::Appearance,
        size: egui::Vec2,
        recurse_on: bool,
        recurse_available: bool,
        show_filename: bool,
    ) -> (Rect, Vec<(String, f32, usize)>) {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx, appearance);
        let mut last = None;
        for _ in 0..3 {
            let out = run_frame(&ctx, appearance, size, |ui, theme| {
                let _ = more_menu(
                    ui,
                    pos2(681.0, 100.0),
                    theme,
                    recurse_on,
                    recurse_available,
                    show_filename,
                );
            });
            let mut texts = Vec::new();
            collect_texts(&out.shapes, &mut texts);
            last = Some((more_menu_rect(&ctx).expect("菜单已画出"), texts));
            out.drop_without_applying_deltas();
        }
        let (rect, texts) = last.unwrap();
        (rect, texts)
    }

    fn collect_texts(shapes: &[egui::epaint::ClippedShape], out: &mut Vec<(String, f32, usize)>) {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, f32, usize)>) {
            match shape {
                egui::Shape::Vec(v) => {
                    for s in v {
                        walk(s, out);
                    }
                }
                egui::Shape::Text(t) => out.push((
                    t.galley.job.text.clone(),
                    t.galley.rect.width(),
                    t.galley.rows.len(),
                )),
                _ => {}
            }
        }
        for c in shapes {
            walk(&c.shape, out);
        }
    }

    /// `M5-MN-01` 回归：菜单面板宽度必须贴合内容，不能跟着窗口剩余宽度走。
    ///
    /// 旧实现：`Area` 无宽度约束 + 按钮 `min_size(available_width, ..)`，首帧
    /// 尺寸取自 egui 的 `spacing.default_area_size`（600 px），于是 1400 宽的
    /// 窗口里菜单面板是 598 px 的白条（见 M5-R3 截图）。修复后应 ≈220 px。
    #[test]
    fn m5_more_menu_width_is_content_sized_not_window_width() {
        for appearance in [
            picasa_core::Appearance::Light,
            picasa_core::Appearance::Dark,
        ] {
            let mut widths = Vec::new();
            for size in [
                vec2(1400.0, 900.0),
                vec2(900.0, 600.0),
                vec2(1920.0, 1080.0),
            ] {
                let (rect, _) = open_menu(appearance, size, false, true, true);
                assert!(
                    rect.width() < 300.0,
                    "{appearance:?} @ {size:?}：菜单仍被拉到 {} px（上限 300）",
                    rect.width()
                );
                assert!(
                    rect.width() >= MORE_MENU_WIDTH,
                    "{appearance:?} @ {size:?}：菜单 {} px 窄于设计宽度 {MORE_MENU_WIDTH}",
                    rect.width()
                );
                widths.push(rect.width());
            }
            // 三种窗口宽度下菜单宽度必须一致（与窗口剩余宽度无关）。
            let first = widths[0];
            for w in &widths[1..] {
                assert!(
                    (w - first).abs() < 0.5,
                    "{appearance:?}：窗口变化后菜单宽度跟着变了：{widths:?}"
                );
            }
        }
    }

    /// 菜单至少放得下现有最长项：所有按钮文字都单行、不截断、不超出内容区。
    #[test]
    fn m5_more_menu_fits_every_existing_item() {
        for appearance in [
            picasa_core::Appearance::Light,
            picasa_core::Appearance::Dark,
        ] {
            let (rect, texts) = open_menu(appearance, vec2(1400.0, 900.0), false, true, true);
            let labels: Vec<String> = more_menu_items(false, true, true)
                .into_iter()
                .map(|(l, _, _)| l)
                .collect();
            for label in &labels {
                let hit = texts
                    .iter()
                    .find(|(t, _, _)| t == label)
                    .unwrap_or_else(|| {
                        panic!("{appearance:?}：菜单里没画出「{label}」：{texts:?}")
                    });
                assert_eq!(hit.2, 1, "{appearance:?}：「{label}」被折成多行");
                // 文本宽度 + 左右内边距（6 边框 + 8 按钮内边距）必须容得下。
                assert!(
                    hit.1 + 2.0 * (MORE_MENU_INNER_MARGIN + 8.0) <= rect.width() + 0.5,
                    "{appearance:?}：「{label}」文字 {} px 放不进 {} px 的菜单",
                    hit.1,
                    rect.width()
                );
            }
        }
    }

    /// 亮暗两套主题打开的是**同一套**菜单项（宽度与文案都不随主题变）。
    #[test]
    fn m5_more_menu_items_are_identical_in_light_and_dark() {
        let (light_rect, light_texts) = open_menu(
            picasa_core::Appearance::Light,
            vec2(1400.0, 900.0),
            true,
            true,
            true,
        );
        let (dark_rect, dark_texts) = open_menu(
            picasa_core::Appearance::Dark,
            vec2(1400.0, 900.0),
            true,
            true,
            true,
        );
        assert!(
            (light_rect.width() - dark_rect.width()).abs() < 0.5,
            "亮/暗菜单宽度不一致：{} vs {}",
            light_rect.width(),
            dark_rect.width()
        );
        let mut light: Vec<String> = light_texts.into_iter().map(|(t, _, _)| t).collect();
        let mut dark: Vec<String> = dark_texts.into_iter().map(|(t, _, _)| t).collect();
        light.sort();
        dark.sort();
        assert_eq!(light, dark, "亮/暗菜单项不一致");
        for expected in more_menu_items(true, true, true) {
            assert!(
                light.contains(&expected.0),
                "菜单缺少「{}」：{light:?}",
                expected.0
            );
        }
    }

    /// 「点空白关闭」的命中区 = 菜单实际矩形（不是写死的 220×260）。
    #[test]
    fn m5_more_menu_hit_rect_is_the_real_menu_rect() {
        assert!(
            more_menu_rect(&egui::Context::default()).is_none(),
            "没打开菜单时不该有命中矩形"
        );
        let (rect, _) = open_menu(
            picasa_core::Appearance::Light,
            vec2(1400.0, 900.0),
            false,
            true,
            true,
        );
        assert!(
            rect.width() < 300.0 && rect.height() > 100.0,
            "菜单矩形异常：{rect:?}"
        );
        // 菜单右半边的按钮区域必须落在命中区内，否则点它会被「点空白」抢关。
        let right_side = pos2(rect.right() - 8.0, rect.center().y);
        assert!(rect.contains(right_side), "菜单右侧不在命中区：{rect:?}");
    }

    /// `M41-UI-01`：双态判定的默认值（无选中）应让两个按钮都不进选中态。
    #[test]
    fn object_action_state_default_is_unselected() {
        let s = ObjectActionState::default();
        assert!(!s.has_selection);
        assert!(!s.all_favorite);
        assert!(!s.all_in_tray);
    }

    /// 原型 `.toolbar` 高 48、按钮中心距窗口顶部 60（红绿灯中心 18）。
    /// 门禁：顶栏 rect 的任何按钮/搜索/排布都按 `toolbar_center_y` 排在下半区，
    /// 不再用 `rect.center()`（那会得到 42，仍贴上标题带）。
    #[test]
    fn m5_v1_toolbar_content_sits_below_the_title_band() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, Metrics::topbar_h(false)));
        let cy = toolbar_center_y(rect, false);
        assert_eq!(cy, 60.0, "工具条中心应在 y=60");
        assert_eq!(
            rect.center().y,
            42.0,
            "rect 几何中心仍是 42——不能拿它画按钮"
        );
        assert!(cy - rect.center().y >= 18.0, "内容须整体下移 ≥18px");

        // 右簇（排布胶囊 / 搜索 / 排序 / 筛选 / 更多）也都落在工具条中心
        let (_layout, search, sort, filter, more) =
            topbar_right_rects(rect, Metrics::topbar_icon_size(false), false);
        for r in [search, sort, filter, more] {
            assert!(
                (r.center().y - cy).abs() < 0.01,
                "右簇控件中心 {} 偏离工具条中线 {cy}",
                r.center().y
            );
            assert!(
                r.top() >= Metrics::topbar_title_h(false),
                "控件侵入标题带：{r:?}"
            );
        }
    }

    /// H8：macOS 隐藏 AppKit 原生标题后，egui 自绘的应用名须落在标题带正中，
    /// 且水平上远离左端红绿灯（x≈0..78）——不能像原生标题那样盖住红绿灯。
    #[test]
    fn m5_v1_app_name_is_centered_and_clears_traffic_lights() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, Metrics::topbar_h(false)));
        let c = app_name_center(rect, false);
        assert_eq!(
            c.y,
            Metrics::topbar_title_h(false) / 2.0,
            "应用名在标题带竖直中线"
        );
        assert_eq!(c.x, 640.0, "应用名水平居中");
        assert!(c.x - 60.0 > 78.0, "应用名左缘须远离红绿灯区（x≈0..78）");
    }
}
