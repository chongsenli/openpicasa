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
use picasa_core::thumbs::JobKind;
use picasa_core::{IndexSnapshot, ThumbJob, ThumbService, ViewMode, library};
use picasa_db::{Db, PhotoRow};

use crate::icons::{self, Icon};
use crate::index_progress;
use crate::textures::{TexState, TextureCache};
use crate::theme::{Metrics, Theme};

/// 每帧最多往图集里装几张（`07-A` §A-2.2 的「批量构建」）。
///
/// 32 × 8 = 256 是实测折中：每张要「解 T0 JPEG → 缩到 32² → set_partial」，
/// 单张约 0.1 ms，8 张 ≈ 0.8 ms，在 16.7 ms 的帧预算里可以接受；
/// 一次装 32 张就会吃掉 3 ms 以上，开始在弱机上掉帧。
const ATLAS_UPLOADS_PER_FRAME: usize = 8;

/// masonry 模式下 `cell_px` 的实际下限。滑块仍显示 22～512，但瀑布流把列宽/行高
/// 钳到 ≥ 64：低于这个值一张图不足几像素，且几千列会让每帧的布局与绘制都失控。
const MASONRY_MIN_CELL: f32 = 64.0;

/// masonry 视口裁剪桶的粒度（每多少个索引一个桶）。
/// 128 张 × ~200px ≈ 2.6 万 px 的主轴跨度，一次二分后只需扫描 1～2 个桶。
const MASONRY_BUCKET: usize = 128;

/// 一个分组（日期）在内容坐标里的位置。
#[derive(Clone, Debug)]
pub struct Group {
    pub title: String,
    pub y: f32,
    pub first: usize,
    pub count: usize,
}

/// 横向瀑布流的行数。
///
/// 两个约束同时成立才不会出现「右侧大片空白、下面一条几乎不能滚的横条」：
///
/// 1. 行数不能多于视口高度塞得下的行（这个模式只做横向滚动，超高的行会被裁掉）；
/// 2. 在此之上尽量少分行，让每一行的平均宽度仍 ≥ 视口宽度。照片不够铺满时退回 1 行。
fn masonry_h_rows(viewport_w: f32, viewport_h: f32, slot_h: f32, total_w: f32) -> usize {
    let gap = Metrics::CELL_GAP;
    let pad = Metrics::GRID_PAD;
    let mut rows_fit = 1usize;
    loop {
        let next = rows_fit + 1;
        if next > 64 {
            break;
        }
        let h = pad * 2.0 + next as f32 * slot_h + (next - 1) as f32 * gap;
        if h > viewport_h + 0.5 {
            break;
        }
        rows_fit = next;
    }
    let target = (viewport_w - 2.0 * pad).max(slot_h);
    let rows_fill = (total_w / target).floor() as usize;
    if rows_fill == 0 {
        1
    } else {
        rows_fill.min(rows_fit).max(1)
    }
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
    /// 视图模式。`MasonryV`/`MasonryH` 下坐标由 `masonry_rects` 决定，
    /// `thumb_rect` 走瀑布流坐标而不是等宽方格坐标。
    pub mode: ViewMode,
    /// masonry 模式下每个索引的（x, y, w, h）——**内容坐标**，按索引升序。
    /// `Grid` 模式为空（走方格算式）。
    masonry: Vec<egui::Vec2>,
    /// masonry 模式下每个索引的 rect 起点（x, y）。与 `masonry` 尺寸一一对应。
    masonry_pos: Vec<egui::Vec2>,
    /// masonry 视口裁剪桶：每 `MASONRY_BUCKET` 个索引记录主轴坐标（V=y / H=x）
    /// 的 `(min, max)`。贪心放置的坐标**不随索引单调**，不能直接二分；
    /// 用桶把线性扫描限制在命中的少数桶里，保持虚拟化成立。
    masonry_buckets: Vec<(f32, f32)>,
}

impl GridLayout {
    pub fn build(
        photos: &[PhotoRow],
        width: f32,
        cell_px: f32,
        has_headers: bool,
        mode: ViewMode,
    ) -> Self {
        // 只有一个尺寸时，横向模式把宽高都当成这个数（单测入口）。
        // 真正的视口走 [`Self::build_in_viewport`]。
        Self::build_in_viewport(photos, width, width, cell_px, has_headers, mode)
    }

    /// `viewport_w` 是可用宽度，`viewport_h` 是可用高度。
    ///
    /// 方格 / 纵向只用宽度。横向瀑布流两个都要：行数按「铺满宽度、且不高出视口」来定，
    /// 不能拿高度当宽度去排成一窄条竖列。
    pub fn build_in_viewport(
        photos: &[PhotoRow],
        viewport_w: f32,
        viewport_h: f32,
        cell_px: f32,
        has_headers: bool,
        mode: ViewMode,
    ) -> Self {
        Self::build_in_viewport_opts(
            photos,
            viewport_w,
            viewport_h,
            cell_px,
            has_headers,
            mode,
            true,
        )
    }

    /// 与 [`Self::build_in_viewport`] 相同，但方格模式可以关掉文件名行
    /// （§7.4「显示文件名」可选项）。瀑布流保持既有行为。
    pub fn build_in_viewport_opts(
        photos: &[PhotoRow],
        viewport_w: f32,
        viewport_h: f32,
        cell_px: f32,
        has_headers: bool,
        mode: ViewMode,
        show_filename: bool,
    ) -> Self {
        // masonry 下 `cell_px` 语义变为「目标列宽/行高基准」，下限拉到可读的最小值，
        // 避免拖到 22px 时几千列/行直接卡死。
        let cell = if mode == ViewMode::Grid {
            cell_px
        } else {
            cell_px.max(MASONRY_MIN_CELL)
        };
        // 方格文件名是 §7.4 的可选项；瀑布流仍显示（宽高比布局的既有占位）。
        let show_filename = show_filename && mode == ViewMode::Grid;
        let cell_h = cell
            + if show_filename {
                Metrics::FILENAME_H
            } else {
                0.0
            };

        let mut layout = if mode == ViewMode::Grid {
            Self::build_grid(photos, viewport_w, cell, cell_h, has_headers, show_filename)
        } else {
            Self::build_masonry(photos, viewport_w, viewport_h, cell, mode)
        };
        layout.mode = mode;
        layout
    }

    /// 默认等宽等高方格布局（原 `build` 主体）。
    fn build_grid(
        photos: &[PhotoRow],
        width: f32,
        cell: f32,
        cell_h: f32,
        has_headers: bool,
        show_filename: bool,
    ) -> Self {
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
            mode: ViewMode::Grid,
            masonry: Vec::new(),
            masonry_pos: Vec::new(),
            masonry_buckets: Vec::new(),
        }
    }

    /// masonry 瀑布流布局（M3 追加）。
    ///
    /// - `MasonryV`：列宽 = `cell`，图片按真实宽高比缩放到列宽，放进**最矮的列**；纵向滚动。
    /// - `MasonryH`：行高 = `cell`，图片按真实宽高比缩放到行高，放进**最窄的行**；横向滚动。
    ///   行数取「平均行宽仍能铺满视口宽度」与「塞进视口高度」的较小值，避免小图库被排成窄竖条。
    ///
    /// 宽高缺失（`width/height` 为 `None`）按 1:1 占位；DimsService 回填后布局重排。
    /// 分组头不画（贪心坐标无法对齐行边界，见 `ViewMode` 文档），`groups` 只放一个
    /// 覆盖全量的哑分组让 `group_of`/`groups_in_range` 继续工作。
    fn build_masonry(
        photos: &[PhotoRow],
        viewport_w: f32,
        viewport_h: f32,
        cell: f32,
        mode: ViewMode,
    ) -> Self {
        let gap = Metrics::CELL_GAP;
        let pad = Metrics::GRID_PAD;

        let mut sizes = Vec::with_capacity(photos.len());
        let mut positions = Vec::with_capacity(photos.len());

        let (total_w, total_h) = match mode {
            ViewMode::MasonryV => {
                let width = viewport_w;
                let usable = (width - 2.0 * pad).max(cell);
                let cols = (((usable + gap) / (cell + gap)).floor() as usize).max(1);
                let col_w = ((usable - gap * (cols as f32 - 1.0)) / cols as f32).max(cell * 0.5);
                let mut col_y = vec![pad; cols];
                for row in photos {
                    let ratio = match (row.width, row.height) {
                        (Some(w), Some(h)) if w > 0 && h > 0 => w as f32 / h as f32,
                        _ => 1.0,
                    };
                    let h = (col_w / ratio).clamp(cell * 0.3, col_w * 4.0) + Metrics::FILENAME_H; // 文件名行跟在图下方，占位防重叠
                    // 最矮列贪心放置
                    let (ci, y) = col_y
                        .iter()
                        .enumerate()
                        .min_by(|a, b| a.1.total_cmp(b.1).then(a.0.cmp(&b.0)))
                        .map(|(i, y)| (i, *y))
                        .unwrap_or((0, pad));
                    let x = pad + ci as f32 * (col_w + gap);
                    positions.push(egui::vec2(x, y));
                    sizes.push(egui::vec2(col_w, h));
                    col_y[ci] = y + h + gap;
                }
                let bottom = col_y.iter().copied().fold(pad, f32::max);
                (width, bottom + pad)
            }
            ViewMode::MasonryH => {
                // 图片行高 = cell；文件名带跟在图下方（与纵向瀑布流同一占位），
                // 否则文件名会画进图片底边里。
                let img_h = cell;
                let slot_h = img_h + Metrics::FILENAME_H;
                let widths: Vec<f32> = photos
                    .iter()
                    .map(|row| {
                        let ratio = match (row.width, row.height) {
                            (Some(w), Some(h)) if w > 0 && h > 0 => w as f32 / h as f32,
                            _ => 1.0,
                        };
                        (img_h * ratio).clamp(img_h * 0.3, img_h * 8.0)
                    })
                    .collect();
                let total_w: f32 = widths.iter().map(|w| w + gap).sum();
                let rows = masonry_h_rows(viewport_w, viewport_h, slot_h, total_w);
                let mut row_x = vec![pad; rows];
                for w in widths {
                    let (ri, x) = row_x
                        .iter()
                        .enumerate()
                        .min_by(|a, b| a.1.total_cmp(b.1).then(a.0.cmp(&b.0)))
                        .map(|(i, x)| (i, *x))
                        .unwrap_or((0, pad));
                    let y = pad + ri as f32 * (slot_h + gap);
                    positions.push(egui::vec2(x, y));
                    sizes.push(egui::vec2(w, slot_h));
                    row_x[ri] = x + w + gap;
                }
                let right = row_x.iter().copied().fold(pad, f32::max);
                let content_h =
                    pad * 2.0 + rows as f32 * slot_h + (rows.saturating_sub(1)) as f32 * gap;
                (right + pad, content_h)
            }
            ViewMode::Grid => unreachable!("build_masonry 只处理瀑布流模式"),
        };

        // 视口裁剪桶：按主轴（V=y / H=x）记录每 128 个索引的 (min, max)。
        let vertical = mode == ViewMode::MasonryV;
        let mut buckets: Vec<(f32, f32)> = Vec::new();
        for chunk in positions.chunks(MASONRY_BUCKET) {
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for p in chunk {
                let v = if vertical { p.y } else { p.x };
                lo = lo.min(v);
                hi = hi.max(v);
            }
            buckets.push((lo, hi));
        }

        Self {
            cell,
            cell_h: cell,
            cols: 1,
            groups: vec![Group {
                title: String::new(),
                y: 0.0,
                first: 0,
                count: photos.len(),
            }],
            total_h,
            content_w: total_w,
            show_filename: true,
            has_headers: false,
            mode,
            masonry: sizes,
            masonry_pos: positions,
            masonry_buckets: buckets,
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
        if self.mode != ViewMode::Grid {
            // masonry：坐标在 build 时已经算好，直接查表。
            let size = self.masonry.get(index).copied()?;
            let p = self.masonry_pos.get(index).copied()?;
            return Some(Rect::from_min_size(pos2(p.x, p.y), size));
        }
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
        if self.mode == ViewMode::Grid {
            // 方格：文件名画在格子**下方**的独立行（行高已计入 cell_h，不会重叠）
            Some(Rect::from_min_size(
                pos2(r.left(), r.bottom() + 1.0),
                vec2(self.cell, Metrics::FILENAME_H),
            ))
        } else {
            // masonry：rect 已包含文件名带（build 时高度 +FILENAME_H），图片填满
            // 上部的 img_rect，文件名必须画在**格子内部**的底部带里——画到
            // `rect.bottom()` 之外会落进 gap / 下一张图的格子上，被盖住。
            Some(Rect::from_min_size(
                pos2(r.left(), r.bottom() - Metrics::FILENAME_H),
                vec2(r.width(), Metrics::FILENAME_H),
            ))
        }
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
        if self.mode != ViewMode::Grid {
            return self.masonry_visible_range(viewport, n);
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

    /// masonry 的视口裁剪：先二分命中裁剪桶，再只扫命中桶里的索引。
    /// 精确（逐 rect 相交判断），且代价 O(log 桶数 + 可见数)。
    fn masonry_visible_range(&self, viewport: Rect, n: usize) -> (usize, usize) {
        if n == 0 || self.masonry_buckets.is_empty() {
            return (0, 0);
        }
        let vertical = self.mode == ViewMode::MasonryV;
        let (vlo, vhi) = if vertical {
            (viewport.min.y, viewport.max.y)
        } else {
            (viewport.min.x, viewport.max.x)
        };
        let mut first = n;
        let mut last = 0usize;
        for (bi, (lo, hi)) in self.masonry_buckets.iter().enumerate() {
            // 桶与视口在主轴上不相交 → 跳过
            if *hi < vlo - 2.0 || *lo > vhi + 2.0 {
                continue;
            }
            let start = bi * MASONRY_BUCKET;
            let end = (start + MASONRY_BUCKET).min(n);
            for i in start..end {
                let Some(size) = self.masonry.get(i) else {
                    break;
                };
                let Some(p) = self.masonry_pos.get(i) else {
                    break;
                };
                let (a, b) = if vertical {
                    (p.y, p.y + size.y)
                } else {
                    (p.x, p.x + size.x)
                };
                if b >= vlo && a <= vhi {
                    first = first.min(i);
                    last = last.max(i + 1);
                }
            }
        }
        if first >= last {
            (n, n)
        } else {
            (first.min(n), last.min(n))
        }
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
        if self.mode != ViewMode::Grid {
            // masonry：坐标无规律，只能按桶缩小范围后逐格判（桶是主轴有序的近似，
            // 实际直接线性扫——命中测试只在点击时调用一次，不是热路径）。
            for (i, (size, p)) in self.masonry.iter().zip(&self.masonry_pos).enumerate() {
                let r = Rect::from_min_size(pos2(p.x, p.y), *size);
                if r.contains(pos) {
                    return Some(i);
                }
            }
            return None;
        }
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
    /// M3 ZOOM-01：连续滑块值（22～512），取代离散 `tier`。用 bits 存，
    /// 拖 1px 就换 key → 布局实时重排。
    cell_px_bits: u32,
    headers: bool,
    /// 视图模式（方格 / 纵向瀑布流 / 横向瀑布流）。
    mode: u8,
    /// 方格模式是否显示文件名（§7.4 可选项）。切换时必须全量重排：
    /// 文件名行是否占高会改变每行高度。
    show_filename: bool,
    /// 横向瀑布流的视口高度。行数由它和宽度一起决定，高度变了必须重排。
    cross_bits: u32,
    /// 宽高签名：masonry 的坐标依赖每张图的宽高，DimsService 回填后必须重排。
    /// 把「可见宽高是否为空」折进 8 位签名：任一格从 None → Some 都会换 key。
    /// （比每帧哈希全部宽高便宜得多，且 masonry 重排本来就是切模式/回填时才发生。）
    dims_sig: u64,
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
                mode: ViewMode::Grid,
                masonry: Vec::new(),
                masonry_pos: Vec::new(),
                masonry_buckets: Vec::new(),
            },
            key: LayoutKey {
                width_bits: 0,
                cell_px_bits: 0,
                headers: false,
                mode: 0,
                show_filename: true,
                cross_bits: 0,
                dims_sig: 0,
                first_id: 0,
                last_id: 0,
            },
            built: 0,
            ready: false,
        }
    }

    /// 取（必要时算）当前帧的布局。
    ///
    /// `cross` 是视口高度，只对横向瀑布流行数有意义。
    pub fn get(
        &mut self,
        photos: &[PhotoRow],
        width: f32,
        cross: f32,
        cell_px: f32,
        has_headers: bool,
        mode: ViewMode,
    ) -> &GridLayout {
        self.get_with_filename(photos, width, cross, cell_px, has_headers, mode, true)
    }

    /// 同 [`Self::get`]，但可关闭方格文件名行（§7.4）。
    #[allow(clippy::too_many_arguments)]
    pub fn get_with_filename(
        &mut self,
        photos: &[PhotoRow],
        width: f32,
        cross: f32,
        cell_px: f32,
        has_headers: bool,
        mode: ViewMode,
        show_filename: bool,
    ) -> &GridLayout {
        // 瀑布流始终显示文件名（布局器内部处理），这里只让方格开关参与缓存键。
        let show_filename = show_filename && mode == ViewMode::Grid;
        // masonry 坐标依赖宽高：把「可见宽高是否为空」折成签名。
        // 增量回填一张图（None → Some）就换签名 → 全量重排一次（回填是低频事件）。
        let dims_sig = if mode == ViewMode::Grid {
            0
        } else {
            photos
                .iter()
                .take(MASONRY_BUCKET * 8)
                .map(|p| match (p.width.is_some(), p.height.is_some()) {
                    (true, true) => 3u64,
                    (true, false) => 2,
                    (false, true) => 1,
                    (false, false) => 0,
                })
                .fold(0u64, |acc, v| (acc << 2) | v)
        };
        let key = LayoutKey {
            width_bits: width.to_bits(),
            cell_px_bits: cell_px.to_bits(),
            headers: has_headers,
            mode: mode as u8,
            show_filename,
            cross_bits: if mode == ViewMode::MasonryH {
                cross.to_bits()
            } else {
                0
            },
            dims_sig,
            first_id: photos.first().map(|p| p.id).unwrap_or(0),
            last_id: photos.last().map(|p| p.id).unwrap_or(0),
        };

        if self.ready {
            // ① 完全没变
            if key == self.key && photos.len() == self.built {
                return &self.layout;
            }
            // ② 前缀不变、尾部变长 → 增量追加（分页补齐）。
            //    仅方格模式支持增量：masonry 是贪心放置，中途插入会级联改坐标。
            let prefix_same = mode == ViewMode::Grid
                && key.width_bits == self.key.width_bits
                && key.cell_px_bits == self.key.cell_px_bits
                && key.headers == self.key.headers
                && key.show_filename == self.key.show_filename
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
        self.layout = GridLayout::build_in_viewport_opts(
            photos,
            width,
            cross,
            cell_px,
            has_headers,
            mode,
            show_filename,
        );
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
    has_headers: bool,
    // 视图模式（M3：方格 / 纵向瀑布流 / 横向瀑布流）
    mode: ViewMode,
    restore_scroll: Option<f32>,
    // 「重载后锚定」的目标照片 id（D 流式展示）。`None` = 不用锚定。
    anchor_id: Option<i64>,
    db: &Db,
    service: &ThumbService,
    cache: &mut TextureCache,
    grid: &mut GridCache,
    theme: &Theme,
    snapshot: &IndexSnapshot,
    // 微缩图集（`07-A` §A-2.2）：格宽 ≤ 48 px 时走它，而不是每格一张纹理
    atlas: &mut crate::atlas::ThumbAtlas,
    // 当前格宽（逻辑 px）——由滑块决定，用来判断走不走图集
    cell_px: f32,
    // 方格模式是否显示文件名（§7.4；瀑布流不受此开关影响）
    show_filename: bool,
    // 空列表时怎么写：图库是空的，还是搜索/筛选没有命中
    empty: EmptyReason,
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
        draw_empty_state(ui, full, theme, cache, empty);
        return out;
    }

    let ctx = ui.ctx().clone();
    // 走不走微缩图集（`07-A` §A-2.2）：≤ 48 逻辑 px 时走。
    // 判据与 `picasa_core::zoom::uses_atlas` 同一份——不能有两套阈值。
    // masonry 下不走进图集：瀑布流的格子是变高的，图集 32² 槽装不下。
    let use_atlas = mode == ViewMode::Grid && picasa_core::uses_atlas(cell_px);
    let avail_w = (ui.available_width() - Metrics::SCROLLBAR).max(120.0);
    // 横向瀑布流的行数同时看宽和高：宽决定要铺满，高决定最多能排几行。
    // 以前把高度当成唯一输入，小图库会被排成贴左的窄竖条，右侧大片空白。
    let avail_h = (ui.available_height() - Metrics::SCROLLBAR).max(120.0);
    // 每帧「算布局」在 M2 是 5 万张 110 ms 的开销，而输入几乎每帧都一样——
    // 交给 GridCache，滚动时这一行是 0 代价。
    let layout = grid.get_with_filename(
        photos,
        avail_w,
        avail_h,
        cell_px,
        has_headers,
        mode,
        show_filename,
    );
    let content_w = avail_w;
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
    let horizontal = mode == ViewMode::MasonryH;
    let anchor_off = anchor_id.and_then(|id| {
        let idx = photos.iter().position(|p| p.id == id)?;
        layout.thumb_rect(idx).map(|r| {
            // 锚定偏移跟主轴走：横向瀑布流锚 x，其余锚 y
            if horizontal {
                (r.left() - Metrics::GRID_PAD).max(0.0)
            } else {
                (r.top() - Metrics::GRID_PAD).max(0.0)
            }
        })
    });
    let scroll_off = restore_scroll.or(anchor_off);

    let mut area = if horizontal {
        ScrollArea::horizontal()
            .id_salt("grid-scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
    } else {
        ScrollArea::vertical()
            .id_salt("grid-scroll")
            .auto_shrink([false, false])
    };
    if let Some(off) = scroll_off {
        area = if horizontal {
            area.horizontal_scroll_offset(off)
        } else {
            area.vertical_scroll_offset(off)
        };
    }

    let mut budget = Metrics::TEX_UPLOAD_PER_FRAME;

    let area_out = area.show_viewport(ui, |ui, viewport| {
        if horizontal {
            // 横向瀑布流：内容尺寸 = 布局算出的 (content_w × total_h)
            ui.set_width(layout.content_w);
            ui.set_height(layout.total_h);
        } else {
            ui.set_height(layout.total_h);
        }
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
            // masonry 的 rect 含文件名行，图片画在去掉文件名行的内矩形里
            let img_rect = if mode == ViewMode::Grid {
                rect
            } else {
                Rect::from_min_max(
                    rect.left_top(),
                    pos2(rect.right(), rect.bottom() - Metrics::FILENAME_H),
                )
            };

            // ---- 宽高空着的，报给回填服务（M2.1 B「可见区域按需回填」）----
            // 放在占位分支**之前**：占位格是「缩略图还没好」，与「宽高还不知道」
            // 是两件事——一张图可能缩略图早就有了、宽高却因为当年盘没插而一直缺着。
            if (row.width.is_none() || row.height.is_none()) && out.need_dims.len() < NEED_DIMS_MAX
            {
                out.need_dims.push(row.id);
            }

            // ---- 单元格底（像素校准：定稿效果图方格无白框、无描边——
            // 白底照片的边界由**软阴影 + 圆角**表达；占位格仍走灰底）----
            // masonry 图片填满格子，行为不变。
            if mode != ViewMode::Grid {
                painter.rect_filled(rect, 1.0, theme.grid_bg);
            }

            // ---- 缩略图 / 占位格（`04` §3.2.1，P-06）----
            //
            // 未就绪就画**灰底「待生成」**，不隐藏、不放低清预览：
            // 隐藏会让栅格在索引过程中不断重排，用户正看的那张图会随缩略图完成而位移。
            let failed = row.thumb_state == 2 || row.thumb_state == 3;
            let placeholder =
                failed || (row.thumb_state == 0 && indexing_folders.contains(&row.folder_id));
            // 描边目标：方格 = 实际贴图区（Ready 时是 contain 后的 fitted），
            // masonry = 去掉文件名带的图区。默认 img_rect，Ready 分支里覆写。
            let mut frame = img_rect;
            let frame_radius = if mode == ViewMode::Grid {
                Metrics::THUMB_RADIUS
            } else {
                0.0
            };
            if placeholder {
                index_progress::draw_placeholder(&painter, img_rect, theme, failed);
            } else if use_atlas {
                // ---- 微缩图集路径（`07-A` §A-2.2，格宽 ≤ 48 px）
                //
                // 为什么不走 `cache.get`：那条路是「每张图一张 GPU 纹理」，
                // 17 000 个可见格就是 17 000 张纹理 → 直接撞 TextureBudget。
                // 图集把 16 384 张塞进一张 4096² 纹理，一帧最多 2 张图集。
                match draw_from_atlas(&painter, atlas, db, row, img_rect) {
                    true => {}
                    false => {
                        // 还没进图集：画占位，等它进来（不阻塞本帧）
                        index_progress::draw_placeholder(&painter, rect, theme, false);
                    }
                }
            } else {
                match cache.get(&ctx, db, service, row, &mut budget) {
                    TexState::Ready(tex_id, tex_size) => {
                        // masonry 的格子比例就是图片比例：直接填满，消除 contain
                        // 舍入造成的 1px 白边；方格仍用 contain（格子是正方形）。
                        let fitted = if mode == ViewMode::Grid {
                            fit_rect(img_rect, tex_size)
                        } else {
                            img_rect
                        };
                        frame = fitted;
                        if mode == ViewMode::Grid {
                            // 像素校准（§7.4 效果图）：软阴影 + **圆角贴图**，
                            // 不再画 1px 白框。圆角贴图走 RectShape 的 brush
                            // 纹理通路（epaint 0.37），代替无圆角的 painter.image。
                            painter
                                .add(theme.thumb_shadow.as_shape(fitted, Metrics::THUMB_RADIUS));
                            painter.add(
                                egui::epaint::RectShape::filled(
                                    fitted,
                                    Metrics::THUMB_RADIUS,
                                    Color32::WHITE,
                                )
                                .with_texture(
                                    tex_id,
                                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                ),
                            );
                        } else {
                            painter.image(
                                tex_id,
                                fitted,
                                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
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
                    img_rect,
                    frame_radius,
                    Stroke::new(1.0, theme.cell_border),
                    StrokeKind::Inside,
                );
            } else {
                // ---- 描边：选中 > 当前 > 悬停 > 常态 ----
                // 像素校准：方格常态**无描边**（阴影已表达边界）；masonry 保留 1px 灰框。
                let response = interact_cell(ui, rect, row.id, &painter, theme);
                let hovered = response.hovered();
                let stroke = if is_sel {
                    Stroke::new(2.0, theme.selected)
                } else if is_current {
                    Stroke::new(1.0, theme.selected)
                } else if hovered {
                    Stroke::new(1.0, theme.selected.gamma_multiply(0.6))
                } else if mode == ViewMode::Grid {
                    Stroke::NONE
                } else {
                    Stroke::new(1.0, theme.cell_border)
                };
                if stroke.color != Color32::TRANSPARENT {
                    painter.rect_stroke(frame, frame_radius, stroke, StrokeKind::Inside);
                }

                // ---- 选中角标（M32-UI-10：勾选标记，与托盘/星标角标不混形）----
                // 选中 = 左上角实心圆 + 白勾。托盘成员态、星标角标另在各自域实现，
                // 三者形状/位置在 `docs/10` M32-UI-10 登记为互不相同。
                if is_sel {
                    let c = pos2(rect.left() + 8.0, rect.top() + 8.0);
                    let r = 7.0;
                    painter.circle_filled(c, r, theme.selected);
                    let w = Stroke::new(1.8, Color32::WHITE);
                    let a = pos2(c.x - 3.2, c.y + 0.2);
                    let b = pos2(c.x - 1.0, c.y + 2.6);
                    let d = pos2(c.x + 3.4, c.y - 2.4);
                    painter.line_segment([a, b], w);
                    painter.line_segment([b, d], w);
                }

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

            // ---- 文件名（像素校准：定稿效果图 = 居中、近黑、比旧值大一号）----
            if let Some(fr) = layout.filename_rect(index) {
                let fr = fr.translate(origin.to_vec2());
                let font = FontId::new(Metrics::FILENAME_FONT_SIZE, egui::FontFamily::Proportional);
                // 宽度不能按「字符数 × 平均字宽」估：中文一个字约 11px、拉丁约 6px，
                // 用固定 6.4 会让「苏州行_2011年5月13日.jpg」这种名字在格子里溢出，
                // 反过来又让全是 ASCII 的名字被截得太短。这里用真实字形宽度量。
                let text = ellipsize_to_width(ui, &row.name, &font, fr.width());
                painter.text(
                    fr.center(),
                    Align2::CENTER_CENTER,
                    text,
                    font,
                    theme.text_primary,
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

/// 从图集里画一格。返回 `false` 表示这张还没进图集（调用方画占位）。
///
/// ## 为什么这里不「顺手装进去」
///
/// 装图集要 `set_partial`（一次 GPU 上传）。一帧传几十张就会把帧时间顶穿。
/// 所以装的动作在**预算化**的路径里做（[`pump_atlas_from_db`]，每帧限量），
/// 这里只负责「有就画、没有就算」——绘制路径必须是无副作用的。
fn draw_from_atlas(
    painter: &egui::Painter,
    atlas: &crate::atlas::ThumbAtlas,
    _db: &Db,
    row: &PhotoRow,
    rect: Rect,
) -> bool {
    let Some(slot) = atlas.get(row.id) else {
        return false;
    };
    let Some(tex) = atlas.texture_of(slot.page) else {
        return false;
    };
    painter.image(
        tex.id(),
        rect,
        Rect::from_min_max(slot.uv[0], slot.uv[1]),
        Color32::WHITE,
    );
    true
}

/// 每帧限量地把「可见但还没进图集」的缩略图装进去。
///
/// ## 为什么限量
///
/// 4096² 图集的一次 `set_partial` 虽然只传 4 KB，但**首次建页**要分配 64 MB。
/// 一帧装 100 张 = 400 KB 上传 + 可能的建页，还行；装 1000 张就会掉帧。
/// 取 [`ATLAS_UPLOADS_PER_FRAME`] 是与 `TEX_UPLOAD_PER_FRAME` 同样的折中。
///
/// ## 为什么从 DB 读而不是等 ThumbService
///
/// 22 px 档下缩略图早就生成过了（它们在 T0 档已经存在于 `thumbs` 表里），
/// 这里只是「降采样到 32² 再装进图集」。走 `ThumbService` 会重新排队解码原图，
/// 那比直接从 BLOB 解 512² 再缩到 32² 慢一个数量级，而且没必要。
/// 公开给 `app.rs` 的入口（`tick_atlas` 用）。
///
/// 存在的理由：`pump_atlas_from_db` 在栅格绘制路径里，而「装图集」必须
/// **在绘制之前**跑（否则本帧新装的图要等下一帧才显示）。
/// 两个调用点共用一个实现，避免两处各写一遍限量逻辑。
pub fn pump_atlas_public(
    ctx: &egui::Context,
    atlas: &mut crate::atlas::ThumbAtlas,
    db: &Db,
    service: &ThumbService,
    rows: &[&PhotoRow],
) -> usize {
    pump_atlas_from_db(ctx, atlas, db, service, rows)
}

fn pump_atlas_from_db(
    ctx: &egui::Context,
    atlas: &mut crate::atlas::ThumbAtlas,
    db: &Db,
    service: &ThumbService,
    rows: &[&PhotoRow],
) -> usize {
    use crate::atlas::SLOT_PX;
    let mut done = 0;
    for row in rows {
        if done >= ATLAS_UPLOADS_PER_FRAME {
            break;
        }
        if atlas.contains(row.id) {
            continue;
        }
        // 还没生成过缩略图：交给后台服务去生成（`request` 自带去重），
        // 下一帧这里就能从库里读到。早先直接 `continue` 掉，等于
        // 「没生成过的照片在 ≤48 px 档永远灰着」——大图路径没有这个洞。
        if row.thumb_state == 0 {
            service.request(ThumbJob {
                photo_id: row.id,
                path: row.abs_path(),
                kind: JobKind::Thumb,
            });
            continue;
        }
        if row.thumb_state != 1 {
            continue;
        }
        let Ok(Some(buf)) = db.get_thumb(row.id, 0) else {
            continue;
        };
        // 解 T0 → 缩到 SLOT_PX²
        let Ok(img) = image::load_from_memory(&buf.data) else {
            continue;
        };
        let small = img.resize_exact(
            SLOT_PX as u32,
            SLOT_PX as u32,
            image::imageops::FilterType::Triangle,
        );
        let rgba = small.to_rgba8();
        if atlas.insert(ctx, row.id, rgba.as_raw()).is_some() {
            done += 1;
        }
    }
    done
}

/// 栅格没有照片时该说什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyReason {
    /// 没有搜索、也没有筛选：图库本身是空的。
    Library,
    /// 搜索或筛选把结果收成了空。不要引导去添加文件夹。
    NoMatches,
}

/// 当前列表是不是被搜索或筛选收窄过。
///
/// 默认的 [`picasa_db::PhotoFilter`]（全空）不算筛选——那只是「没设条件」。
pub fn empty_reason(search: &str, filter: Option<&picasa_db::PhotoFilter>) -> EmptyReason {
    let searching = !search.trim().is_empty();
    let filtering = filter.is_some_and(|f| f != &picasa_db::PhotoFilter::default());
    if searching || filtering {
        EmptyReason::NoMatches
    } else {
        EmptyReason::Library
    }
}

/// 空图库第二行。M3.1 起工具栏上的「添加文件夹」按钮已移除，
/// 入口是顶栏「…」→「添加文件夹…」（与左栏空态同一条路径）。
const EMPTY_GRID_HINT: &str =
    "点顶栏「…」→「添加文件夹…」，选一个放照片的目录（只读，不改你的文件）";

const NO_MATCH_TITLE: &str = "没有符合条件的照片";
const NO_MATCH_HINT: &str = "当前搜索或筛选没有结果";

fn empty_grid_lines(reason: EmptyReason) -> (&'static str, &'static str) {
    match reason {
        EmptyReason::Library => ("这里还没有照片", EMPTY_GRID_HINT),
        EmptyReason::NoMatches => (NO_MATCH_TITLE, NO_MATCH_HINT),
    }
}

fn draw_empty_state(
    ui: &mut Ui,
    full: Rect,
    theme: &Theme,
    cache: &mut TextureCache,
    reason: EmptyReason,
) {
    let _ = cache;
    let (title, hint) = empty_grid_lines(reason);
    let painter = ui.painter_at(full);
    painter.text(
        pos2(full.center().x, full.center().y - 18.0),
        Align2::CENTER_CENTER,
        title,
        FontId::new(15.0, egui::FontFamily::Proportional),
        theme.text_secondary,
    );
    painter.text(
        pos2(full.center().x, full.center().y + 6.0),
        Align2::CENTER_CENTER,
        hint,
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

/// 按**真实字形宽度**截断，超出则加省略号（U-26 的「单行、超出省略号」）。
///
/// 为什么不复用 [`ellipsize`]：那个按字符数截，而这个名字行是**混排**的——
/// 中文一个字约等于 11px、拉丁字母约 6px、数字更窄。按字符数估会出现两种错：
/// 全中文的名字溢出格子（压到右边邻居），全 ASCII 的名字被截得莫名其妙地短。
/// 这里直接问 egui 要字形的实际宽度，代价是每个可见格子一次 `layout_no_wrap`，
/// 但**只有可见的几十个**（虚拟化已经把视口外的都裁掉了），实测可忽略。
pub fn ellipsize_to_width(ui: &Ui, s: &str, font: &FontId, max_w: f32) -> String {
    let measure = |t: &str| -> f32 {
        // `layout_no_wrap` 是 `&mut self`（会往字形缓存里插条目），
        // 所以走 `fonts_mut` 而不是 `fonts`。这不影响并发：egui 的字体是
        // 单线程 UI 内共享的，`fonts_mut` 只是把它标成可变借用。
        ui.fonts_mut(|f| {
            f.layout_no_wrap(t.to_owned(), font.clone(), Color32::WHITE)
                .rect
                .width()
        })
    };
    if measure(s) <= max_w {
        return s.to_string();
    }
    // 二分找「加上省略号后仍放得下」的最长前缀。
    // 线性往回一个字符一个字符试在最坏情况下要 layout 几十次，这里 log2 次就够。
    let chars: Vec<char> = s.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let mut probe: String = chars[..mid].iter().collect();
        probe.push('…');
        if measure(&probe) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    // 连一个字符都放不下时，至少留省略号，别返回空串（空串会让格子看起来是坏的）
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
            starred: false,
        }
    }

    /// 造 n 张「同一天」的照片（同一个分组）。
    fn same_day(n: usize) -> Vec<PhotoRow> {
        (1..=n as i64)
            .map(|i| photo(i, 1_300_000_000 + i))
            .collect()
    }

    #[test]
    fn empty_grid_hint_points_at_more_menu_not_removed_toolbar_button() {
        let (title, hint) = empty_grid_lines(EmptyReason::Library);
        assert_eq!(title, "这里还没有照片");
        assert!(hint.contains("顶栏"));
        assert!(hint.contains("「…」"));
        assert!(hint.contains("添加文件夹…"));
        assert!(!hint.contains("点上方"));
    }

    #[test]
    fn search_or_filter_with_no_hits_does_not_ask_to_add_a_folder() {
        assert_eq!(empty_reason("", None), EmptyReason::Library);
        assert_eq!(empty_reason("   ", None), EmptyReason::Library);
        assert_eq!(empty_reason("cat", None), EmptyReason::NoMatches);

        let untouched = picasa_db::PhotoFilter::default();
        assert_eq!(empty_reason("", Some(&untouched)), EmptyReason::Library);
        let starred = picasa_db::PhotoFilter {
            starred_only: true,
            ..picasa_db::PhotoFilter::default()
        };
        assert_eq!(empty_reason("", Some(&starred)), EmptyReason::NoMatches);
        assert_eq!(empty_reason("cat", Some(&starred)), EmptyReason::NoMatches);

        let (title, hint) = empty_grid_lines(EmptyReason::NoMatches);
        assert!(title.contains("没有"));
        assert!(!title.contains("添加文件夹"));
        assert!(!hint.contains("添加文件夹"));
        assert!(!hint.contains("点上方"));
    }

    #[test]
    fn columns_are_derived_from_width_but_cell_size_is_fixed() {
        let photos = same_day(10);
        let a = GridLayout::build(
            &photos,
            1000.0,
            Metrics::CELL_SIZES[0],
            true,
            ViewMode::Grid,
        );
        let b = GridLayout::build(
            &photos,
            2000.0,
            Metrics::CELL_SIZES[0],
            true,
            ViewMode::Grid,
        );
        // U-21：单元格边长**不随窗口变化**
        assert_eq!(a.cell, Metrics::CELL_SIZES[0]);
        assert_eq!(b.cell, Metrics::CELL_SIZES[0]);
        // 宽度变了只影响列数
        assert!(b.cols > a.cols);
        // U-22：间距也是常量——用「相邻两格左边缘之差 - 单元格边长」反推间距，
        // 两种窗口宽度下必须都等于当前主题常量（M3.2 定稿 16px，不写死数值）。
        let pitch_a = a.thumb_rect(1).unwrap().left() - a.thumb_rect(0).unwrap().left();
        let pitch_b = b.thumb_rect(1).unwrap().left() - b.thumb_rect(0).unwrap().left();
        assert_eq!(pitch_a - a.cell, Metrics::CELL_GAP);
        assert_eq!(pitch_b - b.cell, Metrics::CELL_GAP);
    }

    #[test]
    fn three_tiers_have_fixed_pixel_sizes() {
        let photos = same_day(3);
        for (i, expected) in Metrics::CELL_SIZES.iter().enumerate() {
            // M3 ZOOM-01：布局吃连续 cell_px，这里用三个历史档位值验证尺寸映射
            let layout = GridLayout::build(&photos, 1200.0, *expected, true, ViewMode::Grid);
            assert_eq!(layout.cell, *expected, "第 {i} 档边长");
        }
    }

    #[test]
    fn continuous_cell_px_drives_layout() {
        // 连续滑块：不同 cell_px 给出不同列数与单元格尺寸（修复前布局只看离散档）
        let photos = same_day(40);
        let small = GridLayout::build(&photos, 1000.0, 48.0, true, ViewMode::Grid);
        let big = GridLayout::build(&photos, 1000.0, 256.0, true, ViewMode::Grid);
        assert_eq!(small.cell, 48.0);
        assert_eq!(big.cell, 256.0);
        assert!(big.cols < small.cols, "cell 越大列数越少");
    }

    #[test]
    fn first_cell_sits_after_padding_and_header() {
        let photos = same_day(4);
        let layout = GridLayout::build(
            &photos,
            1000.0,
            Metrics::CELL_SIZES[1],
            true,
            ViewMode::Grid,
        );
        let r = layout.thumb_rect(0).unwrap();
        assert_eq!(r.left(), Metrics::GRID_PAD);
        assert_eq!(r.top(), Metrics::GRID_PAD + Metrics::GROUP_HEADER_H);
        assert_eq!(r.width(), Metrics::CELL_SIZES[1]);
    }

    #[test]
    fn cells_are_laid_out_in_rows() {
        let photos = same_day(40);
        let layout = GridLayout::build(
            &photos,
            1000.0,
            Metrics::CELL_SIZES[0],
            true,
            ViewMode::Grid,
        );
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
        // 行距 = 单元格边长 + 文件名行（U-26：始终显示）+ 间距。
        // 这里用 `cell_h` 而不是硬写边长，否则以后调文件名行高又会静默错位。
        assert_eq!(
            second_row.top(),
            r0.top() + layout.cell_h + Metrics::CELL_GAP,
            "第二行应正好下移一个行距（cell_h 含文件名行）"
        );
    }

    #[test]
    fn total_height_grows_with_photos() {
        let small = GridLayout::build(
            &same_day(3),
            1000.0,
            Metrics::CELL_SIZES[0],
            true,
            ViewMode::Grid,
        );
        let big = GridLayout::build(
            &same_day(300),
            1000.0,
            Metrics::CELL_SIZES[0],
            true,
            ViewMode::Grid,
        );
        assert!(big.total_h > small.total_h);
        assert!(small.total_h > 0.0);
    }

    #[test]
    fn visible_range_is_bounded_and_covers_viewport() {
        let photos = same_day(1000);
        let layout = GridLayout::build(
            &photos,
            1000.0,
            Metrics::CELL_SIZES[0],
            true,
            ViewMode::Grid,
        );
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
        let layout = GridLayout::build(
            &photos,
            1000.0,
            Metrics::CELL_SIZES[0],
            true,
            ViewMode::Grid,
        );
        // 还没滚：分组头就在正常位置，不需要吸顶补画
        assert_eq!(layout.sticky_group(0.0), None);
        // 滚过分组头之后：需要吸顶。第一组内容从 GRID_PAD 开始，所以要把
        // 「上下留白 + 分组头高度」一起滚过去（只滚 GROUP_HEADER_H 是不够的）。
        assert_eq!(
            layout.sticky_group(Metrics::GRID_PAD + Metrics::GROUP_HEADER_H + 5.0),
            Some(0)
        );
        // 关掉分组时不吸顶
        let plain = GridLayout::build(
            &photos,
            1000.0,
            Metrics::CELL_SIZES[0],
            false,
            ViewMode::Grid,
        );
        assert_eq!(plain.sticky_group(100.0), None);
    }

    #[test]
    fn huge_single_group_stays_virtualized() {
        // 「同一天连拍 3000 张」是最坏情况：只有一个分组，按分组边界返回就会整组返回
        let photos = same_day(3000);
        let layout = GridLayout::build(
            &photos,
            1000.0,
            Metrics::CELL_SIZES[0],
            true,
            ViewMode::Grid,
        );
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
        let layout = GridLayout::build(
            &photos,
            1000.0,
            Metrics::CELL_SIZES[0],
            true,
            ViewMode::Grid,
        );
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
        let layout = GridLayout::build(
            &photos,
            1000.0,
            Metrics::CELL_SIZES[0],
            true,
            ViewMode::Grid,
        );
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
        let layout = GridLayout::build(
            &photos,
            1000.0,
            Metrics::CELL_SIZES[1],
            true,
            ViewMode::Grid,
        );
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
        let layout = GridLayout::build(&[], 1000.0, Metrics::CELL_SIZES[0], true, ViewMode::Grid);
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

    /// 文件名行在所有 cell_px 下都占位（U-26：三档都显示 → 连续模式下始终显示）。
    ///
    /// 守的是**布局**：`cell_h` 少算 `FILENAME_H` 的话，文件名会压到下一行的
    /// 缩略图上；多算的话每行多一条空带。两种都不报错，只会「看起来怪」，
    /// 所以只能靠用例拦。
    #[test]
    fn filename_row_reserves_height_in_every_tier() {
        for px in Metrics::CELL_SIZES {
            let layout = GridLayout::build(&[], 1280.0, px, true, ViewMode::Grid);
            assert!(layout.show_filename, "{px}px 应显示文件名");
            assert_eq!(
                layout.cell_h,
                px + crate::theme::Metrics::FILENAME_H,
                "{px}px 的单元格总高没算上文件名行"
            );
        }
    }

    /// 纵向瀑布流：不同宽高比的图给出不同高度，放进列内；列数由宽度与 cell 决定。
    #[test]
    fn masonry_v_sizes_follow_aspect_ratio() {
        let mut photos = same_day(6);
        // 前 3 张 4:3 横图，后 3 张 9:16 竖图
        for (i, p) in photos.iter_mut().enumerate() {
            if i < 3 {
                p.width = Some(400);
                p.height = Some(300);
            } else {
                p.width = Some(300);
                p.height = Some(600);
            }
        }
        let layout = GridLayout::build(&photos, 1000.0, 128.0, true, ViewMode::MasonryV);
        assert_eq!(layout.mode, ViewMode::MasonryV);
        assert!(!layout.has_headers, "masonry 不画分组头");
        let r0 = layout.thumb_rect(0).unwrap();
        let r1 = layout.thumb_rect(1).unwrap();
        // 同列等宽
        assert_eq!(r0.width(), r1.width());
        // 横图高度 < 竖图高度（同列宽下）
        let r_h = layout.thumb_rect(3).unwrap();
        assert!(
            r_h.height() > r0.height(),
            "9:16 应比 4:3 高：{:?} vs {:?}",
            r_h,
            r0
        );
        // 高度 = 宽度 / 比例 + 文件名行
        let expect = r0.width() / (400.0 / 300.0) + crate::theme::Metrics::FILENAME_H;
        assert!(
            (r0.height() - expect).abs() < 0.5,
            "高度算式不对：{} vs {}",
            r0.height(),
            expect
        );
    }

    /// 横向瀑布流：按宽高比得宽，横向滚动；总宽 > 视口宽。
    #[test]
    fn masonry_h_extends_horizontally() {
        let mut photos = same_day(60);
        for (i, p) in photos.iter_mut().enumerate() {
            if i < 3 {
                p.width = Some(400);
                p.height = Some(300);
            } else {
                p.width = Some(300);
                p.height = Some(600);
            }
        }
        let layout = GridLayout::build(&photos, 400.0, 128.0, true, ViewMode::MasonryH);
        assert_eq!(layout.mode, ViewMode::MasonryH);
        let r0 = layout.thumb_rect(0).unwrap();
        let r3 = layout.thumb_rect(3).unwrap();
        // 同行等高
        assert_eq!(r0.height(), r3.height());
        // 横图比竖图宽（同行高下）
        assert!(
            r0.width() > r3.width(),
            "4:3 应比 9:16 宽：{:?} vs {:?}",
            r0,
            r3
        );
        // 内容比视口宽（需要横向滚动）
        assert!(layout.content_w > 400.0, "content_w={}", layout.content_w);
        assert!(
            layout.total_h <= 400.0 + 1.0,
            "横向模式只横滚，内容不能高出视口：total_h={}",
            layout.total_h
        );
    }

    /// B6：小图库在又宽又高的视口里应沿横向铺开，而不是按高度排成贴左的两列。
    #[test]
    fn masonry_h_small_library_fills_width() {
        let mut photos = same_day(9);
        for p in photos.iter_mut() {
            p.width = Some(1000);
            p.height = Some(1000);
        }
        let viewport_w = 960.0;
        let viewport_h = 800.0;
        let cell = 128.0;
        let layout = GridLayout::build_in_viewport(
            &photos,
            viewport_w,
            viewport_h,
            cell,
            true,
            ViewMode::MasonryH,
        );
        let r0 = layout.thumb_rect(0).unwrap();
        assert!(
            (r0.height() - cell - Metrics::FILENAME_H).abs() < 0.5,
            "图片行高应≈cell（外加文件名带）：{:?}",
            r0
        );
        assert!(
            layout.content_w >= viewport_w * 0.85,
            "横向没铺满宽度：content_w={} viewport={viewport_w}",
            layout.content_w
        );
        let y0 = r0.top();
        for i in 1..9 {
            let y = layout.thumb_rect(i).unwrap().top();
            assert!(
                (y - y0).abs() < 1.0,
                "9 张正方形应排成一行而不是多列：index {i} y={y} y0={y0}"
            );
        }
        assert!(
            layout.total_h <= viewport_h + 1.0,
            "内容高出视口：total_h={}",
            layout.total_h
        );
        assert!(
            layout.content_w > viewport_w,
            "content_w={}",
            layout.content_w
        );
    }

    /// masonry 下宽高回填（None → Some）必须触发布局重排。
    #[test]
    fn masonry_rebuilds_when_dims_backfill() {
        let mut photos = same_day(20);
        // 清空宽高：模拟「刚入库还没回填」的占位状态
        for p in photos.iter_mut() {
            p.width = None;
            p.height = None;
        }
        let mut cache = GridCache::new();
        let before = cache
            .get(&photos, 1000.0, 800.0, 128.0, true, ViewMode::MasonryV)
            .thumb_rect(0)
            .unwrap();
        // 回填第 0 张的宽高（很扁的全景图）
        let mut photos2 = photos.clone();
        photos2[0].width = Some(1000);
        photos2[0].height = Some(300);
        let after = cache
            .get(&photos2, 1000.0, 800.0, 128.0, true, ViewMode::MasonryV)
            .thumb_rect(0)
            .unwrap();
        assert!(
            (before.height() - after.height()).abs() > 1.0,
            "宽高回填后 masonry 没重排：before={before:?} after={after:?}"
        );
    }

    /// masonry 视口裁剪：只返回视口附近的索引（虚拟化仍然成立）。
    #[test]
    fn masonry_visible_range_stays_virtualized() {
        let photos = same_day(2000);
        let layout = GridLayout::build(&photos, 1000.0, 128.0, true, ViewMode::MasonryV);
        let viewport = Rect::from_min_size(pos2(0.0, 3000.0), vec2(1000.0, 800.0));
        let (first, last) = layout.visible_index_range(viewport);
        assert!(first < last, "视口内应有可见格");
        assert!(
            last - first < 400,
            "masonry 虚拟化失效：{} 张",
            last - first
        );
        // 命中的 rect 确实与视口相交
        let r = layout.thumb_rect(first).unwrap();
        assert!(r.bottom() >= viewport.min.y - 1.0 && r.top() <= viewport.max.y + 1.0);
    }

    /// 视口之外点击不命中（masonry 的 hit test 走 rect 表）。
    #[test]
    fn masonry_hit_test_round_trips() {
        let mut photos = same_day(20);
        for (i, p) in photos.iter_mut().enumerate() {
            p.width = Some(400 + (i as i64) * 10);
            p.height = Some(300);
        }
        let layout = GridLayout::build(&photos, 1000.0, 128.0, true, ViewMode::MasonryV);
        for index in [0usize, 5, 11, 19] {
            let r = layout.thumb_rect(index).unwrap();
            assert_eq!(
                layout.index_at(r.center()),
                Some(index),
                "索引 {index} 的命中测试不一致"
            );
        }
        assert_eq!(layout.index_at(pos2(99999.0, 99999.0)), None);
    }

    /// 方格文件名是 §7.4 的可选项：关掉后不占行高、不返回文件名 rect；
    /// 重新打开必须触发布局重算（缓存键包含这个开关）。
    #[test]
    fn grid_filename_toggle_changes_layout_and_invalidates_cache() {
        let photos = same_day(4);
        let hidden = GridLayout::build_in_viewport_opts(
            &photos,
            1000.0,
            800.0,
            128.0,
            true,
            ViewMode::Grid,
            false,
        );
        assert!(!hidden.show_filename);
        assert_eq!(hidden.cell_h, 128.0, "关掉文件名后单元格不应再预留行高");
        assert_eq!(hidden.filename_rect(0), None);

        let mut cache = GridCache::new();
        let with_name = cache
            .get_with_filename(&photos, 1000.0, 800.0, 128.0, true, ViewMode::Grid, true)
            .cell_h;
        let without = cache
            .get_with_filename(&photos, 1000.0, 800.0, 128.0, true, ViewMode::Grid, false)
            .cell_h;
        assert_eq!(with_name, 128.0 + Metrics::FILENAME_H);
        assert_eq!(without, 128.0);
    }

    /// masonry：文件名带必须在**格子内部**的底部（图片带的正下方）。
    ///
    /// 守的是「名字被下一张图盖住」：早期实现把文件名画到 `rect.bottom()` 之外，
    /// 落进 gap / 邻格区域，被后续绘制的图片盖住——表现为「图片名字被遮挡」。
    #[test]
    fn masonry_filename_sits_inside_the_cell() {
        let mut photos = same_day(12);
        for (i, p) in photos.iter_mut().enumerate() {
            p.width = Some(400 + (i as i64) * 30);
            p.height = Some(300);
        }
        let layout = GridLayout::build(&photos, 1000.0, 128.0, true, ViewMode::MasonryV);
        for i in 0..12 {
            let cell = layout.thumb_rect(i).unwrap();
            let img_bottom = cell.bottom() - crate::theme::Metrics::FILENAME_H;
            let name = layout.filename_rect(i).unwrap();
            assert!(
                name.top() >= img_bottom - 0.5,
                "文件名带 ({:?}) 与图片带（底 {img_bottom}）重叠：索引 {i}",
                name
            );
            assert!(
                name.bottom() <= cell.bottom() + 0.5,
                "文件名带 ({:?}) 超出格子底边：索引 {i}",
                name
            );
        }
    }

    /// 文件名 rect 必须在缩略图**正下方**、同宽、高度正确。
    #[test]
    fn filename_rect_sits_below_the_thumbnail() {
        let photos: Vec<PhotoRow> = (0..3).map(|i| photo(i, 1_600_000_000 + i)).collect();
        let layout = GridLayout::build(
            &photos,
            1280.0,
            Metrics::CELL_SIZES[1],
            false,
            ViewMode::Grid,
        );
        for i in 0..3 {
            let thumb = layout.thumb_rect(i).unwrap();
            let name = layout.filename_rect(i).unwrap();
            assert!(
                name.top() >= thumb.bottom(),
                "文件名压到缩略图上了：thumb={thumb:?} name={name:?}"
            );
            assert_eq!(name.width(), thumb.width(), "文件名行应与缩略图同宽");
            assert_eq!(
                name.height(),
                crate::theme::Metrics::FILENAME_H,
                "文件名行高不对"
            );
        }
    }
}
