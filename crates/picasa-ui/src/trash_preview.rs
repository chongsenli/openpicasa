//! 废纸篓确认框的**路径预览缓存**（`M5-FS-01` / `M5-VW-15` / `M5-KB-14`）。
//!
//! ## 为什么单独一个模块
//!
//! 确认框每帧最多只画 3 条路径，但旧写法是「对选中的每个 id 都
//! `photos.iter().find(...)` 再分配一条 `String`」——O(选中数 × 图库数)／帧，
//! 大选集上纯属白烧。张数提示（「另有 N 个文件」）又必须按**完整确认集合**
//! 算，不能因为只预览 3 条就把剩余数量丢掉。
//!
//! 这里把「预览什么」与「确认多少」拆开：
//!
//! * 预览（[`TrashPreviewCache`]）只在**确认框打开 / 范围重置 / 数据版本变化**
//!   时做一次有界扫描（上界 `O(N)`，N = 当前已装入的 `lib.photos` 行数），
//!   之后每帧直接复用 ≤3 条字符串；
//! * 张数与执行集合仍然只认 [`picasa_core::TrashScope`]（`app.rs` 的决策表），
//!   本模块只读、不执行、不碰磁盘。
//!
//! 缓存键刻意不含「每帧散列全部 id」：只比较 `TrashScope` 本身（有序去重的
//! id 切片，逐项相等即同一集合）与 [`picasa_core::Library::photos_rev`]
//! （O(1) 计数器）。因此换选集、**同数不同 id**、路径/库数据变化、关闭重开
//! 都会重新生成；没有变化时一次扫描都不会发生。

use picasa_core::TrashScope;
use picasa_db::PhotoRow;

/// 确认框里最多预览几条真实路径（与原型一致）。
pub const PREVIEW_LIMIT: usize = 3;

/// 一次确认框绘制所需的路径预览（≤ [`PREVIEW_LIMIT`] 条）。
///
/// 只缓存「路径字符串」这一份派生数据；确认张数、执行集合始终来自
/// `TrashScope`，不从本缓存反推。
#[derive(Debug, Default)]
pub struct TrashPreviewCache {
    /// 生成这份预览时对应的完整确认集合（用于逐项相等判定，不每帧克隆）。
    scope: Option<TrashScope>,
    /// 生成时的图库数据版本（`Library::photos_rev`）。
    photos_rev: u64,
    /// 已生成的最多 3 条路径。
    paths: Vec<String>,
    /// 实际做过多少次「遍历 photos」的有界扫描（测试可观测）。
    scans: u64,
    /// 命中缓存的次数（测试可观测）。
    hits: u64,
    /// 最近一次扫描实际查看过多少行（测试可观测：证明找到 3 条就停）。
    visited_rows: usize,
}

impl TrashPreviewCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 关框 / 换会话时清空：下次打开必定重新生成（关闭重开也覆盖）。
    pub fn reset(&mut self) {
        self.scope = None;
        self.photos_rev = 0;
        self.paths.clear();
    }

    /// 当前确认集合的预览路径（最多 3 条）。
    ///
    /// 命中条件：**确认集合逐项相等** 且 `photos_rev` 未变。
    /// 未命中时做一次单遍扫描（`O(N)`，提前找到 3 条即可停），
    /// 不会出现每帧 O(选中数 × 图库数)。
    pub fn paths(&mut self, scope: &TrashScope, photos_rev: u64, photos: &[PhotoRow]) -> &[String] {
        let hit = self.photos_rev == photos_rev && self.scope.as_ref() == Some(scope);
        if hit {
            self.hits = self.hits.wrapping_add(1);
            return &self.paths;
        }
        self.scope = Some(scope.clone());
        self.photos_rev = photos_rev;
        let (paths, visited) = preview_paths(scope.ids(), photos, PREVIEW_LIMIT);
        self.paths = paths;
        self.visited_rows = visited;
        self.scans = self.scans.wrapping_add(1);
        &self.paths
    }

    /// 做过多少次有界扫描。
    pub fn scan_count(&self) -> u64 {
        self.scans
    }

    /// 命中过多少次。
    pub fn hit_count(&self) -> u64 {
        self.hits
    }

    /// 最近一次生成预览时真正查看过的 `photos` 行数。
    ///
    /// 上界是 `photos.len()`，且通常远小于它（找到 3 条就停）；用它证明
    /// 「不是每帧 N×S 扫描」，也不依赖机器耗时阈值。
    pub fn visited_rows(&self) -> usize {
        self.visited_rows
    }

    /// 当前缓存的预览路径条数。
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// 从内存照片行里取最多 `limit` 条、属于 `ids` 的路径。
///
/// `ids` 是 [`TrashScope`] 的有序去重切片，而**要预览的就是它最前面的
/// `limit` 个 id**——与旧实现「按 id 升序取前 3 条」的可见顺序一致，
/// 不因为换写法就让用户看到另外三张。
///
/// 做法：先截出前 `limit` 个 id（≤3 个，切片复用，不分配），单遍扫
/// `photos`，用二分查找判断这行是不是那 ≤3 个之一；三条都找到即停。
/// 单次上界 `O(N log limit)` = `O(N)`，N = `photos.len()`，且每个缓存键
/// 只做一次。找不到的 id 不伪造路径（确认张数仍按完整集合算）。
fn preview_paths(ids: &[i64], photos: &[PhotoRow], limit: usize) -> (Vec<String>, usize) {
    if limit == 0 || ids.is_empty() {
        return (Vec::new(), 0);
    }
    let wanted = &ids[..limit.min(ids.len())];
    // (在 wanted 里的下标 → 路径)，最后按 wanted 顺序收拢。
    let mut slots: Vec<Option<String>> = vec![None; wanted.len()];
    let mut found = 0usize;
    let mut visited = 0usize;
    for p in photos {
        visited += 1;
        if let Ok(i) = wanted.binary_search(&p.id)
            && slots[i].is_none()
        {
            slots[i] = Some(p.abs_path().to_string_lossy().into_owned());
            found += 1;
            if found == wanted.len() {
                break;
            }
        }
    }
    (slots.into_iter().flatten().collect(), visited)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(id: i64, name: &str) -> PhotoRow {
        PhotoRow {
            id,
            folder_path: "/home/me/Pictures/2024".into(),
            name: name.into(),
            ..Default::default()
        }
    }

    fn library(n: i64) -> Vec<PhotoRow> {
        (0..n).map(|i| photo(i, &format!("p{i}.jpg"))).collect()
    }

    /// 大选集：打开一次后连续多帧绘制，**零新增扫描**、全部命中。
    #[test]
    fn large_selection_scans_once_then_hits_every_frame() {
        let photos = library(5000);
        let scope = TrashScope::new((0..1200).map(|i| i * 3));
        let mut cache = TrashPreviewCache::new();
        for _ in 0..100 {
            let paths = cache.paths(&scope, 1, &photos);
            assert_eq!(paths.len(), PREVIEW_LIMIT);
        }
        assert_eq!(cache.scan_count(), 1, "100 帧只应有 1 次有界扫描");
        assert_eq!(cache.hit_count(), 99);
        // 预览确实是最多 3 条真实路径，不是把 1200 条全收集起来。
        assert_eq!(cache.len(), PREVIEW_LIMIT);
        // 单次扫描的上界：1200 个选中 id × 5000 行不是它的复杂度。
        // 命中行是 id 0 / 3 / 6，第三条第 7 行出现时即停，后面的行不再看。
        assert_eq!(cache.visited_rows(), 7, "找到 3 条预览就应停止扫描");
    }

    /// 选中集合很大、命中行在末尾时，单次扫描仍只有 O(N)，且只走一遍。
    #[test]
    fn one_bounded_pass_even_when_matches_are_at_the_end() {
        let photos = library(5000);
        let scope = TrashScope::new([4997, 4998, 4999, 10, 11]);
        let mut cache = TrashPreviewCache::new();
        let paths = cache.paths(&scope, 1, &photos);
        assert_eq!(paths.len(), 3);
        assert_eq!(cache.scan_count(), 1, "一次生成只扫描一遍");
        // 第三条命中在 id=4997（第 4998 行），从此不再看后面的行。
        assert_eq!(
            cache.visited_rows(),
            4998,
            "最坏情况上界就是 photos.len()，不是每帧 N×S"
        );
        // 后续帧不再扫描。
        for _ in 0..50 {
            let _ = cache.paths(&scope, 1, &photos);
        }
        assert_eq!(cache.scan_count(), 1);
    }

    /// 同数不同 id（集合真的变了）必须重新生成，不能拿旧预览冒充。
    #[test]
    fn same_size_different_ids_invalidates_the_preview() {
        let photos = library(5000);
        let a = TrashScope::new((0..1200).map(|i| i * 3));
        let b = TrashScope::new((0..1200).map(|i| i * 3 + 1));
        assert_eq!(a.len(), b.len());
        let mut cache = TrashPreviewCache::new();
        let first = cache.paths(&a, 7, &photos).to_vec();
        let second = cache.paths(&b, 7, &photos).to_vec();
        assert_eq!(cache.scan_count(), 2, "换集合必须重新生成");
        assert_ne!(first, second, "同数不同 id 的预览不能一样");
    }

    /// 数据版本变化（补页 / 重载 / 清空）与关闭重开都要失效。
    #[test]
    fn data_version_change_and_reopen_invalidate_the_preview() {
        let photos = library(50);
        let scope = TrashScope::new([0, 1, 2, 3, 4]);
        let mut cache = TrashPreviewCache::new();
        let _ = cache.paths(&scope, 1, &photos);
        assert_eq!(cache.scan_count(), 1);

        // 库数据版本变化：即使集合一字不差也要重生成（路径可能已变）。
        let _ = cache.paths(&scope, 2, &photos);
        assert_eq!(cache.scan_count(), 2, "photos_rev 变化必须失效");

        // 关闭重开：reset 后同集合同版本也要重生成。
        cache.reset();
        let _ = cache.paths(&scope, 2, &photos);
        assert_eq!(cache.scan_count(), 3, "关闭重开必须失效");
    }

    /// 与决策表联用：范围在框内变化（Rearm）→ 预览按**新的完整集合**重生成，
    /// 张数也用新集合；用户点确认不会执行旧集合。
    #[test]
    fn rearm_from_the_decision_table_regenerates_bounded_preview() {
        use crate::trash_ui::{Decision, UserAction, decide};

        let photos = library(4000);
        let confirmed = TrashScope::new((0..1000).map(|i| i * 2));
        let mut cache = TrashPreviewCache::new();
        let first = cache.paths(&confirmed, 9, &photos).to_vec();
        assert_eq!(first.len(), PREVIEW_LIMIT);

        // 同数换一批 id：决策表要求重新确认，预览也必须换。
        let next = TrashScope::new((0..1000).map(|i| i * 2 + 1));
        assert_eq!(next.len(), confirmed.len());
        assert_eq!(
            decide(UserAction::None, &confirmed, &next),
            Decision::Rearm(next.clone())
        );
        let second = cache.paths(&next, 9, &photos).to_vec();
        assert_eq!(cache.scan_count(), 2, "Rearm 后必须重新生成");
        assert_ne!(first, second, "新集合的预览不能复用旧的");
        // 张数仍是完整集合大小，与预览条数无关。
        assert_eq!(next.len(), 1000);

        // 用户对着旧集合点确认 → 只能 Rearm，不能 Execute。
        assert!(matches!(
            decide(UserAction::Confirm, &confirmed, &next),
            Decision::Rearm(_)
        ));
    }

    /// 预览顺序与旧实现一致：按 id 升序的前 3 条（不是照片列表里的前 3 条）。
    #[test]
    fn preview_keeps_the_lowest_three_ids_regardless_of_row_order() {
        // 行顺序故意倒过来（最近的在前），最小三个 id 在列表末尾。
        let mut photos = library(100);
        photos.reverse();
        let scope = TrashScope::new([3, 50, 99, 7, 40]);
        let mut cache = TrashPreviewCache::new();
        let paths = cache.paths(&scope, 1, &photos);
        let expected: Vec<String> = [3, 7, 40]
            .iter()
            .map(|i| format!("/home/me/Pictures/2024/p{i}.jpg"))
            .collect();
        assert_eq!(paths, expected.as_slice(), "应取 id 最小的三条，且按升序");
        assert_eq!(scope.len(), 5, "张数仍是完整集合");
    }

    /// 集合里的 id 在内存里找不到时：预览少画，但不影响「确认多少」。
    #[test]
    fn missing_rows_are_not_fabricated_in_the_preview() {
        let photos = library(10);
        let scope = TrashScope::new([2, 500, 501, 502, 503]);
        let mut cache = TrashPreviewCache::new();
        let paths = cache.paths(&scope, 1, &photos);
        assert_eq!(paths.len(), 1, "只有 id=2 在内存里");
        assert_eq!(scope.len(), 5, "完整确认集合仍是 5");
    }
}
