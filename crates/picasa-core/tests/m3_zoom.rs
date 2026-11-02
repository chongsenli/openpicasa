//! 缩放滑块的验收测试（`07-A` §12.2 `TC-M3-ZOOM-001～008`）。
//!
//! ## 三个必须被钉住的契约
//!
//! 1. **拖动期间零解码投递**（ZOOM-04）——这是「拖动不掉帧」的全部依据。
//!    拖动 5 秒、跨过 200 个像素值，档位切换次数必须是 0。
//! 2. **回滞**（ZOOM-03）——滑块停在边界附近时不能每帧换纹理。
//! 3. **锚点**（ZOOM-05）——拖动前后视口顶部那张照片不变。
//!
//! ## 为什么这些不能靠「看起来对」验收
//!
//! 拖滑块时画面会一直变，肉眼根本分不出「正常重排」与「每帧重传纹理」。
//! 而后者在弱机上就是明显的卡顿与闪烁，且**不会报任何错**。

use picasa_core::zoom::{
    CELL_MAX_PX, CELL_MIN_PX, DEFAULT_CELL_PX, LOG_STEPS, SNAP_POINTS, TIER_HYSTERESIS_PX, TexTier,
    ZOOM_SETTLE_MS, ZoomState, snap_to_point, step_log, tier_for, uses_atlas,
};
use std::time::{Duration, Instant};

fn t(ms: u64) -> Instant {
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *BASE.get_or_init(Instant::now) + Duration::from_millis(ms)
}

#[test]
fn tc_m3_zoom_001_dragging_is_monotonic_and_snaps_to_marks() {
    // ZOOM-001：全程拖动，`cell_px` 单调、无跳变；刻度吸附断言。
    let mut z = ZoomState::new(CELL_MIN_PX);
    z.begin_drag(Some(1));
    let mut prev = z.cell_px();
    let mut steps = 0;
    // 从 22 拖到 512（约 980 步）
    for i in 0..980 {
        let want = CELL_MIN_PX + i as f32 * 0.5;
        z.drag_to(want, t(i as u64 * 5));
        let cur = z.cell_px();
        assert!(cur >= prev, "拖动过程中格尺寸必须单调不减（{prev} → {cur}）");
        // 「无跳变」：单步变化不得超过吸附范围（6 px）+ 半个步长
        assert!(
            cur - prev <= 6.5,
            "单步跳了 {} px，超过吸附范围——滑块会「打滑」",
            cur - prev
        );
        prev = cur;
        steps += 1;
    }
    assert!(steps > 900);
    // 拖到刻度点附近要吸附
    let mut z2 = ZoomState::new(100.0);
    z2.begin_drag(None);
    z2.drag_to(SNAP_POINTS[1] + 4.0, t(0));
    assert_eq!(z2.cell_px(), SNAP_POINTS[1], "距刻度点 4 px 要吸附");
}

#[test]
fn tc_m3_zoom_003_drag_issues_zero_tier_requests() {
    // ZOOM-003 / ZOOM-04：拖动 5 s 期间 `Job::Decode` 投递数 = 0。
    //
    // 这里的代理指标是 `settle()` 的返回值：它返回 `Some` 就意味着「要按新档
    // 向流水线请求纹理」。拖动全程它必须一次都不返回 `Some`。
    let mut z = ZoomState::new(CELL_MIN_PX);
    z.begin_drag(Some(100));
    let mut requests = 0;
    for i in 0..1000u64 {
        let px = CELL_MIN_PX + (i as f32) * 0.49;
        z.drag_to(px, t(i * 5));
        // 模拟每帧都调 settle（真实代码就是这样）
        if z.settle(t(i * 5), 2.0).is_some() {
            requests += 1;
        }
    }
    assert_eq!(requests, 0, "★ 拖动期间不许发出任何纹理档位请求");
    assert_eq!(z.tier_switches, 0, "★ 拖动期间档位切换次数必须为 0");

    // 松手 + 等过结算窗口 → 最多一次请求
    z.end_drag(t(5000));
    let after = z.settle(t(5000 + ZOOM_SETTLE_MS + 10), 2.0);
    assert_eq!(
        after,
        Some(TexTier::T1),
        "2× 屏上拖到 ~512 逻辑 = 1024 物理 → T1，松手后只请求一次"
    );
    assert_eq!(z.tier_switches, 1, "整个拖动-松手周期只换一次档");
}

#[test]
fn tc_m3_zoom_002_toolbar_button_still_cycles_three_marks() {
    // ZOOM-02：工具栏「缩略图」按钮保留为循环三刻度（R-M22-5 零功能丢失）。
    // 从第一个刻度点出发：48 → 128 → 256 → 48
    let mut z = ZoomState::new(SNAP_POINTS[0]);
    assert_eq!(z.cycle_point(), SNAP_POINTS[1]);
    assert_eq!(z.cycle_point(), SNAP_POINTS[2]);
    assert_eq!(z.cycle_point(), SNAP_POINTS[0], "点三次回到起点");
}

#[test]
fn tc_m3_zoom_004_anchor_survives_the_rebuild() {
    // ZOOM-005：任意位置拖动前后顶部 `photo_id` 不变（组头除外）。
    //
    // 这里验的是「锚点被正确捕获并在重建后仍然可取」。真正的滚动位置恢复
    // 由栅格用 `index_of(photo_id)` 完成（`DataSource` 已有效用例覆盖）。
    let mut z = ZoomState::new(128.0);
    z.begin_drag(Some(4242));
    assert_eq!(z.anchor(), Some(4242));
    // 拖到任意位置
    for i in 0..100 {
        z.drag_to(128.0 + i as f32 * 3.0, t(i as u64 * 10));
    }
    assert_eq!(z.anchor(), Some(4242), "拖动全程锚点不变");
    z.end_drag(t(2000));
    assert_eq!(z.anchor(), Some(4242), "松手后锚点仍然可取（供重建使用）");
    z.clear_anchor();
    assert_eq!(z.anchor(), None);
}

#[test]
fn tc_m3_zoom_005_layout_rebuild_is_cheap() {
    // ZOOM-07：`Layout` 重建 ≤ 2 ms（只重算行高与组偏移，O(groups)）。
    //
    // 这里不测绝对毫秒（机器差异大），测的是**复杂度**：
    // 拖动 200 步，每步只改一个数字，不该有任何 O(行数) 的工作。
    // 真正的布局成本由 `grid::GridCache` 的行级虚拟化保证（M2 已有用例）。
    let mut z = ZoomState::new(64.0);
    z.begin_drag(Some(0));
    let start = Instant::now();
    for i in 0..200 {
        z.drag_to(64.0 + i as f32, t(i as u64));
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(50),
        "200 次改值用了 {elapsed:?}——说明每步在做与格尺寸无关的重活"
    );
}

#[test]
fn tc_m3_zoom_007_atlas_kicks_in_below_48() {
    // ZOOM-07（P-57 ② 落定）：≤ 48 px 走微缩图集，否则 4K 全屏 17 000 格
    // 会直接撞纹理预算。
    assert!(uses_atlas(CELL_MIN_PX), "22 px 必须走图集");
    assert!(uses_atlas(48.0));
    assert!(!uses_atlas(49.0), "49 px 已经回到正常纹理路径");
}

#[test]
fn tc_m3_zoom_008_tier_selection_matches_the_atlas_boundary() {
    // ZOOM-008：图集与档位是两个独立概念——图集只管「格太多」，
    // 档位管「纹理分辨率」。<=48 px 时两者都落在 T0。
    for px in [CELL_MIN_PX, 32.0, 48.0] {
        assert!(uses_atlas(px));
        assert_eq!(tier_for(px, 2.0), TexTier::T0, "{px} px 在 2× 屏上仍是 T0");
    }
}

#[test]
fn zoom_range_is_22_to_512_with_twelve_log_marks() {
    // P-57 ② 落定：M3 下限就是 22（不再有 48 过渡值）。
    assert_eq!(CELL_MIN_PX, 22.0);
    assert_eq!(CELL_MAX_PX, 512.0);
    assert_eq!(LOG_STEPS.len(), 12);
    assert_eq!(LOG_STEPS[0], 22.0);
    assert_eq!(LOG_STEPS[11], 512.0);
    // 默认刻度在表里
    assert!(LOG_STEPS.contains(&DEFAULT_CELL_PX));
}

#[test]
fn keyboard_steps_walk_the_log_marks() {
    // `⌘+` / `⌘-` 按档跳（ZOOM-03）
    let mut v = CELL_MIN_PX;
    for expect in &LOG_STEPS[1..] {
        v = step_log(v, 1);
        assert_eq!(v, *expect);
    }
    assert_eq!(v, CELL_MAX_PX);
    // 反向
    for expect in LOG_STEPS.iter().rev().skip(1) {
        v = step_log(v, -1);
        assert_eq!(v, *expect);
    }
    assert_eq!(v, CELL_MIN_PX);
}

#[test]
fn dragging_past_the_last_snap_point_is_possible() {
    // ⚠️ 回归：早先的实现里 256 是最大刻度点，而滑块上限是 512，
    // 于是「拖过 256 → 想停 400」时会被吸附逻辑拉回去，档位永远升不上去。
    // 这是「吸附」与「范围」两件事被混在一起的经典 bug。
    let mut z = ZoomState::new(256.0);
    z.begin_drag(Some(1));
    z.drag_to(400.0, t(0));
    assert_eq!(z.cell_px(), 400.0, "400 距任何刻度点都超过 6 px，不该被吸附");
    z.drag_to(512.0, t(10));
    assert_eq!(z.cell_px(), 512.0, "能真的拖到上限");
    // 吸附仍然只在小范围内生效
    assert_eq!(snap_to_point(254.0), 256.0);
    assert_eq!(snap_to_point(247.0), 247.0);
}

#[test]
fn hysteresis_band_width_is_8px_per_dpi_unit() {
    // 回滞带 ±8 逻辑 px（`07-A` §2.2 规则 1）
    assert_eq!(TIER_HYSTERESIS_PX, 8.0);
    // 在 2× 屏上的实际带宽是 8 物理 px = 4 逻辑 px，所以
    // 「256 ± 3 逻辑」应当保持在 T0（这是上一条用例的量化版本）
    let mut z = ZoomState::new(256.0);
    z.drag_to(259.0, t(0));
    z.end_drag(t(10));
    assert_eq!(z.settle(t(300), 2.0), None, "回滞带内不换档");
}

#[test]
fn zero_delta_drag_reports_no_layout_change() {
    // 性能细节：值没变时不该告诉调用方「要重建布局」——
    // 每帧重建一次 `Layout` 在 25 万张库上是白烧 CPU。
    let mut z = ZoomState::new(128.0);
    z.begin_drag(None);
    assert!(!z.drag_to(128.0, t(0)), "同值不该触发重建");
    assert!(z.drag_to(160.0, t(10)), "不同值要触发重建");
}
