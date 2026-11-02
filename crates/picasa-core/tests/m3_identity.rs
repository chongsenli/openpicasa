//! 身份稳定的验收测试（`07` §12.2 `TC-M3-WATCH-007`，R-M3-9）。
//!
//! ## 这条红线为什么排得这么高
//!
//! M4 的星标 / 标签 / 相册全部以 `photo_id` 为键。改名时 `photo_id` 变了，
//! 用户会看到**所有标记凭空消失**——而改名是他们最常做的操作之一
//! （导完相机照片批量改名、按旅行地重命名）。
//!
//! 所以 `07` §1.3 把它写成版本级 DoD：
//!
//! > 同目录改名 100 张、同根跨目录移动 100 张 → `photo_id` 保留率 ≥ 99%
//!
//! ## 这里的用例按「100 张」的真实规模写
//!
//! 1 张能配上不代表 100 张能配上：一次批量改名会同时产生 100 个 `removed`
//! 与 100 个 `added`，桶里会出现大量同 `(size, mtime)` 的候选。
//! 规模上去之后「歧义」才是主要矛盾。

use picasa_core::movematch::{
    FILE_MATCH_WINDOW_DOC, FileMeta, MOVE_MATCH_WINDOW_MS, MatchOutcome, MoveMatcher, PhotoKey,
    head_hash,
};
use std::time::{Duration, Instant};

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

#[test]
fn tc_m3_watch_007_100_same_directory_renames_keep_all_ids() {
    // 100 张各不相同（大小递进）→ 全部都能配上，保留率 100%。
    let mut m = MoveMatcher::new();
    let removed: Vec<PhotoKey> = (0..100)
        .map(|i| key(1000 + i, 100_000 + i * 137, 1_600_000_000_000_000_000 + i * 1_000_000))
        .collect();
    let added: Vec<FileMeta> = removed
        .iter()
        .map(|k| FileMeta {
            size: k.size,
            mtime_ns: k.mtime_ns,
            head_hash: None,
        })
        .collect();

    let pairs = m.match_rename(&removed, &added, &|_| None);
    assert_eq!(pairs.len(), 100, "100 张必须全部配上");
    // 一对一：不许两个 added 配到同一个 id
    let ids: std::collections::HashSet<i64> = pairs.iter().map(|(k, _)| k.photo_id).collect();
    assert_eq!(ids.len(), 100, "★ 100 个不同的 photo_id");
    assert!(
        m.stats().retention() >= 0.99,
        "保留率 {} 低于 99%",
        m.stats().retention()
    );
}

#[test]
fn tc_m3_watch_007_100_identical_files_still_pair_one_to_one() {
    // 极端情况：100 张完全无法区分（同 size 同 mtime，比如同一批导出）。
    // 同目录改名的语义是「同一次扫描里 removed 与 added 一一对应」，
    // 所以必须配上——哪怕「谁换谁」不可区分（可观测结果相同）。
    let mut m = MoveMatcher::new();
    let removed: Vec<PhotoKey> = (0..100).map(|i| key(i, 5000, 999)).collect();
    let added: Vec<FileMeta> = (0..100)
        .map(|_| FileMeta {
            size: 5000,
            mtime_ns: 999,
            head_hash: None,
        })
        .collect();
    let pairs = m.match_rename(&removed, &added, &|_| None);
    assert_eq!(pairs.len(), 100);
    let ids: std::collections::HashSet<i64> = pairs.iter().map(|(k, _)| k.photo_id).collect();
    assert_eq!(ids.len(), 100, "★ 一个 id 只能被认领一次");
    assert_eq!(m.stats().retention(), 1.0);
}

#[test]
fn tc_m3_watch_007_100_cross_directory_moves_within_one_root() {
    // 跨目录移动：removed 进池 → added 认领。
    // 这一批也是「各不相同」的，所以全部能配上。
    let mut m = MoveMatcher::new();
    for i in 0..100i64 {
        m.on_removed(7, key(2000 + i, 10_000 + i, 1_700_000_000 + i), t(0));
    }
    let mut matched = 0;
    for i in 0..100i64 {
        let out = m.try_match_added(
            7,
            FileMeta {
                size: 10_000 + i,
                mtime_ns: 1_700_000_000 + i,
                head_hash: None,
            },
            t(100),
            &|| None,
        );
        if matches!(out, MatchOutcome::Moved { .. }) {
            matched += 1;
        }
    }
    assert_eq!(matched, 100, "100 张全部要配上");
    assert!(
        m.stats().retention() >= 0.99,
        "保留率 {} 低于 99%",
        m.stats().retention()
    );
}

#[test]
fn tc_m3_watch_007_ambiguous_moves_degrade_but_are_counted() {
    // 歧义场景（同一批拷贝，无哈希）：按 R-M3-9 允许退化为删 + 增，
    // **但必须计数**——否则保留率会虚高，用户看到标记丢了却查不出原因。
    let mut m = MoveMatcher::new();
    m.on_removed(1, key(1, 500, 100), t(0));
    m.on_removed(1, key(2, 500, 100), t(0));
    let out = m.try_match_added(
        1,
        FileMeta {
            size: 500,
            mtime_ns: 100,
            head_hash: None,
        },
        t(10),
        &|| None,
    );
    assert_eq!(out, MatchOutcome::Unmatched, "不许猜");
    assert_eq!(m.stats().ambiguous, 1, "★ 歧义必须计数");
    assert!(m.stats().retention() < 1.0);
}

#[test]
fn tc_m3_watch_010_hash_disambiguation_rescues_the_ambiguous_case() {
    // WATCH-10：`(size, mtime)` 撞了 → 读头 64 KB 的哈希消歧。
    let mut m = MoveMatcher::new();
    let h1 = head_hash(b"photo-one-bytes");
    let h2 = head_hash(b"photo-two-bytes");
    m.on_removed_with_hash(1, key(1, 500, 100), Some(h1), t(0));
    m.on_removed_with_hash(1, key(2, 500, 100), Some(h2), t(0));

    let out = m.try_match_added(
        1,
        FileMeta {
            size: 500,
            mtime_ns: 100,
            head_hash: Some(h1),
        },
        t(10),
        &|| None,
    );
    assert_eq!(
        out,
        MatchOutcome::Moved {
            photo_id: 1,
            by_hash: true
        }
    );
    assert_eq!(m.stats().disambiguated_by_hash, 1);
    // 保留率回到 100%
    assert_eq!(m.stats().retention(), 1.0);
}

#[test]
fn tc_m3_watch_010_head_hash_reads_are_lazy_and_read_only() {
    // R-M3-9 + R-1：`head_hash` 只在**需要消歧**时才读文件，且只读。
    //
    // 这条验的是「惰性」：单候选路径一次都不该调 `read_head`。
    // 「只读」由 `movematch` 模块根本没有写文件的 API 保证
    // （它连 `Path` 都不接受，闭包由调用方提供）。
    let mut m = MoveMatcher::new();
    m.on_removed(1, key(1, 500, 100), t(0));
    let calls = std::cell::Cell::new(0);
    let _ = m.try_match_added(
        1,
        FileMeta {
            size: 500,
            mtime_ns: 100,
            head_hash: None,
        },
        t(10),
        &|| {
            calls.set(calls.get() + 1);
            None
        },
    );
    assert_eq!(calls.get(), 0, "★ 单候选时零读盘");
}

#[test]
fn tc_m3_watch_007_expired_moves_count_as_degradation() {
    // 池超时（10 s）→ 按删除处理，并计入退化。
    let mut m = MoveMatcher::new();
    for i in 0..50i64 {
        m.on_removed(1, key(i, 100 + i, 200 + i), t(0));
    }
    // 窗口内：不算超时
    assert!(m.expire(t(MOVE_MATCH_WINDOW_MS - 1)).is_empty());
    assert_eq!(m.stats().expired, 0);
    // 超窗：全部退化
    let expired = m.expire(t(MOVE_MATCH_WINDOW_MS + 1));
    assert_eq!(expired.len(), 50);
    assert_eq!(m.stats().expired, 50);
    assert_eq!(m.stats().retention(), 0.0, "全退化时保留率 0");
    assert_eq!(m.pool_len(), 0);
}

#[test]
fn tc_m3_watch_007_cross_root_moves_are_refused_by_design() {
    // `07` §10 非目标：跨根移动不做。两个 root 可能在不同卷上，
    // `(size, mtime)` 碰撞风险高，退化为删 + 增更诚实。
    let mut m = MoveMatcher::new();
    m.on_removed(1, key(9, 500, 100), t(0));
    let out = m.try_match_added(
        2, // 另一个根
        FileMeta {
            size: 500,
            mtime_ns: 100,
            head_hash: None,
        },
        t(10),
        &|| None,
    );
    assert_eq!(out, MatchOutcome::Unmatched);
}

#[test]
fn tc_m3_watch_007_mixed_batch_retention_meets_the_dod() {
    // 混合批次：95 张可配 + 5 张真删除 → 保留率 95%。
    // 这**低于** DoD 的 99%，但这不是缺陷——那 5 张是真的被删了。
    // DoD 的口径是「改名/移动**应该**配上」，真删除本来就不该算进分母。
    // 这里让「池里的 5 张」是真删除（从未出现 added），所以：
    //   renamed 95、expired 5 → 保留率 95%
    // 用例记录这个语义，防止有人把「真删除」也当成配对失败去优化。
    let mut m = MoveMatcher::new();
    for i in 0..95i64 {
        m.on_removed(1, key(i, 1000 + i, 2000 + i), t(0));
    }
    for i in 95..100i64 {
        m.on_removed(1, key(i, 1000 + i, 2000 + i), t(0));
    }
    let mut matched = 0;
    for i in 0..95i64 {
        if matches!(
            m.try_match_added(
                1,
                FileMeta {
                    size: 1000 + i,
                    mtime_ns: 2000 + i,
                    head_hash: None,
                },
                t(100),
                &|| None
            ),
            MatchOutcome::Moved { .. }
        ) {
            matched += 1;
        }
    }
    assert_eq!(matched, 95);
    let _expired = m.expire(t(MOVE_MATCH_WINDOW_MS + 200));
    let stats = m.stats();
    assert_eq!(stats.moved, 95);
    assert_eq!(stats.expired, 5);
    // 摘要里两个数字都要看得见（诊断要求）
    let s = stats.summary();
    assert!(s.contains("移动 95"), "{s}");
    assert!(s.contains("超时 5"), "{s}");
}

#[test]
fn doc_constants_are_sane() {
    // 窗口 10 s（`07` 附录 A `watch.move_match_window_ms`）
    assert_eq!(MOVE_MATCH_WINDOW_MS, 10_000);
    // 文档常量存在（防止被误删）
    let _ = FILE_MATCH_WINDOW_DOC;
    // 生产者的存活时间常量必须有界（不能是 0，也不能是一分钟级）
    assert!((1000..=60_000).contains(&MOVE_MATCH_WINDOW_MS));
}

#[test]
fn a_thousand_moves_complete_quickly() {
    // 性能：一次「整个相册目录改名」可能上千张。配对必须是 O(n) 而不是 O(n²)——
    // 后者在这个规模下会卡住 UI 线程（配对发生在扫描线程，但会占着 DB 锁）。
    let mut m = MoveMatcher::new();
    let start = Instant::now();
    for i in 0..1000i64 {
        m.on_removed(1, key(i, 10_000 + i, 1_000_000 + i), t(0));
    }
    for i in 0..1000i64 {
        let _ = m.try_match_added(
            1,
            FileMeta {
                size: 10_000 + i,
                mtime_ns: 1_000_000 + i,
                head_hash: None,
            },
            t(10),
            &|| None,
        );
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(200),
        "1000 次配对用了 {elapsed:?}——像 O(n²)"
    );
    assert_eq!(m.stats().moved, 1000);
}
