//! `Db::root_thumb_bytes` 的 `EXISTS` 写法必须与朴素 `JOIN` 写法算出**同一个数**。
//!
//! ## 这条测试为什么存在
//!
//! 2026-11-01 修过一个死机：点「索引」按钮后界面冻结 90 秒。根因是
//! `root_stats()` 里对每个根跑 `SUM(LENGTH(t.data))` 的 **JOIN** 写法，
//! 在 25.9 万张 / 1.26 GB 缩略图的库上要 112 秒；改成 `EXISTS` 后 0.28 秒。
//!
//! 但**快和算对是两件事**。半连接（`EXISTS`）最容易出的错是跨根重复计数或漏计，
//! 而这两种错在界面上都只是「数字不对」，没人会联想到是 SQL 写法的问题。
//! 所以这里用一份小固件，把 `EXISTS` 的结果和朴素 `JOIN` 的结果逐根对拍。
//!
//! 顺带守住 `root_stats()` 的计数改写：照片数改成对 `folders.direct_count`
//! 分组求和（4330 行 → 2 ms），也必须和逐行 `COUNT(*)` 一致。

use picasa_db::{Db, MIGRATIONS};
use std::path::{Path, PathBuf};

/// 每个用例一条**独立**路径：R-1 不允许删文件，所以靠唯一文件名而不是「先删再建」
/// 来保证用例之间不互相污染。
fn tmp_dir(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("op-thumbbytes-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 造一个 2 根 / 3 目录 / 6 照片 / 6 缩略图的库。
///
/// 缩略图长度刻意取 10/20/30/40/50/60，任一「算错」都会体现在数字上：
/// 根 1 = 照片 1,2,3（目录 1 的两个 + 子目录 2 的一个）= 60；
/// 根 2 = 照片 4,5,6 = 150。
fn make_fixture(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (
             version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
    )
    .unwrap();
    for m in MIGRATIONS.iter() {
        conn.execute_batch(m.sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations VALUES (?1, 0)",
            rusqlite::params![m.version],
        )
        .unwrap();
    }
    conn.execute_batch(
        r#"
        INSERT INTO roots (id, path, path_key) VALUES
            (1, '/a', '/a'),
            (2, '/b', '/b');
        INSERT INTO folders (id, path, path_key, root_id, parent_id, name,
                             direct_count, last_scan_at) VALUES
            (1, '/a',   '/a',   1, NULL, 'a', 2, 100),
            (2, '/a/x', '/a/x', 1, 1,    'x', 1, 100),
            (3, '/b',   '/b',   2, NULL, 'b', 3, 200);
        INSERT INTO photos (id, folder_id, name, name_key, size_bytes, mtime_ns,
                            thumb_state, decoder_ver) VALUES
            (1, 1, 'p1.jpg', 'p1.jpg', 1, 1, 1, 0),
            (2, 1, 'p2.jpg', 'p2.jpg', 1, 1, 1, 0),
            (3, 2, 'p3.jpg', 'p3.jpg', 1, 1, 1, 0),
            (4, 3, 'p4.jpg', 'p4.jpg', 1, 1, 1, 0),
            (5, 3, 'p5.jpg', 'p5.jpg', 1, 1, 1, 0),
            (6, 3, 'p6.jpg', 'p6.jpg', 1, 1, 1, 0);
        "#,
    )
    .unwrap();
    for (id, n) in [(1i64, 10usize), (2, 20), (3, 30), (4, 40), (5, 50), (6, 60)] {
        conn.execute(
            "INSERT INTO thumbs (photo_id, tier, w, h, format, data)
             VALUES (?1, 0, 8, 8, 0, ?2)",
            rusqlite::params![id, vec![7u8; n]],
        )
        .unwrap();
    }
}

fn naive_root_bytes(path: &Path, root_id: i64) -> u64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(LENGTH(t.data)), 0) FROM thumbs t
               JOIN photos  p ON p.id = t.photo_id
               JOIN folders f ON f.id = p.folder_id
              WHERE f.root_id = ?1",
            rusqlite::params![root_id],
            |r| r.get(0),
        )
        .unwrap();
    n as u64
}

#[test]
fn exists_form_matches_naive_join_per_root() {
    let dir = tmp_dir("per-root");
    let path = dir.join("library.sqlite");
    make_fixture(&path);

    let db = Db::open(&path).unwrap();
    let got = db.root_thumb_bytes(&[1, 2]).unwrap();

    // 先盯死绝对值：子目录的照片必须算进所属根（根 1 = 60，不是 30）
    assert_eq!(got.get(&1).copied(), Some(60), "根 1 含子目录 /a/x");
    assert_eq!(got.get(&2).copied(), Some(150), "根 2");

    // 再与朴素 JOIN 对拍——这是本测试真正要守的东西
    for rid in [1, 2] {
        assert_eq!(
            got.get(&rid).copied().unwrap_or(0),
            naive_root_bytes(&path, rid),
            "根 {rid}：EXISTS 写法与 JOIN 写法不一致"
        );
    }
}

#[test]
fn root_thumb_bytes_of_unknown_root_is_zero_not_error() {
    let dir = tmp_dir("unknown");
    let path = dir.join("library.sqlite");
    make_fixture(&path);

    let db = Db::open(&path).unwrap();
    let got = db.root_thumb_bytes(&[999]).unwrap();
    assert_eq!(got.get(&999).copied(), Some(0), "不存在的根应给 0，不是报错");
}

#[test]
fn thumb_cache_stats_matches_naive_total() {
    let dir = tmp_dir("total");
    let path = dir.join("library.sqlite");
    make_fixture(&path);

    let db = Db::open(&path).unwrap();
    let (count, bytes) = db.thumb_cache_stats().unwrap();

    let conn = rusqlite::Connection::open(&path).unwrap();
    let (n2, b2): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(data)), 0) FROM thumbs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    assert_eq!(count, n2 as u64);
    assert_eq!(bytes, b2 as u64);
    assert_eq!((count, bytes), (6, 210), "10+20+30+40+50+60");
}

#[test]
fn root_stats_counts_match_naive_per_photo_count() {
    let dir = tmp_dir("counts");
    let path = dir.join("library.sqlite");
    make_fixture(&path);

    let db = Db::open(&path).unwrap();
    let stats = db.root_stats().unwrap();

    let conn = rusqlite::Connection::open(&path).unwrap();
    for rs in &stats {
        let naive: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM photos p JOIN folders f ON f.id = p.folder_id
                  WHERE f.root_id = ?1",
                rusqlite::params![rs.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            rs.photo_count, naive as u64,
            "根 {} 的照片数：分组求和与逐行 COUNT 不一致",
            rs.id
        );
    }
    // 顺带钉住顺序与绝对值（界面按 path 排序显示）
    assert_eq!(stats.len(), 2);
    assert_eq!(stats[0].path, "/a");
    assert_eq!(stats[0].photo_count, 3);
    assert_eq!(stats[1].path, "/b");
    assert_eq!(stats[1].photo_count, 3);
    assert_eq!(stats[0].last_indexed_at, Some(100));
}
