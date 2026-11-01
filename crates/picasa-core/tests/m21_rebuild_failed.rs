//! M2.1 · 「重建失败索引」的边界测试。
//!
//! 用户看到的现象：一部分照片永远显示灰格子（缩略图解码失败被永久记为
//! `thumb_state = 2`），而新策略下启动扫描不会再去碰它们。于是需要一个
//! **显式入口**把它们修回来。
//!
//! 这个入口最容易写错的地方不是"能不能修"，而是"顺手修了别的"：
//! 同样的库里还有 22 万张**从未尝试过**的待生成照片（历史欠账）。如果重建走的是
//! 「把本子树所有待生成的都补一遍」那条路，用户点一下按钮就是几小时的满载——
//! 这轮修的正是"打开就卡死"，绝不能从另一个按钮把同一个问题请回来。
//!
//! 所以这条测试钉的是：**重建只重做点名的那些，一张欠账都不碰。**

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

/// 收录只看扩展名；`probe` / 解码都会失败，正好用来造"失败项"。
fn touch_jpeg(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), b"not-a-real-jpeg").unwrap();
}

fn wait_idle(board: &IndexBoard, timeout: Duration) {
    let end = Instant::now() + timeout;
    while Instant::now() < end && board.snapshot().running() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!board.snapshot().running(), "任务超时未结束");
}

#[test]
fn rebuild_failed_redoes_only_the_named_photos() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    for i in 0..3 {
        touch_jpeg(&media, &format!("bad{i}.jpg"));
    }

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);

    // ---- 第一次扫描：3 张全部补图失败 ----
    service.enqueue_scan(std::slice::from_ref(&media));
    wait_idle(&board, Duration::from_secs(60));
    let s = lib.db().unwrap().library_thumb_stats().unwrap();
    assert_eq!((s.total, s.failed), (3, 3));

    // ---- 造出「2 张历史欠账 + 1 张真失败」的局面 ----
    // 真实库里这两者都表现为 `thumb_state = 0` 之外的另一种状态，
    // 但重建的判据不能依赖"哪来的"，只能是"点名了哪些 id"。
    let rows = lib
        .db()
        .unwrap()
        .photos(None, Default::default(), 100)
        .unwrap();
    assert_eq!(rows.len(), 3);
    let broken = rows[0].id;
    for r in &rows[1..] {
        lib.db()
            .unwrap()
            .set_thumb_state(r.id, ThumbState::Pending)
            .unwrap();
    }
    let pre = lib.db().unwrap().library_thumb_stats().unwrap();
    assert_eq!(
        (pre.pending, pre.failed),
        (2, 1),
        "前置条件：2 张欠账 + 1 张失败"
    );

    // ---- 重建：只点名那一张失败的 ----
    let ids = vec![broken];
    let reset = lib.db().unwrap().reset_failed_thumbs_by_ids(&ids).unwrap();
    assert_eq!(reset, 1, "只有点名的失败项会被重置");
    service.enqueue_rebuild_failed(ids, &media);
    wait_idle(&board, Duration::from_secs(60));

    let after = lib.db().unwrap().library_thumb_stats().unwrap();
    assert_eq!(
        after.failed, 1,
        "重建过的那张还是坏文件 → 仍应记为失败（这证明真的重试过）"
    );
    assert_eq!(
        after.pending, 2,
        "❌ 重建顺手把历史欠账也补了：只该动点名的那批。\
         这就是「点一下按钮就满载几小时」的复现路径"
    );

    // ---- 进度面板要能看出这是重建批次：分母 = 点名数量，不是整棵子树 ----
    //
    // 子树里共有 3 张照片。若任务的分母是 3（或 22 万这种欠账总量），
    // 就说明它走的是"整批补全"那条路，而不是点名重建。
    let snap = board.snapshot();
    let task = snap
        .tasks
        .iter()
        .find(|t| t.discovered == 1)
        .expect("应当有一个分母为 1 的重建任务");
    assert_eq!(
        task.discovered, 1,
        "分母必须等于点名数量，否则进度条会把「补全整棵子树」当成这次重建"
    );
    assert_eq!(task.failed, 1, "重建失败的那张要计进失败数");
}

#[test]
fn rebuild_with_empty_list_is_a_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    touch_jpeg(&media, "a.jpg");

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);
    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);

    // 空名单：不入队、不建任务、不发事件
    assert_eq!(service.enqueue_rebuild_failed(Vec::new(), &media), 0);
    assert!(
        board.snapshot().tasks.is_empty(),
        "空名单不该在进度面板里留下一个空任务"
    );
    assert!(!board.snapshot().running());
}
