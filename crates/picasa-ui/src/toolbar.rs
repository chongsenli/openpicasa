//! 顶部工具栏：图标在上、文案在下，按钮之间以竖线分组（U-06~U-12）。
//!
//! ## 红线 R-2 在这里的体现
//!
//! **M1 的工具栏里没有「编辑」「编辑室」「星标」按钮。** 不是置灰，是**不存在**
//! （01 U-12 的验收判据就是「全量截图核对无编辑入口」）。
//!
//! 唯一保留的置灰入口是左栏的「星标」——它是版式的一部分（U-19 的例外说明）。

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Response, Sense, Ui, pos2, vec2};
use picasa_core::Tier;
use picasa_db::SortKey;

use crate::icons::{self, Icon};
use crate::theme::{Metrics, Theme};

/// 工具栏产出的动作。App 负责执行（工具栏自己不碰状态）。
#[derive(Clone, Debug, PartialEq)]
pub enum ToolbarAction {
    AddFolder,
    RevealCurrent,
    ToggleSortMenu,
    PickSort(SortKey),
    ToggleSortDirection,
    CycleTier,
    ToggleFullscreen,
    /// 菜单「档案 → 退出」（Ctrl+Q 同款路径）
    Quit,
    /// 菜单「说明 → 关于」：App 用 toast 回应（不做独立窗口）
    About,
}

/// 「索引管理」二级菜单里的动作。
///
/// 这里**只放用户真的需要在主界面随手点到的两件事**：修坏图、看进度。
/// 暂停/删除索引属于「看着进度决定」的操作，留在进度面板里，
/// 主界面不再单开「索引管理」入口 —— M2.2 §2.6 起，状态栏是唯一入口。
pub struct ToolbarOutput {
    pub actions: Vec<ToolbarAction>,
    /// 搜索框内容（双向绑定由 App 处理）
    pub search_changed: Option<String>,
    pub sort_button: Option<Response>,
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    theme: &Theme,
    tier: Tier,
    sort: picasa_db::SortSpec,
    has_selection: bool,
    search: &mut String,
    sort_menu_open: bool,
) -> ToolbarOutput {
    let mut out = ToolbarOutput {
        actions: Vec::new(),
        search_changed: None,
        sort_button: None,
    };
    let rect = Rect::from_min_size(
        ui.max_rect().left_top(),
        vec2(ui.max_rect().width(), Metrics::TOOLBAR_H),
    );
    let _ = ui.allocate_space(vec2(rect.width(), rect.height()));

    // ---- 段 1：菜单栏（21px，白→淡蓝渐变，底边 #B6BCCC）----
    // 量自 09-library-zh-full.png y=20..40。原版有 8 个菜单（档案…说明），
    // 我们只放**有真实功能项**的三个 —— R-2 红线：编辑(E)/建立(C) 是编辑入口，
    // M1 判据「无编辑入口」照旧适用，版式照抄不等于照抄功能。
    let mb = Rect::from_min_size(rect.left_top(), vec2(rect.width(), Metrics::MENUBAR_H));
    draw_menubar(ui, mb, theme, &mut out);
    painter_line(
        ui,
        pos2(mb.left(), mb.bottom() + 0.5),
        pos2(mb.right(), mb.bottom() + 0.5),
        Metrics::MENUBAR_BORDER,
    );

    // ---- 段 2：工具行（38px，#E8E8E8 底，横排小按钮 + 右侧搜索框）----
    let tr = Rect::from_min_size(
        pos2(rect.left(), mb.bottom() + 1.0),
        vec2(rect.width(), Metrics::TOOLROW_H),
    );
    let painter = ui.painter_at(tr);
    painter.rect_filled(tr, 0.0, Metrics::TOOLROW_BG);
    painter.line_segment(
        [tr.left_bottom(), tr.right_bottom()],
        egui::Stroke::new(1.0, Metrics::TOOLROW_BORDER),
    );

    let mut x = tr.left() + 6.0;
    let btn_h = 26.0;
    let btn_y = tr.top() + (Metrics::TOOLROW_H - btn_h) * 0.5;

    // 组 1：返回（M1 恒禁用，U-09 保留占位）
    let back_rect = Rect::from_min_size(pos2(x, btn_y), vec2(30.0, btn_h));
    row_button(
        ui,
        &painter,
        back_rect,
        Icon::Back,
        "",
        false,
        "返回图库（M1 无其他视图）",
        theme,
    );
    x = back_rect.right() + 6.0;
    x = draw_sep(&painter, x, tr);

    // 组 2：添加文件夹 / 显示位置
    let add_rect = Rect::from_min_size(pos2(x, btn_y), vec2(88.0, btn_h));
    if row_button(
        ui,
        &painter,
        add_rect,
        Icon::AddFolder,
        "添加文件夹",
        true,
        "把一个照片目录加入图库（只读，不改你的文件）",
        theme,
    ) {
        out.actions.push(ToolbarAction::AddFolder);
    }
    x = add_rect.right() + 4.0;
    let reveal_rect = Rect::from_min_size(pos2(x, btn_y), vec2(76.0, btn_h));
    if row_button(
        ui,
        &painter,
        reveal_rect,
        Icon::Reveal,
        "显示位置",
        has_selection,
        "在 Finder 中显示选中照片（Ctrl+Enter）",
        theme,
    ) {
        out.actions.push(ToolbarAction::RevealCurrent);
    }
    x = reveal_rect.right() + 6.0;
    x = draw_sep(&painter, x, tr);

    // 组 3：排序 / 缩略图大小
    let sort_rect = Rect::from_min_size(pos2(x, btn_y), vec2(78.0, btn_h));
    let sort_label = sort_label(sort.key, sort.descending);
    let sort_resp = row_button_resp(
        ui,
        &painter,
        sort_rect,
        Icon::Sort,
        sort_label,
        true,
        "排序方式",
        theme,
        sort_menu_open,
    );
    if sort_resp.clicked() {
        out.actions.push(ToolbarAction::ToggleSortMenu);
    }
    out.sort_button = Some(sort_resp);
    x = sort_rect.right() + 4.0;
    let size_rect = Rect::from_min_size(pos2(x, btn_y), vec2(72.0, btn_h));
    if row_button(
        ui,
        &painter,
        size_rect,
        Icon::GridSize,
        "缩略图",
        true,
        "切换缩略图大小（Ctrl+1 小 / Ctrl+2 大）",
        theme,
    ) {
        out.actions.push(ToolbarAction::CycleTier);
    }
    x = size_rect.right() + 6.0;
    x = draw_sep(&painter, x, tr);

    // 组 4：全屏
    let fs_rect = Rect::from_min_size(pos2(x, btn_y), vec2(56.0, btn_h));
    if row_button(
        ui,
        &painter,
        fs_rect,
        Icon::Fullscreen,
        "全屏",
        true,
        "全屏（F11）",
        theme,
    ) {
        out.actions.push(ToolbarAction::ToggleFullscreen);
    }

    // ---- 右侧：搜索框（参考图 x=1250 处：#7F9DB9 边 + 白芯，高 23px）----
    let search_w = 200.0_f32.min(tr.width() * 0.30);
    let search_rect = Rect::from_min_size(
        pos2(tr.right() - search_w - 12.0, tr.center().y - 11.5),
        vec2(search_w, 23.0),
    );
    painters_search_box(ui, &painter, search_rect, theme, search, &mut out);

    // 档位提示（搜索框左侧小字；原版工具行中部也有灰字提示）
    painter.text(
        pos2(search_rect.left() - 8.0, tr.center().y),
        Align2::RIGHT_CENTER,
        format!("{}px", Metrics::CELL_SIZES[tier.index()] as i32),
        FontId::new(10.0, egui::FontFamily::Proportional),
        theme.text_secondary,
    );

    out
}

/// 一个菜单项：`(标题, 可用, 点击后发出的动作)`。
type MenuItem<'a> = (&'a str, bool, ToolbarAction);
/// 一个菜单：`(菜单名, 菜单项列表)`。
type MenuDef<'a> = (&'a str, &'a [MenuItem<'a>]);

/// 菜单栏：渐变底 + 文字菜单（egui `menu_button`，静止态透明底、悬停才亮）。
fn draw_menubar(ui: &mut Ui, rect: Rect, _theme: &Theme, out: &mut ToolbarOutput) {
    let painter = ui.painter_at(rect);
    // 三段渐变：顶 #FEFEFF → 中 #D4DBEE → 底 #E1E6F6（两个三角带）
    use eframe::epaint::Mesh;
    let mut mesh = Mesh::default();
    let mid_y = rect.top() + rect.height() * 0.45;
    for (p, c) in [
        (pos2(rect.left(), rect.top()), Metrics::MENUBAR_TOP),
        (pos2(rect.right(), rect.top()), Metrics::MENUBAR_TOP),
        (pos2(rect.right(), mid_y), Metrics::MENUBAR_MID),
        (pos2(rect.left(), mid_y), Metrics::MENUBAR_MID),
        (pos2(rect.right(), rect.bottom()), Metrics::MENUBAR_BOT),
        (pos2(rect.left(), rect.bottom()), Metrics::MENUBAR_BOT),
    ] {
        mesh.colored_vertex(p, c);
    }
    // 上下两个四边形（顶点 0-1-2-3 与 2-3-4-5）
    for (a, b, c) in [(0, 1, 2), (0, 2, 3), (2, 4, 5), (2, 5, 3)] {
        mesh.add_triangle(a, b, c);
    }
    painter.add(egui::Shape::Mesh(std::sync::Arc::new(mesh)));

    // 菜单项。egui menu_button 的字色跟 override_text_color 走（#202020），静止底透明。
    let menus: &[MenuDef] = &[
        (
            "档案(F)",
            &[
                ("添加文件夹", true, ToolbarAction::AddFolder),
                ("在 Finder 中显示", false, ToolbarAction::RevealCurrent),
            ],
        ),
        (
            "视图(V)",
            &[
                ("排序方式…", true, ToolbarAction::ToggleSortMenu),
                ("切换缩略图大小", true, ToolbarAction::CycleTier),
                ("全屏", true, ToolbarAction::ToggleFullscreen),
            ],
        ),
        (
            "说明(H)",
            &[("关于 OpenPicasa", true, ToolbarAction::About)],
        ),
    ];
    for (title, items) in menus {
        ui.menu_button(*title, |ui| {
            for (label, enabled, action) in *items {
                if ui
                    .add_enabled(*enabled, egui::Button::new(*label))
                    .clicked()
                {
                    out.actions.push(action.clone());
                    ui.close();
                }
            }
        });
    }
}

fn sort_label(key: SortKey, desc: bool) -> &'static str {
    match key {
        SortKey::TakenAt => {
            if desc {
                "按日期"
            } else {
                "按日期↑"
            }
        }
        SortKey::Name => {
            if desc {
                "按名称↓"
            } else {
                "按名称"
            }
        }
        SortKey::ModifiedAt => "修改时间",
        SortKey::Size => "文件大小",
        SortKey::Dimensions => "尺寸",
    }
}

fn painter_line(ui: &Ui, a: Pos2, b: Pos2, color: Color32) {
    ui.painter()
        .line_segment([a, b], egui::Stroke::new(1.0, color));
}

/// 工具行组间竖线，返回推进后的 x。
fn draw_sep(painter: &egui::Painter, x: f32, rect: Rect) -> f32 {
    painter.line_segment(
        [pos2(x, rect.top() + 7.0), pos2(x, rect.bottom() - 7.0)],
        egui::Stroke::new(1.0, Color32::from_rgb(0xC8, 0xC8, 0xC8)),
    );
    x + 6.0
}

/// 工具行按钮：图标在左、文字在右（一行内横排，区别于旧版的上下堆叠）。
#[allow(clippy::too_many_arguments)]
fn row_button(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    icon: Icon,
    label: &str,
    enabled: bool,
    tooltip: &str,
    theme: &Theme,
) -> bool {
    row_button_resp(
        ui, painter, rect, icon, label, enabled, tooltip, theme, false,
    )
    .clicked()
}

#[allow(clippy::too_many_arguments)]
fn row_button_resp(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    icon: Icon,
    label: &str,
    enabled: bool,
    tooltip: &str,
    theme: &Theme,
    pressed: bool,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let resp = ui.interact(rect, ui.scope_id().with(("toolrow", label)), sense);
    let hovered = enabled && resp.hovered();

    if pressed || hovered {
        painter.rect_filled(rect, 3.0, theme.cell_hover);
    }

    let color = if enabled {
        theme.text_primary
    } else {
        theme.text_disabled
    };
    let mut cx = rect.left() + 6.0;
    if !label.is_empty() {
        let icon_rect = Rect::from_center_size(pos2(cx, rect.center().y), vec2(14.0, 14.0));
        icons::draw(painter, icon, icon_rect, color, 1.3);
        cx += 18.0;
        painter.text(
            pos2(cx, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::new(11.5, egui::FontFamily::Proportional),
            color,
        );
    } else {
        let icon_rect =
            Rect::from_center_size(pos2(rect.center().x, rect.center().y), vec2(14.0, 14.0));
        icons::draw(painter, icon, icon_rect, color, 1.3);
    }

    if hovered || !enabled {
        resp.clone().on_hover_text(tooltip);
    }
    resp
}

/// 搜索框：白底、#7F9DB9 蓝灰边（量自参考图 x=1250，XP 经典风格）、左侧放大镜。
fn painters_search_box(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    theme: &Theme,
    search: &mut String,
    out: &mut ToolbarOutput,
) {
    painter.rect_filled(rect, 2.0, Color32::WHITE);
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, Metrics::SEARCH_BORDER),
        egui::StrokeKind::Inside,
    );
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 10.0, rect.center().y), vec2(12.0, 12.0));
    icons::draw(painter, Icon::Search, icon_rect, theme.text_secondary, 1.2);

    let edit_rect = Rect::from_min_max(
        pos2(rect.left() + 20.0, rect.top() + 2.0),
        pos2(rect.right() - 4.0, rect.bottom() - 2.0),
    );
    let id = ui.scope_id().with("search-box");
    let mut buf = search.clone();
    let resp = ui.put(
        edit_rect,
        egui::TextEdit::singleline(&mut buf)
            .id(id)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::symmetric(2, 3))
            .hint_text("搜索文件名 / 文件夹 / 日期")
            .text_color(theme.text_primary),
    );
    if resp.changed() {
        *search = buf.clone();
        out.search_changed = Some(buf);
    }
}

/// 排序弹出菜单（由 App 画，因为它要管开关状态）。
pub fn sort_menu(ui: &mut Ui, at: Pos2, current: picasa_db::SortSpec) -> Option<ToolbarAction> {
    let mut action = None;
    egui::Area::new(ui.scope_id().with("sort-menu"))
        .fixed_pos(at)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(Color32::WHITE)
                .stroke(egui::Stroke::new(1.0, Color32::from_gray(0xC0)))
                .corner_radius(3.0)
                .inner_margin(egui::Margin::same(4))
                .shadow(egui::epaint::Shadow {
                    offset: [1, 2],
                    blur: 6,
                    spread: 0,
                    color: Color32::from_black_alpha(40),
                })
                .show(ui, |ui| {
                    ui.set_min_width(150.0);
                    for (key, label) in [
                        (SortKey::TakenAt, "拍摄日期"),
                        (SortKey::Name, "文件名"),
                        (SortKey::ModifiedAt, "修改日期"),
                        (SortKey::Size, "文件大小"),
                        (SortKey::Dimensions, "图片尺寸"),
                    ] {
                        let mark = if current.key == key { "✓ " } else { "   " };
                        if ui
                            .add(egui::Button::new(format!("{mark}{label}")).frame(false))
                            .clicked()
                        {
                            action = Some(ToolbarAction::PickSort(key));
                        }
                    }
                    ui.separator();
                    let dir = if current.descending {
                        "↓ 降序（点击切换为升序）"
                    } else {
                        "↑ 升序（点击切换为降序）"
                    };
                    if ui.add(egui::Button::new(dir).frame(false)).clicked() {
                        action = Some(ToolbarAction::ToggleSortDirection);
                    }
                });
        });
    action
}

// ⚠️ M2.2 §2.6：原「索引管理」二级菜单**已整体移除**。
// 它做的两件事（修坏图、看进度）现在都在状态栏唯一入口 → 索引管理页里，
// 位置见 [`crate::index_mgr`]。这里不留残骸：留着就是第二个入口。
