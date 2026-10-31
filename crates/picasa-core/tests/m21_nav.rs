//! M2.1 · E 工作包（导航）验收：**关闭根目录 / 排除子目录 / 排除抗重扫 / 零写入**。
//!
//! 这四条覆盖的是「导航」最容易写错、而且用户一眼就能看出来的地方：
//! 1. 关闭根目录**不删索引**，重新添加要复用同一个 `root_id`（秒开）；
//! 2. 关闭子目录写的是**排除规则**，不是只在树里藏一下；
//! 3. 排除规则必须在**下一次扫描**时生效——否则用户会看到「关了又自己出现」；
//! 4. 以上所有动作都**不碰照片目录**（R-1）。

use picasa_core::{AppPaths, CoreEvent, IndexBoard, IndexService, Library, paths};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

fn isolated_paths(root: &Path) -> AppPaths {
    AppPaths {
        support: root.join("Library/Application Support/app.openpicasa"),
        preferences: root.join("Library/Preferences/app.openpicasa"),
        caches: root.join("Library/Caches/app.openpicasa"),
        logs: root.join("Library/Logs/app.openpicasa"),
    }
}

/// 造一个「看起来像图片」的文件：扫描按扩展名收录，`probe` 失败会退默认值。
fn touch_jpeg(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), b"not-a-real-jpeg").unwrap();
}

/// 用真实 `IndexService` 扫一遍并**等到任务终态**。
///
/// 为什么不用 `apply_scan` 直接造数据：这条测试要验证的正是「扫描器读不读排除规则」，
/// 绕过扫描器就什么都没证明。
fn scan_blocking(lib: &Library, root: &Path) {
    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);
    service.enqueue_scan(&[root.to_path_buf()]);

    let key = paths::canonical_string(root);
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        let snap = board.snapshot();
        // ⚠️ 成功且无失败时 `IndexBoard::finish` 会把**任务列表整个清空**
        // （面板不留历史），所以「任务没了」也是一种完成态，不能只等 `is_terminal`。
        if !snap.running() {
            match snap.task_by_path(&key) {
                Some(t) => {
                    if t.state.is_terminal() {
                        return;
                    }
                }
                None => return, // 列表被清空 = 全部完成
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    panic!("扫描超时：{key}");
}

// 用 i64 纳秒而不是 `SystemTime`：项目 lint 禁止 `SystemTime`（跨平台序列化不稳定），
// 这里只需要「前后两次取值是否相等」，纳秒整数足够且可比较、可打印。
fn rel_snapshot(root: &Path) -> Vec<(PathBuf, u64, Option<i64>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64);
            if md.is_dir() {
                stack.push(p.clone());
            }
            out.push((p, md.len(), mtime));
        }
    }
    out.sort();
    out
}

#[test]
fn closing_a_root_keeps_index_and_reopening_reuses_it() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = isolated_paths(tmp.path());
    paths.ensure().unwrap();

    let photos = tmp.path().join("photos");
    touch_jpeg(&photos.join("a"), "1.jpg");
    touch_jpeg(&photos.join("b"), "2.jpg");

    let mut lib = Library::open(paths.clone()).unwrap();
    lib.add_root(&photos);
    scan_blocking(&lib, &photos);
    lib.reload_tree().unwrap();

    let root = paths::canonical_string(&photos);
    let root_id = lib.db().unwrap().root_by_path(&root).unwrap().unwrap().id;
    let photos_before = lib.db().unwrap().count_photos(None).unwrap();
    assert!(photos_before >= 2, "扫描应入库至少 2 张：{photos_before}");

    // ---- 关闭：树里消失，索引原样保留
    lib.close_root(&photos);
    assert!(lib.root_paths().is_empty(), "关闭后不应还在活跃根目录里");
    let row = lib.db().unwrap().root_by_path(&root).unwrap().unwrap();
    assert_eq!(row.id, root_id, "关闭不新建行");
    assert_eq!(row.state, 1, "state 应置为 closed");
    assert_eq!(
        lib.db().unwrap().count_photos(None).unwrap(),
        photos_before,
        "关闭根目录**不能**删索引（与「从图库移除」的区别）"
    );

    // ---- 重新添加：复用同一个 root_id（这就是「秒开」的结构保证）
    lib.add_root(&photos);
    let row2 = lib.db().unwrap().root_by_path(&root).unwrap().unwrap();
    assert_eq!(row2.id, root_id, "再次添加必须复用 root_id");
    assert_eq!(row2.state, 0, "再次添加应恢复 active");
    assert_eq!(lib.root_paths(), vec![photos.clone()]);
}

#[test]
fn excluding_a_subfolder_survives_a_rescan() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = isolated_paths(tmp.path());
    paths.ensure().unwrap();

    let photos = tmp.path().join("photos");
    touch_jpeg(&photos.join("keep"), "1.jpg");
    touch_jpeg(&photos.join("drop"), "2.jpg");

    let mut lib = Library::open(paths.clone()).unwrap();
    lib.add_root(&photos);
    scan_blocking(&lib, &photos);
    lib.reload_tree().unwrap();

    let drop_dir = photos.join("drop");
    let drop_key = paths::canonical_string(&drop_dir);
    assert!(
        lib.db().unwrap().folder_id(&drop_key).unwrap().is_some(),
        "扫描后 drop 应已入库"
    );
    assert!(
        lib.visible_folders.iter().any(|f| f.path == drop_key),
        "扫描后 drop 应在树里"
    );

    // ---- 关闭子目录
    let excluded = lib.exclude_folder(&drop_dir).unwrap();
    assert!(excluded, "子目录应能被排除");
    assert!(
        lib.db().unwrap().folder_id(&drop_key).unwrap().is_none(),
        "排除后索引应立即移除（不能等下次扫描）"
    );
    assert!(
        !lib.visible_folders.iter().any(|f| f.path == drop_key),
        "排除后树里应隐藏"
    );
    let excl = lib.all_exclusions();
    assert_eq!(excl.len(), 1, "应有一条排除记录");
    assert_eq!(excl[0].1.relative_path, "drop", "相对路径相对根目录");

    // ---- 重扫：排除规则必须生效，**不能**被加回来
    scan_blocking(&lib, &photos);
    lib.reload_tree().unwrap();
    assert!(
        lib.db().unwrap().folder_id(&drop_key).unwrap().is_none(),
        "重扫把被排除的目录加回来了——用户会看到「关了又自己出现」"
    );
    let keep_key = paths::canonical_string(&photos.join("keep"));
    assert!(
        lib.db().unwrap().folder_id(&keep_key).unwrap().is_some(),
        "未排除的目录必须还在"
    );
}

#[test]
fn nav_operations_never_write_the_photo_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = isolated_paths(tmp.path());
    paths.ensure().unwrap();

    let photos = tmp.path().join("photos");
    touch_jpeg(&photos.join("keep"), "1.jpg");
    touch_jpeg(&photos.join("drop"), "2.jpg");

    let mut lib = Library::open(paths.clone()).unwrap();
    lib.add_root(&photos);
    scan_blocking(&lib, &photos);
    lib.reload_tree().unwrap();

    // 扫描之后、导航动作之前：照片目录的清单与 mtime
    let before = rel_snapshot(&photos);

    // 一串导航动作：关闭根目录 → 重开 → 关闭子目录 → 恢复排除
    lib.close_root(&photos);
    lib.add_root(&photos);
    lib.exclude_folder(&photos.join("drop")).unwrap();
    if let Some((_, e)) = lib.all_exclusions().first().cloned() {
        lib.restore_exclusion(e.id).unwrap();
    }

    let after = rel_snapshot(&photos);
    assert_eq!(
        before, after,
        "导航动作改动了照片目录（R-1 违规）：文件/appear 或 mtime 变了"
    );
}
