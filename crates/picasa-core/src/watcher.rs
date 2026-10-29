//! 文件系统监视（F1-04：外部变更 5s 内反映）。
//!
//! macOS 的 FSEvents 语义是「可能重复、可能合并、重命名表现为 delete+create」，
//! 所以这里只做**一件事**：把变化过的目录去抖后交给 UI，由 UI 决定「重扫哪个目录」。
//! 不去解析事件类型——M1 的重扫成本很低（一个目录一次 stat 遍历），
//! 猜事件类型反而容易产生幽灵条目（01 F1 风险 2）。

use crate::scanner::CoreEvent;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// 去抖窗口：一次 Finder 操作会连发十几个事件，300ms 足够合并。
const DEBOUNCE: Duration = Duration::from_millis(300);

pub struct WatchHandle {
    _watcher: RecommendedWatcher,
    _thread: std::thread::JoinHandle<()>,
}

/// 启动监视。返回 `None` 表示监视不可用——**不是致命错误**，
/// 应用退化为「手动刷新」，不能因为监视失败就不让用。
pub fn spawn_watch(roots: Vec<PathBuf>, tx: Sender<CoreEvent>) -> Option<WatchHandle> {
    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<PathBuf>();

    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                for p in ev.paths {
                    let dir = if p.is_dir() {
                        p
                    } else {
                        p.parent().map(|d| d.to_path_buf()).unwrap_or(p)
                    };
                    let _ = raw_tx.send(dir);
                }
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e, "无法创建文件监视器，外部变更需手动刷新");
                return None;
            }
        };

    for root in &roots {
        if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
            tracing::warn!(root = %root.display(), error = %e, "无法监视该目录");
        }
    }

    let thread = std::thread::Builder::new()
        .name("op-watch".into())
        .spawn(move || {
            let mut pending: HashSet<PathBuf> = HashSet::new();
            let mut last: Option<Instant> = None;
            loop {
                match raw_rx.recv_timeout(DEBOUNCE) {
                    Ok(dir) => {
                        pending.insert(dir);
                        last = Some(Instant::now());
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if pending.is_empty() {
                            continue;
                        }
                        let quiet = last.map(|t| t.elapsed() >= DEBOUNCE).unwrap_or(true);
                        if !quiet {
                            continue;
                        }
                        let paths: Vec<String> = pending
                            .drain()
                            .map(|p| crate::paths::canonical_string(&p))
                            .collect();
                        if tx.send(CoreEvent::FsChanged { paths }).is_err() {
                            break; // UI 已退出
                        }
                        last = None;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .ok()?;

    Some(WatchHandle {
        _watcher: watcher,
        _thread: thread,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 监视在临时目录上是可用的（CI 里也应通过；失败就当「环境不支持」跳过断言的一半）。
    #[test]
    fn detects_new_file_in_watched_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let Some(_h) = spawn_watch(vec![tmp.path().to_path_buf()], tx) else {
            return; // 环境不支持监视，跳过
        };
        // 等监视器就绪
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(tmp.path().join("new.jpg"), b"x").unwrap();

        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline {
            if let Ok(CoreEvent::FsChanged { .. }) = rx.recv_timeout(Duration::from_millis(500)) {
                return;
            }
        }
        panic!("6 秒内没有收到文件变化事件");
    }
}
