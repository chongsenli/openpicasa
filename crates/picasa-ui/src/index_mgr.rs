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
    /// 用户逐任务暂停（M4.3 阶段 1，带库里的 task_id）
    PauseTask(i64),
    /// 用户逐任务继续（M4.3 阶段 1，带库里的 task_id）
    ResumeTask(i64),
    CancelTask(i64),
    RetryKind(FailureKind),
    IgnoreKind(FailureKind),
    UnignoreKind(FailureKind),
    // ---- 关注文件夹标签
    RefreshRoot(i64),
    ReindexRoot(i64),
    /// 「强制重扫」按钮（IDX-13）：跳过 L1 快筛，直接对该根做文件级 L2 校验，
    /// 变化目录由 `VerifyRunner::take_changed()` 转成 `enqueue_refresh`。非破坏性，无需确认框。
    ForceRescan(i64),
    /// 「全量重建」按钮：请求弹出作用域确认框（**不执行重建**，IDX-11）。
    RequestRebuild(i64),
    /// 全量重建确认框点「确认」后发出的真正重建动作（带稳定 root id）。
    RebuildRoot(i64),
    CloseRoot(i64),
    ReopenRoot(i64),
    RevealRoot(i64),
    /// 「删除索引」按钮：请求弹出二次确认框（**不执行删除**）。
    ///
    /// m10756 之后这个按钮就是**彻底删除**：确认后 `purge_index_for_root`
    /// 连 `roots` 行一起删，索引管理里不再留空壳（原「删除索引」的
    /// 留一行可恢复的语义已由用户明示取消，见 docs/18 v0.7-dev30）。
    DeleteIndexRoot(i64),
    /// 二次确认框点「确认」后发出的真正删除动作（带稳定 root id）。
    /// 与 `DeleteIndexRoot` 分开，避免「按钮 / 确认框都发同一个 action」的死循环。
    ConfirmDeleteIndexRoot(i64),
    // ---- 排除项标签
    RestoreExclusion(i64),
    PruneStaleExclusions,
    // ---- 存储与诊断标签
    ClearThumbs,
    Vacuum,
    ExportDiagLog,
    /// m10756：请求弹框清空**运行日志文件**（`~/Library/Logs/app.openpicasa/openpicasa.log`）。
    ClearRunLog,
    /// m10756：请求弹框清空**任务历史**（任务页的已结束记录与失败明细）。
    ClearTaskHistory,
    /// m10756：确认框点「确认」后真正清空运行日志。
    ConfirmClearRunLog,
    /// m10756：确认框点「确认」后真正清空任务历史。
    ConfirmClearTaskHistory,
    ToggleFrameStats(bool),
    // ---- M4「导入 / 导出」标签（docs/12 S2～S7）
    /// 打开系统保存对话框 → 导出（携带是否含缩略图）
    ExportIndex { with_thumbs: bool },
    /// IDX-06：在 Finder 中显示最近一次成功备份文件的位置。
    RevealBackup,
    /// 打开系统文件对话框 → 选择索引文件并校验/预览
    PickImportFile,
    /// 导入：切换合并 / 替换策略
    SetImportStrategy(picasa_db::ImportStrategy),
    /// 导入：开始执行（替换策略会先弹二次确认）
    StartImport,
    /// 导入：取消
    CancelImport,
    /// M4.3 §3A ⑦：一键重来上次中断的导出（目标与「含缩略图」从持久标记读回）。
    ReplayExport,
    /// M4.3 §3A ⑦：用户选择「忽略」中断的导出提示，清掉持久标记。
    DismissInterruptedExport,
    /// M4.3 §3A ⑦：用户确认「上次导入提交前中断已回滚」提示，清掉持久标记。
    DismissInterruptedImport,
    /// 导入后重新匹配某个来源根目录：(根路径, 用户选的本机目录)
    RemapRoot { from: String, to: String },
    /// IDX-19：浏览某次导入带进来的照片（临时 scope「本次导入 · N 张」）。
    BrowseImportedPhotos(i64),
    /// IDX-21：把「仅从分组隐藏」的来源恢复显示。
    UnhideImportSource(i64),
}

// ---------------------------------------------------------------- 二次确认

/// 需要二次确认的危险动作（§4.2 M22-IDX-06；M4.3 IDX-11 增强）。
///
/// ⚠️ 确认文案**必须**写清：作用范围、路径、照片数/标记数、保留项与「原图不受影响」。
/// 数量拿不到时显示「待计算」，**不能填 0**（§2 M43-IDX-11）；按钮默认落在「取消」上。
///
/// 含 `String` 所以不再是 `Copy`：确认框/`show()` 里改成 `clone()` 或借用。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Confirm {
    #[default]
    None,
    /// 全量重建：目标 root、路径、照片数、标记数（后两者 `None` = 待计算）。
    RebuildRoot {
        id: i64,
        path: String,
        photos: Option<u64>,
        markers: Option<i64>,
    },
    /// 删除索引：目标 root、路径、照片数、标记数（后两者 `None` = 待计算）。
    DeleteIndex {
        id: i64,
        path: String,
        photos: Option<u64>,
        markers: Option<i64>,
    },
    /// 清理全部缩略图（全库范围）
    ClearThumbs,
    /// m10756：清空**运行日志文件**（`~/Library/Logs/app.openpicasa/openpicasa.log`）。
    ///
    /// 与 [`Confirm::ClearTaskHistory`] 分开：一个动磁盘上的日志文件，一个动库里的
    /// 任务历史，混成一个按钮就会让人以为「清日志」连任务列表一起清了。
    ClearRunLog,
    /// m10756：清空**任务历史**（任务页的已结束记录 + 失败明细）。
    ClearTaskHistory,
}

/// 数量未知时显示「待计算」，绝不冒充 0（IDX-11）。
fn count_or_pending(v: Option<u64>) -> String {
    match v {
        Some(n) => n.to_string(),
        None => "待计算".to_string(),
    }
}

fn marker_or_pending(v: Option<i64>) -> String {
    match v {
        Some(n) if n >= 0 => n.to_string(),
        _ => "待计算".to_string(),
    }
}

impl Confirm {
    /// 确认框标题。
    pub fn title(&self) -> &'static str {
        match self {
            Self::None => "",
            Self::RebuildRoot { .. } => "确认全量重建",
            Self::DeleteIndex { .. } => "确认删除索引",
            Self::ClearThumbs => "确认清理缩略图",
            Self::ClearRunLog => "确认清空运行日志",
            Self::ClearTaskHistory => "确认清空任务历史",
        }
    }

    /// 确认框正文：范围 / 路径 / 照片数·标记数 / 保留项 / 原图不受影响（IDX-11）。
    pub fn body(&self) -> String {
        match self {
            Self::None => String::new(),
            Self::RebuildRoot {
                path,
                photos,
                markers,
                ..
            } => format!(
                "范围：此根及其全部子文件夹（根 / 子树）。\n\
                 路径：{path}\n\
                 照片数：{} 张 · 标记数：{} 张（带收藏/标签/说明/相册的照片）\n\n\
                 将重新生成全部缩略图；原图不受影响，照片文件不受影响，用户标记全部保留。\n\
                 重建期间旧缩略图保持可见，直到被新图替换。",
                count_or_pending(*photos),
                marker_or_pending(*markers),
            ),
            Self::DeleteIndex {
                path,
                photos,
                markers,
                ..
            } => format!(
                "范围：此根及其全部子文件夹（根 / 子树）。\n\
                 路径：{path}\n\
                 照片数：{} 张 · 标记数：{} 张（带收藏/标签/说明/相册的照片）\n\n\
                 将**彻底删除**该范围的索引：照片记录、缩略图，以及索引管理里的这一行\n\
                 （关注关系与本根的排除记录）都会消失；左栏与网格里这些文件夹和预览图片也随之不见。\n\
                 删除后不能撤销，无法再「重新索引（恢复）」，需要重新添加这个文件夹并扫描。\n\n\
                 不受影响：原图与磁盘文件一张都不会被删除或修改（只动本产品的索引数据库）。\n\
                 会保留：收藏 / 标签 / 说明 / 相册等用户标记按身份骨架保留，\n\
                 以后重新添加同一个目录并扫描，会自动挂回这些标记。",
                count_or_pending(*photos),
                marker_or_pending(*markers),
            ),
            Self::ClearThumbs => {
                "范围：全库。将清除全部缩略图，下次浏览时重新生成。\n\
                 原图不受影响，照片文件不受影响，用户标记全部保留。"
                    .to_string()
            }
            Self::ClearRunLog => {
                "范围：运行日志文件（~/Library/Logs/app.openpicasa/openpicasa.log）。\n\
                 将清空这个文件的全部内容，之后的新日志继续往同一个文件里写。\n\n\
                 不影响：索引库、任务历史、照片文件，以及「导出诊断日志」已经导出过的副本。"
                    .to_string()
            }
            Self::ClearTaskHistory => {
                "范围：任务页里**已结束**的索引任务记录，以及它们的失败明细。\n\n\
                 正在运行、排队或暂停的任务一条都不会被删除。\n\
                 清空后「失败分类」的角标会归零（失败明细随任务一起删）。\n\n\
                 不影响：照片、缩略图、用户标记，以及运行日志文件。"
                    .to_string()
            }
        }
    }

    pub fn is_none(&self) -> bool {
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

/// M4.3 IDX-06：最近一次**成功发布**的备份记录（跨重启显示）。
///
/// 只在导出索引文件真正成功后写入，并持久化到 `settings["m43_last_backup"]`；
/// 失败 / 取消绝不写（「不把失败记录当成功」）。`sha256` 是产物文件的
/// SHA-256（区别于清单 FNV 校验和），可由用户事后复核。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupInfo {
    pub path: String,
    /// UTC 秒（`ExportReport.created_at`）。
    pub created_at: i64,
    pub sha256: String,
    /// 导出时记录的本地时区标签（IDX-04「记录版本/数量/时区」）。
    pub timezone: String,
}

impl BackupInfo {
    /// 持久化到 settings 的单行 JSON。
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "path": self.path,
            "created_at": self.created_at,
            "sha256": self.sha256,
            "timezone": self.timezone,
        })
        .to_string()
    }

    /// 从 settings 读回。坏记录 / 缺字段一律 `None`，绝不把半截记录显示成成功备份。
    pub fn from_json(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        Some(Self {
            path: v.get("path")?.as_str()?.to_string(),
            created_at: v.get("created_at")?.as_i64()?,
            sha256: v.get("sha256")?.as_str()?.to_string(),
            timezone: v.get("timezone")?.as_str()?.to_string(),
        })
    }
}

/// IDX-06 的 settings 键：最近一次**成功**导出留下的备份记录。
pub const LAST_BACKUP_KEY: &str = "m43_last_backup";

/// 从库里的 settings 读回「最近成功备份」。
///
/// 抽成函数（而不是散在 `App::new` 里）是为了让「重启后仍显示记录」这条验收点
/// 有一个**端到端**用例：写 settings → 重新读 → 记录还在。
/// 读失败 / 没有记录 / 记录损坏一律 `None`——绝不拿半截数据充成功备份。
pub fn load_last_backup(db: &picasa_db::Db) -> Option<BackupInfo> {
    let json = db.setting_raw(LAST_BACKUP_KEY).ok().flatten()?;
    BackupInfo::from_json(&json)
}

/// 写入「最近成功备份」。**只允许在导出真正成功之后调用**：
/// 失败、取消、导入都不该走到这里（见 `import_task::run_export` 的 `Err` 分支）。
pub fn save_last_backup(
    db: &picasa_db::Db,
    info: &BackupInfo,
) -> std::result::Result<(), picasa_db::DbError> {
    db.set_setting_raw(LAST_BACKUP_KEY, Some(&info.to_json()))
}

/// IDX-06 显示用：UTC 时间 + 导出时记录的本地时区（均为人话、无 ETA / 假百分比）。
pub fn format_backup_time(secs: i64, timezone: &str) -> String {
    let utc = chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| secs.to_string());
    format!("{utc} UTC（记录时区 {timezone}）")
}

/// M4.3 §3A ⑦：上次会话**中断的导出**（进程在导出中途被杀 / 崩溃）。
///
/// 导出开始前写 `settings["m43_export_pending"]`，终态（成功 / 失败 / 取消）一律清掉。
/// 重启时若这一行还在，说明上次导出**没跑完**：清掉目标目录里的 `.tmp` 临时产物，
/// 并在「导入 / 导出」页提示「上次导出未完成」+ 一键重来（目标与是否含缩略图原样保留）。
/// 绝不自动重跑——用户可能已经改主意了。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InterruptedExport {
    /// 用户上次选的导出目标（绝对路径）。
    pub target: String,
    pub with_thumbs: bool,
}

impl InterruptedExport {
    pub fn to_json(&self) -> String {
        serde_json::json!({ "target": self.target, "with_thumbs": self.with_thumbs }).to_string()
    }

    /// 坏记录 / 缺字段一律 `None`——宁可少提示，也不假装「上次导出未完成」。
    pub fn from_json(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        Some(Self {
            target: v.get("target")?.as_str()?.to_string(),
            with_thumbs: v.get("with_thumbs")?.as_bool()?,
        })
    }
}

/// M4.3 §3A ⑦：**已提交**的导入结果摘要（重启后仍显示完成结果）。
///
/// 只导入事务提交成功才写；提交前中断的导入不写这一行（见 `m43_import_pending`），
/// 因此「有摘要」== 「这批关系真的进了主库」，不会把回滚的当成功。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    /// UTC 秒。
    pub at: i64,
    pub imported: i64,
    pub skipped: i64,
    pub failed: i64,
    pub conflicts: i64,
}

impl ImportSummary {
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "at": self.at,
            "imported": self.imported,
            "skipped": self.skipped,
            "failed": self.failed,
            "conflicts": self.conflicts,
        })
        .to_string()
    }

    pub fn from_json(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        Some(Self {
            at: v.get("at")?.as_i64()?,
            imported: v.get("imported")?.as_i64()?,
            skipped: v.get("skipped")?.as_i64()?,
            failed: v.get("failed")?.as_i64()?,
            conflicts: v.get("conflicts")?.as_i64()?,
        })
    }

    /// 摘要文案（纯函数，无 ETA）。
    pub fn text(&self) -> String {
        format!(
            "成功 {} · 跳过 {} · 失败 {} · 冲突 {}",
            self.imported, self.skipped, self.failed, self.conflicts
        )
    }
}

/// M4.3 IDX-18：导入 / 导出任务的进度快照（docs/18 §3.1:102 要求的
/// `stage / done / total / current / errors / revision` 全量口径）。
///
/// 纯数据，供 `IoUi`、后台 `Msg::Progress` 与状态栏全局指示器共用。
/// `total = None`（或 0）表示总数未知 → 不确定态（循环条），**绝不用假百分比**。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgressSnapshot {
    /// 阶段名（校验 / 预览 / 备份 / 复制 / 合并 / 自检 / 发布）。
    pub stage: String,
    /// 已完成项数。
    pub done: u64,
    /// 总数；`None` 或 0 = 未知 → 不确定态。
    pub total: Option<u64>,
    /// 当前项（长路径在渲染时截断），可为空。
    pub current: String,
    /// 已收集的错误条目（有界展示）。
    pub errors: Vec<String>,
    /// 单调递增的修订号；乱序到达的旧快照不得回退进度。
    pub revision: u64,
}

impl ProgressSnapshot {
    /// 确定态分数；总数未知 / 为 0 时返回 `None`（不确定态，禁假百分比）。
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total?;
        if total == 0 {
            return None;
        }
        Some((self.done as f32 / total as f32).clamp(0.0, 1.0))
    }

    /// 百分比 0..100；总数未知时 `None`。
    pub fn percent(&self) -> Option<u32> {
        self.fraction().map(|f| (f * 100.0) as u32)
    }
}

/// 进度摘要文案（纯函数）：阶段 + 已处理 / 总数 + 当前项（截断 40 字）。
/// **不含 ETA / 预计 / 剩余**（永久禁令，docs/11 §2.1.2）。
pub fn progress_label(s: &ProgressSnapshot) -> String {
    let mut out = s.stage.clone();
    match s.total {
        Some(total) if total > 0 => {
            out.push_str(&format!(" · 已处理 {} / {}", s.done, total));
        }
        _ => out.push_str(&format!(" · 已处理 {}", s.done)),
    }
    if !s.current.is_empty() {
        let cur: String = s.current.chars().take(40).collect();
        out.push_str(" · ");
        out.push_str(&cur);
    }
    out
}

/// 状态栏紧凑指示器用的短文案（阶段 + 已处理 N / M），不含当前项 / ETA。
pub fn progress_short_label(s: &ProgressSnapshot) -> String {
    match s.total {
        Some(total) if total > 0 => format!("{} · {} / {}", s.stage, s.done, total),
        _ => format!("{} · 已处理 {}", s.stage, s.done),
    }
}

/// 失败明细摘要（纯函数）：有界展示前 50 条，超出部分只报数量。
pub fn failure_summary(errors: &[String]) -> Option<String> {
    if errors.is_empty() {
        return None;
    }
    const LIMIT: usize = 50;
    let mut out = String::new();
    for (i, e) in errors.iter().take(LIMIT).enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, e));
    }
    if errors.len() > LIMIT {
        out.push_str(&format!("… 另有 {} 项", errors.len() - LIMIT));
    }
    Some(out.trim_end().to_string())
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
    /// M4.2 `M42-UI-07`：导入前预览用的包内缩略图 `(photo_id, 字节)`。
    /// 只存原始字节（不存 `TextureHandle` —— 它没有 `Debug`，会破坏本结构派生），
    /// 渲染时由 `import_step_validate` 用 `ctx.load_texture_cached` 按需建纹理。
    /// 来自 `ImportPreview.thumbs`（只读包内 thumbs 表、绝不读原图）。
    pub preview_thumbs: Vec<(i64, Vec<u8>)>,
    /// M4.2 `M42-PERF-01`：与后台 `ImportTask` **同一个**取消标志（单一真相源）。
    /// `Option` 是因为任务未启动/已结束时为 `None`；`Arc<AtomicBool>` 满足
    /// Clone/Debug/Default(=None)，不会破坏 `#[derive(Clone, Debug, Default)]`。
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// M4.3 IDX-06：最近一次成功发布的备份（跨重启由 settings 读回）。
    /// 只在导出成功后写；失败 / 取消绝不写。
    pub last_backup: Option<BackupInfo>,
    /// M4.3 IDX-18：最近一次进度快照（后台 `Msg::Progress` 写入）。
    /// Cancelled / Done 时清空；状态栏全局指示器与执行页共用同一份数据。
    pub snapshot: Option<ProgressSnapshot>,
    /// M4.3 IDX-18：单调不回退的百分比上包络（0..100）。
    /// 只在新任务启动（m4_export_index / m4_start_import）重置为 0；
    /// 镜像 `IndexBoard::bump_percent` 的「上包络」语义。
    pub percent: u32,
    /// M4.3 §3A ⑦：上次会话中断的导出（重启后提示「上次导出未完成」+ 一键重来）。
    /// 启动时从 `settings["m43_export_pending"]` 读回，读到即说明上次没跑完。
    pub interrupted_export: Option<InterruptedExport>,
    /// M4.3 §3A ⑦：上次会话**提交前中断**的导入标记。
    /// 启动时从 `settings["m43_import_pending"]` 读回，读到即提示「现有索引未改动」。
    pub interrupted_import: bool,
    /// M4.3 §3A ⑦：最近一次**已提交**的导入结果（重启后仍显示完成结果）。
    pub last_import: Option<ImportSummary>,
    /// IDX-21：被「仅从分组隐藏」的来源 (id, 名称)。
    ///
    /// 只是**分组显示**的开关，索引与标记一动不动；给用户留一条恢复入口，
    /// 否则一旦点了隐藏就再也找不到这个来源（既不能看也不能移除）。
    pub hidden_sources: Vec<(i64, String)>,
}

/// 重启语义横幅上的两个按钮：直接复用 `MgrAction`，由 app 侧执行。
impl IoUi {
    /// M4.3 §3A ⑦：渲染「上次导出未完成」横幅。返回用户点的按钮（若有）。
    ///
    /// 只画提示与按钮，**自己不执行**任何 DB / 文件操作——由 app 侧消费动作。
    fn interrupted_export_banner(&self, ui: &mut Ui, theme: &Theme) -> Option<MgrAction> {
        let i = self.interrupted_export.as_ref()?;
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("上次导出未完成（进程中断）· 临时产物已清理，可一键重来")
                    .color(theme.warn)
                    .size(11.0),
            );
            if ui.button("重来").clicked() {
                action = Some(MgrAction::ReplayExport);
            }
            if ui.button("忽略").clicked() {
                action = Some(MgrAction::DismissInterruptedExport);
            }
        });
        ui.label(
            RichText::new(format!(
                "目标：{}（含缩略图：{}）",
                i.target,
                if i.with_thumbs { "是" } else { "否" }
            ))
            .color(theme.text_secondary)
            .size(10.5),
        );
        action
    }
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

/// 结束摘要（M4.3 IDX-17）：完成 / 跳过 / 失败，以及取消原因。
///
/// 只给终态任务用（运行中仍显示 `task_progress_text`，不带 ETA）。
/// 失败数来自按「失败条目」去重的计数（不是重试次数）；取消原因来自持久
/// `cancel_reason`，跨重启可追溯。因此「部分成功」与「已取消」在历史里
/// 不会被混为一谈（TC-M43-11 / TC-M43-13）。
pub fn task_end_summary(t: &TaskView) -> String {
    let mut s = format!(
        "完成 {} · 跳过 {} · 失败 {}",
        t.completed, t.skipped, t.failed
    );
    if let Some(reason) = &t.cancel_reason
        && !reason.is_empty()
    {
        s.push_str(&format!(" · 已取消（{reason}）"));
    }
    s
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

/// 「删除索引」按钮的唯一构造点。
///
/// m10756 起这是**彻底删除**：确认后连 `roots` 行一起删，索引管理里不再留空壳，
/// 所以不需要第二个「彻底删除」按钮——两个按钮一个留空壳一个删干净，
/// 用户反而要在两个都叫「删除」的东西之间做选择。
///
/// 抽出来是因为有**两个**绘制位置：正常行底部的按钮区，和「索引已移除」
/// 那一行的早退分支（它在到达底部按钮区之前就 `return` 了）。
/// 文案与 hover 提示只有一份，才不会出现两个位置承诺不一致——这个操作
/// 不可逆，承诺必须处处相同。
fn delete_index_button(ui: &mut Ui, id: i64, actions: &mut Vec<MgrAction>) {
    if ui
        .button("删除索引")
        .on_hover_text(
            "彻底删除该根的索引与缩略图，并从索引管理里移除这一行（原图不动、不能撤销）",
        )
        .clicked()
    {
        actions.push(MgrAction::DeleteIndexRoot(id));
    }
}

/// 「删除索引」是否对该根可用。
///
/// m10609 的要求是「**不管什么状态**的索引都要能删」：活跃 / 已关闭 /
/// 离线 / 索引已移除，四种状态都必须给入口。早先只在底部按钮区画，
/// 而「索引已移除」的行在那里之前就 `return` 了——恰恰是最需要删干净的
/// 那种状态反而没有入口。
///
/// 恒真不是偷懒：彻底删除只动本产品自有数据库（`purge_index_for_root` 的
/// 两段式 `clear_index_for_root` → `delete_root_row`），不读原图、不碰磁盘，
/// 所以与盘在不在线无关。离线根的索引同样占空间、同样让左栏多一行。
pub fn delete_index_available(_r: &RootView) -> bool {
    true
}

/// 「清空任务历史…」是否该给（m10756）。
///
/// 判据是「有没有已结束的记录」，而不是「任务列表空不空」：列表空但库里留着
/// 一堆历史任务时，按钮仍然要有；列表里全是正在跑的任务时，按钮要灰掉
/// ——点下去什么都不会发生，比不让点更让人困惑。
///
/// 「已结束」用 `outcome.is_some()` 判：只有终态才写 outcome，
/// 运行/排队/暂停三种活任务的 outcome 都是 `None`。
pub fn history_clear_available(snap: &MgmtSnapshot) -> bool {
    snap.tasks.iter().any(|t| t.outcome.is_some())
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
                    MgmtTab::Tasks => {
                        tasks_tab(ui, theme, snap, &mut actions, &mut state.confirm)
                    }
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

fn tasks_tab(
    ui: &mut Ui,
    theme: &Theme,
    snap: &MgmtSnapshot,
    actions: &mut Vec<MgrAction>,
    confirm: &mut Confirm,
) {
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
    // m10756：「清空任务历史」按钮。放在列表上方而不是最底部——任务一多，
    // 用户得先滚到列表尽头才能看见它，等于没有。
    ui.horizontal(|ui| {
        let has_history = history_clear_available(snap);
        if ui
            .add_enabled(has_history, egui::Button::new("清空任务历史…"))
            .on_hover_text("删除任务页里已结束的记录与失败明细；正在跑的任务不受影响")
            .clicked()
        {
            *confirm = Confirm::ClearTaskHistory;
        }
        if !has_history {
            ui.label(
                RichText::new("没有已结束的任务记录")
                    .size(11.0)
                    .color(theme.text_secondary),
            );
        }
    });
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
                // 只有对齐到库里持久化意图（task_id != 0）的任务才有逐任务
                // 暂停 / 继续 / 取消；会话内任务（id = 0）不给会误导的空按钮。
                if t.task_id != 0 {
                    if t.user_paused {
                        ui.label(RichText::new("已暂停").size(11.0).color(theme.warn));
                        if ui.button("继续").clicked() {
                            actions.push(MgrAction::ResumeTask(t.task_id));
                        }
                    } else if ui.button("暂停").clicked() {
                        actions.push(MgrAction::PauseTask(t.task_id));
                    }
                    if ui.button("取消").clicked() {
                        actions.push(MgrAction::CancelTask(t.task_id));
                    }
                }
            } else {
                if let Some(o) = &t.outcome {
                    ui.label(RichText::new(o).size(11.0).color(theme.text_secondary));
                }
                // IDX-17：终态行一律给结束摘要（完成/跳过/失败/取消+原因）。
                ui.label(
                    RichText::new(task_end_summary(t))
                        .size(11.0)
                        .color(theme.text_secondary),
                );
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
                } else if r.index_removed {
                    // 不是「坏」也不是「正常」：索引真的没了，但随时可恢复。
                    theme.warn
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

            // 索引已移除的行：监视已停（`close_root` 把它置成 closed，
            // `load_configs` 只取 active 根），再画一行「未监视」只会误导。
            if r.index_removed {
                ui.label(
                    RichText::new(
                        "索引已移除：只有收藏 / 标签 / 说明 / 相册这些用户标记还在，\
                         重新索引会按身份自动挂回；原图与磁盘文件从未被动过。",
                    )
                    .size(11.0)
                    .color(theme.warn),
                );
                ui.horizontal(|ui| {
                    if ui.button("重新索引（恢复）").clicked() {
                        actions.push(MgrAction::ReindexRoot(r.id));
                    }
                    if ui.button("在 Finder 中显示").clicked() {
                        actions.push(MgrAction::RevealRoot(r.id));
                    }
                    // 这一行同样要能删——而且这里最需要它：「索引已移除」意味着
                    // 派生数据已经清空、只剩一条空壳记录，用户想让它从索引管理里
                    // 消失，只有这个入口。
                    if delete_index_available(r) {
                        delete_index_button(ui, r.id, actions);
                    }
                });
                return;
            }

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
                    if ui.button("强制重扫").clicked() {
                        // IDX-13：跳过 L1，强制文件级核对（离线盘期间目录 mtime 可能没变）。
                        actions.push(MgrAction::ForceRescan(r.id));
                    }
                    if ui.button("全量重建").clicked() {
                        // IDX-11：先弹作用域确认框，不直接执行重建。
                        actions.push(MgrAction::RequestRebuild(r.id));
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
                if delete_index_available(r) {
                    delete_index_button(ui, r.id, actions);
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
        if ui
            .button("清空运行日志")
            .on_hover_text("清空 ~/Library/Logs/app.openpicasa/openpicasa.log（不影响索引库与任务历史）")
            .clicked()
        {
            *confirm = Confirm::ClearRunLog;
        }
        if ui.button("导出诊断日志").clicked() {
            actions.push(MgrAction::ExportDiagLog);
        }
    });
    // PERF-18：帧时间直方图开关
    ui.checkbox(frame_stats, "显示帧时间直方图（p50/p95/p99/max）");
}

fn confirm_dialog(ctx: &Context, state: &mut IndexMgrUi, actions: &mut Vec<MgrAction>) {
    // `Confirm` 含 String，不再是 Copy：先克隆一份供本帧渲染（IDX-11）。
    let confirm = state.confirm.clone();
    let mut open = true;
    egui::Window::new(confirm.title())
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(confirm.body());
            // 默认取消（IDX-11）：「取消」在前且默认聚焦，回车/空格落在取消上；
            // Escape 与关闭窗口同样等价于取消。危险动作必须二次主动点击「确认」。
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                state.confirm = Confirm::None;
                return;
            }
            ui.horizontal(|ui| {
                let cancel_btn = ui.button("取消");
                cancel_btn.request_focus();
                if cancel_btn.clicked() {
                    state.confirm = Confirm::None;
                }
                if ui.button("确认").clicked() {
                    match confirm {
                        Confirm::RebuildRoot { id, .. } => actions.push(MgrAction::RebuildRoot(id)),
                        Confirm::DeleteIndex { id, .. } => {
                            actions.push(MgrAction::ConfirmDeleteIndexRoot(id))
                        }

                        Confirm::ClearThumbs => actions.push(MgrAction::ClearThumbs),
                        Confirm::ClearRunLog => actions.push(MgrAction::ConfirmClearRunLog),
                        Confirm::ClearTaskHistory => {
                            actions.push(MgrAction::ConfirmClearTaskHistory)
                        }
                        Confirm::None => {}
                    }
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
    // IDX-21 ⑤：「仅从分组隐藏」的来源要留恢复入口——隐藏只影响左栏分组，
    // 索引与标记都还在，这里给它一条回家的路。没有任何隐藏来源时整段不画。
    if !io.hidden_sources.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("已从分组隐藏：")
                    .color(theme.text_secondary)
                    .size(11.0),
            );
            for (id, name) in io.hidden_sources.clone() {
                if ui
                    .add(
                        egui::Button::new(RichText::new(format!("恢复「{name}」")).size(11.0))
                            .frame(false),
                    )
                    .on_hover_text("只影响左栏分组显示；索引与用户标记一直都在")
                    .clicked()
                {
                    actions.push(MgrAction::UnhideImportSource(id));
                }
            }
        });
        ui.separator();
    }
    // M4.3 §3A ⑦ 重启语义：上次导出未完成 / 上次导入提交前中断 / 最近已提交的导入结果。
    // 三条都只在**有持久记录**时出现，正常会话里这一屏与以前完全一样。
    if let Some(a) = io.interrupted_export_banner(ui, theme) {
        actions.push(a);
        ui.separator();
    }
    if io.interrupted_import {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("上次导入在提交前中断 · 已回滚，现有索引未改动，可重新开始")
                    .color(theme.warn)
                    .size(11.0),
            );
            if ui.button("知道了").clicked() {
                actions.push(MgrAction::DismissInterruptedImport);
            }
        });
        ui.separator();
    }
    // 上次**已提交**的导入结果：重启后仍显示完成结果（§3A ⑦「已提交则显示完成结果」）。
    // 本次会话刚跑完时不重复显示（那由结果页负责）。
    if io.report.is_none()
        && let Some(prev) = &io.last_import
    {
        ui.label(
            RichText::new(format!(
                "上次已提交的导入（{}）· {}",
                format_backup_time(prev.at, "本地"),
                prev.text()
            ))
            .color(theme.text_secondary)
            .size(10.5),
        );
        ui.separator();
    }
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

    // M4.3 IDX-06：最近成功备份（跨重启显示、可打开位置、可复核 SHA-256）。
    // 只在导出真正成功后写，失败 / 取消绝不显示成成功。
    if let Some(b) = &io.last_backup {
        ui.separator();
        section(ui, theme, "最近成功备份");
        ui.label(
            RichText::new(format_backup_time(b.created_at, &b.timezone))
                .color(theme.text_primary)
                .size(11.5),
        );
        ui.label(
            RichText::new(format!("SHA-256：{}…", &b.sha256[..b.sha256.len().min(12)]))
                .color(theme.text_secondary)
                .size(10.5),
        );
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&b.path)
                    .color(theme.text_secondary)
                    .size(10.5),
            );
            if ui.button("在 Finder 中显示").clicked() {
                actions.push(MgrAction::RevealBackup);
            }
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

    // M4.3 PERF：校验（选完文件到出预览）在后台线程跑。9 GiB 的包要读两遍、
    // 做一次 integrity_check、再整份复制一份副本——不把进度和「取消」摆在
    // 这里，用户只能看着一个不动的按钮，跟「无响应」没有区别。
    if io.state == IoState::Preparing && io.preview.is_none() {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(
                RichText::new("正在校验索引文件…（后台执行，界面可继续操作）")
                    .color(theme.text_primary),
            );
        });
        ui.add(egui::ProgressBar::new(0.3).animate(true));
        ui.label(
            RichText::new("大文件需要读两遍做完整性复核，期间不会修改任何数据 · 无 ETA")
                .color(theme.text_secondary)
                .size(10.5),
        );
        if ui.button("取消校验").clicked() {
            if let Some(flag) = &io.cancel {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            actions.push(MgrAction::CancelImport);
        }
        ui.separator();
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
    // M4.2 `M42-UI-07`：选前缩略图预览格。只读索引包内 thumbs 的字节、绝不读原图；
    // 无缩略图包（with_thumbs=false）优雅降级为「░ 无预览」占位。
    preview_thumbs_row(ui, theme, io);
    ui.separator();
    section(ui, theme, "冲突与路径匹配");
    row(ui, theme, "重复记录", &format!("{} 条（按身份键处理）", p.duplicates));
    row(ui, theme, "路径无法匹配", &format!("{} 个根", p.unmatched_roots));
    // M4.3 IDX-20：文件夹树的来源与规模。推导出来的树必须明确告知用户
    // （P-M43-08：v1 包 / 缺 folders 时按照片路径推导，不能让用户以为结构与源机一致）。
    let tree = &p.tree;
    row(
        ui,
        theme,
        "文件夹树",
        &format!("{} 个根 · {} 个文件夹", tree.roots.len(), tree.folders.len()),
    );
    row(ui, theme, "树来源", p.tree_source.label());
    if p.tree_source.needs_notice() {
        ui.label(
            RichText::new(format!(
                "⚠ {}：包内没有可校验的文件夹树，层级与计数按包内容重新推导，可能与源机不完全一致",
                p.tree_source.label()
            ))
            .color(theme.warn)
            .size(11.0),
        );
    }
    ui.label(
        RichText::new("ℹ 未匹配的照片导入后可重新匹配根目录，将标记为离线，不复制或移动照片")
            .color(theme.text_secondary)
            .size(11.0),
    );
}

/// M4.2 `M42-UI-07`：选前缩略图预览格。
///
/// - 数据来源只有 `IoUi.preview_thumbs`（=包内 `thumbs.data` 字节），**绝不读取原始照片**；
/// - 用 `image::load_from_memory` 解码 → `ColorImage` → `ctx.load_texture_cached`
///   按需建纹理（按 `Id` 缓存，≤12 张、仅 Validate 步、仅在向导打开时画）；
///   解码失败或无缩略图包显示「░ 无预览」字符块占位；
/// - `TextureHandle` 没有 `Debug`，所以**不进 `IoUi`**，只在渲染时临时创建。
const PREVIEW_THUMB_SIZE: f32 = 56.0;
const PREVIEW_THUMB_MAX: usize = 12;

/// 解码预览字节；失败返回 `None`（不阻断导入流程）。
fn decode_preview_thumb(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [img.width() as usize, img.height() as usize],
        img.as_raw(),
    ))
}

fn preview_thumbs_row(ui: &mut Ui, theme: &Theme, io: &IoUi) {
    section(ui, theme, "内容预览（选前）");
    ui.horizontal_wrapped(|ui| {
        // 单一真相源：优先用 app 侧填进 IoUi 的 preview_thumbs；没填时回退到
        // preview.thumbs（同一份数据），避免两处不一致。
        let thumbs: &[(i64, Vec<u8>)] = if io.preview_thumbs.is_empty() {
            io.preview.as_ref().map_or(&[], |p| p.thumbs.as_slice())
        } else {
            io.preview_thumbs.as_slice()
        };
        if thumbs.is_empty() {
            ui.label(
                RichText::new("░ 无预览").color(theme.text_secondary).size(11.0),
            );
        } else {
            for (photo_id, bytes) in thumbs.iter().take(PREVIEW_THUMB_MAX) {
                let cell = egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .corner_radius(6.0)
                    .inner_margin(3.0)
                    .show(ui, |ui| {
                        if let Some(img) = decode_preview_thumb(bytes) {
                            // Id 由 photo_id + 字节长度哈希：同一张照片稳定命中缓存。
                            let id = egui::Id::unique(("m42_preview_thumb", *photo_id, bytes.len()));
                            let tex = ui.ctx().load_texture_cached(
                                format!("m42-preview-{photo_id}"),
                                id,
                                img,
                                egui::TextureOptions::LINEAR,
                            );
                            ui.add(
                                egui::Image::from_texture(&tex)
                                    .fit_to_exact_size(egui::vec2(PREVIEW_THUMB_SIZE, PREVIEW_THUMB_SIZE))
                                    .corner_radius(4.0),
                            );
                        } else {
                            // 占位：░ 字符块，不依赖外部文件、不读原图。
                            ui.add_sized(
                                [PREVIEW_THUMB_SIZE, PREVIEW_THUMB_SIZE],
                                egui::Label::new(
                                    RichText::new("░\n无预览").color(theme.text_secondary).size(10.0),
                                ),
                            );
                        }
                    });
                let _ = cell.response;
            }
        }
    });
    ui.label(
        RichText::new("仅来自索引包内缩略图，不读取原图 · 最多 12 张")
            .color(theme.text_secondary)
            .size(10.5),
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
        // M42-PERF-01：阶段文案 / 进度全部来自后台 `ImportTask` 发来的
        // `Msg::Progress`（经 `apply_io_msg` 写进 io.progress / io.fraction），
        // 不再有固定假阶段文案（R-M42-5 单一真相源）。
        ui.label(RichText::new(&io.progress).color(theme.text_primary));
        // IDX-18 ③④：计数 / 百分比。确定态显示单调百分比（io.percent 上包络，
        // 不会因乱序旧快照回退）；总数未知显示不确定条 +「已处理 N」，绝不编造百分比。
        if let Some(snap) = &io.snapshot {
            let f = snap.fraction().unwrap_or(0.3);
            let bar = if snap.fraction().is_some() {
                egui::ProgressBar::new(f).show_percentage()
            } else {
                egui::ProgressBar::new(f).animate(true)
            };
            ui.add(bar);
            if snap.fraction().is_some() {
                ui.label(
                    RichText::new(format!("已完成 {}%（单调不回退）", io.percent))
                        .color(theme.text_secondary)
                        .size(10.5),
                );
            }
            if !snap.current.is_empty() {
                ui.label(
                    RichText::new(format!("当前：{}", snap.current))
                        .color(theme.text_secondary)
                        .size(10.5),
                );
            }
            if !snap.errors.is_empty() {
                ui.label(
                    RichText::new(format!("已收集 {} 项错误", snap.errors.len()))
                        .color(theme.warn)
                        .size(10.5),
                );
            }
        } else {
            ui.add(egui::ProgressBar::new(0.3).animate(true));
        }
        ui.label(
            RichText::new("导入在后台线程执行，界面可继续操作 · 无 ETA")
                .color(theme.text_secondary)
                .size(11.0),
        );
        if ui.button("取消导入").clicked() {
            // 真取消：直接置位共享 `Arc<AtomicBool>`（与后台 ImportTask 同一个标志，
            // 单一真相源 R-M42-5）。后台线程在 execute_import 阶段间检查到即
            // 单事务回滚退出，随后 `Msg::Cancelled` 把六态收敛到 Cancelled。
            if let Some(flag) = &io.cancel {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            // 同时通知 app 侧（lead 的 m4_cancel_import 也会调 ImportTask::cancel，
            // 同一个 Arc，幂等；即便 IoUi.cancel 为空也仍可取消）。
            actions.push(MgrAction::CancelImport);
        }
    } else if io.state == IoState::Cancelled {
        // 展示后台返回的完整回滚说明（整库回滚、无残留 staging 文件/连接）。
        let text = if io.message.is_empty() {
            "⏹ 已取消 · 整库回滚、无残留 staging 文件/连接 · 现有索引未改动".to_string()
        } else {
            io.message.clone()
        };
        ui.label(RichText::new(text).color(theme.warn));
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
        // IDX-19：导入成功后直接跳到本次导入的照片集合。
        // 只有拿到批次 id 且确有照片带进来时才给入口——空批次不给，
        // 否则点进去是一片空白，比没有按钮更让人困惑。
        if let Some(batch) = r.import_batch_id
            && r.imported > 0
        {
            ui.add_space(4.0);
            if ui.button("浏览导入的照片").clicked() {
                actions.push(MgrAction::BrowseImportedPhotos(batch));
            }
            ui.label(
                RichText::new("只读索引与缩略图；原图按原路径读取，不写照片目录")
                    .color(theme.text_secondary)
                    .size(10.5),
            );
        }
        // M4.3 IDX-18 ⑤「部分失败详情」：终态保留错误明细，有界展示（最多 50 条），
        // 供用户定位并「查看详情」；此前 failures 只在 ImportReport 里从未渲染。
        if !r.failures.is_empty() {
            section(ui, theme, "失败明细（查看详情）");
            if let Some(text) = failure_summary(&r.failures) {
                ui.label(RichText::new(text).color(theme.danger).size(10.5));
            }
        }
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
        IoState::Preparing => (
            io_state_label(io.state),
            theme.progress,
            if io.progress.is_empty() {
                "正在准备… 校验目标 / 计算预计内容 / 读清单".to_string()
            } else {
                io.progress.clone()
            },
        ),
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
    // IDX-18 ⑤：部分成功（r.failed > 0）时，即便 state 是 Success 也要露出错误数 + 详情提示。
    if io.state == IoState::Success
        && let Some(r) = &io.report
        && r.failed > 0
    {
        ui.label(
            RichText::new(format!("部分失败：{} 项错误，请在「导入结果」查看详情", r.failed))
                .color(theme.warn)
                .size(10.5),
        );
    }
    // IDX-18 ③：运行态把单调百分比也带进状态区（与状态栏全局指示器一致）。
    if io.state == IoState::Running
        && let Some(snap) = &io.snapshot
        && let Some(p) = snap.percent()
    {
        ui.label(
            RichText::new(format!("已完成 {}% · 已处理 {} 项", io.percent.max(p), snap.done))
                .color(theme.text_secondary)
                .size(10.5),
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
            index_removed: false,
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

    /// IDX-06：备份记录 JSON 往返逐字段一致；坏记录绝不显示成成功备份。
    #[test]
    fn backup_info_json_roundtrip_and_bad_record_is_ignored() {
        let b = BackupInfo {
            path: "/Volumes/USB/backup.opix".into(),
            created_at: 1_700_000_123,
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
            timezone: "Asia/Shanghai".into(),
        };
        assert_eq!(BackupInfo::from_json(&b.to_json()), Some(b));
        assert_eq!(BackupInfo::from_json("not-json"), None);
        assert_eq!(BackupInfo::from_json("{\"path\":\"x\"}"), None);
        assert_eq!(BackupInfo::from_json("{\"path\":1}"), None);
    }

    /// IDX-06「重启后仍显示记录」的端到端证据：写 settings → 换一个连接读回 → 记录还在。
    ///
    /// 这条不能只靠 `BackupInfo::from_json` 的单测：那只证明「字段能解析」，
    /// 不证明「真的落到库里、重启时真的取得回来」。
    #[test]
    fn last_backup_survives_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("library.sqlite");
        let backup = BackupInfo {
            path: "/Users/me/Desktop/index.opicasaidx".into(),
            created_at: 1_793_536_496,
            sha256: "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
                .into(),
            timezone: "Asia/Shanghai".into(),
        };
        {
            let db = picasa_db::Db::open(&db_path).expect("open");
            save_last_backup(&db, &backup).expect("write");
            // 同一个连接立刻能读回（写进去的是真记录，不是内存态）。
            assert_eq!(load_last_backup(&db), Some(backup.clone()));
            // 落库的是 UTF-8 JSON 单行，不是什么二进制/临时文件——「不暗建」的另一半
            // 就在于这里：整个记录只有 settings 一行，没有后台任务、没有缓存目录。
            let raw = db.setting_raw(LAST_BACKUP_KEY).expect("raw").expect("row");
            assert!(raw.starts_with('{'), "settings 里应是一行 JSON：{raw}");
        }
        // 模拟重启：关连接、重开同一个库文件。
        let reopened = picasa_db::Db::open(&db_path).expect("reopen");
        assert_eq!(
            load_last_backup(&reopened),
            Some(backup.clone()),
            "重启后最近成功备份必须还在（IDX-06 P1）"
        );
        // 损坏的记录不能冒充成功备份。
        reopened
            .set_setting_raw(LAST_BACKUP_KEY, Some("{\"path\":\"/x\"}"))
            .expect("corrupt");
        assert_eq!(load_last_backup(&reopened), None, "半截记录一律当作没有");
        // 清掉之后也不再显示。
        reopened
            .set_setting_raw(LAST_BACKUP_KEY, None)
            .expect("clear");
        assert_eq!(load_last_backup(&reopened), None);
    }

    /// IDX-06：时间文案带 UTC 与记录时区，且不含 ETA / 假百分比。
    #[test]
    fn format_backup_time_is_utc_and_records_timezone() {
        let s = format_backup_time(0, "Asia/Shanghai");
        assert!(s.contains("UTC"), "{s}");
        assert!(s.contains("Asia/Shanghai"), "{s}");
        assert!(!s.contains("预计"), "{s}");
    }

    /// M4.3 §3A ⑦：中断导出 / 中断导入 / 已提交导入三种持久记录都是
    /// 「宁可少提示也不误报」——坏 JSON、缺字段、类型不对一律丢弃。
    #[test]
    fn interrupted_export_json_rejects_broken_records() {
        let ok = InterruptedExport {
            target: "/Users/x/Desktop/backup.opicasaidx".into(),
            with_thumbs: true,
        };
        let json = ok.to_json();
        assert_eq!(InterruptedExport::from_json(&json), Some(ok));
        assert_eq!(InterruptedExport::from_json("not json"), None);
        assert_eq!(InterruptedExport::from_json("{}"), None);
        assert_eq!(
            InterruptedExport::from_json(r#"{"target":"/a","with_thumbs":"yes"}"#),
            None
        );
    }

    #[test]
    fn import_summary_json_roundtrip_and_text_has_no_eta() {
        let s = ImportSummary {
            at: 1_793_536_496,
            imported: 120,
            skipped: 3,
            failed: 1,
            conflicts: 2,
        };
        assert_eq!(ImportSummary::from_json(&s.to_json()), Some(s.clone()));
        assert_eq!(ImportSummary::from_json("oops"), None);
        let text = s.text();
        assert!(text.contains("成功 120"), "{text}");
        assert!(text.contains("失败 1"), "{text}");
        for banned in ["预计", "剩余", "ETA"] {
            assert!(!text.contains(banned), "{text} 不得含 {banned}");
        }
    }

    /// 三种重启提示各自有独立动作，且都带「清持久标记」语义——
    /// `DismissInterruptedExport` / `DismissInterruptedImport` 不能被派发成对方。
    #[test]
    fn restart_banner_actions_are_distinct() {
        assert_ne!(MgrAction::ReplayExport, MgrAction::DismissInterruptedExport);
        assert_ne!(MgrAction::DismissInterruptedExport, MgrAction::DismissInterruptedImport);
        assert_ne!(MgrAction::ReplayExport, MgrAction::CancelImport);
    }

    /// 默认 `IoUi` 里三条重启记录都为空：正常会话首屏不该冒出任何「上次未完成」。
    #[test]
    fn io_ui_default_has_no_restart_banners() {
        let io = IoUi::default();
        assert!(io.interrupted_export.is_none());
        assert!(!io.interrupted_import);
        assert!(io.last_import.is_none());
        // IDX-21 ⑤：默认没有隐藏的来源，导入导出页不该凭空多出「恢复」区。
        assert!(io.hidden_sources.is_empty());
    }

    /// M4.3 阶段 3C（IDX-19/21）：导入结果页的两个新动作必须可区分，
    /// 且携带的是库里的真实 id —— 混淆了就会「浏览错批次」或「恢复错来源」。
    #[test]
    fn import_result_actions_carry_real_ids() {
        assert_eq!(
            MgrAction::BrowseImportedPhotos(11),
            MgrAction::BrowseImportedPhotos(11)
        );
        assert_ne!(
            MgrAction::BrowseImportedPhotos(11),
            MgrAction::BrowseImportedPhotos(12),
            "不同批次必须可区分，否则会浏览到别的批次"
        );
        assert_ne!(
            MgrAction::BrowseImportedPhotos(11),
            MgrAction::UnhideImportSource(11),
            "浏览照片与恢复分组是两个动作"
        );
        assert_ne!(
            MgrAction::UnhideImportSource(2),
            MgrAction::UnhideImportSource(3),
            "不同来源必须可区分"
        );
    }

    /// 「已从分组隐藏」的恢复区文案不许出现 ETA，也不许暗示会动索引。
    #[test]
    fn hidden_source_hint_never_promises_index_changes() {
        let hint = "只影响左栏分组显示；索引与用户标记一直都在";
        for banned in ["预计", "剩余", "ETA", "删除"] {
            assert!(!hint.contains(banned), "提示里不许出现 {banned}");
        }
        assert!(hint.contains("索引"), "要明确索引还在，用户才敢恢复");
    }

    /// M4.3 阶段 1：逐任务暂停 / 继续动作带的是库里的 task_id，
    /// 且三个任务动作彼此可区分（PartialEq 让派发与断言都可靠）。
    #[test]
    fn per_task_actions_carry_the_task_id() {
        assert_eq!(MgrAction::PauseTask(7), MgrAction::PauseTask(7));
        assert_eq!(MgrAction::ResumeTask(7), MgrAction::ResumeTask(7));
        assert_ne!(MgrAction::PauseTask(7), MgrAction::ResumeTask(7));
        assert_ne!(MgrAction::PauseTask(7), MgrAction::CancelTask(7));
        assert_ne!(MgrAction::PauseTask(7), MgrAction::PauseTask(8));
    }

    /// M4.3 阶段 2 · 第三批（IDX-13）：「强制重扫」是一个独立动作，
    /// 绝不能被误派发成「重新索引」或「全量重建」（后者要弹确认框）。
    #[test]
    fn force_rescan_action_is_distinguishable_from_reindex_and_rebuild() {
        assert_eq!(MgrAction::ForceRescan(7), MgrAction::ForceRescan(7));
        assert_ne!(MgrAction::ForceRescan(7), MgrAction::ForceRescan(8));
        assert_ne!(MgrAction::ForceRescan(7), MgrAction::ReindexRoot(7));
        assert_ne!(MgrAction::ForceRescan(7), MgrAction::RequestRebuild(7));
        assert_ne!(MgrAction::ForceRescan(7), MgrAction::RebuildRoot(7));
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
            user_paused: false,
            skipped: 0,
            cancel_reason: None,
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
            user_paused: false,
            skipped: 0,
            cancel_reason: None,
        };
        let s = task_progress_text(&t);
        assert!(s.contains("120") && s.contains("01:23"), "{s}");
        assert!(!s.contains("预计") && !s.contains("剩余"), "ETA 是永久禁令");
    }

    /// M4.3 IDX-17：终态行的结束摘要必须同时给出「完成 / 跳过 / 失败 /
    /// 取消及原因」，让「部分成功」与「已取消」在面板上一眼可分（TC-M43-11）。
    #[test]
    fn task_end_summary_covers_completed_skipped_failed_and_cancel_reason() {
        let mut t = TaskView {
            task_id: 1,
            running: false,
            scope: "/a".into(),
            kind: "首次索引".into(),
            status: "部分失败".into(),
            discovered: 10,
            completed: 7,
            failed: 2,
            stage: "—".into(),
            batch: None,
            started_at: Some(0),
            elapsed_text: "00:10".into(),
            outcome: Some("部分失败".into()),
            root_id: None,
            user_paused: false,
            skipped: 1,
            cancel_reason: None,
        };
        let s = task_end_summary(&t);
        assert!(s.contains("完成 7") && s.contains("跳过 1") && s.contains("失败 2"), "{s}");
        assert!(!s.contains("取消"), "未取消的行不该出现取消字样");
        assert!(!s.contains("预计") && !s.contains("剩余"), "ETA 是永久禁令");

        t.cancel_reason = Some("用户取消".into());
        t.outcome = Some("已取消".into());
        let s2 = task_end_summary(&t);
        assert!(s2.contains("已取消（用户取消）"), "{s2}");
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
            path: "/Users/me/Pictures".into(),
            photos: Some(4321),
            markers: Some(17),
        };
        let b = c.body();
        assert!(b.contains("4321"), "照片数必须出现在文案里：{b}");
        assert!(b.contains("17"), "标记数必须出现在文案里：{b}");
        assert!(b.contains("/Users/me/Pictures"), "路径必须出现在文案里：{b}");
        assert!(b.contains("照片文件不受影响"), "{b}");
        assert!(b.contains("缩略图"), "{b}");
        // 「不先清再建」的承诺也要写进去（§4.2 M22-IDX-06）
        assert!(b.contains("旧缩略图保持可见"), "{b}");
        assert_eq!(c.title(), "确认全量重建");
        assert!(Confirm::None.is_none());
    }

    #[test]
    fn idx11_confirmation_shows_scope_path_counts_and_pending_never_zero() {
        let c = Confirm::DeleteIndex {
            id: 7,
            path: "/media/2020".into(),
            photos: None,   // 统计失败/未知
            markers: None,  // 统计失败/未知
        };
        let b = c.body();
        assert!(b.contains("范围"), "必须写明作用范围：{b}");
        assert!(b.contains("/media/2020"), "必须写明路径：{b}");
        assert!(b.contains("待计算"), "未知数量必须显示「待计算」：{b}");
        assert!(!b.contains("照片数：0 张"), "未知数量不能填 0：{b}");
        assert!(!b.contains("标记数：0 张"), "未知数量不能填 0：{b}");
        assert!(b.contains("原图"), "必须说明原图不动：{b}");
        assert!(b.contains("不会"), "必须写明不会删除磁盘文件：{b}");
        assert!(b.contains("保留"), "必须写明保留项：{b}");
        assert_eq!(c.title(), "确认删除索引");
        // 把「取消」放前 + 聚焦 + Escape 关闭属于窗口行为，见 confirm_dialog 实装；
        // 这里保证纯函数文案齐全、且默认语义不由文案冒充数字。
    }

    /// m10756：现在只有一个「删除索引」按钮，它承担彻底删除。
    ///
    /// 文案必须把「这一行也消失、不能撤销」讲清楚——用户抱怨的正是
    /// 「删了还在」，而旧文案承诺的是「保留、可恢复」，两个承诺正好相反。
    /// 同时继续遵守 IDX-11（未知数量显示「待计算」，绝不填 0）。
    #[test]
    fn the_delete_confirmation_now_promises_a_thorough_removal() {
        let c = Confirm::DeleteIndex {
            id: 9,
            path: "/media/2021".into(),
            photos: Some(1234),
            markers: Some(7),
        };
        let b = c.body();
        assert_eq!(c.title(), "确认删除索引");
        assert!(b.contains("范围"), "必须写明作用范围：{b}");
        assert!(b.contains("/media/2021"), "必须写明路径：{b}");
        assert!(b.contains("1234"), "必须显示照片数：{b}");
        assert!(b.contains('7'), "必须显示标记数：{b}");
        assert!(b.contains("彻底删除"), "{b}");
        // 用户抱怨的正是「删了还在」，所以文案必须承诺「这一行也消失」。
        assert!(b.contains("这一行"), "必须说明索引管理里那一行也会消失：{b}");
        assert!(b.contains("不能撤销"), "不可逆必须写明：{b}");
        assert!(b.contains("缩略图"), "必须说明缩略图一起删：{b}");
        assert!(b.contains("原图"), "必须说明原图不动：{b}");
        assert!(b.contains("不会"), "必须写明不会删除磁盘文件：{b}");
        // 未知数量仍然不许填 0。
        let unknown = Confirm::DeleteIndex {
            id: 9,
            path: "/media/2021".into(),
            photos: None,
            markers: None,
        };
        let ub = unknown.body();
        assert!(ub.contains("待计算"), "未知数量必须显示「待计算」：{ub}");
        assert!(!ub.contains("照片数：0 张"), "{ub}");
    }

    /// m10609：**不管什么状态**的索引都要能彻底删除。
    ///
    /// 早先「彻底删除索引…」只画在底部按钮区，而「索引已移除」那一行在
    /// 到达按钮区之前就 `return` 了 —— 恰恰是「派生数据已清空、只剩一条空壳」
    /// 这种最需要删干净的状态反而没有入口。这里把四种状态钉住：
    /// 活跃 / 已关闭 / 离线 / 索引已移除。
    #[test]
    fn delete_index_is_offered_for_every_root_state() {
        let base = RootView {
            id: 3,
            path: "/Volumes/Expansion".into(),
            ..RootView::default()
        };
        assert!(delete_index_available(&base), "活跃根必须能彻底删除");

        let closed = RootView {
            closed: true,
            ..base.clone()
        };
        assert!(delete_index_available(&closed), "已关闭的根必须能彻底删除");

        let offline = RootView {
            offline: true,
            ..base.clone()
        };
        assert!(delete_index_available(&offline), "离线根必须能彻底删除（只动自有 DB，不读原图）");

        let removed = RootView {
            index_removed: true,
            closed: true,
            ..base.clone()
        };
        assert!(
            delete_index_available(&removed),
            "索引已移除的空壳行必须能彻底删除，否则它永远留在索引管理里"
        );
    }

    /// 源码级回归：那个早退分支里**必须**真的调用 `delete_index_button`。
    ///
    /// 只断言 `delete_index_available()` 恒真是抓不住这个 bug 的——真正的坑是
    /// 「分支在到达底部按钮区之前就 `return` 了」，函数再对也没人调用它。
    /// 所以像 `textures.rs` 那样直接钉源码：`if r.index_removed {` 到第一个
    /// `return;` 之间必须出现 `delete_index_button`。
    #[test]
    fn the_index_removed_branch_actually_calls_the_delete_button() {
        let src = include_str!("index_mgr.rs");
        let start = src
            .find("if r.index_removed {")
            .expect("找不到「索引已移除」的早退分支");
        let rest = &src[start..];
        let end = rest.find("return;").expect("该分支必须以 return 结束");
        let branch = &rest[..end];
        assert!(
            branch.contains("delete_index_button"),
            "「索引已移除」这一行在 return 之前必须画「删除索引」，否则这种状态永远删不掉：{branch}"
        );
    }

    #[test]
    fn idx11_clear_thumbs_is_scoped_to_whole_library() {
        let c = Confirm::ClearThumbs;
        let b = c.body();
        assert!(b.contains("全库"), "全库动作必须写明范围：{b}");
        assert!(b.contains("照片文件不受影响"), "{b}");
        assert_eq!(c.title(), "确认清理缩略图");
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
        // M42-UI-07 / M42-PERF-01 新增字段必须仍是 Default，不能破坏会话初态。
        assert!(io.preview_thumbs.is_empty());
        assert!(io.cancel.is_none());
    }

    #[test]
    fn cancel_flag_in_io_ui_is_shared_and_observable() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        // 验证 IoUi.cancel 指向的 Arc 与「后台任务持有的 Arc」可共享置位（单一真相源）。
        let flag = Arc::new(AtomicBool::new(false));
        let io = IoUi {
            cancel: Some(Arc::clone(&flag)),
            ..Default::default()
        };
        io.cancel.as_ref().unwrap().store(true, Ordering::SeqCst);
        assert!(flag.load(Ordering::SeqCst), "同一个 Arc 必须看到置位");
    }

    #[test]
    fn preview_thumbs_defaults_to_empty_without_breaking_preview() {
        // 无缩略图包（with_thumbs=false）→ preview_thumbs 为空 → 预览格走「░ 无预览」占位。
        let io = IoUi::default();
        assert!(io.preview_thumbs.is_empty());
        assert!(io.preview.is_none(), "空预览数据不构造假 preview");
    }

    #[test]
    fn index_mgr_ui_default_keeps_io_state() {
        let ui = IndexMgrUi::new();
        assert!(!ui.open);
        assert_eq!(ui.tab, MgmtTab::Tasks);
        assert_eq!(ui.io.state, IoState::Idle);
    }
}
