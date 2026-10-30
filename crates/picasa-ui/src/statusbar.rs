//! 底部状态栏（U-33~U-35）+ **索引进度入口**（M2-FD-37，`docs/04` §2.2）。
//!
//! 左：「当前视图名 · 张数」（索引中的文件夹显示 **已就绪 / 已发现** 双数，
//! 因为"只报一个数"会让用户误判目录规模，`04` §3.2）。
//! 右：`[索引进度]` 常驻按钮（四态）+ 选中数。
//!
//! 入口**放在状态栏右区**：不占栅格宽度，也不动左栏布局——这是 `04` §2.2 的硬约束，
//! 因为进度 UI 的本分是"如实呈现"，不是重新排版整个界面。

use eframe::egui::{self, Align2, FontId, Rect, Ui, pos2, vec2};
use picasa_core::{IndexSnapshot, Library, View};

use crate::index_progress::IndexProgressUi;
use crate::theme::{Metrics, Theme};

/// 状态栏的一帧输出（只有「入口被点」需要上层处理）。
#[derive(Clone, Copy, Debug, Default)]
pub struct StatusbarOut {
    pub index_clicked: bool,
    pub button_rect: Option<Rect>,
}

/// `索引进度` 按钮宽度（图标 + 文案 + 状态点）。定值，避免文字长度影响布局。
const INDEX_BTN_W: f32 = 86.0;

pub fn show(
    ui: &mut Ui,
    lib: &Library,
    theme: &Theme,
    snap: &IndexSnapshot,
) -> StatusbarOut {
    let mut out = StatusbarOut::default();
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

    // 左：「视图名 · N 张」／索引中的文件夹用「已就绪 X / 已发现 Y」
    let left = match double_count_text(lib, snap) {
        Some(counts) => format!("{} · {counts}", lib.view_title()),
        None => format!("{} · {} 张", lib.view_title(), lib.photos.len()),
    };
    painter.text(
        pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        left,
        font.clone(),
        theme.status_text,
    );

    // 右区：入口按钮 → 选中数
    let btn = Rect::from_center_size(
        pos2(rect.right() - 8.0 - INDEX_BTN_W / 2.0, rect.center().y),
        vec2(INDEX_BTN_W, Metrics::STATUS_H - 4.0),
    );
    out.button_rect = Some(btn);
    out.index_clicked = IndexProgressUi::status_button(ui, theme, snap, btn);

    let mut x = btn.left() - 10.0;
    if lib.selected_count() > 0 {
        painter.text(
            pos2(x, rect.center().y),
            Align2::RIGHT_CENTER,
            format!("已选 {} 张", lib.selected_count()),
            font,
            theme.status_text,
        );
        x -= 90.0;
    }
    let _ = x;
    out
}

/// 当前视图有对应索引任务时，返回 `已就绪 X / 已发现 Y`（`04` §3.2 第四条）。
///
/// 只对**文件夹视图**生效：在「所有照片」下把整库分母摆到状态栏上，
/// 会和左栏的库总数打架（一个是视图口径、一个是任务口径）。
fn double_count_text(lib: &Library, snap: &IndexSnapshot) -> Option<String> {
    let View::Folder(id) = lib.view else {
        return None;
    };
    let path = lib.folder_path(id)?;
    let task = snap.task_by_path(&path)?;
    Some(format!("已就绪 {} / 已发现 {}", task.completed, task.discovered))
}
