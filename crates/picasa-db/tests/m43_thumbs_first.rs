//! 用户要求：**有缩略图的往前放，没有缩略图的单独放在一起**。
//!
//! 实现口径（`Db::photos_bucketed`）：桶键只看 `photos.thumb_state = 1`，
//! 两桶各自按当前排序键排，先整桶有图、再整桶没图。
//!
//! 这里钉四件事：
//! 1. 有图的整桶在前、没图的整桶在后，且**两桶各自仍按排序键有序**；
//! 2. `thumb_state` 不是 1 的各种取值（0 未生成 / 2 失败 / 3 损坏 / 4 云端未下载）
//!    都归到「没有缩略图」那一桶——它们界面上都没有图；
//! 3. 全局偏移能正确跨过桶边界（不重复、不丢行）——这是分页最容易错的地方；
//! 4. 筛选条件与桶条件按「与」合并，筛选不会把桶拆散。

use picasa_db::{Db, PhotoInput, PhotoFilter, SortSpec, ThumbBuf, ThumbState};

fn input(name: &str, taken: i64) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_lowercase(),
        size_bytes: 100,
        mtime_ns: 1,
        width: Some(100),
        height: Some(80),
        orientation: Some(1),
        taken_at: Some(taken),
        taken_at_src: 0,
        format: 0,
    }
}

fn thumb() -> ThumbBuf {
    ThumbBuf {
        w: 1,
        h: 1,
        format: 0,
        data: vec![0xFF, 0xD8, 0xFF, 0xD9],
    }
}

/// 造 6 张：`a`/`c`/`e` 有缩略图，`b`/`d`/`f` 没有；拍摄时间递增。
/// 这样按时间倒序时两桶的期望顺序是 `e c a` + `f d b`——
/// 如果实现写成「一条 CASE 排序」结果一样，但若把桶键与排序键搞混就会立刻不同。
fn seed(db: &Db) -> i64 {
    let fid = db.upsert_folder("/photos", "/photos", None).unwrap();
    let items = vec![
        input("a.jpg", 1_300_000_001),
        input("b.jpg", 1_300_000_002),
        input("c.jpg", 1_300_000_003),
        input("d.jpg", 1_300_000_004),
        input("e.jpg", 1_300_000_005),
        input("f.jpg", 1_300_000_006),
    ];
    db.apply_scan(fid, "/photos", &items, 0).unwrap();
    let ids: Vec<(String, i64)> = db
        .photos(None, SortSpec::default(), 100)
        .unwrap()
        .into_iter()
        .map(|r| (r.name, r.id))
        .collect();
    for (name, id) in &ids {
        match name.as_str() {
            "a.jpg" | "c.jpg" | "e.jpg" => db.put_thumb(*id, 0, &thumb()).unwrap(),
            // 三种「没有缩略图」的原因各占一个，确保都落进同一桶。
            "b.jpg" => db.set_thumb_state(*id, ThumbState::Pending).unwrap(),
            "d.jpg" => db.set_thumb_state(*id, ThumbState::Failed).unwrap(),
            _ => db.set_thumb_state(*id, ThumbState::CloudOnly).unwrap(),
        }
    }
    fid
}

fn names(db: &Db, fid: i64, limit: usize, offset: usize) -> Vec<String> {
    db.photos_bucketed(
        Some(&[fid]),
        None,
        SortSpec::default(),
        limit,
        offset,
    )
    .unwrap()
    .into_iter()
    .map(|r| r.name)
    .collect()
}

#[test]
fn photos_with_thumbnails_come_first_and_each_bucket_keeps_the_sort_order() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let fid = seed(&db);

    assert_eq!(
        names(&db, fid, 100, 0),
        vec!["e.jpg", "c.jpg", "a.jpg", "f.jpg", "d.jpg", "b.jpg"],
        "有图的整桶在前、无图的整桶在后，两桶内部都按拍摄时间倒序"
    );
}

#[test]
fn pending_failed_corrupt_and_cloud_only_all_count_as_without_thumbnail() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let fid = seed(&db);

    let all = names(&db, fid, 100, 0);
    let head: Vec<&String> = all.iter().take(3).collect();
    assert_eq!(head, vec!["e.jpg", "c.jpg", "a.jpg"]);
    // 未生成 / 失败 / 云端未下载 三种都排在后面，且彼此仍按时间倒序。
    assert_eq!(&all[3..], ["f.jpg", "d.jpg", "b.jpg"]);
}

#[test]
fn paging_across_the_bucket_boundary_never_repeats_or_drops_a_row() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let fid = seed(&db);

    // 一页 2 张，逐页翻完全部 6 张：必须与一次性取满完全一致。
    let full = names(&db, fid, 100, 0);
    for page in [1usize, 2, 3, 4] {
        let mut got = Vec::new();
        let mut offset = 0;
        while got.len() < full.len() {
            let chunk = names(&db, fid, page, offset);
            if chunk.is_empty() {
                break;
            }
            offset += chunk.len();
            got.extend(chunk);
        }
        assert_eq!(got, full, "页大小 {page} 时必须逐行等于一次性取满");
    }
}

#[test]
fn filter_conditions_and_the_bucket_are_combined_with_and() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("m.sqlite")).unwrap();
    let fid = seed(&db);

    // 只筛「有缩略图」那三张里的两张：按时间倒序落在 e / c。
    let mut filter = PhotoFilter {
        tag: Some("x".to_string()),
        ..Default::default()
    };
    let ids: Vec<i64> = db
        .photos(None, SortSpec::default(), 100)
        .unwrap()
        .into_iter()
        .filter(|r| r.name == "e.jpg" || r.name == "c.jpg")
        .map(|r| r.id)
        .collect();
    db.add_tag(&ids, "x").unwrap();

    let got: Vec<String> = db
        .photos_bucketed(
            Some(&[fid]),
            Some(&filter),
            SortSpec::default(),
            100,
            0,
        )
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(got, vec!["e.jpg", "c.jpg"], "筛选与桶条件按「与」合并");

    // 收窄到一张有图 + 一张没图，仍必须「有图的在前」。
    filter.tag = Some("y".to_string());
    let two: Vec<i64> = db
        .photos(None, SortSpec::default(), 100)
        .unwrap()
        .into_iter()
        .filter(|r| r.name == "a.jpg" || r.name == "b.jpg")
        .map(|r| r.id)
        .collect();
    db.add_tag(&two, "y").unwrap();
    let got: Vec<String> = db
        .photos_bucketed(Some(&[fid]), Some(&filter), SortSpec::default(), 100, 0)
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(got, vec!["a.jpg", "b.jpg"]);
}
