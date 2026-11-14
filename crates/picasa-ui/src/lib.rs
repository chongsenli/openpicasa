//! # picasa-ui · eframe 应用
//!
//! M3.1 §7 定稿为**三行结构**（顶栏通栏 / 中区左右分栏 / 状态栏通栏）：
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │ ① 工具栏（通栏，图标化：导入/排布/缩放/搜索） │
//! ├──────────┬───────────────────────────────────┤
//! │ 标题     │  ③ 中央栅格                       │
//! │ 张数·日期│    · 日期分组头（sticky）          │
//! │ 文件树   │    · 虚拟滚动 + 三档尺寸           │
//! │  ───────  │                                   │
//! │ ④ 托盘    │                                   │
//! │（贴底）   │                                   │
//! ├──────────┴───────────────────────────────────┤
//! │ ⑤ 状态栏（信任提示 · 计数 · 最右索引管理）    │
//! └──────────────────────────────────────────────┘
//! ```
//!
//! 文件树挂在**中区左侧、标题之下**，不是通栏通高的侧栏。

pub mod app;
pub mod atlas;
pub mod capture;
pub mod diagnostics;
pub mod grid;
pub mod icons;
pub mod import_task;
pub mod index_mgr;
pub mod index_progress;
pub mod macos;
pub mod shortcuts;
pub mod sidebar;
pub mod statusbar;
pub mod tag_counts_task;
pub mod textures;
pub mod theme;
pub mod toolbar;
pub mod topbar;
pub mod viewer;

pub use app::OpenPicasaApp;

/// 应用入口（`main.rs` 只调这个，方便以后加第二个二进制做诊断）。
pub fn run() -> eframe::Result {
    let paths = picasa_core::AppPaths::resolve();
    if let Err(e) = paths.ensure() {
        // 容器建不出来是**真致命**：没有地方放索引，只能用系统日志说明原因后退出。
        eprintln!("无法创建应用数据目录 {:?}: {e}", paths.support);
        return Ok(());
    }
    init_tracing(&paths);
    // 数据目录自检日志必须在 tracing 初始化**之后**再打一次（resolve() 里那条
    // 早于 init_tracing，只会落到 stderr）；这里让「隔离是否生效」进正式日志，
    // 便于事后核对，而不是靠猜。
    if let Some(dir) = std::env::var_os("OPENPICASA_DATA_DIR") {
        tracing::info!(
            data_dir = %std::path::PathBuf::from(&dir).display(),
            support = %paths.support.display(),
            "启动数据目录（已隔离，不会读 ~/Library 的真实图库）"
        );
    } else {
        tracing::info!(support = %paths.support.display(), "启动数据目录（默认 ~/Library）");
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // U-01：首次打开是横向偏宽、非最大化的窗口（⚠️待核对具体尺寸）
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("OpenPicasa")
            .with_app_id("app.openpicasa")
            .with_icon(window_icon())
            // M3.1：macOS「照片」风格——红绿灯嵌进自绘顶栏。
            // fullsize_content_view 让窗口内容延伸到标题栏区域；
            // 每帧用 `Frame::set_traffic_lights_position` 把三枚按钮
            // 摆在顶栏（TOPBAR_H）的垂直中线、左侧留白处。
            // titlebar 隐藏系统标题文字（macOS 上栏体自绘）。
            .with_fullsize_content_view(cfg!(target_os = "macos"))
            .with_titlebar_shown(!cfg!(target_os = "macos"))
            .with_title_shown(!cfg!(target_os = "macos")),
        // 02 §5.2.2 方案 ①：把 eframe 的 UI 状态指到 Preferences 侧，
        // 不和索引库混在同一个目录语义里。
        persistence_path: Some(paths.ui_state_file()),
        ..Default::default()
    };

    eframe::run_native(
        "OpenPicasa",
        native_options,
        Box::new(move |cc| Ok(Box::new(OpenPicasaApp::new(cc, paths)))),
    )
}

/// 窗口图标。图来自 `packaging/logo.png`，由 `packaging/make_icons.py --single`
/// 生成后提交在 `assets/logo-256.png`（已裁圆、透明底、256×256）。
///
/// 注意分工：GNOME/Wayland 下停靠栏与概览里的图标走 `.desktop` + app_id
/// （见 `packaging/make_deb.sh`），**不看**这个；这里管的是 X11/XWayland
/// 与独立窗口管理器的窗口/任务栏图标。
fn window_icon() -> egui::IconData {
    const PNG: &[u8] = include_bytes!("../assets/logo-256.png");
    // 编译期就固定的资源，解码失败说明资产文件坏了，panic 比静默无图标好排查
    let img = image::load_from_memory(PNG)
        .expect("内置图标 assets/logo-256.png 解码失败")
        .to_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }
}

fn init_tracing(paths: &picasa_core::AppPaths) {
    use tracing_subscriber::EnvFilter;
    // 默认级别按构建类型分：
    // - debug 构建：自己的 crate 开 debug（排查期要看得见布局与查询）
    // - release 构建：只开 info。栅格每帧都会打一条 debug 日志，release 下开着等于
    //   每秒几十次写盘（实测 .app 一启动日志就上千行），对普通用户毫无意义。
    let default = if cfg!(debug_assertions) {
        "info,picasa_core=debug,picasa_ui=debug"
    } else {
        "info"
    };
    // 用户可用 `OPENPICASA_LOG=debug` 覆盖（注意不是 RUST_LOG：这里显式指定了变量名，
    // 避免和用户 shell 里其他 Rust 程序的 RUST_LOG 互相干扰）。
    let filter =
        EnvFilter::try_from_env("OPENPICASA_LOG").unwrap_or_else(|_| EnvFilter::new(default));
    // 日志写应用容器（`~/Library/Logs/app.openpicasa/`），**不写照片目录**。
    match LogSink::open(&paths.logs.join("openpicasa.log")) {
        Some(sink) => {
            // 存一份给「清空运行日志」用：截断必须走**同一个已打开的句柄**，
            // 否则文件被清空后旧句柄的偏移还停在原处，下一次写会留下一个巨大的空洞。
            let _ = LOG_SINK.set(sink.clone());
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(sink)
                .try_init();
        }
        None => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        }
    }
}

/// 日志文件的唯一持有者（m10756「清空运行日志」）。
///
/// `tracing_subscriber` 的 writer 一旦装好就换不掉，所以「清空」只能
/// 在**同一个 `File` 句柄**上 `set_len(0)` + `seek(0)`。包一层 `Mutex` 既满足
/// `MakeWriter` 要求的 `Send + Sync`，也保证截断与写日志不会互相踩到偏移。
///
/// 外面再包一层 newtype 是**孤儿规则**逼的：`Arc` 和 `MakeWriter` 都不在本 crate，
/// 没法直接 `impl MakeWriter for Arc<LogWriter>`。
#[derive(Clone)]
pub struct LogSink {
    file: std::sync::Arc<std::sync::Mutex<std::fs::File>>,
}

/// 全局唯一的日志写入者；`clear_log_file()` 借它做截断。
static LOG_SINK: std::sync::OnceLock<LogSink> = std::sync::OnceLock::new();

impl LogSink {
    fn open(path: &std::path::Path) -> Option<Self> {
        let file = std::fs::File::create(path).ok()?;
        Some(Self {
            file: std::sync::Arc::new(std::sync::Mutex::new(file)),
        })
    }

    /// 截断到 0 字节并把写游标归零，返回清掉的字节数。
    fn truncate(&self) -> std::io::Result<u64> {
        let mut file = match self.file.lock() {
            Ok(f) => f,
            // 锁中毒（某次写日志时 panic）：拿回内部值继续，清空日志不该因此失败。
            Err(e) => e.into_inner(),
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        file.set_len(0)?;
        // 偏移必须一起归零：`set_len` 不动游标，否则下一次写会从旧位置开始，
        // 在文件里留下 0 填充的空洞（看起来像「清空后日志变成一坨 NUL」）。
        std::io::Seek::seek(&mut *file, std::io::SeekFrom::Start(0))?;
        Ok(len)
    }
}

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.file.lock() {
            Ok(mut f) => f.write(buf),
            // 锁中毒不能把日志吞掉也不能 panic：退化成丢这一行，日志线程继续活着。
            Err(_) => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.file.lock() {
            Ok(mut f) => f.flush(),
            Err(_) => Ok(()),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogSink {
    type Writer = LogSink;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// 清空运行日志文件（`~/Library/Logs/app.openpicasa/openpicasa.log`）。
///
/// 返回清掉的字节数。**只截断、不删除、不换文件**：删除会让已经打开的句柄指向
/// 一个没人看得到的 inode，之后所有日志都写进虚空；换文件则要重建 subscriber，
/// 运行期做不到。截断 + 归零偏移是唯一两头都对的做法。
///
/// 还没初始化日志（例如无窗口测试）时返回 `Ok(0)`，不报错。
pub fn clear_log_file() -> std::io::Result<u64> {
    // 还没初始化日志（例如无窗口测试）时返回 0，不报错。
    match LOG_SINK.get() {
        Some(sink) => sink.truncate(),
        None => Ok(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// m10756：清空运行日志必须**真的把文件截到 0**，而且要把写游标一起归零。
    ///
    /// 只断 `metadata().len() == 0` 抓不住「游标没归零」这个 bug：那是写下去之后
    /// 才暴露的——文件会从旧偏移开始写，前面留一大段 0。所以这里清空后**再写一行**
    /// 并断言文件内容就是那一行（长度等于它、开头不是 NUL）。
    #[test]
    fn clearing_the_run_log_truncates_the_file_and_resets_the_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openpicasa.log");
        let mut sink = LogSink::open(&path).expect("能创建日志文件");

        use std::io::Write as _;
        sink.write_all("旧日志一行\n".as_bytes()).unwrap();
        sink.flush().unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 0);

        let cleared = sink.truncate().unwrap();
        assert_eq!(cleared, "旧日志一行\n".len() as u64, "返回值应为清掉的字节数");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

        // 清空后继续写：内容必须紧贴文件开头，不能有空洞。
        sink.write_all("新日志一行\n".as_bytes()).unwrap();
        sink.flush().unwrap();
        let body = std::fs::read(&path).unwrap();
        assert_eq!(body, "新日志一行\n".as_bytes());
    }

    /// 日志还没初始化时清空是**无操作而不是报错**（无窗口测试/早期启动路径）。
    #[test]
    fn clearing_before_logging_is_initialised_is_a_no_op() {
        // 本测试进程可能已经初始化过（其它用例），这里只验「不会 panic、不会 Err」。
        let r = clear_log_file();
        assert!(r.is_ok(), "清空日志不该失败：{r:?}");
    }
}
