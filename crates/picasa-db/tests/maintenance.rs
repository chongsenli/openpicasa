//! F 加固 · 诊断页背后的数据接口：**缓存占用统计 / 清理缩略图 / VACUUM**。
//!
//! 这三件事的契约很简单，但每一条都有「看起来做了其实没做」的陷阱：
//! - `cache_stats` 必须真的读库（不是数内存），否则诊断页显示的是假数；
//! - `clear_all_thumbs` 只删缩略图，**照片一行不少**，且把状态标回「待生成」；
//! - `vacuum` 在 WAL 下必须能跑通（不先 checkpoint 容易报占用）。

use picasa_db::{Db, PhotoInput, SortSpec, ThumbBuf};

fn input(name: &str) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_lowercase(),
        size_bytes: 10,
        mtime_ns: 1,
        width: Some(100),
        height: Some(80),
        orientation: Some(1),
        taken_at: Some(1_300_000_000),
        taken_at_src: 0,
    }
}

#[test]
fn cache_stats_count_thumbs_and_clear_drops_only_thumbs() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();

    let key = "/x/photos".to_string();
    let fid = db.upsert_folder(&key, &key, None).unwrap();
    let items = vec![input("a.jpg"), input("b.jpg")];
    db.apply_scan(fid, &key, &items, 0).unwrap();

    let rows = db.photos(None, SortSpec::default(), 10).unwrap();
    assert_eq!(rows.len(), 2);

    // 给两张都写一条 100 字节的缩略图
    for r in &rows {
        db.put_thumb(
            r.id,
            0,
            &ThumbBuf {
                w: 8,
                h: 8,
                format: 0,
                data: vec![0u8; 100],
            },
        )
        .unwrap();
    }

    let s = db.cache_stats().unwrap();
    assert_eq!(s.photo_count, 2, "照片数来自 SQL 聚合");
    assert_eq!(s.thumb_count, 2);
    assert_eq!(s.thumb_bytes, 200);

    // ---- 清理：只删缩略图
    let removed = db.clear_all_thumbs().unwrap();
    assert_eq!(removed, 2);
    let s2 = db.cache_stats().unwrap();
    assert_eq!(s2.thumb_count, 0);
    assert_eq!(s2.thumb_bytes, 0);
    assert_eq!(s2.photo_count, 2, "清缩略图不能删照片");

    let rows2 = db.photos(None, SortSpec::default(), 10).unwrap();
    assert_eq!(rows2.len(), 2);
    assert!(
        rows2.iter().all(|r| r.thumb_state == 0),
        "清空后应标回「待生成」，否则永远不再重建"
    );

    // ---- VACUUM 不报错（WAL 下要先 checkpoint）
    db.vacuum().unwrap();
}

#[test]
fn cache_stats_on_empty_library_is_zero_not_error() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("empty.sqlite")).unwrap();
    let s = db.cache_stats().unwrap();
    assert_eq!(s.photo_count, 0);
    assert_eq!(s.thumb_count, 0);
    assert_eq!(s.thumb_bytes, 0);
    assert!(s.db_bytes > 0, "空库也有文件头，字节数不该是 0");
}
