//! 身份稳定：改名 / 移动后 `photo_id` 不变（`07` §2.2 WATCH-09/10，R-M3-9）。
//!
//! ## 为什么这件事必须做
//!
//! M4 的星标 / 标签 / 相册全部以 `photo_id` 为键（`08` §2.2「身份即标记寿命」）。
//! 如果用户把 `IMG_0001.JPG` 改名为 `2019北京.jpg`，而软件把它当成
//! 「删掉一张 + 新增一张」，那么**所有标记都会消失**——用户会觉得软件丢了他的数据。
//!
//! 而改名是**极其常见**的操作：从相机导入后批量改名、按旅行地重命名、
//! 从手机导出后整理。
//!
//! ## 怎么做（两条路，成本差一个数量级）
//!
//! | 路径 | 判据 | 成本 |
//! | --- | --- | --- |
//! | 同目录改名 | 同目录内 `removed` 与 `added` 按 `(size, mtime)` 配对 | 一次索引查询 |
//! | 跨目录移动 | `removed` 进「待配对池」，存活 10 s，池内按 `(size, mtime)` 与同根 `added` 配对 | 同上 |
//!
//! `head_hash`（前 64 KB 的 blake3）是**消歧的最后一招**：同一 `(size, mtime)`
//! 有多个候选时才读文件（`07` WATCH-10）。P-22 建议先只用 `(size, mtime)`，
//! 统计歧义率 > 1% 再开——所以这里把它做成**可选注入**，默认不读盘。
//!
//! ## 红线
//!
//! - **只在同根内配对**（`07` §10 非目标：跨根移动不做）。两个 root 可能在
//!   不同卷上，`(size, mtime)` 碰撞风险高，退化为删 + 增更诚实。
//! - `head_hash` 只读 `O_RDONLY`（R-1），不缓存到照片目录。
//! - 配对失败**允许**退化为删 + 增，但必须记日志并计数（R-M3-9）。

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 待配对池的存活时间（`07` 附录 A `watch.move_match_window_ms = 10 000`）。
///
/// 为什么是 10 s：跨目录移动在文件系统上是「先在新目录出现、再从旧目录消失」
/// （或反过来），两个事件可能隔几秒。窗口太短会配不上，太长会把
/// 「真的删了 + 真的新增了」错配成一次移动。
pub const MOVE_MATCH_WINDOW_MS: u64 = 10_000;

/// 同目录改名与跨目录移动的窗口语义差异（文档常量，供诊断页引用）。
///
/// 同目录改名**不需要窗口**：`removed` 与 `added` 来自同一次扫描，当场配上。
/// 跨目录移动需要 [`MOVE_MATCH_WINDOW_MS`]：两个事件可能隔几秒。
pub const FILE_MATCH_WINDOW_DOC: &str = "同目录改名即时配对；跨目录移动窗口 10 s";

/// 消歧时读多少字节算 `head_hash`（`07` 附录 A `watch.head_hash_bytes = 65 536`）。
pub const HEAD_HASH_BYTES: usize = 65_536;

/// 一张照片的身份键（用于配对）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PhotoKey {
    pub photo_id: i64,
    /// 文件大小。与 mtime 一起构成**唯一可用的廉价指纹**。
    pub size: i64,
    /// 纳秒 mtime（与 `photos.mtime_ns` 同口径）。
    pub mtime_ns: i64,
}

/// 一个「刚在磁盘上出现」的文件（用来跟待配对池比对）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileMeta {
    pub size: i64,
    pub mtime_ns: i64,
    /// 可选的头哈希（只在需要消歧时有值）
    pub head_hash: Option<[u8; 32]>,
}

/// 配对结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchOutcome {
    /// 配对成功：`photo_id` 保持不变，只需要更新 `folder_id` / `filename`
    Moved {
        photo_id: i64,
        /// 配对是靠 `(size, mtime)` 还是靠 `head_hash` 消歧的
        by_hash: bool,
    },
    /// 无法配对：按 M2.1 的删除语义处理（删 + 增）
    Unmatched,
}

/// 一次配对尝试的统计（进诊断；R-M3-9 要求「退化必须计数」）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MoveStats {
    /// 同目录改名配上的
    pub renamed: u64,
    /// 跨目录移动配上的
    pub moved: u64,
    /// 靠 head_hash 消歧配上的
    pub disambiguated_by_hash: u64,
    /// 歧义（多个候选、且没有 head_hash 或哈希也相同）→ 放弃配对
    pub ambiguous: u64,
    /// 池超时未配对的（退化为删 + 增）
    pub expired: u64,
}

impl MoveStats {
    /// 保留率 = 配对成功 / (成功 + 退化)。`07` §1.3 要求同目录改名 + 同根跨目录移动
    /// 的保留率 **≥ 99%**。
    pub fn retention(&self) -> f64 {
        let ok = (self.renamed + self.moved) as f64;
        let total = (ok + self.ambiguous as f64 + self.expired as f64).max(1.0);
        ok / total
    }

    /// 诊断文案。
    pub fn summary(&self) -> String {
        format!(
            "改名 {} · 移动 {} · 哈希消歧 {} · 歧义 {} · 超时 {} · 保留率 {:.1}%",
            self.renamed,
            self.moved,
            self.disambiguated_by_hash,
            self.ambiguous,
            self.expired,
            self.retention() * 100.0
        )
    }
}

/// 待配对池 + 配对逻辑（`07` §9.1 的 `MoveMatcher`）。
///
/// ## 为什么按 `(root_id, size, mtime)` 建桶
///
/// 一次「整个相册目录改名」会同时产生几百个 `removed` 与几百个 `added`。
/// 逐个线性比对是 O(n²)：500 张就是 25 万次比较。按 `(size, mtime)` 建哈希桶
/// 后每个 `added` 只查一个桶，O(n)。
///
/// 桶里**可能**有多个候选（同一批拷贝出来的文件大小 mtime 常常一样），
/// 那正是需要 `head_hash` 消歧的场合。
#[derive(Debug, Default)]
pub struct MoveMatcher {
    /// `(root_id, size, mtime_ns)` → 待配对的照片
    pool: HashMap<(i64, i64, i64), Vec<PoolEntry>>,
    stats: MoveStats,
}

#[derive(Debug, Clone, Copy)]
struct PoolEntry {
    key: PhotoKey,
    head_hash: Option<[u8; 32]>,
    added_at: Instant,
}

impl MoveMatcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> MoveStats {
        self.stats
    }

    pub fn pool_len(&self) -> usize {
        self.pool.values().map(Vec::len).sum()
    }

    /// 重置统计（每轮校验结束时调用一次，诊断看的是**本轮**的数字）。
    pub fn reset_stats(&mut self) {
        self.stats = MoveStats::default();
    }

    /// 一个照片从磁盘消失了 → 进待配对池（`07` §9.1 `on_removed`）。
    pub fn on_removed(&mut self, root_id: i64, key: PhotoKey, now: Instant) {
        self.on_removed_with_hash(root_id, key, None, now);
    }

    /// 同上，但带上已知的 `head_hash`（若之前为消歧算过）。
    pub fn on_removed_with_hash(
        &mut self,
        root_id: i64,
        key: PhotoKey,
        head_hash: Option<[u8; 32]>,
        now: Instant,
    ) {
        self.pool
            .entry((root_id, key.size, key.mtime_ns))
            .or_default()
            .push(PoolEntry {
                key,
                head_hash,
                added_at: now,
            });
    }

    /// 一个文件在磁盘上出现了 → 试着从池里认领它（`07` §9.1 `try_match_added`）。
    ///
    /// `read_head` 是**惰性**的：只有在桶里有多个候选**且**它们无法用
    /// 已有的 `head_hash` 区分时才会调它。这样 99% 的移动（桶里只有一个候选）
    /// 完全不用读文件。
    ///
    /// ⚠️ `read_head` 的实现必须只读（R-1）。这里的签名不知道路径，
    /// 由调用方闭包提供——本模块因此完全不碰文件系统。
    pub fn try_match_added(
        &mut self,
        root_id: i64,
        meta: FileMeta,
        now: Instant,
        read_head: &dyn Fn() -> Option<[u8; 32]>,
    ) -> MatchOutcome {
        let bucket_key = (root_id, meta.size, meta.mtime_ns);
        let Some(bucket) = self.pool.get_mut(&bucket_key) else {
            return MatchOutcome::Unmatched;
        };
        if bucket.is_empty() {
            return MatchOutcome::Unmatched;
        }

        // 单个候选：直接认领（最常见的情况，零读盘）
        //
        // ⚠️ `by_hash` 描述的是「这次配对**是否依赖**哈希」，不是「池里有没有哈希」。
        // 单候选时它一定是 false——哪怕这个候选之前为别的原因算过哈希。
        if bucket.len() == 1 {
            let entry = bucket.remove(0);
            self.stats.moved += 1;
            return MatchOutcome::Moved {
                photo_id: entry.key.photo_id,
                by_hash: false,
            };
        }

        // 多个候选：先看池里有没有已经算过、且能唯一定位的哈希
        let wanted = meta.head_hash.or_else(read_head);
        if let Some(want) = wanted {
            let hits: Vec<usize> = bucket
                .iter()
                .enumerate()
                .filter(|(_, e)| e.head_hash == Some(want))
                .map(|(i, _)| i)
                .collect();
            if hits.len() == 1 {
                let entry = bucket.remove(hits[0]);
                self.stats.moved += 1;
                self.stats.disambiguated_by_hash += 1;
                return MatchOutcome::Moved {
                    photo_id: entry.key.photo_id,
                    by_hash: true,
                };
            }
            // 哈希也区分不了（同一个文件被拷了两份），或池里还没算过哈希
            if hits.len() > 1 {
                self.stats.ambiguous += 1;
                return MatchOutcome::Unmatched;
            }
        }

        // 没有可用哈希、又有多候选 → **放弃配对**（R-M3-9：允许退化，但必须计数）
        //
        // 为什么不去猜「取最早进池的那个」：猜错比不猜更糟——
        // 用户会看到「A 的星标跑到了 B 身上」，那比星标消失更让人不信任软件。
        self.stats.ambiguous += 1;
        let _ = now;
        MatchOutcome::Unmatched
    }

    /// 同目录改名（`07` WATCH-09 的前半段）。
    ///
    /// 这条路**不需要池**：同一次目录扫描里，`removed[]` 与 `added[]` 都在手上，
    /// 直接按 `(size, mtime)` 配对即可。比跨目录那条简单得多，也更可靠
    /// （同一个目录里的 `(size, mtime)` 碰撞概率远低于全根）。
    pub fn match_rename(
        &mut self,
        removed: &[PhotoKey],
        added: &[FileMeta],
        read_head: &dyn Fn(&PhotoKey) -> Option<[u8; 32]>,
    ) -> Vec<(PhotoKey, usize)> {
        if removed.is_empty() || added.is_empty() {
            return Vec::new();
        }
        // 按 (size, mtime) 建桶
        let mut buckets: HashMap<(i64, i64), Vec<PhotoKey>> = HashMap::new();
        for r in removed {
            buckets
                .entry((r.size, r.mtime_ns))
                .or_default()
                .push(*r);
        }

        let mut out = Vec::new();
        let mut used: Vec<PhotoKey> = Vec::new();
        for (i, a) in added.iter().enumerate() {
            let Some(cands) = buckets.get_mut(&(a.size, a.mtime_ns)) else {
                continue;
            };
            // 剔除已经被用掉的
            cands.retain(|c| !used.contains(c));
            match cands.len() {
                0 => continue,
                1 => {
                    let r = cands.remove(0);
                    used.push(r);
                    self.stats.renamed += 1;
                    out.push((r, i));
                }
                _ => {
                    // 多候选：先用头哈希消歧
                    let want = a.head_hash.or_else(|| cands.first().and_then(read_head));
                    if let Some(want) = want {
                        let hits: Vec<usize> = cands
                            .iter()
                            .enumerate()
                            .filter(|(_, c)| read_head(c) == Some(want))
                            .map(|(i, _)| i)
                            .collect();
                        if hits.len() == 1 {
                            let r = cands.remove(hits[0]);
                            used.push(r);
                            self.stats.renamed += 1;
                            self.stats.disambiguated_by_hash += 1;
                            out.push((r, i));
                            continue;
                        }
                        self.stats.ambiguous += 1;
                        continue;
                    }

                    // ⚠️ **没有哈希可用、且候选完全不可区分** → 按顺序一对一配。
                    //
                    // 与跨目录移动不同：这里 `removed` 与 `added` 来自**同一个目录的
                    // 同一次扫描**，数量与指纹都一一对应。「放弃配对」会让两张
                    // 完全相同的照片都被记成「删一张 + 增一张」，而它们的可观测结果
                    // （谁的星标给谁）本来就不可区分——配对比不配对更好。
                    //
                    // 跨目录移动不做这个放宽：那时 `added` 可能来自完全不同的目录，
                    // 「凑巧大小 mtime 一样」的概率比同目录高得多。
                    let r = cands.remove(0);
                    used.push(r);
                    self.stats.renamed += 1;
                    out.push((r, i));
                }
            }
        }
        out
    }

    /// 清掉超时未配对的（`07` §9.1 `expire`）。
    ///
    /// 返回应该按「删除」处理的照片 key。调用方把它投递给落库线程。
    pub fn expire(&mut self, now: Instant) -> Vec<PhotoKey> {
        let window = Duration::from_millis(MOVE_MATCH_WINDOW_MS);
        let mut out = Vec::new();
        self.pool.retain(|_, entries| {
            entries.retain(|e| {
                if now.saturating_duration_since(e.added_at) >= window {
                    out.push(e.key);
                    false
                } else {
                    true
                }
            });
            !entries.is_empty()
        });
        self.stats.expired += out.len() as u64;
        out
    }

    /// 清空（根关闭 / 卸载监视时）。
    pub fn clear(&mut self) {
        self.pool.clear();
    }
}

/// 算「文件头 64 KB」的 blake3（`07` WATCH-10）。
///
/// ⚠️ 本仓库没有 `blake3` 依赖。为了**不为一个消歧辅助拉一棵新依赖树**，
/// 这里用一个明确的替代：`fnv1a-64` 双通道（两个不同初值）拼成 16 字节。
///
/// 这不是密码学哈希，但用它来判断「两份文件是不是同一份」完全够用：
/// 判据是「前 64 KB 逐字节相同」，而 FNV 对随机差异的碰撞率约 2^-64。
/// 真正的风险是**恶意构造碰撞**——而这里的输入是用户自己的照片，
/// 不是攻击面。
///
/// > 若将来引入 `blake3`（P-22 打开 `head_hash` 时），只换这个函数，
/// > `[u8; 32]` 的类型不变。剩余 16 字节填 0 并在比较时只比前 16 字节。
pub fn head_hash(bytes: &[u8]) -> [u8; 32] {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut a = FNV_OFFSET;
    let mut b = FNV_OFFSET ^ 0x9e37_79b9_7f4a_7c15;
    for &byte in bytes {
        a ^= u64::from(byte);
        a = a.wrapping_mul(FNV_PRIME);
        // 第二条通道用不同的混合顺序，降低「同一输入两类都撞」的概率
        b = b.rotate_left(5) ^ u64::from(byte);
        b = b.wrapping_mul(FNV_PRIME);
    }
    let mut out = [0u8; 32];
    out[..8].copy_from_slice(&a.to_le_bytes());
    out[8..16].copy_from_slice(&b.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ms: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_millis(ms)
    }

    fn key(id: i64, size: i64, mtime: i64) -> PhotoKey {
        PhotoKey {
            photo_id: id,
            size,
            mtime_ns: mtime,
        }
    }

    fn meta(size: i64, mtime: i64) -> FileMeta {
        FileMeta {
            size,
            mtime_ns: mtime,
            head_hash: None,
        }
    }

    fn no_hash() -> Option<[u8; 32]> {
        None
    }

    #[test]
    fn a_single_candidate_is_claimed_without_reading_the_file() {
        // 绝大多数移动都是这一种：桶里只有一个候选，零读盘。
        let mut m = MoveMatcher::new();
        m.on_removed(1, key(42, 1000, 500), t(0));
        let read = std::cell::Cell::new(0);
        let out = m.try_match_added(1, meta(1000, 500), t(10), &|| {
            read.set(read.get() + 1);
            None
        });
        assert_eq!(
            out,
            MatchOutcome::Moved {
                photo_id: 42,
                by_hash: false
            }
        );
        assert_eq!(read.get(), 0, "★ 单候选时不该读任何文件（R-1 与性能都要求）");
        assert_eq!(m.stats().moved, 1);
    }

    #[test]
    fn different_size_or_mtime_does_not_match() {
        let mut m = MoveMatcher::new();
        m.on_removed(1, key(42, 1000, 500), t(0));
        // 大小不同
        assert_eq!(
            m.try_match_added(1, meta(2000, 500), t(10), &no_hash),
            MatchOutcome::Unmatched
        );
        // mtime 不同
        assert_eq!(
            m.try_match_added(1, meta(1000, 900), t(10), &no_hash),
            MatchOutcome::Unmatched
        );
    }

    #[test]
    fn matching_is_scoped_to_one_root() {
        // `07` §10 非目标：跨根移动不做。两个 root 可能在不同卷上，
        // `(size, mtime)` 碰撞风险高，退化为删 + 增更诚实。
        let mut m = MoveMatcher::new();
        m.on_removed(1, key(42, 1000, 500), t(0));
        assert_eq!(
            m.try_match_added(2, meta(1000, 500), t(10), &no_hash),
            MatchOutcome::Unmatched,
            "★ 不许跨根配对"
        );
    }

    #[test]
    fn multiple_candidates_use_the_head_hash_to_disambiguate() {
        // 同一批拷贝出来的文件 size/mtime 完全一样 → 必须靠头哈希区分。
        let mut m = MoveMatcher::new();
        let h1 = head_hash(b"file-one-content");
        let h2 = head_hash(b"file-two-content");
        assert_ne!(h1, h2);
        m.on_removed_with_hash(1, key(1, 1000, 500), Some(h1), t(0));
        m.on_removed_with_hash(1, key(2, 1000, 500), Some(h2), t(0));

        let mut target = meta(1000, 500);
        target.head_hash = Some(h2);
        let out = m.try_match_added(1, target, t(10), &no_hash);
        assert_eq!(
            out,
            MatchOutcome::Moved {
                photo_id: 2,
                by_hash: true
            },
            "应当靠哈希认领 id=2"
        );
        assert_eq!(m.stats().disambiguated_by_hash, 1);
        // 剩下的那个还能被下一个 added 认领。
        // 此时桶里只剩一个候选 → 不需要哈希 → `by_hash: false`
        // （`by_hash` 描述的是「这次配对是否依赖哈希」，不是「有没有哈希」）
        let mut target2 = meta(1000, 500);
        target2.head_hash = Some(h1);
        assert_eq!(
            m.try_match_added(1, target2, t(20), &no_hash),
            MatchOutcome::Moved {
                photo_id: 1,
                by_hash: false
            }
        );
    }

    #[test]
    fn ambiguous_matches_are_refused_rather_than_guessed() {
        // 多个候选 + 没有哈希 → **放弃配对**，不猜。
        // 猜错比不猜更糟：用户会看到「A 的星标跑到 B 身上」，
        // 那比星标消失更让人不信任软件。
        let mut m = MoveMatcher::new();
        m.on_removed(1, key(1, 1000, 500), t(0));
        m.on_removed(1, key(2, 1000, 500), t(0));
        let out = m.try_match_added(1, meta(1000, 500), t(10), &no_hash);
        assert_eq!(out, MatchOutcome::Unmatched);
        assert_eq!(m.stats().ambiguous, 1);
        // 但池里两条都还在（可能后面有更明确的 added 来认领）
        assert_eq!(m.pool_len(), 2);
    }

    #[test]
    fn lazy_head_reading_only_happens_on_ambiguity() {
        let mut m = MoveMatcher::new();
        m.on_removed(1, key(1, 1000, 500), t(0));
        m.on_removed(1, key(2, 1000, 500), t(0));
        let reads = std::cell::Cell::new(0);
        // 第一次 added 触发一次读（因为有两个候选）
        let _ = m.try_match_added(1, meta(1000, 500), t(10), &|| {
            reads.set(reads.get() + 1);
            None
        });
        assert_eq!(reads.get(), 1, "多候选时才读，且只读一次");
    }

    #[test]
    fn expired_entries_become_deletions() {
        // 池存活 10 s，超时的按删除处理（R-M3-9：允许退化，但必须计数）。
        let mut m = MoveMatcher::new();
        m.on_removed(1, key(7, 100, 200), t(0));
        // 还没到窗口
        assert!(m.expire(t(5000)).is_empty());
        assert_eq!(m.pool_len(), 1);
        // 超过窗口
        let expired = m.expire(t(MOVE_MATCH_WINDOW_MS + 1));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].photo_id, 7);
        assert_eq!(m.stats().expired, 1);
        assert_eq!(m.pool_len(), 0);
    }

    #[test]
    fn same_directory_rename_matches_by_size_and_mtime() {
        let mut m = MoveMatcher::new();
        let removed = vec![key(10, 1000, 500), key(11, 2000, 600)];
        let added = vec![meta(2000, 600), meta(1000, 500)];
        let pairs = m.match_rename(&removed, &added, &|_| None);
        assert_eq!(pairs.len(), 2, "两个都要配上");
        // added[0] (2000,600) ↔ id 11；added[1] (1000,500) ↔ id 10
        assert!(pairs.contains(&(key(11, 2000, 600), 0)));
        assert!(pairs.contains(&(key(10, 1000, 500), 1)));
        assert_eq!(m.stats().renamed, 2);
    }

    #[test]
    fn rename_with_one_side_empty_is_a_no_op() {
        let mut m = MoveMatcher::new();
        assert!(m.match_rename(&[], &[meta(1, 1)], &|_| None).is_empty());
        assert!(m.match_rename(&[key(1, 1, 1)], &[], &|_| None).is_empty());
        assert_eq!(m.stats().renamed, 0);
    }

    #[test]
    fn two_identically_sized_renames_do_not_steal_each_others_ids() {
        // 同一目录里两张大小 mtime 完全相同的照片同时改名：
        // 必须一张配一个，不能两张都配到同一个 id。
        let mut m = MoveMatcher::new();
        let removed = vec![key(1, 500, 100), key(2, 500, 100)];
        let added = vec![meta(500, 100), meta(500, 100)];
        let pairs = m.match_rename(&removed, &added, &|_| None);
        let ids: std::collections::HashSet<i64> = pairs.iter().map(|(k, _)| k.photo_id).collect();
        assert_eq!(pairs.len(), 2, "两对都要配上");
        assert_eq!(ids.len(), 2, "★ 不能两张都配到同一个 photo_id");
    }

    #[test]
    fn retention_is_one_when_nothing_degrades() {
        let mut m = MoveMatcher::new();
        m.stats.renamed = 100;
        m.stats.moved = 100;
        assert!((m.stats.retention() - 1.0).abs() < 1e-9);
        // 退化 1% → 约 0.99
        m.stats.expired = 2;
        let r = m.stats.retention();
        assert!(r > 0.98 && r < 1.0, "保留率 {r}");
    }

    #[test]
    fn stats_summary_is_human_readable() {
        let mut m = MoveMatcher::new();
        m.stats.renamed = 100;
        m.stats.moved = 100;
        let s = m.stats().summary();
        assert!(s.contains("改名 100"), "{s}");
        assert!(s.contains("保留率 100.0%"), "{s}");
    }

    #[test]
    fn head_hash_separates_similar_but_different_content() {
        // 前 64 KB 决定身份：改一个字节就要变
        let a = vec![7u8; 1000];
        let mut b = a.clone();
        b[500] = 8;
        assert_ne!(head_hash(&a), head_hash(&b));
        // 同样的内容必须稳定
        assert_eq!(head_hash(&a), head_hash(&vec![7u8; 1000]));
        // 空输入也不 panic
        let _ = head_hash(&[]);
    }

    #[test]
    fn clearing_the_pool_drops_everything() {
        let mut m = MoveMatcher::new();
        m.on_removed(1, key(1, 1, 1), t(0));
        m.on_removed(1, key(2, 2, 2), t(0));
        assert_eq!(m.pool_len(), 2);
        m.clear();
        assert_eq!(m.pool_len(), 0);
    }
}
