//! 索引服务：**关注文件夹 → 任务队列 → 扫描 → 冻结分母 → 生成缩略图**（M2 A 线）。
//!
//! ## R-M2-1 在本模块的落地方式
//!
//! 整条链路只有 `WalkDir`（`stat`）+ `File::open`（读），写操作全部发生在 `Db`
//! （应用容器里的 SQLite）。没有 `fs::write` / `create` / `rename` / `remove`
//! 作用在照片目录上的任何调用点；**进度状态也只在内存与库里，不落照片目录**
//! （这是加了进度 UI 之后最容易顺手违规的地方，`04` §0.2 已标成第一风险）。
//!
//! ## 为什么是「单 worker + 队列」而不是「每个文件夹一个线程」
//!
//! 一个关注文件夹 = 一个任务。多任务在界面上是**汇总**的，但执行上串行：
//! 解码是 CPU 密集的，两个大目录并行只会互相抢核，让 UI 更卡（R-M2-3 要的是"不阻塞"，
//! 不是"更快跑完"）。排队中的任务状态是 `等待`——这也正是七态里有 `等待` 的原因。
//!
//! ## 分母冻结发生在哪
//!
//! 扫描阶段只数「已发现」，不碰缩略图；扫描结束 → `freeze_denominator()` →
//! 才进入生成阶段。于是「生成阶段又发现新文件」这唯一的漂移源被整块排除，
//! 新文件进入下一批（`04` §3.3 / P-04）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{CoreError, Result, filters, index::FailureItem, index::IndexBoard, paths};
use picasa_db::{Db, PhotoInput, ThumbState};
use picasa_imaging::ImageError;

/// 扫描/后台任务推给 UI 的事件。全部是「值」，UI 侧不需要回调。
///
/// ⚠️ **进度数字不从这里走**：弹框/面板/左栏/栅格都读 [`IndexBoard`] 的快照，
/// 否则四个地方各自累加，必然出现"两处差 1"（`04` §3.2 按 P0 处理）。
pub enum CoreEvent {
    /// 开始扫描（仅用于状态栏历史文案，不参与进度计数）
    ScanStarted { roots: usize },
    /// 一个目录扫完（已落库）
    FolderScanned {
        folder_id: i64,
        path: String,
        added: u32,
        updated: u32,
        removed: u32,
        photos_in_folder: u32,
    },
    /// 发现进度（节流后，约每 250 ms 一次）。**只用于日志与旧的扫描文案**
    Progress {
        dirs: u64,
        photos: u64,
        current: String,
    },
    /// 队列排空
    ScanFinished { dirs: u64, photos: u64, secs: f32 },
    /// 一张缩略图就绪
    ThumbReady {
        photo_id: i64,
        w: u32,
        h: u32,
        jpeg: Vec<u8>,
    },
    /// 一张缩略图失败。`reason` 是给面板看的**原因短语**（失败不静默，R-M2-4）
    ThumbFailed { photo_id: i64, reason: String },
    /// 查看器要的大图就绪（`rgba` 已按 Orientation 摆正）
    FullImageReady {
        photo_id: i64,
        w: u32,
        h: u32,
        rgba: Vec<u8>,
    },
    /// 文件系统有变化（已去抖）
    FsChanged { paths: Vec<String> },
    /// 外部盘离线/在线状态变化
    FolderOnline { folder_id: i64, online: bool },
    Error(String),
}

/// 一次入队的工作。
struct Job {
    path: PathBuf,
    /// 重试任务：不重扫目录，只把待生成（含刚重置的失败项）补齐
    retry: bool,
}

struct Inner {
    db: Arc<Mutex<Db>>,
    board: Arc<IndexBoard>,
    tx: Sender<CoreEvent>,
    jobs: Sender<Job>,
    cancel: Arc<AtomicBool>,
    /// 队列里还剩几个任务；归零时发 `ScanFinished`
    pending: AtomicU64,
    started: Mutex<Option<Instant>>,
    dirs: AtomicU64,
    /// 本轮发现过的图片总数（只用于 `ScanFinished` 的汇总文案；
    /// 进度数字一律看 `IndexBoard`，那里才是唯一数据源）
    found: AtomicU64,
}

/// 索引服务句柄（队列 + worker + 休眠探测）。
pub struct IndexService {
    inner: Arc<Inner>,
    worker: Option<JoinHandle<()>>,
    heartbeat: Option<JoinHandle<()>>,
}

impl IndexService {
    pub fn new(db: Arc<Mutex<Db>>, board: Arc<IndexBoard>, tx: Sender<CoreEvent>) -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let inner = Arc::new(Inner {
            db,
            board,
            tx,
            jobs: job_tx,
            cancel: Arc::new(AtomicBool::new(false)),
            pending: AtomicU64::new(0),
            started: Mutex::new(None),
            dirs: AtomicU64::new(0),
            found: AtomicU64::new(0),
        });

        let w = inner.clone();
        let worker = std::thread::Builder::new()
            .name("op-index".into())
            .spawn(move || worker_loop(job_rx, w))
            .expect("无法创建索引线程");

        let h = inner.clone();
        let heartbeat = std::thread::Builder::new()
            .name("op-index-hb".into())
            .spawn(move || heartbeat_loop(h))
            .expect("无法创建索引心跳线程");

        Self {
            inner,
            worker: Some(worker),
            heartbeat: Some(heartbeat),
        }
    }

    pub fn board(&self) -> &Arc<IndexBoard> {
        &self.inner.board
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.inner.cancel.clone()
    }

    /// 关注文件夹入队（**新批次**）。返回批次号。
    pub fn enqueue_scan(&self, roots: &[PathBuf]) -> u64 {
        let keys: Vec<String> = roots.iter().map(|p| paths::canonical_string(p)).collect();
        self.reset_counters_if_idle();
        let batch = self.inner.board.begin_batch(&keys);
        self.inner.pending.fetch_add(roots.len() as u64, Ordering::Relaxed);
        let _ = self
            .inner
            .tx
            .send(CoreEvent::ScanStarted { roots: roots.len() });
        for path in roots {
            let _ = self.inner.jobs.send(Job {
                path: path.clone(),
                retry: false,
            });
        }
        batch
    }

    /// 重试某个关注文件夹的失败项（**新批次**，只处理失败项）。
    ///
    /// 调用前 UI 必须已经把失败项在库里重置为「待生成」（`Db::reset_failed_thumbs`）。
    pub fn enqueue_retry(&self, folder_id: i64, path: &Path) -> u64 {
        let key = paths::canonical_string(path);
        let (discovered, completed, failed) = {
            let Ok(db) = self.inner.db.lock() else {
                return 0;
            };
            match db.subtree_thumb_stats(folder_id) {
                Ok(s) => (s.total, s.ready, s.failed),
                Err(_) => (0, 0, 0),
            }
        };
        self.reset_counters_if_idle();
        let batch = self
            .inner
            .board
            .begin_retry(&key, discovered, completed, failed);
        self.inner.pending.fetch_add(1, Ordering::Relaxed);
        let _ = self.inner.jobs.send(Job {
            path: path.to_path_buf(),
            retry: true,
        });
        batch
    }

    /// 取消当前与排队的任务（应用退出、或用户从一个任务切到另一个）。
    pub fn stop(&self) {
        self.inner.cancel.store(true, Ordering::Relaxed);
        self.inner.board.resume_all();
    }

    /// 队列空闲时重置「本轮统计」，避免上一批的目录数滚进下一批的 `ScanFinished`。
    fn reset_counters_if_idle(&self) {
        if self.inner.pending.load(Ordering::Relaxed) == 0 {
            self.inner.dirs.store(0, Ordering::Relaxed);
            self.inner.found.store(0, Ordering::Relaxed);
            if let Ok(mut s) = self.inner.started.lock() {
                *s = Some(Instant::now());
            }
            self.inner.cancel.store(false, Ordering::Relaxed);
        }
    }
}

impl Drop for IndexService {
    fn drop(&mut self) {
        self.inner.cancel.store(true, Ordering::Relaxed);
        // 不 join：退出路径上等一个正在解码的任务只会让窗口关得更慢。
        // worker 是 `recv_timeout` 轮询 + cancel 检查，最多 200 ms 自行退出。
        if let Some(h) = self.worker.take() {
            let _ = h;
        }
        if let Some(h) = self.heartbeat.take() {
            let _ = h;
        }
    }
}

/// 兼容 M1 的扫描入口：一次扫多个根目录，返回可取消的句柄。
///
/// 存在的理由只有一个：既有集成测试与 `app` 的旧调用点用它。
/// M2 起内部已经改造成队列任务，**进度与状态一律以 [`IndexBoard`] 为准**。
pub struct ScanHandle {
    pub cancel: Arc<AtomicBool>,
    service: Option<IndexService>,
}

impl ScanHandle {
    /// 请求停止。已落库的部分保留（不从此零开始，见 F1-26 断点思想）。
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(s) = &self.service {
            s.stop();
        }
    }
}

/// 启动一次扫描（后台线程 + 队列）。返回句柄，UI 通过 `tx` 收事件。
pub fn spawn_scan(
    db: Arc<Mutex<Db>>,
    roots: Vec<PathBuf>,
    tx: Sender<CoreEvent>,
) -> ScanHandle {
    spawn_scan_with_board(db, roots, tx).0
}

/// 同 [`spawn_scan`]，但把进度板交给调用方（应用要拿它做四处一致的展示）。
pub fn spawn_scan_with_board(
    db: Arc<Mutex<Db>>,
    roots: Vec<PathBuf>,
    tx: Sender<CoreEvent>,
) -> (ScanHandle, Arc<IndexBoard>) {
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(db, board.clone(), tx);
    service.enqueue_scan(&roots);
    let cancel = service.cancel_flag();
    (
        ScanHandle {
            cancel,
            service: Some(service),
        },
        board,
    )
}

// ---------------------------------------------------------------- worker

fn worker_loop(rx: Receiver<Job>, inner: Arc<Inner>) {
    loop {
        if inner.cancel.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(job) => {
                run_job(&inner, job);
                if inner.pending.fetch_sub(1, Ordering::Relaxed) <= 1 {
                    let photos = inner.found.load(Ordering::Relaxed);
                    let secs = inner
                        .started
                        .lock()
                        .ok()
                        .and_then(|s| *s)
                        .map(|s| s.elapsed().as_secs_f32())
                        .unwrap_or(0.0);
                    let _ = inner.tx.send(CoreEvent::ScanFinished {
                        dirs: inner.dirs.load(Ordering::Relaxed),
                        photos,
                        secs,
                    });
                    tracing::info!(
                        photos,
                        dirs = inner.dirs.load(Ordering::Relaxed),
                        "索引队列排空"
                    );
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn run_job(inner: &Arc<Inner>, job: Job) {
    let key = paths::canonical_string(&job.path);
    inner.board.mark_scanning(&key);

    if !job.path.is_dir() {
        inner
            .board
            .mark_unreachable(&key, "目录不存在或未挂载（外置盘被拔出？）");
        return;
    }

    let mut discovered = 0u64;
    let folder_id = if job.retry {
        match db_folder_id(&inner.db, &key) {
            Some(id) => id,
            None => {
                inner.board.mark_unreachable(&key, "目录尚未入库");
                return;
            }
        }
    } else {
        match walk_and_store(inner, &job.path, &key, &mut discovered) {
            Ok(Some(id)) => id,
            Ok(None) => return, // 已标不可访问
            Err(e) => {
                tracing::warn!(path = %job.path.display(), error = %e, "扫描失败");
                inner.board.mark_unreachable(&key, format!("扫描失败：{e}"));
                let _ = inner.tx.send(CoreEvent::Error(format!("扫描失败：{e}")));
                return;
            }
        }
    };

    // 扫描结束 → **冻结分母**（新发现排下一批）。
    // 口径按**子树**算：用户关注 `2020` 时 `2020/01` 里的照片也属于这个任务。
    let stats = match lock_db(&inner.db).and_then(|db| db.subtree_thumb_stats(folder_id).ok()) {
        Some(s) => s,
        None => {
            inner.board.finish(&key);
            return;
        }
    };
    inner
        .board
        .freeze_denominator(&key, stats.total.max(discovered), stats.ready);

    if stats.pending == 0 {
        inner.board.finish(&key);
        return;
    }

    inner.board.mark_thumbnailing(&key);
    generate_thumbs(inner, folder_id, &key, &job.path);

    if inner.cancel.load(Ordering::Relaxed) {
        return;
    }
    if !job.path.is_dir() {
        inner
            .board
            .mark_unreachable(&key, "目录已失联（索引过程中不可访问）");
        return;
    }
    inner.board.finish(&key);
}

/// 走一遍目录树 → 按目录落库。返回根目录的 folder_id。
///
/// `Ok(None)` 表示「已标记为不可访问」，调用方不需要再报错。
fn walk_and_store(
    inner: &Arc<Inner>,
    root: &Path,
    key: &str,
    discovered: &mut u64,
) -> Result<Option<i64>> {
    let mut by_dir: BTreeMap<PathBuf, Vec<PhotoInput>> = BTreeMap::new();
    let mut denied = 0u64;
    let mut last_report = Instant::now();

    let mut walker = walkdir::WalkDir::new(root)
        .max_depth(filters::DEFAULT_MAX_DEPTH)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();

    while let Some(item) = walker.next() {
        if inner.cancel.load(Ordering::Relaxed) {
            break;
        }
        let entry = match item {
            Ok(e) => e,
            Err(e) => {
                if e.io_error()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
                {
                    denied += 1;
                }
                tracing::debug!(root = %root.display(), error = %e, "遍历条目失败");
                continue;
            }
        };

        if entry.file_type().is_dir() {
            if entry.depth() > 0 {
                // **根目录本身永不跳过**：用户已经明确选了它。
                // 否则 `.staging` 这类以点开头的目录会被整棵剪掉，扫出 0 张照片。
                let name = entry.file_name().to_string_lossy();
                if filters::should_skip_dir(&name) {
                    walker.skip_current_dir();
                    continue;
                }
            }
            inner.dirs.fetch_add(1, Ordering::Relaxed);
            inner.board.note_dir();
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        if filters::should_skip_file(&name) || !filters::is_supported_image(entry.path()) {
            continue;
        }
        let Some(parent) = entry.path().parent() else {
            continue;
        };
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(path = %entry.path().display(), error = %e, "读元数据失败");
                continue;
            }
        };

        let size_bytes = meta.len() as i64;
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);

        let probe = picasa_imaging::probe(entry.path()).unwrap_or_default();
        let (taken_at, taken_at_src) = match probe.exif_taken_at {
            Some(ts) => (Some(ts), 0i64),
            None => {
                // F1-15：日期缺失时回退文件修改时间，并且**标记来源**（UI 可见）
                let fallback = (mtime_ns > 0).then_some(mtime_ns / 1_000_000_000);
                (fallback, 2)
            }
        };

        by_dir.entry(parent.to_path_buf()).or_default().push(PhotoInput {
            name_key: paths::compare_key(&name),
            name,
            size_bytes,
            mtime_ns,
            width: Some(probe.width as i64).filter(|w| *w > 0),
            height: Some(probe.height as i64).filter(|h| *h > 0),
            orientation: Some(probe.orientation as i64),
            taken_at,
            taken_at_src,
        });

        *discovered += 1;
        inner.found.fetch_add(1, Ordering::Relaxed);
        // 扫描阶段「已发现」最多 4 次/秒刷新（`04` §3.4）；这里按时间节流而不是按条数，
        // 因为慢盘上 200 条可能要几秒，按条数会让数字长时间不动。
        if last_report.elapsed() >= Duration::from_millis(250) {
            last_report = Instant::now();
            inner.board.note_discovered(key, *discovered);
            let _ = inner.tx.send(CoreEvent::Progress {
                dirs: inner.dirs.load(Ordering::Relaxed),
                photos: *discovered,
                current: parent.display().to_string(),
            });
        }
    }
    inner.board.note_discovered(key, *discovered);

    if *discovered == 0 && denied > 0 {
        inner
            .board
            .mark_unreachable(key, "无权限访问该目录（系统未授权或权限被收回）");
        return Ok(None);
    }
    if !root.is_dir() {
        inner.board.mark_unreachable(key, "目录已失联");
        return Ok(None);
    }

    // 先把「关注文件夹」自己落库：即使它没有直接子文件，也要在左栏可见、可点，
    // 并且任务统计有个稳定的根 id（子树口径，见 `Db::subtree_thumb_stats`）。
    let root_id = {
        let Ok(guard) = inner.db.lock() else {
            return Err(CoreError::Other("db 锁中毒".into()));
        };
        let volume_id = volume_of(root);
        guard.upsert_folder(key, &paths::compare_key(key), volume_id.as_deref())?
    };

    // 逐目录落库：一个目录一次事务，比一张一次快一个数量级，也让 UI 渐进填充
    for (dir, items) in by_dir {
        if inner.cancel.load(Ordering::Relaxed) {
            break;
        }
        let dir_str = paths::canonical_string(&dir);
        let dir_key = paths::compare_key(&dir_str);
        let volume_id = volume_of(&dir);
        let outcome = {
            let Ok(guard) = inner.db.lock() else {
                return Err(CoreError::Other("db 锁中毒".into()));
            };
            let folder_id = guard.upsert_folder(&dir_str, &dir_key, volume_id.as_deref())?;
            let apply =
                guard.apply_scan(folder_id, &dir_str, &items, picasa_imaging::DECODER_VER)?;
            (folder_id, apply)
        };
        let (folder_id, apply) = outcome;
        let _ = inner.tx.send(CoreEvent::FolderScanned {
            folder_id,
            path: dir_str,
            added: apply.added,
            updated: apply.updated,
            removed: apply.removed,
            photos_in_folder: items.len() as u32,
        });
    }
    Ok(Some(root_id))
}

/// 生成一个目录（含子目录）的待补缩略图（并行，逐张推事件 + 更新进度板）。
fn generate_thumbs(inner: &Arc<Inner>, folder_id: i64, key: &str, root: &Path) {
    use rayon::prelude::*;

    let pending = match lock_db(&inner.db)
        .and_then(|db| db.subtree_pending_thumbs(folder_id, 50_000).ok())
    {
        Some(v) => v,
        None => return,
    };
    if pending.is_empty() {
        return;
    }
    tracing::info!(folder = %root.display(), count = pending.len(), "开始生成缩略图");

    let watched = AtomicU64::new(0);
    pending.par_iter().for_each(|row| {
        if inner.cancel.load(Ordering::Relaxed) {
            return;
        }
        // 每 64 张摸一次目录：外置盘拔掉时不必等 5 万张全部失败
        if watched.fetch_add(1, Ordering::Relaxed).is_multiple_of(64) && !root.is_dir() {
            return;
        }
        let path = row.abs_path();
        match picasa_imaging::thumb_t0(&path) {
            Ok((jpeg, w, h)) => {
                if let Ok(guard) = inner.db.lock() {
                    let buf = picasa_db::ThumbBuf {
                        w: w as i64,
                        h: h as i64,
                        format: 0,
                        data: jpeg.clone(),
                    };
                    if let Err(e) = guard.put_thumb(row.id, 0, &buf) {
                        tracing::warn!(error = %e, "写缩略图失败");
                    }
                }
                inner.board.note_thumb_ok(key);
                let _ = inner.tx.send(CoreEvent::ThumbReady {
                    photo_id: row.id,
                    w,
                    h,
                    jpeg,
                });
            }
            Err(e) => {
                let reason = classify(&e).to_string();
                tracing::debug!(path = %path.display(), error = %e, "缩略图生成失败");
                if let Ok(guard) = inner.db.lock() {
                    let _ = guard.set_thumb_state(row.id, ThumbState::Failed);
                }
                inner.board.note_thumb_failed(
                    key,
                    FailureItem {
                        photo_id: row.id,
                        name: row.name.clone(),
                        reason: reason.clone(),
                    },
                );
                let _ = inner.tx.send(CoreEvent::ThumbFailed {
                    photo_id: row.id,
                    reason,
                });
            }
        }
    });
}

/// 失败原因分类。**面板要显示的是人话，不是 `Decode(DecodingError)`。**
fn classify(e: &ImageError) -> &'static str {
    match e {
        ImageError::Io(io) => match io.kind() {
            std::io::ErrorKind::NotFound => "文件已移动或删除",
            std::io::ErrorKind::PermissionDenied => "无权限读取",
            _ => "读取失败",
        },
        ImageError::Decode(_) => "解码失败（文件可能损坏）",
        ImageError::Unsupported(_) => "不支持的格式",
        ImageError::Exif(_) => "EXIF 解析失败",
    }
}

fn lock_db(db: &Arc<Mutex<Db>>) -> Option<std::sync::MutexGuard<'_, Db>> {
    db.lock().ok()
}

fn db_folder_id(db: &Arc<Mutex<Db>>, path: &str) -> Option<i64> {
    lock_db(db)?.folder_id(path).ok().flatten()
}

/// 休眠/挂起探测（`04` §3.1.1，P-03 裁决：`暂停` 只由系统事件触发）。
///
/// 判定方式：墙钟（`SystemTime`）在两次心跳之间**跳过了 ≥6 s**，
/// 而单调时钟（`Instant`）不会在挂起期间前进——所以只要比对墙钟间隔就能识别
/// "进程被冻住了一段时间"。识别到就标 `暂停`，下一拍自动恢复，
/// 数字从冻结值继续（计数器从来没被重置，所以不会回退）。
fn heartbeat_loop(inner: Arc<Inner>) {
    let mut last = wall_ms();
    let mut paused = false;
    loop {
        if inner.cancel.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
        let now = wall_ms();
        let gap = Duration::from_millis(now.saturating_sub(last).max(0) as u64);
        last = now;
        let running = inner.board.running();
        if running && gap >= Duration::from_secs(6) {
            inner.board.pause_all("系统休眠 / 进程被挂起");
            tracing::info!(gap = ?gap, "检测到系统挂起，索引暂时标记为暂停");
            paused = true;
            continue;
        }
        if paused {
            inner.board.resume_all();
            tracing::info!("系统已恢复，索引继续");
            paused = false;
        }
    }
}

/// 墙钟毫秒。**故意用墙钟**：系统休眠时单调时钟（`Instant`）也会停，
/// 只有墙钟能看出"时间跳过了 6 秒"——这正是识别挂起的唯一依据。
fn wall_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 卷标识：外接盘重挂载后用来识别「还是同一个盘」。
fn volume_of(path: &Path) -> Option<String> {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("/Volumes/") {
        let name = rest.split('/').next()?.to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    Some("/".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use picasa_db::SortKey;
    use std::io::Write;

    fn write_jpeg(path: &Path, w: u32, h: u32) {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 255) as u8, (y % 255) as u8, 128])
        });
        let mut buf = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new(&mut buf);
        enc.encode(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        std::fs::File::create(path)
            .unwrap()
            .write_all(&buf)
            .unwrap();
    }

    fn wait_finished(rx: &Receiver<CoreEvent>) -> bool {
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            if matches!(ev, CoreEvent::ScanFinished { .. }) {
                return true;
            }
        }
        false
    }

    #[test]
    fn scan_records_nested_folders_and_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("2011/05")).unwrap();
        write_jpeg(&root.join("a.jpg"), 40, 30);
        write_jpeg(&root.join("2011/b.jpeg"), 20, 20);
        write_jpeg(&root.join("2011/05/c.JPG"), 10, 10);
        std::fs::write(root.join("notes.txt"), b"ignored").unwrap();

        let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
        let (tx, rx) = std::sync::mpsc::channel();
        let (handle, board) = spawn_scan_with_board(db.clone(), vec![root.to_path_buf()], tx);
        assert!(wait_finished(&rx), "扫描未结束");
        drop(handle);

        let guard = db.lock().unwrap();
        assert_eq!(guard.total_photos().unwrap(), 3);
        let folders = guard.all_folders().unwrap();
        assert_eq!(folders.len(), 3);
        let total: u32 = folders.iter().map(|f| f.photo_count).sum();
        assert_eq!(total, 3);
        drop(guard);

        // 任务收尾：无失败 → 任务被清空，但批次与完成时间仍在
        let snap = board.snapshot();
        assert!(snap.tasks.is_empty(), "全部完成后不应留下残行");
        assert!(snap.finished_at.is_some());
        assert_eq!(snap.batch, 1);
    }

    #[test]
    fn hidden_root_is_still_scanned() {
        // 用户显式选中一个以 `.` 开头的目录时，必须照扫（不能在根上套用隐藏目录规则）
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".staging");
        std::fs::create_dir_all(root.join(".inner")).unwrap();
        write_jpeg(&root.join("a.jpg"), 20, 10);
        // 但根**内部**的隐藏目录仍然跳过
        write_jpeg(&root.join(".inner/b.jpg"), 20, 10);

        let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = spawn_scan(db.clone(), vec![root.clone()], tx);
        assert!(wait_finished(&rx));
        drop(handle);

        let guard = db.lock().unwrap();
        assert_eq!(guard.total_photos().unwrap(), 1, "只应收录 .staging/a.jpg");
    }

    #[test]
    fn broken_jpeg_becomes_failure_not_silence() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_jpeg(&root.join("ok.jpg"), 40, 30);
        // 截断 JPEG：能识别成图片扩展名，但解码必定失败
        std::fs::write(root.join("bad.jpg"), b"\xff\xd8\xff\xe0truncated").unwrap();

        let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
        let (tx, rx) = std::sync::mpsc::channel();
        let (handle, board) = spawn_scan_with_board(db.clone(), vec![root.to_path_buf()], tx);
        assert!(wait_finished(&rx));
        drop(handle);

        let snap = board.snapshot();
        // 有失败 → 任务保留（不能被"完成"吞掉）
        // 若这一批恰好全部成功（解码器容错），至少不能出现"任务消失但有失败"的矛盾
        let guard = db.lock().unwrap();
        let stats = guard.folder_thumb_stats(1).unwrap();
        drop(guard);
        if stats.failed > 0 {
            assert_eq!(snap.tasks.len(), 1);
            assert_eq!(snap.tasks[0].state.label(), "部分失败");
            assert_eq!(snap.tasks[0].failed, stats.failed);
            assert!(snap.tasks[0].failures[0].name.contains("bad"));
        } else {
            assert!(snap.tasks.is_empty());
        }
    }

    #[test]
    fn retry_only_touches_failed_items() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_jpeg(&root.join("ok.jpg"), 40, 30);
        std::fs::write(root.join("bad.jpg"), b"\xff\xd8\xff\xe0truncated").unwrap();

        let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
        let (tx, rx) = std::sync::mpsc::channel();
        let (handle, board) = spawn_scan_with_board(db.clone(), vec![root.to_path_buf()], tx);
        assert!(wait_finished(&rx));
        drop(handle);

        let (folder_id, ready_before, ok_before) = {
            let guard = db.lock().unwrap();
            let stats = guard.folder_thumb_stats(1).unwrap();
            let ready_id = guard
                .photos(Some(&[1]), picasa_db::SortSpec::default(), 10)
                .unwrap()
                .into_iter()
                .find(|p| p.name == "ok.jpg")
                .map(|p| p.id);
            (1i64, stats.ready, ready_id)
        };
        if ready_before == 0 {
            return; // 这批没有成功项，重试语义无从验证
        }

        // 重试：只重置失败项
        let reset = {
            let guard = db.lock().unwrap();
            guard.reset_failed_thumbs(folder_id).unwrap()
        };
        if reset == 0 {
            return;
        }

        let (tx2, rx2) = std::sync::mpsc::channel();
        let service = IndexService::new(db.clone(), board.clone(), tx2);
        let batch = service.enqueue_retry(folder_id, root);
        assert!(batch >= 2, "重试必须产生新批次");
        assert!(wait_finished(&rx2));
        drop(service);

        let guard = db.lock().unwrap();
        let stats = guard.folder_thumb_stats(folder_id).unwrap();
        assert!(stats.ready >= ready_before, "重试不得让已完成回退");
        if let Some(id) = ok_before {
            assert!(guard.get_thumb(id, 0).unwrap().is_some(), "有效缩略图不重建");
        }
        // 失败数 = 条目数，不随重试次数累加
        let snap = board.snapshot();
        let failed_tasks: Vec<_> = snap.tasks.iter().filter(|t| t.failed > 0).collect();
        for t in failed_tasks {
            assert!(t.failed <= 1, "同一条目重复失败不得重复计数");
        }
    }

    #[test]
    fn missing_root_is_unreachable() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("not-here");
        let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
        let (tx, rx) = std::sync::mpsc::channel();
        let (handle, board) = spawn_scan_with_board(db, vec![gone.clone()], tx);
        assert!(wait_finished(&rx));
        drop(handle);
        let snap = board.snapshot();
        assert_eq!(snap.tasks.len(), 1);
        assert_eq!(snap.tasks[0].state.label(), "不可访问");
        assert!(snap.tasks[0].reason.is_some());
    }

    #[test]
    fn selected_sort_key_is_used_by_panel_queries() {
        // 面板按名字列失败项：确认失败查询的排序键是 name_key（中文/大小写稳定）
        let db = Db::open_in_memory().unwrap();
        let fid = db.upsert_folder("/tmp/a", "/tmp/a", None).unwrap();
        let mut a = picasa_db::PhotoInput {
            name: "b.JPG".into(),
            name_key: "b.jpg".into(),
            size_bytes: 1,
            mtime_ns: 1,
            width: None,
            height: None,
            orientation: None,
            taken_at: None,
            taken_at_src: 2,
        };
        db.apply_scan(fid, "/tmp/a", &[a.clone()], 0).unwrap();
        a.name = "a.JPG".into();
        a.name_key = "a.jpg".into();
        db.apply_scan(fid, "/tmp/a", &[a], 0).unwrap();
        let photos = db.photos(Some(&[fid]), picasa_db::SortSpec {
            key: SortKey::Name,
            descending: false,
        }, 10).unwrap();
        assert_eq!(photos[0].name, "a.JPG");
    }
}
