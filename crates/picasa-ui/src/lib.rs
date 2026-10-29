//! # picasa-ui · eframe 应用
//!
//! 五区结构（01 U-02 / 02 §8.1，**托盘在左栏底部**是 01 U-36 的已定结论，
//! 覆盖 02 §8.1 早期的「右栏托盘」示意）：
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │ ① 顶部工具栏（图标在上 + 文案在下 + 分组竖线） │
//! ├──────────┬───────────────────────────────────┤
//! │ ② 左栏    │  ③ 中央栅格                       │
//! │  收藏     │    · 日期分组头（sticky）          │
//! │  文件夹树 │    · 虚拟滚动 + 三档尺寸           │
//! │  ───────  │                                   │
//! │ ④ 托盘    │                                   │
//! │（贴底）   │                                   │
//! ├──────────┴───────────────────────────────────┤
//! │ ⑤ 底部状态栏                                  │
//! └──────────────────────────────────────────────┘
//! ```

pub mod app;
pub mod capture;
pub mod grid;
pub mod icons;
pub mod macos;
pub mod shortcuts;
pub mod sidebar;
pub mod statusbar;
pub mod textures;
pub mod theme;
pub mod toolbar;
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

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // U-01：首次打开是横向偏宽、非最大化的窗口（⚠️待核对具体尺寸）
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("OpenPicasa")
            .with_app_id("app.openpicasa"),
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
    let file = std::fs::File::create(paths.logs.join("openpicasa.log")).ok();
    match file {
        Some(f) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::sync::Mutex::new(f))
                .try_init();
        }
        None => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        }
    }
}
