//! 库会话：视图、选择集、托盘、搜索、排序——**UI 的唯一数据源**。
//!
//! 这一层刻意保持「同步 + 独占」：UI 线程调用它，它同步查 DB（都是索引查询，10 万张量级下
//! 单次几十毫秒），没有后台线程参与。**简单性优先**——M1 的性能瓶颈在渲染与解码，不在查询。

use crate::{
    CoreError, Result, Tier, ViewMode,
    paths::{self, AppPaths},
    prefs::Preferences,
    scanner::CoreEvent,
};
use picasa_db::{Db, ExclusionRow, PhotoFilter, PhotoRow, SortKey, SortSpec};
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

/// 首屏页大小：用户点开文件夹后，「立刻能看见并能滚动」的行数。
///
/// 取 1500 的依据：中档（144px）1280 宽的窗口一屏约 6×8=48 格，
/// 1500 行 ≈ 31 屏，足够把「滚一下看看」的动作全部覆盖；
/// 再大就是在替用户预读他大概率看不到的行，首屏时间白白变长。
pub const PAGE_FIRST: usize = 1500;

/// 每次补齐追加的行数。
///
/// 4000 行的追加在 5 万行的库上实测 ~5 ms（走 `idx_photos_eff_taken` 顺序跳过），
/// 放在空闲帧里完全无感；太小会让「滚到底」的等待变成几十次小抖动，
/// 太大则单帧超过 8ms 的余量。
pub const PAGE_STEP: usize = 4000;

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
    /// 搜索范围：`true` = 只搜当前视图（文件夹/聚合入口），`false` = 全库。
    /// 默认「当前视图」——用户在某个文件夹里搜索，预期就是在这个文件夹里找。
    pub search_in_current: bool,
    /// 照片筛选（日期/格式/尺寸/收藏/标签）。`None` = 无筛选。
    pub filter: Option<PhotoFilter>,
    pub tier: Tier,
    /// 栅格视图模式（默认方格 / 纵向瀑布流 / 横向瀑布流）。
    pub view_mode: ViewMode,
    pub sort: SortSpec,
    /// 栅格排序时把「已经有缩略图」的照片排在前面（用户要求）。
    ///
    /// 只是**显示策略**，所以不进 `SortSpec`（那个是「按什么键排」），
    /// 单独一个开关存在偏好里，重启保留。分桶用两条查询实现，
    /// 见 `Db::photos_bucketed`。
    pub thumbs_first: bool,
    /// 「包含子文件夹」开关（`07-A` §3，P-28 裁决全局一份）。
    ///
    /// 关（默认）= 原版行为，只显示 F 的直接照片；
    /// 开 = F 及**全部后代**目录，口径与 `07` §4.1 的 `total` 一致。
    ///
    /// ⚠️ 这是**全局**设置而不是按文件夹记（P-28）：按文件夹记要加一张表，
    /// 而且「同一个开关在不同文件夹下是不同状态」会让用户彻底失去方向感。
    pub recurse: bool,

    /// 树（未展开/折叠的完整结构）
    pub roots: Vec<FolderNode>,
    /// 扁平化后的可见行（每帧重建代价极低，直接算）
    pub visible_folders: Vec<FolderRowView>,
    /// 当前 active 的根目录（**来自 DB `roots` 表**，不再读偏好）。
    ///
    /// 保留成缓存是为了 `reload_tree` / `scan_roots` 不必每次查库；
    /// 写入点只有 `refresh_roots()` 一个，避免两处各改一半。
    root_paths: Vec<String>,
    /// 本机根之下的可见行（IDX-21 ⑦：左栏「文件夹」组用这份）。
    pub local_visible_folders: Vec<FolderRowView>,
    /// 导入来源 id → 该来源根之下的可见行（左栏「导入的索引」组用）。
    pub imported_visible_folders: std::collections::BTreeMap<i64, Vec<FolderRowView>>,
    /// 根路径 → (roots.id, origin, import_source_id)。
    ///
    /// IDX-21 ⑦ 用：本机「文件夹」组只显示 `origin = local` 的根，
    /// 导入带进来的根归到「导入的索引」分组下，同一棵树不会显示两次。
    root_origins: std::collections::HashMap<String, (i64, String, Option<i64>)>,

    // ---- 状态文案（状态栏/工具栏用）----
    pub total_in_db: u64,
    pub scanning: bool,
    pub scan_progress: Option<(u64, u64)>,
    pub scan_summary: Option<String>,
    pub toast: Option<(String, f64)>,
    pub last_error: Option<String>,

    /// 当前视图**总共有多少张**（C1：首屏先拿总数，不为了知道总数把 5 万行读进来）。
    ///
    /// `photos.len()` 在补齐完成前**小于**它；界面要显示「共 N 张」时必须用这个字段。
    /// 有筛选时它是**筛过之后**的张数，与 `photos` 同一口径——用可见全库的总数
    /// 会让分页补齐把没筛中的行再拼回来。
    pub photos_total: usize,
    /// 分页补齐是否已到底（`photos.len() >= photos_total` 时为 true）。
    pub photos_complete: bool,

    dirty_prefs: bool,
    path_to_id: HashMap<String, i64>,
    /// 左栏树需要重建（扫描落库是**每个目录一条事件**，不能照着事件数重建树）
    tree_dirty: bool,
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
            view_mode: prefs.view_mode,
            sort: prefs.sort,
            thumbs_first: prefs.thumbs_first,
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
            search_in_current: true,
            filter: None,
            recurse: false, // 下面从 `settings.recurse_subfolders` 覆盖（P-28：全局一份）
            roots: Vec::new(),
            visible_folders: Vec::new(),
            root_paths,
            local_visible_folders: Vec::new(),
            imported_visible_folders: std::collections::BTreeMap::new(),
            root_origins: std::collections::HashMap::new(),
            total_in_db: 0,
            scanning: false,
            scan_progress: None,
            scan_summary: None,
            toast: None,
            last_error: None,
            photos_total: 0,
            photos_complete: true,
            dirty_prefs: false,
            path_to_id: HashMap::new(),
            tree_dirty: false,
        };
        lib.tray = lib.prefs.tray.clone();
        lib.prune_tray();
        // `settings.recurse_subfolders`（`07-A` §6）。读失败就用默认关——
        // 这个开关读不到只影响「一次多显示几张」，不该拦住启动。
        lib.recurse = lib.db().map(|db| db.recurse_subfolders()).unwrap_or(Ok(false)).unwrap_or(false);
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
        // 任何显式重建都算把脏标记清掉了（扫描期的节流见 `take_tree_dirty`）
        self.tree_dirty = false;
        self.refresh_roots();
        let folders = self.db()?.all_folders()?;
        // 「所有照片」旁的数字只数**活动关注文件夹**下的照片。
        // `all_folders()` 是物理全表（含已关闭根目录的行——关闭不删索引，
        // 那些行必须留着，`m21_nav` 靠它们断言）；直接求和就会让左栏显示
        // 「所有照片 13」，而栅格按可见口径只画出 3 张。
        self.total_in_db = folders
            .iter()
            .filter(|f| is_under_any_root(&f.path, &self.root_paths))
            .map(|f| f.photo_count as u64)
            .sum();
        self.path_to_id = folders.iter().map(|f| (f.path.clone(), f.id)).collect();

        // 1) 所有候选路径：roots 本身 + DB 里的目录
        //
        // ⚠️ 这里曾经是 O(N²)：先按 `path_to_id` 铺 N 条，再对每个 folder 用
        // `entries.iter_mut().find(...)` 线性回填计数。`tree_bench` 实测：2000 个
        // 相册目录单次 42.8 ms、8000 个目录 462 ms；而 `apply_event` 的
        // `FolderScanned` 分支**每扫完一个目录就调一次这个函数**——8000 个目录
        // 累计 3744 秒卡在 UI 线程上，界面完全动不了。
        // `folders` 本身就带着 id 和 photo_count，直接映射即可，不必再查一遍。
        let mut entries: Vec<(String, Option<i64>, u32)> = folders
            .iter()
            .map(|f| (f.path.clone(), Some(f.id), f.photo_count))
            .collect();
        // roots 数量是个位数，这里保持线性判断（不值得为它引入集合）
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

    /// 取走「树需要重建」的脏标记。
    ///
    /// 调用方（UI）负责节流：扫描落库是每个目录一条 `FolderScanned`，而重建树是
    /// O(N log N) 的重活。`tree_bench` 实测 8000 个相册目录时单次 46.8 ms，
    /// 照事件数各来一次就是 405 秒——**这不是慢，是界面死掉**。
    pub fn take_tree_dirty(&mut self) -> bool {
        std::mem::take(&mut self.tree_dirty)
    }

    pub fn rebuild_visible_folders(&mut self) {
        let mut out = Vec::new();
        for r in &self.roots {
            r.flatten(&mut out);
        }
        self.visible_folders = out;
        // IDX-21 ⑦：本机「文件夹」组与「导入的索引」组各取一份。
        // 同一棵树只在一处出现，计数不重复统计。
        self.local_visible_folders = self
            .local_roots()
            .into_iter()
            .flat_map(|r| {
                let mut v = Vec::new();
                r.flatten(&mut v);
                v
            })
            .collect();
        let mut by_source: std::collections::BTreeMap<i64, Vec<FolderRowView>> =
            std::collections::BTreeMap::new();
        for r in &self.roots {
            if let Some(src) = self.import_source_of_root(&r.path) {
                let mut v = Vec::new();
                r.flatten(&mut v);
                by_source.entry(src).or_default().extend(v);
            }
        }
        self.imported_visible_folders = by_source;
    }

    /// IDX-21：切换一个**非目录**分组的折叠状态，并标记偏好待存。
    ///
    /// 复用 `prefs.collapsed` 列表存一条哨兵键（`\u{0}` 前缀，真实路径不可能
    /// 以它开头），这样不用为「导入的索引」扩一套偏好字段，重启照样记得。
    /// 返回切换后的状态（true = 已折叠）。
    pub fn toggle_section_collapsed(&mut self, section: &str) -> bool {
        let key = format!("\u{0}{section}");
        let now_collapsed = if self.prefs.collapsed.iter().any(|p| p == &key) {
            self.prefs.collapsed.retain(|p| p != &key);
            false
        } else {
            self.prefs.collapsed.push(key);
            true
        };
        self.dirty_prefs = true;
        now_collapsed
    }

    /// IDX-21：分组当前是不是折叠的（启动时读回用）。
    pub fn section_collapsed(&self, section: &str) -> bool {
        let key = format!("\u{0}{section}");
        self.prefs.collapsed.iter().any(|p| p == &key)
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
        self.scope_folder_ids(self.recurse)
    }

    /// 按指定的「含不含子目录」算视图范围（`07-A` §3.1）。
    ///
    /// 抽出来带参数而不是直接读 `self.recurse`，是为了让「切开关时保持锚点」
    /// （RECURSE-02）能先算一眼新范围，而**不改动**当前状态。
    pub fn scope_folder_ids(&self, recurse: bool) -> Option<Vec<i64>> {
        match self.view {
            View::All | View::Recent => None,
            View::Folder(id) => {
                if !recurse {
                    // 关闭：只有它自己。**不走树**——树里可能拿不到这个节点
                    // （关闭的关注文件夹、尚未落库的新目录），那时返回 `vec![id]`
                    // 而不是空，否则栅格会空掉。
                    return Some(vec![id]);
                }
                let Some(node) = self.node_by_id(id) else {
                    return Some(vec![id]);
                };
                let mut ids = Vec::new();
                collect_ids(node, &mut ids);
                Some(ids)
            }
        }
    }

    /// 当前视图**在开关开/关两种状态下**各有多少张（RECURSE-02 的锚点判断用）。
    pub fn count_both_scopes(&self) -> Result<(usize, usize)> {
        let direct = self.db()?.count_photos_visible(self.scope_folder_ids(false).as_deref())?;
        let subtree = self.db()?.count_photos_visible(self.scope_folder_ids(true).as_deref())?;
        Ok((direct as usize, subtree as usize))
    }

    /// 切换「包含子文件夹」（`07-A` RECURSE-01/02）。
    ///
    /// 返回 `true` 表示范围真的变了（`View::All` / `Recent` 下这个开关没有意义）。
    ///
    /// ## 锚点处理（RECURSE-02）
    ///
    /// 切换时**保持当前选中文件夹**；顶部 `photo_id` 若仍在新视图内则对齐，
    /// 否则回顶。这里只把「要不要回顶」的决定权交出去（返回是否变了），
    /// 具体位置由栅格的 `anchor_id` 机制处理——它本来就有这个能力。
    /// 切换栅格视图模式。切到 masonry 需要宽高，宽高缺失的格子按 1:1 占位，
    /// DimsService 回填后布局自动重排。
    pub fn set_view_mode(&mut self, mode: ViewMode) {
        if self.view_mode == mode {
            return;
        }
        self.view_mode = mode;
        self.prefs.view_mode = mode;
        self.dirty_prefs = true;
    }

    pub fn set_recurse(&mut self, on: bool) -> Result<bool> {
        if self.recurse == on {
            return Ok(false);
        }
        self.recurse = on;
        let meaningful = matches!(self.view, View::Folder(_));
        if meaningful {
            self.reload_photos()?;
        }
        Ok(meaningful)
    }

    /// 「所有照片」视图下开关置灰（`07-A` RECURSE-09：语义重叠）。
    pub fn recurse_available(&self) -> bool {
        matches!(self.view, View::Folder(_))
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
    ///
    /// ## C1：**只取首屏页**，其余留给 `extend_photos`
    ///
    /// M1/M2 是一次 `LIMIT 200_000` 把全库读进内存。5 万张实测 **278 ms**，
    /// 用户点一下文件夹就卡在那里（首屏时间全花在「读完我根本还没滚到的 4.8 万行」上）。
    /// 现在先 `COUNT` 拿到总数，再只读 `PAGE_FIRST` 行——
    /// 总数是 O(1)（无 WHERE 的 `COUNT(*)` 走页数统计，有 folder_id 时走索引），
    /// 首屏查询命中 `idx_photos_eff_taken` 后只要 **1.8 ms**。
    ///
    /// `photos` 的**语义没变**（仍是「当前视图的照片，按当前排序」），
    /// 只是分页补齐前不是完整的——所以新增了 `photos_total` 与 `photos_complete`。
    /// 任何想显示「共 N 张」的地方都要用 `photos_total`，不能用 `photos.len()`。
    pub fn reload_photos(&mut self) -> Result<()> {
        // 搜索范围切换（用户功能 #10）：「当前文件夹」用视图 scope，「全部图库」传 None。
        // 无搜索词时范围无意义，筛选仍按 scope 走（筛选的是「视图内」的照片）。
        let searching = !self.search.trim().is_empty();
        let folder_ids = if searching && !self.search_in_current {
            None
        } else {
            self.view_folder_ids()
        };
        let filter = self.filter.clone().unwrap_or_default();
        let has_filter = filter.is_active();

        let (photos, total) = if !searching && !has_filter {
            // ⚠️ 用 `count_photos_visible` / `photos_from_visible`，**不是**无后缀的那两个：
            // 后者是「物理全库」，会把已关闭关注文件夹的照片也算进来。
            // 用户关掉一个关注文件夹后，树里没了、栅格却还在——那正是这条 bug 的形态。
            let total = self.db()?.count_photos_visible(folder_ids.as_deref())? as usize;
            let photos = match self.view {
                // 「最近更改」按 `mtime_ns` 排序，有 `idx_photos_mtime`，全库 5 万也只要几毫秒；
                // 且它本身就是「最近 N 条」的语义，不需要分页补齐。
                View::Recent => self.db()?.recently_changed(200_000)?,
                // 分桶（「有缩略图的往前放」）：两桶各自排序，先整桶有图。
                // 实测 5.6 万张 0.01 s（两条独立查询），见 `Db::photos_bucketed`。
                _ if self.thumbs_first => self.db()?.photos_bucketed(
                    folder_ids.as_deref(),
                    None,
                    self.sort,
                    PAGE_FIRST.min(total.max(1)),
                    0,
                )?,
                _ => self.db()?.photos_from_visible(
                    folder_ids.as_deref(),
                    self.sort,
                    PAGE_FIRST.min(total.max(1)),
                    0,
                )?,
            };
            let total = if self.view == View::Recent {
                photos.len()
            } else {
                total
            };
            (photos, total)
        } else if has_filter {
            // 带筛选（可与搜索词叠加，也可单独用）。筛选查询自带排序与分页口径。
            //
            // 总数必须走 `count_photos_filtered`，不能复用上面的可见全库 COUNT。
            // 以前首屏是筛过的，但 `photos_total` 仍是 9 张全库，`photos_complete`
            // 为 false，下一帧 `extend_photos` 按无筛选 OFFSET 把 gif/png 和
            // 已经出现过的格子再拼进来——筛选芯片亮了，栅格却像没筛过。
            // 有筛选时同样分桶（筛选条件与桶条件在 `photos_bucketed` 里按「与」合并）。
            let rows = if self.thumbs_first {
                self.db()?.photos_bucketed(
                    folder_ids.as_deref(),
                    Some(&filter),
                    self.sort,
                    200_000,
                    0,
                )?
            } else {
                self.db()?.photos_filtered(
                    folder_ids.as_deref(),
                    &filter,
                    self.sort,
                    200_000,
                    0,
                )?
            };
            let photos = if !searching {
                rows
            } else {
                // 搜索词与筛选叠加：取并集口径太重（LIKE ∩ 筛选），直接在内存里过滤
                let needle = paths::compare_key(self.search.trim());
                rows.into_iter()
                    .filter(|p| {
                        p.name.to_lowercase().contains(&needle)
                            || p.folder_path.to_lowercase().contains(&needle)
                    })
                    .collect()
            };
            let total = if searching || self.view == View::Recent {
                photos.len()
            } else {
                self.db()?
                    .count_photos_filtered(folder_ids.as_deref(), &filter)? as usize
            };
            (photos, total)
        } else {
            let needle = paths::compare_key(self.search.trim());
            let range = parse_date_range(self.search.trim());
            // 搜索不分页：LIKE 的结果集通常远小于全库，且用户能立刻用关键字收窄。
            // 真到了几十万行搜不动的那天，另开一页搜索索引，不在这里糊。
            let photos = self
                .db()?
                .search(&needle, range, folder_ids.as_deref(), 200_000)?;
            let total = photos.len();
            (photos, total)
        };

        self.photos = photos;
        self.photos_total = total;
        self.photos_complete = self.photos.len() >= self.photos_total;
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

    /// 追加下一页（C1）。返回 `true` 表示这次真的读到了新行。
    ///
    /// 由 UI 在**空闲帧**调用（一帧最多一次）：分页是为了让首屏快，
    /// 不是为了把「读完 5 万行」这件事挪到下一帧原地卡住。
    pub fn extend_photos(&mut self) -> Result<bool> {
        if self.photos_complete || !self.search.trim().is_empty() || self.view == View::Recent {
            return Ok(false);
        }
        let folder_ids = self.view_folder_ids();
        let filter = self.filter.clone().unwrap_or_default();
        // 有筛选时必须沿同一条 `photos_filtered` 翻页。改走 `photos_from_visible`
        // 会从「已装入的筛后行数」这个 OFFSET 去捞无筛选的尾部：gif/png 回来了，
        // 而且已经在首屏里的 jpeg 会再出现一次。
        // 键集翻页（`docs/18` TC-M43-12）：`OFFSET` 的代价是 O(offset)，
        // 25 万张深页实测 103 ms；改成「从上一页末行的 (eff_taken_at, name_key)
        // 之后取」走索引 seek，同一页 2.3 ms。
        //
        // 游标取**已装入的最后一行的排序键**（`self.photos.last()`），不是
        // `photos.len()` 当偏移——这两者在有筛选、有去重时并不等价。
        //
        // 只有「按拍摄时间」能走键集：这条排序有 `idx_photos_eff_taken
        // (eff_taken_at DESC, name_key ASC)`。其余排序键（名称是 TEXT、尺寸是
        // 表达式、修改时间只有单列索引）硬走键集会让 SQLite 退化成全表 +
        // 临时 B 树排序，比 OFFSET 更慢——所以那些排序老实按偏移翻页。
        // 分桶模式下不能走键集：游标只有 (eff_taken_at, name_key)，不带桶号，
        // 翻页会跨桶重复/漏行。分桶退回 OFFSET（实测 5.6 万张 OFFSET 5 万仍 0.05 s）。
        let keyed = self.sort.key == picasa_db::SortKey::TakenAt && !self.thumbs_first;
        let cursor = match self.photos.last() {
            Some(last) if keyed => Some((last.eff_taken_at, paths::compare_key(&last.name))),
            _ => None,
        };
        let mut more = if keyed {
            if filter.is_active() {
                self.db()?.photos_filtered_after(
                    folder_ids.as_deref(),
                    &filter,
                    self.sort,
                    PAGE_STEP,
                    cursor,
                )?
            } else {
                self.db()?.photos_from_visible_after(
                    folder_ids.as_deref(),
                    self.sort,
                    PAGE_STEP,
                    cursor,
                )?
            }
        } else if self.thumbs_first {
            self.db()?.photos_bucketed(
                folder_ids.as_deref(),
                filter.is_active().then_some(&filter),
                self.sort,
                PAGE_STEP,
                self.photos.len(),
            )?
        } else if filter.is_active() {
            self.db()?.photos_filtered(
                folder_ids.as_deref(),
                &filter,
                self.sort,
                PAGE_STEP,
                self.photos.len(),
            )?
        } else {
            self.db()?.photos_from_visible(
                folder_ids.as_deref(),
                self.sort,
                PAGE_STEP,
                self.photos.len(),
            )?
        };
        if more.is_empty() {
            // 库里的行数比 COUNT 说的少（补齐期间被删了），按「到底」处理，
            // 否则会每帧都来问一次空结果。
            self.photos_complete = true;
            return Ok(false);
        }
        let seen: HashSet<i64> = self.photos.iter().map(|p| p.id).collect();
        more.retain(|p| !seen.contains(&p.id));
        if more.is_empty() {
            // 这一页全是栅格里已经有的 id。继续按 OFFSET 翻只会重复，停在这里。
            self.photos_complete = true;
            return Ok(false);
        }
        self.photos_total = self.photos_total.max(self.photos.len() + more.len());
        self.photos.extend(more);
        self.photos_complete = self.photos.len() >= self.photos_total;
        Ok(true)
    }

    /// 视图内还需要补齐多少张（界面显示「正在载入 N 张」用）。
    pub fn photos_remaining(&self) -> usize {
        self.photos_total.saturating_sub(self.photos.len())
    }

    pub fn set_view(&mut self, view: View) -> Result<()> {
        // 「星标」不是独立的 `View`，而是叠在当前视图上的 `starred_only`。
        // 用户已经停在「所有照片」上再点一次时，视图值没变，若在这里直接返回，
        // 侧栏高亮还在、筛选芯片还亮，栅格也回不到全部照片。
        let clear_star =
            view == View::All && self.filter.as_ref().is_some_and(|f| f.starred_only);
        if self.view == view && !clear_star {
            return Ok(());
        }
        if self.view != view {
            self.view = view;
            self.prefs.last_view = Some(view.as_pref_string());
            self.prefs.grid_scroll = 0.0;
            self.selection.clear();
            self.anchor = None;
            self.dirty_prefs = true;
        }
        if clear_star {
            self.clear_starred_only();
        }
        self.reload_photos()
    }

    /// 退出「只看收藏」。格式 / 日期 / 尺寸 / 标签这些维度留下。
    fn clear_starred_only(&mut self) {
        let Some(mut f) = self.filter.take() else {
            return;
        };
        f.starred_only = false;
        self.filter = f.is_active().then_some(f);
    }

    pub fn set_search(&mut self, s: String) -> Result<()> {
        if self.search == s {
            return Ok(());
        }
        self.search = s;
        self.reload_photos()
    }

    /// 切换搜索范围（当前文件夹 ↔ 全部图库）。
    pub fn set_search_in_current(&mut self, on: bool) -> Result<()> {
        if self.search_in_current == on {
            return Ok(());
        }
        self.search_in_current = on;
        // 只在有搜索词时影响结果；无词也 reload 无害（幂等）
        self.reload_photos()
    }

    /// 设置/清除筛选。`None` = 清除。
    pub fn set_filter(&mut self, f: Option<PhotoFilter>) -> Result<()> {
        if self.filter == f {
            return Ok(());
        }
        self.filter = f;
        self.reload_photos()
    }

    /// 批量设置收藏（多选工具栏调用）。
    pub fn batch_set_starred(&mut self, on: bool) -> Result<usize> {
        let ids: Vec<i64> = self.selection.iter().copied().collect();
        if ids.is_empty() {
            return Ok(0);
        }
        self.db()?.set_starred(&ids, on)?;
        // 立即反映到当前列表的行状态（不必整页重查）
        for p in self.photos.iter_mut() {
            if self.selection.contains(&p.id) {
                p.starred = on;
            }
        }
        Ok(ids.len())
    }

    /// 设置单张的收藏（右键菜单用）。会同步刷新当前列表里的行状态。
    pub fn batch_set_starred_ids(&mut self, ids: Vec<i64>, on: bool) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        self.db()?.set_starred(&ids, on)?;
        for p in self.photos.iter_mut() {
            if ids.contains(&p.id) {
                p.starred = on;
            }
        }
        Ok(ids.len())
    }

    /// 批量加标签。
    pub fn batch_add_tag(&mut self, label: &str) -> Result<usize> {
        let ids: Vec<i64> = self.selection.iter().copied().collect();
        if ids.is_empty() {
            return Ok(0);
        }
        self.db()?.add_tag(&ids, label)?;
        Ok(ids.len())
    }

    /// 给单张加标签（标签编辑窗口用）。
    pub fn batch_add_tag_id(&mut self, photo_id: i64, label: &str) -> Result<()> {
        self.db()?.add_tag(&[photo_id], label).map_err(CoreError::from)
    }

    /// 从单张摘标签。
    pub fn batch_remove_tag_id(&mut self, photo_id: i64, label: &str) -> Result<()> {
        self.db()?.remove_tag(&[photo_id], label).map_err(CoreError::from)
    }

    /// 批量摘标签。
    pub fn batch_remove_tag(&mut self, label: &str) -> Result<usize> {
        let ids: Vec<i64> = self.selection.iter().copied().collect();
        if ids.is_empty() {
            return Ok(0);
        }
        self.db()?.remove_tag(&ids, label)?;
        Ok(ids.len())
    }

    /// 设置单张说明（查看器 / 右键菜单调用）。
    pub fn set_caption(&mut self, photo_id: i64, text: &str) -> Result<()> {
        self.db()?.set_caption(photo_id, text).map_err(CoreError::from)
    }

    /// 读单张说明。
    pub fn caption_of(&self, photo_id: i64) -> Result<String> {
        self.db()?.caption_of(photo_id).map_err(CoreError::from)
    }

    /// 读单张的标签列表。
    pub fn tags_of(&self, photo_id: i64) -> Result<Vec<String>> {
        self.db()?.tags_of(photo_id).map_err(CoreError::from)
    }

    /// 全库标签清单（筛选面板）。**未排序、未过滤 0 计数**——左栏请走
    /// [`Library::sidebar_tags`]，那里按用户选择的排序与可见性整理过。
    pub fn all_tags(&self) -> Result<Vec<(String, i64)>> {
        self.db()?.all_tags().map_err(CoreError::from)
    }

    /// M4.3 `IDX-22` P1「本视图 N」：当前视图范围内的标签计数。
    ///
    /// 与 [`Library::all_tags`] 的唯一区别是**范围**：只数落在当前视图（视图
    /// 文件夹子树 + 「含子文件夹」开关）里、且 root 未关闭的照片——与栅格同一
    /// 口径。两个数字（全库 / 本视图）必须能对上网格，否则用户会看到三方不一致。
    pub fn view_tag_counts(&self) -> Result<Vec<(String, i64)>> {
        let scope = self.view_folder_ids();
        self.db()?
            .tag_counts_visible(scope.as_deref())
            .map_err(CoreError::from)
    }

    /// M4.3 `IDX-22` ④⑥：左栏「标签」组要画的清单——按偏好里的排序方式排好、
    /// 默认滤掉 0 计数。计数口径仍是 `Db::all_tags()` 的**全库口径**（含离线根
    /// 与导入的索引，按 `photo_id` 去重，天然只算一份）。
    ///
    /// 纯数据变换（[`crate::tag_counts::sort_tag_counts`]），不额外查库；
    /// 调用方应在**脏标记触发时**调用，而不是每帧。
    pub fn sidebar_tags(&self) -> Result<Vec<(String, i64)>> {
        let raw = self.all_tags()?;
        Ok(crate::tag_counts::sort_tag_counts(
            &raw,
            self.prefs.tags_sort,
            self.prefs.show_zero_tags,
        ))
    }

    /// 切换左栏标签的排序方式（IDX-22 ④，选择持久化）。
    pub fn set_tags_sort(&mut self, sort: crate::tag_counts::TagsSort) {
        if self.prefs.tags_sort == sort {
            return;
        }
        self.prefs.tags_sort = sort;
        self.dirty_prefs = true;
    }

    /// 切换「显示空标签」（IDX-22 ⑥，默认隐藏）。
    pub fn toggle_show_zero_tags(&mut self) -> bool {
        self.prefs.show_zero_tags = !self.prefs.show_zero_tags;
        self.dirty_prefs = true;
        self.prefs.show_zero_tags
    }

    pub fn show_zero_tags(&self) -> bool {
        self.prefs.show_zero_tags
    }

    pub fn tags_sort(&self) -> crate::tag_counts::TagsSort {
        self.prefs.tags_sort
    }

    /// M4.1 `M41-UI-08`：一次查库拿到「带某标签的全部照片 id」。
    ///
    /// 批量标签浮层用它算「选中集里有多少张带此标签」，避免对每个标签 ×
    /// 每个选中 id 各查一次 `tags_of`（选中 3 万张时会退化成几十万次查询）。
    pub fn ids_with_tag(&self, label: &str) -> Result<Vec<i64>> {
        self.db()?.ids_with_tag(label).map_err(CoreError::from)
    }

    /// M4.2 `M42-UI-06`：一次查库拿到「选中集里每张照片各自的标签」。
    /// 查库次数与标签数量无关（选中集为空时 0 次），供批量标签浮层算三态。
    pub fn tags_for_ids(&self, photo_ids: &[i64]) -> Result<HashMap<i64, Vec<String>>> {
        self.db()?.tags_for_ids(photo_ids).map_err(CoreError::from)
    }

    /// M4.2 `M42-UI-06`：由「每张照片的标签」+「全库标签清单」算三态计数。
    ///
    /// 纯函数（不碰 DB、不持锁），返回值是 `标签名 -> 选中集里带此标签的照片数`，
    /// 浮层据此显示 ☑(with==n) / ▣(with>0) / ☐(with==0)。抽成纯函数是为了能
    /// 无 DB 单测（TC-M42-B-06-02），也让 UI 侧只在缓存失效时算一次。
    pub fn tri_state_counts(
        photo_ids: &[i64],
        tags_by_photo: &HashMap<i64, Vec<String>>,
        all_labels: &[(String, i64)],
    ) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (label, _) in all_labels {
            let with = photo_ids
                .iter()
                .filter(|id| {
                    tags_by_photo
                        .get(*id)
                        .is_some_and(|tags| tags.iter().any(|t| t == label))
                })
                .count();
            counts.insert(label.clone(), with);
        }
        counts
    }

    /// 时间轴：按拍摄年份的计数（左栏时间轴分组用）。
    pub fn timeline_years(&self) -> Result<Vec<(i32, i64)>> {
        self.db()?.timeline_years().map_err(CoreError::from)
    }

    // ---------------------------------------------------------- M4：应用内相册
    // 全部只写自有数据库，不复制/移动/修改原图（docs/11 §5.2）。

    pub fn create_album(&self, name: &str, now: i64) -> Result<i64> {
        self.db()?.create_album(name, now).map_err(CoreError::from)
    }
    pub fn rename_album(&self, id: i64, new_name: &str) -> Result<()> {
        self.db()?.rename_album(id, new_name).map_err(CoreError::from)
    }
    pub fn delete_album(&self, id: i64) -> Result<()> {
        self.db()?.delete_album(id).map_err(CoreError::from)
    }
    pub fn all_albums(&self) -> Result<Vec<picasa_db::AlbumRow>> {
        self.db()?.all_albums().map_err(CoreError::from)
    }
    pub fn album_photo_ids(&self, album_id: i64) -> Result<Vec<i64>> {
        self.db()?.album_photo_ids(album_id).map_err(CoreError::from)
    }
    pub fn albums_of(&self, photo_id: i64) -> Result<Vec<picasa_db::AlbumRow>> {
        self.db()?.albums_of(photo_id).map_err(CoreError::from)
    }
    pub fn add_to_album(&self, album_id: i64, ids: &[i64], now: i64) -> Result<usize> {
        self.db()?
            .add_to_album(album_id, ids, now)
            .map_err(CoreError::from)
    }
    pub fn remove_from_album(&self, album_id: i64, ids: &[i64]) -> Result<usize> {
        self.db()?
            .remove_from_album(album_id, ids)
            .map_err(CoreError::from)
    }

    // ---------------------------------------------------------- M4：标签定义
    pub fn rename_tag(&self, old: &str, new: &str) -> Result<usize> {
        self.db()?.rename_tag(old, new).map_err(CoreError::from)
    }
    pub fn delete_tag(&self, label: &str) -> Result<usize> {
        self.db()?.delete_tag(label).map_err(CoreError::from)
    }

    // ---------------------------------------------------------- M4：EXIF 摘要
    pub fn set_photo_exif(&self, id: i64, e: &picasa_db::PhotoExif) -> Result<()> {
        self.db()?.set_photo_exif(id, e).map_err(CoreError::from)
    }
    pub fn photo_exif(&self, id: i64) -> Result<picasa_db::PhotoExif> {
        self.db()?.photo_exif(id).map_err(CoreError::from)
    }
    pub fn photos_pending_exif(&self, limit: usize) -> Result<Vec<i64>> {
        self.db()?.photos_pending_exif(limit).map_err(CoreError::from)
    }
    pub fn camera_options(&self) -> Result<Vec<(String, String)>> {
        self.db()?.camera_options().map_err(CoreError::from)
    }
    pub fn lens_options(&self) -> Result<Vec<String>> {
        self.db()?.lens_options().map_err(CoreError::from)
    }

    // ---------------------------------------------------------- M4：近期搜索
    pub fn push_recent_search(&self, query_json: &str, now: i64) -> Result<()> {
        self.db()?
            .push_recent_search(query_json, now)
            .map_err(CoreError::from)
    }
    pub fn recent_searches(&self) -> Result<Vec<picasa_db::RecentSearch>> {
        self.db()?.recent_searches().map_err(CoreError::from)
    }
    pub fn clear_recent_searches(&self) -> Result<()> {
        self.db()?.clear_recent_searches().map_err(CoreError::from)
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

    /// 切换「有缩略图的排在前面」。只影响显示顺序，不改库里任何数据。
    pub fn set_thumbs_first(&mut self, on: bool) -> Result<()> {
        if self.thumbs_first == on {
            return Ok(());
        }
        self.thumbs_first = on;
        self.prefs.thumbs_first = on;
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
        // (id, path, origin, import_source_id)
        let pairs: Vec<(i64, String, String, Option<i64>)> = match self.db() {
            Ok(db) => match db.roots(false) {
                Ok(rows) => rows
                    .into_iter()
                    .map(|r| (r.id, r.path, r.origin, r.import_source_id))
                    .collect(),
                Err(e) => {
                    tracing::warn!(error = %e, "读 roots 失败");
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        // IDX-21 ⑦：本机「文件夹」组不显示导入带进来的根，否则同一棵树会
        // 在左栏出现两次。这份映射是「哪个根属于哪个导入来源」的唯一真相，
        // 树重建与侧边栏分组都读它。
        self.root_origins = pairs
            .iter()
            .map(|(id, path, origin, src)| (path.clone(), (*id, origin.clone(), *src)))
            .collect();
        self.root_paths = if pairs.is_empty() && !self.prefs.roots.is_empty() {
            // 降级兜底：`roots` 表不可用（迁移失败 / 老库）时退回偏好。
            // 这段代码存在的唯一理由就是「别让用户看到一个空图库」。
            self.prefs.roots.clone()
        } else {
            pairs.into_iter().map(|(_, p, _, _)| p).collect()
        };
    }

    /// 这个根是不是导入带进来的（`origin = imported`）。
    pub fn is_imported_root(&self, path: &str) -> bool {
        self.root_origins
            .get(path)
            .map(|(_, origin, _)| origin == "imported")
            .unwrap_or(false)
    }

    /// 某个根属于哪个导入来源（`import_sources.id`）；本机根返回 `None`。
    pub fn import_source_of_root(&self, path: &str) -> Option<i64> {
        self.root_origins.get(path).and_then(|(_, _, src)| *src)
    }

    /// 只含**本机**关注根的顶层节点（IDX-21 ⑦：导入的树走自己的分组）。
    pub fn local_roots(&self) -> Vec<&FolderNode> {
        self.roots
            .iter()
            .filter(|r| !self.is_imported_root(&r.path))
            .collect()
    }

    /// 某个导入来源的顶层节点（按来源 id 归组，供「导入的索引」分组用）。
    pub fn imported_roots_of(&self, source_id: i64) -> Vec<&FolderNode> {
        self.roots
            .iter()
            .filter(|r| self.import_source_of_root(&r.path) == Some(source_id))
            .collect()
    }

    /// 当前关注根目录（绝对路径）。UI 的「添加文件夹」从中挑起点。
    pub fn root_paths(&self) -> Vec<PathBuf> {
        self.root_paths.iter().map(PathBuf::from).collect()
    }

    pub fn add_root(&mut self, root: &Path) {
        // 走 DB：`roots` 表是唯一真相源；偏好里的 roots 不再写入。
        let canonical = paths::canonical_string(root);
        // 可追溯性：`add_root` 的 4 个调用点全是用户动作（导入 / 重新打开根 /
        // M4 重新匹配 / 进度面板）。曾出现「隔离空库凭空多出真实照片目录」的
        // 误判——有了这条日志（含调用点标记）就能一眼区分「用户导入」与「串库」。
        tracing::info!(root = %canonical, "add_root 被调用（用户动作或重新匹配）");
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
        // M4.3 补遗：加根**必须**把树标脏。
        //
        // 「关闭」走 `close_root`，它末尾自己 `reload_tree()`，所以关闭立刻生效；
        // 而「重新打开」走这里，旧实现只更新了 `root_paths` 就返回——`flush_reload`
        // 里的 `take_tree_dirty()` 拿到 false，直接跳过 `reload_tree()`，于是侧栏
        // 文件夹里根本没有这个根，用户点「重新打开」看不出任何变化（实机复现）。
        //
        // 这里只置脏、不直接重建：加根可能紧跟着一整轮扫描，逐事件重建树的老问题
        // 依旧要靠 UI 侧节流兜住（见 `flush_reload` 的文档）。
        self.tree_dirty = true;
    }

    pub fn remove_root(&mut self, root: &Path) {
        let s = paths::canonical_string(root);
        // 关闭根目录：**只改状态，不删数据**（P-07 / `docs/05` §1.3.2），保留
        // roots 行（含 id / 排除项），便于再次添加时复用。
        //
        // 「从图库移除」的既有语义（F2-08）：索引也一起清掉。M4.3 阶段 2 起改走
        // `clear_index_for_root(row.id)`——旧实现只 `delete_folder(根夹)`，会让整棵
        // 子树 folders+photos 成孤儿，且只快照根夹的标记、子目录标记静默丢失，
        // 违反 R-M43-02。作用域版本按路径前缀整棵子树快照 + 删除，并作废该 scope
        // 的持久任务意图（IDX-11）。
        // ⚠️ 只删自己的索引，不动磁盘文件；「关闭但保留索引」是 E 导航的独立动作。
        if let Ok(db) = self.db()
            && let Ok(Some(row)) = db.root_by_path(&s)
        {
            let _ = db.set_root_closed(row.id, true);
            let _ = db.clear_index_for_root(row.id);
        }
        self.root_paths.retain(|p| p != &s);
        self.refresh_roots();
        // 移除根同样要置脏：`roots(false)` 里已经没有它，但 `self.roots` 还是旧的。
        self.tree_dirty = true;
    }

    /// **彻底**移除一个关注根（m10218）：索引、缩略图、`roots` 行一起删掉。
    ///
    /// 与 [`remove_root`](Self::remove_root) 的差别：
    /// - `remove_root` = 「从图库移除」但保留 roots 行（`state=1` + 「索引已移除」标记），
    ///   索引管理页里那行空壳还在，可以「重新索引（恢复）」；
    /// - `purge_root` = 用户要的「彻底删除」：DB 侧由
    ///   [`Db::purge_index_for_root`](picasa_db::Db::purge_index_for_root) 连根行一起删，
    ///   这里再把内存侧的 `root_paths` / 树一起刷新，界面上该根及其子树立即消失。
    ///
    /// 不再调 `close_root`：根行已经没了，没有可关闭的行；再关一次只会白跑
    /// 一次 `root_by_path` 查询。标记仍按身份骨架保留（R-M43-02）。
    pub fn purge_root(&mut self, root: &Path) {
        let s = paths::canonical_string(root);
        self.root_paths.retain(|p| p != &s);
        self.refresh_roots();
        // 置脏而不是直接重建：与 `add_root` / `remove_root` 一致，重建交给
        // `flush_reload` 的节流（UI 侧随后会 `reload_now`，两者任一都能生效）。
        self.tree_dirty = true;
    }

    /// 关闭根目录（E 导航）：只把 `roots.state` 置 closed，**索引与缩略图全留着**。
    ///
    /// 与 [`remove_root`](Self::remove_root) 的区别是这条：`remove_root` 是「从图库移除」，
    /// 连索引一起删；这里是「先不在左栏显示」。下次重新添加同一路径时
    /// `Db::upsert_root` 会把 `state` 置回 active 并复用 `root_id`，排除记录也原样沿用
    /// ——这就是 `docs/05` §1.3.2 说的「再次添加秒开」。
    pub fn close_root(&mut self, root: &Path) {
        let s = paths::canonical_string(root);
        {
            if let Ok(db) = self.db()
                && let Ok(Some(row)) = db.root_by_path(&s)
            {
                let _ = db.set_root_closed(row.id, true);
            }
        }
        self.refresh_roots();
        let _ = self.reload_tree();
        let _ = self.reload_photos();
    }

    /// 已关闭的根目录（诊断页「已关闭的根目录」区用）。
    pub fn closed_roots(&self) -> Vec<String> {
        let Ok(db) = self.db() else {
            return Vec::new();
        };
        db.roots(true)
            .map(|rows| {
                rows.into_iter()
                    .filter(|r| r.state != 0)
                    .map(|r| r.path)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 关闭某个**子文件夹**（E 导航）：写一条排除规则，并把它连同子树的索引删掉。
    ///
    /// 返回 `Ok(false)` 表示它是根目录本身或找不到所属根——根本身不能排除
    /// （要停掉整个根，用 `close_root`）。返回 `Ok(true)` 表示已排除。
    ///
    /// ⚠️ 排除规则**不只在树里隐藏**：`scanner::walk_and_store` 会读同一张 `exclusions`
    /// 表并在遍历时整棵剪掉，否则下一次扫描又会把它加回来。
    pub fn exclude_folder(&mut self, path: &Path) -> Result<bool> {
        let s = paths::canonical_string(path);
        let (root_id, root_path) = {
            let db = self.db()?;
            let root = db
                .roots(false)?
                .into_iter()
                .filter(|r| r.path != s && s.starts_with(&format!("{}/", r.path)))
                .max_by_key(|r| r.path.len());
            match root {
                Some(r) => (r.id, r.path),
                None => return Ok(false),
            }
        };
        let rel = s
            .strip_prefix(&root_path)
            .unwrap_or("")
            .trim_start_matches('/')
            .to_string();
        if rel.is_empty() {
            return Ok(false);
        }
        {
            let db = self.db()?;
            db.add_exclusion(root_id, &rel)?;
            // 立刻从索引里拿掉，别等下一次扫描：用户点了「关闭」，照片就该马上消失。
            if let Ok(Some(id)) = db.folder_id(&s)
                && let Ok(ids) = db.subtree_folder_ids(id)
            {
                for fid in ids {
                    let _ = db.delete_folder(fid);
                }
            }
        }
        self.reload_tree()?;
        self.reload_photos()?;
        Ok(true)
    }

    /// 全部排除项（附所属根目录路径），诊断页用。**包含已关闭根目录**的排除项。
    pub fn all_exclusions(&self) -> Vec<(String, ExclusionRow)> {
        let Ok(db) = self.db() else {
            return Vec::new();
        };
        let roots = db.roots(true).unwrap_or_default();
        let mut out = Vec::new();
        for r in roots {
            if let Ok(list) = db.exclusions(r.id) {
                for e in list {
                    out.push((r.path.clone(), e));
                }
            }
        }
        out
    }

    /// 恢复一条排除项：删掉记录，返回它所属的根目录路径（调用方据此触发重扫把它加回来）。
    pub fn restore_exclusion(&mut self, exclusion_id: i64) -> Result<Option<String>> {
        let (found, root_path) = {
            let db = self.db()?;
            let mut found = None;
            let mut root_path = None;
            for r in db.roots(true)? {
                if let Ok(list) = db.exclusions(r.id)
                    && list.iter().any(|e| e.id == exclusion_id)
                {
                    found = Some(exclusion_id);
                    root_path = Some(r.path);
                    break;
                }
            }
            (found, root_path)
        };
        if found.is_some() {
            let db = self.db()?;
            db.remove_exclusion(exclusion_id)?;
        }
        Ok(root_path)
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
                // ⚠️ 这里**不能**直接 `reload_tree()`。
                //
                // 落库是逐目录一事务、逐目录一条事件：一个装照片的移动硬盘动辄几千个
                // 相册目录。`tree_bench` 实测 8000 个目录时单次 `reload_tree` 46.8 ms，
                // 若照事件数各来一次，累计 405 秒全花在 UI 线程重建树上——界面就是
                // 「一动不动」。改成置脏，由 UI 侧节流调用 `flush_tree_if_due`。
                self.tree_dirty = true;
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
                // IDX-16：离线/在线状态变化要让面板刷新（root 行的离线标记、
                // 目录树的 is_online 都跟着变），否则发射了事件界面也看不到。
                true
            }
            CoreEvent::IndexCleared { photos, thumbs } => {
                // 删索引是**整体重置**，不是增量刷新：照片列表、树、选择集、
                // 查看器指针、扫描状态全部归零，否则会留下一堆指向已不存在行的引用。
                self.scanning = false;
                self.scan_progress = None;
                self.scan_summary = None;
                self.photos.clear();
                self.photos_total = 0;
                self.photos_complete = true;
                self.selection.clear();
                self.current = None;
                self.tree_dirty = true;
                let _ = self.reload_tree();
                let msg = if photos == 0 && thumbs == 0 {
                    "索引已清空（本来就没有索引数据）".to_string()
                } else {
                    format!("索引已删除：{photos} 张照片、{thumbs} 张缩略图")
                };
                self.toast = Some((msg, 4.0));
                true
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
        self.prefs.view_mode = self.view_mode;
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
        // 标题按「范围由窄到宽」自报家门：一次性范围（IDX-19）> 标签（IDX-22）
        // > 星标 > 视图名。窄的范围才是用户最近一次动作的依据；把宽的名字写在
        // 顶上会让用户以为筛选没生效（U-34 / IDX-22 ⑤）。
        if self.filter.as_ref().is_some_and(|f| f.ids.is_some()) {
            let n = self.photos_total.max(self.photos.len());
            return format!("本次导入 · {n} 张");
        }
        // IDX-22 ⑤：点左栏标签后网格显示该标签的**全部照片**，标题必须自报家门。
        // 允许与星标叠加，此时报标签（规格：标题「标签：X · N 张」）。
        if let Some(tag) = self.filter.as_ref().and_then(|f| f.tag.as_deref()) {
            let n = self.photos_total.max(self.photos.len());
            return format!("标签：{tag} · {n} 张");
        }
        // 侧栏「星标」和筛选面板「只看收藏」是同一个 `starred_only`。
        // 标题跟着它走，否则高亮已经切过去了，顶上仍写着「所有照片 · N 张」。
        if self.filter.as_ref().is_some_and(|f| f.starred_only) {
            return "星标".to_string();
        }
        match self.view {
            View::All => "所有照片".to_string(),
            View::Recent => "最近更改".to_string(),
            View::Folder(id) => self
                .node_by_id(id)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "未命名文件夹".to_string()),
        }
    }

    /// 顶栏双语境路径面包屑（`09` §7.3）。
    ///
    /// * 文件夹语境：`图库` + 各级祖先名 + 当前文件夹名。除根段外每段都带
    ///   `folder_id`（祖先可点击跳转；根段只是语境前缀，不可点）。
    /// * 根语境（所有照片 / 最近更改）：按 §7.3「弱化面包屑」收敛成单段
    ///   `图库`——语境由左栏标题区表达，顶栏不再重复视图名。
    ///
    /// 纯树遍历命名逻辑，不查库、不加数据层能力。
    pub fn view_breadcrumb(&self) -> Vec<(String, Option<i64>)> {
        let root_crumb = ("图库".to_string(), None);
        let View::Folder(id) = self.view else {
            return vec![root_crumb];
        };
        let Some(node) = self.node_by_id(id) else {
            return vec![root_crumb];
        };
        let mut crumbs = vec![root_crumb];
        let target = node.path.as_str();
        // 从各根往下按「target 是否在该子树内」逐级下钻，天然得到祖先链。
        fn walk(nodes: &[FolderNode], target: &str, out: &mut Vec<(String, Option<i64>)>) {
            for n in nodes {
                if target == n.path || is_descendant(target, &n.path) {
                    out.push((n.name.clone(), n.folder_id));
                    if target != n.path {
                        walk(&n.children, target, out);
                    }
                    return;
                }
            }
        }
        walk(&self.roots, target, &mut crumbs);
        // 兜底：树里居然没走到（理论不可达，防御树重建中间态）——至少显示当前文件夹名。
        if crumbs.len() < 2 || crumbs.last().and_then(|(_, id)| *id) != node.folder_id {
            crumbs.push((node.name.clone(), node.folder_id));
        }
        crumbs
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
/// 某一年的拍摄日期范围（epoch 秒）。时间轴 / 筛选面板共用同一条算式。
pub fn year_range_seconds(year: i32) -> (i64, i64) {
    use chrono::TimeZone;
    let start = chrono::Utc
        .with_ymd_and_hms(year, 1, 1, 0, 0, 0)
        .single()
        .map(|t| t.timestamp())
        .unwrap_or(0);
    let end = chrono::Utc
        .with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0)
        .single()
        .map(|t| t.timestamp())
        .unwrap_or(i64::MAX / 2);
    (start, end)
}

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
///
/// ## 为什么这段要写得这么小心
///
/// 它曾经是 55 FPS 的头号杀手：5 万张**每帧 92 ms**，一帧就吃掉 5 帧的时间。
/// 原因不在循环本身，而在两处隐蔽的开销：
///
/// 1. `same_day` 每张照片做 **2 次时区换算 + 2 次 `format!` 字符串分配**（10 万次分配）；
/// 2. 结果没有人缓存，photos 没变也每帧从头算一遍。
///
/// 这里改掉第 1 点（组首的本地日只算一次、比较改用 `NaiveDate` 不分配内存），
/// 第 2 点交给 `picasa-ui` 的布局缓存（photos 不变就整块复用）。
pub fn group_by_day(photos: &[PhotoRow]) -> Vec<(String, usize, usize)> {
    use chrono::{Local, NaiveDate, TimeZone};
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    let mut i = 0;
    while i < photos.len() {
        let ts = photos[i].effective_taken_at();
        // 组首算一次日期：既是分组标题，也是后面逐张比较的基准。
        let day: Option<NaiveDate> = Local
            .timestamp_opt(ts, 0)
            .single()
            .map(|dt| dt.date_naive());
        let title = match day {
            Some(d) => d.format("%Y年%m月%d日").to_string(),
            None => "未知日期".to_string(),
        };
        let mut j = i + 1;
        while j < photos.len() {
            let other = photos[j].effective_taken_at();
            // 时间戳完全相同最常见（连拍、导入同一秒），先走整数快路径，省一次时区换算。
            let same = if other == ts {
                true
            } else {
                match day {
                    Some(d) => Local
                        .timestamp_opt(other, 0)
                        .single()
                        .map(|dt| dt.date_naive() == d)
                        .unwrap_or(false),
                    None => false,
                }
            };
            if !same {
                break;
            }
            j += 1;
        }
        out.push((title, i, j));
        i = j;
    }
    out
}

impl Library {
    /// 当前视图的根目录列表（用于「添加文件夹」后启动扫描）。
    pub fn scan_roots(&self) -> Vec<PathBuf> {
        self.root_paths()
    }
}
