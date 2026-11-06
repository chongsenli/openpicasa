//! # 索引管理页（M2.2 §4.1）—— 四个标签、一个页面
//!
//! ## 它取代了什么（R-M22-5 零功能丢失）
//!
//! M2.1 里索引相关的操作散在三个地方：工具栏「索引管理」二级菜单、底部「诊断」按钮、
//! 底部「索引进度」按钮。分散的代价不是"不好看"，是**功能会漏**：
//! 加一个新操作时要记得在三个地方都加一遍，漏一处就是"功能消失了"。
//!
//! 本页把它们收成一个入口。§4.3 那张对照表里的每一行，在这里都有落点。
//!
//! ## 本模块**只画不改**
//!
//! 所有写库动作都变成 [`MgrAction`] 交回 `app` 执行（与 `diagnostics.rs` 同款）。
//! 界面层不直连 SQL、不自己 `SELECT`：所有数字都来自 [`MgmtSnapshot`]，
//! 而快照由 core 以 ≤ 4 次/秒发布（§6.1）。这条同时是 R-M22-6（四处一致）的保障。
//!
//! ## 无窗口也能测
//!
//! 画界面需要 `egui::Ui`，但**决定画什么**的逻辑全是纯函数：
//! [`visible_tasks`]、[`human_bytes`]、[`relative_time`]、[`root_summary_line`]、
//! [`Confirm::title`]……它们都在这里有单元测试。

use eframe::egui::{self, Color32, Context, RichText, Ui};
use picasa_core::{ExclusionView, MgmtSnapshot, MgmtTab, RootView, TaskView};
use picasa_db::{FailureGroup, FailureKind};

use crate::theme::Theme;

// ---------------------------------------------------------------- 动作

/// 页面上用户能点出的动作。交回 `app` 执行，本模块**不碰数据库**。
#[derive(Clone, Debug, PartialEq)]
pub enum MgrAction {
    // ---- 任务标签
    CancelTask(i64),
    RetryKind(FailureKind),
    IgnoreKind(FailureKind),
    UnignoreKind(FailureKind),
    // ---- 关注文件夹标签
    RefreshRoot(i64),
    ReindexRoot(i64),
    RebuildRoot(i64),
    CloseRoot(i64),
    ReopenRoot(i64),
    RevealRoot(i64),
    DeleteIndexRoot(i64),
    // ---- 排除项标签
    RestoreExclusion(i64),
    PruneStaleExclusions,
    // ---- 存储与诊断标签
    ClearThumbs,
    Vacuum,
    ExportDiagLog,
    ToggleFrameStats(bool),
    // ---- M4「导入 / 导出」标签（docs/12 S2～S7）
    /// 打开系统保存对话框 → 导出（携带是否含缩略图）
    ExportIndex { with_thumbs: bool },
    /// 打开系统文件对话框 → 选择索引文件并校验/预览
    PickImportFile,
    /// 导入：切换合并 / 替换策略
    SetImportStrategy(picasa_db::ImportStrategy),
    /// 导入：开始执行（替换策略会先弹二次确认）
    StartImport,
    /// 导入：取消
    CancelImport,
    /// 导入后重新匹配某个来源根目录：(根路径, 用户选的本机目录)
    RemapRoot { from: String, to: String },
}

// ---------------------------------------------------------------- 二次确认

/// 需要二次确认的危险动作（§4.2 M22-IDX-06）。
///
/// ⚠️ 全量重建的确认文案**必须**写清两件事：清的是缩略图、照片文件不受影响。
/// 用户看到「重建」两个字的第一反应是"我的照片要没了"——不说清楚就是在吓人。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Confirm {
    #[default]
    None,
    /// 全量重建：`(root_id, 该根的照片数)`
    RebuildRoot { id: i64, photos: u64 },
    /// 删除索引：`(root_id, 照片数)`
    DeleteIndex { id: i64, photos: u64 },
    /// 清理全部缩略图
    ClearThumbs,
}

impl Confirm {
    /// 确认框标题。
    pub fn title(self) -> &'static str {
        match self {
            Self::None => "",
            Self::RebuildRoot { .. } => "确认全量重建",
            Self::DeleteIndex { .. } => "确认删除索引",
            Self::ClearThumbs => "确认清理缩略图",
        }
    }

    /// 确认框正文。数量**必须**出现在文案里（§4.2验收要点）。
    pub fn body(self) -> String {
        match self {
            Self::None => String::new(),
            Self::RebuildRoot { photos, .. } => format!(
                "将清除该文件夹全部 {photos} 张照片的缩略图并重新生成，照片文件不受影响。\n\n\
                 重建期间旧缩略图保持可见，直到被新图替换。"
            ),
            Self::DeleteIndex { photos, .. } => format!(
                "将删除该文件夹 {photos} 张照片的索引与缩略图，照片文件不受影响。\n\n\
                 关注关系与已关闭子目录的设置会保留。"
            ),
            Self::ClearThumbs => {
                "将清除全部缩略图，下次浏览时重新生成。照片文件不受影响。".to_string()
            }
        }
    }

    pub fn is_none(self) -> bool {
        matches!(self, Self::None)
    }
}

// ---------------------------------------------------------------- 页面状态

/// M4 导入 / 导出操作的六种状态（docs/11 §2.1.2、docs/12 §9.1）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IoState {
    #[default]
    Idle,
    /// 校验 / 计算预计内容 / 读清单；总数未知时不显示百分比
    Preparing,
    /// 进行中（进度条 + 已处理 N / M，无 ETA）
    Running,
    Success,
    Failed,
    Cancelled,
}

/// 导入向导的四步（docs/12 S3～S6）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImportStep {
    #[default]
    Validate,
    Strategy,
    Execute,
    Result,
}

/// M4「导入 / 导出」标签的会话态（不进偏好）。
#[derive(Clone, Debug, Default)]
pub struct IoUi {
    pub state: IoState,
    /// 进度文案（"已处理 620 / 1 249 项"），M 未知时只写 "已处理 N 项"
    pub progress: String,
    /// 0..1，None = 不确定态（循环条）
    pub fraction: Option<f32>,
    /// 结果 / 错误 / 取消的正文（可理解的人话）
    pub message: String,
    /// 上次操作结果摘要（空闲态显示）
    pub last_result: String,
    /// 导出目标路径（用户选的；显示用）
    pub export_path: String,
    pub export_with_thumbs: bool,
    /// 导入文件路径
    pub import_path: String,
    pub import_step: ImportStep,
    pub strategy: picasa_db::ImportStrategy,
    /// 导入预览（校验通过后由 app 填入）
    pub preview: Option<picasa_db::ImportPreview>,
    /// 导入结果（执行完成后由 app 填入）
    pub report: Option<picasa_db::ImportReport>,
    /// 需要重新匹配的来源根（路径, 是否已匹配）
    pub roots_to_remap: Vec<(String, bool)>,
    /// 我已了解影响范围（替换策略勾选框）
    pub understood: bool,
}

/// 索引管理页的界面状态（**不进偏好**：它是临时窗口状态，不该跨会话复活）。
#[derive(Clone, Debug, Default)]
pub struct IndexMgrUi {
    pub open: bool,
    pub tab: MgmtTab,
    pub confirm: Confirm,
    /// 帧时间直方图浮层开关（PERF-18，存这里因为是「诊断」的一部分）
    pub frame_stats: bool,
    /// M4 导入 / 导出标签状态
    pub io: IoUi,
}

impl IndexMgrUi {
    pub fn new() -> Self {
        Self::default()
    }

    /// 打开，并定位到**有问题的那个标签**（§4.2 M22-IDX-02）。
    ///
    /// 用户点状态栏角标进来，期望的是直接看到问题在哪，
    /// 而不是自己在四个标签里找一遍。
    pub fn open_at(&mut self, snap: &MgmtSnapshot, failed_tasks: u64) {
        self.open = true;
        self.tab = snap.default_tab(failed_tasks);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.confirm = Confirm::None;
    }
}

// ---------------------------------------------------------------- 纯函数（可单测）

/// 字节数转人类可读（1.2 GB / 34.5 MB / 812 KB / 42 B）。
pub fn human_bytes(n: u64) -> String {
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

/// 「3 分钟前 / 2 小时前 / 3 天前 / —」。
///
/// ⚠️ 只做**相对时间**的粗粒度：索引管理页回答的是"最近跑过没有"，
/// 精确到秒的时间戳在这里是噪音。
pub fn relative_time(at: Option<i64>, now: i64) -> String {
    let Some(t) = at else {
        return "从未".to_string();
    };
    let d = now - t;
    if d < 0 {
        // 时钟回拨（NTP 校正、用户改系统时间）——显示"刚刚"比显示负数合理
        return "刚刚".to_string();
    }
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    match d {
        d if d < MIN => "刚刚".to_string(),
        d if d < HOUR => format!("{} 分钟前", d / MIN),
        d if d < DAY => format!("{} 小时前", d / HOUR),
        d if d < 30 * DAY => format!("{} 天前", d / DAY),
        _ => "30 天前以上".to_string(),
    }
}

/// 任务行右侧的「已处理 N 张 · 已用 mm:ss」（§4.2 M22-IDX-08）。
///
/// ⚠️ **没有 ETA**，也不许加：`TC-IDX-003` 是永久禁令。
/// 分母在扫描期间一直在漂，任何 ETA 都是撒谎。
pub fn task_progress_text(t: &TaskView) -> String {
    format!("已处理 {} 张 · 已用 {}", t.discovered, t.elapsed_text)
}

/// 「关注文件夹」标签里一行的摘要：`状态 · N 张 · 占用 · 失败 M`。
pub fn root_summary_line(r: &RootView) -> String {
    let mut s = format!("{} · {} 张", r.status_text(), r.photo_count);
    if r.thumb_bytes > 0 {
        s.push_str(&format!(" · 缩略图 {}", human_bytes(r.thumb_bytes)));
    }
    if r.failed > 0 {
        s.push_str(&format!(" · 失败 {}", r.failed));
    }
    s
}

/// 任务标签要显示的任务行：进行中的在前，历史按结束时间倒序。
///
/// 历史已经在 `build_snapshot` 里按 `finished_at DESC LIMIT 50` 取好，
/// 这里只负责把"活任务"拼到前面——**不去重**：同一个任务不会既在活在历史里
/// （历史只取终态），拼错了会显示两行，那是数据层该修的 bug，不该在 UI 里藏。
pub fn visible_tasks(snap: &MgmtSnapshot) -> Vec<&TaskView> {
    let mut out: Vec<&TaskView> = snap.tasks.iter().filter(|t| t.running).collect();
    out.extend(snap.tasks.iter().filter(|t| !t.running));
    out
}

/// 排除项标签里「批量恢复 / 清理失效项」是否可用。
pub fn exclusion_bulk_state(items: &[ExclusionView]) -> (bool, bool) {
    (!items.is_empty(), items.iter().any(|e| e.path_missing))
}

/// 某类失败该不该给「重试全部」按钮。
///
/// `Unsupported` 是唯一的永久失败（§4.5），给它重试按钮的结果是
/// 用户点一次失败一次，最后以为软件坏了。
pub fn retry_enabled(g: &FailureGroup) -> bool {
    let Some(kind) = g.kind.and_then(FailureKind::from_code) else {
        return false;
    };
    kind.retryable() && g.count > 0
}

// ---------------------------------------------------------------- 绘制

/// 画索引管理页。返回本次帧点出的动作（可能有多个）。
pub fn show(
    ctx: &Context,
    theme: &Theme,
    state: &mut IndexMgrUi,
    snap: &MgmtSnapshot,
    failed_tasks: u64,
    now: i64,
) -> Vec<MgrAction> {
    let mut actions = Vec::new();
    if !state.open {
        return actions;
    }

    let mut open = state.open;
    let mut tab = state.tab;
    let mut frame_stats = state.frame_stats;

    egui::Window::new("索引管理")
        // ⚠️ 非模态（`04` §2.1）：关闭**不影响后台任务**——这不是"取消"，
        // 只是把窗口收起来。用户关掉页面后索引必须继续跑。
        .open(&mut open)
        .resizable(true)
        .default_size([760.0, 560.0])
        .min_size([560.0, 380.0])
        .show(ctx, |ui| {
            // ---- M4 总览摘要区（常驻，不随标签切换消失；docs/12 §2）
            // 五字段全部来自同一份 MgmtSnapshot，与主区标题 / 状态栏数字同源。
            overview_summary(ui, theme, snap, now);
            ui.separator();
            // ---- 五个标签（M4 新增「导入 / 导出」）
            ui.horizontal(|ui| {
                for t in MgmtTab::ALL {
                    let sel = tab == t;
                    if ui.selectable_label(sel, t.label()).clicked() {
                        tab = t;
                    }
                }
                ui.separator();
                // 角标解释（§4.4 悬停三分量 → 这里改成常驻一行小字，
                // 因为悬停提示在无窗口测试里验不了，而"角标为什么是 3"必须能自证）
                if snap.badge > 0 {
                    ui.label(
                        RichText::new(snap.badge_tooltip(failed_tasks).join(" · "))
                            .size(11.0)
                            .color(theme.warn),
                    );
                }
            });
            ui.separator();

            egui::ScrollArea::vertical()
                .id_salt("index-mgr-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| match tab {
                    MgmtTab::Tasks => tasks_tab(ui, theme, snap, &mut actions),
                    MgmtTab::Folders => folders_tab(ui, theme, snap, now, &mut actions),
                    MgmtTab::Exclusions => exclusions_tab(ui, theme, snap, &mut actions),
                    MgmtTab::Storage => storage_tab(
                        ui,
                        theme,
                        snap,
                        &mut frame_stats,
                        &mut actions,
                        &mut state.confirm,
                    ),
                    // M4 第五标签：导入 / 导出（总览摘要区见 show() 开头，
                    // 具体操作与四态反馈见 import_export_tab）。
                    MgmtTab::ImportExport => import_export_tab(ui, theme, state, &mut actions),
                });
        });

    state.open = open;
    state.tab = tab;
    state.frame_stats = frame_stats;
    if !open {
        state.confirm = Confirm::None;
    }

    // 二次确认（画在页面之外，避免滚动区把它带走）
    if !state.confirm.is_none() {
        confirm_dialog(ctx, state, &mut actions);
    }

    actions
}

fn tasks_tab(ui: &mut Ui, theme: &Theme, snap: &MgmtSnapshot, actions: &mut Vec<MgrAction>) {
    let tasks = visible_tasks(snap);

    // ---- 失败分类分组（§4.2 M22-IDX-04）：永远画在最上面
    section(ui, theme, "失败分类");
    let mut any = false;
    for (i, g) in snap.failures_by_kind.iter().enumerate() {
        let Some(kind) = FailureKind::from_code(i as i64 + 1) else {
            continue;
        };
        let total = g.count + g.ignored;
        if total == 0 {
            continue;
        }
        any = true;
        ui.horizontal(|ui| {
            ui.label(RichText::new(kind.label()).strong());
            ui.label(format!("{} 条", g.count));
            if g.ignored > 0 {
                ui.label(
                    RichText::new(format!("已忽略 {}（仍可见）", g.ignored))
                        .size(11.0)
                        .color(theme.text_secondary),
                );
            }
            if retry_enabled(g) {
                if ui.button("重试全部").clicked() {
                    actions.push(MgrAction::RetryKind(kind));
                }
            } else {
                // 不可重试的分类也**必须给出口**（R-M2-4）：给「忽略」而不是留空
                ui.label(
                    RichText::new("不支持重试")
                        .size(11.0)
                        .color(theme.text_disabled),
                );
            }
            if g.count > 0 && ui.button("忽略全部").clicked() {
                actions.push(MgrAction::IgnoreKind(kind));
            }
            if g.ignored > 0 && ui.button("取消忽略").clicked() {
                actions.push(MgrAction::UnignoreKind(kind));
            }
        });
    }
    if !any {
        ui.label(RichText::new("没有失败项").color(theme.text_secondary));
    }

    // ---- 任务列表
    section(ui, theme, "任务");
    if tasks.is_empty() {
        // M2-FD-44 的空态文案**不动**
        ui.label(RichText::new("暂无索引任务").color(theme.text_secondary));
        return;
    }
    for t in tasks {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&t.scope).strong());
            ui.label(&t.kind);
            ui.label(&t.status);
            ui.label(task_progress_text(t));
            if t.running {
                if ui.button("取消").clicked() {
                    actions.push(MgrAction::CancelTask(t.task_id));
                }
            } else if let Some(o) = &t.outcome {
                ui.label(RichText::new(o).size(11.0).color(theme.text_secondary));
            }
        });
    }

    // P-03：系统暂停态**不给「继续」按钮**，只如实说明
    if snap.paused_by_system {
        ui.label(
            RichText::new("系统暂停中（休眠 / 低电量），恢复由系统事件自动发生")
                .size(11.0)
                .color(theme.warn),
        );
    }
}

fn folders_tab(
    ui: &mut Ui,
    theme: &Theme,
    snap: &MgmtSnapshot,
    now: i64,
    actions: &mut Vec<MgrAction>,
) {
    if snap.roots.is_empty() {
        ui.label(RichText::new("还没有关注任何文件夹").color(theme.text_secondary));
        return;
    }
    // 角标里监视分量的来源说明（R-M3-7：降级必须在页面可见，不能只在 tooltip 里）
    if snap.watch_degraded_roots > 0 {
        ui.label(
            RichText::new(format!(
                "{} 个文件夹的实时监视不可用，已改为定时检查（详情见各行的「为什么？」）",
                snap.watch_degraded_roots
            ))
            .size(11.0)
            .color(theme.warn),
        );
        ui.add_space(4.0);
    }
    for r in &snap.roots {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&r.path).strong());
                ui.label(RichText::new(r.status_text()).color(if r.offline {
                    theme.danger
                } else if r.closed {
                    theme.text_disabled
                } else {
                    theme.ok
                }));
            });
            ui.label(
                RichText::new(root_summary_line(r))
                    .size(11.0)
                    .color(theme.text_secondary),
            );
            ui.label(
                RichText::new(format!(
                    "最近索引：{}",
                    relative_time(r.last_indexed_at, now)
                ))
                .size(11.0)
                .color(theme.text_secondary),
            );

            // ---- M3-UI-01（U-61）：监视状态列
            //
            // R-M3-7 的硬要求：任何根处于非 Active 状态都必须**逐行可见**。
            // 所以这一行永远画（`Active` 也画，显示「实时」），
            // 而不是「有问题才出现」——后者会让「一切正常」和「这行没渲染」
            // 长得一模一样，而后者正是缺陷。
            ui.horizontal(|ui| {
                ui.label(RichText::new("监视").size(11.0).color(theme.text_secondary));
                let color = if r.watch.is_active() {
                    theme.ok
                } else if r.watch.counts_for_badge() {
                    theme.warn
                } else {
                    theme.text_disabled
                };
                ui.label(RichText::new(r.watch.label()).size(11.0).color(color));
                // 「为什么？」：只有真的降级时才给链接（正常态点它没有意义）
                if r.watch.counts_for_badge()
                    && let Some(advice) = r.watch.advice()
                {
                    ui.label(
                        RichText::new("为什么？")
                            .size(11.0)
                            .color(theme.text_secondary),
                    )
                    .on_hover_text(advice);
                }
            });

            ui.horizontal(|ui| {
                // 离线根：只给「连接后刷新」（§2.8），其他按钮点了必然失败
                if r.offline {
                    if ui.button("连接后刷新").clicked() {
                        actions.push(MgrAction::RefreshRoot(r.id));
                    }
                } else {
                    if ui.button("刷新").clicked() {
                        actions.push(MgrAction::RefreshRoot(r.id));
                    }
                    if ui.button("重新索引").clicked() {
                        actions.push(MgrAction::ReindexRoot(r.id));
                    }
                    if ui.button("全量重建").clicked() {
                        actions.push(MgrAction::RebuildRoot(r.id));
                    }
                    if r.closed {
                        if ui.button("重新打开").clicked() {
                            actions.push(MgrAction::ReopenRoot(r.id));
                        }
                    } else if ui.button("关闭").clicked() {
                        actions.push(MgrAction::CloseRoot(r.id));
                    }
                    if ui.button("在 Finder 中显示").clicked() {
                        actions.push(MgrAction::RevealRoot(r.id));
                    }
                }
                if ui.button("删除索引").clicked() {
                    actions.push(MgrAction::DeleteIndexRoot(r.id));
                }
            });
        });
    }
}

fn exclusions_tab(ui: &mut Ui, theme: &Theme, snap: &MgmtSnapshot, actions: &mut Vec<MgrAction>) {
    let (any, any_stale) = exclusion_bulk_state(&snap.exclusions);
    if !any {
        ui.label(RichText::new("没有已关闭的子目录").color(theme.text_secondary));
        return;
    }
    ui.horizontal(|ui| {
        // 批量恢复与清理失效项（§4.1 第三行）
        if ui.button("全部恢复").clicked() {
            for e in &snap.exclusions {
                actions.push(MgrAction::RestoreExclusion(e.id));
            }
        }
        ui.add_enabled(any_stale, egui::Button::new("清理失效项"));
        if any_stale && ui.button("清理失效项").clicked() {
            actions.push(MgrAction::PruneStaleExclusions);
        }
    });
    for e in &snap.exclusions {
        ui.horizontal(|ui| {
            ui.label(&e.path);
            if e.path_missing {
                ui.label(RichText::new("已失效").size(11.0).color(theme.warn));
            }
            if ui.button("恢复").clicked() {
                actions.push(MgrAction::RestoreExclusion(e.id));
            }
        });
    }
}

fn storage_tab(
    ui: &mut Ui,
    theme: &Theme,
    snap: &MgmtSnapshot,
    frame_stats: &mut bool,
    actions: &mut Vec<MgrAction>,
    confirm: &mut Confirm,
) {
    let s = &snap.storage;
    section(ui, theme, "占用");
    row(ui, theme, "索引库文件", &human_bytes(s.db_bytes));
    row(ui, theme, "WAL", &human_bytes(s.wal_bytes));
    row(
        ui,
        theme,
        "缩略图",
        // 未算完必须说「计算中」，**不能显示 0**：0 会被读成「我没缩略图」，
        // 而真相只是后台线程还没跑完（冷缓存下要 1 秒）。
        &if s.thumb_stats_known {
            format!("{} 条 · {}", s.thumb_count, human_bytes(s.thumb_bytes))
        } else {
            "计算中…".to_string()
        },
    );
    row(ui, theme, "照片", &format!("{} 张", s.photo_count));

    section(ui, theme, "按根目录分布");
    if s.by_root.is_empty() {
        // B4′：统计还没回来时也不能给一块空白。用快照里已有的根先占行，
        // 值明确写「计算中…」，算完后由 `apply_storage` 换成真实字节数。
        if snap.roots.is_empty() {
            ui.label(
                RichText::new("还没有关注文件夹")
                    .size(11.0)
                    .color(theme.text_secondary),
            );
        } else {
            for r in &snap.roots {
                let value = if s.thumb_stats_known {
                    human_bytes(r.thumb_bytes)
                } else {
                    "计算中…".to_string()
                };
                row(ui, theme, &r.path, &value);
            }
        }
    } else {
        for (path, bytes) in &s.by_root {
            row(ui, theme, path, &human_bytes(*bytes));
        }
    }

    section(ui, theme, "操作");
    ui.horizontal(|ui| {
        if ui.button("清理缩略图").clicked() {
            *confirm = Confirm::ClearThumbs;
        }
        if ui.button("VACUUM").clicked() {
            actions.push(MgrAction::Vacuum);
        }
        if ui.button("导出诊断日志").clicked() {
            actions.push(MgrAction::ExportDiagLog);
        }
    });
    // PERF-18：帧时间直方图开关
    ui.checkbox(frame_stats, "显示帧时间直方图（p50/p95/p99/max）");
}

fn confirm_dialog(ctx: &Context, state: &mut IndexMgrUi, actions: &mut Vec<MgrAction>) {
    let confirm = state.confirm;
    let mut open = true;
    egui::Window::new(confirm.title())
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(confirm.body());
            ui.horizontal(|ui| {
                if ui.button("确认").clicked() {
                    match confirm {
                        Confirm::RebuildRoot { id, .. } => actions.push(MgrAction::RebuildRoot(id)),
                        Confirm::DeleteIndex { id, .. } => {
                            actions.push(MgrAction::DeleteIndexRoot(id))
                        }
                        Confirm::ClearThumbs => actions.push(MgrAction::ClearThumbs),
                        Confirm::None => {}
                    }
                    state.confirm = Confirm::None;
                }
                if ui.button("取消").clicked() {
                    state.confirm = Confirm::None;
                }
            });
        });
    if !open {
        state.confirm = Confirm::None;
    }
}

// ---------------------------------------------------------------- M4 总览摘要区

/// 总览摘要区（docs/11 §2.1.1、docs/12 §2）：标签栏之上常驻。
///
/// 数据源**只有** `MgmtSnapshot` 一份，不自行维护任何计数；异常常驻且可点击定位。
fn overview_summary(ui: &mut Ui, theme: &Theme, snap: &MgmtSnapshot, now: i64) {
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            // 图库根目录：逐行（非离线根也要可见，R-M3-7）
            ui.horizontal(|ui| {
                ui.label(RichText::new("图库根目录").color(theme.text_secondary));
                if snap.roots.is_empty() {
                    ui.label(RichText::new("尚未添加图库根目录").color(theme.warn));
                } else {
                    ui.vertical(|ui| {
                        for r in &snap.roots {
                            let s = root_summary_line(r);
                            let color = if r.offline { theme.warn } else { theme.text_primary };
                            ui.label(RichText::new(s).color(color).size(11.5));
                        }
                    });
                }
            });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                // 索引状态：最近活跃任务，无任务显示空闲
                let status = snap
                    .tasks
                    .iter()
                    .find(|t| t.running)
                    .map(|t| t.status.clone())
                    .unwrap_or_else(|| "空闲".to_string());
                ui.label(RichText::new("索引状态").color(theme.text_secondary));
                ui.label(RichText::new(format!("● {status}")).strong());
                ui.add_space(16.0);
                // 照片数量：与 main 标题同源的 all_photos
                ui.label(RichText::new("照片数量").color(theme.text_secondary));
                ui.label(RichText::new(format!("{} 张", snap.all_photos)).strong());
                ui.add_space(16.0);
                // 最近更新：取根目录 / 任务里最近的时间戳（同一快照内）
                let last = snap
                    .roots
                    .iter()
                    .filter_map(|r| r.last_indexed_at)
                    .chain(snap.tasks.iter().filter_map(|t| t.started_at))
                    .max();
                let rel = match last {
                    Some(t) => relative_time(Some(t), now),
                    None => "尚未索引".to_string(),
                };
                ui.label(RichText::new("最近更新").color(theme.text_secondary));
                ui.label(RichText::new(rel));
            });
            // 异常提示：0 异常时整行不渲染（不显示「一切正常」式噪音）
            if snap.badge > 0 {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⚠").color(theme.warn));
                    ui.label(
                        RichText::new(snap.badge_tooltip(0).join(" · "))
                            .color(theme.warn)
                            .size(11.0),
                    );
                });
            }
        });
}

// ---------------------------------------------------------------- M4 导入 / 导出 标签

/// 第五标签：导出、导入四步向导、四态反馈、二次确认入口（docs/12 S2～S7）。
fn import_export_tab(
    ui: &mut Ui,
    theme: &Theme,
    state: &mut IndexMgrUi,
    actions: &mut Vec<MgrAction>,
) {
    let io = &mut state.io;
    section(ui, theme, "导出索引文件");
    ui.label(
        RichText::new("把索引记录与整理关系存成文件 · 不含原始照片")
            .color(theme.text_secondary)
            .size(11.5),
    );
    ui.horizontal(|ui| {
        ui.checkbox(&mut io.export_with_thumbs, "包含缩略图（生成索引包）");
    });
    if ui.button("导出索引文件…").clicked() {
        actions.push(MgrAction::ExportIndex {
            with_thumbs: io.export_with_thumbs,
        });
    }
    ui.separator();

    section(ui, theme, "导入索引文件");
    ui.label(
        RichText::new("从文件恢复索引与整理关系 · 导入前可预览、可选合并 / 替换")
            .color(theme.text_secondary)
            .size(11.5),
    );
    if ui.button("选择索引文件…").clicked() {
        actions.push(MgrAction::PickImportFile);
    }

    // 四步向导（校验通过后才展开）
    if io.preview.is_some() {
        ui.separator();
        let steps = [
            ("① 校验与预览", ImportStep::Validate),
            ("② 策略与范围", ImportStep::Strategy),
            ("③ 执行", ImportStep::Execute),
            ("④ 结果与匹配", ImportStep::Result),
        ];
        ui.horizontal(|ui| {
            for (label, s) in steps {
                let on = io.import_step == s;
                if ui.selectable_label(on, label).clicked() && io.state != IoState::Running {
                    io.import_step = s;
                }
            }
        });
        ui.separator();
        match io.import_step {
            ImportStep::Validate => import_step_validate(ui, theme, io),
            ImportStep::Strategy => import_step_strategy(ui, theme, io, actions),
            ImportStep::Execute => import_step_execute(ui, theme, io, actions),
            ImportStep::Result => import_step_result(ui, theme, io, actions),
        }
    }

    // 操作状态区：六态常驻（docs/11 §2.1.2）
    ui.separator();
    io_state_area(ui, theme, io);
    ui.label(
        RichText::new("ⓘ 相关操作不修改或移动原始照片 · 导入采用事务保护，失败可回滚")
            .color(theme.text_secondary)
            .size(10.5),
    );
}

fn import_step_validate(ui: &mut Ui, theme: &Theme, io: &IoUi) {
    let Some(p) = &io.preview else { return };
    ui.label(RichText::new("✓ 完整性校验通过 · 版本兼容").color(theme.ok));
    section(ui, theme, "索引信息");
    row(ui, theme, "生成应用", &p.manifest.app_version);
    row(ui, theme, "格式版本", &format!("v{}", p.manifest.format_version));
    row(ui, theme, "数据库版本", &format!("v{}", p.manifest.schema_version));
    let c = &p.manifest.counts;
    row(
        ui,
        theme,
        "数量",
        &format!(
            "照片 {} · 根 {} · 文件夹 {} · 标签 {} · 相册 {}",
            c.photos, c.roots, c.folders, c.tags, c.albums
        ),
    );
    ui.separator();
    section(ui, theme, "冲突与路径匹配");
    row(ui, theme, "重复记录", &format!("{} 条（按身份键处理）", p.duplicates));
    row(ui, theme, "路径无法匹配", &format!("{} 个根", p.unmatched_roots));
    ui.label(
        RichText::new("ℹ 未匹配的照片导入后可重新匹配根目录，将标记为离线，不复制或移动照片")
            .color(theme.text_secondary)
            .size(11.0),
    );
}

fn import_step_strategy(ui: &mut Ui, theme: &Theme, io: &mut IoUi, actions: &mut Vec<MgrAction>) {
    section(ui, theme, "导入策略（明确说明影响范围）");
    let merge = io.strategy == picasa_db::ImportStrategy::Merge;
    ui.horizontal(|ui| {
        if ui.selectable_label(merge, "◉ 合并（推荐，默认）").clicked() {
            io.strategy = picasa_db::ImportStrategy::Merge;
            actions.push(MgrAction::SetImportStrategy(io.strategy));
        }
        if ui.selectable_label(!merge, "○ 替换").clicked() {
            io.strategy = picasa_db::ImportStrategy::Replace;
            actions.push(MgrAction::SetImportStrategy(io.strategy));
        }
    });
    egui::Frame::new()
        .fill(if merge { ui.visuals().extreme_bg_color } else { theme.danger.gamma_multiply(0.08) })
        .corner_radius(6.0)
        .inner_margin(8.0)
        .show(ui, |ui| {
            if merge {
                ui.label("合并：只新增、不删除现有记录；身份相同且内容不同保留现有，计入冲突清单。");
                ui.label("标签 / 相册按名称合并去重，星标与说明保留现有；现有图库根目录不变。");
            } else {
                ui.label(RichText::new("⚠ 影响范围（替换）").color(theme.danger));
                if let Some(p) = &io.preview {
                    ui.label(format!(
                        "现有独有的整理数据（约 {} 项）将被移除；执行前先生成备份。",
                        p.existing_only_user_data
                    ));
                }
                ui.label("现有照片记录以导入文件为准覆盖。✓ 照片文件不受影响（不复制、不移动、不重命名）");
            }
        });
    ui.checkbox(&mut io.understood, "我已了解上述影响范围");
    ui.horizontal(|ui| {
        let enabled = io.understood && io.state != IoState::Running;
        if ui.add_enabled(enabled, egui::Button::new("开始导入")).clicked() {
            actions.push(MgrAction::StartImport);
            io.import_step = ImportStep::Execute;
        }
    });
}

fn import_step_execute(ui: &mut Ui, theme: &Theme, io: &mut IoUi, actions: &mut Vec<MgrAction>) {
    section(ui, theme, "执行导入");
    if io.state == IoState::Running || io.state == IoState::Preparing {
        ui.label(RichText::new(&io.progress).color(theme.text_primary));
        if let Some(f) = io.fraction {
            ui.add(egui::ProgressBar::new(f).show_percentage());
        } else {
            let bar = egui::ProgressBar::new(0.3).animate(true);
            ui.add(bar);
        }
        ui.label(
            RichText::new("当前阶段：写入整理关系（后台执行，界面可继续操作）· 无 ETA")
                .color(theme.text_secondary)
                .size(11.0),
        );
        if ui.button("取消导入").clicked() {
            actions.push(MgrAction::CancelImport);
        }
    } else if io.state == IoState::Cancelled {
        ui.label(RichText::new("⏹ 已取消 · 现有索引未改动").color(theme.warn));
    } else if io.state == IoState::Failed {
        ui.label(RichText::new(&io.message).color(theme.danger));
    } else {
        ui.label(RichText::new("准备就绪，点「开始导入」执行。").color(theme.text_secondary));
    }
    ui.label(
        RichText::new("ⓘ 关闭本窗口不会取消导入；重开可继续查看进度")
            .color(theme.text_secondary)
            .size(10.5),
    );
}

fn import_step_result(ui: &mut Ui, theme: &Theme, io: &mut IoUi, actions: &mut Vec<MgrAction>) {
    section(ui, theme, "导入结果");
    if let Some(r) = &io.report {
        row(ui, theme, "成功导入", &format!("{} 条", r.imported));
        row(ui, theme, "跳过", &format!("{} 条", r.skipped));
        row(ui, theme, "失败", &format!("{} 条", r.failed));
        row(ui, theme, "冲突", &format!("{} 条", r.conflicts));
    }
    ui.separator();
    section(ui, theme, "重新匹配图库根目录（可选后续动作）");
    let mut remap: Option<(String, String)> = None;
    for (path, matched) in &io.roots_to_remap {
        ui.horizontal(|ui| {
            if *matched {
                ui.label(RichText::new(format!("✓ {}", path)).color(theme.ok));
            } else {
                ui.label(RichText::new(format!("⚠ {} · 路径不存在（离线）", path)).color(theme.warn));
                if ui.button("选择本机目录…").clicked() {
                    // 路径注入由 app 层用 rfd 完成；这里先发一个带原路径的动作
                    remap = Some((path.clone(), String::new()));
                }
            }
        });
    }
    if let Some((from, to)) = remap {
        actions.push(MgrAction::RemapRoot { from, to });
    }
    ui.label(
        RichText::new("ℹ 重新匹配不会复制、移动或重命名照片；映射后做增量核对")
            .color(theme.text_secondary)
            .size(10.5),
    );
}

/// 操作状态区：六态共用一套展示（docs/11 §2.1.2）。
/// 纯函数拆出来是为了可单测（本仓库 UI 测试的惯例：测「决定画什么」，不测 egui 绘制）。
pub fn io_state_label(state: IoState) -> &'static str {
    match state {
        IoState::Idle => "空闲",
        IoState::Preparing => "准备中",
        IoState::Running => "进行中",
        IoState::Success => "✓ 成功",
        IoState::Failed => "⛔ 失败",
        IoState::Cancelled => "⏹ 已取消",
    }
}

fn io_state_area(ui: &mut Ui, theme: &Theme, io: &IoUi) {
    section(ui, theme, "操作状态");
    let (label, color, body): (&str, Color32, String) = match io.state {
        IoState::Idle => (io_state_label(io.state), theme.text_secondary, io.last_result.clone()),
        IoState::Preparing => (io_state_label(io.state), theme.progress, "正在准备… 校验目标 / 计算预计内容 / 读清单".into()),
        IoState::Running => (io_state_label(io.state), theme.progress, io.progress.clone()),
        IoState::Success => (io_state_label(io.state), theme.ok, io.message.clone()),
        IoState::Failed => (io_state_label(io.state), theme.danger, io.message.clone()),
        IoState::Cancelled => (io_state_label(io.state), theme.warn, "已取消 · 现有索引未改动".into()),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(color).strong());
    });
    if !body.is_empty() {
        ui.label(RichText::new(body).size(11.0).color(theme.text_primary));
    }
    if io.state == IoState::Failed || io.state == IoState::Cancelled {
        ui.label(
            RichText::new("现有索引保持可用；可重新选择文件或稍后重试")
                .size(10.5)
                .color(theme.text_secondary),
        );
    }
}

fn section(ui: &mut Ui, theme: &Theme, title: &str) {
    ui.label(RichText::new(title).strong().color(theme.text_primary));
}

fn row(ui: &mut Ui, theme: &Theme, k: &str, v: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(k).color(theme.text_secondary));
        ui.label(RichText::new(v).color(theme.text_primary));
    });
}

/// 状态栏入口的角标颜色（§2.6「红色角标数字」）。
pub fn badge_color(theme: &Theme) -> Color32 {
    theme.danger
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;
    use picasa_core::{MgmtSnapshot, RootView};

    fn root(id: i64, path: &str, closed: bool, offline: bool) -> RootView {
        RootView {
            id,
            path: path.to_string(),
            closed,
            offline,
            photo_count: 100,
            thumb_bytes: 1024,
            failed: 2,
            last_indexed_at: Some(1_000_000),
            watch: Default::default(),
        }
    }

    #[test]
    fn human_bytes_uses_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    #[test]
    fn relative_time_is_coarse_and_never_negative() {
        let now = 1_000_000;
        assert_eq!(relative_time(None, now), "从未");
        assert_eq!(relative_time(Some(now), now), "刚刚");
        assert_eq!(relative_time(Some(now - 59), now), "刚刚");
        assert_eq!(relative_time(Some(now - 60), now), "1 分钟前");
        assert_eq!(relative_time(Some(now - 7200), now), "2 小时前");
        assert_eq!(relative_time(Some(now - 3 * 86400), now), "3 天前");
        // 时钟回拨不能显示负数
        assert_eq!(relative_time(Some(now + 500), now), "刚刚");
    }

    #[test]
    fn root_summary_line_carries_status_and_counts() {
        let r = root(1, "/Pictures", false, false);
        let s = root_summary_line(&r);
        assert!(s.contains("正常") && s.contains("100 张"), "{s}");
        assert!(s.contains("失败 2"), "{s}");

        let off = root(2, "/Volumes/X", false, true);
        assert!(root_summary_line(&off).starts_with("离线"));
    }

    #[test]
    fn running_tasks_come_before_history() {
        let mut snap = MgmtSnapshot::default();
        let mk = |id: i64, running: bool| TaskView {
            task_id: id,
            running,
            scope: format!("s{id}"),
            kind: "刷新".into(),
            status: if running { "扫描中" } else { "已完成" }.into(),
            discovered: 1,
            completed: 1,
            failed: 0,
            stage: "—".into(),
            batch: None,
            started_at: None,
            elapsed_text: "00:01".into(),
            outcome: if running { None } else { Some("成功".into()) },
            root_id: None,
        };
        snap.tasks = vec![mk(1, false), mk(2, true), mk(3, false)];
        let ids: Vec<i64> = visible_tasks(&snap).iter().map(|t| t.task_id).collect();
        assert_eq!(ids, vec![2, 1, 3], "进行中的必须排在最前");
    }

    #[test]
    fn task_progress_text_has_no_eta() {
        let t = TaskView {
            task_id: 1,
            running: true,
            scope: "/a".into(),
            kind: "刷新".into(),
            status: "扫描中".into(),
            discovered: 120,
            completed: 100,
            failed: 1,
            stage: "生成缩略图".into(),
            batch: None,
            started_at: Some(0),
            elapsed_text: "01:23".into(),
            outcome: None,
            root_id: None,
        };
        let s = task_progress_text(&t);
        assert!(s.contains("120") && s.contains("01:23"), "{s}");
        assert!(!s.contains("预计") && !s.contains("剩余"), "ETA 是永久禁令");
    }

    #[test]
    fn retry_is_disabled_for_unsupported_only() {
        let mut g = FailureGroup {
            kind: Some(FailureKind::Unsupported.code()),
            count: 5,
            ignored: 0,
        };
        assert!(!retry_enabled(&g), "格式不支持不可重试");
        g.kind = Some(FailureKind::Unreachable.code());
        assert!(retry_enabled(&g), "失联可重试");
        g.count = 0;
        assert!(!retry_enabled(&g), "0 条不给按钮");
    }

    #[test]
    fn exclusion_bulk_state_reflects_stale_items() {
        let mk = |missing: bool| ExclusionView {
            id: 1,
            root_id: 1,
            path: "/a".into(),
            path_missing: missing,
        };
        assert_eq!(exclusion_bulk_state(&[]), (false, false));
        assert_eq!(exclusion_bulk_state(&[mk(false)]), (true, false));
        assert_eq!(exclusion_bulk_state(&[mk(false), mk(true)]), (true, true));
    }

    #[test]
    fn rebuild_confirmation_text_says_photos_are_safe_and_names_the_count() {
        let c = Confirm::RebuildRoot {
            id: 1,
            photos: 4321,
        };
        let b = c.body();
        assert!(b.contains("4321"), "数量必须出现在文案里：{b}");
        assert!(b.contains("照片文件不受影响"), "{b}");
        assert!(b.contains("缩略图"), "{b}");
        // 「不先清再建」的承诺也要写进去（§4.2 M22-IDX-06）
        assert!(b.contains("旧缩略图保持可见"), "{b}");
        assert_eq!(c.title(), "确认全量重建");
        assert!(Confirm::None.is_none());
    }

    #[test]
    fn open_at_lands_on_the_problem_tab() {
        let mut ui = IndexMgrUi::new();
        let snap = MgmtSnapshot {
            roots: vec![root(1, "/a", false, true)], // 离线
            ..Default::default()
        };
        ui.open_at(&snap, 0);
        assert!(ui.open);
        assert_eq!(ui.tab, MgmtTab::Folders, "有离线根 → 定位到关注文件夹");
        ui.close();
        assert!(!ui.open);
    }

    // ---- M4 新增逻辑的纯函数测试（docs/12 §0.1、docs/11 §2.1.2）----

    #[test]
    fn mgmt_tab_has_five_entries_with_import_export_last() {
        assert_eq!(MgmtTab::ALL.len(), 5);
        assert_eq!(MgmtTab::ALL[4], MgmtTab::ImportExport);
        assert_eq!(MgmtTab::ImportExport.label(), "导入 / 导出");
    }

    #[test]
    fn io_state_labels_cover_all_six_states() {
        use IoState::*;
        let cases = [
            (Idle, "空闲"),
            (Preparing, "准备中"),
            (Running, "进行中"),
            (Success, "✓ 成功"),
            (Failed, "⛔ 失败"),
            (Cancelled, "⏹ 已取消"),
        ];
        for (s, want) in cases {
            assert_eq!(io_state_label(s), want);
        }
    }

    #[test]
    fn io_ui_default_is_idle_and_merge_first() {
        let io = IoUi::default();
        assert_eq!(io.state, IoState::Idle);
        assert_eq!(io.import_step, ImportStep::Validate);
        assert_eq!(io.strategy, picasa_db::ImportStrategy::Merge);
        assert!(io.preview.is_none());
        assert!(io.report.is_none());
    }

    #[test]
    fn index_mgr_ui_default_keeps_io_state() {
        let ui = IndexMgrUi::new();
        assert!(!ui.open);
        assert_eq!(ui.tab, MgmtTab::Tasks);
        assert_eq!(ui.io.state, IoState::Idle);
    }
}
