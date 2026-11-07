//! 缩略图尺寸滑块（`07-A` §2 ZOOM，P-57 ② 落定为 22～512 px）。
//!
//! ## 为什么「连续尺寸 + 离散档位」需要专门的逻辑
//!
//! 存储档位**冻结为三档**（`07` §6.2：T0/T1/T2 的像素与 `thumbs` 结构不改）。
//! 而滑块是**连续**的（22～512 逻辑 px）。于是每拖动 1 px，理论上都可能要换一次纹理。
//!
//! 三条规则缺一条就会被评审打回：
//!
//! 1. **回滞**（HC-1）：升档与降档的阈值不同（±8 px），否则滑块停在边界附近
//!    每帧都在换纹理，画面闪、GPU 忙。
//! 2. **拖动中不换纹理**（HC-2）：拖动期间只改布局的格尺寸，纹理用现有的拉伸显示。
//!    松手（或值稳定 150 ms）后才按新档位请求。拖动期间**零 DB 读、零解码投递**。
//! 3. **锚点**（HC-3）：取拖动前视口顶部那张照片的 `photo_id`，重建后按
//!    `index_of(photo_id)` 恢复滚动位置。这条与 M2.2「切档不丢锚点」是同一机制，
//!    只是从「离散三档」变成「连续」。
//!
//! ## 22 px 的真实代价（`08-D` §A-2.2，P-57 ② 决定进 M3）
//!
//! 4K 全屏 17 000 格 × T0（512²×4 = 1 MB）会直接撞纹理预算。所以 ≤ 48 px 走
//! **微缩图集**：从 T0 下采样到 32² 物理 px（4 KB），打包进 4096² 图集纹理。
//! 17 000 格 ≈ 2 张图集，纹理占用 ≤ 200 MB。
//!
//! ⚠️ **图集本身在 `picasa-ui` 的渲染层实现**（需要 `TextureHandle`）。
//! 本模块只负责**口径**：档位映射、回滞、吸附、锚点，以及「≤48 px 走图集」这个判定。
//! 这个分工的理由与 `TextureBudget` 相同——策略要能脱离 GPU 单测。

/// 滑块下限（逻辑 px，P-57 ②：M3 即为 22，不再有 48 的过渡值）。
pub const CELL_MIN_PX: f32 = 22.0;
/// 滑块上限（逻辑 px）。
///
/// 512 = T2 档物理边长 ÷ HiDPI 2×：2× 屏上 512 逻辑 = 1024 物理，**刚好吃满 T2**，
/// 不出现放大虚像；1× 屏上是 T2 下采样，清晰。再大就是查看器（分块纹理）的事。
pub const CELL_MAX_PX: f32 = 512.0;

/// 三个刻度点（对应原版 S / M / L）。
///
/// 注意：这三个点**不是**档位边界，只是「吸附点 + 工具栏循环按钮的目标」。
/// 档位边界是 [`tier_boundaries`]。
pub const SNAP_POINTS: [f32; 3] = [48.0, 128.0, 256.0];

/// 默认刻度（`07-A` §2.3：`⌘0` 回到它）。
///
/// M4.1 `M41-UI-03`：由 128 降到 **96**，让全新库 / `⌘0` 复位时一屏能看到更多行
/// （行距 162 → 121 px）。96 本就在 [`LOG_STEPS`] 里，不新增档位。
/// 老用户已保存的 `grid_cell_px` 偏好仍优先（`ZoomState::from_setting`），不会被覆盖。
pub const DEFAULT_CELL_PX: f32 = 96.0;

/// 拖到刻度点附近多少 px 内就吸附。
pub const TIER_SNAP_PX: f32 = 6.0;

/// 升档/降档的回滞带宽度（`07-A` §2.2 规则 1，初值 ±8）。
pub const TIER_HYSTERESIS_PX: f32 = 8.0;

/// 值稳定多久才算「松手」（`07-A` 附录 A `grid.zoom_settle_ms = 150`）。
pub const ZOOM_SETTLE_MS: u64 = 150;

/// 滚轮每格调多少 px（`07-A` 附录 A `grid.wheel_zoom_step_px = 16`）。
pub const WHEEL_ZOOM_STEP_PX: f32 = 16.0;

/// 12 档对数刻度（`08-D` §A-2.1）。
///
/// `⌘+` / `⌘-` 按档跳，`⌘0` 回默认。对数刻度让「小图区域」的档位更密——
/// 22→48 有 4 档，而 256→512 只有 3 档，符合「小图更需要微调」的直觉。
pub const LOG_STEPS: [f32; 12] = [
    22.0, 32.0, 48.0, 64.0, 96.0, 128.0, 160.0, 192.0, 256.0, 320.0, 400.0, 512.0,
];

/// 纹理档位（与 `thumbs.tier` 一致：0 = T0 / 1 = T1 / 2 = T2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TexTier {
    T0 = 0,
    T1 = 1,
    T2 = 2,
}

impl TexTier {
    pub fn index(self) -> usize {
        self as usize
    }

    /// 该档的物理边长（`picasa-imaging` 的 THUMB_T0 = 512，T1 = 2048）。
    ///
    /// `thumbs` 表只有 T0（栅格）与 T1（查看器）两档实际生成；
    /// T2 是「查看器全分辨率」的占位。栅格滑块只会用到 T0。
    pub fn physical_px(self) -> u32 {
        match self {
            Self::T0 => 512,
            Self::T1 => 2048,
            Self::T2 => 4096,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::T0 => "T0",
            Self::T1 => "T1",
            Self::T2 => "T2",
        }
    }
}

/// 显示尺寸落在哪一档（`07-A` §2.2 的 `tier(cell_px)`）。
///
/// 规则：取**最小的** `Tk` 使 `Tk.px ≥ 显示像素`；超过 T2 → 用 T2 放大（允许模糊，
/// 不新增档）。`dpi` 是 HiDPI 系数（Retina 2.0）。
///
/// ⚠️ 「显示像素」= 逻辑 px × dpi。这一步不能省：512 逻辑在 2× 屏上要 1024 物理，
/// 按逻辑 px 选档会让所有 Retina 机器上的缩略图都糊一半。
pub fn tier_for(cell_px: f32, dpi: f32) -> TexTier {
    let physical = (cell_px * dpi).max(1.0);
    if physical <= TexTier::T0.physical_px() as f32 {
        TexTier::T0
    } else if physical <= TexTier::T1.physical_px() as f32 {
        TexTier::T1
    } else {
        TexTier::T2
    }
}

/// 档位边界（逻辑 px，1× 屏）——`tier_for` 在哪个 `cell_px` 上会升档。
pub fn tier_boundaries(dpi: f32) -> [f32; 2] {
    [
        TexTier::T0.physical_px() as f32 / dpi,
        TexTier::T1.physical_px() as f32 / dpi,
    ]
}

/// 是否走微缩图集（`08-D` §A-2.2：≤ 48 px）。
///
/// 判据用**逻辑 px** 而不是物理：22 px 档在小屏上也只有 22 逻辑，
/// 而图集的意义是「格太多」，与 dpi 无关。
pub fn uses_atlas(cell_px: f32) -> bool {
    cell_px <= 48.0
}

/// 吸附到最近的**刻度点**（`07-A` ZOOM-01：拖到 `TIER_SNAP_PX` 内吸附）。
///
/// ⚠️ 吸附只在**刻度点附近**发生，且**不制造新的吸附中心**：
/// 早先的实现里 256 这个刻度点会把 250～262 全部吸到 256，
/// 于是「拖过 256 之后想停 262」永远做不到——表现是滑块卡在刻度点上「打滑」。
/// 更糟的是它会让 `tier` 永远升不上去（512 是最大刻度点，任何 506～518 都被吸回 512，
/// 而升档需要 ≥ 520）。所以吸附后的值还必须**落在合法范围内**且**不与请求值差太多**。
pub fn snap_to_point(cell_px: f32) -> f32 {
    let clamped = cell_px.clamp(CELL_MIN_PX, CELL_MAX_PX);
    let mut best: Option<(f32, f32)> = None; // (距离, 刻度点)
    for p in SNAP_POINTS {
        let d = (clamped - p).abs();
        if d <= TIER_SNAP_PX && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, p));
        }
    }
    match best {
        // 吸附只「抹掉很小的抖动」：拖动的方向感不能被它吃掉。
        // 阈值 6 px 对应约 1.2% 的滑块行程（22～512），手感上是「轻轻一顿」。
        Some((_, p)) => p,
        None => clamped,
    }
}

/// 对数档是否已经顶到这一侧（再 [`step_log`] 不会变）。
///
/// 顶栏 −/+ 的置灰用这个，而不是把当前值跟 22/512 做一次裸比较：
/// 按钮实际能走到的端点就是 `step_log` 的不动点。
pub fn at_log_bound(cell_px: f32, dir: i32) -> bool {
    let cur = cell_px.clamp(CELL_MIN_PX, CELL_MAX_PX);
    (step_log(cur, dir) - cur).abs() < 0.5
}

/// 按一个对数档位跳（`⌘+` / `⌘-`）。
///
/// `dir = 1` 往大跳，`dir = -1` 往小跳。已在端点时返回端点本身（不循环）。
pub fn step_log(cell_px: f32, dir: i32) -> f32 {
    if dir > 0 {
        LOG_STEPS
            .iter()
            .copied()
            .find(|p| *p > cell_px + 0.5)
            .unwrap_or(CELL_MAX_PX)
    } else if dir < 0 {
        LOG_STEPS
            .iter()
            .copied()
            .rev()
            .find(|p| *p < cell_px - 0.5)
            .unwrap_or(CELL_MIN_PX)
    } else {
        cell_px
    }
}

/// 滑块的完整状态机（`07-A` §2.2 三条规则）。
///
/// ## 为什么要做成状态机而不是几个函数
///
/// 三条规则**互相依赖**：回滞要记住上一次的档位（否则无从判断「这是升还是降」），
/// 拖动中不换纹理要记住「拖动开始了吗」与「值多久没变了」，
/// 锚点要在**开始拖动时**捕获（结束时就晚了——布局已经重建过）。
/// 三者拆开各自维护状态，必然出现「档位换了但锚点没记」的中间态。
#[derive(Debug)]
pub struct ZoomState {
    /// 当前格边长（逻辑 px）
    cell_px: f32,
    /// 当前请求中的纹理档位（回滞依赖它）
    tier: TexTier,
    /// 上一次值变化的时刻（结算用）
    last_change: Option<std::time::Instant>,
    /// 是否正在拖动（拖动中不换纹理）
    dragging: bool,
    /// 拖动开始时记住的锚点照片
    anchor: Option<i64>,
    /// 上次结算后发出的档位（用来判断「要不要发请求」）
    requested_tier: Option<TexTier>,
    /// 累计的档位切换次数（诊断：拖动全程它应当只有 0～1 次）
    pub tier_switches: u64,
}

impl Default for ZoomState {
    fn default() -> Self {
        Self::new(DEFAULT_CELL_PX)
    }
}

impl ZoomState {
    pub fn new(cell_px: f32) -> Self {
        let cell = cell_px.clamp(CELL_MIN_PX, CELL_MAX_PX);
        Self {
            cell_px: cell,
            tier: tier_for(cell, 1.0),
            last_change: None,
            dragging: false,
            anchor: None,
            requested_tier: None,
            tier_switches: 0,
        }
    }

    pub fn cell_px(&self) -> f32 {
        self.cell_px
    }

    /// 当前请求中的档位（`07-A` §2.2 的 `tier`）。
    pub fn tier(&self) -> TexTier {
        self.tier
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// 拖动开始：**捕获锚点**（`07-A` §2.2 规则 3）。
    ///
    /// `top_photo` 是拖动前视口顶部第一张照片的 id。捕获时机必须在这里——
    /// 等拖动结束再取，视口已经按新格尺寸重排过了，那时顶部那张照片可能已经换人。
    pub fn begin_drag(&mut self, top_photo: Option<i64>) {
        self.dragging = true;
        self.anchor = top_photo;
    }

    /// 拖动结束 / 松手。
    pub fn end_drag(&mut self, now: std::time::Instant) {
        self.dragging = false;
        self.last_change = Some(now);
    }

    /// 拖动中改值（`07-A` ZOOM-01/04）。
    ///
    /// 返回 `true` 表示**布局需要重建**（格尺寸变了）。
    /// ⚠️ 这里**不**改 `self.tier`：拖动中不换纹理。纹理档位只在 [`Self::settle`] 里更新。
    pub fn drag_to(&mut self, cell_px: f32, now: std::time::Instant) -> bool {
        let next = snap_to_point(cell_px);
        if (next - self.cell_px).abs() < f32::EPSILON {
            return false;
        }
        self.cell_px = next;
        self.last_change = Some(now);
        true
    }

    /// 拖动结束后取值并「结算」档位（`07-A` §2.2 规则 2）。
    ///
    /// 返回 `Some(tier)` 表示**应当按这个档位向流水线请求纹理**；
    /// `None` 表示档位没变，什么都不用做（拖动期间可能换了十几种格尺寸，
    /// 但只要没跨过档位边界，就不该有任何解码投递）。
    pub fn settle(&mut self, now: std::time::Instant, dpi: f32) -> Option<TexTier> {
        if self.dragging {
            return None;
        }
        // 还在结算窗口里：继续等
        if let Some(t) = self.last_change
            && now.saturating_duration_since(t)
                < std::time::Duration::from_millis(ZOOM_SETTLE_MS)
        {
            return None;
        }
        let want = self.tier_with_hysteresis(dpi);
        // ⚠️ 只有**真的换了档**才返回 `Some`。
        //
        // 早先写成「第一次结算必定返回 Some」，于是每个新视图都会在首帧
        // 白请求一轮纹理——而那一轮的结果与已在缓存的完全相同（同档），
        // 只是让流水线空转一次。`07-A` ZOOM-04 的验收是「松手后 ≤ 1 轮请求」，
        // 它要求的是「该请求时请求」，不是「每次都请求」。
        if want == self.tier {
            self.requested_tier = Some(want);
            return None;
        }
        self.tier_switches += 1;
        self.tier = want;
        self.requested_tier = Some(want);
        Some(want)
    }

    /// 带**回滞**的档位判定（`07-A` §2.2 规则 1）。
    ///
    /// 升档要求超过边界 + `H`，降档要求低于边界 − `H`：
    ///
    /// ```text
    /// 当前 T0，边界 512：
    ///   显示 < 512 - 8  → 仍 T0（降无可降）
    ///   显示 ≥ 512 + 8  → 升 T1
    /// 当前 T1，边界 512：
    ///   显示 ≤ 512 - 8  → 降 T0
    ///   显示 ≥ 512 + 8  → 升 T2
    ///   中间 8 px 带     → 保持 T1
    /// ```
    fn tier_with_hysteresis(&self, dpi: f32) -> TexTier {
        let physical = (self.cell_px * dpi).max(1.0);
        let bounds = [
            TexTier::T0.physical_px() as f32,
            TexTier::T1.physical_px() as f32,
        ];
        let h = TIER_HYSTERESIS_PX * dpi;

        // 先按「无回滞」算目标，再向当前档位收敛一格
        let naive = tier_for(self.cell_px, dpi);
        if naive > self.tier {
            // 想升：必须真的越过边界 + 回滞
            let boundary = bounds[self.tier.index()];
            if physical >= boundary + h { naive } else { self.tier }
        } else if naive < self.tier {
            // 想降：必须真的低于边界 - 回滞
            let boundary = bounds[naive.index()];
            if physical <= boundary - h { naive } else { self.tier }
        } else {
            self.tier
        }
    }

    /// 拖动开始时捕获的锚点照片（`Layout` 重建后按它恢复滚动位置）。
    pub fn anchor(&self) -> Option<i64> {
        self.anchor
    }

    /// 松手后清掉锚点（已经在 [`Self::settle`] 时用过）。
    pub fn clear_anchor(&mut self) {
        self.anchor = None;
    }

    /// 工具栏「缩略图」按钮：跳到下一个刻度点（`07-A` ZOOM-02：保留，不删）。
    ///
    /// 循环顺序 S → M → L → S，与 M2.2 的三档循环等价。
    pub fn cycle_point(&mut self) -> f32 {
        let cur = SNAP_POINTS
            .iter()
            .position(|p| (self.cell_px - p).abs() < 0.5);
        let next = match cur {
            Some(0) => SNAP_POINTS[1],
            Some(1) => SNAP_POINTS[2],
            _ => SNAP_POINTS[0],
        };
        self.cell_px = next;
        self.last_change = Some(std::time::Instant::now());
        next
    }

    /// `⌘0`：回到默认刻度。
    pub fn reset(&mut self) -> f32 {
        self.cell_px = DEFAULT_CELL_PX;
        self.last_change = Some(std::time::Instant::now());
        DEFAULT_CELL_PX
    }

    /// 滚轮缩放（`07-A` ZOOM-03）。返回新的 `cell_px`。
    pub fn wheel(&mut self, delta_steps: f32) -> f32 {
        let next = (self.cell_px + delta_steps * WHEEL_ZOOM_STEP_PX)
            .clamp(CELL_MIN_PX, CELL_MAX_PX);
        if (next - self.cell_px).abs() > f32::EPSILON {
            self.cell_px = next;
            self.last_change = Some(std::time::Instant::now());
        }
        self.cell_px
    }

    /// 从设置恢复（`07-A` ZOOM-06：`settings.grid_cell_px`，全局一份）。
    ///
    /// `None` = 没设过 → 用默认刻度。
    pub fn from_setting(saved: Option<f32>) -> Self {
        Self::new(saved.unwrap_or(DEFAULT_CELL_PX))
    }

    /// 要存进 `settings.grid_cell_px` 的值。
    ///
    /// ⚠️ 超出有效范围的旧值（比如手改过库）要**夹回来**，否则重启会得到一个
    /// 22 以下的格尺寸，画面上是一片挤在一起的小方块。
    pub fn to_setting(&self) -> i64 {
        self.cell_px.round().clamp(CELL_MIN_PX, CELL_MAX_PX) as i64
    }

    /// 是否处于「拖动中」且**还没结算**（栅格据此决定要不要显示占位而不是请求纹理）。
    pub fn pending_settle(&self, now: std::time::Instant) -> bool {
        self.dragging
            || self
                .last_change
                .map(|t| {
                    now.saturating_duration_since(t)
                        < std::time::Duration::from_millis(ZOOM_SETTLE_MS)
                })
                .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn t(ms: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_millis(ms)
    }

    #[test]
    fn tier_mapping_uses_physical_pixels_not_logical() {
        // 512 逻辑在 2× 屏上要 1024 物理 → 必须升到 T1。
        // 按逻辑 px 选档会让所有 Retina 机器糊一半——这是最容易犯的错。
        assert_eq!(tier_for(512.0, 1.0), TexTier::T0);
        assert_eq!(tier_for(512.0, 2.0), TexTier::T1);
        assert_eq!(tier_for(22.0, 2.0), TexTier::T0);
        // 2048 恰好等于 T1 的物理边长 → 仍是 T1（「≥」语义，不浪费升档）
        assert_eq!(tier_for(512.0, 4.0), TexTier::T1);
        // 真正超过 T1 才进 T2
        assert_eq!(tier_for(512.0, 4.1), TexTier::T2);
    }

    #[test]
    fn tier_boundaries_are_the_inverse_of_tier_for() {
        for dpi in [1.0f32, 2.0] {
            let [b0, b1] = tier_boundaries(dpi);
            assert_eq!(tier_for(b0, dpi), TexTier::T0, "边界上仍在低档");
            assert_eq!(tier_for(b0 + 0.5, dpi), TexTier::T1, "刚过边界就升档");
            assert_eq!(tier_for(b1, dpi), TexTier::T1);
            assert_eq!(tier_for(b1 + 0.5, dpi), TexTier::T2);
        }
    }

    #[test]
    fn at_1x_the_whole_slider_range_fits_in_t0() {
        // ⚠️ 这条是**设计结论**，不是巧合：T0 长边 512，而滑块上限正好 512 逻辑。
        // 1× 屏上 512 逻辑 = 512 物理，T0 刚好覆盖整个滑块范围 → 只用 T0，永不升档。
        // 只有 Retina（dpi ≥ 2）才会用到 T1。
        for px in [22.0, 48.0, 128.0, 256.0, 400.0, 512.0] {
            assert_eq!(tier_for(px, 1.0), TexTier::T0, "{px} 逻辑 px 在 1× 屏上是 T0");
        }
    }

    #[test]
    fn hysteresis_prevents_thrashing_at_the_boundary() {
        // 停在 Retina 的 T0/T1 边界（256 逻辑 = 512 物理）附近来回抖：
        // 档位必须**纹丝不动**。没有回滞的话每帧换一次纹理 → 画面闪、GPU 忙。
        let mut z = ZoomState::new(256.0);
        assert_eq!(z.tier(), TexTier::T0, "256 逻辑在 2× 屏上正好是 T0 的边界");
        // 首次结算：档位没变 → None（没有要请求的东西）
        assert_eq!(z.settle(t(0), 2.0), None);

        // 在边界上下抖 20 次，每次都在回滞带（±8 物理 = ±4 逻辑）以内
        for i in 0..20 {
            let v = 256.0 + if i % 2 == 0 { 3.0 } else { -3.0 };
            z.drag_to(v, t(100 * i));
        }
        z.end_drag(t(3000));
        assert_eq!(
            z.settle(t(4000), 2.0),
            None,
            "回滞带内来回抖不能换档（返回 None = 不请求任何纹理）"
        );
        assert_eq!(z.tier_switches, 0);
    }

    #[test]
    fn crossing_the_boundary_does_switch_the_tier() {
        // 2× 屏：边界 256 逻辑。明确越过 256 + 4（= 8 物理回滞）。
        let mut z = ZoomState::new(256.0);
        assert_eq!(z.tier(), TexTier::T0);
        z.drag_to(270.0, t(0)); // 270×2 = 540 物理 ≥ 512 + 8
        z.end_drag(t(10));
        assert_eq!(
            z.settle(t(300), 2.0),
            Some(TexTier::T1),
            "越过边界 + 回滞要升档"
        );
        assert_eq!(z.tier_switches, 1);
    }

    #[test]
    fn dragging_never_changes_the_tier_until_settled() {
        // `07-A` ZOOM-04：拖动 5 s 期间「Job::Decode 投递数 = 0」。
        // 这里验的是前一半：拖动过程中 `tier()` 一直不变。
        let mut z = ZoomState::new(22.0);
        let start = z.tier();
        z.begin_drag(Some(42));
        for i in 0..500 {
            let v = 22.0 + (i as f32) * 0.9; // 一路拖到 ~472
            z.drag_to(v, t(i as u64 * 10));
            assert!(z.is_dragging());
            // 拖动中结算必须拒绝
            assert_eq!(z.settle(t(i as u64 * 10), 1.0), None, "拖动中不许换档");
            assert_eq!(z.tier(), start, "拖动中档位必须保持");
        }
        assert_eq!(z.tier_switches, 0, "拖动全程档位切换次数必须是 0");

        // 松手 + 等结算窗口 → 才可能换档
        z.end_drag(t(5000));
        let got = z.settle(t(5200), 1.0);
        assert_eq!(got, None, "1× 屏全程都在 T0，没有要换的档");
        assert_eq!(z.tier_switches, 0, "没跨档就不该记一次切换");
    }

    #[test]
    fn settling_waits_for_the_quiet_window() {
        let mut z = ZoomState::new(128.0);
        z.drag_to(200.0, t(0));
        z.end_drag(t(10));
        // 距最后变化 100 ms < 150 ms → 继续等（且 `pending_settle` 要说 true）
        assert_eq!(z.settle(t(100), 1.0), None);
        assert!(z.pending_settle(t(100)));
        // 过了 150 ms → 结算。1× 屏上 200 px 仍是 T0，档位没变 → 返回 None，
        // 但「等待」这件事必须结束（不然栅格会一直画占位）。
        assert_eq!(z.settle(t(200), 1.0), None);
        assert!(!z.pending_settle(t(200)), "结算后不再 pending");
    }

    #[test]
    fn anchor_is_captured_at_drag_start_not_at_the_end() {
        // 规则 3：锚点必须在**拖动开始**时捕获。
        // 结束时视口已经按新格尺寸重排过，那时顶部那张照片可能已经换人。
        let mut z = ZoomState::new(128.0);
        assert_eq!(z.anchor(), None);
        z.begin_drag(Some(777));
        assert_eq!(z.anchor(), Some(777));
        z.drag_to(300.0, t(0));
        z.end_drag(t(100));
        assert_eq!(z.anchor(), Some(777), "拖动过程中锚点不变");
        z.clear_anchor();
        assert_eq!(z.anchor(), None);
    }

    #[test]
    fn snap_points_attract_within_six_pixels() {
        assert_eq!(snap_to_point(48.0), 48.0);
        assert_eq!(snap_to_point(52.0), 48.0, "在 ±6 内要吸附");
        assert_eq!(snap_to_point(53.0), 48.0, "53 距 48 只有 5 px，要吸附");
        assert_eq!(snap_to_point(55.0), 55.0, "55 距 48 有 7 px，超出 ±6 不吸附");
        // 256 附近同理
        assert_eq!(snap_to_point(260.0), 256.0);
        assert_eq!(snap_to_point(265.0), 265.0);
        assert_eq!(snap_to_point(131.0), 128.0);
        // 夹紧到范围
        assert_eq!(snap_to_point(1.0), CELL_MIN_PX);
        assert_eq!(snap_to_point(9999.0), CELL_MAX_PX);
    }

    #[test]
    fn cycling_points_walks_s_m_l_and_wraps() {
        // ZOOM-02：点 3 次回到原刻度（R-M22-5 零功能丢失）
        let mut z = ZoomState::new(SNAP_POINTS[0]);
        assert_eq!(z.cycle_point(), SNAP_POINTS[1]);
        assert_eq!(z.cycle_point(), SNAP_POINTS[2]);
        assert_eq!(z.cycle_point(), SNAP_POINTS[0], "第三次回到起点");
    }

    #[test]
    fn log_steps_never_overshoot_and_clamp_at_the_ends() {
        let mut v = CELL_MIN_PX;
        // 一路上到顶
        for _ in 0..20 {
            let next = step_log(v, 1);
            assert!(next >= v, "往上跳不能变小");
            v = next;
        }
        assert_eq!(v, CELL_MAX_PX, "到顶后停在顶，不循环");
        // 一路下到底
        for _ in 0..20 {
            let next = step_log(v, -1);
            assert!(next <= v, "往下跳不能变大");
            v = next;
        }
        assert_eq!(v, CELL_MIN_PX, "到底后停在底");
    }

    #[test]
    fn at_log_bound_is_only_the_two_ends() {
        assert!(at_log_bound(CELL_MIN_PX, -1), "最小档 − 应置灰");
        assert!(!at_log_bound(CELL_MIN_PX, 1), "最小档 + 仍可点");
        assert!(at_log_bound(CELL_MAX_PX, 1), "最大档 + 应置灰");
        assert!(!at_log_bound(CELL_MAX_PX, -1), "最大档 − 仍可点");
        assert!(!at_log_bound(DEFAULT_CELL_PX, 1));
        assert!(!at_log_bound(DEFAULT_CELL_PX, -1));
        let mut v = CELL_MIN_PX;
        assert!(at_log_bound(v, -1));
        for _ in 0..LOG_STEPS.len() + 2 {
            let next = step_log(v, 1);
            if (next - v).abs() < 0.5 {
                assert!(at_log_bound(v, 1));
                break;
            }
            assert!(!at_log_bound(v, 1), "{v} 还没到顶，+ 不该置灰");
            assert!(!at_log_bound(next, -1), "{next} 还能再缩小");
            v = next;
        }
        assert_eq!(v, CELL_MAX_PX);
    }

    #[test]
    fn reset_goes_back_to_the_default_mark() {
        let mut z = ZoomState::new(400.0);
        assert_eq!(z.reset(), DEFAULT_CELL_PX);
        assert_eq!(z.cell_px(), DEFAULT_CELL_PX);
    }

    #[test]
    fn wheel_steps_and_clamps() {
        let mut z = ZoomState::new(100.0);
        assert_eq!(z.wheel(1.0), 116.0);
        assert_eq!(z.wheel(-2.0), 84.0);
        // 夹到上下限
        for _ in 0..50 {
            z.wheel(1.0);
        }
        assert_eq!(z.cell_px(), CELL_MAX_PX);
        for _ in 0..100 {
            z.wheel(-1.0);
        }
        assert_eq!(z.cell_px(), CELL_MIN_PX);
    }

    #[test]
    fn setting_round_trips_and_clamps_corrupt_values() {
        let z = ZoomState::from_setting(Some(200.0));
        assert_eq!(z.to_setting(), 200);
        // 没设过 → 默认
        assert_eq!(ZoomState::from_setting(None).cell_px(), DEFAULT_CELL_PX);
        // 库里的坏值（手改过）要在读的时候夹回来，否则重启后画面会挤成一团
        assert_eq!(ZoomState::from_setting(Some(1.0)).to_setting(), 22);
        assert_eq!(ZoomState::from_setting(Some(9999.0)).to_setting(), 512);
    }

    #[test]
    fn atlas_kicks_in_at_and_below_48() {
        // `08-D` §A-2.2：≤ 48 px 走微缩图集，否则 4K 全屏会撞纹理预算
        assert!(uses_atlas(22.0));
        assert!(uses_atlas(48.0));
        assert!(!uses_atlas(48.5));
        assert!(!uses_atlas(128.0));
    }

    #[test]
    fn min_and_max_match_the_decided_range() {
        // P-57 ②：M3 下限就是 22（不再有 48 的过渡值）；上限 512 = T2 吃满 2× 屏
        assert_eq!(CELL_MIN_PX, 22.0);
        assert_eq!(CELL_MAX_PX, 512.0);
        // 12 档对数刻度，首尾对齐范围
        assert_eq!(LOG_STEPS.len(), 12);
        assert_eq!(LOG_STEPS[0], CELL_MIN_PX);
        assert_eq!(LOG_STEPS[11], CELL_MAX_PX);
        // 严格递增
        for w in LOG_STEPS.windows(2) {
            assert!(w[0] < w[1], "对数档位必须严格递增：{w:?}");
        }
        // 默认刻度在档位表里
        assert!(LOG_STEPS.contains(&DEFAULT_CELL_PX));
    }
}

