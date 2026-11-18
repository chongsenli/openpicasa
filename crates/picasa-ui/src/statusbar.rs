//! 底部状态栏（U-33~U-35）+ **M5 的只读索引进度**与缩略图滑块。
//!
//! ## M5-IA-02 / M5-IA-04 / M5-IDX-01：底栏不再是入口
//!
//! | 改动 | 前（M4 / 被 v1.4 覆盖） | 后（M5） |
//! | --- | --- | --- |
//! | 「索引管理」按钮 | 底栏右区恒为一个按钮（四态文案都是「索引管理」+ 红角标） | **删除** |
//! | 角标 | 底栏按钮上的红底数字 + 三句悬停 | **删除**（失败信息仍在索引管理「任务」页与浮层） |
//! | IO 短进度 pill | 可点，点开切到「导入 / 导出」标签 | **只读**，放底栏左侧，不可点 |
//! | 缩略图滑块 | 顶栏 −/+ 胶囊（对数档） | 底栏**最右**：线性滑块 + 实时像素文案 |
//!
//! 索引管理的入口改由「更多 → 设置… → 打开索引管理」与「更多 → 索引管理…」
//! 直达（见 `app.rs`）；本模块只画状态，不产出任何打开窗口的动作。
//!
//! ## 只读运行状态的文案口径
//!
//! * 索引任务运行中：`已处理 N 张 · 已用 mm:ss`（[`running_status_text`]）。
//!   N 取 `IndexSnapshot::processed_running()`：扫描阶段是已入库张数，生成阶段
//!   是「已出图 + 已判失败」的张数——**和真实后台处理同源**，不会因为生成缩略图
//!   而长期停在 0。**没有分母、没有百分比、没有 ETA**——`TaskView.elapsed_text`
//!   明确写了「不许加 ETA」。
//! * 导入 / 导出运行中：短句沿用 [`crate::index_mgr::progress_short_label`]
//!   （阶段 + 已处理 N / M）。这是**条目口径**，与照片张数分开说。
//! * 两者同时进行：索引短句在前、IO 短句在后，中间用 `·` 分隔，仍是一行状态。
//!
//! ## 缩略图滑块
//!
//! 范围 **22–512 px**、线性 **step=1**、`input` 即刻改网格边长（`ZoomState::set_cell_px`），
//! 松手（`drag_stopped`）与键盘动作后写入 `settings.grid_cell_px`。
//! 与键盘共用同一状态：⌘−/⌘1 小 16 px、⌘+/⌘=/⌘2 大 16 px、⌘0 回 96 px。

use eframe::egui::{self, Align2, FontId, Rect, Ui, pos2, vec2};
use picasa_core::{IndexSnapshot, Library};

use crate::index_mgr::IoUi;
use crate::theme::{Metrics, Theme};

/// 状态栏的一帧输出。
///
/// M5 起**不再**有 `index_clicked` / `import_clicked`：底栏没有任何可点入口
/// （`M5-IA-02` 已确认，不再作为待定）。只剩滑块的「拖动中 / 松手」两类事件。
#[derive(Clone, Debug, Default)]
pub struct StatusbarOut {
    /// 滑块本帧被拖动 / 点轨道改值：新的格边长（逻辑 px，整数）。
    ///
    /// `Some` 表示「立刻改网格边长」（`M5-TB-10` 的 `input` 即刻生效）；
    /// `None` 表示本帧没动滑块。
    pub zoom_px: Option<f32>,
    /// 滑块松手（或点击轨道后）——把值写进 `settings.grid_cell_px`。
    pub zoom_released: bool,
}

/// 滑块两侧的像素文案宽度（「512 px」），定值让滑块不会随数字长度抖动。
const PX_LABEL_W: f32 = 48.0;
/// 「缩略图」标题宽度。
const SLIDER_TITLE_W: f32 = 42.0;
/// 滑块本体宽度（成组贴右，v1.4 量感 ≈ 96）。
const SLIDER_W: f32 = 96.0;

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    lib: &Library,
    theme: &Theme,
    // §4.8 启动校验的进度文案（空 = 没在跑）；M5 起只在左侧只读状态里出现
    verify_text: &str,
    // 当前缩略图边长（逻辑 px）
    zoom_px: f32,
    // 索引进度快照：左侧只读「已处理 N 张 · 已用 mm:ss」
    snap: &IndexSnapshot,
    // 本次连续运行已持续多少秒（None = 当前没有运行中的索引任务）。
    //
    // 为什么不直接用快照的 `started_at`：watcher 每次外部变化都会开一个新批次，
    // 用批次时刻会让「已用」在连续运行时反复归零（实测 9 张的库每几秒一次）。
    // 由 App 用单调时钟记住「第一次看到 running 的时刻」，跨批次连续。
    elapsed_secs: Option<u64>,
    // M4.3 IDX-18：导入 / 导出进度（只读短句）
    io: &IoUi,
) -> StatusbarOut {
    let mut out = StatusbarOut::default();
    let rect = Rect::from_min_size(
        ui.max_rect().left_top(),
        vec2(ui.max_rect().width(), Metrics::STATUS_H),
    );
    let _ = ui.allocate_space(rect.size());
    let painter = ui.painter_at(rect);
    // M5 §5.4：底栏 = 窗口色 + 顶缘发丝线；padding ≈ 0 12px 0 14px。
    painter.rect_filled(rect, 0.0, theme.chrome_bg);
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        egui::Stroke::new(1.0, theme.separator),
    );

    let font = FontId::new(11.0, egui::FontFamily::Proportional);

    // ---- 右区：缩略图滑块 + 实时像素文案（成组贴右）----
    // 先画右区，拿到它的左缘，左区才知道自己能用多少宽度。
    let slider = draw_zoom_slider(ui, &painter, rect, theme, zoom_px, &mut out);

    // ---- 左区：只读运行状态（不可点）----
    // 有运行任务时先报索引任务、再报 IO；都空闲时才显示「已选 N 张」。
    // 右侧硬边界 = 滑块组左缘 - 12，状态文字**永远在滑块左边**。
    let mut texts: Vec<String> = Vec::new();
    if let Some(t) = running_status_text(snap, elapsed_secs) {
        texts.push(t);
    }
    if let Some(t) = io_status_text(io) {
        texts.push(t);
    }
    // 启动校验（L1/L2）也是后台状态，只读展示；不带百分比、不带 ETA。
    if !verify_text.is_empty() {
        texts.push(format!("启动校验：{verify_text}"));
    }
    if texts.is_empty() {
        let sel = lib.selected_count();
        if sel > 0 {
            texts.push(format!("已选 {sel} 张"));
        }
    }
    // 运行中的「已用 mm:ss」要跟着走：egui 是事件驱动的，没有新帧就不会重算，
    // 底栏会停在「已用 00:00」直到用户碰鼠标。这里按 4 fps 约一帧即可
    // （不打满 60 fps：软件光栅下那是白烧 CPU）。
    if running_status_text(snap, elapsed_secs).is_some() || io_status_text(io).is_some() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
    if let Some(text) = texts.first() {
        let x = rect.left() + 16.0;
        let mut galley = painter.layout_no_wrap(text.clone(), font.clone(), theme.status_text);
        // 窄窗口：放不下就截断成「…」，而不是压到滑块上。
        let room = (slider.left() - 12.0 - x).max(0.0);
        if galley.size().x > room {
            let keep = ((text.chars().count() as f32) * room / galley.size().x).floor() as usize;
            let short: String = text.chars().take(keep.max(1)).collect();
            galley = painter.layout_no_wrap(format!("{short}…"), font.clone(), theme.status_text);
        }
        painter.galley(
            pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            theme.status_text,
        );
    }

    out
}

/// 运行中的索引任务只读文案：`已处理 N 张 · 已用 mm:ss`。
///
/// 口径（`M5-IDX-01`）：
/// * **只有已完成张数与已用时长**，不出现分母、百分号、ETA；
/// * 分母未知时同样只用 N——不造 `N / ?`，更不造百分比；
/// * 没有运行中的索引任务 → `None`（空闲态底栏左侧允许为空）。
pub fn running_status_text(snap: &IndexSnapshot, elapsed_secs: Option<u64>) -> Option<String> {
    // 只看还在跑的任务：已完成 / 失败的旧任务不该在底栏常驻。
    let running: Vec<&picasa_core::TaskSnapshot> =
        snap.tasks.iter().filter(|t| t.state.is_running()).collect();
    if running.is_empty() {
        return None;
    }
    // 数字必须与「真实后台处理」同源：直接用快照上的 `processed_running()`，
    // 不在 UI 里自己累加（否则各处计数迟早差 1）。扫描阶段它随入库增长、
    // 生成阶段随出图增长，同一批次内只增不减。
    let done = snap.processed_running();
    Some(match elapsed_secs.map(format_mmss) {
        Some(t) => format!("已处理 {done} 张 · 已用 {t}"),
        None => format!("已处理 {done} 张"),
    })
}

/// 秒数 → `mm:ss`（超过一小时写 `h:mm:ss`）。
pub fn format_mmss(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// 导入 / 导出运行中的只读短句。空闲 / 终态返回 `None`。
///
/// 沿用 [`crate::index_mgr::progress_short_label`] 的「阶段 · 已处理 N / M」口径；
/// 这是**条目数**，与上面索引的「张」分开说，不混成一个分母。
pub fn io_status_text(io: &IoUi) -> Option<String> {
    if !matches!(
        io.state,
        crate::index_mgr::IoState::Running | crate::index_mgr::IoState::Preparing
    ) {
        return None;
    }
    let Some(snap) = &io.snapshot else {
        return None;
    };
    // 口径与索引管理的短标签一致（阶段 + 已处理），但只读、且**不带百分比 / ETA**。
    Some(match snap.total {
        Some(total) if total > 0 => {
            format!("{} · 已处理 {} / {} 项", snap.stage, snap.done, total)
        }
        _ => format!("{} · 已处理 {} 项", snap.stage, snap.done),
    })
}

/// 底栏最右侧的缩略图滑块 + 像素文案，返回整组的左缘（左区据此排布）。
///
/// * 范围 **22–512**、线性 **step=1**（用 `egui::Slider::integer()`）；
/// * `input` 即刻生效：滑块值每帧写回 `ZoomState`（由调用方执行）；
/// * 松手才落库（`M5-TB-10`「松开后写入偏好」）。
fn draw_zoom_slider(
    ui: &mut Ui,
    painter: &egui::Painter,
    bar: Rect,
    theme: &Theme,
    zoom_px: f32,
    out: &mut StatusbarOut,
) -> Rect {
    let cy = bar.center().y;
    let px_label = Rect::from_center_size(
        pos2(bar.right() - 8.0 - PX_LABEL_W / 2.0, cy),
        vec2(PX_LABEL_W, Metrics::STATUS_H),
    );
    let slider_rect = Rect::from_center_size(
        pos2(px_label.left() - SLIDER_W / 2.0, cy),
        vec2(SLIDER_W, Metrics::STATUS_H),
    );
    let title_rect = Rect::from_center_size(
        pos2(slider_rect.left() - SLIDER_TITLE_W / 2.0, cy),
        vec2(SLIDER_TITLE_W, Metrics::STATUS_H),
    );

    // 「缩略图」小标题（不可点）
    painter.text(
        title_rect.center(),
        Align2::CENTER_CENTER,
        "缩略图",
        FontId::new(11.0, egui::FontFamily::Proportional),
        theme.text_secondary,
    );

    // 实时像素文案：跟着滑块值走，不四舍五入到档位。
    let px = zoom_px
        .round()
        .clamp(picasa_core::CELL_MIN_PX, picasa_core::CELL_MAX_PX) as i32;
    painter.text(
        px_label.center(),
        Align2::CENTER_CENTER,
        format!("{px} px"),
        FontId::new(11.0, egui::FontFamily::Proportional),
        theme.status_text,
    );

    // 滑块本体：在 `slider_rect` 里嵌一个子 Ui，避免影响状态栏的整行分配。
    let mut value = zoom_px.clamp(picasa_core::CELL_MIN_PX, picasa_core::CELL_MAX_PX) as i32;
    // 滑块轨道 / 把手颜色来自主题：亮色轨道 #ebebec、把手强调蓝；暗色轨道 #1c1c1c、
    // 把手强调青。egui 的 slider 从 `widgets.inactive.bg_fill`（轨道）与
    // `widgets.inactive.bg_fill`（把手）取色，这里在局部样式里覆盖，不改全局。
    let mut slider_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("statusbar-zoom-slider")
            .max_rect(slider_rect),
    );
    slider_ui.style_mut().spacing.slider_width = SLIDER_W - 8.0;
    {
        let v = &mut slider_ui.style_mut().visuals.widgets;
        v.inactive.bg_fill = theme.track_bg;
        v.inactive.corner_radius = egui::CornerRadius::same(255);
        v.hovered.bg_fill = theme.track_bg;
        v.hovered.corner_radius = egui::CornerRadius::same(255);
        v.active.bg_fill = theme.track_bg;
        v.active.corner_radius = egui::CornerRadius::same(255);
    }
    slider_ui.style_mut().visuals.selection.bg_fill = theme.selected;
    slider_ui.style_mut().visuals.handle_shape = egui::style::HandleShape::Circle;
    let resp = slider_ui.add(
        egui::Slider::new(
            &mut value,
            picasa_core::CELL_MIN_PX as i32..=picasa_core::CELL_MAX_PX as i32,
        )
        .integer()
        .show_value(false),
    );
    if resp.changed() {
        out.zoom_px = Some(value as f32);
    }
    if resp.drag_stopped() {
        out.zoom_released = true;
    }
    // 点击轨道（不是拖动）也会改值，且没有 drag_stopped：用 clicked 补一次落库。
    if resp.clicked() {
        out.zoom_px = Some(value as f32);
        out.zoom_released = true;
    }
    title_rect.union(px_label)
}

/// 滑块组左缘（供左区排布 / 单测断言「状态不在滑块右侧」）。
pub fn slider_group_left(bar: Rect) -> f32 {
    bar.right() - 8.0 - PX_LABEL_W - SLIDER_W - SLIDER_TITLE_W
}

/// 状态区的可点性：M5 底栏**没有任何**可点索引入口 / IO 入口。
///
/// 保留成函数是为了让「底栏不可点」成为可单测的结构性事实，而不是一句注释。
pub fn statusbar_has_clickable_entry() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use picasa_core::{IndexSnapshot, IndexState, TaskSnapshot};

    use super::*;
    use crate::index_mgr::{IoState, IoUi, ProgressSnapshot};

    fn task(state: IndexState, completed: u64, discovered: u64) -> TaskSnapshot {
        TaskSnapshot {
            path: "/photos".into(),
            name: "photos".into(),
            state,
            batch: 1,
            discovered,
            completed,
            failed: 0,
            percent: if state == IndexState::Scanning {
                None
            } else {
                Some(0)
            },
            reason: None,
            last_update: 0.0,
            failures: Vec::new(),
        }
    }

    /// `M5-IA-STATUS`：运行中只读文案是「已处理 N 张 · 已用 mm:ss」，
    /// **不带分母、不带百分号、不带 ETA**。
    #[test]
    fn m5_running_status_has_no_denominator_percent_or_eta() {
        let mut snap = IndexSnapshot {
            batch: 1,
            tasks: vec![task(IndexState::Thumbnailing, 37, 120)],
            ..Default::default()
        };
        let text = running_status_text(&snap, Some(65)).expect("运行中要有只读状态");
        assert!(text.contains("已处理 37 张"), "{text}");
        assert!(text.contains("已用 01:05"), "已用时长要 mm:ss：{text}");
        for banned in ["/", "%", "％", "预计", "剩余", "ETA", "比"] {
            assert!(!text.contains(banned), "只读状态不许出现 {banned}：{text}");
        }

        // 扫描阶段（分母未知）也只报 N，不造 N / ?。
        snap.tasks = vec![task(IndexState::Scanning, 5, 0)];
        let text = running_status_text(&snap, Some(9)).unwrap();
        assert!(text.contains("已处理 5 张"), "{text}");
        assert!(!text.contains('/'), "{text}");
    }

    /// `M5-R1-closeout`：有真实后台进展时 N 不得停在 0。
    ///
    /// 回归的是 `bigrun-3.png` 的现场：库里有几百张、缩略图在生成，
    /// 底栏却一直写「已处理 0 张」。现在 N 取运行中任务的
    /// `completed + failed`（真正处理完的张数），每补一张就涨。
    /// 仍然**没有分母**：生成阶段的 `discovered` 不进文案。
    #[test]
    fn m5_running_count_tracks_real_progress_and_never_sticks_at_zero() {
        // ① 生成阶段：出图 37 / 已发现 400。文案是 37，不是 400（400 是分母）。
        let mut snap = IndexSnapshot {
            batch: 1,
            tasks: vec![task(IndexState::Thumbnailing, 37, 400)],
            ..Default::default()
        };
        let text = running_status_text(&snap, Some(20)).unwrap();
        assert!(text.contains("已处理 37 张"), "出图数要进文案：{text}");
        assert!(!text.contains("400"), "已发现数不得出现在底栏：{text}");

        // ② 失败也要算「已处理」：30 张出图 + 5 张坏图。
        let mut t = task(IndexState::Thumbnailing, 30, 400);
        t.failed = 5;
        snap.tasks = vec![t];
        assert!(
            running_status_text(&snap, None)
                .unwrap()
                .contains("已处理 35 张")
        );

        // ③ 真实进展会推动文案：同一任务逐张补完，N 必须跟着涨。
        let mut snap = IndexSnapshot {
            batch: 7,
            tasks: vec![task(IndexState::Thumbnailing, 0, 400)],
            ..Default::default()
        };
        assert_eq!(
            running_status_text(&snap, Some(1)).unwrap(),
            "已处理 0 张 · 已用 00:01",
            "起点确实是 0（刚进生成阶段，一张都还没出）"
        );
        for n in 1..=3u64 {
            snap.tasks[0].completed = n;
            let text = running_status_text(&snap, Some(1 + n)).unwrap();
            assert!(
                text.contains(&format!("已处理 {n} 张")),
                "第 {n} 张出图后 N 要动：{text}"
            );
        }
        assert!(
            !running_status_text(&snap, Some(4))
                .unwrap()
                .contains("已处理 0 张"),
            "有真实进展就不会停在 0 —— 这是本轮修复的验收点"
        );

        // ④ 扫描阶段：入库张数也算进展（分母未冻结，不是分母）。
        let scan = IndexSnapshot {
            batch: 8,
            tasks: vec![task(IndexState::Scanning, 0, 120)],
            ..Default::default()
        };
        assert!(
            running_status_text(&scan, Some(3))
                .unwrap()
                .contains("已处理 120 张")
        );
    }

    /// 多任务时按分项之和汇总，且仍不含分母 / 百分比 / ETA。
    #[test]
    fn m5_running_count_sums_running_tasks_only() {
        let done = task(IndexState::Completed, 50, 50);
        let mut snap = IndexSnapshot {
            batch: 2,
            tasks: vec![
                task(IndexState::Thumbnailing, 5, 20),
                task(IndexState::Thumbnailing, 3, 10),
                done,
            ],
            ..Default::default()
        };
        snap.tasks[0].path = "/a".into();
        snap.tasks[1].path = "/b".into();
        let text = running_status_text(&snap, Some(5)).unwrap();
        assert!(text.contains("已处理 8 张"), "只汇总运行中的 5+3：{text}");
        for banned in ["/", "%", "％", "预计", "剩余", "ETA"] {
            assert!(!text.contains(banned), "只读状态不许出现 {banned}：{text}");
        }
    }

    /// 空闲 / 终态：底栏左侧不常驻索引文案。
    #[test]
    fn m5_running_status_is_absent_when_idle() {
        assert_eq!(running_status_text(&IndexSnapshot::default(), None), None);
        let done = IndexSnapshot {
            tasks: vec![task(IndexState::Completed, 10, 10)],
            ..Default::default()
        };
        assert_eq!(
            running_status_text(&done, Some(30)),
            None,
            "已完成任务不该常驻底栏"
        );
    }

    /// `M5-IDX-01`：IO 短句只在准备 / 运行中出现，终态不常驻；
    /// 它是「条目」口径，不是照片分母。
    #[test]
    fn m5_io_status_is_short_and_only_while_running() {
        let mut io = IoUi::default();
        assert_eq!(io_status_text(&io), None);
        io.state = IoState::Success;
        assert_eq!(io_status_text(&io), None);
        io.state = IoState::Running;
        io.snapshot = Some(ProgressSnapshot {
            stage: "导出".into(),
            done: 12,
            total: Some(40),
            current: String::new(),
            errors: Vec::new(),
            revision: 1,
        });
        let text = io_status_text(&io).unwrap();
        assert!(text.contains("已处理 12 / 40"), "{text}");
        // 永久禁令：IO 短句也不许出现 ETA。
        for banned in ["预计", "剩余", "ETA"] {
            assert!(!text.contains(banned), "{text}");
        }
    }

    /// `M5-IA-02`：底栏没有任何可点索引入口 / IO 入口。
    #[test]
    fn m5_statusbar_has_no_clickable_entry() {
        assert!(!statusbar_has_clickable_entry());
        // 结构上也没有「入口被点」的字段了——这里用类型事实兜底：
        // `StatusbarOut` 只有滑块的两种事件。
        let out = StatusbarOut::default();
        assert!(out.zoom_px.is_none());
        assert!(!out.zoom_released);
    }

    /// `M5-IA-STATUS`：滑块组贴右，状态区在它左边（不挤在滑块右侧）。
    #[test]
    fn m5_slider_is_at_the_far_right() {
        let bar = Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, Metrics::STATUS_H));
        let left = slider_group_left(bar);
        assert!(left > 900.0, "滑块组必须贴右：left={left}");
        assert!(bar.right() - left < 210.0, "滑块 + 文案 + 标题的总宽要紧凑");
        assert!(left - bar.left() > 200.0, "左侧要留出只读状态的空间");
    }

    /// 已用时长格式：分:秒，超一小时带小时。
    #[test]
    fn m5_elapsed_formats_mmss() {
        assert_eq!(format_mmss(0), "00:00");
        assert_eq!(format_mmss(65), "01:05");
        assert_eq!(format_mmss(3599), "59:59");
        assert_eq!(format_mmss(3725), "1:02:05", "超过一小时写 h:mm:ss");
    }

    use picasa_core::MgmtSnapshot;

    /// 悬停文案的三个分量加起来必须等于角标总数（§4.4）。
    ///
    /// 这条用例守的是「界面上写的原因」与「角标上的数字」不自相矛盾——
    /// 数字说 6、原因加起来只有 5，用户就会认为角标是瞎算的。
    /// M5 起角标不再画在底栏，但这条口径仍是索引管理窗口里解释数字的依据。
    #[test]
    fn badge_tooltip_components_add_up_to_the_badge() {
        use picasa_core::{ExclusionView, RootView};
        let mut snap = MgmtSnapshot {
            roots: vec![RootView {
                id: 1,
                path: "/Volumes/X".into(),
                offline: true,
                ..Default::default()
            }],
            exclusions: vec![ExclusionView {
                id: 1,
                root_id: 1,
                path: "/a".into(),
                path_missing: true,
            }],
            failed_task_count: 4,
            ..Default::default()
        };
        snap.compute_badge(4, 1);
        assert_eq!(snap.badge, 6, "4 失败任务 + 1 失效排除项 + 1 离线根");
        let tip = snap.badge_tooltip(snap.failed_task_count);
        assert_eq!(tip.len(), 3);
        assert!(tip[0].contains("4"), "{}", tip[0]);
    }
}
