//! 诊断页（F 加固）：**缓存占用 / 清理、失败记录、排除项、全量重建**。
//!
//! ## 为什么单独一页
//!
//! 这几件事都"不常看但必须能看"：占用多少、哪些文件失败了、关了哪些子目录、
//! 想不想推倒重来。把它们塞进索引进度面板会让那个面板同时承担「实时进度」和
//! 「历史与设置」，而 `04` §3.2 的硬约束是进度面板只讲「正在发生什么」。
//!
//! 本模块**只画不改**：所有数据库操作都通过 [`DiagAction`] 交回 `app` 执行，
//! 这样「谁写库」仍然只有 `Library` / `Db` 一层，UI 不直连 SQL。

use std::collections::VecDeque;

use eframe::egui::{self, Color32, Context, RichText, Ui};
use picasa_core::perf::FrameStats;
use picasa_db::{CacheStats, ExclusionRow};

use crate::theme::{Metrics, Theme};

// ---------------------------------------------------------------- 性能基准常量
//
// R-M42-1 / P-M42-09 / AC-04：没有基准不允许改常量或改上传调度。
// 下面五项是 `theme::Metrics` 的「展示用镜像」，抄成字面量用于诊断页文本，
// 并在 `snapshot()` 里逐项核对（一旦有人改了 Metrics 就能立刻看出「被动过」）。

/// 网格滚动 p99 红线（ms）：`frame_ms > 25.0` 记为一次掉帧。
/// 这是 M42-PERF-00「掉帧率」的唯一判据，探针不凭感觉判定。
const SLOW_FRAME_MS: f32 = 25.0;

/// 每帧贴图上传上限（锁死，镜像 `Metrics::TEX_UPLOAD_PER_FRAME`）。
const TEX_UPLOAD_PER_FRAME: u8 = 3;
/// 纹理缓存上限（镜像值，用于 `constants_locked` 自检）。
const LOCKED_TEX_CACHE_CAP: usize = 120;
/// 细滚动条宽度（镜像值，用于 `constants_locked` 自检）。
const LOCKED_SCROLLBAR: f32 = 8.0;

/// 性能探针累计的帧样本上限：覆盖约 60s @144Hz（与 FrameStats 的窗口对齐）。
const PROBE_FRAME_CAP: usize = 8192;

/// 展示用的三行锁死常量文案（`show()` 直接渲染）。
const LOCKED_CONST_LINES: [(&str, &str); 3] = [
    ("TEX_UPLOAD_PER_FRAME", "3（每帧贴图上传数上限）"),
    ("TEX_CACHE_CAP", "120（纹理缓存张数上限）"),
    ("SCROLLBAR", "8.0（细滚动条宽度）"),
];

/// M42-PERF-00 六项展示性能指标的一次快照（供诊断页只读渲染）。
///
/// 所有值都是实测累计的结果；`frame_samples == 0` 表示尚未采样，
/// 此时 p50/p95/p99/掉帧率/空白格率都无意义，`show()` 应显示「—（未采样）」。
#[derive(Clone, Debug, PartialEq)]
pub struct PerfSnapshot {
    /// 首屏出图时间（ms）：从应用启动到第一张缩略图可见。
    pub first_screen_ms: Option<f32>,
    /// 可见区首张出图延迟（ms）：滚动/布局稳定后可见区第一张出图的延迟。
    pub first_visible_ms: Option<f32>,
    /// 滚动帧耗时 p50（ms）。
    pub frame_p50_ms: f32,
    /// 滚动帧耗时 p95（ms）。
    pub frame_p95_ms: f32,
    /// 滚动帧耗时 p99（ms）——网格滚动红线（>25ms 记掉帧）。
    pub frame_p99_ms: f32,
    /// 已累计的帧耗时样本数（0 = 未采样）。
    pub frame_samples: usize,
    /// 每帧贴图上传数（锁死 = 3，来自 `Metrics::TEX_UPLOAD_PER_FRAME`）。
    /// 这是「允许的上限」，改它等于改上传调度（AC-04 禁止）。
    pub uploads_per_frame: u8,
    /// 实测：采样窗口内出现过的「单帧最大上传数」，未采样时为 `None`。
    /// 与 `uploads_per_frame`（上限）分开，便于核对是否越界。
    pub measured_max_uploads: Option<u8>,
    /// 空白格率（0.0~1.0）：`blank_cells / visible_cells` 的累计口径。
    pub blank_cell_rate: f32,
    /// 掉帧率（0.0~1.0）：`frame_ms > SLOW_FRAME_MS` 的帧占比。
    pub dropped_frame_rate: f32,
    /// 三行锁死常量是否全部与 `theme::Metrics` 一致且从未被改动（恒 `true`）。
    pub constants_locked: bool,
}

impl Default for PerfSnapshot {
    fn default() -> Self {
        Self {
            first_screen_ms: None,
            first_visible_ms: None,
            frame_p50_ms: 0.0,
            frame_p95_ms: 0.0,
            frame_p99_ms: 0.0,
            frame_samples: 0,
            // 上传数锁死为常量，连「未采样」也不改：改它等于改上传调度（违反 AC-04）。
            uploads_per_frame: TEX_UPLOAD_PER_FRAME,
            measured_max_uploads: None,
            blank_cell_rate: 0.0,
            dropped_frame_rate: 0.0,
            constants_locked: true,
        }
    }
}

/// 自包含的帧基准探针（M42-PERF-00），供 `app.rs` 持有并按帧喂数据。
///
/// ## 设计约束（对应红线）
/// - **零副作用**：不持有 `egui::Context`、不调 `request_repaint()`、不起线程、不读文件。
///   egui 是事件驱动的，探针只是被动累计，`app` 在已有帧里顺手 `note_frame`。
/// - **不重写分位数**：直接复用 `picasa_core::perf::FrameStats`（`push`/`percentiles`），
///   保证「诊断页看到的 p99」与 `perf_bench` / ⌥+点击浮层是同一个算法。
/// - 环形上限 [`PROBE_FRAME_CAP`]：满则丢最旧一帧的累计量，避免长时间运行内存增长。
///
/// 内部用 `FrameStats` 记帧耗时；空白格、掉帧、上传数用纯计数器累计
/// （`FrameStats` 只存 ms，无法捎带这些维度）。
pub struct PerfProbe {
    frames: FrameStats,
    first_screen_ms: Option<f32>,
    first_visible_ms: Option<f32>,
    /// 环形缓冲：每帧一个 `(visible_cells, blank_cells)`。
    /// 与 `frames` 同步入队/出队，保证「空白格率 / 掉帧率」与分位数同窗口。
    cells: VecDeque<(usize, usize)>,
    slow_flags: VecDeque<bool>,
    uploads: VecDeque<u8>,
}

impl Default for PerfProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl PerfProbe {
    pub fn new() -> Self {
        Self {
            frames: FrameStats::new(),
            first_screen_ms: None,
            first_visible_ms: None,
            cells: VecDeque::new(),
            slow_flags: VecDeque::new(),
            uploads: VecDeque::new(),
        }
    }

    /// 记一帧的实测数据（由 `app.rs` 的 `ui()` 每帧调用一次）。
    ///
    /// - `frame_ms`：本帧耗时（来自 eframe 的帧时间，或 `FrameStats::tick()`）。
    /// - `visible_cells` / `blank_cells`：可见格数 / 其中空白（未出图）格数。
    /// - `uploads_this_frame`：本帧实际上传的贴图张数（用于核对 ≤ 3 的锁死常量）。
    pub fn note_frame(
        &mut self,
        frame_ms: f32,
        visible_cells: usize,
        blank_cells: usize,
        uploads_this_frame: u8,
    ) {
        self.frames.push(frame_ms);
        self.cells.push_back((visible_cells, blank_cells));
        self.slow_flags.push_back(frame_ms > SLOW_FRAME_MS);
        self.uploads.push_back(uploads_this_frame);

        // FrameStats 自带 60s 时间窗口淘汰；本模块的 deque 必须按「实际保留帧数」
        // 同步剪掉最旧项，否则时间一长 slow_count / cells 会比 n 多，比率失真。
        // 同时再按容量上限兜底（短时高频帧）。
        let keep = self.frames.len().min(PROBE_FRAME_CAP);
        while self.cells.len() > keep {
            self.cells.pop_front();
            self.slow_flags.pop_front();
            self.uploads.pop_front();
        }
    }

    /// 记「可见区首张出图延迟」（只取第一次）。
    pub fn note_first_visible(&mut self, ms: f32) {
        if self.first_visible_ms.is_none() {
            self.first_visible_ms = Some(ms);
        }
    }

    /// 记「首屏出图时间」（只取第一次）。
    pub fn note_first_screen(&mut self, ms: f32) {
        if self.first_screen_ms.is_none() {
            self.first_screen_ms = Some(ms);
        }
    }

    /// 生成一份六指标快照（纯读，不改状态）。
    pub fn snapshot(&self) -> PerfSnapshot {
        let p = self.frames.percentiles();
        let n = self.frames.len();
        let visible_total: usize = self.cells.iter().map(|(v, _)| v).sum();
        let blank_total: usize = self.cells.iter().map(|(_, b)| b).sum();
        let slow_count = self.slow_flags.iter().filter(|&&s| s).count();

        let blank_cell_rate = if visible_total == 0 {
            0.0
        } else {
            blank_total as f32 / visible_total as f32
        };
        let dropped_frame_rate = if n == 0 {
            0.0
        } else {
            slow_count as f32 / n as f32
        };
        let measured_max_uploads = self.uploads.iter().copied().max();

        // 锁死常量自检：三个展示值必须与 theme::Metrics 同源，否则视为「被动过」。
        // 用容差比较 f32，避免 clippy::float_cmp，也避免 8.0 的精确 == 争议。
        let scrollbar_ok = (Metrics::SCROLLBAR - LOCKED_SCROLLBAR).abs() < f32::EPSILON;
        let constants_locked = TEX_UPLOAD_PER_FRAME as usize == Metrics::TEX_UPLOAD_PER_FRAME
            && LOCKED_TEX_CACHE_CAP == Metrics::TEX_CACHE_CAP
            && scrollbar_ok;

        PerfSnapshot {
            first_screen_ms: self.first_screen_ms,
            first_visible_ms: self.first_visible_ms,
            frame_p50_ms: p.p50,
            frame_p95_ms: p.p95,
            frame_p99_ms: p.p99,
            frame_samples: n,
            // 上限锁死为常量，改它等于改上传调度（违反 AC-04）。
            uploads_per_frame: TEX_UPLOAD_PER_FRAME,
            measured_max_uploads,
            blank_cell_rate,
            dropped_frame_rate,
            constants_locked,
        }
    }
}

/// 诊断页上的一个动作，交回 `app` 执行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagAction {
    /// 清空缩略图缓存（释放空间，下次扫描重建）
    ClearThumbs,
    /// VACUUM 回收磁盘
    Vacuum,
    /// 全量重建（清空缩略图 + 重新扫描所有根目录）
    RebuildAll,
    /// 恢复一条排除项
    RestoreExclusion(i64),
    /// 重新打开一个已关闭的根目录
    ReopenRoot(String),
    /// 重新读取统计（缓存占用/失败/排除项可能变了）
    Refresh,
}

/// 一条失败记录（来自 `index_task_failures`）。
#[derive(Clone, Debug, Default)]
pub struct DiagFailure {
    pub scope: String,
    pub path: String,
    pub code: String,
}

/// 诊断页需要展示的全部数据（由 `app` 一次性取好）。
#[derive(Clone, Debug, Default)]
pub struct DiagData {
    pub stats: CacheStats,
    pub failures: Vec<DiagFailure>,
    pub exclusions: Vec<(String, ExclusionRow)>,
    pub closed_roots: Vec<String>,
    /// M42-PERF-00 展示性能基准快照（由 `app.rs` 的 `gather_diag_data` 填入）。
    /// `#[derive(Default)]` 保证 `DiagData::default()` 仍在无样本时给出全默认快照。
    pub perf: PerfSnapshot,
}

/// 字节数转人类可读（1.2 MB / 340 KB）。
fn human_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    let f = n as f64;
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

/// 画诊断窗口。返回用户点出的动作（最多一个）。
pub fn show(
    ctx: &Context,
    theme: &Theme,
    open: &mut bool,
    data: &DiagData,
    confirm_rebuild: &mut bool,
) -> Option<DiagAction> {
    let mut action = None;
    egui::Window::new("诊断")
        .open(open)
        .resizable(true)
        .default_width(520.0)
        .default_height(560.0)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;

            // ---------------- 缓存占用 ----------------
            section(ui, "缓存与占用", theme);
            let s = &data.stats;
            grid_row(ui, theme, "索引库文件", &human_bytes(s.db_bytes));
            grid_row(ui, theme, "照片", &format!("{} 张", s.photo_count));
            grid_row(
                ui,
                theme,
                "缩略图",
                &format!("{} 条 · {}", s.thumb_count, human_bytes(s.thumb_bytes)),
            );
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui
                    .button("清理缩略图")
                    .on_hover_text("删掉所有缩略图缓存（不删照片），下次扫描会重建")
                    .clicked()
                {
                    action = Some(DiagAction::ClearThumbs);
                }
                if ui
                    .button("VACUUM 回收")
                    .on_hover_text("物理压缩数据库文件。清理缩略图后点这里才真的变小")
                    .clicked()
                {
                    action = Some(DiagAction::Vacuum);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if *confirm_rebuild {
                        if ui
                            .button(
                                RichText::new("确认重建？")
                                    .color(Color32::from_rgb(0xB0, 0x30, 0x20)),
                            )
                            .clicked()
                        {
                            action = Some(DiagAction::RebuildAll);
                            *confirm_rebuild = false;
                        }
                        if ui.button("取消").clicked() {
                            *confirm_rebuild = false;
                        }
                    } else if ui
                        .button("全量重建…")
                        .on_hover_text("清空全部缩略图并重新扫描所有根目录。耗时很长")
                        .clicked()
                    {
                        *confirm_rebuild = true;
                    }
                });
            });

            ui.add_space(6.0);

            // ---------------- 失败记录 ----------------
            section(ui, &format!("失败记录（{}）", data.failures.len()), theme);
            if data.failures.is_empty() {
                muted(ui, "没有失败项。");
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("diag-failures")
                    .max_height(150.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for f in &data.failures {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(&f.code)
                                        .size(11.0)
                                        .color(Color32::from_rgb(0xB0, 0x30, 0x20)),
                                );
                                ui.label(RichText::new(&f.path).size(11.5));
                                ui.label(
                                    RichText::new(&f.scope)
                                        .size(10.5)
                                        .color(theme.text_secondary),
                                );
                            });
                        }
                    });
            }

            ui.add_space(6.0);

            // ---------------- 排除项 ----------------
            section(
                ui,
                &format!("已关闭的文件夹（{}）", data.exclusions.len()),
                theme,
            );
            if data.exclusions.is_empty() {
                muted(ui, "没有排除项。右键文件夹可以「关闭此文件夹」。");
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("diag-excl")
                    .max_height(130.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (root, e) in &data.exclusions {
                            ui.horizontal(|ui| {
                                if ui.small_button("恢复").clicked() {
                                    action = Some(DiagAction::RestoreExclusion(e.id));
                                }
                                ui.label(RichText::new(&e.relative_path).size(11.5));
                                ui.label(
                                    RichText::new(root).size(10.0).color(theme.text_secondary),
                                );
                            });
                        }
                    });
            }

            ui.add_space(6.0);

            // ---------------- 已关闭的根目录 ----------------
            section(
                ui,
                &format!("已关闭的关注文件夹（{}）", data.closed_roots.len()),
                theme,
            );
            if data.closed_roots.is_empty() {
                muted(ui, "没有已关闭的关注文件夹。");
            } else {
                for root in &data.closed_roots {
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("重新添加")
                            .on_hover_text("复用原有索引，秒开")
                            .clicked()
                        {
                            action = Some(DiagAction::ReopenRoot(root.clone()));
                        }
                        ui.label(RichText::new(root).size(11.0));
                    });
                }
            }

            ui.add_space(6.0);

            // ---------------- 展示性能基准（M42-PERF-00） ----------------
            // docs/17 S7：诊断视图（非主界面常驻 HUD）；事件驱动，静止不重绘。
            render_perf_section(ui, &data.perf, theme);

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("刷新统计").clicked() {
                    action = Some(DiagAction::Refresh);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new("只读：本页不做任何写入，除非你点按钮")
                            .size(10.0)
                            .color(theme.text_secondary),
                    );
                });
            });
        });
    action
}

/// 画「展示性能基准（M42-PERF-00）」分区：6 项指标 + 三行锁死常量。
///
/// 抽成独立函数是为了让 `show()` 不越过 clippy.toml 的
/// `too-many-lines-threshold = 200`：诊断页分区天然多，但绘制逻辑不该挤在一个闭包里。
/// 无样本时数字显示「—（未采样）」，绝不编造数字（AC-04：没有基准不许动常量）。
fn render_perf_section(ui: &mut Ui, perf: &PerfSnapshot, theme: &Theme) {
    section(ui, "展示性能基准（M42-PERF-00）", theme);
    let sampled = perf.frame_samples > 0;

    // 1) 首屏出图时间
    grid_row(
        ui,
        theme,
        "首屏出图时间",
        &opt_ms(perf.first_screen_ms),
    );
    // 2) 可见区首张出图延迟
    grid_row(
        ui,
        theme,
        "可见区首张出图延迟",
        &opt_ms(perf.first_visible_ms),
    );

    // 3) 滚动帧耗时 p50 / p95 / p99
    if sampled {
        grid_row(
            ui,
            theme,
            "滚动帧耗时 p50 / p95 / p99",
            &format!(
                "{:.1} / {:.1} / {:.1} ms",
                perf.frame_p50_ms, perf.frame_p95_ms, perf.frame_p99_ms
            ),
        );
        grid_row(
            ui,
            theme,
            "帧样本数",
            &format!("{} 帧", perf.frame_samples),
        );
    } else {
        grid_row(ui, theme, "滚动帧耗时 p50 / p95 / p99", "—（未采样）");
        grid_row(ui, theme, "帧样本数", "—（未采样）");
    }

    // 4) 每帧贴图上传数：上限锁死 3；实测峰值另起一行，未采样显示占位。
    grid_row(
        ui,
        theme,
        "每帧贴图上传数上限（≤3 锁死）",
        &format!("{} 张", perf.uploads_per_frame),
    );
    let measured = match perf.measured_max_uploads {
        Some(n) => format!("{n} 张/帧（峰值）"),
        None => "—（未采样）".to_owned(),
    };
    grid_row(ui, theme, "实测单帧上传峰值", &measured);
    // 5) 空白格率
    grid_row(ui, theme, "空白格率", &rate_or_unsampled(perf.blank_cell_rate, sampled));
    // 6) 掉帧率（frame_ms > 25.0）
    grid_row(
        ui,
        theme,
        "掉帧率（>25ms）",
        &rate_or_unsampled(perf.dropped_frame_rate, sampled),
    );

    ui.add_space(2.0);
    // 三行锁死常量：🔒 绝不动（不用「✓」字形——历史隔离空库字体缺该 glyph）。
    let lock_label = if perf.constants_locked {
        "🔒 绝不动"
    } else {
        "!!! 常量被改动，违反 R-M42-1——禁止改上传调度"
    };
    ui.label(
        RichText::new(lock_label)
            .size(11.0)
            .strong()
            .color(if perf.constants_locked {
                theme.text_secondary
            } else {
                Color32::from_rgb(0xB0, 0x30, 0x20)
            }),
    );
    for (name, value) in LOCKED_CONST_LINES {
        ui.horizontal(|ui| {
            ui.label(RichText::new(name).size(10.5).color(theme.text_secondary));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(value).size(10.5));
            });
        });
    }
}

/// `Option<f32>` 毫秒值：无值显示「—（未采样）」。
fn opt_ms(v: Option<f32>) -> String {
    match v {
        Some(ms) => format!("{ms:.1} ms"),
        None => "—（未采样）".to_owned(),
    }
}

/// 比率（0.0~1.0）：未采样显示占位，采样后显示百分比。
fn rate_or_unsampled(rate: f32, sampled: bool) -> String {
    if sampled {
        format!("{:.1}%", rate * 100.0)
    } else {
        "—（未采样）".to_owned()
    }
}

fn section(ui: &mut Ui, title: &str, theme: &Theme) {
    ui.add_space(4.0);
    ui.label(
        RichText::new(title)
            .size(12.5)
            .strong()
            .color(theme.text_primary),
    );
    ui.separator();
}

fn grid_row(ui: &mut Ui, theme: &Theme, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(11.5).color(theme.text_secondary));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).size(11.5));
        });
    });
}

fn muted(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(11.0)
            .color(Color32::from_gray(0x88)),
    );
}

/// 失败记录行的人类可读折行（供日志与测试复用）。
pub fn failure_line(f: &DiagFailure) -> String {
    format!("{} · {} · {}", f.scope, f.path, f.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_switches_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
        assert!(human_bytes(2 * 1024 * 1024 * 1024).ends_with("GB"));
    }

    #[test]
    fn failure_line_joins_fields() {
        let f = DiagFailure {
            scope: "/photos".into(),
            path: "a/b.jpg".into(),
            code: "decode_failed".into(),
        };
        assert_eq!(failure_line(&f), "/photos · a/b.jpg · decode_failed");
    }

    // ------------------------------------------------ M42-PERF-00 纯函数单测
    // 不依赖 DB / egui Context：只喂数字、断言 PerfSnapshot。

    #[test]
    fn perf_snapshot_default_has_locked_constants_and_no_samples() {
        let d = PerfSnapshot::default();
        assert_eq!(d.first_screen_ms, None);
        assert_eq!(d.first_visible_ms, None);
        assert_eq!(d.frame_samples, 0);
        assert_eq!(d.frame_p50_ms, 0.0);
        assert_eq!(d.frame_p95_ms, 0.0);
        assert_eq!(d.frame_p99_ms, 0.0);
        // 上限锁死为 3，未采样时也不编造/不改。
        assert_eq!(d.uploads_per_frame, 3);
        assert!(d.uploads_per_frame <= TEX_UPLOAD_PER_FRAME);
        assert_eq!(d.measured_max_uploads, None);
        assert_eq!(d.blank_cell_rate, 0.0);
        assert_eq!(d.dropped_frame_rate, 0.0);
        // 恒 true：三行「绝不动」的语义。
        assert!(d.constants_locked);
    }

    #[test]
    fn empty_probe_snapshot_equals_default() {
        let p = PerfProbe::new();
        assert_eq!(p.snapshot(), PerfSnapshot::default());
    }

    #[test]
    fn probe_computes_percentiles_drop_rate_blank_rate_and_upload_cap() {
        let mut p = PerfProbe::new();
        // 10 帧：8ms × 9 + 30ms × 1。可见格 10，其中 3 空白；上传 2 张/帧。
        for _ in 0..9 {
            p.note_frame(8.0, 10, 3, 2);
        }
        p.note_frame(30.0, 10, 3, 3);
        let s = p.snapshot();

        assert_eq!(s.frame_samples, 10);
        // 分位数（FrameStats 线性插值）：sorted=[8×9, 30]，n=10。
        // p50 = 8；p95 = 8 + 0.55*(30-8) ≈ 20.1；p99 = 8 + 0.91*(30-8) ≈ 28.02。
        assert!((s.frame_p50_ms - 8.0).abs() < 0.01, "p50={}", s.frame_p50_ms);
        assert!((s.frame_p95_ms - 20.1).abs() < 0.5, "p95={}", s.frame_p95_ms);
        assert!((s.frame_p99_ms - 28.02).abs() < 0.5, "p99={}", s.frame_p99_ms);
        // 掉帧率：仅 30ms 那帧 > 25.0 → 1/10 = 0.1。
        assert!((s.dropped_frame_rate - 0.1).abs() < 1e-6, "drop={}", s.dropped_frame_rate);
        // 空白格率：3/10 = 0.3。
        assert!((s.blank_cell_rate - 0.3).abs() < 1e-6, "blank={}", s.blank_cell_rate);
        // 实测峰值 3，且上限锁死为 3，绝不越过。
        assert_eq!(s.measured_max_uploads, Some(3));
        assert_eq!(s.uploads_per_frame, 3);
        assert!(s.measured_max_uploads.unwrap() <= s.uploads_per_frame);
        assert!(s.constants_locked);
    }

    #[test]
    fn slow_frame_threshold_is_strictly_greater_than_25ms() {
        let mut p = PerfProbe::new();
        // 25.0 本身不算掉帧（严格大于）；25.001 算。
        p.note_frame(25.0, 1, 0, 1);
        p.note_frame(25.001, 1, 0, 1);
        let s = p.snapshot();
        assert!((s.dropped_frame_rate - 0.5).abs() < 1e-6, "drop={}", s.dropped_frame_rate);
    }

    #[test]
    fn first_screen_and_visible_ms_are_recorded_once() {
        let mut p = PerfProbe::new();
        p.note_first_screen(123.0);
        p.note_first_screen(456.0); // 第二次不覆盖
        p.note_first_visible(42.0);
        p.note_first_visible(99.0);
        let s = p.snapshot();
        assert_eq!(s.first_screen_ms, Some(123.0));
        assert_eq!(s.first_visible_ms, Some(42.0));
    }

    #[test]
    fn zero_visible_cells_yields_zero_blank_rate() {
        let mut p = PerfProbe::new();
        p.note_frame(10.0, 0, 0, 0);
        let s = p.snapshot();
        assert_eq!(s.blank_cell_rate, 0.0);
        assert_eq!(s.frame_samples, 1);
    }

    #[test]
    fn diag_data_default_contains_default_perf_snapshot() {
        // 保证 app.rs 的 `DiagData::default()` 不改动即可编译、perf 全默认。
        let d = DiagData::default();
        assert_eq!(d.perf, PerfSnapshot::default());
    }

    #[test]
    fn helpers_render_unsampled_placeholders() {
        assert_eq!(opt_ms(None), "—（未采样）");
        assert_eq!(opt_ms(Some(12.34)), "12.3 ms");
        assert_eq!(rate_or_unsampled(0.0, false), "—（未采样）");
        assert_eq!(rate_or_unsampled(0.1, true), "10.0%");
    }
}
