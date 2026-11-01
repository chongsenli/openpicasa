//! 索引进度的三块可见界面（M2 A 线）：**右下角弹框 / 进度面板 / 状态栏入口**。
//!
//! 需求见 `docs/04` §1（弹框）、§2（入口与面板）、§3（状态与一致性）。
//!
//! ## 三条不能走样的规则（写在这里，防止后来者"优化"掉）
//!
//! 1. **数字只有一份**：三块界面都从同一个 [`IndexSnapshot`] 读，汇总与单文件夹
//!    只是同一个快照的不同切片。任何"自己订阅事件累加计数"的写法都会制造差 1 的 P0 缺陷。
//! 2. **阈值是命名常量**（[`APPEAR_AFTER`] / [`LONG_TASK`] / [`HIDE_AFTER`]），
//!    不许散落成魔法数字，也不许为了"测试好看"去改（`04` §1.4，P-05）。
//! 3. **关闭记忆只在内存**：`closed` 是一个 `HashSet<批次号>`，进程退出即清空，
//!    **绝不落盘**——写进 Preferences 只会制造下次启动的脏状态（`04` §1.2.1，P-02）。
//!
//! ## 没有 ETA
//!
//! 全文件不出现"剩余时间"。没有稳定吞吐模型时给 ETA 等于给用户一个必然落空的承诺
//! （`04` §1.3 明令禁止）。

use std::collections::HashSet;

use eframe::egui::{
    self, Align, Align2, Color32, Context, FontId, Layout, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, pos2, vec2,
};
use picasa_core::{IndexBoard, IndexSnapshot, IndexState, index::now_secs};

use crate::icons::{self, Icon};
use crate::theme::{Metrics, Theme};

/// 任务总耗时 < 1.5 s：**完全不出现**（连骨架都不画，`04` §1.4）
pub const APPEAR_AFTER: f64 = 1.5;
/// > 3 s 的任务不得提前收起（>3 s 常驻至完成）
pub const LONG_TASK: f64 = 3.0;
/// 出现后完成，1 s 内收起
pub const HIDE_AFTER: f64 = 1.0;
/// 进度快照刷新上限 4 次/秒（`04` §3.4）
pub const POLL_INTERVAL: f64 = 0.25;

/// 弹框宽度（浮层不参与栅格布局，所以是个定值）
const TOAST_W: f32 = 300.0;

/// 面板/弹框里产生的动作，交回 `app` 执行（这里不碰 Library，保持纯绘制）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgressAction {
    /// 「添加关注文件夹」——空态与入口共用
    AddFolder,
    /// 重试某个关注文件夹的失败项（路径）
    Retry(String),
    /// 全部重试（有失败项时才可用）
    RetryAll,
    /// 暂停索引（在目录边界停下，已建的索引全部保留）
    PauseIndex,
    /// 继续索引（从暂停处接着跑，不重扫）
    ResumeIndex,
    /// 删除索引数据（界面上需要二次确认，这里只负责"用户点了"）
    DeleteIndex,
    /// 面板被用户关闭
    ClosePanel,
}

/// 状态栏入口的四种表现（`04` §2.2）。**互斥**，不得同时出现两种标识。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonState {
    Idle,
    Running,
    Failed,
    Paused,
}

impl ButtonState {
    pub fn from_snapshot(s: &IndexSnapshot) -> Self {
        if s.tasks.is_empty() {
            return Self::Idle;
        }
        // 优先级：失败/不可访问 > 系统暂停 > 进行中 > 空闲
        // （"有失败不静默"比"正在跑"更需要被看见）
        if s.has_failures() || s.has_unreachable() {
            return Self::Failed;
        }
        if s.paused() {
            return Self::Paused;
        }
        if s.running() {
            return Self::Running;
        }
        Self::Idle
    }

    pub fn hover_text(self, s: &IndexSnapshot) -> String {
        match self {
            Self::Idle => "索引进度".to_string(),
            Self::Running => format!("正在索引：{}", s.stage_label()),
            Self::Failed => {
                if s.has_unreachable() {
                    "有目录不可访问".to_string()
                } else {
                    format!("{} 项失败", s.failed())
                }
            }
            Self::Paused => {
                let reason = s
                    .tasks
                    .iter()
                    .find(|t| t.state == IndexState::Paused)
                    .and_then(|t| t.reason.clone())
                    .unwrap_or_else(|| "系统挂起".into());
                format!("已暂停：{reason}")
            }
        }
    }
}

pub struct IndexProgressUi {
    snapshot: IndexSnapshot,
    rev: u64,
    next_poll: f64,

    /// 已观察到的最新批次号
    batch_seen: u64,
    /// 计划出现时刻（`None` = 本批次不打算出现）
    appear_at: Option<f64>,
    /// 弹框是否已显示
    shown: bool,
    /// 收起为一行摘要
    minimized: bool,
    /// 用户主动关闭过的批次（**只在内存**）
    closed: HashSet<u64>,
    /// 自动收起时刻
    hide_at: Option<f64>,

    panel_open: bool,
    /// 面板里展开失败明细的文件夹路径
    failures_open: Option<String>,
    /// 是否展开「已完成」的任务（F 加固：默认收起，只留一行可展开的汇总）
    show_completed: bool,

    /// 调试/验收用：本会话出现过的批次数（不做历史任务列表，只用于判定"第 N 批"）
    batches: u64,
}

impl Default for IndexProgressUi {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexProgressUi {
    pub fn new() -> Self {
        Self {
            snapshot: IndexSnapshot::default(),
            rev: u64::MAX,
            next_poll: 0.0,
            batch_seen: 0,
            appear_at: None,
            shown: false,
            minimized: false,
            closed: HashSet::new(),
            hide_at: None,
            panel_open: false,
            failures_open: None,
            show_completed: false,
            batches: 0,
        }
    }

    pub fn snapshot(&self) -> &IndexSnapshot {
        &self.snapshot
    }

    pub fn panel_open(&self) -> bool {
        self.panel_open
    }

    pub fn open_panel(&mut self) {
        self.panel_open = true;
    }

    /// 每帧调用：按 250 ms 节流取快照 + 推进弹框生命周期。
    ///
    /// **只读快照，不做任何索引工作**（R-M2-3：UI 线程不遍历、不解码、不写库）。
    pub fn tick(&mut self, ctx: &Context, board: &IndexBoard) {
        let now = ctx.input(|i| i.time);
        if self.rev != board.revision() && now >= self.next_poll {
            self.rev = board.revision();
            self.snapshot = board.snapshot();
            self.next_poll = now + POLL_INTERVAL;
        }
        self.advance(now);
    }

    /// 弹框出现/收起的状态机（`04` §1.4，P-05）。
    fn advance(&mut self, now: f64) {
        let batch = self.snapshot.batch;
        if batch != self.batch_seen {
            self.batch_seen = batch;
            self.batches = self.batches.saturating_add(1);
            self.minimized = false;
            self.hide_at = None;
            // 新批次必弹（除非这个批次已经被关闭过——批次号唯一，等价于"不会"）
            self.appear_at = if self.closed.contains(&batch) {
                None
            } else {
                Some(now + APPEAR_AFTER)
            };
        }

        if !self.shown {
            if let Some(at) = self.appear_at
                && now >= at
            {
                if self.snapshot.running() {
                    self.shown = true;
                } else {
                    // 任务在阈值时刻之前就结束了 → 连骨架都不画（<1.5 s 不出现）
                    self.appear_at = None;
                }
            }
        } else if !self.snapshot.running() {
            // 结束：有失败/不可访问 → 保持显示（降级为异常摘要），否则 1 s 内收起
            if self.snapshot.has_failures()
                || self.snapshot.has_unreachable()
                || self.snapshot.paused()
            {
                self.hide_at = None;
            } else if self.hide_at.is_none() {
                self.hide_at = Some(now + HIDE_AFTER);
            }
        } else {
            // 还在跑：任何"到点收起"都必须撤销（>3 s 不得提前收起）
            self.hide_at = None;
        }

        if let Some(at) = self.hide_at
            && now >= at
        {
            self.shown = false;
            self.hide_at = None;
        }
    }

    /// 右下角弹框。返回用户动作（关闭/收起/详情/重试）。
    pub fn show_toast(&mut self, ctx: &Context, theme: &Theme) -> Option<ProgressAction> {
        if !self.shown || self.closed.contains(&self.snapshot.batch) {
            return None;
        }
        let mut minimize = false;
        let mut close = false;
        let mut open_panel = false;
        let snap = self.snapshot.clone();
        let time = ctx.input(|i| i.time);

        egui::Area::new(egui::Id::unique("index-toast"))
            .anchor(
                Align2::RIGHT_BOTTOM,
                vec2(-12.0, -(Metrics::STATUS_H + 12.0)),
            )
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme.overlay_bg)
                    .stroke(Stroke::new(1.0, theme.separator))
                    .corner_radius(3.0)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(TOAST_W - 20.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("正在建立缩略图索引")
                                    .size(12.0)
                                    .color(theme.text_primary)
                                    .strong(),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new("×").size(12.0))
                                            .frame(false),
                                    )
                                    .on_hover_text("关闭（索引会继续）")
                                    .clicked()
                                {
                                    close = true;
                                }
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new("收起").size(11.0))
                                            .frame(false),
                                    )
                                    .clicked()
                                {
                                    minimize = true;
                                }
                            });
                        });

                        if self.minimized {
                            // 一行摘要：阶段 + 数字，仍可点开
                            let (done, found, _) = snap.totals();
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {done} / {found}",
                                        snap.stage_label()
                                    ))
                                    .size(11.0)
                                    .color(theme.text_secondary),
                                );
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new("展开").size(11.0))
                                            .frame(false),
                                    )
                                    .clicked()
                                {
                                    minimize = false;
                                }
                            });
                            return;
                        }

                        ui.add_space(2.0);
                        let folder = snap
                            .current_folder()
                            .map(|t| t.name.clone())
                            .unwrap_or_else(|| "—".into());
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(ellipsize(&folder, 16))
                                    .size(11.0)
                                    .color(theme.text_primary),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if let Some(label) = snap.batch_label() {
                                    ui.label(
                                        RichText::new(label).size(10.0).color(theme.text_secondary),
                                    );
                                }
                                ui.label(
                                    RichText::new(snap.stage_label())
                                        .size(10.5)
                                        .color(theme.text_secondary),
                                );
                            });
                        });

                        ui.add_space(3.0);
                        progress_bar(ui, snap.percent.map(|p| p as f32 / 100.0), theme, time);
                        ui.add_space(2.0);

                        let (done, found, failed) = snap.totals();
                        let mut line = if snap.percent.is_some() {
                            format!("已完成 {done} / 已发现 {found}")
                        } else {
                            format!("已发现 {found} 张")
                        };
                        if failed > 0 {
                            line.push_str(&format!(" · 失败 {failed} 项"));
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(line).size(10.5).color(theme.text_secondary));
                            if ui
                                .with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.add(
                                        egui::Button::new(
                                            RichText::new("详情").size(11.0).color(theme.progress),
                                        )
                                        .frame(false),
                                    )
                                })
                                .inner
                                .clicked()
                            {
                                open_panel = true;
                            }
                        });

                        if snap.has_failures() || snap.has_unreachable() {
                            let msg = if snap.has_unreachable() {
                                "有目录不可访问，请检查外接盘或权限".to_string()
                            } else {
                                format!("部分失败 {failed} 项，索引未完全成功")
                            };
                            ui.label(RichText::new(msg).size(10.5).color(theme.warn));
                        }
                    });
            });

        if close {
            self.shown = false;
            self.closed.insert(self.snapshot.batch);
            self.hide_at = None;
        }
        if minimize {
            self.minimized = !self.minimized;
        }
        if open_panel {
            self.panel_open = true;
        }
        None
    }

    /// 进度面板（浮层，不是模态对话框；打开时仍可滚动栅格）。
    pub fn show_panel(
        &mut self,
        ctx: &Context,
        theme: &Theme,
        paused: bool,
    ) -> Option<ProgressAction> {
        if !self.panel_open {
            return None;
        }
        let mut action = None;
        let mut open = true;
        let snap = self.snapshot.clone();
        let now = now_secs();
        let mut retry_all = false;
        let mut add_folder = false;
        let mut ask_delete = false;

        egui::Window::new("索引进度")
            .id(egui::Id::unique("index-progress-panel"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .default_pos(pos2(
                (ctx.content_rect().right() - 470.0).max(40.0),
                Metrics::TOOLBAR_H + 42.0,
            ))
            .show(ctx, |ui| {
                ui.set_width(430.0);
                if snap.tasks.is_empty() {
                    // M2-FD-44：明确空态，不显示空白列表、不留骨架、不出"未知错误"
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("暂无索引任务")
                            .size(13.0)
                            .color(theme.text_primary),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("添加关注文件夹后，程序会在后台建立缩略图；\n索引在后台进行，不阻塞浏览。")
                            .size(11.0)
                            .color(theme.text_secondary),
                    );
                    ui.add_space(8.0);
                    if ui.button("添加关注文件夹").clicked() {
                        add_folder = true;
                    }
                    // 没有任务 ≠ 没有索引数据：上次扫完、任务被清空之后，
                    // 库里可能仍然躺着几十万行。所以这里也得给「删除索引」的入口。
                    ui.add_space(4.0);
                    if ui
                        .add(egui::Button::new(RichText::new("删除索引").size(11.0)).frame(false))
                        .on_hover_text("删除已建立的照片索引与缩略图，关注文件夹保留")
                        .clicked()
                    {
                        ask_delete = true;
                    }
                    return;
                }

                // 汇总行
                let (done, found, _failed) = snap.totals();
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{} 个文件夹", snap.tasks.len()))
                            .size(11.5)
                            .color(theme.text_primary),
                    );
                    ui.label(
                        RichText::new(format!("已完成 {done} / 已发现 {found}"))
                            .size(11.5)
                            .color(theme.text_secondary),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(label) = snap.batch_label() {
                            ui.label(RichText::new(label).size(10.5).color(theme.text_secondary));
                        }
                        ui.label(
                            RichText::new(snap.stage_label())
                                .size(11.0)
                                .color(theme.text_secondary),
                        );
                    });
                });
                progress_bar(ui, snap.percent.map(|p| p as f32 / 100.0), theme, now);
                ui.add_space(4.0);

                // 已完成项自动收起（F 加固）：面板的本分是「正在发生什么」，
                // 跑完的文件夹不该继续占着位置把进行中的挤下去。默认收起，一行可展开。
                let completed_count = snap
                    .tasks
                    .iter()
                    .filter(|t| t.state.is_terminal() && t.failed == 0)
                    .count();
                if completed_count > 0 {
                    let label = if self.show_completed {
                        format!("已完成 {completed_count} 项（点击收起）")
                    } else {
                        format!("已完成 {completed_count} 项（点击展开）")
                    };
                    if ui
                        .add(egui::Button::new(RichText::new(label).size(10.5)).frame(false))
                        .clicked()
                    {
                        self.show_completed = !self.show_completed;
                    }
                }
                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for t in &snap.tasks {
                            // 已完成的（无失败的）默认不画，见上面的展开开关
                            if t.state.is_terminal() && t.failed == 0 && !self.show_completed {
                                continue;
                            }
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(ellipsize(&t.name, 22))
                                        .size(12.0)
                                        .color(theme.text_primary),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(t.state.label())
                                            .size(11.0)
                                            .color(state_color(t.state, theme)),
                                    );
                                });
                            });
                            // 路径尾段（失联/无权限时整行标异常色并给原因）
                            ui.label(
                                RichText::new(tail_path(&t.path))
                                    .size(10.0)
                                    .color(theme.text_secondary),
                            );
                            if let Some(reason) = &t.reason {
                                ui.label(
                                    RichText::new(reason)
                                        .size(10.5)
                                        .color(state_color(t.state, theme)),
                                );
                            }
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "已完成 {} / 已发现 {}",
                                        t.completed, t.discovered
                                    ))
                                    .size(10.5)
                                    .color(theme.text_secondary),
                                );
                                if t.failed > 0 {
                                    ui.label(
                                        RichText::new(format!("失败 {} 项", t.failed))
                                            .size(10.5)
                                            .color(theme.warn),
                                    );
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let rel = relative_time(now, t.last_update);
                                    ui.label(RichText::new(rel).size(10.0).color(theme.text_secondary))
                                        .on_hover_text(absolute_time(t.last_update));
                                });
                            });
                            // 进行中的行才画进度条；非进行中给了假进度就是撒谎
                            if t.state.is_running() {
                                progress_bar(
                                    ui,
                                    t.percent.map(|p| p as f32 / 100.0),
                                    theme,
                                    now,
                                );
                            }
                            if t.failed > 0 {
                                ui.horizontal(|ui| {
                                    let open = self.failures_open.as_deref() == Some(t.path.as_str());
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new(if open {
                                                    "收起失败项"
                                                } else {
                                                    "查看失败项"
                                                })
                                                .size(10.5),
                                            )
                                            .frame(false),
                                        )
                                        .clicked()
                                    {
                                        self.failures_open = if open {
                                            None
                                        } else {
                                            Some(t.path.clone())
                                        };
                                    }
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("重试失败项").size(10.5),
                                            )
                                            .frame(false),
                                        )
                                        .clicked()
                                    {
                                        action = Some(ProgressAction::Retry(t.path.clone()));
                                    }
                                });
                                if self.failures_open.as_deref() == Some(t.path.as_str()) {
                                    for f in t.failures.iter().take(50) {
                                        ui.label(
                                            RichText::new(format!("· {} — {}", f.name, f.reason))
                                                .size(10.0)
                                                .color(theme.text_secondary),
                                        );
                                    }
                                    if t.failures.len() > 50 {
                                        ui.label(
                                            RichText::new(format!(
                                                "… 另有 {} 项",
                                                t.failed.saturating_sub(50)
                                            ))
                                            .size(10.0)
                                            .color(theme.text_secondary),
                                        );
                                    }
                                }
                            }
                            ui.add_space(4.0);
                            ui.separator();
                        }
                    });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    // 暂停/继续：只有真的在跑（或有任务）时才给，空闲时按了没意义
                    if paused {
                        if ui.button("继续索引").clicked() {
                            action = Some(ProgressAction::ResumeIndex);
                        }
                    } else if (snap.running() || !snap.tasks.is_empty())
                        && ui.button("暂停索引").clicked()
                    {
                        action = Some(ProgressAction::PauseIndex);
                    }
                    if snap.has_failures() && ui.button("全部重试").clicked() {
                        retry_all = true;
                    }
                    // 放最右：破坏性操作不跟常用按钮挨着
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("删除索引").size(11.0).color(theme.warn),
                                )
                                .frame(false),
                            )
                            .on_hover_text("删除照片索引与缩略图，关注文件夹保留")
                            .clicked()
                        {
                            ask_delete = true;
                        }
                        if ui.button("关闭").clicked() {
                            self.panel_open = false;
                        }
                    });
                });
            });

        if !open {
            self.panel_open = false;
            return Some(ProgressAction::ClosePanel);
        }
        if add_folder {
            return Some(ProgressAction::AddFolder);
        }
        if retry_all {
            return Some(ProgressAction::RetryAll);
        }
        if ask_delete {
            return Some(ProgressAction::DeleteIndex);
        }
        action
    }

    /// 状态栏入口（图标 + 文案 + 四态标识）。返回是否被点击。
    ///
    /// 位置在状态栏右区，**不占栅格宽度**（`04` §2.2）。
    pub fn status_button(ui: &mut Ui, theme: &Theme, snap: &IndexSnapshot, rect: Rect) -> bool {
        let state = ButtonState::from_snapshot(snap);
        let painter = ui.painter_at(rect);
        let resp = ui.interact(
            rect,
            ui.scope_id().with("index-status-button"),
            Sense::click(),
        );
        let hot = resp.hovered();
        if hot {
            painter.rect_filled(rect, 2.0, theme.cell_hover);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let color = match state {
            ButtonState::Idle => theme.status_text,
            ButtonState::Failed => theme.danger,
            ButtonState::Paused => theme.warn,
            ButtonState::Running => theme.status_text,
        };
        let icon_rect =
            Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), vec2(11.0, 11.0));
        icons::draw(&painter, Icon::Index, icon_rect, color, 1.2);
        painter.text(
            pos2(rect.left() + 18.0, rect.center().y),
            Align2::LEFT_CENTER,
            "索引进度",
            FontId::new(11.0, egui::FontFamily::Proportional),
            color,
        );
        // 四态标识：进行中=小圆环；失败=警示点；暂停=两竖条；空闲=无
        let mark = pos2(rect.right() - 9.0, rect.center().y);
        match state {
            ButtonState::Idle => {}
            ButtonState::Running => {
                painter.circle_stroke(mark, 4.0, Stroke::new(1.2, theme.progress));
                // 轻量旋转弧（不显示百分比数字，避免与弹框抢注意力）
                let t = ui.ctx().input(|i| i.time) as f32;
                let a0 = t * 3.0;
                let pts: Vec<egui::Pos2> = (0..8)
                    .map(|i| {
                        let a = a0 + i as f32 * 0.12;
                        pos2(mark.x + 4.0 * a.cos(), mark.y + 4.0 * a.sin())
                    })
                    .collect();
                painter.add(egui::Shape::line(pts, Stroke::new(1.4, theme.progress)));
                // 5 fps 足够表达"在动"，不给栅格滚动添帧（`04` §3.4）
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(200));
            }
            ButtonState::Failed => {
                painter.circle_filled(mark, 3.5, theme.warn);
            }
            ButtonState::Paused => {
                painter.rect_filled(
                    Rect::from_min_max(
                        pos2(mark.x - 3.0, mark.y - 4.0),
                        pos2(mark.x - 1.0, mark.y + 4.0),
                    ),
                    0.5,
                    theme.warn,
                );
                painter.rect_filled(
                    Rect::from_min_max(
                        pos2(mark.x + 1.0, mark.y - 4.0),
                        pos2(mark.x + 3.0, mark.y + 4.0),
                    ),
                    0.5,
                    theme.warn,
                );
            }
        }
        let _ = resp.clone().on_hover_text(state.hover_text(snap));
        resp.clicked()
    }
}

fn state_color(state: IndexState, theme: &Theme) -> Color32 {
    match state {
        IndexState::Completed => theme.ok,
        IndexState::PartialFailed | IndexState::Unreachable => theme.danger,
        IndexState::Paused => theme.warn,
        _ => theme.progress,
    }
}

/// 进度条。`frac == None` 是**不确定态**（扫描阶段），画一条来回扫动的块，
/// 绝不用固定的 0% / 100% 冒充（`04` §3.3）。
fn progress_bar(ui: &mut Ui, frac: Option<f32>, theme: &Theme, time: f64) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 6.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, Color32::from_gray(0xD8));
    match frac {
        Some(f) => {
            let f = f.clamp(0.0, 1.0);
            if f > 0.0 {
                let fill = Rect::from_min_size(rect.min, vec2(rect.width() * f, rect.height()));
                p.rect_filled(fill, 3.0, theme.progress);
            }
        }
        None => {
            let seg = (rect.width() * 0.3).max(14.0);
            let span = rect.width() + seg;
            let x = ((time * 0.55) % 1.0) as f32 * span - seg;
            let left = (rect.left() + x).max(rect.left());
            let right = (rect.left() + x + seg).min(rect.right());
            if right > left {
                p.rect_filled(
                    Rect::from_min_max(pos2(left, rect.top()), pos2(right, rect.bottom())),
                    3.0,
                    theme.progress,
                );
            }
            // ⚠️ 这里是**不定态动画**，必须持续重绘，但绝不能 `request_repaint()` 不设间隔：
            // 那等于「下一帧立刻再画一遍」，在走 llvmpipe 软件光栅（沙箱无 `/dev/dri` 时
            // 必然如此）的机器上是 2 个核持续 100%，UI 反而更卡。
            // 100 ms = 10 fps，一个来回扫动的块看不出来是 10 fps，但 CPU 降到 1/6。
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// 面板里给「状态点」用的小圆（左栏与面板共用同一颜色语义）。
pub fn draw_state_dot(
    painter: &egui::Painter,
    center: egui::Pos2,
    state: IndexState,
    theme: &Theme,
) {
    let color = state_color(state, theme);
    if state == IndexState::Paused {
        painter.rect_filled(Rect::from_center_size(center, vec2(6.0, 2.0)), 0.5, color);
    } else {
        painter.circle_filled(center, 3.0, color);
    }
    // 已完成用空心环：避免左栏出现一排"红绿点"抢注意力
    if state == IndexState::Completed {
        painter.circle_filled(center, 3.0, theme.sidebar_bg);
        painter.circle_stroke(center, 3.0, Stroke::new(1.2, color));
    }
}

/// 占位格（`04` §3.2.1）：灰底 +「待生成」，**不放任何图片内容**。
pub fn draw_placeholder(painter: &egui::Painter, rect: Rect, theme: &Theme, failed: bool) {
    painter.rect_filled(rect.shrink(1.0), 1.0, theme.placeholder_bg);
    painter.rect_stroke(
        rect,
        1.0,
        Stroke::new(1.0, theme.cell_border),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        if failed { "失败" } else { "待生成" },
        FontId::new(11.0, egui::FontFamily::Proportional),
        if failed {
            theme.warn
        } else {
            theme.text_secondary
        },
    );
    if failed {
        // 警示角标：失败的那张不消失、也不假装"文件不存在"
        let c = pos2(rect.right() - 9.0, rect.top() + 9.0);
        painter.circle_filled(c, 4.5, theme.warn);
        painter.text(
            c,
            Align2::CENTER_CENTER,
            "!",
            FontId::new(8.0, egui::FontFamily::Proportional),
            Color32::WHITE,
        );
    }
}

pub fn ellipsize(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 路径尾段：`/Users/x/Pictures/2020/05` → `…/2020/05`（超长省略中段）
fn tail_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    format!("…/{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
}

pub fn relative_time(now: f64, ts: f64) -> String {
    if ts <= 0.0 {
        return "—".into();
    }
    let d = (now - ts).max(0.0);
    if d < 10.0 {
        "刚刚".into()
    } else if d < 60.0 {
        format!("{} 秒前", d as u64)
    } else if d < 3600.0 {
        format!("{} 分钟前", (d / 60.0) as u64)
    } else {
        format!("{} 小时前", (d / 3600.0) as u64)
    }
}

pub fn absolute_time(ts: f64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(ts as i64, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => "—".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use picasa_core::index::{FailureItem, IndexBoard};

    fn ui_with_snapshot(snap: IndexSnapshot) -> IndexProgressUi {
        let mut ui = IndexProgressUi::new();
        ui.batch_seen = snap.batch;
        ui.snapshot = snap;
        ui
    }

    fn running_board() -> IndexBoard {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into()]);
        b.mark_scanning("/p/a");
        b.freeze_denominator("/p/a", 10, 0);
        b
    }

    #[test]
    fn fast_task_never_appears() {
        let b = IndexBoard::new();
        b.begin_batch(&["/p/a".into()]);
        b.mark_scanning("/p/a");
        b.freeze_denominator("/p/a", 2, 0);
        b.note_thumb_ok("/p/a");
        b.note_thumb_ok("/p/a");
        b.finish("/p/a");
        let mut ui = ui_with_snapshot(b.snapshot());
        ui.appear_at = Some(1.5);
        ui.advance(1.6);
        assert!(!ui.shown, "< 1.5 s 完成的任务不得出现");
        assert!(ui.appear_at.is_none());
    }

    #[test]
    fn long_task_appears_after_threshold_and_hides_within_1s() {
        let b = running_board();
        let mut ui = ui_with_snapshot(b.snapshot());
        ui.appear_at = Some(1.5);
        ui.advance(1.0);
        assert!(!ui.shown, "到达阈值前不得出现（不允许先出现再消失）");
        ui.advance(1.6);
        assert!(ui.shown, "超过阈值且仍在跑 → 出现");

        // 完成 → 1 s 内收起
        for _ in 0..10 {
            b.note_thumb_ok("/p/a");
        }
        b.finish("/p/a");
        ui.snapshot = b.snapshot();
        ui.advance(5.0);
        assert!(ui.shown, "刚完成的一瞬间还在（等 1 s）");
        ui.advance(6.1);
        assert!(!ui.shown, "完成后 1 s 内收起");
    }

    #[test]
    fn failures_keep_toast_on_screen() {
        let b = running_board();
        b.note_thumb_failed(
            "/p/a",
            FailureItem {
                photo_id: 1,
                name: "bad.jpg".into(),
                reason: "解码失败".into(),
            },
        );
        b.finish("/p/a");
        let mut ui = ui_with_snapshot(b.snapshot());
        ui.shown = true;
        ui.advance(100.0);
        assert!(ui.shown, "有失败项时弹框不得自动收起");
        assert!(ui.hide_at.is_none());
    }

    #[test]
    fn close_memory_is_per_batch() {
        let b = running_board();
        let mut ui = ui_with_snapshot(b.snapshot());
        ui.shown = true;
        ui.closed.insert(b.snapshot().batch);
        ui.shown = false;
        // 同批次再推进：不会自己回来
        ui.advance(10.0);
        assert!(!ui.shown);

        // 新批次（重试）：必弹
        b.begin_retry("/p/a", 10, 9, 1);
        ui.snapshot = b.snapshot();
        ui.advance(11.0);
        ui.advance(13.0);
        assert!(ui.appear_at.is_some(), "新批次要重新排出现");
    }

    #[test]
    fn button_state_priority() {
        let b = running_board();
        assert_eq!(
            ButtonState::from_snapshot(&b.snapshot()),
            ButtonState::Running
        );
        b.note_thumb_failed(
            "/p/a",
            FailureItem {
                photo_id: 1,
                name: "bad.jpg".into(),
                reason: "解码失败".into(),
            },
        );
        assert_eq!(
            ButtonState::from_snapshot(&b.snapshot()),
            ButtonState::Failed,
            "有失败时优先显示警示点"
        );
        b.pause_all("系统休眠");
        assert_eq!(
            ButtonState::from_snapshot(&b.snapshot()),
            ButtonState::Failed,
            "四态互斥：有失败就不叠加暂停标识"
        );
    }

    #[test]
    fn empty_snapshot_is_idle() {
        assert_eq!(
            ButtonState::from_snapshot(&IndexSnapshot::default()),
            ButtonState::Idle
        );
    }

    #[test]
    fn relative_time_reads_naturally() {
        assert_eq!(relative_time(1000.0, 995.0), "刚刚");
        assert_eq!(relative_time(1000.0, 970.0), "30 秒前");
        assert_eq!(relative_time(1000.0, 400.0), "10 分钟前");
        assert_eq!(relative_time(10_000.0, 400.0), "2 小时前");
    }
}
