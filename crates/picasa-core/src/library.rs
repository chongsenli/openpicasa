//! 库会话：视图、选择集、托盘、搜索、排序——**UI 的唯一数据源**。
//!
//! 这一层刻意保持「同步 + 独占」：UI 线程调用它，它同步查 DB（都是索引查询，10 万张量级下
//! 单次几十毫秒），没有后台线程参与。**简单性优先**——M1 的性能瓶颈在渲染与解码，不在查询。

use crate::{
    CoreError, Result, Tier,
    paths::{self, AppPaths},
    prefs::Preferences,
    scanner::CoreEvent,
};
use picasa_db::{Db, PhotoRow, SortKey, SortSpec};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 三档单元格边长（逻辑 pt）。**U-21 待回填**，改动只改这一行。
pub const CELL_SIZES: [f32; 3] = [85.0, 144.0, 200.0];
/// 单元格间距（U-22 待回填，估 8~12）。
pub const CELL_GAP: f32 = 10.0;
/// 栅格四周留白（U-23 待回填）。
pub const GRID_PADDING: f32 = 12.0;
/// 日期分组头高度（U-30 待回填，估 22~28）。
pub const HEADER_HEIGHT: f32 = 26.0;
/// 文件名行高（U-26：大档才显示）。
pub const FILENAME_HEIGHT: f32 = 16.0;

/// 图库视图。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// 所有照片（自有索引）
    All,
    /// 最近更改（按 mtime 倒序）
    Recent,
    /// 单个文件夹（含子文件夹）
    Folder(i64),
}

impl View {
    pub fn as_pref_string(self) -> String {
        match self {
            View::All => "all".into(),
            View::Recent => "recent".into(),
            View::Folder(id) => format!("folder:{id}"),
        }
    }

    pub fn from_pref_string(s: &str) -> Self {
        match s {
            "all" => View::All,
            "recent" => View::Recent,
            other => other
                .strip_prefix("folder:")
                .and_then(|id| id.parse().ok())
                .map(View::Folder)
                .unwrap_or(View::All),
        }
    }
}

/// 左栏树节点。
#[derive(Clone, Debug)]
pub struct FolderNode {
    pub path: String,
    pub name: String,
    pub folder_id: Option<i64>,
    /// 该目录**直接**收录的图片数。
    pub own_count: u32,
    /// 含子目录的合计（Picasa 显示的是这个）。
    pub count: u32,
    pub children: Vec<FolderNode>,
    pub collapsed: bool,
    pub depth: usize,
}

impl FolderNode {
    /// 把整棵子树按显示顺序摊平成「行」，供左栏虚拟列表使用。
    pub fn flatten(&self, out: &mut Vec<FolderRowView>) {
        out.push(FolderRowView {
            path: self.path.clone(),
            name: self.name.clone(),
            folder_id: self.folder_id,
            count: self.count,
            has_children: !self.children.is_empty(),
            collapsed: self.collapsed,
            depth: self.depth,
        });
        if !self.collapsed {
            for c in &self.children {
                c.flatten(out);
            }
        }
    }
}

/// 左栏一行（扁平化后）。
#[derive(Clone, Debug)]
pub struct FolderRowView {
    pub path: String,
    pub name: String,
    pub folder_id: Option<i64>,
    pub count: u32,
    pub has_children: bool,
    pub collapsed: bool,
    pub depth: usize,
}

/// 库会话。
pub struct Library {
    pub paths: AppPaths,
    pub db: Arc<Mutex<Db>>,
    pub prefs: Preferences,

    pub view: View,
    pub photos: Vec<PhotoRow>,
    pub selection: HashSet<i64>,
    /// 选择锚点（Shift 连选用）
    pub anchor: Option<usize>,
    /// 查看器当前索引（`None` = 没进查看器）
    pub current: Option<usize>,

    pub tray: Vec<i64>,

    pub search: String,
    pub tier: Tier,
    pub sort: SortSpec,

    /// 树（未展开/折叠的完整结构）
    pub roots: Vec<FolderNode>,
    /// 扁平化后的可见行（每帧重建代价极低，直接算）
    pub visible_folders: Vec<FolderRowView>,
    /// 当前 active 的根目录（**来自 DB `roots` 表**，不再读偏好）。
    ///
    /// 保留成缓存是为了 `reload_tree` / `scan_roots` 不必每次查库；
    /// 写入点只有 `refresh_roots()` 一个，避免两处各改一半。
    root_paths: Vec<String>,

    // ---- 状态文案（状态栏/工具栏用）----
    pub total_in_db: u64,
    pub scanning: bool,
    pub scan_progress: Option<(u64, u64)>,
    pub scan_summary: Option<String>,
    pub toast: Option<(String, f64)>,
    pub last_error: Option<String>,

    dirty_prefs: bool,
    path_to_id: HashMap<String, i64>,
}

impl Library {
    /// 打开库（索引库 + 偏好）。**这是应用启动时唯一会创建应用容器的入口**。
    pub fn open(paths: AppPaths) -> Result<Self> {
        Self::open_with_db(paths, false)
    }

    /// 退化路径：索引库不可用时用内存库，至少还能浏览（R-3：不能崩在启动）。
    pub fn open_in_memory(paths: AppPaths) -> Result<Self> {
        Self::open_with_db(paths, true)
    }

    fn open_with_db(paths: AppPaths, in_memory: bool) -> Result<Self> {
        paths.ensure()?;
        let db = if in_memory {
            Db::open_in_memory()?
        } else {
            match Db::open(paths.db_file()) {
                Ok(db) => db,
                Err(e) => {
                    // 索引库坏了不该让应用起不来：用一个空的内存库顶上，
                    // 用户依然能看图，只是本次会话不持久化（02 §5.6：索引本来可重建）。
                    tracing::error!(error = %e, "索引库打开失败，改用内存库");
                    Db::open_in_memory()?
                }
            }
        };
        let prefs = Preferences::load(&paths);
        // M2.1 数据步骤：把 prefs.roots 迁进 `roots` 表 + 推导文件夹层级（幂等）。
        // 放在 `reload_tree` 之前——树只认 DB，迁移没跑完树会是空的。
        if let Err(e) = crate::migrate::ensure_v2_data(&db, &prefs, &paths) {
            tracing::warn!(error = %e, "v2 数据步骤失败，本次以偏好里的根目录兜底");
        }
        let root_paths = crate::migrate::active_root_paths(&db);
        let mut lib = Self {
            paths,
            db: Arc::new(Mutex::new(db)),
            tier: prefs.tier,
            sort: prefs.sort,
            view: prefs
                .last_view
                .as_deref()
                .map(View::from_pref_string)
                .unwrap_or(View::All),
            prefs,
            photos: Vec::new(),
            selection: HashSet::new(),
            anchor: None,
            current: None,
            tray: Vec::new(),
            search: String::new(),
            roots: Vec::new(),
            visible_folders: Vec::new(),
            root_paths,
            total_in_db: 0,
            scanning: false,
            scan_progress: None,
            scan_summary: None,
            toast: None,
            last_error: None,
            dirty_prefs: false,
            path_to_id: HashMap::new(),
        };
        lib.tray = lib.prefs.tray.clone();
        lib.prune_tray();
        lib.reload_tree()?;
        lib.reload_photos()?;
        Ok(lib)
    }

    pub fn db(&self) -> Result<std::sync::MutexGuard<'_, Db>> {
        self.db
            .lock()
            .map_err(|_| CoreError::Other("数据库锁失效".into()))
    }

    // ------------------------------------------------------------------ 树

    /// 重建左栏树（从 DB 的 folders 表 + `roots` 表）。
    ///
    /// ⚠️ 根目录来自 **DB**，不是偏好文件（M2.1 起 `Preferences.roots` 只读兼容）。
    /// 表还没建好（老库迁移失败、降级运行）时退回偏好，避免树整个空掉。
    pub fn reload_tree(&mut self) -> Result<()> {
        self.refresh_roots();
        let folders = self.db()?.all_folders()?;
        self.total_in_db = folders.iter().map(|f| f.photo_count as u64).sum();
        self.path_to_id = folders.iter().map(|f| (f.path.clone(), f.id)).collect();

        // 1) 所有候选路径：roots 本身 + DB 里的目录
        let mut entries: Vec<(String, Option<i64>, u32)> = Vec::new();
        for (path, id) in &self.path_to_id {
            entries.push((path.clone(), Some(*id), 0));
        }
        for f in &folders {
            if let Some(e) = entries.iter_mut().find(|e| e.0 == f.path) {
                e.2 = f.photo_count;
            }
        }
        for root in &self.root_paths {
            if !entries.iter().any(|e| &e.0 == root) {
                entries.push((root.clone(), None, 0));
            }
        }

        // 2) 只保留 roots 之下的路径（roots 为空 → 树为空，走空态引导）
        entries.retain(|(p, _, _)| is_under_any_root(p, &self.root_paths));
        // 字典序保证「父目录一定在子目录之前」，配合栈就能建树
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let collapsed: HashSet<&str> = self.prefs.collapsed.iter().map(|s| s.as_str()).collect();
        let mut roots: Vec<FolderNode> = Vec::new();
        let mut stack: Vec<FolderNode> = Vec::new();

        for (path, id, own_count) in entries {
            let node = FolderNode {
                name: display_name(&path),
                path: path.clone(),
                folder_id: id,
                own_count,
                count: own_count,
                children: Vec::new(),
                collapsed: collapsed.contains(path.as_str()),
                depth: 0, // 建完树再统一赋深度
            };

            // 弹栈直到栈顶是当前路径的**严格祖先**
            while let Some(top) = stack.last() {
                if is_descendant(&path, &top.path) {
                    break;
                }
                let done = stack.pop().expect("stack 非空");
                attach(&mut roots, &mut stack, done);
            }
            stack.push(node);
        }
        // 收尾：把栈里剩下的自底向上挂回去
        while let Some(done) = stack.pop() {
            attach(&mut roots, &mut stack, done);
        }

        // 深度与合计计数
        for r in &mut roots {
            assign_depth_and_sum(r, 0);
        }
        self.roots = roots;
        self.rebuild_visible_folders();
        Ok(())
    }

    pub fn rebuild_visible_folders(&mut self) {
        let mut out = Vec::new();
        for r in &self.roots {
            r.flatten(&mut out);
        }
        self.visible_folders = out;
    }

    /// 折叠/展开一个目录节点（U-17：状态重启后保持）。
    pub fn toggle_folder(&mut self, path: &str) {
        if let Some(node) = find_node_mut(&mut self.roots, path) {
            node.collapsed = !node.collapsed;
            let collapsed = node.collapsed;
            if collapsed {
                if !self.prefs.collapsed.iter().any(|p| p == path) {
                    self.prefs.collapsed.push(path.to_string());
                }
            } else {
                self.prefs.collapsed.retain(|p| p != path);
            }
            self.dirty_prefs = true;
        }
        self.rebuild_visible_folders();
    }

    /// 视图内包含的 folder id（含子目录）。搜索与查询都用它划范围。
    pub fn view_folder_ids(&self) -> Option<Vec<i64>> {
        match self.view {
            View::All | View::Recent => None,
            View::Folder(id) => {
                let Some(node) = self.node_by_id(id) else {
                    return Some(vec![id]);
                };
                let mut ids = Vec::new();
                collect_ids(node, &mut ids);
                Some(ids)
            }
        }
    }

    fn node_by_id(&self, id: i64) -> Option<&FolderNode> {
        fn walk(nodes: &[FolderNode], id: i64) -> Option<&FolderNode> {
            for n in nodes {
                if n.folder_id == Some(id) {
                    return Some(n);
                }
                if let Some(found) = walk(&n.children, id) {
                    return Some(found);
                }
            }
            None
        }
        walk(&self.roots, id)
    }

    pub fn node_by_path(&self, path: &str) -> Option<&FolderNode> {
        fn walk<'a>(nodes: &'a [FolderNode], path: &str) -> Option<&'a FolderNode> {
            for n in nodes {
                if n.path == path {
                    return Some(n);
                }
                if let Some(found) = walk(&n.children, path) {
                    return Some(found);
                }
            }
            None
        }
        walk(&self.roots, path)
    }

    /// 文件夹 id → 规范化路径。状态栏双数计数与栅格占位判断都要用它把
    /// 「视图口径的 folder id」翻译成「任务口径的路径」。
    pub fn folder_path(&self, id: i64) -> Option<String> {
        self.node_by_id(id).map(|n| n.path.clone())
    }

    /// 当前视图里是否有正在索引的关注文件夹（栅格据此决定占位格要不要请求解码）。
    pub fn is_folder_in_view(&self, folder_id: i64) -> bool {
        match self.view_folder_ids() {
            None => true,
            Some(ids) => ids.contains(&folder_id),
        }
    }

    // -------------------------------------------------------------- 照片列表

    /// 重新计算当前视图的照片列表（视图 + 搜索 + 排序）。
    pub fn reload_photos(&mut self) -> Result<()> {
        let folder_ids = self.view_folder_ids();
        let photos = if self.search.trim().is_empty() {
            match self.view {
                View::Recent => self.db()?.recently_changed(200_000)?,
                _ => self
                    .db()?
                    .photos(folder_ids.as_deref(), self.sort, 200_000)?,
            }
        } else {
            let needle = paths::compare_key(self.search.trim());
            let range = parse_date_range(self.search.trim());
            self.db()?
                .search(&needle, range, folder_ids.as_deref(), 200_000)?
        };
        self.photos = photos;
        // 选择集与照片列表求交，避免「删了照片但 id 还在选择集里」
        let alive: HashSet<i64> = self.photos.iter().map(|p| p.id).collect();
        self.selection.retain(|id| alive.contains(id));
        if let Some(cur) = self.current
            && cur >= self.photos.len()
        {
            self.current = self.photos.len().checked_sub(1);
        }
        Ok(())
    }

    pub fn set_view(&mut self, view: View) -> Result<()> {
        if self.view == view {
            return Ok(());
        }
        self.view = view;
        self.prefs.last_view = Some(view.as_pref_string());
        self.prefs.grid_scroll = 0.0;
        self.selection.clear();
        self.anchor = None;
        self.dirty_prefs = true;
        self.reload_photos()
    }

    pub fn set_search(&mut self, s: String) -> Result<()> {
        if self.search == s {
            return Ok(());
        }
        self.search = s;
        self.reload_photos()
    }

    pub fn set_sort(&mut self, key: SortKey) -> Result<()> {
        if self.sort.key == key {
            self.sort.descending = !self.sort.descending;
        } else {
            self.sort = SortSpec {
                key,
                descending: matches!(key, SortKey::TakenAt | SortKey::ModifiedAt | SortKey::Size),
            };
        }
        self.prefs.sort = self.sort;
        self.dirty_prefs = true;
        self.reload_photos()
    }

    pub fn set_tier(&mut self, tier: Tier) {
        self.tier = tier;
        self.prefs.tier = tier;
        self.prefs.grid_scroll = 0.0;
        self.dirty_prefs = true;
        if let View::Folder(id) = self.view
            && let Ok(db) = self.db()
        {
            let _ = db.set_folder_thumb_tier(id, tier.index() as i64);
        }
    }

    pub fn cycle_tier_bigger(&mut self) {
        self.set_tier(self.tier.next_bigger());
    }

    pub fn cycle_tier_smaller(&mut self) {
        self.set_tier(self.tier.next_smaller());
    }

    // ---------------------------------------------------------------- 选择集

    pub fn selected_count(&self) -> usize {
        self.selection.len()
    }

    pub fn is_selected(&self, id: i64) -> bool {
        self.selection.contains(&id)
    }

    /// 单击选择。`modifiers` 由 UI 从 egui 输入里取。
    pub fn click_select(&mut self, index: usize, additive: bool, range: bool) {
        if index >= self.photos.len() {
            return;
        }
        if range {
            let from = self.anchor.unwrap_or(index);
            let (a, b) = if from <= index {
                (from, index)
            } else {
                (index, from)
            };
            if !additive {
                self.selection.clear();
            }
            for i in a..=b {
                if let Some(p) = self.photos.get(i) {
                    self.selection.insert(p.id);
                }
            }
        } else if additive {
            let id = self.photos[index].id;
            if !self.selection.remove(&id) {
                self.selection.insert(id);
            }
            self.anchor = Some(index);
        } else {
            self.selection.clear();
            self.selection.insert(self.photos[index].id);
            self.anchor = Some(index);
        }
    }

    pub fn select_all(&mut self) {
        self.selection = self.photos.iter().map(|p| p.id).collect();
    }

    pub fn select_none(&mut self) {
        self.selection.clear();
        self.anchor = None;
    }

    pub fn select_invert(&mut self) {
        let all: HashSet<i64> = self.photos.iter().map(|p| p.id).collect();
        self.selection = all.difference(&self.selection).copied().collect();
    }

    /// Home / End（F1-18）。
    pub fn select_edge(&mut self, end: bool) {
        if self.photos.is_empty() {
            return;
        }
        let idx = if end { self.photos.len() - 1 } else { 0 };
        self.selection.clear();
        self.selection.insert(self.photos[idx].id);
        self.anchor = Some(idx);
    }

    // ------------------------------------------------------------------ 托盘

    pub fn to_tray(&mut self) -> usize {
        let ids: Vec<i64> = self
            .photos
            .iter()
            .filter(|p| self.selection.contains(&p.id))
            .map(|p| p.id)
            .collect();
        let mut added = 0;
        for id in ids {
            if !self.tray.contains(&id) {
                self.tray.push(id);
                added += 1;
            }
        }
        if added > 0 {
            self.prefs.tray = self.tray.clone();
            self.dirty_prefs = true;
        }
        added
    }

    pub fn tray_remove(&mut self, id: i64) {
        self.tray.retain(|x| *x != id);
        self.prefs.tray = self.tray.clone();
        self.dirty_prefs = true;
    }

    pub fn tray_clear(&mut self) {
        self.tray.clear();
        self.prefs.tray.clear();
        self.dirty_prefs = true;
    }

    /// 托盘里的照片记录（用于画缩略图条）。O(n) 单条查询，n 通常 < 50。
    pub fn tray_photos(&self) -> Vec<PhotoRow> {
        let Ok(db) = self.db() else {
            return Vec::new();
        };
        self.tray
            .iter()
            .filter_map(|id| db.photo(*id).ok().flatten())
            .collect()
    }

    fn prune_tray(&mut self) {
        // 先算出「哪些 id 还在索引里」，再动 self.tray——
        // 否则借用检查会因为「db() 借了 &self、retain 又要 &mut self」直接报错。
        let alive: Vec<i64> = {
            let Ok(db) = self.db() else { return };
            self.tray
                .iter()
                .copied()
                .filter(|id| matches!(db.photo(*id), Ok(Some(_))))
                .collect()
        };
        let before = self.tray.len();
        self.tray = alive;
        if self.tray.len() != before {
            self.prefs.tray = self.tray.clone();
            self.dirty_prefs = true;
        }
    }

    // ------------------------------------------------------------------ 根目录

    /// 从 DB 刷新「当前关注根目录」缓存。**唯一的写入点**。
    fn refresh_roots(&mut self) {
        let pairs: Vec<(i64, String)> = match self.db() {
            Ok(db) => match db.roots(false) {
                Ok(rows) => rows.into_iter().map(|r| (r.id, r.path)).collect(),
                Err(e) => {
                    tracing::warn!(error = %e, "读 roots 失败");
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        self.root_paths = if pairs.is_empty() && !self.prefs.roots.is_empty() {
            // 降级兜底：`roots` 表不可用（迁移失败 / 老库）时退回偏好。
            // 这段代码存在的唯一理由就是「别让用户看到一个空图库」。
            self.prefs.roots.clone()
        } else {
            pairs.into_iter().map(|(_, p)| p).collect()
        };
    }

    /// 当前关注根目录（绝对路径）。UI 的「添加文件夹」从中挑起点。
    pub fn root_paths(&self) -> Vec<PathBuf> {
        self.root_paths.iter().map(PathBuf::from).collect()
    }

    pub fn add_root(&mut self, root: &Path) {
        // 走 DB：`roots` 表是唯一真相源；偏好里的 roots 不再写入。
        let canonical = paths::canonical_string(root);
        let ci = crate::migrate::probe_case_insensitive(root);
        let now = chrono::Utc::now().timestamp();
        if let Ok(db) = self.db() {
            match db.upsert_root(&canonical, now, ci) {
                Ok(_) => {
                    tracing::info!(root = %canonical, case_insensitive = ci, "关注根目录已入库")
                }
                Err(e) => tracing::warn!(error = %e, "写入 roots 失败"),
            }
        }
        if !self.root_paths.contains(&canonical) {
            self.root_paths.push(canonical);
        }
        self.refresh_roots();
    }

    pub fn remove_root(&mut self, root: &Path) {
        let s = paths::canonical_string(root);
        // 关闭根目录：**只改状态，不删数据**（P-07 / `docs/05` §1.3.2）。
        if let Ok(db) = self.db()
            && let Ok(Some(row)) = db.root_by_path(&s)
        {
            let _ = db.set_root_closed(row.id, true);
        }
        // 「从图库移除」的既有语义（F2-08）：索引也一起清掉。
        // ⚠️ 只删自己的索引，不动磁盘文件；「关闭但保留索引」是 E 导航的独立动作。
        if let Ok(db) = self.db()
            && let Ok(Some(id)) = db.folder_id(&s)
        {
            let _ = db.delete_folder(id);
        }
        self.root_paths.retain(|p| p != &s);
        self.refresh_roots();
    }

    // ------------------------------------------------------------------ 动作

    /// 删除到废纸篓（F2-08）。**只对当前选择生效，需 UI 先确认。**
    ///
    /// 返回 (成功数, 失败原因列表)。
    pub fn trash_selected(&mut self) -> (usize, Vec<String>) {
        let targets: Vec<PhotoRow> = self
            .photos
            .iter()
            .filter(|p| self.selection.contains(&p.id))
            .cloned()
            .collect();
        let mut ok = 0;
        let mut errors = Vec::new();
        for row in targets {
            match crate::trash::move_to_trash(&row.abs_path()) {
                Ok(_) => {
                    if let Ok(db) = self.db() {
                        let _ = db.forget_photo(row.id);
                    }
                    ok += 1;
                }
                Err(e) => errors.push(e.to_string()),
            }
        }
        if ok > 0 {
            self.selection.clear();
            let _ = self.reload_tree();
            let _ = self.reload_photos();
            self.toast = Some((format!("已移到废纸篓 {ok} 张"), 3.0));
        }
        (ok, errors)
    }

    /// 应用一条后台事件。返回 `true` 表示需要刷新照片列表。
    pub fn apply_event(&mut self, ev: CoreEvent) -> bool {
        match ev {
            CoreEvent::ScanStarted { roots } => {
                // 进度文案一律由进度弹框/面板负责（`04` §0.1：本轮只做"如实呈现"）。
                // 这里不再冒一个底部 toast，否则会出现"两个地方在说同一件事、数字还不一样"。
                tracing::debug!(roots, "索引入队");
                self.scanning = true;
                self.scan_progress = Some((0, 0));
                false
            }
            CoreEvent::FolderScanned {
                path,
                added,
                removed,
                ..
            } => {
                self.scanning = true;
                tracing::debug!(path = %path, added, removed, "目录已落库");
                let _ = self.reload_tree();
                true
            }
            CoreEvent::Progress { dirs, photos, .. } => {
                self.scan_progress = Some((photos, dirs));
                false
            }
            CoreEvent::ScanFinished { dirs, photos, secs } => {
                self.scanning = false;
                self.scan_progress = None;
                self.scan_summary = Some(format!(
                    "索引完成：{photos} 张 / {dirs} 个目录 / {secs:.1}s"
                ));
                let _ = self.reload_tree();
                true
            }
            CoreEvent::FsChanged { paths } => {
                // 外部变化：重扫变化目录（只读，不写）
                let _ = paths;
                let _ = self.reload_tree();
                true
            }
            CoreEvent::FolderOnline { folder_id, online } => {
                if let Ok(db) = self.db() {
                    let _ = db.set_folder_online(folder_id, online);
                }
                false
            }
            CoreEvent::Error(e) => {
                self.last_error = Some(e.clone());
                self.toast = Some((e, 6.0));
                self.scanning = false;
                false
            }
            // 图片事件由 UI 直接消费（纹理上传），不需要穿过 Library
            CoreEvent::ThumbReady { .. }
            | CoreEvent::ThumbFailed { .. }
            | CoreEvent::FullImageReady { .. } => false,
        }
    }

    /// 落盘偏好（退出前 + 定时）。**只在应用容器内写**。
    pub fn save_prefs(&mut self) {
        self.prefs.tier = self.tier;
        self.prefs.sort = self.sort;
        self.prefs.tray = self.tray.clone();
        if let Err(e) = self.prefs.save(&self.paths) {
            tracing::warn!(error = %e, "偏好保存失败");
        } else {
            self.dirty_prefs = false;
        }
    }

    pub fn prefs_dirty(&self) -> bool {
        self.dirty_prefs
    }

    pub fn mark_prefs_dirty(&mut self) {
        self.dirty_prefs = true;
    }

    // ------------------------------------------------------------- 状态栏文案

    /// 左侧：当前视图名 + 张数（U-34 待核对排布）。
    pub fn view_title(&self) -> String {
        match self.view {
            View::All => "所有照片".to_string(),
            View::Recent => "最近更改".to_string(),
            View::Folder(id) => self
                .node_by_id(id)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "未命名文件夹".to_string()),
        }
    }

    pub fn photo_at(&self, index: usize) -> Option<&PhotoRow> {
        self.photos.get(index)
    }

    pub fn index_of(&self, id: i64) -> Option<usize> {
        self.photos.iter().position(|p| p.id == id)
    }
}

// ---------------------------------------------------------------- 树构建辅助

fn display_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

fn is_descendant(child: &str, parent: &str) -> bool {
    if child == parent {
        return false;
    }
    let p = if parent.ends_with('/') {
        parent.to_string()
    } else {
        format!("{parent}/")
    };
    child.starts_with(&p)
}

fn is_under_any_root(path: &str, roots: &[String]) -> bool {
    roots.iter().any(|r| path == r || is_descendant(path, r))
}

/// 把已完成的节点挂到「栈里上一层节点的 children」或 roots 上。
fn attach(roots: &mut Vec<FolderNode>, stack: &mut [FolderNode], done: FolderNode) {
    match stack.last_mut() {
        Some(parent) if is_descendant(&done.path, &parent.path) => {
            parent.children.push(done);
        }
        _ => roots.push(done),
    }
}

fn assign_depth_and_sum(node: &mut FolderNode, depth: usize) {
    node.depth = depth;
    let mut sum = node.own_count;
    for c in &mut node.children {
        assign_depth_and_sum(c, depth + 1);
        sum += c.count;
    }
    node.count = sum;
    node.children.sort_by(|a, b| a.name.cmp(&b.name));
}

fn find_node_mut<'a>(nodes: &'a mut [FolderNode], path: &str) -> Option<&'a mut FolderNode> {
    for n in nodes.iter_mut() {
        if n.path == path {
            return Some(n);
        }
        if let Some(found) = find_node_mut(&mut n.children, path) {
            return Some(found);
        }
    }
    None
}

fn collect_ids(node: &FolderNode, out: &mut Vec<i64>) {
    if let Some(id) = node.folder_id {
        out.push(id);
    }
    for c in &node.children {
        collect_ids(c, out);
    }
}

/// 搜索框里输入 `2011` / `2011-05` / `2011-05-13` 时，转成一个日期区间匹配拍摄时间。
///
/// 这是 F1-21「EXIF 拍摄日期」这一路数据源；纯文件名搜索不受影响（OR 关系）。
pub fn parse_date_range(s: &str) -> Option<(i64, i64)> {
    use chrono::{Local, NaiveDate, TimeZone};
    let digits: Vec<u32> = s
        .split(['-', '/', '.'])
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse::<u32>().ok())
        .collect();
    if digits.is_empty() || digits[0] < 1000 {
        return None;
    }
    let year = digits[0] as i32;
    let (from, to) = match digits.len() {
        1 => (
            NaiveDate::from_ymd_opt(year, 1, 1)?,
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?,
        ),
        2 => {
            let m = digits[1];
            if !(1..=12).contains(&m) {
                return None;
            }
            let start = NaiveDate::from_ymd_opt(year, m, 1)?;
            let end = if m == 12 {
                NaiveDate::from_ymd_opt(year + 1, 1, 1)?
            } else {
                NaiveDate::from_ymd_opt(year, m + 1, 1)?
            };
            (start, end)
        }
        _ => {
            let (m, d) = (digits[1], digits[2]);
            let start = NaiveDate::from_ymd_opt(year, m, d)?;
            let end = start.succ_opt()?;
            (start, end)
        }
    };
    let a = Local
        .from_local_datetime(&from.and_hms_opt(0, 0, 0)?)
        .single()?
        .timestamp();
    let b = Local
        .from_local_datetime(&to.and_hms_opt(0, 0, 0)?)
        .single()?
        .timestamp();
    Some((a, b))
}

/// 把当前视图的照片按「天」分组（U-30 日期分组头）。返回 (分组标题, 起止索引)。
pub fn group_by_day(photos: &[PhotoRow]) -> Vec<(String, usize, usize)> {
    use chrono::{Local, TimeZone};
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    let mut i = 0;
    while i < photos.len() {
        let ts = photos[i].effective_taken_at();
        let day = Local
            .timestamp_opt(ts, 0)
            .single()
            .map(|dt| dt.format("%Y年%m月%d日").to_string())
            .unwrap_or_else(|| "未知日期".to_string());
        let mut j = i + 1;
        while j < photos.len() && same_day(photos[j].effective_taken_at(), ts) {
            j += 1;
        }
        out.push((day, i, j));
        i = j;
    }
    out
}

fn same_day(a: i64, b: i64) -> bool {
    use chrono::{Local, TimeZone};
    match (
        Local.timestamp_opt(a, 0).single(),
        Local.timestamp_opt(b, 0).single(),
    ) {
        (Some(x), Some(y)) => x.format("%Y-%m-%d").to_string() == y.format("%Y-%m-%d").to_string(),
        _ => false,
    }
}

impl Library {
    /// 当前视图的根目录列表（用于「添加文件夹」后启动扫描）。
    pub fn scan_roots(&self) -> Vec<PathBuf> {
        self.root_paths()
    }
}
