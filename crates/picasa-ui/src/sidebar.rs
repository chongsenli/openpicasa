//! 左栏：中区左侧的「标题 + A 样式文件树」（M3.1 §7 定稿）。
//!
//! ## 保留的功能（与 M4 版一致，条目一个不少）
//!
//! * 「图库」组：所有照片 / 最近更改 / 星标（→ 只看收藏筛选）
//! * 时间轴：按拍摄年份筛选（数据来自 `timeline_years`）
//! * 「标签」「相册」组（应用内虚拟集合，M4）
//! * 「文件夹」组：真实目录树
//! * 「导入的索引」组（仅有来源时，紧挨文件夹后，IDX-21）
//! * 底部照片托盘（收起 / 展开行为不变）
//!
//! ## M5-IA-01 / M5-SB-01 的固定顺序
//!
//! **图库 → 标签 → 相册 → 文件夹 → 导入的索引（仅有来源时）→ 底部托盘。**
//! 设置组与「索引管理」侧栏子项不存在；它们不是被隐藏，而是本文件里没有绘制点。
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
use picasa_core::{FolderRowView, IndexSnapshot, Library, ThumbService, View};
use picasa_db::{Db, PhotoRow};

use crate::icons::{self, Icon};
use crate::index_progress;
use crate::textures::{TexState, TextureCache};
use crate::theme::{Metrics, Theme};

/// 「导入的索引」分组的视觉标记（IDX-21 ③④）。
///
/// 全部来自 App 已缓存的导入来源信息，绘制时不查库。
#[derive(Clone, Default)]
pub struct MarkSet<'a> {
    /// 这组行是不是「导入的索引」里的（决定图标与悬停提示）。
    pub imported: bool,
    /// 该来源整体是否离线（原路径不可读）。
    pub offline: bool,
    /// 悬停提示里的原路径（离线时尤其重要：**不只靠颜色**表达状态）。
    pub abs_paths: &'a [String],
    /// 与「本机已有」同路径的节点：以本机为准，只标一行说明。
    pub local_dupes: &'a [String],
}

impl MarkSet<'_> {
    /// 这组行是否属于「导入的索引」。
    pub fn is_imported(&self) -> bool {
        self.imported
    }

    /// 悬停提示：离线/在线都要说清楚，且离线时给出原路径（IDX-21 ③）。
    pub fn tip(&self, row: &FolderRowView) -> Option<String> {
        let mut lines = Vec::new();
        lines.push(if self.offline {
            "离线（导出时的原路径当前不可读）".to_string()
        } else if self.imported {
            "来自导入的索引".to_string()
        } else {
            return None;
        });
        if !self.abs_paths.is_empty() {
            lines.push(format!("原路径：{}", self.abs_paths.join("、")));
        }
        lines.push(format!("导出时计数：{}", row.count));
        if self.local_dupes.iter().any(|p| p == &row.path) {
            lines.push("本机已有同名路径，以本机为准（计数只算一份）".to_string());
        }
        Some(lines.join("\n"))
    }
}

/// 「导入的索引」分组里的一个来源（IDX-21 ②）。
///
/// 由 App 缓存后传入：名字/时间/照片数来自 `import_sources` 表，
/// `offline`/`abs_paths` 来自该来源根的实际可读性探测，`local_dupes`
/// 用于标注「本机已有」（P-M43-10）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportGroup {
    pub source_id: i64,
    pub name: String,
    /// 导入时间（已格式化，绝不出现 ETA）。
    pub when: String,
    pub photos: u64,
    pub offline: bool,
    pub abs_paths: Vec<String>,
    pub local_dupes: Vec<String>,
}

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
    /// M4.2 `M42-UI-05`：相册行右键（相册名、屏幕位置），供改名/删除/加入照片。
    AlbumContext(String, Pos2),
    /// IDX-21 ⑤：导入来源节点右键（来源 id、屏幕位置）→ 移除 / 仅隐藏。
    ImportSourceContext(i64, Pos2),
    /// IDX-22 ④：切换标签排序方式（按名称 / 按数量），选择持久化。
    SetTagsSort(picasa_core::TagsSort),
    /// IDX-22 ⑥：切换「显示空标签」（默认隐藏 0 计数）。
    ToggleShowZeroTags,
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
    /// 相册名 → 封面照片行（App 缓存）。
    ///
    /// 暗色相册组按原型画成三列缩略图卡（`js/app.js:890-916` 的 `<div class="alb-covers">`），
    /// 亮色仍是文字行。封面取相册里 id 最小的那张（原型存的是建相册时选中的第一张，
    /// 应用侧 `albums` 表没有 cover 列，只能按成员表取，见 `refresh_sidebar_groups`）。
    pub album_covers: &'a [(String, Option<PhotoRow>)],
    /// IDX-22 ②：当前生效的标签筛选（行高亮选中态）；无则 None。
    pub tags_active: Option<&'a str>,
    /// 当前生效的相册筛选（暗色缩略图卡据此画选中环，原型 `.cover.on`）；无则 None。
    pub album_active: Option<&'a str>,
    /// IDX-22 ④：标签排序方式（组头菜单据此打勾）。
    pub tags_sort: picasa_core::TagsSort,
    /// IDX-22 ⑥：是否显示 0 计数标签（菜单据此打勾）。
    pub show_zero_tags: bool,
    /// IDX-22 P1「本视图 N」：当前视图范围内的标签计数（App 缓存）。
    ///
    /// 只有**非全库视图**才画第二个数字；全库视图下它等于全库计数，
    /// 画两个一样的数字只会增加噪音。
    pub tags_in_view: &'a [(String, i64)],
    /// 当前视图是不是「所有照片」（P1 第二数字只在非全库时出现）。
    pub view_is_all: bool,
    /// IDX-21：导入来源列表（App 缓存，`import_sources` + 实际可读性）。
    pub import_groups: &'a [ImportGroup],
    /// IDX-21：「导入的索引」分组折叠状态。
    pub imported_collapsed: bool,
    pub has_focus: bool,
    /// 索引进度快照：左栏的**状态点**与计数必须与面板/弹框同源（`04` §3.2）
    pub snapshot: &'a IndexSnapshot,
}

/// M5-IA-01 / M5-SB-01 的固定分组顺序（「导入的索引」只在有来源时追加在末尾）。
///
/// 这是左栏结构的**单一事实源**：`show()` 按它逐组绘制，单测按它断言顺序。
/// 「设置」组与「索引管理」子项不在此列——M5 已整组删除；设置页与索引管理
/// 窗口本身仍在（入口改到更多菜单 / 设置壳，见 `app.rs`）。
pub const SIDEBAR_GROUPS: [&str; 4] = ["图库", "标签", "相册", "文件夹"];

/// 左栏「还没有加入任何文件夹」的指引（`M5-SH-04`）。
///
/// 指向**顶栏「导入」**——更多菜单里的「添加文件夹…」已在 M5 删除，
/// 旧文案会指向一个不存在的菜单项。
pub const EMPTY_SIDEBAR_HINT: &str = "还没有加入任何文件夹\n点顶栏「导入」添加照片文件夹";

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
        // 用户 2026-11-25：取消托盘外框的灰色描边（侧栏背景与窗底同色，不需要框）。
        .frame(egui::Frame::new().fill(theme.sidebar_bg))
        .show(ui, |ui| {
            tray(ui, lib, &mut *ctx, &mut actions);
        });

    egui::CentralPanel::no_frame()
        .frame(egui::Frame::new().fill(theme.sidebar_bg))
        .show(ui, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, theme.sidebar_bg);

            // 原型侧栏：`div.side-scroll` 从 body 顶（84）再下移 6 起画；行与行、组与组
            // 之间**没有** egui 的默认 item 间距（`a.row` 高 30 紧贴，组间只隔 8）。
            // 不归零的话每一行会多吃 5px，六行之后整栏就“散架”了。
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(6.0);

            // M5-IA-01 / M5-SB-01：固定顺序
            //   图库 → 标签 → 相册 → 文件夹 → 导入的索引（仅有来源时，紧挨文件夹后）→ 托盘
            // 托盘已先占底部；「设置」组及其「索引管理」子项**整组不存在**（不是隐藏）。
            for group in SIDEBAR_GROUPS {
                if group != SIDEBAR_GROUPS[0] {
                    // 原型：上一个组最后一行底 = 下一个组头行框顶；组头自身高 27.2，
                    // 组与组之间的可见空隙是 35.2 − 27.2 = 8。
                    ui.add_space(8.0);
                }
                match group {
                    "图库" => {
                        section_title(ui, ctx.theme, "图库", ctx.favorites_collapsed, &mut actions);
                        if !ctx.favorites_collapsed {
                            favorites(ui, lib, &*ctx, &mut actions);
                        }
                    }
                    "标签" => m4_group(
                        ui,
                        ctx.theme,
                        "标签",
                        ctx.tags_collapsed,
                        ctx.tags.to_vec(),
                        &mut actions,
                        true,
                        ctx.tags_active,
                        ctx.tags_sort,
                        ctx.show_zero_tags,
                        if ctx.view_is_all {
                            None
                        } else {
                            Some(ctx.tags_in_view)
                        },
                    ),
                    "相册" => album_group(ui, &mut *ctx, &mut actions),
                    "文件夹" => {
                        section_title(ui, ctx.theme, "文件夹", ctx.folders_collapsed, &mut actions);
                        if !ctx.folders_collapsed {
                            folder_tree(ui, lib, &*ctx, &mut actions);
                        }
                    }
                    _ => {}
                }
            }

            // ---- IDX-21 / M5-IA-01：导入的索引（仅当有来源时，紧挨「文件夹」之后）----
            if !ctx.import_groups.is_empty() {
                ui.add_space(8.0);
                section_title(
                    ui,
                    ctx.theme,
                    "导入的索引",
                    ctx.imported_collapsed,
                    &mut actions,
                );
                if !ctx.imported_collapsed {
                    imported_index(ui, lib, &*ctx, &mut actions);
                }
            }
        });

    actions
}

/// 分组标题：灰色小字（截图的「相簿」样式），整行可点 = 折叠。
fn section_title(
    ui: &mut Ui,
    theme: &Theme,
    title: &str,
    collapsed: bool,
    actions: &mut Vec<SidebarAction>,
) {
    let (rect, resp) = ui.allocate_exact_size(
        vec2(ui.available_width(), Metrics::sidebar_section_h(theme.dark)),
        Sense::click(),
    );
    let painter = ui.painter_at(rect);
    let color = theme.sidebar_section_text;
    painter.text(
        pos2(rect.left() + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        // M5 §5.2 + 原型 `div.sec`：分组标题 600 字重，暗色再弱化颜色。
        crate::theme::semibold(sidebar_row_font_size(theme)),
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
///
/// 分组头（标题 + 右端「＋」；标签组多一枚「⋯」菜单）。
///
/// 标签组与相册组共用这一份——相册组在暗色下换的是**行**的画法（缩略图卡），
/// 组头不该跟着分叉（`M5` §3 要求两套主题的信息架构一致）。
/// 侧栏文字行的字号：原型 `.row .name` 亮色 13px / 暗色 12.5px。
pub fn sidebar_row_font_size(theme: &Theme) -> f32 {
    if theme.dark { 12.5 } else { 13.0 }
}

/// 侧栏计数（行尾灰色小数字）：原型 `.row .cnt` 亮色 11.5px / 暗色 10.5px。
/// 同一行里计数比行名小一号，各自按「中心」对齐会让两个字号的中文**底边**
/// 错开（小字浮起）。这里把计数下移半个字号差，让两者落在同一基线上。
pub fn count_center_nudge(theme: &Theme) -> f32 {
    (sidebar_row_font_size(theme) - sidebar_count_font_size(theme)) * 0.6
}

pub fn sidebar_count_font_size(theme: &Theme) -> f32 {
    if theme.dark { 10.5 } else { 11.5 }
}

fn group_header(
    ui: &mut Ui,
    theme: &Theme,
    title: &str,
    is_tag: bool,
    tags_sort: picasa_core::TagsSort,
    show_zero_tags: bool,
    actions: &mut Vec<SidebarAction>,
) {
    let (rect, resp) = ui.allocate_exact_size(
        vec2(
            ui.available_width().max(1.0),
            Metrics::sidebar_section_h(theme.dark),
        ),
        Sense::click(),
    );
    // 防御：窗口极窄 / 侧栏收起过渡帧时 rect 可能退化（宽度 0 或负）。
    // 退化几何会让 egui 后续合并 Mesh 时产生无效索引（debug 下 `is_valid()`
    // 断言 panic：Other mesh is invalid），所以这里直接跳过绘制。
    if rect.width() < 2.0 || rect.height() < 2.0 || !rect.is_finite() {
        return;
    }
    let painter = ui.painter_at(rect);
    let color = theme.sidebar_section_text;
    painter.text(
        pos2(rect.left() + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        // 原型 `div.sec{font-weight:600}`。
        crate::theme::semibold(sidebar_row_font_size(theme)),
        color,
    );
    // 右端「＋」新建
    let plus = Rect::from_center_size(pos2(rect.right() - 14.0, rect.center().y), vec2(11.0, 11.0));
    icons::draw(&painter, Icon::Plus, plus, color, 1.2);
    // IDX-22 ④⑥：标签组头在「＋」左侧再来一枚「⋯」，点开是排序 / 显示空标签。
    let menu_btn =
        Rect::from_center_size(pos2(rect.right() - 32.0, rect.center().y), vec2(11.0, 11.0));
    if is_tag {
        icons::draw(&painter, Icon::Ellipsis, menu_btn, color, 1.2);
    }
    // IDX-22 ④⑥：「⋯」是独立的热区（`ui.interact`），否则点它会连带触发展开
    // 折叠——`resp` 覆盖整行，两个 Sense::click 会同时命中。
    if is_tag {
        let menu_resp = ui.interact(
            menu_btn,
            ui.scope_id().with("tag-group-menu"),
            Sense::click(),
        );
        menu_resp.clone().on_hover_text("标签排序 / 显示空标签");
        egui::Popup::menu(&menu_resp).show(|ui| {
            ui.set_min_width(120.0);
            ui.label("标签排序");
            for s in [picasa_core::TagsSort::Name, picasa_core::TagsSort::Count] {
                if ui.selectable_label(s == tags_sort, s.label()).clicked() {
                    actions.push(SidebarAction::SetTagsSort(s));
                }
            }
            ui.separator();
            if ui.selectable_label(show_zero_tags, "显示空标签").clicked() {
                actions.push(SidebarAction::ToggleShowZeroTags);
            }
        });
    }
    if resp.clicked() {
        let pos = resp.interact_pointer_pos().unwrap_or_default();
        if plus.contains(pos) {
            actions.push(if is_tag {
                SidebarAction::NewTag
            } else {
                SidebarAction::NewAlbum
            });
        } else if !(is_tag && menu_btn.contains(pos)) {
            actions.push(SidebarAction::ToggleSectionM4(title.to_string()));
        }
    }
}

/// M4.3 `IDX-22` 增量（仅标签组）：
/// - ② 行右侧计数右对齐；名称超长按字符截断 + 悬停给全名；
///   当前生效的标签筛选行画选中底色（`active`）。
/// - ④⑥ 组头「⋯」菜单切排序方式 / 显示空标签（相册组没有这个菜单）。
///
/// 组头本身在 [`group_header`]：相册组暗色换的是**行**的画法，组头两套主题一致。
///
/// 参数确实多（组头 + 列表 + 三个标签专属项），拆成结构体只会让两个调用点更啰嗦；
/// 收在一个私有函数里，代价只有一个 allow。
#[allow(clippy::too_many_arguments)]
fn m4_group(
    ui: &mut Ui,
    theme: &Theme,
    title: &str,
    collapsed: bool,
    items: Vec<(String, i64)>,
    actions: &mut Vec<SidebarAction>,
    is_tag: bool,
    active: Option<&str>,
    tags_sort: picasa_core::TagsSort,
    show_zero_tags: bool,
    in_view: Option<&[(String, i64)]>,
) {
    group_header(ui, theme, title, is_tag, tags_sort, show_zero_tags, actions);
    if collapsed {
        return;
    }
    rows(ui, theme, items, actions, is_tag, active, in_view);
}

/// 一组文字行（标签组 / 亮色相册组共用）。
#[allow(clippy::too_many_arguments)]
fn rows(
    ui: &mut Ui,
    theme: &Theme,
    items: Vec<(String, i64)>,
    actions: &mut Vec<SidebarAction>,
    is_tag: bool,
    active: Option<&str>,
    in_view: Option<&[(String, i64)]>,
) {
    for (name, count) in items {
        let (r, row_resp) = ui.allocate_exact_size(
            vec2(
                ui.available_width().max(1.0),
                Metrics::sidebar_row_h(theme.dark),
            ),
            Sense::click(),
        );
        if r.width() < 2.0 || !r.is_finite() {
            continue; // 退化行不绘制，避免无效 Mesh
        }
        let selected = is_tag && active == Some(name.as_str());
        let hovered = row_resp.hovered();
        if selected {
            // M5：选中行 = 主题的选中片（亮白卡 / 暗 #232323），行圆角跟主题。
            ui.painter_at(r).rect_filled(
                r.shrink2(vec2(4.0, 1.0)),
                theme.radius_btn,
                theme.sidebar_selected,
            );
        } else if hovered {
            ui.painter_at(r).rect_filled(
                r.shrink2(vec2(4.0, 1.0)),
                theme.radius_btn,
                theme.sidebar_hover,
            );
        }
        let p = ui.painter_at(r);
        // 名称最长占「整行宽 - 左侧缩进 - 右侧计数位（约 34px）」，按字符截断，
        // 截断只影响画出来的那段，悬停仍给全名（IDX-22 ②）。
        let count_w = 34.0;
        let avail = (r.width() - 28.0 - count_w).max(24.0);
        let per_char = 7.4_f32; // 12.5px 比例字体下汉字的近似步进（宁多截不留白）
        let max_chars = ((avail / per_char).floor() as usize).max(1);
        let shown = picasa_core::truncate_label(&name, max_chars);
        p.text(
            pos2(r.left() + 33.0, r.center().y),
            Align2::LEFT_CENTER,
            &shown,
            // 原型 `span.name`：13px（暗 12.5）/ font-weight 500。
            crate::theme::medium(sidebar_row_font_size(theme)),
            if selected {
                theme.row_selected_text
            } else {
                theme.tree_text
            },
        );
        // P1「本视图 N」：全库计数在右，本视图计数紧挨着它左边、用更淡的颜色。
        // 只在非全库视图下画；两个数字相同就没有信息量。
        let in_view_count = in_view
            .and_then(|rows| rows.iter().find(|(l, _)| l == &name).map(|(_, n)| *n))
            .filter(|n| *n != count);
        let count_right = if in_view_count.is_some() {
            r.right() - 44.0
        } else {
            r.right() - 10.0
        };
        p.text(
            pos2(count_right, r.center().y),
            Align2::RIGHT_CENTER,
            count.to_string(),
            // 原型 `span.cnt`：11.5px（暗 10.5）/ font-weight 500。
            crate::theme::medium(sidebar_count_font_size(theme)),
            theme.text_secondary,
        );
        if let Some(n) = in_view_count {
            p.text(
                pos2(r.right() - 10.0, r.center().y + count_center_nudge(theme)),
                Align2::RIGHT_CENTER,
                format!("·{n}"),
                FontId::new(10.5, egui::FontFamily::Proportional),
                theme.text_disabled,
            );
        }
        let row_resp = if shown != name {
            row_resp.on_hover_text(&name)
        } else {
            row_resp
        };
        if row_resp.clicked() {
            actions.push(if is_tag {
                SidebarAction::FilterByTag(name.clone())
            } else {
                SidebarAction::FilterByAlbum(name.clone())
            });
        }
        // M4.2 `M42-UI-05`：相册行右键 → 改名 / 删除 / 加入当前选中集。
        // 与 folder_tree 同范式：`Sense::click()` 仍会报告 secondary_clicked。
        if !is_tag && row_resp.secondary_clicked() {
            actions.push(SidebarAction::AlbumContext(
                name.clone(),
                row_resp.interact_pointer_pos().unwrap_or(r.center()),
            ));
        }
    }
}

/// 相册组：亮色文字行，暗色三列缩略图卡。
///
/// 原型 `js/app.js:890-916` 两套主题画法不同（亮色 `<a class="row">`、
/// 暗色 `<div class="alb-covers">` 三列封面），但**行为一致**：点击筛到该相册、
/// 右键弹出改名/删除/加入照片、当前相册有选中态（`M5` §3 允许主题换画法，
/// 不允许出现「只有某套主题能用」的分叉）。
///
/// 封面取相册里 id 最小的那张成员（原型存的是建相册时选中的第一张；
/// 应用侧 `albums` 表没有 cover 列，只能按成员表取，见 `refresh_sidebar_groups`）。
fn album_group(ui: &mut Ui, ctx: &mut SidebarCtx<'_>, actions: &mut Vec<SidebarAction>) {
    let theme = *ctx.theme;
    group_header(
        ui,
        &theme,
        "相册",
        false,
        picasa_core::TagsSort::Name,
        false,
        actions,
    );
    if ctx.albums_collapsed {
        return;
    }
    if !theme.dark {
        rows(ui, &theme, ctx.albums.to_vec(), actions, false, None, None);
        return;
    }
    album_covers(ui, ctx, actions);
}

/// 暗色相册封面的列数与间距（原型 `.alb-covers`：`grid-template-columns:1fr 1fr 1fr`、
/// `gap:6px`、`padding:2px 6px 8px`）。
const ALBUM_COLS: usize = 3;
const ALBUM_GAP: f32 = 6.0;
const ALBUM_PAD_X: f32 = 6.0;
const ALBUM_LABEL_H: f32 = 17.0;
/// 封面与名字之间的间距（原型 `.cover{gap:4px}`）。
const ALBUM_COVER_LABEL_GAP: f32 = 4.0;

/// 一格封面的边长：三列等宽、扣掉左右内边距与列间距。
pub fn album_cover_side(total_w: f32) -> f32 {
    if !total_w.is_finite() || total_w <= 0.0 {
        return 0.0;
    }
    let inner_w = (total_w - ALBUM_PAD_X * 2.0).max(0.0);
    let gaps = ALBUM_GAP * (ALBUM_COLS as f32 - 1.0);
    ((inner_w - gaps) / ALBUM_COLS as f32).max(0.0)
}

/// 封面下那行名字能放几个字符（10px 居中，按每字 6px 估）。
pub fn album_label_chars(side: f32) -> usize {
    if !side.is_finite() || side <= 0.0 {
        return 1;
    }
    ((side / 6.0).floor() as usize).max(1)
}

/// 暗色相册组的三列封面（原型 `.cover img{aspect-ratio:1;object-fit:cover;border-radius:8px}`、
/// `.cover span{font-size:10px}`）。
fn album_covers(ui: &mut Ui, ctx: &mut SidebarCtx<'_>, actions: &mut Vec<SidebarAction>) {
    let cols = ALBUM_COLS;
    let gap = ALBUM_GAP;
    let pad_x = ALBUM_PAD_X;

    let theme = *ctx.theme;
    let total_w = ui.available_width();
    if total_w < 2.0 || ctx.albums.is_empty() {
        return;
    }
    let side = album_cover_side(total_w).max(1.0);
    let cell_h = side + ALBUM_COVER_LABEL_GAP + ALBUM_LABEL_H;

    let items = ctx.albums.to_vec();
    let covers = ctx.album_covers;
    let mut budget = cols;

    for chunk in items.chunks(cols) {
        let (row_rect, _) = ui.allocate_exact_size(vec2(total_w, cell_h + gap), Sense::hover());
        if row_rect.width() < 2.0 || !row_rect.is_finite() {
            continue;
        }
        for (i, (name, count)) in chunk.iter().enumerate() {
            let left = row_rect.left() + pad_x + i as f32 * (side + gap);
            let cell = Rect::from_min_size(pos2(left, row_rect.top() + 2.0), vec2(side, side));
            if cell.width() < 2.0 || !cell.is_finite() {
                continue;
            }
            let resp = ui.interact(
                Rect::from_min_size(pos2(left, row_rect.top() + 2.0), vec2(side, cell_h)),
                ui.scope_id().with(("alb-cover", name.as_str())),
                Sense::click(),
            );
            let p = ui.painter_at(row_rect);
            let selected = ctx.album_active == Some(name.as_str());
            let hovered = resp.hovered();
            let ring = if selected {
                Some(Stroke::new(2.0, theme.selected))
            } else if hovered {
                Some(Stroke::new(1.0, theme.hairline))
            } else {
                None
            };
            let cover = covers
                .iter()
                .find(|(n, _)| n == name)
                .and_then(|(_, row)| row.as_ref());
            match cover {
                Some(row) => {
                    let state = ctx
                        .cache
                        .get(ui.ctx(), ctx.db, ctx.service, row, &mut budget);
                    if let TexState::Ready(tex, size) = state {
                        // 封面是正方形 cover：按 `object-fit:cover` 裁 UV，不压扁。
                        let uv = crate::viewer::cover_uv(cell, size);
                        crate::viewer::paint_rounded_image(&p, tex, cell, 8.0, uv);
                    } else {
                        p.rect_filled(cell, 8.0, theme.chip_bg);
                    }
                }
                None => {
                    p.rect_filled(cell, 8.0, theme.chip_bg);
                }
            }
            if let Some(stroke) = ring {
                p.rect_stroke(cell, 8.0, stroke, StrokeKind::Outside);
            }
            // 名字：10px、居中、单行截断（原型 `.cover span`）。
            let label_rect = Rect::from_min_size(
                pos2(left, cell.bottom() + ALBUM_COVER_LABEL_GAP),
                vec2(side, ALBUM_LABEL_H),
            );
            let max_chars = album_label_chars(side);
            let shown = picasa_core::truncate_label(name, max_chars);
            p.text(
                label_rect.center(),
                Align2::CENTER_CENTER,
                &shown,
                FontId::new(10.0, egui::FontFamily::Proportional),
                theme.text_disabled,
            );
            let resp = if shown != *name {
                resp.on_hover_text(format!("{name} · {count} 张"))
            } else {
                resp.on_hover_text(format!("{count} 张"))
            };
            if resp.clicked() {
                actions.push(SidebarAction::FilterByAlbum(name.clone()));
            }
            if resp.secondary_clicked() {
                actions.push(SidebarAction::AlbumContext(
                    name.clone(),
                    resp.interact_pointer_pos().unwrap_or(cell.center()),
                ));
            }
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
        // 原型侧栏这一行是五角星（`icon("star", 15)`），心形只用于收藏按钮。
        Icon::Star,
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
    let row_h = Metrics::sidebar_row_h(theme.dark);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
    let painter = ui.painter_at(rect);
    // 内容区留 6px 左右边距，让选中胶囊不顶满整行（截图样式）
    let pill = rect.shrink2(vec2(6.0, 1.0));
    let icon_color = theme.toolbar_icon;
    if active {
        painter.rect_filled(pill, theme.radius_btn, theme.sidebar_selected);
    } else if resp.hovered() && enabled {
        painter.rect_filled(pill, theme.radius_btn, theme.sidebar_hover);
    }
    let text_color = if !enabled {
        theme.text_disabled
    } else if active {
        theme.row_selected_text
    } else {
        theme.text_primary
    };
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), vec2(16.0, 16.0));
    icons::draw(&painter, icon, icon_rect, icon_color, 1.3);
    painter.text(
        pos2(rect.left() + 40.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        // 原型 `span.name`：13px（暗 12.5）/ font-weight 500。
        crate::theme::medium(sidebar_row_font_size(theme)),
        text_color,
    );
    if let Some(n) = count {
        // 计数右对齐、灰色小字（截图的「9」），原型 `span.cnt` 500。
        painter.text(
            pos2(
                rect.right() - 16.0,
                rect.center().y + count_center_nudge(theme),
            ),
            Align2::RIGHT_CENTER,
            n.to_string(),
            crate::theme::medium(sidebar_count_font_size(theme)),
            theme.sidebar_section_text,
        );
    }
    if resp.hovered() && enabled {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    resp
}

fn folder_tree(ui: &mut Ui, lib: &Library, ctx: &SidebarCtx<'_>, actions: &mut Vec<SidebarAction>) {
    // IDX-21 ⑦：本机「文件夹」组只显示 `origin = local` 的根，
    // 导入带进来的根归到「导入的索引」分组，同一棵树不会显示两次。
    let rows = &lib.local_visible_folders;
    let t = *ctx.theme;
    if rows.is_empty() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(
                egui::RichText::new(EMPTY_SIDEBAR_HINT)
                    .size(11.5)
                    .color(t.text_secondary),
            );
        });
        return;
    }
    let active = match lib.view {
        View::Folder(id) => Some(id),
        _ => None,
    };
    let t_dark = ctx.theme.dark;
    let row_h = Metrics::sidebar_row_h(t_dark);
    // M5.1：文件夹是最后一组，把「画完它之后还剩多少」全给它（`TREE_MIN_ROWS` 保底），
    // 只是要给「导入的索引」留出它必须占的高度。折叠掉上面三组 → 整栏都归文件夹树。
    let reserved_below = imported_reserved_below(ui.available_height(), lib, ctx, row_h);
    let budget = folder_tree_budget(ui.available_height(), reserved_below, row_h);
    render_tree_rows(
        ui,
        rows,
        active,
        &MarkSet::default(),
        ctx,
        actions,
        TreeSlot {
            salt: "folder-tree",
            max_h: budget,
        },
    );
}

/// 「导入的索引」分组：每个导入来源一个节点，节点下是其还原的文件夹树。
///
/// 与「文件夹」组是两条独立入口：本机根不在此出现（IDX-21 ⑦），
/// 导入根也不在「文件夹」里出现，同一棵树不会显示两次。
fn imported_index(
    ui: &mut Ui,
    lib: &Library,
    ctx: &SidebarCtx<'_>,
    actions: &mut Vec<SidebarAction>,
) {
    let t = *ctx.theme;
    let row_h = Metrics::sidebar_row_h(t.dark);
    let active = match lib.view {
        View::Folder(id) => Some(id),
        _ => None,
    };
    for g in ctx.import_groups {
        let rows: &[FolderRowView] = lib
            .imported_visible_folders
            .get(&g.source_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        // 来源节点（一行）：名称 + 导入时间 + 照片数；离线时置灰并说明
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width().max(1.0), row_h), Sense::click());
        if rect.width() < 2.0 || !rect.is_finite() {
            continue;
        }
        let painter = ui.painter_at(rect);
        if resp.hovered() {
            painter.rect_filled(rect.shrink2(vec2(6.0, 1.0)), t.radius_btn, t.sidebar_hover);
        }
        let icon_color = if g.offline {
            t.toolbar_icon.gamma_multiply(0.45)
        } else {
            t.toolbar_icon
        };
        let icon_rect = Rect::from_center_size(
            pos2(rect.left() + 8.0 + 24.0, rect.center().y),
            vec2(16.0, 14.0),
        );
        icons::draw(&painter, Icon::PhotoStack, icon_rect, icon_color, 1.2);
        painter.text(
            pos2(icon_rect.right() + 7.0, rect.center().y),
            Align2::LEFT_CENTER,
            &g.name,
            FontId::new(12.5, egui::FontFamily::Proportional),
            if g.offline {
                t.text_secondary.gamma_multiply(0.8)
            } else {
                t.text_primary
            },
        );
        if g.photos > 0 {
            painter.text(
                pos2(rect.right() - 14.0, rect.center().y),
                Align2::RIGHT_CENTER,
                g.photos.to_string(),
                FontId::new(
                    if t.dark { 10.5 } else { 11.5 },
                    egui::FontFamily::Proportional,
                ),
                t.sidebar_section_text,
            );
        }
        let mut tip = format!("导入时间：{}", g.when);
        if g.offline {
            tip.push_str("\n离线：导出时的原路径当前不可读");
        }
        if !g.abs_paths.is_empty() {
            tip.push_str(&format!("\n原路径：{}", g.abs_paths.join("、")));
        }
        tip.push_str("\n右键可从导入的索引中移除（不碰原文件）");
        resp.clone().on_hover_text(tip);
        if resp.clicked() {
            // 展开/收起来源节点与「文件夹」组一致：点组头切换
            actions.push(SidebarAction::ToggleSectionM4(format!(
                "导入的索引#{}",
                g.source_id
            )));
        }
        if resp.secondary_clicked() {
            actions.push(SidebarAction::ImportSourceContext(
                g.source_id,
                resp.interact_pointer_pos().unwrap_or(rect.center()),
            ));
        }

        let marks = MarkSet {
            imported: true,
            offline: g.offline,
            abs_paths: &g.abs_paths,
            local_dupes: &g.local_dupes,
        };
        if !rows.is_empty() {
            // 「导入的索引」在自己的组内滚动：给它的预算同样按当前剩余高度算，
            // 但不抢「文件夹」树——它是最后一组，能吃完剩下的全部高度。
            let budget = folder_tree_budget(ui.available_height(), 0.0, row_h);
            render_tree_rows(
                ui,
                rows,
                active,
                &marks,
                ctx,
                actions,
                TreeSlot {
                    salt: "imported-tree",
                    max_h: budget,
                },
            );
        }
    }
}

/// 树区的最小高度（3 行）：再挤也要让人看出「这里是一棵树」。
///
/// 为什么不再有固定的 `TREE_MAX_H = 260`：那是 M5 固定顺序（图库→文件夹→导入的索引
/// →标签→相册）时代的上界，目的是不让树把后面的组挤出面板。M5.1 的顺序已固定为
/// 图库→标签→相册→文件夹（+ 导入的索引），文件夹是**最后一组**，再写死 260 的后果是
/// 上面三组折叠后腾出的高度没人接管，文件夹列表只画到 260px、下面留一大片空白
/// （用户 2026-11-28 反馈：「折叠后文件夹列表只显示一部分，没有一直显示到整个 sidebar」）。
///
/// 现在的口径是**动态预算**：树高 = 侧栏当前剩余高度 − 之后分组（导入的索引）
/// 必须保留的高度，见 [`folder_tree_budget`]。
const TREE_MIN_ROWS: f32 = 3.0;

/// 分给文件夹树的高度预算。
///
/// `available`：此刻侧栏还剩多少高度（底部托盘已先占位，图库/标签/相册已画完）。
/// `reserved_below`：「文件夹」之后的分组（「导入的索引」）至少要保留的高度。
/// `row_h`：当前主题的行高。
///
/// 语义是「把剩下来的高度全给树」——这正是用户要的「一直显示到整个 sidebar」；
/// 同时**保底** 3 行，窄窗/多分组叠加时也不会退回成一条缝或负数。
fn folder_tree_budget(available: f32, reserved_below: f32, row_h: f32) -> f32 {
    let floor = row_h * TREE_MIN_ROWS;
    if !available.is_finite() {
        return floor;
    }
    let rest = available - if reserved_below.is_finite() {
        reserved_below.max(0.0)
    } else {
        0.0
    };
    rest.max(floor)
}

/// 「导入的索引」分组要预留的高度（`文件夹` 树据此让位）。
///
/// * 没有导入来源 → 0（文件夹树直接吃满剩余高度）。
/// * 该组折叠 → 只有组头的 8px 间隔 + 组高。
/// * 展开 → 组头 + 该组树的一份**公平份额**：最多拿可用高度的 45%，
///   并且不超过它自己的内容高度。不让它按内容全量预留，否则来源一多，
///   文件夹树又会被挤成 3 行——那正是这次要修的病症换了个方向。
fn imported_reserved_below(
    available: f32,
    lib: &Library,
    ctx: &SidebarCtx<'_>,
    row_h: f32,
) -> f32 {
    if ctx.import_groups.is_empty() {
        return 0.0;
    }
    let t = *ctx.theme;
    let head = 8.0 + Metrics::sidebar_section_h(t.dark);
    if ctx.imported_collapsed {
        return head;
    }
    let mut need = 0.0_f32;
    for g in ctx.import_groups {
        need += row_h; // 来源节点那一行
        if let Some(rows) = lib.imported_visible_folders.get(&g.source_id) {
            need += rows.len() as f32 * row_h;
        }
    }
    let share_cap = (available * 0.45).max(row_h * TREE_MIN_ROWS);
    head + need.min(share_cap)
}

/// 一棵树的绘制位：滚动区的盐 + 高度预算。
///
/// 合成一个参数而不是两个散参：`render_tree_rows` 已经 6 个参数，再散加就破
/// `clippy::too_many_arguments` 的 7 个上限了。
struct TreeSlot<'a> {
    salt: &'a str,
    max_h: f32,
}

/// 一组文件夹行（本机树 / 导入来源树共用同一套绘制与交互，IDX-21 ②）。
///
/// `marks` 控制「导入的索引」专用的视觉标记（导入角标 / 离线置灰 / 本机已有），
/// 本机树传默认值，行为与 M2 时期逐像素一致。
fn render_tree_rows(
    ui: &mut Ui,
    rows: &[FolderRowView],
    active: Option<i64>,
    marks: &MarkSet<'_>,
    ctx: &SidebarCtx<'_>,
    actions: &mut Vec<SidebarAction>,
    slot: TreeSlot<'_>,
) {
    let t = *ctx.theme;
    let row_h = Metrics::sidebar_row_h(t.dark);
    let total = rows.len();

    // 目录多时虚拟化（1 万目录也不卡）。
    // 高度 = 调用方按「剩余高度」算好的预算（M5.1：文件夹是最后一组，吃满整栏），
    // 超出预算的目录在框内滚动。
    egui::ScrollArea::vertical()
        .id_salt(slot.salt)
        .max_height(slot.max_h)
        .auto_shrink([false, false])
        .show_rows(ui, row_h, total, |ui, range| {
            for i in range {
                let Some(row) = rows.get(i) else { continue };
                let active = active.is_some() && active == row.folder_id;
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let painter = ui.painter_at(rect);

                // 内容胶囊（与 fav_row 同款留边）
                let pill = rect.shrink2(vec2(6.0, 1.0));
                let indent = 8.0 + row.depth as f32 * Metrics::SIDEBAR_INDENT;
                if active {
                    painter.rect_filled(pill, t.radius_btn, t.sidebar_selected);
                } else if resp.hovered() {
                    painter.rect_filled(pill, t.radius_btn, t.sidebar_hover);
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
                // IDX-21 ③：导入来的树用「导入文件夹」图标（同线宽同尺寸 +
                // 右下角小角标）；离线时整行置灰并画断开角标——**不只靠颜色**，
                // 悬停提示里还会写明原路径与状态。
                let imported = marks.is_imported();
                let icon_color = if imported && marks.offline {
                    t.toolbar_icon.gamma_multiply(0.45)
                } else {
                    t.toolbar_icon
                };
                let icon_rect =
                    Rect::from_center_size(pos2(icon_x, rect.center().y), vec2(16.0, 14.0));
                draw_folder_icon_line(&painter, icon_rect, icon_color);
                if imported {
                    let badge = Rect::from_center_size(
                        pos2(icon_rect.right() - 1.0, icon_rect.bottom() - 1.0),
                        vec2(8.0, 8.0),
                    );
                    icons::draw(
                        &painter,
                        if marks.offline {
                            Icon::Close
                        } else {
                            Icon::Import
                        },
                        badge,
                        if marks.offline {
                            t.text_secondary.gamma_multiply(0.7)
                        } else {
                            t.toolbar_icon
                        },
                        1.0,
                    );
                }

                // 文字与图标间距：图标右沿 + 7
                let text_x = icon_x + 16.0 + 7.0;
                let text_color = if imported && marks.offline {
                    t.text_secondary.gamma_multiply(0.8)
                } else if active {
                    t.row_selected_text
                } else {
                    t.text_primary
                };
                painter.text(
                    pos2(text_x, rect.center().y),
                    Align2::LEFT_CENTER,
                    &row.name,
                    FontId::new(sidebar_row_font_size(&t), egui::FontFamily::Proportional),
                    text_color,
                );
                if imported && let Some(tip) = marks.tip(row) {
                    resp.clone().on_hover_text(tip);
                }
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
        painter.rect_filled(cell, t.radius_btn.min(6.0), t.chip_bg);
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

#[cfg(test)]
mod m5_font_tests {
    use super::*;
    use crate::theme::Theme;
    use picasa_core::Appearance;

    /// 侧栏字号按原型 `.row .name` / `.row .cnt` 取（亮 13/11.5，暗 12.5/10.5）。
    ///
    /// 逐像素比对时发现应用两套主题都偏小：行名亮色 12.5（应 13）、暗色 11（应 12.5），
    /// 计数暗色也用了亮色的 11.5（应 10.5）。
    /// 同一行里计数比行名小一号；各自按中心对齐会让两个字号的中文底边错开
    /// （小字浮起）。三处计数都必须走 `count_center_nudge` 落到同一基线。
    #[test]
    fn sidebar_count_shares_the_row_baseline() {
        let light = Theme::for_appearance(Appearance::Light);
        let dark = Theme::for_appearance(Appearance::Dark);
        assert!(count_center_nudge(&light) > 0.0);
        assert!(
            count_center_nudge(&dark) >= count_center_nudge(&light),
            "暗色字号差更大，修正量不应更小（实测 {} vs {}）",
            count_center_nudge(&dark),
            count_center_nudge(&light)
        );
        let src = include_str!("sidebar.rs");
        assert!(
            src.matches("count_center_nudge(theme)").count() >= 2,
            "计数绘制点都必须走同一基线修正"
        );
    }

    #[test]
    fn sidebar_fonts_follow_the_prototype_scale() {
        let light = Theme::for_appearance(Appearance::Light);
        let dark = Theme::for_appearance(Appearance::Dark);
        assert_eq!(sidebar_row_font_size(&light), 13.0);
        assert_eq!(sidebar_row_font_size(&dark), 12.5);
        assert_eq!(sidebar_count_font_size(&light), 11.5);
        assert_eq!(sidebar_count_font_size(&dark), 10.5);
        // 两套主题都要有差异，别又合成同一个值。
        assert_ne!(sidebar_row_font_size(&light), sidebar_row_font_size(&dark));
        assert_ne!(
            sidebar_count_font_size(&light),
            sidebar_count_font_size(&dark)
        );
    }
}

#[cfg(test)]
mod prototype_spec_tests {
    /// 用户 2026-11-26（1:1 还原）：原型侧栏是**紧贴**的 —— `a.row` 高 30、行与行之间
    /// 没有间距（第一行 y 121.2、第二行 151.2），组与组之间只隔 8（上一个组最后一行底
    /// 301.2 = 下一个组头 301.2 − 8 的空档）。
    ///
    /// egui 默认 `item_spacing.y = 4~5`，不清零每一行会多吃 5px：六行之后整栏错位 30。
    #[test]
    fn sidebar_rows_are_packed_without_item_spacing() {
        let src = include_str!("sidebar.rs");
        assert!(
            src.contains("ui.spacing_mut().item_spacing.y = 0.0;"),
            "侧栏行必须紧贴，不能吃 egui 的默认 item 间距"
        );
        assert!(
            src.contains("ui.add_space(8.0);"),
            "组与组之间隔 8（原型 35.2 − 27.2）"
        );
        assert_eq!(
            crate::theme::Metrics::SIDEBAR_ROW_H,
            30.0,
            "原型 a.row 高 30"
        );
    }

    use super::*;

    /// 原型（`docs/m5/prototype/v1.4`，1280×820 实测）：
    /// `aside.side` 宽 **228**、`a.row` 高 **30**、`div.sec` 高 **27.19**。
    #[test]
    fn sidebar_metrics_match_the_prototype() {
        assert_eq!(Metrics::SIDEBAR_DEFAULT_W, 228.0, "原型 aside.side 宽 228");
        assert_eq!(Metrics::SIDEBAR_ROW_H, 30.0, "原型 a.row 高 30");
        assert_eq!(Metrics::SIDEBAR_SECTION_H, 27.0, "原型 div.sec 高 27.19");
        // 行高必须大于组头，否则行看起来比标题还扁
        assert!(Metrics::SIDEBAR_ROW_H > Metrics::SIDEBAR_SECTION_H);
    }

    /// 原型行内几何：行 x12 w204，图标 x22（行左 +10），名字 x45（图标左 +23），
    /// 计数右缘 x206（行右 216 −10）。名称与计数都不许贴边。
    #[test]
    fn sidebar_row_text_and_count_use_the_prototype_insets() {
        const SRC: &str = include_str!("sidebar.rs");
        assert!(
            SRC.contains("pos2(r.left() + 33.0, r.center().y)"),
            "行名左边距按原型 33（行左 +10 内边距 +23 图标位）"
        );
        assert!(SRC.contains("r.right() - 10.0"), "计数右缘按原型距行右 10");
    }
}

#[cfg(test)]
mod m5_sidebar_order_tests {
    use super::*;

    /// `M5-IA-01` / `M5-SB-01`：固定顺序是
    /// 图库 → 标签 → 相册 → 文件夹；「导入的索引」只在有来源时紧挨文件夹之后。
    #[test]
    fn m5_sidebar_group_order_is_fixed() {
        assert_eq!(SIDEBAR_GROUPS, ["图库", "标签", "相册", "文件夹"]);
        // 设置组与「索引管理」子项整组不存在（不是隐藏）。
        assert!(!SIDEBAR_GROUPS.contains(&"设置"));
        assert!(!SIDEBAR_GROUPS.iter().any(|g| g.contains("索引管理")));
    }

    /// 暗色相册卡的三列几何（原型 `.alb-covers`：三列、`gap:6px`、左右 6px）。
    ///
    /// 目标是「三格等宽 + 两处间距 + 左右内边距 = 可用宽」，且格宽随侧栏宽度线性变。
    #[test]
    fn album_cover_grid_is_three_equal_columns() {
        for total_w in [200.0_f32, 264.0, 320.0] {
            let side = album_cover_side(total_w);
            assert!(side > 0.0, "{total_w} 宽应算得出正边长，实际 {side}");
            let expect = (total_w - ALBUM_PAD_X * 2.0 - ALBUM_GAP * 2.0) / 3.0;
            assert!(
                (side - expect).abs() < 0.01,
                "{total_w} 宽：三列等分应为 {expect}，实际 {side}"
            );
            // 三格 + 两间距 + 两侧内边距刚好铺满，不溢出也不留缝。
            let used = side * 3.0 + ALBUM_GAP * 2.0 + ALBUM_PAD_X * 2.0;
            assert!(
                (used - total_w).abs() < 0.01,
                "铺满可用宽：{used} vs {total_w}"
            );
        }
        // 宽到能装下时，边长应比窄的时候大（不是写死的常量）。
        assert!(album_cover_side(320.0) > album_cover_side(200.0));
        // 退化输入不产生 NaN / 负数（窄窗过渡帧会传 0 或负）。
        assert_eq!(album_cover_side(0.0), 0.0);
        assert_eq!(album_cover_side(-10.0), 0.0);
        assert!(album_cover_side(f32::NAN).is_finite());
    }

    /// 封面下那行名字的截断宽（10px 字、按 6px/字估），窄到放不下也至少留 1 字。
    #[test]
    fn album_label_chars_follow_the_cell_side() {
        assert_eq!(album_label_chars(0.0), 1);
        assert_eq!(album_label_chars(-5.0), 1);
        assert_eq!(album_label_chars(6.0), 1);
        assert_eq!(album_label_chars(60.0), 10);
        assert!(album_label_chars(120.0) > album_label_chars(60.0));
    }

    /// 相册组两套主题画法不同、**行为必须一致**（`M5` §3：允许主题换画法，
    /// 不允许「只有某套主题能用」）。
    ///
    /// 这条是源码锁：`album_group` 必须按 `theme.dark` 分派，而缩略图卡那条路
    /// 必须自己发 `FilterByAlbum` / `AlbumContext`——否则暗色下点击、右键会静默失效，
    /// 而只测 `rows()` 是测不出来的。
    #[test]
    fn dark_album_covers_keep_the_same_actions_as_rows() {
        let src = include_str!("sidebar.rs");
        let start = src.find("fn album_group(").expect("album_group 应存在");
        let end = src.find("fn favorites(").expect("favorites 应在其后");
        let block = &src[start..end];
        assert!(
            block.contains("if !theme.dark"),
            "相册组应按主题分派\n{block}"
        );
        assert!(block.contains("album_covers(ui, ctx, actions)"));
        // 卡片那条路要保留与文字行相同的三个行为。
        let covers = &block[block.find("fn album_covers(").expect("album_covers 应存在")..];
        assert!(
            covers.contains("SidebarAction::FilterByAlbum"),
            "暗色卡片点击要能筛相册"
        );
        assert!(
            covers.contains("SidebarAction::AlbumContext"),
            "暗色卡片右键要能出相册菜单"
        );
        assert!(
            covers.contains("ctx.album_active"),
            "暗色卡片要画当前相册的选中环"
        );
    }

    /// `M5-SH-04`：左栏空态指向顶栏导入，不再指向已删除的「… → 添加文件夹…」。
    #[test]
    fn m5_sidebar_empty_hint_points_at_import() {
        let hint = EMPTY_SIDEBAR_HINT;
        assert!(hint.contains("还没有加入任何文件夹"), "{hint}");
        assert!(hint.contains("「导入」"), "{hint}");
        assert!(!hint.contains("「…」"), "{hint}");
        assert!(!hint.contains("添加文件夹"), "{hint}");
    }
}

#[cfg(test)]
mod imported_index_tests {
    use super::*;

    fn row(path: &str, name: &str, count: u32) -> FolderRowView {
        FolderRowView {
            path: path.to_string(),
            name: name.to_string(),
            folder_id: Some(7),
            count,
            has_children: false,
            collapsed: false,
            depth: 1,
        }
    }

    #[test]
    fn local_rows_get_no_import_tip() {
        let marks = MarkSet::default();
        assert!(!marks.is_imported());
        assert_eq!(
            marks.tip(&row("/Users/me/Photos", "Photos", 12)),
            None,
            "本机文件夹不该出现「来自导入的索引」之类的提示"
        );
    }

    #[test]
    fn imported_tip_carries_original_path_and_export_time_count() {
        let abs = vec!["/Volumes/Archive/2020".to_string()];
        let marks = MarkSet {
            imported: true,
            offline: false,
            abs_paths: &abs,
            local_dupes: &[],
        };
        let tip = marks.tip(&row("/media/2020", "2020", 340)).unwrap();
        assert!(tip.contains("来自导入的索引"), "{tip}");
        assert!(tip.contains("/Volumes/Archive/2020"), "要给原路径：{tip}");
        // 「导出时计数」而不是「N 张」：导入后本机可能又扫过，口径必须说清。
        assert!(tip.contains("导出时计数：340"), "{tip}");
        // 永久禁令：任何提示都不许出现 ETA/预计/剩余。
        for banned in ["预计", "剩余", "ETA"] {
            assert!(!tip.contains(banned), "提示里不许出现 {banned}：{tip}");
        }
    }

    #[test]
    fn offline_imported_rows_say_offline_in_words_not_only_by_color() {
        let abs = vec!["/Volumes/Unplugged".to_string()];
        let marks = MarkSet {
            imported: true,
            offline: true,
            abs_paths: &abs,
            local_dupes: &[],
        };
        let tip = marks.tip(&row("/media/x", "x", 5)).unwrap();
        assert!(
            tip.contains("离线"),
            "离线必须用文字表达，不能只靠置灰：{tip}"
        );
        assert!(tip.contains("/Volumes/Unplugged"), "{tip}");
    }

    #[test]
    fn local_duplicate_is_marked_but_count_stays_one() {
        let abs = vec!["/Users/me/Photos".to_string()];
        let dupes = vec!["/Users/me/Photos".to_string()];
        let marks = MarkSet {
            imported: true,
            offline: false,
            abs_paths: &abs,
            local_dupes: &dupes,
        };
        let tip = marks.tip(&row("/Users/me/Photos", "Photos", 88)).unwrap();
        assert!(tip.contains("本机已有"), "{tip}");
        assert!(tip.contains("以本机为准"), "{tip}");
        // 提示只说「以本机为准」，计数依旧是该行自己的数，不翻倍。
        assert!(tip.contains("导出时计数：88"), "{tip}");
    }

    #[test]
    fn import_group_default_is_renderable() {
        // Default 必须能直接喂给绘制代码（`refresh_sidebar_groups` 早期就构造过空表）。
        let g = ImportGroup::default();
        assert_eq!(g.photos, 0);
        assert!(!g.offline);
        assert!(g.abs_paths.is_empty());
        assert!(g.local_dupes.is_empty());
        // 名字与时间都要能参与 `format!` 渲染，不能是占位 None。
        assert_eq!(g.name, "");
        assert_eq!(g.when, "");
    }
}

#[cfg(test)]
mod m51_folder_tree_height_tests {
    use super::*;

    /// M5.1 用户反馈：「图库/标签/相册折叠后，文件夹列表只显示一部分，
    /// 没有一直显示到整个 sidebar」。
    ///
    /// 旧实现是 `max_height(260.0)`，折叠后腾出的高度没人接管。现在的口径是
    /// 「剩余高度全给文件夹树」——`folder_tree_budget` 必须随可用高度**线性增长**，
    /// 且不再有 260 这个天花板。
    #[test]
    fn folder_tree_uses_the_whole_remaining_height() {
        let row_h = Metrics::sidebar_row_h(false); // 30
        // 上面三组折叠、又没有导入来源：整栏都归文件夹树。
        for avail in [200.0_f32, 400.0, 600.0, 900.0] {
            let h = folder_tree_budget(avail, 0.0, row_h);
            assert!(
                (h - avail).abs() < 0.01,
                "可用 {avail} 时应把剩余高度全给树，实际 {h}"
            );
            assert!(h > 260.0 || avail <= 260.0, "不再受 260 封顶：{avail} → {h}");
        }
        // 窗口够高时高度必须超过旧的 260 上限（这正是本次修复的回归点）。
        assert!(folder_tree_budget(800.0, 0.0, row_h) > 260.0);
    }

    /// 有「导入的索引」在下面时，只让出它必须占的高度，其余仍归文件夹树。
    #[test]
    fn folder_tree_only_yields_what_the_imported_group_needs() {
        let row_h = Metrics::sidebar_row_h(false);
        let avail = 700.0_f32;
        let reserved = 120.0_f32;
        let h = folder_tree_budget(avail, reserved, row_h);
        assert!((h - (avail - reserved)).abs() < 0.01, "剩余 {h}");
        // 让出之后仍远大于旧上限，说明没有退回 260。
        assert!(h > 260.0);
    }

    /// 极端输入不产生负数 / NaN：窄窗、超多分组叠加时会低于保底，此时给 3 行。
    #[test]
    fn folder_tree_budget_never_collapses_to_a_sliver() {
        let row_h = Metrics::sidebar_row_h(true); // 28
        let floor = row_h * TREE_MIN_ROWS;
        assert_eq!(folder_tree_budget(10.0, 0.0, row_h), floor);
        assert_eq!(folder_tree_budget(0.0, 0.0, row_h), floor);
        assert_eq!(folder_tree_budget(-50.0, 0.0, row_h), floor);
        assert_eq!(folder_tree_budget(100.0, 500.0, row_h), floor);
        // 退化输入不得产生 NaN / 负数。
        assert_eq!(folder_tree_budget(f32::NAN, 0.0, row_h), floor);
        let h = folder_tree_budget(100.0, f32::NAN, row_h);
        assert!(h.is_finite() && h >= floor, "{h}");
        // 负的预留高度按 0 处理（调用方算错也不会把树挤没）。
        assert!((folder_tree_budget(400.0, -100.0, row_h) - 400.0).abs() < 0.01);
    }

    /// 绘制点必须走动态预算：源码里不许再出现写死的 `TREE_MAX_H`。
    /// 这是源码锁——纯函数测对了、绘制点却仍调 `.max_height(260.0)` 的话，
    /// 用户看到的还是老样子。
    #[test]
    fn render_path_passes_the_computed_budget_not_a_constant() {
        let src = include_str!("sidebar.rs");
        let prod = match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        };
        // 注释里会提到「不再有固定的 260 上限」这个历史结论，那不算绘制点；
        // 只检查真正的代码行。
        let code: String = prod
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("TREE_MAX_H"),
            "写死的高度上限已删除，绘制点改用 `folder_tree_budget`"
        );
        // 两处绘制点（本机树 / 导入来源树）都必须传预算参数。
        assert!(
            code.matches("folder_tree_budget(").count() >= 2,
            "本机树与导入来源树都要按剩余高度算预算"
        );
        assert!(
            code.contains("folder_tree_budget(ui.available_height()"),
            "文件夹树按 `ui.available_height()` 现算高度"
        );
        // ScrollArea 的高度来自入参，不再来自常量。
        assert!(
            code.contains(".max_height(slot.max_h)"),
            "`render_tree_rows` 的高度必须由调用方传入"
        );
    }
}
