//! 暗色全屏查看器（U-39~U-42 / F2-01~F2-05、F2-09、F2-11、F2-17）。
//!
//! ## 三个要点（02 §8.5）
//!
//! 1. **深灰而不是纯黑**（U-39）：纯黑背景下纯黑照片看不出边界。
//! 2. **`ViewportCommand::Fullscreen` 是正规全屏路径**，不是窗口 flag hack。
//! 3. **鼠标静止 3 秒隐藏 chrome + 光标**，一动就回来。
//!
//! ## M1 的明确取舍
//!
//! 查看器里**没有旋转、没有编辑**（R-2）。顶部只保留「返回」与「在 Finder 显示」
//! ——这是 01 U-42 登记过的**有意差异**。
//!
//! ## M4.2 M42-UI-01：补上星标 / 标签的**只读显示**
//!
//! M4.2 起底部信息条会显示收藏星标与照片标签（chips）。数据层只有
//! `starred: bool`（0/1 收藏态，无 1~5 评分列，R-M42-2 不新增字段），
//! 所以 `true` → 一颗金色五角星 + 「★ 收藏」，`false` → 不显示；标签经
//! `db.tags_of(photo_id)` 只读查询。这里**仍然不提供编辑**（不在查看器里
//! 改星标 / 改标签），编辑入口在网格工具栏，保持 R-2 的原意。

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Rect, Sense, Stroke, TextureHandle, Ui, Vec2, pos2,
    vec2,
};
use picasa_core::{ThumbJob, ThumbService, thumbs::JobKind};
use picasa_db::PhotoRow;

use crate::icons::{self, Icon};
use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bg {
    Black,
    White,
    Grey,
}

impl Bg {
    fn next(self) -> Self {
        match self {
            Bg::Black => Bg::White,
            Bg::White => Bg::Grey,
            Bg::Grey => Bg::Black,
        }
    }

    fn color(self, theme: &Theme) -> Color32 {
        match self {
            Bg::Black => theme.viewer_bg,
            Bg::White => Color32::from_rgb(0xF7, 0xF7, 0xF7),
            Bg::Grey => Color32::from_rgb(0x80, 0x80, 0x80),
        }
    }
}

/// 查看器状态。
pub struct Viewer {
    pub index: usize,
    /// 绝对缩放（1.0 = 100% 像素）。初始为 `fit`。
    pub zoom: f32,
    pub pan: Vec2,
    pub tex: Option<TextureHandle>,
    pub tex_size: Vec2,
    /// 逐级加载的**第一级**：栅格 T0 缩略图，先顶上，等大图到了再换。
    pub preview: Option<(egui::TextureId, Vec2)>,
    /// 已加载大图的照片 id（`None` = 还在等）
    pub loaded_id: Option<i64>,
    pub requested_id: Option<i64>,
    pub bg: Bg,
    pub info_open: bool,
    /// 上次鼠标移动的时间（用于 3 秒后隐藏 chrome）
    pub last_move: f64,
    pub fullscreen: bool,
    /// EXIF 摘要（打开时同步读一次，1~5ms）
    pub exif: picasa_imaging::ExifSummary,
    /// M4：拍摄时间来源标签（EXIF / 文件名 / 文件修改时间）与元数据状态
    pub taken_src: String,
    pub exif_status: String,
    /// 直方图（M4 #11）：纹理加载时算一次。`None` = 还没加载 / 算不出来。
    pub histogram: Option<picasa_imaging::Histogram>,
    /// 并排比较（M4 #11）：与相邻照片并排。`true` 时同时加载 index±1 的纹理。
    pub compare: bool,
    /// 比较对象（右侧图）的纹理与尺寸：(photo_id, texture, size)。
    pub compare_tex: Option<(i64, TextureHandle, Vec2)>,
    /// 已排队等待的比较图 photo_id（防重复排队）。
    pub compare_requested: Option<i64>,
    pub dims: Option<(u32, u32)>,
    /// M4.2 M42-UI-01：当前照片的标签（只读显示），由 `app::draw_viewer` 在
    /// 打开 / 翻页时短锁查 `db.tags_of` 后填入，查看器内不二次拿 Db 锁
    /// （std Mutex 不可重入，见 `sidebar.rs` 启动 hang 127s 的教训）。
    pub tags: Vec<String>,
    /// 已为哪张照片查过标签；`None` 或 != 当前 id 时触发重查（翻页后由
    /// `jump_to` 清空）。
    pub tags_for: Option<i64>,
}

impl Viewer {
    pub fn new(index: usize) -> Self {
        Self {
            index,
            zoom: 1.0,
            pan: Vec2::ZERO,
            tex: None,
            tex_size: Vec2::ZERO,
            preview: None,
            loaded_id: None,
            requested_id: None,
            bg: Bg::Black,
            info_open: true,
            last_move: 0.0,
            fullscreen: false,
            exif: Default::default(),
            taken_src: String::new(),
            exif_status: String::new(),
            histogram: None,
            compare: false,
            compare_tex: None,
            compare_requested: None,
            dims: None,
            tags: Vec::new(),
            tags_for: None,
        }
    }

    /// 换页：重置缩放与平移，并丢掉上一张的纹理（避免「上一张残影」，F2-16）。
    pub fn jump_to(&mut self, index: usize) {
        self.index = index;
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.tex = None;
        self.tex_size = Vec2::ZERO;
        self.preview = None;
        self.loaded_id = None;
        self.requested_id = None;
        self.dims = None;
        self.exif = Default::default();
        self.histogram = None;
        self.compare_tex = None;
        // 换页后当前标签作废，下一帧 app::draw_viewer 重查（M42-UI-01）。
        self.tags.clear();
        self.tags_for = None;
    }

    pub fn zoom_in(&mut self) {
        self.zoom = (self.zoom * 1.25).min(8.0);
    }

    pub fn zoom_out(&mut self) {
        self.zoom = (self.zoom / 1.25).max(0.05);
    }

    pub fn toggle_bg(&mut self) {
        self.bg = self.bg.next();
    }
}

/// 查看器产出的动作。
#[derive(Clone, Debug, PartialEq)]
pub enum ViewerAction {
    Close,
    Reveal,
    Next,
    Prev,
}

/// 打开/换页时确保大图已排队。
pub fn ensure_loaded(viewer: &mut Viewer, row: &PhotoRow, service: &ThumbService) {
    if viewer.requested_id == Some(row.id) || viewer.loaded_id == Some(row.id) {
        return;
    }
    viewer.requested_id = Some(row.id);
    service.request(ThumbJob {
        photo_id: row.id,
        path: row.abs_path(),
        kind: JobKind::Viewer,
    });
    // EXIF 只读探测：100MB 的 EXIF 解析也就几毫秒，同步做即可（换页手感更稳）
    // M4-META-02/04：来源标签 + 读取状态，缺失就明说，不编造。
    match picasa_imaging::probe(&row.abs_path()) {
        Ok(probe) => {
            viewer.exif = probe.summary;
            viewer.dims = if probe.width > 0 {
                Some((probe.width, probe.height))
            } else {
                None
            };
            viewer.taken_src = match row.taken_at_src {
                0 => "EXIF 拍摄时间",
                1 => "文件名",
                _ => "文件修改时间",
            }
            .to_string();
            viewer.exif_status = if viewer.exif.make.is_none()
                && viewer.exif.model.is_none()
                && viewer.exif.taken_at_text.is_none()
            {
                "该文件没有 EXIF 信息".to_string()
            } else {
                "读取成功".to_string()
            };
        }
        Err(e) => {
            viewer.exif = Default::default();
            viewer.dims = None;
            viewer.taken_src = "—".to_string();
            viewer.exif_status = format!("元数据读取失败（{}）", e);
        }
    }
}

pub fn show(
    ui: &mut Ui,
    viewer: &mut Viewer,
    photos: &[PhotoRow],
    theme: &Theme,
    has_focus: bool,
) -> Option<ViewerAction> {
    let mut action = None;
    let rect = ui.max_rect();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, viewer.bg.color(theme));

    let Some(row) = photos.get(viewer.index) else {
        return Some(ViewerAction::Close);
    };

    // ---- 鼠标静止计时（F2-04）----
    let now = ui.input(|i| i.time);
    if ui.input(|i| i.pointer.is_moving()) {
        viewer.last_move = now;
    }
    let idle = now - viewer.last_move;
    let chrome_visible = idle < 3.0 || ui.input(|i| i.pointer.any_down());

    // ---- 居中 + 缩放 ----
    let avail = rect.shrink2(vec2(0.0, 0.0));
    let (iw, ih) = if viewer.tex_size.x > 0.0 {
        (viewer.tex_size.x, viewer.tex_size.y)
    } else {
        (
            row.width.unwrap_or(1000) as f32,
            row.height.unwrap_or(700) as f32,
        )
    };
    let fit = (avail.width() / iw).min(avail.height() / ih).min(1.0);
    let fit = if fit.is_finite() && fit > 0.0 {
        fit
    } else {
        1.0
    };
    if viewer.zoom == 1.0 && viewer.loaded_id.is_none() {
        // 首次进来：先按「适应窗口」显示
        viewer.zoom = fit;
    }

    // ---- 并排比较（M4 #11）：主图占左半屏，比较图占右半屏 ----
    let avail = if viewer.compare && viewer.compare_tex.is_some() {
        Rect::from_min_size(
            avail.left_top(),
            vec2(avail.width() / 2.0 - 4.0, avail.height()),
        )
    } else {
        avail
    };
    let size = vec2(iw * viewer.zoom, ih * viewer.zoom);
    let mut center = rect.center() + viewer.pan;
    if viewer.compare && viewer.compare_tex.is_some() {
        // 主图中心移到左半屏中心
        center.x = avail.center().x;
    }
    // 图片比视口小就居中（不让人把它拖出屏幕）
    if size.x <= avail.width() {
        center.x = avail.center().x;
        viewer.pan.x = 0.0;
    }
    if size.y <= avail.height() {
        center.y = avail.center().y;
        viewer.pan.y = 0.0;
    }
    let img_rect = Rect::from_center_size(center, size);

    // ---- 图：**逐级加载** ----
    //
    // ① 大图（2560）就绪 → 画它；
    // ② 大图还在解码、但栅格 T0 缩略图在缓存里 → 先画缩略图当预览（立刻有画面，不是白框）；
    // ③ 两者都没有 → 才显示「正在解码…」。
    //
    // 为什么值得单列一级：24MP 的 JPEG 全解码在弱机上要 200~400ms，这段时间里
    // 用户看到的是纯黑 + 一行字。而缩略图早就有了，先顶上能把「打开大图」的感知延迟压到接近 0。
    if let Some(tex) = viewer.tex.as_ref() {
        painter.image(
            tex.id(),
            img_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else if let Some((tex_id, psize)) = viewer.preview {
        // 占位（compare 模式下同样生效）
        // 预览按自身比例贴合（缩略图与大图比例一致，但用自身尺寸更稳）
        let prect = fit_rect(img_rect, psize);
        painter.image(
            tex_id,
            prect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        painter.text(
            pos2(rect.center().x, rect.bottom() - 52.0),
            Align2::CENTER_CENTER,
            "正在载入原图…",
            FontId::new(11.5, egui::FontFamily::Proportional),
            Color32::from_white_alpha(140),
        );
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "正在解码…",
            FontId::new(13.0, egui::FontFamily::Proportional),
            Color32::from_white_alpha(160),
        );
    }

    // ---- 并排比较图（M4 #11）：右半屏 ----
    if viewer.compare {
        let right_rect = Rect::from_min_size(
            pos2(rect.center().x + 4.0, rect.top()),
            vec2(rect.width() / 2.0 - 8.0, rect.height()),
        );
        if let Some((_, tex, csize)) = viewer.compare_tex.as_ref() {
            let crect = fit_rect(right_rect, *csize);
            painter.image(tex.id(), crect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            if let Some(next) = photos.get(viewer.index + 1) {
                painter.text(
                    pos2(crect.center().x, crect.bottom() + 14.0),
                    Align2::CENTER_CENTER,
                    &next.name,
                    FontId::new(11.0, egui::FontFamily::Proportional),
                    Color32::from_white_alpha(150),
                );
            }
        } else {
            painter.text(
                right_rect.center(),
                Align2::CENTER_CENTER,
                "正在载入比较图…",
                FontId::new(12.0, egui::FontFamily::Proportional),
                Color32::from_white_alpha(140),
            );
        }
        // 中缝分隔线
        painter.line_segment(
            [pos2(rect.center().x, rect.top()), pos2(rect.center().x, rect.bottom())],
            Stroke::new(1.0, Color32::from_white_alpha(40)),
        );
    }

    // ---- 拖动平移（F2-03）----
    let drag = ui.interact(
        rect,
        ui.scope_id().with("viewer-canvas"),
        Sense::click_and_drag(),
    );
    if drag.dragged() && viewer.zoom > fit * 1.01 {
        viewer.pan += drag.drag_delta();
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    }

    // ---- 左右翻页热区（U-40）----
    let zone_w = (rect.width() * 0.08).clamp(48.0, 120.0);
    let left_zone = Rect::from_min_max(rect.left_top(), pos2(rect.left() + zone_w, rect.bottom()));
    let right_zone =
        Rect::from_min_max(pos2(rect.right() - zone_w, rect.top()), rect.right_bottom());
    let lresp = ui.interact(left_zone, ui.scope_id().with("viewer-prev"), Sense::click());
    let rresp = ui.interact(
        right_zone,
        ui.scope_id().with("viewer-next"),
        Sense::click(),
    );
    if lresp.clicked() {
        action = Some(ViewerAction::Prev);
    }
    if rresp.clicked() {
        action = Some(ViewerAction::Next);
    }
    if lresp.hovered() || rresp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    if chrome_visible {
        // 半透明大箭头
        let alpha = 90u8;
        if viewer.index > 0 {
            draw_big_chevron(&painter, left_zone.center(), false, alpha);
        }
        if viewer.index + 1 < photos.len() {
            draw_big_chevron(&painter, right_zone.center(), true, alpha);
        }

        // ---- 顶部工具条（U-42：**只有返回与在 Finder 显示**）----
        let top = Rect::from_min_size(rect.left_top(), vec2(rect.width(), 34.0));
        painter.rect_filled(top, 0.0, Color32::from_black_alpha(160));
        let mut x = top.left() + 8.0;
        let back = button(
            &painter,
            &*ui,
            ui.scope_id().with("viewer-back"),
            Rect::from_min_size(pos2(x, top.top() + 5.0), vec2(70.0, 24.0)),
            Icon::Back,
            "返回",
            theme,
        );
        if back.clicked() {
            action = Some(ViewerAction::Close);
        }
        x += 76.0;
        let reveal = button(
            &painter,
            &*ui,
            ui.scope_id().with("viewer-reveal"),
            Rect::from_min_size(pos2(x, top.top() + 5.0), vec2(92.0, 24.0)),
            Icon::Reveal,
            "显示位置",
            theme,
        );
        if reveal.clicked() {
            action = Some(ViewerAction::Reveal);
        }

        // 右侧：文件名（原版在顶部显示文件名）
        painter.text(
            pos2(top.right() - 12.0, top.center().y),
            Align2::RIGHT_CENTER,
            &row.name,
            FontId::new(12.0, egui::FontFamily::Proportional),
            Color32::from_white_alpha(210),
        );

        // ---- 底部信息条（U-41）----
        let bar = Rect::from_min_size(
            pos2(rect.left(), rect.bottom() - 26.0),
            vec2(rect.width(), 26.0),
        );
        painter.rect_filled(bar, 0.0, Color32::from_black_alpha(160));
        let dims = viewer.dims.map(|(w, h)| format!("{w} × {h}")).or_else(|| {
            Some(format!(
                "{} × {}",
                row.width.unwrap_or(0),
                row.height.unwrap_or(0)
            ))
        });
        let left_font = FontId::new(11.5, egui::FontFamily::Proportional);
        let left_text = format!(
            "{}   {}   第 {} / {} 张   {}%",
            row.name,
            dims.unwrap_or_else(|| "—".into()),
            viewer.index + 1,
            photos.len(),
            (viewer.zoom * 100.0).round() as i32
        );
        // 量真实字形宽度，让星标/标签精确接在左侧文本之后（中文/ASCII 混排
        // 用固定字符数估会溢出，沿用 grid.rs 文件名 ellipsize 的同一教训）。
        let left_w = painter
            .layout_no_wrap(left_text.clone(), left_font.clone(), Color32::WHITE)
            .size()
            .x;
        painter.text(
            pos2(bar.left() + 10.0, bar.center().y),
            Align2::LEFT_CENTER,
            left_text,
            left_font,
            Color32::from_white_alpha(215),
        );
        // M42-UI-01：星标 + 标签 chips 接在左侧文本之后（只读显示，不编辑）。
        let right_limit = bar.right() - 10.0;
        let hint_w = if viewer.info_open { 330.0 } else { 0.0 };
        draw_star_and_tags(
            &painter,
            bar,
            row.starred,
            &viewer.tags,
            bar.left() + 10.0 + left_w + 12.0,
            right_limit - hint_w,
            theme,
        );
        if viewer.info_open {
            painter.text(
                pos2(bar.right() - 10.0, bar.center().y),
                Align2::RIGHT_CENTER,
                "I 隐藏信息 · Space 切背景 · C 并排比较 · ←/→ 翻页 · Esc 返回",
                FontId::new(11.0, egui::FontFamily::Proportional),
                Color32::from_white_alpha(150),
            );
        }

        // ---- EXIF 信息面板（F2-09）----
        if viewer.info_open {
            draw_info_panel(&painter, rect, viewer, theme);
        }
    } else {
        // 隐藏光标（F2-04）
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
    }
    if chrome_visible {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
    }

    // 滚轮缩放（F2-02）
    // 查看器里滚轮直接缩放（原版行为）；Ctrl/⌘+滚轮是系统级「放大整屏」，我们不再叠加缩放，
    // 两条分支取值相同，所以直接读 smooth_scroll_delta。
    if has_focus {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.5 && !ui.input(|i| i.modifiers.shift) {
            let factor = 1.0 + scroll * 0.0015;
            viewer.zoom = (viewer.zoom * factor).clamp(0.05, 8.0);
        }
        // 键盘缩放键（+/-/0）在 app 层统一处理，这里只处理滚轮
    }

    action
}

/// 等比放进 `outer`（长边贴合，短边留白，不裁切不拉伸）。
fn fit_rect(outer: Rect, tex: Vec2) -> Rect {
    if tex.x <= 0.0 || tex.y <= 0.0 {
        return outer;
    }
    let scale = (outer.width() / tex.x).min(outer.height() / tex.y);
    let size = vec2(tex.x * scale, tex.y * scale);
    Rect::from_center_size(outer.center(), size)
}

fn draw_big_chevron(painter: &egui::Painter, center: egui::Pos2, right: bool, alpha: u8) {
    let color = Color32::from_white_alpha(alpha);
    let s = Stroke::new(3.0, color);
    let d = 10.0;
    if right {
        painter.line_segment(
            [
                pos2(center.x - d, center.y - d),
                pos2(center.x + d, center.y),
            ],
            s,
        );
        painter.line_segment(
            [
                pos2(center.x + d, center.y),
                pos2(center.x - d, center.y + d),
            ],
            s,
        );
    } else {
        painter.line_segment(
            [
                pos2(center.x + d, center.y - d),
                pos2(center.x - d, center.y),
            ],
            s,
        );
        painter.line_segment(
            [
                pos2(center.x - d, center.y),
                pos2(center.x + d, center.y + d),
            ],
            s,
        );
    }
}

fn button(
    painter: &egui::Painter,
    ui: &Ui,
    id: egui::Id,
    rect: Rect,
    icon: Icon,
    label: &str,
    _theme: &Theme,
) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click());
    if resp.hovered() {
        painter.rect_filled(rect, 3.0, Color32::from_white_alpha(28));
    }
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 12.0, rect.center().y), vec2(13.0, 13.0));
    icons::draw(
        painter,
        icon,
        icon_rect,
        Color32::from_white_alpha(220),
        1.4,
    );
    painter.text(
        pos2(rect.left() + 22.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::new(11.5, egui::FontFamily::Proportional),
        Color32::from_white_alpha(220),
    );
    resp
}

/// M4.2 M42-UI-01：底部信息条里的「星标 + 标签 chips」只读显示。
///
/// `starred` 直接来自 `PhotoRow`（数据层只有 0/1 收藏态，无 1~5 评分列，
/// R-M42-2 不新增字段）：`true` → 一颗金色五角星 + 「★ 收藏」文字，
/// `false` → 不画星。标签用 `painter.layout_no_wrap` 量真实字形宽度画圆角
/// chips；`max_x` 之后放不下的收成「+N」，避免与右侧快捷键提示/文件名重叠。
/// 全程不查库（标签已由调用方短锁预填到 `viewer.tags`）、不引入重绘。
fn draw_star_and_tags(
    painter: &egui::Painter,
    bar: Rect,
    starred: bool,
    tags: &[String],
    start_x: f32,
    max_x: f32,
    theme: &Theme,
) {
    let font = FontId::new(11.0, egui::FontFamily::Proportional);
    let mut x = start_x;
    let cy = bar.center().y;
    if x > max_x {
        return;
    }

    if starred {
        let r = 6.0;
        let c = pos2(x + r, cy);
        icons::draw_star(painter, c, r, Color32::GOLD);
        x += r * 2.0 + 3.0;
        let galley = painter.layout_no_wrap(
            "收藏".to_owned(),
            font.clone(),
            Color32::from_white_alpha(215),
        );
        painter.galley(pos2(x, cy - galley.size().y / 2.0), galley, Color32::WHITE);
        x += 26.0 + 8.0;
    }

    let chip_color = Color32::from_white_alpha(40);
    let text_color = Color32::from_white_alpha(225);
    let mut shown = 0usize;
    for label in tags {
        let galley = painter.layout_no_wrap(label.clone(), font.clone(), text_color);
        let w = galley.size().x + 12.0;
        if x + w > max_x {
            break;
        }
        let chip = Rect::from_min_size(pos2(x, cy - 9.0), vec2(w, 18.0));
        painter.rect_filled(chip, 8.0, chip_color);
        painter.galley(
            pos2(x + 6.0, cy - galley.size().y / 2.0),
            galley,
            Color32::WHITE,
        );
        x += w + 4.0;
        shown += 1;
    }
    if shown < tags.len() {
        let more = format!("+{}", tags.len() - shown);
        let galley = painter.layout_no_wrap(more, font, text_color);
        painter.galley(
            pos2(x, cy - galley.size().y / 2.0),
            galley,
            Color32::WHITE,
        );
    }
    // theme 参与签名是为了让样式基线（v1.8）可在后续统一收口；当前 chips
    // 用半透明白而非新增 token，避免破坏已锁死的 M4.1 v1.8 视觉基线。
    let _ = theme;
}

fn draw_info_panel(painter: &egui::Painter, rect: Rect, viewer: &Viewer, theme: &Theme) {
    let w = 250.0;
    let hist_h = if viewer.histogram.is_some() { 92.0 } else { 0.0 };
    let panel = Rect::from_min_size(
        pos2(rect.right() - w - 10.0, rect.top() + 44.0),
        vec2(w, 210.0 + hist_h),
    );
    painter.rect_filled(panel, 4.0, Color32::from_black_alpha(190));
    painter.rect_stroke(
        panel,
        4.0,
        Stroke::new(1.0, Color32::from_white_alpha(30)),
        egui::StrokeKind::Inside,
    );

    // ---- 直方图（M4 #11）----
    if let Some(hist) = &viewer.histogram {
        draw_histogram(painter, panel, hist);
    }

    let e = &viewer.exif;
    let rows: Vec<(&str, Option<String>)> = vec![
        ("相机", join_opt(&e.make, &e.model)),
        ("镜头", e.lens.clone()),
        ("焦距", e.focal_length.clone()),
        ("光圈", e.f_number.clone()),
        ("快门", e.exposure_time.clone()),
        ("ISO", e.iso.clone()),
        ("拍摄时间", e.taken_at_text.clone()),
        ("时间来源", Some(viewer.taken_src.clone())),
        ("色彩空间", e.color_space.clone()),
        ("位置", e.gps.clone()),
        ("元数据", Some(viewer.exif_status.clone())),
    ];

    let mut y = panel.top() + 10.0 + hist_h;
    painter.text(
        pos2(panel.left() + 12.0, y),
        Align2::LEFT_TOP,
        "照片信息",
        FontId::new(11.5, egui::FontFamily::Proportional),
        Color32::from_white_alpha(170),
    );
    y += 20.0;
    for (label, value) in rows {
        painter.text(
            pos2(panel.left() + 12.0, y),
            Align2::LEFT_TOP,
            label,
            FontId::new(11.0, egui::FontFamily::Proportional),
            theme.viewer_text.gamma_multiply(0.6),
        );
        painter.text(
            pos2(panel.left() + 80.0, y),
            Align2::LEFT_TOP,
            value.unwrap_or_else(|| "—".to_string()),
            FontId::new(11.0, egui::FontFamily::Proportional),
            theme.viewer_text,
        );
        y += 19.0;
    }
}

fn join_opt(a: &Option<String>, b: &Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (Some(a), None) => Some(a.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    }
}


/// 画 RGB 直方图：三条半透明折线叠在半透明底上（92px 高，占面板宽减 24px 边距）。
fn draw_histogram(painter: &egui::Painter, panel: Rect, hist: &picasa_imaging::Histogram) {
    let inset = 10.0;
    let bar = Rect::from_min_size(
        pos2(panel.left() + inset, panel.top() + 8.0),
        vec2(panel.width() - inset * 2.0, 76.0),
    );
    painter.rect_filled(bar, 2.0, Color32::from_black_alpha(120));

    let channels: [(&[f32; 256], Color32); 3] = [
        (&hist.r, Color32::from_rgba_unmultiplied(255, 80, 80, 140)),
        (&hist.g, Color32::from_rgba_unmultiplied(80, 230, 110, 140)),
        (&hist.b, Color32::from_rgba_unmultiplied(90, 140, 255, 140)),
    ];
    for (data, color) in channels {
        let mut mesh = egui::Mesh::default();
        let n = data.len();
        let base = pos2(bar.left(), bar.bottom());
        mesh.colored_vertex(base, color);
        for (i, v) in data.iter().enumerate() {
            let x = bar.left() + (i as f32 / (n - 1) as f32) * bar.width();
            let y = bar.bottom() - v.clamp(0.0, 1.0) * (bar.height() - 2.0);
            mesh.colored_vertex(pos2(x, y), color);
        }
        // 闭合到右下角
        mesh.colored_vertex(pos2(bar.right(), bar.bottom()), color);
        let count = n as u32 + 2;
        for i in 0..count - 1 {
            mesh.add_triangle(0, i + 1, i + 2);
        }
        painter.add(egui::Shape::Mesh(std::sync::Arc::new(mesh)));
    }
    painter.rect_stroke(
        bar,
        2.0,
        Stroke::new(1.0, Color32::from_white_alpha(30)),
        egui::StrokeKind::Inside,
    );
}
