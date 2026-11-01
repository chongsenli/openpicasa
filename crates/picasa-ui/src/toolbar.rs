//! 顶部工具栏：图标在上、文案在下，按钮之间以竖线分组（U-06~U-12）。
//!
//! ## 红线 R-2 在这里的体现
//!
//! **M1 的工具栏里没有「编辑」「编辑室」「星标」按钮。** 不是置灰，是**不存在**
//! （01 U-12 的验收判据就是「全量截图核对无编辑入口」）。
//!
//! 唯一保留的置灰入口是左栏的「星标」——它是版式的一部分（U-19 的例外说明）。

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Response, RichText, Sense, Ui, pos2, vec2,
};
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
    /// 打开/收起「索引管理」二级菜单
    ToggleIndexMenu,
}

/// 「索引管理」二级菜单里的动作。
///
/// 这里**只放用户真的需要在主界面随手点到的两件事**：修坏图、看进度。
/// 暂停/删除索引属于「看着进度决定」的操作，留在进度面板里，
/// 菜单项只负责把人送到那里——同一件事两个入口两套状态，迟早会不一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexMenuAction {
    /// 重建失败索引（重新解码缩略图失败的照片）
    RebuildFailed,
    /// 打开索引进度面板（暂停/继续/删除都在里面）
    OpenProgress,
}

pub struct ToolbarOutput {
    pub actions: Vec<ToolbarAction>,
    /// 搜索框内容（双向绑定由 App 处理）
    pub search_changed: Option<String>,
    pub sort_button: Option<Response>,
    /// 「索引管理」按钮的响应（App 用它定位二级菜单）
    pub index_button: Option<Response>,
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
    index_menu_open: bool,
    failed_thumbs: u64,
) -> ToolbarOutput {
    let mut out = ToolbarOutput {
        actions: Vec::new(),
        search_changed: None,
        sort_button: None,
        index_button: None,
    };
    let rect = Rect::from_min_size(
        ui.max_rect().left_top(),
        vec2(ui.max_rect().width(), Metrics::TOOLBAR_H),
    );
    let _ = ui.allocate_space(vec2(rect.width(), rect.height()));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme.chrome_bg);
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(1.0, theme.separator),
    );

    let mut x = rect.left() + 6.0;
    let btn_y = rect.top() + 6.0;
    let btn_h = Metrics::TOOLBAR_H - 12.0;

    // ---- 组 1：返回（M1 恒禁用）----
    // 原版有「返回图库」，M1 没有别的房间可返回，所以**保留占位但禁用**（U-09）。
    let back_rect = Rect::from_min_size(pos2(x, btn_y), vec2(Metrics::TOOL_BTN_W, btn_h));
    tool_button(
        ui,
        &painter,
        back_rect,
        Icon::Back,
        "返回",
        false,
        "返回图库（M1 无其他视图）",
        theme,
    );
    x = back_rect.right() + Metrics::TOOL_GROUP_PAD;
    group_separator(&painter, x, rect, theme);
    x += Metrics::TOOL_GROUP_PAD;

    // ---- 组 2：添加文件夹 / 在 Finder 中显示 ----
    let add_rect = Rect::from_min_size(pos2(x, btn_y), vec2(Metrics::TOOL_BTN_W, btn_h));
    if tool_button(
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
    x = add_rect.right() + 2.0;

    let reveal_rect = Rect::from_min_size(pos2(x, btn_y), vec2(Metrics::TOOL_BTN_W, btn_h));
    if tool_button(
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
    x = reveal_rect.right() + Metrics::TOOL_GROUP_PAD;
    group_separator(&painter, x, rect, theme);
    x += Metrics::TOOL_GROUP_PAD;

    // ---- 组 3：排序 / 缩略图大小 ----
    let sort_rect = Rect::from_min_size(pos2(x, btn_y), vec2(Metrics::TOOL_BTN_W, btn_h));
    let sort_label = sort_label(sort.key, sort.descending);
    let sort_resp = tool_button_response(
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
    x = sort_rect.right() + 2.0;

    let size_rect = Rect::from_min_size(pos2(x, btn_y), vec2(Metrics::TOOL_BTN_W, btn_h));
    if tool_button(
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
    x = size_rect.right() + Metrics::TOOL_GROUP_PAD;
    group_separator(&painter, x, rect, theme);
    x += Metrics::TOOL_GROUP_PAD;

    // ---- 组 4：全屏 ----
    let fs_rect = Rect::from_min_size(pos2(x, btn_y), vec2(Metrics::TOOL_BTN_W, btn_h));
    if tool_button(
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
    x = fs_rect.right() + Metrics::TOOL_GROUP_PAD;
    group_separator(&painter, x, rect, theme);
    x += Metrics::TOOL_GROUP_PAD;

    // ---- 组 5：索引管理 ----
    //
    // 为什么常驻而不是"有失败才出现"：工具栏是肌肉记忆的位置。按钮时有时无，
    // 用户下次想找它就会在同一个位置点空；而角标已经承担了"有坏图"的提醒。
    let idx_rect = Rect::from_min_size(pos2(x, btn_y), vec2(Metrics::TOOL_BTN_W, btn_h));
    let idx_resp = tool_button_response(
        ui,
        &painter,
        idx_rect,
        Icon::Index,
        "索引管理",
        true,
        "索引管理：重建失败索引 / 查看索引进度",
        theme,
        index_menu_open,
    );
    if idx_resp.clicked() {
        out.actions.push(ToolbarAction::ToggleIndexMenu);
    }
    if failed_thumbs > 0 {
        draw_badge(
            &painter,
            idx_rect,
            &failed_thumbs.to_string(),
            Color32::from_rgb(0xC0, 0x39, 0x2B),
        );
    }
    out.index_button = Some(idx_resp);

    // ---- 右侧：搜索框（原版也在右上角）----
    let search_w = 180.0_f32.min(rect.width() * 0.28);
    let search_rect = Rect::from_min_size(
        pos2(rect.right() - search_w - 12.0, rect.center().y - 11.0),
        vec2(search_w, 22.0),
    );
    painters_search_box(ui, &painter, search_rect, theme, search, &mut out);

    // 档位提示（右下方小字，帮助演示时确认当前档）
    painter.text(
        pos2(search_rect.left(), search_rect.bottom() + 3.0),
        Align2::LEFT_TOP,
        format!(
            "{}px · {}",
            Metrics::CELL_SIZES[tier.index()] as i32,
            match tier {
                Tier::Small => "小",
                Tier::Medium => "中",
                Tier::Large => "大",
            }
        ),
        FontId::new(10.0, egui::FontFamily::Proportional),
        theme.text_secondary,
    );

    out
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

fn group_separator(painter: &egui::Painter, x: f32, rect: Rect, theme: &Theme) {
    painter.line_segment(
        [pos2(x, rect.top() + 10.0), pos2(x, rect.bottom() - 10.0)],
        egui::Stroke::new(1.0, theme.separator),
    );
}

/// 一个工具栏按钮：图标在上、文案在下（U-07），悬停才出现底框。
///
/// 参数多是因为这里刻意**不引入结构体**：所有按钮都是即画即弃的，
/// 拆成 `ToolButtonSpec` 只会让 7 个调用点多一层构造。
#[allow(clippy::too_many_arguments)]
fn tool_button(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    icon: Icon,
    label: &str,
    enabled: bool,
    tooltip: &str,
    theme: &Theme,
) -> bool {
    tool_button_response(
        ui, painter, rect, icon, label, enabled, tooltip, theme, false,
    )
    .clicked()
}

#[allow(clippy::too_many_arguments)]
fn tool_button_response(
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
    let resp = ui.interact(rect, ui.scope_id().with(("tool", label)), sense);
    let hovered = enabled && resp.hovered();

    if pressed || hovered {
        painter.rect_filled(rect.shrink(2.0), 3.0, theme.cell_hover);
        painter.rect_stroke(
            rect.shrink(2.0),
            3.0,
            egui::Stroke::new(1.0, theme.separator),
            egui::StrokeKind::Inside,
        );
    }

    let color = if enabled {
        theme.text_primary
    } else {
        theme.text_disabled
    };

    // 图标在上
    let icon_center_y = rect.top() + 14.0 + Metrics::TOOL_ICON * 0.5;
    let icon_rect = Rect::from_center_size(
        pos2(rect.center().x, icon_center_y),
        vec2(Metrics::TOOL_ICON, Metrics::TOOL_ICON),
    );
    icons::draw(painter, icon, icon_rect, color, 1.4);

    // 文案在下
    painter.text(
        pos2(rect.center().x, icon_rect.bottom() + 5.0),
        Align2::CENTER_TOP,
        label,
        FontId::new(11.0, egui::FontFamily::Proportional),
        if enabled {
            theme.text_primary
        } else {
            theme.text_disabled
        },
    );

    if hovered {
        // U-11：悬停 500ms 出 tooltip（egui 的 on_hover_text 自带延迟）
        resp.clone().on_hover_text(tooltip);
    } else if !enabled {
        resp.clone().on_hover_text(tooltip);
    }
    resp
}

/// 搜索框：白底、1px 边框、左侧放大镜（对齐 Picasa 右上角的外观）。
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
        egui::Stroke::new(1.0, theme.separator),
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

/// 「索引管理」二级菜单（由 App 画：开关状态归 App，工具栏保持无状态）。
///
/// 只做两件事：修坏图、进进度面板。暂停/删除索引仍由进度面板负责——
/// 那些是「看着进度做决定」的动作，放进菜单就会出现两套状态。
///
/// 返回 `(动作, 实测矩形)`：矩形给调用方做「点外面就关」的判定。
/// 猜尺寸（`vec2(180, 200)`）在换字号/换语言后就会错，右键菜单踩过这个坑。
pub fn index_menu(ui: &mut Ui, at: Pos2, failed: u64) -> (Option<IndexMenuAction>, Rect) {
    let mut action = None;
    let area = egui::Area::new(egui::Id::unique("picasa.index-menu"))
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
                    ui.set_min_width(220.0);
                    let label = if failed > 0 {
                        format!("重建失败索引（{failed} 项）")
                    } else {
                        "重建失败索引（无失败项）".to_string()
                    };
                    // 没有失败项时置灰而不是隐藏：位置要稳定，用户才知道自己没点错
                    if ui
                        .add_enabled(failed > 0, egui::Button::new(label).frame(false))
                        .clicked()
                    {
                        action = Some(IndexMenuAction::RebuildFailed);
                    }
                    if failed == 0 {
                        ui.label(
                            RichText::new("缩略图都正常，无需重建")
                                .size(10.5)
                                .color(Color32::from_gray(0x77)),
                        );
                    }
                    ui.separator();
                    if ui
                        .add(egui::Button::new("索引进度 / 暂停 / 删除…").frame(false))
                        .clicked()
                    {
                        action = Some(IndexMenuAction::OpenProgress);
                    }
                });
        });
    (action, area.response.rect)
}

/// 按钮左上角的数量角标（失败项统计）。数字超过 999 显示 `999+`，免得角标比按钮还宽。
fn draw_badge(painter: &egui::Painter, btn: Rect, text: &str, color: Color32) {
    let shown = if text.len() > 4 {
        "999+".to_string()
    } else {
        text.to_string()
    };
    let font = FontId::new(9.5, egui::FontFamily::Proportional);
    let galley = painter.layout_no_wrap(shown, font, Color32::WHITE);
    let pad = vec2(4.0, 0.5);
    let size = vec2(galley.size().x + pad.x * 2.0, galley.size().y + pad.y * 2.0);
    // 图标居中于按钮上方：角标压在图标右上角的外侧一点，不遮住图形本身
    let icon_right = btn.center().x + Metrics::TOOL_ICON * 0.5;
    let icon_top = btn.top() + 14.0;
    let min = pos2(icon_right - size.x * 0.45, icon_top - size.y * 0.5);
    let rect = Rect::from_min_size(min, size);
    painter.rect_filled(rect, size.y * 0.5, color);
    painter.galley(rect.min + pad, galley, Color32::WHITE);
}
