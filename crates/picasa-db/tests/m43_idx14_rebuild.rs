//! M4.3 IDX-14：全量重建（force）路径的 DB 契约。
//!
//! 核心：force 绕过 L1 缓存命中，让每张已存在照片都进 `touched` 重新生成缩略图；
//! 但**不删旧缩略图 BLOB、不把 `thumb_state` 归零**——旧图保留到被 `put_thumb`
//! 原子替换为止，UI 栅格才不会在全量重建过程中闪成空白。普通扫描必须保持原行为
//! （文件变化就删 BLOB、`thumb_state` 归零）。

use picasa_db::{Db, PhotoInput, SortSpec, TaskKind, ThumbBuf};

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
        format: 0,
    }
}

fn thumb(data: Vec<u8>) -> ThumbBuf {
    ThumbBuf {
        w: 8,
        h: 8,
        format: 0,
        data,
    }
}

#[test]
fn force_scan_bypasses_cache_and_preserves_old_blob() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();

    let key = "/x/photos".to_string();
    let fid = db.upsert_folder(&key, &key, None).unwrap();
    let items = vec![input("a.jpg")];
    let first = db.apply_scan(fid, &key, &items, 0).unwrap();
    assert_eq!(first.added, 1);
    let id = first.touched[0];

    // 旧缩略图：写一条，thumb_state 变成 1(Ok)
    db.put_thumb(id, 0, &thumb(vec![1u8; 100])).unwrap();
    assert!(db.get_thumb(id, 0).unwrap().is_some());

    // ---- force：用完全相同的 items（size/mtime/ver 都相同）
    let out = db.apply_scan_force(fid, &key, &items, 0).unwrap();
    assert!(
        out.touched.contains(&id),
        "force 必须让已存在照片进 touched 重新生成缩略图"
    );
    assert_eq!(out.unchanged, 0, "L1 缓存命中必须被 force 绕过");

    // BLOB 没被删
    assert!(
        db.get_thumb(id, 0).unwrap().is_some(),
        "force 不能删旧缩略图 BLOB（UI 栅格要继续显示旧图）"
    );
    // thumb_state 没被归零
    let rows = db.photos(None, SortSpec::default(), 10).unwrap();
    let row = rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!(row.thumb_state, 1, "force 必须保留旧 thumb_state=1");

    // ---- put_thumb 用不同字节原子替换：仍只有一条 thumb 行
    db.put_thumb(id, 0, &thumb(vec![9u8; 50])).unwrap();
    let replaced = db.get_thumb(id, 0).unwrap().unwrap();
    assert_eq!(replaced.data, vec![9u8; 50]);
    assert_eq!(
        db.cache_stats().unwrap().thumb_count,
        1,
        "UPSERT 替换，不能产生重复 thumb 行"
    );
}

#[test]
fn normal_scan_still_drops_blob_on_change() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();

    let key = "/x/photos".to_string();
    let fid = db.upsert_folder(&key, &key, None).unwrap();
    let items = vec![input("a.jpg")];
    let first = db.apply_scan(fid, &key, &items, 0).unwrap();
    let id = first.touched[0];
    db.put_thumb(id, 0, &thumb(vec![1u8; 100])).unwrap();
    assert!(db.get_thumb(id, 0).unwrap().is_some());

    // 普通扫描：mtime 变了 → 必须删旧 BLOB 并归零 thumb_state
    let mut changed = input("a.jpg");
    changed.mtime_ns = 2;
    let out = db.apply_scan(fid, &key, &[changed], 0).unwrap();
    assert!(out.touched.contains(&id));
    assert!(
        db.get_thumb(id, 0).unwrap().is_none(),
        "普通扫描变化必须删旧 BLOB（守卫既有行为）"
    );
    let rows = db.photos(None, SortSpec::default(), 10).unwrap();
    let row = rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!(row.thumb_state, 0, "普通扫描变化必须把 thumb_state 归零");
}

#[test]
fn photos_by_ids_for_force_ignores_thumb_state() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();

    let key = "/x/photos".to_string();
    let fid = db.upsert_folder(&key, &key, None).unwrap();
    let items = vec![input("a.jpg")];
    let first = db.apply_scan(fid, &key, &items, 0).unwrap();
    let id = first.touched[0];
    db.put_thumb(id, 0, &thumb(vec![1u8; 100])).unwrap();

    // thumb_state=1：pending_thumbs_by_ids 带 `WHERE thumb_state = 0` → 空
    assert!(
        db.pending_thumbs_by_ids(&[id], 10).unwrap().is_empty(),
        "有旧图(状态 Ok)时不应出现在待重建队列"
    );
    // force 专用查询不过滤 thumb_state → 返回该行
    let rows = db.photos_by_ids_for_force(&[id], 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].thumb_state, 1);
}

#[test]
fn mark_task_reindex_sets_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();

    let id = db
        .create_task(None, "/x", TaskKind::FirstScan)
        .unwrap();
    db.mark_task_reindex(id).unwrap();

    let intent = db.task_intent(id).unwrap().unwrap();
    assert_eq!(
        intent.kind,
        TaskKind::Reindex.code(),
        "force 意图靠 kind==Reindex 持久化"
    );
}
