//! 底部状态栏（U-33~U-35）。
//!
//! 左：「当前视图名 · 张数」；右：「选中 N / 扫描进度」。

use eframe::egui::{self, Align2, FontId, Rect, Ui, pos2, vec2};
use picasa_core::Library;

use crate::theme::{Metrics, Theme};

pub fn show(ui: &mut Ui, lib: &Library, theme: &Theme) {
    let rect = Rect::from_min_size(
        ui.max_rect().left_top(),
        vec2(ui.max_rect().width(), Metrics::STATUS_H),
    );
    let _ = ui.allocate_space(rect.size());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme.chrome_bg);
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        egui::Stroke::new(1.0, theme.separator),
    );

    let font = FontId::new(11.0, egui::FontFamily::Proportional);

    // 左：「视图名 · N 张」
    let left = format!("{} · {} 张", lib.view_title(), lib.photos.len());
    painter.text(
        pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        left,
        font.clone(),
        theme.status_text,
    );

    // 右侧依次：扫描进度 / 选中数
    let mut x = rect.right() - 10.0;
    if lib.scanning {
        let text = match lib.scan_progress {
            Some((photos, dirs)) => format!("正在索引 {photos} 张 / {dirs} 个目录"),
            None => "正在索引…".to_string(),
        };
        painter.text(
            pos2(x, rect.center().y),
            Align2::RIGHT_CENTER,
            text,
            font.clone(),
            theme.status_text,
        );
        x -= 220.0;
    }

    if lib.selected_count() > 0 {
        painter.text(
            pos2(x, rect.center().y),
            Align2::RIGHT_CENTER,
            format!("已选 {} 张", lib.selected_count()),
            font,
            theme.status_text,
        );
    }
}
