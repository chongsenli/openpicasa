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
//! 查看器里**没有旋转、没有星标、没有编辑**（R-2）。顶部只保留「返回」与「在 Finder 显示」
//! ——这是 01 U-42 登记过的**有意差异**。

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
    pub dims: Option<(u32, u32)>,
}

impl Viewer {
    pub fn new(index: usize) -> Self {
        Self {
            index,
            zoom: 1.0,
            pan: Vec2::ZERO,
            tex: None,
            tex_size: Vec2::ZERO,
            loaded_id: None,
            requested_id: None,
            bg: Bg::Black,
            info_open: true,
            last_move: 0.0,
            fullscreen: false,
            exif: Default::default(),
            dims: None,
        }
    }

    /// 换页：重置缩放与平移，并丢掉上一张的纹理（避免「上一张残影」，F2-16）。
    pub fn jump_to(&mut self, index: usize) {
        self.index = index;
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.tex = None;
        self.tex_size = Vec2::ZERO;
        self.loaded_id = None;
        self.requested_id = None;
        self.dims = None;
        self.exif = Default::default();
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
    if let Ok(probe) = picasa_imaging::probe(&row.abs_path()) {
        viewer.exif = probe.summary;
        viewer.dims = if probe.width > 0 {
            Some((probe.width, probe.height))
        } else {
            None
        };
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

    let size = vec2(iw * viewer.zoom, ih * viewer.zoom);
    let mut center = rect.center() + viewer.pan;
    // 图片比视口小就居中（不让人把它拖出屏幕）
    if size.x <= avail.width() {
        center.x = rect.center().x;
        viewer.pan.x = 0.0;
    }
    if size.y <= avail.height() {
        center.y = rect.center().y;
        viewer.pan.y = 0.0;
    }
    let img_rect = Rect::from_center_size(center, size);

    // ---- 图（还没加载好时画个提示，不画白帧）----
    if let Some(tex) = viewer.tex.as_ref() {
        painter.image(
            tex.id(),
            img_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
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
        let left_text = format!(
            "{}   {}   第 {} / {} 张   {}%",
            row.name,
            dims.unwrap_or_else(|| "—".into()),
            viewer.index + 1,
            photos.len(),
            (viewer.zoom * 100.0).round() as i32
        );
        painter.text(
            pos2(bar.left() + 10.0, bar.center().y),
            Align2::LEFT_CENTER,
            left_text,
            FontId::new(11.5, egui::FontFamily::Proportional),
            Color32::from_white_alpha(215),
        );
        if viewer.info_open {
            painter.text(
                pos2(bar.right() - 10.0, bar.center().y),
                Align2::RIGHT_CENTER,
                "I 隐藏信息 · Space 切背景 · ←/→ 翻页 · Esc 返回",
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

fn draw_info_panel(painter: &egui::Painter, rect: Rect, viewer: &Viewer, theme: &Theme) {
    let w = 250.0;
    let panel = Rect::from_min_size(
        pos2(rect.right() - w - 10.0, rect.top() + 44.0),
        vec2(w, 210.0),
    );
    painter.rect_filled(panel, 4.0, Color32::from_black_alpha(190));
    painter.rect_stroke(
        panel,
        4.0,
        Stroke::new(1.0, Color32::from_white_alpha(30)),
        egui::StrokeKind::Inside,
    );

    let e = &viewer.exif;
    let rows: Vec<(&str, Option<String>)> = vec![
        ("相机", join_opt(&e.make, &e.model)),
        ("镜头", e.lens.clone()),
        ("焦距", e.focal_length.clone()),
        ("光圈", e.f_number.clone()),
        ("快门", e.exposure_time.clone()),
        ("ISO", e.iso.clone()),
        ("拍摄时间", e.taken_at_text.clone()),
        ("色彩空间", e.color_space.clone()),
        ("位置", e.gps.clone()),
    ];

    let mut y = panel.top() + 10.0;
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
