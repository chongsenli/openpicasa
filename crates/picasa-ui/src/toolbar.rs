//! 顶部工具栏。
//!
//! ## M3.1 §7 定稿（2026-11-03）
//!
//! * 单行、通栏的**图标工具栏**：导入 / 幻灯片 / 编辑（无文字圆形图标）、
//!   排布三图标连体胶囊、−/+、搜索（紧接在 + 后）；
//! * 大号文件夹标题与文件树**不在**顶栏——它们移到中区左侧；
//! * 排序 / 筛选 / 「…」保留在右侧图标组（既有功能不丢）；「…」里承载
//!   添加文件夹、含子文件夹、方格文件名开关、全屏、关于；
//! * 「视图」弹出菜单与 XP 菜单栏/工具行已退役。
//!
//! 实际绘制在 [`crate::topbar`]，本文件保留筛选面板（弹层）与旧类型转调，
//! 让 `app.rs` 的改动最小。

use eframe::egui::{self, Color32, Pos2, Ui};

use picasa_core::ViewMode;

pub use crate::topbar::{MoreAction, ObjectActionState, TopbarAction, TopbarOutput};

/// 顶栏（转调 [`crate::topbar::show`]）。
///
/// 保留原函数名与近似签名，`app.rs` 只需换枚举处理分支。
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    theme: &crate::theme::Theme,
    _cell_px: f32,
    breadcrumb: &[(String, Option<i64>)],
    sort: picasa_db::SortSpec,
    _has_selection: bool,
    search: &mut String,
    _sort_menu_open: bool,
    _recurse_on: bool,
    _recurse_available: bool,
    filter_active: bool,
    search_in_current: bool,
    search_open: bool,
    view_mode: ViewMode,
    zoom_min: bool,
    zoom_max: bool,
    // 左栏当前宽度（左段右缘与文件夹树右缘对齐，M32-UI-01/02 v1.3）
    sidebar_w: f32,
    // M4.1：四个对象操作按钮的双态判定
    obj: crate::topbar::ObjectActionState,
) -> TopbarOutput {
    crate::topbar::show(
        ui,
        theme,
        breadcrumb,
        view_mode,
        sort,
        filter_active,
        search,
        search_open,
        search_in_current,
        zoom_min,
        zoom_max,
        sidebar_w,
        obj,
    )
}

// ⚠️ M2.2 §2.6：原「索引管理」二级菜单已整体移除；
// M3.1 起整个 XP 工具行也由单行顶栏替代（见文件头）。不留残骸。

/// 排序弹出菜单：转调顶栏版本（观感与截图一致）。
pub fn sort_menu(ui: &mut Ui, at: Pos2, current: picasa_db::SortSpec) -> Option<TopbarAction> {
    crate::topbar::sort_menu(ui, at, current)
}

/// 「…」弹出菜单：转调顶栏版本。
pub fn more_menu(
    ui: &mut Ui,
    at: Pos2,
    recurse_on: bool,
    recurse_available: bool,
    show_filename: bool,
) -> Vec<TopbarAction> {
    crate::topbar::more_menu(ui, at, recurse_on, recurse_available, show_filename)
}

/// 筛选面板的动作（M3.1 起与顶栏动作解耦，避免枚举递归）。
///
/// M4.1 `M41-UI-08`：删掉 `BatchStar` / `BatchAddTag` —— 筛选面板只管「查什么」，
/// 「对选中集做什么」全部收进工具栏 ♥ / ⌗ 按钮。
#[derive(Clone, Debug)]
pub enum FilterPanelAction {
    Apply(Box<picasa_db::PhotoFilter>),
    Clear,
}

/// 筛选弹出面板（M4 #5/#9：日期 / 类型 / 尺寸 / 标签 / 收藏）。
///
/// M4.1 `M41-UI-07`：类型组首位新增「全部」；`M41-UI-08`：删除两段批量操作 UI。
/// 返回 `(可能的动作, 草稿标签回写)`——标签输入框的内容要跨帧保存在 App 里。
///
/// M3.1 起观感改为 macOS 弹层（白卡、圆角 8、柔影），交互不变。
pub fn filter_menu(
    ui: &mut Ui,
    at: Pos2,
    current: &picasa_db::PhotoFilter,
    draft_tag: &mut String,
) -> (Option<FilterPanelAction>, String) {
    let mut action = None;
    let mut tag_out = draft_tag.clone();
    let mut draft = current.clone();

    egui::Area::new(ui.scope_id().with("filter-menu"))
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
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_min_width(230.0);
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("筛选")
                            .size(13.0)
                            .strong()
                            .color(Color32::from_rgb(0x20, 0x20, 0x20)),
                    );
                    ui.add_space(4.0);
                    ui.separator();

                    // ---- 收藏 ----
                    if ui
                        .checkbox(&mut draft.starred_only, "只看收藏（星标）")
                        .changed()
                    {
                        action = Some(FilterPanelAction::Apply(Box::new(draft.clone())));
                    }

                    // ---- 类型 ----
                    // M4.1 `M41-UI-07`：首位「全部」只清空类型维度，不动其他维度。
                    ui.label("类型");
                    ui.horizontal_wrapped(|ui| {
                        let mut all = draft.formats.is_empty();
                        if ui.toggle_value(&mut all, "全部").clicked() && all {
                            draft.formats.clear();
                            action = Some(FilterPanelAction::Apply(Box::new(draft.clone())));
                        }
                        for (slug, label) in [
                            ("jpeg", "JPEG"),
                            ("png", "PNG"),
                            ("gif", "GIF"),
                            ("webp", "WebP"),
                            ("tiff", "TIFF"),
                            ("svg", "SVG"),
                            ("heic", "HEIC"),
                            ("avif", "AVIF"),
                        ] {
                            let mut on = draft.formats.iter().any(|f| f == slug);
                            if ui.toggle_value(&mut on, label).clicked() {
                                if on {
                                    draft.formats.push(slug.into());
                                } else {
                                    draft.formats.retain(|f| f != slug);
                                }
                                action = Some(FilterPanelAction::Apply(Box::new(draft.clone())));
                            }
                        }
                        // RAW 家族一个选项覆盖全部
                        let mut raw_on = draft
                            .formats
                            .iter()
                            .any(|f| matches!(f.as_str(), "cr2" | "cr3" | "nef" | "arw" | "dng"));
                        if ui.toggle_value(&mut raw_on, "RAW").clicked() {
                            for slug in ["cr2", "cr3", "nef", "arw", "dng"] {
                                if raw_on {
                                    draft.formats.push(slug.into());
                                } else {
                                    draft.formats.retain(|f| f != slug);
                                }
                            }
                            action = Some(FilterPanelAction::Apply(Box::new(draft.clone())));
                        }
                    });

                    // ---- 尺寸 ----
                    ui.label("尺寸");
                    ui.horizontal(|ui| {
                        let mut none = draft.size_class.is_none();
                        if ui.toggle_value(&mut none, "全部").clicked() {
                            draft.size_class = None;
                            action = Some(FilterPanelAction::Apply(Box::new(draft.clone())));
                        }
                        for sc in [
                            picasa_db::SizeClass::Small,
                            picasa_db::SizeClass::Medium,
                            picasa_db::SizeClass::Large,
                        ] {
                            let mut on = draft.size_class == Some(sc);
                            if ui.toggle_value(&mut on, sc.label()).clicked() {
                                draft.size_class = Some(sc);
                                action = Some(FilterPanelAction::Apply(Box::new(draft.clone())));
                            }
                        }
                    });

                    // ---- 标签 ----
                    ui.label("标签");
                    ui.horizontal(|ui| {
                        if ui.text_edit_singleline(draft_tag).changed() {
                            tag_out = draft_tag.clone();
                        }
                        if ui.button("按此标签筛").clicked() {
                            let t = draft_tag.trim().to_string();
                            draft.tag = (!t.is_empty()).then_some(t);
                            action = Some(FilterPanelAction::Apply(Box::new(draft.clone())));
                        }
                    });

                    // ---- M4 组合筛选新增维度（docs/11 §4.1）----
                    ui.separator();
                    ui.label("相机 / 镜头");
                    ui.horizontal(|ui| {
                        let mut cam = draft.camera.clone().unwrap_or_default();
                        ui.add(egui::TextEdit::singleline(&mut cam).hint_text("相机型号").desired_width(90.0));
                        draft.camera = (!cam.trim().is_empty()).then(|| cam.trim().to_string());
                        let mut lens = draft.lens.clone().unwrap_or_default();
                        ui.add(egui::TextEdit::singleline(&mut lens).hint_text("镜头").desired_width(80.0));
                        draft.lens = (!lens.trim().is_empty()).then(|| lens.trim().to_string());
                        if ui.button("应用").clicked() {
                            action = Some(FilterPanelAction::Apply(Box::new(draft.clone())));
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut miss_date = draft.missing_date;
                        if ui.checkbox(&mut miss_date, "缺拍摄时间").changed() {
                            draft.missing_date = miss_date;
                            action = Some(FilterPanelAction::Apply(Box::new(draft.clone())));
                        }
                        let mut miss_exif = draft.missing_exif;
                        if ui.checkbox(&mut miss_exif, "缺元数据").changed() {
                            draft.missing_exif = miss_exif;
                            action = Some(FilterPanelAction::Apply(Box::new(draft.clone())));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("文件大小 (KB)");
                        let mut lo = draft
                            .size_bytes
                            .map(|(a, _)| (a / 1024).to_string())
                            .unwrap_or_default();
                        let mut hi = draft
                            .size_bytes
                            .map(|(_, b)| (b / 1024).to_string())
                            .unwrap_or_default();
                        ui.add(egui::TextEdit::singleline(&mut lo).desired_width(48.0));
                        ui.label("～");
                        ui.add(egui::TextEdit::singleline(&mut hi).desired_width(48.0));
                        if ui.button("应用").clicked() {
                            let a: i64 = lo.parse::<i64>().unwrap_or(0) * 1024;
                            let b: i64 = hi.parse::<i64>().unwrap_or(i64::MAX / 1024) * 1024;
                            draft.size_bytes = (a > 0 || b < i64::MAX).then_some((a, b));
                            action = Some(FilterPanelAction::Apply(Box::new(draft.clone())));
                        }
                    });

                    // M4.1 `M41-UI-08`：批量收藏 / 批量打标签已移到工具栏 ♥ / ⌗ 按钮，
                    // 筛选面板只保留「清除全部筛选」（清所有维度）。
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("清除全部筛选").clicked() {
                            action = Some(FilterPanelAction::Clear);
                        }
                    });
                });
        });
    (action, tag_out)
}
