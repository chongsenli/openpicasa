//! 应用主结构：把「库会话（core）」「纹理缓存」「后台事件」「五区界面」缝在一起。
//!
//! ## 这个文件只做编排，不做算法
//!
//! 排序/选择/扫描/解码都在 `picasa-core`；布局/绘制在 `grid`/`sidebar`/`viewer`。
//! 这里负责：**面板嵌套顺序**、**事件泵**、**动作分发**、**偏好落盘**。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, Align2, Color32, Pos2, Rect, vec2};
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
use crate::textures::TextureCache;
use crate::theme::{Metrics, Theme};
use crate::toolbar::{self, ToolbarAction};
use crate::viewer::{self, Viewer, ViewerAction};

pub struct OpenPicasaApp {
    lib: Library,
    /// 应用容器的路径（诊断/自检用；也保证「谁能写哪儿」在 UI 层只有这一份引用）
    paths: AppPaths,
    theme: Theme,

    cache: TextureCache,
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
    tray_expanded: bool,
    favorites_collapsed: bool,
    folders_collapsed: bool,
    folder_ctx: Option<(String, Pos2)>,
    confirm_trash: Option<usize>,

    /// 需要恢复的滚动位置（只在必要时设置一次，否则会锁死滚动）
    restore_scroll: Option<f32>,
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
            tray_expanded: false,
            favorites_collapsed: false,
            folders_collapsed: false,
            folder_ctx: None,
            confirm_trash: None,
            restore_scroll,
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
        app.start_index();
        app
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
                other => {
                    if self.lib.apply_event(other) {
                        need_reload = true;
                    }
                    self.tray_rows_dirty = true;
                }
            }
        }
        if need_reload {
            // 视图变了（新目录入库/外部变更）：**保持当前位置**，不强制跳回顶部
            if let Err(e) = self.lib.reload_photos() {
                tracing::warn!(error = %e, "刷新照片列表失败");
            }
        }
        if self.tray_rows_dirty {
            self.refresh_tray_rows();
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
                );
                let sort_btn_rect = out.sort_button.as_ref().map(|r| r.rect);
                if let Some(new_search) = out.search_changed {
                    if let Err(e) = self.lib.set_search(new_search) {
                        tracing::warn!(error = %e, "搜索失败");
                    }
                    self.restore_scroll = Some(0.0);
                }
                if let Some(rect) = sort_btn_rect {
                    self.sort_menu_pos = rect.left_bottom() + vec2(0.0, 2.0);
                }
                for action in out.actions {
                    match action {
                        ToolbarAction::AddFolder => self.add_folder(),
                        ToolbarAction::RevealCurrent => self.reveal_selected(),
                        ToolbarAction::ToggleSortMenu => {
                            self.sort_menu_open = !self.sort_menu_open;
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
                let out = grid::show(
                    ui,
                    &self.lib.photos,
                    &self.lib.selection,
                    None,
                    self.lib.tier,
                    matches!(self.lib.sort.key, SortKey::TakenAt),
                    self.restore_scroll,
                    &db,
                    &self.service,
                    &mut self.cache,
                    &theme,
                    &snap,
                );
                drop(db);

                self.restore_scroll = None;
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

    fn apply_sidebar_action(&mut self, action: SidebarAction, _ctx: &egui::Context) {
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
            }
        }
    }

    fn draw_viewer(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        if let Some(v) = self.viewer.as_mut() {
            if let Some(row) = self.lib.photos.get(v.index) {
                viewer::ensure_loaded(v, row, &self.service);
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
            // 点空白处关掉
            if ctx.input(|i| i.pointer.any_click()) {
                let pos = ctx.input(|i| i.pointer.interact_pos());
                if let Some(p) = pos
                    && !Rect::from_min_size(self.sort_menu_pos, vec2(180.0, 200.0)).contains(p)
                {
                    self.sort_menu_open = false;
                }
            }
        }

        // 文件夹右键菜单
        if let Some((path, pos)) = self.folder_ctx.clone() {
            let mut close = false;
            egui::Area::new(ui.scope_id().with("folder-ctx"))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    egui::Frame::new()
                        .fill(Color32::WHITE)
                        .stroke(egui::Stroke::new(1.0, Color32::from_gray(0xC0)))
                        .corner_radius(3.0)
                        .inner_margin(egui::Margin::same(4))
                        .show(ui, |ui| {
                            ui.set_min_width(160.0);
                            if ui
                                .add(egui::Button::new("在 Finder 中打开").frame(false))
                                .clicked()
                            {
                                let _ = crate::macos::open_folder(std::path::Path::new(&path));
                                close = true;
                            }
                            if ui.button("从图库移除（不删文件）").clicked() {
                                self.lib.remove_root(std::path::Path::new(&path));
                                // 任务跟着关注文件夹一起撤掉，否则面板会留一行"幽灵任务"
                                self.board.forget(&path);
                                if let Err(e) = self.lib.reload_tree() {
                                    tracing::warn!(error = %e, "刷新目录树失败");
                                }
                                let _ = self.lib.reload_photos();
                                close = true;
                            }
                        });
                });
            if close || ctx.input(|i| i.pointer.any_click()) {
                self.folder_ctx = None;
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
            action = self.progress.show_panel(&ctx, &self.theme);
        }
        match action {
            Some(ProgressAction::AddFolder) => self.add_folder(),
            Some(ProgressAction::Retry(path)) => self.retry_folder(&path),
            Some(ProgressAction::RetryAll) => self.retry_all(),
            Some(ProgressAction::ClosePanel) | None => {}
        }

        let _ = theme;
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
