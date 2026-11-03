//! M3.1 §7 定稿顶栏：一行**图标工具栏**（不再是「标题栏」）。
//!
//! 布局（从左到右，量值按 52px 栏高收敛）：
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────────┐
//! │ ●●●  [侧栏] [导入][幻灯片][编辑]  [方格|纵瀑|横瀑]  [−|+]  [🔍] [⇅][≡][…] │
//! └──────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! 关键约定（`docs/09` §7.2）：
//!
//! * 顶栏**不含**大号文件夹标题、不含文件树——标题移到中区左侧、树的上方；
//! * 导入 / 幻灯片 / 编辑是**无文字圆形图标**；
//! * 排布是**无文字三图标连体胶囊**（方格 / 纵向瀑布 / 横向瀑布），造型与
//!   `−|+` 胶囊同一套；**不是**「年 / 月 / 所有照片」时间胶囊；
//! * 搜索紧接在 `+` 之后（图标按钮；点开后原地展开输入框）。
//!
//! 排序 / 筛选 / 「…」保留为右侧图标组：它们是既有功能，不能在版式重排里丢。

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Response, Sense, Ui, pos2, vec2};
use picasa_core::ViewMode;
use picasa_db::SortKey;

use crate::icons::{self, Icon};
use crate::theme::{Metrics, Theme};

/// 顶栏产出的动作，由 App 执行（顶栏自己不碰状态）。
#[derive(Clone, Debug, PartialEq)]
pub enum TopbarAction {
    /// 侧栏开关
    ToggleSidebar,
    /// 导入 = 添加关注文件夹（既有 `add_folder` 流程）
    AddFolder,
    /// 占位入口（幻灯片 / 编辑）：App 用 toast 说明，不做假功能
    Placeholder(&'static str),
    /// 缩小一格（对数档，`⌘−` 同路径）
    ZoomOut,
    /// 放大一格（对数档，`⌘+` 同路径）
    ZoomIn,
    /// 视图模式三选一（原「视图」菜单的三项）
    PickViewMode(ViewMode),
    /// 打开排序弹出菜单（由 App 画，位置取按钮 rect）
    ToggleSortMenu,
    /// 选择排序键（同键再点 = 翻转方向，App 处理）
    PickSortKey(SortKey),
    /// 翻转排序方向
    ToggleSortDirection,
    /// 打开筛选面板（原「筛选」按钮）
    ToggleFilterMenu,
    /// 打开或关闭「…」弹出菜单
    ToggleMoreMenu,
    /// 「…」菜单里的动作（见 [`more_menu`]）
    More(MoreAction),
    /// 展开/收起顶栏搜索框（搜索按钮紧接在 `+` 之后）
    ToggleSearch,
    /// 搜索框内容变化
    SearchChanged(String),
    /// 点了搜索框左侧的范围小按钮（当前文件夹 / 全部图库）
    ToggleSearchScope,
}

/// 「…」菜单里的真实功能项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoreAction {
    AddFolder,
    ToggleRecurse,
    ToggleFullscreen,
    About,
    /// 方格模式下显示/隐藏文件名（§7.4）
    ToggleShowFilename,
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

/// 顶栏入参。
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    theme: &Theme,
    // 排布当前值
    view_mode: ViewMode,
    // 排序（按钮上要显示当前排序的语义）
    sort: picasa_db::SortSpec,
    // 筛选是否有激活条件（按钮高亮）
    filter_active: bool,
    // 搜索
    search: &mut String,
    search_open: bool,
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
        [
            pos2(rect.left(), rect.bottom() - 0.5),
            pos2(rect.right(), rect.bottom() - 0.5),
        ],
        egui::Stroke::new(1.0, theme.toolbar_border),
    );

    // macOS 下红绿灯占左侧约 80pt。非 macOS 或未启用 fullsize content view 时
    // 这段空间是空的，但也无碍——顶栏是自绘的。
    #[cfg(target_os = "macos")]
    let traffic_w = 78.0;
    #[cfg(not(target_os = "macos"))]
    let traffic_w = 8.0;

    let mut x = rect.left() + traffic_w;

    // ---- ① 侧栏开关 ----
    let side_rect = circle_btn(x, rect);
    if icon_btn(
        ui,
        &painter,
        side_rect,
        Icon::SidebarToggle,
        theme,
        true,
        "显示/隐藏边栏",
    )
    .clicked()
    {
        out.actions.push(TopbarAction::ToggleSidebar);
    }
    x = side_rect.right() + 8.0;

    // ---- ② 导入 / 幻灯片 / 编辑（无文字圆形图标）----
    let action_btns: &[(Icon, &'static str, Option<TopbarAction>)] = &[
        (
            Icon::Import,
            "导入：添加文件夹…",
            Some(TopbarAction::AddFolder),
        ),
        (
            Icon::Slideshow,
            "幻灯片（下一版开放）",
            Some(TopbarAction::Placeholder("幻灯片将在下一版开放")),
        ),
        (
            Icon::Edit,
            "编辑（下一版开放）",
            Some(TopbarAction::Placeholder("编辑将在下一版开放")),
        ),
    ];
    for (icon, tip, action) in action_btns {
        let r = circle_btn(x, rect);
        if icon_btn(ui, &painter, r, *icon, theme, true, tip).clicked()
            && let Some(action) = action
        {
            out.actions.push(action.clone());
        }
        x = r.right() + 4.0;
    }
    x += 10.0;

    // ---- ③ 排布：无文字三图标连体胶囊 ----
    let layout_w = 3.0 * 34.0;
    let layout_rect = Rect::from_min_size(
        pos2(x, rect.center().y - Metrics::ZOOM_PILL_H / 2.0),
        vec2(layout_w, Metrics::ZOOM_PILL_H),
    );
    draw_layout_pill(ui, &painter, layout_rect, theme, view_mode, &mut out);
    x = layout_rect.right() + 8.0;

    // ---- ④ −/+ 缩放胶囊 ----
    let zoom_rect = Rect::from_min_size(
        pos2(x, rect.center().y - Metrics::ZOOM_PILL_H / 2.0),
        vec2(76.0, Metrics::ZOOM_PILL_H),
    );
    draw_zoom_pill(ui, &painter, zoom_rect, theme, zoom_min, zoom_max, &mut out);
    x = zoom_rect.right() + 8.0;

    // ---- ⑤ 右侧图标组（先量出来，给搜索框留位）----
    let right_group = Rect::from_min_size(
        pos2(
            rect.right() - 12.0 - 3.0 * Metrics::TOPBAR_ICON_D - 8.0,
            rect.top(),
        ),
        vec2(3.0 * Metrics::TOPBAR_ICON_D + 8.0, rect.height()),
    );

    // ---- ⑥ 搜索：紧接在 + 之后 ----
    if search_open {
        let max_right = right_group.left() - 10.0;
        let avail = (max_right - x).max(120.0);
        let w = 190.0_f32.min(avail);
        let search_rect = Rect::from_min_size(pos2(x, rect.center().y - 15.0), vec2(w, 30.0));
        draw_search(
            ui,
            &painter,
            search_rect,
            theme,
            search,
            search_in_current,
            &mut out,
        );
        x = search_rect.right();
    } else {
        let r = circle_btn(x, rect);
        if icon_btn(ui, &painter, r, Icon::Search, theme, true, "搜索").clicked() {
            out.actions.push(TopbarAction::ToggleSearch);
        }
        x = r.right();
    }
    let _ = x;

    // ---- ⑦ 排序 / 筛选 / 更多 ----
    let mut rx = right_group.left();
    let sort_r = circle_btn(rx, rect);
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
    if icon_btn(ui, &painter, more_r, Icon::Ellipsis, theme, true, "更多").clicked() {
        out.actions.push(TopbarAction::ToggleMoreMenu);
    }
    out.more_button = Some(more_r);

    out
}

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
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let resp = ui.interact(rect, ui.scope_id().with(("topbar-btn", tooltip)), sense);
    if enabled && resp.hovered() {
        painter.rect_filled(rect, Metrics::TOPBAR_ICON_D / 2.0, theme.sidebar_hover);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let color = if enabled {
        theme.toolbar_icon
    } else {
        theme.text_disabled
    };
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
        [
            pos2(mid_x, rect.top() + 6.0),
            pos2(mid_x, rect.bottom() - 6.0),
        ],
        egui::Stroke::new(1.0, Color32::from_rgba_premultiplied(0, 0, 0, 36)),
    );
    let half_w = rect.width() / 2.0;
    let minus_r = Rect::from_min_size(rect.left_top(), vec2(half_w, rect.height()));
    let plus_r = Rect::from_min_size(pos2(mid_x, rect.top()), vec2(half_w, rect.height()));

    for (r, icon, enabled, tip, action) in [
        (
            minus_r,
            Icon::Minus,
            !zoom_min,
            "缩小缩略图（⌘−）",
            TopbarAction::ZoomOut,
        ),
        (
            plus_r,
            Icon::Plus,
            !zoom_max,
            "放大缩略图（⌘+）",
            TopbarAction::ZoomIn,
        ),
    ] {
        // 端点只响应悬停：再点不会发出一次无效缩放，光标也不会变成手形。
        let sense = if enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
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

/// 排布胶囊：三枚无文字图标（方格 / 纵向瀑布 / 横向瀑布），造型对齐 −|+。
fn draw_layout_pill(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    theme: &Theme,
    current: ViewMode,
    out: &mut TopbarOutput,
) {
    painter.rect_filled(rect, rect.height() / 2.0, theme.seg_bg);
    // 与 −|+ 同款中缝：未选中的两片之间有一条极淡的竖线。
    let w = rect.width() / 3.0;
    for i in 1..3 {
        let sx = rect.left() + i as f32 * w;
        painter.line_segment(
            [pos2(sx, rect.top() + 7.0), pos2(sx, rect.bottom() - 7.0)],
            egui::Stroke::new(1.0, Color32::from_rgba_premultiplied(0, 0, 0, 26)),
        );
    }
    let modes = [
        (ViewMode::Grid, Icon::LayoutGrid, "方格"),
        (ViewMode::MasonryV, Icon::LayoutMasonryV, "纵向瀑布流"),
        (ViewMode::MasonryH, Icon::LayoutMasonryH, "横向瀑布流"),
    ];
    for (i, (mode, icon, label)) in modes.iter().enumerate() {
        let piece = Rect::from_min_size(
            pos2(rect.left() + i as f32 * w, rect.top()),
            vec2(w, rect.height()),
        );
        let active = *mode == current;
        if active {
            let radius = if i == 0 || i == modes.len() - 1 {
                rect.height() / 2.0
            } else {
                8.0
            };
            painter.rect_filled(piece.shrink(1.5), radius, theme.seg_active);
            painter.line_segment(
                [
                    pos2(piece.left() + 4.0, piece.bottom() - 1.5),
                    pos2(piece.right() - 4.0, piece.bottom() - 1.5),
                ],
                egui::Stroke::new(1.0, Color32::from_rgba_premultiplied(0, 0, 0, 14)),
            );
        }
        let resp = ui.interact(piece, ui.scope_id().with(("layout", label)), Sense::click());
        let color = if active {
            theme.toolbar_icon
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
    painter.rect_filled(rect, rect.height() / 2.0, Color32::WHITE);
    painter.rect_stroke(
        rect,
        rect.height() / 2.0,
        egui::Stroke::new(1.0, Color32::from_rgba_premultiplied(0, 0, 0, 36)),
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

    // 右端收起按钮（不把搜索词带进「清空」歧义：收起只是隐藏输入框）
    let close =
        Rect::from_center_size(pos2(rect.right() - 12.0, rect.center().y), vec2(14.0, 14.0));
    let close_resp = ui.interact(close, ui.scope_id().with("search-close"), Sense::click());
    if close_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    icons::draw(
        painter,
        Icon::Close,
        close.shrink(2.0),
        if close_resp.hovered() {
            theme.toolbar_icon
        } else {
            theme.text_secondary
        },
        1.2,
    );
    if close_resp.clicked() {
        out.actions.push(TopbarAction::ToggleSearch);
    }
}

/// 「…」弹出菜单：真实功能项（添加文件夹 / 含子文件夹 / 全屏 / 关于 / 文件名）。
pub fn more_menu(
    ui: &mut Ui,
    at: Pos2,
    recurse_on: bool,
    recurse_available: bool,
    show_filename: bool,
) -> Vec<TopbarAction> {
    let mut actions = Vec::new();
    egui::Area::new(ui.scope_id().with("topbar-more-menu"))
        .fixed_pos(at)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(Color32::WHITE)
                .corner_radius(8.0)
                .stroke(egui::Stroke::new(
                    1.0,
                    Color32::from_rgba_premultiplied(0, 0, 0, 30),
                ))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 18,
                    spread: 0,
                    color: Color32::from_black_alpha(45),
                })
                .inner_margin(egui::Margin::same(5))
                .show(ui, |ui| {
                    ui.set_min_width(210.0);
                    let items: Vec<(String, bool, MoreAction)> = vec![
                        ("添加文件夹…".to_string(), true, MoreAction::AddFolder),
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
                        ("全屏".to_string(), true, MoreAction::ToggleFullscreen),
                        ("关于 OpenPicasa".to_string(), true, MoreAction::About),
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
                .stroke(egui::Stroke::new(
                    1.0,
                    Color32::from_rgba_premultiplied(0, 0, 0, 30),
                ))
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
