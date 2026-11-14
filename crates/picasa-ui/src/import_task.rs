//! M4.2 `M42-PERF-01`：索引导入 / 导出的后台任务封装（自包含接线模块）。
//!
//! 背景：`Db` 含 `rusqlite::Connection`、不是 `Send`，不能把主线程的 `Db`（藏在
//! `Library.db: Arc<Mutex<Db>>` 里）搬进后台线程。所以后台线程用 `AppPaths::db_file()`
//! **自己打开一个独立连接**，跑完即 `drop`；UI 帧只通过 `mpsc::Receiver` 非阻塞取消息，
//! 绝不等线程、也绝不碰 `egui::Context`（重绘由 app 侧在任务活跃期按需安排）。
//!
//! 取消：`ImportTask` 持有一个 `Arc<AtomicBool>`；`cancel()`、`IoUi.cancel` 与「取消导入」
//! 按钮都指向**同一个**原子布尔（单一真相源 R-M42-5）。`execute_import` 本就是单事务、
//! 阶段间检查取消标志；事务回滚天然保证「现有索引零改动」，staging 文件由
//! `execute_import` 自己删除，独立连接在线程结束时释放 —— 无残留 staging 文件 / 连接。

use crate::index_mgr::{BackupInfo, ImportStep, IoState, IoUi, ProgressSnapshot, human_bytes};
use picasa_db::{
    Db, ImportPreview, ImportReport, ImportStrategy, execute_import, export_index,
    inspect_index_cancellable,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

/// M4.3 §3A ⑦：后台任务是哪一种。app 侧用它决定终态时清哪个「进行中」重启标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoJobKind {
    Export,
    Import,
    /// M4.3 PERF：选文件后的「校验 + 预览」。不写库，只读原文件与工作目录。
    Inspect,
}

/// 后台任务终态后，UI 帧需要执行的 **app 级副作用**。
///
/// 这些是 `OpenPicasaApp` 的私有状态（目录树缓存、侧栏缓存、EXIF 回填、toast），
/// `import_task` 作为兄弟模块碰不到，因此以纯数据形式交回 app 侧消费。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskEffects {
    /// 成功后重新加载目录树 / 照片缓存（导入新增了记录）。
    pub reload_library: bool,
    /// 侧栏标签 / 相册缓存脏标记。
    pub sidebar_groups_dirty: bool,
    /// 触发 EXIF 回填（导入的照片也要回填）。
    pub exif_pending: bool,
    /// 状态栏 toast：`(文案, 秒数)`。
    pub toast: Option<(String, f32)>,
    /// IDX-06：导出成功后要持久化的备份记录 JSON（写入 `settings["m43_last_backup"]`）。
    /// 用 JSON 字符串而非 `BackupInfo` 只是避免 `TaskEffects` 跨模块依赖类型；
    /// 失败 / 取消 / 导入一律 `None`。
    pub last_backup_json: Option<String>,
    /// M4.3 §3A ⑦：导入**提交成功**后的结果摘要（写入 `settings["m43_import_summary"]`）。
    /// 只有单事务提交成功才置 `Some`；提交前失败 / 取消一律 `None`
    /// （那时 `m43_import_pending` 还在，重启只能提示「已回滚」，绝不能显示成成功）。
    pub import_summary: Option<crate::index_mgr::ImportSummary>,
}

/// 终态消息：`apply_io_msg` 把它的内容一次性写进 `IoUi`。
#[derive(Clone, Debug)]
pub struct Done {
    pub state: IoState,
    /// 结果 / 错误正文（可理解的人话）。
    pub message: String,
    /// 仅导出成功：写进 `io.export_path`。
    pub export_path: Option<String>,
    /// 仅导入成功：写进 `io.report` 并把向导切到 Result。
    pub report: Option<ImportReport>,
    /// IDX-06：仅导出成功后写入 `io.last_backup`（同时经 effects 持久化到 settings）。
    pub backup: Option<BackupInfo>,
    /// app 级副作用（由 `apply_io_msg` 返回给调用方消费）。
    pub effects: TaskEffects,
}

impl Done {
    /// 失败 / 异常终态：无导出路径、无报告、无副作用。
    fn failure(message: impl Into<String>) -> Self {
        Self {
            state: IoState::Failed,
            message: message.into(),
            export_path: None,
            report: None,
            backup: None,
            effects: TaskEffects::default(),
        }
    }
}

/// 后台线程 → UI 帧的消息。
#[derive(Clone, Debug)]
pub enum Msg {
    /// 进度更新（M4.3 IDX-18）：携带完整 `ProgressSnapshot`。
    /// `total = None`（或 0）= 不确定态（循环条），**不用假百分比**。
    Progress(ProgressSnapshot),
    /// 终态成功 / 失败。`Done` 装箱：它含 `ImportReport`（带 SHA-256 / 备份路径等
    /// 较多字段），不装箱会把整个 `Msg` 撑到 240B，channel 里每条消息都跟着变大。
    Done(Box<Done>),
    /// 取消（整库回滚、无残留 staging 文件 / 连接）。
    Cancelled,
    /// 仅 `IoJobKind::Inspect`：校验通过，把预览交给 UI 帧。
    ///
    /// 装箱同 `Done`：`ImportPreview` 含清单、文件夹树与最多 12 张缩略图字节，
    /// 直接放进枚举会把 channel 里每条消息都撑大。
    Preview(Box<PreviewDone>),
}

/// 校验通过后的预览载荷：预览本身 + 它来自哪个文件。
///
/// 带上路径是因为 `apply_io_msg` 要把 `io.import_path` 一起写好——后续
/// 「开始导入」直接读 `io.import_path`，不该指望调用方再传一次。
#[derive(Clone, Debug)]
pub struct PreviewDone {
    pub src: String,
    pub preview: ImportPreview,
}

/// 构造终态消息（装箱）。发送点统一走这里，避免各处手写 `Box::new`。
fn done_msg(d: Done) -> Msg {
    Msg::Done(Box::new(d))
}

/// 构造进度消息（M4.3 IDX-18）：阶段 + 已处理 / 总数 + 当前项 + 修订号。
/// `total = None` 表示总数未知（导出 / 校验这类不透明单次调用）→ 不确定态，
/// 按 §3A 口径**不得编造百分比**。
fn progress_msg(stage: &str, done: u64, total: Option<u64>, revision: u64) -> Msg {
    Msg::Progress(ProgressSnapshot {
        stage: stage.to_string(),
        done,
        total,
        current: String::new(),
        errors: Vec::new(),
        revision,
    })
}

/// 进度文案（纯函数）。**不含 ETA / 预计 / 剩余**（永久禁令，docs/11 §2.1.2）。
pub fn progress_text(stage: &str) -> String {
    format!("{stage}（后台执行，界面可继续操作）")
}

/// 取消态文案：明确「整库回滚、无残留 staging 文件/连接」（docs/16 §3）。
pub fn cancel_message() -> String {
    "已取消 · 整库回滚、无残留 staging 文件/连接 · 现有索引未改动".to_string()
}

/// 取消标志 → 终态（纯函数，单测入口）。
pub fn map_cancel(cancelled: bool) -> IoState {
    if cancelled {
        IoState::Cancelled
    } else {
        IoState::Failed
    }
}

/// 失败分支的终态判定（纯函数）：错误文本含「取消」优先认取消，否则看取消标志。
pub fn classify_failure(err_text: &str, cancelled: bool) -> IoState {
    if err_text.contains("取消") {
        IoState::Cancelled
    } else {
        map_cancel(cancelled)
    }
}

/// 把后台消息应用到 `IoUi`（纯函数，不碰 egui / DB / 线程）。
///
/// 返回 `Some(effects)` 仅当消息是终态 `Done`（app 侧据此执行 app 级副作用）；
/// `Progress` / `Cancelled` 返回 `None`。六态由此函数统一收敛。
pub fn apply_io_msg(io: &mut IoUi, msg: Msg) -> Option<TaskEffects> {
    match msg {
        Msg::Progress(snap) => {
            // M4.3 IDX-18：乱序到达的旧 revision 不得回退进度（§3A 百分比单调）。
            // 新任务启动时 app 侧把 io.revision 置 0，这里以 revision 为主键去重。
            if let Some(prev) = &io.snapshot
                && snap.revision < prev.revision
            {
                return None;
            }
            io.state = IoState::Running;
            io.progress = crate::index_mgr::progress_label(&snap);
            io.fraction = snap.fraction();
            // 上包络：百分比只增不减（镜像 IndexBoard::bump_percent）。
            if let Some(p) = snap.percent() {
                io.percent = io.percent.max(p);
            }
            io.snapshot = Some(snap);
            None
        }
        Msg::Cancelled => {
            io.state = IoState::Cancelled;
            io.message = cancel_message();
            io.progress.clear();
            io.fraction = None;
            io.snapshot = None;
            None
        }
        Msg::Preview(p) => {
            // 校验通过：回到空闲态、进「校验结果」步，等用户确认后才真正写库。
            let PreviewDone { src, preview } = *p;
            io.state = IoState::Idle;
            io.message.clear();
            io.progress.clear();
            io.fraction = None;
            io.snapshot = None;
            io.cancel = None;
            io.import_path = src;
            io.roots_to_remap = preview
                .manifest
                .source_roots
                .iter()
                .map(|r| (r.path.clone(), std::path::Path::new(&r.path).exists()))
                .collect();
            io.preview_thumbs = preview.thumbs.clone();
            io.preview = Some(preview);
            io.import_step = ImportStep::Validate;
            io.understood = false;
            io.report = None;
            None
        }
        Msg::Done(d) => {
            let d = *d;
            io.state = d.state;
            io.message = d.message.clone();
            io.progress.clear();
            io.fraction = None;
            io.snapshot = None;
            if d.state == IoState::Success {
                io.last_result = d.message.clone();
            }
            if let Some(p) = d.export_path {
                io.export_path = p;
            }
            if let Some(r) = d.report {
                io.report = Some(r);
                io.import_step = ImportStep::Result;
            }
            // IDX-06：仅成功导出写「最近成功备份」，失败 / 取消 / 导入都不写。
            if let Some(b) = d.backup {
                io.last_backup = Some(b);
            }
            Some(d.effects)
        }
    }
}

/// 一个正在跑（或刚跑完）的后台导入 / 导出任务。
///
/// 故意**不 derive Clone/Debug**：它含 `Receiver<Msg>` 与 `JoinHandle<()>`，二者都不可
/// Clone。由 `OpenPicasaApp` 以 `Option<ImportTask>` 持有；`IoUi` 仍 derive
/// Clone/Debug/Default，里面只放纯数据（见 lead 的编译陷阱提醒）。
pub struct ImportTask {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// 是否已消费到终态消息（用于停止活跃期的 `request_repaint_after`）。
    finished: bool,
    /// M4.3 §3A ⑦：这是导出还是导入。app 侧在终态（含取消）时据此清掉对应的
    /// `settings["m43_export_pending" / "m43_import_pending"]` 标记。
    kind: IoJobKind,
}

impl ImportTask {
    /// 启动**导出**后台线程（`db_path` 来自 `AppPaths::db_file()`）。
    pub fn start_export(db_path: PathBuf, target: PathBuf, with_thumbs: bool) -> Self {
        let mut task =
            Self::spawn(move |cancel, tx| run_export(db_path, target, with_thumbs, cancel, tx));
        task.kind = IoJobKind::Export;
        task
    }

    /// 启动**导入**后台线程（`db_path` 来自 `AppPaths::db_file()`）。
    pub fn start_import(
        db_path: PathBuf,
        src: PathBuf,
        work_dir: PathBuf,
        strategy: ImportStrategy,
    ) -> Self {
        let mut task = Self::spawn(move |cancel, tx| {
            run_import(db_path, src, work_dir, strategy, cancel, tx)
        });
        task.kind = IoJobKind::Import;
        task
    }

    /// M4.3 PERF：启动**预览校验**后台线程（选完文件立刻返回，UI 不再冻结）。
    ///
    /// 用户真实包 9 GiB：校验 + integrity_check + 双遍摘要要一分钟以上，
    /// 放在 UI 线程上就是「程序无响应」。这里只读不写，取消时删掉工作副本即可。
    pub fn start_inspect(db_path: PathBuf, src: PathBuf, work_dir: PathBuf) -> Self {
        let mut task = Self::spawn(move |cancel, tx| {
            run_inspect(db_path, src, work_dir, cancel, tx)
        });
        task.kind = IoJobKind::Inspect;
        task
    }

    fn spawn<F>(f: F) -> Self
    where
        F: FnOnce(Arc<AtomicBool>, Sender<Msg>) + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let handle = std::thread::spawn(move || f(thread_cancel, tx));
        Self {
            rx,
            cancel,
            handle: Some(handle),
            finished: false,
            kind: IoJobKind::Import,
        }
    }

    /// M4.3 §3A ⑦：本任务属于导出还是导入（终态时决定清哪个重启标记）。
    pub fn kind(&self) -> IoJobKind {
        self.kind
    }

    /// 真取消：把共享标志置位（`SeqCst`）。后台线程阶段间检查到即回滚退出。
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// 取出**同一个**取消标志，供 `IoUi.cancel` / 「取消导入」按钮直接置位（单一真相源）。
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// UI 帧调用：非阻塞取一条消息（内部绝不碰 `egui::Context`）。
    pub fn tick(&mut self) -> Option<Msg> {
        match self.rx.try_recv() {
            Ok(msg) => {
                // `Preview` 也算本任务的最后一条消息：`run_inspect` 发完就返回，
                // 线程随即结束。不标记 finished 的话，channel 断开后
                // `tick` 会兜底造一个假的「后台任务异常退出」失败态。
                if matches!(msg, Msg::Done(_) | Msg::Cancelled | Msg::Preview(_)) {
                    self.finished = true;
                }
                Some(msg)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                // 线程异常退出却没发终态：兜底一次失败 Done，避免把「进行中」钉死。
                if self.finished {
                    return None;
                }
                self.finished = true;
                let note = if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
                    "后台任务异常退出（未返回结果）"
                } else {
                    "后台任务通道异常断开"
                };
                Some(done_msg(Done::failure(format!(
                    "{note}（现有索引未改动）"
                ))))
            }
        }
    }

    /// 任务是否仍活跃（未消费到终态消息）。app 侧据此决定活跃期是否
    /// `ctx.request_repaint_after(120ms)`；终态一到即返回 false、停止重绘。
    pub fn is_active(&self) -> bool {
        !self.finished
    }
}

/// 校验 + 预览（`IoJobKind::Inspect`）。全程只读：`inspect_index_cancellable`
/// 在 `work_dir` 里做副本，结束自己删；失败也删。绝不碰现有索引。
fn run_inspect(
    db_path: PathBuf,
    src: PathBuf,
    work_dir: PathBuf,
    cancel: Arc<AtomicBool>,
    tx: Sender<Msg>,
) {
    let _ = tx.send(progress_msg("正在校验索引文件", 0, None, 1));
    if cancel.load(Ordering::SeqCst) {
        let _ = tx.send(Msg::Cancelled);
        return;
    }
    // 只读校验也要自己开连接：Db 不是 Send，不能搬主线程那个。
    let db = match Db::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            let _ = tx.send(done_msg(Done::failure(format!(
                "无法读取现有索引库：{e}（未做任何改动）"
            ))));
            return;
        }
    };
    let result = inspect_index_cancellable(&db, &src, &work_dir, Some(cancel.as_ref()));
    drop(db);
    match result {
        Ok(preview) => {
            // 校验完立即发预览；取消在这之后到达也没关系——那只是用户改主意，
            // UI 侧会把状态收敛成取消，staging 已由 inspect 自己删掉。
            if cancel.load(Ordering::SeqCst) {
                let _ = tx.send(Msg::Cancelled);
                return;
            }
            let _ = tx.send(Msg::Preview(Box::new(PreviewDone {
                src: src.to_string_lossy().to_string(),
                preview,
            })));
        }
        Err(e) => {
            let msg = e.to_string();
            if classify_failure(&msg, cancel.load(Ordering::SeqCst)) == IoState::Cancelled {
                let _ = tx.send(Msg::Cancelled);
            } else {
                let _ = tx.send(done_msg(Done::failure(format!(
                    "无法导入：{msg}（现有索引未改动）"
                ))));
            }
        }
    }
}

fn run_export(
    db_path: PathBuf,
    target: PathBuf,
    with_thumbs: bool,
    cancel: Arc<AtomicBool>,
    tx: Sender<Msg>,
) {
    let _ = tx.send(progress_msg("正在导出索引文件", 0, None, 1));
    // 关键点检查取消（导出在关键点检查即可，docs/16 §3）。
    if cancel.load(Ordering::SeqCst) {
        let _ = tx.send(Msg::Cancelled);
        return;
    }
    // 关键：自己 open 独立连接（Db 不是 Send，不能搬主线程的）。
    let db = match Db::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            let _ = tx.send(done_msg(Done::failure(format!(
                "导出失败：无法打开索引库：{e}（现有索引未改动）"
            ))));
            return;
        }
    };
    if cancel.load(Ordering::SeqCst) {
        let _ = tx.send(Msg::Cancelled);
        return;
    }
    let now = chrono::Utc::now().timestamp();
    let result = export_index(
        &db,
        &target,
        with_thumbs,
        env!("CARGO_PKG_VERSION"),
        now,
    );
    drop(db); // 独立连接用完即弃，不留残留连接
    match result {
        Ok(rep) => {
            // IDX-06：只有产物真正发布成功（`Ok(rep)`）才记录备份。
            let backup = BackupInfo {
                path: target.to_string_lossy().to_string(),
                created_at: rep.created_at,
                sha256: rep.sha256.clone(),
                timezone: rep.timezone.clone(),
            };
            let _ = tx.send(done_msg(Done {
                state: IoState::Success,
                message: format!(
                    "导出成功 · {} · 照片 {} · 标签 {} · 相册 {}",
                    human_bytes(rep.bytes),
                    rep.counts.photos,
                    rep.counts.tags,
                    rep.counts.albums
                ),
                export_path: Some(target.to_string_lossy().to_string()),
                report: None,
                backup: Some(backup.clone()),
                effects: TaskEffects {
                    toast: Some(("✓ 导出成功".into(), 3.0)),
                    last_backup_json: Some(backup.to_json()),
                    ..Default::default()
                },
            }));
        }
        Err(e) => {
            let msg = e.to_string();
            if classify_failure(&msg, cancel.load(Ordering::SeqCst)) == IoState::Cancelled {
                let _ = tx.send(Msg::Cancelled);
            } else {
                let _ = tx.send(done_msg(Done::failure(format!(
                    "导出失败：{msg}（现有索引未改动）"
                ))));
            }
        }
    }
}

fn run_import(
    db_path: PathBuf,
    src: PathBuf,
    work_dir: PathBuf,
    strategy: ImportStrategy,
    cancel: Arc<AtomicBool>,
    tx: Sender<Msg>,
) {
    let _ = tx.send(progress_msg("正在校验索引文件并准备导入", 0, None, 1));
    if cancel.load(Ordering::SeqCst) {
        let _ = tx.send(Msg::Cancelled);
        return;
    }
    // 关键：自己 open 独立连接，execute_import 在该连接上跑单事务 + ATTACH。
    let mut db = match Db::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            let _ = tx.send(done_msg(Done::failure(format!(
                "无法打开索引库：{e}（现有索引未改动）"
            ))));
            return;
        }
    };
    if cancel.load(Ordering::SeqCst) {
        let _ = tx.send(Msg::Cancelled);
        return;
    }
    let _ = tx.send(progress_msg("正在写入整理关系（单事务）", 0, None, 2));
    let result = execute_import(&mut db, &src, &work_dir, strategy, cancel.as_ref());
    drop(db); // 独立连接 + ATTACH 一并释放
    match result {
        Ok(rep) => {
            // §3A ⑦：提交成功才写「已提交的导入结果」摘要（重启后仍显示完成结果）。
            let summary = crate::index_mgr::ImportSummary {
                at: chrono::Utc::now().timestamp(),
                imported: rep.imported,
                skipped: rep.skipped,
                failed: rep.failed,
                conflicts: rep.conflicts,
            };
            let _ = tx.send(done_msg(Done {
                state: IoState::Success,
                message: format!(
                    "导入完成 · 成功 {} · 跳过 {} · 失败 {} · 冲突 {}",
                    rep.imported, rep.skipped, rep.failed, rep.conflicts
                ),
                export_path: None,
                report: Some(rep),
                backup: None,
                effects: TaskEffects {
                    reload_library: true,
                    sidebar_groups_dirty: true,
                    exif_pending: true,
                    toast: Some(("✓ 导入完成".into(), 3.0)),
                    import_summary: Some(summary),
                    ..Default::default()
                },
            }));
        }
        Err(e) => {
            let msg = e.to_string();
            if classify_failure(&msg, cancel.load(Ordering::SeqCst)) == IoState::Cancelled {
                let _ = tx.send(Msg::Cancelled);
            } else {
                let _ = tx.send(done_msg(Done::failure(format!(
                    "{msg}（现有索引未改动）"
                ))));
            }
        }
    }
}

// ---------------------------------------------------------------- 测试（纯函数，不依赖真实 DB / 线程）

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_mgr::io_state_label;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn map_cancel_maps_flagged_to_cancelled() {
        assert_eq!(map_cancel(true), IoState::Cancelled);
        assert_eq!(map_cancel(false), IoState::Failed);
    }

    #[test]
    fn classify_failure_detects_cancel_text_and_flag() {
        assert_eq!(classify_failure("导入已取消", false), IoState::Cancelled);
        assert_eq!(classify_failure("磁盘已满", true), IoState::Cancelled);
        assert_eq!(classify_failure("磁盘已满", false), IoState::Failed);
    }

    #[test]
    fn cancel_is_shared_arc_with_seqcst_store() {
        // 单一真相源：`cancel()` 与 `cancel_flag()` 指向同一个 Arc，后台线程
        // 检查 `load(SeqCst)` 能立刻看到置位（真取消，不再是不可达的局部 Arc）。
        let rx = mpsc::channel::<Msg>().1;
        let task = ImportTask {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            handle: None,
            finished: false,
            kind: IoJobKind::Import,
        };
        let shared = task.cancel_flag();
        assert!(!shared.load(Ordering::SeqCst));
        task.cancel();
        assert!(shared.load(Ordering::SeqCst), "store(SeqCst) 对同一 Arc 可见");
        assert!(task.cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn progress_text_has_no_eta() {
        let s = progress_text("正在写入整理关系");
        assert!(s.contains("后台执行"), "{s}");
        assert!(!s.contains("ETA"), "{s}");
        assert!(!s.contains("预计"), "{s}");
        assert!(!s.contains("剩余"), "{s}");
    }

    #[test]
    fn cancel_message_explains_full_rollback() {
        let s = cancel_message();
        assert!(s.contains("整库回滚"), "{s}");
        assert!(s.contains("无残留"), "{s}");
        assert!(s.contains("staging"), "{s}");
        assert!(s.contains("连接"), "{s}");
    }

    #[test]
    fn all_six_states_have_distinct_non_empty_labels() {
        let states = [
            IoState::Idle,
            IoState::Preparing,
            IoState::Running,
            IoState::Success,
            IoState::Failed,
            IoState::Cancelled,
        ];
        let labels: Vec<&str> = states.iter().map(|s| io_state_label(*s)).collect();
        assert!(labels.iter().all(|l| !l.is_empty()));
        assert_eq!(labels.iter().copied().collect::<HashSet<_>>().len(), 6);
    }

    #[test]
    fn apply_progress_sets_running_and_indeterminate_fraction() {
        let mut io = IoUi::default();
        let eff = apply_io_msg(&mut io, progress_msg("正在导出", 0, None, 1));
        assert_eq!(io.state, IoState::Running);
        assert!(io.fraction.is_none(), "不用假百分比");
        assert!(io.progress.contains("正在导出"), "{}", io.progress);
        assert!(io.snapshot.is_some());
        assert!(eff.is_none(), "进度消息不产生 app 级副作用");
    }

    #[test]
    fn progress_snapshot_is_monotonic_and_deterministic() {
        use crate::index_mgr::{progress_label, progress_short_label, ProgressSnapshot};
        let mut io = IoUi::default();
        // 总数一旦得出 → 确定态 + 百分比。
        let s = ProgressSnapshot {
            stage: "复制".into(),
            done: 25,
            total: Some(100),
            current: "/very/long/path/that/should/be/truncated/photo.jpg".into(),
            errors: Vec::new(),
            revision: 3,
        };
        assert_eq!(s.fraction(), Some(0.25));
        assert_eq!(s.percent(), Some(25));
        let label = progress_label(&s);
        assert!(label.contains("已处理 25 / 100"), "{label}");
        assert!(label.contains("/very/long/path/that/should/be/truncated"), "{label}");
        assert!(!label.contains("photo.jpg"), "长路径必须截断到 40 字");
        assert!(!label.contains("ETA") && !label.contains("预计") && !label.contains("剩余"));
        assert!(progress_short_label(&s).contains("25 / 100"));
        // 应用后百分比 25；再发一个更旧的 revision（done 更小）不得回退。
        let _ = apply_io_msg(&mut io, Msg::Progress(s.clone()));
        assert_eq!(io.percent, 25);
        let older = ProgressSnapshot { done: 10, revision: 2, ..s };
        let _ = apply_io_msg(&mut io, Msg::Progress(older));
        assert_eq!(io.percent, 25, "百分比单调不回退");
        assert_eq!(io.snapshot.as_ref().map(|x| x.done), Some(25), "乱序旧快照不得覆盖");
        // 不确定态：total=None → fraction None，percent 不变。
        let indeterminate = ProgressSnapshot {
            stage: "校验".into(),
            done: 7,
            total: None,
            revision: 4,
            ..Default::default()
        };
        assert!(indeterminate.fraction().is_none());
        assert!(indeterminate.percent().is_none());
        assert!(progress_label(&indeterminate).contains("已处理 7"));
        let _ = apply_io_msg(&mut io, Msg::Progress(indeterminate));
        assert_eq!(io.percent, 25, "不确定态不得重置/回退百分比");
    }

    #[test]
    fn failure_summary_is_bounded_and_contains_no_eta() {
        use crate::index_mgr::failure_summary;
        assert!(failure_summary(&[]).is_none());
        let errors: Vec<String> = (0..60).map(|i| format!("错误 {i}")).collect();
        let s = failure_summary(&errors).expect("有错误就有摘要");
        assert!(s.contains("1. 错误 0"), "{s}");
        assert!(s.contains("50. 错误 49"), "{s}");
        assert!(s.contains("另有 10 项"), "{s}");
        assert!(!s.contains("错误 50"), "第 51 条起不逐条列出");
        assert!(!s.contains("ETA") && !s.contains("预计"));
    }

    #[test]
    fn apply_cancelled_sets_state_and_rollback_message() {
        let mut io = IoUi::default();
        let eff = apply_io_msg(&mut io, Msg::Cancelled);
        assert_eq!(io.state, IoState::Cancelled);
        assert!(io.message.contains("整库回滚"), "{}", io.message);
        assert!(eff.is_none());
    }

    #[test]
    fn apply_import_success_carries_effects_and_switches_step() {
        let mut io = IoUi::default();
        let done = Done {
            state: IoState::Success,
            message: "导入完成 · 成功 3 · 跳过 0 · 失败 0 · 冲突 0".into(),
            export_path: None,
            report: Some(ImportReport {
                imported: 3,
                ..Default::default()
            }),
            backup: None,
            effects: TaskEffects {
                reload_library: true,
                sidebar_groups_dirty: true,
                exif_pending: true,
                toast: Some(("✓ 导入完成".into(), 3.0)),
                ..Default::default()
            },
        };
        let eff = apply_io_msg(&mut io, done_msg(done)).expect("终态应返回副作用");
        assert_eq!(io.state, IoState::Success);
        assert_eq!(io.import_step, ImportStep::Result);
        assert!(io.report.is_some());
        assert_eq!(io.last_result, io.message);
        assert!(eff.reload_library && eff.sidebar_groups_dirty && eff.exif_pending);
        assert!(eff.toast.is_some());
    }

    #[test]
    fn apply_failure_has_no_side_effects() {
        let mut io = IoUi::default();
        let eff = apply_io_msg(
            &mut io,
            done_msg(Done::failure("导出失败：磁盘已满（现有索引未改动）")),
        )
        .expect("终态");
        assert_eq!(io.state, IoState::Failed);
        assert!(io.report.is_none());
        assert_eq!(eff, TaskEffects::default());
        // IDX-06：失败绝不能被当成成功备份。
        assert!(io.last_backup.is_none());
    }

    #[test]
    fn apply_export_success_carries_backup_and_persist_json() {
        let mut io = IoUi::default();
        let backup = BackupInfo {
            path: "/Users/me/Backups/openpicasa.opix".into(),
            created_at: 1_700_000_000,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            timezone: "Asia/Shanghai".into(),
        };
        let done = Done {
            state: IoState::Success,
            message: "导出成功".into(),
            export_path: Some(backup.path.clone()),
            report: None,
            backup: Some(backup.clone()),
            effects: TaskEffects {
                toast: Some(("✓ 导出成功".into(), 3.0)),
                last_backup_json: Some(backup.to_json()),
                ..Default::default()
            },
        };
        let eff = apply_io_msg(&mut io, done_msg(done)).expect("终态应返回副作用");
        assert_eq!(io.last_backup, Some(backup.clone()));
        let json = eff.last_backup_json.expect("成功后应带持久化 JSON");
        // 模拟重启：从 settings JSON 读回必须逐字段一致。
        assert_eq!(BackupInfo::from_json(&json), Some(backup));
    }
}
