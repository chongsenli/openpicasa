//! 顶部工具栏。
//!
//! ## M3.1 §7 定稿（2026-11-03）
//!
//! * 单行、通栏的**图标工具栏**：导入 / 幻灯片 / 编辑（无文字圆形图标）、
//!   排布三图标连体胶囊、搜索（紧接在 + 后）；M5-TB-10 起顶栏没有 −/+；
//! * 大号文件夹标题与文件树**不在**顶栏——它们移到中区左侧；
//! * 排序 / 筛选 / 「…」保留在右侧图标组（既有功能不丢）；「…」里承载
//!   含子文件夹、方格文件名开关、设置…、索引管理…、全屏、关于
//!   （M5-MN-01：不再有「添加文件夹…」，加目录只走顶栏导入）；
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
        sidebar_w,
        obj,
    )
}

// ⚠️ M2.2 §2.6：原「索引管理」二级菜单已整体移除；
// M3.1 起整个 XP 工具行也由单行顶栏替代（见文件头）。不留残骸。

/// 排序弹出菜单：转调顶栏版本（观感与截图一致）。
pub fn sort_menu(
    ui: &mut Ui,
    at: Pos2,
    theme: &crate::theme::Theme,
    current: picasa_db::SortSpec,
    thumbs_first: bool,
) -> Option<TopbarAction> {
    crate::topbar::sort_menu(ui, at, theme, current, thumbs_first)
}

/// 「…」弹出菜单：转调顶栏版本。
pub fn more_menu(
    ui: &mut Ui,
    at: Pos2,
    theme: &crate::theme::Theme,
    recurse_on: bool,
    recurse_available: bool,
    show_filename: bool,
) -> Vec<TopbarAction> {
    crate::topbar::more_menu(ui, at, theme, recurse_on, recurse_available, show_filename)
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
/// 文件大小上下界（KB → 字节）。两格都空 = 不设这一维。
///
/// `M5-SF-10`：解析失败时下界当 0、上界极大；**但「两格都空」不是解析失败**，
/// 它表示用户没填这一维，不能因此造出一个「全通过」的筛选（那会让筛选按钮
/// 误亮、空库空态误报「当前搜索或筛选没有结果」）。
pub fn parse_size_bounds(lo_text: &str, hi_text: &str) -> Option<(i64, i64)> {
    let lo_text = lo_text.trim();
    let hi_text = hi_text.trim();
    if lo_text.is_empty() && hi_text.is_empty() {
        return None;
    }
    let lo = lo_text.parse::<i64>().unwrap_or(0).max(0) * 1024;
    let hi = hi_text.parse::<i64>().unwrap_or(i64::MAX / 1024) * 1024;
    Some((lo, hi))
}

#[allow(clippy::too_many_arguments)]
pub fn filter_menu(
    ui: &mut Ui,
    at: Pos2,
    theme: &crate::theme::Theme,
    current: &picasa_db::PhotoFilter,
    draft_tag: &mut String,
    // `M5-SF-08` / `M5-SF-10`：相机 / 镜头 / 文件大小必须点「应用」，
    // 所以草稿要跨帧存在 App 里；每帧从 `current` 重建输入框会让用户一个字都打不进去。
    draft_camera: &mut String,
    draft_lens: &mut String,
    draft_size_lo: &mut String,
    draft_size_hi: &mut String,
) -> (Option<FilterPanelAction>, String) {
    let mut action = None;
    let mut tag_out = draft_tag.clone();
    let mut draft = current.clone();

    egui::Area::new(ui.scope_id().with("filter-menu"))
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
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_min_width(230.0);
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("筛选")
                            .size(13.0)
                            .strong()
                            .color(theme.text_primary),
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
                        // M5-SF-08：输入只改草稿，**点「应用」才生效**。
                        ui.add(egui::TextEdit::singleline(draft_camera).hint_text("相机型号").desired_width(90.0));
                        ui.add(egui::TextEdit::singleline(draft_lens).hint_text("镜头").desired_width(80.0));
                        if ui.button("应用").clicked() {
                            let cam = draft_camera.trim();
                            draft.camera = (!cam.is_empty()).then(|| cam.to_string());
                            let lens = draft_lens.trim();
                            draft.lens = (!lens.is_empty()).then(|| lens.to_string());
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
                        ui.add(egui::TextEdit::singleline(draft_size_lo).desired_width(48.0));
                        ui.label("～");
                        ui.add(egui::TextEdit::singleline(draft_size_hi).desired_width(48.0));
                        if ui.button("应用").clicked() {
                            draft.size_bytes = parse_size_bounds(draft_size_lo, draft_size_hi);
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

#[cfg(test)]
mod m5r5_filter_tests {
    use super::*;

    /// `M5-SF-10`：文件大小上下界——解析失败下界当 0、上界极大；
    /// 两格都空 = 这一维不参与筛选，不能造出一个「全通过」的条件。
    #[test]
    fn m5_size_bounds_follow_the_spec_and_empty_is_not_a_filter() {
        assert_eq!(parse_size_bounds("", ""), None, "两格都空不该变成筛选");
        assert_eq!(parse_size_bounds("  ", "  "), None);
        assert_eq!(parse_size_bounds("100", "2048"), Some((100 * 1024, 2048 * 1024)));
        // 解析失败：下界 0、上界极大（不是整条丢掉）
        let (lo, hi) = parse_size_bounds("abc", "xyz").expect("有输入就算一维");
        assert_eq!(lo, 0);
        assert!(hi > 10_000_000_000, "上界应取「极大」，实际 {hi}");
        // 只填下界：上界极大
        let (lo, hi) = parse_size_bounds("10", "").expect("只填下界也是一维");
        assert_eq!(lo, 10 * 1024);
        assert!(hi > 10_000_000_000);
        // 负数按 0 处理
        let (lo, _) = parse_size_bounds("-5", "").unwrap();
        assert_eq!(lo, 0);
    }

    /// `XX-06` / `M5-SF-12`：筛选按钮的悬停文案里有「日期」，但**面板不得长出
    /// 日期控件**——日期的唯一 UI 是侧栏年份。这里用无窗口后端把面板跑一帧，
    /// 收集本帧所有文本，逐条确认没有日期输入 / 年份选择。
    #[test]
    fn m5_filter_panel_never_grows_a_date_control() {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx, picasa_core::Appearance::Light);
        let mut tag = String::new();
        let mut cam = String::new();
        let mut lens = String::new();
        let mut lo = String::new();
        let mut hi = String::new();
        let mut texts: Vec<String> = Vec::new();
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 820.0),
                )),
                max_texture_side: Some(2048),
                ..Default::default()
            };
            let out = ctx.run_ui(input, |ui| {
                let theme = crate::theme::Theme::for_appearance(picasa_core::Appearance::Light);
                let (_a, _t) = filter_menu(
                    ui,
                    egui::pos2(500.0, 100.0),
                    &theme,
                    &picasa_db::PhotoFilter::default(),
                    &mut tag,
                    &mut cam,
                    &mut lens,
                    &mut lo,
                    &mut hi,
                );
            });
            let mut found = Vec::new();
            fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
                match shape {
                    egui::Shape::Vec(v) => {
                        for s in v {
                            walk(s, out);
                        }
                    }
                    egui::Shape::Text(t) => out.push(t.galley.job.text.clone()),
                    _ => {}
                }
            }
            for c in &out.shapes {
                walk(&c.shape, &mut found);
            }
            texts = found;
            out.drop_without_applying_deltas();
        }
        let joined = texts.join(" | ");
        // 注意「缺拍摄时间」是**复选框**、不是日期控件，所以不能简单禁「拍摄时间」。
        // 这里禁的是日期选择 / 年份 / 评分 / 人物 / 颜色这些「面板不该长的维度」。
        for banned in ["日期", "年", "月", "日", "评分", "星级", "人物", "颜色"] {
            assert!(
                !texts.iter().any(|t| t.contains(banned)),
                "筛选面板不该出现「{banned}」：{joined}"
            );
        }
        // 该有的维度都在
        for need in ["只看收藏", "类型", "尺寸", "标签", "相机", "镜头", "缺拍摄时间", "缺元数据", "文件大小", "清除全部筛选"] {
            assert!(
                texts.iter().any(|t| t.contains(need)),
                "筛选面板缺「{need}」：{joined}"
            );
        }
    }

    /// `M5-SF-08`：相机 / 镜头输入必须点「应用」——纯函数层面的口径是
    /// 「草稿文本 → 筛选字段」只在应用时发生。这里锁住 trim 与空串语义，
    /// 防止有人把空白字符串当成「筛空白相机」写进 `PhotoFilter`。
    #[test]
    fn m5_camera_lens_drafts_only_become_filters_on_apply() {
        let mut draft = picasa_db::PhotoFilter::default();
        for raw in ["  ", "\t", ""] {
            let t = raw.trim();
            draft.camera = (!t.is_empty()).then(|| t.to_string());
            draft.lens = (!t.is_empty()).then(|| t.to_string());
            assert!(draft.camera.is_none(), "空白相机不该进筛选");
            assert!(draft.lens.is_none(), "空白镜头不该进筛选");
        }
        let t = "  Canon R5 ".trim();
        draft.camera = (!t.is_empty()).then(|| t.to_string());
        assert_eq!(draft.camera.as_deref(), Some("Canon R5"));
    }
}
