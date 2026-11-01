//! 左栏：收藏入口 + 文件夹树 + **底部照片托盘**。
//!
//! ## 托盘为什么在左栏底部
//!
//! 01 U-36 是**已定结论**（原文还留了勘误痕迹：v2.0 初稿曾按「常见看图软件惯例」
//! 写成右侧托盘，核对后确认原版在左栏底部）。02 §8.1 的示意图写的是右栏，
//! 按 01 §11.5.6 的「具约束力」原则，**以 01 为准**。
//!
//! 所以：托盘是左栏 `Panel::bottom(...)`，展开时左栏变高、**栅格宽度不变**（U-37）。

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui,
    pos2, vec2,
};
use picasa_core::{IndexSnapshot, Library, ThumbService, View};
use picasa_db::{Db, PhotoRow};

use crate::icons::{self, Icon};
use crate::index_progress;
use crate::textures::{TexState, TextureCache};
use crate::theme::{Metrics, Theme};

#[derive(Clone, Debug, PartialEq)]
pub enum SidebarAction {
    ShowView(View),
    ToggleFolder(String),
    ToggleSection(String),
    TrayToggle,
    TrayClear,
    TrayRemove(i64),
    TrayOpen(i64),
    /// 右键：路径、屏幕位置
    FolderContext(String, Pos2),
}

pub struct SidebarCtx<'a> {
    pub theme: &'a Theme,
    pub db: &'a Db,
    pub service: &'a ThumbService,
    pub cache: &'a mut TextureCache,
    pub tray_rows: &'a [PhotoRow],
    pub tray_expanded: bool,
    pub favorites_collapsed: bool,
    pub folders_collapsed: bool,
    pub has_focus: bool,
    /// 索引进度快照：左栏的**状态点**与计数必须与面板/弹框同源（`04` §3.2）
    pub snapshot: &'a IndexSnapshot,
}

pub fn show(ui: &mut Ui, lib: &Library, ctx: &mut SidebarCtx<'_>) -> Vec<SidebarAction> {
    let mut actions = Vec::new();
    let theme = *ctx.theme;

    // 托盘在底部（先占位，保证不被上面内容挤出去）
    egui::Panel::bottom("tray-panel")
        .exact_size(if ctx.tray_expanded {
            Metrics::TRAY_EXPANDED_H
        } else {
            Metrics::TRAY_COLLAPSED_H
        })
        .frame(
            egui::Frame::new()
                .fill(theme.sidebar_bg)
                .stroke(Stroke::new(1.0, theme.separator)),
        )
        .show(ui, |ui| {
            tray(ui, lib, &mut *ctx, &mut actions);
        });

    egui::CentralPanel::no_frame()
        .frame(egui::Frame::new().fill(theme.sidebar_bg))
        .show(ui, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, theme.sidebar_bg);

            // ---- 收藏（固定三个入口，U-19）----
            section_header(ui, "收藏", ctx.favorites_collapsed, &theme, &mut actions);
            if !ctx.favorites_collapsed {
                favorites(ui, lib, &*ctx, &mut actions);
            }

            // ---- 文件夹（U-13~U-18）----
            section_header(ui, "文件夹", ctx.folders_collapsed, &theme, &mut actions);
            if !ctx.folders_collapsed {
                folder_tree(ui, lib, &*ctx, &mut actions);
            }
        });

    actions
}

fn section_header(
    ui: &mut Ui,
    title: &str,
    collapsed: bool,
    theme: &Theme,
    actions: &mut Vec<SidebarAction>,
) {
    let t = *theme;
    let (rect, resp) = ui.allocate_exact_size(
        vec2(ui.available_width(), Metrics::SECTION_H),
        Sense::click(),
    );
    let painter = ui.painter_at(rect);

    // ---- 组头白盒（量自 09-library-zh-full.png y=82..102 / 144..164）----
    // 底是 (241,243,246)→(220,221,225) 的竖直渐变，四周 1px (155,155,155) 边框。
    // 之前画的「灰字 + 右端箭头」和原版差得很远——原版是**一个带边框的盒子**。
    {
        use eframe::epaint::Mesh;
        let inner = rect.shrink(1.0); // 渐变铺在边框内侧
        let (c_top, c_bot) = (t.header_top, t.header_bottom);
        let mut mesh = Mesh::default();
        for (p, c) in [
            (inner.left_top(), c_top),
            (inner.right_top(), c_top),
            (inner.right_bottom(), c_bot),
            (inner.left_bottom(), c_bot),
        ] {
            mesh.colored_vertex(p, c);
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(Shape::Mesh(std::sync::Arc::new(mesh)));
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, t.header_border),
            StrokeKind::Inside,
        );
    }

    // ---- 折叠钮：Picasa 绿的圆角方块 + 白色三角（量自参考图 #0A9000）----
    let disc = Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(15.0, 15.0));
    painter.rect_filled(disc, 3.0, t.disclosure_green);
    let tri_h = if collapsed { 5.0 } else { 4.5 };
    let (p1, p2, p3) = if collapsed {
        (
            pos2(disc.center().x - 2.0, disc.center().y - tri_h),
            pos2(disc.center().x - 2.0, disc.center().y + tri_h),
            pos2(disc.center().x + 3.0, disc.center().y),
        )
    } else {
        (
            pos2(disc.center().x - tri_h, disc.center().y - 2.0),
            pos2(disc.center().x + tri_h, disc.center().y - 2.0),
            pos2(disc.center().x, disc.center().y + 3.0),
        )
    };
    painter.add(Shape::convex_polygon(
        vec![p1, p2, p3],
        Color32::WHITE,
        Stroke::NONE,
    ));

    // ---- 标题：粗体 #4D4D4D（量值），文字起点在折叠钮右侧 ----
    painter.text(
        pos2(rect.left() + 34.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::new(12.0, egui::FontFamily::Proportional),
        t.tree_text,
    );
    // 粗体：参考图的组头是 bold。egui 没注册 bold 字族，用 0.5px 重影模拟。
    painter.text(
        pos2(rect.left() + 34.5, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::new(12.0, egui::FontFamily::Proportional),
        t.tree_text,
    );

    if resp.clicked() {
        actions.push(SidebarAction::ToggleSection(title.to_string()));
    }
}

/// I-02：树里的拟物文件夹图标。
///
/// **量自 `09-library-zh-full.png`**（1312×768 原生分辨率，非选中行 y=213..224）：
/// - 箱体 15×12px（行高 22px 里垂直居中）；
/// - 前面板竖直渐变 `#F9DCA1 → #EBCD9A → #C29F68`（逐行实测均值）；
/// - 描边 `#A67C59`（图标区域内最深像素）。
///
/// 用矢量而不是 PNG：15×12 的位图在高分屏上会糊，且免掉资源管线。
/// 后盖（tab）比前面板深一档，形成「盒盖在后面」的层次——这是原版图标的关键特征。
fn draw_folder_icon(painter: &egui::Painter, rect: Rect) {
    const OUTLINE: Color32 = Color32::from_rgb(0xA6, 0x7C, 0x59);
    const BACK: Color32 = Color32::from_rgb(0xC2, 0x9B, 0x5E);
    const FRONT_TOP: Color32 = Color32::from_rgb(0xF9, 0xDC, 0xA1);
    const FRONT_MID: Color32 = Color32::from_rgb(0xEB, 0xCD, 0x9A);
    const FRONT_BOT: Color32 = Color32::from_rgb(0xC2, 0x9F, 0x68);

    let w = rect.width();
    let h = rect.height();
    // 后盖：左上带标签耳（占 60% 宽、高 55%），右下被前面板盖住
    let tab_w = w * 0.55;
    let tab_h = h * 0.5;
    painter.rect_filled(
        Rect::from_min_size(rect.left_top(), vec2(tab_w, tab_h + 1.0)),
        1.0,
        BACK,
    );
    painter.rect_filled(
        Rect::from_min_size(
            rect.left_top() + vec2(0.0, tab_h * 0.6),
            vec2(w, h - tab_h * 0.6),
        ),
        0.0,
        BACK,
    );
    // 前面板：竖直三段渐变（顶亮 → 底深），用 Mesh 顶点色插值
    let front = Rect::from_min_size(
        rect.left_top() + vec2(0.0, tab_h * 0.8),
        vec2(w, h - tab_h * 0.8),
    );
    let mut mesh = eframe::epaint::Mesh::default();
    let mid_y = (front.top() + front.bottom()) * 0.45;
    for (p, c) in [
        (pos2(front.left(), front.top()), FRONT_TOP),
        (pos2(front.right(), front.top()), FRONT_TOP),
        (pos2(front.right(), mid_y), FRONT_MID),
        (pos2(front.left(), mid_y), FRONT_MID),
        (pos2(front.right(), front.bottom()), FRONT_BOT),
        (pos2(front.left(), front.bottom()), FRONT_BOT),
    ] {
        mesh.colored_vertex(p, c);
    }
    // 顶带 0-1-2 / 0-2-3，底带 2-3-4 / 3-4-5（顶点顺序：0左上 1右上 2右中 3左中 4右下 5左下）
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    mesh.add_triangle(3, 2, 4);
    mesh.add_triangle(3, 4, 5);
    painter.add(Shape::Mesh(std::sync::Arc::new(mesh)));
    // 描边：前面板 + 整体轮廓各一圈 1px
    painter.rect_stroke(front, 0.0, Stroke::new(1.0, OUTLINE), StrokeKind::Inside);
    painter.rect_stroke(
        Rect::from_min_size(rect.left_top(), vec2(tab_w, tab_h + 1.0)),
        1.0,
        Stroke::new(1.0, OUTLINE),
        StrokeKind::Inside,
    );
}

fn favorites(ui: &mut Ui, lib: &Library, ctx: &SidebarCtx<'_>, actions: &mut Vec<SidebarAction>) {
    let t = *ctx.theme;
    // 所有照片
    let row = fav_row(
        ui,
        &t,
        "所有照片",
        Some(lib.total_in_db),
        matches!(lib.view, View::All),
        true,
        ctx.has_focus,
    );
    if row.clicked() {
        actions.push(SidebarAction::ShowView(View::All));
    }
    // 最近更改
    let row = fav_row(
        ui,
        &t,
        "最近更改",
        None,
        matches!(lib.view, View::Recent),
        true,
        ctx.has_focus,
    );
    if row.clicked() {
        actions.push(SidebarAction::ShowView(View::Recent));
    }
    // 星标：**置灰 + 说明**（U-19 的唯一例外，红线 R-2 允许）
    let row = fav_row(ui, &t, "星标", None, false, false, ctx.has_focus);
    row.on_hover_text("本版本暂不支持星标（M2 提供）。这里保留入口是为了保持左栏版式与原版一致。");
}

fn fav_row(
    ui: &mut Ui,
    theme: &Theme,
    label: &str,
    count: Option<u64>,
    active: bool,
    enabled: bool,
    has_focus: bool,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), Metrics::ROW_H), Sense::click());
    let painter = ui.painter_at(rect);
    if active {
        let bg = if has_focus {
            theme.sidebar_selected
        } else {
            theme.sidebar_selected_inactive
        };
        painter.rect_filled(rect, 0.0, bg);
    } else if resp.hovered() && enabled {
        painter.rect_filled(rect, 0.0, theme.cell_hover);
    }
    let text_color = if !enabled {
        theme.text_disabled
    } else if active && has_focus {
        Color32::WHITE
    } else {
        theme.text_primary
    };
    painter.text(
        pos2(rect.left() + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::new(12.5, egui::FontFamily::Proportional),
        text_color,
    );
    if let Some(n) = count {
        // U-15：计数灰色小字右对齐
        painter.text(
            pos2(rect.right() - 10.0, rect.center().y),
            Align2::RIGHT_CENTER,
            n.to_string(),
            FontId::new(11.0, egui::FontFamily::Proportional),
            if active && has_focus {
                Color32::from_white_alpha(220)
            } else {
                theme.text_secondary
            },
        );
    }
    if resp.hovered() && enabled {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    resp
}

fn folder_tree(ui: &mut Ui, lib: &Library, ctx: &SidebarCtx<'_>, actions: &mut Vec<SidebarAction>) {
    let t = *ctx.theme;
    let rows = &lib.visible_folders;
    if rows.is_empty() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(
                egui::RichText::new("还没有加入任何文件夹\n点上方「添加文件夹」")
                    .size(11.5)
                    .color(t.text_secondary),
            );
        });
        return;
    }

    let row_h = Metrics::ROW_H;
    let total = rows.len();

    // 目录多时虚拟化（1 万目录也不卡）
    egui::ScrollArea::vertical()
        .id_salt("folder-tree")
        .auto_shrink([false, false])
        .show_rows(ui, row_h, total, |ui, range| {
            for i in range {
                let Some(row) = rows.get(i) else { continue };
                let active = matches!(lib.view, View::Folder(id) if Some(id) == row.folder_id);
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let painter = ui.painter_at(rect);

                if active {
                    let bg = if ctx.has_focus {
                        t.sidebar_selected
                    } else {
                        t.sidebar_selected_inactive
                    };
                    painter.rect_filled(rect, 0.0, bg);
                } else if resp.hovered() {
                    painter.rect_filled(rect, 0.0, t.cell_hover);
                }

                // U-14：每级缩进固定步长
                let indent = 8.0 + row.depth as f32 * Metrics::INDENT;
                // 图标槽：所有行的图标都从 `indent + 12` 起（有折叠箭头的行，
                // 箭头占用前面的 12px 槽）——保证父子行的图标**垂直对齐**，
                // 这与参考图里「图标全部左对齐」一致。
                let icon_x = rect.left() + indent + 12.0;
                // 折叠箭头（有子目录才出现，U-17）
                if row.has_children {
                    let chev = Rect::from_center_size(
                        pos2(rect.left() + indent + 5.0, rect.center().y),
                        vec2(10.0, 10.0),
                    );
                    icons::draw(
                        &painter,
                        if row.collapsed {
                            Icon::ChevronRight
                        } else {
                            Icon::ChevronDown
                        },
                        chev,
                        if active && ctx.has_focus {
                            Color32::WHITE
                        } else {
                            t.text_secondary
                        },
                        1.3,
                    );
                    // 箭头是可点的：窄区域，避免误触「选中」
                    let chev_rect = Rect::from_center_size(
                        pos2(rect.left() + indent + 5.0, rect.center().y),
                        vec2(14.0, row_h),
                    );
                    let chev_resp = ui.interact(
                        chev_rect,
                        ui.scope_id().with(("chev", &row.path)),
                        Sense::click(),
                    );
                    if chev_resp.clicked() {
                        actions.push(SidebarAction::ToggleFolder(row.path.clone()));
                        continue;
                    }
                }

                // I-02：拟物文件夹图标（15×12，量自参考图）。选中行图标不反色——
                // 参考图里选中行的文件夹仍是彩色的。
                draw_folder_icon(
                    &painter,
                    Rect::from_min_size(pos2(icon_x, rect.center().y - 6.0), vec2(15.0, 12.0)),
                );

                // 文字与图标间距：量自参考图（图标右沿 x=45，文字起点 x≈50）
                let text_x = icon_x + 15.0 + 5.0;
                painter.text(
                    pos2(text_x, rect.center().y),
                    Align2::LEFT_CENTER,
                    &row.name,
                    FontId::new(12.0, egui::FontFamily::Proportional),
                    if active && ctx.has_focus {
                        Color32::WHITE
                    } else {
                        t.tree_text
                    },
                );
                // U-15：计数右对齐、灰色；为 0 不显示
                //
                // M2：关注文件夹多一个**状态点**（`04` §3.2 第三行）。有任务时把计数
                // 左移 8px 给点让位——数字本身不变，仍然是「已发现」口径，
                // 这样左栏与面板同一行数字一致。
                let task = ctx.snapshot.task_by_path(&row.path);
                let count_right = if task.is_some() {
                    rect.right() - 16.0
                } else {
                    rect.right() - 8.0
                };
                if row.count > 0 {
                    painter.text(
                        pos2(count_right, rect.center().y),
                        Align2::RIGHT_CENTER,
                        // 参考图里 Picasa 的计数是 `(1)` 形式，与标签同色（#4D4D4D）
                        format!("({})", row.count),
                        FontId::new(11.0, egui::FontFamily::Proportional),
                        if active && ctx.has_focus {
                            Color32::from_white_alpha(210)
                        } else {
                            t.tree_text
                        },
                    );
                }
                if let Some(task) = task {
                    index_progress::draw_state_dot(
                        &painter,
                        pos2(rect.right() - 7.0, rect.center().y),
                        task.state,
                        &t,
                    );
                    resp.clone().on_hover_text(format!(
                        "{}：{} · 已完成 {} / 已发现 {}",
                        task.state.label(),
                        task.name,
                        task.completed,
                        task.discovered
                    ));
                }

                if resp.clicked()
                    && let Some(id) = row.folder_id
                {
                    actions.push(SidebarAction::ShowView(View::Folder(id)));
                }
                if resp.secondary_clicked() {
                    actions.push(SidebarAction::FolderContext(
                        row.path.clone(),
                        resp.interact_pointer_pos().unwrap_or(rect.center()),
                    ));
                }
            }
        });
}

fn tray(ui: &mut Ui, lib: &Library, ctx: &mut SidebarCtx<'_>, actions: &mut Vec<SidebarAction>) {
    let t = *ctx.theme;
    let width = ui.available_width();
    let header_h = Metrics::TRAY_COLLAPSED_H;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, header_h), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, t.chrome_bg);

    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 16.0, rect.center().y), vec2(13.0, 13.0));
    icons::draw(&painter, Icon::Tray, icon_rect, t.text_secondary, 1.3);

    let count = lib.tray.len();
    painter.text(
        pos2(rect.left() + 28.0, rect.center().y),
        Align2::LEFT_CENTER,
        if count == 0 {
            "照片托盘（空）".to_string()
        } else {
            format!("照片托盘 · {count} 张")
        },
        FontId::new(11.5, egui::FontFamily::Proportional),
        t.text_secondary,
    );
    // 展开/收起箭头（U-37）
    let chev = Rect::from_center_size(pos2(rect.right() - 14.0, rect.center().y), vec2(9.0, 9.0));
    icons::draw(
        &painter,
        if ctx.tray_expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        },
        chev,
        t.text_secondary,
        1.3,
    );
    if resp.clicked() {
        actions.push(SidebarAction::TrayToggle);
    }

    if !ctx.tray_expanded {
        return;
    }

    // 展开：横向缩略图条 + 清空
    ui.add_space(2.0);
    let body_h = Metrics::TRAY_EXPANDED_H - header_h - 6.0;
    let (body, _) = ui.allocate_exact_size(vec2(width, body_h), Sense::hover());
    let painter = ui.painter_at(body);

    if lib.tray.is_empty() {
        painter.text(
            body.center(),
            Align2::CENTER_CENTER,
            "选中照片后按 Ctrl+H 放入托盘",
            FontId::new(11.0, egui::FontFamily::Proportional),
            t.text_secondary,
        );
        return;
    }

    let side = Metrics::TRAY_THUMB;
    let gap = 4.0;
    let mut x = body.left() + 6.0;
    let mut budget = 1;
    for row in ctx.tray_rows {
        if x + side > body.right() {
            break;
        }
        let cell = Rect::from_min_size(pos2(x, body.top()), vec2(side, side));
        painter.rect_filled(cell, 1.0, Color32::WHITE);
        let state = ctx
            .cache
            .get(ui.ctx(), ctx.db, ctx.service, row, &mut budget);
        if let TexState::Ready(tex, _) = state {
            painter.image(
                tex,
                cell.shrink(2.0),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let resp = ui.interact(cell, ui.scope_id().with(("tray", row.id)), Sense::click());
        painter.rect_stroke(
            cell,
            1.0,
            Stroke::new(
                1.0,
                if resp.hovered() {
                    t.selected
                } else {
                    t.cell_border
                },
            ),
            StrokeKind::Inside,
        );
        if resp.double_clicked() {
            actions.push(SidebarAction::TrayOpen(row.id));
        }
        if resp.secondary_clicked() {
            actions.push(SidebarAction::TrayRemove(row.id));
        }
        x += side + gap;
    }

    // 清空按钮（U-38）
    let clear = Rect::from_min_size(
        pos2(body.right() - 40.0, body.bottom() - 18.0),
        vec2(36.0, 16.0),
    );
    let cresp = ui.interact(clear, ui.scope_id().with("tray-clear"), Sense::click());
    painter.text(
        clear.center(),
        Align2::CENTER_CENTER,
        "清空",
        FontId::new(11.0, egui::FontFamily::Proportional),
        if cresp.hovered() {
            t.text_primary
        } else {
            t.text_secondary
        },
    );
    if cresp.clicked() {
        actions.push(SidebarAction::TrayClear);
    }
}
