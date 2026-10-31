//! 宽高回填服务（M2.1 · B 工作包）。
//!
//! ## 它解决的是「历史数据的缺口」，不是「新数据的缺口」
//!
//! 新照片在扫描时就被 `probe()` 读出了宽高（`scanner.rs`），所以刚扫完的库
//! `dims_status = 1` 是齐的。**缺口来自旧库**：M2 的库里 `orientation` 有一批是 NULL、
//! 宽高有一批从来没读到过（当时盘没插、或格式当时不支持）。实测样本 1150 张里 61 张待回填，
//! **全部位于一块当时没挂载的外置盘上**——这一条直接决定了本模块最重要的那个判断（见下）。
//!
//! ## 四条设计约束
//!
//! ### 1. 可见优先（`docs/05` §2.1「可见区域按需回填」）
//!
//! 用户滚到哪，哪几张立刻补。栅格每帧把视口里「宽高还空着」的 id 递进来，
//! 走 [`DimsService::request_visible`]，**插在后台队列前面**。
//!
//! 这里刻意**不做**「等攒够一批再插队」的优化：插队的意义就是下一帧能画对，
//! 攒批会把延迟从毫秒变成秒。
//!
//! ### 2. 索引让路（`docs/05` §2.1「优先级控制」）
//!
//! 扫描 / 生成缩略图在跑时，回填**整段停手**（每 500 ms 看一眼 [`IndexBoard`]）。
//! 理由与 `scanner.rs` 里「单 worker 而不是每目录一线程」同源：这台机器核少（i5-8200Y，2C4T），
//! 回填抢走的每一个核都会直接变成用户眼里的卡顿。用户先要看到照片，再谈宽高。
//!
//! ### 3. 「读不到」不等于「坏了」——离线不判死
//!
//! 这是本模块**最容易写错**的地方，也是最贵的一处错：
//! 把「文件打不开」写成 `dims_status = 2`（读取失败），用户把外置盘插回来时，
//! 那 61 张就**永远**不会再被试一次，而界面上一切正常——这种错没人会报 bug，
//! 只会觉得「这软件有时候不准」。
//!
//! 因此判据拆成两条（见 `picasa_imaging::DimsError`）：
//! - 目录不存在 / 文件打不开 → **不动状态**，计 `deferred`，等盘回来再试；
//! - 文件读得到但拿不到宽高（HEIC/AVIF/文件头损坏）→ `dims_status = 2`，终态。
//!
//! 盘不在的时候整批都会 `deferred`，所以还要**退避**：一轮零进展就歇 30 s，
//! 否则 61 行 × 每秒若干轮会变成空转的风扇声。
//!
//! ### 4. 状态位只写一次，避免「重复失败累加」
//!
//! 栅格每帧都会把「宽高空着」的 id 递进来——包括已经判定失败的（它的宽高仍是 NULL）。
//! 服务侧用内存里的 `attempted` 挡住重复尝试，否则一张坏图会以 60 次/秒的频率重开文件。
//!
//! ## 真相源边界
//!
//! 实时进度**不进** [`IndexBoard`]：那是「文件夹索引任务」的四处一致口径（`docs/04` §3.2），
//! 把回填塞进去会让「已发现/已就绪」两个数字莫名其妙。回填自己的计数在
//! [`DimsService::progress`] 里，历史任务行写 `index_tasks`（`kind = 4`），
//! 与 `docs/05` §1.3.5 的分工一致。

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use picasa_db::{Db, PhotoRow, TaskKind, TaskStatus};

use crate::index::IndexBoard;

/// 一批处理多少张就让出一次。16 是「读 16 个文件头 ≈ 几毫秒」与
/// 「插队请求最多等这么久」的折中——批太大，用户滚到的那张要排在 60 张后面。
const CHUNK: usize = 16;
/// 批之间歇一下：连续读文件头会让外置盘的排队深度上去，索引线程的 `stat` 跟着变慢。
const CHUNK_PAUSE: Duration = Duration::from_millis(150);
/// 索引在跑时的让路轮询间隔。
const YIELD_POLL: Duration = Duration::from_millis(500);
/// 全库没有待回填时的空闲轮询。
const IDLE_POLL: Duration = Duration::from_secs(5);
/// 有待回填但一张也没读成（盘没挂）时的退避。
const OFFLINE_BACKOFF: Duration = Duration::from_secs(30);
/// `attempted` 的上限。超过就清空——它只用来挡「同一张一帧一次」的重复请求，
/// 清空最多让每张多被试一次，比把内存吃到几十 MB 划算。
const ATTEMPTED_CAP: usize = 200_000;

/// 回填自己的进度计数（**不是** `IndexBoard` 的一部分，见模块注释）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DimsProgress {
    /// 本次会话回填成功
    pub done: u64,
    /// 本次会话判定为终态失败
    pub failed: u64,
    /// 本次会话因「盘不在 / 文件暂时打不开」推迟
    pub deferred: u64,
    /// 最近一次查到的全库待回填数
    pub pending: u64,
}

/// 可见区域的待回填请求队列。
///
/// 单独抽出来是为了能直测「先进先出 + 去重」这两个容易写错的点——
/// 两者任一写错，表现都是「滚动时同一张反复排队」或「排队里排了 60 份同样的 id」。
#[derive(Default)]
struct UrgentQueue {
    ids: VecDeque<i64>,
    queued: HashSet<i64>,
}

impl UrgentQueue {
    fn push(&mut self, ids: &[i64]) {
        for &id in ids {
            if self.queued.insert(id) {
                self.ids.push_back(id);
            }
        }
    }

    fn pop(&mut self) -> Option<i64> {
        let id = self.ids.pop_front()?;
        self.queued.remove(&id);
        Some(id)
    }

    fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    fn clear(&mut self) {
        self.ids.clear();
        self.queued.clear();
    }
}

/// 一张照片的处理结局。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// 写入了宽高
    Done,
    /// 读得到文件但拿不到宽高 → `dims_status = 2`
    Failed,
    /// 盘不在 / 暂时打不开 → 不动状态，等下次
    Deferred,
    /// 本会话已经出过结论，跳过
    Skipped,
}

struct Inner {
    db: Arc<Mutex<Db>>,
    board: Arc<IndexBoard>,
    shutdown: Arc<AtomicBool>,
    done: AtomicU64,
    failed: AtomicU64,
    deferred: AtomicU64,
    pending: AtomicU64,
    /// 本会话已经出过结论的 id（成功或终态失败）。挡的是「每帧一次的重复请求」。
    attempted: Mutex<HashSet<i64>>,
    /// 本次回填在 `index_tasks` 里的行（懒建；`kind = 4`）
    task_id: Mutex<Option<i64>>,
    /// 退避提示只打一次日志，避免刷屏
    warned_offline: AtomicBool,
    /// 「已清空」的日志只打一次（否则每 5 s 一条）
    reported_done: AtomicBool,
}

impl Inner {
    fn lock_db(&self) -> Option<std::sync::MutexGuard<'_, Db>> {
        self.db.lock().ok()
    }

    /// 处理一张。所有状态位的变化都收口在这里。
    fn process(&self, row: &PhotoRow) -> Outcome {
        // `attempted` 只在两次极短的临界区里拿，**不跨文件 I/O**：
        // 持锁读文件是那种平时看不出来、出问题（盘卡住）就整个服务僵住的写法。
        if self.is_settled(row.id) {
            return Outcome::Skipped;
        }

        // 目录都不在（盘被拔了 / 没挂载）：**连锁都不用加**，直接推迟。
        // 这一条挡在 `probe_dims` 之前，是为了让「盘不在」这种情况不产生 61 次失败的 open。
        if !Path::new(&row.folder_path).is_dir() {
            self.deferred.fetch_add(1, Ordering::Relaxed);
            return Outcome::Deferred;
        }

        let path = row.abs_path();
        match picasa_imaging::probe_dims(&path) {
            Ok(d) => {
                let written = self.lock_db().map(|db| {
                    db.set_photo_dims(
                        row.id,
                        Some(d.width as i64),
                        Some(d.height as i64),
                        Some(d.orientation as i64),
                        true,
                    )
                });
                match written {
                    Some(Ok(())) => {
                        self.remember(row.id);
                        self.done.fetch_add(1, Ordering::Relaxed);
                        tracing::trace!(photo_id = row.id, w = d.width, h = d.height, "宽高已回填");
                        Outcome::Done
                    }
                    Some(Err(e)) => {
                        tracing::warn!(photo_id = row.id, error = %e, "宽高写库失败");
                        Outcome::Deferred
                    }
                    None => Outcome::Deferred,
                }
            }
            Err(e) if e.is_retryable() => {
                // 与「目录不在」同一类：不写状态位，等盘回来。
                self.deferred.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(photo_id = row.id, error = %e, "宽高暂时读不到，下次再试");
                Outcome::Deferred
            }
            Err(e) => {
                if let Some(db) = self.lock_db() {
                    let _ = db.set_photo_dims(row.id, None, None, None, false);
                } else {
                    return Outcome::Deferred;
                }
                self.remember(row.id);
                self.failed.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(photo_id = row.id, path = %path.display(), error = %e, "宽高读取失败（终态）");
                Outcome::Failed
            }
        }
    }

    fn remember(&self, id: i64) {
        if let Ok(mut attempted) = self.attempted.lock() {
            if attempted.len() >= ATTEMPTED_CAP {
                attempted.clear();
            }
            attempted.insert(id);
        }
    }

    /// 这张本会话是否已经出过结论（成功 / 终态失败）。
    fn is_settled(&self, id: i64) -> bool {
        self.attempted
            .lock()
            .map(|set| set.contains(&id))
            .unwrap_or(false)
    }

    /// 取（或新建）本次回填的任务行。
    ///
    /// **优先复用上次没跑完的那一行**：`docs/04` P-02 把「重启后恢复未完成任务」列成第三类批次，
    /// 回填也是任务（`kind = 4`）。每次都新建的话，一个「盘常年不在」的库每启动一次就留一条
    /// `Scanning` 状态的历史行，诊断页上会堆成一串永远「进行中」的幽灵任务。
    fn ensure_task(&self) -> Option<i64> {
        let mut slot = self.task_id.lock().ok()?;
        if let Some(id) = *slot {
            return Some(id);
        }
        let db = self.lock_db()?;
        let reused = db
            .unfinished_tasks()
            .ok()
            .and_then(|tasks| {
                tasks
                    .into_iter()
                    .find(|t| t.kind == TaskKind::DimsBackfill.code())
                    .map(|t| t.task_id)
            });
        let id = match reused {
            Some(id) => id,
            None => db.create_task(None, "/", TaskKind::DimsBackfill).ok()?,
        };
        let _ = db.set_task_status(id, TaskStatus::Scanning, None);
        *slot = Some(id);
        Some(id)
    }

    /// 收尾任务行：`pending == 0` 时才结案，否则留着让下次会话接着算。
    fn finish_task(&self) {
        let Ok(mut slot) = self.task_id.lock() else {
            return;
        };
        let Some(id) = slot.take() else {
            return;
        };
        let failed = self.failed.load(Ordering::Relaxed);
        let status = if failed == 0 {
            TaskStatus::Completed
        } else {
            TaskStatus::PartialFailed
        };
        if let Some(db) = self.lock_db() {
            let _ = db.set_task_status(
                id,
                status,
                (failed > 0).then_some("dims_read_failed"),
            );
        }
    }

    fn progress(&self) -> DimsProgress {
        DimsProgress {
            done: self.done.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            deferred: self.deferred.load(Ordering::Relaxed),
            pending: self.pending.load(Ordering::Relaxed),
        }
    }
}

/// 宽高回填服务。**单线程**，随 `Drop` 退出（不 join，与 `IndexService` 一致）。
pub struct DimsService {
    inner: Arc<Inner>,
    tx: Sender<Vec<i64>>,
    worker: Option<JoinHandle<()>>,
}

impl DimsService {
    /// 起一个回填线程。`board` 与索引服务共用（只读，用于让路判断）。
    pub fn new(db: Arc<Mutex<Db>>, board: Arc<IndexBoard>) -> Self {
        let (tx, rx) = channel::<Vec<i64>>();
        let inner = Arc::new(Inner {
            db,
            board,
            shutdown: Arc::new(AtomicBool::new(false)),
            done: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            deferred: AtomicU64::new(0),
            pending: AtomicU64::new(0),
            attempted: Mutex::new(HashSet::new()),
            task_id: Mutex::new(None),
            warned_offline: AtomicBool::new(false),
            reported_done: AtomicBool::new(false),
        });

        let w = inner.clone();
        let worker = std::thread::Builder::new()
            .name("op-dims".into())
            .spawn(move || worker_loop(rx, w))
            .expect("无法创建回填线程");

        Self {
            inner,
            tx,
            worker: Some(worker),
        }
    }

    /// 视口里宽高还空着的照片——**插到队首**。
    ///
    /// 调用方（栅格）每帧都调，传进来的 id 里必然混着已经处理过的：
    /// 内存里的 `PhotoRow` 是启动时拍的快照，回填把库里改好了，快照里的 `width`
    /// 仍然是 NULL，直到下一次 `reload_photos`。所以**在这里就把已出结论的滤掉**——
    /// 不滤的话，一张已经回填好的照片会以 60 次/秒的频率重新排队，
    /// 回填线程就会把整个核烧在「弹出一个已知答案的 id 再丢掉」上。
    pub fn request_visible(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        let fresh: Vec<i64> = match self.inner.attempted.lock() {
            Ok(set) => ids.iter().copied().filter(|id| !set.contains(id)).collect(),
            Err(_) => return,
        };
        if fresh.is_empty() {
            return;
        }
        let _ = self.tx.send(fresh);
    }

    /// 本会话的回填计数。
    pub fn progress(&self) -> DimsProgress {
        self.inner.progress()
    }
}

impl Drop for DimsService {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::Relaxed);
        // 不 join：退出路径上等一张图读完只会让窗口关得更慢。
        // worker 是 `recv_timeout` 轮询 + shutdown 检查，最多 200 ms 自行退出。
        if let Some(h) = self.worker.take() {
            let _ = h;
        }
    }
}

// ---------------------------------------------------------------- worker

fn worker_loop(rx: Receiver<Vec<i64>>, inner: Arc<Inner>) {
    let mut urgent = UrgentQueue::default();
    // 用 recv_timeout 代替 sleep：既能歇，又能被插队请求立刻唤醒。
    let wait = |d: Duration, urgent: &mut UrgentQueue| -> bool {
        match rx.recv_timeout(d) {
            Ok(ids) => {
                urgent.push(&ids);
                true
            }
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => false,
        }
    };

    loop {
        if inner.shutdown.load(Ordering::Relaxed) {
            break;
        }
        // 把这一轮攒下的请求都收进来（非阻塞）
        while let Ok(ids) = rx.try_recv() {
            urgent.push(&ids);
        }

        // ---- 1. 可见优先：一张一张做，每张都回来看有没有新请求 ----
        if let Some(id) = urgent.pop() {
            // 先查内存再查库：已出结论的 id 不该为它摸一次 DB 锁。
            if inner.is_settled(id) {
                continue;
            }
            if let Some(row) = inner
                .lock_db()
                .and_then(|db| db.photos_by_ids(&[id]).ok())
                .and_then(|mut v| v.pop())
            {
                let outcome = inner.process(&row);
                if outcome == Outcome::Done || outcome == Outcome::Failed {
                    inner.ensure_task();
                }
            }
            continue;
        }

        // ---- 2. 索引在跑 → 整段让路 ----
        if inner.board.running() {
            if !wait(YIELD_POLL, &mut urgent) {
                break;
            }
            continue;
        }

        // ---- 3. 后台批量 ----
        let pending = inner
            .lock_db()
            .and_then(|db| db.pending_dims_count().ok())
            .unwrap_or(0);
        inner.pending.store(pending, Ordering::Relaxed);
        if pending == 0 {
            inner.finish_task();
            let p = inner.progress();
            if (p.done > 0 || p.failed > 0 || p.deferred > 0)
                && !inner.reported_done.swap(true, Ordering::Relaxed)
            {
                tracing::info!(
                    done = p.done,
                    failed = p.failed,
                    deferred = p.deferred,
                    "宽高回填完成"
                );
            }
            if !wait(IDLE_POLL, &mut urgent) {
                break;
            }
            continue;
        }
        inner.reported_done.store(false, Ordering::Relaxed);

        let rows = inner
            .lock_db()
            .and_then(|db| db.photos_pending_dims(None, CHUNK).ok())
            .unwrap_or_default();
        if rows.is_empty() {
            if !wait(IDLE_POLL, &mut urgent) {
                break;
            }
            continue;
        }

        let mut progressed = 0usize;
        let mut deferred = 0usize;
        for row in &rows {
            if inner.shutdown.load(Ordering::Relaxed) {
                break;
            }
            // 插队请求随时可以打断批量：批里剩下的行状态还是 0，下一轮自然会再取到。
            while let Ok(ids) = rx.try_recv() {
                urgent.push(&ids);
            }
            if !urgent.is_empty() {
                break;
            }
            match inner.process(row) {
                Outcome::Done | Outcome::Failed => {
                    progressed += 1;
                    inner.ensure_task();
                }
                Outcome::Deferred => deferred += 1,
                Outcome::Skipped => {}
            }
        }

        if progressed == 0 {
            // 一整批零进展：盘不在（或全是已出结论的行）。退避，别空转。
            if deferred > 0 && !inner.warned_offline.swap(true, Ordering::Relaxed) {
                tracing::info!(
                    deferred,
                    "宽高回填本轮零进展（多半是外置盘未挂载），推迟重试"
                );
            }
            if !wait(OFFLINE_BACKOFF, &mut urgent) {
                break;
            }
        } else {
            inner.warned_offline.store(false, Ordering::Relaxed);
            if !wait(CHUNK_PAUSE, &mut urgent) {
                break;
            }
        }
    }

    // 退出前把「已经跑完」的任务结案；没跑完的留着，下次会话接着算（见 `ensure_task`）。
    if inner.pending.load(Ordering::Relaxed) == 0 {
        inner.finish_task();
    }
    urgent.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgent_queue_is_fifo_and_dedups() {
        let mut q = UrgentQueue::default();
        q.push(&[7, 8, 7, 9]);
        assert_eq!(q.pop(), Some(7));
        q.push(&[7, 10]); // 7 已经出队，可以再次入队
        assert_eq!(q.pop(), Some(8));
        assert_eq!(q.pop(), Some(9));
        assert_eq!(q.pop(), Some(7));
        assert_eq!(q.pop(), Some(10));
        assert_eq!(q.pop(), None);
        assert!(q.is_empty());
    }

    #[test]
    fn urgent_queue_does_not_grow_on_repeated_frames() {
        // 栅格每帧都递同一批 id：队列长度必须保持恒定，否则滚动一会儿就排了几千份
        let mut q = UrgentQueue::default();
        for _ in 0..60 {
            q.push(&[1, 2, 3]);
        }
        assert_eq!(q.ids.len(), 3);
    }
}
