//! 底部状态栏（U-33~U-35）+ **唯一的索引入口**（M2.2 §2.6 / M3.1 §7.5）。
//!
//! ## §7.5 定稿
//!
//! * 左：本地信任提示「本地 · 不改原文件」；
//! * 中：视图语境计数、已选数等（精简，不重复标题副标题）；
//! * **最右**：「索引管理」——既有索引管理流程的唯一显式按钮入口。
//!
//! ## 三个入口合成一个（**过渡期：旧「诊断」入口暂时保留**）
//!
//! 之前这里有「索引进度」和「诊断」两个按钮，工具栏还有一个「索引管理」二级菜单。
//! 三个入口的问题是**功能会漏**（R-M22-5）：加一个操作得记得在三处都加一遍。
//! 现在索引只剩这一个按钮，点开是四个标签的索引管理页（[`crate::index_mgr`]）。
//!
//! ⚠️ **「诊断」按钮为什么还在**（2026-11-01 评审 §1.1 / 规格 §9 迭代 3）：
//! 规格的迁移顺序是「新页面功能齐备 → 并行一段时间 → 再删旧入口」，不是反过来。
//! 而对账发现新页面的「存储与诊断」标签还有**两项没齐**：
//! 诊断日志导出（§4.9）未接线、VACUUM 无进度也无取消（IDX-010）。
//! 在补齐之前删旧入口 = 让这两项在界面上彻底不可达，直接违反 R-M22-5「零功能丢失」。
//! 所以旧入口**恢复**，等这两项落地后再按规格顺序删。
//!
//! 入口**放在状态栏右区**：不占栅格宽度，也不动左栏布局——这是 `04` §2.2 的硬约束，
//! 因为进度 UI 的本分是"如实呈现"，不是重新排版整个界面。
//!
//! ## 四态互斥（§2.6）
//!
//! | 态 | 条件 |
//! | --- | --- |
//! | 空闲 | 无任务、角标 = 0 |
//! | 进行中 | 有 `Running` 任务（+ 环形指示，**无百分比数字**） |
//! | 系统暂停 | 任意任务 `Paused` |
//! | 需要处理 | 角标 > 0 |
//!
//! 判定的**唯一实现**在 [`picasa_core::MgmtSnapshot::entry_state`]，这里只负责画——
//! 「失败优先于进度」（R-M2-4）那条规则不能有两份。

use eframe::egui::{self, Align2, FontId, Rect, Ui, pos2, vec2};
use picasa_core::{IndexEntryState, IndexSnapshot, Library, MgmtSnapshot, View};

use crate::theme::{Metrics, Theme};

/// 状态栏的一帧输出（只有「入口被点」需要上层处理）。
///
/// ## M3.1 变更
///
/// 缩放滑块已从状态栏移除（用户指令：滑块改成顶栏的 −/+ 按钮）。
/// `zoom_drag` / `zoom_released` 字段与滑块绘制一并退役。
#[derive(Clone, Debug, Default)]
pub struct StatusbarOut {
    /// 索引入口被点（打开索引管理页）
    pub index_clicked: bool,
    /// 「诊断」入口被点（F 加固）。**过渡期保留**，见文件头注释。
    pub diag_clicked: bool,
    pub button_rect: Option<Rect>,
}

/// 索引入口按钮宽度（图标 + 文案 + 角标）。定值，避免文字长度影响布局。
const INDEX_BTN_W: f32 = 104.0;
/// 「诊断」按钮宽度
const DIAG_BTN_W: f32 = 52.0;
/// 角标圆点直径
const BADGE_D: f32 = 14.0;

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    lib: &Library,
    theme: &Theme,
    snap: &IndexSnapshot,
    mgmt: &MgmtSnapshot,
    // §4.8 启动校验的进度文案（空 = 没在跑）
    verify_text: &str,
    // M3.1 起不再使用：缩放改为顶栏 −/+ 按钮（参数保留以减少调用点改动）
    _zoom_px: f32,
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

    // 左：本地信任提示（§7.5，吸收方案 C）——「本地 · 不改原文件」
    let trust = "本地 · 不改原文件";
    let trust_galley = painter.layout_no_wrap(trust.to_owned(), font.clone(), theme.status_text);
    painter.galley(
        pos2(
            rect.left() + 10.0,
            rect.center().y - trust_galley.size().y / 2.0,
        ),
        trust_galley.clone(),
        theme.status_text,
    );
    let sep_x = rect.left() + 10.0 + trust_galley.size().x + 10.0;
    painter.line_segment(
        [
            pos2(sep_x, rect.top() + 5.0),
            pos2(sep_x, rect.bottom() - 5.0),
        ],
        egui::Stroke::new(1.0, theme.separator),
    );

    // 中左：视图语境计数（与标题副标题同口径，但精简，不重复堆砌）。
    // ⚠️ 用 `photos_total` 而不是 `photos.len()`：C 包分页后 `photos` 只是已加载的前几页。
    let total = lib.photos_total.max(lib.photos.len());
    let view_text = match double_count_text(lib, snap) {
        Some(counts) => format!("{} · {counts}", lib.view_title()),
        None => format!("{} · {total} 张", lib.view_title()),
    };
    painter.text(
        pos2(sep_x + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        view_text,
        font.clone(),
        theme.status_text,
    );

    // 右区：唯一入口 → 选中数
    let btn = Rect::from_center_size(
        pos2(rect.right() - 8.0 - INDEX_BTN_W / 2.0, rect.center().y),
        vec2(INDEX_BTN_W, Metrics::STATUS_H - 4.0),
    );
    out.button_rect = Some(btn);
    out.index_clicked = index_entry_button(ui, theme, mgmt, btn, &font, verify_text);

    // 诊断入口（过渡期保留）：放在索引入口左边，不占栅格宽度、不动左栏。
    // 它承载的两件事（诊断日志导出、带进度的 VACUUM）在新的「存储与诊断」标签里还没齐。
    let diag = Rect::from_center_size(
        pos2(btn.left() - 6.0 - DIAG_BTN_W / 2.0, rect.center().y),
        vec2(DIAG_BTN_W, Metrics::STATUS_H - 6.0),
    );
    let dresp = ui.interact(diag, ui.scope_id().with("diag-btn"), egui::Sense::click());
    let dpainter = ui.painter_at(diag);
    if dresp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        dpainter.rect_filled(diag, 2.0, theme.cell_hover);
    }
    dpainter.text(
        diag.center(),
        Align2::CENTER_CENTER,
        "诊断",
        font.clone(),
        theme.status_text,
    );
    out.diag_clicked = dresp.clicked();

    // ---- 缩放滑块已于 M3.1 移除（顶栏 −/+ 按钮替代）----
    // 原滑块占位（diag 左侧 144px）不再保留，状态栏右侧依次是：
    // 「已选 N 张」→ 诊断 → 索引入口。
    if lib.selected_count() > 0 {
        painter.text(
            pos2(diag.left() - 10.0, rect.center().y),
            Align2::RIGHT_CENTER,
            format!("已选 {} 张", lib.selected_count()),
            font,
            theme.status_text,
        );
    }
    out
}

/// 画唯一入口：四态 + 角标。右键**无菜单**（§2.6）。
fn index_entry_button(
    ui: &mut Ui,
    theme: &Theme,
    mgmt: &MgmtSnapshot,
    rect: Rect,
    font: &FontId,
    // §4.8 启动校验的进度文案（空 = 没在跑）
    verify_text: &str,
) -> bool {
    let painter = ui.painter_at(rect);
    let resp = ui.interact(
        rect,
        ui.scope_id().with("index-entry"),
        egui::Sense::click(),
    );
    let state = mgmt.entry_state();

    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        painter.rect_filled(rect, 2.0, theme.cell_hover);
    }

    let (label, color) = match state {
        // §7.5：入口文案固定为「索引管理」，状态只体现在颜色/指示器/角标上，
        // 避免用户找不到「索引管理」四个字。
        IndexEntryState::Idle => ("索引管理", theme.status_text),
        IndexEntryState::Running => ("索引管理", theme.progress),
        IndexEntryState::Paused => ("索引管理", theme.warn),
        IndexEntryState::Attention => ("索引管理", theme.danger),
    };

    // 进行中：轻量环形指示（**无百分比数字**，§2.6）
    if state.shows_spinner() {
        let c = pos2(rect.left() + 10.0, rect.center().y);
        let r = 5.0;
        // 用 egui 的时间驱动旋转：每帧重绘，不必自己起动画线程
        let t = ui.ctx().input(|i| i.time);
        let a = (t * 3.0) as f32;
        painter.circle_stroke(c, r, egui::Stroke::new(1.5, theme.separator));
        painter.line_segment(
            [c, pos2(c.x + r * a.cos(), c.y + r * a.sin())],
            egui::Stroke::new(1.5, color),
        );
    }

    painter.text(
        pos2(rect.left() + 22.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font.clone(),
        color,
    );

    // 角标：只在「需要处理」态画（§2.6），红底白字，≥100 显示 99+
    if state.shows_badge() {
        let c = pos2(rect.right() - BADGE_D / 2.0 - 2.0, rect.center().y);
        painter.circle_filled(c, BADGE_D / 2.0, theme.danger);
        painter.text(
            c,
            Align2::CENTER_CENTER,
            mgmt.badge_text(),
            FontId::new(9.0, egui::FontFamily::Proportional),
            egui::Color32::WHITE,
        );
    }

    // 悬停解释角标（§4.4：三行，为 0 的分量省略）
    // ⚠️ `on_hover_text` 会**移走** `Response`，所以点击状态必须在它之前取出来。
    let clicked = resp.clicked();
    if resp.hovered() {
        let mut lines = mgmt.badge_tooltip(mgmt.failed_task_count);
        // §4.8 要求启动校验「可见」：进度挂在入口的 tooltip 上，
        // 不额外占状态栏——校验是后台行为，进了角标反而像「出问题了」。
        if !verify_text.is_empty() {
            lines.push(String::new());
            lines.push(format!("启动校验：{verify_text}"));
        }
        resp.on_hover_text(lines.join("\n"));
    }
    clicked
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
    Some(format!(
        "已就绪 {} / 已发现 {}",
        task.completed, task.discovered
    ))
}

#[cfg(test)]
mod tests {
    use picasa_core::{ExclusionView, MgmtSnapshot, RootView};

    /// 悬停文案的三个分量加起来必须等于角标总数（§4.4）。
    ///
    /// 这条用例守的是「界面上写的原因」与「角标上的数字」不自相矛盾——
    /// 数字说 6、原因加起来只有 5，用户就会认为角标是瞎算的。
    #[test]
    fn badge_tooltip_components_add_up_to_the_badge() {
        let mut snap = MgmtSnapshot {
            roots: vec![RootView {
                id: 1,
                path: "/Volumes/X".into(),
                offline: true,
                ..Default::default()
            }],
            exclusions: vec![ExclusionView {
                id: 1,
                root_id: 1,
                path: "/a".into(),
                path_missing: true,
            }],
            failed_task_count: 4,
            ..Default::default()
        };
        snap.compute_badge(4, 1);
        assert_eq!(snap.badge, 6, "4 失败任务 + 1 失效排除项 + 1 离线根");
        let tip = snap.badge_tooltip(snap.failed_task_count);
        assert_eq!(tip.len(), 3);
        assert!(tip[0].contains("4"), "{}", tip[0]);
    }
}
