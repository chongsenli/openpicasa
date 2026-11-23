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

use eframe::egui::{Color32, Mesh, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2};

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

/// 在 `rect` 内绘制图标。线条宽度按 `stroke` 缩放（Retina 下自动变细）。
pub fn draw(painter: &Painter, icon: Icon, rect: Rect, color: Color32, stroke: f32) {
    let s = Stroke::new(stroke, color);
    // 归一化坐标 → 屏幕坐标
    let p = |x: f32, y: f32| -> Pos2 {
        pos2(
            rect.left() + x * rect.width(),
            rect.top() + y * rect.height(),
        )
    };
    let inset = rect.shrink(rect.width() * 0.16);
    let ip = |x: f32, y: f32| -> Pos2 {
        pos2(
            inset.left() + x * inset.width(),
            inset.top() + y * inset.height(),
        )
    };

    match icon {
        Icon::Back => {
            painter.line_segment([p(0.18, 0.5), p(0.84, 0.5)], s);
            painter.line_segment([p(0.18, 0.5), p(0.44, 0.24)], s);
            painter.line_segment([p(0.18, 0.5), p(0.44, 0.76)], s);
        }
        Icon::AddFolder => {
            // 文件夹主体
            let body = Rect::from_min_max(ip(0.0, 0.22), ip(1.0, 0.9));
            painter.rect_stroke(body, 1.0, s, StrokeKind::Inside);
            painter.line_segment([ip(0.0, 0.22), ip(0.0, 0.1)], s);
            painter.line_segment([ip(0.0, 0.1), ip(0.42, 0.1)], s);
            painter.line_segment([ip(0.42, 0.1), ip(0.52, 0.22)], s);
            // 加号（在主体右下角外侧，视觉上更清楚）
            let c = p(0.72, 0.7);
            painter.line_segment([pos2(c.x - 4.0, c.y), pos2(c.x + 4.0, c.y)], s);
            painter.line_segment([pos2(c.x, c.y - 4.0), pos2(c.x, c.y + 4.0)], s);
        }
        // 显示位置：原型 `locate` 是水滴形图钉（<circle cx=12 cy=11 r=3> + 水滴轮廓），
        // 不是放大镜。这里画水滴轮廓 + 中心圆孔。
        Icon::Reveal => {
            painter.add(Shape::closed_line(
                locate_points(rect),
                egui::epaint::PathStroke::new(stroke, color),
            ));
            painter.circle_stroke(ip(0.5, 0.44), rect.width() * 0.13, s);
        }
        Icon::Sort => {
            painter.line_segment([ip(0.05, 0.25), ip(0.95, 0.25)], s);
            painter.line_segment([ip(0.05, 0.5), ip(0.7, 0.5)], s);
            painter.line_segment([ip(0.05, 0.75), ip(0.45, 0.75)], s);
        }
        Icon::GridSize => {
            let w = rect.width() * 0.3;
            let gap = rect.width() * 0.1;
            let left = rect.center().x - w - gap / 2.0;
            let top = rect.center().y - w - gap / 2.0;
            for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let r = Rect::from_min_size(
                    pos2(left + dx * (w + gap), top + dy * (w + gap)),
                    vec2(w, w),
                );
                painter.rect_filled(r, 1.0, color);
            }
        }
        Icon::Fullscreen => {
            let l = 0.22;
            // 四个角
            painter.line_segment([ip(0.0, l), ip(0.0, 0.0)], s);
            painter.line_segment([ip(0.0, 0.0), ip(l, 0.0)], s);
            painter.line_segment([ip(1.0 - l, 0.0), ip(1.0, 0.0)], s);
            painter.line_segment([ip(1.0, 0.0), ip(1.0, l)], s);
            painter.line_segment([ip(1.0, 1.0 - l), ip(1.0, 1.0)], s);
            painter.line_segment([ip(1.0, 1.0), ip(1.0 - l, 1.0)], s);
            painter.line_segment([ip(l, 1.0), ip(0.0, 1.0)], s);
            painter.line_segment([ip(0.0, 1.0), ip(0.0, 1.0 - l)], s);
        }
        Icon::Search => {
            let c = ip(0.4, 0.4);
            painter.circle_stroke(c, rect.width() * 0.26, s);
            painter.line_segment([pos2(c.x + 7.0, c.y + 7.0), ip(0.98, 0.98)], s);
        }
        Icon::ChevronRight => {
            painter.line_segment([ip(0.32, 0.12), ip(0.72, 0.5)], s);
            painter.line_segment([ip(0.72, 0.5), ip(0.32, 0.88)], s);
        }
        Icon::ChevronDown => {
            painter.line_segment([ip(0.12, 0.32), ip(0.5, 0.72)], s);
            painter.line_segment([ip(0.5, 0.72), ip(0.88, 0.32)], s);
        }
        // 五角星：侧栏「星标」行。原型这里用的是 `star` 而不是心形
        // （心形只出现在收藏按钮上：顶栏批量收藏、查看器单张收藏）。
        Icon::Star => {
            let c = rect.center();
            let outer = rect.width().min(rect.height()) * 0.5;
            if stroke <= 0.0 {
                draw_star(painter, c, outer, color);
            } else {
                let pts = star_points(c, outer, outer * 0.45).to_vec();
                painter.add(Shape::closed_line(
                    pts,
                    egui::epaint::PathStroke::new(stroke, color),
                ));
            }
        }
        Icon::Tray => {
            let body = Rect::from_min_max(ip(0.05, 0.42), ip(0.95, 0.95));
            painter.rect_stroke(body, 1.0, s, StrokeKind::Inside);
            painter.line_segment([ip(0.05, 0.42), ip(0.3, 0.12)], s);
            painter.line_segment([ip(0.3, 0.12), ip(0.7, 0.12)], s);
            painter.line_segment([ip(0.7, 0.12), ip(0.95, 0.42)], s);
        }
        Icon::Trash => {
            painter.line_segment([ip(0.1, 0.22), ip(0.9, 0.22)], s);
            painter.line_segment([ip(0.38, 0.22), ip(0.38, 0.08)], s);
            painter.line_segment([ip(0.38, 0.08), ip(0.62, 0.08)], s);
            painter.line_segment([ip(0.62, 0.08), ip(0.62, 0.22)], s);
            let body = Rect::from_min_max(ip(0.18, 0.22), ip(0.82, 0.95));
            painter.line_segment([body.left_top(), body.left_bottom()], s);
            painter.line_segment([body.right_top(), body.right_bottom()], s);
            painter.line_segment([body.left_bottom(), body.right_bottom()], s);
        }
        Icon::Info => {
            painter.circle_stroke(rect.center(), rect.width() * 0.4, s);
            painter.line_segment([ip(0.5, 0.42), ip(0.5, 0.75)], s);
            painter.circle_filled(ip(0.5, 0.25), stroke * 0.9, color);
        }
        Icon::Index => {
            // 时钟：圆环 + 时针/分针（"后台正在建索引"，不是"照片"也不是"设置"）
            let c = rect.center();
            painter.circle_stroke(c, rect.width() * 0.4, s);
            painter.line_segment([c, pos2(c.x, c.y - rect.height() * 0.24)], s);
            painter.line_segment(
                [
                    c,
                    pos2(c.x + rect.width() * 0.18, c.y + rect.height() * 0.08),
                ],
                s,
            );
        }
        Icon::Close => {
            painter.line_segment([ip(0.15, 0.15), ip(0.85, 0.85)], s);
            painter.line_segment([ip(0.85, 0.15), ip(0.15, 0.85)], s);
        }
        // 日历：圆角方框 + 顶部装订线 + 两个挂耳
        Icon::Calendar => {
            let body = Rect::from_min_max(ip(0.08, 0.2), ip(0.92, 0.94));
            painter.rect_stroke(body, 2.0, s, StrokeKind::Middle);
            painter.line_segment([ip(0.08, 0.4), ip(0.92, 0.4)], s);
            painter.line_segment([ip(0.3, 0.1), ip(0.3, 0.28)], s);
            painter.line_segment([ip(0.7, 0.1), ip(0.7, 0.28)], s);
        }
        // 文档：纸张轮廓 + 右上折角 + 两条正文线
        Icon::File => {
            let body = Rect::from_min_max(ip(0.2, 0.06), ip(0.8, 0.94));
            painter.rect_stroke(body, 2.0, s, StrokeKind::Middle);
            painter.line_segment([ip(0.58, 0.06), ip(0.8, 0.28)], s);
            painter.line_segment([ip(0.58, 0.06), ip(0.58, 0.28)], s);
            painter.line_segment([ip(0.58, 0.28), ip(0.8, 0.28)], s);
            painter.line_segment([ip(0.35, 0.52), ip(0.65, 0.52)], s);
            painter.line_segment([ip(0.35, 0.7), ip(0.65, 0.7)], s);
        }
        // 图片：方框 + 太阳 + 山脊
        Icon::Image => {
            let body = Rect::from_min_max(ip(0.06, 0.14), ip(0.94, 0.86));
            painter.rect_stroke(body, 2.0, s, StrokeKind::Middle);
            painter.circle_stroke(ip(0.34, 0.38), rect.width() * 0.08, s);
            painter.line_segment([ip(0.14, 0.76), ip(0.42, 0.5)], s);
            painter.line_segment([ip(0.42, 0.5), ip(0.62, 0.7)], s);
            painter.line_segment([ip(0.62, 0.7), ip(0.74, 0.58)], s);
            painter.line_segment([ip(0.74, 0.58), ip(0.9, 0.76)], s);
        }
        // 相机：机身 + 顶部取景凸起 + 镜头圈
        Icon::Camera => {
            let body = Rect::from_min_max(ip(0.04, 0.28), ip(0.96, 0.86));
            painter.rect_stroke(body, 3.0, s, StrokeKind::Middle);
            painter.line_segment([ip(0.3, 0.28), ip(0.36, 0.14)], s);
            painter.line_segment([ip(0.36, 0.14), ip(0.64, 0.14)], s);
            painter.line_segment([ip(0.64, 0.14), ip(0.7, 0.28)], s);
            painter.circle_stroke(ip(0.5, 0.57), rect.width() * 0.17, s);
        }
        // 文件夹：一个梯形口袋 + 一个标签页（16 px 下仍可辨认的最小形状）
        Icon::Folder => {
            painter.rect_stroke(
                Rect::from_min_max(ip(0.12, 0.30), ip(0.88, 0.82)),
                1.5,
                s,
                egui::StrokeKind::Middle,
            );
            // 标签页：左上角那一条
            painter.line_segment([ip(0.12, 0.30), ip(0.12, 0.20)], s);
            painter.line_segment([ip(0.12, 0.20), ip(0.40, 0.20)], s);
            painter.line_segment([ip(0.40, 0.20), ip(0.46, 0.30)], s);
        }
        // ---- M3.1 ----
        // 加/减号：短横线（±竖线），线帽圆一点更接近 macOS 细字形
        Icon::Plus => {
            let y = 0.5;
            painter.line_segment([ip(0.12, y), ip(0.88, y)], Stroke::new(stroke * 1.5, color));
            painter.line_segment(
                [ip(0.5, 0.12), ip(0.5, 0.88)],
                Stroke::new(stroke * 1.5, color),
            );
        }
        Icon::Minus => {
            painter.line_segment(
                [ip(0.12, 0.5), ip(0.88, 0.5)],
                Stroke::new(stroke * 1.5, color),
            );
        }
        // 左栏开关：圆角方框，内部左右两瓣（截图里的形状）
        Icon::SidebarToggle => {
            painter.rect_stroke(
                Rect::from_min_max(ip(0.08, 0.2), ip(0.92, 0.8)),
                3.0,
                Stroke::new(stroke * 1.2, color),
                StrokeKind::Middle,
            );
            painter.line_segment(
                [ip(0.36, 0.2), ip(0.36, 0.8)],
                Stroke::new(stroke * 1.2, color),
            );
        }
        // 排序：上箭头 + 下箭头并列
        Icon::SortArrows => {
            // 上箭头（左列）
            painter.line_segment(
                [ip(0.32, 0.85), ip(0.32, 0.15)],
                Stroke::new(stroke * 1.3, color),
            );
            painter.line_segment(
                [ip(0.32, 0.15), ip(0.14, 0.36)],
                Stroke::new(stroke * 1.3, color),
            );
            painter.line_segment(
                [ip(0.32, 0.15), ip(0.5, 0.36)],
                Stroke::new(stroke * 1.3, color),
            );
            // 下箭头（右列）
            painter.line_segment(
                [ip(0.68, 0.15), ip(0.68, 0.85)],
                Stroke::new(stroke * 1.3, color),
            );
            painter.line_segment(
                [ip(0.68, 0.85), ip(0.5, 0.64)],
                Stroke::new(stroke * 1.3, color),
            );
            painter.line_segment(
                [ip(0.68, 0.85), ip(0.86, 0.64)],
                Stroke::new(stroke * 1.3, color),
            );
        }
        // 列表：三横线，第二行短（截图样式）
        Icon::ListLines => {
            painter.line_segment(
                [ip(0.1, 0.28), ip(0.9, 0.28)],
                Stroke::new(stroke * 1.4, color),
            );
            painter.line_segment(
                [ip(0.1, 0.5), ip(0.9, 0.5)],
                Stroke::new(stroke * 1.4, color),
            );
            painter.line_segment(
                [ip(0.1, 0.72), ip(0.62, 0.72)],
                Stroke::new(stroke * 1.4, color),
            );
        }
        // 省略号：三个点
        Icon::Ellipsis => {
            let r = rect.width() * 0.045;
            for x in [0.22, 0.5, 0.78] {
                painter.circle_filled(ip(x, 0.5), r.max(1.2), color);
            }
        }
        // 分享：方框 + 从中缝向上的箭头
        Icon::Share => {
            painter.line_segment(
                [ip(0.22, 0.42), ip(0.22, 0.9)],
                Stroke::new(stroke * 1.2, color),
            );
            painter.line_segment(
                [ip(0.22, 0.9), ip(0.78, 0.9)],
                Stroke::new(stroke * 1.2, color),
            );
            painter.line_segment(
                [ip(0.78, 0.9), ip(0.78, 0.42)],
                Stroke::new(stroke * 1.2, color),
            );
            painter.line_segment(
                [ip(0.5, 0.08), ip(0.5, 0.62)],
                Stroke::new(stroke * 1.2, color),
            );
            painter.line_segment(
                [ip(0.5, 0.08), ip(0.32, 0.26)],
                Stroke::new(stroke * 1.2, color),
            );
            painter.line_segment(
                [ip(0.5, 0.08), ip(0.68, 0.26)],
                Stroke::new(stroke * 1.2, color),
            );
        }
        // 心形：原型 `M19.5 12.6 12 20l-7.5-7.4A4.5 4.5 0 0 1 12 6.2a4.5 4.5 0 0 1 7.5 6.4z`
        // 的等价折线（两段半径 4.5 的真圆弧 + 茎部直线）。**stroke <= 0 表示实心**：
        // 收藏态是实心、未收藏是描边（原型 `icon("heart", 15, starred ? 0 : 1.75)`）。
        Icon::Heart => {
            let pts = heart_points(rect);
            if stroke <= 0.0 {
                // 凹多边形不能用 `convex_polygon`：走中心顶点扇形 Mesh（同 `draw_star`）。
                let mut mesh = Mesh::default();
                let n = pts.len() as f32;
                let cx = pts.iter().map(|p| p.x).sum::<f32>() / n;
                let cy = pts.iter().map(|p| p.y).sum::<f32>() / n;
                mesh.colored_vertex(pos2(cx, cy), color); // #0 = 形心
                for &p in &pts {
                    mesh.colored_vertex(p, color);
                }
                for i in 0..pts.len() {
                    mesh.add_triangle(0, 1 + i as u32, 1 + ((i + 1) % pts.len()) as u32);
                }
                painter.add(mesh);
            } else {
                painter.add(Shape::closed_line(
                    pts,
                    egui::epaint::PathStroke::new(stroke, color),
                ));
            }
        }
        // 相簿/书本：打开的书（两页）
        Icon::Album => {
            painter.line_segment(
                [ip(0.5, 0.2), ip(0.5, 0.82)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.5, 0.24), ip(0.12, 0.18)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.12, 0.18), ip(0.12, 0.78)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.12, 0.78), ip(0.5, 0.82)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.5, 0.24), ip(0.88, 0.18)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.88, 0.18), ip(0.88, 0.78)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.88, 0.78), ip(0.5, 0.82)],
                Stroke::new(stroke * 1.1, color),
            );
        }
        // 照片堆叠：两张错开的圆角矩形
        Icon::PhotoStack => {
            painter.rect_stroke(
                Rect::from_min_max(ip(0.26, 0.1), ip(0.94, 0.62)),
                2.0,
                Stroke::new(stroke * 1.0, color),
                StrokeKind::Middle,
            );
            painter.rect_stroke(
                Rect::from_min_max(ip(0.06, 0.34), ip(0.74, 0.9)),
                2.0,
                Stroke::new(stroke * 1.3, color),
                StrokeKind::Middle,
            );
        }
        // 地图：折页三折
        Icon::Map => {
            painter.line_segment(
                [ip(0.08, 0.22), ip(0.36, 0.12)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.36, 0.12), ip(0.64, 0.22)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.64, 0.22), ip(0.92, 0.12)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.08, 0.22), ip(0.08, 0.86)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.08, 0.86), ip(0.36, 0.76)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.36, 0.76), ip(0.36, 0.12)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.36, 0.76), ip(0.64, 0.86)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.64, 0.86), ip(0.64, 0.22)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.64, 0.86), ip(0.92, 0.76)],
                Stroke::new(stroke * 1.1, color),
            );
            painter.line_segment(
                [ip(0.92, 0.76), ip(0.92, 0.12)],
                Stroke::new(stroke * 1.1, color),
            );
        }
        // 摄像机：圆角矩形 + 右侧三角镜头
        Icon::Video => {
            painter.rect_stroke(
                Rect::from_min_max(ip(0.06, 0.28), ip(0.66, 0.78)),
                2.5,
                Stroke::new(stroke * 1.2, color),
                StrokeKind::Middle,
            );
            painter.add(Shape::convex_polygon(
                vec![
                    ip(0.68, 0.42),
                    ip(0.94, 0.26),
                    ip(0.94, 0.78),
                    ip(0.68, 0.6),
                ],
                Stroke::NONE.color,
                Stroke::NONE,
            ));
            painter.add(Shape::line(
                vec![
                    ip(0.68, 0.42),
                    ip(0.94, 0.26),
                    ip(0.94, 0.78),
                    ip(0.68, 0.6),
                ],
                egui::epaint::PathStroke::new(stroke * 1.1, color),
            ));
        }
        // 人物：头圆 + 肩弧
        Icon::Person => {
            painter.circle_stroke(
                ip(0.5, 0.32),
                rect.width() * 0.17,
                Stroke::new(stroke * 1.2, color),
            );
            painter.line_segment(
                [ip(0.14, 0.92), ip(0.14, 0.84)],
                Stroke::new(stroke * 1.2, color),
            );
            // 肩：一条弧线近似（三段折线 + 圆头）
            painter.add(Shape::line(
                vec![
                    ip(0.14, 0.9),
                    ip(0.24, 0.62),
                    ip(0.5, 0.56),
                    ip(0.76, 0.62),
                    ip(0.86, 0.9),
                ],
                egui::epaint::PathStroke::new(stroke * 1.2, color),
            ));
        }
        // ---- M3.1 §7：首页工具栏 ----
        // 导入：托盘开口 + 向下箭头（“放进图库”）
        Icon::Import => {
            let s2 = Stroke::new(stroke * 1.2, color);
            painter.line_segment([ip(0.1, 0.62), ip(0.1, 0.9)], s2);
            painter.line_segment([ip(0.1, 0.9), ip(0.9, 0.9)], s2);
            painter.line_segment([ip(0.9, 0.9), ip(0.9, 0.62)], s2);
            painter.line_segment([ip(0.5, 0.08), ip(0.5, 0.66)], s2);
            painter.line_segment([ip(0.5, 0.66), ip(0.3, 0.46)], s2);
            painter.line_segment([ip(0.5, 0.66), ip(0.7, 0.46)], s2);
        }
        // 幻灯片：圆角屏幕 + 播放三角
        Icon::Slideshow => {
            painter.rect_stroke(
                Rect::from_min_max(ip(0.06, 0.18), ip(0.94, 0.82)),
                2.0,
                Stroke::new(stroke * 1.2, color),
                StrokeKind::Middle,
            );
            painter.add(Shape::convex_polygon(
                vec![ip(0.42, 0.36), ip(0.42, 0.64), ip(0.68, 0.5)],
                color,
                Stroke::NONE,
            ));
        }
        // 编辑：铅笔（斜杆 + 笔尖）
        Icon::Edit => {
            let s2 = Stroke::new(stroke * 1.2, color);
            painter.line_segment([ip(0.18, 0.82), ip(0.72, 0.28)], s2);
            painter.line_segment([ip(0.72, 0.28), ip(0.84, 0.4)], s2);
            painter.line_segment([ip(0.84, 0.4), ip(0.3, 0.94)], s2);
            painter.line_segment([ip(0.3, 0.94), ip(0.18, 0.82)], s2);
            painter.line_segment([ip(0.18, 0.82), ip(0.3, 0.94)], s2);
        }
        // 排布：方格（2×2 圆角方块）
        Icon::LayoutGrid => {
            for (x, y) in [(0.08, 0.08), (0.55, 0.08), (0.08, 0.55), (0.55, 0.55)] {
                painter.rect_filled(
                    Rect::from_min_max(ip(x, y), ip(x + 0.37, y + 0.37)),
                    1.5,
                    color,
                );
            }
        }
        // 排布：纵向瀑布流（三列高低错落）
        Icon::LayoutMasonryV => {
            let col_w = 0.2;
            for (x, top, bottom) in [(0.08, 0.08, 0.62), (0.40, 0.08, 0.92), (0.72, 0.08, 0.42)] {
                painter.rect_filled(
                    Rect::from_min_max(ip(x, top), ip(x + col_w, bottom)),
                    1.5,
                    color,
                );
            }
        }
        // 排布：横向瀑布流（三行宽窄错落）
        Icon::LayoutMasonryH => {
            let row_h = 0.2;
            for (x, top, right) in [(0.08, 0.08, 0.62), (0.08, 0.40, 0.92), (0.08, 0.72, 0.42)] {
                painter.rect_filled(
                    Rect::from_min_max(ip(x, top), ip(right, top + row_h)),
                    1.5,
                    color,
                );
            }
        }
        // M4.1：标签（吊牌：圆角多边形轮廓 + 左上角圆孔）
        Icon::Tag => {
            painter.line_segment([ip(0.12, 0.16), ip(0.52, 0.16)], s);
            painter.line_segment([ip(0.12, 0.16), ip(0.12, 0.52)], s);
            painter.line_segment([ip(0.12, 0.52), ip(0.52, 0.88)], s);
            painter.line_segment([ip(0.52, 0.16), ip(0.88, 0.52)], s);
            painter.line_segment([ip(0.88, 0.52), ip(0.52, 0.88)], s);
            painter.circle_filled(ip(0.30, 0.34), rect.width() * 0.055, color);
        }
        // M4.1：说明（圆角对话气泡 + 两条横线 + 尾部小尖）
        Icon::Caption => {
            painter.line_segment([ip(0.10, 0.20), ip(0.90, 0.20)], s);
            painter.line_segment([ip(0.10, 0.20), ip(0.10, 0.68)], s);
            painter.line_segment([ip(0.10, 0.68), ip(0.30, 0.68)], s);
            painter.line_segment([ip(0.30, 0.68), ip(0.24, 0.88)], s);
            painter.line_segment([ip(0.24, 0.88), ip(0.44, 0.68)], s);
            painter.line_segment([ip(0.44, 0.68), ip(0.90, 0.68)], s);
            painter.line_segment([ip(0.90, 0.20), ip(0.90, 0.68)], s);
            painter.line_segment([ip(0.24, 0.36), ip(0.76, 0.36)], s);
            painter.line_segment([ip(0.24, 0.52), ip(0.64, 0.52)], s);
        }
    }
}

/// 图钉（定位）的水滴轮廓，按原型 `locate` 的 `<path d="M12 21s7-6.2 7-11a7 7
/// 0 1 0-14 0c0 4.8 7 11 7 11z"/>` 换算（viewBox 0 0 24 24，闭合折线，首尾不重复）。
/// 顶部是半径 7 的圆弧、底部收成尖角，中心圆孔由调用方另画。
pub fn locate_points(rect: Rect) -> Vec<Pos2> {
    const NORM: [(f32, f32); 14] = [
        (0.500, 0.875),
        (0.275, 0.602),
        (0.223, 0.506),
        (0.209, 0.398),
        (0.236, 0.292),
        (0.300, 0.204),
        (0.393, 0.145),
        (0.500, 0.125),
        (0.607, 0.145),
        (0.700, 0.204),
        (0.764, 0.292),
        (0.791, 0.398),
        (0.777, 0.506),
        (0.725, 0.602),
    ];
    NORM.iter()
        .map(|(x, y)| {
            pos2(
                rect.left() + x * rect.width(),
                rect.top() + y * rect.height(),
            )
        })
        .collect()
}

/// 心形的轮廓顶点（**闭合**，首尾不重复），按原型 SVG path 换算成 0–1 归一化坐标。
///
/// 原型：`M19.5 12.6 12 20l-7.5-7.4A4.5 4.5 0 0 1 12 6.2a4.5 4.5 0 0 1 7.5 6.4z`
/// （viewBox 0 0 24 24）。顺序照 path 走：右肩 → 直线到心尖 (12,20) → 直线到左肩
/// (4.5,12.6) → 左瓣半圆到凹口 (12,6.2) → 右瓣半圆回到右肩。
///
/// 两段 `A4.5` 声明的半径 4.5 小于弦长 9.86/2，SVG 规范要求等比放大到 4.929，
/// 圆心分别是 (8.25,9.4) 与 (15.75,9.4)；各取半圆 10 段折线逼近。
/// 外接框 0.139–0.861 × 0.187–0.833，视觉重心略高于中线，与原型一致。
///
/// 保留为**纯函数**便于测试：顶点顺序必须是连续的（相邻点距离远小于半个图形），
/// 且相对形心同向绕行（扇形三角化不产生自交）。
pub fn heart_points(rect: Rect) -> Vec<Pos2> {
    const NORM: [(f32, f32); 22] = [
        (0.8125, 0.5250),
        (0.5000, 0.8333),
        (0.1875, 0.5250),
        (0.1539, 0.4702),
        (0.1390, 0.4077),
        (0.1440, 0.3436),
        (0.1687, 0.2843),
        (0.2104, 0.2354),
        (0.2652, 0.2019),
        (0.3277, 0.1869),
        (0.3918, 0.1920),
        (0.4512, 0.2166),
        (0.5000, 0.2583),
        (0.5488, 0.2166),
        (0.6082, 0.1920),
        (0.6723, 0.1869),
        (0.7348, 0.2019),
        (0.7896, 0.2354),
        (0.8313, 0.2843),
        (0.8560, 0.3436),
        (0.8610, 0.4077),
        (0.8461, 0.4702),
    ];
    NORM.iter()
        .map(|(x, y)| {
            pos2(
                rect.left() + x * rect.width(),
                rect.top() + y * rect.height(),
            )
        })
        .collect()
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
        // 第一个点在正上方
        assert!((pts[0].x - c.x).abs() < 0.001);
        assert!(pts[0].y < c.y);
        assert_eq!(pts.len(), 10);
    }

    /// 心形轮廓：顶点连续（相邻点不会跳半个图形）、同向绕形心、跨满整个 rect。
    ///
    /// 这条钉住的是**形状本身**：旧实现是两个圆 + 一个三角，渲染成实心药丸；
    /// 现在的 21 点折线来自原型的两段真圆弧，凹口在正上方中央。
    #[test]
    fn heart_points_form_a_continuous_concave_outline() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(24.0, 24.0));
        let pts = heart_points(rect);
        assert_eq!(pts.len(), 22, "原型 path 换算出的顶点数");
        // 相邻点距离不超过对角线的一半（不能跨跳）
        let diag = rect.size().length();
        for i in 0..pts.len() {
            let a = pts[i];
            let b = pts[(i + 1) % pts.len()];
            let d = (b - a).length();
            assert!(d > 0.0 && d < diag * 0.5, "第 {i} 段长度异常：{d}");
        }
        // 凹口：正上方中央应有一个点**低于**左右两肩（y 更大 = 更靠下）
        let cx = rect.center().x;
        let notch = pts
            .iter()
            .min_by(|a, b| {
                let da = (a.x - cx).abs();
                let db = (b.x - cx).abs();
                da.partial_cmp(&db).unwrap()
            })
            .unwrap();
        let shoulder = pts
            .iter()
            .filter(|p| p.y < notch.y)
            .count();
        assert!(shoulder >= 2, "心形上半应有比凹口更高的两肩");
        // 凹口必须在正中偏上：x 落在 0.45–0.55，且比两肩低至少 5% 高度
        let cx_n = (notch.x - rect.left()) / rect.width();
        let cy_n = (notch.y - rect.top()) / rect.height();
        assert!(
            (0.45..=0.55).contains(&cx_n),
            "凹口应在水平中央，实际 x={cx_n}"
        );
        let highest = pts
            .iter()
            .map(|p| (p.y - rect.top()) / rect.height())
            .fold(f32::MAX, f32::min);
        assert!(
            cy_n - highest > 0.05,
            "凹口要比两肩低出可感知的深度：凹口 y={cy_n}，最高肩 y={highest}"
        );
        // 心尖在正中、且是全图最低点（心形下半是两条直线收成一点）
        let tip = pts
            .iter()
            .max_by(|a, b| a.y.partial_cmp(&b.y).unwrap())
            .unwrap();
        assert!(((tip.x - rect.center().x).abs()) < rect.width() * 0.02, "心尖应在正中");
        assert!(
            (tip.y - rect.top()) / rect.height() > 0.80,
            "心尖应落到接近底部"
        );
        // 外接框：跨满大半，但留出描边余量
        let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in &pts {
            x0 = x0.min(p.x);
            x1 = x1.max(p.x);
            y0 = y0.min(p.y);
            y1 = y1.max(p.y);
        }
        assert!(x1 - x0 > rect.width() * 0.6, "心形太窄");
        assert!(y1 - y0 > rect.height() * 0.6, "心形太扁");
        assert!(x0 > rect.left() && x1 < rect.right());
        assert!(y0 > rect.top() && y1 < rect.bottom());
    }

    /// 定位图钉：水滴尖在正下方中央，顶部圆弧左右对称，中心留出圆孔位置。
    #[test]
    fn locate_points_are_a_symmetric_pin_with_a_bottom_tip() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(24.0, 24.0));
        let pts = locate_points(rect);
        assert_eq!(pts.len(), 14);
        let tip = pts[0];
        assert!(
            (tip.x - rect.center().x).abs() < 0.01,
            "水滴尖应在中线：{tip:?}"
        );
        assert!(tip.y > rect.bottom() - rect.height() * 0.2, "尖角应在底部");
        // 左右对称：每个点都能找到镜像点
        for p in &pts {
            let mirror_x = 2.0 * rect.center().x - p.x;
            assert!(
                pts.iter()
                    .any(|q| (q.x - mirror_x).abs() < 0.01 && (q.y - p.y).abs() < 0.01),
                "缺少镜像点：{p:?}"
            );
        }
    }

    #[test]
    fn draw_star_mesh_is_valid_and_single_tone() {
        // 用离屏 Painter 不方便，这里验证几何生成的 mesh 结构：10 顶点扇形
        // 由纯函数保证；直接构造同样的 mesh 检查索引都在范围内（is_valid 语义）。
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
