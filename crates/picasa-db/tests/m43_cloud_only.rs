//! M4.3 补遗：云端占位文件（OneDrive / iCloud 的 dataless 文件）不能算「失败」。
//!
//! 背景（实机复现）：`~/Library/CloudStorage/OneDrive-个人/Pictures/...` 下的
//! 4 个目录共 214 张图全是 `SF_DATALESS` 占位文件，本机一个字节都没有；读它们
//! 直接 `ETIMEDOUT`。旧实现把它归成 `Io(_)` → `Unreachable` → `thumb_state = 2`
//! 「失败」，用户看到的是 214 个「失败」灰格，而且怎么重试都不会好。
//!
//! 本文件只覆盖 DB 层的状态迁移语义（文件系统侧由 `picasa-imaging::cloud` 的
//! 单测与 `picasa-core` 的扫描用例覆盖）：
//! - `mark_photos_cloud_only` 把指定照片标成 `CloudOnly`，且**不降级已经出过图的**；
//! - `clear_cloud_only` 把它们放回「待生成」，但不碰失败项；
//! - `ThumbStats.cloud_only` 与 `failed` 分开计数。

use picasa_db::{Db, PhotoInput, SortSpec, ThumbState};

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

fn seed(db: &Db, names: &[&str]) -> i64 {
    let fid = db.upsert_folder("/photos", "/photos", None).unwrap();
    let items: Vec<PhotoInput> = names.iter().map(|n| input(n)).collect();
    db.apply_scan(fid, "/photos", &items, 0).unwrap();
    fid
}

fn id_of(db: &Db, name: &str) -> i64 {
    db.photos(None, SortSpec::default(), 100)
        .unwrap()
        .into_iter()
        .find(|r| r.name == name)
        .expect("照片必须存在")
        .id
}

fn state_of(db: &Db, name: &str) -> i64 {
    db.photos(None, SortSpec::default(), 100)
        .unwrap()
        .into_iter()
        .find(|r| r.name == name)
        .unwrap()
        .thumb_state
}

#[test]
fn cloud_only_is_its_own_state_and_counts_separately() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let fid = seed(&db, &["a.jpg", "b.jpg", "c.jpg"]);

    // 三张全部待生成
    let before = db.folder_thumb_stats(fid).unwrap();
    assert_eq!((before.pending, before.failed, before.cloud_only), (3, 0, 0));

    // 两张标成云端占位
    let n = db
        .mark_photos_cloud_only(fid, &["a.jpg".into(), "b.jpg".into()])
        .unwrap();
    assert_eq!(n, 2);

    assert_eq!(state_of(&db, "a.jpg"), ThumbState::CloudOnly as i64);
    assert_eq!(state_of(&db, "b.jpg"), ThumbState::CloudOnly as i64);
    assert_eq!(state_of(&db, "c.jpg"), ThumbState::Pending as i64);

    let s = db.folder_thumb_stats(fid).unwrap();
    assert_eq!(
        (s.pending, s.failed, s.cloud_only),
        (1, 0, 2),
        "云端占位必须与失败分开计数，否则用户以为图坏了"
    );
    // 全库口径同样分开
    let lib = db.library_thumb_stats().unwrap();
    assert_eq!((lib.failed, lib.cloud_only), (0, 2));
}

#[test]
fn already_rendered_thumbs_are_never_downgraded_to_cloud_only() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let fid = seed(&db, &["ready.jpg", "cold.jpg"]);
    let ready = id_of(&db, "ready.jpg");

    // 让 ready.jpg 真有一个 BLOB 并进入 Ok
    db.put_thumb(
        ready,
        0,
        &picasa_db::ThumbBuf {
            w: 2,
            h: 2,
            format: 0,
            data: vec![0xFF, 0xD8, 0xFF, 0xD9],
        },
    )
    .unwrap();
    assert_eq!(state_of(&db, "ready.jpg"), ThumbState::Ok as i64);

    db.mark_photos_cloud_only(fid, &["ready.jpg".into(), "cold.jpg".into()])
        .unwrap();

    assert_eq!(
        state_of(&db, "ready.jpg"),
        ThumbState::Ok as i64,
        "已经出过图的不能被标成云端未下载——那会把一张能看的图变成灰格"
    );
    assert_eq!(state_of(&db, "cold.jpg"), ThumbState::CloudOnly as i64);
}

#[test]
fn clearing_cloud_only_puts_them_back_in_the_queue_without_touching_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let fid = seed(&db, &["a.jpg", "b.jpg", "bad.jpg"]);
    let bad = id_of(&db, "bad.jpg");

    db.mark_photos_cloud_only(fid, &["a.jpg".into(), "b.jpg".into()])
        .unwrap();
    db.set_thumb_state(bad, ThumbState::Failed).unwrap();

    let n = db.clear_cloud_only(fid).unwrap();
    assert_eq!(n, 2, "只把云端占位的两行放回待生成");

    assert_eq!(state_of(&db, "a.jpg"), ThumbState::Pending as i64);
    assert_eq!(state_of(&db, "b.jpg"), ThumbState::Pending as i64);
    assert_eq!(
        state_of(&db, "bad.jpg"),
        ThumbState::Failed as i64,
        "真失败不能被顺手清掉"
    );

    // 复位后再标一次：幂等
    db.mark_photos_cloud_only(fid, &["a.jpg".into()]).unwrap();
    assert_eq!(state_of(&db, "a.jpg"), ThumbState::CloudOnly as i64);
}

#[test]
fn cloud_only_rows_are_not_in_failed_lists_or_retry_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let fid = seed(&db, &["cloud.jpg", "bad.jpg"]);

    db.mark_photos_cloud_only(fid, &["cloud.jpg".into()])
        .unwrap();
    db.set_thumb_state(id_of(&db, "bad.jpg"), ThumbState::Failed)
        .unwrap();

    let failed = db.failed_photos(None).unwrap();
    assert_eq!(failed.len(), 1, "失败明细里只该有真失败的那张");
    assert_eq!(failed[0].name, "bad.jpg");

    // 「重试失败项」只清 (2,3)，不该碰云端占位
    let reset = db.reset_failed_thumbs(fid).unwrap();
    assert_eq!(reset, 1);
    assert_eq!(state_of(&db, "cloud.jpg"), ThumbState::CloudOnly as i64);
    assert_eq!(state_of(&db, "bad.jpg"), ThumbState::Pending as i64);
}

#[test]
fn a_cloud_only_row_can_still_carry_a_thumbnail_blob() {
    // 用户反馈（v0.7-dev22 的缺陷）：从别的电脑导出的索引包会把缩略图一起带
    // 过来（`content_flags.thumbnails`），那些照片的原图在本机根本不存在，行上
    // 迟早会被标成 `CloudOnly`。**状态与 BLOB 是两件事**：只要库里存着 BLOB，
    // 界面就必须把它画出来；`CloudOnly` 只说明「原图字节不在本机」。
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let fid = seed(&db, &["imported.jpg"]);
    let id = id_of(&db, "imported.jpg");

    db.put_thumb(
        id,
        0,
        &picasa_db::ThumbBuf {
            w: 2,
            h: 2,
            format: 0,
            data: vec![0xFF, 0xD8, 0xFF, 0xD9],
        },
    )
    .unwrap();
    // 模拟「扫描发现原图是云端占位」：状态转 CloudOnly，但 BLOB 一条不少。
    db.set_thumb_state(id, ThumbState::CloudOnly).unwrap();

    assert_eq!(state_of(&db, "imported.jpg"), ThumbState::CloudOnly as i64);
    let blob = db.get_thumb(id, 0).unwrap();
    assert!(
        blob.is_some(),
        "云端占位行上的缩略图 BLOB 必须保留——导入的索引包全靠它显示"
    );
    assert_eq!(blob.unwrap().data, vec![0xFF, 0xD8, 0xFF, 0xD9]);

    // 而且这种行不该出现在「失败」里，也不该被「重试失败项」搅动。
    assert!(db.failed_photos(None).unwrap().is_empty());
    assert_eq!(db.reset_failed_thumbs(fid).unwrap(), 0);
}
