//! M2.1 · 「启动只做增量发现、不补历史缩略图」的契约测试。
//!
//! 背景是真实库的规模：25.9 万张照片、4330 个目录、其中 **22.4 万张没有缩略图**。
//! 旧实现每次启动都把「整棵子树的待补缩略图」补一遍，在 5W 无风扇的机器上
//! 是**几小时**的满载——用户看到的是「打开就卡死」，而不是「相册」。
//!
//! 现在的口径：
//! · 普通扫描只给**本次新增 / 缓存键变化**的照片补图（`ScanApply::touched`）；
//! · 历史欠账只有在用户显式点「重试 / 补全」时才动。
//!
//! 下面这条测试把这条边界钉死——一旦有人把 `generate_thumbs` 又接回扫描主路径，
//! 它会立刻失败。

use picasa_core::{AppPaths, CoreEvent, IndexBoard, IndexService, Library};
use picasa_db::ThumbState;
use std::path::Path;
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

/// 造一个「看起来像图片」的文件：收录只看扩展名，`probe` 会失败并退默认值。
/// 这里**故意不用真图片**——测试只关心「有没有去补」，不关心补出来的像素。
fn touch_jpeg(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), b"not-a-real-jpeg").unwrap();
}

fn wait_idle(board: &IndexBoard, timeout: Duration) {
    let end = Instant::now() + timeout;
    while Instant::now() < end && board.snapshot().running() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!board.snapshot().running(), "扫描超时未结束");
}

fn states(lib: &Library) -> picasa_db::ThumbStats {
    lib.db().unwrap().library_thumb_stats().unwrap()
}

#[test]
fn startup_scan_only_thumbs_what_is_new_and_leaves_the_backlog_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    for i in 0..3 {
        touch_jpeg(&media, &format!("old{i}.jpg"));
    }

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);

    // ---- 第一次扫描：3 张都会去补图（假图片 → 解码失败 → Failed）----
    service.enqueue_scan(std::slice::from_ref(&media));
    wait_idle(&board, Duration::from_secs(60));

    let s = states(&lib);
    assert_eq!(s.total, 3, "第一次扫描应入库 3 张");
    assert_eq!(
        s.failed, 3,
        "假图片应当全部被标记为失败（这证明扫描确实尝试过补图）"
    );

    // ---- 人为把这 3 张退回「待生成」，模拟 22 万张历史欠账 ----
    let ids: Vec<i64> = lib
        .db()
        .unwrap()
        .photos(None, Default::default(), 100)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids.len(), 3);
    for id in &ids {
        lib.db()
            .unwrap()
            .set_thumb_state(*id, ThumbState::Pending)
            .unwrap();
    }
    assert_eq!(
        states(&lib).pending,
        3,
        "前置条件：3 张都应为待生成"
    );

    // ---- 第二次扫描：目录里只多了一张新照片 ----
    touch_jpeg(&media, "brand-new.jpg");
    service.enqueue_scan(std::slice::from_ref(&media));
    wait_idle(&board, Duration::from_secs(60));

    let after = states(&lib);
    assert_eq!(after.total, 4, "第二次扫描后应共 4 张");
    assert_eq!(
        after.pending, 3,
        "❌ 启动扫描把历史欠账也补了：应当只处理新增的那 1 张。\
         这正是「打开程序就满载几小时」的根因"
    );
    assert_eq!(after.failed, 1, "新增的那张应当被尝试补图并标记失败");

    // ---- 显式重试：这时候才允许动历史欠账 ----
    let root_id = lib
        .db()
        .unwrap()
        .folder_id(&media.to_string_lossy())
        .unwrap()
        .unwrap();
    lib.db()
        .unwrap()
        .reset_failed_thumbs_subtree(root_id)
        .unwrap();
    service.enqueue_retry(root_id, &media);
    wait_idle(&board, Duration::from_secs(60));

    let retried = states(&lib);
    assert_eq!(
        retried.pending, 0,
        "显式重试应当把那 3 张历史欠账都过一遍"
    );
}
