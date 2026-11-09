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

use crate::index_mgr::{ImportStep, IoState, IoUi, human_bytes};
use picasa_db::{Db, ImportReport, ImportStrategy, execute_import, export_index};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

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
            effects: TaskEffects::default(),
        }
    }
}

/// 后台线程 → UI 帧的消息。
#[derive(Clone, Debug)]
pub enum Msg {
    /// 进度更新；`fraction = None` 表示不确定态（进度条循环动画），**不用假百分比**。
    Progress { fraction: Option<f32>, text: String },
    /// 终态成功 / 失败。
    Done(Done),
    /// 取消（整库回滚、无残留 staging 文件 / 连接）。
    Cancelled,
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
        Msg::Progress { fraction, text } => {
            io.state = IoState::Running;
            io.progress = text;
            io.fraction = fraction;
            None
        }
        Msg::Cancelled => {
            io.state = IoState::Cancelled;
            io.message = cancel_message();
            io.progress.clear();
            io.fraction = None;
            None
        }
        Msg::Done(d) => {
            io.state = d.state;
            io.message = d.message.clone();
            io.progress.clear();
            io.fraction = None;
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
}

impl ImportTask {
    /// 启动**导出**后台线程（`db_path` 来自 `AppPaths::db_file()`）。
    pub fn start_export(db_path: PathBuf, target: PathBuf, with_thumbs: bool) -> Self {
        Self::spawn(move |cancel, tx| run_export(db_path, target, with_thumbs, cancel, tx))
    }

    /// 启动**导入**后台线程（`db_path` 来自 `AppPaths::db_file()`）。
    pub fn start_import(
        db_path: PathBuf,
        src: PathBuf,
        work_dir: PathBuf,
        strategy: ImportStrategy,
    ) -> Self {
        Self::spawn(move |cancel, tx| {
            run_import(db_path, src, work_dir, strategy, cancel, tx)
        })
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
        }
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
                if matches!(msg, Msg::Done(_) | Msg::Cancelled) {
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
                Some(Msg::Done(Done::failure(format!(
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

fn run_export(
    db_path: PathBuf,
    target: PathBuf,
    with_thumbs: bool,
    cancel: Arc<AtomicBool>,
    tx: Sender<Msg>,
) {
    let _ = tx.send(Msg::Progress {
        fraction: None,
        text: progress_text("正在导出索引文件"),
    });
    // 关键点检查取消（导出在关键点检查即可，docs/16 §3）。
    if cancel.load(Ordering::SeqCst) {
        let _ = tx.send(Msg::Cancelled);
        return;
    }
    // 关键：自己 open 独立连接（Db 不是 Send，不能搬主线程的）。
    let db = match Db::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            let _ = tx.send(Msg::Done(Done::failure(format!(
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
            let _ = tx.send(Msg::Done(Done {
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
                effects: TaskEffects {
                    toast: Some(("✓ 导出成功".into(), 3.0)),
                    ..Default::default()
                },
            }));
        }
        Err(e) => {
            let msg = e.to_string();
            if classify_failure(&msg, cancel.load(Ordering::SeqCst)) == IoState::Cancelled {
                let _ = tx.send(Msg::Cancelled);
            } else {
                let _ = tx.send(Msg::Done(Done::failure(format!(
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
    let _ = tx.send(Msg::Progress {
        fraction: None,
        text: progress_text("正在校验索引文件并准备导入"),
    });
    if cancel.load(Ordering::SeqCst) {
        let _ = tx.send(Msg::Cancelled);
        return;
    }
    // 关键：自己 open 独立连接，execute_import 在该连接上跑单事务 + ATTACH。
    let mut db = match Db::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            let _ = tx.send(Msg::Done(Done::failure(format!(
                "无法打开索引库：{e}（现有索引未改动）"
            ))));
            return;
        }
    };
    if cancel.load(Ordering::SeqCst) {
        let _ = tx.send(Msg::Cancelled);
        return;
    }
    let _ = tx.send(Msg::Progress {
        fraction: None,
        text: progress_text("正在写入整理关系（单事务）"),
    });
    let result = execute_import(&mut db, &src, &work_dir, strategy, cancel.as_ref());
    drop(db); // 独立连接 + ATTACH 一并释放
    match result {
        Ok(rep) => {
            let _ = tx.send(Msg::Done(Done {
                state: IoState::Success,
                message: format!(
                    "导入完成 · 成功 {} · 跳过 {} · 失败 {} · 冲突 {}",
                    rep.imported, rep.skipped, rep.failed, rep.conflicts
                ),
                export_path: None,
                report: Some(rep),
                effects: TaskEffects {
                    reload_library: true,
                    sidebar_groups_dirty: true,
                    exif_pending: true,
                    toast: Some(("✓ 导入完成".into(), 3.0)),
                },
            }));
        }
        Err(e) => {
            let msg = e.to_string();
            if classify_failure(&msg, cancel.load(Ordering::SeqCst)) == IoState::Cancelled {
                let _ = tx.send(Msg::Cancelled);
            } else {
                let _ = tx.send(Msg::Done(Done::failure(format!(
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
        let eff = apply_io_msg(
            &mut io,
            Msg::Progress {
                fraction: None,
                text: progress_text("正在导出"),
            },
        );
        assert_eq!(io.state, IoState::Running);
        assert!(io.fraction.is_none(), "不用假百分比");
        assert!(io.progress.contains("后台执行"));
        assert!(eff.is_none(), "进度消息不产生 app 级副作用");
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
            effects: TaskEffects {
                reload_library: true,
                sidebar_groups_dirty: true,
                exif_pending: true,
                toast: Some(("✓ 导入完成".into(), 3.0)),
            },
        };
        let eff = apply_io_msg(&mut io, Msg::Done(done)).expect("终态应返回副作用");
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
            Msg::Done(Done::failure("导出失败：磁盘已满（现有索引未改动）")),
        )
        .expect("终态");
        assert_eq!(io.state, IoState::Failed);
        assert!(io.report.is_none());
        assert_eq!(eff, TaskEffects::default());
    }
}
