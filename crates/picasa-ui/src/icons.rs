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

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    /// 返回（←）。M1 里恒为禁用态——没有别的「房间」可返回。
    Back,
    /// 添加文件夹（folder + plus）
    AddFolder,
    /// 在 Finder 中显示（放大镜）
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
        Icon::Reveal => {
            let c = ip(0.42, 0.42);
            painter.circle_stroke(c, rect.width() * 0.24, s);
            painter.line_segment([pos2(c.x + 6.0, c.y + 6.0), ip(0.95, 0.95)], s);
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
            painter.line_segment([c, pos2(c.x + rect.width() * 0.18, c.y + rect.height() * 0.08)], s);
        }
        Icon::Close => {
            painter.line_segment([ip(0.15, 0.15), ip(0.85, 0.85)], s);
            painter.line_segment([ip(0.85, 0.15), ip(0.15, 0.85)], s);
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
            painter.line_segment([ip(0.32, 0.85), ip(0.32, 0.15)], Stroke::new(stroke * 1.3, color));
            painter.line_segment([ip(0.32, 0.15), ip(0.14, 0.36)], Stroke::new(stroke * 1.3, color));
            painter.line_segment([ip(0.32, 0.15), ip(0.5, 0.36)], Stroke::new(stroke * 1.3, color));
            // 下箭头（右列）
            painter.line_segment([ip(0.68, 0.15), ip(0.68, 0.85)], Stroke::new(stroke * 1.3, color));
            painter.line_segment([ip(0.68, 0.85), ip(0.5, 0.64)], Stroke::new(stroke * 1.3, color));
            painter.line_segment([ip(0.68, 0.85), ip(0.86, 0.64)], Stroke::new(stroke * 1.3, color));
        }
        // 列表：三横线，第二行短（截图样式）
        Icon::ListLines => {
            painter.line_segment([ip(0.1, 0.28), ip(0.9, 0.28)], Stroke::new(stroke * 1.4, color));
            painter.line_segment([ip(0.1, 0.5), ip(0.9, 0.5)], Stroke::new(stroke * 1.4, color));
            painter.line_segment([ip(0.1, 0.72), ip(0.62, 0.72)], Stroke::new(stroke * 1.4, color));
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
            painter.line_segment([ip(0.22, 0.42), ip(0.22, 0.9)], Stroke::new(stroke * 1.2, color));
            painter.line_segment([ip(0.22, 0.9), ip(0.78, 0.9)], Stroke::new(stroke * 1.2, color));
            painter.line_segment([ip(0.78, 0.9), ip(0.78, 0.42)], Stroke::new(stroke * 1.2, color));
            painter.line_segment([ip(0.5, 0.08), ip(0.5, 0.62)], Stroke::new(stroke * 1.2, color));
            painter.line_segment([ip(0.5, 0.08), ip(0.32, 0.26)], Stroke::new(stroke * 1.2, color));
            painter.line_segment([ip(0.5, 0.08), ip(0.68, 0.26)], Stroke::new(stroke * 1.2, color));
        }
        // 心形：两段贝塞尔近似（egui 无贝塞尔，用三点二次近似）
        Icon::Heart => {
            let c = ip(0.5, 0.38);
            painter.circle_filled(ip(0.28, 0.36), rect.width() * 0.19, color);
            painter.circle_filled(ip(0.72, 0.36), rect.width() * 0.19, color);
            let bottom = ip(0.5, 0.88);
            painter.add(Shape::convex_polygon(
                vec![ip(0.12, 0.44), ip(0.88, 0.44), bottom],
                color,
                Stroke::NONE,
            ));
            let _ = c;
        }
        // 相簿/书本：打开的书（两页）
        Icon::Album => {
            painter.line_segment([ip(0.5, 0.2), ip(0.5, 0.82)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.5, 0.24), ip(0.12, 0.18)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.12, 0.18), ip(0.12, 0.78)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.12, 0.78), ip(0.5, 0.82)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.5, 0.24), ip(0.88, 0.18)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.88, 0.18), ip(0.88, 0.78)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.88, 0.78), ip(0.5, 0.82)], Stroke::new(stroke * 1.1, color));
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
            painter.line_segment([ip(0.08, 0.22), ip(0.36, 0.12)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.36, 0.12), ip(0.64, 0.22)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.64, 0.22), ip(0.92, 0.12)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.08, 0.22), ip(0.08, 0.86)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.08, 0.86), ip(0.36, 0.76)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.36, 0.76), ip(0.36, 0.12)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.36, 0.76), ip(0.64, 0.86)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.64, 0.86), ip(0.64, 0.22)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.64, 0.86), ip(0.92, 0.76)], Stroke::new(stroke * 1.1, color));
            painter.line_segment([ip(0.92, 0.76), ip(0.92, 0.12)], Stroke::new(stroke * 1.1, color));
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
                vec![ip(0.68, 0.42), ip(0.94, 0.26), ip(0.94, 0.78), ip(0.68, 0.6)],
                Stroke::NONE.color,
                Stroke::NONE,
            ));
            painter.add(Shape::line(
                vec![ip(0.68, 0.42), ip(0.94, 0.26), ip(0.94, 0.78), ip(0.68, 0.6)],
                egui::epaint::PathStroke::new(stroke * 1.1, color),
            ));
        }
        // 人物：头圆 + 肩弧
        Icon::Person => {
            painter.circle_stroke(ip(0.5, 0.32), rect.width() * 0.17, Stroke::new(stroke * 1.2, color));
            painter.line_segment(
                [ip(0.14, 0.92), ip(0.14, 0.84)],
                Stroke::new(stroke * 1.2, color),
            );
            // 肩：一条弧线近似（三段折线 + 圆头）
            painter.add(Shape::line(
                vec![ip(0.14, 0.9), ip(0.24, 0.62), ip(0.5, 0.56), ip(0.76, 0.62), ip(0.86, 0.9)],
                egui::epaint::PathStroke::new(stroke * 1.2, color),
            ));
        }
    }
}
