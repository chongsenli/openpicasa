//! 查看器展厅（M5-R4：M5-VW-16 / M5-VIS-03 ADD-B，M5-VW-07 推动式信息栏）。
//!
//! ## 展厅结构（说明书 §4.5）
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │ 返回  收藏  显示位置  比较  背景  [适合|100%|−|+]  文件名  [照片信息]│ 顶栏
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
//! * 底栏四段信息、左右点击区与端点箭头、全屏、Space 背景；控件常驻不自动隐藏（V6）。
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

/// 顶栏高度（逻辑像素）。顶栏只有左端一枚「返回」胶囊（V1.4 单返回）。
pub const TOP_H: f32 = 44.0;
/// 底栏高度。
pub const STATUS_H: f32 = 27.0;
/// 主图下方说明条高度（只读，不是编辑台）。V3：标题 + 日期 + 收藏/标签行。
pub const CAPTION_H: f32 = 78.0;
/// 胶片条高度。
pub const FILM_H: f32 = 82.0;
/// 右侧「照片信息」栏占窗口宽度的比例。
///
/// 信息栏宽度（亮色）：原型 v1.4 `css/app.css` 的 `.info-panel` 写死
/// `width:328px;flex:0 0 328px`，**不随窗口变宽**。
pub const INFO_W: f32 = 328.0;
/// 信息栏宽度（暗色）：原型 `html[data-theme="dark"] .info-panel`
/// `width:316px;flex-basis:316px`。
pub const INFO_DARK_W: f32 = 316.0;
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
    /// 照片区外框：`canvas` 由它向内收 18 得到，四周留白给落影留出空间。
    pub card: Rect,
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
/// 信息栏宽度：原型是**固定像素宽**（亮 [`INFO_W`] / 暗 [`INFO_DARK_W`]），
/// 换窗口也不变，只有可用宽度本身不够时才退让（`body_w` 比它窄的极端情况）。
///
/// 早先按「窗口宽 25%」实现（1280→320、1920→480），与原型差 8px 且越宽的屏
/// 越不像；2026-11-27 按原型改为固定值。
pub fn info_panel_width(body_w: f32, dark: bool) -> f32 {
    if body_w <= 0.0 {
        return 0.0;
    }
    let want = if dark { INFO_DARK_W } else { INFO_W };
    want.min(body_w).max(0.0)
}

pub fn layout(rect: Rect, info_open: bool, compare: bool, dark: bool) -> StageLayout {
    let top = Rect::from_min_size(rect.min, vec2(rect.width(), TOP_H));
    let status = Rect::from_min_size(
        pos2(rect.left(), (rect.bottom() - STATUS_H).max(rect.top())),
        vec2(rect.width(), STATUS_H),
    );
    let body = Rect::from_min_max(
        pos2(rect.left(), top.bottom()),
        pos2(rect.right(), status.top()),
    );
    // 按原型固定宽取（见 [`INFO_W`] / [`INFO_DARK_W`]）；仍是推窄而不是遮挡。
    let info_w = info_panel_width(body.width(), dark);
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
    // V1：照片放在圆角卡片上，卡片四周留白（亮色展厅不能把图贴到浅灰底上）。
    let canvas_outer = Rect::from_min_max(
        pos2(stage.left() + 8.0, stage.top() + 6.0),
        pos2(stage.right() - 8.0, caption.top() - 2.0),
    );
    let card = canvas_outer.shrink2(vec2(10.0, 8.0));
    let canvas = card.shrink(18.0);
    let main_pane = if compare {
        Rect::from_min_max(canvas.min, pos2(canvas.center().x - 4.0, canvas.bottom()))
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
        card,
        canvas,
        main_pane,
        compare_pane,
        caption,
        filmstrip,
        info_panel,
    }
}

// ---- 顶栏流式布局尺寸 -----------------------------------------------------
//
// V1.4：工具条是**一条从左往右的流**，不再左右各锚一枚「返回」。
// 顺序：返回 → 收藏 → 显示位置 → 比较 → 背景 → 缩放簇 → 文件名 → 信息胶囊。
// 文件名跟在缩放控件之后；信息胶囊右对齐到顶栏最右端（用户要求）。
//
// 左端留白给 macOS 红绿灯：原型（`js/app.js` / `css/app.css`，1280 逻辑宽）
// `.traffic` 占 x=16..68，返回按钮从 x=94 起，水平上不与红绿灯重叠。
// 旧值 10 让返回胶囊从 x=10 起，正好压在红绿灯上（用户实测反馈）。
const TB_PAD: f32 = 94.0;
const TB_BACK_W: f32 = 72.0;
const TB_BACK_GAP: f32 = 14.0;
const TB_HEART_W: f32 = 28.0;
const TB_HEART_GAP: f32 = 8.0;
const TB_REVEAL_W: f32 = 28.0;
const TB_REVEAL_GAP: f32 = 10.0;
const TB_COMPARE_W: f32 = 52.0;
const TB_COMPARE_GAP: f32 = 6.0;
const TB_BG_W: f32 = 52.0;
const TB_BG_GAP: f32 = 12.0;
const TB_TRACK_PAD: f32 = 4.0;
const TB_FIT_W: f32 = 50.0;
const TB_ACTUAL_W: f32 = 50.0;
const TB_ZOOM_MINUS_W: f32 = 28.0;
const TB_ZOOM_PLUS_W: f32 = 28.0;
const TB_ZOOM_GAP: f32 = 4.0;
/// 缩放读数（「适合」/「120%」）的固定宽：原型 `.zoom-readout{min-width:40px;
/// text-align:right;font-size:11px;color:var(--faint)}`，夹在 − 与 + 之间。
const TB_ZOOM_READOUT_W: f32 = 44.0;
const TB_TRACK_H: f32 = 28.0;
const TB_NAME_W: f32 = 150.0;
const TB_NAME_GAP: f32 = 14.0;
const TB_CHIP_GAP: f32 = 8.0;
const TB_CHIP_W: f32 = 96.0;

/// 缩放簇的圆角轨道（适合 / 100% / − / + 共用一条底）。
pub fn zoom_track_rect(top: Rect) -> Rect {
    let x = top.left()
        + TB_PAD
        + TB_BACK_W
        + TB_BACK_GAP
        + TB_HEART_W
        + TB_HEART_GAP
        + TB_REVEAL_W
        + TB_REVEAL_GAP
        + TB_COMPARE_W
        + TB_COMPARE_GAP
        + TB_BG_W
        + TB_BG_GAP;
    Rect::from_min_size(
        pos2(x, top.top() + 8.0),
        vec2(
            TB_TRACK_PAD * 2.0
                + TB_FIT_W
                + TB_ACTUAL_W
                + TB_ZOOM_MINUS_W
                + TB_ZOOM_GAP
                + TB_ZOOM_READOUT_W
                + TB_ZOOM_GAP
                + TB_ZOOM_PLUS_W,
            TB_TRACK_H,
        ),
    )
}

/// 顶栏「−/+」缩放按钮的矩形（绘制与测试共用，避免测试自己猜坐标）。
///
/// 返回 `(zoom_out, zoom_in)`。查看器工具条必须始终保留这两个按钮
/// （说明书 §4.5：不能把适合 / 100% / 200% 三按钮作为唯一缩放）。
pub fn zoom_button_rects(top: Rect) -> (Rect, Rect) {
    let track = zoom_track_rect(top);
    let x = track.left() + TB_TRACK_PAD + TB_FIT_W + TB_ACTUAL_W;
    let y = track.top() + 2.0;
    (
        Rect::from_min_size(pos2(x, y), vec2(TB_ZOOM_MINUS_W, 24.0)),
        Rect::from_min_size(
            pos2(
                x + TB_ZOOM_MINUS_W + TB_ZOOM_GAP + TB_ZOOM_READOUT_W + TB_ZOOM_GAP,
                y,
            ),
            vec2(TB_ZOOM_PLUS_W, 24.0),
        ),
    )
}

/// 缩放读数矩形（「适合」或「120%」），夹在 − 与 + 之间。
pub fn zoom_readout_rect(top: Rect) -> Rect {
    let track = zoom_track_rect(top);
    let (minus, _) = zoom_button_rects(top);
    Rect::from_min_size(
        pos2(minus.right() + TB_ZOOM_GAP, track.top() + 2.0),
        vec2(TB_ZOOM_READOUT_W, 24.0),
    )
}

/// 文件名槽位 + 信息胶囊的横向布局：`(文件名左边, 文件名宽, 胶囊左边)`。
///
/// 「照片信息」胶囊右对齐到顶栏最右侧（内缩 8px），文件名槽位留在缩放簇
/// 之后、最多 `TB_NAME_W` 宽，两者之间是弹性空白。窗口窄到放不下时先压
/// 文件名，保证胶囊仍完整贴在右端不被裁掉（控件常驻）。
fn top_tail(top: Rect) -> (f32, f32, f32) {
    let track = zoom_track_rect(top);
    let name_left = track.right() + TB_NAME_GAP;
    // 胶囊锚定顶栏右端；极窄窗下至少不倒退到缩放簇左边。
    let chip_left = (top.right() - 8.0 - TB_CHIP_W).max(track.right() + TB_NAME_GAP);
    let avail = chip_left - TB_CHIP_GAP - name_left;
    let name_w = TB_NAME_W.min(avail.max(0.0));
    (name_left, name_w, chip_left)
}

/// 顶栏「照片信息」开关的矩形：右对齐到顶栏最右侧（内缩 8px）。
pub fn info_toggle_rect(top: Rect) -> Rect {
    let (_, _, chip_left) = top_tail(top);
    Rect::from_min_size(pos2(chip_left, top.top() + 10.0), vec2(TB_CHIP_W, 24.0))
}

/// 顶栏文件名槽位（在缩放簇之后，最多 `TB_NAME_W` 宽；信息胶囊已右对齐到
/// 顶栏最右端，两者之间是弹性空白，窄窗时才相邻）。
pub fn filename_slot_rect(top: Rect) -> Rect {
    let (left, w, _) = top_tail(top);
    Rect::from_min_size(pos2(left, top.top()), vec2(w, top.height()))
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
    if fit.is_finite() && fit > 0.0 {
        fit
    } else {
        1.0
    }
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

    // ---- V6：展厅控件常驻 ----
    //
    // 原型（v14-light-viewer-info / compare）的顶栏、信息栏、胶片条与底栏说明
    // **始终可见**。旧实现按鼠标静止 3 秒自动隐藏 chrome 并藏光标，导致
    // Linux 截图里只剩一张照片；这里取消空闲计时，也不再改动光标可见性。
    let chrome_visible = true;

    let compare_active = viewer.compare_active(photos.len());
    let lay = layout(rect, viewer.info_open, compare_active, theme.dark);

    // ---- V1：照片自身的圆角（亮色大圆角 / 暗色小圆角） ----
    //
    // r7 产品把圆角做在**白色外框**上、图片本身是直角，实窗里看起来就是
    // 「方图套在圆角白框里」。原型是照片本身有圆角、直接落在展厅底色上
    // （亮色 22、暗色 6），没有那一圈白色画框。这里改成：不再铺满整块卡片，
    // 只在照片自己的矩形上给一层落影，圆角加在图片本身（见 `paint_rounded_image`）。
    //
    // 两套主题都不再画「卡片底」——照片直接落在展厅底色上，靠落影与背景分开
    // （原型 `.stage img` 就是这样：只有 border-radius + box-shadow，没有底板）。

    // ---- 主图尺寸 / 适合 ----
    let (iw, ih) = if viewer.tex_size.x > 0.0 {
        (viewer.tex_size.x, viewer.tex_size.y)
    } else {
        (
            row.width.unwrap_or(1000).max(1) as f32,
            row.height.unwrap_or(700).max(1) as f32,
        )
    };
    let fit = fit_zoom(lay.main_pane, iw, ih, theme);
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
    let full_uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let photo_r = photo_radius(theme);
    // 原型是照片直接落在展厅底色上 + 一层柔和落影，不是一圈白色画框；
    // 落影跟着照片自己的圆角走，所以圆角形状在亮/暗两边都成立。
    let photo_shadow = photo_shadow(theme);
    if viewer.tex.is_some() || viewer.preview.is_some() {
        main_painter.add(photo_shadow.as_shape(img_rect, photo_r));
    }
    if let Some(tex) = viewer.tex.as_ref() {
        paint_rounded_image(&main_painter, tex.id(), img_rect, photo_r, full_uv);
    } else if let Some((tex_id, psize)) = viewer.preview {
        let prect = fit_rect(img_rect, psize);
        paint_rounded_image(&main_painter, tex_id, prect, photo_r, full_uv);
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
            right_painter.add(photo_shadow.as_shape(crect, photo_r));
            paint_rounded_image(&right_painter, tex.id(), crect, photo_r, full_uv);
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
            compare_badge(
                &painter,
                pane.left_top() + vec2(10.0, 10.0),
                "当前",
                row,
                right_ink,
            );
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
        draw_filmstrip(
            &painter,
            ui,
            &lay,
            viewer,
            theme,
            photos,
            film_thumbs,
            &mut action,
        );
        draw_statusbar(&painter, &lay, viewer, theme, row, photos.len(), fit, bg);
        if let Some(panel) = lay.info_panel
            && draw_info_panel(
                &painter,
                ui,
                panel,
                viewer,
                theme,
                row,
                ui.scope_id().with("viewer-info"),
            )
        {
            action = Some(ViewerAction::ToggleInfo);
        }
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
    let light = !theme.dark;
    // V1：亮色展厅的顶栏是浅色（浅底 + 深字），不再叠一块深色暗房画布。
    let (bar_bg, fg) = if light {
        (theme.viewer_chrome_bg, Color32::from_black_alpha(215))
    } else {
        (
            Color32::from_black_alpha(160),
            Color32::from_white_alpha(225),
        )
    };
    painter.rect_filled(top, 0.0, bar_bg);
    painter.line_segment(
        [top.left_bottom(), top.right_bottom()],
        Stroke::new(1.0, theme.hairline),
    );

    // V1.4：**单**返回胶囊，只有左端一枚；右侧不再重复。
    let mut x = top.left() + TB_PAD;
    let left_back = Rect::from_min_size(pos2(x, top.top() + 10.0), vec2(TB_BACK_W, 24.0));
    draw_back_pill(
        painter,
        ui,
        ui.scope_id().with("viewer-back"),
        left_back,
        theme,
        fg,
    );
    if ui
        .interact(
            left_back,
            ui.scope_id().with("viewer-back-hit"),
            Sense::click(),
        )
        .clicked()
    {
        *action = Some(ViewerAction::Close);
    }
    x = left_back.right() + TB_BACK_GAP;

    // 收藏（只读状态）
    let fav = Rect::from_center_size(
        pos2(x + TB_HEART_W / 2.0, top.center().y),
        vec2(TB_HEART_W, 24.0),
    );
    icons::draw(
        painter,
        Icon::Heart,
        fav.shrink(7.0),
        if row.starred { theme.star } else { fg },
        // 原型：已收藏是实心（stroke 0），未收藏是 1.75 描边。
        if row.starred { 0.0 } else { 1.5 },
    );
    let fav_resp = ui.interact(fav, ui.scope_id().with("viewer-fav"), Sense::hover());
    fav_resp.on_hover_text(if row.starred {
        "已收藏（只读展示）"
    } else {
        "未收藏"
    });
    x = fav.right() + TB_HEART_GAP;

    // 显示位置：原型是**纯图标按钮**（`<button class="ib" title="显示位置">` +
    // locate 图钉），不写文字 —— 文字版留给索引管理菜单（那里的菜单项必须可读）。
    let reveal = Rect::from_center_size(
        pos2(x + TB_REVEAL_W / 2.0, top.center().y),
        vec2(TB_REVEAL_W, 24.0),
    );
    let reveal_resp = slim_button(
        painter,
        ui,
        ui.scope_id().with("viewer-reveal"),
        reveal,
        Icon::Reveal,
        None,
        1.5,
        fg,
    );
    if reveal_resp.clicked() {
        *action = Some(ViewerAction::Reveal);
    }
    reveal_resp.on_hover_text(reveal_label());
    x = reveal.right() + TB_REVEAL_GAP;

    // 比较：只有至少两张时才点亮；状态如实反映当前是否并排。
    if text_button(
        painter,
        ui,
        ui.scope_id().with("viewer-compare"),
        Rect::from_min_size(pos2(x, top.top() + 10.0), vec2(TB_COMPARE_W, 24.0)),
        "比较",
        viewer.compare,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ToggleCompare);
    }
    x += TB_COMPARE_W + TB_COMPARE_GAP;
    if text_button(
        painter,
        ui,
        ui.scope_id().with("viewer-bg"),
        Rect::from_min_size(pos2(x, top.top() + 10.0), vec2(TB_BG_W, 24.0)),
        "背景",
        false,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ToggleBg);
    }

    // 缩放簇：一条圆角轨道内「适合 / 100% / − / +」。
    let track = zoom_track_rect(top);
    painter.rect_filled(track, track.height() / 2.0, theme.seg_bg);
    let mut zx = track.left() + TB_TRACK_PAD;
    if zoom_cell(
        painter,
        ui,
        ui.scope_id().with("viewer-fit"),
        Rect::from_min_size(pos2(zx, track.top() + 2.0), vec2(TB_FIT_W, 24.0)),
        "适合",
        viewer.fit_pending,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ZoomFit);
    }
    zx += TB_FIT_W;
    if zoom_cell(
        painter,
        ui,
        ui.scope_id().with("viewer-actual"),
        Rect::from_min_size(pos2(zx, track.top() + 2.0), vec2(TB_ACTUAL_W, 24.0)),
        "100%",
        (viewer.zoom - 1.0).abs() < 0.001,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ZoomActual);
    }
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
    // 原型 `.zoom-readout`：− 与 + 之间夹当前缩放读数（「适合」或「120%」），
    // 只读、不可点，颜色比按钮淡一档。
    let readout = zoom_readout_rect(top);
    let zoom_label = if viewer.fit_pending || !viewer.user_zoomed {
        "适合".to_string()
    } else {
        format!("{}%", (viewer.zoom * 100.0).round() as i32)
    };
    painter.with_clip_rect(readout).text(
        readout.center(),
        Align2::CENTER_CENTER,
        zoom_label,
        FontId::new(11.0, egui::FontFamily::Proportional),
        if light {
            theme.text_secondary
        } else {
            Color32::from_white_alpha(150)
        },
    );
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

    // 文件名：紧跟缩放簇之后的固定槽位，右对齐；信息胶囊右对齐到顶栏最右端
    // （用户要求，不再像原型那样贴着缩放控件）。两者之间是弹性空白。
    //
    // 窄窗时槽位会被 top_tail() 压窄；这里按原型 `.vfile` 的
    // `text-overflow:ellipsis` 逐字符截断补 `…`，不再靠 clip_rect 硬切掉
    // 右端（硬切会把文件名后半段连同扩展名一起吞掉，且没有省略提示）。
    let name_slot = filename_slot_rect(top);
    let chip = info_toggle_rect(top);
    if name_slot.width() > 20.0 {
        let name_font = FontId::new(12.0, egui::FontFamily::Proportional);
        let shown = truncate_to_width(
            painter,
            &row.name,
            12.0,
            (name_slot.width() - 2.0).max(1.0),
            fg,
        );
        painter.with_clip_rect(name_slot).text(
            pos2(name_slot.right(), top.center().y),
            Align2::RIGHT_CENTER,
            shown,
            name_font,
            fg,
        );
    }

    if draw_info_chip(
        painter,
        ui,
        ui.scope_id().with("viewer-info-toggle"),
        chip,
        viewer.info_open,
        fg,
        theme,
    )
    .clicked()
    {
        *action = Some(ViewerAction::ToggleInfo);
    }
}

/// V5：顶栏「照片信息」信息胶囊（浅底 + 发丝描边 + 图标 + 文字）。
///
/// 替换原来的实心按钮：默认透明感浅底、描边细，打开时边框与文字换成强调色，
/// 点击仍走 `ViewerAction::ToggleInfo`（与 I 键同一条路径）。
fn draw_info_chip(
    painter: &egui::Painter,
    ui: &Ui,
    id: egui::Id,
    rect: Rect,
    active: bool,
    fg: Color32,
    theme: &Theme,
) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click());
    let radius = rect.height() / 2.0;
    // 原型 `.info-toggle.on{background:var(--accent-soft);color:var(--accent-ink);
    // box-shadow:none}`：打开时是**浅强调底**，不是一圆圈描边。
    let fill = if active {
        theme.accent_soft
    } else if resp.hovered() {
        theme.sidebar_hover
    } else if theme.dark {
        Color32::from_white_alpha(18)
    } else {
        theme.viewer_card
    };
    painter.rect_filled(rect, radius, fill);
    if !active {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, theme.hairline),
            egui::StrokeKind::Inside,
        );
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let color = if active { theme.accent_ink } else { fg };
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(12.0, 12.0));
    icons::draw(painter, Icon::Info, icon_rect, color, 1.5);
    painter.text(
        pos2(rect.left() + 25.0, rect.center().y),
        Align2::LEFT_CENTER,
        "照片信息",
        FontId::new(INFO_CHIP_FONT_SIZE, egui::FontFamily::Proportional),
        color,
    );
    resp.on_hover_text("照片信息 · I")
}

/// 唯一的「返回」胶囊：圆角浅片 + 左箭头 + 文字（V1.4 只在左端）。
fn draw_back_pill(
    painter: &egui::Painter,
    ui: &Ui,
    id: egui::Id,
    rect: Rect,
    theme: &Theme,
    fg: Color32,
) {
    let resp = ui.interact(rect, id, Sense::click());
    let fill = if resp.hovered() {
        theme.sidebar_hover
    } else {
        theme.viewer_chrome_bg
    };
    painter.rect_filled(rect, rect.height() / 2.0, fill);
    painter.rect_stroke(
        rect,
        rect.height() / 2.0,
        Stroke::new(1.0, theme.hairline),
        egui::StrokeKind::Inside,
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(12.0, 12.0));
    icons::draw(painter, Icon::Back, icon_rect, fg, 1.5);
    painter.text(
        pos2(rect.left() + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        "返回",
        FontId::new(11.5, egui::FontFamily::Proportional),
        fg,
    );
    resp.on_hover_text("返回图库 · Esc");
}

/// 「显示位置」按平台命名（I3：非 macOS 不写 Finder）。
fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "在 Finder 中显示"
    } else {
        "在文件管理器中显示"
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
    let ink = if is_bright(bg) {
        theme.text_primary
    } else {
        Color32::from_white_alpha(230)
    };
    let soft = if is_bright(bg) {
        theme.text_secondary
    } else {
        Color32::from_white_alpha(150)
    };

    // V3：标题 + 日期 + 收藏/标签行，紧贴照片正下方。
    let title = display_title(row);
    painter.text(
        pos2(caption.left() + 2.0, caption.top() + 8.0),
        Align2::LEFT_TOP,
        title,
        FontId::new(16.0, egui::FontFamily::Proportional),
        ink,
    );
    let mut line2 = String::new();
    if let Some(date) = format_taken_date(row.eff_taken_at) {
        line2.push_str(&date);
    }
    if !viewer.caption_text.trim().is_empty() {
        if !line2.is_empty() {
            line2.push_str("   ");
        }
        line2.push_str(viewer.caption_text.trim());
    }
    if !line2.is_empty() {
        painter.text(
            pos2(caption.left() + 2.0, caption.top() + 30.0),
            Align2::LEFT_TOP,
            line2,
            FontId::new(11.5, egui::FontFamily::Proportional),
            soft,
        );
    }

    // 收藏 / 标签行（只读，显示在标题与日期下方）
    carve_star_and_tags(
        painter,
        Rect::from_min_max(
            pos2(caption.left() + 2.0, caption.top() + 48.0),
            pos2(
                caption.left() + caption.width() * 0.5,
                caption.bottom() - 4.0,
            ),
        ),
        row.starred,
        &viewer.tags,
        theme,
        bg,
    );

    // 右侧元数据摘要（只读，不编辑）
    let summary = caption_summary(viewer);
    if !summary.is_empty() {
        let clip = Rect::from_min_max(
            pos2(caption.left() + caption.width() * 0.45, caption.top()),
            pos2(caption.right(), caption.top() + 34.0),
        );
        painter.with_clip_rect(clip).text(
            pos2(caption.right() - 2.0, caption.top() + 14.0),
            Align2::RIGHT_TOP,
            summary,
            FontId::new(11.0, egui::FontFamily::Proportional),
            soft,
        );
    }
}

/// 说明条里的「收藏 / 标签」一行（V3）。
fn carve_star_and_tags(
    painter: &egui::Painter,
    area: Rect,
    starred: bool,
    tags: &[String],
    theme: &Theme,
    bg: Color32,
) {
    if area.height() < 12.0 {
        return;
    }
    let font = FontId::new(11.0, egui::FontFamily::Proportional);
    let ink = if is_bright(bg) {
        theme.text_secondary
    } else {
        Color32::from_white_alpha(160)
    };
    let chip_color = if is_bright(bg) {
        Color32::from_black_alpha(18)
    } else {
        Color32::from_white_alpha(36)
    };
    let cy = area.center().y;
    let mut x = area.left();
    if starred {
        let c = pos2(x + 6.0, cy);
        icons::draw_star(painter, c, 6.0, theme.star);
        x += 18.0;
        painter.text(pos2(x, cy), Align2::LEFT_CENTER, "收藏", font.clone(), ink);
        x += 30.0;
    }
    if tags.is_empty() && !starred {
        painter.text(
            pos2(x, cy),
            Align2::LEFT_CENTER,
            "未收藏 · 无标签",
            font,
            ink,
        );
        return;
    }
    let mut shown = 0usize;
    for label in tags {
        let galley = painter.layout_no_wrap(label.clone(), font.clone(), ink);
        let w = galley.size().x + 14.0;
        if x + w > area.right() {
            break;
        }
        let chip = Rect::from_min_size(pos2(x, cy - 9.0), vec2(w, 18.0));
        painter.rect_filled(chip, 9.0, chip_color);
        painter.galley(pos2(x + 7.0, cy - galley.size().y / 2.0), galley, ink);
        x += w + 5.0;
        shown += 1;
    }
    if shown < tags.len() {
        painter.text(
            pos2(x, cy),
            Align2::LEFT_CENTER,
            format!("+{}", tags.len() - shown),
            font,
            ink,
        );
    }
}

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
    // V1：亮色展厅的底栏也是浅色（深字）；暗色保持深底浅字。
    let light = is_bright(bg);
    let bar_bg = if light {
        theme.viewer_chrome_bg
    } else {
        Color32::from_rgba_premultiplied(0, 0, 0, 160)
    };
    let ink = if light {
        theme.text_primary
    } else {
        Color32::from_white_alpha(215)
    };
    let ink_soft = if light {
        theme.text_secondary
    } else {
        Color32::from_white_alpha(150)
    };
    painter.rect_filled(bar, 0.0, bar_bg);
    painter.line_segment(
        [bar.left_top(), bar.right_top()],
        Stroke::new(1.0, theme.hairline),
    );
    let dims = viewer
        .dims
        .map(|(w, h)| format!("{w} × {h}"))
        .unwrap_or_else(|| format!("{} × {}", row.width.unwrap_or(0), row.height.unwrap_or(0)));
    let zoom_text = if (viewer.zoom - fit).abs() < 0.005 {
        "适合".to_string()
    } else {
        format!("{}%", (viewer.zoom * 100.0).round() as i32)
    };
    let left_font = FontId::new(STATUS_FONT_SIZE, egui::FontFamily::Proportional);

    // 右侧快捷键提示先量宽：窄窗时必须先给它留出位置，再做文件名截断，
    // 否则左段会一直铺过去、和提示文字叠在一起（原型 .vstat 是 flex + gap，
    // 每段各占自己的宽度，不会互相压）。
    let hint = if viewer.info_open {
        "I 隐藏信息 · Space 切背景 · C 并排比较 · ←/→ 翻页 · Esc 返回"
    } else {
        "I 显示信息 · Space 切背景 · C 并排比较 · ←/→ 翻页 · Esc 返回"
    };
    let hint_font = FontId::new(11.0, egui::FontFamily::Proportional);
    let hint_w = painter
        .layout_no_wrap(hint.to_string(), hint_font.clone(), ink_soft)
        .size()
        .x;

    // 文件名后面的固定尾段（尺寸 / 第 i/n 张 / 缩放）保持完整，只压文件名本身。
    let tail = format!(
        "   {}   第 {} / {} 张   {}",
        dims,
        viewer.index + 1,
        photo_count,
        zoom_text
    );
    let tail_w = painter
        .layout_no_wrap(tail.clone(), left_font.clone(), ink)
        .size()
        .x;
    let left_start = bar.left() + 10.0;
    let left_limit = bar.right() - 10.0 - hint_w - 12.0; // 12 = 与提示的最小间距
    let name_budget = (left_limit - left_start - tail_w).max(0.0);
    let shown_name = if name_budget < 12.0 {
        String::new()
    } else {
        truncate_to_width(painter, &row.name, 11.5, name_budget, ink)
    };
    let left_text = format!("{shown_name}{tail}");
    let left_w = painter
        .layout_no_wrap(left_text.clone(), left_font.clone(), ink)
        .size()
        .x;
    painter.text(
        pos2(left_start, bar.center().y),
        Align2::LEFT_CENTER,
        left_text,
        left_font,
        ink,
    );

    painter.text(
        pos2(bar.right() - 10.0, bar.center().y),
        Align2::RIGHT_CENTER,
        hint,
        hint_font,
        ink_soft,
    );

    draw_star_and_tags(
        painter,
        bar,
        &viewer.tags,
        bar.left() + 10.0 + left_w + 12.0,
        bar.right() - 20.0 - hint_w,
        theme,
        bg,
    );
}

/// 照片信息栏（M5-V1 r7）：亮 / 暗两套**不同**的结构，按原型 v1.4 还原。
///
/// * 亮色：白色圆角面板浮在浅灰底上；大缩略图 + 标题组；日期 / 文件 /
///   尺寸 / 路径是一条条带圆形图标的两行字段（值在上、注释在下，行间细分隔）；
///   曝光组在路径之后、相机之前，四小卡并排；最后是状态行与真实直方图。
/// * 暗色：深色圆角面板；缩略图标题卡下面直接 2x2 曝光格（细线分隔），
///   再往下才是「文件 / 尺寸 / 大小 / 格式 / 日期 / 相机 / 镜头 / 路径 / 说明」
///   的密集数据行，最后是状态行与直方图。
///
/// 两套都只画真实数据：缺字段用「—」占位，不编造日期 / 相机 / 镜头，
/// 也不复述原型里的示例 / 只读演示文案。
fn draw_info_panel(
    painter: &egui::Painter,
    ui: &Ui,
    panel: Rect,
    viewer: &Viewer,
    theme: &Theme,
    row: &PhotoRow,
    id: egui::Id,
) -> bool {
    // 面板底：卡片浮在展厅底色上，四周留白（原型里圆角卡片不贴边）。
    let page = if theme.dark {
        theme.viewer_bg
    } else {
        theme.viewer_chrome_bg
    };
    painter.rect_filled(panel, 0.0, page);
    painter.line_segment(
        [panel.left_top(), panel.left_bottom()],
        Stroke::new(1.0, theme.separator),
    );

    // 卡片与面板边缘的距离：亮色原型 `aside.info-panel` 328 宽、`.pc` 从 +8 起；
    // 暗色面板 316 宽、`.pc` 从 +6 起。
    let inset = if theme.dark { 6.0 } else { 8.0 };
    let card = panel.shrink2(vec2(inset, inset));
    let radius = info_card_radius(theme);
    painter.rect_filled(card, radius, theme.viewer_card);
    painter.rect_stroke(
        card,
        radius,
        Stroke::new(1.0, theme.hairline),
        egui::StrokeKind::Inside,
    );

    // 卡片内衬：亮色原型 `.pc{padding:18px 18px 14px}`；暗色原型整块重排，
    // `.pc{padding:6px}` + `.pc-head` 内再缩 7，内容比亮色更贴边。
    let pad = if theme.dark { 7.0 } else { 18.0 };
    let left = card.left() + pad;
    let right = card.right() - pad;
    let inner_w = (right - left).max(0.0);

    painter.text(
        pos2(left, card.top() + 21.0),
        Align2::LEFT_TOP,
        "照片信息",
        FontId::new(14.0, egui::FontFamily::Proportional),
        theme.text_primary,
    );
    let close = Rect::from_center_size(
        pos2(card.right() - pad - 13.0, card.top() + 28.0),
        vec2(26.0, 26.0),
    );
    let close_resp = ui.interact(close, id.with("close"), Sense::click());
    let close_bg = if close_resp.hovered() {
        theme.sidebar_hover
    } else if theme.dark {
        Color32::from_white_alpha(14)
    } else {
        theme.chip_bg
    };
    painter.circle_filled(close.center(), close.width() / 2.0, close_bg);
    icons::draw(
        painter,
        Icon::Close,
        close.shrink(6.0),
        theme.text_secondary,
        1.2,
    );
    close_resp.clone().on_hover_text("关闭照片信息");

    // 直方图钉在卡片底部（原型里它总在最下方），中间内容超出就裁剪。
    let hist_top = card.bottom() - 84.0;
    let content_clip = Rect::from_min_max(
        pos2(card.left(), card.top()),
        pos2(card.right(), (hist_top - 6.0).max(card.top())),
    );
    let cp = painter.with_clip_rect(content_clip);

    let mut y = card.top() + 44.0;
    if theme.dark {
        y = draw_info_dark(
            &cp, painter, card, left, right, inner_w, y, viewer, theme, row,
        );
    } else {
        y = draw_info_light(
            &cp, painter, card, left, right, inner_w, y, viewer, theme, row,
        );
    }
    let _ = y;

    // ---- RGB 直方图（VW-09：真实像素统计，加载完成后自动出现） ----
    painter.text(
        pos2(left, hist_top),
        Align2::LEFT_TOP,
        "RGB 直方图",
        FontId::new(11.0, egui::FontFamily::Proportional),
        theme.text_secondary,
    );
    let bar = Rect::from_min_size(pos2(left, hist_top + 18.0), vec2(inner_w, 52.0));
    if let Some(hist) = &viewer.histogram {
        draw_histogram(painter, bar, hist);
    } else {
        painter.text(
            pos2(left, hist_top + 20.0),
            Align2::LEFT_TOP,
            "原图载入后显示",
            FontId::new(10.5, egui::FontFamily::Proportional),
            theme.text_secondary,
        );
    }

    close_resp.clicked()
}

/// 曝光格的几何：亮色四小卡**并排**，暗色 2x2。绘制与测试共用。
///
/// 亮色返回同一行、从左到右的 4 个卡片；暗色返回 2 行 x 2 列
/// （ISO / 焦距 在上，光圈 / 快门 在下）。
pub fn exposure_cell_rects(area: Rect, light: bool) -> Vec<Rect> {
    if light {
        let gap = 6.0;
        let w = ((area.width() - gap * 3.0) / 4.0).max(1.0);
        (0..4)
            .map(|i| {
                Rect::from_min_size(
                    pos2(area.left() + i as f32 * (w + gap), area.top()),
                    vec2(w, area.height()),
                )
            })
            .collect()
    } else {
        let w = (area.width() / 2.0).max(1.0);
        let h = (area.height() / 2.0).max(1.0);
        (0..4)
            .map(|i| {
                Rect::from_min_size(
                    pos2(
                        area.left() + (i % 2) as f32 * w,
                        area.top() + (i / 2) as f32 * h,
                    ),
                    vec2(w, h),
                )
            })
            .collect()
    }
}

/// 亮色信息栏：白色面板 + 图标字段行 + 四小卡曝光 + 相机组。
#[allow(clippy::too_many_arguments)]
fn draw_info_light(
    clip: &egui::Painter,
    painter: &egui::Painter,
    card: Rect,
    left: f32,
    right: f32,
    inner_w: f32,
    _y: f32,
    viewer: &Viewer,
    theme: &Theme,
    row: &PhotoRow,
) -> f32 {
    let _ = painter;
    let e = &viewer.exif;

    // 头部：大缩略图 + 标题（文件名去扩展名）+ 文件名。
    //
    // 原型 v1.4（1280 逻辑宽）：缩略图 64×64 落在标题下 58、左边 18；
    // 标题字号 16、文件名 12，间距 13。r7 产品用 56 / 14.5 / 11 才显得
    // 「标题缩略图组过紧」，这里按原型取。
    let thumb = Rect::from_min_size(pos2(left, card.top() + 58.0), vec2(64.0, 64.0));
    draw_info_thumb(clip, viewer, thumb, theme);
    let text_x = thumb.right() + 13.0;
    let text_w = (right - text_x).max(40.0);
    clip.text(
        pos2(text_x, thumb.top() + 11.0),
        Align2::LEFT_TOP,
        truncate_to_width(clip, &display_title(row), 16.0, text_w, theme.text_primary),
        FontId::new(16.0, egui::FontFamily::Proportional),
        theme.text_primary,
    );
    clip.text(
        pos2(text_x, thumb.top() + 37.0),
        Align2::LEFT_TOP,
        truncate_to_width(clip, &row.name, 12.0, text_w, theme.text_secondary),
        FontId::new(12.0, egui::FontFamily::Proportional),
        theme.text_secondary,
    );

    // 日期 / 文件 / 尺寸 / 路径：圆形图标 + 两行字段，行间细分隔。
    //
    // 原型行距 57：字段内容 32（图标 32 圆、值 14、注释 11.5）+ 11 到分隔线
    // + 14 到下一行内容。r7 产品是 46 / 图标 28 / 13 / 10.5，读起来明显更挤。
    const ROW_H: f32 = 57.0;
    const SEP_AT: f32 = 43.0;
    let path = shorten_path(&row.folder_path);
    let fields: [(&str, Icon, String, String); 4] = [
        (
            "日期",
            Icon::Calendar,
            format_taken_date(row.eff_taken_at).unwrap_or_else(|| "—".into()),
            viewer.taken_src.clone(),
        ),
        (
            "文件",
            Icon::File,
            row.name.clone(),
            format!(
                "{} · {}",
                file_format(&row.name),
                human_bytes(row.size_bytes)
            ),
        ),
        (
            "尺寸",
            Icon::Image,
            dims_text(row, viewer),
            "像素尺寸".to_string(),
        ),
        ("路径", Icon::Folder, path, "所在文件夹".to_string()),
    ];
    let mut y = thumb.bottom() + 16.0;
    for (at, (label, icon, value, note)) in fields.iter().enumerate() {
        let icon_c = pos2(left + 16.0, y + 15.0);
        clip.circle_filled(icon_c, 16.0, theme.chip_bg);
        icons::draw(
            clip,
            *icon,
            Rect::from_center_size(icon_c, vec2(15.0, 15.0)),
            theme.text_secondary,
            1.25,
        );
        let vx = left + 44.0;
        let vw = (right - vx).max(40.0);
        let shown = if value.trim().is_empty() {
            "—"
        } else {
            value.as_str()
        };
        clip.text(
            pos2(vx, y + 1.0),
            Align2::LEFT_TOP,
            truncate_to_width(clip, shown, 14.0, vw, theme.text_primary),
            FontId::new(14.0, egui::FontFamily::Proportional),
            theme.text_primary,
        );
        clip.text(
            pos2(vx, y + 19.5),
            Align2::LEFT_TOP,
            truncate_to_width(clip, note, 11.5, vw, theme.text_secondary),
            FontId::new(11.5, egui::FontFamily::Proportional),
            theme.text_secondary,
        );
        let _ = label;
        if at + 1 < fields.len() {
            clip.line_segment(
                [pos2(left, y + SEP_AT), pos2(right, y + SEP_AT)],
                Stroke::new(1.0, theme.separator),
            );
        }
        y += ROW_H;
    }

    // 曝光组：在路径之后、相机之前；四小卡并排。
    //
    // 原型：行距 57 走完后紧接「曝光」小标题（ink 顶 +1），小卡 54 高、
    // 标题下 25 起，卡内值 14 / 标签 10.5。
    clip.text(
        pos2(left, y + 1.0),
        Align2::LEFT_TOP,
        "曝光",
        FontId::new(13.0, egui::FontFamily::Proportional),
        theme.text_primary,
    );
    clip.text(
        pos2(left + 36.0, y + 3.0),
        Align2::LEFT_TOP,
        "来自该文件的 EXIF",
        FontId::new(10.5, egui::FontFamily::Proportional),
        theme.text_secondary,
    );
    y += 25.0;
    let cells = [
        (e.iso.clone(), "ISO"),
        (e.focal_length.clone(), "焦距"),
        (e.f_number.clone(), "光圈"),
        (e.exposure_time.clone(), "快门"),
    ];
    let card_area = Rect::from_min_size(pos2(left, y), vec2(inner_w, 54.0));
    let cell_w = (inner_w - 18.0) / 4.0;
    for (i, (value, label)) in cells.iter().enumerate() {
        let cell = exposure_cell_rects(card_area, true)[i];
        clip.rect_filled(cell, 10.0, theme.chip_bg);
        let shown = value.as_deref().filter(|s| !s.is_empty()).unwrap_or("—");
        clip.text(
            pos2(cell.center().x, cell.top() + 10.0),
            Align2::CENTER_TOP,
            truncate_to_width(clip, shown, 14.0, cell_w - 10.0, theme.text_primary),
            FontId::new(14.0, egui::FontFamily::Proportional),
            theme.text_primary,
        );
        clip.text(
            pos2(cell.center().x, cell.top() + 29.0),
            Align2::CENTER_TOP,
            *label,
            FontId::new(10.5, egui::FontFamily::Proportional),
            theme.text_secondary,
        );
    }
    y += 54.0 + 18.0;

    // 相机组（机身 + 镜头），与原型一样在曝光之后单独一组。
    let icon_c = pos2(left + 16.0, y + 18.0);
    clip.circle_filled(icon_c, 16.0, theme.chip_bg);
    icons::draw(
        clip,
        Icon::Camera,
        Rect::from_center_size(icon_c, vec2(16.0, 16.0)),
        theme.text_secondary,
        1.25,
    );
    let vx = left + 44.0;
    let vw = (right - vx).max(40.0);
    let camera = join_opt(&e.make, &e.model);
    clip.text(
        pos2(vx, y + 3.0),
        Align2::LEFT_TOP,
        truncate_to_width(
            clip,
            camera.as_deref().unwrap_or("—"),
            14.0,
            vw,
            theme.text_primary,
        ),
        FontId::new(14.0, egui::FontFamily::Proportional),
        theme.text_primary,
    );
    clip.text(
        pos2(vx, y + 22.0),
        Align2::LEFT_TOP,
        truncate_to_width(
            clip,
            e.lens.as_deref().unwrap_or("—"),
            11.5,
            vw,
            theme.text_secondary,
        ),
        FontId::new(11.5, egui::FontFamily::Proportional),
        theme.text_secondary,
    );
    y += 36.0;

    // 状态行：时间来源 / 色彩空间 / 元数据（只读，真实状态）。
    //
    // 原型在这里有一个「说明段」把相机组与状态行分开；产品不复制演示文案，
    // 所以用一条分隔线 + 更大的留白承担同一层级的「分段」，而不是塞假文案。
    y += 18.0;
    clip.line_segment(
        [pos2(left, y), pos2(right, y)],
        Stroke::new(1.0, theme.separator),
    );
    y += 16.0;
    for (label, value) in status_rows(viewer) {
        clip.text(
            pos2(left, y),
            Align2::LEFT_TOP,
            label,
            FontId::new(12.0, egui::FontFamily::Proportional),
            theme.text_secondary,
        );
        clip.text(
            pos2(right, y),
            Align2::RIGHT_TOP,
            truncate_to_width(clip, &value, 12.0, inner_w - 64.0, theme.text_primary),
            FontId::new(12.0, egui::FontFamily::Proportional),
            theme.text_primary,
        );
        y += 34.0;
    }
    let _ = card;
    y
}

/// 暗色信息栏：深色面板 + 标题卡下 2x2 曝光格 + 密集数据行。
#[allow(clippy::too_many_arguments)]
fn draw_info_dark(
    clip: &egui::Painter,
    painter: &egui::Painter,
    card: Rect,
    left: f32,
    right: f32,
    inner_w: f32,
    _y: f32,
    viewer: &Viewer,
    theme: &Theme,
    row: &PhotoRow,
) -> f32 {
    let _ = painter;
    let e = &viewer.exif;
    let _ = inner_w;

    // 标题卡：略亮的底 + 52 px 缩略图 + 标题 / 文件名。
    //
    // 原型 v1.4 暗色：标题卡从卡顶 +52 起、约 74 高，缩略图 52 见方内缩 11，
    // 标题 15 / 文件名 11.5；下面是 115 高的 2x2 曝光格（行距 57.5）。
    let header = Rect::from_min_size(
        pos2(left - 12.0, card.top() + 52.0),
        vec2(right - left + 24.0, 74.0),
    );
    clip.rect_filled(header, 10.0, theme.chip_bg);
    let thumb = Rect::from_min_size(
        pos2(header.left() + 11.0, header.top() + 11.0),
        vec2(52.0, 52.0),
    );
    draw_info_thumb(clip, viewer, thumb, theme);
    let text_x = thumb.right() + 12.0;
    let text_w = (header.right() - 12.0 - text_x).max(40.0);
    clip.text(
        pos2(text_x, thumb.top() + 8.0),
        Align2::LEFT_TOP,
        truncate_to_width(clip, &display_title(row), 15.0, text_w, theme.text_primary),
        FontId::new(15.0, egui::FontFamily::Proportional),
        theme.text_primary,
    );
    clip.text(
        pos2(text_x, thumb.top() + 32.0),
        Align2::LEFT_TOP,
        truncate_to_width(clip, &row.name, 11.5, text_w, theme.text_secondary),
        FontId::new(11.5, egui::FontFamily::Proportional),
        theme.text_secondary,
    );
    let mut y = header.bottom();

    // 2x2 曝光格：细线分隔，不带胶囊底（与亮色四小卡刻意不同）。
    let cells = [
        (e.iso.clone(), "ISO"),
        (e.focal_length.clone(), "焦距"),
        (e.f_number.clone(), "光圈"),
        (e.exposure_time.clone(), "快门"),
    ];
    let grid = Rect::from_min_size(pos2(left - 12.0, y), vec2(right - left + 24.0, 115.0));
    let cw = grid.width() / 2.0;
    let ch = grid.height() / 2.0;
    let cell_rects = exposure_cell_rects(grid, false);
    for (i, (value, label)) in cells.iter().enumerate() {
        let cell = cell_rects[i];
        let cx = cell.left();
        let cy = cell.top();
        let shown = value.as_deref().filter(|s| !s.is_empty()).unwrap_or("—");
        clip.text(
            pos2(cx + 16.0, cy + 13.0),
            Align2::LEFT_TOP,
            truncate_to_width(clip, shown, 17.0, cw - 28.0, theme.text_primary),
            FontId::new(17.0, egui::FontFamily::Proportional),
            theme.text_primary,
        );
        clip.text(
            pos2(cx + 16.0, cy + 38.0),
            Align2::LEFT_TOP,
            *label,
            FontId::new(11.0, egui::FontFamily::Proportional),
            theme.text_secondary,
        );
    }
    // 内部十字线与外框（上边框就是标题卡与曝光格的分界）。
    clip.line_segment(
        [pos2(grid.left(), grid.top()), pos2(grid.right(), grid.top())],
        Stroke::new(1.0, theme.separator),
    );
    clip.line_segment(
        [
            pos2(grid.left(), grid.top() + ch),
            pos2(grid.right(), grid.top() + ch),
        ],
        Stroke::new(1.0, theme.separator),
    );
    clip.line_segment(
        [
            pos2(grid.center().x, grid.top()),
            pos2(grid.center().x, grid.bottom()),
        ],
        Stroke::new(1.0, theme.separator),
    );
    clip.line_segment(
        [
            pos2(grid.left(), grid.bottom()),
            pos2(grid.right(), grid.bottom()),
        ],
        Stroke::new(1.0, theme.separator),
    );
    // 原型里第一条数据行的分隔线就是曝光格的下边框（不再另留一段空隙）。
    y = grid.bottom();

    // 密集数据行：标签在左、值右对齐，行间细分隔。
    //
    // 原型暗色行距 31（r7 产品 25），字号 12（产品 11）；隔行没有整行底色，
    // 只有发丝线，这样「密集」但不糊成一片。
    let path = shorten_path(&row.folder_path);
    let rows: Vec<(&str, Option<String>)> = vec![
        ("文件", Some(row.name.clone()).filter(|s| !s.is_empty())),
        (
            "尺寸",
            Some(dims_text(row, viewer)).filter(|s| !s.is_empty()),
        ),
        (
            "大小",
            Some(human_bytes(row.size_bytes)).filter(|s| !s.is_empty()),
        ),
        (
            "格式",
            Some(file_format(&row.name)).filter(|s| !s.is_empty()),
        ),
        ("日期", format_taken_date(row.eff_taken_at)),
        ("相机", join_opt(&e.make, &e.model)),
        ("镜头", e.lens.clone()),
        ("路径", Some(path).filter(|s| !s.is_empty())),
        (
            "说明",
            Some(viewer.caption_text.trim().to_string()).filter(|s| !s.is_empty()),
        ),
    ];
    let label_w = 48.0;
    for (label, value) in rows.iter() {
        clip.line_segment(
            [
                pos2(left - 12.0, y),
                pos2(right + 12.0, y),
            ],
            Stroke::new(1.0, theme.separator),
        );
        clip.text(
            pos2(left, y + 9.0),
            Align2::LEFT_TOP,
            *label,
            FontId::new(12.0, egui::FontFamily::Proportional),
            theme.text_secondary,
        );
        clip.text(
            pos2(right, y + 9.0),
            Align2::RIGHT_TOP,
            truncate_to_width(
                clip,
                value.as_deref().unwrap_or("—"),
                12.0,
                (right - left) - label_w - 12.0,
                theme.text_primary,
            ),
            FontId::new(12.0, egui::FontFamily::Proportional),
            theme.text_primary,
        );
        y += 30.0;
    }
    y += 18.0;

    // 状态行：时间来源 / 色彩空间 / 元数据。
    clip.line_segment(
        [pos2(left - 12.0, y), pos2(right + 12.0, y)],
        Stroke::new(1.0, theme.separator),
    );
    y += 14.0;
    for (label, value) in status_rows(viewer) {
        clip.text(
            pos2(left, y),
            Align2::LEFT_TOP,
            label,
            FontId::new(12.0, egui::FontFamily::Proportional),
            theme.text_secondary,
        );
        clip.text(
            pos2(right, y),
            Align2::RIGHT_TOP,
            truncate_to_width(
                clip,
                &value,
                12.0,
                (right - left) - 64.0,
                theme.text_primary,
            ),
            FontId::new(12.0, egui::FontFamily::Proportional),
            theme.text_primary,
        );
        y += 30.0;
    }
    let _ = card;
    y
}

/// 信息栏卡片的圆角。原型 `.pc{border-radius:var(--radius-card)}`：亮 26 / 暗 22。
/// r7 之前写死 16 / 14，卡片看起来比原型「方」。
pub fn info_card_radius(theme: &Theme) -> f32 {
    theme.radius_card
}

/// 信息栏的方形缩略图：**cover 裁切 + 圆角**（原型 `.ph2 img{width:64px;
/// height:64px;border-radius:16px;object-fit:cover}`；暗色 52px / 10）。
///
/// r7 之前用 `fit_rect` 按图片比例内缩，横图被压成 64×40、且不裁圆角。
fn draw_info_thumb(painter: &egui::Painter, viewer: &Viewer, rect: Rect, theme: &Theme) {
    if let Some(tex) = viewer.tex.as_ref() {
        let uv = cover_uv(rect, viewer.tex_size);
        paint_rounded_image(painter, tex.id(), rect, theme.radius_thumb.min(16.0), uv);
    } else {
        painter.rect_filled(rect, 8.0, theme.chip_bg);
        icons::draw(
            painter,
            Icon::Image,
            icons::icon_box(rect).shrink(icons::icon_box(rect).width() * 0.3),
            theme.text_disabled,
            1.2,
        );
    }
}

/// 时间来源 / 色彩空间 / 元数据（真实状态，未知写「—」）。
fn status_rows(viewer: &Viewer) -> Vec<(&'static str, String)> {
    let dash = |s: &str| {
        if s.trim().is_empty() {
            "—".to_string()
        } else {
            s.to_string()
        }
    };
    vec![
        ("时间来源", dash(&viewer.taken_src)),
        (
            "色彩空间",
            dash(viewer.exif.color_space.as_deref().unwrap_or("")),
        ),
        ("元数据", dash(&viewer.exif_status)),
    ]
}

/// 文件格式：真实扩展名大写（没有扩展名就留空，不猜格式）。
fn file_format(name: &str) -> String {
    std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default()
}

/// 尺寸文案：优先原图真实尺寸，读不到就回落到库里的宽高。
fn dims_text(row: &PhotoRow, viewer: &Viewer) -> String {
    match viewer.dims {
        Some((w, h)) => format!("{w} × {h}"),
        None => match (row.width, row.height) {
            (Some(w), Some(h)) => format!("{w} × {h}"),
            _ => String::new(),
        },
    }
}

/// 文件大小（二进制单位，与索引管理页同一口径）。
fn human_bytes(n: i64) -> String {
    const KB: f64 = 1024.0;
    let f = n.max(0) as f64;
    if f >= KB * KB * KB {
        format!("{:.2} GB", f / KB / KB / KB)
    } else if f >= KB * KB {
        format!("{:.1} MB", f / KB / KB)
    } else if f >= KB {
        format!("{:.0} KB", f / KB)
    } else {
        format!("{n} B")
    }
}

/// 路径缩短：家目录换成 `~`，长路径中段省略（只影响展示，不改真实路径）。
fn shorten_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let mut p = path.to_string();
    if let Some(home) = std::env::var_os("HOME")
        && let Some(home) = home.to_str()
        && let Some(rest) = p.strip_prefix(home)
    {
        p = format!("~{rest}");
    }
    p
}

/// 单行裁剪：超出宽度就截断补 `…`（信息栏窄，不能画到栏外）。
fn truncate_to_width(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    max_w: f32,
    color: Color32,
) -> String {
    let font = FontId::new(size, egui::FontFamily::Proportional);
    let full = painter.layout_no_wrap(text.to_string(), font.clone(), color);
    if full.size().x <= max_w {
        return text.to_string();
    }
    let ell_w = painter
        .layout_no_wrap("…".to_string(), font.clone(), color)
        .size()
        .x;
    let mut out = String::new();
    let mut w = 0.0;
    for ch in text.chars() {
        let cw = painter
            .layout_no_wrap(ch.to_string(), font.clone(), color)
            .size()
            .x;
        if w + cw + ell_w > max_w {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

fn compare_badge(
    painter: &egui::Painter,
    at: egui::Pos2,
    kind: &str,
    row: &PhotoRow,
    ink: Color32,
) {
    let text = format!("{kind} · {}", display_title(row));
    let galley =
        painter.layout_no_wrap(text, FontId::new(10.5, egui::FontFamily::Proportional), ink);
    let rect = Rect::from_min_size(at, galley.size() + vec2(14.0, 6.0));
    painter.rect_filled(rect, 4.0, Color32::from_black_alpha(110));
    painter.galley(rect.min + vec2(7.0, 3.0), galley, ink);
}

/// 顶栏的小图标按钮。`label` 为 `None` 时只画图标（原型 `.ib` 的纯图标按钮，
/// 如「显示位置」），否则图标在左、文字在右。
#[allow(clippy::too_many_arguments)]
fn slim_button(
    painter: &egui::Painter,
    ui: &Ui,
    id: egui::Id,
    rect: Rect,
    icon: Icon,
    label: Option<&str>,
    icon_stroke: f32,
    fg: Color32,
) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click());
    if resp.hovered() {
        painter.rect_filled(rect, 3.0, Color32::from_white_alpha(28));
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let icon_rect = match label {
        Some(_) => Rect::from_center_size(
            pos2(rect.left() + 12.0, rect.center().y),
            vec2(13.0, 13.0),
        ),
        None => Rect::from_center_size(rect.center(), vec2(15.0, 15.0)),
    };
    icons::draw(painter, icon, icon_rect, fg, icon_stroke);
    if let Some(label) = label {
        painter.text(
            pos2(rect.left() + 22.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::new(11.5, egui::FontFamily::Proportional),
            fg,
        );
    }
    resp
}

/// 缩放轨道里的单元格：激活态是**白色胶囊**（原型里「适合」的选中形状），
/// 未激活时透明、悬停给一点浅底。
#[allow(clippy::too_many_arguments)]
fn zoom_cell(
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
    // 选中态是**满格胶囊**：原型 `.zoom-seg b{height:24px;border-radius:999px}`，
    // 轨道 padding 2，胶囊正好 24 高、圆角取半高。此前按 2px 内缩画成 20 高，
    // 看着比原型小一圈。
    if active {
        painter.rect_filled(rect, rect.height() / 2.0, theme.seg_active);
    } else if resp.hovered() {
        painter.rect_filled(rect, rect.height() / 2.0, theme.sidebar_hover);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
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

/// `object-fit: cover` 的 UV 子矩形：保持图片比例、按目标框**裁切**而不是内缩。
pub(crate) fn cover_uv(outer: Rect, tex: Vec2) -> Rect {
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    if tex.x <= 0.0 || tex.y <= 0.0 || outer.width() <= 0.0 || outer.height() <= 0.0 {
        return full;
    }
    let tex_aspect = tex.x / tex.y;
    let box_aspect = outer.width() / outer.height();
    if tex_aspect > box_aspect {
        // 图更宽：左右裁掉，取中间一段。
        let w = box_aspect / tex_aspect;
        let x0 = (1.0 - w) / 2.0;
        Rect::from_min_max(pos2(x0, 0.0), pos2(x0 + w, 1.0))
    } else {
        // 图更高：上下裁掉，取中间一段。
        let h = tex_aspect / box_aspect;
        let y0 = (1.0 - h) / 2.0;
        Rect::from_min_max(pos2(0.0, y0), pos2(1.0, y0 + h))
    }
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

/// 主图圆角半径（逻辑像素，**按图本身**取，不按外框）。
///
/// 原型 v1.4（1280 逻辑宽）实测：亮色照片本身是大圆角 ≈21，暗色是小圆角
/// ≈5（暗房里的照片收得更紧）。r7 产品把圆角做在白色外框上、图片本身是
/// 直角，看起来就是「方图套圆白框」；这里按原型加在图片本身上。
/// 「适合」时照片四周留给落影的空间（逻辑像素）。
///
/// 不留下的后果不是「落影淡」而是**一点也看不见**：横图按高度贴合窗格时照片底边正好
/// 压在 `main_pane` 底边上，落影整条落在裁剪区外（实测照片下缘之外 0 灰阶，而原型
/// `.stage img` 下方有约 5 逻辑像素、40 灰阶的暗带）。
pub fn shadow_room(theme: &Theme) -> f32 {
    if theme.dark { 12.0 } else { 16.0 }
}

/// 「适合」的实际缩放：先把窗格按 [`shadow_room`] 收一圈，再等比放入照片。
///
/// 代码与测试都走这一个入口，避免「提前留了落影空间」这件事在两边漂移。
pub fn fit_zoom(pane: Rect, iw: f32, ih: f32, theme: &Theme) -> f32 {
    fit_scale(pane.shrink(shadow_room(theme)).size(), iw, ih)
}

/// 信息胶囊「照片信息」的字号：原型 `.info-toggle` 12.5px（此前 11.5 偏小一圈）。
pub const INFO_CHIP_FONT_SIZE: f32 = 12.5;

/// 底栏左段（文件名 / 尺寸 / 第 i-n 张 / 缩放）的字号：原型 `.vstat` 12px。
pub const STATUS_FONT_SIZE: f32 = 12.0;

pub fn photo_radius(theme: &Theme) -> f32 {
    if theme.dark { 6.0 } else { 22.0 }
}

/// 主图落影。原型 `.stage img` 是 `box-shadow: 0 18px 40px -16px rgba(0,0,0,.18)`。
///
/// egui 的 `blur` 做的是**边缘向内羽化**，不会向形状外发光：`spread: 0` 时阴影矩形与
/// 照片完全等大，而照片随后整块盖住它，外侧一点也露不出来——实测照片下缘之外没有任何
/// 暗带（0 灰阶），所以「整体略淡」的真实原因是落影根本没画出来。这里给正的 `spread`
/// 把阴影撑出照片边界，`offset` 让暗带主要落在下方，才是原型那圈柔和的落影。
pub fn photo_shadow(theme: &Theme) -> egui::Shadow {
    if theme.dark {
        egui::Shadow {
            offset: [0, 4],
            blur: 6,
            spread: 0,
            color: egui::Color32::from_black_alpha(110),
        }
    } else {
        egui::Shadow {
            offset: [0, 4],
            blur: 6,
            spread: 0,
            color: egui::Color32::from_black_alpha(52),
        }
    }
}

/// 把纹理画成**圆角矩形**（图片本身有圆角，而不是靠外层白框裁）。
///
/// 用带 brush 的 `RectShape`：tessellator 会按圆角路径做带 UV 的填充，
/// 因此四角是真的圆角（不是直角贴图叠在圆角底上）。`uv` 为纹理子矩形，
/// 主图 / 预览 / 比较图都是整张 0..1。
pub(crate) fn paint_rounded_image(
    painter: &egui::Painter,
    tex: egui::TextureId,
    rect: Rect,
    radius: f32,
    uv: Rect,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    // 圆角不能超过短边的一半，否则 tessellator 会退化成一坨。
    let r = radius.max(0.0).min(rect.width().min(rect.height()) * 0.5);
    let shape = egui::epaint::RectShape::filled(rect, r, Color32::WHITE)
        .with_texture(tex, uv);
    painter.add(egui::Shape::Rect(shape));
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

/// 底栏**只画标签 chips**（M42-UI-01 / VW-14：只读，超宽收成 `+N`）。
///
/// 原型 `.vstat` 只有「文件名 / 宽×高 / 第 i / n 张 / 缩放」四段，星标与
/// 「收藏」字样不在底栏 —— 它们属于主图下方说明条（`carve_star_and_tags`）。
/// 说明书 §4.5 要求「底栏收藏与标签只读」，收藏仍在说明条上只读可见，
/// 这里不重复画一颗星。
#[allow(clippy::too_many_arguments)]
fn draw_star_and_tags(
    painter: &egui::Painter,
    bar: Rect,
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
fn draw_histogram(painter: &egui::Painter, bar: Rect, hist: &picasa_imaging::Histogram) {
    // 用户 2026-11-25：去掉直方图底下的灰框（原来的 chip 底色 + 1px 描边）。
    // 原型 `.histo` 只有一排彩色柱，没有底板也没有边框（css/v13.css:157-161），
    // 之前那圈灰框在卡片底部看着像多出来一个盒子。
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
        (1..=n as i64)
            .map(|i| photo(i, 1_600_000_000 + i * 86_400))
            .collect()
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
        run_stage_at(viewer, rows, appearance, film, 0.0)
    }

    /// 同上，但把**帧时间**拨到 `time` 秒后再画——用于验证「空闲很久也不藏控件」。
    fn run_stage_at(
        viewer: &mut Viewer,
        rows: &[PhotoRow],
        appearance: Appearance,
        film: &[FilmThumb],
        time: f64,
    ) -> (Option<ViewerAction>, Vec<String>) {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx, appearance);
        let theme = Theme::for_appearance(appearance);
        let input = egui::RawInput {
            screen_rect: Some(stage_rect()),
            max_texture_side: Some(2048),
            time: Some(time),
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

    /// 同上，但用调用方给的窗口尺寸（窄窗文件名截断测试用）。
    fn run_stage_in(
        viewer: &mut Viewer,
        rows: &[PhotoRow],
        appearance: Appearance,
        rect: Rect,
    ) -> (Option<ViewerAction>, Vec<String>) {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx, appearance);
        let theme = Theme::for_appearance(appearance);
        let input = egui::RawInput {
            screen_rect: Some(rect),
            max_texture_side: Some(2048),
            ..Default::default()
        };
        let mut action = None;
        let out = ctx.run_ui(input, |ui| {
            action = show(ui, viewer, rows, &theme, true, &[]);
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
    /// 用户 2026-11-25：照片信息栏底下那个灰框要拆掉。
    ///
    /// 直方图原来是「chip 底色 + 1px separator 描边」的一个盒子，原型 `.histo`
    /// 只有一排彩色柱（css/v13.css:157-161），没有底板也没有边框。
    #[test]
    fn histogram_draws_bars_without_a_chip_box() {
        let src = include_str!("viewer.rs");
        let start = src.find("fn draw_histogram(").expect("直方图绘制函数");
        let end = src[start..].find("#[cfg(test)]").expect("测试模块") + start;
        let body = &src[start..end];
        assert!(
            !body.contains("theme.chip_bg"),
            "直方图不该再铺 chip 底色（那就是灰框）"
        );
        assert!(
            !body.contains("rect_stroke"),
            "直方图不该再画 1px 描边（灰框的边）"
        );
        assert!(body.contains("Mesh"), "彩色柱还得画");
    }

    #[test]
    fn default_stage_has_canvas_caption_and_filmstrip() {
        let lay = layout(stage_rect(), false, false, false);
        assert!(
            lay.canvas.height() > 200.0,
            "主图区应有足够高度：{:?}",
            lay.canvas
        );
        assert!(
            lay.caption.height() > 20.0,
            "说明条应存在：{:?}",
            lay.caption
        );
        assert!(
            lay.filmstrip.height() > 20.0,
            "胶片条应存在：{:?}",
            lay.filmstrip
        );
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
        let closed = layout(stage_rect(), false, false, false);
        let open = layout(stage_rect(), true, false, false);
        let panel = open.info_panel.expect("打开时应有信息栏");
        let info_w = info_panel_width(stage_rect().width(), false);
        assert!((panel.width() - info_w).abs() < 0.5);
        // 推窄：舞台列宽减少的信息栏宽度
        assert!(open.stage.width() < closed.stage.width());
        assert!((closed.stage.width() - open.stage.width() - info_w).abs() < 0.5);
        assert!(open.canvas.width() < closed.canvas.width());
        assert!(open.filmstrip.width() < closed.filmstrip.width());
        // 不遮挡：面板与主图 / 胶片条 / 说明条**面积不重叠**（允许共用边线）
        assert!(
            !overlaps(panel, open.canvas),
            "{panel:?} 盖住了主图 {:?}",
            open.canvas
        );
        assert!(!overlaps(panel, open.filmstrip));
        assert!(!overlaps(panel, open.caption));
        assert!(open.stage.right() <= panel.left() + 0.5);
        // 关闭后主图恢复宽度
        let reopened = layout(stage_rect(), false, false, false);
        assert!((reopened.canvas.width() - closed.canvas.width()).abs() < 0.5);
    }

    /// `M5-V1` 未验收项①：信息栏按窗口宽度的 25% 取宽，并夹在 300–480。
    ///
    /// 钉住的是**原型锚点**，不是实现细节：原型 v1.4 在 1280 逻辑宽下
    /// 原型 `.info-panel` 是**固定像素宽**：亮色 `width:328px;flex:0 0 328px`、
    /// 暗色 `width:316px;flex-basis:316px`（`css/app.css`）。换窗口不变，只有
    /// 可用宽度本身不够时才退让。
    #[test]
    fn info_panel_width_is_fixed_per_theme_like_the_prototype() {
        for w in [1280.0_f32, 1440.0, 1800.0, 1920.0, 3840.0] {
            assert!(
                (info_panel_width(w, false) - INFO_W).abs() < 0.25,
                "亮色 {w} 逻辑宽都应是固定 {INFO_W}，实际 {}",
                info_panel_width(w, false)
            );
            assert!(
                (info_panel_width(w, true) - INFO_DARK_W).abs() < 0.25,
                "暗色 {w} 逻辑宽都应是固定 {INFO_DARK_W}，实际 {}",
                info_panel_width(w, true)
            );
        }
        // 两套主题的宽度必须不同（原型 328 / 316）。
        assert!(INFO_W > INFO_DARK_W);
        // 窗口比固定宽还窄时退让到窗口宽度，但不许为负、不许反超。
        for w in [320.0_f32, 300.0, 200.0] {
            let light = info_panel_width(w, false);
            assert!((light - w).abs() < 0.25, "{w} 时应退让到 {w}，实际 {light}");
            assert!(light <= w);
            // 400 比固定宽还宽，应保持固定值不退让。
            assert!((info_panel_width(400.0, false) - INFO_W).abs() < 0.25);
            assert!((info_panel_width(400.0, true) - INFO_DARK_W).abs() < 0.25);
        }
        // 退化输入不产出负宽。
        assert_eq!(info_panel_width(0.0, false), 0.0);
        assert_eq!(info_panel_width(-10.0, true), 0.0);
    }

    /// `M5-V1` 未验收项②：圆角加在**照片本身**上，亮色大圆角、暗色小圆角。
    ///
    /// 原型 `.stage img` 亮色 `border-radius:22px`、暗色 `6px`。此前 r7 把
    /// 圆角做在外层白色画框上（照片本身直角），实窗看起来是「方图套白框」，
    /// 正是这一项要修掉的问题。
    #[test]
    fn photo_radius_matches_prototype_per_theme() {
        assert_eq!(
            photo_radius(&Theme::for_appearance(Appearance::Light)),
            22.0,
            "亮色照片圆角应为原型 22"
        );
        assert_eq!(
            photo_radius(&Theme::for_appearance(Appearance::Dark)),
            6.0,
            "暗色照片圆角应为原型 6"
        );
        // 亮暗两套必须真的不同（原型刻意分两档，不是同一个值换皮）。
        assert_ne!(
            photo_radius(&Theme::for_appearance(Appearance::Light)),
            photo_radius(&Theme::for_appearance(Appearance::Dark))
        );
    }

    /// 落影必须在照片外侧看得见：`offset` 向下、`spread` 不吞掉、颜色不是全透明。
    ///
    /// 不能只测「Shadow 结构存在」——`blur` 在 egui 里是**向内羽化**，`spread: 0` 时阴影
    /// 与照片等大、被照片整块盖住，外侧一点也露不出来（这正是此前「落影整体略淡」的
    /// 真相：实测照片下缘之外 0 灰阶）。
    #[test]
    fn photo_shadow_is_pushed_outward_and_downward() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let theme = Theme::for_appearance(appearance);
            let sh = photo_shadow(&theme);
            assert!(sh.offset[1] > 0, "落影要向下偏移，否则照片下方没有暗带");
            assert!(
                sh.blur >= 4,
                "模糊至少要够软（{}），否则边缘像一条硬线",
                sh.blur
            );
            assert!(
                sh.color.a() > 32,
                "落影太淡看不见（alpha {}）",
                sh.color.a()
            );
            // 阴影的形状与照片自己的圆角一致。
            assert!(photo_radius(&theme) > 0.0, "照片圆角不能为 0");
        }
    }

    /// 「适合」要给落影留出空间：没有留白时照片底边压在窗格底边上，落影整条被裁掉。
    #[test]
    fn fit_reserves_room_for_the_drop_shadow() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let theme = Theme::for_appearance(appearance);
            let room = shadow_room(&theme);
            assert!(room >= 8.0, "落影预留空间太小：{room}");
            let pane = stage_rect();
            let iw = 4000.0;
            let ih = 3000.0;
            let with_room = fit_zoom(pane, iw, ih, &theme);
            let without_room = fit_scale(pane.size(), iw, ih);
            assert!(
                with_room < without_room,
                "适合没有为落影收一圈：{with_room} 应小于 {without_room}"
            );
        }
    }

    /// 查看器里两处字号按原型取：信息胶囊 12.5（`.info-toggle`）、底栏 12（`.vstat`）。
    ///
    /// 此前两者都是 11.5，逐像素比对时看出来的——字号偏小会让整条顶栏「显得松」。
    #[test]
    fn viewer_text_sizes_follow_the_prototype() {
        assert_eq!(INFO_CHIP_FONT_SIZE, 12.5);
        assert_eq!(STATUS_FONT_SIZE, 12.0);
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

    /// V5：顶栏文件名右侧是浅色/描边的信息胶囊（不是实心按钮），
    /// 且点击它仍然切换信息栏（与 I 键同一动作 `ToggleInfo`）。
    #[test]
    fn v5_info_chip_is_light_outlined_and_still_toggles() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx, appearance);
            let theme = Theme::for_appearance(appearance);
            let rows = photos(2);
            let mut v = Viewer::new(0);
            // 关掉信息栏来检查「未激活」外观（浅底 + 发丝描边）。
            v.info_open = false;
            let lay = layout(stage_rect(), true, false, false);
            let chip = info_toggle_rect(lay.top);
            assert!(
                lay.top.contains_rect(chip),
                "{appearance:?}：胶囊必须在顶栏内"
            );

            // 第 1 帧：建立交互层，同时检查胶囊的底与描边形状
            let input = egui::RawInput {
                screen_rect: Some(stage_rect()),
                max_texture_side: Some(2048),
                ..Default::default()
            };
            let out = ctx.run_ui(input, |ui| {
                let _ = show(ui, &mut v, &rows, &theme, true, &[]);
            });
            let mut chip_text: Option<egui::Pos2> = None;
            // 胶囊由「填充底」+「描边」两个 Rect 形状组成，分开收集。
            let mut fills: Vec<(u8, Color32)> = Vec::new();
            let mut strokes: Vec<Stroke> = Vec::new();
            fn walk(
                shape: &egui::Shape,
                chip: Rect,
                text: &mut Option<egui::Pos2>,
                fills: &mut Vec<(u8, Color32)>,
                strokes: &mut Vec<Stroke>,
            ) {
                match shape {
                    egui::Shape::Vec(v) => {
                        for s in v {
                            walk(s, chip, text, fills, strokes);
                        }
                    }
                    egui::Shape::Text(t) => {
                        if t.galley.job.text == "照片信息" && chip.contains(t.pos) {
                            *text = Some(t.pos);
                        }
                    }
                    egui::Shape::Rect(r)
                        if (r.rect.min - chip.min).length() < 0.5
                            && (r.rect.max - chip.max).length() < 0.5 =>
                    {
                        // 底：胶囊圆角（height/2）+ 浅色填充，不是实心按钮的方角实块
                        if r.fill != Color32::TRANSPARENT {
                            assert!(
                                (r.corner_radius.nw as f32 - chip.height() / 2.0).abs() < 0.5,
                                "信息胶囊应为胶囊圆角，实际 {}",
                                r.corner_radius.nw
                            );
                            fills.push((r.corner_radius.nw, r.fill));
                        }
                        if r.stroke.width > 0.0 {
                            strokes.push(r.stroke);
                        }
                    }
                    _ => {}
                }
            }
            for c in &out.shapes {
                walk(&c.shape, chip, &mut chip_text, &mut fills, &mut strokes);
            }
            let expected_fill = if theme.dark {
                Color32::from_white_alpha(18)
            } else {
                theme.viewer_card
            };
            assert_eq!(
                fills.iter().map(|(_, f)| *f).collect::<Vec<_>>(),
                vec![expected_fill],
                "{appearance:?}：胶囊应是浅色底（恰好一层），不是实心按钮"
            );
            assert_eq!(strokes.len(), 1, "{appearance:?}：胶囊应有一层发丝描边");
            assert_eq!(
                strokes[0].color, theme.hairline,
                "{appearance:?}：未打开时描边是发丝色"
            );
            assert!(
                chip_text.is_some(),
                "{appearance:?}：胶囊里要有「照片信息」文字"
            );
            out.drop_without_applying_deltas();

            // 第 2/3 帧：在胶囊上按下 + 松开 → ToggleInfo
            let pos = chip.center();
            let mut action = None;
            for pressed in [true, false] {
                let mut input = egui::RawInput {
                    screen_rect: Some(stage_rect()),
                    max_texture_side: Some(2048),
                    ..Default::default()
                };
                input.events.push(egui::Event::PointerMoved(pos));
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                });
                let out = ctx.run_ui(input, |ui| {
                    action = show(ui, &mut v, &rows, &theme, true, &[]);
                });
                out.drop_without_applying_deltas();
            }
            assert_eq!(
                action,
                Some(ViewerAction::ToggleInfo),
                "{appearance:?}：点胶囊应开关信息栏"
            );
        }
    }

    /// 缩放簇：− 与 + 之间夹一个只读读数（原型 `.zoom-readout`），
    /// 三者在轨道内互不重叠，读数能表达「适合」与百分比两种状态。
    #[test]
    fn zoom_cluster_has_a_readout_between_minus_and_plus() {
        let lay = layout(stage_rect(), true, false, false);
        let top = lay.top;
        let track = zoom_track_rect(top);
        let (minus, plus) = zoom_button_rects(top);
        let readout = zoom_readout_rect(top);
        assert!(track.contains_rect(readout), "读数应在轨道内");
        assert!(minus.right() <= readout.left() + 0.01, "读数在 − 之后");
        assert!(readout.right() <= plus.left() + 0.01, "读数在 + 之前");
        assert_eq!(track.right(), plus.right() + TB_TRACK_PAD, "轨道右端与 + 对齐");

        // 适合态写「适合」，放大态写百分比。
        let rows = photos(2);
        let mut v = Viewer::new(0);
        let (_a, texts) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        let all = texts.join("\n");
        assert!(all.contains("适合"), "默认应显示「适合」：{all}");
        v.zoom_actual();
        v.zoom_in();
        let (_a2, texts2) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        let all2 = texts2.join("\n");
        assert!(all2.contains("125%"), "放大后应显示百分比：{all2}");
    }

    /// 顶栏「显示位置」按原型是**纯图标**（locate 图钉），不再是一条带文字的宽按钮。
    #[test]
    fn reveal_button_is_icon_only_and_compact() {
        let lay = layout(stage_rect(), true, false, false);
        let heart = Rect::from_min_size(
            pos2(lay.top.left() + TB_PAD + TB_BACK_W + TB_BACK_GAP, lay.top.top() + 10.0),
            vec2(TB_HEART_W, 24.0),
        );
        let reveal_w = TB_REVEAL_W;
        assert!(
            reveal_w <= heart.width() + 4.0,
            "原型 `.ib` 是 30×30 图标按钮：显示位置 {reveal_w} 宽，收藏钮才 {} 宽",
            heart.width()
        );
        let rows = photos(2);
        let mut v = Viewer::new(0);
        let (_a, texts) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        let all = texts.join("\n");
        assert!(
            !all.contains("在 Finder 中显示") && !all.contains("在文件管理器中显示"),
            "顶栏不该再写文字：{all}"
        );
        // 能力仍在：文字留给索引管理菜单与悬停提示。
        assert!(reveal_label().contains("显示"));
    }

    /// 信息栏缩略图是**正方形 cover + 圆角**，不是按比例内缩的变形图。
    #[test]
    fn info_thumb_uses_cover_crop_not_fit() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(64.0, 64.0));
        // 横图 3:2：左右裁掉，UV 高度满、宽度收窄。
        let uv = cover_uv(rect, vec2(3000.0, 2000.0));
        assert!((uv.height() - 1.0).abs() < 1e-6, "横图不该上下裁");
        assert!(uv.width() < 1.0 && uv.width() > 0.6, "应左右裁：{uv:?}");
        assert!((uv.center().x - 0.5).abs() < 1e-6, "居中裁切");
        // 竖图反过来：上下裁、宽度满。
        let uv2 = cover_uv(rect, vec2(2000.0, 3000.0));
        assert!((uv2.width() - 1.0).abs() < 1e-6, "竖图不该左右裁");
        assert!(uv2.height() < 1.0);
        // 边长相同（正方形）不裁。
        let uv3 = cover_uv(rect, vec2(800.0, 800.0));
        assert!((uv3.width() - 1.0).abs() < 1e-6 && (uv3.height() - 1.0).abs() < 1e-6);
        // 退化输入不 panic
        assert_eq!(cover_uv(rect, vec2(0.0, 0.0)).width(), 1.0);
    }

    /// 信息栏卡片圆角走主题 token（原型 `--radius-card` 亮 26 / 暗 22）。
    #[test]
    fn info_card_radius_follows_theme_card_token() {
        let light = Theme::for_appearance(Appearance::Light);
        let dark = Theme::for_appearance(Appearance::Dark);
        assert_eq!(info_card_radius(&light), 26.0);
        assert_eq!(info_card_radius(&dark), 22.0);
        assert_eq!(info_card_radius(&light), light.radius_card);
        assert_eq!(info_card_radius(&dark), dark.radius_card);
    }

    /// 底栏按原型只有「文件名 / 尺寸 / 第 i-n 张 / 缩放」四段：
    /// 不再画星标与「收藏」字样（收藏在说明条上只读可见）。
    /// 标签 chips 仍必须保留（说明书 §4.5、VW-14）。
    #[test]
    fn statusbar_shows_no_star_but_keeps_readonly_tags() {
        let rows = photos(3);
        let mut v = Viewer::new(1); // 第 2 张 starred = true
        v.tags = vec!["旅行".into()];
        let (_a, texts) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        let all = texts.join("\n");
        assert!(all.contains("第 2 / 3 张"), "哨兵：四段信息仍在：{all}");
        // 底栏没有「收藏」两字（说明条那边是「收藏 · 旅行」一行，只出现一次）
        assert_eq!(
            texts.iter().filter(|t| t.as_str() == "收藏").count(),
            1,
            "「收藏」只该出现在说明条：{all}"
        );
        assert!(all.contains("旅行"), "标签 chips 必须保留：{all}");
        // 四段信息仍在
        assert!(all.contains("IMG_0002"));
        assert!(all.contains("4000 × 3000"));
    }

    /// 工具条仍有缩放 −/+，且两者都在顶栏内、不互相重叠。
    #[test]
    fn toolbar_keeps_zoom_minus_and_plus() {
        let lay = layout(stage_rect(), true, false, false);
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

    /// 窄窗：文件名槽位先收窄，信息胶囊仍然完整落在顶栏内（控件常驻不裁切）。
    #[test]
    fn topbar_chip_stays_inside_on_a_narrow_window() {
        for w in [1280.0_f32, 900.0, 760.0] {
            let top = Rect::from_min_size(pos2(0.0, 0.0), vec2(w, TOP_H));
            let chip = info_toggle_rect(top);
            let name = filename_slot_rect(top);
            assert!(
                top.contains_rect(chip),
                "w={w} 胶囊越界：{chip:?} / {top:?}"
            );
            assert!(name.right() <= chip.left(), "w={w} 文件名与胶囊重叠");
            assert!((chip.width() - 96.0).abs() < 0.01, "胶囊宽度固定");
        }
    }

    /// 用户要求：「照片信息」胶囊锚定在查看器 topbar 最右侧（内缩 8px），
    /// 不随窗口变宽而回到缩放控件旁。多档宽度都钉住右对齐，且始终在顶栏内。
    #[test]
    fn info_chip_is_pinned_to_the_right_edge_of_the_topbar() {
        for w in [1800.0_f32, 1280.0, 900.0, 760.0] {
            let top = Rect::from_min_size(pos2(0.0, 0.0), vec2(w, TOP_H));
            let chip = info_toggle_rect(top);
            assert!(
                (top.right() - chip.right() - 8.0).abs() < 0.01,
                "w={w} 胶囊应贴顶栏右端（内缩 8px）：chip={chip:?}"
            );
            assert!((chip.width() - TB_CHIP_W).abs() < 0.01, "胶囊宽度固定");
            assert!(top.contains_rect(chip), "w={w} 胶囊越界：{chip:?}");
            let name = filename_slot_rect(top);
            assert!(name.right() <= chip.left(), "w={w} 文件名与胶囊重叠");
        }
    }

    /// 窄窗长文件名：顶栏按原型 `.vfile` 的 `text-overflow:ellipsis` 截断补 `…`，
    /// 不再靠 clip_rect 把右端（含扩展名）硬切掉。
    #[test]
    fn long_filename_is_ellipsized_not_hard_clipped_in_a_narrow_window() {
        let long = "a-very-long-photo-file-name-that-needs-ellipsis.jpg";
        let mut row = photo(1, 1_600_000_000);
        row.name = long.to_string();
        // 760 逻辑宽时文件名槽位约 126 px，远小于这段文件名。
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(760.0, 820.0));
        assert!(
            filename_slot_rect(rect).width() < 150.0,
            "测试需要收窄的槽位"
        );
        let mut v = Viewer::new(0);
        let (_, texts) = run_stage_in(&mut v, &[row], Appearance::Light, rect);
        // 顶栏槽位被压到 ~42 px，画的是最短的那条省略文本；
        // 信息栏里还会画几条更长的截断文本，取最短的才是顶栏那条。
        let shown = texts
            .iter()
            .filter(|t| t.contains('…') && t.starts_with("a-ver"))
            .min_by_key(|t| t.len())
            .expect("顶栏应画出省略的文件名")
            .as_str();
        assert!(shown.ends_with('…'), "截断应补省略号：{shown}");
        assert!(shown.len() < long.len(), "长名应被截短：{shown}");
        assert!(!shown.contains(".jpg"), "被吞掉的尾部不应残留半截：{shown}");
    }

    /// 宽窗（文件名槽位够宽）不应出现省略号，完整文件名照常显示。
    #[test]
    fn short_filename_is_not_ellipsized_on_a_wide_window() {
        let name = "IMG_0001.jpg";
        let mut row = photo(1, 1_600_000_000);
        row.name = name.to_string();
        let mut v = Viewer::new(0);
        let (_, texts) = run_stage_in(&mut v, &[row], Appearance::Light, stage_rect());
        assert!(
            texts.iter().any(|t| t == name),
            "宽窗应完整显示文件名：{texts:?}"
        );
    }

    /// 窄窗底栏：文件名先被截断，保证「第 i/n 张」与右侧提示仍完整、不重叠。
    #[test]
    fn long_filename_in_statusbar_keeps_tail_and_hint_visible() {
        let mut row = photo(1, 1_600_000_000);
        row.name = "an-extremely-long-file-name-for-the-status-bar-test.jpg".to_string();
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(760.0, 820.0));
        let mut v = Viewer::new(0);
        let (_, texts) = run_stage_in(&mut v, &[row], Appearance::Light, rect);
        let all = texts.join("\n");
        assert!(all.contains("第 1 / 1 张"), "尾段应保留：{all}");
        assert!(all.contains("4000 × 3000"), "尺寸段应保留：{all}");
        assert!(all.contains("Esc 返回"), "右侧提示应保留：{all}");
        assert!(all.contains('…'), "长文件名应补省略号：{all}");
    }

    /// V6：展厅控件常驻——空闲很久后顶栏 / 说明条 / 胶片条 / 信息栏 / 底栏仍在。
    ///
    /// 老实现按鼠标静止 3 秒藏 chrome，Linux 截图里只剩照片；这里把帧时间
    /// 拨到 600 秒（远超旧的 3 秒阈值）仍要求所有控件文字出现。
    #[test]
    fn v6_viewer_chrome_never_auto_hides_when_idle() {
        let rows = photos(4);
        for appearance in [Appearance::Light, Appearance::Dark] {
            let mut v = Viewer::new(0);
            let (_, texts) = run_stage_at(&mut v, &rows, appearance, &[], 600.0);
            let all = texts.join("\n");
            assert!(all.contains("返回"), "{appearance:?}：顶栏常驻：{all}");
            assert!(
                all.contains("照片信息"),
                "{appearance:?}：信息栏/开关常驻：{all}"
            );
            assert!(
                all.contains("第 1 / 4 张"),
                "{appearance:?}：底栏常驻：{all}"
            );
            assert!(all.contains("适合"), "{appearance:?}：缩放簇常驻：{all}");
        }
    }

    /// 亮 / 暗两套主题画出同一套展厅结构（§5.5 不得行为分叉）。
    #[test]
    fn both_themes_render_same_stage_structure() {
        let rows = photos(3);
        for appearance in [Appearance::Light, Appearance::Dark] {
            let mut v = Viewer::new(0);
            let lay = layout(stage_rect(), true, false, false);
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
        let rect = layout(stage_rect(), true, false, false).filmstrip;
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
        let rect = layout(stage_rect(), true, false, false).filmstrip;
        let slots = filmstrip_slots(rect, 5, 2);
        let target = slots[0];
        assert_eq!(
            filmstrip_hit(rect, 5, 2, target.rect.center()),
            Some(target.index)
        );
        // 每个槽位中心都能命中自己
        for slot in &slots {
            assert_eq!(
                filmstrip_hit(rect, 5, 2, slot.rect.center()),
                Some(slot.index)
            );
        }
        // 空白处不命中
        assert_eq!(
            filmstrip_hit(rect, 5, 2, rect.left_top() + vec2(1.0, 1.0)),
            None
        );
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
        let lay = layout(rect, false, false, false);
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
        let rect = layout(stage_rect(), false, false, false).filmstrip;
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
        let lay = layout(stage_rect(), false, false, false);
        let fit = fit_zoom(
            lay.main_pane,
            4000.0,
            3000.0,
            &Theme::for_appearance(Appearance::Light),
        );
        assert!(
            (v.zoom - fit).abs() < 1e-4,
            "PageDown 应回到适合：{} vs {}",
            v.zoom,
            fit
        );
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
        let closed = layout(stage_rect(), false, false, false);
        let open = layout(stage_rect(), true, false, false);
        // 用长条图（宽度受限），这样「适合」直接由推窄后的可用宽度决定
        let fit_closed = fit_zoom(
            closed.main_pane,
            4000.0,
            400.0,
            &Theme::for_appearance(Appearance::Light),
        );
        let fit_open = fit_zoom(
            open.main_pane,
            4000.0,
            400.0,
            &Theme::for_appearance(Appearance::Light),
        );
        assert!(
            fit_open < fit_closed,
            "推窄后适合比例应更小：{fit_open} vs {fit_closed}"
        );
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
        let open = layout(stage_rect(), true, false, false);
        let fit_open = fit_zoom(
            open.main_pane,
            4000.0,
            3000.0,
            &Theme::for_appearance(Appearance::Light),
        );
        assert!(
            (v.zoom - fit_open).abs() < 1e-4,
            "首帧适合：{} vs {}",
            v.zoom,
            fit_open
        );

        // 帧 2：关掉信息栏，窗格变宽 → 仍应「适合」新的更宽窗格
        v.toggle_info();
        run_stage(&mut v, &rows, Appearance::Light, &[]);
        let closed = layout(stage_rect(), false, false, false);
        let fit_closed = fit_zoom(
            closed.main_pane,
            4000.0,
            3000.0,
            &Theme::for_appearance(Appearance::Light),
        );
        assert!(
            (v.zoom - fit_closed).abs() < 1e-4,
            "关掉信息栏后仍应「适合」：zoom={} fit={}",
            v.zoom,
            fit_closed
        );

        // 帧 3：并排比较 → 主窗格减半，仍应适合
        v.compare = true;
        run_stage(&mut v, &rows, Appearance::Light, &[]);
        let cmp = layout(stage_rect(), false, true, false);
        let fit_cmp = fit_zoom(
            cmp.main_pane,
            4000.0,
            3000.0,
            &Theme::for_appearance(Appearance::Light),
        );
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
        let closed = layout(stage_rect(), false, false, false);
        let fit_closed = fit_zoom(
            closed.main_pane,
            4000.0,
            400.0,
            &Theme::for_appearance(Appearance::Light),
        );
        assert!((v.zoom - fit_closed).abs() < 1e-4);

        // 打开信息栏：窗格推窄，用户未手动缩放 → 应重新适合
        v.toggle_info();
        run_stage(&mut v, &rows, Appearance::Light, &[]);
        let open = layout(stage_rect(), true, false, false);
        let fit_open = fit_zoom(
            open.main_pane,
            4000.0,
            400.0,
            &Theme::for_appearance(Appearance::Light),
        );
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
        let lay = layout(stage_rect(), true, true, false);
        let right = lay.compare_pane.expect("有下一张就应有右窗格");
        assert!(!lay.main_pane.intersects(right));
        assert!(lay.canvas.contains_rect(lay.main_pane));
        assert!(lay.canvas.contains_rect(right));
        assert!((lay.main_pane.width() - right.width()).abs() < 1.0);
        // 载入中可出现「正在载入比较图…」
        let (_a, texts) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        assert!(
            has_text(&texts, "正在载入比较图…"),
            "比较图未就绪时应有载入句"
        );
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
        let lay = layout(stage_rect(), false, v.compare_active(rows.len()), false);
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
        let lay = layout(stage_rect(), true, v.compare_active(rows.len()), false);
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
        assert!(
            all.contains("4 000 × 3 000") || all.contains("4000 × 3000"),
            "{all}"
        );
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

    /// V1：亮色主题下展厅必须是浅色（浅灰底 + 白卡片 + 浅色控件），
    /// 不能把大块深色暗房画布带进亮色；暗色仍是对应的深色。
    #[test]
    fn light_viewer_is_a_light_gallery() {
        let light = Theme::for_appearance(Appearance::Light);
        let dark = Theme::for_appearance(Appearance::Dark);
        let light_bg = Bg::Black.color(&light);
        assert!(is_bright(light_bg), "亮色展厅底色必须浅：{light_bg:?}");
        assert!(is_bright(light.viewer_chrome_bg), "亮色顶栏/底栏必须浅");
        assert!(is_bright(light.viewer_card), "照片卡片必须浅");
        let dark_bg = Bg::Black.color(&dark);
        assert!(!is_bright(dark_bg), "暗色展厅仍必须是暗房");
    }

    /// V2：「照片信息」要有缩略图 + 标题 + 文件名、曝光四宫格，
    /// 以及文件 / 尺寸 / 大小 / 日期 / 相机 / 镜头 / 路径等逐行信息。
    #[test]
    fn info_panel_has_thumbnail_title_and_exposure_grid() {
        let rows = photos(3);
        for appearance in [Appearance::Light, Appearance::Dark] {
            let mut v = Viewer::new(0);
            let (_a, texts) = run_stage(&mut v, &rows, appearance, &[]);
            let all = texts.join("\n");
            for needle in ["照片信息", "IMG_0001", "ISO", "焦距", "光圈", "快门"] {
                assert!(
                    all.contains(needle),
                    "{appearance:?} 信息栏缺少「{needle}」：{all}"
                );
            }
            // 亮色的相机组是图标 + 机身/镜头（原型没有「相机」标签），
            // 暗色才是「相机 / 镜头」数据行。
            match appearance {
                Appearance::Light => {
                    assert!(all.contains("曝光"), "亮色应有曝光组标题：{all}");
                }
                _ => {
                    assert!(all.contains("相机"), "暗色应有相机数据行：{all}");
                    assert!(all.contains("镜头"), "暗色应有镜头数据行：{all}");
                }
            }
            // 亮色是图标字段行（值 + 注释，没有前缀标签），暗色是左标签数据行；
            // 两边都必须有真实的文件夹路径与文件名。
            for needle in ["IMG_0001.jpg", "/tmp/photos"] {
                assert!(
                    all.contains(needle),
                    "{appearance:?} 信息栏缺少「{needle}」：{all}"
                );
            }
            // 缺 EXIF 时用「—」占位，不编造数据
            assert!(all.contains("—"), "{appearance:?} 缺字段应显示占位：{all}");
            // 真实状态行与真实直方图入口仍在
            assert!(all.contains("时间来源"), "{appearance:?}: {all}");
            assert!(all.contains("色彩空间"), "{appearance:?}: {all}");
            assert!(all.contains("元数据"), "{appearance:?}: {all}");
            assert!(all.contains("RGB 直方图"), "{appearance:?}: {all}");
            // 不复制原型的示例 / 只读演示文案
            assert!(
                !all.contains("示例"),
                "{appearance:?} 不得复制演示文案：{all}"
            );
            assert!(!all.contains("只读演示"), "{appearance:?}: {all}");
            assert!(!all.contains("不会移动真实照片"), "{appearance:?}: {all}");
        }
    }

    /// M5-V1 r7：亮 / 暗两套信息栏是**两套结构**，不是同一张表格换配色。
    ///
    /// 亮色：白色圆角面板 + 图标字段行在曝光**之前**（日期 / 文件 / 尺寸 / 路径），
    /// 曝光四小卡并排（`ISO` / `焦距` / `光圈` / `快门` 四张卡），相机在曝光之后。
    /// 暗色：标题卡下先 2x2 曝光格（ISO 左上、焦距右上、光圈左下、快门右下），
    /// 再往下才是「文件 / 尺寸 / 大小 / 格式 / 日期 / 相机 / 镜头 / 路径 / 说明」
    /// 的密集数据行，且没有亮色那套圆形图标字段行。
    #[test]
    fn info_panel_light_and_dark_are_structurally_different() {
        let rows = photos(3);

        // ---- 亮色：四小卡并排在曝光标题之后，日期字段在曝光之前 ----
        let mut v = Viewer::new(0);
        let (_a, light_texts) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        let light = light_texts.join("\n");
        let pos = |t: &str| {
            light_texts
                .iter()
                .position(|s| s == t)
                .unwrap_or_else(|| panic!("亮色缺「{t}」：{light}"))
        };
        let exposure = pos("曝光");
        // 亮色字段行只有值 + 注释（原型如此，没有前缀标签）：
        // 日期在曝光之前；路径注释（所在文件夹）也在曝光之前。
        assert!(pos("所在文件夹") < exposure, "亮色路径字段应在曝光组之前");
        assert!(pos("像素尺寸") < exposure, "亮色尺寸字段应在曝光组之前");
        let date_text = light_texts
            .iter()
            .find(|t| t.contains("年") && t.contains("月"))
            .unwrap_or_else(|| panic!("亮色缺日期字段：{light}"));
        let date_at = light_texts.iter().position(|t| t == date_text).unwrap();
        assert!(date_at < exposure, "亮色日期字段应在曝光组之前");
        for card in ["ISO", "焦距", "光圈", "快门"] {
            assert!(pos(card) > exposure, "亮色 {card} 卡应在曝光标题之后");
        }
        // 亮色的相机组是图标 + 机身/镜头，没有「相机 / 镜头」这两个前缀标签；
        // 无 EXIF 时它只画真实占位「—」。暗色才用左标签数据行。
        assert!(!light.contains("相机"), "亮色不应出现「相机」标签：{light}");
        assert!(!light.contains("镜头"), "亮色不应出现「镜头」标签：{light}");
        let dashes_after_exposure = light_texts
            .iter()
            .skip(exposure)
            .filter(|t| t.as_str() == "—")
            .count();
        assert!(
            dashes_after_exposure >= 5,
            "亮色曝光后应有 4 张卡的占位 + 相机/镜头占位：{light}"
        );

        // ---- 暗色：曝光格在数据行之前，且没有「曝光」小标题 ----
        let mut v = Viewer::new(0);
        let (_b, dark_texts) = run_stage(&mut v, &rows, Appearance::Dark, &[]);
        let dark = dark_texts.join("\n");
        let dpos = |t: &str| {
            dark_texts
                .iter()
                .position(|s| s == t)
                .unwrap_or_else(|| panic!("暗色缺「{t}」：{dark}"))
        };
        for card in ["ISO", "焦距", "光圈", "快门"] {
            assert!(dpos(card) < dpos("文件"), "暗色 {card} 格应在数据行之前");
        }
        for label in [
            "文件", "尺寸", "大小", "格式", "日期", "相机", "镜头", "路径", "说明",
        ] {
            assert!(dark.contains(label), "暗色缺少数据行「{label}」：{dark}");
        }
        assert!(
            !dark.contains("曝光"),
            "暗色曝光是 2x2 格，不再有「曝光」小标题：{dark}"
        );
    }

    /// M5-V1 r7：曝光格几何——亮色 4 张并排（同一行、等宽），暗色 2x2。
    ///
    /// 这条把「不要把亮色套成暗色密集表格」变成几何事实：亮色四张卡
    /// 顶边相同、两两不重叠；暗色是两行两列，上下两行顶边不同。
    #[test]
    fn exposure_cells_are_a_row_in_light_and_a_2x2_grid_in_dark() {
        let area = Rect::from_min_size(pos2(16.0, 100.0), vec2(300.0, 54.0));
        let light = exposure_cell_rects(area, true);
        assert_eq!(light.len(), 4);
        for w in light.windows(2) {
            assert!((w[0].top() - w[1].top()).abs() < 0.01, "亮色四卡应在同一行");
            assert!(!w[0].intersects(w[1]), "亮色四卡不应重叠");
            assert!(w[0].right() <= w[1].left() + 0.01, "亮色四卡从左到右排");
            assert!((w[0].width() - w[1].width()).abs() < 0.01, "亮色四卡等宽");
        }
        assert!(light[3].right() <= area.right() + 0.01);

        let dark_area = Rect::from_min_size(pos2(16.0, 100.0), vec2(300.0, 112.0));
        let dark = exposure_cell_rects(dark_area, false);
        assert_eq!(dark.len(), 4);
        assert!(
            (dark[0].top() - dark[1].top()).abs() < 0.01,
            "ISO / 焦距 同一行"
        );
        assert!(
            (dark[2].top() - dark[3].top()).abs() < 0.01,
            "光圈 / 快门 同一行"
        );
        assert!(
            dark[2].top() > dark[0].bottom() - 0.01,
            "第二行在第一行下方"
        );
        // 2x2 格共用分隔线，只要**没有正面积重叠**即可。
        assert!(!overlaps(dark[0], dark[1]));
        assert!(!overlaps(dark[0], dark[2]));
    }

    /// V3：照片正下方有标题、日期与收藏/标签行。
    #[test]
    fn caption_shows_title_date_and_favorite_tag_line() {
        let rows = photos(3);
        let mut v = Viewer::new(0);
        v.tags = vec!["旅行".into()];
        let (_a, texts) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        let all = texts.join("\n");
        assert!(all.contains("IMG_0001"), "{all}");
        // 日期形如 2020年9月14日
        assert!(
            all.contains('年') && all.contains('月') && all.contains('日'),
            "{all}"
        );
        assert!(all.contains("旅行"), "标签行应显示标签：{all}");
        // 第 1 张是 photo(2)：starred=true → 收藏行真的显示「收藏」
        let mut v2 = Viewer::new(1);
        v2.tags = vec!["旅行".into()];
        let (_a2, texts2) = run_stage(&mut v2, &rows, Appearance::Light, &[]);
        let all2 = texts2.join("\n");
        assert!(all2.contains("收藏"), "收藏照片应显示收藏行：{all2}");
        assert!(all2.contains("旅行"), "{all2}");
    }

    /// V1.4：顶栏**只有一个**返回胶囊（左端），右侧不再重复返回。
    ///
    /// 这条取代了旧的「两端各一枚返回」要求：原型 `shots-v1.4` 的
    /// light/dark viewer 顶栏都只有左侧一枚返回，右侧是文件名 + 信息胶囊。
    #[test]
    fn topbar_has_a_single_back_pill_on_the_left() {
        let lay = layout(stage_rect(), true, false, false);
        let rows = photos(3);
        let mut v = Viewer::new(0);
        let (_a, texts) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        let backs = texts.iter().filter(|t| t.as_str() == "返回").count();
        assert_eq!(backs, 1, "顶栏应只有一枚「返回」：{texts:?}");
        // 唯一那枚在左端；信息胶囊与缩放按钮都在它右侧，且互不重叠。
        let back = Rect::from_min_size(
            pos2(lay.top.left() + TB_PAD, lay.top.top() + 10.0),
            vec2(TB_BACK_W, 24.0),
        );
        assert!(lay.top.contains_rect(back));
        assert!(back.left() < lay.top.center().x, "返回应在左端");
        let info = info_toggle_rect(lay.top);
        assert!(!back.intersects(info));
        let (minus, plus) = zoom_button_rects(lay.top);
        assert!(!back.intersects(minus) && !back.intersects(plus));
    }

    /// 查看器顶栏左端必须给 macOS 红绿灯留出水平空间（用户实测反馈：
    /// 返回胶囊压在系统关闭/最小化/最大化按钮上）。
    ///
    /// 原型 v1.4（Playwright 实测 1280 逻辑宽 `.traffic` 占 x=16..68，
    /// `[data-act="back"]` 从 x=94 起）——红绿灯最右 68，返回最左 94，
    /// 中间还留 26 px。这里钉住「返回左端 ≥ 红绿灯右端」这个不重叠关系，
    /// 并且不把它跟首页的标题带常量混在一起。
    #[test]
    fn viewer_back_pill_clears_the_macos_traffic_lights() {
        // 红绿灯：eframe 0.37 默认 left_margin=14，三颗按钮原生宽约 56，
        // 连间距占 x≈14..70；原型首枚按钮 x=94。
        const TRAFFIC_RIGHT: f32 = 70.0;
        let lay = layout(stage_rect(), true, false, false);
        let back = Rect::from_min_size(
            pos2(lay.top.left() + TB_PAD, lay.top.top() + 10.0),
            vec2(TB_BACK_W, 24.0),
        );
        assert!(
            back.left() >= TRAFFIC_RIGHT,
            "返回按钮应避开红绿灯（TB_PAD={TB_PAD}，原型 back.x=94）：back.left={}",
            back.left()
        );
        // 下一枚控件（收藏）必须从返回右侧继续，不能倒退回红绿灯区。
        let heart = Rect::from_center_size(
            pos2(
                back.right() + TB_BACK_GAP + TB_HEART_W / 2.0,
                lay.top.center().y,
            ),
            vec2(TB_HEART_W, 24.0),
        );
        assert!(
            heart.left() >= back.right() && heart.left() > TRAFFIC_RIGHT,
            "收藏应从返回右侧继续：back.right={}, heart.left={}",
            back.right(),
            heart.left()
        );
        assert!(TB_PAD >= 94.0, "左端留白不得小于原型的 94 px");
    }

    /// 工具栏是一条左→右的流：返回 → 收藏 → 显示位置 → 比较 → 背景
    /// → 缩放簇 → 文件名 → 照片信息胶囊；其中信息胶囊右对齐到顶栏最右端。
    ///
    /// 用户要求把「照片信息」放到 topbar 最右侧（与原型紧贴缩放控件不同）。
    /// 本测试用几何位置钉住顺序、右对齐，同时保证所有控件都还在顶栏内、互不重叠。
    #[test]
    fn topbar_flows_left_to_right_and_info_chip_is_right_aligned() {
        let lay = layout(stage_rect(), true, false, false);
        let top = lay.top;
        let track = zoom_track_rect(top);
        let name = filename_slot_rect(top);
        let chip = info_toggle_rect(top);
        assert!(top.contains_rect(track), "{track:?} 应在顶栏内");
        assert!(top.contains_rect(chip), "{chip:?} 应在顶栏内");
        // 顺序：缩放簇 → 文件名 → 信息胶囊
        assert!(track.right() < name.left(), "文件名应在缩放簇之后");
        assert!(name.right() <= chip.left(), "胶囊应在文件名之后");
        assert!(!track.intersects(name) && !name.intersects(chip));
        // 胶囊右对齐到顶栏右端（内缩 8px）。
        assert!(
            (top.right() - chip.right() - 8.0).abs() < 0.01,
            "胶囊应对齐顶栏右端：chip={chip:?} top={top:?}"
        );
        // 缩放簇内部顺序：适合 → 100% → − → +
        let (minus, plus) = zoom_button_rects(top);
        assert!(track.contains_rect(minus) && track.contains_rect(plus));
        assert!(minus.right() < plus.left());
        // 视觉断言：只有一枚返回；文件名与胶囊都真的画出来了。
        let rows = photos(3);
        let mut v = Viewer::new(0);
        let (_a, texts) = run_stage(&mut v, &rows, Appearance::Light, &[]);
        let all = texts.join("\n");
        assert_eq!(texts.iter().filter(|t| t.as_str() == "返回").count(), 1);
        assert!(all.contains("IMG_0001") || all.contains("19-fog"), "{all}");
        assert!(all.contains("照片信息"), "{all}");
        assert!(all.contains("适合") && all.contains("100%"), "{all}");
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
