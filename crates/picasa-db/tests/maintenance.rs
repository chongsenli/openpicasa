//! F 加固 · 诊断页背后的数据接口：**缓存占用统计 / 清理缩略图 / VACUUM**。
//!
//! 这三件事的契约很简单，但每一条都有「看起来做了其实没做」的陷阱：
//! - `cache_stats` 必须真的读库（不是数内存），否则诊断页显示的是假数；
//! - `clear_all_thumbs` 只删缩略图，**照片一行不少**，且把状态标回「待生成」；
//! - `vacuum` 在 WAL 下必须能跑通（不先 checkpoint 容易报占用）。

use picasa_db::{Db, PhotoInput, SortSpec, ThumbBuf, ThumbState};

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

/// 「删除索引」：删干净索引数据，但**关注文件夹与排除规则必须留着**。
///
/// 这条测试的真正价值在第 3 段：`roots` / `exclusions` 是**用户配置**，
/// 不是索引产物。删索引顺手把用户添加的文件夹也清了，是这类操作最容易犯的错，
/// 而且从"图库空了"这个现象上完全看不出来。
#[test]
fn clear_index_drops_index_but_keeps_user_config() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("clear.sqlite")).unwrap();

    // 用户配置：一个关注根目录 + 一条"关闭此子文件夹"的排除规则
    let root_path = "/x/photos".to_string();
    let root_id = db.upsert_root(&root_path, 1, true).unwrap();
    db.add_exclusion(root_id, "secret").unwrap();

    // 索引数据：两个目录、三张照片、两条缩略图
    let key_a = "/x/photos/2020".to_string();
    let key_b = "/x/photos/2021".to_string();
    let fid_a = db.upsert_folder(&key_a, &key_a, None).unwrap();
    let fid_b = db.upsert_folder(&key_b, &key_b, None).unwrap();
    db.apply_scan(fid_a, &key_a, &[input("a.jpg"), input("b.jpg")], 0)
        .unwrap();
    db.apply_scan(fid_b, &key_b, &[input("c.jpg")], 0).unwrap();
    for r in db.photos(None, SortSpec::default(), 10).unwrap() {
        db.put_thumb(
            r.id,
            0,
            &ThumbBuf {
                w: 8,
                h: 8,
                format: 0,
                data: vec![0u8; 50],
            },
        )
        .unwrap();
    }
    assert_eq!(db.cache_stats().unwrap().photo_count, 3);

    // ---- 删
    let (photos, thumbs) = db.clear_index().unwrap();
    assert_eq!(photos, 3, "返回的照片数要如实");
    assert_eq!(thumbs, 3);

    // ---- 索引没了
    let s = db.cache_stats().unwrap();
    assert_eq!(s.photo_count, 0);
    assert_eq!(s.thumb_count, 0);
    assert!(db.photos(None, SortSpec::default(), 10).unwrap().is_empty());
    assert!(
        db.folder_id(&key_a).unwrap().is_none(),
        "目录行也要删，否则左栏会留一堆空壳"
    );

    // ---- 用户配置还在（这才是重点）
    let roots = db.roots(true).unwrap();
    assert_eq!(roots.len(), 1, "关注文件夹是用户配置，删索引不能带走它");
    assert_eq!(roots[0].path, root_path);
    assert_eq!(
        db.exclusions(root_id).unwrap().len(),
        1,
        "「关闭此文件夹」的规则必须继续生效，否则重建索引后它又冒出来了"
    );

    // ---- 再删一次是幂等的（不会因为"已经空了"报错）
    let (p2, t2) = db.clear_index().unwrap();
    assert_eq!((p2, t2), (0, 0));
}

/// 「重建失败索引」的取数口径：**只动点名的那批失败项**。
///
/// 三个陷阱各自对应一条断言：
/// 1. 范围 —— 没点名的失败项必须保持失败（否则界面上「失败 1 项」会莫名变成 0）；
/// 2. 不倒退 —— 已完成的缩略图不能被这条语句碰上（`04` §2.4 已完成不回退）；
/// 3. 幂等 —— 再点一次重建不该报错，也不该把已完成的重置掉。
#[test]
fn reset_failed_thumbs_by_ids_touches_only_named_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("rebuild.sqlite")).unwrap();

    let key = "/x/photos".to_string();
    let fid = db.upsert_folder(&key, &key, None).unwrap();
    db.apply_scan(
        fid,
        &key,
        &[input("a.jpg"), input("b.jpg"), input("c.jpg")],
        0,
    )
    .unwrap();
    let rows = db.photos(None, SortSpec::default(), 10).unwrap();
    assert_eq!(rows.len(), 3);
    let by_name = |n: &str| rows.iter().find(|r| r.name == n).unwrap().id;

    // a、b 失败；c 已经有缩略图
    db.set_thumb_state(by_name("a.jpg"), ThumbState::Failed)
        .unwrap();
    db.set_thumb_state(by_name("b.jpg"), ThumbState::Failed)
        .unwrap();
    db.put_thumb(
        by_name("c.jpg"),
        0,
        &ThumbBuf {
            w: 8,
            h: 8,
            format: 0,
            data: vec![0u8; 50],
        },
    )
    .unwrap();
    assert_eq!(
        (
            db.library_thumb_stats().unwrap().failed,
            db.library_thumb_stats().unwrap().ready
        ),
        (2, 1)
    );

    // ---- 只点名 a
    let reset = db
        .reset_failed_thumbs_by_ids(&[by_name("a.jpg"), by_name("c.jpg")])
        .unwrap();
    assert_eq!(reset, 1, "已完成的那张不算重置，只该重置 1 条");
    let s = db.library_thumb_stats().unwrap();
    assert_eq!((s.pending, s.failed, s.ready), (1, 1, 1));
    assert_eq!(
        s.failed, 1,
        "没点名的失败项必须保持失败——否则面板上的失败数会凭空消失"
    );

    // ---- 再点一次：幂等，已完成照样不动
    assert_eq!(
        db.reset_failed_thumbs_by_ids(&[by_name("c.jpg")]).unwrap(),
        0
    );
    assert_eq!(db.library_thumb_stats().unwrap().ready, 1);

    // ---- 空名单：不报错、不做事
    assert_eq!(db.reset_failed_thumbs_by_ids(&[]).unwrap(), 0);
}
