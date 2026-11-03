//! M3.1 顶栏：macOS「照片」风格的单行工具栏。
//!
//! 布局（量自参考截图 2400×1440 ÷2，左右两半各量一遍）：
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────┐
//! │ [sidebar]  标题       [− | +]   年 | 月 | 所有照片   [⇅][≡][…]  [搜索] │  52px
//! └──────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! * 左 1：侧栏开关（红绿灯右侧）。红绿灯由 `eframe::Frame::set_traffic_lights_position`
//!   放进这条栏里（`with_fullsize_content_view`）。
//! * 标题：视图名 + 日期范围小字（截图是「图库 / 2026年2月18日–5月2日」）。
//! * −/+：**替代 M3 的状态栏缩放滑块**（用户 M3.1 指令：滑块改成加号减号按钮放进顶栏）。
//! * 分段控件：**替代 M4 的「视图」弹出菜单**（用户 M3.1 指令：三个子菜单模仿
//!   截图的「年 / 月 / 所有照片」按钮组；去掉缩略图循环按钮）。
//! * 右：排序 / 筛选列表 / 更多（…）——截图右侧图标组的**前三个**；
//!   搜索、信息、分享、收藏在更右侧（搜索保留原功能，其余在本版是入口占位，
//!   点了给出 toast 说明，不做假功能）。
//!
//! ## 为什么不再画 XP 菜单栏
//!
//! M3.1 的验收就是「像素级模仿这张截图」，截图里没有菜单栏、没有文字按钮、
//! 没有状态栏滑块。原「档案/视图/说明」菜单的功能（添加文件夹、排序、全屏、关于）
//! 全部保留：排序与筛选在顶栏图标，添加文件夹与关于在「…」菜单里。

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Response, Sense, Ui, pos2, vec2,
};
use picasa_core::ViewMode;
use picasa_db::SortKey;

use crate::icons::{self, Icon};
use crate::theme::{Metrics, Theme};

/// 顶栏产出的动作，由 App 执行（顶栏自己不碰状态）。
#[derive(Clone, Debug, PartialEq)]
pub enum TopbarAction {
    /// 侧栏开关
    ToggleSidebar,
    /// 缩小一格（对数档，`⌘−` 同路径）
    ZoomOut,
    /// 放大一格（对数档，`⌘+` 同路径）
    ZoomIn,
    /// 视图模式三选一（原「视图」弹出菜单的三项）
    PickViewMode(ViewMode),
    /// 打开排序弹出菜单（由 App 画，位置取按钮 rect）
    ToggleSortMenu,
    /// 选择排序键（同键再点 = 翻转方向，App 处理）
    PickSortKey(SortKey),
    /// 翻转排序方向
    ToggleSortDirection,
    /// 打开筛选面板（原「筛选」按钮）
    ToggleFilterMenu,
    /// 打开或关闭「…」弹出菜单（与排序/筛选同一套开关，不直接进选目录）
    ToggleMoreMenu,
    /// 「…」菜单里的动作（见 [`more_menu`]）
    More(MoreAction),
    /// 搜索框内容变化
    SearchChanged(String),
    /// 点了搜索框左侧的范围小按钮（当前文件夹 / 全部图库）
    ToggleSearchScope,
    /// 占位入口（信息 / 分享 / 收藏）被点：App 用 toast 说明
    Placeholder(&'static str),
}

/// 「…」菜单里的真实功能项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoreAction {
    AddFolder,
    ToggleRecurse,
    ToggleFullscreen,
    About,
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

/// 顶栏入参。能拆成小结构体但参数就 8 个，直接列。
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    theme: &Theme,
    // 标题两行：视图名 + 副标题（日期范围或总数）
    title: &str,
    subtitle: &str,
    // 分段控件当前值
    view_mode: ViewMode,
    // 排序（按钮上要显示当前排序的语义）
    sort: picasa_db::SortSpec,
    // 筛选是否有激活条件（按钮高亮）
    filter_active: bool,
    // 搜索
    search: &mut String,
    search_in_current: bool,
    // 缩放是否到端点（−/+ 置灰）
    zoom_min: bool,
    zoom_max: bool,
) -> TopbarOutput {
    let mut out = TopbarOutput {
        actions: Vec::new(),
        sort_button: None,
        filter_button: None,
        more_button: None,
    };
    let rect = Rect::from_min_size(
        ui.max_rect().left_top(),
        vec2(ui.max_rect().width(), Metrics::TOPBAR_H),
    );
    let _ = ui.allocate_space(rect.size());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme.toolbar_bg);
    // 底边发丝线（半透明，压在内容上不抢眼）
    painter.line_segment(
        [pos2(rect.left(), rect.bottom() - 0.5), pos2(rect.right(), rect.bottom() - 0.5)],
        egui::Stroke::new(1.0, theme.toolbar_border),
    );

    // macOS 下红绿灯占左侧约 80pt（三个圆点 + 边距）。非 macOS 或未启用
    // fullsize content view 时这段空间是空的，但也无碍——顶栏是自绘的。
    #[cfg(target_os = "macos")]
    let traffic_w = 78.0;
    #[cfg(not(target_os = "macos"))]
    let traffic_w = 8.0;

    let mut x = rect.left() + traffic_w;

    // ---- ① 侧栏开关 ----
    let side_rect = circle_btn(x, rect);
    if icon_btn(ui, &painter, side_rect, Icon::SidebarToggle, theme, true, "显示/隐藏边栏").clicked()
    {
        out.actions.push(TopbarAction::ToggleSidebar);
    }
    x = side_rect.right() + GAP_AFTER_BTN;

    // ---- ② 标题两行 ----
    let title_font = FontId::new(15.0, egui::FontFamily::Proportional);
    let sub_font = FontId::new(11.0, egui::FontFamily::Proportional);
    painter.text(
        pos2(x, rect.center().y - 8.0),
        Align2::LEFT_CENTER,
        title,
        title_font,
        theme.text_primary,
    );
    painter.text(
        pos2(x, rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        subtitle,
        sub_font,
        theme.text_secondary,
    );
    // 标题宽度给文字留出自然空间（这里不换行，宽度不够时挤压后面的空隙）
    x += 150.0_f32.min(rect.width() * 0.22);

    // ---- ③ −/+ 缩放胶囊（M3 滑块的替代）----
    let pill_w = 76.0;
    let pill = Rect::from_min_size(
        pos2(x, rect.center().y - Metrics::ZOOM_PILL_H / 2.0),
        vec2(pill_w, Metrics::ZOOM_PILL_H),
    );
    draw_zoom_pill(ui, &painter, pill, theme, zoom_min, zoom_max, &mut out);
    x = pill.right() + GAP_AFTER_BTN;

    // ---- ④ 分段控件（年 | 月 | 所有照片 的版式，三个视图模式）----
    let seg_w = 286.0_f32.min(rect.width() * 0.34);
    let seg = Rect::from_min_size(
        pos2(x, rect.center().y - Metrics::SEG_H / 2.0),
        vec2(seg_w, Metrics::SEG_H),
    );
    draw_segmented(ui, &painter, seg, theme, view_mode, &mut out);
    x = seg.right();

    // ---- ⑤ 右侧图标组（从右往左排，与截图一致）----
    // 搜索框（保留原功能，样式改为圆形热区内的放大镜 + 展开输入）
    let search_w = 150.0_f32.min(rect.width() * 0.2);
    let search_rect = Rect::from_min_size(
        pos2(rect.right() - search_w - 12.0, rect.center().y - 14.0),
        vec2(search_w, 28.0),
    );
    draw_search(ui, &painter, search_rect, theme, search, search_in_current, &mut out);

    // 信息 / 分享 / 收藏（占位入口：有热区、点了给说明，不做假功能）
    let right_icons: &[(Icon, &'static str, &'static str)] = &[
        (Icon::Search, "搜索（⌘F 聚焦搜索框）", "__search_focus__"),
        (Icon::Info, "显示照片信息（下一版开放）", "信息面板将在下一版开放"),
        (Icon::Share, "分享（下一版开放）", "分享将在下一版开放"),
        (Icon::Heart, "收藏选中照片：用右键菜单或筛选面板的「只看收藏」", "收藏请用右键菜单或筛选面板"),
    ];
    let mut ix = search_rect.left() - 6.0;
    for (icon, tip, toast) in right_icons.iter().rev() {
        let r = circle_btn(ix - Metrics::TOPBAR_ICON_D, rect);
        if icon == &Icon::Search {
            // 搜索按钮叠在搜索框左端，点了聚焦输入框
            let resp = icon_btn(ui, &painter, r, *icon, theme, true, tip);
            if resp.clicked() {
                ui.memory_mut(|m| m.request_focus(egui::Id::unique("topbar-search")));
            }
        } else if icon_btn(ui, &painter, r, *icon, theme, true, tip).clicked() {
            out.actions.push(TopbarAction::Placeholder(toast));
        }
        ix = r.left();
    }
    let _ = ix;

    // 排序 / 列表(筛选) / 更多：紧挨分段控件右侧（截图位置）
    let mut rx = x + 18.0;
    let sort_r = circle_btn(rx, rect);
    let sort_resp = icon_btn(
        ui,
        &painter,
        sort_r,
        Icon::SortArrows,
        theme,
        true,
        match sort.key {
            SortKey::TakenAt => if sort.descending { "排序：拍摄日期（新在前）" } else { "排序：拍摄日期（旧在前）" },
            SortKey::Name => if sort.descending { "排序：文件名（Z→A）" } else { "排序：文件名（A→Z）" },
            SortKey::ModifiedAt => "排序：修改时间",
            SortKey::Size => "排序：文件大小",
            SortKey::Dimensions => "排序：图片尺寸",
        },
    );
    if sort_resp.clicked() {
        out.actions.push(TopbarAction::ToggleSortMenu);
    }
    out.sort_button = Some(sort_r);
    rx = sort_r.right() + 4.0;

    let filter_r = circle_btn(rx, rect);
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
        // 激活态：底色点缀（截图没有这个态，取「选中片」的白底 + 主题蓝描边）
        painter.rect_filled(filter_r, Metrics::TOPBAR_ICON_D / 2.0, theme.seg_active);
        painter.rect_stroke(
            filter_r,
            Metrics::TOPBAR_ICON_D / 2.0,
            egui::Stroke::new(1.5, theme.selected),
            egui::StrokeKind::Inside,
        );
    }
    out.filter_button = Some(filter_r);
    rx = filter_r.right() + 4.0;

    let more_r = circle_btn(rx, rect);
    // 只负责开关菜单。真正的「添加文件夹…」在 [`more_menu`] 里，
    // 不能在这里直接发 `More(AddFolder)`——那样菜单永远打不开，
    // 空库时用户也到不了选目录。
    if icon_btn(ui, &painter, more_r, Icon::Ellipsis, theme, true, "更多").clicked() {
        out.actions.push(TopbarAction::ToggleMoreMenu);
    }
    out.more_button = Some(more_r);

    out
}

/// 两个相邻按钮之间的空隙（截图：侧栏钮与标题间约 20px，−/+ 与分段间约 20px）。
const GAP_AFTER_BTN: f32 = 20.0;

/// 圆形按钮热区：中心在 `x + 半径`，垂直居中。
fn circle_btn(x: f32, bar: Rect) -> Rect {
    Rect::from_center_size(
        pos2(x + Metrics::TOPBAR_ICON_D / 2.0, bar.center().y),
        vec2(Metrics::TOPBAR_ICON_D, Metrics::TOPBAR_ICON_D),
    )
}

/// 圆形图标按钮：悬停浅灰圆底，无文字。
fn icon_btn(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    icon: Icon,
    theme: &Theme,
    enabled: bool,
    tooltip: &str,
) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(rect, ui.scope_id().with(("topbar-btn", tooltip)), sense);
    if enabled && resp.hovered() {
        painter.rect_filled(rect, Metrics::TOPBAR_ICON_D / 2.0, theme.sidebar_hover);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let color = if enabled { theme.toolbar_icon } else { theme.text_disabled };
    let inner = rect.shrink(Metrics::TOPBAR_ICON_D * 0.28);
    icons::draw(painter, icon, inner, color, 1.4);
    if enabled {
        resp.clone().on_hover_text(tooltip);
    }
    resp
}

/// −/+ 缩放胶囊：灰白圆角底、中间 1px 竖线，左右两枚可点。
///
/// 点击走**对数档**（`picasa_core::step_log`）——与 ⌘+/⌘− 同一条路径；
/// 到端点后按钮置灰（截图里按钮永远可见，端点行为由置灰表达）。
fn draw_zoom_pill(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    theme: &Theme,
    zoom_min: bool,
    zoom_max: bool,
    out: &mut TopbarOutput,
) {
    painter.rect_filled(rect, rect.height() / 2.0, theme.seg_bg);
    // 中缝竖线
    let mid_x = rect.center().x;
    painter.line_segment(
        [pos2(mid_x, rect.top() + 6.0), pos2(mid_x, rect.bottom() - 6.0)],
        egui::Stroke::new(1.0, Color32::from_rgba_premultiplied(0, 0, 0, 36)),
    );
    let half_w = rect.width() / 2.0;
    let minus_r = Rect::from_min_size(rect.left_top(), vec2(half_w, rect.height()));
    let plus_r = Rect::from_min_size(pos2(mid_x, rect.top()), vec2(half_w, rect.height()));

    for (r, icon, enabled, tip, action) in [
        (minus_r, Icon::Minus, !zoom_min, "缩小缩略图（⌘−）", TopbarAction::ZoomOut),
        (plus_r, Icon::Plus, !zoom_max, "放大缩略图（⌘+）", TopbarAction::ZoomIn),
    ] {
        // 端点只响应悬停：再点不会发出一次无效缩放，光标也不会变成手形。
        let sense = if enabled { Sense::click() } else { Sense::hover() };
        let resp = ui.interact(r, ui.scope_id().with(("zoom", tip)), sense);
        if !enabled {
            painter.rect_filled(
                r,
                rect.height() / 2.0,
                Color32::from_rgba_unmultiplied(255, 255, 255, 150),
            );
        } else if resp.hovered() {
            painter.rect_filled(r, rect.height() / 2.0, theme.sidebar_hover);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let color = if enabled {
            theme.toolbar_icon
        } else {
            theme.text_disabled
        };
        let inner = Rect::from_center_size(r.center(), vec2(14.0, 14.0));
        icons::draw(painter, icon, inner, color, 1.4);
        let tip = if enabled {
            tip
        } else if matches!(action, TopbarAction::ZoomOut) {
            "已是最小缩略图"
        } else {
            "已是最大缩略图"
        };
        resp.clone().on_hover_text(tip);
        if enabled && resp.clicked() {
            out.actions.push(action);
        }
    }
}

/// 分段控件：灰白胶囊底 + 选中的白片。三个视图模式共用这套版式。
///
/// 截图的三段是「年 | 月 | 所有照片」，我们的三项是
/// 「方格 / 纵向瀑布流 / 横向瀑布流」（用户拍板：把「视图」菜单三项搬进来）。
/// 文案取两字/三字短语，保持截图的视觉节奏。
fn draw_segmented(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    theme: &Theme,
    current: ViewMode,
    out: &mut TopbarOutput,
) {
    painter.rect_filled(rect, rect.height() / 2.0, theme.seg_bg);
    let modes = [
        (ViewMode::Grid, "方格"),
        (ViewMode::MasonryV, "纵向"),
        (ViewMode::MasonryH, "横向"),
    ];
    let w = rect.width() / modes.len() as f32;
    for (i, (mode, label)) in modes.iter().enumerate() {
        let piece = Rect::from_min_size(
            pos2(rect.left() + i as f32 * w, rect.top()),
            vec2(w, rect.height()),
        );
        let active = *mode == current;
        if active {
            // 选中片：白底胶囊（左右端用整圆角，中间片用小圆角——视觉上与截图一致）
            let radius = if i == 0 || i == modes.len() - 1 {
                rect.height() / 2.0
            } else {
                10.0
            };
            painter.rect_filled(piece.shrink(1.5), radius, theme.seg_active);
            // 截图的选中片有极轻的落影；egui 阴影成本高，用 1px 底边发丝线示意
            painter.line_segment(
                [pos2(piece.left() + 4.0, piece.bottom() - 1.5), pos2(piece.right() - 4.0, piece.bottom() - 1.5)],
                egui::Stroke::new(1.0, Color32::from_rgba_premultiplied(0, 0, 0, 14)),
            );
        }
        let resp = ui.interact(piece, ui.scope_id().with(("seg", label)), Sense::click());
        let color = if active { theme.text_primary } else { theme.text_secondary };
        painter.text(
            piece.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::new(12.5, egui::FontFamily::Proportional),
            color,
        );
        if resp.clicked() && !active {
            out.actions.push(TopbarAction::PickViewMode(*mode));
        }
    }
}

/// 顶栏搜索框：白底 + 细灰边 + 圆角 + 右侧放大镜（截图样式）。
fn draw_search(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    theme: &Theme,
    search: &mut String,
    search_in_current: bool,
    out: &mut TopbarOutput,
) {
    painter.rect_filled(rect, rect.height() / 2.0, Color32::WHITE);
    painter.rect_stroke(
        rect,
        rect.height() / 2.0,
        egui::Stroke::new(1.0, Color32::from_rgba_premultiplied(0, 0, 0, 36)),
        egui::StrokeKind::Inside,
    );
    // 范围小字（当前文件夹 / 全库）：放在框内左端，点击切换
    let scope_w = 14.0;
    let scope_rect = Rect::from_min_size(
        pos2(rect.left() + 6.0, rect.top() + 3.0),
        vec2(scope_w, rect.height() - 6.0),
    );
    let scope_resp = ui.interact(scope_rect, ui.scope_id().with("search-scope"), Sense::click());
    painter.text(
        scope_rect.center(),
        Align2::CENTER_CENTER,
        if search_in_current { "文" } else { "全" },
        FontId::new(9.0, egui::FontFamily::Proportional),
        if scope_resp.hovered() { theme.selected } else { theme.text_secondary },
    );
    if scope_resp.clicked() {
        out.actions.push(TopbarAction::ToggleSearchScope);
    }
    scope_resp.clone().on_hover_text(if search_in_current {
        "搜索范围：当前文件夹。点击改为搜索全部图库"
    } else {
        "搜索范围：全部图库。点击改回当前文件夹"
    });

    let edit_rect = Rect::from_min_max(
        pos2(scope_rect.right() + 4.0, rect.top() + 3.0),
        pos2(rect.right() - 22.0, rect.bottom() - 3.0),
    );
    let id = egui::Id::unique("topbar-search");
    let mut buf = search.clone();
    let resp = ui.put(
        edit_rect,
        egui::TextEdit::singleline(&mut buf)
            .id(id)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::symmetric(2, 4))
            .hint_text("搜索")
            .text_color(theme.text_primary),
    );
    if resp.changed() {
        *search = buf.clone();
        out.actions.push(TopbarAction::SearchChanged(buf));
    }
    // 右侧放大镜（装饰性，因为输入框已在框内）
    let mag = Rect::from_center_size(
        pos2(rect.right() - 12.0, rect.center().y),
        vec2(12.0, 12.0),
    );
    icons::draw(painter, Icon::Search, mag, theme.text_secondary, 1.2);
}

/// 「…」弹出菜单：真实功能项（添加文件夹 / 含子文件夹 / 全屏 / 关于）。
pub fn more_menu(
    ui: &mut Ui,
    at: Pos2,
    recurse_on: bool,
    recurse_available: bool,
) -> Vec<TopbarAction> {
    let mut actions = Vec::new();
    egui::Area::new(ui.scope_id().with("topbar-more-menu"))
        .fixed_pos(at)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(Color32::WHITE)
                .corner_radius(8.0)
                .stroke(egui::Stroke::new(1.0, Color32::from_rgba_premultiplied(0, 0, 0, 30)))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 18,
                    spread: 0,
                    color: Color32::from_black_alpha(45),
                })
                .inner_margin(egui::Margin::same(5))
                .show(ui, |ui| {
                    ui.set_min_width(190.0);
                    let items: Vec<(&str, bool, MoreAction)> = vec![
                        ("添加文件夹…", true, MoreAction::AddFolder),
                        (
                            if recurse_on { "✓ 只看本文件夹" } else { "含子文件夹" },
                            recurse_available,
                            MoreAction::ToggleRecurse,
                        ),
                        ("全屏", true, MoreAction::ToggleFullscreen),
                        ("关于 OpenPicasa", true, MoreAction::About),
                    ];
                    for (label, enabled, action) in items {
                        let btn = ui.add_enabled(
                            enabled,
                            egui::Button::new(label)
                                .frame(false)
                                .min_size(vec2(ui.available_width(), 22.0)),
                        );
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
pub fn sort_menu(ui: &mut Ui, at: Pos2, current: picasa_db::SortSpec) -> Option<TopbarAction> {
    let mut action = None;
    egui::Area::new(ui.scope_id().with("topbar-sort-menu"))
        .fixed_pos(at)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(Color32::WHITE)
                .corner_radius(8.0)
                .stroke(egui::Stroke::new(1.0, Color32::from_rgba_premultiplied(0, 0, 0, 30)))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 18,
                    spread: 0,
                    color: Color32::from_black_alpha(45),
                })
                .inner_margin(egui::Margin::same(5))
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
                        let dir_mark = if current.key == key && current.descending { " ↓" } else { "" };
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
                    let dir = if current.descending { "改为升序" } else { "改为降序" };
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
