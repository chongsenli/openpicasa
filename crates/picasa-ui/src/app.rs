//! 应用主结构：把「库会话（core）」「纹理缓存」「后台事件」「五区界面」缝在一起。
//!
//! ## 这个文件只做编排，不做算法
//!
//! 排序/选择/扫描/解码都在 `picasa-core`；布局/绘制在 `grid`/`sidebar`/`viewer`。
//! 这里负责：**面板嵌套顺序**、**事件泵**、**动作分发**、**偏好落盘**。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, Pos2, Rect, RichText, vec2};
use picasa_core::{
    AppPaths, CoreEvent, DimsService, IndexBoard, IndexService, Library, ThumbService, Tier,
    watcher,
};
use picasa_db::{PhotoRow, SortKey};

use crate::grid;
use crate::index_progress::{IndexProgressUi, ProgressAction};
use crate::shortcuts::{self, KeyAction};
use crate::sidebar::{self, SidebarAction, SidebarCtx};
use crate::statusbar;
use crate::textures::{TexState, TextureCache};
use crate::theme::{Metrics, Theme};
use crate::toolbar::{self, ToolbarAction};
use crate::viewer::{self, Viewer, ViewerAction};

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
    tx: Sender<CoreEvent>,
    /// 索引服务（M2 A 线）：关注文件夹 → 任务队列 → 扫描/缩略图
    index: IndexService,
    /// 宽高回填服务（M2.1 B）：可见优先、给索引让路
    dims: DimsService,
    /// 索引进度板：**四处展示的唯一数据源**
    board: Arc<IndexBoard>,
    /// 进度弹框/面板的界面状态（含"本会话关闭过哪个批次"）
    progress: IndexProgressUi,
    watch: Option<watcher::WatchHandle>,

    viewer: Option<Viewer>,

    // ---- 纯 UI 状态（不进偏好，除非注明）----
    search_buf: String,
    sort_menu_open: bool,
    sort_menu_pos: Pos2,
    /// 排序菜单打开的帧号，用途同 `folder_ctx_frame`（点按钮也是 click，同一帧会被误关）
    sort_menu_frame: u64,
    tray_expanded: bool,
    favorites_collapsed: bool,
    folders_collapsed: bool,
    folder_ctx: Option<(String, Pos2)>,
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

    // ---- 工具栏「索引管理」----
    /// 二级菜单是否打开
    index_menu_open: bool,
    /// 二级菜单的落点（按钮左下角）
    index_menu_pos: Pos2,
    /// 菜单打开的帧号，用途同 `sort_menu_frame`
    index_menu_frame: u64,
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
            watch: None,
            viewer: None,
            search_buf,
            sort_menu_open: false,
            sort_menu_pos: Pos2::ZERO,
            sort_menu_frame: u64::MAX,
            tray_expanded: false,
            favorites_collapsed: false,
            folders_collapsed: false,
            folder_ctx: None,
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
            index_menu_open: false,
            index_menu_pos: Pos2::ZERO,
            index_menu_frame: u64::MAX,
            failed_thumbs: 0,
            reload_now: false,
            restore_scroll,
            anchor_id: None,
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
        // 角标要在**第一帧**就有数：用户打开程序的第一眼就该知道"有 970 张坏图"，
        // 而不是等他点开菜单才去算。
        app.refresh_failed_thumbs();
        app.start_index();
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
        self.watch = watcher::spawn_watch(roots, self.tx.clone());
    }

    fn refresh_tray_rows(&mut self) {
        self.tray_rows = self.lib.tray_photos();
        self.tray_rows_dirty = false;
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
            KeyAction::TierSmall => self.lib.set_tier(Tier::Small),
            KeyAction::TierLarge => self.lib.set_tier(Tier::Large),
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
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        match action {
            KeyAction::CloseViewer => self.viewer = None,
            KeyAction::Next => {
                if v.index + 1 < self.lib.photos.len() {
                    v.jump_to(v.index + 1);
                }
            }
            KeyAction::Prev => {
                if v.index > 0 {
                    v.jump_to(v.index - 1);
                }
            }
            KeyAction::ZoomIn => v.zoom_in(),
            KeyAction::ZoomOut => v.zoom_out(),
            KeyAction::ZoomFit => v.zoom = 0.0001, // 下一帧会被 fit 覆盖
            KeyAction::ZoomActual => v.zoom = 1.0,
            KeyAction::ToggleBackground => v.toggle_bg(),
            KeyAction::ToggleInfo => v.info_open = !v.info_open,
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
        // 本帧的进度切片：状态栏/左栏/栅格**读同一份**（`04` §3.2 的四处一致）
        let snap = self.progress.snapshot().clone();

        // ① 顶部工具栏
        egui::Panel::top("toolbar-panel")
            .exact_size(Metrics::TOOLBAR_H)
            .frame(egui::Frame::new().fill(theme.chrome_bg))
            .show(ui, |ui| {
                let out = toolbar::show(
                    ui,
                    &theme,
                    self.lib.tier,
                    self.lib.sort,
                    !self.lib.selection.is_empty(),
                    &mut self.search_buf,
                    self.sort_menu_open,
                    self.index_menu_open,
                    self.failed_thumbs,
                );
                let sort_btn_rect = out.sort_button.as_ref().map(|r| r.rect);
                let index_btn_rect = out.index_button.as_ref().map(|r| r.rect);
                if let Some(new_search) = out.search_changed {
                    if let Err(e) = self.lib.set_search(new_search) {
                        tracing::warn!(error = %e, "搜索失败");
                    }
                    self.restore_scroll = Some(0.0);
                }
                if let Some(rect) = sort_btn_rect {
                    self.sort_menu_pos = rect.left_bottom() + vec2(0.0, 2.0);
                }
                if let Some(rect) = index_btn_rect {
                    self.index_menu_pos = rect.left_bottom() + vec2(0.0, 2.0);
                }
                for action in out.actions {
                    match action {
                        ToolbarAction::AddFolder => self.add_folder(),
                        ToolbarAction::RevealCurrent => self.reveal_selected(),
                        ToolbarAction::ToggleSortMenu => {
                            self.sort_menu_open = !self.sort_menu_open;
                            self.index_menu_open = false; // 两个二级菜单不同时开
                            if self.sort_menu_open {
                                self.sort_menu_frame = ctx.cumulative_frame_nr();
                            }
                        }
                        ToolbarAction::PickSort(key) => {
                            self.pick_sort(key);
                            self.sort_menu_open = false;
                        }
                        ToolbarAction::ToggleSortDirection => {
                            let key = self.lib.sort.key;
                            self.pick_sort(key);
                            self.sort_menu_open = false;
                        }
                        ToolbarAction::CycleTier => {
                            let next = match self.lib.tier {
                                Tier::Small => Tier::Medium,
                                Tier::Medium => Tier::Large,
                                Tier::Large => Tier::Small,
                            };
                            self.lib.set_tier(next);
                        }
                        ToolbarAction::ToggleFullscreen => {
                            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                        }
                        ToolbarAction::ToggleIndexMenu => {
                            self.index_menu_open = !self.index_menu_open;
                            self.sort_menu_open = false; // 两个二级菜单不同时开
                            if self.index_menu_open {
                                self.index_menu_frame = ctx.cumulative_frame_nr();
                                // 打开时重取一次：角标可能是几小时前算的
                                self.refresh_failed_thumbs();
                            }
                        }
                    }
                }
            });

        // ⑤ 底部状态栏（**在左栏之前**，否则侧栏会盖住它）
        egui::Panel::bottom("statusbar-panel")
            .exact_size(Metrics::STATUS_H)
            .frame(egui::Frame::new().fill(theme.chrome_bg))
            .show(ui, |ui| {
                let out = statusbar::show(ui, &self.lib, &theme, &snap);
                if out.index_clicked {
                    self.progress.open_panel();
                }
                if out.diag_clicked {
                    self.diagnostics_open = !self.diagnostics_open;
                    self.diag_dirty = true;
                }
            });

        // ② 左栏（含底部托盘）
        let sidebar_resp = egui::Panel::left("sidebar-panel")
            .default_size(self.lib.prefs.sidebar_width)
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
                let actions = sidebar::show(ui, &self.lib, &mut sctx);
                drop(db);
                actions
            });

        // 左栏宽度持久化（U-03：左栏宽度固定，栅格吸收剩余宽度）
        let w = sidebar_resp.response.rect.width();
        if (w - self.lib.prefs.sidebar_width).abs() > 0.5 {
            self.lib.prefs.sidebar_width = w;
            self.lib.mark_prefs_dirty();
        }
        for action in sidebar_resp.inner {
            self.apply_sidebar_action(action, &ctx);
        }

        // ③ 中央栅格
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
                    self.lib.tier,
                    matches!(self.lib.sort.key, SortKey::TakenAt),
                    self.restore_scroll,
                    anchor,
                    &db,
                    &self.service,
                    &mut self.cache,
                    &mut self.grid_layout,
                    &theme,
                    &snap,
                );
                drop(db);

                self.restore_scroll = None;
                // 记住这一帧视口顶部在看哪张：下次重载（扫描出新照片、切排序）时用它锚回来。
                self.anchor_id = self.lib.photos.get(out.top_index).map(|p| p.id);
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
                    let _ = pos;
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
                Some(ViewerAction::Close) => self.viewer = None,
                Some(ViewerAction::Reveal) => self.reveal_selected(),
                Some(ViewerAction::Next) => {
                    if let Some(v) = self.viewer.as_mut()
                        && v.index + 1 < self.lib.photos.len()
                    {
                        let next = v.index + 1;
                        v.jump_to(next);
                    }
                }
                Some(ViewerAction::Prev) => {
                    if let Some(v) = self.viewer.as_mut()
                        && v.index > 0
                    {
                        let prev = v.index - 1;
                        v.jump_to(prev);
                    }
                }
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
                    ToolbarAction::PickSort(key) => self.pick_sort(key),
                    ToolbarAction::ToggleSortDirection => {
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

        // 索引管理二级菜单
        //
        // 关闭规则与排序菜单同款（三条，顺序不能改）：点了菜单项 → 关；
        // 本帧刚由按钮打开 → 豁免（按钮的 click 与「点别处」同帧为真）；
        // 点击落在实测矩形外 → 关。矩形用 `Area` 返回值，不猜尺寸。
        if self.index_menu_open && self.viewer.is_none() {
            let (action, menu_rect) =
                toolbar::index_menu(ui, self.index_menu_pos, self.failed_thumbs);
            if let Some(action) = action {
                self.index_menu_open = false;
                match action {
                    toolbar::IndexMenuAction::RebuildFailed => {
                        self.confirm_rebuild_failed = Some(self.failed_thumbs);
                    }
                    toolbar::IndexMenuAction::OpenProgress => self.progress.open_panel(),
                }
            }
            if self.index_menu_frame != ctx.cumulative_frame_nr()
                && ctx.input(|i| i.pointer.any_click())
            {
                let pos = ctx.input(|i| i.pointer.interact_pos());
                if let Some(p) = pos
                    && !menu_rect.contains(p)
                {
                    self.index_menu_open = false;
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
            Some(ProgressAction::ClosePanel) | None => {}
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
        // 原生 NSOpenPanel：TCC 授权的唯一正规入口（F10-25 / R-3）
        let Some(dir) = crate::macos::pick_folder(start.as_deref()) else {
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
            Ok(true) => ctx.request_repaint(),
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

impl eframe::App for OpenPicasaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.theme = Theme::picasa();

        self.pump_events(&ctx);
        self.flush_reload(&ctx);
        self.handle_keys(&ctx);
        // 索引进度：按 250 ms 节流取快照（不在 UI 线程做任何索引工作）
        self.progress.tick(&ctx, &self.board);

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
        self.lib.save_prefs();
        tracing::info!("退出：偏好已保存");
    }
}
