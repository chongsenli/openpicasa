//! 纹理预算与内存自保护（`07` §6.1 PERF-08 / §5.2 SCALE-08）。
//!
//! ## 为什么需要「应用层」的纹理预算
//!
//! egui 的 `TextureManager` 会**自己**管生命周期，但它不知道「这张纹理离屏幕有多远」，
//! 也不接受「按字节淘汰」这种策略——它只有引用计数。于是有两种坏结局：
//!
//! 1. 我们主动 `drop(handle)` 太多 → 每帧重新上传，滚动时纹理狂刷；
//! 2. 我们不敢 `drop` → 25 万张滚一圈，纹理堆到几个 GB，进程被系统杀掉。
//!
//! 折中是**在应用层**维护一份账本：登记每张纹理的字节数（`w*h*4`）与「屏距」
//! （这张照片离当前视口多少屏）。超预算时在**帧末**释放「屏距 ≥ 3」的 handle，
//! **屏距 0 永不释放**——正在看的图不能被自己淘汰掉。
//!
//! ## 屏距为什么是 3 而不是「离屏就丢」
//!
//! 用户在栅格里上下滚动时，刚滚出去的那几张随时会滚回来。丢早了就是
//! 「往回滚一格，所有图重新解码一遍」——比不丢还糟。3 屏 ≈ 180 行的余量，
//! 够覆盖一次快速来回。

use std::collections::HashMap;

/// 纹理缓存预算上限（MB）——`07` 附录 A：`min(384, phys_mem / 32)`。
///
/// 为什么是 `phys_mem / 32`：一台 16 GB 的机器给 512 MB 太狠（用户还要跑别的），
/// 8 GB 的机器给 256 MB 才合适。除以 32 恰好落在这两个数之间，
/// 而且**不随机器型号硬编码**——换台机器不必改代码。
pub fn texture_cache_budget_mb(phys_mem_mb: u64) -> u32 {
    let by_mem = (phys_mem_mb / 32) as u32;
    by_mem.clamp(64, 384)
}

/// 内存软上限（MB）：超过 → 纹理降到最低档、行窗口缩到 1 屏、解码池 `Half`。
pub const MEM_SOFT_LIMIT_MB: u64 = 2048;
/// 内存硬上限（MB）：超过 → 解码池 `Suspended` 并记诊断。**不退出进程**。
pub const MEM_HARD_LIMIT_MB: u64 = 2458;

/// 屏距达到多少就开始参与淘汰（`07` §6.1 PERF-08：**屏距 ≥ 3 优先**）。
pub const EVICT_SCREEN_DISTANCE: u32 = 3;

/// 屏距为 0（正在看）的纹理**永不释放**。
pub const NEVER_EVICT_DISTANCE: u32 = 0;

/// 一张纹理的账目。
#[derive(Clone, Copy, Debug)]
struct Entry {
    bytes: u32,
    /// 屏距：0 = 当前视口内，1/2/3 = 附近，`u32::MAX` = 未知（按最远处理）
    screen_distance: u32,
    /// 帧号：同屏距下先淘汰最久没被看到的
    last_seen_frame: u64,
}

/// 纹理预算账本（`07` §9.4）。
///
/// 它**不持有** `TextureHandle`——handle 在 `TextureCache` 里，这里只记账。
/// 分开的理由：账本要能被单元测试直接构造与断言（`TextureHandle` 需要 `egui::Context`），
/// 而「预算算得对不对」这件事跟 GPU 无关。
#[derive(Debug)]
pub struct TextureBudget {
    /// 预算（MB）
    pub limit_mb: u32,
    entries: HashMap<i64, Entry>,
    used_bytes: u64,
    frame: u64,
}

impl TextureBudget {
    pub fn new(limit_mb: u32) -> Self {
        Self {
            limit_mb,
            entries: HashMap::new(),
            used_bytes: 0,
            frame: 0,
        }
    }

    /// 按物理内存算出预算（`texture_cache_budget_mb`）。
    pub fn from_physical_memory(phys_mem_mb: u64) -> Self {
        Self::new(texture_cache_budget_mb(phys_mem_mb))
    }

    /// 已用（MB，向上取整——显示「384 MB」而不是「383 MB」更像真的）。
    pub fn used_mb(&self) -> u32 {
        self.used_bytes.div_ceil(1024 * 1024) as u32
    }

    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 超预算了吗（`07` §6.1：超预算在帧末释放屏距 ≥ 3 的 handle）。
    pub fn over_budget(&self) -> bool {
        self.used_bytes > u64::from(self.limit_mb) * 1024 * 1024
    }

    /// 预算使用率（诊断浮层用）。
    pub fn fill_ratio(&self) -> f32 {
        let cap = u64::from(self.limit_mb) * 1024 * 1024;
        if cap == 0 {
            return 1.0;
        }
        (self.used_bytes as f32 / cap as f32).clamp(0.0, 1.0)
    }

    /// 开始新的一帧（屏距与「最近看到」都按帧算）。
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// 登记一张纹理（上传成功后调）。
    ///
    /// `bytes` 由调用方给（`w * h * 4`）。重复登记同一 `photo_id` 会**替换**旧账，
    /// 不会重复计费——换档重传是最常见的情况。
    pub fn register(&mut self, photo_id: i64, bytes: u32, screen_distance: u32) {
        self.register_at_frame(photo_id, bytes, screen_distance, self.frame);
    }

    fn register_at_frame(&mut self, photo_id: i64, bytes: u32, distance: u32, frame: u64) {
        if let Some(old) = self.entries.remove(&photo_id) {
            self.used_bytes = self.used_bytes.saturating_sub(u64::from(old.bytes));
        }
        self.entries.insert(
            photo_id,
            Entry {
                bytes,
                screen_distance: distance,
                last_seen_frame: frame,
            },
        );
        self.used_bytes += u64::from(bytes);
    }

    /// 纹理被释放了（无论谁释放的，都要销账，否则预算会只涨不落）。
    pub fn forget(&mut self, photo_id: i64) {
        if let Some(old) = self.entries.remove(&photo_id) {
            self.used_bytes = self.used_bytes.saturating_sub(u64::from(old.bytes));
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.used_bytes = 0;
    }

    /// 本帧看到了哪些照片（屏幕距离由调用方算好）。
    ///
    /// 这个「一次性告知整屏」而不是「逐张 touch」的接口，是为了让 UI 侧
    /// **每帧只算一次**视口→下标的映射，而不是每张纹理各算一次。
    pub fn note_visible(&mut self, distances: impl IntoIterator<Item = (i64, u32)>) {
        let frame = self.frame;
        for (photo_id, distance) in distances {
            if let Some(e) = self.entries.get_mut(&photo_id) {
                e.screen_distance = distance;
                e.last_seen_frame = frame;
            }
        }
    }

    /// 帧末淘汰（`07` §6.1 PERF-08）。
    ///
    /// 返回**应该 drop 掉的 photo_id 列表**，由调用方真正 `drop(TextureHandle)`。
    /// 为什么分两步：这里不持有 handle，而且 drop 必须发生在**帧末**
    /// （帧中 drop 会让本帧剩下的绘制拿到失效的 `TextureId`）。
    ///
    /// 规则，按顺序：
    /// 1. 先淘汰屏距最远的；
    /// 2. 同屏距下淘汰最久没被看到的；
    /// 3. **屏距 0 永不淘汰**——哪怕还超预算（宁可这一帧多占一点，
    ///    也不能把用户正在看的图变成灰块）。
    pub fn evict_end_of_frame(&mut self) -> Vec<i64> {
        if !self.over_budget() {
            return Vec::new();
        }
        let cap = u64::from(self.limit_mb) * 1024 * 1024;

        // 候选：屏距 ≥ EVICT_SCREEN_DISTANCE 的
        let mut candidates: Vec<(i64, u32, u64)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.screen_distance >= EVICT_SCREEN_DISTANCE)
            .map(|(id, e)| (*id, e.screen_distance, e.last_seen_frame))
            .collect();
        // 远的先淘汰；同距离时旧的先淘汰
        candidates.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.2.cmp(&b.2))
                // id 兜底，保证顺序确定（用例可重复）
                .then_with(|| a.0.cmp(&b.0))
        });

        let mut freed = 0u64;
        let mut out = Vec::new();
        for (id, _dist, _seen) in candidates {
            if self.used_bytes.saturating_sub(freed) <= cap {
                break;
            }
            if let Some(e) = self.entries.get(&id) {
                freed += u64::from(e.bytes);
                out.push(id);
            }
        }
        // 销账推迟到调用方真的 drop 之后？不——调用方紧接着就 drop，
        // 这里先销账以免下一次 `over_budget()` 把同一批再算一遍。
        for id in &out {
            self.forget(*id);
        }
        out
    }

    /// 屏距分布（诊断浮层展示「0 屏 N 张 / 1 屏 M 张」）。
    pub fn distance_histogram(&self) -> [usize; 4] {
        let mut out = [0usize; 4];
        for e in self.entries.values() {
            let idx = (e.screen_distance as usize).min(3);
            out[idx] += 1;
        }
        out
    }
}

/// 内存压力档位（`07` §5.2 SCALE-08）。
///
/// 从 `Normal` 到 `Suspended` 是**单向升级、逐级回落**：一帧超限就降级，
/// 连续 N 帧低水位才升级。加回滞的理由与缩放滑块相同——
/// 在阈值上反复横跳会让画面一会儿清晰一会儿糊。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemPressure {
    /// 正常：纹理按预算、行窗口按 2 屏、解码池全速
    Normal,
    /// 软超限：纹理降到最低档、行窗口缩到 1 屏、解码池 `Half`
    Soft,
    /// 硬超限：解码池 `Suspended`，只画已有纹理
    Hard,
}

impl MemPressure {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "正常",
            Self::Soft => "内存偏高",
            Self::Hard => "内存吃紧",
        }
    }

    /// 行窗口缩到几屏（`07` SCALE-08：软超限时缩到 1 屏）。
    pub fn row_window_screens(self) -> usize {
        match self {
            Self::Normal => 2,
            Self::Soft | Self::Hard => 1,
        }
    }

    /// 纹理预算档位（软超限降到最低档）。
    pub fn texture_budget_mb(self, base: u32) -> u32 {
        match self {
            Self::Normal => base,
            Self::Soft | Self::Hard => base.min(64),
        }
    }

    /// 解码池是否继续接活。
    pub fn decode_enabled(self) -> bool {
        self != Self::Hard
    }
}

/// 内存压力判断器（带回滞）。
///
/// ## 为什么回滞不能省
///
/// 内存占用在阈值附近抖动是常态（一次 GC、一次缩略图批量落库就能让 RSS 涨落 50 MB）。
/// 没有回滞的话，`MemPressure` 会以帧率频率在 `Normal`/`Soft` 之间跳，
/// 用户看到的是「画面一会儿清晰一会儿糊」，而且纹理会反复被淘汰重建。
#[derive(Debug)]
pub struct MemGuard {
    pressure: MemPressure,
    /// 连续多少帧在「可升级」水位
    good_frames: u32,
    /// 连续多少帧在「不变」水位（回滞带内不动）
    hold_frames: u32,
}

/// 从「软」回落到「正常」需要连续多少帧低于软上限的 90%。
pub const MEM_RECOVER_FRAMES: u32 = 30;
/// 回滞带：低于上限的这个比例才算「可以升级回来」。
pub const MEM_RECOVER_RATIO: f64 = 0.90;

impl Default for MemGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl MemGuard {
    pub fn new() -> Self {
        Self {
            pressure: MemPressure::Normal,
            good_frames: 0,
            hold_frames: 0,
        }
    }

    pub fn pressure(&self) -> MemPressure {
        self.pressure
    }

    /// 喂一次当前 `phys_footprint`（MB），返回更新后的档位。
    ///
    /// ## 语义（三条，缺一条就会横跳）
    ///
    /// 1. **变差立刻生效**：低于硬上限的读数下一秒就可能变成硬超限，
    ///    等几帧再降级等于让进程顶着 OOM 再跑一帧。
    /// 2. **变好必须连续 [`MEM_RECOVER_FRAMES`] 帧**：RSS 抖 50 MB 是常态。
    /// 3. **一次只回一级**：从 `Hard` 直接回 `Normal` 会在下一秒又冲上去，
    ///    表现是「画面一会儿清晰一会儿糊」。
    ///
    /// 注意「回一级」是**相对当前档位**算的：从 `Hard` 往下回时，
    /// 只要读数落在「软」水位以下（含回滞带以外的正常水位）都算「可以变好」，
    /// 够帧数后落到 `Soft`；要再往 `Normal` 走，需要再凑一轮。
    pub fn update(&mut self, footprint_mb: u64) -> MemPressure {
        let target = if footprint_mb >= MEM_HARD_LIMIT_MB {
            MemPressure::Hard
        } else if footprint_mb >= MEM_SOFT_LIMIT_MB {
            MemPressure::Soft
        } else if footprint_mb < (MEM_SOFT_LIMIT_MB as f64 * MEM_RECOVER_RATIO) as u64 {
            MemPressure::Normal
        } else {
            // 回滞带内（软上限的 90%～100%）：保持现状，且**不累计**回升帧数——
            // 这个区间正是「可能马上又超」的区间，不该被当成恢复的证据。
            self.hold_frames = self.hold_frames.saturating_add(1);
            self.good_frames = 0;
            return self.pressure;
        };

        if target > self.pressure {
            // 变差：立刻，且丢掉之前攒的恢复帧数
            self.pressure = target;
            self.good_frames = 0;
            return self.pressure;
        }

        if target == self.pressure {
            // 与当前档位一致：什么都不用做
            self.good_frames = 0;
            return self.pressure;
        }

        // target < pressure：读数比当前档位好 → 攒帧数，够了才**升一级**
        self.good_frames = self.good_frames.saturating_add(1);
        if self.good_frames >= MEM_RECOVER_FRAMES {
            self.pressure = match self.pressure {
                MemPressure::Hard => MemPressure::Soft,
                MemPressure::Soft => MemPressure::Normal,
                MemPressure::Normal => MemPressure::Normal,
            };
            self.good_frames = 0;
        }
        self.pressure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mb(n: u32) -> u32 {
        n * 1024 * 1024
    }

    #[test]
    fn budget_scales_with_physical_memory_and_is_capped() {
        // `min(384, phys/32)`，地板 64
        assert_eq!(texture_cache_budget_mb(16 * 1024), 384, "16 GB → 512 被压到 384");
        assert_eq!(texture_cache_budget_mb(8 * 1024), 256);
        assert_eq!(texture_cache_budget_mb(4 * 1024), 128);
        assert_eq!(texture_cache_budget_mb(1024), 64, "地板 64，不能算出 32");
    }

    #[test]
    fn registering_and_forgetting_keeps_the_ledger_balanced() {
        let mut b = TextureBudget::new(100);
        b.register(1, mb(10), 0);
        b.register(2, mb(20), 1);
        assert_eq!(b.used_mb(), 30);
        assert_eq!(b.len(), 2);

        b.forget(1);
        assert_eq!(b.used_mb(), 20);
        assert_eq!(b.len(), 1);

        // 重复登记同一张：替换而不是叠加（换档重传就是这个场景）
        b.register(2, mb(5), 1);
        assert_eq!(b.used_mb(), 5, "重复登记必须替换旧账");
    }

    #[test]
    fn eviction_takes_the_farthest_first() {
        let mut b = TextureBudget::new(10);
        b.begin_frame();
        b.register(0, mb(4), 0); // 正在看
        b.register(5, mb(4), 5); // 很远
        b.register(2, mb(4), 2); // 近，但没到淘汰线
        b.register(9, mb(4), 9); // 最远
        assert!(b.over_budget());

        let evicted = b.evict_end_of_frame();
        assert!(evicted.contains(&9), "最远的必须先走：{evicted:?}");
        assert!(!evicted.contains(&0), "★ 屏距 0 永不释放");
        assert!(!evicted.contains(&2), "屏距 2 没到淘汰线（{EVICT_SCREEN_DISTANCE}）");
        // 淘汰够了就停
        assert!(b.used_bytes() <= u64::from(b.limit_mb) * 1024 * 1024);
    }

    #[test]
    fn on_screen_textures_are_never_evicted_even_when_over_budget() {
        // 极端情况：一屏全是 4K 大图，屏距全 0，预算必然超。
        // 这时候**宁可超预算**，也不能把正在看的图变成灰块。
        let mut b = TextureBudget::new(1);
        b.begin_frame();
        for id in 0..10 {
            b.register(id, mb(1), 0);
        }
        assert!(b.over_budget());
        let evicted = b.evict_end_of_frame();
        assert!(evicted.is_empty(), "屏距 0 一个都不许淘汰：{evicted:?}");
    }

    #[test]
    fn no_eviction_when_under_budget() {
        let mut b = TextureBudget::new(100);
        b.register(1, mb(10), 9);
        assert!(b.evict_end_of_frame().is_empty(), "没超预算不该淘汰任何东西");
    }

    #[test]
    fn note_visible_updates_distance_so_a_returning_row_is_protected() {
        let mut b = TextureBudget::new(10);
        b.begin_frame();
        b.register(1, mb(9), 5);
        // 用户滚回来，这张变成屏距 0
        b.begin_frame();
        b.note_visible([(1, 0)]);
        b.register(2, mb(9), 5); // 攒超预算
        let evicted = b.evict_end_of_frame();
        assert!(!evicted.contains(&1), "刚回到屏幕内的不该被淘汰");
        assert!(evicted.contains(&2));
    }

    #[test]
    fn distance_histogram_counts_buckets() {
        let mut b = TextureBudget::new(100);
        b.begin_frame();
        b.register(1, 1, 0);
        b.register(2, 1, 0);
        b.register(3, 1, 2);
        b.register(4, 1, 99);
        assert_eq!(b.distance_histogram(), [2, 0, 1, 1]);
    }

    #[test]
    fn mem_guard_escalates_immediately() {
        let mut g = MemGuard::new();
        assert_eq!(g.update(100), MemPressure::Normal);
        // 一帧冲上硬上限 → 立刻 Hard（不能等，进程可能下一秒被杀）
        assert_eq!(g.update(MEM_HARD_LIMIT_MB + 1), MemPressure::Hard);
    }

    #[test]
    fn mem_guard_de_escalates_one_step_at_a_time_with_hysteresis() {
        let mut g = MemGuard::new();
        g.update(MEM_HARD_LIMIT_MB + 1);
        assert_eq!(g.pressure(), MemPressure::Hard);

        // 掉到「软」水位：读数比 Hard 好，但**要凑够帧数才回**，且只回一级。
        for _ in 0..MEM_RECOVER_FRAMES - 1 {
            assert_eq!(
                g.update(MEM_SOFT_LIMIT_MB + 10),
                MemPressure::Hard,
                "帧数没凑够，不许回退"
            );
        }
        assert_eq!(
            g.update(MEM_SOFT_LIMIT_MB + 10),
            MemPressure::Soft,
            "凑够帧数只回一级：Hard → Soft"
        );

        // 掉到正常水位：再凑一轮才到 Normal（不会一步回全速）
        for _ in 0..MEM_RECOVER_FRAMES - 1 {
            assert_eq!(g.update(100), MemPressure::Soft, "帧数没凑够不回退");
        }
        assert_eq!(g.update(100), MemPressure::Normal, "凑够帧数升一级到 Normal");
    }

    #[test]
    fn mem_guard_holds_steady_inside_the_hysteresis_band() {
        // 软上限的 90%～100% 之间：不动。否则 RSS 抖动会让画面反复变糊。
        let mut g = MemGuard::new();
        let band = (MEM_SOFT_LIMIT_MB as f64 * 0.95) as u64;
        for _ in 0..100 {
            assert_eq!(g.update(band), MemPressure::Normal, "回滞带内必须保持不动");
        }
    }

    #[test]
    fn pressure_controls_reduce_resources() {
        assert_eq!(MemPressure::Normal.row_window_screens(), 2);
        assert_eq!(MemPressure::Soft.row_window_screens(), 1, "SCALE-08：软超限缩到 1 屏");
        assert_eq!(MemPressure::Soft.texture_budget_mb(384), 64, "纹理降到最低档");
        assert!(!MemPressure::Hard.decode_enabled(), "硬超限停解码");
        assert!(MemPressure::Soft.decode_enabled(), "软超限仍解码（只是减半）");
    }

    #[test]
    fn zero_limit_does_not_divide_by_zero() {
        let b = TextureBudget::new(0);
        assert_eq!(b.fill_ratio(), 1.0);
    }
}
