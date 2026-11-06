//! M4 性能回归：EXIF 回填**不得长期持有 DB 锁**（docs/11 §13.7）。
//!
//! 现场：早期版本在 `Mutex<Db>` 锁内逐张 `probe()` 文件（File::open + 解析 EXIF），
//! 把索引 / 缩略图写入线程全堵死，用户感知「索引速度太慢」。
//! 现在拆成 collect_pending（短锁）→ probe_paths（**完全无锁**）→ write_batch（短锁）。
//!
//! 本测试用「探测期间另一个线程能否立刻拿到锁并写入」来证明锁粒度确实放开了。

use picasa_core::m4_exif as exif;
use picasa_db::{Db, PhotoInput};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn photo(name: &str) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_ascii_lowercase(),
        size_bytes: 10,
        mtime_ns: 1_000_000_000,
        width: Some(1),
        height: Some(1),
        orientation: Some(1),
        taken_at: Some(1000),
        taken_at_src: 2,
        format: 1,
    }
}

fn seed(dir: &Path, n: usize) -> Db {
    let db = Db::open(dir.join("lib.sqlite")).unwrap();
    let root = db.upsert_root(dir.to_str().unwrap(), 0, false).unwrap();
    let fid = db
        .upsert_folder(dir.to_str().unwrap(), dir.to_str().unwrap(), None)
        .unwrap();
    db.set_folder_root(fid, root, None).unwrap();
    let mut items = Vec::with_capacity(n);
    for i in 0..n {
        let name = format!("p{i:04}.jpg");
        // 内容是文本，probe() 会降级但不 panic；重点是它要 File::open
        std::fs::write(dir.join(&name), b"not a real jpeg").unwrap();
        items.push(photo(&name));
    }
    db.apply_scan(fid, dir.to_str().unwrap(), &items, 0).unwrap();
    db
}

#[test]
fn probe_phase_does_not_hold_the_db_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Arc::new(Mutex::new(seed(tmp.path(), 60)));

    // ① 短锁取待探测路径 —— 之后**立刻放锁**
    let pending = {
        let guard = db.lock().unwrap();
        exif::collect_pending(&guard, 60).unwrap()
    };
    assert_eq!(pending.len(), 60, "应有 60 张待回填");

    // ② 探测阶段（最贵）：在**不持锁**的线程里跑，同时主线程反复尝试拿锁写入。
    // 若实现错误地在锁内 probe，这里会明显被堵（每段写入 > 100ms）。
    let probe_handle = std::thread::spawn(move || exif::probe_paths(pending));

    let mut max_lock_wait = Duration::ZERO;
    for i in 0..40 {
        let t = Instant::now();
        {
            let guard = db.lock().unwrap();
            // 模拟索引/缩略图线程的一次写入
            let value = format!("{i}");
            guard
                .set_setting_raw("bench_probe", Some(&value))
                .unwrap();
        }
        max_lock_wait = max_lock_wait.max(t.elapsed());
        std::thread::sleep(Duration::from_millis(2));
    }
    let (items, _failed) = probe_handle.join().unwrap();
    assert_eq!(items.len(), 60);

    // 探测进行中，主线程拿锁不应被明显阻塞（留 250ms 余量给 CI 抖动）
    assert!(
        max_lock_wait < Duration::from_millis(250),
        "探测阶段疑似持锁：单次等锁最久 {max_lock_wait:?}"
    );

    // ③ 短锁写回
    let written = {
        let guard = db.lock().unwrap();
        exif::write_batch(&guard, &items).unwrap()
    };
    assert_eq!(written, 60);
    assert_eq!(
        db.lock().unwrap().photos_pending_exif(100).unwrap().len(),
        0,
        "回填后无待处理行"
    );
}

#[test]
fn three_stages_are_equivalent_to_backfill_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let db = seed(tmp.path(), 10);
    let pending = exif::collect_pending(&db, 10).unwrap();
    let (items, failed) = exif::probe_paths(pending);
    let ok = exif::write_batch(&db, &items).unwrap();
    assert_eq!(ok, 10);
    assert_eq!(failed, 0);
    assert_eq!(db.photos_pending_exif(100).unwrap().len(), 0);
}
