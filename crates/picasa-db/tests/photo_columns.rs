//! 「所有返回 `PhotoRow` 的查询必须与 `map_photo` 的列数一致」——一条回归测试。
//!
//! ## 为什么要专门测这个
//!
//! `map_photo` 按**列下标**取值（`r.get(0)`、`r.get(1)`…）。SQL 的 `SELECT` 列表
//! 在代码里**重复写了 8 遍**（photos / search / recently_changed / photos_by_ids /
//! photos_pending_dims / failed_photos / subtree_pending_thumbs / search 的变体）。
//! 给 `PhotoRow` 加一个字段，只要漏改其中一处，那个方法就会在**运行期**抛
//! `InvalidColumnIndex`——而不是编译期报错。
//!
//! M2.1 加 `dims_status` 时漏过一次，加 `eff_taken_at` 时又漏了 `dims.rs` 那两处。
//! 两次都是靠别的测试「顺手」发现（`m21_dims` 的三个用例超时），
//! 换一个没被别的测试覆盖的查询，就会一直错到用户那里。
//!
//! 所以这里把**每个**这样的查询都真跑一遍：库里有 3 张照片、1 个待回填、1 个失败，
//! 保证每个查询都至少命中一行（空结果集不会暴露列数不匹配）。

use picasa_db::{Db, SortSpec, TaskKind};

/// 建一个「什么查询都能命中至少一行」的小库。
fn fixture() -> (tempfile::TempDir, Db, i64) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("cols.sqlite")).unwrap();

    let dir = tmp.path().join("photos");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.to_string_lossy().to_string();
    let fid = db.upsert_folder(&path, &path.to_lowercase(), None).unwrap();

    let items: Vec<picasa_db::PhotoInput> = ["a.jpg", "b.jpg", "c.jpg"]
        .iter()
        .enumerate()
        .map(|(i, name)| picasa_db::PhotoInput {
            name: name.to_string(),
            name_key: name.to_lowercase(),
            size_bytes: 1000 + i as i64,
            mtime_ns: 1_600_000_000_000_000_000 + i as i64,
            // a.jpg 故意不给宽高：apply_scan 会把它标成 dims_status=0（待回填）
            width: if i == 0 { None } else { Some(4000) },
            height: if i == 0 { None } else { Some(3000) },
            orientation: Some(1),
            taken_at: Some(1_300_000_000 + i as i64),
            taken_at_src: 0,
        })
        .collect();
    db.apply_scan(fid, &path, &items, 0).unwrap();

    // 三张各占一个状态：`a.jpg` 待回填（apply_scan 时没给宽高 → dims_status=0）、
    // `b.jpg` 已完成、`c.jpg` 终态失败（同时 thumb_state 也置失败，喂给 failed_photos）。
    let by_name = |n: &str| -> i64 {
        db.photos(Some(&[fid]), SortSpec::default(), 10)
            .unwrap()
            .into_iter()
            .find(|p| p.name == n)
            .unwrap()
            .id
    };
    let a = by_name("a.jpg");
    let c = by_name("c.jpg");
    db.set_photo_dims(c, None, None, None, false).unwrap(); // dims_status = 2
    db.set_thumb_state(c, picasa_db::ThumbState::Failed)
        .unwrap();
    let _ = a; // a 由 apply_scan 自然落到 0，不需要额外设置

    (tmp, db, fid)
}

#[test]
fn every_photo_row_query_matches_map_photo() {
    let (_tmp, db, fid) = fixture();

    // ① 全库 / 单目录，两条主路径
    let all = db.photos(None, SortSpec::default(), 100).unwrap();
    assert_eq!(all.len(), 3, "photos(None) 应能看到全部");
    let one = db.photos(Some(&[fid]), SortSpec::default(), 100).unwrap();
    assert_eq!(one.len(), 3);

    // ② 分页（C1 新增）：offset 用得对，列也必须对
    let page = db
        .photos_from(Some(&[fid]), SortSpec::default(), 2, 0)
        .unwrap();
    assert_eq!(page.len(), 2);
    let tail = db
        .photos_from(Some(&[fid]), SortSpec::default(), 2, 2)
        .unwrap();
    assert_eq!(tail.len(), 1, "共 3 行，跳过 2 行应剩 1 行");
    assert_ne!(page[0].id, tail[0].id, "分页不能返回同一行");

    // ③ 三种排序键（每种都走不同的 ORDER BY 分支）
    for key in [
        picasa_db::SortKey::TakenAt,
        picasa_db::SortKey::Name,
        picasa_db::SortKey::ModifiedAt,
        picasa_db::SortKey::Size,
        picasa_db::SortKey::Dimensions,
    ] {
        let rows = db
            .photos(
                Some(&[fid]),
                SortSpec {
                    key,
                    descending: true,
                },
                10,
            )
            .unwrap();
        assert_eq!(rows.len(), 3, "{key:?} 排序应返回 3 行");
    }

    // ④ 最近更改
    assert_eq!(db.recently_changed(10).unwrap().len(), 3);

    // ⑤ 搜索（文件名命中）
    let hits = db.search("a.jpg", None, Some(&[fid]), 10).unwrap();
    assert!(!hits.is_empty(), "搜索应有命中，否则测不到列数");

    // ⑥ 按 id 批量取（回填的可见优先路径）
    let ids: Vec<i64> = all.iter().map(|p| p.id).collect();
    let by_ids = db.photos_by_ids(&ids).unwrap();
    assert_eq!(by_ids.len(), 3);

    // ⑦ 待回填（部分索引）与失败列表：两个都必须有命中
    let pending = db.photos_pending_dims(Some(&[fid]), 100).unwrap();
    assert!(!pending.is_empty(), "应有待回填行");
    let failed = db.failed_photos(Some(fid)).unwrap();
    assert!(!failed.is_empty(), "应有失败行");

    // ⑧ 待生成缩略图（子树版）
    let _ = db.subtree_pending_thumbs(fid, 100).unwrap();

    // ⑨ 任务历史（不是 PhotoRow，但同样是手写列清单，一并 smoke）
    let _ = db.recent_tasks(5).unwrap();
    let _ = db.task_failures(1).unwrap();
    let _ = TaskKind::DimsBackfill.code();
}

/// `eff_taken_at` 必须与「COALESCE(taken_at, mtime/1e9)」逐行相等。
///
/// 它是 v3 生成列，SQL 侧与 Rust 侧的 `effective_taken_at()` 都用它——
/// 一旦两者口径分叉，就会出现「SQL 按 A 排序、界面按 B 分组」，
/// 表现为「同一天的照片被拆成两个分组头」，且只在没有 EXIF 的照片上出现。
#[test]
fn eff_taken_at_matches_fallback_rule() {
    let (_tmp, db, fid) = fixture();

    // 再造一张**没有** taken_at 的：它必须回退到 mtime
    let path = db
        .all_folders()
        .unwrap()
        .into_iter()
        .find(|f| f.id == fid)
        .unwrap()
        .path;
    db.apply_scan(
        fid,
        &path,
        &[picasa_db::PhotoInput {
            name: "no_exif.jpg".into(),
            name_key: "no_exif.jpg".into(),
            size_bytes: 9,
            mtime_ns: 1_500_000_000_123_456_789,
            width: None,
            height: None,
            orientation: None,
            taken_at: None,
            taken_at_src: 0,
        }],
        0,
    )
    .unwrap();

    for p in db.photos(Some(&[fid]), SortSpec::default(), 100).unwrap() {
        let expected = p.taken_at.unwrap_or(p.mtime_ns / 1_000_000_000);
        assert_eq!(
            p.eff_taken_at, expected,
            "{} 的 eff_taken_at 与回退规则不一致",
            p.name
        );
        assert_eq!(
            p.effective_taken_at(),
            expected,
            "Rust 侧方法与 SQL 列必须同口径"
        );
    }

    let no_exif = db
        .photos(Some(&[fid]), SortSpec::default(), 100)
        .unwrap()
        .into_iter()
        .find(|p| p.name == "no_exif.jpg")
        .unwrap();
    assert_eq!(
        no_exif.eff_taken_at, 1_500_000_000,
        "mtime_ns 是纳秒，取整秒时不能四舍五入（123ms 那部分要丢掉）"
    );
}

/// 迁移到 v3 后，**排序必须走索引**，不能退回「全表排序」。
///
/// 这条是 C1 的核心：`ORDER BY COALESCE(...)` 曾让 5 万张的 `LIMIT 2000` 也要 199 ms。
/// 这里用 `EXPLAIN QUERY PLAN` 断言执行计划里出现 `idx_photos_eff_taken`、且**没有**
/// `USE TEMP B-TREE FOR ORDER BY`。抓不到索引就说明有人把 `order_by()` 改回表达式了。
#[test]
fn sort_by_taken_at_uses_index_not_temp_btree() {
    let (_tmp, db, _fid) = fixture();
    let plan = db.explain_photos_plan(SortSpec::default()).unwrap();
    let joined = plan.join("\n");
    assert!(
        joined.contains("idx_photos_eff_taken"),
        "按拍摄时间排序没有命中 idx_photos_eff_taken，计划是：\n{joined}"
    );
    assert!(
        !joined.contains("TEMP B-TREE FOR ORDER BY"),
        "排序还在建临时 B-tree（说明用的是表达式而不是生成列）：\n{joined}"
    );
}
