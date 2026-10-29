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
    self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2,
    vec2,
};
use picasa_core::{Library, ThumbService, View};
use picasa_db::{Db, PhotoRow};

use crate::icons::{self, Icon};
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
    painter.text(
        pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::new(11.5, egui::FontFamily::Proportional),
        t.text_secondary,
    );
    // 折叠箭头（U-13：灰色小字 + 折叠箭头，不是按钮样式）
    let chev = Rect::from_center_size(pos2(rect.right() - 14.0, rect.center().y), vec2(9.0, 9.0));
    icons::draw(
        &painter,
        if collapsed {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        },
        chev,
        t.text_secondary,
        1.2,
    );
    if resp.clicked() {
        actions.push(SidebarAction::ToggleSection(title.to_string()));
    }
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
                // 折叠箭头（有子目录才出现，U-17）
                if row.has_children {
                    let chev = Rect::from_center_size(
                        pos2(rect.left() + indent, rect.center().y),
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
                        pos2(rect.left() + indent, rect.center().y),
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

                let text_x = rect.left() + indent + 14.0;
                painter.text(
                    pos2(text_x, rect.center().y),
                    Align2::LEFT_CENTER,
                    &row.name,
                    FontId::new(12.0, egui::FontFamily::Proportional),
                    if active && ctx.has_focus {
                        Color32::WHITE
                    } else {
                        t.text_primary
                    },
                );
                // U-15：计数右对齐、灰色；为 0 不显示
                if row.count > 0 {
                    painter.text(
                        pos2(rect.right() - 8.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        row.count.to_string(),
                        FontId::new(10.5, egui::FontFamily::Proportional),
                        if active && ctx.has_focus {
                            Color32::from_white_alpha(210)
                        } else {
                            t.text_secondary
                        },
                    );
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
