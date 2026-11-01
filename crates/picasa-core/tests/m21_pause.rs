//! M2.1 · 「暂停索引」的契约测试。
//!
//! 用户诉求：**索引界面要有暂停**。听起来像换个按钮文案，实际有三个坑：
//!
//! 1. **暂停必须真的停写**。只把状态标成"已暂停"、后台还在往库里灌数据，
//!    是最坏的一种：用户看到"已暂停"，磁盘还在响，CPU 还在烧。
//! 2. **暂停不能丢进度**。停在目录边界、恢复后从同一位置继续，
//!    而不是"恢复 = 从头再扫一遍"（移动硬盘上那是几十分钟）。
//! 3. **暂停不能卡住图库**。暂停的是「建立索引」，已经入库的照片照常浏览。
//!
//! 下面的测试锁住前两条（第 3 条靠 `m21_stream.rs` 的流式落库保证）。

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

fn touch_jpeg(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), b"not-a-real-jpeg").unwrap();
}

#[test]
fn pause_stops_writing_and_resume_finishes_the_same_scan() {
    // 目录数要够多，保证「按下暂停」时扫描还没跑完——否则这条测试什么都验证不到。
    let dirs = 600usize;
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

    let count = || lib.db().unwrap().count_photos(None).unwrap();

    // ① 等扫描真的动起来（已入库一部分，但远没扫完）
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut started = false;
    while Instant::now() < deadline {
        let n = count();
        if n > 0 && n < total {
            started = true;
            break;
        }
        if !board.snapshot().running() && n == total {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        started,
        "扫描没能在「部分入库」的窗口里被观察到（当前 {} 张 / 共 {total}）——\
         这条测试依赖扫描仍在进行中，先看流式落库是否退化",
        count()
    );

    // ② 暂停
    service.pause();
    assert!(service.is_paused());
    // 已经在处理的那个目录会写完（暂停点在目录边界），给它一个落定窗口
    std::thread::sleep(Duration::from_millis(400));
    let after_pause = count();

    // ③ 真的停住了：再等 600 ms，一行都不该多
    std::thread::sleep(Duration::from_millis(600));
    let later = count();
    assert_eq!(
        later, after_pause,
        "暂停后仍在往库里写：{after_pause} → {later}。\
         \"已暂停\"必须真的停写，否则用户看到的状态是假的"
    );
    assert!(
        later < total,
        "暂停时其实已经扫完了（{later}/{total}）——暂停没生效，或测试规模太小"
    );

    // ④ 恢复：从暂停处接着跑，最终必须一张不少
    service.resume();
    assert!(!service.is_paused());

    let end = Instant::now() + Duration::from_secs(120);
    while Instant::now() < end && board.snapshot().running() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let final_n = count();
    assert_eq!(
        final_n, total,
        "恢复后总数不对：期望 {total}，实际 {final_n}。\
         暂停丢掉进度（或从错误位置续跑）都会撞在这里"
    );
}

/// 暂停只影响"建索引"，不影响已经入库的照片——图库照常可读。
#[test]
fn pausing_does_not_block_reading_the_library() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    for i in 0..20 {
        touch_jpeg(&media.join(format!("a{i:03}")), "p.jpg");
    }

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);
    service.enqueue_scan(std::slice::from_ref(&media));

    // 立刻暂停（可能连一个目录都没扫完，正好检验"暂停时读库不卡"）
    service.pause();

    // 读路径不能被暂停挡住：这里只是普通查询，若它被卡住就是设计错误
    let n = lib.db().unwrap().count_photos(None).unwrap();
    assert!(n <= 20);

    service.resume();
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end && board.snapshot().running() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(lib.db().unwrap().count_photos(None).unwrap(), 20);
}
