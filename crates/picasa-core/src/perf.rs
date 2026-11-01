//! 帧预算、帧时间直方图与「UI 线程阻塞 I/O」审计（M2.2 §3.3，PERF-05/09/10/18）。
//!
//! ## 为什么要这三个东西
//!
//! 「浏览不顺畅」这件事**没法靠肉眼看基线截图验收**，只能靠数字。而数字要可信，
//! 必须先解决三件事：
//!
//! 1. **每帧有预算**（[`FrameBudget`]）：一帧里能做多少事是**有上限**的，
//!    超了就推下一帧。没有预算，一次「顺手多上传两张纹理」就是一次掉帧，
//!    而且没人知道是哪两行代码干的。
//! 2. **帧时间要能自己举证**（[`FrameStats`]）：`perf_bench` 与 ⌥+点击的直方图
//!    读**同一份数据**，不各采一套（各采一套就会出现「基准说 18 ms、浮层说 30 ms」）。
//! 3. **阻塞 I/O 要有现场**（[`AuditLog`] + [`ui_guard`]）：R-M2-3 要求 UI 线程
//!    零阻塞 I/O。光靠 code review 守不住——一个 `stat` 藏在三层调用里照样卡 8 ms。
//!    所以每个 DB/文件入口都用 `ui_guard!` 裹上，超 2 ms 就记名 + 耗时。
//!
//! ## 为什么放在 `picasa-core` 而不是 `picasa-ui`
//!
//! 因为这个项目**很可能在没有窗口的环境里开发**（M2.1 就是在无 `/dev/dri` 的沙箱里做的）。
//! 这三样都不依赖 egui，可以在 `cargo test` 里直接断言——不能自动化验证的性能约束
//! 等于没有约束。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------- 常量（附录 A）

/// 每帧纹理上传槽位初值 / 下限 / 上限。
///
/// 4 是实测的折中：2 太保守（首屏铺不满，用户看到一片灰格子往上爬），
/// 8 会在弱机上把一帧拉到 30 ms 以上（GPU 时间是自适应规则的输入，见
/// [`FrameBudget::begin_frame`]）。
pub const UPLOAD_SLOTS_INIT: u8 = 4;
pub const UPLOAD_SLOTS_MIN: u8 = 2;
pub const UPLOAD_SLOTS_MAX: u8 = 8;

/// GPU 时间超过这个值 → 下一帧减一个槽位（说明上一帧已经压不住了）。
pub const GPU_MS_DEGRADE: f32 = 12.0;
/// GPU 时间低于这个值 → 累积一帧「余量」。
pub const GPU_MS_HEALTHY: f32 = 6.0;
/// 连续多少帧「余量」才加一个槽位。**必须连续**：单帧抖动就加会来回震荡，
/// 用户看到的是缩略图填充速度周期性变化。
pub const HEALTHY_FRAMES_TO_GROW: u8 = 10;

/// UI 线程单次操作超过多少毫秒记一条审计。
///
/// 2 ms 来自「60 Hz 一帧 16.7 ms」：一次操作吃掉 2 ms，同帧再来几次就该掉帧了。
/// 定得更严（0.5 ms）会淹没在噪声里，更松（5 ms）就漏掉真问题。
pub const UI_GUARD_THRESHOLD_MS: f64 = 2.0;

/// 帧时间直方图保留多久（秒）。
pub const FRAME_STATS_WINDOW_SECS: f64 = 60.0;
/// 环形缓冲容量：60 s × 144 Hz（高刷屏）还有富余，超出就丢最旧的。
const FRAME_STATS_CAP: usize = 8192;

/// 审计环形缓冲容量（附录 A：200 条）。
const AUDIT_CAP: usize = 200;

// ---------------------------------------------------------------- FrameBudget

/// 一帧能干多少活。每帧开头 [`FrameBudget::begin_frame`] 重置，
/// 之后每次要上传纹理先问 [`FrameBudget::try_upload`]、要查库先问 [`FrameBudget::try_db`]。
///
/// ## 自适应为什么用「上一帧的 GPU 时间」
///
/// 不用 CPU 时间：CPU 时间包含等待，而等待在弱机和强机上的含义完全不同。
/// GPU 时间是「这一帧真的画了多久」，是唯一跨机器可比的输入。
/// 规则刻意做成**减得快、加得慢**（减 1 帧生效，加要连续 10 帧）——
/// 抖动时先退让，稳定了再贪心。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameBudget {
    pub upload_slots: u8,
    pub db_query_used: bool,
    slots_left: u8,
    healthy_streak: u8,
}

impl Default for FrameBudget {
    fn default() -> Self {
        Self::new(UPLOAD_SLOTS_INIT)
    }
}

impl FrameBudget {
    pub fn new(init: u8) -> Self {
        let init = init.clamp(UPLOAD_SLOTS_MIN, UPLOAD_SLOTS_MAX);
        Self {
            upload_slots: init,
            db_query_used: false,
            slots_left: init,
            healthy_streak: 0,
        }
    }

    /// 新的一帧：重置配额，并按上一帧的 GPU 时间调整槽位。
    pub fn begin_frame(&mut self, last_gpu_ms: f32) {
        if last_gpu_ms > GPU_MS_DEGRADE {
            self.upload_slots = self.upload_slots.saturating_sub(1).max(UPLOAD_SLOTS_MIN);
            self.healthy_streak = 0;
        } else if last_gpu_ms < GPU_MS_HEALTHY {
            self.healthy_streak = self.healthy_streak.saturating_add(1);
            if self.healthy_streak >= HEALTHY_FRAMES_TO_GROW {
                self.upload_slots = self.upload_slots.saturating_add(1).min(UPLOAD_SLOTS_MAX);
                self.healthy_streak = 0;
            }
        } else {
            // 中间地带：既不退让也不贪心，但连续计数清零
            self.healthy_streak = 0;
        }
        self.slots_left = self.upload_slots;
        self.db_query_used = false;
    }

    /// 还能上传一张吗？能就**扣掉一个槽位**（调用方必须真的用掉它）。
    pub fn try_upload(&mut self) -> bool {
        if self.slots_left == 0 {
            return false;
        }
        self.slots_left -= 1;
        true
    }

    /// 本帧还能查一次库吗？**每帧只允许一次**（且必须是批量 `IN`）。
    ///
    /// 为什么是「一次」而不是「N 次」：R-M2-3 的原话是「UI 线程对 DB 只允许
    /// 每帧 ≤ 1 次批量只读查询」。一次一条的循环查询在 5 万张库上是几百次
    /// 索引查找，每一毫秒都在抢扫描线程持有的那把 DB 锁。
    pub fn try_db(&mut self) -> bool {
        if self.db_query_used {
            return false;
        }
        self.db_query_used = true;
        true
    }

    /// 本帧剩余上传槽位（诊断显示用）。
    pub fn slots_left(&self) -> u8 {
        self.slots_left
    }
}

// ----------------------------------------------------------------- FrameStats

/// 一帧的采样。带时间戳是为了按 60 s 窗口滚动淘汰（不是按条数猜）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    at_secs: f64,
    ms: f32,
}

/// 帧时间直方图（PERF-18）。
///
/// `⌥` + 点击状态栏入口的浮层与 `perf_bench --scroll` 都读这里，
/// 所以「界面上看到的 p99」与「CI 里判定的 p99」是同一个数。
///
/// ⚠️ 不 `derive(Debug)`：字段里有注入的时钟闭包，而 `dyn Fn` 没有 `Debug`。
/// 诊断要打印时用 [`FrameStats::summary_text`]。
pub struct FrameStats {
    samples: VecDeque<Sample>,
    now: Box<dyn Fn() -> f64 + Send + Sync>,
}

impl Default for FrameStats {
    fn default() -> Self {
        Self::new()
    }
}

/// 帧时间的四个分位（`⌥`+点击的浮层与 `perf_bench` 都用这四个数）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Percentiles {
    pub p50: f32,
    pub p95: f32,
    pub p99: f32,
    pub max: f32,
}

impl FrameStats {
    /// 用**单调时钟**计时（帧间隔必须用单调时钟；墙钟在休眠后会跳变，
    /// 一帧被算成几十秒，直方图就废了）。
    pub fn new() -> Self {
        let base = std::time::Instant::now();
        Self {
            samples: VecDeque::with_capacity(1024),
            now: Box::new(move || base.elapsed().as_secs_f64()),
        }
    }

    /// 测试用：注入时钟，让「60 s 窗口」不必真的等 60 s。
    pub fn with_clock(now: Box<dyn Fn() -> f64 + Send + Sync>) -> Self {
        Self {
            samples: VecDeque::with_capacity(1024),
            now,
        }
    }

    pub fn push(&mut self, ms: f32) {
        let at = (self.now)();
        self.samples.push_back(Sample { at_secs: at, ms });
        self.evict(at);
    }

    /// 记一帧（从「上一帧的开始」到「这一帧的开始」）。返回记下的毫秒数。
    pub fn tick(&mut self) -> f32 {
        let at = (self.now)();
        let ms = self
            .samples
            .back()
            .map(|s| ((at - s.at_secs) * 1000.0) as f32)
            .unwrap_or(0.0);
        self.samples.push_back(Sample { at_secs: at, ms });
        self.evict(at);
        ms
    }

    fn evict(&mut self, now: f64) {
        while let Some(front) = self.samples.front() {
            if now - front.at_secs <= FRAME_STATS_WINDOW_SECS
                && self.samples.len() <= FRAME_STATS_CAP
            {
                break;
            }
            self.samples.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// 窗口内的毫秒序列（升序）。**升序**是 p50/p95/p99 的前提，
    /// 排序只在取用时做一次，不影响 `push` 的开销（每帧都排序就本末倒置了）。
    fn sorted_ms(&self) -> Vec<f32> {
        let mut v: Vec<f32> = self.samples.iter().map(|s| s.ms).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        v
    }

    fn percentile(sorted: &[f32], p: f32) -> f32 {
        if sorted.is_empty() {
            return 0.0;
        }
        // 线性插值的下标：`p * (n-1)`。不取整到最近的样本——
        // n 很小时（刚启动）取整会让 p99 直接等于 max，读数虚高。
        let idx = p * (sorted.len() - 1) as f32;
        let lo = idx.floor() as usize;
        let hi = idx.ceil() as usize;
        if lo == hi {
            return sorted[lo];
        }
        let frac = idx - lo as f32;
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }

    pub fn percentiles(&self) -> Percentiles {
        let s = self.sorted_ms();
        if s.is_empty() {
            return Percentiles::default();
        }
        Percentiles {
            p50: Self::percentile(&s, 0.50),
            p95: Self::percentile(&s, 0.95),
            p99: Self::percentile(&s, 0.99),
            max: s[s.len() - 1],
        }
    }

    /// 直方图分桶：`(桶上界 ms, 条数)`。桶宽 4 ms，覆盖到 68 ms，
    /// 最后一桶是「> 68 ms」（掉帧到 15 fps 以下都归这里）。
    pub fn histogram(&self) -> Vec<(f32, usize)> {
        const BUCKETS: usize = 17;
        const WIDTH: f32 = 4.0;
        let mut out = vec![(0.0, 0usize); BUCKETS];
        for (i, slot) in out.iter_mut().enumerate() {
            slot.0 = ((i + 1) as f32) * WIDTH;
        }
        for s in &self.samples {
            let idx = ((s.ms / WIDTH).floor() as usize).min(BUCKETS - 1);
            out[idx].1 += 1;
        }
        out
    }

    /// 可复制为文本的摘要（浮层的「复制」按钮与 `perf_bench` 的 JSON 都用它）。
    pub fn summary_text(&self) -> String {
        let p = self.percentiles();
        format!(
            "帧数 {} · p50 {:.1}ms · p95 {:.1}ms · p99 {:.1}ms · max {:.1}ms",
            self.len(),
            p.p50,
            p.p95,
            p.p99,
            p.max
        )
    }
}

// ------------------------------------------------------------------- AuditLog

/// 一条「UI 线程干得太久」的记录。
#[derive(Clone, Debug, PartialEq)]
pub struct AuditEntry {
    /// 调用点名（`ui_guard!("photos.reload", { ... })`）
    pub name: String,
    pub ms: f64,
    /// 单调时钟秒数
    pub at_secs: f64,
}

/// UI 线程审计环形缓冲（PERF-10）。
///
/// release 下也保留：一次 `Instant::now()` 是 20 ns 量级，
/// 而它守的是「UI 线程不许干重活」这条红线——省这点开销不值得。
///
/// ⚠️ 同上：字段含注入的时钟闭包，不 `derive(Debug)`。
pub struct AuditLog {
    entries: Mutex<VecDeque<AuditEntry>>,
    threshold_ms: f64,
    clock: Box<dyn Fn() -> f64 + Send + Sync>,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditLog {
    pub fn new() -> Self {
        let base = std::time::Instant::now();
        Self {
            entries: Mutex::new(VecDeque::with_capacity(64)),
            threshold_ms: UI_GUARD_THRESHOLD_MS,
            clock: Box::new(move || base.elapsed().as_secs_f64()),
        }
    }

    /// 测试用：注入时钟与阈值。
    pub fn with(threshold_ms: f64, clock: Box<dyn Fn() -> f64 + Send + Sync>) -> Self {
        Self {
            entries: Mutex::new(VecDeque::with_capacity(64)),
            threshold_ms,
            clock,
        }
    }

    /// 记一条（**只在超过阈值时被宏调用**）。
    pub fn record(&self, name: &str, ms: f64) {
        if ms <= self.threshold_ms {
            return;
        }
        let at_secs = (self.clock)();
        let Ok(mut q) = self.entries.lock() else {
            return;
        };
        q.push_back(AuditEntry {
            name: name.to_string(),
            ms,
            at_secs,
        });
        while q.len() > AUDIT_CAP {
            q.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.entries.lock().map(|q| q.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn snapshot(&self) -> Vec<AuditEntry> {
        self.entries
            .lock()
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// 清空（`TC-M22-PERF-007` 跑之前先清一次，避免历史污染判定）。
    pub fn clear(&self) {
        if let Ok(mut q) = self.entries.lock() {
            q.clear();
        }
    }
}

/// 进程级审计日志：`ui_guard!` 默认往这里记。
fn global() -> &'static AuditLog {
    static LOG: OnceLock<AuditLog> = OnceLock::new();
    LOG.get_or_init(AuditLog::new)
}

/// `ui_guard!` 的落点（宏展开会调它，不要直接调）。
pub fn audit_record(name: &str, ms: f64) {
    global().record(name, ms);
}

/// 取一份审计快照（诊断页「存储与诊断」标签显示用）。
pub fn audit_snapshot() -> Vec<AuditEntry> {
    global().snapshot()
}

pub fn audit_clear() {
    global().clear();
}

/// 把一段**只允许很短**的代码裹起来，超时就记一条审计。
///
/// ```ignore
/// let rows = ui_guard!("db.photos", { db.photos(None, sort, 2000) })?;
/// ```
///
/// ⚠️ 宏**不改变**返回值与错误传播：它只是计时 + 记名。
/// 所以把它套在任何同步块上都是安全的，删掉也不影响语义。
#[macro_export]
macro_rules! ui_guard {
    ($name:expr, $body:block) => {{
        let __t0 = ::std::time::Instant::now();
        let __out = $body;
        let __ms = __t0.elapsed().as_secs_f64() * 1000.0;
        if __ms > $crate::perf::UI_GUARD_THRESHOLD_MS {
            $crate::perf::audit_record($name, __ms);
        }
        __out
    }};
}

// ------------------------------------------------------------------ 滚动方向

/// 最近若干帧的滚动位移，用来判「往哪个方向滚、滚多快」（PERF-06/07）。
///
/// 为什么记 5 帧而不是 1 帧：单帧位移在「用户甩了一下又停住」时会给出
/// 一个极大的瞬时速度，于是立刻触发降级/预取，而用户其实已经停了。
/// 5 帧（约 80 ms）足够平滑掉这种抖动。
#[derive(Debug)]
pub struct ScrollTracker {
    recent: VecDeque<f32>,
}

impl Default for ScrollTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ScrollTracker {
    pub fn new() -> Self {
        Self {
            recent: VecDeque::with_capacity(8),
        }
    }

    /// 记一帧的位移（**带符号**：向下为正）。
    pub fn note(&mut self, delta_px: f32) {
        self.recent.push_back(delta_px);
        if self.recent.len() > 5 {
            self.recent.pop_front();
        }
    }

    /// 平均速度（px/帧，带符号）。没有样本时为 0。
    pub fn velocity(&self) -> f32 {
        if self.recent.is_empty() {
            return 0.0;
        }
        self.recent.iter().sum::<f32>() / self.recent.len() as f32
    }

    /// 方向：`1` 向下、`-1` 向上、`0` 静止。
    pub fn direction(&self) -> i8 {
        let v = self.velocity();
        if v > 1.0 {
            1
        } else if v < -1.0 {
            -1
        } else {
            0
        }
    }

    /// 是否在快速滚动（PERF-07 的降级判据）。
    pub fn is_fast(&self, threshold_px_per_frame: f32) -> bool {
        self.velocity().abs() > threshold_px_per_frame
    }

    /// 按速度决定预取几屏（PERF-06）。越快越远，上限 2 屏。
    pub fn prefetch_screens(&self, viewport_px: f32, max_screens: u8) -> u8 {
        if self.recent.is_empty() || viewport_px <= 0.0 {
            return 0;
        }
        let v = self.velocity().abs();
        let per_frame = v / viewport_px;
        let n = (per_frame * 2.0).ceil() as u8;
        n.clamp(0, max_screens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_enforces_upload_slots_and_one_db_query_per_frame() {
        let mut b = FrameBudget::new(4);
        b.begin_frame(5.0);
        assert!(b.try_db());
        assert!(!b.try_db(), "每帧只允许一次 DB 查询");
        for _ in 0..4 {
            assert!(b.try_upload());
        }
        assert!(!b.try_upload(), "槽位用完了就不能再上传");

        b.begin_frame(5.0);
        assert!(b.try_db(), "新的一帧重置配额");
        assert!(b.try_upload());
    }

    #[test]
    fn budget_shrinks_fast_and_grows_slowly() {
        let mut b = FrameBudget::new(4);
        // 一帧很慢 → 立刻减 1
        b.begin_frame(20.0);
        assert_eq!(b.upload_slots, 3);

        // 连续 9 帧健康还不够加回去（必须满 10 帧）
        for _ in 0..9 {
            b.begin_frame(2.0);
        }
        assert_eq!(b.upload_slots, 3, "差一帧都不许加");
        b.begin_frame(2.0);
        assert_eq!(b.upload_slots, 4);

        // 加到上限就不再加
        for _ in 0..100 {
            b.begin_frame(2.0);
        }
        assert_eq!(b.upload_slots, UPLOAD_SLOTS_MAX);
        // 减到下限就不再减
        for _ in 0..100 {
            b.begin_frame(50.0);
        }
        assert_eq!(b.upload_slots, UPLOAD_SLOTS_MIN);
    }

    /// 测试用的可注入时钟（必须 `Send + Sync`，所以用 `Arc<Mutex>` 而不是 `Cell`）。
    fn fake_clock() -> (
        std::sync::Arc<Mutex<f64>>,
        Box<dyn Fn() -> f64 + Send + Sync>,
    ) {
        let t = std::sync::Arc::new(Mutex::new(0.0f64));
        let weak = std::sync::Arc::clone(&t);
        (t, Box::new(move || *weak.lock().unwrap()))
    }

    #[test]
    fn frame_stats_computes_percentiles_and_evicts_old_samples() {
        let (t, clock) = fake_clock();
        let mut s = FrameStats::with_clock(clock);
        // 100 帧：99 帧 10 ms，1 帧 200 ms
        for i in 0..100 {
            *t.lock().unwrap() = i as f64 * 0.1;
            s.push(if i == 50 { 200.0 } else { 10.0 });
        }
        let p = s.percentiles();
        assert!((p.p50 - 10.0).abs() < 0.5, "p50 应是 10ms，实得 {}", p.p50);
        assert!(
            (p.p95 - 10.0).abs() < 0.5,
            "p95 仍是 10ms：只有 1% 的帧是 200ms"
        );
        assert_eq!(p.max, 200.0);

        // 推进到 100 秒：全部旧样本过期
        *t.lock().unwrap() = 100.0;
        s.push(7.0);
        assert_eq!(s.len(), 1, "超过 60s 窗口的样本必须被淘汰");
        assert_eq!(s.percentiles().max, 7.0);
    }

    #[test]
    fn frame_stats_histogram_buckets_every_sample() {
        let mut s = FrameStats::with_clock(Box::new(|| 0.0));
        for ms in [1.0, 3.0, 5.0, 9.0, 100.0] {
            s.push(ms);
        }
        let h = s.histogram();
        let total: usize = h.iter().map(|(_, n)| n).sum();
        assert_eq!(total, 5, "每个样本都必须落进某个桶，一个都不能丢");
        // 100ms 落在最后一个（>68ms）桶
        assert_eq!(h.last().unwrap().1, 1);
        assert!(s.summary_text().contains("p99"));
    }

    #[test]
    fn audit_log_only_records_over_threshold_and_rings() {
        let (_t, clock) = fake_clock();
        let log = AuditLog::with(2.0, clock);
        log.record("fast.enough", 1.5);
        assert!(log.is_empty(), "没超阈值不该记");

        log.record("db.photos", 8.0);
        log.record("db.tree", 3.0);
        assert_eq!(log.len(), 2);
        assert_eq!(log.snapshot()[0].name, "db.photos");

        // 环形缓冲：超过 200 条丢最旧的
        for i in 0..300 {
            log.record(&format!("op{i}"), 5.0);
        }
        assert_eq!(log.len(), AUDIT_CAP);
        let snap = log.snapshot();
        assert_eq!(
            snap[0].name, "op100",
            "最旧的 100 条应被丢掉（300 - 200 = 100）"
        );
        log.clear();
        assert!(log.is_empty());
    }

    #[test]
    fn ui_guard_macro_passes_value_through_and_records() {
        // 宏只做「计时 + 记名」，不改变返回值与错误传播
        let v = ui_guard!("unit.test", { 1 + 1 });
        assert_eq!(v, 2);
        let err: Result<i32, String> = ui_guard!("unit.err", { Err("boom".to_string()) });
        assert_eq!(err.unwrap_err(), "boom", "错误传播不受宏影响");
    }

    #[test]
    fn scroll_tracker_smooths_direction_and_speed() {
        let mut s = ScrollTracker::new();
        assert_eq!(s.direction(), 0);
        assert_eq!(s.prefetch_screens(800.0, 2), 0, "没样本时不预取");

        for _ in 0..5 {
            s.note(120.0);
        }
        assert_eq!(s.direction(), 1);
        assert!(!s.is_fast(400.0), "均速 120px 不算快速滚动");
        assert_eq!(s.prefetch_screens(800.0, 2), 1);

        // 甩一下：平均 500px/帧 → 快速，预取拉满
        let mut f = ScrollTracker::new();
        for _ in 0..5 {
            f.note(500.0);
        }
        assert!(f.is_fast(400.0));
        assert_eq!(f.prefetch_screens(800.0, 2), 2, "上限 2 屏");

        // 方向反转：只保留最近 5 帧，旧的正向位移被挤出
        for _ in 0..5 {
            f.note(-500.0);
        }
        assert_eq!(f.direction(), -1, "反转后必须立刻改向");
    }
}
