//! R-1「照片目录零写入」的自动化断言（01 §11.5.3 / 03 §5C）。
//!
//! ## 这个测试在防什么
//!
//! 它是**唯一**能自动抓住「某天有人在扫描/缩略图链路里顺手写了个文件」的防线。
//! 03 §10.2 把「照片目录零写入」列为 M1 的 DoD 硬项，这里用**双侧断言**实现：
//!
//! 1. **反向**：照片目录内每一个文件的内容哈希、大小、mtime 全等 → 我们没动过它；
//! 2. **正向**：应用容器里该更新的确实更新了 → 我们不是「干脆什么都不做」。
//!
//! 只做第 1 条会让「完全不工作」也通过测试——这是 01 §11.5.3 特别提醒的坑。

use picasa_core::{AppPaths, CoreEvent, Library, spawn_scan};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Snapshot {
    files: BTreeMap<PathBuf, (u64, i64, u64)>,
}

impl Snapshot {
    fn take(root: &Path) -> Self {
        let mut files = BTreeMap::new();
        collect(root, &mut files);
        Self { files }
    }

    fn assert_unchanged(&self, other: &Snapshot) {
        assert_eq!(
            self.files.keys().collect::<Vec<_>>(),
            other.files.keys().collect::<Vec<_>>(),
            "照片目录的文件清单变了（新增或删除）——违反 R-1"
        );
        for (path, after) in &other.files {
            let before = self.files.get(path).expect("路径应存在");
            assert_eq!(before.0, after.0, "{} 的大小变了", path.display());
            assert_eq!(before.1, after.1, "{} 的 mtime 变了", path.display());
            assert_eq!(before.2, after.2, "{} 的内容哈希变了", path.display());
        }
    }
}

fn collect(dir: &Path, out: &mut BTreeMap<PathBuf, (u64, i64, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            collect(&path, out);
        } else {
            let bytes = std::fs::read(&path).unwrap_or_default();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            out.insert(path, (meta.len(), mtime, fnv(&bytes)));
        }
    }
}

/// 不引入 sha2 依赖：FNV-1a 足够抓住「内容被改了一个字节」。
fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn write_jpeg(path: &Path, w: u32, h: u32) {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 251) as u8, (y % 241) as u8, 96])
    });
    let mut buf = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new(&mut buf);
    enc.encode(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    std::fs::write(path, buf).unwrap();
}

fn isolated_paths(root: &Path) -> AppPaths {
    AppPaths {
        support: root.join("Library/Application Support/app.openpicasa"),
        preferences: root.join("Library/Preferences/OpenPicasa"),
        caches: root.join("Library/Caches/app.openpicasa"),
        logs: root.join("Library/Logs/app.openpicasa"),
    }
}

#[test]
fn scanning_and_thumbnailing_never_writes_into_photo_directory() {
    // 布局：<tmp>/container（应用容器） 与 <tmp>/photos（用户照片目录）
    let tmp = tempfile::tempdir().unwrap();
    let photos = tmp.path().join("photos");
    std::fs::create_dir_all(photos.join("2011/05")).unwrap();
    write_jpeg(&photos.join("a.jpg"), 64, 48);
    write_jpeg(&photos.join("2011/b.jpg"), 40, 30);
    write_jpeg(&photos.join("2011/05/c.jpg"), 32, 32);
    // 一个「原版 Picasa 留下的」隐藏文件：我们既不该读它，更不该改它
    std::fs::write(photos.join(".picasa.ini"), b"[a.jpg]\nstar=yes\n").unwrap();

    let before = Snapshot::take(&photos);

    let paths = isolated_paths(tmp.path());
    paths.ensure().unwrap();
    let mut lib = Library::open(paths.clone()).unwrap();
    lib.add_root(&photos);
    lib.save_prefs();

    let (tx, rx) = std::sync::mpsc::channel();
    let handle = spawn_scan(lib.db.clone(), vec![photos.clone()], tx);

    // 等扫描结束（含缩略图生成——ScanFinished 在缩略图之后发出）
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut finished = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(CoreEvent::ScanFinished { photos, .. }) => {
                assert_eq!(photos, 3, "3 张图都应被索引");
                finished = true;
                break;
            }
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    handle.stop();
    assert!(finished, "扫描未在 60 秒内结束");

    // ---- 反向断言：照片目录零变化 ----
    let after = Snapshot::take(&photos);
    before.assert_unchanged(&after);

    // ---- 正向断言：应用容器里确实有东西被写 ----
    assert!(paths.db_file().exists(), "索引库应落在 Application Support");
    let db_len = std::fs::metadata(paths.db_file()).unwrap().len();
    assert!(db_len > 0, "索引库不应为空");

    let prefs = std::fs::read_to_string(paths.preferences_file()).unwrap();
    assert!(
        prefs.contains("photos"),
        "偏好应记住加入的根目录（否则「零写入」是因为根本没工作）"
    );

    // 缩略图确实生成了（否则栅格永远是空框）
    let thumbs = {
        let db = lib.db().unwrap();
        db.photos_pending_thumbs(None, 100).unwrap().len()
    };
    assert_eq!(thumbs, 0, "扫描结束后不应还有待生成缩略图的照片");
}

#[test]
fn removing_root_keeps_files_on_disk() {
    // 「从图库移除」= 只删索引，不动磁盘（F2-08 的独立语义）
    let tmp = tempfile::tempdir().unwrap();
    let photos = tmp.path().join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    write_jpeg(&photos.join("keep.jpg"), 16, 16);

    let paths = isolated_paths(tmp.path());
    paths.ensure().unwrap();
    let mut lib = Library::open(paths).unwrap();
    lib.add_root(&photos);

    let (tx, rx) = std::sync::mpsc::channel();
    let handle = spawn_scan(lib.db.clone(), vec![photos.clone()], tx);
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(CoreEvent::ScanFinished { .. }) = rx.recv_timeout(Duration::from_secs(2)) {
            break;
        }
    }
    handle.stop();

    lib.remove_root(&photos);
    assert!(photos.join("keep.jpg").exists(), "文件必须还在磁盘上");
}
