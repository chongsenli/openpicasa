//! 中央栅格：**虚拟布局 + 吸顶分组头**——M1 的最大技术风险（02 §8.2）。
//!
//! ## 为什么不用 `show_rows`
//!
//! `show_rows` 假设「所有行等高」。而这里分组头 26px、单元格行 200+px，
//! 混在一起没有统一的「行高」，所以它不可用。
//!
//! ## 采用的方案
//!
//! `ScrollArea::show_viewport` 把**内容坐标系**交给我们：
//! 自己算每个分组头的 y、每行的高、当前视口覆盖哪些行，只画那几十个。
//! 视口外的照片**一个 rect 都不创建**——这是 10 万张还能 60fps 的唯一原因。
//!
//! ```text
//! content 坐标（y=0 是内容顶端，随滚动不变）
//! ┌─────────────────────────┐ ▲
//! │ 2011年5月13日  (26px)   │ │ group.y
//! │ [img][img][img][img]    │ │
//! │ [img][img]              │ │
//! │ 2011年5月12日           │ │
//! │ [img][img][img]         │ │
//! └─────────────────────────┘ ▼ total_h
//! ```
//!
//! `viewport` 是**相对于内容原点**的矩形：`viewport.min.y` 就是当前滚到哪。

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Rect, Response, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, Vec2, pos2, vec2,
};
use picasa_core::{IndexSnapshot, ThumbService, Tier, library};
use picasa_db::{Db, PhotoRow};

use crate::icons::{self, Icon};
use crate::index_progress;
use crate::textures::{TexState, TextureCache};
use crate::theme::{Metrics, Theme};

/// 一个分组（日期）在内容坐标里的位置。
#[derive(Clone, Debug)]
pub struct Group {
    pub title: String,
    pub y: f32,
    pub first: usize,
    pub count: usize,
}

/// 栅格布局（每帧重建；重建代价 O(分组数)，10 万张约 3000 天分组 → 可忽略）。
pub struct GridLayout {
    pub cell: f32,
    pub cell_h: f32,
    pub cols: usize,
    pub groups: Vec<Group>,
    pub total_h: f32,
    pub content_w: f32,
    pub show_filename: bool,
    pub has_headers: bool,
}

impl GridLayout {
    pub fn build(photos: &[PhotoRow], width: f32, tier: Tier, has_headers: bool) -> Self {
        let cell = Metrics::CELL_SIZES[tier.index()];
        let show_filename = tier.shows_filename();
        let cell_h = cell
            + if show_filename {
                Metrics::FILENAME_H
            } else {
                0.0
            };
        let gap = Metrics::CELL_GAP;
        let pad = Metrics::GRID_PAD;

        let usable = (width - 2.0 * pad).max(cell);
        let cols = (((usable + gap) / (cell + gap)).floor() as usize).max(1);

        let mut groups = Vec::new();
        let mut y = pad;

        let raw_groups: Vec<(String, usize, usize)> = if has_headers {
            library::group_by_day(photos)
        } else {
            vec![(String::new(), 0, photos.len())]
        };

        for (title, start, end) in raw_groups {
            let count = end.saturating_sub(start);
            if count == 0 {
                continue;
            }
            groups.push(Group {
                title,
                y,
                first: start,
                count,
            });
            let rows = count.div_ceil(cols);
            let header = if has_headers {
                Metrics::GROUP_HEADER_H
            } else {
                0.0
            };
            // 每行占 (cell_h + gap)，行间 gap 由这个式子统一吃掉
            y += header + rows as f32 * (cell_h + gap) + gap;
        }

        let total_h = y + pad;
        Self {
            cell,
            cell_h,
            cols,
            groups,
            total_h,
            content_w: width,
            show_filename,
            has_headers,
        }
    }

    /// 某个索引对应的分组下标。
    fn group_of(&self, index: usize) -> Option<usize> {
        // 分组按 first 升序，二分查找最后一个 first <= index
        let mut lo = 0usize;
        let mut hi = self.groups.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.groups[mid].first <= index {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let idx = lo.saturating_sub(1);
        self.groups.get(idx).map(|_| idx)
    }

    /// 索引 → 缩略图方块的内容坐标 rect。
    pub fn thumb_rect(&self, index: usize) -> Option<Rect> {
        let gi = self.group_of(index)?;
        let g = &self.groups[gi];
        if index < g.first || index >= g.first + g.count {
            return None;
        }
        let local = index - g.first;
        let row = local / self.cols;
        let col = local % self.cols;
        let x = Metrics::GRID_PAD + col as f32 * (self.cell + Metrics::CELL_GAP);
        let y = g.y + self.header() + row as f32 * (self.cell_h + Metrics::CELL_GAP);
        Some(Rect::from_min_size(pos2(x, y), vec2(self.cell, self.cell)))
    }

    /// 索引 → 文件名行的内容坐标 rect（不显示文件名时返回 None）。
    pub fn filename_rect(&self, index: usize) -> Option<Rect> {
        if !self.show_filename {
            return None;
        }
        let r = self.thumb_rect(index)?;
        Some(Rect::from_min_size(
            pos2(r.left(), r.bottom() + 1.0),
            vec2(self.cell, Metrics::FILENAME_H),
        ))
    }

    /// 分组头高度（无分组头时 0）。
    fn header(&self) -> f32 {
        if self.has_headers {
            Metrics::GROUP_HEADER_H
        } else {
            0.0
        }
    }

    /// 照片总数。用于把「视口完全在内容下方」表达成一个空区间。
    pub fn total_count(&self) -> usize {
        self.groups.last().map(|g| g.first + g.count).unwrap_or(0)
    }

    /// 增量追加：把 `photos[from..]` 接到现有布局的尾部（C1 分页补齐用）。
    ///
    /// **不做全量重算**——这是分页的意义所在：补齐时若每次都 `build` 一遍，
    /// 5 万张要 100 ms，用户滚到底会周期性卡顿，等于把首屏省下的时间又还回去。
    ///
    /// 做法：从 `from - 1`（上一张已排的照片）开始重新分组，这样第一组必然接得上
    /// 现有最后一天；标题相同且位置连续就合并进最后一天，否则追加新组。
    /// 之后整体重算 y——O(分组数)，1800 组实测 ~10 µs，且不发生内存分配。
    fn extend(&mut self, photos: &[PhotoRow], from: usize) {
        if from == 0 || from >= photos.len() {
            return;
        }
        let anchor = from - 1;
        let raw = library::group_by_day(&photos[anchor..]);
        for (i, (title, start, end)) in raw.into_iter().enumerate() {
            let abs_first = anchor + start;
            let abs_end = anchor + end;
            if i == 0
                && let Some(last) = self.groups.last_mut()
                && last.title == title
                && last.first + last.count == abs_first
            {
                // 与最后一个分组是同一天：把新照片并进去，不新建组。
                last.count = abs_end - last.first;
                continue;
            }
            self.groups.push(Group {
                title,
                y: 0.0, // 下面 recompute_y 统一填
                first: abs_first,
                count: abs_end - abs_first,
            });
        }
        self.recompute_y();
    }

    /// 按当前 `cols` / `cell_h` 重排所有分组的 y 与 `total_h`。
    ///
    /// 追加与「列数变了但分组没变」都走这里。纯算术，无分配。
    fn recompute_y(&mut self) {
        let header = self.header();
        let mut y = Metrics::GRID_PAD;
        for g in &mut self.groups {
            g.y = y;
            let rows = g.count.div_ceil(self.cols);
            y += header + rows as f32 * (self.cell_h + Metrics::CELL_GAP) + Metrics::CELL_GAP;
        }
        self.total_h = y + Metrics::GRID_PAD;
    }

    /// 索引区间覆盖到的分组下标范围（左闭右开），用于画**普通位置**的分组头。
    ///
    /// 分组头只有在「离开视口顶部」时才交给吸顶头补画；在正常位置时必须自己画出来，
    /// 否则每组上方会留下一条 26px 的空白带，用户永远看不到日期（实测踩到过）。
    pub fn groups_in_range(&self, first: usize, last: usize) -> std::ops::Range<usize> {
        if self.groups.is_empty() || last <= first {
            return 0..0;
        }
        let g0 = self.group_of(first).unwrap_or(0);
        let g1 = self.group_of(last - 1).unwrap_or(g0);
        g0..(g1 + 1)
    }

    /// 视口覆盖的单元格索引区间（左闭右开）。
    ///
    /// **必须按行号算，不能按分组边界算**：一个「同一天有 3000 张」的分组如果整组返回，
    /// 就等于每帧给 3000 个单元格建 rect，虚拟化直接失效（这是实测踩到的坑）。
    /// 因此这里先定位视口命中的首/末分组，再在分组内部把 y 换算成行号，只返回行区间对应的索引。
    /// 两侧各留一行 overscan，避免快速滚动时看到「先空白后补齐」。
    pub fn visible_index_range(&self, viewport: Rect) -> (usize, usize) {
        let n = self.total_count();
        if self.groups.is_empty() {
            return (0, 0);
        }
        let top = viewport.min.y;
        let bottom = viewport.max.y;
        if bottom <= 0.0 {
            return (0, 0);
        }

        // g0 = 第一个「底部还没越过视口顶部」的分组
        let mut g0 = 0usize;
        while g0 + 1 < self.groups.len() && self.group_bottom(g0) < top {
            g0 += 1;
        }
        if self.group_bottom(g0) < top {
            return (n, n); // 视口整个在内容下方（一次滚动跳太远时的中间帧）
        }
        // g1 = 最后一个「顶部还没越过视口底部」的分组
        let mut g1 = g0;
        while g1 + 1 < self.groups.len() && self.groups[g1 + 1].y <= bottom {
            g1 += 1;
        }

        let first = self.first_index_in(g0, top);
        let last = self.last_index_in(g1, bottom);
        let last = last.max(first).min(n);
        (first.min(n), last)
    }

    /// 分组 `gi` 内、与 `top` 相交的第一行的全局索引（向上 overscan 一行）。
    fn first_index_in(&self, gi: usize, top: f32) -> usize {
        let g = &self.groups[gi];
        let pitch = self.cell_h + Metrics::CELL_GAP;
        let local_y = top - g.y - self.header();
        let row = if local_y <= 0.0 {
            0
        } else {
            (local_y / pitch).floor() as usize
        };
        let row = row.saturating_sub(1);
        g.first + (row * self.cols).min(g.count)
    }

    /// 分组 `gi` 内、与 `bottom` 相交的最后一行之后的全局索引（向下 overscan 一行）。
    fn last_index_in(&self, gi: usize, bottom: f32) -> usize {
        let g = &self.groups[gi];
        let pitch = self.cell_h + Metrics::CELL_GAP;
        let local_y = bottom - g.y - self.header();
        let row = if local_y <= 0.0 {
            0
        } else {
            (local_y / pitch).ceil() as usize
        };
        let row = row + 1;
        g.first + (row * self.cols).min(g.count)
    }

    fn group_bottom(&self, gi: usize) -> f32 {
        match self.groups.get(gi + 1) {
            Some(next) => next.y,
            None => self.total_h,
        }
    }

    /// 视口顶部正在「压住」哪个分组（吸顶头用）。
    pub fn sticky_group(&self, viewport_top: f32) -> Option<usize> {
        if !self.has_headers {
            return None;
        }
        let mut found = None;
        for (i, g) in self.groups.iter().enumerate() {
            if g.y <= viewport_top {
                found = Some(i);
            } else {
                break;
            }
        }
        found.filter(|i| {
            // 只有「头已经滚出视口」时才需要吸顶补画；否则它本来就在正常位置
            viewport_top > self.groups[*i].y + 1.0
        })
    }

    /// 内容坐标 → 命中的索引（用于点击空白判断；M1 不做框选）。
    pub fn index_at(&self, pos: egui::Pos2) -> Option<usize> {
        for (gi, g) in self.groups.iter().enumerate() {
            if pos.y < g.y || pos.y > self.group_bottom(gi) {
                continue;
            }
            let local_y = pos.y - g.y - self.header();
            if local_y < 0.0 {
                return None;
            }
            let row = (local_y / (self.cell_h + Metrics::CELL_GAP)).floor() as usize;
            let col_x = pos.x - Metrics::GRID_PAD;
            if col_x < 0.0 {
                return None;
            }
            let col = (col_x / (self.cell + Metrics::CELL_GAP)).floor() as usize;
            if col >= self.cols {
                return None;
            }
            let local = row * self.cols + col;
            if local >= g.count {
                return None;
            }
            return Some(g.first + local);
        }
        None
    }
}

/// 栅格布局的缓存（C1）。**跨帧复用**，`photos` 没变就一次 `build` 都不做。
///
/// ## 为什么必须有它
///
/// `GridLayout::build` 会对全部照片跑一遍 `group_by_day`（含时区换算）。
/// 5 万张实测 **110 ms/帧**——它是「M1 每帧都在做的事」里最贵的一件，
/// 而它**每帧输入的都一模一样**。缓存之后，滚动时的代价是 0。
///
/// ## 三种命中
///
/// | 情况 | 动作 | 代价 |
/// | --- | --- | --- |
/// | 什么都没变 | 直接返回 | 0 |
/// | photos 尾部变长（分页补齐） | 只排新增部分 | O(新增量) |
/// | 窗口宽度 / 档位 / 分组头 / 排序变了 | 全量重建 | O(n) |
pub struct GridCache {
    layout: GridLayout,
    key: LayoutKey,
    /// 布局已纳入的照片数（可能小于 `photos.len()`，因为 extend 是分步的）
    built: usize,
    ready: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LayoutKey {
    width_bits: u32,
    tier: u8,
    headers: bool,
    /// 首尾 id 用来识别「同样长度但内容换了」（切排序、切视图）
    first_id: i64,
    last_id: i64,
}

impl Default for GridCache {
    fn default() -> Self {
        Self::new()
    }
}

impl GridCache {
    pub fn new() -> Self {
        Self {
            layout: GridLayout {
                cell: 0.0,
                cell_h: 0.0,
                cols: 1,
                groups: Vec::new(),
                total_h: 0.0,
                content_w: 0.0,
                show_filename: false,
                has_headers: false,
            },
            key: LayoutKey {
                width_bits: 0,
                tier: 0,
                headers: false,
                first_id: 0,
                last_id: 0,
            },
            built: 0,
            ready: false,
        }
    }

    /// 取（必要时算）当前帧的布局。
    pub fn get(
        &mut self,
        photos: &[PhotoRow],
        width: f32,
        tier: Tier,
        has_headers: bool,
    ) -> &GridLayout {
        let key = LayoutKey {
            width_bits: width.to_bits(),
            tier: tier.index() as u8,
            headers: has_headers,
            first_id: photos.first().map(|p| p.id).unwrap_or(0),
            last_id: photos.last().map(|p| p.id).unwrap_or(0),
        };

        if self.ready {
            // ① 完全没变
            if key == self.key && photos.len() == self.built {
                return &self.layout;
            }
            // ② 前缀不变、尾部变长 → 增量追加（分页补齐）
            let prefix_same = key.width_bits == self.key.width_bits
                && key.tier == self.key.tier
                && key.headers == self.key.headers
                && key.first_id == self.key.first_id
                && photos.len() > self.built;
            if prefix_same {
                self.layout.extend(photos, self.built);
                self.built = photos.len();
                self.key = key;
                return &self.layout;
            }
        }

        // ③ 全量重建
        self.layout = GridLayout::build(photos, width, tier, has_headers);
        self.built = photos.len();
        self.key = key;
        self.ready = true;
        &self.layout
    }
}

/// 一次点击的信息（交给 `Library` 处理，栅格自己不碰选择集）。
#[derive(Clone, Copy, Debug)]
pub struct ClickInfo {
    pub index: usize,
    pub additive: bool,
    pub range: bool,
}

/// 一帧最多上报多少条待回填 id。
///
/// 正常视口只有几十个格子，这个上限只是防「窗口被拉得特别高 + 小档位」时的意外：
/// 上万条 id 穿一次 channel 是几百 KB，而回填线程一次也只做一张，报了也没用。
const NEED_DIMS_MAX: usize = 256;

pub struct GridOutput {
    pub clicked: Option<ClickInfo>,
    pub opened: Option<usize>,
    pub context: Option<(usize, egui::Pos2)>,
    pub scroll_y: f32,
    pub total_h: f32,
    pub visible: usize,
    /// 视口顶部第一张可见照片的索引（用于「重载后锚定」——记住用户在看哪一张）。
    pub top_index: usize,
    /// 视口里**宽高还空着**的照片 id（M2.1 B：可见区域按需回填）。
    ///
    /// 只看内存里的 `PhotoRow`，**不查库、不加锁**：栅格是每帧跑的，
    /// 为这件事去摸一次 DB 锁，会和索引线程的写事务顶牛。
    /// 去重与「已经试过的别再试」都在 `DimsService` 里做，这里只管如实上报。
    pub need_dims: Vec<i64>,
}

/// 画整个栅格。
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    photos: &[PhotoRow],
    selection: &std::collections::HashSet<i64>,
    current_index: Option<usize>,
    tier: Tier,
    has_headers: bool,
    restore_scroll: Option<f32>,
    // 「重载后锚定」的目标照片 id（D 流式展示）。`None` = 不用锚定。
    anchor_id: Option<i64>,
    db: &Db,
    service: &ThumbService,
    cache: &mut TextureCache,
    grid: &mut GridCache,
    theme: &Theme,
    snapshot: &IndexSnapshot,
) -> GridOutput {
    let mut out = GridOutput {
        clicked: None,
        opened: None,
        context: None,
        scroll_y: 0.0,
        total_h: 0.0,
        visible: 0,
        top_index: 0,
        need_dims: Vec::new(),
    };

    let full = ui.available_rect_before_wrap();
    ui.painter().rect_filled(full, 0.0, theme.grid_bg);

    if photos.is_empty() {
        draw_empty_state(ui, full, theme, cache);
        return out;
    }

    let ctx = ui.ctx().clone();
    let content_w = (ui.available_width() - Metrics::SCROLLBAR).max(120.0);
    // 每帧「算布局」在 M2 是 5 万张 110 ms 的开销，而输入几乎每帧都一样——
    // 交给 GridCache，滚动时这一行是 0 代价。
    let layout = grid.get(photos, content_w, tier, has_headers);
    // 正在索引的文件夹（含子目录）：这些目录里 `thumb_state == 0` 的格子只画占位，
    // **不要**再向 ThumbService 要一次缩略图（会和索引线程抢同一张图的解码）。
    let indexing_folders: std::collections::HashSet<i64> = {
        let mut set = std::collections::HashSet::new();
        for t in snapshot.tasks.iter().filter(|t| t.state.is_running()) {
            if let Ok(Some(id)) = db.folder_id(&t.path)
                && let Ok(ids) = db.subtree_folder_ids(id)
            {
                set.extend(ids);
            }
        }
        set
    };
    out.total_h = layout.total_h;
    // 布局体检：栅格列数/单元格尺寸是「界面还原」最容易出错的地方，
    // 出问题时（比如列数恒为 1）靠肉眼从截图反推很不可靠，这里直接给出数值。
    tracing::debug!(
        cols = layout.cols,
        cell = layout.cell,
        content_w,
        groups = layout.groups.len(),
        total_h = layout.total_h,
        photos = photos.len(),
        "栅格布局"
    );

    // 滚动位置：显式 `restore_scroll` 优先（切视图/搜索要回顶）；
    // 否则若给了锚点 id，就把它滚到视口顶部——这就是「重载后用户还在看同一张」。
    // 注意 O(n) 的 position 只在锚定这一帧跑一次，滚动时 `anchor_id` 恒为 None，不产生每帧开销。
    let anchor_off = anchor_id.and_then(|id| {
        let idx = photos.iter().position(|p| p.id == id)?;
        layout
            .thumb_rect(idx)
            .map(|r| (r.top() - Metrics::GRID_PAD).max(0.0))
    });
    let scroll_off = restore_scroll.or(anchor_off);

    let mut area = ScrollArea::vertical()
        .id_salt("grid-scroll")
        .auto_shrink([false, false]);
    if let Some(off) = scroll_off {
        area = area.vertical_scroll_offset(off);
    }

    let mut budget = Metrics::TEX_UPLOAD_PER_FRAME;

    let area_out = area.show_viewport(ui, |ui, viewport| {
        ui.set_height(layout.total_h);
        let origin = ui.max_rect().left_top();
        let clip = ui.clip_rect();
        let painter = ui.painter_at(clip);
        let (first, last) = layout.visible_index_range(viewport);
        out.visible = last.saturating_sub(first);
        out.top_index = first.min(photos.len());

        // ---- 普通分组头：画在各自的位置上（吸顶头是「滚出去之后」的替身，两者互斥）----
        if layout.has_headers {
            for gi in layout.groups_in_range(first, last) {
                let g = &layout.groups[gi];
                let bar = Rect::from_min_max(
                    pos2(clip.left(), origin.y + g.y),
                    pos2(clip.right(), origin.y + g.y + Metrics::GROUP_HEADER_H),
                );
                if bar.bottom() <= clip.top() + 1.0 {
                    continue; // 已经滚出视口顶部，交给吸顶头
                }
                if bar.top() >= clip.bottom() {
                    break;
                }
                draw_group_header(&painter, bar, &g.title, theme);
            }
        }

        for index in first..last {
            let Some(row) = photos.get(index) else {
                continue;
            };
            let Some(content_rect) = layout.thumb_rect(index) else {
                continue;
            };
            let rect = content_rect.translate(origin.to_vec2());
            if !clip.intersects(rect.expand(2.0)) {
                continue;
            }
            let is_sel = selection.contains(&row.id);
            let is_current = current_index == Some(index);

            // ---- 宽高空着的，报给回填服务（M2.1 B「可见区域按需回填」）----
            // 放在占位分支**之前**：占位格是「缩略图还没好」，与「宽高还不知道」
            // 是两件事——一张图可能缩略图早就有了、宽高却因为当年盘没插而一直缺着。
            if (row.width.is_none() || row.height.is_none()) && out.need_dims.len() < NEED_DIMS_MAX {
                out.need_dims.push(row.id);
            }

            // ---- 单元格背景（U-24：1px 浅灰框，不是投影）----
            painter.rect_filled(rect, 1.0, Color32::WHITE);

            // ---- 缩略图 / 占位格（`04` §3.2.1，P-06）----
            //
            // 未就绪就画**灰底「待生成」**，不隐藏、不放低清预览：
            // 隐藏会让栅格在索引过程中不断重排，用户正看的那张图会随缩略图完成而位移。
            let failed = row.thumb_state == 2 || row.thumb_state == 3;
            let placeholder =
                failed || (row.thumb_state == 0 && indexing_folders.contains(&row.folder_id));
            if placeholder {
                index_progress::draw_placeholder(&painter, rect, theme, failed);
            } else {
                match cache.get(&ctx, db, service, row, &mut budget) {
                    TexState::Ready(tex_id, tex_size) => {
                        let fitted = fit_rect(rect, tex_size);
                        painter.image(
                            tex_id,
                            fitted,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    TexState::Pending => {
                        index_progress::draw_placeholder(&painter, rect, theme, false);
                    }
                    TexState::Failed => {
                        index_progress::draw_placeholder(&painter, rect, theme, true);
                    }
                }
            }

            if placeholder {
                // 占位格：**不可点开、不参与选中**、右键不弹依赖缩略图的菜单。
                // 只留一个普通描边，让"这是图、但还没做好"一眼可辨。
                painter.rect_stroke(
                    rect,
                    1.0,
                    Stroke::new(1.0, theme.cell_border),
                    StrokeKind::Inside,
                );
            } else {
                // ---- 描边：选中 > 当前 > 悬停 > 常态 ----
                let response = interact_cell(ui, rect, row.id, &painter, theme);
                let hovered = response.hovered();
                let stroke = if is_sel {
                    Stroke::new(2.0, theme.selected)
                } else if is_current {
                    Stroke::new(1.0, theme.selected)
                } else if hovered {
                    Stroke::new(1.0, theme.selected.gamma_multiply(0.6))
                } else {
                    Stroke::new(1.0, theme.cell_border)
                };
                painter.rect_stroke(rect, 1.0, stroke, StrokeKind::Inside);

                if hovered {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }

                // ---- 交互 ----
                if response.double_clicked() {
                    out.opened = Some(index);
                } else if response.clicked() {
                    let mods = ui.input(|i| i.modifiers);
                    out.clicked = Some(ClickInfo {
                        index,
                        additive: mods.command || mods.ctrl,
                        range: mods.shift,
                    });
                }
                if response.secondary_clicked() {
                    out.context = Some((
                        index,
                        response.interact_pointer_pos().unwrap_or(rect.center()),
                    ));
                }
            }

            // ---- 文件名（U-26：大档才显示，单行省略）----
            if let Some(fr) = layout.filename_rect(index) {
                let fr = fr.translate(origin.to_vec2());
                let font = FontId::new(11.0, egui::FontFamily::Proportional);
                let max_chars = ((fr.width() / 6.4).floor() as usize).max(4);
                let text = ellipsize(&row.name, max_chars);
                painter.text(
                    fr.center(),
                    Align2::CENTER_CENTER,
                    text,
                    font,
                    theme.text_secondary,
                );
            }
        }

        // ---- 吸顶分组头：**必须最后画**（否则被单元格盖住，02 §8.2.3）----
        if let Some(gi) = layout.sticky_group(viewport.min.y) {
            let g = &layout.groups[gi];
            let bar = Rect::from_min_max(
                pos2(clip.left(), clip.top()),
                pos2(clip.right(), clip.top() + Metrics::GROUP_HEADER_H),
            );
            draw_group_header(&painter, bar, &g.title, theme);
        }
    });

    out.scroll_y = area_out.state.clamped_offset().y;

    // 滚动条的空白处点击 = 取消选择（Picasa 的行为）
    if let Some(pos) = ui.input(|i| {
        i.pointer
            .primary_clicked()
            .then(|| i.pointer.interact_pos())
            .flatten()
    }) && full.contains(pos)
        && out.clicked.is_none()
        && out.opened.is_none()
    {
        // 交给上层：点击空白清空选择
        out.clicked = None;
    }

    out
}

/// 画一条分组头（普通位置与吸顶位置共用）。
fn draw_group_header(painter: &egui::Painter, bar: Rect, title: &str, theme: &Theme) {
    painter.rect_filled(bar, 0.0, theme.header_bg);
    painter.line_segment(
        [bar.left_bottom(), bar.right_bottom()],
        Stroke::new(1.0, theme.separator),
    );
    painter.text(
        pos2(bar.left() + Metrics::GRID_PAD, bar.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::new(12.0, egui::FontFamily::Proportional),
        theme.header_text,
    );
}

/// 单元格交互：**用固定 id（照片 id）保证跨帧稳定**，否则 hover 会闪烁。
fn interact_cell(
    ui: &mut Ui,
    rect: Rect,
    photo_id: i64,
    painter: &egui::Painter,
    theme: &Theme,
) -> Response {
    let id = ui.scope_id().with(("cell", photo_id));
    let resp = ui.interact(rect, id, Sense::click());
    if resp.hovered() {
        painter.rect_stroke(
            rect,
            1.0,
            Stroke::new(1.0, theme.selected.gamma_multiply(0.5)),
            StrokeKind::Inside,
        );
    }
    resp
}

fn draw_empty_state(ui: &mut Ui, full: Rect, theme: &Theme, cache: &mut TextureCache) {
    let _ = cache;
    let painter = ui.painter_at(full);
    painter.text(
        pos2(full.center().x, full.center().y - 18.0),
        Align2::CENTER_CENTER,
        "这里还没有照片",
        FontId::new(15.0, egui::FontFamily::Proportional),
        theme.text_secondary,
    );
    painter.text(
        pos2(full.center().x, full.center().y + 6.0),
        Align2::CENTER_CENTER,
        "点上方「添加文件夹」，选一个放照片的目录（我们只读，不改你的文件）",
        FontId::new(12.0, egui::FontFamily::Proportional),
        theme.text_secondary.gamma_multiply(0.85),
    );
}

/// 等比放进正方形（U-25：长边贴合，短边留白，不裁切不拉伸）。
fn fit_rect(outer: Rect, tex: Vec2) -> Rect {
    if tex.x <= 0.0 || tex.y <= 0.0 {
        return outer;
    }
    let scale = (outer.width() / tex.x).min(outer.height() / tex.y);
    let size = vec2(tex.x * scale, tex.y * scale);
    Rect::from_center_size(outer.center(), size)
}

pub fn ellipsize(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// 工具栏/侧栏共用的「分隔线」。
pub fn separator_v(painter: &egui::Painter, x: f32, top: f32, bottom: f32, theme: &Theme) {
    painter.line_segment(
        [pos2(x, top), pos2(x, bottom)],
        Stroke::new(1.0, theme.separator),
    );
}

/// 小工具：画一个「禁用态」的图标（灰）。
pub fn draw_icon(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32) {
    icons::draw(painter, icon, rect, color, 1.4);
}

#[cfg(test)]
mod tests {
    use super::*;
    use picasa_db::PhotoRow;

    fn photo(id: i64, taken_at: i64) -> PhotoRow {
        PhotoRow {
            id,
            folder_id: 1,
            folder_path: "/tmp/photos".into(),
            name: format!("IMG_{id:04}.jpg"),
            size_bytes: 1024,
            mtime_ns: taken_at * 1_000_000_000,
            width: Some(4000),
            height: Some(3000),
            orientation: Some(1),
            taken_at: Some(taken_at),
            taken_at_src: 0,
            thumb_state: 1,
            decoder_ver: 0,
            // 与 v3 生成列同口径：有 taken_at 就用它
            eff_taken_at: taken_at,
        }
    }

    /// 造 n 张「同一天」的照片（同一个分组）。
    fn same_day(n: usize) -> Vec<PhotoRow> {
        (1..=n as i64)
            .map(|i| photo(i, 1_300_000_000 + i))
            .collect()
    }

    #[test]
    fn columns_are_derived_from_width_but_cell_size_is_fixed() {
        let photos = same_day(10);
        let a = GridLayout::build(&photos, 1000.0, Tier::Small, true);
        let b = GridLayout::build(&photos, 2000.0, Tier::Small, true);
        // U-21：单元格边长**不随窗口变化**
        assert_eq!(a.cell, Metrics::CELL_SIZES[0]);
        assert_eq!(b.cell, Metrics::CELL_SIZES[0]);
        // 宽度变了只影响列数
        assert!(b.cols > a.cols);
        // U-22：间距也是常量
        assert_eq!(Metrics::CELL_GAP, 10.0);
    }

    #[test]
    fn three_tiers_have_fixed_pixel_sizes() {
        let photos = same_day(3);
        for (i, expected) in Metrics::CELL_SIZES.iter().enumerate() {
            let layout = GridLayout::build(&photos, 1200.0, Tier::from_index(i), true);
            assert_eq!(layout.cell, *expected, "第 {i} 档边长");
        }
    }

    #[test]
    fn first_cell_sits_after_padding_and_header() {
        let photos = same_day(4);
        let layout = GridLayout::build(&photos, 1000.0, Tier::Medium, true);
        let r = layout.thumb_rect(0).unwrap();
        assert_eq!(r.left(), Metrics::GRID_PAD);
        assert_eq!(r.top(), Metrics::GRID_PAD + Metrics::GROUP_HEADER_H);
        assert_eq!(r.width(), Metrics::CELL_SIZES[1]);
    }

    #[test]
    fn cells_are_laid_out_in_rows() {
        let photos = same_day(40);
        let layout = GridLayout::build(&photos, 1000.0, Tier::Small, true);
        let cols = layout.cols;
        assert!(cols >= 3);
        let r0 = layout.thumb_rect(0).unwrap();
        let r1 = layout.thumb_rect(1).unwrap();
        assert_eq!(r1.top(), r0.top(), "同一行");
        assert_eq!(
            r1.left(),
            r0.left() + Metrics::CELL_SIZES[0] + Metrics::CELL_GAP
        );

        // 第 cols 张必然是第二行的第一格
        let second_row = layout.thumb_rect(cols).unwrap();
        assert_eq!(second_row.left(), r0.left());
        assert_eq!(
            second_row.top(),
            r0.top() + Metrics::CELL_SIZES[0] + Metrics::CELL_GAP,
            "第二行应正好下移一个行距（小档不显示文件名）"
        );
    }

    #[test]
    fn total_height_grows_with_photos() {
        let small = GridLayout::build(&same_day(3), 1000.0, Tier::Small, true);
        let big = GridLayout::build(&same_day(300), 1000.0, Tier::Small, true);
        assert!(big.total_h > small.total_h);
        assert!(small.total_h > 0.0);
    }

    #[test]
    fn visible_range_is_bounded_and_covers_viewport() {
        let photos = same_day(1000);
        let layout = GridLayout::build(&photos, 1000.0, Tier::Small, true);
        let viewport = Rect::from_min_size(pos2(0.0, 500.0), vec2(1000.0, 600.0));
        let (first, last) = layout.visible_index_range(viewport);
        assert!(first < last);
        // **核心性能断言**：只画视口附近的一小部分，而不是 1000 张
        assert!(last - first < 120, "可见区间过大：{}", last - first);
        // 视口内的第一个单元格必须落在区间里
        let mut covered = false;
        for i in first..last {
            if let Some(r) = layout.thumb_rect(i)
                && r.intersects(viewport)
            {
                covered = true;
                break;
            }
        }
        assert!(covered, "可见区间没有覆盖视口内的任何单元格");
    }

    #[test]
    fn sticky_header_appears_only_after_scrolling_past_it() {
        let photos = same_day(50);
        let layout = GridLayout::build(&photos, 1000.0, Tier::Small, true);
        // 还没滚：分组头就在正常位置，不需要吸顶补画
        assert_eq!(layout.sticky_group(0.0), None);
        // 滚过分组头之后：需要吸顶
        assert_eq!(layout.sticky_group(Metrics::GROUP_HEADER_H + 5.0), Some(0));
        // 关掉分组时不吸顶
        let plain = GridLayout::build(&photos, 1000.0, Tier::Small, false);
        assert_eq!(plain.sticky_group(100.0), None);
    }

    #[test]
    fn huge_single_group_stays_virtualized() {
        // 「同一天连拍 3000 张」是最坏情况：只有一个分组，按分组边界返回就会整组返回
        let photos = same_day(3000);
        let layout = GridLayout::build(&photos, 1000.0, Tier::Small, true);
        assert_eq!(layout.groups.len(), 1, "构造前提：只有一个分组");

        let mid = Rect::from_min_size(pos2(0.0, 5000.0), vec2(1000.0, 800.0));
        let (first, last) = layout.visible_index_range(mid);
        assert!(last - first < 200, "单组过大时虚拟化失效：{}", last - first);

        // 区间两端的单元格必须真的落在视口附近（不能是「返回了但没覆盖」）
        for i in [first, last - 1] {
            let r = layout.thumb_rect(i).unwrap();
            assert!(
                r.bottom() >= mid.min.y - 300.0 && r.top() <= mid.max.y + 300.0,
                "索引 {i} 不在视口附近：{r:?}"
            );
        }
    }

    #[test]
    fn viewport_below_content_yields_empty_range() {
        let photos = same_day(20);
        let layout = GridLayout::build(&photos, 1000.0, Tier::Small, true);
        let below = Rect::from_min_size(pos2(0.0, layout.total_h + 500.0), vec2(1000.0, 400.0));
        let (first, last) = layout.visible_index_range(below);
        assert_eq!(first, last, "视口在内容下方时应返回空区间");
    }

    #[test]
    fn groups_in_range_covers_every_visible_group() {
        // 三天、每天 5 张：分组边界是 0/5/10
        let photos: Vec<PhotoRow> = (1..=15i64)
            .map(|i| photo(i, 1_300_000_000 + ((i - 1) / 5) * 86_400))
            .collect();
        let layout = GridLayout::build(&photos, 1000.0, Tier::Small, true);
        assert_eq!(layout.groups.len(), 3, "应分成三天");
        assert_eq!(layout.groups_in_range(0, 5), 0..1);
        assert_eq!(layout.groups_in_range(0, 15), 0..3);
        assert_eq!(layout.groups_in_range(7, 12), 1..3);
        // 空区间不该产出任何分组
        assert_eq!(layout.groups_in_range(4, 4), 0..0);
    }

    #[test]
    fn hit_testing_round_trips() {
        let photos = same_day(20);
        let layout = GridLayout::build(&photos, 1000.0, Tier::Medium, true);
        for index in [0usize, 3, 7, 19] {
            let r = layout.thumb_rect(index).unwrap();
            assert_eq!(
                layout.index_at(r.center()),
                Some(index),
                "索引 {index} 的命中测试不一致"
            );
        }
    }

    #[test]
    fn empty_view_has_zero_height_and_no_cells() {
        let layout = GridLayout::build(&[], 1000.0, Tier::Small, true);
        assert_eq!(layout.groups.len(), 0);
        assert_eq!(layout.thumb_rect(0), None);
        assert!(layout.total_h >= 0.0);
    }

    #[test]
    fn ellipsize_keeps_short_names_and_truncates_long_ones() {
        assert_eq!(ellipsize("IMG_0001.jpg", 20), "IMG_0001.jpg");
        let long = ellipsize("苏州行_2011年5月13日_非常长的文件名.jpg", 10);
        assert_eq!(long.chars().count(), 10);
        assert!(long.ends_with('…'));
    }
}
