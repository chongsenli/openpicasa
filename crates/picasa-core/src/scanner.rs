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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{CoreError, Result, filters, index::FailureItem, index::IndexBoard, paths};
use picasa_db::{Db, PhotoInput, PhotoRow, ThumbState};
use picasa_imaging::ImageError;

/// 扫描/后台任务推给 UI 的事件。全部是「值」，UI 侧不需要回调。
///
/// ⚠️ **进度数字不从这里走**：弹框/面板/左栏/栅格都读 [`IndexBoard`] 的快照，
/// 否则四个地方各自累加，必然出现"两处差 1"（`04` §3.2 按 P0 处理）。
pub enum CoreEvent {
    /// 开始扫描（仅用于状态栏历史文案，不参与进度计数）
    ScanStarted {
        roots: usize,
    },
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
    ScanFinished {
        dirs: u64,
        photos: u64,
        secs: f32,
    },
    /// 一张缩略图就绪
    ThumbReady {
        photo_id: i64,
        w: u32,
        h: u32,
        jpeg: Vec<u8>,
    },
    /// 一张缩略图失败。`reason` 是给面板看的**原因短语**（失败不静默，R-M2-4）
    ThumbFailed {
        photo_id: i64,
        reason: String,
    },
    /// 查看器要的大图就绪（`rgba` 已按 Orientation 摆正）
    FullImageReady {
        photo_id: i64,
        w: u32,
        h: u32,
        rgba: Vec<u8>,
    },
    /// 文件系统有变化（已去抖）
    FsChanged {
        paths: Vec<String>,
    },
    /// 外部盘离线/在线状态变化
    FolderOnline {
        folder_id: i64,
        online: bool,
    },
    /// 索引数据已被删除（`IndexService::delete_index`）。
    ///
    /// 单独一条事件而不是复用 `FolderScanned`：界面收到它必须**整体重置**
    /// （照片列表、目录树、进度面板），而不是"某个目录变了"的增量刷新。
    IndexCleared {
        photos: u64,
        thumbs: u64,
    },
    Error(String),
}

/// 一次入队的工作。
struct Job {
    path: PathBuf,
    /// 重试任务：不重扫目录，只把待生成（含刚重置的失败项）补齐
    retry: bool,
    /// 非空 = **点名重建**（「重建失败索引」）：只重做这些 id，不重扫目录。
    ///
    /// 为什么不复用 `retry`：retry 的取数口径是「本子树里所有待生成」。
    /// 25.9 万张的库里有 22 万张从未尝试过的欠账，用户点一次「重建失败索引」
    /// 只想修那 970 张坏的，不该顺手把 22 万张一起卷进来变成几小时的满载。
    ids: Vec<i64>,
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
    /// 已在队列里（尚未由 worker 取走）的文件夹路径。
    ///
    /// 「右键刷新」用它做**合并去重**：同一个文件夹连点五次刷新，只入队一次。
    /// worker 取走任务时移出——所以它表达的是「排队中」，不含「正在跑」
    /// （后者看 `IndexBoard` 的 `running()` 状态）。
    queued: Mutex<HashSet<String>>,
    /// 用户按下的「暂停索引」。
    ///
    /// 与 `cancel` 的区别是**可逆**：`cancel` 会让 worker 线程整个退出（只用于停机），
    /// 而 `paused` 只是让worker 停在**目录边界**上等着——已落库的部分保留，
    /// 恢复后从同一位置继续，不需要重扫。
    paused: AtomicBool,
    /// 索引"世代号"。`delete_index` 期间加一，用来让**正在跑的那次扫描**认出
    /// 「我手里的活已经作废了」，从而放弃后续写入。
    ///
    /// 为什么必须有它：删完索引后，扫描线程可能正好走到下一个目录又写回去，
    /// 用户会看到"删了 3 万张，界面又冒出 200 张"。判据放在 `apply_dir` 里、
    /// **在持有 db 锁时比较**，与 `delete_index` 在同一把锁下改号，
    /// 两者互斥，任何交错都不会让旧世代的行落库。
    epoch: AtomicU64,
    /// 正在删除索引（防止连点重复触发）
    deleting: AtomicBool,
    /// 本次删除索引期间，抑制"索引完成"的收尾事件（否则会弹一条误导性的完成提示）
    suppress_finish: AtomicBool,
    /// 缩略图回填**专用**的线程池。
    ///
    /// 为什么不能直接用全局池（`pending.par_iter()` 的默认行为）：全局池的线程数
    /// = 逻辑核数，在 2 核 4 线程的机器上就是 4 个线程同时解 JPEG。
    /// 缩略图是**可以延后**的工作，UI 与合成才是**必须及时**的——
    /// 两边抢同样的核，结果就是界面一帧画不出来，用户说"死机"。
    /// 实测这台机器（i5-8200Y，5W 无风扇）缩略图 4 线程跑满时
    /// `x86_pkg_temp` 从 49°C 升到 74°C，界面完全无响应。
    ///
    /// 所以单独建池、只给一半逻辑核，剩下的一半留给 egui 重绘与合成器。
    thumb_pool: rayon::ThreadPool,
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
        // 一半逻辑核，至少 1 个：见 `Inner::thumb_pool` 的注释。
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let thumb_threads = (cores / 2).max(1);
        let thumb_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(thumb_threads)
            .thread_name(|i| format!("op-thumb-{i}"))
            .build()
            .expect("无法创建缩略图线程池");
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
            queued: Mutex::new(HashSet::new()),
            paused: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            deleting: AtomicBool::new(false),
            suppress_finish: AtomicBool::new(false),
            thumb_pool,
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

    /// 用户按下的「暂停索引」。
    ///
    /// 语义：**停在目录边界，不重扫、不丢进度**。worker 不再取新任务，
    /// 当前任务在下一个目录写完后就停住（最多等一个目录的处理时间）。
    /// 已经落库的照片照常显示、照常浏览——暂停的是"建立索引"，不是图库。
    pub fn pause(&self) {
        self.inner.paused.store(true, Ordering::SeqCst);
        // 显示层跟着走：面板/状态栏的「已暂停」来自 IndexBoard，不是这里自己记
        self.inner.board.pause_all("已手动暂停");
    }

    /// 继续索引：从暂停处接着跑，不需要重扫。
    pub fn resume(&self) {
        self.inner.paused.store(false, Ordering::SeqCst);
        self.inner.board.resume_all();
    }

    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::Relaxed)
    }

    /// 删除索引数据（照片 / 缩略图 / 目录 / 任务记录），保留关注文件夹与排除规则。
    ///
    /// 在**后台线程**执行：删完可能是几十万行 + 需要下一轮 VACUUM 才有意义，
    /// 放在 UI 线程上就是一次可见的卡死。完成后广播 [`CoreEvent::IndexCleared`]。
    ///
    /// 返回 `false` 表示已有一个删除在跑（连点保护）。
    pub fn delete_index(&self) -> bool {
        if self.inner.deleting.swap(true, Ordering::SeqCst) {
            return false;
        }
        let inner = self.inner.clone();
        let spawned = std::thread::Builder::new()
            .name("op-index-clear".into())
            .spawn(move || {
                // ① 先停写：暂停 + 作废当前世代，让在跑的扫描放弃后续目录
                inner.suppress_finish.store(true, Ordering::SeqCst);
                inner.paused.store(true, Ordering::SeqCst);
                inner.epoch.fetch_add(1, Ordering::SeqCst);

                // ② 等手里那一次 `apply_dir` 落完（它要么在锁内看到新世代而放弃，
                //    要么已经写完——两种都由 db 锁串行化，这里只要等到队列安静）
                let deadline = Instant::now() + Duration::from_secs(10);
                while inner.pending.load(Ordering::Relaxed) > 0 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(30));
                }

                // ③ 删。**在同一把锁里再抬一次世代**：`apply_dir` 也是在锁内比世代，
                //    所以此后再没有任何写入能漏进来。
                let outcome: Result<(u64, u64)> = match lock_db(&inner.db) {
                    Some(db) => {
                        inner.epoch.fetch_add(1, Ordering::SeqCst);
                        db.clear_index()
                            .map_err(|e| CoreError::Other(e.to_string()))
                    }
                    None => Err(CoreError::Other("db 锁中毒".into())),
                };

                // ④ 收尾：计数器、排队集合、面板任务全部归零
                inner.dirs.store(0, Ordering::Relaxed);
                inner.found.store(0, Ordering::Relaxed);
                inner.pending.store(0, Ordering::Relaxed);
                if let Ok(mut q) = inner.queued.lock() {
                    q.clear();
                }
                if let Ok(mut s) = inner.started.lock() {
                    *s = None;
                }
                inner.board.forget_all();
                inner.paused.store(false, Ordering::SeqCst);
                inner.suppress_finish.store(false, Ordering::SeqCst);
                inner.deleting.store(false, Ordering::SeqCst);

                match outcome {
                    Ok((photos, thumbs)) => {
                        tracing::info!(photos, thumbs, "索引数据已删除");
                        let _ = inner.tx.send(CoreEvent::IndexCleared { photos, thumbs });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "删除索引失败");
                        let _ = inner
                            .tx
                            .send(CoreEvent::Error(format!("删除索引失败：{e}")));
                    }
                }
            });
        if spawned.is_err() {
            self.inner.deleting.store(false, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// 关注文件夹入队（**新批次**）。返回批次号。
    pub fn enqueue_scan(&self, roots: &[PathBuf]) -> u64 {
        let keys: Vec<String> = roots.iter().map(|p| paths::canonical_string(p)).collect();
        if let Ok(mut q) = self.inner.queued.lock() {
            q.extend(keys.iter().cloned());
        }
        self.reset_counters_if_idle();
        let batch = self.inner.board.begin_batch(&keys);
        self.inner
            .pending
            .fetch_add(roots.len() as u64, Ordering::Relaxed);
        let _ = self
            .inner
            .tx
            .send(CoreEvent::ScanStarted { roots: roots.len() });
        for path in roots {
            let _ = self.inner.jobs.send(Job {
                path: path.clone(),
                retry: false,
                ids: Vec::new(),
            });
        }
        batch
    }

    /// 右键「刷新此文件夹」：重新扫一个目录（**合并去重**）。
    ///
    /// 返回 `false` 表示这次刷新被**合并**了——该文件夹要么还在队列里，
    /// 要么正在被索引。连点五次刷新只会真的跑一次，这是 E 导航的要求。
    ///
    /// 与 `enqueue_scan` 的区别是单个目录、且带去重；批次语义相同。
    pub fn enqueue_refresh(&self, path: &Path) -> bool {
        let key = paths::canonical_string(path);
        // ① 已在队列（尚未取走）→ 合并
        {
            let Ok(mut q) = self.inner.queued.lock() else {
                return false;
            };
            if !q.insert(key.clone()) {
                return false;
            }
        }
        // ② 正在索引同一目录 → 合并（回滚刚才的插入，避免「排队集合」留脏）
        let snap = self.inner.board.snapshot();
        if let Some(t) = snap.task_by_path(&key)
            && t.state.is_running()
        {
            if let Ok(mut q) = self.inner.queued.lock() {
                q.remove(&key);
            }
            return false;
        }
        self.enqueue_scan(&[path.to_path_buf()]);
        true
    }

    /// 重试某个关注文件夹的失败项（**新批次**，只处理失败项）。
    ///
    /// 调用前 UI 必须已经把失败项在库里重置为「待生成」（`Db::reset_failed_thumbs`）。
    pub fn enqueue_retry(&self, folder_id: i64, path: &Path) -> u64 {
        let key = paths::canonical_string(path);
        if let Ok(mut q) = self.inner.queued.lock() {
            q.insert(key.clone());
        }
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
            ids: Vec::new(),
        });
        batch
    }

    /// 「重建失败索引」：把**点名的一批照片**重新解码一遍（新批次）。
    ///
    /// 调用前 UI 必须已经用 `Db::reset_failed_thumbs_by_ids` 把**同一批 id**
    /// 重置为「待生成」——这里不再动状态，只负责把它们补齐。
    ///
    /// `root` 只用于任务在进度面板上的落点（同一关注文件夹下的失败项归到一个任务里），
    /// **不会**被当成扫描起点去重扫目录。
    pub fn enqueue_rebuild_failed(&self, ids: Vec<i64>, root: &Path) -> u64 {
        if ids.is_empty() {
            return 0;
        }
        let key = paths::canonical_string(root);
        if let Ok(mut q) = self.inner.queued.lock() {
            q.insert(key.clone());
        }
        let n = ids.len() as u64;
        self.reset_counters_if_idle();
        // 分母 = 这批的点名数量，起点是「0 张完成、0 张失败」——
        // 它们此刻已经是「待生成」，画成失败会让用户以为按了按钮没效果。
        let batch = self.inner.board.begin_rebuild(&key, n);
        self.inner.pending.fetch_add(1, Ordering::Relaxed);
        let _ = self.inner.jobs.send(Job {
            path: root.to_path_buf(),
            retry: false,
            ids,
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
pub fn spawn_scan(db: Arc<Mutex<Db>>, roots: Vec<PathBuf>, tx: Sender<CoreEvent>) -> ScanHandle {
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

/// 在暂停期间原地等；返回 `true` 表示等的时候被要求停机（调用方应立刻收工）。
///
/// 用 80 ms 轮询而不是条件变量：暂停是**秒级**的人机交互，不是高并发原语，
/// 一个 `Condvar` 换来的启动复杂度（谁负责 notify、取消怎么唤醒）不值得。
/// 采样 80 ms 也保证了「按暂停」到「真的停下」的最大延迟是肉眼可接受的。
fn wait_if_paused(inner: &Inner) -> bool {
    while inner.paused.load(Ordering::Relaxed) {
        if inner.cancel.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    inner.cancel.load(Ordering::Relaxed)
}

/// 暂停期间死等（用于 rayon 的并行闭包里——那里没有"跳过后重来"的语义）。
fn block_while_paused(inner: &Inner) {
    while inner.paused.load(Ordering::Relaxed) && !inner.cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(80));
    }
}

fn worker_loop(rx: Receiver<Job>, inner: Arc<Inner>) {
    loop {
        if inner.cancel.load(Ordering::Relaxed) {
            break;
        }
        // 暂停时不取新任务：队列里的活原地等着，恢复后按原顺序继续。
        if wait_if_paused(&inner) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(job) => {
                // 已取走 → 移出「排队中」集合，「去重」从此改由 `IndexBoard` 的
                // running 状态接管（否则一个长任务期间永远无法再刷新它）。
                let key = paths::canonical_string(&job.path);
                if let Ok(mut q) = inner.queued.lock() {
                    q.remove(&key);
                }
                run_job(&inner, job);
                if inner.pending.fetch_sub(1, Ordering::Relaxed) <= 1 {
                    // 删除索引期间不发"索引完成"：那条提示会让用户以为刚删掉的东西又建好了
                    if !inner.suppress_finish.swap(false, Ordering::SeqCst) {
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

    // ---- 点名重建（「重建失败索引」）：不扫目录，只把点名的这几张重做 ----
    //
    // 放在扫描分支之前：这条路径**不碰目录树**，也就不该受 `walk_and_store`
    // 的任何副作用影响（它会删掉磁盘上已消失的照片、重算层级、广播目录事件）。
    if !job.ids.is_empty() {
        // 用 keep_reason 版本：`begin_rebuild` 写的「重建失败索引」不能被清掉，
        // 否则任务行在跑的时候只剩下通用的「正在生成缩略图」，看不出是用户点的那次。
        inner.board.mark_thumbnailing_keep_reason(&key);
        let epoch = inner.epoch.load(Ordering::SeqCst);
        generate_thumbs_for_ids(inner, &job.ids, &key, &job.path, epoch);
        if !inner.cancel.load(Ordering::Relaxed) {
            inner.board.finish(&key);
        }
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

    // ---- 缩略图：分两条路，别混 ----
    //
    // · **普通扫描**（启动、拖入文件夹）：`walk_and_store` 里已经只为「本次新增 /
    //   缓存键变化」的照片补过图了，历史欠账一张都不碰，扫完即收工。
    //   25.9 万张的库有 22 万张欠账，若在这里补全，一次启动就是几小时，
    //   用户要的「打开就能看」永远做不到。
    // · **显式重试**（用户点了「重试 / 补全」）：把这一子树里所有待补的都补完。
    //   这是用户主动要求的长任务，进度条会一直转，也有暂停按钮。
    if job.retry && stats.pending > 0 {
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
    }
    inner.board.finish(&key);
}

/// 只枚举目录（不读文件内容），按「父先于子」的顺序返回。
///
/// ## 为什么要拆成两遍
///
/// `apply_scan` 把传入的 `items` 当作**该目录的完整快照**——磁盘上没有的会被判为
/// `removed` 并从库里删掉。而 `walkdir` 的顺序里，同一个目录的直接文件会被子目录
/// 的递归**打断**：`/a/1.jpg`、`/a/b_sub/x.jpg`、`/a/2.jpg` 中 `1.jpg` 与 `2.jpg`
/// 不连续。若边遍历边落库，先落 `[1.jpg]` 就会把 `2.jpg` 当成「已删除」。
///
/// 先枚举目录（只 `read_dir` 判 `is_dir`，不 stat 文件、不读 EXIF，很快），
/// 再对每个目录单独列直接文件，落库时快照一定是完整的。
fn collect_dirs(inner: &Arc<Inner>, root: &Path, exclusion: &Option<ExclusionSet>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if inner.cancel.load(Ordering::Relaxed) {
            break;
        }
        out.push(dir.clone());
        if depth >= filters::DEFAULT_MAX_DEPTH {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut subdirs: Vec<PathBuf> = Vec::new();
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // 根目录本身不参与判定（它不在任何一次 read_dir 的结果里）
            if filters::should_skip_dir(&name) {
                continue;
            }
            let path = entry.path();
            // 被用户「关闭」的子目录：整棵剪掉，连目录都不记
            if let Some(ex) = exclusion
                && ex.hits(&path)
            {
                continue;
            }
            subdirs.push(path);
        }
        // 逆序压栈，弹出时即为字典序（与原 walkdir 的 `sort_by_file_name` 一致）
        subdirs.sort();
        for path in subdirs.into_iter().rev() {
            stack.push((path, depth + 1));
        }
    }
    out
}

/// 把一个目录的**完整快照**落库，并广播一条 `FolderScanned`。
///
/// 返回 `Ok(None)` = 落库前一刻发现索引已被删除（世代变了），本轮扫描作废；
/// `Ok(Some(id))` = 这个目录的 folder_id（调用方拿它去补本目录的缩略图）。
fn apply_dir(
    inner: &Arc<Inner>,
    dir: &Path,
    items: &[PhotoInput],
    epoch: u64,
) -> Result<Option<(i64, Vec<i64>)>> {
    let dir_str = paths::canonical_string(dir);
    let dir_key = paths::compare_key(&dir_str);
    let volume_id = volume_of(dir);
    let (folder_id, apply) = {
        let Ok(guard) = inner.db.lock() else {
            return Err(CoreError::Other("db 锁中毒".into()));
        };
        // ⚠️ 世代判据必须在**持锁时**比较：`delete_index` 也是在锁内加号，
        // 两边互斥，所以不存在"删完了又被写回来"的交错。返回 `false` = 本目录没落库。
        if inner.epoch.load(Ordering::SeqCst) != epoch {
            return Ok(None);
        }
        let folder_id = guard.upsert_folder(&dir_str, &dir_key, volume_id.as_deref())?;
        let apply = guard.apply_scan(folder_id, &dir_str, items, picasa_imaging::DECODER_VER)?;
        (folder_id, apply)
    };
    let _ = inner.tx.send(CoreEvent::FolderScanned {
        folder_id,
        path: dir_str,
        added: apply.added,
        updated: apply.updated,
        removed: apply.removed,
        photos_in_folder: items.len() as u32,
    });
    // `touched` = 本次新增或缓存键变化、缩略图被作废的照片。
    // 扫描阶段**只为这批**补图：库里那 22 万张历史欠账不该在每次启动时重跑。
    Ok(Some((folder_id, apply.touched)))
}

/// 走一遍目录树 → **逐目录**落库。返回根目录的 folder_id。
///
/// `Ok(None)` 表示「已标记为不可访问」或「索引已被删除、本次作废」，
/// 调用方不需要再报错，也不该继续走缩略图阶段。
fn walk_and_store(
    inner: &Arc<Inner>,
    root: &Path,
    key: &str,
    discovered: &mut u64,
) -> Result<Option<i64>> {
    // 本轮扫描的世代号。`delete_index` 会把它加一——之后本函数
    // 在目录边界上就会发现自己作废，立刻收工（详见 `Inner::epoch`）。
    let epoch = inner.epoch.load(Ordering::SeqCst);
    if !root.is_dir() {
        inner.board.mark_unreachable(key, "目录已失联");
        return Ok(None);
    }

    // ---- 排除规则（E 导航「关闭此子文件夹」）：进来前一次性载入，遍历时纯内存判定 ----
    //
    // 为什么在扫描侧做、而不是只在树里隐藏：只在树里隐藏的话，下一次任何扫描
    // 都会把被排除的目录原样加回来，用户会看到「关了又自己出现」——这是最招骂的一类 bug。
    let exclusion = load_exclusion_set(inner, root);

    // 0) 根目录先落库：即使它一张照片都没有，也要在左栏可见、可点，
    //    并且任务统计有个稳定的根 id（子树口径，见 `Db::subtree_thumb_stats`）。
    let root_id = {
        let Ok(guard) = inner.db.lock() else {
            return Err(CoreError::Other("db 锁中毒".into()));
        };
        // 与 `apply_dir` 同一个判据：删索引那一瞬间不许再插回一行
        if inner.epoch.load(Ordering::SeqCst) != epoch {
            return Ok(None);
        }
        let volume_id = volume_of(root);
        guard.upsert_folder(key, &paths::compare_key(key), volume_id.as_deref())?
    };

    // 1) 先枚举目录（便宜）
    let dirs = collect_dirs(inner, root, &exclusion);

    // 2) 再逐目录处理（贵）：列直接文件 → probe → 落库 → 报进度
    //
    // ⚠️ 这里每处理完一个目录就**立刻**落库并广播，而不是等整棵树走完。
    // 移动硬盘上「遍历完再写」等于让用户盯着一片空白等几十分钟——他要的是
    // 「索引在后台跑，主界面随时能看」。
    let mut denied = 0u64;
    let mut last_report = Instant::now();

    for dir in &dirs {
        // 暂停 → 就停在这个目录边界上等；恢复后从同一个 dir 继续（不重扫前面的）
        if wait_if_paused(inner) {
            break;
        }
        // 索引被删了（世代变了）→ 剩下的目录没有意义，收工
        if inner.epoch.load(Ordering::SeqCst) != epoch {
            tracing::info!(root = %root.display(), "索引已删除，本次扫描作废");
            return Ok(None);
        }
        inner.dirs.fetch_add(1, Ordering::Relaxed);
        inner.board.note_dir();

        let Ok(rd) = std::fs::read_dir(dir) else {
            denied += 1;
            continue;
        };

        // 这个目录在库里记过什么？没变过的文件就不必再读盘解析 EXIF。
        //
        // 为什么重要：全量探测一次要读 25.9 万个文件的头部，在 USB 移动硬盘上是
        // **几十分钟**。而「启动只做增量发现」如果每次都把这几十分钟再走一遍，
        // 等于没做增量——用户等的是「发现新照片」，不是「把老照片重读一遍」。
        let cached = {
            let dir_str = paths::canonical_string(dir);
            match lock_db(&inner.db) {
                Some(db) => match db.folder_id(&dir_str) {
                    Ok(Some(id)) => db.folder_cached_meta(id).unwrap_or_default(),
                    _ => Default::default(),
                },
                None => Default::default(),
            }
        };

        let mut items: Vec<PhotoInput> = Vec::new();
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            if filters::should_skip_file(&name) || !filters::is_supported_image(&path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };

            let size_bytes = meta.len() as i64;
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            let name_key = paths::compare_key(&name);

            // 缓存命中：尺寸/时间戳/解码器版本全对得上，且宽高这件事**已经出过结论**。
            // 直接复用库里的元数据 —— 这一次**完全不碰文件内容**。
            //
            // 「出过结论」包含两种：读到了宽高，以及读不到、已判终态（`dims_status = 2`）。
            // 后者若不算命中，这批文件每次启动都会被重新 `probe()` 一遍再判一次失败——
            // 真实库里这个数字是 903 张，而它们的结论永远不会变。
            let hit = cached
                .get(&name_key)
                .filter(|c| c.matches(size_bytes, mtime_ns, picasa_imaging::DECODER_VER))
                .filter(|c| c.dims_settled());

            let input = if let Some(c) = hit {
                PhotoInput {
                    name,
                    name_key,
                    size_bytes,
                    mtime_ns,
                    width: c.width,
                    height: c.height,
                    orientation: c.orientation,
                    taken_at: c.taken_at,
                    taken_at_src: c.taken_at_src,
                }
            } else {
                let probe = picasa_imaging::probe(&path).unwrap_or_default();
                let (taken_at, taken_at_src) = match probe.exif_taken_at {
                    Some(ts) => (Some(ts), 0i64),
                    None => {
                        // F1-15：日期缺失时回退文件修改时间，并且**标记来源**（UI 可见）
                        let fallback = (mtime_ns > 0).then_some(mtime_ns / 1_000_000_000);
                        (fallback, 2)
                    }
                };
                PhotoInput {
                    name,
                    name_key,
                    size_bytes,
                    mtime_ns,
                    width: Some(probe.width as i64).filter(|w| *w > 0),
                    height: Some(probe.height as i64).filter(|h| *h > 0),
                    orientation: Some(probe.orientation as i64),
                    taken_at,
                    taken_at_src,
                }
            };
            items.push(input);

            *discovered += 1;
            inner.found.fetch_add(1, Ordering::Relaxed);
        }

        // 空目录不落库：原实现里 `by_dir` 也只收录「有图片的目录」。
        // 这样纯目录骨架（相册分类夹常是空壳）不会产生几千个空事务。
        //
        // `None` = 落库前一刻发现索引已被删除（世代变了），整轮收工。
        if !items.is_empty() {
            let Some((_folder_id, touched)) = apply_dir(inner, dir, &items, epoch)? else {
                tracing::info!(root = %root.display(), "索引已删除，本次扫描作废");
                return Ok(None);
            };

            // ⚠️ 缩略图**就地补，但只补本次新增/变化的那几张**。
            //
            // 两个错误极端都踩过：
            //  · 老实现把补图放在 `run_job` 末尾、整棵子树一把梭 —— 25.9 万张的库
            //    光遍历就要十几分钟，这十几分钟里一张图都不出，用户以为程序死了；
            //  · 而每扫一个目录就把该目录**所有**待补的都补掉，等于每次启动都把
            //    22 万张历史欠账重跑一遍，启动动辄几小时。
            // 正确的口径是「新增的立刻看得见，历史欠账等用户显式要求」。
            if !touched.is_empty() {
                generate_thumbs_for_ids(inner, &touched, key, root, epoch);
            }
        }

        // 进度节流：按时间而不是按条目。慢盘上一个目录可能要几秒，
        // 按条目刷会让数字长时间不动，看起来像卡死。
        if last_report.elapsed() >= Duration::from_millis(250) {
            last_report = Instant::now();
            inner.board.note_discovered(key, *discovered);
            let _ = inner.tx.send(CoreEvent::Progress {
                dirs: inner.dirs.load(Ordering::Relaxed),
                photos: *discovered,
                current: dir.display().to_string(),
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

    // 层级与直接计数跟上（M2.1）：新扫出来的子目录要挂上父链，`direct_count` 要跟着变。
    // 做法是「读一遍 folders 再回写」——几千行是毫秒级，比增量维护计数少一整类 bug。
    if inner.epoch.load(Ordering::SeqCst) != epoch {
        return Ok(None);
    }
    if let Ok(guard) = inner.db.lock() {
        match guard.refresh_hierarchy() {
            Ok((created, linked)) => tracing::debug!(created, linked, "文件夹层级已刷新"),
            Err(e) => tracing::warn!(error = %e, "刷新文件夹层级失败"),
        }
    }
    Ok(Some(root_id))
}

/// 每次从库里取多少条待补缩略图。
///
/// 不能一次取 5 万：① 单条 SQL 要把 5 万行（含 `folders.path` 的 join）读进内存，
/// 这台 5W 无风扇机上就是几百毫秒的阻塞；② 一批全做完才落库，进度是一格一格跳的。
/// 小批 + 循环，进度连续，「暂停」也能在 2000 张之内生效。
const THUMB_CHUNK: usize = 2_000;

/// 处理一批已从库里取出的待补照片。返回 `true` = 至少有一条状态被推进。
///
/// 抽出来是为了让「扫描时只补新增」和「显式重试时补全历史欠账」共用同一段并发逻辑。
/// 两处各写一遍的话，暂停语义、拔盘检测、「写失败必须改状态否则死循环」这些细节
/// 迟早会在某一处走样。
fn process_batch(
    inner: &Arc<Inner>,
    pending: &[PhotoRow],
    key: &str,
    root: &Path,
    epoch: u64,
    watched: &AtomicU64,
) -> bool {
    use rayon::prelude::*;

    // 本批有效处理条数（写成功 + 标记失败）。若一批下来一条都没推进，
    // 说明状态写不进库——此时再循环就是死循环，必须退出去。
    let progressed = AtomicU64::new(0);

    // 在**专用池**里跑，不用全局池——全局池线程数等于逻辑核数，
    // 会把界面线程和合成器一起挤掉（见 `Inner::thumb_pool` 的注释）。
    inner.thumb_pool.install(|| {
        pending.par_iter().for_each(|row| {
            // 暂停：在这里死等。缩略图阶段是「暂停索引」最贵的部分（每张都要解码），
            // 不停下来的话用户按了暂停还在掉 CPU，等于按钮没生效。
            block_while_paused(inner);
            if inner.cancel.load(Ordering::Relaxed) || inner.epoch.load(Ordering::SeqCst) != epoch {
                return;
            }
            // 每 64 张摸一次目录：外置盘拔掉时不必等几千张全部失败
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
                            // ⚠️ 写失败也必须把状态改掉：留在 `thumb_state = 0`
                            // 会被下一批原样再取出来，变成永远补不完的死循环。
                            tracing::warn!(error = %e, "写缩略图失败，标记为失败");
                            let _ = guard.set_thumb_state(row.id, ThumbState::Failed);
                        }
                        progressed.fetch_add(1, Ordering::Relaxed);
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
                        progressed.fetch_add(1, Ordering::Relaxed);
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
    });

    progressed.load(Ordering::Relaxed) > 0
}

/// 给**指定的这一批照片**补缩略图（扫描主路径）。
///
/// 传进来的 id 已经由 `apply_scan` 过滤成「新增 / 缓存键变化」，
/// 所以这里不会顺手把库里 22 万张历史欠账也补一遍。
/// 取的时候仍然带 `thumb_state = 0` 条件：同一次扫描里重复的目录、
/// 或者刚被别处补过的照片，会被自然跳过（跳过的也会从列表里消失，不会死循环）。
fn generate_thumbs_for_ids(inner: &Arc<Inner>, ids: &[i64], key: &str, root: &Path, epoch: u64) {
    if ids.is_empty() {
        return;
    }
    let watched = AtomicU64::new(0);
    for chunk in ids.chunks(THUMB_CHUNK) {
        if inner.cancel.load(Ordering::Relaxed) || inner.epoch.load(Ordering::SeqCst) != epoch {
            return;
        }
        let pending = match lock_db(&inner.db)
            .and_then(|db| db.pending_thumbs_by_ids(chunk, THUMB_CHUNK).ok())
        {
            Some(v) => v,
            None => return,
        };
        if pending.is_empty() {
            continue;
        }
        if !process_batch(inner, &pending, key, root, epoch, &watched) {
            return;
        }
    }
}

/// 把一个目录（含子目录）里**所有**待补缩略图补到没有为止。
///
/// 只给「用户显式重试 / 补全」用。启动扫描**不走这里**：
/// 25.9 万张的库有 22 万张历史欠账，每次启动全补一遍就是几小时，
/// 那不是「打开相册」，那是「离线批处理」。
fn generate_thumbs(inner: &Arc<Inner>, folder_id: i64, key: &str, root: &Path) {
    let epoch = inner.epoch.load(Ordering::SeqCst);
    let watched = AtomicU64::new(0);
    let ids = match lock_db(&inner.db).and_then(|db| db.subtree_folder_ids(folder_id).ok()) {
        Some(v) => v,
        None => return,
    };
    for id in ids {
        loop {
            if inner.cancel.load(Ordering::Relaxed) || inner.epoch.load(Ordering::SeqCst) != epoch {
                return;
            }
            let pending = match lock_db(&inner.db)
                .and_then(|db| db.photos_pending_thumbs(Some(&[id]), THUMB_CHUNK).ok())
            {
                Some(v) => v,
                None => return,
            };
            if pending.is_empty() {
                break;
            }
            if !process_batch(inner, &pending, key, root, epoch, &watched) {
                return;
            }
        }
    }
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

/// 扫描时用的排除集：一组「相对最近关注根目录」的比较键。
///
/// 存在意义：用户「关闭某个子文件夹」后，**任何一次重扫都不能把它加回来**。
/// 只在左栏树里隐藏是假修复——下一次扫描照旧入库，用户看到「关了又自己出现」。
struct ExclusionSet {
    root: String,
    case_insensitive: bool,
    keys: HashSet<String>,
}

impl ExclusionSet {
    fn hits(&self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return false;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.is_empty() {
            return false; // 根目录本身永不被排除
        }
        let key = if self.case_insensitive {
            paths::compare_key(&rel)
        } else {
            paths::nfc(&rel)
        };
        self.keys.contains(&key)
    }
}

/// 取 `path` 所属的**最近**关注根目录的排除集（没有排除项就返回 `None`，省掉每次判定）。
fn load_exclusion_set(inner: &Arc<Inner>, path: &Path) -> Option<ExclusionSet> {
    let target = paths::canonical_string(path);
    let db = lock_db(&inner.db)?;
    let root = db
        .roots(false)
        .ok()?
        .into_iter()
        .filter(|r| target == r.path || target.starts_with(&format!("{}/", r.path)))
        .max_by_key(|r| r.path.len())?;
    let keys = db.exclusion_keys(root.id).ok()?;
    if keys.is_empty() {
        return None;
    }
    Some(ExclusionSet {
        root: root.path,
        case_insensitive: root.case_insensitive,
        keys,
    })
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
            assert!(
                guard.get_thumb(id, 0).unwrap().is_some(),
                "有效缩略图不重建"
            );
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
        let photos = db
            .photos(
                Some(&[fid]),
                picasa_db::SortSpec {
                    key: SortKey::Name,
                    descending: false,
                },
                10,
            )
            .unwrap();
        assert_eq!(photos[0].name, "a.JPG");
    }
}
