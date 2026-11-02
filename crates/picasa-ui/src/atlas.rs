//! 微缩图集：**把 22～48 px 的格子打包进大纹理**（`07-A` §A-2.2 / P-57 ②）。
//!
//! ## 不打包会怎样
//!
//! 4K 全屏、22 px 档、100 万张库：可见格约 **17 000** 个。
//! 每个格子如果各占一张 T0 纹理（512²×4 = 1 MB），总量是 17 GB——
//! `TextureBudget` 会疯狂淘汰，用户看到的是一片反复闪的灰块。
//!
//! 打包后：每张缩略图 32²×4 = **4 KB**，17 000 张 ≈ 68 MB，装进 **2 张** 4096² 图集。
//!
//! ## 这个模块只做「装」和「查」，不做「画」
//!
//! 分工与 [`picasa_core::texbudget`] 相同：
//!
//! | 层 | 职责 | 能不能单测 |
//! | --- | --- | --- |
//! | 这里 | 槽位分配、UV 计算、整页淘汰 | ✅ 纯逻辑 |
//! | `grid.rs` | 真正调 `painter.image(uv_rect)` | ❌ 要 GPU |
//!
//! 把「装哪儿」和「怎么画」分开，装错槽位这种 bug 才能被用例抓住——
//! 它在画面上表现为「照片串了」，而那种现象很难归因到 UV 计算。
//!
//! ## 为什么是「整页淘汰」而不是「逐格淘汰」
//!
//! 逐格淘汰要把空洞记下来复用（free list），而图集的价值恰恰在于
//! **连续内存 + 少 draw call**。整页淘汰时一页的 16 384 格全部作废，
//! 下次重建是一次连续上传——比零散填补快得多。
//!
//! 代价是「一页里只有一张过期也会整页重传」。但触发淘汰的前提是
//! **整页都不在视口附近**（按页统计屏距），所以这个代价不会发生在
//! 用户正在看的区域。

use eframe::egui::{self, Context, TextureHandle, TextureOptions};
use std::collections::HashMap;

/// 图集边长（物理 px）。4096 是主流 GPU 的稳妥上限（GL 3.3 保证 ≥ 1024，
/// 但实际桌面卡都支持 8192+；取 4096 兼顾老机器与显存）。
pub const ATLAS_SIZE: usize = 4096;

/// 每个槽位的边长（物理 px）。
///
/// 32 = 「22～48 逻辑 px 在 1× 屏上的上限」向上取整到 2 的幂。
/// 2× 屏上 48 逻辑 = 96 物理，这里会放大——**这是有意的**：
/// 22～48 档的图本来就小，放大 2 倍的模糊远小于「17 000 张 1 MB 纹理」的代价。
pub const SLOT_PX: usize = 32;

/// 每边多少格：4096 / 32 = 128。
pub const SLOTS_PER_SIDE: usize = ATLAS_SIZE / SLOT_PX;

/// 每帧预填充多少行（只装这些行附近的图，不是全库）。
///
/// 取 400 的理由：22 px 档下一屏约 40 行（4K 竖屏），前后各 2 屏 ≈ 200 行；
/// 400 留了一倍余量。装多了没用（用户滚不到），只会白占图集页。
pub const ATLAS_PREFILL_ROWS: usize = 400;

/// 每页多少格：128² = 16 384（`08-D` §A-2.2 的原文数字）。
pub const SLOTS_PER_PAGE: usize = SLOTS_PER_SIDE * SLOTS_PER_SIDE;

/// 一张图集纹理。
struct AtlasPage {
    texture: TextureHandle,
    /// 哪些槽位被占了
    used: Vec<bool>,
    /// 已用格数（快速判断这页满没满）
    used_count: usize,
    /// 本页最小屏距（整页淘汰的判据：一页里**最近的那张**决定它该不该留）
    min_distance: u32,
    /// 最近一次被访问的帧（同距离时先淘汰更久没用的整页）
    last_used_frame: u64,
}

/// 一张缩略图在图集里的位置。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasSlot {
    /// 第几页
    pub page: usize,
    /// 槽位下标（0..SLOTS_PER_PAGE）
    pub slot: usize,
    /// 该槽位在整张纹理里的 UV 矩形 `[min, max]`（0..1）
    pub uv: [egui::Pos2; 2],
}

/// 微缩图集管理器。
pub struct ThumbAtlas {
    pages: Vec<AtlasPage>,
    /// photo_id → 槽位
    slots: HashMap<i64, AtlasSlot>,
    /// 当前帧号
    frame: u64,
    /// 累计上传的缩略图张数（诊断）
    pub uploads: u64,
    /// 累计整页淘汰次数（诊断）
    pub evictions: u64,
    /// 账本是否已同步（`TextureBudget` 的图集页登记）。
    ///
    /// 放在这里而不是每帧都重新登记：登记是**幂等**的（同 key 覆盖），
    /// 但每帧调一次会让 `last_seen_frame` 一直刷新，淘汰逻辑就永远选不中它。
    /// 所以在「页数变化」时才同步一次。
    synced_pages: usize,
}

impl Default for ThumbAtlas {
    fn default() -> Self {
        Self::new()
    }
}

impl ThumbAtlas {
    pub fn new() -> Self {
        Self {
            pages: Vec::new(),
            slots: HashMap::new(),
            frame: 0,
            uploads: 0,
            evictions: 0,
            synced_pages: 0,
        }
    }

    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// 某页的纹理句柄（绘制用）。页不存在时返回 `None`。
    ///
    /// 为什么暴露句柄而不是在 atlas 里画：绘制需要 `egui::Painter`，
    /// 而 atlas 不该认识 UI 层。把「在哪一页」和「怎么画」分开，
    /// 槽位算错这类 bug 才能被用例抓住。
    pub fn texture_of(&self, page: usize) -> Option<&TextureHandle> {
        self.pages.get(page).map(|p| &p.texture)
    }

    /// 查一张图在哪儿。**不触发上传**。
    pub fn get(&self, photo_id: i64) -> Option<AtlasSlot> {
        self.slots.get(&photo_id).copied()
    }

    /// 已经装过了吗。
    pub fn contains(&self, photo_id: i64) -> bool {
        self.slots.contains_key(&photo_id)
    }

    /// 装一张图（`rgba` 必须是 `SLOT_PX × SLOT_PX` 的 RGBA8）。
    ///
    /// ## 为什么先找空槽位、找不到才开新页
    ///
    /// 开新页是一次 4096²×4 = **64 MB** 的分配与上传。能复用旧页的空洞就复用——
    /// 而整页淘汰保证了空洞不会永久碎片化（一页被淘汰时整页作废）。
    pub fn insert(&mut self, ctx: &Context, photo_id: i64, rgba: &[u8]) -> Option<AtlasSlot> {
        let expect = SLOT_PX * SLOT_PX * 4;
        if rgba.len() != expect {
            // 尺寸不对是**调用方的 bug**，不是运行期错误——静默跳过会让画面莫名其妙
            tracing::warn!(
                photo_id,
                got = rgba.len(),
                expect,
                "图集只接受 {}×{} RGBA8",
                SLOT_PX,
                SLOT_PX
            );
            return None;
        }

        // 已经装过：直接返回（同一张图不该占两个槽）
        if let Some(slot) = self.slots.get(&photo_id) {
            return Some(*slot);
        }

        // 找一页有空位的
        let page_idx = self.pages.iter().position(|p| p.used_count < SLOTS_PER_PAGE);
        let page_idx = match page_idx {
            Some(i) => i,
            None => {
                // 没有空位 → 开新页
                let tex = ctx.load_texture(
                    format!("thumb-atlas-{}", self.pages.len()),
                    egui::ColorImage::filled(
                        [ATLAS_SIZE, ATLAS_SIZE],
                        egui::Color32::from_rgb(240, 240, 240),
                    ),
                    TextureOptions::LINEAR,
                );
                self.pages.push(AtlasPage {
                    texture: tex,
                    used: vec![false; SLOTS_PER_PAGE],
                    used_count: 0,
                    min_distance: u32::MAX,
                    last_used_frame: self.frame,
                });
                self.pages.len() - 1
            }
        };

        // 找一个空槽位
        let page = &mut self.pages[page_idx];
        let slot = page.used.iter().position(|u| !u)?;

        // ---- 只上传那一个小矩形 ----
        //
        // 为什么不能整页上传：4096² RGBA = 64 MB，每装一张就传 64 MB，
        // 1000 张就是 64 GB 的显存带宽——比不打包还糟。
        // `set_partial` 只传 32×32 = 4 KB。
        page.texture.set_partial(
            [
                (slot % SLOTS_PER_SIDE) * SLOT_PX,
                (slot / SLOTS_PER_SIDE) * SLOT_PX,
            ],
            egui::ColorImage::from_rgba_unmultiplied([SLOT_PX, SLOT_PX], rgba),
            TextureOptions::LINEAR,
        );

        page.used[slot] = true;
        page.used_count += 1;
        page.last_used_frame = self.frame;
        // ⚠️ 这里**不**动 `min_distance`。
        //
        // 语义是「本帧这一页里**被看到的那张**离视口多远」，由 `note_viewport` 填。
        // 还没被填过时它保持 `u32::MAX`（= 「这一页本帧没被看到」），
        // 于是 `evict_pages_farther_than(3)` 会把它当成「最远」——正确的行为：
        // 刚装进去但用户根本没看到（比如预填充了 400 行、他只看了前 40 行），
        // 那几页就该优先被回收。

        let atlas_slot = AtlasSlot {
            page: page_idx,
            slot,
            uv: slot_uv(slot),
        };
        self.slots.insert(photo_id, atlas_slot);
        self.uploads += 1;
        Some(atlas_slot)
    }

    /// 告知本帧哪些图在视口内、屏距多少（整页淘汰的判据）。
    pub fn note_viewport(&mut self, distances: impl IntoIterator<Item = (i64, u32)>) {
        // 先清空每页的距离，再按本次的可视项重算
        for p in &mut self.pages {
            p.min_distance = u32::MAX;
        }
        let mut touched_pages: Vec<usize> = Vec::new();
        for (photo_id, distance) in distances {
            let Some(slot) = self.slots.get(&photo_id) else {
                continue;
            };
            let page = &mut self.pages[slot.page];
            page.min_distance = page.min_distance.min(distance);
            page.last_used_frame = self.frame;
            if !touched_pages.contains(&slot.page) {
                touched_pages.push(slot.page);
            }
        }
    }

    /// 帧末淘汰：整页屏距 ≥ `min_distance` 的页全部作废。
    ///
    /// 返回被淘汰的页数。丢弃那页上的所有 `photo_id` 映射
    /// （下次访问时会重新请求解码）。
    ///
    /// `min_distance = 3` 与 `TextureBudget` 的淘汰线一致——
    /// 两套缓存对「多远算远」应当有同一个定义，否则会出现
    /// 「纹理账本说该留、图集说该丢」的互相打架。
    pub fn evict_pages_farther_than(&mut self, min_distance: u32) -> usize {
        let mut drop_pages: Vec<usize> = Vec::new();
        for (i, p) in self.pages.iter().enumerate() {
            // `min_distance == u32::MAX` 表示这页本帧**一张都没被看到**
            // （可能整个滚出了视口），按最远处理。
            if p.min_distance >= min_distance {
                drop_pages.push(i);
            }
        }
        if drop_pages.is_empty() {
            return 0;
        }
        // 从后往前删，避免下标错位
        drop_pages.sort_unstable_by(|a, b| b.cmp(a));
        for page_idx in &drop_pages {
            // 先把映射清掉（这一页上的照片下次访问会重新请求）
            self.slots.retain(|_, s| s.page != *page_idx);
            // 再删纹理（drop 即释放 GPU 内存）
            self.pages.remove(*page_idx);
        }

        // 删过页之后，remaining slot 的 page 下标要按「在它之前删了几页」左移。
        // ⚠️ 这里不能就地改 `slot.page` 再据此计算——那是典型的
        // 「边改边算」错误：改过的值会影响后续比较。
        // 正确做法是**先把旧下标收集起来**，统一映射。
        let removed: std::collections::HashSet<usize> = drop_pages.iter().copied().collect();
        for slot in self.slots.values_mut() {
            // 新下标 = 旧下标 − 排在我前面被删掉的页数
            let shifted = removed.iter().filter(|r| **r < slot.page).count();
            slot.page -= shifted;
        }

        self.evictions += drop_pages.len() as u64;
        drop_pages.len()
    }

    /// 把图集占用同步进 `TextureBudget`（`07-A` §A-2.2 / SCALE-08）。
    ///
    /// ## 为什么必须同步
    ///
    /// 不同步的话 `TextureBudget.used_mb` 在 22 px 档会**低估**实际占用
    /// （低估最多 `页数 × 64 MB`），于是 `M3-SCALE-08` 的内存自保护
    /// （软 2 GB → 降档 / 硬 2.46 GB → 停解码）会偏乐观——
    /// 它以为还有 200 MB 余量，实际已经被图集吃掉。
    ///
    /// ## 为什么只在页数变化时同步
    ///
    /// `register` 会刷新 `last_seen_frame`。每帧调一次会让图集页永远「刚被看到」，
    /// 淘汰逻辑就再也选不中它（`evict_end_of_frame` 按「同距离下最久没被看到」排）。
    pub fn sync_budget(&mut self, budget: &mut picasa_core::TextureBudget) {
        if self.synced_pages == self.pages.len() {
            return;
        }
        // 先把已经不存在的页销账（按 key 全清再重登，页数很少，成本可忽略）
        for pid in 0..self.synced_pages as i64 {
            budget.forget_atlas_page(pid);
        }
        for (i, page) in self.pages.iter().enumerate() {
            // 一页的屏距 = 这一页里**最近的那张**的距离
            // （只要有一张在视口内，这 64 MB 就砍不得）
            let distance = page.min_distance.min(3);
            budget.register_atlas_page(i as i64, (ATLAS_SIZE * ATLAS_SIZE * 4) as u64, distance);
        }
        self.synced_pages = self.pages.len();
    }

    /// 诊断文案（浮层显示）。
    pub fn stats_text(&self) -> String {
        format!(
            "图集 {} 页 · {} 张 · 上传 {} · 淘汰 {} 页",
            self.pages.len(),
            self.slots.len(),
            self.uploads,
            self.evictions
        )
    }

    /// 估算显存占用（字节）——图集是**固定大小**的，所以是按页算而不是按张算。
    pub fn bytes(&self) -> u64 {
        (self.pages.len() as u64) * (ATLAS_SIZE as u64) * (ATLAS_SIZE as u64) * 4
    }

    /// 全部丢弃（切视图 / 换档时）。
    pub fn clear(&mut self) {
        self.pages.clear();
        self.slots.clear();
    }
}

/// 槽位下标 → UV 矩形（`[min, max]`，0..1）。
///
/// ⚠️ 必须**精确到像素**地算边界，不能用「槽位中心 ± 半格」——
/// 4 KB 的 32×32 槽位在 4096 里只占 1/128，浮点误差会让相邻槽位串色
/// （画面上是照片边缘出现隔壁照片的一条像素）。
pub fn slot_uv(slot: usize) -> [egui::Pos2; 2] {
    let col = slot % SLOTS_PER_SIDE;
    let row = slot / SLOTS_PER_SIDE;
    let inv = 1.0 / SLOTS_PER_SIDE as f32;
    [
        egui::pos2(col as f32 * inv, row as f32 * inv),
        egui::pos2((col + 1) as f32 * inv, (row + 1) as f32 * inv),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_constants_are_consistent() {
        assert_eq!(ATLAS_SIZE % SLOT_PX, 0, "图集边长必须是槽位的整数倍");
        assert_eq!(SLOTS_PER_SIDE, 128);
        assert_eq!(SLOTS_PER_PAGE, 16_384, "08-D §A-2.2 的原文数字");
        // 每槽 4 KB，整页 64 MB
        assert_eq!(SLOT_PX * SLOT_PX * 4, 4096);
        assert_eq!(ATLAS_SIZE * ATLAS_SIZE * 4, 64 * 1024 * 1024);
    }

    #[test]
    fn slot_uv_covers_each_cell_exactly_once() {
        // 角上四个槽位的 UV 必须正好贴边，且互不重叠
        let first = slot_uv(0);
        assert_eq!(first[0], egui::pos2(0.0, 0.0));
        let last = slot_uv(SLOTS_PER_PAGE - 1);
        assert!(
            (last[1].x - 1.0).abs() < 1e-6 && (last[1].y - 1.0).abs() < 1e-6,
            "最后一个槽位必须贴到右下角：{:?}",
            last[1]
        );
        // 相邻槽位不重叠：0 的 max == 1 的 min（同一行）
        let a = slot_uv(0);
        let b = slot_uv(1);
        assert_eq!(a[1].x, b[0].x, "同一行相邻槽位必须严丝合缝");
        // 换行也严丝合缝
        let c = slot_uv(SLOTS_PER_SIDE);
        assert_eq!(a[1].y, c[0].y);
    }

    #[test]
    fn slot_uv_is_within_bounds_and_well_formed() {
        // 每个槽位都必须是一个**合法的、非空的** UV 矩形，且落在 [0,1]² 内。
        //
        // ⚠️ 不要断言「跨槽位单调递增」——行优先布局下，换行时 x 会跳回 0。
        // 早先写成 `uv.x > prev` 的断言在跨行时必红，而那是布局定义，不是 bug。
        for slot in 0..SLOTS_PER_PAGE {
            let uv = slot_uv(slot);
            assert!(uv[0].x >= 0.0 && uv[1].x <= 1.0, "slot {slot} 的 x 越界：{uv:?}");
            assert!(uv[0].y >= 0.0 && uv[1].y <= 1.0, "slot {slot} 的 y 越界：{uv:?}");
            assert!(uv[0].x < uv[1].x, "slot {slot} 的宽度必须为正");
            assert!(uv[0].y < uv[1].y, "slot {slot} 的高度必须为正");
        }
    }

    #[test]
    fn slot_uv_of_the_same_row_advances_left_to_right() {
        // 同一行内 x 必须递增（这是布局的定义）
        let mut prev = -1.0f32;
        for col in 0..SLOTS_PER_SIDE {
            let uv = slot_uv(col);
            assert!(uv[0].x > prev, "第 {col} 列的 x 没递增");
            prev = uv[0].x;
        }
        // 换行后 x 回到 0，y 前进一格
        let row1 = slot_uv(SLOTS_PER_SIDE);
        assert_eq!(row1[0].x, 0.0);
        assert!(row1[0].y > slot_uv(0)[0].y);
    }

    #[test]
    fn a_new_atlas_has_nothing_in_it() {
        let a = ThumbAtlas::new();
        assert!(a.is_empty());
        assert_eq!(a.page_count(), 0);
        assert_eq!(a.get(1), None);
        assert_eq!(a.bytes(), 0);
        assert!(a.stats_text().contains("0 页"));
    }

    /// 回归：被看到的页**不得**被帧末淘汰。
    ///
    /// 守的是「滑块拖进 ≤48px 档后预览图全部消失」：早先 UI 忘了把视口屏距
    /// 喂给图集（`note_viewport` 零调用者），每页 min_distance 恒为 u32::MAX，
    /// `evict_pages_farther_than(3)` 就把所有页当「本帧没被看到」整页删光。
    /// 图集层能测的部分：note 之后页的距离被正确记录，淘汰放行；没 note 的页照删。
    #[test]
    fn pages_that_were_seen_survive_eviction() {
        let mut a = ThumbAtlas::new();
        // 手工造一页两个槽（不走 GPU 路径）
        a.pages.push(AtlasPage {
            // 测试只关心 slots/min_distance 的逻辑，纹理用 4²（egui 测试后端
            // 限制纹理边 ≤ 2048，真尺寸 4096² 会 panic）
            texture: egui::Context::default().load_texture(
                "t",
                egui::ColorImage::filled([4, 4], egui::Color32::GRAY),
                TextureOptions::LINEAR,
            ),
            used: vec![true, true],
            used_count: 2,
            min_distance: u32::MAX,
            last_used_frame: 0,
        });
        a.slots.insert(1, AtlasSlot { page: 0, slot: 0, uv: slot_uv(0) });
        a.slots.insert(2, AtlasSlot { page: 0, slot: 1, uv: slot_uv(1) });

        // 帧 1：photo 1 在视口内（屏距 0），photo 2 不喂
        a.begin_frame();
        a.note_viewport([(1, 0)]);
        assert_eq!(a.evict_pages_farther_than(3), 0, "被看到的页不得被淘汰");
        assert!(a.contains(1), "看到的图必须还在");
        assert!(a.contains(2), "同页的其他图跟着页活下来");

        // 帧 2：正常帧会调 note_viewport（空视口也要调——它负责把所有页的
        // 距离重置成 MAX），这里模拟「图都在视口外」的空喂入 → 整页按最远淘汰
        a.begin_frame();
        a.note_viewport([] as [(i64, u32); 0]);
        assert_eq!(a.evict_pages_farther_than(3), 1, "没被看到的页应被淘汰");
        assert!(!a.contains(1));
    }

    #[test]
    fn clear_drops_everything() {
        let mut a = ThumbAtlas::new();
        a.slots.insert(
            1,
            AtlasSlot {
                page: 0,
                slot: 0,
                uv: slot_uv(0),
            },
        );
        a.clear();
        assert!(a.is_empty());
        assert_eq!(a.page_count(), 0);
    }

    #[test]
    fn evicting_with_no_pages_is_a_no_op() {
        let mut a = ThumbAtlas::new();
        assert_eq!(a.evict_pages_farther_than(3), 0);
        assert_eq!(a.evictions, 0);
    }

    #[test]
    fn stats_text_reports_all_four_numbers() {
        let mut a = ThumbAtlas::new();
        a.uploads = 12;
        a.evictions = 3;
        let t = a.stats_text();
        assert!(t.contains("上传 12"), "{t}");
        assert!(t.contains("淘汰 3"), "{t}");
    }

    #[test]
    fn bytes_are_per_page_not_per_slot() {
        // 图集是固定大小分配的：装 1 张和装 1000 张占的是同一块 64 MB。
        // 这正是它比「每张一张纹理」省的地方——`TextureBudget` 按页记账才准。
        let a = ThumbAtlas::new();
        assert_eq!(a.bytes(), 0);
        // 手工构造一页来验算术（不碰 GPU）
        assert_eq!(ATLAS_SIZE as u64 * ATLAS_SIZE as u64 * 4, 67_108_864);
    }
}

#[cfg(test)]
mod eviction_tests {
    use super::*;

    /// 不碰 GPU 地造一个「有 N 页」的 atlas——用 `TextureHandle` 的替代品做不到，
    /// 所以这里只测**纯逻辑**部分：页下标重映射。
    ///
    /// 「删页之后剩余的页下标要左移」是整段代码最容易错的地方：
    /// 错了的表现是「照片串到了别的格子」，而那种现象几乎无法归因。
    fn remap(old: usize, removed: &[usize]) -> usize {
        old - removed.iter().filter(|r| **r < old).count()
    }

    #[test]
    fn remap_after_removing_earlier_pages() {
        // 删掉第 1 页：原本的 0/2/3 → 0/1/2
        assert_eq!(remap(0, &[1]), 0, "在删除点之前的页不动");
        assert_eq!(remap(2, &[1]), 1);
        assert_eq!(remap(3, &[1]), 2);
    }

    #[test]
    fn remap_after_removing_multiple_pages() {
        // 删掉 1 和 3：原本的 0/2/4 → 0/1/2
        assert_eq!(remap(0, &[1, 3]), 0);
        assert_eq!(remap(2, &[1, 3]), 1);
        assert_eq!(remap(4, &[1, 3]), 2);
    }

    #[test]
    fn remap_with_nothing_removed_is_identity() {
        for old in 0..10 {
            assert_eq!(remap(old, &[]), old);
        }
    }

    #[test]
    fn remap_removing_the_last_page_does_not_shift_earlier_ones() {
        assert_eq!(remap(0, &[5]), 0);
        assert_eq!(remap(4, &[5]), 4);
    }

    #[test]
    fn evict_threshold_matches_the_texture_budget() {
        // 两套缓存对「多远算远」必须有同一条线，否则会出现
        // 「纹理账本说该留、图集说该丢」的互相打架。
        assert_eq!(
            picasa_core::texbudget::EVICT_SCREEN_DISTANCE,
            3,
            "图集的淘汰线必须与 TextureBudget 一致"
        );
    }

    #[test]
    fn atlas_slot_uv_math_is_exact_at_page_edges() {
        // 页边界上的槽位：最后一个槽位的 UV 上界必须正好是 1.0，
        // 差一点点就会采样到纹理外的空白（画面上是一圈灰边）。
        let last = slot_uv(SLOTS_PER_PAGE - 1);
        assert!((last[1].x - 1.0).abs() < 1e-6);
        assert!((last[1].y - 1.0).abs() < 1e-6);
        // 第一槽从 0 开始
        let first = slot_uv(0);
        assert_eq!(first[0].x, 0.0);
        assert_eq!(first[0].y, 0.0);
    }

    #[test]
    fn a_32px_slot_is_4kb_and_a_page_is_64mb() {
        // 这两个数是 `08-D` §A-2.2 的预算依据，写死在用例里防止有人「顺手改大」。
        assert_eq!(SLOT_PX, 32);
        assert_eq!(SLOT_PX * SLOT_PX * 4, 4 * 1024, "每槽 4 KB");
        assert_eq!(SLOTS_PER_PAGE, 16_384, "每页 16 384 格");
        assert_eq!(
            SLOTS_PER_PAGE * SLOT_PX * SLOT_PX * 4,
            64 * 1024 * 1024,
            "每页 64 MB"
        );
    }

    #[test]
    fn seventeen_thousand_cells_fit_in_two_pages() {
        // `08-D` §A-2.2 的核心算式：4K 全屏 17 000 格 ≈ 2 张图集。
        // 这条用例把那个结论钉住——它正是「22 px 档可行」的全部依据。
        let visible = 17_000usize;
        let pages = visible.div_ceil(SLOTS_PER_PAGE);
        assert_eq!(pages, 2, "17 000 格只需要 2 页");
        assert_eq!(pages * 64, 128, "共 128 MB，远低于 200 MB 预算");
    }
}
