//! 应用主结构：把「库会话（core）」「纹理缓存」「后台事件」「五区界面」缝在一起。
//!
//! ## 这个文件只做编排，不做算法
//!
//! 排序/选择/扫描/解码都在 `picasa-core`；布局/绘制在 `grid`/`sidebar`/`viewer`。
//! 这里负责：**面板嵌套顺序**、**事件泵**、**动作分发**、**偏好落盘**。

use std::collections::HashMap;
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

/// M4.2 `M42-UI-06`：批量标签浮层三态结果缓存。
///
/// `key` = 选中集 id（排序后）的指纹；命中则**零查库**直接复用 `all_tags`/`counts`。
/// 任何标签写操作（`changed`）后由调用方清空，下一帧按新库状态重算一次
/// （`all_tags()` + `tags_for_ids()`，恰好 2 次查库、与标签数量无关），
/// 满足 AC-01「打开帧查库次数恒定/0」、AC-02「变更后重开三态正确」、
/// AC-03「不引入每帧重绘、失效判定不持 Mutex<Db> 做 I/O」。
struct TagTriCache {
    key: Vec<i64>,
    all_tags: Vec<(String, i64)>,
    counts: HashMap<String, usize>,
}

/// M4.2 `M42-UI-06`：由选中集算缓存指纹（排序后比较，与 `HashSet` 迭代顺序无关）。
fn tag_tri_key(ids: &[i64]) -> Vec<i64> {
    let mut key = ids.to_vec();
    key.sort_unstable();
    key
}

/// M4.2 `M42-UI-04`：左栏「＋」新建会话的种类。
///
/// 标签与相册的语义不同（见 `docs/16` §3 实装口径）：相册有独立定义行，
/// `create_album` 可直接建；标签只是 `(photo_id,label)` 关联表，没有独立定义，
/// 「新建标签」= 对当前选中集建首条关联。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NewItemKind {
    Tag,
    Album,
}

/// M4.2 `M42-UI-04`：新建名称合法性（去空白后非空）。纯函数便于单测。
fn new_item_name_valid(raw: &str) -> bool {
    !raw.trim().is_empty()
}

/// M4.2 `M42-UI-05`：按相册名反查 id（`refresh_sidebar_groups` 里另存了 name→id）。
fn album_id_by_name(ids: &[(String, i64)], name: &str) -> Option<i64> {
    ids.iter().find(|(n, _)| n == name).map(|(_, id)| *id)
}

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
    /// M4：标签 / 相册分组折叠状态（会话级）
    tags_collapsed: bool,
    albums_collapsed: bool,
    folder_ctx: Option<(String, Pos2)>,
    /// M4.1：说明编辑窗口（`Some(-1)` = 多选集模式；否则为单张 photo_id）
    caption_editor: Option<i64>,
    /// M4.1：批量标签浮层（`Some(-1)` = 选中集模式）
    tag_editor: Option<i64>,
    /// M4.2 `M42-UI-06`：批量标签浮层三态缓存（打开帧命中即零查库）。
    tag_tri_cache: Option<TagTriCache>,
    /// 编辑框草稿
    caption_buf: String,
    tag_buf: String,
    /// 文件夹右键菜单在哪一帧打开（打开帧豁免，避免「刚画出来就被自己关掉」）。
    folder_ctx_frame: u64,
    /// M4.2 `M42-UI-05`：相册右键菜单（相册名、屏幕位置）与打开帧。
    album_ctx: Option<(String, Pos2)>,
    album_ctx_frame: u64,
    /// M4.2 `M42-UI-04`：「＋」新建会话（种类 + 草稿）。
    new_item: Option<NewItemKind>,
    new_item_buf: String,
    /// M4.2 `M42-UI-05`：相册改名 / 删除二次确认会话。
    rename_album: Option<String>,
    rename_album_buf: String,
    confirm_delete_album: Option<String>,
    /// M4.2 `M42-UI-05`：相册 name→id 映射（`refresh_sidebar_groups` 维护）。
    /// `sidebar_albums` 只留 `(name,count)` 给左栏绘制，id 单独存这里供 CRUD。
    sidebar_album_ids: Vec<(String, i64)>,
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
    /// M4.2 `M42-PERF-00`：展示性能探针（被动累计，零副作用、不触发重绘）。
    /// 每帧在 `ui()` 顺手 `note_frame`，诊断页打开时取快照渲染。
    perf: crate::diagnostics::PerfProbe,
    /// 首屏 / 可见区首张出图的计时起点（进程启动时刻）。
    perf_start: std::time::Instant,
    /// 首屏出图只记一次（第一帧画出照片网格时）。
    perf_first_screen_done: bool,
    /// 可见区首张出图只记一次（第一帧有真实缩略图、非全部占位时）。
    perf_first_visible_done: bool,
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
    /// M4.2 `M42-PERF-01`：当前跑着的后台导入 / 导出任务（`None` = 空闲）。
    /// `ImportTask` 含 `Receiver`/`JoinHandle`，不可 Clone/Debug，故用 `Option` 持有。
    import_task: Option<crate::import_task::ImportTask>,
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
    /// M4 EXIF 回填：**后台线程**收件口（`None` = 当前没有在算）。
    /// ⚠️ 绝不在 UI 线程 `probe()` 文件——那会抢缩略图解码的帧预算，
    /// 实测表现为「索引和展示缩略图变慢」。
    exif_rx: Option<Receiver<(usize, usize)>>,
    /// 是否还有待回填行（后台批次跑完后由结果置 false）
    m4_exif_pending: bool,
    /// M4 左栏标签 / 相册列表缓存（每帧绘制只读内存，不查库）。
    /// 在数据集变化时由 `refresh_sidebar_groups()` 失效重取。
    sidebar_tags: Vec<(String, i64)>,
    /// IDX-22 P1「本视图 N」：当前视图范围内的标签计数（与 `sidebar_tags` 同一次刷新）。
    sidebar_tags_in_view: Vec<(String, i64)>,
    /// IDX-22 ⑧：标签计数在**后台线程**算，UI 帧只读缓存。
    ///
    /// 25 万张库上这是一次全库聚合（稳态 15 ms、冷启动 27 ms），
    /// 占着 UI 线程就等于每帧都要赌它还赶得上 16 ms 的预算。
    tag_counts: crate::tag_counts_task::TagCountsService,
    /// 已应用的后台结果 revision（丢弃乱序到达的旧结果）。
    tag_counts_applied: u64,
    sidebar_albums: Vec<(String, i64)>,
    /// IDX-21：左栏「导入的索引」分组数据（App 缓存，不每帧查库）。
    sidebar_import_groups: Vec<crate::sidebar::ImportGroup>,
    /// IDX-21：「导入的索引」分组折叠状态。
    imported_collapsed: bool,
    /// IDX-21 ⑤：导入来源节点右键菜单（来源 id、屏幕位置）。
    import_ctx: Option<(i64, egui::Pos2)>,
    import_ctx_frame: u64,
    /// IDX-21 ⑤：等待确认移除的导入来源（确认框走 IDX-11 同一套文案口径）。
    confirm_import_source: Option<i64>,
    sidebar_groups_dirty: bool,
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
        // IDX-22 ⑧：后台计数线程自己开连接，所以只要路径，不要 `Db`（它非 Send）。
        let db_file = paths.db_file();

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
            sidebar_import_groups: Vec::new(),
            imported_collapsed: false,
            import_ctx: None,
            import_ctx_frame: 0,
            confirm_import_source: None,
            tags_collapsed: false,
            albums_collapsed: false,
            folder_ctx: None,
            caption_editor: None,
            tag_editor: None,
            tag_tri_cache: None,
            caption_buf: String::new(),
            tag_buf: String::new(),
            folder_ctx_frame: u64::MAX,
            album_ctx: None,
            album_ctx_frame: u64::MAX,
            new_item: None,
            new_item_buf: String::new(),
            rename_album: None,
            rename_album_buf: String::new(),
            confirm_delete_album: None,
            sidebar_album_ids: Vec::new(),
            reload_dirty: false,
            last_reload: Instant::now(),
            confirm_trash: None,
            diagnostics_open: false,
            diag_data: crate::diagnostics::DiagData::default(),
            diag_dirty: false,
            perf: crate::diagnostics::PerfProbe::default(),
            perf_start: std::time::Instant::now(),
            perf_first_screen_done: false,
            perf_first_visible_done: false,
            confirm_rebuild: false,
            confirm_delete_index: false,
            confirm_rebuild_failed: None,
            index_mgr: crate::index_mgr::IndexMgrUi::new(),
            import_task: None,
            mgmt: picasa_core::MgmtSnapshot::default(),
            offline_probe: picasa_core::default_offline_probe(),
            mgmt_next_refresh: std::time::Instant::now(),
            thumb_storage: None,
            thumb_rx: None,
            thumb_req_at: None,
            exif_rx: None,
            m4_exif_pending: true, // 启动时也回填一次（老库升级到 v8 后 status 全为 0）
            sidebar_tags: Vec::new(),
            sidebar_tags_in_view: Vec::new(),
            tag_counts: crate::tag_counts_task::TagCountsService::new(db_file),
            tag_counts_applied: 0,
            sidebar_albums: Vec::new(),
            sidebar_groups_dirty: true,
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
        // M4.3 IDX-03：清掉上次崩溃 / 取消留下的 staging 副本（只读目录、不碰照片）；
        // 同时 IDX-06 从 settings 读回「最近成功备份」，让重启后仍显示。
        let cleaned = picasa_db::cleanup_staging(&app.paths.support);
        if cleaned > 0 {
            tracing::info!(staging = cleaned, "启动时清理残留的导入暂存文件");
        }
        if let Ok(db) = app.lib.db()
            && let Some(info) = crate::index_mgr::load_last_backup(&db)
        {
            app.index_mgr.io.last_backup = Some(info);
        }
        // M4.3 §3A ⑦ 重启语义：上次会话的导入 / 导出**没有跑完**时，标记行还在。
        //  - 导出：目标旁可能留下 `.tmp` 产物 → 清掉（只在用户选的导出目录，绝不碰照片），
        //    并置横幅「上次导出未完成，可一键重来」。
        //  - 导入：事务提交前中断 == 整库回滚，置横幅「现有索引未改动，可重新开始」。
        //  - 已提交的导入结果：单独一行摘要，重启后仍显示完成结果。
        if let Ok(db) = app.lib.db() {
            if let Ok(Some(json)) = db.setting_raw("m43_export_pending")
                && let Some(pending) = crate::index_mgr::InterruptedExport::from_json(&json)
            {
                let removed = picasa_db::cleanup_export_tmp(std::path::Path::new(&pending.target));
                if removed > 0 {
                    tracing::info!(removed, target = %pending.target, "清理上次导出中断的临时产物");
                }
                app.index_mgr.io.interrupted_export = Some(pending);
            }
            if let Ok(Some(_)) = db.setting_raw("m43_import_pending") {
                app.index_mgr.io.interrupted_import = true;
            }
            if let Ok(Some(json)) = db.setting_raw("m43_import_summary")
                && let Some(summary) = crate::index_mgr::ImportSummary::from_json(&json)
            {
                app.index_mgr.io.last_import = Some(summary);
            }
        }
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
            // M4.3 阶段 1：在 start_index() **之前**数出上次进程遗留的
            // 未完成任务（scanner 会在本次启动把它们自动续跑）。放在前面
            // 才不会把本次刚创建的任务也算进去。
            let n = app
                .lib
                .db()
                .ok()
                .and_then(|db| db.unfinished_index_tasks().ok())
                .map(|v| v.len())
                .unwrap_or(0);
            if n > 0 {
                app.lib.toast = Some((
                    format!("检测到 {n} 个上次未完成的索引任务，已自动续跑（暂停/取消的任务保持原状）"),
                    5.0,
                ));
            }
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
        // 校验是逐目录推进的分帧任务，静态界面不会自己重绘；只要还有 runner
        // 在跑就持续唤醒下一帧（IDX-13 的 UI 触发场景尤其需要，不能只靠启动时一次）。
        ctx.request_repaint_after(std::time::Duration::from_millis(120));
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
                // M4.3 阶段 1：逐任务暂停 / 继续 / 取消，带库里的 task_id，
                // 由 scanner 的 IndexService 落到持久化意图，不影响其它任务。
                MgrAction::PauseTask(id) => {
                    if self.index.pause_task(id) {
                        self.lib.toast = Some(("该索引任务已暂停（其它任务不受影响）".into(), 3.0));
                        self.mgmt_next_refresh = std::time::Instant::now();
                    }
                }
                MgrAction::ResumeTask(id) => {
                    if self.index.resume_task(id) {
                        self.lib.toast = Some(("该索引任务已继续（从检查点续跑）".into(), 3.0));
                        self.mgmt_next_refresh = std::time::Instant::now();
                    }
                }
                MgrAction::CancelTask(id) => {
                    if id != 0 && self.index.cancel_task(id, "用户取消") {
                        self.lib.toast = Some((
                            "该索引任务已取消（已建索引与用户标记保留）".into(),
                            3.0,
                        ));
                        self.mgmt_next_refresh = std::time::Instant::now();
                    }
                }
                MgrAction::RetryKind(kind) => self.retry_failure_kind(kind),
                MgrAction::IgnoreKind(kind) => self.set_failure_kind_ignored(kind, true),
                MgrAction::UnignoreKind(kind) => self.set_failure_kind_ignored(kind, false),
                MgrAction::RefreshRoot(id) | MgrAction::ReindexRoot(id) => self.reindex_root(id),
                // IDX-13：强制重扫（跳过 L1 直接 L2），非破坏性、无需确认框。
                MgrAction::ForceRescan(id) => self.force_rescan_root(id),
                // IDX-11：「全量重建」先弹作用域确认框，确认后才执行。
                MgrAction::RequestRebuild(id) => {
                    let (path, photos, markers) = self.scope_confirm_data(id);
                    self.index_mgr.confirm = crate::index_mgr::Confirm::RebuildRoot {
                        id,
                        path,
                        photos,
                        markers,
                    };
                }
                MgrAction::RebuildRoot(id) => self.rebuild_root(id),
                MgrAction::CloseRoot(id) => self.set_root_closed(id, true),
                MgrAction::ReopenRoot(id) => self.set_root_closed(id, false),
                MgrAction::RevealRoot(id) => self.reveal_root(id),
                // 「删除索引」按钮只弹**作用域**确认框，绝不再丢弃 id 转全库
                // `self.confirm_delete_index = true`（M4.3 阶段 0 止血）。
                // IDX-11：路径 + 照片数/标记数（未知显示「待计算」，绝不填 0）。
                //
                // m10756：这个按钮现在承担**彻底删除**——确认后连 `roots` 行一起删，
                // 索引管理里不再留「索引已移除」空壳（用户明示取消旧的
                // 「留一行、可一键恢复」语义，见 docs/18 v0.7-dev30）。
                MgrAction::DeleteIndexRoot(id) => {
                    let (path, photos, markers) = self.scope_confirm_data(id);
                    self.index_mgr.confirm = crate::index_mgr::Confirm::DeleteIndex {
                        id,
                        path,
                        photos,
                        markers,
                    };
                }
                // 确认框点「确认」→ 真正的彻底删除（带稳定 root id）。
                MgrAction::ConfirmDeleteIndexRoot(id) => self.delete_index_root(id),
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
                // M4-BUG-04：诊断日志导出接线（§4.9 / docs/11 §1.5）
                MgrAction::ExportDiagLog => self.m4_export_diag_log(),
                // m10756：两个「清空」各自先弹确认框，确认后才真正动。
                MgrAction::ClearRunLog => {
                    self.index_mgr.confirm = crate::index_mgr::Confirm::ClearRunLog;
                }
                MgrAction::ClearTaskHistory => {
                    self.index_mgr.confirm = crate::index_mgr::Confirm::ClearTaskHistory;
                }
                MgrAction::ConfirmClearRunLog => self.clear_run_log(),
                MgrAction::ConfirmClearTaskHistory => self.clear_task_history(),
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
                // ---- M4：索引导入 / 导出（docs/11 §2、docs/12 S2～S7）----
                MgrAction::ExportIndex { with_thumbs } => self.m4_export_index(with_thumbs),
                MgrAction::RevealBackup => self.reveal_last_backup(),
                MgrAction::PickImportFile => self.m4_pick_import_file(),
                MgrAction::SetImportStrategy(s) => {
                    self.index_mgr.io.strategy = s;
                }
                MgrAction::StartImport => self.m4_start_import(),
                MgrAction::CancelImport => self.m4_cancel_import(),
                MgrAction::ReplayExport => self.m4_replay_export(),
                MgrAction::DismissInterruptedExport => {
                    self.index_mgr.io.interrupted_export = None;
                    self.clear_io_marker("m43_export_pending");
                }
                MgrAction::DismissInterruptedImport => {
                    self.index_mgr.io.interrupted_import = false;
                    self.clear_io_marker("m43_import_pending");
                }
                MgrAction::RemapRoot { from, .. } => self.m4_remap_root(&from),
                MgrAction::BrowseImportedPhotos(batch) => self.browse_imported_photos(batch),
                MgrAction::UnhideImportSource(id) => {
                    let done = self.lib.db().map_err(|e| e.to_string()).and_then(|db| {
                        db.set_import_source_hidden(id, false).map_err(|e| e.to_string())
                    });
                    match done {
                        Ok(()) => {
                            self.lib.toast = Some(("已恢复该来源的分组显示".into(), 3.0));
                            self.sidebar_groups_dirty = true;
                        }
                        Err(e) => {
                            self.lib.toast =
                                Some((format!("恢复分组显示失败：{e}"), 4.0));
                        }
                    }
                }
            }
        }
    }

    /// M4 导出：选目标 → 校验目录 → 调 `export_index` → 四态反馈。
    /// M4.3 阶段 3 第二批：导入 / 导出 / 预览共用同一写入协调器闸。
    /// 后台任务自己 `Db::open` 独立连接，与 scanner 的 `Arc<Mutex<Db>>` 不共享；
    /// 在 UI 侧把这些入口串行化，避免两个独立连接同时写主库（§3.1:102）。
    fn io_busy(&self) -> bool {
        self.import_task.as_ref().is_some_and(|t| t.is_active())
    }

    /// M4.3 §3A ⑦：清掉一个导入 / 导出重启标记（`settings` 里那一行）。
    ///
    /// 标记只在任务**跑着**时存在；终态（成功 / 失败 / 取消）与用户「忽略」都清掉它，
    /// 所以下次启动看见它 == 上次真的没跑完。写失败只记日志——这层是提示，不是真相源。
    fn clear_io_marker(&self, key: &str) {
        if let Ok(db) = self.lib.db()
            && let Err(e) = db.set_setting_raw(key, None)
        {
            tracing::warn!(error = %e, key, "清理导入导出重启标记失败");
        }
    }

    /// M4.3 §3A ⑦：写一个导入 / 导出重启标记（任务真正启动前调用）。
    /// IDX-19：跳到「本次导入 · N 张」这个临时范围。
    ///
    /// 实现口径刻意复用既有筛选通道（`PhotoFilter.ids`）而不是新造一个 `View`：
    /// 视图值仍是「所有照片」，范围由筛选表达，这样面包屑、分页、查看器翻页、
    /// 「清除筛选」全都自动正确，也不需要把「本次导入」这种一次性状态塞进偏好。
    /// 空批次**不**降级成「不过滤」——那会把整个图库显示出来。
    fn browse_imported_photos(&mut self, batch_id: i64) {
        let ids = self
            .lib
            .db()
            .map_err(|e| e.to_string())
            .and_then(|db| {
                picasa_db::import_batch_photo_ids(&db, batch_id).map_err(|e| e.to_string())
            });
        let ids = match ids {
            Ok(ids) => ids,
            Err(e) => {
                self.lib.toast = Some((format!("读取本次导入的照片失败：{e}"), 4.0));
                return;
            }
        };
        if ids.is_empty() {
            self.lib.toast = Some(("这次导入没有带进任何照片".into(), 3.0));
            return;
        }
        let n = ids.len();
        let mut filter = self.lib.filter.clone().unwrap_or_default();
        filter.ids = Some(ids);
        if let Err(e) = self.lib.set_view(picasa_core::View::All) {
            tracing::warn!(error = %e, "切换到全部照片失败");
        }
        if let Err(e) = self.lib.set_filter(Some(filter)) {
            tracing::warn!(error = %e, "切换到本次导入范围失败");
            return;
        }
        self.restore_scroll = Some(0.0);
        // 关掉管理面板，让用户直接看到网格（IDX-19 ①「跳到……照片集合」）。
        self.index_mgr.open = false;
        self.lib.toast = Some((format!("本次导入 · {n} 张"), 3.0));
    }

    fn mark_io_pending(&self, key: &str, json: &str) {
        if let Ok(db) = self.lib.db()
            && let Err(e) = db.set_setting_raw(key, Some(json))
        {
            tracing::warn!(error = %e, key, "写导入导出重启标记失败");
        }
    }

    /// M4.3 §3A ⑦：一键重来上次中断的导出。
    ///
    /// 目标路径与「含缩略图」都从持久标记原样取回，**不重新弹保存对话框**——
    /// 用户点「重来」就是要再试一次同一个目标。安全检查照常：
    /// 目标不得落在任何已注册根之下（R-M43-01），且此刻不能有别的 IO 任务在跑。
    fn m4_replay_export(&mut self) {
        if self.io_busy() {
            self.lib.toast = Some(("已有导入 / 导出任务在运行，请稍候".into(), 3.0));
            return;
        }
        let Some(pending) = self.index_mgr.io.interrupted_export.clone() else {
            return;
        };
        let target = std::path::PathBuf::from(&pending.target);
        let roots: Vec<String> = self
            .lib
            .root_paths()
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let dir = target
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.paths.support.clone());
        if let Err(e) = picasa_db::check_export_dir(&dir, &roots) {
            // 旧目标现在落在照片目录里（用户后来加了根）：不重来，就地改成失败提示。
            let io = &mut self.index_mgr.io;
            io.interrupted_export = None;
            io.state = crate::index_mgr::IoState::Failed;
            io.message = e.to_string();
            self.clear_io_marker("m43_export_pending");
            return;
        }
        let io = &mut self.index_mgr.io;
        io.interrupted_export = None;
        io.state = crate::index_mgr::IoState::Running;
        io.progress = "正在导出…".into();
        io.fraction = None;
        io.percent = 0;
        io.snapshot = None;
        io.export_path = pending.target.clone();
        self.mark_io_pending("m43_export_pending", &pending.to_json());
        self.import_task = Some(crate::import_task::ImportTask::start_export(
            self.paths.db_file(),
            target,
            pending.with_thumbs,
        ));
    }

    fn m4_export_index(&mut self, with_thumbs: bool) {
        if self.io_busy() {
            self.lib.toast = Some(("已有导入 / 导出任务在运行，请稍候".into(), 3.0));
            return;
        }
        let io = &mut self.index_mgr.io;
        io.state = crate::index_mgr::IoState::Preparing;
        io.message.clear();
        let start = self.paths.support.clone();
        let Some(target) = crate::macos::pick_export_index(Some(&start), with_thumbs) else {
            io.state = crate::index_mgr::IoState::Cancelled;
            io.message = "已取消 · 现有索引未改动".into();
            return;
        };
        // 目标目录不得落在任何已注册图库根目录内（R-M2-1）
        let roots: Vec<String> = self.lib.root_paths().iter().map(|p| p.to_string_lossy().to_string()).collect();
        let dir = target.parent().unwrap_or(&start);
        if let Err(e) = picasa_db::check_export_dir(dir, &roots) {
            io.state = crate::index_mgr::IoState::Failed;
            io.message = e.to_string();
            return;
        }
        io.state = crate::index_mgr::IoState::Running;
        io.progress = "正在导出…".into();
        io.fraction = None;
        // IDX-18：新任务重置单调百分比与快照（否则会显示上一次任务的旧百分比）。
        io.percent = 0;
        io.snapshot = None;
        // M4.2 `M42-PERF-01`：导出改后台线程，UI 帧不再被 `export_index` 阻塞。
        // `ImportTask` 自己 `Db::open(paths.db_file())` 独立连接（`Db` 非 Send），
        // 跑完即 drop；进度 / 六态 / toast 都由 `ui()` tick 里的 `apply_io_msg` 驱动。
        //
        // M4.3 §3A ⑦：动手前先落一个「导出进行中」标记。它只在任务跑着时存在，
        // 终态（成功 / 失败 / 取消）一律清掉；因此**下次启动还看得见它 == 上次没跑完**。
        io.export_path = target.to_string_lossy().to_string();
        self.mark_io_pending(
            "m43_export_pending",
            &crate::index_mgr::InterruptedExport {
                target: target.to_string_lossy().to_string(),
                with_thumbs,
            }
            .to_json(),
        );
        self.import_task = Some(crate::import_task::ImportTask::start_export(
            self.paths.db_file(),
            target,
            with_thumbs,
        ));
    }

    /// M4 导入 Step1：选文件 → 校验 + 预览。
    ///
    /// **必须走后台线程**（`M4.3 PERF`）：用户真实索引包有 9 GiB，同步跑
    /// `inspect_index` 要付「两遍 SHA-256 + 一次 integrity_check + 一次整文件复制」
    /// 约一两分钟，而且旧实现还用 `std::fs::read` 把整份文件读进内存——
    /// 界面在这段时间完全冻住，用户看到的就是「程序无响应」。
    /// 现在这里只做三件事：状态置 `Preparing`、起线程、把取消标志接给 UI。
    fn m4_pick_import_file(&mut self) {
        if self.io_busy() {
            self.lib.toast = Some(("已有导入 / 导出任务在运行，请稍候".into(), 3.0));
            return;
        }
        let start = self.paths.support.clone();
        let Some(src) = crate::macos::pick_import_index(Some(&start)) else {
            let io = &mut self.index_mgr.io;
            io.state = crate::index_mgr::IoState::Cancelled;
            io.message = "已取消 · 现有索引未改动".into();
            return;
        };
        let work = self.paths.support.clone();
        let task = crate::import_task::ImportTask::start_inspect(
            self.paths.db_file(),
            src,
            work,
        );
        let io = &mut self.index_mgr.io;
        io.state = crate::index_mgr::IoState::Preparing;
        io.message.clear();
        io.progress = "正在校验索引文件（后台执行，界面可继续操作）".into();
        io.fraction = None;
        io.snapshot = None;
        io.percent = 0;
        io.preview = None;
        io.preview_thumbs.clear();
        // 校验阶段就能取消：大包读一遍也要几十秒，用户不该被迫等完。
        io.cancel = Some(task.cancel_flag());
        self.import_task = Some(task);
    }

    /// M4 导入 Step3：执行（替换策略先二次确认由 UI 勾选 + 说明保证）。
    fn m4_start_import(&mut self) {
        if self.io_busy() {
            self.lib.toast = Some(("已有导入 / 导出任务在运行，请稍候".into(), 3.0));
            return;
        }
        let io = &mut self.index_mgr.io;
        let Some(_preview) = &io.preview else { return };
        let src = std::path::PathBuf::from(&io.import_path);
        let strategy = io.strategy;
        let work = self.paths.support.clone();
        io.state = crate::index_mgr::IoState::Running;
        io.progress = "正在校验索引包…（后台执行，界面可继续操作）".into();
        // IDX-18：新任务重置单调百分比与快照。
        io.percent = 0;
        io.snapshot = None;
        // M4.2 `M42-PERF-01`：不再用不可达的局部 Arc + 假 0.3 进度 + 同步
        // `execute_import`（会阻塞 UI 帧）。改后台线程 + 真取消标志；成功后
        // `reload_tree/reload_photos/侧栏脏/EXIF/toast` 以 `TaskEffects` 纯数据
        // 返回，由 `ui()` 的 tick 消费（碰不到 app 私有字段的兄弟模块）。
        let task = crate::import_task::ImportTask::start_import(
            self.paths.db_file(),
            src,
            work,
            strategy,
        );
        io.cancel = Some(task.cancel_flag()); // 与「取消导入」按钮同一个 Arc
        io.fraction = None;
        // M4.3 §3A ⑦：落「导入进行中」标记。导入是单事务：提交成功才算数，
        // 所以这个标记在**提交前**一直留着；下次启动看见它 == 上次提交前就中断了
        // → 提示「现有索引未改动」。成功后由 Done 效果改写为已提交摘要。
        io.interrupted_import = false;
        self.mark_io_pending("m43_import_pending", "1");
        self.import_task = Some(task);
    }

    /// M4：刷新左栏标签 / 相册缓存。只在脏标记置位时查库（不是每帧）。
    ///
    /// 触发点：启动、导入完成、标签/相册增删改、数据集变化。这样绘制每帧只读
    /// `Vec`，不再跑 `all_tags()`（全表 GROUP BY）+ `all_albums()`（相关子查询）。
    /// 取回后台算好的标签计数（IDX-22 ⑧）。**非阻塞**：拿不到就下一帧再来。
    ///
    /// 结果落地时才应用排序规则——线程里只算「标签 → 数量」，
    /// 这样用户切排序不必等一次全库聚合。
    fn poll_tag_counts(&mut self, ctx: &egui::Context) {
        let Some((rev, counts)) = self.tag_counts.poll() else {
            // 还在算：请求下一帧来收（120 ms 够用了，计数不是逐帧变化的东西）。
            if self.tag_counts.is_busy() {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
            return;
        };
        if rev <= self.tag_counts_applied {
            // 乱序到达的旧结果：丢弃。用旧计数覆盖新计数会让左栏数字往回跳。
            return;
        }
        self.tag_counts_applied = rev;
        let sort = self.lib.tags_sort();
        let show_zero = self.lib.show_zero_tags();
        self.sidebar_tags = picasa_core::sort_tag_counts(&counts.all, sort, show_zero);
        self.sidebar_tags_in_view = counts
            .in_view
            .map(|raw| picasa_core::sort_tag_counts(&raw, sort, show_zero))
            .unwrap_or_default();
    }

    fn refresh_sidebar_groups(&mut self) {
        if !self.sidebar_groups_dirty {
            return;
        }
        // IDX-22 ⑧：计数**不在这一帧算**。脏标记只负责「派一次后台计算」，
        // 结果由 `ui()` 里的 `poll_tag_counts()` 非阻塞取回，帧内只做排序（微秒级）。
        //
        // 排序规则（按名称 / 按数量、是否显示 0 计数）在结果落地时应用，
        // 所以改了排序只需置脏重派，不用关心线程里用的是哪套偏好。
        let view_scope = if matches!(self.lib.view, picasa_core::View::All)
            && !self.lib.filter.as_ref().is_some_and(|f| f.starred_only)
        {
            None
        } else {
            Some(self.lib.view_folder_ids().unwrap_or_default())
        };
        if !self.tag_counts.is_busy() {
            self.tag_counts.request(view_scope);
        }
        if let Ok(db) = self.lib.db() {
            let albums = db.all_albums().unwrap_or_default();
            // M4.2 `M42-UI-05`：同时留一份 name→id，供改名/删除/加入照片调用
            // Core 的 `*_album(id, ..)`；`sidebar_albums` 仍只画 (name,count)。
            self.sidebar_album_ids = albums
                .iter()
                .map(|a| (a.name.clone(), a.id))
                .collect();
            self.sidebar_albums = albums
                .into_iter()
                .map(|a| (a.name, a.photo_count))
                .collect();
            // IDX-21：导入来源列表 + 每个来源的根路径/可读性。
            // 只在这里查一次库；`path_is_offline` 只做元数据判断，不扫目录。
            // P-M43-10：同一条路径在本机也有时以本机为准，标「本机已有」。
            // 只是显示层提示，不去重、不改计数口径（计数本来就只算一份：
            // 自然键命中既有根时导入不会新建根行）。
            let local_paths: std::collections::HashSet<String> = db
                .roots(false)
                .unwrap_or_default()
                .into_iter()
                .filter(|r| !r.is_imported())
                .map(|r| r.path)
                .collect();
            let mut groups = Vec::new();
            let mut hidden = Vec::new();
            for src in db.import_sources().unwrap_or_default() {
                if db.import_source_hidden(src.id).unwrap_or(false) {
                    hidden.push((src.id, src.name.clone()));
                    continue;
                }
                let roots = db.imported_roots(src.id).unwrap_or_default();
                let photos = db.import_source_photo_count(src.id).unwrap_or(0);
                let offline = roots.iter().any(|(_, _, online, _)| !*online);
                let abs_paths: Vec<String> = roots.iter().map(|(_, p, _, _)| p.clone()).collect();
                let local_dupes = abs_paths
                    .iter()
                    .filter(|p| local_paths.contains(*p))
                    .cloned()
                    .collect();
                groups.push(crate::sidebar::ImportGroup {
                    source_id: src.id,
                    name: src.name.clone(),
                    when: crate::index_mgr::format_backup_time(src.created_at, "本地"),
                    photos,
                    offline,
                    abs_paths,
                    local_dupes,
                });
            }
            self.sidebar_import_groups = groups;
            self.index_mgr.io.hidden_sources = hidden;
            self.sidebar_groups_dirty = false;
        }
    }

    /// M4：EXIF 回填**完全在后台线程**跑（docs/11 验收重点 6 / R-M2-3）。
    ///
    /// ⚠️ 性能回归教训：早期版本每帧在 UI 线程同步 `probe()` 20 个文件 + 20 次
    /// 独立 `UPDATE` 事务提交，还用 `request_repaint_after(200ms)` 强制持续重绘——
    /// 直接抢掉缩略图解码的帧预算，用户感知就是「索引和展示缩略图变慢」。
    /// 现在改成：后台线程按 `try_lock` 短锁分批跑，UI 帧里只 `try_recv` 一个结果，
    /// 不强制重绘（下一帧自然来），没有待处理行就彻底停手。
    fn tick_m4_exif(&mut self, _ctx: &egui::Context) {
        // 1) 先收上一批的结果（非阻塞，绝不等线程）
        if let Some(rx) = &self.exif_rx {
            match rx.try_recv() {
                Ok((ok, _failed)) => {
                    self.exif_rx = None;
                    if ok == 0 {
                        self.m4_exif_pending = false; // 全库回填完成
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // 线程异常退出：清掉收件口，下一帧重试（不把「回填中」钉死）
                    self.exif_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.m4_exif_pending || self.lib.scanning || self.exif_rx.is_some() {
            return;
        }
        // 2) 发起一批后台任务（同一时刻最多一个在跑）
        let db = Arc::clone(&self.lib.db);
        let (tx, rx) = channel();
        self.exif_rx = Some(rx);
        std::thread::spawn(move || {
            use picasa_core::m4_exif as exif;
            // ⚠️ 锁粒度是性能红线：`probe()` 每个文件都要 File::open + 解析 EXIF，
            // 绝不能持锁做。三段式：① 持锁取路径（毫秒）→ ② 完全无锁探测
            // → ③ 持锁单事务写回（毫秒）。中间那段再慢也不堵索引/缩略图线程。
            let pending = {
                let mut out = None;
                for _ in 0..40 {
                    match db.try_lock() {
                        Ok(guard) => {
                            out = exif::collect_pending(&guard, 20).ok();
                            break;
                        }
                        Err(std::sync::TryLockError::WouldBlock) => {
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        Err(std::sync::TryLockError::Poisoned(_)) => break,
                    }
                }
                out
            };
            let Some(pending) = pending else { return }; // 通道断开 → UI 侧重试
            let (items, failed) = exif::probe_paths(pending); // 无锁，最贵的一步
            let written = {
                let mut n = 0;
                for _ in 0..40 {
                    match db.try_lock() {
                        Ok(guard) => {
                            n = exif::write_batch(&guard, &items).unwrap_or(0);
                            break;
                        }
                        Err(std::sync::TryLockError::WouldBlock) => {
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        Err(std::sync::TryLockError::Poisoned(_)) => break,
                    }
                }
                n
            };
            let _ = tx.send((written, failed));
        });
    }

    /// M4-BUG-04：导出诊断日志（§4.9）。只写用户显式选择的目标目录，
    /// 先过 `check_export_dir` 保证不落在照片目录内（R-M2-1）。
    fn m4_export_diag_log(&mut self) {
        let Some(dir) = crate::macos::pick_folder(Some(&self.paths.support)) else {
            self.lib.toast = Some(("已取消导出诊断日志".into(), 2.5));
            return;
        };
        if let Err(e) = picasa_core::mgmt::check_export_dir(&dir, &self.mgmt) {
            self.lib.toast = Some((e, 4.0));
            return;
        }
        let name = format!(
            "openpicasa-diag-{}.json",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        );
        let target = dir.join(name);
        // M4.3 IDX-17：导出的是脱敏后的任务摘要与失败列表 —— 绝不写任何绝对
        // 路径或用户照片名。scope / 失败路径都经 anonymize_path 哈希；取消原因、
        // outcome、完成/失败数来自持久层，跨重启可追溯（TC-M43-11/13）。
        let (task_summary, failures) = self
            .lib
            .db()
            .map(|db| {
                let tasks = db.recent_tasks(50).unwrap_or_default();
                let summary: Vec<_> = tasks
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "scope": picasa_core::mgmt::anonymize_path(&t.scope_path),
                            "kind": t.kind,
                            "status": t.status,
                            "outcome": t.outcome().map(|o| o.label().to_string()),
                            "completed": t.processed.saturating_sub(t.failed).max(0),
                            "failed": t.failed,
                            "cancel_reason": t.cancel_reason,
                            "user_paused": t.user_paused,
                            "generation": t.generation,
                        })
                    })
                    .collect();
                let mut fails = Vec::new();
                for t in &tasks {
                    if let Ok(list) = db.task_failures(t.task_id) {
                        for (rel, code, attempts) in list {
                            fails.push(serde_json::json!({
                                "scope": picasa_core::mgmt::anonymize_path(&t.scope_path),
                                "path": picasa_core::mgmt::anonymize_path(&rel),
                                "code": code,
                                "attempts": attempts,
                            }));
                        }
                    }
                }
                (summary, fails)
            })
            .unwrap_or_default();
        let payload = serde_json::json!({
            "app_version": env!("CARGO_PKG_VERSION"),
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "schema_version": self.lib.db().map(|d| d.schema_version().unwrap_or(0)).unwrap_or(0),
            "all_photos": self.mgmt.all_photos,
            "roots": self.mgmt.roots.iter().map(|r| serde_json::json!({
                "path": picasa_core::mgmt::anonymize_path(&r.path),
                "offline": r.offline, "closed": r.closed, "photo_count": r.photo_count,
            })).collect::<Vec<_>>(),
            "tasks": task_summary,
            "failures": failures,
            "storage": {
                "db_bytes": self.mgmt.storage.db_bytes,
                "thumb_bytes": self.mgmt.storage.thumb_bytes,
                "thumb_count": self.mgmt.storage.thumb_count,
            },
            "badge": self.mgmt.badge,
        });
        match std::fs::write(&target, serde_json::to_string_pretty(&payload).unwrap_or_default()) {
            Ok(()) => self.lib.toast = Some((format!("✓ 已导出诊断日志：{}", target.display()), 3.0)),
            Err(e) => self.lib.toast = Some((format!("诊断日志导出失败：{e}"), 4.0)),
        }
    }

    fn m4_cancel_import(&mut self) {
        // M4.2 `M42-PERF-01`：真取消——把后台线程共享的 AtomicBool 置位，
        // `execute_import` / `export_index` 在阶段间检查后整库回滚。
        // 与 index_mgr 的「取消导入」按钮同一个 Arc（幂等）。
        if let Some(t) = &self.import_task {
            t.cancel();
        }
        let io = &mut self.index_mgr.io;
        io.state = crate::index_mgr::IoState::Cancelled;
        io.message = "已取消 · 现有索引未改动".into();
        io.fraction = None;
    }

    fn m4_remap_root(&mut self, from: &str) {
        let Some(to) = crate::macos::pick_remap_folder(None) else {
            return;
        };
        let io = &mut self.index_mgr.io;
        if let Some((_p, matched)) = io.roots_to_remap.iter_mut().find(|(p, _)| p == from) {
            *matched = to.exists();
        }
        self.lib.add_root(&to);
        if let Err(e) = self.lib.reload_tree() {
            tracing::warn!(error = %e, "重新匹配后刷新目录树失败");
        }
        self.lib.toast = Some(("✓ 已重新匹配图库根目录".into(), 3.0));
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

    /// IDX-11：作用域确认框的数据（路径 / 照片数 / 标记数）。
    ///
    /// 任一统计失败或 root 不存在时该项为 `None` → 确认框显示「待计算」，
    /// **绝不填 0**（§2 M43-IDX-11「未知数量显示待计算，不能填 0」）。
    /// 一次取 db 锁、复用同一个 guard，避免计数之间库被改动。
    fn scope_confirm_data(&self, id: i64) -> (String, Option<u64>, Option<i64>) {
        let path = self
            .mgmt
            .root_by_id(id)
            .map(|r| r.path.clone())
            .or_else(|| {
                self.lib
                    .db()
                    .ok()
                    .and_then(|db| db.root(id).ok().flatten())
                    .map(|r| r.path)
            })
            .unwrap_or_default();
        let (photos, markers) = match self.lib.db() {
            Ok(db) => {
                let photos = db.photo_count_for_root(id).ok().and_then(|n| {
                    if n >= 0 {
                        Some(n as u64)
                    } else {
                        None
                    }
                });
                // marker_count_for_root 本身用 Option 区分「确实 0」与「查不到」。
                let markers = db.marker_count_for_root(id).ok().flatten();
                (photos, markers)
            }
            Err(_) => (None, None),
        };
        (path, photos, markers)
    }

    fn reindex_root(&mut self, id: i64) {
        let Some((path, removed)) = self
            .mgmt
            .root_by_id(id)
            .map(|r| (r.path.clone(), r.index_removed))
        else {
            return;
        };
        // 索引已移除的根被「重新索引」时，这是一次**恢复**：`add_root` 会把
        // `roots.state` 置回 active（并清掉「已移除」标记），否则扫描队列建了
        // 任务却因为根不在 `roots(false)` 里进不了树。
        if removed {
            self.lib.add_root(std::path::Path::new(&path));
            self.lib.toast = Some((
                format!("正在重新索引「{path}」（用户标记会按身份自动挂回）"),
                4.0,
            ));
        }
        self.index.enqueue_refresh(std::path::Path::new(&path));
        self.reload_now = true;
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    /// IDX-13：强制重扫单个根。跳过 L1 快筛、直接对该根做文件级 L2 比对；
    /// `VerifyRunner::take_changed()` 的结果由 `tick_startup_verify` 的完成块
    /// 统一转成 `enqueue_refresh`（变化目录才重扫）。非破坏性，无需确认框。
    ///
    /// `VerifyRunner::load` 只按 `roots.path` 精确匹配解析，故这里必须用
    /// `root_by_id` 取到的**已注册根路径**，不能传任意子目录。
    fn force_rescan_root(&mut self, id: i64) {
        let Some(path) = self.mgmt.root_by_id(id).map(|r| r.path.clone()) else {
            return;
        };
        // 同一 self.verify 槽位：已有校验在跑时不要覆盖它。
        if self.verify.is_some() {
            self.lib.toast = Some(("已有校验任务在运行，请稍候".into(), 3.0));
            return;
        }
        let plan = picasa_core::VerifyPlan::from_root_paths([std::path::PathBuf::from(&path)]);
        if plan.is_empty() {
            return;
        }
        self.verify = Some(picasa_core::verify::VerifyRunner::new_forced_full(plan));
        self.verify_done = false;
        self.lib.toast = Some((
            format!("已对 {path} 开始强制重扫（跳过快筛，文件级核对）"),
            3.0,
        ));
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    /// 全量重建（§4.2 M22-IDX-06）：**不先清再建**，旧缩略图保持可见直到被替换。
    fn rebuild_root(&mut self, id: i64) {
        let Some(root) = self.mgmt.root_by_id(id).map(|r| r.path.clone()) else {
            return;
        };
        let path = std::path::PathBuf::from(&root);
        // IDX-14：走 force 全量重建入口。旧实现只 `reset_failed_thumbs_subtree`
        // （仅失败行）+ 普通 enqueue_scan（L1 全命中啥都不做），根本不是全量重建；
        // force 路径逐目录绕过 L1、保留旧 BLOB / thumb_state=1 到 put_thumb 原子替换，
        // 因此这里不再手工 reset（归零 thumb_state 反而会让重建期间满屏灰格）。
        self.index.enqueue_rebuild(std::slice::from_ref(&path));
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

    /// IDX-06：在 Finder 中显示最近一次成功备份文件的位置。
    fn reveal_last_backup(&mut self) {
        let Some(info) = &self.index_mgr.io.last_backup else {
            return;
        };
        let path = std::path::PathBuf::from(&info.path);
        // 文件可能被用户移走：reveal 失败时给出明确提示，但绝不改写记录。
        if !path.is_file() {
            self.lib.toast = Some(("备份文件已不在原位置（可能被移动或删除）".into(), 3.0));
            return;
        }
        if let Err(e) = crate::macos::reveal_in_finder(&path) {
            self.lib.toast = Some((e, 3.0));
        }
    }

    /// M4.3 阶段 0：删除**某一个 root** 的索引（后台线程、带稳定 id、保留用户标记）。
    ///
    /// 旧接线 `DeleteIndexRoot(_)` 丢弃 id 后走 `self.index.delete_index()` 清全库，
    /// 是需求说明书 §0.1 点名的危险接线；这里改用作用域版本，且 `Db` 侧删前会
    /// 快照标记到 v9 身份骨架（R-M43-02）。
    /// **彻底**删除某个根的索引（m10756 起这是唯一的「删除索引」语义）。
    ///
    /// 三处都要动，缺一处用户就看得到「删了还在」：
    /// - DB：`purge_index_for_root` = `clear_index_for_root`（标记快照 + 作废任务 +
    ///   删 photos/folders/thumbs）+ `delete_root_row`（删 roots/exclusions/设置键
    ///   + 会一并收掉已无根的导入来源）；
    /// - 内存：`Library::purge_root` 让 `root_paths` 与文件夹树立刻少掉这棵子树
    ///   （不能调 `close_root`——它要按路径查 roots 行，删完就查不到了）；
    /// - 显存：`cache.clear()` 丢掉已上传纹理，否则那批缩略图还挂在绘制管线里。
    fn delete_index_root(&mut self, id: i64) {
        let Some(root) = self.mgmt.root_by_id(id).map(|r| r.path.clone()) else {
            self.lib.toast = Some(("该索引已经不存在".into(), 3.0));
            self.mgmt_next_refresh = std::time::Instant::now();
            return;
        };
        if !self.index.purge_index_for_root(id) {
            self.lib.toast = Some(("已有删除任务在跑，请稍候".into(), 3.0));
            return;
        }
        self.lib.purge_root(std::path::Path::new(&root));
        // 索引面板里该根的任务行（运行中/等待中）也要撤掉，否则它会一直占着一行。
        self.board.forget(&root);
        // m10756：彻底删除导入根后，来源节点可能已经不剩任何根了，
        // 左栏「导入的索引」分组必须重算——否则那个节点会留在树上（点开是空的）。
        // 脏标记只负责「下一帧重取」，真正的裁剪在 DB 层 `purge_index_for_root`。
        self.sidebar_groups_dirty = true;
        // 已上传的缩略图纹理必须清掉（m10218「界面上和该索引相关的预览图片」）。
        self.cache.clear();
        self.reload_now = true;
        self.mgmt_next_refresh = std::time::Instant::now();
        self.lib.toast = Some((
            format!("已彻底删除「{root}」的索引（索引数据与缩略图已删除，原图未动）"),
            4.5,
        ));
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

    /// 清空运行日志文件（m10756）。
    ///
    /// 走 `crate::clear_log_file()` 而不是自己 `File::create`：日志订阅者
    /// 已经拿着一个 `File`，另开一个句柄清空只会让**旧句柄继续往旧偏移写**，
    /// 文件里立刻出现一大片 0 填充的空洞。
    fn clear_run_log(&mut self) {
        let msg = match crate::clear_log_file() {
            Ok(0) => "运行日志文件已经是空的（或日志未初始化）".to_string(),
            Ok(n) => format!("已清空运行日志（{:.1} KB）", n as f64 / 1024.0),
            Err(e) => format!("清空运行日志失败：{e}"),
        };
        self.lib.toast = Some((msg, 3.0));
        self.mgmt_next_refresh = std::time::Instant::now();
    }

    /// 清空任务历史（m10756）：只删已结束的记录，活任务一条不动。
    fn clear_task_history(&mut self) {
        let (msg, ok) = match self.lib.db() {
            Ok(db) => match db.clear_task_history() {
                Ok(0) => ("没有已结束的任务记录".to_string(), false),
                Ok(n) => (format!("已清空 {n} 条任务历史"), true),
                Err(e) => (format!("清空任务历史失败：{e}"), false),
            },
            Err(e) => (format!("清空任务历史失败：{e}"), false),
        };
        if ok {
            // 失败分类的角标读的是 index_task_failures，已随任务级联删除，
            // 必须重算快照，否则右栏角标还停在旧数字上。
            self.mgmt_next_refresh = std::time::Instant::now();
        }
        self.lib.toast = Some((msg, 3.0));
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
                // IDX-22 P1：范围变了，「本视图 N」必须跟着重算。
                self.sidebar_groups_dirty = true;
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
                    if reason == picasa_imaging::CLOUD_ONLY_REASON {
                        // 云端占位不是失败：不写进「本帧不再重试」的黑名单，
                        // 否则用户把文件下载到本地之后，这张图在整个会话里都不会再出图。
                        tracing::debug!(photo_id, "缩略图跳过：文件在云端，本机未下载");
                    } else {
                        self.cache.mark_failed(photo_id);
                        tracing::debug!(photo_id, reason = %reason, "缩略图失败（计入失败项）");
                    }
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
                    // M4：扫描完成后由**后台线程**继续回填 EXIF（不占 UI 帧预算）。
                    self.m4_exif_pending = true;
                    self.sidebar_groups_dirty = true;
                    // 索引被清空/改写过，管理面板必须**立刻**重算快照。
                    // 不置这一位就得等 `MGMT_REFRESH_MS`(1s) 的定时刷新，而 egui
                    // 是事件驱动的——鼠标不动根本没有下一帧，面板会一直停在删除前
                    // 的数字上（实机反馈「删除索引后，索引列表没有更新」）。
                    self.mgmt_next_refresh = std::time::Instant::now();
                    ctx.request_repaint();
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
        // M4.3 补遗：`reload_now` 自己就是「这帧必须落屏」的意图，不能要求调用方
        // 连带记得置 `reload_dirty`。
        //
        // 旧判据只看 `reload_dirty`，而 6 处「用户点了按钮 / 终态事件到了」的调用点
        // 都只设了 `reload_now`（`reindex_root` / `set_root_closed` / `delete_index_root`
        // / `restore_exclusion` / 清理缩略图 / `ScanFinished|IndexCleared`）——
        // 于是这些「立刻」全被这一行吞掉，只能等下一次扫描事件顺手置脏才生效。
        // 实机表现：点「重新打开」后侧栏什么都不变。
        if !self.reload_dirty && !self.reload_now {
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

    // ------------------------------------------------------------------
    // M4.1 `M41-UI-01`：工具栏四枚对象操作钮（全部作用于当前 `lib.selection`）
    // ------------------------------------------------------------------

    /// 无选中时的统一提示（四个按钮共用，不隐藏、不置灰——功能已实装，
    /// 只是条件不满足；`10` M32-UI-02「不放假入口」）。
    fn need_selection(&mut self) -> bool {
        if self.lib.selection.is_empty() {
            self.lib.toast = Some(("请先选择照片".into(), 2.0));
            true
        } else {
            false
        }
    }

    /// ♥ 收藏：选中集全已收藏 → 取消；否则（含混合态）→ 全部收藏。
    fn object_favorite(&mut self) {
        if self.need_selection() {
            return;
        }
        let all = self
            .lib
            .photos
            .iter()
            .filter(|p| self.lib.selection.contains(&p.id))
            .all(|p| p.starred);
        let on = !all;
        match self.lib.batch_set_starred(on) {
            Ok(n) if n > 0 => {
                self.lib.toast = Some((
                    format!("{} {n} 张", if on { "已收藏" } else { "已取消收藏" }),
                    2.0,
                ));
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "切换收藏失败"),
        }
    }

    /// ⓘ 说明：打开多选版「照片说明」窗口（预填单张已有说明）。
    fn object_caption(&mut self) {
        if self.need_selection() {
            return;
        }
        // 单张时预填已有说明；多选时留空（不把第一张的说明误灌给其余张）。
        self.caption_buf = if self.lib.selection.len() == 1 {
            let id = *self.lib.selection.iter().next().unwrap_or(&0);
            self.lib.caption_of(id).unwrap_or_default()
        } else {
            String::new()
        };
        self.caption_editor = Some(-1); // -1 = 多选集模式（作用在 lib.selection）
    }

    /// ⌗ 标签：打开批量标签浮层（复用 tag_editor，语义改为「选中集」）。
    fn object_tag(&mut self) {
        if self.need_selection() {
            return;
        }
        self.tag_buf.clear();
        // M4.2 `M42-UI-06`：打开浮层即作废旧缓存，按当前库状态重算一次。
        self.tag_tri_cache = None;
        self.tag_editor = Some(-1);
    }

    /// ▣ 照片托盘：选中集全在托盘 → 全部移出；否则 → 全部加入。
    fn object_tray(&mut self) {
        if self.need_selection() {
            return;
        }
        let all = self
            .lib
            .selection
            .iter()
            .all(|id| self.lib.tray.contains(id));
        let ids: Vec<i64> = self.lib.selection.iter().copied().collect();
        if all {
            for id in &ids {
                self.lib.tray_remove(*id);
            }
            self.tray_rows_dirty = true;
            self.lib.toast = Some((format!("已移出托盘 {} 张", ids.len()), 2.0));
        } else {
            let n = self.lib.to_tray();
            if n > 0 {
                self.tray_expanded = true;
                self.tray_rows_dirty = true;
            }
            self.lib.toast = Some((format!("已加入托盘 {n} 张"), 2.0));
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
                // M4.1 `M41-UI-01`：四个对象操作按钮的双态判定，只读内存、不查库。
                let obj = toolbar::ObjectActionState {
                    has_selection: !self.lib.selection.is_empty(),
                    all_favorite: !self.lib.selection.is_empty()
                        && self
                            .lib
                            .photos
                            .iter()
                            .filter(|p| self.lib.selection.contains(&p.id))
                            .all(|p| p.starred),
                    all_in_tray: !self.lib.selection.is_empty()
                        && self
                            .lib
                            .selection
                            .iter()
                            .all(|id| self.lib.tray.contains(id)),
                };
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
                    obj,
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
                            self.sidebar_groups_dirty = true;
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
                        TopbarAction::ToggleThumbsFirst => {
                            let next = !self.lib.thumbs_first;
                            if let Err(e) = self.lib.set_thumbs_first(next) {
                                self.lib.toast = Some((format!("切换排序失败：{e}"), 4.0));
                            }
                            self.restore_scroll = Some(0.0);
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
                        // M4-BUG-01：`TopbarAction::More(...)` 只由 `toolbar::more_menu()` 发出，
                        // 在 `draw_overlays` 里消费（见本文件 ~2254）。`topbar::show()` 永不发
                        // 这个变体，所以这里不再重复翻转 grid_show_filename —— 两处都写会二次翻转。
                        TopbarAction::More(_) => {
                            tracing::warn!("菜单动作应只在 draw_overlays 消费");
                        }
                        // ---- M4.1 `M41-UI-01`：四枚对象操作钮 ----
                        TopbarAction::Favorite => self.object_favorite(),
                        TopbarAction::Caption => self.object_caption(),
                        TopbarAction::Tag => self.object_tag(),
                        TopbarAction::Tray => self.object_tray(),
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
                    &self.index_mgr.io,
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
                if out.import_clicked {
                    // M4.3 IDX-18：点全局进度 pill → 打开索引管理并定位「导入 / 导出」。
                    // 注意顺序：open_at 会按 mgmt 快照选默认标签，所以 tab 要在其后设。
                    self.refresh_mgmt();
                    self.index_mgr
                        .open_at(&self.mgmt, self.mgmt.failed_task_count);
                    self.index_mgr.tab = picasa_core::MgmtTab::ImportExport;
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
                        tags_collapsed: self.tags_collapsed,
                        albums_collapsed: self.albums_collapsed,
                        tags: &self.sidebar_tags,
                        albums: &self.sidebar_albums,
                        // IDX-22 ②：当前生效的标签筛选 → 行高亮。
                        // `self.lib.filter` 是 `Option<PhotoFilter>`，这里借出。
                        tags_active: self
                            .lib
                            .filter
                            .as_ref()
                            .and_then(|f| f.tag.as_deref()),
                        tags_sort: self.lib.tags_sort(),
                        show_zero_tags: self.lib.show_zero_tags(),
                        tags_in_view: &self.sidebar_tags_in_view,
                        view_is_all: matches!(self.lib.view, picasa_core::View::All)
                            && !self.lib.filter.as_ref().is_some_and(|f| f.starred_only),
                        import_groups: &self.sidebar_import_groups,
                        imported_collapsed: self.imported_collapsed,
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
                // M4.2 `M42-PERF-00`：被动累计展示性能（零副作用、不触发重绘）。
                // `stable_dt` 是秒；钳到 100ms 避免挂起后一帧污染分位数。
                let frame_ms = ctx.input(|i| i.stable_dt.min(0.1)) * 1000.0;
                self.perf.note_frame(
                    frame_ms,
                    out.visible,
                    out.blank_cells,
                    out.uploads_this_frame,
                );
                // 首屏出图：第一帧画出照片网格（有可见格）时记一次进程启动以来的耗时。
                if !self.perf_first_screen_done && out.visible > 0 {
                    self.perf
                        .note_first_screen(self.perf_start.elapsed().as_secs_f32() * 1000.0);
                    self.perf_first_screen_done = true;
                }
                // 可见区首张出图：第一帧出现真实缩略图（不是全部占位）时记一次。
                if !self.perf_first_visible_done
                    && out.visible > 0
                    && out.blank_cells < out.visible
                {
                    self.perf
                        .note_first_visible(self.perf_start.elapsed().as_secs_f32() * 1000.0);
                    self.perf_first_visible_done = true;
                }
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
                // M4.1 `M41-UI-05`：照片右键菜单退役 —— 右键只单选该张，
                // 不再弹菜单；收藏/说明/标签全部由工具栏四按钮承接。
                if let Some((index, _pos)) = out.context {
                    self.lib.click_select(index, false, false);
                    self.folder_ctx = None;
                }
                // M4.1 `M41-UI-09/10`：点空白清空 / 框选命中集。
                if let Some(hit) = out.marquee {
                    if hit.additive {
                        for id in hit.ids {
                            self.lib.selection.insert(id);
                        }
                    } else {
                        self.lib.selection = hit.ids.into_iter().collect();
                    }
                    if let Some(a) = hit.anchor_index {
                        self.lib.anchor = Some(a);
                    }
                    self.anchor_id = None;
                } else if out.clear_selection {
                    self.lib.select_none();
                }
            });
    }

    fn apply_sidebar_action(&mut self, action: SidebarAction, ctx: &egui::Context) {
        match action {
            SidebarAction::ShowView(view) => {
                if let Err(e) = self.lib.set_view(view) {
                    tracing::warn!(error = %e, "切换视图失败");
                }
                // IDX-22 P1：「本视图 N」跟着范围走，换视图必须重算。
                self.sidebar_groups_dirty = true;
                self.restore_scroll = Some(0.0);
                self.viewer = None;
            }
            SidebarAction::TimelineYear(year) => {
                if year == 0 {
                    // 清除年份筛选（保留其他筛选维度）
                    let mut f = self.lib.filter.clone().unwrap_or_default();
                    f.taken_range = None;
                    if let Err(e) = self.lib.set_filter(f.is_active().then_some(f)) {
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
                let f = f.is_active().then_some(f);
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
                // M4-BUG 收口：section_title 传的是「图库」/「文件夹」，旧代码判「收藏」
                // 永远不成立，导致点「图库」分组头实际折叠了文件夹组。
                if title == "图库" {
                    self.favorites_collapsed = !self.favorites_collapsed;
                } else if title == "文件夹" {
                    self.folders_collapsed = !self.folders_collapsed;
                } else if title == "导入的索引" {
                    // 折叠状态重启后保留（IDX-21）：走 Library 的哨兵键，
                    // 不为此扩一份偏好字段。
                    self.imported_collapsed =
                        self.lib.toggle_section_collapsed("imported-index");
                }
            }
            // ---- M4：标签 / 相册分组 ----
            SidebarAction::ToggleSectionM4(title) => {
                if title == "标签" {
                    self.tags_collapsed = !self.tags_collapsed;
                } else if title == "相册" {
                    self.albums_collapsed = !self.albums_collapsed;
                }
            }
            SidebarAction::FilterByTag(tag) => {
                // M4.3 IDX-22 ⑤：点标签 = 看这个标签的**全部照片**。
                // 所以先把视图切回「所有照片」（文件夹视图下点数会只显示该夹的
                // 子集，与左栏计数对不上），再叠加 tag 筛选；星标可以同时留着
                // （规格允许叠加，标题先报标签）。再次点同一标签 = 取消。
                let already = self
                    .lib
                    .filter
                    .as_ref()
                    .is_some_and(|f| f.tag.as_deref() == Some(tag.as_str()));
                if let Err(e) = self.lib.set_view(picasa_core::View::All) {
                    tracing::warn!(error = %e, "切换全库视图失败");
                }
                let mut f = self.lib.filter.clone().unwrap_or_default();
                f.tag = (!already).then(|| tag.clone());
                let next = f.is_active().then_some(f);
                if let Err(e) = self.lib.set_filter(next) {
                    tracing::warn!(error = %e, "标签筛选失败");
                }
                self.restore_scroll = Some(0.0);
                self.viewer = None;
            }
            SidebarAction::FilterByAlbum(album) => {
                let mut f = self.lib.filter.clone().unwrap_or_default();
                f.album = Some(album);
                if let Err(e) = self.lib.set_filter(Some(f)) {
                    tracing::warn!(error = %e, "相册筛选失败");
                }
                self.restore_scroll = Some(0.0);
            }
            // M4.2 `M42-UI-04`：「＋」新建改为真接线——弹行内输入窗口，
            // 由 draw_overlays 调 Core 的 create_album / batch_add_tag 落库。
            SidebarAction::NewTag => {
                self.new_item = Some(NewItemKind::Tag);
                self.new_item_buf.clear();
            }
            SidebarAction::NewAlbum => {
                self.new_item = Some(NewItemKind::Album);
                self.new_item_buf.clear();
            }
            // M4.2 `M42-UI-05`：相册行右键 → 改名 / 删除 / 加入照片。
            // 与文件夹右键同范式，记下打开帧供「点别处就关」豁免。
            SidebarAction::AlbumContext(name, pos) => {
                self.album_ctx = Some((name, pos));
                self.album_ctx_frame = ctx.cumulative_frame_nr();
            }
            SidebarAction::SetTagsSort(sort) => {
                self.lib.set_tags_sort(sort);
                self.sidebar_groups_dirty = true;
            }
            SidebarAction::ToggleShowZeroTags => {
                self.lib.toggle_show_zero_tags();
                self.sidebar_groups_dirty = true;
            }
            SidebarAction::ImportSourceContext(id, pos) => {
                self.import_ctx = Some((id, pos));
                self.import_ctx_frame = ctx.cumulative_frame_nr();
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
        // M4.2 `M42-PERF-00`：诊断页打开时把被动累计的性能快照一并交付。
        data.perf = self.perf.snapshot();
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
                // IDX-14：与「全量重建」按钮收口到同一口径 —— 走 force 全量重建。
                // 旧实现先 `clear_all_thumbs()` 删光全部 BLOB 再普通 enqueue_scan，
                // 重建期间满屏灰格、且普通扫描 L1 全命中并不重新生成，与需求冲突。
                // force 路径保留旧 BLOB / thumb_state=1 到 put_thumb 原子替换；
                // `cache.clear()` 仍必须留：textures 对已上传纹理有 contains_key 早退，
                // 不清缓存新 BLOB 永远不会生效。VACUUM 交给 storage 页显式动作。
                self.cache.clear();
                let roots = self.lib.root_paths();
                if !roots.is_empty() {
                    self.index.enqueue_rebuild(&roots);
                }
                self.lib.toast = Some(("已开始全量重建（旧缩略图保留直到替换）".into(), 3.0));
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
                // M42-UI-01：只在打开 / 翻页后查一次当前照片标签，短锁读完立即放。
                // 绝不在 viewer::show 里再拿一次 lib.db()（std Mutex 不可重入，
                // sidebar.rs:302-308 记录过启动首帧 hang 127s 的教训）。
                // tags_for 做幂等标记，避免每帧查库（R-M42-1 展示速度不劣化）。
                if v.tags_for != Some(row.id) {
                    let db_arc = self.lib.db.clone();
                    let tags = {
                        let db = db_arc.lock().expect("db 锁");
                        db.tags_of(row.id).unwrap_or_default()
                    };
                    v.tags = tags;
                    v.tags_for = Some(row.id);
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
            if let Some(action) = toolbar::sort_menu(
                ui,
                self.sort_menu_pos,
                self.lib.sort,
                self.lib.thumbs_first,
            ) {
                match action {
                    crate::topbar::TopbarAction::PickSortKey(key) => self.pick_sort(key),
                    crate::topbar::TopbarAction::ToggleSortDirection => {
                        let key = self.lib.sort.key;
                        self.pick_sort(key);
                    }
                    crate::topbar::TopbarAction::ToggleThumbsFirst => {
                        let next = !self.lib.thumbs_first;
                        if let Err(e) = self.lib.set_thumbs_first(next) {
                            self.lib.toast = Some((format!("切换排序失败：{e}"), 4.0));
                        }
                        self.restore_scroll = Some(0.0);
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
                    // M4-BUG-01：菜单动作在 draw_overlays 消费，不能落进 `_ => {}`。
                    // 之前漏接这一支，导致「✓ 方格显示文件名」点击无效果（032-beta4
                    // 验收报告的 P0 阻塞项）；翻转逻辑只放在这里，不再双写。
                    crate::topbar::TopbarAction::More(MoreAction::ToggleShowFilename) => {
                        self.grid_show_filename = !self.grid_show_filename;
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

        // M4.1 `M41-UI-05`：照片右键菜单已退役（收藏/说明/标签全部收进工具栏四按钮）。
        // 右键改为「单选该张」，不弹任何菜单；`folder_ctx` 的打开帧豁免机制保留。

        // M4.1 `M41-UI-01`：说明窗口（多选版）。`caption_editor = Some(-1)` 表示
        // 作用于整个 `lib.selection`；`Some(id)` 保留单张语义（兼容既有调用）。
        if let Some(target) = self.caption_editor {
            let multi = target == -1;
            let n = if multi { self.lib.selection.len() } else { 1 };
            let title = if multi {
                format!("照片说明（{n} 张）")
            } else {
                "照片说明".to_string()
            };
            let mut open = true;
            egui::Window::new(&title)
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .show(ui, |ui| {
                    ui.text_edit_multiline(&mut self.caption_buf);
                    if multi {
                        ui.label(
                            egui::RichText::new(format!(
                                "将保存到选中的 {n} 张照片（只写应用索引，不改原图）"
                            ))
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                        );
                    }
                    ui.horizontal(|ui| {
                        if ui.button("保存").clicked() {
                            let ids: Vec<i64> = if multi {
                                self.lib.selection.iter().copied().collect()
                            } else {
                                vec![target]
                            };
                            let mut ok = 0usize;
                            for id in &ids {
                                if self.lib.set_caption(*id, &self.caption_buf).is_ok() {
                                    ok += 1;
                                }
                            }
                            self.lib.toast = Some((format!("已保存 {ok} 张说明"), 2.0));
                            self.caption_editor = None; // 直接关闭，绕开 open 的借用
                        }
                    });
                });
            if !open {
                self.caption_editor = None;
            }
        }

        // M4.1 `M41-UI-01/08`：批量标签浮层（多选集）。`tag_editor = Some(-1)`。
        // M4.2 `M42-UI-06`：三态结果走内存缓存，命中零查库、未命中恰好 2 次查库
        //（`all_tags()` + `tags_for_ids()`），与标签数量无关；不再对每个标签各查一次
        // `ids_with_tag`（旧实现是 O(标签数) 次查库、且每帧重查）。
        if let Some(_target) = self.tag_editor {
            let ids: Vec<i64> = self.lib.selection.iter().copied().collect();
            let n = ids.len();
            let key = tag_tri_key(&ids);
            let all_tags: Vec<(String, i64)>;
            let tag_counts: HashMap<String, usize>;
            if self.tag_tri_cache.as_ref().is_some_and(|c| c.key == key) {
                // 命中缓存：零查库直接复用（AC-01 / AC-03）。
                let cache = self.tag_tri_cache.as_ref().expect("just checked");
                all_tags = cache.all_tags.clone();
                tag_counts = cache.counts.clone();
            } else {
                // 未命中：一次批量查（`tags_for_ids` = 1 次 IN 查）+ 一次标签清单，
                // 纯函数算三态；查库次数恒定、与标签数量无关。
                all_tags = self.lib.all_tags().unwrap_or_default();
                let by_photo = self.lib.tags_for_ids(&ids).unwrap_or_default();
                tag_counts = Library::tri_state_counts(&ids, &by_photo, &all_tags);
                self.tag_tri_cache = Some(TagTriCache {
                    key,
                    all_tags: all_tags.clone(),
                    counts: tag_counts.clone(),
                });
            }
            let mut open = true;
            let mut changed = false;
            let mut toast_msg: Option<String> = None;
            egui::Window::new(if n > 1 { format!("批量标签 · {n} 张照片") } else { "批量标签".into() })
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .show(ui, |ui| {
                    for (label, _count) in &all_tags {
                        let with = *tag_counts.get(label).unwrap_or(&0);
                        let mark = if with == n && n > 0 {
                            "☑"
                        } else if with > 0 {
                            "▣"
                        } else {
                            "☐"
                        };
                        if ui.button(format!("{mark} {label}")).clicked() {
                            if with == n && n > 0 {
                                if let Ok(k) = self.lib.batch_remove_tag(label) { changed |= k > 0; }
                            } else if let Ok(k) = self.lib.batch_add_tag(label) {
                                changed |= k > 0;
                                toast_msg = Some(format!("已为 {k} 张打上「{label}」"));
                            }
                        }
                    }
                    if all_tags.is_empty() {
                        ui.label("（还没有标签，可在下方新建）");
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.tag_buf);
                        if ui.button("新建并添加").clicked() {
                            let label = self.tag_buf.trim().to_string();
                            if !label.is_empty() {
                                if let Ok(k) = self.lib.batch_add_tag(&label) {
                                    changed |= k > 0;
                                    toast_msg = Some(format!("已为 {k} 张打上「{label}」"));
                                }
                                self.tag_buf.clear();
                            }
                        }
                    });
                    ui.label(
                        egui::RichText::new("取消勾选 = 从选中集同步移除该标签")
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    );
                });
            if changed {
                // M4.2 `M42-UI-06`：标签写操作后失效缓存，下一帧浮层重算一次，
                // 保证重开时三态正确（AC-02）；左栏分组也一并失效重取。
                self.tag_tri_cache = None;
                self.sidebar_groups_dirty = true;
            }
            if let Some(m) = toast_msg {
                self.lib.toast = Some((m, 2.0));
            }
            if !open {
                self.tag_editor = None;
                self.tag_tri_cache = None;
            }
        }

        // M4.1 `M41-UI-08`：筛选面板只管「查什么」；批量收藏 / 批量标签
        // 已收进工具栏 ♥ / ⌗ 按钮，这里不再有批量操作分支。
        if self.filter_menu_open && self.viewer.is_none() {
            let current = self.lib.filter.clone().unwrap_or_default();
            let (action, _tag_out) = toolbar::filter_menu(
                ui,
                self.filter_menu_pos,
                &current,
                &mut self.filter_draft_tag,
            );
            if let Some(action) = action {
                match action {
                    FilterPanelAction::Apply(f) => {
                        let f = *f;
                        // 用 `PhotoFilter::is_active()` 统一判定「全默认不算筛选」
                        // （M4 §4.1 语义），避免此处漏掉 M4 新增维度。
                        let has = f.is_active();
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
            let opened_this_frame =
                context_menu_open_frame_exempt(self.folder_ctx_frame, ctx.cumulative_frame_nr());
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

        // M4.2 `M42-UI-04`：左栏「＋」新建标签 / 相册（真接线，不再只弹 toast）。
        //
        // 两种语义不同（docs/16 §3 实装口径）：相册有独立定义行，create_album 直接建；
        // 标签只是 `(photo_id,label)` 关联表、没有独立定义，新建 = 对当前选中集建首条
        // 关联（batch_add_tag），无选中集时不落库。写操作全部失败即 toast、不脏库。
        if let Some(kind) = self.new_item {
            let (title, hint): (String, String) = match kind {
                NewItemKind::Tag => (
                    "新建标签".into(),
                    "标签依附照片：将为当前选中集打上该标签".into(),
                ),
                NewItemKind::Album => (
                    "新建相册".into(),
                    "相册是独立集合，创建后可右键加入照片".into(),
                ),
            };
            let mut open = true;
            let mut pending: Option<String> = None;
            let mut cancel = false;
            egui::Window::new(&title)
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    ui.text_edit_singleline(&mut self.new_item_buf);
                    ui.label(
                        egui::RichText::new(&hint)
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("创建").clicked() {
                            pending = Some(self.new_item_buf.trim().to_string());
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });
            if let Some(name) = pending {
                if !new_item_name_valid(&name) {
                    self.lib.toast = Some(("名称不能为空".into(), 2.0));
                } else {
                    match kind {
                        NewItemKind::Album => {
                            let now = chrono::Utc::now().timestamp();
                            match self.lib.create_album(&name, now) {
                                Ok(_) => {
                                    self.lib.toast =
                                        Some((format!("已创建相册「{name}」"), 2.0));
                                    self.sidebar_groups_dirty = true;
                                    self.new_item = None;
                                }
                                Err(e) => {
                                    self.lib.toast = Some((format!("创建失败：{e}"), 3.0));
                                }
                            }
                        }
                        NewItemKind::Tag => {
                            if self.lib.selection.is_empty() {
                                self.lib.toast =
                                    Some(("先选中照片再新建标签".into(), 3.0));
                            } else {
                                match self.lib.batch_add_tag(&name) {
                                    Ok(k) => {
                                        self.lib.toast = Some((
                                            format!("已为 {k} 张打上「{name}」"),
                                            2.0,
                                        ));
                                        self.sidebar_groups_dirty = true;
                                        self.new_item = None;
                                    }
                                    Err(e) => {
                                        self.lib.toast =
                                            Some((format!("新建失败：{e}"), 3.0));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if cancel || !open {
                self.new_item = None;
            }
        }

        // M4.2 `M42-UI-05`：相册行右键菜单（改名 / 删除 / 加入当前选中集）。
        // 与文件夹右键同范式：记打开帧豁免「刚画出来就被自己关掉」。
        // IDX-21 ⑤：导入来源节点的右键菜单——移除（保标记）/ 仅从分组隐藏。
        if let Some((source_id, pos)) = self.import_ctx {
            let mut close = false;
            let mut do_remove = false;
            let mut do_hide = false;
            let name = self
                .sidebar_import_groups
                .iter()
                .find(|g| g.source_id == source_id)
                .map(|g| g.name.clone())
                .unwrap_or_else(|| "导入的索引".to_string());
            let area = egui::Area::new(egui::Id::unique("picasa.import-ctx"))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    egui::Frame::new()
                        .fill(Color32::WHITE)
                        .stroke(egui::Stroke::new(1.0, Color32::from_gray(0xC0)))
                        .corner_radius(3.0)
                        .inner_margin(egui::Margin::same(4))
                        .show(ui, |ui| {
                            ui.set_min_width(230.0);
                            ui.label(
                                egui::RichText::new(format!("「{name}」"))
                                    .size(11.0)
                                    .color(Color32::from_gray(0x70)),
                            );
                            if ui
                                .add(egui::Button::new("仅从分组隐藏").frame(false))
                                .clicked()
                            {
                                do_hide = true;
                                close = true;
                            }
                            ui.separator();
                            if ui
                                .add(egui::Button::new("从导入的索引中移除…").frame(false))
                                .clicked()
                            {
                                do_remove = true;
                                close = true;
                            }
                            ui.label(
                                egui::RichText::new("只删索引与缩略图；收藏/标签/说明/相册保留，原图不动")
                                    .size(10.0)
                                    .color(Color32::from_gray(0x88)),
                            );
                        });
                });
            let opened_this_frame = context_menu_open_frame_exempt(
                self.import_ctx_frame,
                ctx.cumulative_frame_nr(),
            );
            if !opened_this_frame {
                let menu_rect = area.response.rect;
                let clicked_outside = ctx.input(|i| i.pointer.any_click())
                    && ctx
                        .input(|i| i.pointer.interact_pos())
                        .is_some_and(|p| !menu_rect.contains(p));
                if close || clicked_outside {
                    self.import_ctx = None;
                }
            }
            if do_hide {
                if let Ok(db) = self.lib.db() {
                    let _ = db.set_import_source_hidden(source_id, true);
                }
                self.sidebar_groups_dirty = true;
                self.lib.toast = Some(("已从「导入的索引」分组隐藏（可随时恢复）".into(), 3.0));
            }
            if do_remove {
                self.confirm_import_source = Some(source_id);
            }
        }

        if let Some((name, pos)) = self.album_ctx.clone() {
            let mut close = false;
            let mut do_rename = false;
            let mut do_delete = false;
            let mut do_add = false;
            let n_sel = self.lib.selection.len();
            let area = egui::Area::new(egui::Id::unique("picasa.album-ctx"))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    egui::Frame::new()
                        .fill(Color32::WHITE)
                        .stroke(egui::Stroke::new(1.0, Color32::from_gray(0xC0)))
                        .corner_radius(3.0)
                        .inner_margin(egui::Margin::same(4))
                        .show(ui, |ui| {
                            ui.set_min_width(210.0);
                            if ui
                                .add(egui::Button::new("改名").frame(false))
                                .clicked()
                            {
                                do_rename = true;
                                close = true;
                            }
                            // 无选中集时禁用（AC-04 空相册 / 无选中禁用态）
                            if ui
                                .add_enabled(
                                    n_sel > 0,
                                    egui::Button::new(format!(
                                        "加入当前选中的 {n_sel} 张照片"
                                    ))
                                    .frame(false),
                                )
                                .clicked()
                            {
                                do_add = true;
                                close = true;
                            }
                            ui.separator();
                            if ui
                                .add(egui::Button::new("删除…").frame(false))
                                .clicked()
                            {
                                do_delete = true;
                                close = true;
                            }
                        });
                });
            let opened_this_frame = context_menu_open_frame_exempt(
                self.album_ctx_frame,
                ctx.cumulative_frame_nr(),
            );
            if !opened_this_frame {
                let menu_rect = area.response.rect;
                let clicked_outside = ctx.input(|i| i.pointer.any_click())
                    && ctx
                        .input(|i| i.pointer.interact_pos())
                        .is_some_and(|p| !menu_rect.contains(p));
                if close || clicked_outside {
                    self.album_ctx = None;
                }
            }
            if do_rename {
                self.rename_album = Some(name.clone());
                self.rename_album_buf = name.clone();
            }
            if do_delete {
                self.confirm_delete_album = Some(name.clone());
            }
            if do_add {
                if let Some(id) = album_id_by_name(&self.sidebar_album_ids, &name) {
                    let ids: Vec<i64> = self.lib.selection.iter().copied().collect();
                    let now = chrono::Utc::now().timestamp();
                    match self.lib.add_to_album(id, &ids, now) {
                        Ok(k) => {
                            self.lib.toast =
                                Some((format!("已加入 {k} 张到「{name}」"), 2.0));
                            self.sidebar_groups_dirty = true;
                        }
                        Err(e) => {
                            self.lib.toast = Some((format!("加入失败：{e}"), 3.0));
                        }
                    }
                } else {
                    self.lib.toast = Some(("找不到该相册，请刷新后重试".into(), 3.0));
                }
            }
        }

        // M4.2 `M42-UI-05`：相册改名（行内输入窗口）。
        if let Some(old_name) = self.rename_album.clone() {
            let mut open = true;
            let mut pending: Option<String> = None;
            let mut cancel = false;
            egui::Window::new("相册改名")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    ui.text_edit_singleline(&mut self.rename_album_buf);
                    ui.horizontal(|ui| {
                        if ui.button("保存").clicked() {
                            pending = Some(self.rename_album_buf.trim().to_string());
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });
            if let Some(new_name) = pending {
                if !new_item_name_valid(&new_name) {
                    self.lib.toast = Some(("名称不能为空".into(), 2.0));
                } else if let Some(id) =
                    album_id_by_name(&self.sidebar_album_ids, &old_name)
                {
                    match self.lib.rename_album(id, &new_name) {
                        Ok(()) => {
                            self.lib.toast =
                                Some((format!("已改名为「{new_name}」"), 2.0));
                            self.sidebar_groups_dirty = true;
                            self.rename_album = None;
                        }
                        Err(e) => {
                            self.lib.toast = Some((format!("改名失败：{e}"), 3.0));
                        }
                    }
                } else {
                    self.lib.toast = Some(("找不到该相册，请刷新后重试".into(), 3.0));
                }
            }
            if cancel || !open {
                self.rename_album = None;
            }
        }

        // M4.2 `M42-UI-05`：删除相册二次确认（只解除关联，不删照片文件）。
        if let Some(name) = self.confirm_delete_album.clone() {
            let mut open = true;
            let mut confirm = false;
            let mut cancel = false;
            egui::Window::new("删除相册？")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .open(&mut open)
                .show(&ctx, |ui| {
                    ui.label(format!(
                        "删除相册「{name}」？\n\n只解除相册关联，不会删除照片文件。取消零副作用。"
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                        if ui.button("删除").clicked() {
                            confirm = true;
                        }
                    });
                });
            if confirm {
                if let Some(id) = album_id_by_name(&self.sidebar_album_ids, &name) {
                    match self.lib.delete_album(id) {
                        Ok(()) => {
                            self.lib.toast =
                                Some((format!("已删除相册「{name}」，照片仍在库"), 2.0));
                            self.sidebar_groups_dirty = true;
                        }
                        Err(e) => {
                            self.lib.toast = Some((format!("删除失败：{e}"), 3.0));
                        }
                    }
                } else {
                    self.lib.toast = Some(("找不到该相册，请刷新后重试".into(), 3.0));
                }
                self.confirm_delete_album = None;
            }
            if cancel || !open {
                self.confirm_delete_album = None;
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
        //
        // M4-BUG-02：`lib.toast` 的第二元素是「持续秒数」（2.0～6.0），不是「到期时刻」。
        // 旧代码写 `remaining = ttl - now`，而 `now` 是自启动累计秒数，导致启动几秒后
        // 任何 toast 的 remaining 恒为负、被立刻清掉（032-beta4 报告 §6 附带观察）。
        // 修法：每帧按 `stable_dt` 递减这个秒数，≤0 才清除。这样 app.rs 约 40 处、
        // core/library.rs 4 处写入点（都只写持续秒数）全部无需改动，时间口径统一。
        if let Some((msg, secs_left)) = self.lib.toast.clone() {
            let dt: f64 = ctx.input(|i| i.stable_dt) as f64;
            let secs_left = secs_left - dt;
            if secs_left <= 0.0 {
                self.lib.toast = None;
            } else {
                self.lib.toast = Some((msg.clone(), secs_left));
                // 查看器是「沉浸式」的：提示框会直接压住照片（实测第一版就挡住了主体），
                // 所以查看器打开时不画 toast——状态栏和索引结果在退出查看器后仍然可见。
                if self.viewer.is_none() {
                    let galley = ui.painter().layout_no_wrap(
                        msg,
                        egui::FontId::proportional(12.0),
                        Color32::WHITE,
                    );
                    let pad = vec2(12.0, 6.0);
                    // egui 0.37 移除了 `Context::screen_rect()`，视口矩形改从 InputState 取
                    let screen = ctx.input(|i| i.viewport_rect());
                    let size = galley.size() + pad * 2.0;
                    // 防御：窗口最小化 / 尺寸变化过渡帧里 viewport_rect 可能退化
                    // （0 宽或负坐标）。此时画一个几何无效的 rect 会让 epaint 合并
                    // Mesh 时触发 debug 断言 `Other mesh is invalid`（实机崩溃现场）。
                    // 视口恢复正常后自然重画，不显示这一帧的 toast 即可。
                    if screen.width() >= size.x + 8.0
                        && screen.height() >= size.y + 56.0
                        && screen.is_finite()
                    {
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
                    }
                }
            }
            // 即使查看器打开时未画，也要继续递减直到过期；每帧重绘保证按时消失。
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
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

        if let Some(source_id) = self.confirm_import_source {
            self.draw_import_source_confirm(&ctx, source_id);
        }

        let _ = theme;
    }

    /// 「删除索引」的二次确认。
    ///
    /// 为什么必须二次确认：这是**唯一**会让用户觉得"照片被删了"的操作。
    /// 事实上只删索引（磁盘上的照片一张不动），但界面上看起来就是图库空了，
    /// 所以文案必须无条件说清"不动磁盘文件"，按钮必须默认不执行。
    fn draw_delete_index_confirm(&mut self, ctx: &egui::Context) {
        // IDX-11：写明「全库」范围与照片数，默认落在「取消」上。
        let photos = self.mgmt.all_photos;
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
                    RichText::new(format!(
                        "范围：全库（所有关注文件夹）。将删除已建立的照片索引与缩略图。\n照片数：{photos} 张（全库）"
                    ))
                    .size(12.5)
                    .color(self.theme.text_primary),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "原图不受影响，照片文件不受影响，磁盘上的照片文件不会被删除；\
                         关注文件夹与「关闭此文件夹」的规则会保留，\
                         收藏 / 标签 / 说明 / 相册等用户标记也会保留（重新扫描后自动挂回）。",
                    )
                    .size(11.0)
                    .color(self.theme.text_secondary),
                );
                ui.add_space(8.0);
                // 默认取消：Escape / 关闭窗口 / 回车都落在「取消」，删除须二次主动点击。
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
                ui.horizontal(|ui| {
                    let cancel_btn = ui.button("取消");
                    cancel_btn.request_focus();
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                    if ui.button("删除索引").clicked() {
                        go = true;
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

    /// IDX-21 ⑤：「从导入的索引中移除」的二次确认。
    ///
    /// 与「删除索引」同一套口径（IDX-11）：写清范围、路径、照片数、保留项，
    /// 默认落在「取消」上。**只删索引与缩略图**，标记走身份骨架保留，
    /// 磁盘上的照片与原目录一概不动（R-M43-01 / R-M43-02）。
    fn draw_import_source_confirm(&mut self, ctx: &egui::Context, source_id: i64) {
        let (name, photos, paths) = self
            .lib
            .db()
            .map(|db| {
                let name = db
                    .import_source(source_id)
                    .ok()
                    .flatten()
                    .map(|s| s.name)
                    .unwrap_or_else(|| "导入的索引".to_string());
                let photos = db.import_source_photo_count(source_id).unwrap_or(0);
                let paths: Vec<String> = db
                    .imported_roots(source_id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(_, p, _, _)| p)
                    .collect();
                (name, photos, paths)
            })
            .unwrap_or_else(|_| ("导入的索引".to_string(), 0, Vec::new()));
        let mut go = false;
        let mut cancel = false;
        egui::Window::new("从导入的索引中移除")
            .id(egui::Id::unique("import-source-confirm"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.label(
                    RichText::new(format!(
                        "范围：导入来源「{name}」下的全部文件夹。\n照片数：{photos} 张"
                    ))
                    .size(12.5)
                    .color(self.theme.text_primary),
                );
                if !paths.is_empty() {
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(format!("原路径：{}", paths.join("、")))
                            .size(11.0)
                            .color(self.theme.text_secondary),
                    );
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "只删除这组导入的索引与缩略图；原图与原目录不受影响（不会删除、不会新建任何文件）。\
                         收藏 / 标签 / 说明 / 相册等用户标记会保留，重新导入或重扫后自动挂回。",
                    )
                    .size(11.0)
                    .color(self.theme.text_secondary),
                );
                ui.add_space(8.0);
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
                ui.horizontal(|ui| {
                    let cancel_btn = ui.button("取消");
                    cancel_btn.request_focus();
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                    if ui.button("移除").clicked() {
                        go = true;
                    }
                });
            });
        if go {
            self.confirm_import_source = None;
            let outcome = self
                .lib
                .db()
                .map_err(|e| e.to_string())
                .and_then(|db| {
                    picasa_db::remove_import_source(&db, source_id).map_err(|e| e.to_string())
                });
            match outcome {
                Ok((photos, thumbs)) => {
                    self.lib.toast = Some((
                        format!("已移除导入的索引：{photos} 张照片、{thumbs} 张缩略图（原图未动）"),
                        4.0,
                    ));
                    self.sidebar_groups_dirty = true;
                    self.mgmt_next_refresh = std::time::Instant::now();
                }
                Err(e) => {
                    self.lib.toast = Some((format!("移除导入的索引失败：{e}"), 5.0));
                }
            }
        }
        if cancel {
            self.confirm_import_source = None;
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
        // `IDX-01` 入口三件事：可读性、同路径复用、父子重叠。
        // 先探可读性——加进去只会得到一棵空树和一条看不懂的失败任务。
        if let Some(why) = picasa_core::root_intake::probe_dir(&dir) {
            tracing::info!(dir = %dir.display(), %why, "拒绝添加不可读目录");
            self.lib.toast = Some((why, 5.0));
            return;
        }
        let wanted = picasa_core::paths::canonical_string(&dir);
        let existing: Vec<String> = self
            .lib
            .db()
            .ok()
            .and_then(|db| db.roots(false).ok())
            .map(|rows| rows.into_iter().map(|r| r.path).collect())
            .unwrap_or_default();
        let intake = picasa_core::root_intake::classify(&wanted, &existing);
        if intake.skips_insert() {
            // 已经关注过：不插第二行，但仍然重扫一次（用户点它多半是想刷新）。
            tracing::info!(dir = %wanted, "重复添加已关注的根，复用既有行");
            self.lib.toast = Some(("这个文件夹已经在图库里，正在重新索引".into(), 3.0));
            self.index.enqueue_scan(&[dir]);
            return;
        }
        self.lib.add_root(&dir);
        self.lib.save_prefs();
        if let Err(e) = self.lib.reload_tree() {
            tracing::warn!(error = %e, "刷新目录树失败");
            self.lib.toast = Some(("无法读取该目录".into(), 4.0));
        }
        // 父子重叠不会让照片重复计数（同一文件只有一行），但树里会同批照片出现两次。
        // 明确说出来，避免用户之后误以为「移除一个根为什么另一个也少了照片」。
        if let picasa_core::RootIntake::ParentOf(covered) = &intake {
            tracing::info!(dir = %wanted, covered = covered.len(), "新根包含已关注的文件夹");
            self.lib.toast = Some((
                format!(
                    "已添加；它包含 {} 个已关注的文件夹，照片不会重复计数",
                    covered.len()
                ),
                5.0,
            ));
        } else if let picasa_core::RootIntake::ChildOf(parent) = &intake {
            tracing::info!(dir = %wanted, parent = %parent, "新根在已关注范围内");
            self.lib.toast = Some(("已添加；它的照片本来也在关注范围内，不会重复计数".into(), 5.0));
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
        // M4：EXIF 回填（后台线程，UI 帧只收结果，不读盘）
        self.tick_m4_exif(&ctx);
        // M4.2 `M42-PERF-01`：轮询后台导入 / 导出任务（try_recv，零阻塞）。
        // 进度 / 六态 / 取消由 `apply_io_msg` 写回 IoUi；成功副作用以纯数据
        // `TaskEffects` 返回，在这里（唯一能碰 app 私有字段的地方）消费。
        // M4.3 §3A ⑦：终态（成功 / 失败 / 取消 / 通道断开兜底）一律清掉
        // `settings["m43_export_pending" / "m43_import_pending"]`——这两个标记
        // 的语义是「任务还在跑」，跑完不清会让下次启动误报「上次未完成」。
        let io_kind = self.import_task.as_ref().map(|t| t.kind());
        while let Some(msg) = self.import_task.as_mut().and_then(|t| t.tick()) {
            // `Preview` 是校验任务的最后一条消息（任务随即结束），一并按终态处理：
            // 它没有持久标记要清，但必须让 `finished` 生效、停止忙等重绘。
            let terminal = matches!(
                msg,
                crate::import_task::Msg::Done(_)
                    | crate::import_task::Msg::Cancelled
                    | crate::import_task::Msg::Preview(_)
            );
            if let Some(eff) = crate::import_task::apply_io_msg(&mut self.index_mgr.io, msg) {
                if eff.reload_library {
                    if let Err(e) = self.lib.reload_tree() {
                        tracing::warn!(error = %e, "导入后刷新目录树失败");
                    } else if let Err(e) = self.lib.reload_photos() {
                        tracing::warn!(error = %e, "导入后刷新照片失败");
                    }
                }
                if eff.sidebar_groups_dirty {
                    self.sidebar_groups_dirty = true;
                }
                if eff.exif_pending {
                    self.m4_exif_pending = true;
                }
                if let Some((text, secs)) = eff.toast {
                    self.lib.toast = Some((text, f64::from(secs)));
                }
                // IDX-06：仅导出成功后持久化「最近成功备份」，重启后仍显示。
                if let Some(json) = eff.last_backup_json
                    && let Ok(db) = self.lib.db()
                    && let Err(e) = db.set_setting_raw(crate::index_mgr::LAST_BACKUP_KEY, Some(&json))
                {
                    tracing::warn!(error = %e, "记录最近成功备份失败");
                }
                // §3A ⑦：导入**提交成功**才写「已提交的结果摘要」+ 清进行中标记。
                if let Some(summary) = eff.import_summary {
                    if let Ok(db) = self.lib.db()
                        && let Err(e) =
                            db.set_setting_raw("m43_import_summary", Some(&summary.to_json()))
                    {
                        tracing::warn!(error = %e, "记录导入结果摘要失败");
                    }
                    self.index_mgr.io.last_import = Some(summary);
                }
            }
            if terminal
                && let Some(kind) = io_kind
            {
                match kind {
                    crate::import_task::IoJobKind::Export => {
                        self.clear_io_marker("m43_export_pending");
                    }
                    crate::import_task::IoJobKind::Import => {
                        self.clear_io_marker("m43_import_pending");
                    }
                    // 校验（Inspect）从不写库，没有「进行中」标记可清。
                    crate::import_task::IoJobKind::Inspect => {}
                }
            }
        }
        // 仅任务未收到终态时请求下一帧；Done/Cancelled 后 finished=true 自动停
        // ⇒ 无常驻动画定时器（R-M41-1 / R-M42-1）。
        if self.import_task.as_ref().is_some_and(|t| t.is_active()) {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
        // IDX-22 ⑧：先把后台算好的标签计数取回来（非阻塞），再决定要不要派新的。
        self.poll_tag_counts(&ctx);
        // M4：左栏标签/相册缓存（脏时才查库，不每帧查）
        self.refresh_sidebar_groups();
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

/// 右键菜单在「打开帧」是否应豁免关闭判定（纯函数，可单测）。
///
/// 右键释放帧里 `secondary_clicked()` 与 `pointer.any_click()` 同时为真，
/// 若不豁免，菜单会在画出来的同一帧被自己关掉——用户看到「右键没反应」。
/// `folder_ctx` 与旧 `photo_ctx` 都依赖它；M4.1 照片菜单退役后仍被 folder_ctx 使用。
fn context_menu_open_frame_exempt(opened_frame: u64, current_frame: u64) -> bool {
    opened_frame == current_frame
}

#[cfg(test)]
mod m4_tests {
    use super::album_id_by_name;
    use super::context_menu_open_frame_exempt;
    use super::new_item_name_valid;
    use super::tag_tri_key;
    use std::collections::HashMap;

    #[test]
    fn context_menu_open_frame_exempt_is_precise() {
        // 打开帧必须豁免：否则右键释放的 any_click 会立刻把菜单关掉
        assert!(context_menu_open_frame_exempt(42, 42));
        // 之后的帧不再豁免，点空白可正常关闭
        assert!(!context_menu_open_frame_exempt(42, 43));
        assert!(!context_menu_open_frame_exempt(42, 1000));
        // 初始值 u64::MAX（还没打开过）不应误豁免
        assert!(!context_menu_open_frame_exempt(u64::MAX, 0));
    }

    /// M4.2 `M42-UI-06` TC-M42-B-06-02：缓存指纹与迭代顺序无关、选中集变化即失效。
    #[test]
    fn tag_tri_key_is_order_independent() {
        let a = tag_tri_key(&[7, 3, 5]);
        let b = tag_tri_key(&[5, 7, 3]);
        let c = tag_tri_key(&[7, 3, 5, 9]);
        assert_eq!(a, vec![3, 5, 7]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(tag_tri_key(&[]).is_empty());
    }

    /// M4.2 `M42-UI-06` TC-M42-B-06-03：三态计数 ☑/▣/☐ 与空集 / 空标签表边界。
    #[test]
    fn tri_state_counts_covers_all_three_states() {
        let ids = [1_i64, 2, 3];
        let mut by_photo: HashMap<i64, Vec<String>> = HashMap::new();
        by_photo.insert(1, vec!["旅行".into(), "人物".into()]);
        by_photo.insert(2, vec!["旅行".into()]);
        // 3 号没有任何标签
        let all_labels = vec![("旅行".to_string(), 2), ("人物".to_string(), 1), ("风景".to_string(), 0)];
        let counts = picasa_core::Library::tri_state_counts(&ids, &by_photo, &all_labels);
        // 旅行：2/3 → ▣（部分）；人物：1/3 → ▣；风景：0 → ☐
        assert_eq!(counts.get("旅行"), Some(&2));
        assert_eq!(counts.get("人物"), Some(&1));
        assert_eq!(counts.get("风景"), Some(&0));
        // 全部命中 → ☑
        let full = picasa_core::Library::tri_state_counts(&[1, 2], &by_photo, &all_labels);
        assert_eq!(full.get("旅行"), Some(&2));
        // 空选中集：所有标签计数都是 0（☐），不 panic、不查库
        let empty_sel = picasa_core::Library::tri_state_counts(&[], &by_photo, &all_labels);
        assert!(empty_sel.values().all(|&v| v == 0));
        // 空标签表：返回空 map
        assert!(picasa_core::Library::tri_state_counts(&ids, &by_photo, &[]).is_empty());
    }

    /// M4.2 `M42-UI-04` TC-M42-B-04-03：空名 / 纯空白名非法，去空白后非空才合法。
    #[test]
    fn new_item_name_rejects_blank() {
        assert!(!new_item_name_valid(""));
        assert!(!new_item_name_valid("   "));
        assert!(new_item_name_valid("  旅行  "));
        assert!(new_item_name_valid("A"));
    }

    /// M4.2 `M42-UI-05` TC-M42-B-05-02：按相册名反查 id，找不到返回 None。
    #[test]
    fn album_id_lookup_by_name() {
        let ids = vec![("2024 夏".to_string(), 3), ("人物".to_string(), 7)];
        assert_eq!(album_id_by_name(&ids, "2024 夏"), Some(3));
        assert_eq!(album_id_by_name(&ids, "人物"), Some(7));
        assert_eq!(album_id_by_name(&ids, "不存在"), None);
        assert_eq!(album_id_by_name(&[], "任何"), None);
    }
}
