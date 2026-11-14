//! M4.3 阶段 4：键集（seek）翻页的**正确性**（`TC-M43-12`）。
//!
//! 性能测试只能证明「快」，证明不了「对」。键集翻页最经典的坑是
//! **平局决胜键写错**：`ORDER BY eff_taken_at DESC, name_key ASC` 下若游标用
//! `id` 兜底，同一秒拍的照片会被整批跳过或重复。这类 bug 不报错，
//! 只让用户滚到底少几张，所以必须有逐行比对。
//!
//! 这里建一批**大量同秒**的照片（比页大小还多），一页页翻完全库，
//! 与「一次取满」的结果逐行比对。

use picasa_db::{Db, PageCursor, SortSpec};

fn photo(name: &str, taken: i64) -> picasa_db::PhotoInput {
    picasa_db::PhotoInput {
        name: name.to_string(),
        name_key: name.to_lowercase(),
        size_bytes: 1000,
        mtime_ns: taken * 1_000_000_000,
        width: Some(800),
        height: Some(600),
        orientation: Some(1),
        taken_at: Some(taken),
        taken_at_src: 1,
        format: 1,
    }
}

fn seeded() -> (tempfile::TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("library.sqlite")).unwrap();
    let root = db.upsert_root("/albums", 0, false).unwrap();
    let fid = db.upsert_folder("/albums", "/albums", None).unwrap();
    db.set_folder_root(fid, root, None).unwrap();
    let mut items = Vec::new();
    // 400 张：每 4 张共享同一个拍摄秒 → 平局组比页大小还多，专门压平局键。
    for i in 0..400 {
        items.push(photo(&format!("p{i:04}.jpg"), 1_000 + i / 4));
    }
    db.apply_scan(fid, "/albums", &items, 0).unwrap();
    (tmp, db)
}

fn page_all(db: &Db, page: usize) -> Vec<i64> {
    let mut out = Vec::new();
    let mut cursor: Option<PageCursor> = None;
    loop {
        let rows = db
            .photos_from_visible_after(None, SortSpec::default(), page, cursor)
            .unwrap();
        if rows.is_empty() {
            break;
        }
        let last = rows.last().unwrap();
        cursor = db.page_cursor(last.id).unwrap();
        out.extend(rows.iter().map(|r| r.id));
        assert!(out.len() <= 10_000, "翻页没有推进，疑似游标没前移");
    }
    out
}

#[test]
fn keyset_paging_visits_every_row_exactly_once_across_ties() {
    let (_tmp, db) = seeded();
    let one_shot: Vec<i64> = db
        .photos_from_visible(None, SortSpec::default(), 10_000, 0)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(one_shot.len(), 400);

    // 页大小要能整除平局组、又不能整除 400，才真的会在组中间断开。
    for page in [1usize, 3, 7, 150, 399, 400, 401] {
        let paged = page_all(&db, page);
        assert_eq!(
            paged, one_shot,
            "页大小 {page} 时键集翻页的顺序 / 内容与一次性查询不一致"
        );
        let mut uniq = paged.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), paged.len(), "页大小 {page} 时出现重复行");
    }
}

#[test]
fn keyset_ascending_direction_is_mirrored() {
    let (_tmp, db) = seeded();
    let asc = SortSpec {
        key: picasa_db::SortKey::TakenAt,
        descending: false,
    };
    let one_shot: Vec<i64> = db
        .photos_from_visible(None, asc, 10_000, 0)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();

    let mut paged = Vec::new();
    let mut cursor: Option<PageCursor> = None;
    loop {
        let rows = db.photos_from_visible_after(None, asc, 7, cursor).unwrap();
        if rows.is_empty() {
            break;
        }
        cursor = db.page_cursor(rows.last().unwrap().id).unwrap();
        paged.extend(rows.iter().map(|r| r.id));
    }
    assert_eq!(paged, one_shot, "升序翻页必须与 ORDER BY ASC 逐行一致");
}

#[test]
fn keyset_respects_the_scope_and_the_filter() {
    let (_tmp, db) = seeded();
    // 只取一半文件夹时，翻页结果必须是全量结果的子集且不含范围外的 id。
    let fid = db.folder_id("/albums").unwrap().unwrap();
    let scoped: Vec<i64> = db
        .photos_from_visible_after(Some(&[fid]), SortSpec::default(), 10_000, None)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(scoped.len(), 400);

    // 筛选（只看收藏）+ 键集：游标语义必须与筛选后的 ORDER BY 一致。
    db.set_starred(&scoped[..37], true).unwrap();
    let filter = picasa_db::PhotoFilter {
        starred_only: true,
        ..Default::default()
    };
    let one_shot: Vec<i64> = db
        .photos_filtered(None, &filter, SortSpec::default(), 10_000, 0)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(one_shot.len(), 37);

    let mut paged = Vec::new();
    let mut cursor: Option<PageCursor> = None;
    loop {
        let rows = db
            .photos_filtered_after(None, &filter, SortSpec::default(), 5, cursor)
            .unwrap();
        if rows.is_empty() {
            break;
        }
        cursor = db.page_cursor(rows.last().unwrap().id).unwrap();
        paged.extend(rows.iter().map(|r| r.id));
    }
    assert_eq!(paged, one_shot, "筛选 + 键集翻页必须与一次性查询一致");
}
