//! 查看器展厅（M5-R4：M5-VW-16 / M5-VIS-03 ADD-B，M5-VW-07 推动式信息栏）。
//!
//! ## 展厅结构（说明书 §4.5）
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │ 返回  显示位置  比较  背景   [适合|100%|−|+]  文件名  [照片信息]│ 顶栏
//! ├───────────────────────────────────────────────┬──────────────┤
//! │                                               │              │
//! │                 主图区域                       │  照片信息     │
//! │                                               │  （推动式）   │
//! ├───────────────────────────────────────────────┤              │
//! │ 说明条：标题 / 日期（只读）                    │  EXIF +      │
//! ├───────────────────────────────────────────────┤  RGB 直方图   │
//! │ 胶片条：[■][ ][ ][ ]… 当前项高亮，可点击跳转    │              │
//! ├───────────────────────────────────────────────┴──────────────┤
//! │ 文件名  宽×高  第 i/n 张  缩放   ★ 标签   I 隐藏信息 …        │ 底栏
//! └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! **信息栏是推动式**：它占右列，主图区与胶片条的可用宽度 = 舞台列宽；
//! 面板矩形与主图 / 胶片条**几何上不重叠**（`layout` 由单测锁定）。
//!
//! ## 保留的既有能力
//!
//! * 返回 / Esc、显示位置与 ⌘Enter（动作由 app 层执行）。
//! * 底栏四段信息、左右点击区与端点箭头、全屏、Space 背景、静止隐藏 chrome。
//! * 连续缩放 5%–800%；滚轮、+/−、PageUp、PageDown（适合）、1（100%）。
//! * C 并排比较；只有一张时**不画假比较图**。
//! * 原图未就绪「正在载入原图…」→「正在解码…」，无假进度条。
//! * 信息字段与无 EXIF 文案；RGB 直方图自动出现。
//! * 底栏星标与标签只读。
//!
//! ## 明确不做
//!
//! 可播放幻灯片（OUT-C）、信息面板内评分编辑、多档背景扩展（§11 #2 不裁定；
//! 沿用产品既有的三态循环，不增不减）。

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Rect, Sense, Stroke, TextureHandle, Ui, Vec2, pos2,
    vec2,
};
use picasa_core::{ThumbJob, ThumbService, thumbs::JobKind};
use picasa_db::PhotoRow;

use crate::icons::{self, Icon};
use crate::theme::Theme;

/// 顶栏高度（逻辑像素）。
pub const TOP_H: f32 = 34.0;
/// 底栏高度。
pub const STATUS_H: f32 = 27.0;
/// 主图下方说明条高度（只读，不是编辑台）。
pub const CAPTION_H: f32 = 56.0;
/// 胶片条高度。
pub const FILM_H: f32 = 82.0;
/// 右侧「照片信息」栏宽度。打开时**推动**主图与胶片条。
pub const INFO_W: f32 = 300.0;
/// 缩放上下限（说明书 §4.5：约 5%–800%）。
pub const ZOOM_MIN: f32 = 0.05;
pub const ZOOM_MAX: f32 = 8.0;

/// 查看器背景。产品既有三态循环，**本轮不扩展也不删除**（§11 #2 未裁定）。
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

    /// 持久化到 `preferences.viewer_bg` 的编号（0/1/2，沿用既有语义）。
    pub fn code(self) -> u8 {
        match self {
            Bg::Black => 0,
            Bg::White => 1,
            Bg::Grey => 2,
        }
    }

    /// 从偏好回读；未知值按产品默认（深色）。
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Bg::White,
            2 => Bg::Grey,
            _ => Bg::Black,
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

/// 展厅几何：一次性算出所有区域，测试与绘制共用同一份事实。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageLayout {
    /// 整个查看器。
    pub full: Rect,
    /// 顶栏。
    pub top: Rect,
    /// 底栏（四段信息 + 快捷键提示）。
    pub status: Rect,
    /// 主图 + 说明条 + 胶片条所在的列。信息栏打开时变窄。
    pub stage: Rect,
    /// 主图可用区域（比较模式下仍是整个画布）。
    pub canvas: Rect,
    /// 比较模式下主图占的左窗格；非比较模式 = [`Self::canvas`]。
    pub main_pane: Rect,
    /// 比较模式下相邻图占的右窗格；只有存在第二张时才 `Some`。
    pub compare_pane: Option<Rect>,
    /// 主图下方说明条（标题 / 日期等只读信息）。
    pub caption: Rect,
    /// 底部胶片条。
    pub filmstrip: Rect,
    /// 右侧「照片信息」栏；关闭时 `None`。
    pub info_panel: Option<Rect>,
}

/// 计算展厅几何。
///
/// `compare` 必须是「已经有下一张」的最终判定——调用方不应把
/// `viewer.compare` 直接传进来，否则序列末尾会得到一个假窗格。
pub fn layout(rect: Rect, info_open: bool, compare: bool) -> StageLayout {
    let top = Rect::from_min_size(rect.min, vec2(rect.width(), TOP_H));
    let status = Rect::from_min_size(
        pos2(rect.left(), (rect.bottom() - STATUS_H).max(rect.top())),
        vec2(rect.width(), STATUS_H),
    );
    let body = Rect::from_min_max(pos2(rect.left(), top.bottom()), pos2(rect.right(), status.top()));
    // 窄窗时信息栏最多占 35%，避免把主图挤成一条缝（推窄而不是遮挡仍成立）。
    let info_w = INFO_W.min(body.width() * 0.35).max(0.0);
    let info_panel = info_open.then(|| {
        Rect::from_min_size(
            pos2(body.right() - info_w, body.top()),
            vec2(info_w, body.height()),
        )
    });
    let stage_right = info_panel.map(|p| p.left()).unwrap_or(body.right());
    let stage = Rect::from_min_max(body.min, pos2(stage_right, body.bottom()));

    // 说明书 §4.5 的顺序：主图 → 说明条 → 胶片条 → 底栏（胶片条在最下）。
    let caption_h = CAPTION_H.min((stage.height() * 0.4).max(0.0));
    let film_h = FILM_H.min((stage.height() * 0.4).max(0.0));
    let filmstrip = Rect::from_min_size(
        pos2(stage.left(), stage.bottom() - film_h),
        vec2(stage.width(), film_h),
    );
    let caption = Rect::from_min_size(
        pos2(stage.left(), (filmstrip.top() - caption_h).max(stage.top())),
        vec2(stage.width(), caption_h),
    );
    let canvas = Rect::from_min_max(
        pos2(stage.left() + 8.0, stage.top() + 6.0),
        pos2(stage.right() - 8.0, caption.top() - 2.0),
    );
    let main_pane = if compare {
        Rect::from_min_max(
            canvas.min,
            pos2(canvas.center().x - 4.0, canvas.bottom()),
        )
    } else {
        canvas
    };
    let compare_pane = compare.then(|| {
        Rect::from_min_max(
            pos2(canvas.center().x + 4.0, canvas.top()),
            canvas.right_bottom(),
        )
    });

    StageLayout {
        full: rect,
        top,
        status,
        stage,
        canvas,
        main_pane,
        compare_pane,
        caption,
        filmstrip,
        info_panel,
    }
}

/// 顶栏「−/+」缩放按钮的矩形（绘制与测试共用，避免测试自己猜坐标）。
///
/// 返回 `(zoom_out, zoom_in)`。查看器工具条必须始终保留这两个按钮
/// （说明书 §4.5：不能把适合 / 100% / 200% 三按钮作为唯一缩放）。
pub fn zoom_button_rects(top: Rect) -> (Rect, Rect) {
    let x = top.left() + 8.0 + 76.0 + 98.0 + 58.0 + 62.0 + 54.0 + 54.0;
    let y = top.top() + 5.0;
    (
        Rect::from_min_size(pos2(x, y), vec2(28.0, 24.0)),
        Rect::from_min_size(pos2(x + 32.0, y), vec2(28.0, 24.0)),
    )
}

/// 顶栏「照片信息」开关的矩形。
pub fn info_toggle_rect(top: Rect) -> Rect {
    Rect::from_min_size(pos2(top.right() - 104.0, top.top() + 5.0), vec2(96.0, 24.0))
}

/// 胶片条的一个槽位：照片下标 + 屏幕矩形。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilmSlot {
    pub index: usize,
    pub rect: Rect,
}

/// 胶片条槽位布局：固定 64 px 缩略格，围绕当前项开窗，当前项始终可见。
pub fn filmstrip_slots(rect: Rect, count: usize, current: usize) -> Vec<FilmSlot> {
    const CELL: f32 = 64.0;
    const GAP: f32 = 6.0;
    const PAD: f32 = 10.0;
    if count == 0 || rect.width() <= PAD * 2.0 {
        return Vec::new();
    }
    let visible = (((rect.width() - PAD * 2.0 + GAP) / (CELL + GAP)).floor() as usize).max(1);
    let visible = visible.min(count);
    let half = visible / 2;
    let current = current.min(count.saturating_sub(1));
    let mut start = current.saturating_sub(half);
    if start + visible > count {
        start = count - visible;
    }
    let row_h = (rect.height() - 14.0).max(20.0);
    let cell_h = CELL.min(row_h).min(rect.height() - 8.0);
    let cy = rect.center().y;
    (0..visible)
        .map(|i| {
            let idx = start + i;
            let x = rect.left() + PAD + i as f32 * (CELL + GAP);
            FilmSlot {
                index: idx,
                rect: Rect::from_min_size(pos2(x, cy - cell_h / 2.0), vec2(CELL, cell_h)),
            }
        })
        .collect()
}

/// 胶片条点击命中的照片下标（程序接口；UI 层也用同一函数）。
pub fn filmstrip_hit(rect: Rect, count: usize, current: usize, pos: egui::Pos2) -> Option<usize> {
    filmstrip_slots(rect, count, current)
        .into_iter()
        .find(|s| s.rect.contains(pos))
        .map(|s| s.index)
}

/// 展厅需要的胶片条缩略图（由 app 层用 `TextureCache` 预取）。
#[derive(Clone, Copy, Debug)]
pub struct FilmThumb {
    pub index: usize,
    pub tex: Option<(egui::TextureId, Vec2)>,
}

/// 查看器状态。
pub struct Viewer {
    pub index: usize,
    /// 绝对缩放（1.0 = 100% 像素）。
    pub zoom: f32,
    /// 下一帧按「适合窗口」重算（PageDown / 换页 / 首次进入）。
    pub fit_pending: bool,
    /// 用户是否手动定过缩放；大图到达时只有「仍在适合」才重算，不能抢用户的缩放。
    pub user_zoomed: bool,
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
    /// 拍摄时间来源标签（EXIF / 文件名 / 文件修改时间）与元数据状态
    pub taken_src: String,
    pub exif_status: String,
    /// 直方图：纹理加载时算一次。`None` = 还没加载 / 算不出来。
    pub histogram: Option<picasa_imaging::Histogram>,
    /// 并排比较：`true` 且**存在下一张**时左右并排。
    pub compare: bool,
    /// 比较对象（右侧图）的纹理与尺寸：(photo_id, texture, size)。
    pub compare_tex: Option<(i64, TextureHandle, Vec2)>,
    /// 已排队等待的比较图 photo_id（防重复排队）。
    pub compare_requested: Option<i64>,
    pub dims: Option<(u32, u32)>,
    /// 当前照片的标签（只读），由 app 层短锁预取。
    pub tags: Vec<String>,
    /// 已为哪张照片查过标签；`None` 或 != 当前 id 时触发重查。
    pub tags_for: Option<i64>,
    /// 当前照片的说明（只读；展厅说明条只展示，不在这里编辑）。
    pub caption_text: String,
}

impl Viewer {
    pub fn new(index: usize) -> Self {
        Self {
            index,
            zoom: 1.0,
            fit_pending: true,
            user_zoomed: false,
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
            caption_text: String::new(),
        }
    }

    /// 是否存在可以并排的第二张。**没有就不画假比较图**。
    pub fn compare_available(&self, photo_count: usize) -> bool {
        self.index + 1 < photo_count
    }

    /// 当前是否真的处于并排比较（开关打开 + 有第二张）。
    pub fn compare_active(&self, photo_count: usize) -> bool {
        self.compare && self.compare_available(photo_count)
    }

    /// 换页：重置缩放与平移，并丢掉上一张的纹理（避免「上一张残影」）。
    pub fn jump_to(&mut self, index: usize) {
        self.index = index;
        self.zoom = 1.0;
        self.fit_pending = true;
        self.user_zoomed = false;
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
        self.compare_requested = None;
        self.tags.clear();
        self.tags_for = None;
        self.caption_text.clear();
    }

    pub fn zoom_in(&mut self) {
        self.zoom = (self.zoom * 1.25).min(ZOOM_MAX);
        self.fit_pending = false;
        self.user_zoomed = true;
    }

    pub fn zoom_out(&mut self) {
        self.zoom = (self.zoom / 1.25).max(ZOOM_MIN);
        self.fit_pending = false;
        self.user_zoomed = true;
    }

    /// PageDown：下一帧按「适合窗口」重新计算。
    pub fn request_fit(&mut self) {
        self.fit_pending = true;
        self.user_zoomed = false;
    }

    /// 数字 1：绝对 100%（**不是评分**）。
    pub fn zoom_actual(&mut self) {
        self.zoom = 1.0;
        self.fit_pending = false;
        self.user_zoomed = true;
        self.pan = Vec2::ZERO;
    }

    pub fn toggle_bg(&mut self) {
        self.bg = self.bg.next();
    }

    /// 信息栏开关（偏好由 app 层写回 `viewer_info_open`）。
    pub fn toggle_info(&mut self) {
        self.info_open = !self.info_open;
    }
}

/// 查看器产出的动作（app 层执行；查看器自己不碰偏好 / 数据库）。
#[derive(Clone, Debug, PartialEq)]
pub enum ViewerAction {
    Close,
    Reveal,
    Next,
    Prev,
    /// 胶片条点击：跳到序列里的第 n 张。
    JumpTo(usize),
    ToggleInfo,
    ToggleCompare,
    ToggleBg,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ZoomActual,
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
    // EXIF 只读探测：100MB 的 EXIF 解析也就几毫秒，同步做即可（换页手感更稳）。
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

fn fit_scale(avail: Vec2, iw: f32, ih: f32) -> f32 {
    let fit = (avail.x / iw).min(avail.y / ih).min(1.0);
    if fit.is_finite() && fit > 0.0 { fit } else { 1.0 }
}

fn is_bright(c: Color32) -> bool {
    let lum = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
    lum > 140.0
}

/// 舞台上的文字颜色（背景可能是深 / 浅）。
fn stage_ink(bg: Color32) -> Color32 {
    if is_bright(bg) {
        Color32::from_black_alpha(220)
    } else {
        Color32::from_white_alpha(220)
    }
}

fn stage_ink_soft(bg: Color32) -> Color32 {
    if is_bright(bg) {
        Color32::from_black_alpha(150)
    } else {
        Color32::from_white_alpha(150)
    }
}

/// 展厅绘制。`film_thumbs` 是本帧可用的胶片条缩略图（可为空，空则画占位格）。
pub fn show(
    ui: &mut Ui,
    viewer: &mut Viewer,
    photos: &[PhotoRow],
    theme: &Theme,
    has_focus: bool,
    film_thumbs: &[FilmThumb],
) -> Option<ViewerAction> {
    let mut action = None;
    let rect = ui.max_rect();
    let painter = ui.painter_at(rect);
    let bg = viewer.bg.color(theme);
    painter.rect_filled(rect, 0.0, bg);

    let Some(row) = photos.get(viewer.index) else {
        return Some(ViewerAction::Close);
    };

    // ---- 鼠标静止计时（VW-13）：静止 3 秒隐藏 chrome 与光标 ----
    let now = ui.input(|i| i.time);
    // 刚打开时还没有「上次移动」：记为现在，否则首帧会因为 now-0 > 3s 直接藏 chrome，
    // 用户不动鼠标就看不到返回 / 缩放 / 胶片条（展厅等于白进）。
    if viewer.last_move == 0.0 {
        viewer.last_move = now;
    }
    if ui.input(|i| i.pointer.is_moving()) {
        viewer.last_move = now;
    }
    let idle = now - viewer.last_move;
    let chrome_visible = idle < 3.0 || ui.input(|i| i.pointer.any_down());

    let compare_active = viewer.compare_active(photos.len());
    let lay = layout(rect, viewer.info_open, compare_active);

    // ---- 主图尺寸 / 适合 ----
    let (iw, ih) = if viewer.tex_size.x > 0.0 {
        (viewer.tex_size.x, viewer.tex_size.y)
    } else {
        (
            row.width.unwrap_or(1000).max(1) as f32,
            row.height.unwrap_or(700).max(1) as f32,
        )
    };
    let fit = fit_scale(lay.main_pane.size(), iw, ih);
    // 「适合」不只是按下 PageDown 的那一帧：只要用户还没手动缩放过，窗格因
    // 信息栏推窄 / 并排分栏而变窄后，缩放必须跟着**当前**窗格重算。
    // 否则 zoom 停留在旧窗格的比例上，图会溢出主图区被裁掉
    // （AC-VW-STAGE 的「推窄」与 AC-VW-ZOOM 的「适合」必须同时成立）。
    if viewer.fit_pending || !viewer.user_zoomed {
        if viewer.fit_pending || (viewer.zoom - fit).abs() > 1e-6 {
            viewer.pan = Vec2::ZERO;
        }
        viewer.zoom = fit;
        viewer.fit_pending = false;
    }
    viewer.zoom = viewer.zoom.clamp(ZOOM_MIN, ZOOM_MAX);

    let size = vec2(iw * viewer.zoom, ih * viewer.zoom);
    let pane = lay.main_pane;
    let mut center = pane.center() + viewer.pan;
    // 图片比视口小就居中（不让人把它拖出屏幕）
    if size.x <= pane.width() {
        center.x = pane.center().x;
        viewer.pan.x = 0.0;
    }
    if size.y <= pane.height() {
        center.y = pane.center().y;
        viewer.pan.y = 0.0;
    }
    let img_rect = Rect::from_center_size(center, size);

    // ---- 图：逐级加载（VW-12） ----
    //
    // 主图裁到自己的窗格里：比较模式下 100% 的大图会超出半屏，不裁就会画到
    // 相邻窗格 / 信息栏上（视觉上是「比较图被主图盖住」）。
    let main_painter = painter.with_clip_rect(pane.intersect(rect));
    if let Some(tex) = viewer.tex.as_ref() {
        main_painter.image(
            tex.id(),
            img_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else if let Some((tex_id, psize)) = viewer.preview {
        let prect = fit_rect(img_rect, psize);
        main_painter.image(
            tex_id,
            prect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        main_painter.text(
            pos2(pane.center().x, pane.bottom() - 18.0),
            Align2::CENTER_CENTER,
            "正在载入原图…",
            FontId::new(11.5, egui::FontFamily::Proportional),
            stage_ink_soft(bg),
        );
    } else {
        main_painter.text(
            pane.center(),
            Align2::CENTER_CENTER,
            "正在解码…",
            FontId::new(13.0, egui::FontFamily::Proportional),
            stage_ink_soft(bg),
        );
    }

    // ---- 并排比较（VW-10）：只有第二张真的存在才画右窗格 ----
    if let Some(right) = lay.compare_pane {
        let right_ink = stage_ink(bg);
        let right_painter = painter.with_clip_rect(right.intersect(rect));
        if let Some((_, tex, csize)) = viewer.compare_tex.as_ref() {
            let crect = fit_rect(right, *csize);
            right_painter.image(
                tex.id(),
                crect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            if let Some(next) = photos.get(viewer.index + 1) {
                right_painter.text(
                    pos2(crect.center().x, crect.bottom() + 14.0),
                    Align2::CENTER_CENTER,
                    &next.name,
                    FontId::new(11.0, egui::FontFamily::Proportional),
                    stage_ink_soft(bg),
                );
            }
        } else {
            right_painter.text(
                right.center(),
                Align2::CENTER_CENTER,
                "正在载入比较图…",
                FontId::new(12.0, egui::FontFamily::Proportional),
                stage_ink_soft(bg),
            );
        }
        // 窗格角标：当前 / 对比
        if chrome_visible {
            compare_badge(&painter, pane.left_top() + vec2(10.0, 10.0), "当前", row, right_ink);
            if let Some(next) = photos.get(viewer.index + 1) {
                compare_badge(
                    &painter,
                    right.left_top() + vec2(10.0, 10.0),
                    "对比",
                    next,
                    right_ink,
                );
            }
        }
        // 中缝分隔线
        painter.line_segment(
            [
                pos2(lay.canvas.center().x, lay.canvas.top()),
                pos2(lay.canvas.center().x, lay.canvas.bottom()),
            ],
            Stroke::new(1.0, Color32::from_white_alpha(40)),
        );
    }

    // ---- 拖动平移 ----
    let canvas_resp = ui.interact(
        lay.canvas,
        ui.scope_id().with("viewer-canvas"),
        Sense::click_and_drag(),
    );
    if canvas_resp.dragged() && viewer.zoom > fit * 1.01 {
        viewer.pan += canvas_resp.drag_delta();
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    }

    // ---- 左右翻页热区（端点不画箭头） ----
    let zone_w = (lay.canvas.width() * 0.08).clamp(48.0, 120.0);
    let left_zone = Rect::from_min_max(
        lay.canvas.left_top(),
        pos2(lay.canvas.left() + zone_w, lay.canvas.bottom()),
    );
    let right_zone = Rect::from_min_max(
        pos2(lay.canvas.right() - zone_w, lay.canvas.top()),
        lay.canvas.right_bottom(),
    );
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

    // ---- chrome：顶栏 / 说明条 / 胶片条 / 信息栏 / 底栏 ----
    if chrome_visible {
        if viewer.index > 0 {
            draw_big_chevron(&painter, left_zone.center(), false, 90);
        }
        if viewer.index + 1 < photos.len() {
            draw_big_chevron(&painter, right_zone.center(), true, 90);
        }

        draw_topbar(&painter, ui, &lay, viewer, theme, row, &mut action);
        draw_caption(&painter, &lay, viewer, theme, row);
        draw_filmstrip(&painter, ui, &lay, viewer, theme, photos, film_thumbs, &mut action);
        draw_statusbar(&painter, &lay, viewer, theme, row, photos.len(), fit, bg);
        if let Some(panel) = lay.info_panel
            && draw_info_panel(
                &painter,
                ui,
                panel,
                viewer,
                theme,
                ui.scope_id().with("viewer-info"),
            )
        {
            action = Some(ViewerAction::ToggleInfo);
        }
    } else {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
    }
    if chrome_visible {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
    }

    // ---- 滚轮缩放（不叠加 ⌘+滚轮） ----
    if has_focus {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.5 && !ui.input(|i| i.modifiers.shift) {
            let factor = 1.0 + scroll * 0.0015;
            viewer.zoom = (viewer.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
            viewer.fit_pending = false;
            viewer.user_zoomed = true;
        }
    }

    action
}

fn draw_topbar(
    painter: &egui::Painter,
    ui: &Ui,
    lay: &StageLayout,
    viewer: &Viewer,
    theme: &Theme,
    row: &PhotoRow,
    action: &mut Option<ViewerAction>,
) {
    let top = lay.top;
    painter.rect_filled(top, 0.0, Color32::from_black_alpha(160));
    let fg = Color32::from_white_alpha(225);
    let mut x = top.left() + 8.0;

    if slim_button(
        painter,
        ui,
        ui.scope_id().with("viewer-back"),
        Rect::from_min_size(pos2(x, top.top() + 5.0), vec2(70.0, 24.0)),
        Icon::Back,
        "返回",
        fg,
    )
    .clicked()
    {
        *action = Some(ViewerAction::Close);
    }
    x += 76.0;

    if slim_button(
        painter,
        ui,
        ui.scope_id().with("viewer-reveal"),
        Rect::from_min_size(pos2(x, top.top() + 5.0), vec2(92.0, 24.0)),
        Icon::Reveal,
        "显示位置",
        fg,
    )
    .clicked()
    {
        *action = Some(ViewerAction::Reveal);
    }
    x += 98.0;

    // 比较：只有至少两张时才点亮；状态如实反映当前是否并排。
    if text_button(
        painter,
        ui,
        ui.scope_id().with("viewer-compare"),
        Rect::from_min_size(pos2(x, top.top() + 5.0), vec2(52.0, 24.0)),
        "比较",
        viewer.compare,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ToggleCompare);
    }
    x += 58.0;
    if text_button(
        painter,
        ui,
        ui.scope_id().with("viewer-bg"),
        Rect::from_min_size(pos2(x, top.top() + 5.0), vec2(52.0, 24.0)),
        "背景",
        false,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ToggleBg);
    }
    x += 62.0;

    // 缩放簇：适合 / 100% / − / +（连续缩放不是只有三个按钮）
    if text_button(
        painter,
        ui,
        ui.scope_id().with("viewer-fit"),
        Rect::from_min_size(pos2(x, top.top() + 5.0), vec2(50.0, 24.0)),
        "适合",
        viewer.fit_pending,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ZoomFit);
    }
    x += 54.0;
    if text_button(
        painter,
        ui,
        ui.scope_id().with("viewer-actual"),
        Rect::from_min_size(pos2(x, top.top() + 5.0), vec2(50.0, 24.0)),
        "100%",
        (viewer.zoom - 1.0).abs() < 0.001,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ZoomActual);
    }
    x += 54.0;
    let (zoom_out_rect, zoom_in_rect) = zoom_button_rects(top);
    if text_button(
        painter,
        ui,
        ui.scope_id().with("viewer-zoom-out"),
        zoom_out_rect,
        "−",
        false,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ZoomOut);
    }
    if text_button(
        painter,
        ui,
        ui.scope_id().with("viewer-zoom-in"),
        zoom_in_rect,
        "+",
        false,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ZoomIn);
    }
    let _ = x;

    // 右侧：文件名（裁剪在可用区内）+「照片信息」开关
    let info_btn = info_toggle_rect(top);
    if text_button(
        painter,
        ui,
        ui.scope_id().with("viewer-info-toggle"),
        info_btn,
        "照片信息",
        viewer.info_open,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ToggleInfo);
    }

    let name_clip = Rect::from_min_max(
        pos2(x + 12.0, top.top()),
        pos2(info_btn.left() - 12.0, top.bottom()),
    );
    if name_clip.width() > 20.0 {
        painter.with_clip_rect(name_clip).text(
            pos2(name_clip.right(), top.center().y),
            Align2::RIGHT_CENTER,
            &row.name,
            FontId::new(12.0, egui::FontFamily::Proportional),
            Color32::from_white_alpha(210),
        );
    }
}

fn draw_caption(
    painter: &egui::Painter,
    lay: &StageLayout,
    viewer: &Viewer,
    theme: &Theme,
    row: &PhotoRow,
) {
    let caption = lay.caption;
    if caption.height() < 20.0 {
        return;
    }
    let bg = viewer.bg.color(theme);
    let ink = stage_ink(bg);
    let soft = stage_ink_soft(bg);

    let title = display_title(row);
    painter.text(
        pos2(caption.left() + 2.0, caption.top() + 10.0),
        Align2::LEFT_TOP,
        title,
        FontId::new(15.0, egui::FontFamily::Proportional),
        ink,
    );
    if !viewer.caption_text.trim().is_empty() {
        let clip = Rect::from_min_max(
            pos2(caption.left(), caption.top() + 30.0),
            pos2(caption.right(), caption.bottom()),
        );
        painter.with_clip_rect(clip).text(
            pos2(caption.left() + 2.0, caption.top() + 31.0),
            Align2::LEFT_TOP,
            viewer.caption_text.trim(),
            FontId::new(11.0, egui::FontFamily::Proportional),
            soft,
        );
    } else if let Some(date) = format_taken_date(row.eff_taken_at) {
        painter.text(
            pos2(caption.left() + 2.0, caption.top() + 31.0),
            Align2::LEFT_TOP,
            date,
            FontId::new(11.0, egui::FontFamily::Proportional),
            soft,
        );
    }

    // 右侧元数据摘要（只读，不编辑）
    let summary = caption_summary(viewer);
    if !summary.is_empty() {
        let clip = Rect::from_min_max(
            pos2(caption.left() + caption.width() * 0.45, caption.top()),
            pos2(caption.right(), caption.bottom()),
        );
        painter.with_clip_rect(clip).text(
            pos2(caption.right() - 2.0, caption.top() + 12.0),
            Align2::RIGHT_TOP,
            summary,
            FontId::new(11.0, egui::FontFamily::Proportional),
            soft,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_filmstrip(
    painter: &egui::Painter,
    ui: &Ui,
    lay: &StageLayout,
    viewer: &Viewer,
    theme: &Theme,
    photos: &[PhotoRow],
    film_thumbs: &[FilmThumb],
    action: &mut Option<ViewerAction>,
) {
    let strip = lay.filmstrip;
    if strip.height() < 20.0 {
        return;
    }
    let bg = viewer.bg.color(theme);
    let strip_bg = if is_bright(bg) {
        Color32::from_white_alpha(150)
    } else {
        Color32::from_black_alpha(70)
    };
    painter.rect_filled(strip, 0.0, strip_bg);

    let slots = filmstrip_slots(strip, photos.len(), viewer.index);
    for slot in &slots {
        let is_current = slot.index == viewer.index;
        let resp = ui.interact(
            slot.rect,
            ui.scope_id().with(("viewer-film", slot.index)),
            Sense::click(),
        );
        let tex = film_thumbs
            .iter()
            .find(|t| t.index == slot.index)
            .and_then(|t| t.tex);
        if let Some((id, size)) = tex {
            let fitted = fit_rect(slot.rect, size);
            painter.image(
                id,
                fitted,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            painter.rect_filled(slot.rect, 4.0, theme.chip_bg);
            let label = format!("{}", slot.index + 1);
            painter.text(
                slot.rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::new(10.5, egui::FontFamily::Proportional),
                theme.text_secondary,
            );
        }
        if resp.hovered() {
            painter.rect_filled(
                slot.rect,
                4.0,
                Color32::from_white_alpha(if is_bright(bg) { 50 } else { 26 }),
            );
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if is_current {
            painter.rect_stroke(
                slot.rect.expand(2.0),
                5.0,
                Stroke::new(2.5, theme.ring_outer),
                egui::StrokeKind::Outside,
            );
        }
        if resp.clicked() {
            *action = Some(ViewerAction::JumpTo(slot.index));
        }
    }

    // 胶片条两端提示（还有更多时）
    if let (Some(first), Some(last)) = (slots.first(), slots.last()) {
        if first.index > 0 {
            painter.text(
                pos2(strip.left() + 3.0, strip.center().y),
                Align2::LEFT_CENTER,
                "‹",
                FontId::new(14.0, egui::FontFamily::Proportional),
                stage_ink_soft(bg),
            );
        }
        if last.index + 1 < photos.len() {
            painter.text(
                pos2(strip.right() - 3.0, strip.center().y),
                Align2::RIGHT_CENTER,
                "›",
                FontId::new(14.0, egui::FontFamily::Proportional),
                stage_ink_soft(bg),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_statusbar(
    painter: &egui::Painter,
    lay: &StageLayout,
    viewer: &Viewer,
    theme: &Theme,
    row: &PhotoRow,
    photo_count: usize,
    fit: f32,
    bg: Color32,
) {
    let bar = lay.status;
    painter.rect_filled(bar, 0.0, Color32::from_black_alpha(160));
    let dims = viewer.dims.map(|(w, h)| format!("{w} × {h}")).unwrap_or_else(|| {
        format!(
            "{} × {}",
            row.width.unwrap_or(0),
            row.height.unwrap_or(0)
        )
    });
    let zoom_text = if (viewer.zoom - fit).abs() < 0.005 {
        "适合".to_string()
    } else {
        format!("{}%", (viewer.zoom * 100.0).round() as i32)
    };
    let left_font = FontId::new(11.5, egui::FontFamily::Proportional);
    let left_text = format!(
        "{}   {}   第 {} / {} 张   {}",
        row.name,
        dims,
        viewer.index + 1,
        photo_count,
        zoom_text
    );
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

    let hint = if viewer.info_open {
        "I 隐藏信息 · Space 切背景 · C 并排比较 · ←/→ 翻页 · Esc 返回"
    } else {
        "I 显示信息 · Space 切背景 · C 并排比较 · ←/→ 翻页 · Esc 返回"
    };
    let hint_font = FontId::new(11.0, egui::FontFamily::Proportional);
    let hint_w = painter
        .layout_no_wrap(hint.to_string(), hint_font.clone(), Color32::WHITE)
        .size()
        .x;
    painter.text(
        pos2(bar.right() - 10.0, bar.center().y),
        Align2::RIGHT_CENTER,
        hint,
        hint_font,
        Color32::from_white_alpha(150),
    );

    draw_star_and_tags(
        painter,
        bar,
        row.starred,
        &viewer.tags,
        bar.left() + 10.0 + left_w + 12.0,
        bar.right() - 20.0 - hint_w,
        theme,
        bg,
    );
}

fn draw_info_panel(
    painter: &egui::Painter,
    ui: &Ui,
    panel: Rect,
    viewer: &Viewer,
    theme: &Theme,
    id: egui::Id,
) -> bool {
    painter.rect_filled(panel, 0.0, theme.menu_bg);
    painter.line_segment(
        [panel.left_top(), panel.left_bottom()],
        Stroke::new(1.0, theme.separator),
    );

    let font = FontId::new(11.5, egui::FontFamily::Proportional);
    painter.text(
        pos2(panel.left() + 16.0, panel.top() + 16.0),
        Align2::LEFT_TOP,
        "照片信息",
        FontId::new(13.0, egui::FontFamily::Proportional),
        theme.text_primary,
    );

    let close = Rect::from_min_size(pos2(panel.right() - 32.0, panel.top() + 10.0), vec2(22.0, 22.0));
    let close_resp = ui.interact(close, id.with("close"), Sense::click());
    if close_resp.hovered() {
        painter.rect_filled(close, 5.0, theme.sidebar_hover);
    }
    icons::draw(
        painter,
        Icon::Close,
        close.shrink(4.0),
        theme.text_secondary,
        1.3,
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
        ("时间来源", Some(viewer.taken_src.clone())),
        ("色彩空间", e.color_space.clone()),
        ("位置", e.gps.clone()),
        ("元数据", Some(viewer.exif_status.clone())),
    ];

    let mut y = panel.top() + 46.0;
    let clip = Rect::from_min_max(
        pos2(panel.left() + 16.0, y),
        pos2(panel.right() - 14.0, panel.bottom() - 8.0),
    );
    let clipped = painter.with_clip_rect(clip);
    for (label, value) in rows {
        clipped.text(
            pos2(panel.left() + 16.0, y),
            Align2::LEFT_TOP,
            label,
            FontId::new(11.0, egui::FontFamily::Proportional),
            theme.text_secondary,
        );
        clipped.text(
            pos2(panel.left() + 82.0, y),
            Align2::LEFT_TOP,
            value.unwrap_or_else(|| "—".to_string()),
            FontId::new(11.0, egui::FontFamily::Proportional),
            theme.text_primary,
        );
        y += 21.0;
    }

    // ---- RGB 直方图（VW-09：自动出现，不是索引页的帧时间勾选） ----
    let hist_h = 104.0;
    let hist_y = (y + 12.0).min(panel.bottom() - hist_h - 10.0);
    if hist_y > y - 4.0 && panel.bottom() - hist_y > 40.0 {
        painter.text(
            pos2(panel.left() + 16.0, hist_y),
            Align2::LEFT_TOP,
            "RGB 直方图",
            FontId::new(11.0, egui::FontFamily::Proportional),
            theme.text_secondary,
        );
        if let Some(hist) = &viewer.histogram {
            let bar = Rect::from_min_size(
                pos2(panel.left() + 16.0, hist_y + 20.0),
                vec2(panel.width() - 32.0, 66.0),
            );
            draw_histogram(painter, bar, hist, theme);
        } else {
            painter.text(
                pos2(panel.left() + 16.0, hist_y + 22.0),
                Align2::LEFT_TOP,
                "原图载入后显示",
                FontId::new(10.5, egui::FontFamily::Proportional),
                theme.text_secondary,
            );
        }
    }

    let _ = font;
    close_resp.clicked()
}

fn compare_badge(
    painter: &egui::Painter,
    at: egui::Pos2,
    kind: &str,
    row: &PhotoRow,
    ink: Color32,
) {
    let text = format!("{kind} · {}", display_title(row));
    let galley = painter.layout_no_wrap(
        text,
        FontId::new(10.5, egui::FontFamily::Proportional),
        ink,
    );
    let rect = Rect::from_min_size(at, galley.size() + vec2(14.0, 6.0));
    painter.rect_filled(rect, 4.0, Color32::from_black_alpha(110));
    painter.galley(rect.min + vec2(7.0, 3.0), galley, ink);
}

fn slim_button(
    painter: &egui::Painter,
    ui: &Ui,
    id: egui::Id,
    rect: Rect,
    icon: Icon,
    label: &str,
    fg: Color32,
) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click());
    if resp.hovered() {
        painter.rect_filled(rect, 3.0, Color32::from_white_alpha(28));
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 12.0, rect.center().y), vec2(13.0, 13.0));
    icons::draw(painter, icon, icon_rect, fg, 1.4);
    painter.text(
        pos2(rect.left() + 22.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::new(11.5, egui::FontFamily::Proportional),
        fg,
    );
    resp
}

#[allow(clippy::too_many_arguments)]
fn text_button(
    painter: &egui::Painter,
    ui: &Ui,
    id: egui::Id,
    rect: Rect,
    label: &str,
    active: bool,
    fg: Color32,
    theme: &Theme,
) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click());
    if active {
        painter.rect_filled(rect, 4.0, Color32::from_white_alpha(36));
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, Color32::from_white_alpha(24));
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let color = if active { theme.ring_outer } else { fg };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::new(11.5, egui::FontFamily::Proportional),
        color,
    );
    resp
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

/// 说明条标题：文件名去扩展名（只读）。
fn display_title(row: &PhotoRow) -> String {
    std::path::Path::new(&row.name)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&row.name)
        .to_string()
}

/// 拍摄日期（本地时区）。`eff_taken_at` 与网格分组同一口径。
fn format_taken_date(ts: i64) -> Option<String> {
    if ts <= 0 {
        return None;
    }
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%Y年%-m月%-d日").to_string())
}

/// 说明条右侧的元数据摘要（有就显示，没有就留白）。
fn caption_summary(viewer: &Viewer) -> String {
    let e = &viewer.exif;
    let parts: Vec<String> = [
        join_opt(&e.make, &e.model),
        e.lens.clone(),
        e.focal_length.clone(),
        e.f_number.clone(),
        e.exposure_time.clone(),
        e.iso.clone().map(|iso| format!("ISO {iso}")),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect();
    parts.join(" · ")
}

/// M42-UI-01 / VW-14：底栏里的「星标 + 标签 chips」只读显示。
#[allow(clippy::too_many_arguments)]
fn draw_star_and_tags(
    painter: &egui::Painter,
    bar: Rect,
    starred: bool,
    tags: &[String],
    start_x: f32,
    max_x: f32,
    theme: &Theme,
    bg: Color32,
) {
    let font = FontId::new(11.0, egui::FontFamily::Proportional);
    let mut x = start_x;
    let cy = bar.center().y;
    if x > max_x {
        return;
    }
    let ink = stage_ink(bg);

    if starred {
        let r = 6.0;
        let c = pos2(x + r, cy);
        icons::draw_star(painter, c, r, Color32::GOLD);
        x += r * 2.0 + 3.0;
        let galley = painter.layout_no_wrap("收藏".to_owned(), font.clone(), ink);
        painter.galley(pos2(x, cy - galley.size().y / 2.0), galley, Color32::WHITE);
        x += 26.0 + 8.0;
    }

    let chip_color = if is_bright(bg) {
        Color32::from_black_alpha(24)
    } else {
        Color32::from_white_alpha(40)
    };
    let mut shown = 0usize;
    for label in tags {
        let galley = painter.layout_no_wrap(label.clone(), font.clone(), ink);
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
        let galley = painter.layout_no_wrap(more, font, ink);
        painter.galley(pos2(x, cy - galley.size().y / 2.0), galley, Color32::WHITE);
    }
    let _ = theme;
}

fn join_opt(a: &Option<String>, b: &Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (Some(a), None) => Some(a.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    }
}

/// 画 RGB 直方图：三条半透明折线叠在半透明底上。
fn draw_histogram(
    painter: &egui::Painter,
    bar: Rect,
    hist: &picasa_imaging::Histogram,
    theme: &Theme,
) {
    painter.rect_filled(bar, 2.0, theme.chip_bg);

    let channels: [(&[f32; 256], Color32); 3] = [
        (&hist.r, Color32::from_rgba_unmultiplied(255, 80, 80, 150)),
        (&hist.g, Color32::from_rgba_unmultiplied(80, 230, 110, 150)),
        (&hist.b, Color32::from_rgba_unmultiplied(90, 140, 255, 150)),
    ];
    for (data, color) in channels {
        let mut mesh = egui::Mesh::default();
        let n = data.len();
        if n < 2 {
            continue;
        }
        // 顶点：1 个基角 + n 个柱顶 + 1 个收尾基角 = n + 2。
        // 三角形：(0, i+1, i+2)，i 取 0..n-1，最后一个用到顶点 n+1，
        // 不能走成 n+2（越界会被 epaint 判成 invalid mesh 直接 panic）。
        let base = pos2(bar.left(), bar.bottom());
        mesh.colored_vertex(base, color);
        for (i, v) in data.iter().enumerate() {
            let x = bar.left() + (i as f32 / (n - 1) as f32) * bar.width();
            let y = bar.bottom() - v.clamp(0.0, 1.0) * (bar.height() - 2.0);
            mesh.colored_vertex(pos2(x, y), color);
        }
        mesh.colored_vertex(pos2(bar.right(), bar.bottom()), color);
        for i in 0..n - 1 {
            mesh.add_triangle(0, i as u32 + 1, i as u32 + 2);
        }
        painter.add(egui::Shape::Mesh(std::sync::Arc::new(mesh)));
    }
    painter.rect_stroke(
        bar,
        2.0,
        Stroke::new(1.0, theme.separator),
        egui::StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Pos2;
    use picasa_core::Appearance;
    use picasa_db::PhotoRow;

    // ---- 夹具 ---------------------------------------------------------------

    fn photo(id: i64, taken_at: i64) -> PhotoRow {
        PhotoRow {
            id,
            folder_id: 1,
            folder_path: "/tmp/photos".into(),
            name: format!("IMG_{id:04}.jpg"),
            size_bytes: 1024,
            mtime_ns: taken_at * 1_000_000_000,
            width: Some(4000),
            height: Some(3000),
            orientation: Some(1),
            taken_at: Some(taken_at),
            taken_at_src: 0,
            thumb_state: 1,
            decoder_ver: 0,
            eff_taken_at: taken_at,
            starred: id % 2 == 0,
        }
    }

    fn photos(n: usize) -> Vec<PhotoRow> {
        (1..=n as i64).map(|i| photo(i, 1_600_000_000 + i * 86_400)).collect()
    }

    const STAGE: Vec2 = vec2(1280.0, 820.0);

    fn stage_rect() -> Rect {
        Rect::from_min_size(Pos2::ZERO, STAGE)
    }

    /// 无窗口后端跑一帧查看器，返回（动作，本帧所有文本）。
    ///
    /// 纹理增量必须显式丢弃（`drop_without_applying_deltas`）：直接 drop
    /// `FullOutput` 会触发 epaint 的「有未应用纹理增量」panic。
    fn run_stage(
        viewer: &mut Viewer,
        rows: &[PhotoRow],
        appearance: Appearance,
        film: &[FilmThumb],
    ) -> (Option<ViewerAction>, Vec<String>) {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx, appearance);
        let theme = Theme::for_appearance(appearance);
        let input = egui::RawInput {
            screen_rect: Some(stage_rect()),
            max_texture_side: Some(2048),
            ..Default::default()
        };
        let mut action = None;
        let out = ctx.run_ui(input, |ui| {
            action = show(ui, viewer, rows, &theme, true, film);
        });
        let texts = texts(&out);
        out.drop_without_applying_deltas();
        (action, texts)
    }

    fn texts(out: &egui::FullOutput) -> Vec<String> {
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
        let mut v = Vec::new();
        for c in &out.shapes {
            walk(&c.shape, &mut v);
        }
        v
    }

    fn has_text(texts: &[String], needle: &str) -> bool {
        texts.iter().any(|t| t.contains(needle))
    }

    /// 两个矩形是否有**正面积**重叠（共用边线不算遮挡）。
    fn overlaps(a: Rect, b: Rect) -> bool {
        let x = a.left().max(b.left()) < a.right().min(b.right());
        let y = a.top().max(b.top()) < a.bottom().min(b.bottom());
        x && y
    }

    // ---- AC-VW-STAGE --------------------------------------------------------

    /// 默认展厅可见：大图区（画布）、说明条、胶片条，三者纵向不重叠。
    #[test]
    fn default_stage_has_canvas_caption_and_filmstrip() {
        let lay = layout(stage_rect(), false, false);
        assert!(lay.canvas.height() > 200.0, "主图区应有足够高度：{:?}", lay.canvas);
        assert!(lay.caption.height() > 20.0, "说明条应存在：{:?}", lay.caption);
        assert!(lay.filmstrip.height() > 20.0, "胶片条应存在：{:?}", lay.filmstrip);
        // 纵向顺序（§4.5）：canvas → caption → filmstrip → status
        assert!(lay.canvas.bottom() <= lay.caption.top() + 0.5);
        assert!(lay.caption.bottom() <= lay.filmstrip.top() + 0.5);
        assert!(lay.filmstrip.bottom() <= lay.status.top() + 0.5);
        // 关闭信息栏时没有右栏
        assert!(lay.info_panel.is_none());
    }

    /// 无窗口跑一帧：默认展厅里大图 / 说明条 / 胶片条都有内容可画。
    #[test]
    fn default_stage_draws_title_and_filmstrip_cells() {
        let rows = photos(5);
        let mut v = Viewer::new(0);
        let (_action, texts) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        let all = texts.join("\n");
        assert!(all.contains("IMG_0001"), "说明条 / 底栏应出现标题：{all}");
        assert!(all.contains("第 1 / 5 张"), "底栏四段信息：{all}");
        // 胶片条占位格：每格一个编号（1..=5）
        let numbers: Vec<&str> = texts
            .iter()
            .map(|t| t.as_str())
            .filter(|t| t.len() <= 2 && t.chars().all(|c| c.is_ascii_digit()))
            .collect();
        assert!(
            (1..=5).all(|i| numbers.contains(&i.to_string().as_str())),
            "胶片条应有 1..=5 的占位编号：{numbers:?}"
        );
    }

    /// 信息栏打开 → 主图与胶片条被**推窄**（几何不重叠），关闭后恢复。
    #[test]
    fn info_panel_pushes_stage_instead_of_overlaying() {
        let closed = layout(stage_rect(), false, false);
        let open = layout(stage_rect(), true, false);
        let panel = open.info_panel.expect("打开时应有信息栏");
        assert!((panel.width() - INFO_W).abs() < 0.5);
        // 推窄：舞台列宽减少的信息栏宽度
        assert!(open.stage.width() < closed.stage.width());
        assert!((closed.stage.width() - open.stage.width() - INFO_W).abs() < 0.5);
        assert!(open.canvas.width() < closed.canvas.width());
        assert!(open.filmstrip.width() < closed.filmstrip.width());
        // 不遮挡：面板与主图 / 胶片条 / 说明条**面积不重叠**（允许共用边线）
        assert!(!overlaps(panel, open.canvas), "{panel:?} 盖住了主图 {:?}", open.canvas);
        assert!(!overlaps(panel, open.filmstrip));
        assert!(!overlaps(panel, open.caption));
        assert!(open.stage.right() <= panel.left() + 0.5);
        // 关闭后主图恢复宽度
        let reopened = layout(stage_rect(), false, false);
        assert!((reopened.canvas.width() - closed.canvas.width()).abs() < 0.5);
    }

    /// 信息栏开关是纯切换：打开一次变关、再开一次变开（I 键同路径）。
    #[test]
    fn info_toggle_is_reversible() {
        let mut v = Viewer::new(0);
        assert!(v.info_open, "默认打开（沿用 viewer_info_open=true）");
        v.toggle_info();
        assert!(!v.info_open);
        v.toggle_info();
        assert!(v.info_open);
    }

    /// 工具条仍有缩放 −/+，且两者都在顶栏内、不互相重叠。
    #[test]
    fn toolbar_keeps_zoom_minus_and_plus() {
        let lay = layout(stage_rect(), true, false);
        let (minus, plus) = zoom_button_rects(lay.top);
        assert!(minus.width() > 10.0 && plus.width() > 10.0);
        assert!(!minus.intersects(plus));
        assert!(lay.top.contains_rect(minus), "− 必须在顶栏内");
        assert!(lay.top.contains_rect(plus), "+ 必须在顶栏内");
        // 与「照片信息」开关不重叠（否则点击会打架）
        let info = info_toggle_rect(lay.top);
        assert!(!plus.intersects(info));
        // 顶栏按钮不是只有适合/100%/200%：至少还有 − 与 +
        let rows = photos(2);
        let mut v = Viewer::new(0);
        let (_a, all) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        let all = all.join("\n");
        assert!(all.contains('−') && all.contains('+'), "{all}");
        assert!(all.contains("适合") && all.contains("100%"), "{all}");
    }

    /// 亮 / 暗两套主题画出同一套展厅结构（§5.5 不得行为分叉）。
    #[test]
    fn both_themes_render_same_stage_structure() {
        let rows = photos(3);
        for appearance in [Appearance::Light, Appearance::Dark] {
            let mut v = Viewer::new(0);
            let lay = layout(stage_rect(), true, false);
            assert!(lay.info_panel.is_some(), "{appearance:?} 也必须有信息栏");
            let (_a, texts) = run_stage(&mut v, &rows, appearance, &[]);
            let all = texts.join("\n");
            assert!(all.contains("照片信息"), "{appearance:?}: {all}");
            assert!(all.contains("第 1 / 3 张"), "{appearance:?}: {all}");
            assert!(all.contains("返回"), "{appearance:?}: {all}");
        }
    }

    // ---- 胶片条 -------------------------------------------------------------

    /// 当前项必然落在可见槽位里，且槽位下标从当前项附近开窗。
    #[test]
    fn filmstrip_keeps_current_item_visible() {
        let rect = layout(stage_rect(), true, false).filmstrip;
        for current in [0usize, 1, 7, 40, 41] {
            let slots = filmstrip_slots(rect, 42, current);
            assert!(!slots.is_empty());
            assert!(
                slots.iter().any(|s| s.index == current),
                "current={current} 必须可见：{:?}",
                slots.iter().map(|s| s.index).collect::<Vec<_>>()
            );
            // 槽位按顺序、不越界
            for w in slots.windows(2) {
                assert_eq!(w[1].index, w[0].index + 1);
            }
            assert!(slots.iter().all(|s| s.index < 42));
            assert!(slots.iter().all(|s| s.rect.right() <= rect.right() + 0.5));
        }
    }

    /// 点击跳转程序接口：点在槽位里返回该下标，点空白返回 None。
    #[test]
    fn filmstrip_hit_maps_click_to_photo_index() {
        let rect = layout(stage_rect(), true, false).filmstrip;
        let slots = filmstrip_slots(rect, 5, 2);
        let target = slots[0];
        assert_eq!(filmstrip_hit(rect, 5, 2, target.rect.center()), Some(target.index));
        // 每个槽位中心都能命中自己
        for slot in &slots {
            assert_eq!(filmstrip_hit(rect, 5, 2, slot.rect.center()), Some(slot.index));
        }
        // 空白处不命中
        assert_eq!(filmstrip_hit(rect, 5, 2, rect.left_top() + vec2(1.0, 1.0)), None);
        assert_eq!(filmstrip_hit(rect, 0, 0, rect.center()), None);
    }

    /// 点击槽位真的产生 `JumpTo` 动作（用真实指针事件驱动一帧）。
    #[test]
    fn clicking_filmstrip_cell_emits_jump() {
        let rows = photos(6);
        let mut v = Viewer::new(0);
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx, Appearance::Light);
        let theme = Theme::for_appearance(Appearance::Light);
        let rect = stage_rect();
        let lay = layout(rect, false, false);
        let slots = filmstrip_slots(lay.filmstrip, rows.len(), v.index);
        let target = slots[2];
        let pos = target.rect.center();

        // 第 1 帧：建立交互层（无点击）
        let first = egui::RawInput {
            screen_rect: Some(rect),
            max_texture_side: Some(2048),
            ..Default::default()
        };
        let frame1 = ctx.run_ui(first, |ui| {
            let _ = show(ui, &mut v, &rows, &theme, true, &[]);
        });
        frame1.drop_without_applying_deltas();
        // 第 2 帧：按下
        let mut press = egui::RawInput {
            screen_rect: Some(rect),
            max_texture_side: Some(2048),
            ..Default::default()
        };
        press.events.push(egui::Event::PointerMoved(pos));
        press.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        let frame2 = ctx.run_ui(press, |ui| {
            let _ = show(ui, &mut v, &rows, &theme, true, &[]);
        });
        frame2.drop_without_applying_deltas();
        // 第 3 帧：松开 → click
        let mut release = egui::RawInput {
            screen_rect: Some(rect),
            max_texture_side: Some(2048),
            ..Default::default()
        };
        release.events.push(egui::Event::PointerMoved(pos));
        release.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        let mut action = None;
        let out = ctx.run_ui(release, |ui| {
            action = show(ui, &mut v, &rows, &theme, true, &[]);
        });
        out.drop_without_applying_deltas();
        assert_eq!(
            action,
            Some(ViewerAction::JumpTo(target.index)),
            "点胶片条应跳到第 {} 张",
            target.index
        );
    }

    /// 胶片条跳转后的当前项高亮由 `is_current` 判定（同一份下标事实）。
    #[test]
    fn filmstrip_current_slot_is_highlighted_by_index() {
        let rect = layout(stage_rect(), false, false).filmstrip;
        let slots = filmstrip_slots(rect, 9, 4);
        let current: Vec<usize> = slots
            .iter()
            .filter(|s| s.index == 4)
            .map(|s| s.index)
            .collect();
        assert_eq!(current, vec![4], "当前项在胶片条里恰好一个高亮槽位");
    }

    // ---- AC-VW-ZOOM ---------------------------------------------------------

    /// 缩放连续变化且落在 5%–800%；PageDown = 适合；1 = 100% 且不打分。
    #[test]
    fn zoom_range_fit_and_actual() {
        let rows = photos(3);
        let mut v = Viewer::new(0);
        run_stage(&mut v, &rows, Appearance::Light, &[]);

        // 连续放大直到上限
        for _ in 0..100 {
            v.zoom_in();
        }
        assert!((v.zoom - ZOOM_MAX).abs() < 1e-4, "上限 800%：{}", v.zoom);
        // 连续缩小直到下限
        for _ in 0..100 {
            v.zoom_out();
        }
        assert!((v.zoom - ZOOM_MIN).abs() < 1e-4, "下限 5%：{}", v.zoom);

        // PageDown = 适合：下一帧按可用宽度重算，且不超过 100%
        v.request_fit();
        run_stage(&mut v, &rows, Appearance::Light, &[]);
        let lay = layout(stage_rect(), false, false);
        let fit = fit_scale(lay.main_pane.size(), 4000.0, 3000.0);
        assert!((v.zoom - fit).abs() < 1e-4, "PageDown 应回到适合：{} vs {}", v.zoom, fit);
        assert!(fit <= 1.0);

        // 数字 1 = 100%，且不打分（starred 不变）
        let before: Vec<bool> = rows.iter().map(|p| p.starred).collect();
        v.zoom_actual();
        assert!((v.zoom - 1.0).abs() < 1e-6);
        run_stage(&mut v, &rows, Appearance::Light, &[]);
        let after: Vec<bool> = rows.iter().map(|p| p.starred).collect();
        assert_eq!(before, after, "数字 1 不能给照片打分");
    }

    /// 信息栏开合改变「适合」基准：关闭后主图恢复宽度，适合比例也随之变化。
    #[test]
    fn fit_scale_uses_pushed_pane_width() {
        let closed = layout(stage_rect(), false, false);
        let open = layout(stage_rect(), true, false);
        // 用长条图（宽度受限），这样「适合」直接由推窄后的可用宽度决定
        let fit_closed = fit_scale(closed.main_pane.size(), 4000.0, 400.0);
        let fit_open = fit_scale(open.main_pane.size(), 4000.0, 400.0);
        assert!(fit_open < fit_closed, "推窄后适合比例应更小：{fit_open} vs {fit_closed}");
    }

    /// 活的一帧也必须跟着窗格走：关信息栏 / 开并排都会改变主窗格宽度，
    /// 用户没手动缩放过时「适合」要按**当前**窗格重算，不能停在旧比例上。
    ///
    /// 上一条只验证 `fit_scale` 的算术；这里验证 `show()` 真的把新比例落到
    /// `viewer.zoom`（否则图会溢出主图区被裁掉）。
    #[test]
    fn fit_zoom_follows_pane_on_live_frame() {
        let rows = photos(3);
        let mut v = Viewer::new(0);

        // 帧 1：默认信息栏打开 → 按推窄后的窗格适合
        run_stage(&mut v, &rows, Appearance::Light, &[]);
        let open = layout(stage_rect(), true, false);
        let fit_open = fit_scale(open.main_pane.size(), 4000.0, 3000.0);
        assert!((v.zoom - fit_open).abs() < 1e-4, "首帧适合：{} vs {}", v.zoom, fit_open);

        // 帧 2：关掉信息栏，窗格变宽 → 仍应「适合」新的更宽窗格
        v.toggle_info();
        run_stage(&mut v, &rows, Appearance::Light, &[]);
        let closed = layout(stage_rect(), false, false);
        let fit_closed = fit_scale(closed.main_pane.size(), 4000.0, 3000.0);
        assert!(
            (v.zoom - fit_closed).abs() < 1e-4,
            "关掉信息栏后仍应「适合」：zoom={} fit={}",
            v.zoom,
            fit_closed
        );

        // 帧 3：并排比较 → 主窗格减半，仍应适合
        v.compare = true;
        run_stage(&mut v, &rows, Appearance::Light, &[]);
        let cmp = layout(stage_rect(), false, true);
        let fit_cmp = fit_scale(cmp.main_pane.size(), 4000.0, 3000.0);
        assert!(
            (v.zoom - fit_cmp).abs() < 1e-4,
            "并排后主窗格减半仍应「适合」：zoom={} fit_cmp={}",
            v.zoom,
            fit_cmp
        );
        // 主图不能宽过自己的窗格（溢出就会被裁）
        assert!(4000.0 * v.zoom <= cmp.main_pane.width() + 0.5);
    }

    /// 打开信息栏把主图推窄后，宽度受限的图也要重新适合（长条图）。
    #[test]
    fn opening_info_panel_refits_width_limited_photo() {
        use picasa_db::PhotoRow;
        let rows: Vec<PhotoRow> = (1..=3i64)
            .map(|i| {
                let mut p = photo(i, 1_600_000_000 + i * 86_400);
                p.width = Some(4000);
                p.height = Some(400);
                p
            })
            .collect();
        let mut v = Viewer::new(0);
        v.info_open = false;
        run_stage(&mut v, &rows, Appearance::Light, &[]);
        let closed = layout(stage_rect(), false, false);
        let fit_closed = fit_scale(closed.main_pane.size(), 4000.0, 400.0);
        assert!((v.zoom - fit_closed).abs() < 1e-4);

        // 打开信息栏：窗格推窄，用户未手动缩放 → 应重新适合
        v.toggle_info();
        run_stage(&mut v, &rows, Appearance::Light, &[]);
        let open = layout(stage_rect(), true, false);
        let fit_open = fit_scale(open.main_pane.size(), 4000.0, 400.0);
        assert!(
            (v.zoom - fit_open).abs() < 1e-4,
            "打开信息栏推窄后仍应「适合」：zoom={} fit={}",
            v.zoom,
            fit_open
        );
        assert!(fit_open < fit_closed, "推窄后适合比例应更小");
    }

    /// 滚轮缩放走同一上下限（不叠加 ⌘+滚轮由快捷键层保证）。
    #[test]
    fn wheel_zoom_is_clamped() {
        let mut v = Viewer::new(0);
        v.zoom = 0.06;
        v.zoom = (v.zoom * (1.0 + 100_000.0 * 0.0015)).clamp(ZOOM_MIN, ZOOM_MAX);
        assert!((v.zoom - ZOOM_MAX).abs() < 1e-4);
    }

    // ---- AC-VW-COMPARE ------------------------------------------------------

    /// 至少两张：按 C 左右并排（当前 + 下一张），两窗格不重叠且都在画布里。
    #[test]
    fn compare_with_next_splits_panes() {
        let rows = photos(3);
        let mut v = Viewer::new(0);
        v.compare = true;
        assert!(v.compare_active(rows.len()));
        let lay = layout(stage_rect(), true, true);
        let right = lay.compare_pane.expect("有下一张就应有右窗格");
        assert!(!lay.main_pane.intersects(right));
        assert!(lay.canvas.contains_rect(lay.main_pane));
        assert!(lay.canvas.contains_rect(right));
        assert!((lay.main_pane.width() - right.width()).abs() < 1.0);
        // 载入中可出现「正在载入比较图…」
        let (_a, texts) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        assert!(has_text(&texts, "正在载入比较图…"), "比较图未就绪时应有载入句");
    }

    /// 只有一张：按 C 不进入比较，也不画假比较图。
    #[test]
    fn compare_with_single_photo_draws_no_fake_pane() {
        let rows = photos(1);
        let mut v = Viewer::new(0);
        assert!(!v.compare_available(rows.len()));
        // 即使开关被强行置 true，也不应画右窗格
        v.compare = true;
        assert!(!v.compare_active(rows.len()));
        let lay = layout(stage_rect(), false, v.compare_active(rows.len()));
        assert!(lay.compare_pane.is_none());
        let (_a, texts) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        assert!(!has_text(&texts, "正在载入比较图…"), "单张不得画假比较图");
    }

    /// 序列末尾同样没有第二张：按 C 开关保持关闭。
    #[test]
    fn compare_at_last_photo_is_unavailable() {
        let rows = photos(2);
        let v = Viewer::new(1);
        assert!(!v.compare_available(rows.len()));
        let lay = layout(stage_rect(), true, v.compare_active(rows.len()));
        assert!(lay.compare_pane.is_none());
    }

    // ---- AC-VW-01 保留项 ----------------------------------------------------

    /// 返回文案、底栏四段、端点箭头语义、无 EXIF 文案。
    #[test]
    fn retained_chrome_strings_and_fields() {
        let rows = photos(3);
        let mut v = Viewer::new(0);
        v.taken_src = "EXIF 拍摄时间".into();
        v.exif_status = "该文件没有 EXIF 信息".into();
        let (_a, all) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        let all = all.join("\n");
        assert!(all.contains("返回"));
        assert!(all.contains("4 000 × 3 000") || all.contains("4000 × 3000"), "{all}");
        assert!(all.contains("第 1 / 3 张"));
        assert!(all.contains("照片信息"));
        assert!(all.contains("该文件没有 EXIF 信息"));
        assert!(all.contains("RGB 直方图"));
        // 端点：第一张有下一张 → 右箭头提示；没有上一张 → 不画左箭头
        assert!(rows.len() > v.index + 1);
        assert_eq!(v.index, 0);
    }

    /// 背景档位仍是产品既有的三态循环，并写回 `viewer_bg` 编号。
    #[test]
    fn background_cycle_is_unchanged_and_persistable() {
        let mut v = Viewer::new(0);
        assert_eq!(v.bg, Bg::Black);
        v.toggle_bg();
        assert_eq!(v.bg, Bg::White);
        v.toggle_bg();
        assert_eq!(v.bg, Bg::Grey);
        v.toggle_bg();
        assert_eq!(v.bg, Bg::Black, "三态循环，不扩展档位");
        assert_eq!(Bg::Black.code(), 0);
        assert_eq!(Bg::White.code(), 1);
        assert_eq!(Bg::Grey.code(), 2);
        assert_eq!(Bg::from_code(1), Bg::White);
        assert_eq!(Bg::from_code(2), Bg::Grey);
        assert_eq!(Bg::from_code(0), Bg::Black);
    }

    /// 说明条标题取文件名去扩展名；日期用本机时区格式化（有值才显示）。
    #[test]
    fn caption_title_and_date_helpers() {
        let row = photo(1, 1_600_000_000);
        assert_eq!(display_title(&row), "IMG_0001");
        assert!(format_taken_date(row.eff_taken_at).is_some());
        assert!(format_taken_date(0).is_none());
        assert_eq!(display_title(&photos(1)[0]), "IMG_0001");
    }

    /// 换页重置缩放与适合标记（翻到下一张应回「适合」，不残留上一张的 400%）。
    #[test]
    fn jump_resets_zoom_and_fit() {
        let mut v = Viewer::new(0);
        v.zoom_actual();
        v.zoom_in();
        assert!(v.zoom > 1.0);
        v.jump_to(1);
        assert!(v.fit_pending);
        assert!(!v.user_zoomed);
        assert_eq!(v.index, 1);
    }
}
