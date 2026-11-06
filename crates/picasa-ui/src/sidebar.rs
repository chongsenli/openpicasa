//! 左栏：中区左侧的「标题 + A 样式文件树」（M3.1 §7 定稿）。
//!
//! ## 保留的功能（与 M4 版一致，条目一个不少）
//!
//! * 「图库」组：所有照片 / 最近更改 / 星标（→ 只看收藏筛选）
//! * 时间轴：按拍摄年份筛选（数据来自 `timeline_years`）
//! * 「文件夹」组：真实目录树
//! * 底部照片托盘（收起 / 展开行为不变）
//!
//! ## §7 版式（覆盖 M3.1 早期的 Photos 大行高侧栏）
//!
//! | 项 | 现值 |
//! | --- | --- |
//! | 标题区 | 文件夹名小标题 + `N 张 · 拍摄日期跨度`，在树**之上** |
//! | 文件树 | 仿方案 A：文件夹图标、树线、26px 密行高 |
//! | 选中行 | 浅灰胶囊 + 深字 |
//! | 左栏底 | #F5F4F2 |
//!
//! 截图里的条目（精选集 / 固定 / 相簿列表 / 最近删除等）在本应用**没有对应
//! 功能**，按用户拍板（2026-11-03）不放假条目：功能集合不变，只是重绘。

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
    /// 左栏「星标」行：切换「只看收藏」筛选。
    ToggleStarredFilter,
    /// 时间轴：按某一年筛选（拍摄日期）。
    TimelineYear(i32),
    ToggleFolder(String),
    ToggleSection(String),
    TrayToggle,
    TrayClear,
    TrayRemove(i64),
    TrayOpen(i64),
    /// 右键：路径、屏幕位置
    FolderContext(String, Pos2),
    /// M4：按标签筛选照片（`M4-ORG-03`）
    FilterByTag(String),
    /// M4：按相册筛选照片（`M4-ORG-03/08`）
    FilterByAlbum(String),
    /// M4：折叠标签 / 相册分组
    ToggleSectionM4(String),
    /// M4：新建标签 / 相册
    NewTag,
    NewAlbum,
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
    /// M4：标签 / 相册分组的折叠状态
    pub tags_collapsed: bool,
    pub albums_collapsed: bool,
    /// M4：标签 / 相册列表（**由 App 缓存后传入**）。
    /// ⚠️ 不能在这里每帧查库：`all_tags()` 是全表 GROUP BY、`all_albums()` 带
    /// 相关子查询计数，绘制每帧都跑会拖慢缩略图展示（实机反馈的性能回归）。
    pub tags: &'a [(String, i64)],
    pub albums: &'a [(String, i64)],
    pub has_focus: bool,
    /// 索引进度快照：左栏的**状态点**与计数必须与面板/弹框同源（`04` §3.2）
    pub snapshot: &'a IndexSnapshot,
}

pub fn show(
    ui: &mut Ui,
    lib: &Library,
    ctx: &mut SidebarCtx<'_>,
    // M32-UI-04 v1.2 起不再使用：标题迁到主区顶部（参数保留以减少调用点改动）
    _title: &str,
    _subtitle: &str,
) -> Vec<SidebarAction> {
    let mut actions = Vec::new();
    let theme = *ctx.theme;

    // M32-UI-04 v1.2：标题迁到主区顶部（`app.rs` 的 view-header-panel），
    // 左栏只保留文件树与底部托盘（§7.3 的标题区在此退役）。

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
                .stroke(Stroke::new(1.0, theme.toolbar_border)),
        )
        .show(ui, |ui| {
            tray(ui, lib, &mut *ctx, &mut actions);
        });

    egui::CentralPanel::no_frame()
        .frame(egui::Frame::new().fill(theme.sidebar_bg))
        .show(ui, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, theme.sidebar_bg);

            // ---- 图库（截图的「图库」组：三行主入口）----
            section_title(ui, "图库", ctx.favorites_collapsed, &mut actions);
            if !ctx.favorites_collapsed {
                favorites(ui, lib, &*ctx, &mut actions);
            }

            // ---- 文件夹（真实目录树）----
            ui.add_space(6.0);
            section_title(ui, "文件夹", ctx.folders_collapsed, &mut actions);
            if !ctx.folders_collapsed {
                folder_tree(ui, lib, &*ctx, &mut actions);
            }

            // ---- M4：标签 / 相册（应用内虚拟集合，不是磁盘目录）----
            ui.add_space(6.0);
            m4_group(
                ui,
                "标签",
                ctx.tags_collapsed,
                ctx.tags.to_vec(),
                &mut actions,
                true,
            );
            ui.add_space(6.0);
            m4_group(
                ui,
                "相册",
                ctx.albums_collapsed,
                ctx.albums.to_vec(),
                &mut actions,
                false,
            );
        });

    actions
}

/// 分组标题：灰色小字（截图的「相簿」样式），整行可点 = 折叠。
fn section_title(ui: &mut Ui, title: &str, collapsed: bool, actions: &mut Vec<SidebarAction>) {
    let (rect, resp) = ui.allocate_exact_size(
        vec2(ui.available_width(), Metrics::SIDEBAR_SECTION_H),
        Sense::click(),
    );
    let painter = ui.painter_at(rect);
    let color = Theme::picasa().sidebar_section_text;
    painter.text(
        pos2(rect.left() + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::new(11.0, egui::FontFamily::Proportional),
        color,
    );
    // 折叠指示：右端小箭头（截图分组默认展开、悬停才现箭头；这里常显更可用）
    let chev = Rect::from_center_size(pos2(rect.right() - 14.0, rect.center().y), vec2(9.0, 9.0));
    icons::draw(
        &painter,
        if collapsed {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        },
        chev,
        color.gamma_multiply(0.8),
        1.2,
    );
    if resp.clicked() {
        actions.push(SidebarAction::ToggleSection(title.to_string()));
    }
}

/// M4 标签 / 相册分组（docs/12 §8.1）。
///
/// 与「文件夹」组刻意区分：这里是**应用内虚拟集合**，数据只存自有 DB，
/// 不会在磁盘上新建目录。组头带「＋」新建入口，行显示计数。
fn m4_group(
    ui: &mut Ui,
    title: &str,
    collapsed: bool,
    items: Vec<(String, i64)>,
    actions: &mut Vec<SidebarAction>,
    is_tag: bool,
) {
    let (rect, resp) = ui.allocate_exact_size(
        vec2(ui.available_width().max(1.0), Metrics::SIDEBAR_SECTION_H),
        Sense::click(),
    );
    // 防御：窗口极窄 / 侧栏收起过渡帧时 rect 可能退化（宽度 0 或负）。
    // 退化几何会让 egui 后续合并 Mesh 时产生无效索引（debug 下 `is_valid()`
    // 断言 panic：Other mesh is invalid），所以这里直接跳过绘制。
    if rect.width() < 2.0 || rect.height() < 2.0 || !rect.is_finite() {
        return;
    }
    let painter = ui.painter_at(rect);
    let color = Theme::picasa().sidebar_section_text;
    painter.text(
        pos2(rect.left() + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::new(11.0, egui::FontFamily::Proportional),
        color,
    );
    // 右端「＋」新建
    let plus = Rect::from_center_size(pos2(rect.right() - 14.0, rect.center().y), vec2(11.0, 11.0));
    icons::draw(&painter, Icon::Plus, plus, color, 1.2);
    if resp.clicked() {
        if plus.contains(resp.interact_pointer_pos().unwrap_or_default()) {
            actions.push(if is_tag {
                SidebarAction::NewTag
            } else {
                SidebarAction::NewAlbum
            });
        } else {
            actions.push(SidebarAction::ToggleSectionM4(title.to_string()));
        }
    }
    if collapsed {
        return;
    }
    for (name, count) in items {
        let (r, row_resp) = ui.allocate_exact_size(
            vec2(ui.available_width().max(1.0), Metrics::SIDEBAR_ROW_H),
            Sense::click(),
        );
        if r.width() < 2.0 || !r.is_finite() {
            continue; // 退化行不绘制，避免无效 Mesh
        }
        let p = ui.painter_at(r);
        p.text(
            pos2(r.left() + 28.0, r.center().y),
            Align2::LEFT_CENTER,
            &name,
            FontId::new(12.5, egui::FontFamily::Proportional),
            Theme::picasa().tree_text,
        );
        p.text(
            pos2(r.right() - 14.0, r.center().y),
            Align2::RIGHT_CENTER,
            count.to_string(),
            FontId::new(11.0, egui::FontFamily::Proportional),
            Theme::picasa().text_secondary,
        );
        if row_resp.clicked() {
            actions.push(if is_tag {
                SidebarAction::FilterByTag(name.clone())
            } else {
                SidebarAction::FilterByAlbum(name.clone())
            });
        }
    }
}

fn favorites(ui: &mut Ui, lib: &Library, ctx: &SidebarCtx<'_>, actions: &mut Vec<SidebarAction>) {
    let t = *ctx.theme;
    // 星标与「所有照片」共用一份 `starred_only`：开着时只高亮星标，
    // 否则两行同时亮，点「所有照片」看起来像没离开星标视图。
    let starred_on = lib.filter.as_ref().is_some_and(|f| f.starred_only);
    // 所有照片
    let row = fav_row(
        ui,
        &t,
        "所有照片",
        Some(lib.total_in_db),
        Icon::PhotoStack,
        matches!(lib.view, View::All) && !starred_on,
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
        Icon::Album,
        matches!(lib.view, View::Recent),
        true,
        ctx.has_focus,
    );
    if row.clicked() {
        actions.push(SidebarAction::ShowView(View::Recent));
    }
    // 星标：M4 起可用——切「只看收藏」筛选（数据只存应用库，不写照片文件）。
    //
    // ⚠️ 必须用 `ctx.db`（调用方 `app.rs` 持锁传入的引用），**绝不能**在这里
    // 调 `lib.db()` 再拿一次锁：std Mutex 不可重入，重入 = 主线程自我死锁
    // （2026-11-02 实测：启动首帧 hang 127 s，主线程与 op-index 互锁——
    // 与 `app.rs` new() 里 `enqueue_retry` 那条教训是同一类错误）。
    let count = ctx.db.starred_ids().ok().map(|v| v.len() as u64);
    let row = fav_row(
        ui,
        &t,
        "星标",
        count,
        Icon::Heart,
        starred_on,
        true,
        ctx.has_focus,
    );
    if row.clicked() {
        actions.push(SidebarAction::ToggleStarredFilter);
    }
    row.on_hover_text(if starred_on {
        "正在只看收藏的照片，再点一次显示全部"
    } else {
        "只看收藏（星标）的照片"
    });

    // ---- 时间轴（M4 #7：按拍摄年份分组浏览）----
    if !ctx.favorites_collapsed {
        ui.add_space(4.0);
        // ⚠️ 同 `starred_ids` 的教训：用 `ctx.db`（调用方持锁传入），不能调
        // `lib.timeline_years()`——它内部会 `self.db()` 重入拿锁 = 死锁。
        if let Ok(years) = ctx.db.timeline_years() {
            for (year, n) in years.iter().take(12) {
                let active = lib
                    .filter
                    .as_ref()
                    .is_some_and(|f| f.taken_range == Some(year_range(*year)));
                let label = format!("{year}");
                let row = fav_row(
                    ui,
                    &t,
                    &label,
                    Some(*n as u64),
                    Icon::Album,
                    active,
                    true,
                    ctx.has_focus,
                );
                if row.clicked() {
                    if active {
                        actions.push(SidebarAction::TimelineYear(0)); // 0 = 清除
                    } else {
                        actions.push(SidebarAction::TimelineYear(*year));
                    }
                }
            }
        }
    }
}

/// 某一年的拍摄日期筛选范围（epoch 秒）。算式在 core（`year_range_seconds`）。
fn year_range(year: i32) -> (i64, i64) {
    picasa_core::year_range_seconds(year)
}

/// 一行主入口：36px 高、线性图标 + 文字 + 右侧计数。
///
/// 选中 = 浅灰胶囊底（截图样式，**不再**用钢青蓝白字）。
#[allow(clippy::too_many_arguments)]
fn fav_row(
    ui: &mut Ui,
    theme: &Theme,
    label: &str,
    count: Option<u64>,
    icon: Icon,
    active: bool,
    enabled: bool,
    _has_focus: bool,
) -> egui::Response {
    let row_h = Metrics::SIDEBAR_ROW_H;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
    let painter = ui.painter_at(rect);
    // 内容区留 6px 左右边距，让选中胶囊不顶满整行（截图样式）
    let pill = rect.shrink2(vec2(6.0, 1.0));
    let icon_color = theme.toolbar_icon;
    if active {
        painter.rect_filled(pill, 6.0, theme.sidebar_hover);
    } else if resp.hovered() && enabled {
        painter.rect_filled(pill, 6.0, theme.sidebar_hover.gamma_multiply(0.6));
    }
    let text_color = if enabled {
        theme.text_primary
    } else {
        theme.text_disabled
    };
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), vec2(16.0, 16.0));
    icons::draw(&painter, icon, icon_rect, icon_color, 1.3);
    painter.text(
        pos2(rect.left() + 40.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::new(12.5, egui::FontFamily::Proportional),
        text_color,
    );
    if let Some(n) = count {
        // 计数右对齐、灰色小字（截图的「9」）
        painter.text(
            pos2(rect.right() - 16.0, rect.center().y),
            Align2::RIGHT_CENTER,
            n.to_string(),
            FontId::new(11.5, egui::FontFamily::Proportional),
            theme.sidebar_section_text,
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
                egui::RichText::new("还没有加入任何文件夹\n点顶栏「…」→「添加文件夹…」")
                    .size(11.5)
                    .color(t.text_secondary),
            );
        });
        return;
    }

    let row_h = Metrics::SIDEBAR_ROW_H;
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

                // 内容胶囊（与 fav_row 同款留边）
                let pill = rect.shrink2(vec2(6.0, 1.0));
                let indent = 8.0 + row.depth as f32 * Metrics::SIDEBAR_INDENT;
                if active {
                    painter.rect_filled(pill, 6.0, t.sidebar_hover);
                } else if resp.hovered() {
                    painter.rect_filled(pill, 6.0, t.sidebar_hover.gamma_multiply(0.6));
                }

                // A 样式树线：每个祖先层级一条细竖线，当前行用一段短横线接到折叠箭头/图标槽。
                let guide = t.separator.gamma_multiply(0.55);
                for d in 0..row.depth {
                    let gx = rect.left() + 8.0 + d as f32 * Metrics::SIDEBAR_INDENT + 7.0;
                    painter.line_segment(
                        [pos2(gx, rect.top()), pos2(gx, rect.bottom())],
                        Stroke::new(1.0, guide),
                    );
                }
                if row.depth > 0 {
                    let gx =
                        rect.left() + 8.0 + (row.depth - 1) as f32 * Metrics::SIDEBAR_INDENT + 7.0;
                    painter.line_segment(
                        [
                            pos2(gx, rect.center().y),
                            pos2(rect.left() + indent + 7.0, rect.center().y),
                        ],
                        Stroke::new(1.0, guide),
                    );
                }

                // 图标槽：所有行的图标都从 `indent + 24` 起（有折叠箭头的行，
                // 箭头占用前面的槽）——保证父子行的图标**垂直对齐**。
                let icon_x = rect.left() + indent + 24.0;
                // 折叠箭头（有子目录才出现）
                if row.has_children {
                    let chev = Rect::from_center_size(
                        pos2(rect.left() + indent + 7.0, rect.center().y),
                        vec2(9.0, 9.0),
                    );
                    icons::draw(
                        &painter,
                        if row.collapsed {
                            Icon::ChevronRight
                        } else {
                            Icon::ChevronDown
                        },
                        chev,
                        t.text_secondary,
                        1.2,
                    );
                    // 箭头是可点的：窄区域，避免误触「选中」
                    let chev_rect = Rect::from_center_size(
                        pos2(rect.left() + indent + 7.0, rect.center().y),
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

                // 线性文件夹图标（A 样式：描边文件夹 + 树线）
                let icon_rect =
                    Rect::from_center_size(pos2(icon_x, rect.center().y), vec2(16.0, 14.0));
                draw_folder_icon_line(&painter, icon_rect, t.toolbar_icon);

                // 文字与图标间距：图标右沿 + 7
                let text_x = icon_x + 16.0 + 7.0;
                painter.text(
                    pos2(text_x, rect.center().y),
                    Align2::LEFT_CENTER,
                    &row.name,
                    FontId::new(12.5, egui::FontFamily::Proportional),
                    t.text_primary,
                );
                // 计数右对齐、灰色；为 0 不显示
                //
                // M2：关注文件夹多一个**状态点**（`04` §3.2 第三行）。有任务时把计数
                // 左移 8px 给点让位——数字本身不变，仍然是「已发现」口径，
                // 这样左栏与面板同一行数字一致。
                let task = ctx.snapshot.task_by_path(&row.path);
                let count_right = if task.is_some() {
                    rect.right() - 20.0
                } else {
                    rect.right() - 14.0
                };
                if row.count > 0 {
                    painter.text(
                        pos2(count_right, rect.center().y),
                        Align2::RIGHT_CENTER,
                        row.count.to_string(),
                        FontId::new(11.5, egui::FontFamily::Proportional),
                        t.sidebar_section_text,
                    );
                }
                if let Some(task) = task {
                    index_progress::draw_state_dot(
                        &painter,
                        pos2(rect.right() - 10.0, rect.center().y),
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

/// 线性文件夹图标（macOS 风）：圆角矩形 + 标签耳，单色描边。
fn draw_folder_icon_line(painter: &egui::Painter, rect: Rect, color: Color32) {
    let s = Stroke::new(1.2, color);
    let body = Rect::from_min_max(
        pos2(rect.left(), rect.top() + rect.height() * 0.22),
        pos2(rect.right(), rect.bottom()),
    );
    painter.rect_stroke(body, 2.5, s, StrokeKind::Middle);
    // 标签耳
    let ear = Shape::line(
        vec![
            pos2(rect.left(), rect.top() + rect.height() * 0.22),
            pos2(rect.left(), rect.top() + rect.height() * 0.06),
            pos2(
                rect.left() + rect.width() * 0.34,
                rect.top() + rect.height() * 0.06,
            ),
            pos2(
                rect.left() + rect.width() * 0.44,
                rect.top() + rect.height() * 0.22,
            ),
        ],
        egui::epaint::PathStroke::new(1.2, color),
    );
    painter.add(ear);
}

fn tray(ui: &mut Ui, lib: &Library, ctx: &mut SidebarCtx<'_>, actions: &mut Vec<SidebarAction>) {
    let t = *ctx.theme;
    let width = ui.available_width();
    let header_h = Metrics::TRAY_COLLAPSED_H;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, header_h), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, t.sidebar_bg);

    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 16.0, rect.center().y), vec2(13.0, 13.0));
    icons::draw(&painter, Icon::PhotoStack, icon_rect, t.text_secondary, 1.3);

    let count = lib.tray.len();
    painter.text(
        pos2(rect.left() + 34.0, rect.center().y),
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
            "选中照片后按 ⌘H 放入托盘",
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
        painter.rect_filled(cell, 4.0, Color32::WHITE);
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
            4.0,
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
