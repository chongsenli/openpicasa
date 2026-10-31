//! M2.1 · B 工作包：宽高回填（`docs/05` §2.1「索引增强」）。
//!
//! 这三条覆盖的是**最容易写错的三件事**，而不是「能跑通」：
//! 1. 后台无人催促时也会把缺口补上（批量路径的入口）；
//! 2. 已经失败的条目不因为「栅格每帧都递一遍」而被反复重试（去重）；
//! 3. **盘不在时不把行判死**（这是真实库样本里 61/1150 那条坑，见 `docs/05` §1.8）。

use picasa_core::{DimsService, IndexBoard, paths};
use picasa_db::{Db, PhotoInput, TaskKind, TaskStatus};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn write_jpeg(path: &Path, w: u32, h: u32) {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 255) as u8, (y % 255) as u8, 96])
    });
    let mut buf = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new(&mut buf);
    enc.encode(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    std::fs::File::create(path)
        .unwrap()
        .write_all(&buf)
        .unwrap();
}

fn input(name: &str) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: paths::compare_key(name),
        size_bytes: 1,
        mtime_ns: 1,
        // 关键：宽高未知 → dims_status = 0（模拟 M2 旧库里那批从没读到过的照片）
        width: None,
        height: None,
        orientation: Some(1),
        taken_at: Some(1_300_000_000),
        taken_at_src: 0,
    }
}

/// 建一个目录行 + 若干「宽高未知」的照片行。返回 folder_id。
fn seed(db: &Db, dir: &Path, names: &[&str]) -> i64 {
    let key = paths::canonical_string(dir);
    let fid = db.upsert_folder(&key, &paths::compare_key(&key), None).unwrap();
    let items: Vec<PhotoInput> = names.iter().map(|n| input(n)).collect();
    db.apply_scan(fid, &key, &items, 0).unwrap();
    fid
}

fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

#[test]
fn backfills_unknown_dims_without_being_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("photos");
    std::fs::create_dir_all(&dir).unwrap();
    write_jpeg(&dir.join("a.jpg"), 400, 300);
    write_jpeg(&dir.join("b.jpg"), 120, 80);

    let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
    let fid = seed(&db.lock().unwrap(), &dir, &["a.jpg", "b.jpg"]);

    let board = Arc::new(IndexBoard::new());
    let _svc = DimsService::new(db.clone(), board);
    assert!(
        wait_until(15, || db.lock().unwrap().dims_stats().unwrap().done == 2),
        "后台应在无人催促时补齐宽高"
    );

    let photos = db
        .lock()
        .unwrap()
        .photos(Some(&[fid]), picasa_db::SortSpec::default(), 10)
        .unwrap();
    let a = photos.iter().find(|p| p.name == "a.jpg").unwrap();
    assert_eq!((a.width, a.height), (Some(400), Some(300)));
    assert_eq!(a.orientation, Some(1), "没有 EXIF 时方向归一为 1");

    let stats = db.lock().unwrap().dims_stats().unwrap();
    assert_eq!((stats.total, stats.done, stats.pending), (2, 2, 0));

    // 任务行走的是 kind=4（回填），并且跑完就结案——不能留一条永远「进行中」的幽灵。
    // ⚠️ 轮询时**每次重新加锁**：把 guard 提到循环外面会让回填线程卡在这把锁上，
    // 于是「永远等不到结案」——第一版就是这么写错的（测试自己制造了死锁）。
    assert!(
        wait_until(10, || {
            db.lock()
                .unwrap()
                .recent_tasks(5)
                .unwrap()
                .iter()
                .any(|t| {
                    t.kind == TaskKind::DimsBackfill.code()
                        && t.status == TaskStatus::Completed.code()
                })
        }),
        "回填任务应落一行 index_tasks 并结案"
    );
}

#[test]
fn failed_photo_is_not_retried_on_every_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("photos");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("broken.jpg"), b"not an image").unwrap();

    let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
    let fid = seed(&db.lock().unwrap(), &dir, &["broken.jpg"]);
    let photo_id = db
        .lock()
        .unwrap()
        .photos(Some(&[fid]), picasa_db::SortSpec::default(), 10)
        .unwrap()[0]
        .id;

    let svc = DimsService::new(db.clone(), Arc::new(IndexBoard::new()));
    assert!(
        wait_until(15, || db.lock().unwrap().dims_stats().unwrap().failed == 1),
        "坏文件应被判为终态失败"
    );

    // 栅格每帧都会把「宽高还是空」的 id 递进来（失败项的宽高也是空）：
    // 连递 200 次，不该产生 200 次读盘、也不该把失败计数堆上去。
    for _ in 0..20 {
        svc.request_visible(&[photo_id; 10]);
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(300));

    assert_eq!(svc.progress().failed, 1, "同一张只算一次失败");
    assert_eq!(db.lock().unwrap().dims_stats().unwrap().failed, 1);
}

#[test]
fn offline_folder_is_deferred_not_marked_failed() {
    let tmp = tempfile::tempdir().unwrap();
    // 目录**不创建**：模拟外置盘没插（真实样本里 61 张就长这样）
    let gone = tmp.path().join("unplugged-volume");

    let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
    seed(&db.lock().unwrap(), &gone, &["a.jpg", "b.jpg", "c.jpg"]);

    let svc = DimsService::new(db.clone(), Arc::new(IndexBoard::new()));
    assert!(
        wait_until(10, || svc.progress().deferred > 0),
        "应当把这几张记为「推迟」而不是失败"
    );
    std::thread::sleep(Duration::from_millis(300));

    let stats = db.lock().unwrap().dims_stats().unwrap();
    assert_eq!(stats.total, 3);
    assert_eq!(
        stats.pending, 3,
        "盘不在时状态位必须留在「待回填」——判死就再也回不来了"
    );
    assert_eq!(stats.failed, 0, "「盘没插」不是「文件坏了」");
    assert_eq!(svc.progress().failed, 0);
    assert!(svc.progress().deferred >= 3);
}
