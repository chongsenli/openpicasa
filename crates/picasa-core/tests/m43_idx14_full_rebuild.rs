//! M4.3 阶段 2 第四批 · IDX-14「全量重建」端到端接线契约。
//!
//! 锁的是 `IndexService::enqueue_rebuild` 这条 force 通路真的贯通到落库与补图，
//! 而不是 DB 层 `apply_scan_force` 的单元行为（后者由
//! `picasa-db/tests/m43_idx14_rebuild.rs` 覆盖）：
//!   · force 绕过普通扫描的 L1 缓存命中，让**每张**已存在照片都重新生成缩略图
//!     （普通增量扫描在文件没变时一张都不补 —— 用 ThumbReady 事件做对照）；
//!   · 旧缩略图 BLOB 从不被删（`get_thumb` 全程 Some），靠 `put_thumb` 原子替换；
//!   · 全量重建的持久任务行标成 `TaskKind::Reindex`，暂停后 `resume_task`
//!     仍走 force 入口，不会退化成「L1 全命中、啥都不做」的普通增量扫描。

use picasa_core::{AppPaths, CoreEvent, IndexBoard, IndexService, Library, paths};
use picasa_db::{SortSpec, TaskKind, TaskScopeKind, TaskStatus};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

fn isolated_paths(root: &Path) -> AppPaths {
    AppPaths {
        support: root.join("Library/Application Support/app.openpicasa"),
        preferences: root.join("Library/Preferences/app.openpicasa"),
        caches: root.join("Library/Caches/app.openpicasa"),
        logs: root.join("Library/Logs/app.openpicasa"),
    }
}

/// 写一张**能真正解码出缩略图**的 JPEG（否则 G1 会如实落 PartialFailed）。
fn touch_jpeg(dir: &Path, name: &str) {
    use image::{Rgba, RgbaImage};
    std::fs::create_dir_all(dir).unwrap();
    let img = RgbaImage::from_pixel(6, 6, Rgba([10, 20, 30, 255]));
    std::fs::write(dir.join(name), picasa_imaging::encode_jpeg(&img, 82).unwrap()).unwrap();
}

fn wait_until<F: Fn() -> bool>(cond: F, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

/// 收干事件流，返回这段窗口里所有 `ThumbReady` 的 photo_id。
/// `quiet` 是「连续多久没新事件就认为安静」，用于等异步 worker 收工。
fn drain_thumb_ready(rx: &Receiver<CoreEvent>, quiet: Duration) -> Vec<i64> {
    let mut ids = Vec::new();
    let cap = Instant::now() + Duration::from_secs(30);
    while Instant::now() < cap {
        match rx.recv_timeout(quiet) {
            Ok(CoreEvent::ThumbReady { photo_id, .. }) => ids.push(photo_id),
            Ok(_) => continue,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        }
    }
    ids
}

fn photo_ids(lib: &Library) -> Vec<i64> {
    lib.db()
        .unwrap()
        .photos(None, SortSpec::default(), 100)
        .unwrap()
        .iter()
        .map(|p| p.id)
        .collect()
}

/// ① force 全量重建必须绕过 L1：文件没变时普通增量扫描一张 ThumbReady 都不发，
///    而 `enqueue_rebuild` 让两张全部重新生成；旧 BLOB 全程存在；任务行标 Reindex。
#[test]
fn enqueue_rebuild_bypasses_l1_preserves_blob_and_marks_reindex() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    touch_jpeg(&media.join("a"), "p.jpg");
    touch_jpeg(&media.join("b"), "p.jpg");

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);
    let scope = paths::canonical_string(&media);

    let (tx, rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);

    // ---- 第一次：普通扫描，建立索引 + 缩略图 ----
    service.enqueue_scan(std::slice::from_ref(&media));
    assert!(
        wait_until(|| lib.db().unwrap().count_photos(None).unwrap() == 2, 30),
        "初次扫描应入库 2 张照片"
    );
    let first_thumbs = drain_thumb_ready(&rx, Duration::from_millis(1500));
    assert_eq!(first_thumbs.len(), 2, "初次扫描应为 2 张照片生成缩略图");

    let ids = photo_ids(&lib);
    assert_eq!(ids.len(), 2);
    for id in &ids {
        assert!(
            lib.db().unwrap().get_thumb(*id, 0).unwrap().is_some(),
            "初次扫描后缩略图 BLOB 应存在"
        );
    }

    // ---- 第二次：普通增量扫描（文件没变）—— 对照组，应为零 ThumbReady ----
    service.enqueue_scan(std::slice::from_ref(&media));
    let incremental = drain_thumb_ready(&rx, Duration::from_millis(1500));
    assert!(
        incremental.is_empty(),
        "文件未变时普通增量扫描必须命中 L1 缓存、不重新生成缩略图，实际发了 {} 个 ThumbReady",
        incremental.len()
    );

    // ---- 第三次：force 全量重建 —— 两张都应重新生成 ----
    service.enqueue_rebuild(std::slice::from_ref(&media));
    let rebuilt = drain_thumb_ready(&rx, Duration::from_millis(1500));
    assert!(
        rebuilt.len() >= 2,
        "force 全量重建必须绕过 L1、让已存在照片重新生成缩略图，实际只有 {} 个 ThumbReady",
        rebuilt.len()
    );
    for id in &ids {
        assert!(
            rebuilt.contains(id),
            "photo {id} 未被 force 重新生成缩略图"
        );
    }

    // 旧 BLOB 从不被删（force 不 DELETE FROM thumbs，只 put_thumb 原子替换）。
    for id in &ids {
        assert!(
            lib.db().unwrap().get_thumb(*id, 0).unwrap().is_some(),
            "全量重建过程中旧缩略图 BLOB 不应被删（photo {id}）"
        );
    }

    // 持久任务行标成 Reindex（force 意图持久化，供 resume_task 识别）。
    let found = wait_until(
        || {
            lib.db()
                .unwrap()
                .task_intents_for_scope(&scope)
                .ok()
                .is_some_and(|v| {
                    v.iter().any(|t| {
                        t.kind == TaskKind::Reindex.code()
                            && t.status == TaskStatus::Completed.code()
                    })
                })
        },
        30,
    );
    assert!(found, "全量重建任务行须为 Reindex 且落到 Completed 终态");
}

/// ② 暂停后的 Reindex 任务行，`resume_task` 必须继续走 force 入口。
///    若错走 `enqueue_scan`，L1 全命中会一张都不补 —— 用 ThumbReady 区分。
#[test]
fn resume_task_keeps_force_for_reindex_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    touch_jpeg(&media, "p.jpg");

    let mut lib = Library::open(isolated_paths(tmp.path())).unwrap();
    lib.add_root(&media);
    let scope = paths::canonical_string(&media);

    let (tx, rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);

    service.enqueue_scan(std::slice::from_ref(&media));
    assert!(
        wait_until(|| lib.db().unwrap().count_photos(None).unwrap() == 1, 30),
        "初次扫描应入库 1 张照片"
    );
    assert_eq!(drain_thumb_ready(&rx, Duration::from_millis(1500)).len(), 1);

    // 手工造一条「被用户暂停的 Reindex 任务行」，模拟崩溃/暂停后待续跑的全量重建。
    let task_id = {
        let db = lib.db().unwrap();
        let id = db
            .begin_index_task(None, &scope, TaskKind::FirstScan, TaskScopeKind::Root)
            .unwrap();
        db.mark_task_reindex(id).unwrap();
        db.set_task_user_paused(id, true).unwrap();
        id
    };

    assert!(service.resume_task(task_id), "resume_task 应成功");

    let resumed = drain_thumb_ready(&rx, Duration::from_millis(1500));
    assert!(
        !resumed.is_empty(),
        "Reindex 任务续跑必须仍走 force 全量重建（重新生成缩略图），\
         退化成普通增量扫描会一张 ThumbReady 都不发"
    );

    let intent = lib
        .db()
        .unwrap()
        .task_intent(task_id)
        .unwrap()
        .expect("任务行仍应存在");
    assert_eq!(
        intent.kind,
        TaskKind::Reindex.code(),
        "续跑不得把 Reindex 意图改写回 FirstScan"
    );
    assert!(!intent.user_paused, "resume 后 user_paused 应被清除");
}
