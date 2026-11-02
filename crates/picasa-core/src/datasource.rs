//! 虚拟数据源：**UI 侧不再持有整列 `Vec<PhotoRow>`**（`07` §5.2 SCALE-01～05）。
//!
//! ## 为什么必须换掉「一次读全列」
//!
//! M2 的分页（`PAGE_FIRST` + `extend_photos`）**只是把卡顿摊薄**，不是消除：
//! 用户滚到底，内存里最终还是躺着 25 万行 `PhotoRow`（约 60 MB 字符串与字段）。
//! 100 万张时是 240 MB 以上，加上缩略图纹理与解码缓冲，直接撞 2 GB 的软上限。
//!
//! 而且 `OFFSET` 分页在**大库上的代价随偏移线性增长**：25 万张滚到最后，
//! 每补一页都要跳过 24 万行。实测 5 万行跳 4.8 万 ≈ 8 ms 尚可接受，
//! 但 100 万张时这个数会到 150 ms 以上——**补齐一页就掉 9 帧**。
//!
//! ## 这个模块给什么
//!
//! - [`Scope`]：看哪儿（单目录 / 子树 / 全库）
//! - [`DataSource`]：`len()` + `group_bounds()` + `range(start, n)`，行窗口缓存前后各 2 屏
//! - 键集分页（`WHERE (sort_key, id) < (?, ?)`），走 `idx_photos_taken_id` 这类复合索引
//!
//! ## 关键约束：`range` 是**唯一**能拿到行内容的地方
//!
//! `Layout` 只依赖 `len` + 组边界 + 档位，**不依赖行内容**——这是 `07` §5.2 的原话，
//! 也是这个设计能成立的全部理由。一旦有人让布局去读行（比如「按宽高比定行高」），
//! 虚拟化就白做了：那要求全量行。M6 的瀑布流要解决这个问题用的是**分段加载**，
//! 不是取消虚拟化。

use std::ops::Range;

use picasa_db::{Db, PhotoRow, SortKey, SortSpec, ThumbState};

use crate::Result;

/// 看哪一片（`07` §9.3 + `07-A` §3.3 的 `Subtree` 变体）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// 单目录（「包含子文件夹」关闭时的默认）
    Folder(i64),
    /// 子树：`ordered_folders` 是**DFS 序**的后代 id（已剔除关闭子树）。
    ///
    /// 为什么带一个有序数组而不是只给根 id：按文件夹分组时组边界直接由它拼出，
    /// **零额外查询**（`07-A` §3.2 RECURSE-04）；再查一次树是多余的一次往返。
    Subtree {
        root_folder: i64,
        ordered_folders: std::sync::Arc<[i64]>,
    },
    /// 全库（「所有照片」）
    AllPhotos,
}

impl Scope {
    /// 这个范围里「按文件夹分组」时该有哪些目录、什么顺序。
    pub fn folders(&self) -> Option<&[i64]> {
        match self {
            Scope::Folder(id) => std::slice::from_ref(id).into(),
            Scope::Subtree {
                ordered_folders, ..
            } => Some(ordered_folders),
            Scope::AllPhotos => None,
        }
    }
}

/// 一行在列表中的位置（键集分页的游标）。
///
/// ⚠️ 键集分页要求「排序键 + id」**唯一且稳定**。`id` 是主键，天然唯一；
/// 排序键可能重复（同一秒拍 100 张），所以必须带上 `id` 兜底，
/// 否则 `WHERE taken_at < ?` 会跳过所有同一秒的照片。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cursor {
    pub sort_value: i64,
    pub id: i64,
}

/// 一个分组边界（组头、日期气泡、时间轴都用它，零额外查询）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupBound {
    /// 在整列里的起始下标
    pub start: usize,
    /// 本组行数
    pub len: usize,
    /// 组头文案（日期 `2024年3月5日` / 文件夹名 / 空串）
    pub label: String,
    /// 组代表的日期（Unix 秒，仅「按日期分组」时有意义）
    pub date: Option<i64>,
}

impl GroupBound {
    pub fn end(&self) -> usize {
        self.start + self.len
    }
}

/// 行窗口缓存多少屏（前后各）。
///
/// `07` 附录 A 的 `scale.row_window_screens = 2`。窗口太小学一个「快速拖滚动条」
/// 就连续 miss（每次 miss 都是一次 DB 往返）；太大则又回到「读半个库进内存」。
/// 2 屏 ≈ 一屏 48 格时约 200 行，单个 `PhotoRow` 约 200 B，一份窗口 40 KB 量级。
pub const ROW_WINDOW_SCREENS: usize = 2;

/// 一屏大约多少行（用于把「屏」换算成行数）。取偏小值：宁可多缓存也不要频繁 miss。
pub const ROWS_PER_SCREEN_ESTIMATE: usize = 60;

/// `IN (...)` 列表上限（`07` 附录 A 的 `scale.in_list_max = 200`）。
///
/// SQLite 对参数个数有上限（默认 32766），而 `07` 取 200 是为了**执行计划稳定**：
/// `IN` 列表太长时优化器会放弃索引走全表扫描。
pub const IN_LIST_MAX: usize = 200;

/// 一次 `range()` 最多取多少行（防止调用方要 50 万行时把库读进内存）。
pub const RANGE_MAX: usize = 2000;

/// 虚拟数据源（`07` §9.3）。
///
/// ## 窗口缓存策略
///
/// 缓存是「一段连续区间」而不是 LRU 逐行：滚动是**局部**行为，
/// 一段连续区间能覆盖绝大多数访问；而逐行 LRU 在快速拖动时命中率为零。
/// 窗口外的一次 `range()` 直接重查（不试图拼接两段）。
pub struct DataSource {
    db: std::sync::Arc<std::sync::Mutex<Db>>,
    scope: Scope,
    sort: SortSpec,
    /// 整列行数（组边界与滚动条都靠它）
    len: usize,
    /// 分组边界（构造时算一次；`dataset_version` 变了才重算）
    groups: Vec<GroupBound>,
    /// 当前缓存窗口 `[window_start, window_start + window_rows.len())`
    window_start: usize,
    window_rows: Vec<PhotoRow>,
    /// 数据源版本：照片集合变了才 +1（PERF-09 同一判据）
    version: u64,
    /// 统计：窗口 miss 次数（用例与浮层都要看这个数）
    pub misses: u64,
}

impl DataSource {
    /// 构造。`len` 与组边界在这里一次算完（聚合查询 + `folder_counts`），
    /// 之后滚动过程中**不再查总数**。
    pub fn new(
        db: std::sync::Arc<std::sync::Mutex<Db>>,
        scope: Scope,
        sort: SortSpec,
        version: u64,
        counts: &picasa_db::CountSnapshot,
    ) -> Result<Self> {
        let folder_ids: Option<Vec<i64>> = match &scope {
            Scope::Folder(id) => Some(vec![*id]),
            Scope::Subtree {
                ordered_folders, ..
            } => Some(ordered_folders.to_vec()),
            Scope::AllPhotos => None,
        };
        let db_guard = db
            .lock()
            .map_err(|_| crate::CoreError::Other("数据库锁失效".into()))?;
        let len = db_guard.count_photos_visible(folder_ids.as_deref())? as usize;
        let groups = build_group_bounds(&db_guard, &scope, &sort, &folder_ids, counts)?;
        drop(db_guard);

        Ok(Self {
            db,
            scope,
            sort,
            len,
            groups,
            window_start: 0,
            window_rows: Vec::new(),
            version,
            misses: 0,
        })
    }

    /// 整列行数。
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn groups(&self) -> &[GroupBound] {
        &self.groups
    }

    pub fn dataset_version(&self) -> u64 {
        self.version
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    pub fn sort(&self) -> SortSpec {
        self.sort
    }

    /// 窗口缓存当前驻留了多少行（**用例断言内存上限用**：`TC-M3-SCALE-004`
    /// 要求「内存中行数 ≤ 4 屏」）。
    pub fn cached_rows(&self) -> usize {
        self.window_rows.len()
    }

    /// 取 `[start, start + n)` 的行（`07` §9.3 SCALE-01）。
    ///
    /// 命中窗口 → 直接切片；未命中 → **一次**键集查询，并重建窗口。
    /// 返回的切片生命周期绑在 `&self` 上，调用方不能跨越下一次 `range()` 持有它。
    pub fn range(&mut self, start: usize, n: usize) -> Result<&[PhotoRow]> {
        if n == 0 || start >= self.len {
            return Ok(&[]);
        }
        let end = (start + n).min(self.len);
        // 命中判断：请求区间必须完整落在缓存窗口内
        if !self.window_rows.is_empty()
            && start >= self.window_start
            && end <= self.window_start + self.window_rows.len()
        {
            let lo = start - self.window_start;
            let hi = end - self.window_start;
            return Ok(&self.window_rows[lo..hi]);
        }

        self.misses += 1;
        let pad = ROW_WINDOW_SCREENS * ROWS_PER_SCREEN_ESTIMATE;

        // 两条取数路径，**返回的行从哪个下标开始**是这里最容易写错的地方：
        //
        // | 路径 | 起点 | 结果覆盖的区间 |
        // | --- | --- | --- |
        // | 键集（窗口尾部紧接请求） | 游标之后 | 正好是 `start` |
        // | OFFSET（跳转/首屏） | `start - pad` | `w_start` |
        //
        // 早先把两者混用（用 OFFSET 的 `w_start` 去索引键集返回的行），
        // 结果窗口标号与实际行错位 `pad` 行——表现是「翻页会漏照片」，
        // 而且**只在滚出第一个窗口之后**才出现（`TC-M3-SCALE-003b` 抓到过）。
        let contiguous = !self.window_rows.is_empty()
            && start == self.window_start + self.window_rows.len()
            && start > 0;

        let (win_start, rows) = if contiguous {
            // 往下滚，接在窗口尾部：键集分页，不重读前面已有的行
            let n = ((end - start) + pad).min(RANGE_MAX);
            let cursor = self.window_rows.last().map(|p| Cursor {
                sort_value: sort_value_of(p, self.sort.key),
                id: p.id,
            });
            let rows = {
                let db = self
                    .db
                    .lock()
                    .map_err(|_| crate::CoreError::Other("数据库锁失效".into()))?;
                fetch_keyset(&db, &self.scope, self.sort, start, n, cursor)?
            };
            (start, rows)
        } else {
            // 跳转 / 首屏 / 往上翻：OFFSET 取一段带前后余量的窗口
            let w_start = start.saturating_sub(pad);
            let w_len = ((end - start) + pad * 2).min(RANGE_MAX);
            let w_end = (w_start + w_len).min(self.len);
            let rows = {
                let db = self
                    .db
                    .lock()
                    .map_err(|_| crate::CoreError::Other("数据库锁失效".into()))?;
                fetch_keyset(&db, &self.scope, self.sort, w_start, w_end - w_start, None)?
            };
            (w_start, rows)
        };

        self.window_start = win_start;
        self.window_rows = rows;

        let lo = start.saturating_sub(self.window_start);
        let hi = (end - self.window_start).min(self.window_rows.len());
        if lo > self.window_rows.len() {
            return Ok(&[]);
        }
        Ok(&self.window_rows[lo..hi])
    }

    /// 定位一张照片在整列里的下标（锚点恢复用；`07` §9.3）。
    ///
    /// 允许 1 次查询：这不是滚动热路径，是「切档 / 切视图 / 搜索结果点进来」时
    /// 各一次的动作。
    pub fn index_of(&self, photo_id: i64) -> Result<Option<usize>> {
        let db = self
            .db
            .lock()
            .map_err(|_| crate::CoreError::Other("数据库锁失效".into()))?;
        index_of_photo(&db, &self.scope, self.sort, photo_id)
    }

    /// 丢弃窗口（`dataset_version` 变化、视图切换时调）。
    pub fn invalidate(&mut self, len: usize, version: u64) {
        self.len = len;
        self.version = version;
        self.window_start = 0;
        self.window_rows.clear();
        self.misses = 0;
    }
}

/// 组边界（`07` §5.2 SCALE-02）。
///
/// - 「按文件夹分组」：直接用 `folder_counts`，**零额外查询**
/// - 「按日期分组」：一次 `GROUP BY date(...)` 聚合
///
/// ⚠️ 组头 N 必须与 `folder_counts` / `folder_totals` **同源**，
/// 否则「组头说 5 张、点进去 6 张」——那是六处一致的直接破口。
fn build_group_bounds(
    db: &Db,
    scope: &Scope,
    sort: &SortSpec,
    folder_ids: &Option<Vec<i64>>,
    counts: &picasa_db::CountSnapshot,
) -> Result<Vec<GroupBound>> {
    let total = db.count_photos_visible(folder_ids.as_deref())? as usize;
    if total == 0 {
        return Ok(Vec::new());
    }
    // 目录分组只在「按文件夹分组」时有意义；本仓库的栅格目前只有日期分组
    // （`GroupBy` 是 M4 的 `Query` 概念）。这里保留通用形状：
    // 非时间排序时退化为「一组」，标签用排序键名。
    let group_by_folder = matches!(sort.key, SortKey::Name) && matches!(scope, Scope::Subtree { .. });

    if group_by_folder {
        // 子树按文件夹分组：DFS 序 + folder_counts（零额外查询）
        let Some(folders) = scope.folders() else {
            return Ok(vec![GroupBound {
                start: 0,
                len: total,
                label: String::new(),
                date: None,
            }]);
        };
        let mut out = Vec::new();
        let mut start = 0usize;
        // ⚠️ 这里用 `direct` 而不是 `totals`：组边界要的是「这个组的行数」，
        // 递归数会把子孙的行重复算进去。
        for f in folders {
            let n = counts.direct.get(f).copied().unwrap_or(0) as usize;
            if n == 0 {
                continue;
            }
            out.push(GroupBound {
                start,
                len: n,
                label: db.folder_path(*f).unwrap_or_else(|_| f.to_string()),
                date: None,
            });
            start += n;
        }
        return Ok(out);
    }

    // 按日期分组：一次聚合
    date_group_bounds(db, folder_ids.as_deref())
}

/// 按天分组的边界（`07` §5.2 SCALE-02 的聚合查询）。
///
/// `GROUP BY date(eff_taken_at, 'unixepoch', 'localtime')` 必须与
/// `library::group_by_day` 用**同一个时区口径**，否则组头与栅格会对不上。
/// 这里显式加 `'localtime'`，与 UI 侧 `chrono::Local` 一致。
fn date_group_bounds(db: &Db, folder_ids: Option<&[i64]>) -> Result<Vec<GroupBound>> {
    let rows = db.date_group_counts(folder_ids)?;
    let mut out = Vec::with_capacity(rows.len());
    let mut start = 0usize;
    for (day, n) in rows {
        let n = n as usize;
        if n == 0 {
            continue;
        }
        out.push(GroupBound {
            start,
            len: n,
            label: day_label(day),
            date: Some(day),
        });
        start += n;
    }
    Ok(out)
}

/// 日期组头文案。与 `library::group_by_day` 的格式保持一字不差。
fn day_label(day_secs: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(day_secs, 0).single() {
        Some(dt) => dt.format("%Y年%m月%d日").to_string(),
        None => "未知日期".to_string(),
    }
}

/// 键集取行（`07` §5.2 SCALE-03）。
///
/// ## 为什么需要「上一窗口的最后一行」当游标
///
/// 当请求的区间**紧接**在当前窗口之后（最常见：往下滚），可以用窗口末行做键集
/// 游标，只取增量；否则退回 `OFFSET`（跨窗口跳转本来就是冷路径，一次 OFFSET 可接受）。
///
/// ⚠️ 键集比较必须与 `ORDER BY` **逐字一致**，包括 `id` 兜底。
/// 少写 `id` 就会跳过「同一秒拍摄」的那批照片——这是键集分页最经典的坑。
fn fetch_keyset(
    db: &Db,
    scope: &Scope,
    sort: SortSpec,
    start: usize,
    n: usize,
    cursor: Option<Cursor>,
) -> Result<Vec<PhotoRow>> {
    let folder_ids: Option<Vec<i64>> = match scope {
        Scope::Folder(id) => Some(vec![*id]),
        Scope::Subtree {
            ordered_folders, ..
        } => Some(ordered_folders.to_vec()),
        Scope::AllPhotos => None,
    };
    Ok(db.range_keyset(
        folder_ids.as_deref(),
        sort,
        start,
        n,
        cursor.map(|c| (c.sort_value, c.id)),
    )?)
}

/// 一行的排序键值（与 `SortSpec::order_by` 的口径一致）。
fn sort_value_of(p: &PhotoRow, key: SortKey) -> i64 {
    match key {
        SortKey::TakenAt => p.effective_taken_at(),
        SortKey::Name => 0, // 文件名排序用 name_key，键集游标走 `(name_key, id)`，见 db 侧
        SortKey::ModifiedAt => p.mtime_ns,
        SortKey::Size => p.size_bytes,
        SortKey::Dimensions => p.width.unwrap_or(0) * p.height.unwrap_or(0),
    }
}

/// 定位照片下标。按日期 / 文件大小 / 修改时间排序时用**计数**而不是数行：
/// `COUNT(*) WHERE (sort_key, id) > (?, ?)` 走索引，O(log n)；
/// 逐行比较要读全表。
fn index_of_photo(
    db: &Db,
    scope: &Scope,
    sort: SortSpec,
    photo_id: i64,
) -> Result<Option<usize>> {
    let folder_ids: Option<Vec<i64>> = match scope {
        Scope::Folder(id) => Some(vec![*id]),
        Scope::Subtree {
            ordered_folders, ..
        } => Some(ordered_folders.to_vec()),
        Scope::AllPhotos => None,
    };
    Ok(db.index_of_photo(folder_ids.as_deref(), sort, photo_id)?)
}

/// 这个范围里还需要生成缩略图的照片 id（`07` §5.2 SCALE-04 / SCALE-05）。
///
/// `IN (...)` 列表限长 [`IN_LIST_MAX`]：`07` §5.2 SCALE-05 的「`IN` 列表 ≤ 200」。
pub fn thumb_ids_in(rows: &[PhotoRow]) -> Vec<i64> {
    rows.iter()
        .filter(|p| p.thumb_state == ThumbState::Ok as i64)
        .map(|p| p.id)
        .take(IN_LIST_MAX)
        .collect()
}

/// 把「屏」换算成行数（给上层算缓存窗口用）。
pub fn rows_per_screens(screens: usize, cols: usize) -> usize {
    ROWS_PER_SCREEN_ESTIMATE.max(1) * screens.max(1) * cols.max(1)
}

/// 组边界的二分查找：`index` 落在哪一组（日期气泡与吸顶组头用）。
pub fn group_of(groups: &[GroupBound], index: usize) -> Option<usize> {
    if groups.is_empty() {
        return None;
    }
    let mut lo = 0usize;
    let mut hi = groups.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if groups[mid].start <= index {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo.checked_sub(1)
}

/// 滚到 `y` 像素时顶部可见的组（`07` UI-04 滚动条日期气泡的数据来源）。
pub fn group_at_offset(groups: &[GroupBound], index: usize) -> Option<&GroupBound> {
    group_of(groups, index).and_then(|i| groups.get(i))
}

/// 一组 id 的区间表示（给 `range()` 做下标 → 全局行号映射）。
pub fn window_of(groups: &[GroupBound], group: usize) -> Option<Range<usize>> {
    groups.get(group).map(|g| g.start..g.end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_lookup_binary_search_is_correct() {
        let groups = vec![
            GroupBound {
                start: 0,
                len: 3,
                label: "a".into(),
                date: None,
            },
            GroupBound {
                start: 3,
                len: 2,
                label: "b".into(),
                date: None,
            },
            GroupBound {
                start: 5,
                len: 1,
                label: "c".into(),
                date: None,
            },
        ];
        assert_eq!(group_of(&groups, 0), Some(0));
        assert_eq!(group_of(&groups, 2), Some(0));
        assert_eq!(group_of(&groups, 3), Some(1));
        assert_eq!(group_of(&groups, 4), Some(1));
        assert_eq!(group_of(&groups, 5), Some(2));
        // 越界：最后一个组（滚动时可能会问到 +1 的位置）
        assert_eq!(group_of(&groups, 99), Some(2));
        assert_eq!(group_at_offset(&groups, 3).map(|g| g.label.as_str()), Some("b"));
    }

    #[test]
    fn empty_groups_yield_none() {
        assert_eq!(group_of(&[], 0), None);
        assert_eq!(group_at_offset(&[], 5), None);
    }

    #[test]
    fn thumb_ids_cap_at_the_in_list_maximum() {
        // SCALE-05：`IN` 列表 ≤ 200。超出部分不请求——剩下的下一帧再要，
        // 而不是一条 2000 个参数的 SQL（那会让优化器放弃索引）。
        let rows: Vec<PhotoRow> = (0..500)
            .map(|i| PhotoRow {
                id: i,
                folder_id: 1,
                folder_path: "/p".into(),
                name: format!("{i}.jpg"),
                size_bytes: 1,
                mtime_ns: 1,
                width: Some(1),
                height: Some(1),
                orientation: Some(1),
                taken_at: Some(1),
                taken_at_src: 0,
                thumb_state: ThumbState::Ok as i64,
                decoder_ver: 0,
                eff_taken_at: 1,
                starred: false,
            })
            .collect();
        assert_eq!(thumb_ids_in(&rows).len(), IN_LIST_MAX);
    }

    #[test]
    fn thumb_ids_skip_rows_that_have_no_thumb_yet() {
        // `thumb_state != Ready` 的行根本没有 BLOB 可读，把它们放进 `IN` 只是白占名额。
        let mut rows: Vec<PhotoRow> = (0..10)
            .map(|i| PhotoRow {
                id: i,
                folder_id: 1,
                folder_path: "/p".into(),
                name: format!("{i}.jpg"),
                size_bytes: 1,
                mtime_ns: 1,
                width: Some(1),
                height: Some(1),
                orientation: Some(1),
                taken_at: Some(1),
                taken_at_src: 0,
                thumb_state: ThumbState::Ok as i64,
                decoder_ver: 0,
                eff_taken_at: 1,
                starred: false,
            })
            .collect();
        rows[0].thumb_state = ThumbState::Pending as i64;
        rows[1].thumb_state = ThumbState::Failed as i64;
        let ids = thumb_ids_in(&rows);
        assert_eq!(ids.len(), 8);
        assert!(!ids.contains(&0));
        assert!(!ids.contains(&1));
    }

    #[test]
    fn rows_per_screens_scales_with_columns() {
        let a = rows_per_screens(2, 6);
        let b = rows_per_screens(2, 12);
        assert_eq!(b, a * 2, "列数翻倍 → 每屏行数翻倍");
        // 0 参数不该返回 0（会导致窗口为空、每帧都 miss）
        assert!(rows_per_screens(0, 0) > 0);
    }
}
