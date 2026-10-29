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

use eframe::egui::{Color32, Painter, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};

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
    /// 关闭
    Close,
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
        Icon::Close => {
            painter.line_segment([ip(0.15, 0.15), ip(0.85, 0.85)], s);
            painter.line_segment([ip(0.85, 0.15), ip(0.15, 0.85)], s);
        }
    }
}
