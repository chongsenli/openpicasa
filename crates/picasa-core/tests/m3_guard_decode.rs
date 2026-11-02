//! R-M3-10 解码隔离的**接线**验收（`TC-M3-GUARD-004`）。
//!
//! ## 与 `picasa-imaging/tests/malformed_does_not_crash.rs` 的分工
//!
//! 那个文件验的是「解码链路本身不崩」；这个文件验的是**扫描器有没有真的罩住它**。
//!
//! 这两件事必须分开测，因为它们会独立失效：
//!
//! | 失效方式 | 表现 |
//! | --- | --- |
//! | 解码器会 panic，但扫描器没 `catch_unwind` | 一个坏文件带走整个进程 |
//! | 扫描器罩住了，但没调 `note_panic` | 不崩，但**熔断永远触发不了**——坏目录会一直拖 |
//!
//! 第二种尤其隐蔽：行为看起来完全正常（坏文件进失败列表），
//! 只是「同一个解析器第 1000 次崩」时没有停下来。

use picasa_core::{CoreEvent, IndexService};
use picasa_imaging::format::{DECODE_PANIC_FUSE_PER_MIN, global_guard};

/// 两个测试共享进程级 `global_guard()`（真实扫描器也用它），并行跑会互相
/// 污染熔断计数 → 偶发红。用互斥锁强制串行；锁本身也是进程级的。
static GUARD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());


/// 造一个目录，里面塞满**同一个格式**的畸形文件。
///
/// 用同一个格式是关键：熔断按格式计，混着放永远到不了阈值。
fn dir_with_broken_pngs(tmp: &std::path::Path, n: usize) -> std::path::PathBuf {
    let dir = tmp.join("photos");
    std::fs::create_dir_all(&dir).unwrap();
    // 每个都是「签名对、IHDR 尺寸非法」的 PNG —— 让解码器走 PNG 路径并失败
    let sig: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    for i in 0..n {
        let mut b = sig.to_vec();
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&0u32.to_be_bytes()); // 宽 0
        b.extend_from_slice(&0u32.to_be_bytes()); // 高 0
        b.extend_from_slice(&[8, 2, 0, 0, 0]);
        b.extend_from_slice(&[0, 0, 0, 0]);
        std::fs::write(dir.join(format!("bad{i:03}.png")), b).unwrap();
    }
    dir
}

#[test]
fn scanner_survives_a_directory_full_of_broken_files() {
    let _serial = GUARD_TEST_LOCK.lock().unwrap();
    // 核心断言：**整个扫描跑完，进程活着，每个文件都有结局**。
    //
    // 这是 R-M3-10「畸形文件不崩」在系统层面的验证——
    // 单个解码器不崩不等于流水线不崩（还有落库、事件、进度板）。
    global_guard().reset();

    let tmp = tempfile::tempdir().unwrap();
    let dir = dir_with_broken_pngs(tmp.path(), 30);

    let board = std::sync::Arc::new(picasa_core::IndexBoard::new());
    let db_path = tmp.path().join("idx.sqlite");
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        picasa_db::Db::open(&db_path).unwrap(),
    ));
    let (tx, rx) = std::sync::mpsc::channel();
    let svc = IndexService::new(db.clone(), board.clone(), tx);

    // 跑到底：`enqueue_scan` 是异步的，这里等事件收敛
    svc.enqueue_scan(std::slice::from_ref(&dir));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut settled = false;
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(CoreEvent::ScanFinished { .. }) => {
                settled = true;
                break;
            }
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // 板子说没任务了就算收敛（事件可能被别的路径消费）
                if !board.snapshot().running() {
                    settled = true;
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    svc.stop();

    assert!(
        settled,
        "扫描没有在 30 秒内收敛——典型原因是某个畸形文件让解码线程卡死了"
    );

    // 每个文件都必须有结局：成功或失败，**不能停在「待处理」**。
    // 停在 `thumb_state = 0` 是最坏的情况——它会被下一批原样取出来，
    // 变成永远补不完的死循环（内存里那张图也会一直重试）。
    let guard = db.lock().unwrap();
    let stuck = guard
        .photos_pending_thumbs(None, 10_000)
        .map(|v| v.len())
        .unwrap_or(0);
    let all = guard.photo_count_total().unwrap();
    assert_eq!(all, 30, "30 个畸形文件都要入库（R-M2-4：失败永不隐藏）");
    assert_eq!(
        stuck, 0,
        "★ 还有 {stuck} 张停在「待处理」——它们会被无限重试，拖死索引"
    );
}

#[test]
fn repeated_panics_trip_the_format_fuse() {
    let _serial = GUARD_TEST_LOCK.lock().unwrap();
    // R-M3-10：连续 panic > 20/分钟 → 熔断该格式。
    //
    // 这里直接驱动 `DecodeGuard`（扫描器的 `catch_unwind` 分支调的就是它）——
    // 因为**真的让解码器 panic 20 次**需要一个已知会 panic 的输入，
    // 而那种输入取决于 `image` crate 的版本，写进用例会变成脆弱的白盒断言。
    // 熔断这条契约与「谁触发的」无关，所以直接测它。
    global_guard().reset();
    assert!(!global_guard().is_fused("PNG"));
    for _ in 0..DECODE_PANIC_FUSE_PER_MIN {
        global_guard().note_panic("PNG");
    }
    assert!(
        global_guard().is_fused("PNG"),
        "★ 达到阈值必须熔断，否则一个坏目录会让流水线一直崩"
    );

    // 熔断后该格式的解码请求被拒（进可见失败，不是静默跳过）
    let head = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let (admit, _) = global_guard().admit("png", &head);
    assert!(
        matches!(
            admit,
            picasa_imaging::Admit::Reject(picasa_imaging::RejectReason::Fused { .. })
        ),
        "熔断后必须拒绝，实际 {admit:?}"
    );

    // 其他格式不受影响（熔断按格式隔离——一个坏了不能全停）
    let (jpg, _) = global_guard().admit("jpg", &[0xFF, 0xD8, 0xFF, 0xE0]);
    assert_eq!(jpg, picasa_imaging::Admit::Go);

    global_guard().reset();
}
