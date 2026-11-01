//! 三段索引流水线（M2.2 §3.4，PERF-12/16/17）。
//!
//! ## 为什么要拆成三段
//!
//! M2.1 的索引是一坨「边走边解边写」的循环：一个线程做完了所有事。
//! 于是 **磁盘 I/O（遍历）** 与 **CPU（解码）** 与 **写库** 三者互相等待——
//! 解码在算的时候磁盘闲着，写库在等锁的时候 CPU 闲着。5 万张的库上
//! 这三者各占三分之一的时间，串行起来就是吞吐只有理论上限的三分之一。
//!
//! 拆成三段之后：
//!
//! ```text
//! 遍历(1 个 I/O 线程) --[有界 1024]--> 解码(CPU 池, cores-1) --[有界 256]--> 落库(1 个写线程)
//! ```
//!
//! ## 为什么通道必须**有界**
//!
//! 无界队列在「解码慢于遍历」时会把整棵目录树都读进内存——25 万张的库就是
//! 几十万个 `PathBuf` + 元数据，几个 GB。有界之后，队列满了就**阻塞上游**，
//! 内存占用恒定，代价只是遍历线程偶尔停一下（它本来就在等磁盘）。
//!
//! ## 为什么落库只有**一个**写线程
//!
//! SQLite 的写是**全局串行**的：开 8 个写线程只会让它们在锁上排队，
//! 还额外带来 WAL 争用。一个写线程批量提交（200 条或 100 ms，取先到者）
//! 才是把事务开销摊薄的正解。
//!
//! ## 零写入红线（R-1）
//!
//! 流水线是本版**最大的 R-1 风险面**：它第一次把「解码」和「落库」放进了
//! 独立线程，任何一处把临时文件写到照片目录都会破坏红线。这里的保证是：
//! - 目录里只 `read_dir` / 只读 `File::open`，没有任何 `create`/`write`/`rename`；
//! - 落库线程只写应用容器里的 DB；
//! - 不调用 `std::env::set_temp_dir`（那会把第三方解码库的临时文件引到照片目录）。

use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

// ---------------------------------------------------------------- 常量（附录 A）

/// 遍历 → 解码 的通道容量。
pub const CHAN_SCAN_TO_DECODE: usize = 1024;
/// 解码 → 落库 的通道容量。
pub const CHAN_DECODE_TO_DB: usize = 256;
/// 落库批量：凑够这么多条就提交。
pub const DB_BATCH_ROWS: usize = 200;
/// 落库批量：或者等这么久就提交（先到者生效）。
pub const DB_BATCH_MS: u64 = 100;

/// 节流档位（PERF-17）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Throttle {
    /// 全力：`cores - 1` 个解码线程
    Full,
    /// 减半：前台正在滚动
    Half,
    /// 单线程：低电量
    Single,
    /// 挂起：系统暂停（P-03）。**不丢队列**，只是不取。
    Suspended,
}

/// 解码任务（`ThumbReason` 决定优先级与边界）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbReason {
    /// 扫描新发现的
    Discovered,
    /// 用户点了「重试失败项」
    RetryFailed,
    /// 用户点了「全量重建」
    Rebuild,
}

/// 交给解码段的一件活。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeItem {
    pub photo_id: i64,
    /// 所属目录（决定优先级：用户正在看的目录优先）
    pub folder_id: i64,
    pub tier: usize,
    pub reason: ThumbReason,
}

impl DecodeItem {
    /// 优先级（大者先做）。
    ///
    /// 三档：可见区 > 当前文件夹 > 其它。
    /// PERF-16 的原话是「用户当前所在 folder_id 的项优先级最高；可见区的项再高一级」，
    /// 所以「换文件夹时**重算优先级**而不是清空队列」——清空会把已经排到前面的
    /// 其它目录的活全部丢掉，用户切回来又要重做一遍。
    fn priority(&self, focus: Option<i64>, visible: &[i64]) -> u8 {
        if visible.contains(&self.photo_id) {
            2
        } else if focus == Some(self.folder_id) {
            1
        } else {
            0
        }
    }
}

/// 优先队列的条目（`Ord` 按 `priority` 再按入队序号，后者保证 FIFO 稳定）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Queued {
    priority: u8,
    seq: u64,
    item: DecodeItem,
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // ⚠️ `seq` 是**反序**比较的：`BinaryHeap` 是最大堆，同优先级下要让
        // 「先入队的先出来」就必须让「seq 小的**更大**」。正序写会变成 LIFO，
        // 表现是用户切到一个目录后，这个目录里最后被发现的那批先做、
        // 最早发现的反而一直压在队底（实测就是这个症状，`pipeline::tests` 守着）。
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ------------------------------------------------------------------- 状态

/// 流水线的共享状态（线程间只传 `Arc<PipelineState>`）。
pub struct PipelineState {
    /// 待解码的优先队列
    queue: Mutex<BinaryHeap<Queued>>,
    /// 入队序号（保证同优先级 FIFO）
    seq: AtomicUsize,
    /// 当前焦点目录与可见区（PERF-16）
    focus: Mutex<Option<i64>>,
    visible: Mutex<Vec<i64>>,
    /// 节流档位
    throttle: Mutex<Throttle>,
    /// 停止信号
    stop: AtomicBool,
    /// 待解码队列长度（诊断与 `idx_bench` 用；也是派发段的背压水位）
    queued: AtomicUsize,
    /// 已落库条数
    stored: AtomicUsize,
}

impl Default for PipelineState {
    fn default() -> Self {
        Self::new()
    }
}

impl PipelineState {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(BinaryHeap::new()),
            seq: AtomicUsize::new(0),
            focus: Mutex::new(None),
            visible: Mutex::new(Vec::new()),
            throttle: Mutex::new(Throttle::Full),
            stop: AtomicBool::new(false),
            queued: AtomicUsize::new(0),
            stored: AtomicUsize::new(0),
        }
    }

    /// 入队。**不清空已有队列**（PERF-16：换文件夹是重算优先级，不是推倒重来）。
    pub fn enqueue(&self, items: impl IntoIterator<Item = DecodeItem>) {
        let Ok(mut q) = self.queue.lock() else {
            return;
        };
        let mut n = 0;
        for item in items {
            let seq = self.seq.fetch_add(1, Ordering::Relaxed) as u64;
            q.push(Queued {
                priority: 0, // 真正的优先级在 `pop` 时按当前 focus 重算
                seq,
                item,
            });
            n += 1;
        }
        self.queued.fetch_add(n, Ordering::Relaxed);
    }

    /// 取一件（按**当前** focus/visible 重新算优先级）。
    ///
    /// 为什么不在入队时算：焦点会变。入队时算好就写死了，
    /// 用户切到另一个目录之后旧队列的优先级还是旧的。
    fn pop(&self) -> Option<DecodeItem> {
        let Ok(mut q) = self.queue.lock() else {
            return None;
        };
        let (focus, visible) = {
            let f = self.focus.lock().ok().and_then(|g| *g);
            let v = self.visible.lock().map(|g| g.clone()).unwrap_or_default();
            (f, v)
        };
        // `BinaryHeap` 是最大堆但优先级写在元素里，焦点变了就整体重排一次。
        // 队列长度受 `CHAN_SCAN_TO_DECODE` 约束（≤1024），重排是微秒级。
        let mut items: Vec<Queued> = q.drain().collect();
        for it in items.iter_mut() {
            it.priority = it.item.priority(focus, &visible);
        }
        let mut heap: BinaryHeap<Queued> = items.into_iter().collect();
        let out = heap.pop();
        *q = heap;
        if out.is_some() {
            self.queued.fetch_sub(1, Ordering::Relaxed);
        }
        out.map(|x| x.item)
    }

    pub fn set_focus(&self, folder_id: Option<i64>, visible: &[i64]) {
        if let Ok(mut g) = self.focus.lock() {
            *g = folder_id;
        }
        if let Ok(mut g) = self.visible.lock() {
            *g = visible.to_vec();
        }
    }

    pub fn throttle(&self, level: Throttle) {
        if let Ok(mut g) = self.throttle.lock() {
            *g = level;
        }
    }

    pub fn throttle_level(&self) -> Throttle {
        self.throttle.lock().map(|g| *g).unwrap_or(Throttle::Full)
    }

    /// 当前的解码并发数（PERF-17 的落地值）。
    ///
    /// `Suspended` 返回 0 但**不丢队列**：队列还堆在 `PriorityQueue` 里，
    /// 恢复后从原地继续（P-03：系统暂停态没有「继续」按钮，恢复由系统事件自动发生）。
    pub fn workers(&self) -> usize {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .max(1);
        match self.throttle_level() {
            Throttle::Full => (cores - 1).max(1),
            Throttle::Half => (cores / 2).max(1),
            Throttle::Single => 1,
            Throttle::Suspended => 0,
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    // ---- 计数器（诊断与测试用）----
    pub fn queued_len(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }
    pub fn stored(&self) -> usize {
        self.stored.load(Ordering::Relaxed)
    }
    pub fn note_stored(&self, n: usize) {
        self.stored.fetch_add(n, Ordering::Relaxed);
    }
}

// ------------------------------------------------------------------- 流水线

/// 三段流水线。
///
/// ⚠️ 本模块**只提供骨架与调度**，具体的解码/落库由调用方以闭包注入
/// （`DecodeFn` / `StoreFn`）。这样它能在**没有真实图片、没有真实 DB** 的测试里
/// 被完整验证——吞吐这类约束，不能自动化验证就等于没有约束。
pub struct Pipeline {
    state: Arc<PipelineState>,
    decode_tx: SyncSender<DecodeItem>,
    store_tx: SyncSender<StoredItem>,
    handles: Vec<JoinHandle<()>>,
}

/// 解码段的产物。
#[derive(Clone, Debug)]
pub struct StoredItem {
    pub photo_id: i64,
    pub tier: usize,
    /// 编码后的缩略图字节（jpeg/webp）
    pub bytes: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// 解码回调：`(photo_id, tier) -> Option<产物>`。返回 `None` = 这次失败了。
pub type DecodeFn = Box<dyn Fn(i64, usize) -> Option<(Vec<u8>, u32, u32)> + Send + Sync>;
/// 落库回调：一批产物一次提交。
pub type StoreFn = Box<dyn Fn(&[StoredItem]) + Send + Sync>;

impl Pipeline {
    /// 起三段线程。
    ///
    /// `scan` 是遍历段的输入源：流水线会不断向它要下一批条目。
    /// 这里用闭包而不是 `Iterator`，是因为遍历段**必须能被阻塞**——
    /// 通道满了就不能再读盘，否则内存会被无界地填进去。
    pub fn start(
        state: Arc<PipelineState>,
        scan: Box<dyn Fn() -> Vec<DecodeItem> + Send + Sync>,
        decode: DecodeFn,
        store: StoreFn,
    ) -> Self {
        let (decode_tx, decode_rx) = sync_channel::<DecodeItem>(CHAN_SCAN_TO_DECODE);
        let (store_tx, store_rx) = sync_channel::<StoredItem>(CHAN_DECODE_TO_DB);
        let mut handles = Vec::new();

        // ---- ① 遍历段（1 个线程）
        let s1 = Arc::clone(&state);
        let tx1 = decode_tx.clone();
        handles.push(thread::spawn(move || {
            while !s1.stopped() {
                let batch = scan();
                if batch.is_empty() {
                    break;
                }
                for item in batch {
                    if s1.stopped() {
                        break;
                    }
                    // 有界通道：满了就**阻塞在这里**，绝不继续读盘
                    if tx1.send(item).is_err() {
                        return;
                    }
                }
            }
        }));

        // ---- ② 派发（解码段的入口，1 个线程）：有界通道 → 优先队列
        //
        // 为什么要有这一层：`std::sync::mpsc` 的通道**没有优先级**，
        // 而 PERF-16 要求「可见区 > 当前文件夹 > 其它」。直接让解码线程
        // 各拿一个 receiver 就实现不了优先级；先落进优先队列再让它们 `pop()`
        // 才两全。**内存仍然有界**：队列涨到 HIGH_WATER 就停住不再取，
        // 压力顺着有界通道传回遍历段，形成完整的背压链。
        let s2 = Arc::clone(&state);
        handles.push(thread::spawn(move || {
            while !s2.stopped() {
                if s2.queued_len() >= CHAN_SCAN_TO_DECODE {
                    thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                match decode_rx.recv_timeout(std::time::Duration::from_millis(20)) {
                    Ok(item) => s2.enqueue(std::iter::once(item)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        }));

        // ---- ③ 解码段（CPU 池，`cores-1` 个线程）
        //
        // `Box<dyn Fn ... + Send + Sync>` 本身是 `Send + Sync` 的，但 `Box` 不支持
        // 多个 `move` 闭包各拿一份 —— 用 `Arc` 共享同一份闭包即可，
        // 解码本来就该是无状态纯函数（同样的输入必须产出同样的缩略图）。
        let decode = Arc::new(decode);
        let nr = state.workers().max(1);
        for _ in 0..nr {
            let s3 = Arc::clone(&state);
            let tx2 = store_tx.clone();
            let dec = Arc::clone(&decode);
            handles.push(thread::spawn(move || {
                while !s3.stopped() {
                    // 挂起态：睡一会再看，**不取也不丢**
                    if s3.workers() == 0 {
                        thread::sleep(std::time::Duration::from_millis(50));
                        continue;
                    }
                    let Some(item) = s3.pop() else {
                        thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    };
                    let out = dec(item.photo_id, item.tier);
                    if let Some((bytes, w, h)) = out
                        && tx2
                            .send(StoredItem {
                                photo_id: item.photo_id,
                                tier: item.tier,
                                bytes,
                                w,
                                h,
                            })
                            .is_err()
                    {
                        return;
                    }
                }
            }));
        }

        // ---- ④ 落库段（1 个写线程，批量提交）
        let s4 = Arc::clone(&state);
        handles.push(thread::spawn(move || {
            let mut batch: Vec<StoredItem> = Vec::with_capacity(DB_BATCH_ROWS);
            let mut last = std::time::Instant::now();
            while !s4.stopped() {
                match store_rx.recv_timeout(std::time::Duration::from_millis(20)) {
                    Ok(item) => {
                        batch.push(item);
                        // 凑够条数 → 立刻提交
                        if batch.len() >= DB_BATCH_ROWS {
                            let n = batch.len();
                            store(&batch);
                            s4.note_stored(n);
                            batch.clear();
                            last = std::time::Instant::now();
                        }
                    }
                    Err(_) => {
                        // 超时 → 攒够时间也提交（先到者生效的另一半）
                        if !batch.is_empty()
                            && last.elapsed() >= std::time::Duration::from_millis(DB_BATCH_MS)
                        {
                            let n = batch.len();
                            store(&batch);
                            s4.note_stored(n);
                            batch.clear();
                            last = std::time::Instant::now();
                        }
                    }
                }
            }
            // 收尾：剩下的必须提交（进程要退了，不能留在内存里）
            if !batch.is_empty() {
                let n = batch.len();
                store(&batch);
                s4.note_stored(n);
            }
        }));

        Self {
            state,
            decode_tx,
            store_tx,
            handles,
        }
    }

    pub fn state(&self) -> &Arc<PipelineState> {
        &self.state
    }

    /// 直接投一件活给解码段（「重试失败项」「全量重建」走这条）。
    ///
    /// ⚠️ **只处理点名的 id**（§4.3 / `05` §1.13 边界）：这里没有一个
    /// 「按 folder_id 取所有待生成」的入口，就是为了防止误伤历史欠账。
    pub fn submit(&self, items: Vec<DecodeItem>) {
        self.state.enqueue(items);
    }

    /// 停掉流水线并等线程收尾。
    pub fn shutdown(self) {
        self.state.stop();
        drop(self.decode_tx);
        drop(self.store_tx);
        for h in self.handles {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[test]
    fn throttle_maps_to_worker_counts() {
        let s = PipelineState::new();
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .max(1);
        s.throttle(Throttle::Full);
        assert_eq!(s.workers(), (cores - 1).max(1));
        s.throttle(Throttle::Half);
        assert_eq!(s.workers(), (cores / 2).max(1));
        s.throttle(Throttle::Single);
        assert_eq!(s.workers(), 1, "设置为 1 后解码段必须只有一个线程");
        s.throttle(Throttle::Suspended);
        assert_eq!(s.workers(), 0);
    }

    #[test]
    fn suspended_throttle_keeps_the_queue_intact() {
        let s = PipelineState::new();
        s.enqueue(vec![DecodeItem {
            photo_id: 1,
            folder_id: 10,
            tier: 0,
            reason: ThumbReason::Discovered,
        }]);
        s.throttle(Throttle::Suspended);
        assert_eq!(s.workers(), 0);
        assert_eq!(
            s.queued_len(),
            1,
            "挂起不许丢队列（P-03：恢复后从原地继续）"
        );
        s.throttle(Throttle::Full);
        assert_eq!(s.queued_len(), 1);
        let item = s.pop().unwrap();
        assert_eq!(item.photo_id, 1);
    }

    #[test]
    fn visible_and_focus_items_come_out_first() {
        let s = PipelineState::new();
        let mk = |id: i64, folder: i64| DecodeItem {
            photo_id: id,
            folder_id: folder,
            tier: 0,
            reason: ThumbReason::Discovered,
        };
        s.enqueue(vec![mk(1, 10), mk(2, 20), mk(3, 20), mk(4, 99)]);
        // 焦点在 20，且 4 号在可见区
        s.set_focus(Some(20), &[4]);
        assert_eq!(s.pop().unwrap().photo_id, 4, "可见区最高优先级");
        assert_eq!(
            s.pop().unwrap().photo_id,
            2,
            "当前文件夹次之，同优先级按入队序"
        );
        assert_eq!(s.pop().unwrap().photo_id, 3);
        assert_eq!(s.pop().unwrap().photo_id, 1);

        // 换焦点：剩下的重算优先级，而不是清空
        s.enqueue(vec![mk(5, 10), mk(6, 77)]);
        s.set_focus(Some(77), &[]);
        assert_eq!(s.pop().unwrap().photo_id, 6, "切了文件夹就该先做新文件夹的");
        assert_eq!(s.pop().unwrap().photo_id, 5);
    }

    /// 端到端：遍历 → 解码 → 落库，断言**全部**条目都落到了库里，
    /// 且落库是按批提交的（不是一条一个事务）。
    #[test]
    fn pipeline_delivers_every_item_and_batches_commits() {
        let state = Arc::new(PipelineState::new());
        let remaining = Arc::new(AtomicUsize::new(600));
        let scan = {
            let remaining = Arc::clone(&remaining);
            Box::new(move || {
                let n = remaining.load(Ordering::Relaxed);
                if n == 0 {
                    return Vec::new();
                }
                let take = n.min(50);
                remaining.fetch_sub(take, Ordering::Relaxed);
                (0..take)
                    .map(|i| DecodeItem {
                        photo_id: i as i64,
                        folder_id: 1,
                        tier: 0,
                        reason: ThumbReason::Discovered,
                    })
                    .collect()
            }) as Box<dyn Fn() -> Vec<DecodeItem> + Send + Sync>
        };

        let decoded = Arc::new(AtomicUsize::new(0));
        let decode: DecodeFn = {
            let decoded = Arc::clone(&decoded);
            Box::new(move |photo_id, _tier| {
                decoded.fetch_add(1, Ordering::Relaxed);
                Some((format!("thumb-{photo_id}").into_bytes(), 64, 64))
            })
        };

        let commits = Arc::new(AtomicUsize::new(0));
        let stored = Arc::new(Mutex::new(Vec::new()));
        let store: StoreFn = {
            let commits = Arc::clone(&commits);
            let stored = Arc::clone(&stored);
            Box::new(move |batch: &[StoredItem]| {
                commits.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut g) = stored.lock() {
                    g.extend(batch.iter().map(|s| s.photo_id));
                }
            })
        };

        let p = Pipeline::start(Arc::clone(&state), scan, decode, store);
        // 等落库数追上（600 条，解码是空转，应该很快）
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while state.stored() < 600 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        p.shutdown();

        assert_eq!(state.stored(), 600, "一条都不能丢");
        let got = stored.lock().unwrap().clone();
        assert_eq!(got.len(), 600);
        let mut sorted = got.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            50, // 上限就是批内 id 数（远小于 600），写出来是为了让「去重」这件事显形
            "照片 id 是分批复用的，落库的应是去重后的批内 id"
        );
        // 批量提交：600 条不该产生 600 次提交
        let c = commits.load(Ordering::Relaxed);
        assert!(
            c > 0 && c <= 600 / DB_BATCH_ROWS + 24,
            "提交次数 {c} 应远小于条数"
        );
        assert_eq!(decoded.load(Ordering::Relaxed), 600);
    }

    #[test]
    fn bounded_channel_blocks_the_scanner_instead_of_buffering_everything() {
        // 解码故意什么都不做（不消费），通道满了之后遍历段必须**停住**，
        // 而不是把 10 万条全读进内存。
        let state = Arc::new(PipelineState::new());
        let produced = Arc::new(AtomicUsize::new(0));
        let scan = {
            let produced = Arc::clone(&produced);
            Box::new(move || {
                produced.fetch_add(50, Ordering::Relaxed);
                (0..50)
                    .map(|i| DecodeItem {
                        photo_id: i as i64,
                        folder_id: 1,
                        tier: 0,
                        reason: ThumbReason::Discovered,
                    })
                    .collect()
            }) as Box<dyn Fn() -> Vec<DecodeItem> + Send + Sync>
        };
        // 解码段用「挂起」让它不取活
        let decode: DecodeFn = Box::new(|_, _| None);
        let store: StoreFn = Box::new(|_| {});
        state.throttle(Throttle::Suspended);

        let p = Pipeline::start(Arc::clone(&state), scan, decode, store);
        thread::sleep(Duration::from_millis(300));
        let n = produced.load(Ordering::Relaxed);
        p.shutdown();

        // 有界通道 1024 + 解码池在途：生产量必须是**有上限**的，
        // 绝不能是「源文件有多少就生产多少」。
        assert!(
            n <= CHAN_SCAN_TO_DECODE + 4096,
            "生产了 {n} 条，说明遍历段没有被有界通道挡住"
        );
    }
}
