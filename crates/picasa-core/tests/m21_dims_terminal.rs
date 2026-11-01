//! M2.1 · 「读不到宽高」是终态，不该每次启动都被重判一次。
//!
//! 真实库里的现象：**903 张照片**永远没有宽高。它们不是丢了、也不是盘没插，
//! 而是文件读得到、但拿不到尺寸（格式不认 / 文件头损坏 / 内容其实是别的格式）。
//! `docs/05` §2.1 的判据早就把它们写成了终态（`dims_status = 2`），问题出在两处
//! 会把终态**改回去**的地方：
//!
//! 1. 扫描落库的「缓存键未变」分支，早期写法是 `ELSE 0` —— 无条件打回「待回填」；
//! 2. 扫描器判断「这个文件不用碰」时只看「宽高非空」，而终态项的宽高**永远是 NULL**
//!    → 每次启动都重新 `probe()` 一遍，再失败一遍。
//!
//! 两处合起来就是一个死循环：启动读 903 个头 → 判 903 次失败 → 下次启动照旧。
//! 下面两条测试把它钉死：**文件没变就保住终态；文件真变了才允许重试一次**。

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

/// 「看起来像图片」的文件：收录只看扩展名，`probe()` 读不出宽高 —— 正是那 903 张的形态。
fn write_fake_jpeg(dir: &Path, name: &str, body: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), body).unwrap();
}

fn wait_idle(board: &IndexBoard, timeout: Duration) {
    let end = Instant::now() + timeout;
    while Instant::now() < end && board.snapshot().running() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!board.snapshot().running(), "扫描超时未结束");
}

/// 造一个已经扫过一遍的库，返回 (库, 扫描服务, 那张照片的 id)。
fn scanned_library(tmp: &Path, body: &[u8]) -> (Library, Arc<IndexBoard>, IndexService, i64) {
    let media = tmp.join("media");
    write_fake_jpeg(&media, "broken.jpg", body);

    let mut lib = Library::open(isolated_paths(tmp)).unwrap();
    lib.add_root(&media);

    let (tx, _rx) = channel::<CoreEvent>();
    let board = Arc::new(IndexBoard::new());
    let service = IndexService::new(lib.db.clone(), board.clone(), tx);

    service.enqueue_scan(std::slice::from_ref(&media));
    wait_idle(&board, Duration::from_secs(60));

    let rows = lib
        .db()
        .unwrap()
        .photos(None, Default::default(), 100)
        .unwrap();
    assert_eq!(rows.len(), 1, "应当入库 1 张");
    let id = rows[0].id;
    (lib, board, service, id)
}

#[test]
fn terminal_dims_failure_survives_rescan() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, board, service, id) = scanned_library(tmp.path(), b"not-a-real-jpeg");

    let db = lib.db().unwrap();
    // 刚扫完：读不到宽高，但状态是「待回填」——回填线程还有机会再试一次。
    let s = db.dims_stats().unwrap();
    assert_eq!(s.total, 1);
    assert_eq!(s.pending, 1, "刚扫完应当是待回填，而不是直接判死");
    assert_eq!(s.failed, 0);

    // 回填线程试过一次，结论是「读得到但拿不到尺寸」→ 终态。
    db.set_photo_dims(id, None, None, None, false).unwrap();
    assert_eq!(db.dims_stats().unwrap().failed, 1);
    assert_eq!(db.pending_dims_count().unwrap(), 0);
    drop(db);

    // ---- 再次启动扫描：文件一个字节都没变 ----
    let media = tmp.path().join("media");
    service.enqueue_scan(std::slice::from_ref(&media));
    wait_idle(&board, Duration::from_secs(60));

    let db = lib.db().unwrap();
    let after = db.dims_stats().unwrap();
    assert_eq!(
        after.failed, 1,
        "❌ 终态被扫描改回去了：这 903 张以后每次启动都会被重读文件头、再判一次失败"
    );
    assert_eq!(
        after.pending, 0,
        "终态项不该被打回「待回填」——那等于每个启动周期重试一次，永无终止"
    );
    assert!(db.pending_dims_count().unwrap() == 0);

    let row = db.photos(None, Default::default(), 10).unwrap().remove(0);
    assert!(
        row.width.is_none() && row.height.is_none(),
        "宽高仍应保持为空"
    );
}

#[test]
fn changed_file_gets_one_more_chance() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, board, service, id) = scanned_library(tmp.path(), b"not-a-real-jpeg");

    let db = lib.db().unwrap();
    db.set_photo_dims(id, None, None, None, false).unwrap();
    assert_eq!(db.dims_stats().unwrap().failed, 1);
    drop(db);

    // 文件被换掉了（长度不同 → 缓存键失效）：内容变了，值得再试一次。
    write_fake_jpeg(
        &tmp.path().join("media"),
        "broken.jpg",
        b"replaced-with-other-bytes",
    );

    let media = tmp.path().join("media");
    service.enqueue_scan(std::slice::from_ref(&media));
    wait_idle(&board, Duration::from_secs(60));

    let s = lib.db().unwrap().dims_stats().unwrap();
    assert_eq!(
        s.pending, 1,
        "文件变过之后应该回到待回填（允许重试一次），而不是继续沿用旧的终态"
    );
    assert_eq!(s.failed, 0, "终态应当被清掉，重新排队");
}
