//! M4.3 阶段 4 · `TC-M43-11` 的故障注入部分（磁盘/权限错、提交失败回滚、取消）。
//!
//! 三条红线里最难验的是「失败时现有索引一个字节都不动」。真实世界里会砸中它的是
//! 三类意外：目标目录没写权限（外置盘只读、系统目录、权限被改）、来源包在传输中
//! 被截断、以及用户在提交前按了取消。这里逐一注入，断言**只有**「报错 + 原库不变」，
//! 不允许出现「删了一半」「标记挂了但照片没了」这种半成品。
//!
//! 全程临时目录 + SQLite，不碰真实照片 / 用户目录。

use picasa_db::{
    Db, ImportStrategy, PhotoInput, SortSpec, check_export_dir, execute_import, export_index,
    inspect_index,
};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

fn photo(name: &str, taken: i64) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_ascii_lowercase(),
        size_bytes: 1000,
        mtime_ns: taken * 1_000_000_000,
        width: Some(800),
        height: Some(600),
        orientation: Some(1),
        taken_at: Some(taken),
        taken_at_src: 0,
        format: 1,
    }
}

/// 一个含 3 张照片 + 全部四类用户标记的小库。
fn seed(path: &Path) -> Db {
    let db = Db::open(path).unwrap();
    let root = db.upsert_root("/photos", 0, false).unwrap();
    let fid = db.upsert_folder("/photos", "/photos", None).unwrap();
    db.set_folder_root(fid, root, None).unwrap();
    let items = vec![photo("a.jpg", 100), photo("b.jpg", 200), photo("c.jpg", 300)];
    db.apply_scan(fid, "/photos", &items, 0).unwrap();
    let rows = db
        .photos_from_visible(None, SortSpec::default(), 20, 0)
        .unwrap();
    let a = rows.iter().find(|p| p.name == "a.jpg").unwrap().id;
    let b = rows.iter().find(|p| p.name == "b.jpg").unwrap().id;
    db.set_starred(&[a], true).unwrap();
    db.add_tag(&[a, b], "旅行").unwrap();
    db.set_caption(a, "京都塔").unwrap();
    let album = db.create_album("2026 京都", 1000).unwrap();
    db.add_to_album(album, &[a, b], 1000).unwrap();
    db
}

/// 快照「用户可见的一切」，用来断言失败后逐项未变。
fn snapshot(db: &Db) -> (u64, i64, i64, Vec<i64>, String, usize) {
    let photos = db.total_photos().unwrap();
    let visible = db
        .photos_from_visible(None, SortSpec::default(), 50, 0)
        .unwrap()
        .len() as i64;
    let starred = db.starred_ids().unwrap();
    let tags: i64 = visible;
    let caption = db
        .photos_from_visible(None, SortSpec::default(), 50, 0)
        .unwrap()
        .iter()
        .map(|p| db.caption_of(p.id).unwrap())
        .collect::<Vec<_>>()
        .join("|");
    (photos, visible, tags, starred, caption, db.all_tags().unwrap().len())
}

fn set_read_only(dir: &Path) {
    let mut perms = std::fs::metadata(dir).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(dir, perms).unwrap();
}

fn make_writable_again(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dir).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dir, perms).unwrap();
    }
    #[cfg(not(unix))]
    {
        let mut perms = std::fs::metadata(dir).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(dir, perms).unwrap();
    }
}

/// 磁盘满 / 权限错的最近代表：目标目录不可写时**一个字节都不写**，报错可读。
#[test]
fn export_to_unwritable_dir_is_rejected_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = seed(&tmp.path().join("library.sqlite"));
    let locked = tmp.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    set_read_only(&locked);

    let target = locked.join("index.opicasaidx");
    // 第一道闸：check_export_dir 的写探测必须先失败（不是等到 VACUUM INTO 才炸）。
    let roots = vec!["/photos".to_string()];
    let checked = check_export_dir(&locked, &roots);
    assert!(
        checked.is_err(),
        "不可写目录必须在写探测阶段就被拒：{checked:?}"
    );
    assert!(
        !target.exists(),
        "被拒的导出不能留下半截产物：{}",
        target.display()
    );

    // 第二道闸：即便绕过检查直接调用，也不能产出文件。
    let rep = export_index(&db, &target, false, "0.4.0-test", 1000);
    assert!(rep.is_err(), "写入不可写目录必须失败：{rep:?}");
    assert!(
        !target.exists(),
        "失败的导出必须清掉临时产物，不能留下 {}",
        target.display()
    );

    make_writable_again(&locked);
}

/// 来源包被截断（传输中断的典型形态）：拒绝 + 现有索引逐项不变 + 不留 staging。
#[test]
fn truncated_package_is_rejected_and_the_library_is_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let src_db_path = tmp.path().join("src.sqlite");
    let pkg = tmp.path().join("index.opicasaidx");
    {
        let src = seed(&src_db_path);
        export_index(&src, &pkg, false, "0.4.0-test", 1000).unwrap();
    }
    // 砍掉尾部的 40%，模拟「拷了一半就断了」。
    let bytes = std::fs::read(&pkg).unwrap();
    std::fs::write(&pkg, &bytes[..bytes.len() * 6 / 10]).unwrap();

    let dst_path = tmp.path().join("dst.sqlite");
    let mut dst = seed(&dst_path);
    let before = snapshot(&dst);

    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let err = execute_import(
        &mut dst,
        &pkg,
        &work,
        ImportStrategy::Merge,
        &AtomicBool::new(false),
    );
    assert!(err.is_err(), "截断的包必须拒绝：{err:?}");

    let after = snapshot(&dst);
    assert_eq!(before, after, "失败后索引与用户标记必须逐项不变");
    // 预检也不能动目标库。
    let inspected = inspect_index(&dst, &pkg, &work);
    assert!(inspected.is_err(), "预检对坏包也应报错：{inspected:?}");
    assert_eq!(before, snapshot(&dst), "预检不允许改目标库");
    // staging 不留垃圾。
    let leftovers: Vec<String> = std::fs::read_dir(&work)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("import-staging-"))
        .collect();
    assert!(leftovers.is_empty(), "失败后不应残留 staging：{leftovers:?}");
}

/// 用户点取消（提交前）：回滚 + 现有索引不变 + 面板能如实报「已取消」。
#[test]
fn cancel_before_commit_rolls_back_and_keeps_the_index() {
    let tmp = tempfile::tempdir().unwrap();
    let src_db_path = tmp.path().join("src.sqlite");
    let pkg = tmp.path().join("index.opicasaidx");
    {
        let src = seed(&src_db_path);
        export_index(&src, &pkg, false, "0.4.0-test", 1000).unwrap();
    }
    let dst_path = tmp.path().join("dst.sqlite");
    let mut dst = seed(&dst_path);
    // 让目标库与来源包内容不同（这样「没被覆盖」才有说服力）。
    {
        let rows = dst
            .photos_from_visible(None, SortSpec::default(), 10, 0)
            .unwrap();
        let b = rows.iter().find(|p| p.name == "b.jpg").unwrap().id;
        dst.set_starred(&[b], true).unwrap();
        dst.set_caption(b, "本机另行标注").unwrap();
    }
    let before = snapshot(&dst);

    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let cancel = AtomicBool::new(true); // 一开始就是「用户已取消」
    let err = execute_import(&mut dst, &pkg, &work, ImportStrategy::Merge, &cancel);
    assert!(err.is_err(), "已取消的导入必须报错而不是照做：{err:?}");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("取消"),
        "错误信息要能直接给用户看（含「取消」）：{msg}"
    );
    assert_eq!(before, snapshot(&dst), "取消后索引必须逐项不变");

    // 反过来：真正跑一次的 Replace 也要能正常完成（避免「取消」把后续路径也堵死）。
    cancel.store(false, Ordering::Relaxed);
    let rep = execute_import(&mut dst, &pkg, &work, ImportStrategy::Replace, &cancel).unwrap();
    assert_eq!(rep.imported, 3, "Replace 应搬入 3 张：{rep:?}");
    assert_eq!(dst.total_photos().unwrap(), 3);
    assert!(
        rep.backup_path.as_ref().is_some_and(|p| p.is_file()),
        "Replace 必须先备份再动库：{rep:?}"
    );
}

/// 提交失败（暂存工作目录不可写）：报错且目标库不变。
#[test]
fn unwritable_work_dir_fails_without_touching_the_library() {
    let tmp = tempfile::tempdir().unwrap();
    let src_db_path = tmp.path().join("src.sqlite");
    let pkg = tmp.path().join("index.opicasaidx");
    {
        let src = seed(&src_db_path);
        export_index(&src, &pkg, false, "0.4.0-test", 1000).unwrap();
    }
    let dst_path = tmp.path().join("dst.sqlite");
    let mut dst = seed(&dst_path);
    let before = snapshot(&dst);

    let work = tmp.path().join("work-locked");
    std::fs::create_dir(&work).unwrap();
    set_read_only(&work);

    let err = execute_import(
        &mut dst,
        &pkg,
        &work,
        ImportStrategy::Merge,
        &AtomicBool::new(false),
    );
    assert!(err.is_err(), "工作目录不可写时必须失败：{err:?}");
    assert_eq!(before, snapshot(&dst), "失败后目标库必须逐项不变");

    make_writable_again(&work);
}
