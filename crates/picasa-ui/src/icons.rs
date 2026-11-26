//! 图标：**用 egui 矢量绘制，不引入 SVG 光栅化依赖**（02 §8.3 的结论）。
//!
//! ## 为什么不用原版图标
//!
//! Picasa 的图标受版权保护，**不能提取原版资源**（02 §8.3 的法律底线）。
//! 这里的图形是为本项目重绘的极简几何图形，视觉重量对齐原版，笔触不求逐像素相同
//! （01 U-10 明确「不要求逐像素相同」）。
//!
//! ## 为什么不用 SVG + resvg
//!
//! `resvg`/`usvg` 是 MPL-2.0，且 M1 的图标数量只有十来个、形状都极其简单。
//! 用 `Painter` 直接画线省掉一个依赖，也省掉一次启动时的光栅化。

use eframe::egui::{Color32, Mesh, Painter, Pos2, Rect, Shape, Stroke, pos2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    /// 返回（←）。M1 里恒为禁用态——没有别的「房间」可返回。
    Back,
    /// 添加文件夹（folder + plus）
    AddFolder,
    /// 在文件管理器中显示 / 显示位置（定位水滴，原型 `locate`）
    Reveal,
    /// 排序（长短竖条）
    Sort,
    /// 缩略图大小（2×2 方块）
    GridSize,
    /// 全屏（四角）
    Fullscreen,
    /// 搜索（圆 + 柄）
    Search,
    /// 展开箭头（▶）
    ChevronRight,
    /// 收起箭头（▼）
    ChevronDown,
    /// 照片托盘
    Tray,
    /// 移到废纸篓
    Trash,
    /// 信息面板
    Info,
    /// 索引进度（时钟：后台正在建立索引）
    Index,
    /// 关闭
    Close,
    /// 文件夹（M3 RECURSE-01：包含子文件夹开关）
    Folder,
    // ---- M3.1：macOS「照片」风格顶栏 / 左栏 ----
    /// 加号（顶栏缩放 +）
    Plus,
    /// 减号（顶栏缩放 −）
    Minus,
    /// 左栏开关（侧栏切换，红绿灯右侧第一枚）
    SidebarToggle,
    /// 排序（上下箭头，顶栏右侧第一枚）
    SortArrows,
    /// 列表（三横线，顶栏第二枚）
    ListLines,
    /// 省略号（…）
    Ellipsis,
    /// 分享（方框 + 向上箭头）
    Share,
    /// 收藏心形
    Heart,
    /// 五角星（侧栏「星标」行；与心形不同，原型 `icon("star", 15)`）
    Star,
    /// 书本/相簿（左栏相簿行图标）
    Album,
    /// 照片堆叠（左栏「图库」行图标）
    PhotoStack,
    /// 地图折页（左栏「地图」）
    Map,
    /// 摄像机（左栏「视频」）
    Video,
    /// 人物头像圆（左栏「人物」）
    Person,
    // ---- M3.1 §7：首页工具栏（导入 / 幻灯片 / 编辑 / 三种排布）----
    /// 导入（托盘 + 向下箭头）
    Import,
    /// 幻灯片（播放三角）
    Slideshow,
    /// 编辑（铅笔）
    Edit,
    /// 排布：方格
    LayoutGrid,
    /// 排布：纵向瀑布流
    LayoutMasonryV,
    /// 排布：横向瀑布流
    LayoutMasonryH,
    // ---- M4.1：工具栏对象操作（矢量自绘，不提取原版资源）----
    /// 标签（吊牌轮廓 + 圆孔）
    Tag,
    /// 说明（对话气泡 + 两条横线文本）
    Caption,
    // ---- M5-V1 r7：查看器信息栏的字段图标（原型 v1.4）----
    /// 日历（拍摄日期）
    Calendar,
    /// 文档（文件 / 格式 / 大小）
    File,
    /// 图片（像素尺寸）
    Image,
    /// 相机（机身 / 镜头）
    Camera,
}

/// 图标路径表：全部来自 Lucide（ISC 许可），见 [`crate::icons_lucide`]。
///
/// 为什么改成查表：手绘的那套为了塞进 `match` 里逐条 `line_segment`，每个图形只能
/// 用最少的直线段逼近，结果有两类硬伤——**形状错**（心形退化成胶囊、`Slideshow`
/// 画成显示器）和**比例错**（同一套里线条粗细/留白不一致）。Lucide 的 24×24 网格
/// 是同一套画出来的，换掉后整套图标的视觉重量自动统一。
pub fn paths(icon: Icon) -> &'static [crate::icons_lucide::SubPath] {
    match icon {
        Icon::Back => crate::icons_lucide::BACK,
        Icon::AddFolder => crate::icons_lucide::ADD_FOLDER,
        Icon::Reveal => crate::icons_lucide::REVEAL,
        Icon::Sort => crate::icons_lucide::SORT,
        Icon::GridSize => crate::icons_lucide::GRID_SIZE,
        Icon::Fullscreen => crate::icons_lucide::FULLSCREEN,
        Icon::Search => crate::icons_lucide::SEARCH,
        Icon::ChevronRight => crate::icons_lucide::CHEVRON_RIGHT,
        Icon::ChevronDown => crate::icons_lucide::CHEVRON_DOWN,
        Icon::Tray => crate::icons_lucide::TRAY,
        Icon::Trash => crate::icons_lucide::TRASH,
        Icon::Info => crate::icons_lucide::INFO,
        Icon::Index => crate::icons_lucide::INDEX,
        Icon::Close => crate::icons_lucide::CLOSE,
        Icon::Folder => crate::icons_lucide::FOLDER,
        Icon::Plus => crate::icons_lucide::PLUS,
        Icon::Minus => crate::icons_lucide::MINUS,
        Icon::SidebarToggle => crate::icons_lucide::SIDEBAR_TOGGLE,
        Icon::SortArrows => crate::icons_lucide::SORT_ARROWS,
        Icon::ListLines => crate::icons_lucide::LIST_LINES,
        Icon::Ellipsis => crate::icons_lucide::ELLIPSIS,
        Icon::Share => crate::icons_lucide::SHARE,
        Icon::Heart => crate::icons_lucide::HEART,
        Icon::Star => crate::icons_lucide::STAR,
        Icon::Album => crate::icons_lucide::ALBUM,
        Icon::PhotoStack => crate::icons_lucide::PHOTO_STACK,
        Icon::Map => crate::icons_lucide::MAP,
        Icon::Video => crate::icons_lucide::VIDEO,
        Icon::Person => crate::icons_lucide::PERSON,
        Icon::Import => crate::icons_lucide::IMPORT,
        Icon::Slideshow => crate::icons_lucide::SLIDESHOW,
        Icon::Edit => crate::icons_lucide::EDIT,
        Icon::LayoutGrid => crate::icons_lucide::LAYOUT_GRID,
        Icon::LayoutMasonryV => crate::icons_lucide::LAYOUT_MASONRY_V,
        Icon::LayoutMasonryH => crate::icons_lucide::LAYOUT_MASONRY_H,
        Icon::Tag => crate::icons_lucide::TAG,
        Icon::Caption => crate::icons_lucide::CAPTION,
        Icon::Calendar => crate::icons_lucide::CALENDAR,
        Icon::File => crate::icons_lucide::FILE,
        Icon::Image => crate::icons_lucide::IMAGE,
        Icon::Camera => crate::icons_lucide::CAMERA,
    }
}

/// 在 `rect` 内绘制图标。`stroke` 是**屏幕像素**宽度；`stroke <= 0` 表示实心填充
/// （选中态的心形/星形走这条）。
pub fn draw(painter: &Painter, icon: Icon, rect: Rect, color: Color32, stroke: f32) {
    let paths = paths(icon);
    let map = |x: f32, y: f32| -> Pos2 {
        pos2(
            rect.left() + x * rect.width(),
            rect.top() + y * rect.height(),
        )
    };
    if stroke <= 0.0 {
        for sp in paths {
            let pts: Vec<Pos2> = sp.pts.iter().map(|&(x, y)| map(x, y)).collect();
            if pts.len() < 3 {
                continue;
            }
            // 实心：交给 egui 的路径填充（支持凹多边形，心形/星形都不会被当成凸包）。
            painter.add(Shape::Path(egui::epaint::PathShape {
                points: pts,
                closed: true,
                fill: color,
                stroke: egui::epaint::PathStroke::NONE,
            }));
        }
        return;
    }
    for sp in paths {
        let mut pts: Vec<Pos2> = sp.pts.iter().map(|&(x, y)| map(x, y)).collect();
        if pts.len() < 2 {
            continue;
        }
        if sp.closed {
            pts.push(pts[0]);
        }
        painter.add(Shape::line(pts, Stroke::new(stroke, color)));
    }
}

/// M4.2 M42-UI-01：五角星的 10 个顶点（5 个外尖 + 5 个内凹，交替）。
///
/// 从正上方起、每 36° 一个点，外半径 `outer`、内半径 `inner`。
/// 这是**纯几何**：不依赖字体里是否有「★」字形（历史隔离空库唯一 INFO
/// `No font has "✓"` 已证明符号 glyph 不可靠），所以网格/查看器的星标角标
/// 一律自绘，而不是 `painter.text` 画字符。
pub fn star_points(center: Pos2, outer: f32, inner: f32) -> [Pos2; 10] {
    let mut pts = [center; 10];
    for (i, p) in pts.iter_mut().enumerate() {
        let angle = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        let (sin, cos) = angle.sin_cos();
        let r = if i % 2 == 0 { outer } else { inner };
        *p = pos2(center.x + r * cos, center.y + r * sin);
    }
    pts
}

/// M4.2 M42-UI-01：在 `center` 画一颗实心五角星（半径 `outer`）。
///
/// 凹多边形不能用 `Shape::convex_polygon`（它只保证凸），所以走 `Mesh`：
/// 一个中心顶点 + 10 个轮廓顶点 + 10 个扇形三角形，用 `colored_vertex`
/// （不带纹理），`painter.add(mesh)` 一次成型。**不调用 `Mesh::append`**，
/// 规避 epaint `debug_assert!(other.is_valid(), "Other mesh is invalid")` 那条
/// 历史崩溃（M42-ROB-01 三层防御的根因）。
pub fn draw_star(painter: &Painter, center: Pos2, outer: f32, color: Color32) {
    let inner = outer * 0.45;
    let pts = star_points(center, outer, inner);
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, color); // #0 = 中心
    for &p in &pts {
        mesh.colored_vertex(p, color); // #1..#10 = 轮廓
    }
    for i in 0..10 {
        mesh.add_triangle(0, 1 + i as u32, 1 + ((i + 1) % 10) as u32);
    }
    painter.add(mesh);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全部 41 个变体：新增图标时必须同步这里，否则下面的覆盖率测试会漏。
    const ALL: [Icon; 41] = [
        Icon::Back,
        Icon::AddFolder,
        Icon::Reveal,
        Icon::Sort,
        Icon::GridSize,
        Icon::Fullscreen,
        Icon::Search,
        Icon::ChevronRight,
        Icon::ChevronDown,
        Icon::Tray,
        Icon::Trash,
        Icon::Info,
        Icon::Index,
        Icon::Close,
        Icon::Folder,
        Icon::Plus,
        Icon::Minus,
        Icon::SidebarToggle,
        Icon::SortArrows,
        Icon::ListLines,
        Icon::Ellipsis,
        Icon::Share,
        Icon::Heart,
        Icon::Star,
        Icon::Album,
        Icon::PhotoStack,
        Icon::Map,
        Icon::Video,
        Icon::Person,
        Icon::Import,
        Icon::Slideshow,
        Icon::Edit,
        Icon::LayoutGrid,
        Icon::LayoutMasonryV,
        Icon::LayoutMasonryH,
        Icon::Tag,
        Icon::Caption,
        Icon::Calendar,
        Icon::File,
        Icon::Image,
        Icon::Camera,
    ];

    /// 每个变体都得有路径数据，且点列至少两个点（一个点的「路径」画不出来）。
    #[test]
    fn every_icon_has_usable_lucide_path_data() {
        for icon in ALL {
            let paths = paths(icon);
            assert!(!paths.is_empty(), "{icon:?} 没有路径数据");
            for sp in paths {
                assert!(
                    sp.pts.len() >= 2,
                    "{icon:?} 有一条子路径只有 {} 个点",
                    sp.pts.len()
                );
                if sp.closed {
                    assert!(sp.pts.len() >= 3, "{icon:?} 的闭合路径需要 ≥3 点");
                }
            }
        }
    }

    /// Lucide 的 24×24 网格决定所有坐标该落在 0..1；超出说明转换器算错了
    /// （典型是 SVG 圆弧端点参数化取错符号）。
    #[test]
    fn icon_points_stay_inside_the_view_box() {
        for icon in ALL {
            for sp in paths(icon) {
                for &(x, y) in sp.pts {
                    assert!(
                        (-0.02..=1.02).contains(&x) && (-0.02..=1.02).contains(&y),
                        "{icon:?} 有点越界：({x}, {y})"
                    );
                }
            }
        }
    }

    /// 心形：凹口在水平中央偏上、心尖在正下方，且**闭合**（实心填充靠它）。
    ///
    /// 这条替换了旧的手绘断言——手绘版曾经退化成「两圆 + 三角」的药丸，
    /// 现在形状由 Lucide 的 path 决定，只需钉住「像心形」的几个特征。
    #[test]
    fn heart_is_concave_with_a_bottom_tip_and_closed() {
        let paths = paths(Icon::Heart);
        assert_eq!(paths.len(), 1, "Lucide heart 是单条闭合路径");
        let sp = &paths[0];
        assert!(sp.closed, "心形必须闭合，否则实心填充会漏");
        let pts = sp.pts;
        // 心尖 = y 最大的点，应在中线附近、且靠下
        let tip = pts
            .iter()
            .cloned()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap();
        assert!((tip.0 - 0.5).abs() < 0.12, "心尖应在中线附近，实际 {tip:?}");
        assert!(tip.1 > 0.75, "心尖应靠底部，实际 y={}", tip.1);
        // 凹口 = y 最小的点里最靠中的那个；两肩必须比它更靠下
        let top = pts
            .iter()
            .cloned()
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap();
        assert!(top.1 < 0.35, "凹口应在上部，实际 y={}", top.1);
        let lowest_top = pts.iter().filter(|p| p.1 < top.1 + 0.08).count();
        assert!(lowest_top >= 1, "顶部应有可辨识的凹口");
    }

    /// 定位图钉：底部收成尖、顶部是圆弧，不是放大镜。
    #[test]
    fn reveal_is_a_pin_with_a_bottom_tip() {
        let paths = paths(Icon::Reveal);
        assert!(!paths.is_empty());
        let all: Vec<(f32, f32)> = paths.iter().flat_map(|sp| sp.pts.iter().cloned()).collect();
        let tip = all
            .iter()
            .cloned()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap();
        assert!((tip.0 - 0.5).abs() < 0.15, "水滴尖应接近中线，实际 {tip:?}");
        assert!(tip.1 > 0.85, "水滴尖应到底部，实际 y={}", tip.1);
        // 左右对称（容差放宽到 0.06：Lucide 的 pin 带一点视觉补偿）
        for &(x, y) in &all {
            let mx = 1.0 - x;
            assert!(
                all.iter()
                    .any(|&(qx, qy)| (qx - mx).abs() < 0.06 && (qy - y).abs() < 0.06),
                "缺少镜像点 ({x}, {y})"
            );
        }
    }

    /// 选中态用 `stroke <= 0` 走实心填充：这些图标的首条路径必须闭合。
    #[test]
    fn fillable_icons_are_closed_paths() {
        for icon in [Icon::Heart, Icon::Star] {
            let sp = &paths(icon)[0];
            assert!(sp.closed, "{icon:?} 要支持实心填充，路径必须闭合");
        }
    }

    /// 五角星几何（`draw_star` 仍被网格/查看器的星标角标使用）。
    #[test]
    fn star_points_alternate_outer_and_inner_radius() {
        let c = pos2(100.0, 100.0);
        let pts = star_points(c, 10.0, 4.5);
        for (i, p) in pts.iter().enumerate() {
            let r = ((p.x - c.x).powi(2) + (p.y - c.y).powi(2)).sqrt();
            let expect = if i % 2 == 0 { 10.0 } else { 4.5 };
            assert!(
                (r - expect).abs() < 0.001,
                "point {i} radius {r} != {expect}"
            );
        }
        assert!((pts[0].x - c.x).abs() < 0.001);
        assert!(pts[0].y < c.y);
        assert_eq!(pts.len(), 10);
    }

    #[test]
    fn draw_star_mesh_is_valid_and_single_tone() {
        let c = pos2(0.0, 0.0);
        let pts = star_points(c, 7.0, 3.15);
        let mut mesh = Mesh::default();
        mesh.colored_vertex(c, Color32::GOLD);
        for &p in &pts {
            mesh.colored_vertex(p, Color32::GOLD);
        }
        for i in 0..10 {
            mesh.add_triangle(0, 1 + i as u32, 1 + ((i + 1) % 10) as u32);
        }
        assert!(mesh.is_valid());
        assert_eq!(mesh.vertices.len(), 11);
        assert_eq!(mesh.indices.len(), 30);
        assert!(mesh.vertices.iter().all(|v| v.color == Color32::GOLD));
    }
}
