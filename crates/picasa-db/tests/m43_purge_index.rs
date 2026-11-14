//! m10218「彻底删除索引」的库层契约测试。
//!
//! 用户原话：「索引管理中增加索引删除功能，点击二次确认后，彻底删除索引，
//! 同时删除界面上和该索引相关的文件夹和预览图片」。
//!
//! 与既有两条路的分工必须钉死，否则以后很容易又揉成一团：
//! - `clear_index_for_root`：清派生数据，**保留 roots 行**并落「索引已移除」标记
//!   （IDX-10 的「可一键恢复」，`m43_remove_index_semantics.rs` 钉住这条）；
//! - `purge_index_for_root`：上一步 **加上** `delete_root_row`，根行、排除项、
//!   「已移除」设置键、历史任务全部消失 —— 这才是用户说的「彻底删除」。
//!
//! 三条不可退让的性质：
//! 1. 彻底删除后 `roots` 里查不到这一行（否则界面上那行永远还在）；
//! 2. 照片 / 文件夹 / 缩略图 / 该根的历史任务全部清掉（否则是假删除）；
//! 3. **用户标记按身份骨架保留**（R-M43-02）：删完重新添加同一目录并扫描，
//!    收藏 / 标签 / 说明 / 相册必须按身份挂回新 id，一张都不许丢。

use picasa_db::{Db, PhotoInput, SortSpec, ThumbBuf};

fn input(name: &str, taken: i64) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_lowercase(),
        size_bytes: 10,
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
        w: 2,
        h: 2,
        format: 0,
        data: vec![0xFF, 0xD8, 0xFF, 0xD9],
    }
}

/// 搭一个根：`root` 目录下 2 张，`root/sub` 下 1 张；两张写缩略图，
/// 第一张带星标 + 标签 + 说明 + 相册。返回 (root_id, 第一张 photo_id)。
fn seed(db: &Db, root: &str, album_name: &str) -> (i64, i64) {
    let root_id = db.upsert_root(root, 1, false).unwrap();
    let sub = format!("{root}/sub");

    let fid = db.upsert_folder(root, root, None).unwrap();
    db.set_folder_root(fid, root_id, None).unwrap();
    db.apply_scan(
        fid,
        root,
        &[input("a.jpg", 1_300_000_000), input("b.jpg", 1_300_000_100)],
        0,
    )
    .unwrap();

    let sid = db.upsert_folder(&sub, &sub, None).unwrap();
    db.set_folder_root(sid, root_id, Some(fid)).unwrap();
    db.apply_scan(sid, &sub, &[input("c.jpg", 1_300_000_200)], 0)
        .unwrap();

    let rows = db.photos(None, SortSpec::default(), 10).unwrap();
    let a = rows.iter().find(|r| r.name == "a.jpg").unwrap().id;
    for r in &rows {
        db.put_thumb(r.id, 0, &thumb()).unwrap();
    }

    db.set_starred(&[a], true).unwrap();
    db.add_tag(&[a], "旅行").unwrap();
    db.set_caption(a, "海边").unwrap();
    let album = db.create_album(album_name, 1).unwrap();
    db.add_to_album(album, &[a], 1).unwrap();

    // 一条该根的历史任务 + 一条排除项，彻底删除后都必须消失。
    let task = db
        .begin_index_task(
            Some(root_id),
            root,
            picasa_db::TaskKind::FirstScan,
            picasa_db::TaskScopeKind::Root,
        )
        .unwrap();
    db.finish_task(
        task,
        picasa_db::TaskStatus::Completed,
        picasa_db::TaskOutcome::Success,
        3,
        0,
        None,
    )
    .unwrap();
    db.add_exclusion(root_id, "skipped").unwrap();

    (root_id, a)
}

/// ① 彻底删除把该根从索引管理里整行抹掉：roots / folders / photos / thumbs /
///    exclusions / 设置键 / 历史任务，一条不留；另一个根完全不受影响。
#[test]
fn purging_a_root_removes_the_row_and_all_its_index_data() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("p.sqlite")).unwrap();

    let (root_id, _a) = seed(&db, "/media/A", "精选 A");
    let (keep_id, _b) = seed(&db, "/media/B", "精选 B");
    assert_eq!(db.cache_stats().unwrap().photo_count, 6, "前置：两个根各 3 张");

    let (photos, thumbs) = db.purge_index_for_root(root_id).unwrap();
    assert_eq!(photos, 3, "返回的计数应当是这一棵子树的照片数");
    assert_eq!(thumbs, 3, "缩略图随 photos 的 CASCADE 一起走");

    // 1) 根行没了 —— 这就是「索引管理里那一行也消失」的数据基础。
    assert!(
        db.root(root_id).unwrap().is_none(),
        "彻底删除必须连 roots 行一起删，否则界面上那行永远还在"
    );
    assert!(
        !db.index_removed_root_ids()
            .unwrap()
            .contains(&root_id),
        "连根行都没了，「索引已移除」设置键也要一并清掉"
    );
    assert!(db.exclusions(root_id).unwrap().is_empty(), "排除项一并删");

    // 2) 索引数据 / 缩略图 / 历史任务清空。
    let left: Vec<String> = db
        .all_folders()
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert!(
        left.iter().all(|p| !p.starts_with("/media/A")),
        "该根下的 folders 必须全删，实际剩：{left:?}"
    );
    assert_eq!(db.photo_count_for_root(root_id).unwrap(), 0);
    assert_eq!(db.cache_stats().unwrap().thumb_count, 3, "只剩 B 的 3 张");
    assert!(
        db.recent_tasks(50)
            .unwrap()
            .iter()
            .all(|t| t.root_id != Some(root_id)),
        "该根的历史任务行随 roots 的 ON DELETE CASCADE 一起消失"
    );

    // 3) 另一个根一点没动。
    assert!(db.root(keep_id).unwrap().is_some(), "别的根不能被顺手删掉");
    assert_eq!(
        db.photo_count_for_root(keep_id).unwrap(),
        3,
        "B 的 3 张照片必须还在"
    );
}

/// ② R-M43-02：彻底删除**不丢用户标记**。删完重新添加同一目录并重扫，
///    收藏 / 标签 / 说明 / 相册必须按身份挂回新的 photo_id。
///
/// 这条必须在「同一批文件」上验证，而不是同一个 photo_id —— 重扫后 id 会变，
/// 按旧 id 恢复与按身份恢复在表面上完全一样，只有改过 id 才能区分。
#[test]
fn purging_keeps_user_markers_and_reattaches_them_to_new_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("p2.sqlite")).unwrap();

    let (root_id, old_id) = seed(&db, "/media/A", "精选");
    // 占住 rowid：清空后 SQLite 会复用 1，不区分就分不清「按身份」与「按旧 id」。
    let keep_fid = db.upsert_folder("/media/keep", "/media/keep", None).unwrap();
    db.apply_scan(keep_fid, "/media/keep", &[input("dummy.jpg", 1)], 0)
        .unwrap();

    db.purge_index_for_root(root_id).unwrap();
    assert!(db.root(root_id).unwrap().is_none());

    // 重新添加同一目录并重扫（模拟用户「以后再加回来」）。
    let new_root = db.upsert_root("/media/A", 2, false).unwrap();
    let fid = db.upsert_folder("/media/A", "/media/A", None).unwrap();
    db.set_folder_root(fid, new_root, None).unwrap();
    db.apply_scan(
        fid,
        "/media/A",
        &[input("a.jpg", 1_300_000_000), input("b.jpg", 1_300_000_100)],
        0,
    )
    .unwrap();

    let rows = db.photos(None, SortSpec::default(), 20).unwrap();
    let a2 = rows.iter().find(|r| r.name == "a.jpg").unwrap().id;
    assert_ne!(a2, old_id, "重建后 photo_id 应当变化，标记必须按身份而非旧 id 恢复");
    assert!(
        db.starred_ids().unwrap().contains(&a2),
        "收藏必须按身份骨架挂回"
    );
    assert!(
        db.tags_of(a2).unwrap().iter().any(|t| t == "旅行"),
        "标签必须按身份骨架挂回"
    );
    assert_eq!(db.caption_of(a2).unwrap(), "海边", "说明必须按身份骨架挂回");
    assert!(
        db.albums_of(a2).unwrap().iter().any(|a| a.name == "精选"),
        "相册归属必须按身份骨架挂回"
    );

    // 标记不能串到同一目录的其它照片上。
    let b2 = rows.iter().find(|r| r.name == "b.jpg").unwrap().id;
    assert!(
        !db.tags_of(b2).unwrap().iter().any(|t| t == "旅行"),
        "b.jpg 从没打过标签，不能被串上"
    );
}

/// ③ 不存在的根：幂等返回 (0,0)，不报错（重复点击 / 后台竞态都不该炸）。
#[test]
fn purging_a_missing_root_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("p3.sqlite")).unwrap();
    assert_eq!(db.purge_index_for_root(9_999).unwrap(), (0, 0));
}

/// ④ 与 `clear_index_for_root` 的分工：那条必须**保留** roots 行。
///
/// 两条路长期并存：一个「可恢复」，一个「彻底」。任何一条被改成另一条的行为，
/// 就会有一半用户被坑（想恢复的恢复不了 / 想删干净的留个空壳）。
#[test]
fn the_two_paths_stay_distinguishable() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("p4.sqlite")).unwrap();

    let (soft_id, _) = seed(&db, "/media/Soft", "精选 S");
    db.clear_index_for_root(soft_id).unwrap();
    assert!(
        db.root(soft_id).unwrap().is_some(),
        "「删除索引」必须保留根行，否则「重新索引（恢复）」无从下手"
    );
    assert!(db.root_index_removed(soft_id).unwrap());

    let (hard_id, _) = seed(&db, "/media/Hard", "精选 H");
    db.purge_index_for_root(hard_id).unwrap();
    assert!(
        db.root(hard_id).unwrap().is_none(),
        "「彻底删除」必须连根行一起删"
    );
}
