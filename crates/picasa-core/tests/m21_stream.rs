//! M2.1 · 「索引与主界面必须异步」的契约测试。
//!
//! 用户诉求原话：**「读取移动硬盘文件夹里面的图片，主界面弹不出来。
//! 我需要检索和主界面是异步的。」**
//!
//! 旧实现把整棵目录树遍历完（含逐张 `probe` 读 EXIF）才逐目录落库。移动硬盘上
//! 这段时间是几分钟到几十分钟——期间库里一行照片都没有，主界面只能是一片空白。
//! 这不是「慢」，是**功能上等于没有**：用户看不到任何进展。
//!
//! 现在改成「先枚举目录，再逐目录落库」，每处理完一个目录就入库并广播。
//! 下面这条测试锁住这个契约：**扫描还没结束，库里就应该已经能查到照片**。

use picasa_core::{AppPaths, CoreEvent, IndexBoard, IndexService, Library};
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

/// 造一个「看起来像图片」的文件：收录只看扩展名，`probe` 失败会退默认值。
fn touch_jpeg(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), b"not-a-real-jpeg").unwrap();
}

#[test]
fn photos_are_queryable_while_the_scan_is_still_running() {
    // 800 个相册目录 × 2 张 = 1600 个文件。数量要够多，让扫描持续几百毫秒以上，
    // 否则观察窗口太窄、测试会因「跑得太快」而随机失败。
    let dirs = 800usize;
    let per_dir = 2usize;
    let total = (dirs * per_dir) as u64;

    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    for i in 0..dirs {
        let d = media.join(format!("album{i:04}"));
        for j in 0..per_dir {
            touch_jpeg(&d, &format!("p{j}.jpg"));
        }
    }

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);
    service.enqueue_scan(std::slice::from_ref(&media));

    let mut saw_partial = false;
    let mut samples = 0u32;
    let deadline = Instant::now() + Duration::from_secs(90);

    while Instant::now() < deadline {
        let snap = board.snapshot();
        let n = lib.db().unwrap().count_photos(None).unwrap();
        // 「部分」= 已经入库但不是全部 → 说明落库与遍历是交错的，不是串行到底
        if n > 0 && n < total {
            saw_partial = true;
            break;
        }
        if !snap.running() {
            break;
        }
        samples += 1;
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        saw_partial,
        "扫描结束（或超时）前一次都没查到「部分照片」（采样 {samples} 次，最后 {} 张）。\
         这说明落库仍被压在整棵树遍历之后——移动硬盘上就是「主界面一直空白」。",
        lib.db().unwrap().count_photos(None).unwrap()
    );

    // 收尾：等扫描真正结束，确认最终数据也是完整的（流式不能以丢数据为代价）
    let end = Instant::now() + Duration::from_secs(90);
    while Instant::now() < end && board.snapshot().running() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let final_n = lib.db().unwrap().count_photos(None).unwrap();
    assert_eq!(
        final_n, total,
        "扫描结束后总数不对：期望 {total}，实际 {final_n}"
    );
}
