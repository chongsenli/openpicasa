//! 应用主结构：把「库会话（core）」「纹理缓存」「后台事件」「五区界面」缝在一起。
//!
//! ## 这个文件只做编排，不做算法
//!
//! 排序/选择/扫描/解码都在 `picasa-core`；布局/绘制在 `grid`/`sidebar`/`viewer`。
//! 这里负责：**面板嵌套顺序**、**事件泵**、**动作分发**、**偏好落盘**。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, Pos2, Rect, RichText, pos2, vec2};
use picasa_core::thumbs::JobKind;
use picasa_core::{
    AppPaths, CoreEvent, DimsService, IndexBoard, IndexService, Library, ThumbJob, ThumbService,
    View,
};
use picasa_db::{PhotoRow, SortKey};

use crate::grid;
use crate::index_progress::{IndexProgressUi, ProgressAction};
use crate::shortcuts::{self, KeyAction};
use crate::sidebar::{self, SidebarAction, SidebarCtx};
use crate::statusbar;
use crate::textures::{TexState, TextureCache};
use crate::theme::{Metrics, Theme};
// M3.1：顶栏动作。旧 `ToolbarAction` 已由 `TopbarAction` 接管；
// 筛选面板单独走 `FilterPanelAction`（与顶栏枚举解耦）。
use crate::toolbar;
#[allow(unused_imports)]
use crate::toolbar::{FilterPanelAction, MoreAction, TopbarAction};
use crate::viewer::{self, Viewer, ViewerAction};

/// 索引管理页数据的刷新间隔（毫秒）。
///
/// 规格 §6.1 的上限是 4 次/秒（`picasa_core::SNAPSHOT_HZ_MAX`），这里取 1 次/秒：
/// 实测一次轻量快照在 25.9 万张库上约 70 ms，4 Hz 会把主线程三成以上的时间
/// 耗在查库上。1 Hz 同样满足「≤ 4 Hz」，且给界面留足余量。
const MGMT_REFRESH_MS: u64 = 1000;

/// 缩略图占用统计的有效期（秒）。这段时间内重开管理页直接复用缓存，不重算。
const THUMB_STATS_TTL_SECS: u64 = 30;

pub struct OpenPicasaApp {
    lib: Library,
    /// 应用容器的路径（诊断/自检用；也保证「谁能写哪儿」在 UI 层只有这一份引用）
    paths: AppPaths,
    theme: Theme,

    cache: TextureCache,
    /// 栅格布局缓存（C1）：photos 不变时布局一次都不重算
    grid_layout: grid::GridCache,
    service: ThumbService,
    rx: Receiver<CoreEvent>,
    /// 后台 → UI 的事件发送端。监视器曾经用它转发 `FsChanged`；
    /// M3 起监视走 `WatchManager` 直接投递刷新任务，但这条通道仍被扫描/解码事件使用。
    #[allow(dead_code)]
    tx: Sender<CoreEvent>,
    /// 索引服务（M2 A 线）：关注文件夹 → 任务队列 → 扫描/缩略图
    index: IndexService,
    /// 宽高回填服务（M2.1 B）：可见优先、给索引让路
    dims: DimsService,
    /// 索引进度板：**四处展示的唯一数据源**
    board: Arc<IndexBoard>,
    /// 进度弹框/面板的界面状态（含"本会话关闭过哪个批次"）
    progress: IndexProgressUi,
    /// 文件系统监视管理器（`07` §2）。每根一个 Watcher，集中管生命周期与降级状态。
    watch: picasa_core::WatchManager,
    /// 缩略图滑块状态机（`07-A` §2 ZOOM）。连续尺寸 + 回滞 + 拖动中不换纹理。
    zoom: picasa_core::ZoomState,
    /// L2 全校的周期调度（`07` §3.2 VERIFY-03）。
    full_verify: picasa_core::FullVerifyScheduler,
    /// 微缩图集（`07-A` §A-2.2）：格宽 ≤ 48 px 时把缩略图打包进 4096² 纹理。
    atlas: crate::atlas::ThumbAtlas,
    /// 当前显示器的 DPI 缩放（Retina = 2.0）。
    ///
    /// 滑块选档必须用它：**512 逻辑在 2× 屏上要 1024 物理**，
    /// 按逻辑 px 选档会让所有 Retina 机器的缩略图糊一半（`07-A` §2.2）。
    /// 每帧从 egui 读一次（多屏拖动窗口时它会变）。
    dpi_scale: f32,

    viewer: Option<Viewer>,

    // ---- 纯 UI 状态（不进偏好，除非注明）----
    search_buf: String,
    /// 顶栏搜索框是否展开（§7.2：搜索按钮紧接在 + 之后）
    search_open: bool,
    sort_menu_open: bool,
    sort_menu_pos: Pos2,
    /// 「更多（…）」弹出菜单开关（M3.1 顶栏）
    more_menu_open: bool,
    more_menu_pos: Pos2,
    /// 「筛选」弹出面板开关与打开帧（豁免逻辑同视图菜单）
    filter_menu_open: bool,
    filter_menu_frame: u64,
    filter_menu_pos: Pos2,
    /// 筛选面板的草稿状态（点「应用」才提交，避免每点一下就重查全库）
    filter_draft_tag: String,
    /// 排序菜单打开的帧号，用途同 `folder_ctx_frame`（点按钮也是 click，同一帧会被误关）
    sort_menu_frame: u64,
    /// 「…」菜单打开的帧号（同上豁免）
    more_menu_frame: u64,
    /// 左栏是否显示（顶栏侧栏开关；不进偏好——会话级）
    sidebar_visible: bool,
    /// 方格模式是否显示文件名（§7.4，默认开；会话级可选项）
    grid_show_filename: bool,
    /// 中区左侧标题/副标题的缓存（`N 张 · 拍摄日期跨度`）。
    /// 只在重载后重算，避免每帧扫一遍已加载照片。
    view_header_title: String,
    view_header_subtitle: String,
    /// 顶栏双语境路径面包屑（§7.3；`Library::view_breadcrumb` 的缓存）
    view_breadcrumb: Vec<(String, Option<i64>)>,
    /// 生成上面两行时的输入签名；不同才重算（避免每帧扫照片算日期跨度）。
    /// 尾部的面包屑末段 id 参与签名：不同文件夹同名时也能触发重算。
    view_header_key: Option<(String, usize, usize, i64, i64, Option<i64>)>,
    tray_expanded: bool,
    favorites_collapsed: bool,
    folders_collapsed: bool,
    folder_ctx: Option<(String, Pos2)>,
    /// 照片右键菜单：(photo_id, 当前是否收藏, 屏幕位置)
    photo_ctx: Option<(i64, bool, Pos2)>,
    /// 说明编辑窗口打开中（photo_id）
    caption_editor: Option<i64>,
    /// 标签编辑窗口打开中（photo_id）
    tag_editor: Option<i64>,
    /// 编辑框草稿
    caption_buf: String,
    tag_buf: String,
    /// 右键菜单是在**哪一帧**打开的。
    /// 必须记这个：右键释放的那一帧里，`secondary_clicked()` 与 `pointer.any_click()`
    /// **同时**为真（egui 的 click 就是在 release 时判定）。菜单在 `draw_library`
    /// 里被设好、在 `draw_overlays` 里画出来，紧接着同一个 `draw_overlays` 的
    /// 「点别处就关」逻辑就把它清掉了——于是菜单只活了一帧，看起来就是「弹不出来」。
    folder_ctx_frame: u64,
    /// 「底层数据变了，界面该重载」的脏标记。
    ///
    /// 不直接重载的原因：扫描落库是**每个目录一条事件**。一个装照片的移动硬盘
    /// 动辄几千个相册目录，各触发一次「重建树 + 重新查首屏」就是几百秒的 UI 卡死。
    reload_dirty: bool,
    /// 上次真正完成重载的时刻，配合 `reload_dirty` 做节流
    last_reload: Instant,
    confirm_trash: Option<usize>,
    /// 诊断页（F 加固）是否打开
    diagnostics_open: bool,
    /// 诊断页数据（打开或动作后重取，不每帧算 `SUM(LENGTH(BLOB))`）
    diag_data: crate::diagnostics::DiagData,
    /// 数据需要重取
    diag_dirty: bool,
    /// 「全量重建」是否已进入二次确认
    confirm_rebuild: bool,
    /// 「删除索引」是否已进入二次确认
    confirm_delete_index: bool,
    /// 「重建失败索引」是否已进入二次确认（`Some(n)` = 待约定的失败项数量）
    confirm_rebuild_failed: Option<u64>,
    /// §4.8 启动增量校验的执行器（首屏之后才建，跑完置 `None`）
    verify: Option<picasa_core::verify::VerifyRunner>,
    /// 启动校验是否已跑完（跑完就不再尝试重建，避免每帧重试）
    verify_done: bool,
    /// 启动校验的进度文案（状态栏 tooltip 显示；空 = 没在跑）
    verify_text: String,

    // ⚠️ M2.2 §2.6：工具栏「索引管理」二级菜单已**整体移除**，
    // 索引操作的唯一入口是状态栏那个按钮 → [`crate::index_mgr`] 的四标签页。
    /// 索引管理页（M2.2 §4.1）
    index_mgr: crate::index_mgr::IndexMgrUi,
    /// 管理页的唯一数据源（由 core 以 ≤ 4 次/秒发布，UI 只读）
    mgmt: picasa_core::MgmtSnapshot,
    /// 离线判定探针（注入到快照构建里，测试可替换；生产用真文件系统）
    offline_probe: picasa_core::OfflineProbe,
    /// `mgmt` 的下一次刷新时刻（节流到 [`MGMT_REFRESH_MS`]）
    mgmt_next_refresh: std::time::Instant,
    /// 缩略图占用统计（**后台线程算**，UI 只读缓存）。
    ///
    /// ## 为什么必须异步
    ///
    /// 它要 `SUM(LENGTH(blob))` 过一遍缩略图表。冷缓存下 1.26 GB 从 eMMC 读出是
    /// 90 秒级（改写成 `EXISTS` 后降到 ~0.9 s），**任何情况下都不该在主线程跑**——
    /// 2026-11-01 的点「索引」按钮死机就是它同步跑在了 UI 线程上。
    thumb_storage: Option<picasa_core::ThumbStorage>,
    /// 后台统计任务的收件口（`None` = 当前没有在算）
    thumb_rx: Option<Receiver<picasa_core::ThumbStorage>>,
    /// 上次发起后台统计的时刻（用于节流：不是每次开页面都重算）
    thumb_req_at: Option<std::time::Instant>,
    /// 缩略图失败项数量（角标 + 菜单文案）。
    ///
    /// **必须缓存**：取它的 `library_thumb_stats()` 是全表扫描，25.9 万张时要几十毫秒，
    /// 每帧算一次等于把界面按在地上。只在「启动 / 打开菜单 / 一个批次结束」时重取。
    failed_thumbs: u64,
    /// 下一次重载**不等节流**立刻做（扫描结束/删索引这类终态必须马上落屏）
    reload_now: bool,

    /// 需要恢复的滚动位置（只在必要时设置一次，否则会锁死滚动）
    restore_scroll: Option<f32>,
    /// 上一帧视口顶部的照片 id（D 流式展示的「锚点」）。
    anchor_id: Option<i64>,
    /// 上一帧视口顶部照片在 `lib.photos` 里的下标（图集装载的中心点）。
    ///
    /// 为什么不用 `anchor_id` 反查下标：那要每帧 `iter().position()`，
    /// 25 万张的列表就是线性扫。栅格每帧本来就产出 `top_index`，顺手存下来。
    viewport_top_index: usize,
    /// 下一次重载后要滚回哪张（重载会重建列表，用户不该被弹回顶部）
    pending_anchor: Option<i64>,
    tray_rows: Vec<PhotoRow>,
    tray_rows_dirty: bool,

    last_autosave: f64,

    /// 自截图（只在设了 `OPENPICASA_SCREENSHOT` 时存在），见 `capture.rs`
    capture: Option<crate::capture::Capture>,

    /// 启动后自动打开查看器的第 N 张（`OPENPICASA_OPEN_INDEX`）。
    /// 存在的唯一理由是：查看器只能靠双击进入，而自动化截图无法点击，
    /// 于是「查看器的界面还原」就永远没有基线。这是**只读的调试入口**，不影响正常使用。
    open_initial: Option<usize>,
    /// 已绘制的帧数。调试钩子用它等「扫描落定」再动作
    /// （照片是逐目录入库的，太早打开会拿到半截列表）。
    frames: u32,
}

impl OpenPicasaApp {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: AppPaths) -> Self {
        crate::theme::apply(&cc.egui_ctx);

        let lib = match Library::open(paths.clone()) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, "打开索引库失败，退化到内存库（本次会话不持久化）");
                // 退化路径：至少让用户能看照片，而不是看到一个崩溃对话框（R-3）
                Library::open_in_memory(paths.clone()).expect("内存库也打不开")
            }
        };

        let (tx, rx) = channel();
        let service = ThumbService::new(tx.clone());
        let search_buf = lib.search.clone();
        let restore_scroll = if lib.prefs.grid_scroll > 0.5 {
            Some(lib.prefs.grid_scroll)
        } else {
            None
        };

        let board = Arc::new(IndexBoard::new());
        let index = IndexService::new(lib.db.clone(), board.clone(), tx.clone());
        // 回填和索引共用一把 DB 锁与同一块进度板：它自己会看板子决定要不要让路
        let dims = DimsService::new(lib.db.clone(), board.clone());

        // 读滑块设置要在 `lib` 被移进结构体之前（它是全局一份，不随视图变）
        let zoom_setting = load_zoom_setting(&lib);

        let mut app = Self {
            lib,
            paths,
            theme: Theme::picasa(),
            cache: TextureCache::new(),
            grid_layout: grid::GridCache::new(),
            service,
            rx,
            tx,
            index,
            dims,
            board,
            progress: IndexProgressUi::new(),
            watch: picasa_core::WatchManager::new(),
            // `settings.grid_cell_px`（`07-A` ZOOM-06）：全局一份，不按文件夹记忆（P-28）。
            // 读库失败/没设过 → 默认刻度（`ZoomState::from_setting(None)`）。
            zoom: picasa_core::ZoomState::from_setting(zoom_setting),
            full_verify: picasa_core::FullVerifyScheduler::new(),
            atlas: crate::atlas::ThumbAtlas::new(),
            dpi_scale: 1.0,
            viewer: None,
            search_buf,
            search_open: false,
            sort_menu_open: false,
            sort_menu_pos: Pos2::ZERO,
            more_menu_open: false,
            more_menu_pos: Pos2::ZERO,
            more_menu_frame: u64::MAX,
            filter_menu_open: false,
            filter_menu_frame: u64::MAX,
            filter_menu_pos: Pos2::ZERO,
            filter_draft_tag: String::new(),
            sort_menu_frame: u64::MAX,
            sidebar_visible: true,
            grid_show_filename: true,
            view_header_title: String::new(),
            view_header_subtitle: String::new(),
            view_breadcrumb: Vec::new(),
            view_header_key: None,
            tray_expanded: false,
            favorites_collapsed: false,
            folders_collapsed: false,
            folder_ctx: None,
            photo_ctx: None,
            caption_editor: None,
            tag_editor: None,
            caption_buf: String::new(),
            tag_buf: String::new(),
            folder_ctx_frame: u64::MAX,
            reload_dirty: false,
            last_reload: Instant::now(),
            confirm_trash: None,
            diagnostics_open: false,
            diag_data: crate::diagnostics::DiagData::default(),
            diag_dirty: false,
            confirm_rebuild: false,
            confirm_delete_index: false,
            confirm_rebuild_failed: None,
            index_mgr: crate::index_mgr::IndexMgrUi::new(),
            mgmt: picasa_core::MgmtSnapshot::default(),
            offline_probe: picasa_core::default_offline_probe(),
            mgmt_next_refresh: std::time::Instant::now(),
            thumb_storage: None,
            thumb_rx: None,
            thumb_req_at: None,
            verify: None,
            verify_done: false,
            verify_text: String::new(),
            failed_thumbs: 0,
            reload_now: false,
            restore_scroll,
            anchor_id: None,
            viewport_top_index: 0,
            pending_anchor: None,
            tray_rows: Vec::new(),
            tray_rows_dirty: true,
            last_autosave: 0.0,
            capture: crate::capture::Capture::from_env(),
            open_initial: std::env::var("OPENPICASA_OPEN_INDEX")
                .ok()
                .and_then(|s| s.parse::<usize>().ok()),
            frames: 0,
        };
        app.refresh_tray_rows();
        app.refresh_view_header();
        // 角标要在**第一帧**就有数：用户打开程序的第一眼就该知道"有 970 张坏图"，
        // 而不是等他点开菜单才去算。
        app.refresh_failed_thumbs();
        // ⚠️ 诊断开关：`OPENPICASA_NO_STARTUP_SCAN=1` 时**启动不扫盘**。
        //
        // 存在的理由：25.9 万张 / 4330 目录 / exFAT USB 的库上，启动扫描要跑十几分钟，
        // 期间它与 UI 抢同一把 db 锁。要判断「界面卡是扫描造成的还是别的」，
        // 最快的办法就是让它别扫——**这是个诊断开关，不是产品的默认行为**。
        // 设了它，界面照常显示库里已有的内容，只是不发现新照片。
        if std::env::var("OPENPICASA_NO_STARTUP_SCAN").as_deref() == Ok("1") {
            app.lib.toast = Some((
                "诊断模式：本次启动未扫描磁盘（OPENPICASA_NO_STARTUP_SCAN=1）".into(),
                6.0,
            ));
            tracing::info!("启动扫描已按环境变量跳过");
        } else {
            app.start_index();
        }

        // M4：解码器升级 / 历史欠账的自动补全（一次性检查，同步做）。
        //
        // 为什么不在 UI tick 里：egui 界面静止时**不重绘**（没有输入、没有
        // request_repaint 就没有新帧），tick 永远不跑——实测 30 秒后 frames 还停
        // 在个位数。这里的检查是单条 EXISTS（毫秒级）+ 入队（微秒级），放 new()
        // 毫无压力；真正的补图在扫描线程里跑，不阻塞启动。
        //
        // 放在 start_index() **之后**：重试任务排在启动扫描后面，不会抢锁。
        {
            // ⚠️ 先把需要的数据全部取出、**释放 DB 锁**，再 enqueue——
            // `enqueue_retry` 内部会再拿同一把锁（`inner.db`），在循环里持有外层
            // guard 调它 = std Mutex 不可重入 = 死锁（实测：主线程卡在启动回调，
            // 所有 thumb worker 等锁，缩略图永远不动）。
            let stale = app
                .lib
                .db()
                .ok()
                .and_then(|db| {
                    db.has_rows_with_decoder_ver_before(picasa_imaging::DECODER_VER)
                        .ok()
                })
                .unwrap_or(false);
            if stale {
                let roots: Vec<(i64, std::path::PathBuf)> = app
                    .lib
                    .db()
                    .ok()
                    .and_then(|db| db.roots(false).ok())
                    .map(|v| {
                        v.into_iter()
                            .map(|r| (r.id, std::path::PathBuf::from(r.path)))
                            .collect()
                    })
                    .unwrap_or_default();
                for (id, path) in &roots {
                    app.index.enqueue_retry(*id, path);
                }
                tracing::info!(
                    roots = roots.len(),
                    "解码器升级/欠账：已对全部根触发自动补全"
                );
            }
        }
        app
    }

    /// 重取「缩略图失败项」数量（全表扫描，**只在必要时调**）。
    fn refresh_failed_thumbs(&mut self) {
        if let Ok(db) = self.lib.db()
            && let Ok(s) = db.library_thumb_stats()
        {
            self.failed_thumbs = s.failed;
        }
    }

    /// 重算 [`picasa_core::MgmtSnapshot`]（索引管理页的唯一数据源）。
    ///
    /// ⚠️ 这是一次十几个查询的批量读，**只能**以 ≤ [`picasa_core::SNAPSHOT_HZ_MAX`]
    /// 次/秒调用，且只在页面打开时调（R-M2-3：UI 线程不许干重活）。
    ///
    /// ## 刷新间隔为什么是 1 Hz 而不是规格允许的上限 4 Hz
    ///
    /// 实测（25.9 万张 / 1.4 GB 库）：一次轻量快照 ≈ 70 ms 热缓存、≈ 0.5 s 冷缓存。
    /// 4 Hz 刷新等于把主线程 30%~100% 的时间花在查库上，在 2 核 5 W 的机器上就是卡。
    /// 规格 §6.1 写的是「≤ 4 次/秒」，1 Hz 同样满足，且给界面留足余量。
    fn refresh_mgmt(&mut self) {
        self.mgmt_next_refresh =
            std::time::Instant::now() + std::time::Duration::from_millis(MGMT_REFRESH_MS);

        let live = picasa_core::live_task_views(&self.progress.snapshot().clone());
        // 监视状态是**运行时**的，从管理器取，不从库取（`07` §2.3）
        let watch = self.watch.states();
        let input = picasa_core::SnapshotInput {
            seq: self.mgmt.seq + 1,
            live: &live,
            paused_by_system: self.index.is_paused(),
            offline: &self.offline_probe,
            watch: &watch,
            prev_dataset_version: self.mgmt.dataset_version,
            prev_photo_count: self.mgmt.storage.photo_count,
        };
        if let Ok(db) = self.lib.db() {
            match picasa_core::build_snapshot(&db, &input) {
                Ok(snap) => {
                    self.mgmt = snap;
                    // 快照本身不带 BLOB 统计（那部分慢），把后台算好的那份贴回去，
                    // 否则每次刷新都会把「缩略图占用」清空成 0。
                    if let Some(st) = &self.thumb_storage {
                        picasa_core::apply_storage(&mut self.mgmt, st);
                    }
                }
                Err(e) => tracing::warn!(error = %e, "索引管理快照刷新失败"),
            }
        }
    }

    /// 收后台线程的缩略图统计（有结果就贴回当前快照）。每帧调，开销是一次 `try_recv`。
    fn poll_thumb_storage(&mut self) {
        let Some(rx) = self.thumb_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(st) => {
                self.thumb_rx = None;
                // 不覆盖诊断同源的库大小 / 照片数（`apply_storage` 只动缩略图字段）。
                picasa_core::apply_storage(&mut self.mgmt, &st);
                self.thumb_storage = Some(st);
            }
            Err(TryRecvError::Empty) => {}
            // 后台拿不到锁就退出、又不发结果时，通道会断开。
            // 以前把断开也当成「还在算」，`thumb_rx` 永远清不掉，「计算中…」就停住了。
            Err(TryRecvError::Disconnected) => {
                self.thumb_rx = None;
                if self.thumb_storage.is_none() {
                    self.thumb_req_at = None;
                }
            }
        }
    }

    /// 发起一次后台缩略图统计。
    ///
    /// - 已在算（`thumb_rx` 有值）→ 直接返回，绝不并发两条；
    /// - `force = false` 且上次发起未超 [`THUMB_STATS_TTL_SECS`] → 复用缓存。
    ///
    /// ⚠️ 后台线程会锁住 `Db`，所以**一次**统计要尽量短——这也是 `collect_storage`
    /// 里坚持用 `EXISTS` 写法的原因（112 s → 0.28 s）。
    fn request_thumb_storage(&mut self, force: bool) {
        self.poll_thumb_storage();
        if self.thumb_rx.is_some() {
            return;
        }
        let root_ids: Vec<i64> = self.mgmt.roots.iter().map(|r| r.id).collect();
        if root_ids.is_empty() {
            return;
        }
        // 只要「按根分布」还是空的，就不能拿 TTL 复用缓存——那会把空行钉住 30 秒。
        let needs_root_breakdown = self
            .thumb_storage
            .as_ref()
            .is_none_or(|st| st.by_root.is_empty());
        if !force
            && !needs_root_breakdown
            && let Some(at) = self.thumb_req_at
            && at.elapsed() < Duration::from_secs(THUMB_STATS_TTL_SECS)
        {
            return;
        }
        let db = Arc::clone(&self.lib.db);
        let (tx, rx) = channel();
        self.thumb_rx = Some(rx);
        self.thumb_req_at = Some(Instant::now());
        std::thread::spawn(move || {
            // ⚠️ **每个根一次短锁，算完立刻放**。
            // 整段只拿一次锁的话，实测会连着占用 2.2 s（3 个根），把 UI 自己的库访问
            // 全堵住——那就只是把死机从 UI 线程挪到了后台，照样卡。
            // 逐根 `try_lock`，拿不到就等 25ms 再试。一直拿不到就退出且不 `send`，
            // UI 把断开的通道清掉后再发起，不会把「计算中…」卡死。
            let mut by_root = Vec::with_capacity(root_ids.len());
            // 守卫不能逃出闭包（生命周期对不上），所以查询在锁内做完再把结果交出来。
            fn with_db<T>(
                db: &Arc<std::sync::Mutex<picasa_db::Db>>,
                f: impl FnOnce(&picasa_db::Db) -> T,
            ) -> Option<T> {
                for _ in 0..40 {
                    match db.try_lock() {
                        Ok(guard) => return Some(f(&guard)),
                        Err(std::sync::TryLockError::WouldBlock) => {
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        Err(std::sync::TryLockError::Poisoned(p)) => {
                            return Some(f(&p.into_inner()));
                        }
                    }
                }
                None
            }
            for rid in &root_ids {
                let Some(bytes) = with_db(&db, |d| {
                    d.root_thumb_bytes(std::slice::from_ref(rid))
                        .ok()
                        .and_then(|m| m.get(rid).copied())
                        .unwrap_or(0)
                }) else {
                    return;
                };
                by_root.push((*rid, bytes));
            }
            let Some((thumb_count, thumb_bytes)) =
                with_db(&db, |d| d.thumb_cache_stats().ok().unwrap_or((0, 0)))
            else {
                return;
            };
            let _ = tx.send(picasa_core::ThumbStorage {
                thumb_count,
                thumb_bytes,
                by_root,
            });
        });
    }

    /// 缩略图占用发生变化（清理缩略图 / VACUUM / 重建）后调用：缓存作废，下次强制重算。
    fn invalidate_thumb_storage(&mut self) {
        self.thumb_storage = None;
        self.thumb_req_at = None;
        self.mgmt.storage.thumb_stats_known = false;
    }

    /// 存储页还是开窗时的全 0 / 「计算中…」，需要用诊断页同一条查询补上。
    fn storage_needs_diag_sync(&self) -> bool {
        let s = &self.mgmt.storage;
        if !s.thumb_stats_known {
            return true;
        }
        // 库里已经有照片，快照却还是默认的 0 字节 + 0 张。
        self.lib.photos_total > 0 && s.photo_count == 0 && s.db_bytes == 0
    }

    /// 把「存储与诊断」对齐到诊断页的 [`picasa_db::Db::cache_stats`]。
    ///
    /// 管理快照的库大小 / 照片数要等 `refresh_mgmt`，缩略图字节还要再等后台线程。
    /// 状态栏点「索引」的那一帧窗口已经画出来了，而 egui 没新输入就不会再出一帧，
    /// 所以全 0 和「计算中…」会停在屏幕上。这里在数字还没对齐时同步读一次诊断用的那条查询。
    fn ensure_index_storage(&mut self) {
        if !self.storage_needs_diag_sync() {
            return;
        }
        let Ok(db) = self.lib.db() else {
            return;
        };
        let Ok(stats) = db.cache_stats() else {
            return;
        };
        let wal = db.wal_bytes();
        // 小库顺手把按根分布也算了（9 张是毫秒级）。大库留给后台线程，避免开窗卡住。
        let modest = stats.photo_count <= 20_000;
        let root_rows = if modest {
            db.root_stats().unwrap_or_default()
        } else {
            Vec::new()
        };
        let bytes_map = if modest && !root_rows.is_empty() && root_rows.len() <= 8 {
            let ids: Vec<i64> = root_rows.iter().map(|r| r.id).collect();
            db.root_thumb_bytes(&ids).unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };
        drop(db);

        picasa_core::apply_diag_cache(&mut self.mgmt, &stats, wal);
        if bytes_map.is_empty() {
            let mut st = self.thumb_storage.clone().unwrap_or_default();
            st.thumb_count = stats.thumb_count;
            st.thumb_bytes = stats.thumb_bytes;
            self.thumb_storage = Some(st);
            // ⚠️ B4′：这里**不能**把 `thumb_req_at` 设成 now。
            // 这条分支意味着「按根分布还没算」，接下来 `request_thumb_storage`
            // 必须能真的发起后台统计；若把它当成「刚算过」，30 秒 TTL 会把
            // 「按根目录分布」压成空行（历史 bug）。缩略图总数已是快数，无需缓存。
            self.thumb_req_at = None;
            return;
        }
        let st = picasa_core::ThumbStorage {
            thumb_count: stats.thumb_count,
            thumb_bytes: stats.thumb_bytes,
            by_root: root_rows
                .iter()
                .map(|r| (r.id, bytes_map.get(&r.id).copied().unwrap_or(0)))
                .collect(),
        };
        if !self.mgmt.roots.is_empty() {
            picasa_core::apply_storage(&mut self.mgmt, &st);
            // apply_storage 不动库大小和照片数；再写一次，跟诊断保持同一组数。
            picasa_core::apply_diag_cache(&mut self.mgmt, &stats, wal);
        } else {
            self.mgmt.storage.by_root = root_rows
                .iter()
                .map(|r| (r.path.clone(), bytes_map.get(&r.id).copied().unwrap_or(0)))
                .collect();
        }
        self.thumb_storage = Some(st);
        self.thumb_req_at = Some(Instant::now());
    }

    /// §4.8 启动增量校验：首屏出图之后才启动，然后每帧推进一步。
    ///
    /// 为什么放在这里而不是启动时：校验要 `read_dir` 4330 个目录，
    /// 虽然每目录只有几十微秒，但**不能在首屏之前**抢 IO——用户先看见照片更重要。
    /// 执行器内部按 `VERIFY_DIR_INTERVAL_MS`（50ms/目录）自己限速，这里不用 sleep。
    /// 解码器版本升级后的自动补全（M4）。
    ///
    /// `DECODER_VER` bump 的语义是「像素输出变了 / 能力变了」——本版新增
    /// HEIC/AVIF/RAW 解码，升级前的失败项与旧缓存都必须重做。靠 L1 快筛
    /// 发现不了它们（dir_mtime 没变），所以启动后主动查一次库，把有旧版本
    /// 行的根走一遍「重试」（与用户点「重试失败项」同一条路径，有进度、可暂停）。
    fn tick_startup_verify(&mut self, ctx: &egui::Context) {
        if self.verify.is_none() && !self.verify_done && self.frames > 30 {
            let paths: Vec<std::path::PathBuf> = {
                let Ok(db) = self.lib.db() else {
                    return;
                };
                db.roots(false)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| std::path::PathBuf::from(r.path))
                    .collect()
            };
            let plan = picasa_core::VerifyPlan::from_root_paths(paths);
            if plan.is_empty() {
                // 没有活跃根（全关或全离线）：本轮无事可做，不再反复尝试
                self.verify_done = true;
            } else {
                self.verify = Some(picasa_core::verify::VerifyRunner::new(plan));
            }
        }

        let Some(runner) = self.verify.as_mut() else {
            return;
        };
        let tick = {
            let Ok(db) = self.lib.db() else {
                return;
            };
            runner.tick(&db)
        };
        self.verify_text = runner.progress().text();

        if tick == picasa_core::verify::VerifyTick::Finished {
            let changed = runner.take_changed();
            let n = changed.len();
            for (_folder_id, path) in changed {
                self.index
                    .enqueue_refresh(std::path::Path::new(path.as_str()));
            }
            if n > 0 {
                tracing::info!(dirs = n, "启动校验发现变化，已排队重新扫描");
                self.lib.toast = Some((format!("启动校验：{n} 个目录有变化，正在更新索引"), 3.0));
            }
            self.verify = None;
            self.verify_done = true;
            self.verify_text.clear();
            ctx.request_repaint();
        }
    }

    /// 把「本帧视口里有哪些照片、离屏多远」告诉纹理账本（`07` §6.1 PERF-08）。
    ///
    /// 屏距按**行**折算：一屏 ≈ `ROWS_PER_SCREEN_ESTIMATE` 行，
    /// 于是「离视口 180 行」≈ 3 屏，正好落在淘汰线附近。
    /// 用行数而不是像素，是因为行数在滚动时是整数、不会逐帧抖动。
    fn note_texture_viewport(&mut self, out: &crate::grid::GridOutput) {
        use picasa_core::datasource::ROWS_PER_SCREEN_ESTIMATE;
        let first = out.top_index;
        let last = first.saturating_add(out.visible);
        let rows_per_screen = ROWS_PER_SCREEN_ESTIMATE.max(1);

        // 只登记「已经在账本里」的那些（`note_visible` 对未登记的 id 是空操作），
        // 所以这里可以放心把视口前后各 3 屏都列进去。
        let from = first.saturating_sub(rows_per_screen * 3);
        let to = last.saturating_add(rows_per_screen * 3);
        let distances: Vec<(i64, u32)> = self
            .lib
            .photos
            .iter()
            .enumerate()
            .skip(from)
            .take(to.saturating_sub(from))
            .map(|(i, p)| {
                let d = if i < first {
                    (first - i) / rows_per_screen
                } else if i >= last {
                    (i - last) / rows_per_screen
                } else {
                    0
                };
                (p.id, d as u32)
            })
            .collect();
        self.cache.note_viewport(distances.iter().copied());
        // ⚠️ 图集必须收到同一份屏距：它是按「整页里最近的那张」判定淘汰的。
        // 早先漏了这条接线，每页 min_distance 恒为 u32::MAX（= 本帧没被看到），
        // 帧末 evict_pages_farther_than(3) 就把**所有图集页**整页删光——
        // 表现为滑块拖进 ≤48px 档后预览图全部消失（只剩灰占位）。
        self.atlas.note_viewport(distances.iter().copied());
    }

    /// L2 全校的周期检查（`07` §3.2 VERIFY-03）。
    ///
    /// ## 为什么单独一个函数而不是塞进 `tick_startup_verify`
    ///
    /// 两件事的**触发条件完全不同**：启动校验只跑一次、必须等首屏；
    /// L2 每 6/24 小时一次、必须等界面空闲。混在一起写，两个条件会互相污染
    /// （比如「启动校验跑完」被误当成「L2 也跑过了」）。
    ///
    /// ## 空闲判据
    ///
    /// `idle = 没有索引任务在跑 && 启动校验已结束 && 用户没在拖滑块`。
    /// 前两条是硬要求（抢 IO）；第三条是体验（拖滑块时每帧都在重排，
    /// 再叠一个 stat 循环会让手感变涩）。
    fn tick_full_verify(&mut self, ctx: &egui::Context) {
        // 启动校验还没跑完 → 先让它跑完
        if self.verify.is_some() || !self.verify_done {
            return;
        }
        let idle = !self.board.snapshot().running() && !self.zoom.is_dragging();
        let watch_active = self.watch.active_count() > 0;
        let running = false; // L2 复用 VerifyRunner，同一时刻只会有一个（由 verify 字段管）
        if !self.full_verify.is_due(watch_active, idle, running) {
            return;
        }
        // 到点了：取这一批目录跑一轮强制 L2
        let interval_h = picasa_core::full_interval_hours(watch_active);
        let batch = {
            let Ok(db) = self.lib.db() else {
                return;
            };
            match picasa_core::FullVerifyScheduler::take_batch(&db, interval_h) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(error = %e, "取 L2 待校验目录失败");
                    return;
                }
            }
        };
        if batch.is_empty() {
            // 全都校验过了：记一次「跑过」，下一轮等周期
            self.full_verify.note_started(watch_active);
            return;
        }
        tracing::info!(
            dirs = batch.len(),
            interval_h,
            watch_active,
            "开始一轮 L2 全校校验"
        );
        // 复用启动校验的执行器：它本来就是「按目录限速、逐个比对」那一套。
        // 区别只是范围（这里是「最久没校验的 200 个」，不是全树）。
        let plan = picasa_core::VerifyPlan::from_root_paths(
            batch
                .iter()
                .map(|(_, p)| std::path::PathBuf::from(p))
                .collect::<Vec<_>>(),
        );
        if !plan.is_empty() {
            self.verify = Some(picasa_core::verify::VerifyRunner::new_forced_full(plan));
            self.verify_done = false;
            self.full_verify.note_started(watch_active);
            self.full_verify.note_batch(batch.len());
            ctx.request_repaint();
        }
    }

    /// 索引管理页点出的动作。
    ///
    /// 本方法**只调已有能力**，不在这里写 SQL——与 `diagnostics` 一样，
    /// 「谁写库」仍然只有 `Library` / `Db` 一层。
    fn handle_mgr_actions(&mut self, actions: Vec<crate::index_mgr::MgrAction>) {
        use crate::index_mgr::MgrAction;
        for a in actions {
            match a {
                // ⚠️ 已知缺口：M2.1 只有「整体暂停/继续」，没有按任务取消。
                // 单任务取消要等流水线（M2.2 迭代 2 / M3）把任务身份落到板上。
                // 这里**不做**「点了取消其实只暂停全部」那种骗人的映射，
                // 而是暂停并如实说明。
                MgrAction::CancelTask(_) => {
                    self.index.pause();
                    self.lib.toast = Some((
                        "已暂停索引（单任务取消随流水线落地，当前只能整体暂停）".into(),
                        4.0,
                    ));
                }
                MgrAction::RetryKind(kind) => self.retry_failure_kind(kind),
                MgrAction::IgnoreKind(kind) => self.set_failure_kind_ignored(kind, true),
                MgrAction::UnignoreKind(kind) => self.set_failure_kind_ignored(kind, false),
                MgrAction::RefreshRoot(id) | MgrAction::ReindexRoot(id) => self.reindex_root(id),
                MgrAction::RebuildRoot(id) => self.rebuild_root(id),
                MgrAction::CloseRoot(id) => self.set_root_closed(id, true),
                MgrAction::ReopenRoot(id) => self.set_root_closed(id, false),
                MgrAction::RevealRoot(id) => self.reveal_root(id),
                MgrAction::DeleteIndexRoot(_) => self.confirm_delete_index = true,
                MgrAction::RestoreExclusion(id) => self.restore_exclusion(id),
                MgrAction::PruneStaleExclusions => self.prune_stale_exclusions(),
                MgrAction::ClearThumbs => {
                    self.clear_thumbs();
                    self.invalidate_thumb_storage();
                }
                MgrAction::Vacuum => {
                    self.vacuum();
                    self.invalidate_thumb_storage();
                }
                // 日志导出与帧直方图：本版接线到位，实现随 §4.9 / PERF-18 的工具落地
                MgrAction::ExportDiagLog => {
                    self.lib.toast =
                        Some(("诊断日志导出（§4.9）待接线：需要系统保存对话框".into(), 3.0));
                }
                MgrAction::ToggleFrameStats(on) => {
                    self.lib.toast = Some((
                        if on {
                            "帧时间直方图已开启（⌥+点击状态栏入口查看）".to_string()
                        } else {
                            "帧时间直方图已关闭".to_string()
                        },
                        2.5,
                    ));
                }
            }
        }
    }

    /// 重试某一类失败项（**按 id 点名**，§4.3 的边界要求：不走整目录重建）。
    fn retry_failure_kind(&mut self, kind: picasa_db::FailureKind) {
        let ids = match self.lib.db() {
            Ok(db) => db.photo_ids_of_failure_kind(kind).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        if ids.is_empty() {
            self.lib.toast = Some((format!("没有可重试的「{}」项", kind.label()), 2.5));
            return;
        }
        let n = ids.len();
        if let Ok(db) = self.lib.db() {
            let _ = db.reset_failed_thumbs_by_ids(&ids);
        }
        if let Some(root) = self.lib.scan_roots().first().cloned() {
            self.index.enqueue_rebuild_failed(ids, &root);
        }
        self.lib.toast = Some((format!("已重新入队 {n} 张（{}）", kind.label()), 3.0));
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    /// 忽略 / 取消忽略某一类失败项。**忽略后仍在列表可见**（R-M2-4）。
    fn set_failure_kind_ignored(&mut self, kind: picasa_db::FailureKind, ignored: bool) {
        // ⚠️ `db()` 借的是 `self.lib`（MutexGuard），借用期间不能再写 `self.lib.toast`。
        // 所以先把结果算成字符串，锁放掉之后才落地提示。
        let msg = match self.lib.db() {
            Ok(db) => {
                let n = if ignored {
                    db.ignore_failures_of_kind(kind)
                } else {
                    db.unignore_failures_of_kind(kind)
                };
                match n {
                    Ok(k) => format!(
                        "已{} {} 条「{}」失败项（列表仍然可见）",
                        if ignored { "忽略" } else { "取消忽略" },
                        k,
                        kind.label()
                    ),
                    Err(_) => "操作失败".to_string(),
                }
            }
            Err(e) => format!("操作失败：{e}"),
        };
        self.lib.toast = Some((msg, 3.0));
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    fn reindex_root(&mut self, id: i64) {
        let Some(path) = self.mgmt.root_by_id(id).map(|r| r.path.clone()) else {
            return;
        };
        self.index.enqueue_refresh(std::path::Path::new(&path));
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    /// 全量重建（§4.2 M22-IDX-06）：**不先清再建**，旧缩略图保持可见直到被替换。
    fn rebuild_root(&mut self, id: i64) {
        let Some(root) = self.mgmt.root_by_id(id).map(|r| r.path.clone()) else {
            return;
        };
        let path = std::path::PathBuf::from(&root);
        if let Ok(db) = self.lib.db()
            && let Ok(Some(fid)) = db.folder_id(&root)
        {
            // 只把缩略图状态重置为「待生成」，**不删已有 BLOB**——
            // 这样重建期间用户看到的还是旧图，不会满屏灰格。
            let _ = db.reset_failed_thumbs_subtree(fid);
        }
        self.index.enqueue_scan(std::slice::from_ref(&path));
        self.lib.toast = Some((
            format!("已对 {root} 开始全量重建（旧缩略图保留直到替换）"),
            3.0,
        ));
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    fn set_root_closed(&mut self, id: i64, closed: bool) {
        let Some(path) = self.mgmt.root_by_id(id).map(|r| r.path.clone()) else {
            return;
        };
        let p = std::path::Path::new(&path);
        if closed {
            self.lib.close_root(p);
        } else {
            self.lib.add_root(p);
        }
        self.reload_now = true;
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    fn reveal_root(&mut self, id: i64) {
        let Some(path) = self.mgmt.root_by_id(id).map(|r| r.path.clone()) else {
            return;
        };
        if let Err(e) = crate::macos::reveal_in_finder(std::path::Path::new(&path)) {
            self.lib.toast = Some((e, 3.0));
        }
    }

    fn restore_exclusion(&mut self, id: i64) {
        match self.lib.restore_exclusion(id) {
            Ok(Some(p)) => {
                self.lib.toast = Some((format!("已恢复 {p}"), 2.5));
                self.reload_now = true;
            }
            Ok(None) => {}
            Err(e) => self.lib.toast = Some((format!("恢复失败：{e}"), 3.0)),
        }
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    /// 清理全部「路径已不存在」的排除项（§4.1 第三行）。
    fn prune_stale_exclusions(&mut self) {
        let stale: Vec<i64> = self
            .mgmt
            .exclusions
            .iter()
            .filter(|e| e.path_missing)
            .map(|e| e.id)
            .collect();
        let n = stale.len();
        for id in stale {
            if let Ok(db) = self.lib.db() {
                let _ = db.remove_exclusion(id);
            }
        }
        self.lib.toast = Some((format!("已清理 {n} 条失效排除项"), 2.5));
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    fn clear_thumbs(&mut self) {
        let (msg, ok) = match self.lib.db() {
            Ok(db) => match db.clear_all_thumbs() {
                Ok(n) => (format!("已清理 {n} 条缩略图"), true),
                Err(e) => (format!("清理失败：{e}"), false),
            },
            Err(e) => (format!("清理失败：{e}"), false),
        };
        if ok {
            self.reload_now = true;
        }
        self.lib.toast = Some((msg, 3.0));
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    fn vacuum(&mut self) {
        let msg = match self.lib.db() {
            Ok(db) => match db.vacuum() {
                Ok(()) => "已完成 VACUUM".to_string(),
                Err(e) => format!("VACUUM 失败：{e}"),
            },
            Err(e) => format!("VACUUM 失败：{e}"),
        };
        self.lib.toast = Some((msg, 3.0));
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    /// 启动时的索引：把每个关注文件夹作为一个任务入队（**同一批次**）。
    ///
    /// 这同时兑现 `04` §1.2.1 的「应用重启后发现上次未完成的任务 → 新批次、必弹」：
    /// 上次没做完的缩略图（`thumb_state = 0`）在这里自然被补齐。
    /// 如果一切都已就绪，整批会在 1.5 s 内结束 —— 于是弹框根本不出现（P-05）。
    fn start_index(&mut self) {
        let roots = self.lib.scan_roots();
        if roots.is_empty() {
            return;
        }
        self.index.enqueue_scan(&roots);
    }

    /// 切换「包含子文件夹」（`07-A` RECURSE-01/02 / U-70）。
    ///
    /// 三处入口（工具栏、树右键、`⌘⇧R`）都走这一个方法——**状态只有一个**，
    /// 三处各存一份必然不同步（RECURSE-01 的「三处状态同步」就是这条）。
    fn toggle_recurse(&mut self) {
        if !self.lib.recurse_available() {
            return; // 「所有照片」视图下置灰（RECURSE-09）
        }
        let next = !self.lib.recurse;
        // 先记住顶部照片：切换后若它仍在新视图里就要对它（RECURSE-02）
        let top = self.anchor_id;
        match self.lib.set_recurse(next) {
            Ok(true) => {
                if let Ok(db) = self.lib.db()
                    && let Err(e) = db.set_recurse_subfolders(next)
                {
                    tracing::warn!(error = %e, "写 recurse_subfolders 失败");
                }
                // 锚点：能找回就对齐，找不回就回顶
                self.pending_anchor = top;
                if top.is_none() {
                    self.restore_scroll = Some(0.0);
                }
                self.cache.clear();
                self.lib.toast = Some((
                    if next {
                        "显示子文件夹".into()
                    } else {
                        "只看本文件夹".into()
                    },
                    1.2,
                ));
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "切换包含子文件夹失败"),
        }
    }

    /// 把缩放值写进 `settings.grid_cell_px`（`07-A` ZOOM-06：全局一份，重启恢复）。
    ///
    /// 写入是**低频**的（只在按钮/快捷键动作时），不会每帧写一次库。
    /// M3.1：滑块已退役，本方法只剩「动作后落库」一个调用语义。
    fn persist_zoom(&mut self) {
        if let Ok(db) = self.lib.db() {
            let _ = db.set_grid_cell_px(self.zoom.to_setting());
        }
    }

    /// 结算滑块：150 ms 静默之后决定要不要换纹理档位（`07-A` ZOOM-04）。
    fn tick_zoom(&mut self) {
        let now = std::time::Instant::now();
        if self.zoom.pending_settle(now) {
            // 还在拖动 / 还在结算窗口：只改布局，**不请求任何纹理**。
            // 这就是 ZOOM-04 的「拖动期间零解码投递」。
            return;
        }
        let dpi = self.dpi_scale;
        if self.zoom.settle(now, dpi).is_some() {
            // 档位真的变了：清掉纹理缓存里不再适配的旧档。
            //
            // 为什么整片清而不是逐张判断：三档并存时「哪张是哪档」要额外记一张表，
            // 而换档本来就是低频动作（一次拖动最多一次），整片清然后按新档重传
            // 比维护第二份账本简单得多。`TextureBudget` 会立刻把已释放的字节销账。
            self.cache.clear();
            self.persist_zoom();
        }
    }

    /// 同步监视器到当前的根集合（`07` WATCH-01）。
    ///
    /// 调用的时机有三个：启动、根增删、设置页拨开关。
    /// **每次都是全量同步**（不是增量）：`WatchManager::sync` 自己是幂等的，
    /// 让它去判断「哪些要装、哪些要卸」，比在这里维护一份对照表可靠。
    fn sync_watchers(&mut self) {
        let configs = {
            let Ok(db) = self.lib.db() else {
                return;
            };
            match picasa_core::watch_mgr::load_configs(&db) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "读监视配置失败，本轮不装监视");
                    return;
                }
            }
        };
        let Ok(db) = self.lib.db() else {
            return;
        };
        if let Err(e) = self.watch.sync(&db, &configs) {
            tracing::warn!(error = %e, "同步监视器失败");
        }
        let states = self.watch.states();
        tracing::info!(
            active = self.watch.active_count(),
            degraded = self.watch.degraded_count(),
            roots = states.len(),
            "监视器已同步"
        );
    }

    /// 每帧推进微缩图集（`07-A` §A-2.2）。
    ///
    /// 三件事，顺序不能换：
    ///
    /// 1. `begin_frame` —— 屏距按帧算
    /// 2. 装几张（限量）—— 只装**视口附近**的，别把全库塞进去
    /// 3. 帧末淘汰整页 —— 整页都不在附近才丢（一页 64 MB，丢早了就是反复重传）
    ///
    /// 只有在 `≤ 48 px` 档才做事：其余档位走 `TextureCache`，图集留空不占显存。
    fn tick_atlas(&mut self, ctx: &egui::Context) {
        if !picasa_core::uses_atlas(self.zoom.cell_px()) {
            // 切回大档：图集不再需要。**立刻清空**而不是留着——
            // 一页 64 MB，留着就是白占，而切回来时会重新装（那本来就是一次视觉过渡）。
            if !self.atlas.is_empty() {
                self.atlas.clear();
            }
            return;
        }
        self.atlas.begin_frame();

        // 视口附近的行（前后各 2 屏）——图集的填充与淘汰都用这一个集合。
        //
        // ⚠️ 中心点必须是**当前视口**，不能是列表开头：早先的实现 `take(400)`
        // 只装前 400 行，用户滚动到后面（或排序变化后视口不在顶部）时，
        // 可见的格子永远等不到进图集，一路都是灰占位——「≤48 px 档预览全灰」。
        // 中心点用上一帧栅格记录的 `viewport_top_index`；首帧还没栅格输出时
        // 自然是 0（列表顶部），行为与旧版一致。
        let first = self.viewport_top_index.min(self.lib.photos.len());
        let window = crate::atlas::ATLAS_PREFILL_ROWS;
        let from = first.saturating_sub(window / 2);
        let rows: Vec<&picasa_db::PhotoRow> =
            self.lib.photos.iter().skip(from).take(window).collect();
        if rows.is_empty() {
            return;
        }

        // ② 装几张（限量，避免一帧建页 + 上传几十张）
        if let Ok(db) = self.lib.db() {
            let n = crate::grid::pump_atlas_public(ctx, &mut self.atlas, &db, &self.service, &rows);
            if n > 0 {
                tracing::debug!(n, "图集装入一批");
            }
        }

        // ②b 把图集占用同步进纹理账本（SCALE-08 的内存自保护依赖它）
        self.atlas.sync_budget(&mut self.cache.budget);

        // ③ 帧末淘汰：屏距 ≥ 3 的整页作废（与 TextureBudget 同一条线）
        let dropped = self.atlas.evict_pages_farther_than(3);
        if dropped > 0 {
            tracing::debug!(dropped, pages = self.atlas.page_count(), "图集整页淘汰");
        }
    }

    /// 每帧推进监视器：收事件 → 去抖 → 排队刷新（`07` §2.2）。
    ///
    /// ⚠️ 这里**只投递 Job**，不自己扫描（R-M3-8：落库线程是唯一写 DB 的线程）。
    fn tick_watchers(&mut self) {
        let now = std::time::Instant::now();
        let outs = self.watch.poll(now);
        for out in outs {
            match out {
                picasa_core::WatchOut::Dirs(dirs) => {
                    for d in dirs {
                        // 复用 M2.1 的目录级刷新语义：**事件处理路径 = 已验证路径**
                        self.index.enqueue_refresh(&d);
                    }
                    self.lib.toast = Some(("检测到外部变化，正在刷新".into(), 1.5));
                }
                picasa_core::WatchOut::Root { root_id, path } => {
                    tracing::info!(root_id, path = %path.display(), "监视溢出，改为整根刷新");
                    self.index.enqueue_refresh(&path);
                }
            }
        }
    }

    fn refresh_tray_rows(&mut self) {
        self.tray_rows = self.lib.tray_photos();
        self.tray_rows_dirty = false;
    }

    /// 重算中区左侧的小标题与副标题（§7.3）。
    ///
    /// 副标题是「N 张 · 拍摄日期跨度」。日期跨度按**已加载**的照片算：
    /// 分页首屏是用户当下最关心的一段，且不额外查库（数据层零新增）。
    fn refresh_view_header(&mut self) {
        let title = self.lib.view_title();
        let breadcrumb = self.lib.view_breadcrumb();
        let total = self.lib.photos_total.max(self.lib.photos.len());
        let len = self.lib.photos.len();
        let first = self.lib.photos.first().map(|p| p.id).unwrap_or(0);
        let last = self.lib.photos.last().map(|p| p.id).unwrap_or(0);
        let crumb_tail = breadcrumb.last().and_then(|(_, id)| *id);
        let key = (title.clone(), total, len, first, last, crumb_tail);
        if self.view_header_key.as_ref() == Some(&key) {
            return;
        }
        self.view_header_key = Some(key);
        self.view_header_title = title;
        self.view_breadcrumb = breadcrumb;
        // M32-UI-09：日期一律带来源标签——这里统一是 EXIF/回退的「拍摄时间」口径
        // （`effective_taken_at`），无来源月份不展示（span 为 None 时只显示张数）。
        self.view_header_subtitle = match photo_date_span(&self.lib.photos) {
            Some(span) => format!("{total} 张 · 拍摄时间：{span}"),
            None => format!("{total} 张"),
        };
    }

    /// 事件泵：后台线程 → UI 状态。**每帧先跑它**，后面所有绘制都基于最新状态。
    fn pump_events(&mut self, ctx: &egui::Context) {
        let mut need_reload = false;
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                CoreEvent::ThumbReady { photo_id, jpeg, .. } => {
                    self.cache.insert_jpeg(photo_id, jpeg);
                }
                CoreEvent::ThumbFailed { photo_id, reason } => {
                    self.cache.mark_failed(photo_id);
                    tracing::debug!(photo_id, reason = %reason, "缩略图失败（计入失败项）");
                }
                CoreEvent::FullImageReady {
                    photo_id,
                    w,
                    h,
                    rgba,
                } => {
                    // 并排比较图先于主图判定（同一 JobKind::Viewer 通道）
                    if let Some(v) = self.viewer.as_mut()
                        && v.compare_requested == Some(photo_id)
                        && v.loaded_id != Some(photo_id)
                    {
                        if let Some(img) = image::RgbaImage::from_raw(w, h, rgba) {
                            let small = picasa_imaging::resize_to_fit(&img, 2560);
                            let size = [small.width() as usize, small.height() as usize];
                            let color =
                                egui::ColorImage::from_rgba_unmultiplied(size, small.as_raw());
                            let tex = ctx.load_texture(
                                format!("compare-{photo_id}"),
                                color,
                                egui::TextureOptions::LINEAR,
                            );
                            v.compare_tex =
                                Some((photo_id, tex, vec2(size[0] as f32, size[1] as f32)));
                        }
                        continue;
                    }
                    if let Some(v) = self.viewer.as_mut()
                        && v.requested_id == Some(photo_id)
                    {
                        let img = image::RgbaImage::from_raw(w, h, rgba);
                        if let Some(img) = img {
                            // 上传前缩到 2560：一张 24MP 的 RGBA 纹理是 96MB 显存，
                            // 而我们最多看到 8 倍缩放，2560 长边足够（M1 取舍，见实施说明）。
                            let small = picasa_imaging::resize_to_fit(&img, 2560);
                            let size = [small.width() as usize, small.height() as usize];
                            let color =
                                egui::ColorImage::from_rgba_unmultiplied(size, small.as_raw());
                            v.tex_size = vec2(size[0] as f32, size[1] as f32);
                            v.tex = Some(ctx.load_texture(
                                format!("viewer-{photo_id}"),
                                color,
                                egui::TextureOptions::LINEAR,
                            ));
                            // M4 #11：直方图——在缩小的图上算（2560 长边已保留全部
                            // 影调信息；采样到 512² 只是省一次 6 万桶的循环）。
                            v.histogram = Some(picasa_imaging::histogram(&small));
                            v.loaded_id = Some(photo_id);
                            // 拿到真实尺寸后按「适应窗口」重新定 zoom
                            let avail = ctx.content_rect();
                            let fit = (avail.width() / v.tex_size.x)
                                .min(avail.height() / v.tex_size.y)
                                .min(1.0);
                            if fit.is_finite() && fit > 0.0 {
                                v.zoom = fit;
                            }
                        }
                    }
                }
                // 终态事件（扫描跑完 / 索引被删）必须**立刻落屏**，不等 150 ms 节流：
                // 这是用户判断"到底索引成没成"的那一眼，晚一帧他就去点别的了。
                ev @ (CoreEvent::ScanFinished { .. } | CoreEvent::IndexCleared { .. }) => {
                    if self.lib.apply_event(ev) {
                        need_reload = true;
                    }
                    self.reload_now = true;
                    self.tray_rows_dirty = true;
                    // 一个批次收尾 → 失败项数量变了（重建完可能从 970 掉到 0），角标要跟上
                    self.refresh_failed_thumbs();
                }
                other => {
                    if self.lib.apply_event(other) {
                        need_reload = true;
                    }
                    self.tray_rows_dirty = true;
                }
            }
        }
        if need_reload {
            // 只置脏。真正的「重建树 + 重查首屏」交给 `flush_reload` 节流执行——
            // 扫描期这里每帧都可能被置脏，而且事件本身是**每个目录一条**。
            self.reload_dirty = true;
        }
        if self.tray_rows_dirty {
            self.refresh_tray_rows();
        }
    }

    /// 把「底层数据变了」按节流真正落成一次界面重载。
    ///
    /// ## 为什么必须节流
    ///
    /// `FolderScanned` 是**每个目录一条**事件。装照片的移动硬盘通常是一堆相册目录，
    /// 几千个很常见。原来每个事件都同步做「重建左栏树 + 重查首屏」，
    /// `tree_bench` 实测 8000 目录时单次重建 46.8 ms —— 累计 405 秒全部花在 UI 线程，
    /// 表现就是用户说的「主界面弹不出来」：窗口在那儿，但一帧都画不完。
    ///
    /// 现在聚合成「最多每 150 ms 刷一次」。150 ms 是权衡：再长用户会觉得数字和缩略图
    /// 跟不上扫描进度；再短就退回到「每几帧重建一次树」。
    fn flush_reload(&mut self, ctx: &egui::Context) {
        const RELOAD_GAP: Duration = Duration::from_millis(150);
        if !self.reload_dirty {
            return;
        }
        let elapsed = self.last_reload.elapsed();
        if !self.reload_now && elapsed < RELOAD_GAP {
            // ⚠️ 必须主动约一次唤醒。egui 是事件驱动的：鼠标不动就没有帧，
            // 不 request_repaint 的话这次「脏」会一直挂到用户下次碰鼠标才生效，
            // 表现为「扫描进度在动，但照片列表和左栏数字一直是旧的」。
            ctx.request_repaint_after(RELOAD_GAP - elapsed);
            return;
        }
        self.reload_dirty = false;
        self.reload_now = false;
        self.last_reload = Instant::now();

        // 树只在真的脏了时重建（一次重建是 O(N log N)，不是每 150 ms 都值得付）
        if self.lib.take_tree_dirty()
            && let Err(e) = self.lib.reload_tree()
        {
            tracing::warn!(error = %e, "重建目录树失败");
        }

        // 重载会换掉整个照片列表：**锚定用户当前看的那张**，不跳回顶部。
        // 只记 id 不记像素——扫描会把新照片插到列表任意位置，旧的像素 offset
        // 指向的已经不是原来那张了。
        self.pending_anchor = self.anchor_id;
        if let Err(e) = self.lib.reload_photos() {
            tracing::warn!(error = %e, "刷新照片列表失败");
        }
        self.refresh_view_header();
    }

    // -------------------------------------------------------------- 键盘

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let in_viewer = self.viewer.is_some();
        for action in shortcuts::collect(ctx, in_viewer) {
            if in_viewer {
                self.apply_viewer_key(action, ctx);
            } else {
                self.apply_library_key(action, ctx);
            }
        }
    }

    fn apply_library_key(&mut self, action: KeyAction, ctx: &egui::Context) {
        match action {
            KeyAction::SelectAll => self.lib.select_all(),
            KeyAction::SelectNone => self.lib.select_none(),
            KeyAction::Invert => self.lib.select_invert(),
            KeyAction::ToStart => self.lib.select_edge(false),
            KeyAction::ToEnd => self.lib.select_edge(true),
            KeyAction::ToTray => {
                let n = self.lib.to_tray();
                if n > 0 {
                    self.tray_expanded = true;
                    self.tray_rows_dirty = true;
                    self.lib.toast = Some((format!("已放入托盘 {n} 张"), 2.5));
                }
            }
            // M3.1：`⌘1`/`⌘2`/`⌘+`/`⌘−` 全部走对数档步进（与顶栏 −/+ 同一
            // 条路径）；`⌘1` = 小一档、`⌘2` = 大一档（旧的三刻度点语义
            // 随「缩略图」按钮一起退役）。
            KeyAction::TierSmall => {
                self.step_zoom(-1);
                ctx.request_repaint();
            }
            KeyAction::TierLarge => {
                self.step_zoom(1);
                ctx.request_repaint();
            }
            KeyAction::ZoomReset => {
                self.zoom.reset();
                self.zoom.end_drag(std::time::Instant::now());
                self.persist_zoom();
            }
            KeyAction::ToggleRecurse => self.toggle_recurse(),
            KeyAction::FocusSearch => {
                self.search_open = true;
                ctx.memory_mut(|m| m.request_focus(egui::Id::unique("topbar-search")));
                ctx.request_repaint();
            }
            KeyAction::ToggleFullscreen => {
                let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
            }
            KeyAction::Reveal => self.reveal_selected(),
            KeyAction::Delete => {
                if !self.lib.selection.is_empty() {
                    self.confirm_trash = Some(self.lib.selection.len());
                }
            }
            KeyAction::OpenViewer => {
                // 打开「当前选中」的第一张，没有选中就打开第 0 张
                let idx = self
                    .lib
                    .photos
                    .iter()
                    .position(|p| self.lib.selection.contains(&p.id))
                    .unwrap_or(0);
                if !self.lib.photos.is_empty() {
                    self.open_viewer(idx);
                }
            }
            _ => {}
        }
    }

    fn apply_viewer_key(&mut self, action: KeyAction, ctx: &egui::Context) {
        // 翻页 / 退出要改选择集，不能同时借着 viewer。
        match action {
            KeyAction::CloseViewer => {
                self.close_viewer();
                return;
            }
            KeyAction::Next => {
                self.nudge_viewer(1);
                return;
            }
            KeyAction::Prev => {
                self.nudge_viewer(-1);
                return;
            }
            _ => {}
        }
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        match action {
            KeyAction::ZoomIn => v.zoom_in(),
            KeyAction::ZoomOut => v.zoom_out(),
            KeyAction::ZoomFit => v.zoom = 0.0001, // 下一帧会被 fit 覆盖
            KeyAction::ZoomActual => v.zoom = 1.0,
            KeyAction::ToggleBackground => v.toggle_bg(),
            KeyAction::ToggleInfo => v.info_open = !v.info_open,
            KeyAction::ToggleCompare => {
                v.compare = !v.compare;
                if !v.compare {
                    v.compare_tex = None;
                }
            }
            KeyAction::ToggleFullscreen => {
                let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
            }
            KeyAction::Reveal => self.reveal_selected(),
            KeyAction::Delete => {
                if let Some(row) = self.lib.photos.get(v.index) {
                    let _ = row;
                    self.confirm_trash = Some(1);
                }
            }
            _ => {}
        }
    }

    /// 对数档走一步。已经在端点时不再写库。
    fn step_zoom(&mut self, dir: i32) {
        if picasa_core::at_log_bound(self.zoom.cell_px(), dir) {
            return;
        }
        let next = picasa_core::step_log(self.zoom.cell_px(), dir);
        let now = std::time::Instant::now();
        self.zoom.drag_to(next, now);
        self.zoom.end_drag(now);
        self.persist_zoom();
    }

    /// 查看器翻一页，并把网格选中跟到这一张。
    fn nudge_viewer(&mut self, delta: isize) {
        let Some(v) = self.viewer.as_ref() else {
            return;
        };
        let next = v.index as isize + delta;
        if next < 0 || next >= self.lib.photos.len() as isize {
            return;
        }
        let next = next as usize;
        if let Some(v) = self.viewer.as_mut() {
            v.jump_to(next);
        }
        self.lib.click_select(next, false, false);
    }

    /// 退出查看器。网格选中改成最后看的那一张（B10）。
    fn close_viewer(&mut self) {
        let idx = self.viewer.as_ref().map(|v| v.index);
        self.viewer = None;
        if let Some(idx) = idx {
            self.lib.click_select(idx, false, false);
        }
    }

    fn reveal_selected(&mut self) {
        let target = self
            .lib
            .photos
            .iter()
            .find(|p| self.lib.selection.contains(&p.id))
            .cloned()
            .or_else(|| {
                self.viewer
                    .as_ref()
                    .and_then(|v| self.lib.photos.get(v.index).cloned())
            });
        match target {
            Some(row) => {
                if let Err(e) = crate::macos::reveal_in_finder(&row.abs_path()) {
                    self.lib.toast = Some((e, 4.0));
                }
            }
            None => self.lib.toast = Some(("先选一张照片".into(), 2.0)),
        }
    }

    fn open_viewer(&mut self, index: usize) {
        let mut v = Viewer::new(index);
        v.info_open = self.lib.prefs.viewer_info_open;
        if let Some(row) = self.lib.photos.get(index) {
            viewer::ensure_loaded(&mut v, row, &self.service);
        }
        self.viewer = Some(v);
    }

    // -------------------------------------------------------------- 绘制

    fn draw_library(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let theme = self.theme;
        // 中区左侧标题可能被任何直接改视图/筛选/搜索的动作改变（不走 reload_dirty），
        // 每帧用轻量签名兜底；真正重算（扫照片算日期跨度）只在签名变化时发生。
        self.refresh_view_header();
        // 本帧的进度切片：状态栏/左栏/栅格**读同一份**（`04` §3.2 的四处一致）
        let snap = self.progress.snapshot().clone();
        // ---- M2.2 §6.1：管理快照 ≤ 4 次/秒刷新（PERF-05 的「每帧 ≤ 1 次 DB 查询」）
        //
        // 只在**页面打开**时才刷：关着的页面不需要数据，白刷等于每 250 ms
        // 做一次十几个查询的批量读，这在主线程上是白花钱（R-M2-3）。
        if self.index_mgr.open && std::time::Instant::now() >= self.mgmt_next_refresh {
            self.refresh_mgmt();
        }

        // ① 顶部工具栏（M3.1：macOS「照片」风格单行顶栏）
        egui::Panel::top("toolbar-panel")
            .exact_size(Metrics::TOPBAR_H)
            .frame(egui::Frame::new().fill(theme.toolbar_bg))
            .show(ui, |ui| {
                let filter_active = self.lib.filter.as_ref().is_some_and(|f| {
                    f.starred_only
                        || f.tag.is_some()
                        || f.taken_range.is_some()
                        || f.size_class.is_some()
                        || !f.formats.is_empty()
                });
                // §7.2：顶栏只放操作/排布/缩放/搜索，不再放大号标题。
                // 标题与副标题已移到中区左侧（文件树之上）。
                let zoom_min = picasa_core::at_log_bound(self.zoom.cell_px(), -1);
                let zoom_max = picasa_core::at_log_bound(self.zoom.cell_px(), 1);
                let out = toolbar::show(
                    ui,
                    &theme,
                    self.zoom.cell_px(),
                    &self.view_breadcrumb,
                    self.lib.sort,
                    !self.lib.selection.is_empty(),
                    &mut self.search_buf,
                    self.sort_menu_open,
                    self.lib.recurse,
                    self.lib.recurse_available(),
                    filter_active,
                    self.lib.search_in_current,
                    self.search_open,
                    self.lib.view_mode,
                    zoom_min,
                    zoom_max,
                    // 左段右缘与文件夹树右缘对齐（侧栏隐藏时退回默认宽度）
                    if self.sidebar_visible {
                        self.lib.prefs.sidebar_width
                    } else {
                        Metrics::SIDEBAR_DEFAULT_W
                    },
                );
                if let Some(rect) = out.sort_button {
                    self.sort_menu_pos = rect.left_bottom() + vec2(0.0, 2.0);
                }
                if let Some(rect) = out.filter_button {
                    self.filter_menu_pos = rect.left_bottom() + vec2(0.0, 2.0);
                }
                if let Some(rect) = out.more_button {
                    self.more_menu_pos = rect.left_bottom() + vec2(0.0, 2.0);
                }
                for action in out.actions {
                    match action {
                        TopbarAction::ToggleSidebar => {
                            self.sidebar_visible = !self.sidebar_visible;
                        }
                        TopbarAction::PickFolder(id) => {
                            // 面包屑祖先段点击：跳到该文件夹（与左栏点击同一条路径）
                            if let Err(e) = self.lib.set_view(View::Folder(id)) {
                                tracing::warn!(error = %e, "面包屑跳转失败");
                            }
                            self.restore_scroll = Some(0.0);
                            self.viewer = None;
                            ctx.request_repaint();
                        }
                        TopbarAction::AddFolder => self.add_folder(),
                        TopbarAction::Placeholder(text) => {
                            self.lib.toast = Some((text.to_string(), 2.5));
                        }
                        TopbarAction::ToggleSearch => {
                            self.search_open = !self.search_open;
                            if self.search_open {
                                // 输入框要到下一帧才画出来；先把焦点记在固定 id 上。
                                ctx.memory_mut(|m| {
                                    m.request_focus(egui::Id::unique("topbar-search"))
                                });
                            } else {
                                // 收起后必须主动交还焦点，否则快捷键会一直被「焦点在输入框」
                                // 挡掉（`shortcuts::collect` 见到 focused 就返回空）。
                                ctx.memory_mut(|m| {
                                    m.surrender_focus(egui::Id::unique("topbar-search"))
                                });
                            }
                            ctx.request_repaint();
                        }
                        TopbarAction::ZoomOut => {
                            // 按钮是在本帧开头按「还没步进」的状态画的。
                            // 不补一帧的话，点到端点后鼠标不动，−/+ 会一直保持可点的颜色。
                            self.step_zoom(-1);
                            ctx.request_repaint();
                        }
                        TopbarAction::ZoomIn => {
                            self.step_zoom(1);
                            ctx.request_repaint();
                        }
                        TopbarAction::PickViewMode(mode) => {
                            self.lib.set_view_mode(mode);
                            // 切布局等于换了一个视图：滚动位置回顶，避免落在空洞处
                            self.restore_scroll = Some(0.0);
                        }
                        TopbarAction::ToggleSortMenu => {
                            self.sort_menu_open = !self.sort_menu_open;
                            if self.sort_menu_open {
                                self.sort_menu_frame = ctx.cumulative_frame_nr();
                            }
                        }
                        TopbarAction::PickSortKey(key) => {
                            self.pick_sort(key);
                            self.sort_menu_open = false;
                        }
                        TopbarAction::ToggleSortDirection => {
                            let key = self.lib.sort.key;
                            self.pick_sort(key);
                            self.sort_menu_open = false;
                        }
                        TopbarAction::ToggleFilterMenu => {
                            self.filter_menu_open = !self.filter_menu_open;
                            if self.filter_menu_open {
                                self.filter_menu_frame = ctx.cumulative_frame_nr();
                                // 面板打开时把现有筛选读进草稿
                                self.filter_draft_tag = self
                                    .lib
                                    .filter
                                    .as_ref()
                                    .and_then(|f| f.tag.clone())
                                    .unwrap_or_default();
                            }
                        }
                        TopbarAction::ToggleMoreMenu => {
                            // 与排序/筛选同一套：按钮只开关菜单，不直接进选目录。
                            self.more_menu_open = !self.more_menu_open;
                            if self.more_menu_open {
                                self.more_menu_frame = ctx.cumulative_frame_nr();
                            }
                        }
                        TopbarAction::ToggleSearchScope => {
                            if let Err(e) =
                                self.lib.set_search_in_current(!self.lib.search_in_current)
                            {
                                tracing::warn!(error = %e, "切换搜索范围失败");
                            }
                        }
                        TopbarAction::SearchChanged(text) => {
                            if let Err(e) = self.lib.set_search(text) {
                                tracing::warn!(error = %e, "搜索失败");
                            }
                            self.restore_scroll = Some(0.0);
                        }
                        // 菜单项动作。按钮本身发的是 ToggleMoreMenu，由 draw_overlays 画菜单。
                        TopbarAction::More(more) => match more {
                            MoreAction::AddFolder => self.add_folder(),
                            MoreAction::ToggleRecurse => self.toggle_recurse(),
                            MoreAction::ToggleFullscreen => {
                                let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                            }
                            MoreAction::ToggleShowFilename => {
                                self.grid_show_filename = !self.grid_show_filename;
                            }
                            MoreAction::About => {
                                self.lib.toast = Some((
                                    format!(
                                        "OpenPicasa {} · 本地照片库（只读不改原文件）",
                                        env!("CARGO_PKG_VERSION")
                                    ),
                                    3.0,
                                ));
                            }
                        },
                    }
                }
            });

        // ⑤ 底部状态栏（**在左栏之前**，否则侧栏会盖住它）
        egui::Panel::bottom("statusbar-panel")
            .exact_size(Metrics::STATUS_H)
            .frame(egui::Frame::new().fill(theme.chrome_bg))
            .show(ui, |ui| {
                let out = statusbar::show(
                    ui,
                    &self.lib,
                    &theme,
                    &self.mgmt,
                    &self.verify_text,
                    self.zoom.cell_px(),
                    &snap,
                );
                // M3.1：状态栏滑块已移除（顶栏 −/+ 按钮替代），
                // zoom_drag / zoom_released 分支随之退役。
                if out.index_clicked {
                    // M2.2 §2.6：唯一入口 → 索引管理页（并自动定位到有问题的标签）。
                    // 旧的 `progress.open_panel()` 不再挂在这里——右下角弹框仍然自己出现
                    // （M2-FD-29~36 保留），但**索引按钮**只剩这一个。
                    // 先刷快照再开窗：这一帧的「存储与诊断」不能拿默认的全 0。
                    self.refresh_mgmt();
                    self.index_mgr
                        .open_at(&self.mgmt, self.mgmt.failed_task_count);
                    ctx.request_repaint();
                }
                if out.diag_clicked {
                    // 过渡期：旧的「诊断」页仍从这里打开。它不是"忘了删的旧入口"，
                    // 而是新页面「存储与诊断」标签还没齐的两项（诊断日志导出、
                    // 带进度/取消的 VACUUM）目前的唯一可达路径。齐了之后按规格 §9 迭代 3 再删。
                    self.diagnostics_open = true;
                    self.diag_dirty = true;
                }
            });

        // ② 左栏（含底部托盘）。M3.1：顶栏侧栏开关可整体收起
        //（`show_collapsible` 会留抓握条，这里直接不建面板更干净）。
        let sidebar_resp = self.sidebar_visible.then(|| {
            egui::Panel::left("sidebar-panel")
                .default_size(if self.lib.prefs.sidebar_width < Metrics::SIDEBAR_MIN {
                    Metrics::SIDEBAR_DEFAULT_W
                } else {
                    self.lib.prefs.sidebar_width
                })
                .size_range(Metrics::SIDEBAR_MIN..=Metrics::SIDEBAR_MAX)
                .size_range(Metrics::SIDEBAR_MIN..=Metrics::SIDEBAR_MAX)
                .resizable(true)
                .frame(egui::Frame::new().fill(theme.sidebar_bg))
                .show(ui, |ui| {
                    let db_arc = self.lib.db.clone();
                    let db = db_arc.lock().expect("db 锁");
                    let mut sctx = SidebarCtx {
                        theme: &theme,
                        db: &db,
                        service: &self.service,
                        cache: &mut self.cache,
                        tray_rows: &self.tray_rows,
                        tray_expanded: self.tray_expanded,
                        favorites_collapsed: self.favorites_collapsed,
                        folders_collapsed: self.folders_collapsed,
                        has_focus: ctx.memory(|m| m.focused()).is_none(),
                        snapshot: &snap,
                    };
                    let actions = sidebar::show(
                        ui,
                        &self.lib,
                        &mut sctx,
                        &self.view_header_title,
                        &self.view_header_subtitle,
                    );
                    drop(db);
                    actions
                })
        });

        // 左栏宽度持久化（U-03：左栏宽度固定，栅格吸收剩余宽度）
        if let Some(sidebar_resp) = &sidebar_resp {
            let w = sidebar_resp.response.rect.width();
            if (w - self.lib.prefs.sidebar_width).abs() > 0.5 {
                self.lib.prefs.sidebar_width = w;
                self.lib.mark_prefs_dirty();
            }
            for action in sidebar_resp.inner.clone() {
                self.apply_sidebar_action(action, &ctx);
            }
        }

        // ③ 主区顶部标题行（M32-UI-04 v1.2：标题迁到主区顶部，两行左对齐；
        // 修订 docs/09 §7.3「标题区在左栏」——左栏只保留文件树与托盘）
        egui::Panel::top("view-header-panel")
            .exact_size(Metrics::VIEW_HEADER_H)
            .frame(egui::Frame::new().fill(theme.grid_bg))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter_at(rect);
                painter.text(
                    pos2(rect.left() + Metrics::GRID_PAD, rect.top() + 12.0),
                    egui::Align2::LEFT_CENTER,
                    &self.view_header_title,
                    egui::FontId::new(15.0, egui::FontFamily::Proportional),
                    theme.text_primary,
                );
                painter.text(
                    pos2(rect.left() + Metrics::GRID_PAD, rect.bottom() - 12.0),
                    egui::Align2::LEFT_CENTER,
                    &self.view_header_subtitle,
                    egui::FontId::new(11.0, egui::FontFamily::Proportional),
                    theme.text_secondary,
                );
            });

        // ④ 中央栅格
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(theme.grid_bg))
            .show(ui, |ui| {
                let db_arc = self.lib.db.clone();
                let db = db_arc.lock().expect("db 锁");
                let anchor = self.pending_anchor.take();
                let out = grid::show(
                    ui,
                    &self.lib.photos,
                    &self.lib.selection,
                    None,
                    matches!(self.lib.sort.key, SortKey::TakenAt),
                    self.lib.view_mode,
                    self.restore_scroll,
                    anchor,
                    &db,
                    &self.service,
                    &mut self.cache,
                    &mut self.grid_layout,
                    &theme,
                    &snap,
                    &mut self.atlas,
                    self.zoom.cell_px(),
                    self.grid_show_filename,
                    grid::empty_reason(&self.lib.search, self.lib.filter.as_ref()),
                );
                drop(db);

                // ---- M3 PERF-08：纹理预算的每帧两步
                //
                // ① 告知视口：哪些照片在屏内、离屏多远。`TextureBudget` 靠它决定谁能淘汰，
                //    并且**屏距 0 永不淘汰**——正在看的图被自己丢掉是最难查的「闪灰块」。
                // ② 帧末释放：本帧攒下的淘汰候选在这里真正 drop。
                //    帧中 drop 会让本帧剩余绘制拿到失效的 TextureId。
                //
                // 两步都在栅格绘制**之后**，顺序不能换：先 drop 再画等于把格子擦干净。
                self.note_texture_viewport(&out);
                let freed = self.cache.flush_evictions();
                if freed > 0 {
                    tracing::debug!(freed, used_mb = self.cache.budget.used_mb(), "纹理预算释放");
                }

                self.restore_scroll = None;
                // 记住这一帧视口顶部在看哪张：下次重载（扫描出新照片、切排序）时用它锚回来。
                self.anchor_id = self.lib.photos.get(out.top_index).map(|p| p.id);
                self.viewport_top_index = out.top_index;
                self.lib.prefs.grid_scroll = out.scroll_y;
                if out.scroll_y > 0.5 {
                    self.lib.mark_prefs_dirty();
                }

                // 视口里宽高还空着的照片：立刻插进回填队列（M2.1 B）。
                // 放在 `drop(db)` 之后——`request_visible` 只是往 channel 里塞 id，
                // 不需要也不该在持有 DB 锁的时候做。
                self.dims.request_visible(&out.need_dims);

                if let Some(click) = out.clicked {
                    self.lib
                        .click_select(click.index, click.additive, click.range);
                }
                if let Some(index) = out.opened {
                    self.open_viewer(index);
                }
                if let Some((index, pos)) = out.context {
                    self.lib.click_select(index, false, false);
                    self.folder_ctx = None;
                    // M4：照片右键菜单（收藏 / 说明 / 标签）
                    if let Some(row) = self.lib.photos.get(index) {
                        self.photo_ctx = Some((row.id, row.starred, pos));
                    }
                }
            });
    }

    fn apply_sidebar_action(&mut self, action: SidebarAction, ctx: &egui::Context) {
        match action {
            SidebarAction::ShowView(view) => {
                if let Err(e) = self.lib.set_view(view) {
                    tracing::warn!(error = %e, "切换视图失败");
                }
                self.restore_scroll = Some(0.0);
                self.viewer = None;
            }
            SidebarAction::TimelineYear(year) => {
                if year == 0 {
                    // 清除年份筛选（保留其他筛选维度）
                    let mut f = self.lib.filter.clone().unwrap_or_default();
                    f.taken_range = None;
                    let has = f.starred_only
                        || f.tag.is_some()
                        || f.taken_range.is_some()
                        || f.size_class.is_some()
                        || !f.formats.is_empty();
                    if let Err(e) = self.lib.set_filter(has.then_some(f)) {
                        tracing::warn!(error = %e, "清除时间轴筛选失败");
                    }
                } else {
                    let mut f = self.lib.filter.clone().unwrap_or_default();
                    f.taken_range = Some(picasa_core::year_range_seconds(year));
                    if let Err(e) = self.lib.set_filter(Some(f)) {
                        tracing::warn!(error = %e, "时间轴筛选失败");
                    }
                }
                self.restore_scroll = Some(0.0);
            }
            SidebarAction::ToggleStarredFilter => {
                let mut f = self.lib.filter.clone().unwrap_or_default();
                f.starred_only = !f.starred_only;
                let f = if f.starred_only
                    || f.tag.is_some()
                    || f.taken_range.is_some()
                    || f.size_class.is_some()
                    || !f.formats.is_empty()
                {
                    Some(f)
                } else {
                    None
                };
                if let Err(e) = self.lib.set_filter(f) {
                    tracing::warn!(error = %e, "切换收藏筛选失败");
                }
                self.restore_scroll = Some(0.0);
            }
            SidebarAction::ToggleFolder(path) => {
                self.lib.toggle_folder(&path);
                self.lib.mark_prefs_dirty();
            }
            SidebarAction::ToggleSection(title) => {
                if title == "收藏" {
                    self.favorites_collapsed = !self.favorites_collapsed;
                } else {
                    self.folders_collapsed = !self.folders_collapsed;
                }
            }
            SidebarAction::TrayToggle => self.tray_expanded = !self.tray_expanded,
            SidebarAction::TrayClear => {
                self.lib.tray_clear();
                self.tray_rows_dirty = true;
            }
            SidebarAction::TrayRemove(id) => {
                self.lib.tray_remove(id);
                self.tray_rows_dirty = true;
            }
            SidebarAction::TrayOpen(id) => {
                if let Some(idx) = self.lib.index_of(id) {
                    self.open_viewer(idx);
                }
            }
            SidebarAction::FolderContext(path, pos) => {
                self.folder_ctx = Some((path, pos));
                // 记下打开帧，供 `draw_overlays` 的「点别处就关」豁免（原因见字段注释）
                self.folder_ctx_frame = ctx.cumulative_frame_nr();
            }
        }
    }

    // -------------------------------------------------------------- 诊断页（F）

    /// 一次性取好诊断页要的全部数据。**不每帧取**：`SUM(LENGTH(data))` 会扫一遍缩略图表。
    fn gather_diag_data(&mut self) {
        let mut data = crate::diagnostics::DiagData::default();
        {
            if let Ok(db) = self.lib.db() {
                if let Ok(s) = db.cache_stats() {
                    data.stats = s;
                }
                if let Ok(tasks) = db.recent_tasks(50) {
                    for t in tasks {
                        if let Ok(list) = db.task_failures(t.task_id) {
                            for (path, code, _attempts) in list {
                                data.failures.push(crate::diagnostics::DiagFailure {
                                    scope: t.scope_path.clone(),
                                    path,
                                    code,
                                });
                            }
                        }
                    }
                }
            }
        }
        data.exclusions = self.lib.all_exclusions();
        data.closed_roots = self.lib.closed_roots();
        self.diag_data = data;
        self.diag_dirty = false;
    }

    fn apply_diag_action(&mut self, action: crate::diagnostics::DiagAction, _ctx: &egui::Context) {
        use crate::diagnostics::DiagAction;
        match action {
            DiagAction::ClearThumbs => {
                let outcome = self.lib.db().map(|db| db.clear_all_thumbs());
                match outcome {
                    Ok(Ok(n)) => {
                        // 内存里的纹理也要丢：否则界面还显示旧缩略图，用户以为没清掉。
                        self.cache.clear();
                        self.lib.toast = Some((format!("已清空 {n} 条缩略图，可再点 VACUUM"), 3.0));
                    }
                    _ => self.lib.toast = Some(("清理失败，详见日志".into(), 3.0)),
                }
            }
            DiagAction::Vacuum => {
                let outcome = self.lib.db().map(|db| db.vacuum());
                match outcome {
                    Ok(Ok(())) => self.lib.toast = Some(("已压缩索引库".into(), 2.5)),
                    _ => self.lib.toast = Some(("VACUUM 失败（可能被占用）".into(), 3.0)),
                }
            }
            DiagAction::RebuildAll => {
                {
                    if let Ok(db) = self.lib.db() {
                        let _ = db.clear_all_thumbs();
                        let _ = db.vacuum();
                    }
                }
                self.cache.clear();
                let roots = self.lib.root_paths();
                if !roots.is_empty() {
                    self.index.enqueue_scan(&roots);
                }
                self.lib.toast = Some(("已开始全量重建".into(), 3.0));
            }
            DiagAction::RestoreExclusion(id) => {
                match self.lib.restore_exclusion(id) {
                    Ok(Some(root)) => {
                        // 恢复排除项后必须重扫一次根目录，否则树里还是空的。
                        self.index.enqueue_refresh(std::path::Path::new(&root));
                        self.lib.toast = Some(("已恢复，正在重新索引".into(), 3.0));
                    }
                    Ok(None) => self.lib.toast = Some(("这条排除记录已不存在".into(), 2.5)),
                    Err(e) => {
                        tracing::warn!(error = %e, "恢复排除项失败");
                        self.lib.toast = Some(("恢复失败，详见日志".into(), 3.0));
                    }
                }
            }
            DiagAction::ReopenRoot(path) => {
                self.lib.add_root(std::path::Path::new(&path));
                self.index.enqueue_refresh(std::path::Path::new(&path));
                let _ = self.lib.reload_tree();
                self.lib.toast = Some(("已重新添加，正在索引".into(), 3.0));
            }
            DiagAction::Refresh => {}
        }
        self.diag_dirty = true;
    }

    fn draw_viewer(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        if let Some(v) = self.viewer.as_mut() {
            if let Some(row) = self.lib.photos.get(v.index) {
                viewer::ensure_loaded(v, row, &self.service);
                // 并排比较（M4 #11）：需要右侧图且还没载入 → 排队下一张的大图。
                // 复用同一 ThumbnailService 通道；载入完成在 pump_events 的
                // FullImageReady 里落进 compare_tex。
                if v.compare
                    && v.compare_tex.is_none()
                    && let Some(next) = self.lib.photos.get(v.index + 1)
                    && v.compare_requested != Some(next.id)
                {
                    v.compare_requested = Some(next.id);
                    self.service.request(ThumbJob {
                        photo_id: next.id,
                        path: next.abs_path(),
                        kind: JobKind::Viewer,
                    });
                }
                // 逐级加载第一级：大图还没到、也还没预览 → 借栅格缓存里的 T0 缩略图顶上。
                // T0 是 512，解码 + 上传只要几毫秒，比起等 2560 大图（24MP 要 200~400ms）
                // 能立刻给用户画面。拿不到就维持「正在解码…」，不阻塞。
                if v.tex.is_none() && v.preview.is_none() {
                    let db_arc = self.lib.db.clone();
                    let db = db_arc.lock().expect("db 锁");
                    let mut budget = 1;
                    if let TexState::Ready(t, s) =
                        self.cache
                            .get(ui.ctx(), &db, &self.service, row, &mut budget)
                    {
                        v.preview = Some((t, s));
                    }
                    drop(db);
                }
            }
            let action = viewer::show(
                ui,
                v,
                &self.lib.photos,
                &theme,
                ui.ctx().memory(|m| m.focused()).is_none(),
            );
            match action {
                Some(ViewerAction::Close) => self.close_viewer(),
                Some(ViewerAction::Reveal) => self.reveal_selected(),
                Some(ViewerAction::Next) => self.nudge_viewer(1),
                Some(ViewerAction::Prev) => self.nudge_viewer(-1),
                None => {}
            }
            // 记住信息面板的展开状态（F2-09「可折叠记忆」）
            if let Some(v) = self.viewer.as_ref()
                && self.lib.prefs.viewer_info_open != v.info_open
            {
                self.lib.prefs.viewer_info_open = v.info_open;
                self.lib.mark_prefs_dirty();
            }
        }
    }

    fn draw_overlays(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        let ctx = ui.ctx().clone();

        // 排序菜单
        if self.sort_menu_open && self.viewer.is_none() {
            if let Some(action) = toolbar::sort_menu(ui, self.sort_menu_pos, self.lib.sort) {
                match action {
                    crate::topbar::TopbarAction::PickSortKey(key) => self.pick_sort(key),
                    crate::topbar::TopbarAction::ToggleSortDirection => {
                        let key = self.lib.sort.key;
                        self.pick_sort(key);
                    }
                    _ => {}
                }
                self.sort_menu_open = false;
            }
            // 点空白处关掉（本帧刚由按钮打开则豁免，理由同文件夹右键菜单）
            if self.sort_menu_frame != ctx.cumulative_frame_nr()
                && ctx.input(|i| i.pointer.any_click())
            {
                let pos = ctx.input(|i| i.pointer.interact_pos());
                if let Some(p) = pos
                    && !Rect::from_min_size(self.sort_menu_pos, vec2(180.0, 200.0)).contains(p)
                {
                    self.sort_menu_open = false;
                }
            }
        }

        // 视图菜单已在 M3.1 移除：三个视图模式改为顶栏分段控件
        //（TopbarAction::PickViewMode 直接切，无弹层）。

        // 「…」弹出菜单（M3.1 顶栏）
        if self.more_menu_open && self.viewer.is_none() {
            let actions = toolbar::more_menu(
                ui,
                self.more_menu_pos,
                self.lib.recurse,
                self.lib.recurse_available(),
                self.grid_show_filename,
            );
            if !actions.is_empty() {
                self.more_menu_open = false;
            }
            for a in actions {
                use crate::topbar::MoreAction;
                match a {
                    crate::topbar::TopbarAction::More(MoreAction::AddFolder) => self.add_folder(),
                    crate::topbar::TopbarAction::More(MoreAction::ToggleRecurse) => {
                        self.toggle_recurse()
                    }
                    crate::topbar::TopbarAction::More(MoreAction::ToggleFullscreen) => {
                        let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                    }
                    crate::topbar::TopbarAction::More(MoreAction::About) => {
                        self.lib.toast = Some((
                            format!(
                                "OpenPicasa {} · 本地照片库（只读不改原文件）",
                                env!("CARGO_PKG_VERSION")
                            ),
                            3.0,
                        ));
                    }
                    _ => {}
                }
            }
            // 打开帧豁免 + 点空白关闭（与排序菜单同款机制）
            if self.more_menu_frame != ctx.cumulative_frame_nr()
                && ctx.input(|i| i.pointer.any_click())
            {
                let pos = ctx.input(|i| i.pointer.interact_pos());
                if let Some(p) = pos
                    && !Rect::from_min_size(self.more_menu_pos, vec2(200.0, 140.0)).contains(p)
                {
                    self.more_menu_open = false;
                }
            }
        }

        // 照片右键菜单（M4：收藏 / 说明 / 标签）
        if let Some((photo_id, starred, pos)) = self.photo_ctx {
            let mut close = false;
            let mut action: Option<PhotoCtxAction> = None;
            egui::Area::new(egui::Id::unique("photo-ctx-menu"))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::WHITE)
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(0xC0)))
                        .corner_radius(3.0)
                        .inner_margin(egui::Margin::same(4))
                        .shadow(egui::epaint::Shadow {
                            offset: [1, 2],
                            blur: 6,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(40),
                        })
                        .show(ui, |ui| {
                            let star_label = if starred {
                                "☆ 取消收藏"
                            } else {
                                "★ 收藏"
                            };
                            if ui.button(star_label).clicked() {
                                action = Some(PhotoCtxAction::ToggleStar);
                            }
                            if ui.button("说明…").clicked() {
                                action = Some(PhotoCtxAction::EditCaption);
                            }
                            if ui.button("标签…").clicked() {
                                action = Some(PhotoCtxAction::EditTags);
                            }
                        });
                });
            if action.is_some() || ctx.input(|i| i.pointer.any_click()) {
                // 菜单里点了按钮不关（下一帧重开编辑框）；点空白关
                if action.is_none() {
                    close = true;
                }
            }
            if let Some(act) = action {
                match act {
                    PhotoCtxAction::ToggleStar => {
                        match self.lib.batch_set_starred_ids(vec![photo_id], !starred) {
                            Ok(_) => {}
                            Err(e) => tracing::warn!(error = %e, "切换收藏失败"),
                        }
                        close = true;
                    }
                    PhotoCtxAction::EditCaption => {
                        self.caption_editor = Some(photo_id);
                        close = true;
                    }
                    PhotoCtxAction::EditTags => {
                        self.tag_editor = Some(photo_id);
                        close = true;
                    }
                }
            }
            if close {
                self.photo_ctx = None;
            }
        }

        // 说明编辑窗口（M4 #8：说明只存应用库）
        if let Some(photo_id) = self.caption_editor {
            let mut open = true;
            egui::Window::new("照片说明")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .show(ui, |ui| {
                    ui.text_edit_multiline(&mut self.caption_buf);
                    ui.horizontal(|ui| {
                        if ui.button("保存").clicked() {
                            if let Err(e) = self.lib.set_caption(photo_id, &self.caption_buf) {
                                tracing::warn!(error = %e, "保存说明失败");
                            } else {
                                self.lib.toast = Some(("说明已保存".into(), 2.0));
                                self.caption_editor = None; // 直接关闭，绕开 open 的借用
                            }
                        }
                    });
                });
            if !open {
                self.caption_editor = None;
            }
        }

        // 标签编辑窗口（M4 #8：标签只存应用库）
        if let Some(photo_id) = self.tag_editor {
            let mut open = true;
            let existing = self.lib.tags_of(photo_id).unwrap_or_default();
            egui::Window::new("照片标签")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for t in &existing {
                            if ui
                                .button(format!("{t} ×"))
                                .on_hover_text("点击移除这个标签")
                                .clicked()
                            {
                                let _ = self.lib.batch_remove_tag_id(photo_id, t);
                            }
                        }
                        if existing.is_empty() {
                            ui.label("（还没有标签）");
                        }
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.tag_buf);
                        if ui.button("添加").clicked() {
                            let label = self.tag_buf.trim().to_string();
                            if !label.is_empty() {
                                let _ = self.lib.batch_add_tag_id(photo_id, &label);
                                self.tag_buf.clear();
                            }
                        }
                    });
                });
            if !open {
                self.tag_editor = None;
            }
        }

        // 筛选面板（日期/类型/尺寸/标签/收藏 + 批量操作）
        if self.filter_menu_open && self.viewer.is_none() {
            let current = self.lib.filter.clone().unwrap_or_default();
            let has_selection = !self.lib.selection.is_empty();
            let (action, tag_out) = toolbar::filter_menu(
                ui,
                self.filter_menu_pos,
                &current,
                &mut self.filter_draft_tag,
                has_selection,
            );
            if tag_out != self.filter_draft_tag {
                // 文本框的实时回写已发生在 &mut 借用里，这里无需处理
            }
            if let Some(action) = action {
                match action {
                    FilterPanelAction::Apply(f) => {
                        let f = *f;
                        let has = f.starred_only
                            || f.tag.is_some()
                            || f.taken_range.is_some()
                            || f.size_class.is_some()
                            || !f.formats.is_empty();
                        if let Err(e) = self.lib.set_filter(has.then_some(f)) {
                            tracing::warn!(error = %e, "应用筛选失败");
                        }
                        self.filter_menu_open = false;
                        self.restore_scroll = Some(0.0);
                    }
                    FilterPanelAction::Clear => {
                        if let Err(e) = self.lib.set_filter(None) {
                            tracing::warn!(error = %e, "清除筛选失败");
                        }
                        self.filter_menu_open = false;
                        self.restore_scroll = Some(0.0);
                    }
                    FilterPanelAction::BatchStar(on) => match self.lib.batch_set_starred(on) {
                        Ok(n) if n > 0 => {
                            self.lib.toast = Some((
                                format!("{} {n} 张照片", if on { "已收藏" } else { "已取消收藏" }),
                                2.5,
                            ));
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "批量收藏失败"),
                    },
                    FilterPanelAction::BatchAddTag => {
                        let label = self.filter_draft_tag.trim().to_string();
                        if !label.is_empty() {
                            match self.lib.batch_add_tag(&label) {
                                Ok(n) if n > 0 => {
                                    self.lib.toast =
                                        Some((format!("已为 {n} 张打上「{label}」"), 2.5));
                                }
                                Ok(_) => {}
                                Err(e) => tracing::warn!(error = %e, "批量加标签失败"),
                            }
                        }
                    }
                }
            }
            if self.filter_menu_frame != ctx.cumulative_frame_nr()
                && ctx.input(|i| i.pointer.any_click())
            {
                let pos = ctx.input(|i| i.pointer.interact_pos());
                if let Some(p) = pos
                    && !Rect::from_min_size(self.filter_menu_pos, vec2(260.0, 420.0)).contains(p)
                {
                    self.filter_menu_open = false;
                }
            }
        }

        // 文件夹右键菜单
        if let Some((path, pos)) = self.folder_ctx.clone() {
            let mut close = false;
            // 固定 id：`ui.scope_id()` 是 ui 层的哈希，菜单不该随调用点上下的
            // 嵌套层数变化而换身份（换身份会让 Area 每帧重建）
            let area = egui::Area::new(egui::Id::unique("picasa.folder-ctx"))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    egui::Frame::new()
                        .fill(Color32::WHITE)
                        .stroke(egui::Stroke::new(1.0, Color32::from_gray(0xC0)))
                        .corner_radius(3.0)
                        .inner_margin(egui::Margin::same(4))
                        .show(ui, |ui| {
                            ui.set_min_width(200.0);
                            if ui
                                .add(egui::Button::new("在 Finder 中打开").frame(false))
                                .clicked()
                            {
                                let _ = crate::macos::open_folder(std::path::Path::new(&path));
                                close = true;
                            }
                            // ---- 刷新此文件夹（E 导航）：合并去重，连点只跑一次 ----
                            if ui
                                .add(egui::Button::new("刷新此文件夹").frame(false))
                                .clicked()
                            {
                                let queued =
                                    self.index.enqueue_refresh(std::path::Path::new(&path));
                                self.lib.toast = Some((
                                    if queued {
                                        "已加入刷新队列".to_string()
                                    } else {
                                        "这个文件夹正在索引，已合并".to_string()
                                    },
                                    2.0,
                                ));
                                close = true;
                            }

                            let is_root = self
                                .lib
                                .root_paths()
                                .iter()
                                .any(|r| r.to_string_lossy().as_ref() == path.as_str());

                            ui.separator();

                            if is_root {
                                // 根本身：「关闭」保留索引（可秒开），「移除」删索引
                                if ui
                                    .add(
                                        egui::Button::new("关闭关注文件夹（保留索引）")
                                            .frame(false),
                                    )
                                    .clicked()
                                {
                                    self.lib.close_root(std::path::Path::new(&path));
                                    self.board.forget(&path);
                                    self.viewer = None;
                                    self.restore_scroll = Some(0.0);
                                    self.lib.toast =
                                        Some(("已关闭，重新添加同一目录可秒开".into(), 3.0));
                                    close = true;
                                }
                                if ui
                                    .add(
                                        egui::Button::new("从图库移除（删索引，不删文件）")
                                            .frame(false),
                                    )
                                    .clicked()
                                {
                                    self.lib.remove_root(std::path::Path::new(&path));
                                    // 任务跟着关注文件夹一起撤掉，否则面板会留一行"幽灵任务"
                                    self.board.forget(&path);
                                    if let Err(e) = self.lib.reload_tree() {
                                        tracing::warn!(error = %e, "刷新目录树失败");
                                    }
                                    let _ = self.lib.reload_photos();
                                    self.refresh_view_header();
                                    self.viewer = None;
                                    self.restore_scroll = Some(0.0);
                                    close = true;
                                }
                            } else {
                                // 子文件夹：「关闭」= 加排除规则 + 删该子树索引
                                if ui
                                    .add(egui::Button::new("关闭此文件夹（不再显示）").frame(false))
                                    .clicked()
                                {
                                    match self.lib.exclude_folder(std::path::Path::new(&path)) {
                                        Ok(true) => {
                                            self.board.forget(&path);
                                            self.viewer = None;
                                            self.restore_scroll = Some(0.0);
                                            self.lib.toast =
                                                Some(("已关闭。可在「诊断」里恢复".into(), 3.0));
                                        }
                                        Ok(false) => {
                                            self.lib.toast =
                                                Some(("根目录不能这样关闭".into(), 3.0));
                                        }
                                        Err(e) => {
                                            tracing::warn!(error = %e, "关闭子目录失败");
                                            self.lib.toast =
                                                Some(("关闭失败，详见日志".into(), 3.0));
                                        }
                                    }
                                    close = true;
                                }
                            }
                        });
                });
            // 关闭规则（三条），顺序与时序都不能改：
            //   1. 点了菜单项 → 关
            //   2. 本帧刚打开 → **豁免**。右键释放的那一帧 `secondary_clicked()` 与
            //      `pointer.any_click()` 同时为真，不豁免就会「刚画出来就被自己关掉」
            //   3. 点击落在菜单矩形外 → 关。用 Area 实测矩形，不猜尺寸
            let opened_this_frame = self.folder_ctx_frame == ctx.cumulative_frame_nr();
            if !opened_this_frame {
                let menu_rect = area.response.rect;
                let clicked_outside = ctx.input(|i| i.pointer.any_click())
                    && ctx
                        .input(|i| i.pointer.interact_pos())
                        .is_some_and(|p| !menu_rect.contains(p));
                if close || clicked_outside {
                    self.folder_ctx = None;
                }
            }
        }

        // 删除二次确认（R-1 的唯一例外，文案必须写清「可在系统里还原」）
        if let Some(n) = self.confirm_trash {
            let mut open = true;
            egui::Window::new("移到废纸篓？")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .open(&mut open)
                .show(&ctx, |ui| {
                    ui.label(format!(
                        "将把 {n} 张照片移到系统废纸篓。\n\n可以在 Finder 的废纸篓里还原，我们不会直接删除文件。"
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("取消").clicked() {
                            self.confirm_trash = None;
                        }
                        if ui.button("移到废纸篓").clicked() {
                            let (ok, errors) = self.lib.trash_selected();
                            self.confirm_trash = None;
                            self.tray_rows_dirty = true;
                            self.restore_scroll = None;
                            if !errors.is_empty() {
                                self.lib.toast =
                                    Some((format!("{ok} 张成功，{} 张失败：{}", errors.len(), errors[0]), 6.0));
                            }
                        }
                    });
                });
            if !open {
                self.confirm_trash = None;
            }
        }

        // 诊断页（F 加固）：缓存占用 / 清理、失败记录、排除项、全量重建
        if self.diagnostics_open {
            if self.diag_dirty {
                self.gather_diag_data();
            }
            let mut open = self.diagnostics_open;
            let act = crate::diagnostics::show(
                &ctx,
                &theme,
                &mut open,
                &self.diag_data,
                &mut self.confirm_rebuild,
            );
            self.diagnostics_open = open;
            if let Some(a) = act {
                self.apply_diag_action(a, &ctx);
            }
        }

        // toast（轻量提示）
        //
        // 这里**故意不用 `Area::anchor`**：Area 的定位用的是「上一帧的尺寸」，
        // 首帧尺寸未知会先落到一个错误位置，而窄宽约束还会把一行提示折成两行
        // （实测出现过「索引完 / 成：9 张」）。改为自己算 rect + 直接用 painter 画，
        // 位置与换行都由我们决定，且不需要额外一帧才收敛。
        if let Some((msg, ttl)) = self.lib.toast.clone() {
            let now = ctx.input(|i| i.time);
            let remaining = ttl - now;
            // 查看器是「沉浸式」的：提示框会直接压住照片（实测第一版就挡住了主体），
            // 所以查看器打开时不画 toast——状态栏和索引结果在退出查看器后仍然可见。
            if remaining <= 0.0 || self.viewer.is_some() {
                if remaining <= 0.0 {
                    self.lib.toast = None;
                }
            } else {
                let galley = ui.painter().layout_no_wrap(
                    msg,
                    egui::FontId::proportional(12.0),
                    Color32::WHITE,
                );
                let pad = vec2(12.0, 6.0);
                // egui 0.37 移除了 `Context::screen_rect()`，视口矩形改从 InputState 取
                let screen = ctx.input(|i| i.viewport_rect());
                let size = galley.size() + pad * 2.0;
                let rect = Rect::from_center_size(
                    egui::pos2(screen.center().x, screen.bottom() - 48.0 - size.y / 2.0),
                    size,
                );
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    egui::Id::unique("toast-layer"),
                ));
                painter.rect_filled(rect, 4.0, Color32::from_black_alpha(190));
                painter.galley(rect.min + pad, galley, Color32::WHITE);
                tracing::debug!(?rect, ?screen, "toast 位置");
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }

        // 错误横幅
        if let Some(err) = self.lib.last_error.clone() {
            egui::Area::new(ui.scope_id().with("error-banner"))
                .anchor(Align2::CENTER_TOP, vec2(0.0, Metrics::TOOLBAR_H + 8.0))
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    egui::Frame::new()
                        .fill(Color32::from_rgb(0xC0, 0x39, 0x2B))
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(12, 6))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(&err).color(Color32::WHITE).size(12.0),
                                );
                                if ui.small_button("×").clicked() {
                                    self.lib.last_error = None;
                                }
                            });
                        });
                });
        }

        // ---- M2 索引进度：右下角弹框 + 进度面板（都是浮层，不参与布局、不抢焦点）----
        let mut action = self.progress.show_toast(&ctx, &self.theme);
        if action.is_none() {
            action = self
                .progress
                .show_panel(&ctx, &self.theme, self.index.is_paused());
        }
        match action {
            Some(ProgressAction::AddFolder) => self.add_folder(),
            Some(ProgressAction::Retry(path)) => self.retry_folder(&path),
            Some(ProgressAction::RetryAll) => self.retry_all(),
            Some(ProgressAction::PauseIndex) => {
                self.index.pause();
                self.lib.toast = Some(("索引已暂停（已建索引保留，可继续）".into(), 3.0));
            }
            Some(ProgressAction::ResumeIndex) => {
                self.index.resume();
                self.lib.toast = Some(("索引已继续".into(), 2.0));
            }
            Some(ProgressAction::DeleteIndex) => self.confirm_delete_index = true,
            Some(ProgressAction::OpenManager) => {
                // 打开前先刷一次：从弹框过来时用户期待看到**当下**的数据
                self.refresh_mgmt();
                self.index_mgr
                    .open_at(&self.mgmt, self.mgmt.failed_task_count);
            }
            Some(ProgressAction::ClosePanel) | None => {}
        }

        // ---- M2.2 §4.1：索引管理页（四个标签，唯一入口打开）
        //
        // ⚠️ 放在 `confirm_delete_index` **之前**：页面里的「删除索引」会把
        // `confirm_delete_index` 置真，下一帧才能画出确认框（同帧画不出来——
        // egui 的 Window 是在调用点当帧绘制的）。
        if self.index_mgr.open {
            // 与诊断页对齐库大小 / 照片数 / 缩略图字节。必须在画窗口之前，
            // 否则这一帧仍是默认的 0 B / 「计算中…」。
            self.ensure_index_storage();
            // 缩略图按根分布是慢查询，只在页面开着时、由后台线程算（死机修复的关键）。
            self.request_thumb_storage(false);
            if self.thumb_rx.is_some() || !self.mgmt.storage.thumb_stats_known {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            let now = chrono::Utc::now().timestamp();
            let actions = crate::index_mgr::show(
                &ctx,
                &self.theme,
                &mut self.index_mgr,
                &self.mgmt,
                self.mgmt.failed_task_count,
                now,
            );
            if !actions.is_empty() {
                self.handle_mgr_actions(actions);
            }
        }

        if self.confirm_delete_index {
            self.draw_delete_index_confirm(&ctx);
        }

        if let Some(n) = self.confirm_rebuild_failed {
            self.draw_rebuild_failed_confirm(&ctx, n);
        }

        let _ = theme;
    }

    /// 「删除索引」的二次确认。
    ///
    /// 为什么必须二次确认：这是**唯一**会让用户觉得"照片被删了"的操作。
    /// 事实上只删索引（磁盘上的照片一张不动），但界面上看起来就是图库空了，
    /// 所以文案必须无条件说清"不动磁盘文件"，按钮必须默认不执行。
    fn draw_delete_index_confirm(&mut self, ctx: &egui::Context) {
        let mut go = false;
        let mut cancel = false;
        egui::Window::new("删除索引")
            .id(egui::Id::unique("delete-index-confirm"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.label(
                    RichText::new("将删除已建立的照片索引与缩略图。")
                        .size(12.5)
                        .color(self.theme.text_primary),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "磁盘上的照片文件不会被删除；关注文件夹与「关闭此文件夹」的规则会保留。",
                    )
                    .size(11.0)
                    .color(self.theme.text_secondary),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("删除索引").clicked() {
                        go = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        if go {
            self.confirm_delete_index = false;
            if self.index.delete_index() {
                self.lib.toast = Some(("开始删除索引…".into(), 3.0));
            } else {
                self.lib.toast = Some(("已在删除中，请稍候".into(), 3.0));
            }
        }
        if cancel {
            self.confirm_delete_index = false;
        }
    }

    /// 「重建失败索引」的二次确认。
    ///
    /// 为什么值得一次确认：它不是零成本的。970 张按实测 111 ms/张、2 线程算约 1 分钟，
    /// 但用户看不到这个数字，只会看到"CPU 一直在跑"。先把数量和耗时说清楚，
    /// 免得他以为程序又卡死了（这轮修的就是"卡死"的第一印象）。
    fn draw_rebuild_failed_confirm(&mut self, ctx: &egui::Context, n: u64) {
        let mut go = false;
        let mut cancel = false;
        // 单张 111 ms / 2 线程 ≈ 每 18 张 1 秒；向上取整到分钟
        let secs = (n as f64 * 0.111 / 2.0).max(1.0);
        let eta = if secs < 90.0 {
            format!("约 {:.0} 秒", secs)
        } else {
            format!("约 {:.0} 分钟", (secs / 60.0).ceil())
        };
        egui::Window::new("重建失败索引")
            .id(egui::Id::unique("rebuild-failed-confirm"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(
                    RichText::new(format!("将重新解码 {n} 张缩略图失败的照片。"))
                        .size(12.5)
                        .color(self.theme.text_primary),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "只需 {eta}。已成功的缩略图不受影响，其他「待生成」的照片也不会被顺带处理。\
                         期间可以用「暂停索引」随时中止。"
                    ))
                    .size(11.0)
                    .color(self.theme.text_secondary),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("开始重建").clicked() {
                        go = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        if go {
            self.confirm_rebuild_failed = None;
            self.rebuild_failed_index();
        }
        if cancel {
            self.confirm_rebuild_failed = None;
        }
    }

    /// 「重建失败索引」：只重置并重做**失败的那些**，一张历史欠账都不碰。
    ///
    /// 关键顺序：**先按关注根分组，再只重置要入队的那些 id**。
    /// 反过来（先全库重置再分组）会让"没匹配到任何关注根"的失败项变成「待生成」，
    /// 而启动扫描只补新增、永远不管待生成——那批照片就此变成永久灰格子，
    /// 比留着失败状态还糟（失败至少能在明细里查到）。
    fn rebuild_failed_index(&mut self) {
        let rows = match self.lib.db() {
            Ok(db) => db.failed_photos(None).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        if rows.is_empty() {
            self.lib.toast = Some(("没有失败项，无需重建".into(), 3.0));
            self.refresh_failed_thumbs();
            return;
        }

        let roots = self.lib.root_paths();
        let mut total_reset = 0u64;
        let mut queued = 0u64;
        for root in roots {
            let prefix = root.to_string_lossy().to_string();
            let ids: Vec<i64> = rows
                .iter()
                .filter(|r| {
                    r.folder_path == prefix || r.folder_path.starts_with(&format!("{prefix}/"))
                })
                .map(|r| r.id)
                .collect();
            if ids.is_empty() {
                continue;
            }
            let reset = match self.lib.db() {
                Ok(db) => db.reset_failed_thumbs_by_ids(&ids).unwrap_or(0),
                Err(_) => 0,
            };
            if reset == 0 {
                continue;
            }
            total_reset += reset;
            self.index.enqueue_rebuild_failed(ids, &root);
            queued += 1;
        }

        if total_reset == 0 {
            self.lib.toast = Some(("失败项都不在关注文件夹内，无法重建".into(), 4.0));
        } else {
            self.lib.toast = Some((
                format!("开始重建 {total_reset} 张失败缩略图（{queued} 个任务）"),
                4.0,
            ));
            // 角标立即反映新状态：这批已经不是失败了
            self.failed_thumbs = self.failed_thumbs.saturating_sub(total_reset);
            self.progress.open_panel();
        }
        tracing::info!(reset = total_reset, tasks = queued, "重建失败索引");
    }

    /// 重试一个关注文件夹的失败项：库里只把失败项重置为待生成，然后入队新批次。
    fn retry_folder(&mut self, path: &str) {
        let folder_id = match self.lib.db() {
            Ok(db) => db.folder_id(path).ok().flatten(),
            Err(_) => None,
        };
        let Some(id) = folder_id else {
            self.lib.toast = Some(("目录尚未入库，无法重试".into(), 3.0));
            return;
        };
        let reset = match self.lib.db() {
            Ok(db) => db.reset_failed_thumbs_subtree(id).unwrap_or(0),
            Err(_) => 0,
        };
        if reset == 0 {
            self.lib.toast = Some(("没有可重试的失败项".into(), 3.0));
            return;
        }
        self.index.enqueue_retry(id, std::path::Path::new(path));
        tracing::info!(path, reset, "重试失败项");
    }

    fn retry_all(&mut self) {
        let paths: Vec<String> = self
            .progress
            .snapshot()
            .tasks
            .iter()
            .filter(|t| t.failed > 0)
            .map(|t| t.path.clone())
            .collect();
        for p in paths {
            self.retry_folder(&p);
        }
    }

    fn pick_sort(&mut self, key: SortKey) {
        if let Err(e) = self.lib.set_sort(key) {
            tracing::warn!(error = %e, "排序失败");
        }
        self.restore_scroll = Some(0.0);
    }

    fn add_folder(&mut self) {
        let start = self
            .lib
            .root_paths()
            .first()
            .map(|p| p.parent().unwrap_or(p).to_path_buf())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Pictures")));
        // 原生 NSOpenPanel：TCC 授权的唯一正规入口（F10-25 / R-3）。
        // Linux 冒烟走同一条 `rfd` 路径；没选到路径时不能静默返回。
        let Some(dir) = crate::macos::pick_folder(start.as_deref()) else {
            let (msg, ttl) = crate::macos::pick_folder_none_toast();
            tracing::info!(msg, "选择文件夹未返回路径");
            self.lib.toast = Some((msg.to_string(), ttl));
            return;
        };
        self.lib.add_root(&dir);
        self.lib.save_prefs();
        if let Err(e) = self.lib.reload_tree() {
            tracing::warn!(error = %e, "刷新目录树失败");
            self.lib.toast = Some(("无法读取该目录".into(), 4.0));
        }
        // 新关注文件夹 = **新批次**，且只索引它（不重扫已有文件夹）：
        // 重扫全部会让已完成的任务重新排队，进度条凭空回到 0（`04` §1.2.1）。
        self.index.enqueue_scan(&[dir]);
    }

    /// 分页补齐：一帧一页（C1）。
    ///
    /// ## 为什么要「一帧一页」而不是「一次补完」
    ///
    /// 补齐 5 万张要读十几批，一次做完就是 60~100 ms 的停顿——和 M2 的首屏卡顿
    /// 是同一个毛病，只是换了个位置犯。摊到每帧一页（约 5 ms）之后，
    /// 滚动与补页共享同一帧预算，用户感知不到。
    ///
    /// 补完一页**必须主动要下一帧**：egui 是事件驱动的，鼠标不动就不会有帧，
    /// 那补齐会在用户「停下来看」的那一刻正好停住，剩下的照片永远不出现。
    fn extend_photos_one_page(&mut self, ctx: &egui::Context) {
        if self.lib.photos_complete || self.lib.photos_remaining() == 0 {
            return;
        }
        match self.lib.extend_photos() {
            Ok(true) => {
                self.refresh_view_header();
                ctx.request_repaint();
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, "分页补齐失败，停止补页");
                self.lib.photos_complete = true;
            }
        }
    }

    fn maybe_autosave(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if now - self.last_autosave < 2.0 {
            return;
        }
        self.last_autosave = now;
        if self.lib.prefs_dirty() {
            self.lib.save_prefs();
        }
        // 让后台事件（缩略图补图）能及时上屏；索引进行中按 100 ms 唤醒，
        // 数字刷新仍在 `IndexProgressUi` 里被压到 4 Hz（`04` §3.4）
        if self.board.running() || !self.cache.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// 已加载照片的拍摄日期跨度（§7.3 副标题）。
///
/// 只读 `PhotoRow::effective_taken_at()`，不查库；没有有效拍摄时间时返回 `None`，
/// 副标题退化为纯张数。同一时刻会先归一化成「YYYY年M月」再比较，避免跨度两端
/// 落在同一个月却显示成「5月 – 5月」。
fn photo_date_span(photos: &[PhotoRow]) -> Option<String> {
    let mut lo = i64::MAX;
    let mut hi = i64::MIN;
    for p in photos {
        let t = p.effective_taken_at();
        if t > 0 {
            lo = lo.min(t);
            hi = hi.max(t);
        }
    }
    if lo == i64::MAX {
        return None;
    }
    let fmt = |secs: i64| {
        chrono::DateTime::from_timestamp(secs, 0).map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y年%-m月")
                .to_string()
        })
    };
    let a = fmt(lo);
    let b = fmt(hi);
    match (a, b) {
        (Some(a), Some(b)) if a == b => Some(a),
        (Some(a), Some(b)) => Some(format!("{a} – {b}")),
        _ => None,
    }
}

/// 从 `settings.grid_cell_px` 读滑块值（`07-A` ZOOM-06）。
///
/// 读不到（库没建好 / 值坏）就返回 `None` → 用默认刻度。
/// **不在这里报警**：滑块位置读不到不影响任何功能，只为它写一条日志反而是噪音。
fn load_zoom_setting(lib: &Library) -> Option<f32> {
    let db = lib.db().ok()?;
    let v = db.grid_cell_px().ok()??;
    Some(v as f32)
}

impl eframe::App for OpenPicasaApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.theme = Theme::picasa();

        // M3.1：macOS 上把红绿灯嵌进自绘顶栏（与 `with_fullsize_content_view`
        // 配套）。AppKit 每次窗口布局都会重置按钮位置，所以**每帧**都要调；
        // 其他平台此调用是空操作。
        // 双行顶栏：红绿灯与面包屑同在上行（§7 像素校准）。
        frame.set_traffic_lights_position(
            &ctx,
            egui::Rangef::new(0.0, Metrics::TOPBAR_LINE1_H),
            14.0,
        );

        self.pump_events(&ctx);
        // M3：监视器同步（幂等，只在根集合真的变了时才装/卸）
        if self.frames.is_multiple_of(120) {
            self.sync_watchers();
        }
        self.tick_watchers();
        // 每帧刷新 DPI（多屏移动窗口时会变），再结算滑块
        self.dpi_scale = ctx.pixels_per_point();
        self.tick_zoom();
        // M3：微缩图集（只在 ≤48 px 档工作）
        self.tick_atlas(&ctx);
        self.flush_reload(&ctx);
        self.handle_keys(&ctx);
        // 索引进度：按 250 ms 节流取快照（不在 UI 线程做任何索引工作）
        self.progress.tick(&ctx, &self.board);
        // §4.8 启动增量校验：首屏之后启动，之后每帧推进一步（**内部自己限速**）
        self.tick_startup_verify(&ctx);
        // M3：L2 周期全校（每 6/24 h 一次，等界面空闲才动手）
        self.tick_full_verify(&ctx);

        // 调试入口：等照片装载好再进查看器（首帧时 photos 还是空的）
        // 45 帧 ≈ 0.7s：扫描是逐目录入库的，太早会拿到半截列表（实测拿到过 3/9）。
        self.frames = self.frames.saturating_add(1);
        if let Some(index) = self.open_initial
            && self.frames > 45
            && !self.lib.photos.is_empty()
            && self.viewer.is_none()
        {
            let index = index.min(self.lib.photos.len() - 1);
            self.open_viewer(index);
            self.open_initial = None;
        }

        if self.viewer.is_some() {
            self.draw_viewer(ui);
        } else {
            self.draw_library(ui);
            // 首屏之外的剩余照片：**每帧只补一页**，补完再要一帧。
            //
            // 放在栅格之后，是为了保证「这一帧先画用户看得见的东西」——
            // 补齐是锦上添花，绝不能因为它在画第一格之前先卡 8ms。
            self.extend_photos_one_page(&ctx);
        }
        self.draw_overlays(ui);
        self.maybe_autosave(&ctx);

        // 截图模式：预热到位后请求一帧，写在回调里，然后关窗
        if let Some(cap) = &mut self.capture {
            cap.tick(&ctx);
        }
        // 容器路径在这里被读一次：既避免「字段未使用」，也让启动日志能定位数据位置
        tracing::debug!(support = %self.paths.support.display(), "应用容器");

        // 让「鼠标静止 3 秒」与滚动惯性有帧可跑
        if self.viewer.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.lib.save_prefs();
    }

    fn on_exit(&mut self) {
        self.index.stop();
        // M22-PERF-04：退出前把 ANALYZE 结果落进 sqlite_stat1，下次启动不必重跑。
        // 失败无所谓——只是少一点查询计划优化，不能因此拦住退出。
        if let Ok(db) = self.lib.db() {
            let _ = db.optimize();
        }
        self.lib.save_prefs();
        tracing::info!("退出：偏好已保存");
    }
}

/// 照片右键菜单的动作。
enum PhotoCtxAction {
    ToggleStar,
    EditCaption,
    EditTags,
}
