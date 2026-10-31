//! 诊断页（F 加固）：**缓存占用 / 清理、失败记录、排除项、全量重建**。
//!
//! ## 为什么单独一页
//!
//! 这几件事都"不常看但必须能看"：占用多少、哪些文件失败了、关了哪些子目录、
//! 想不想推倒重来。把它们塞进索引进度面板会让那个面板同时承担「实时进度」和
//! 「历史与设置」，而 `04` §3.2 的硬约束是进度面板只讲「正在发生什么」。
//!
//! 本模块**只画不改**：所有数据库操作都通过 [`DiagAction`] 交回 `app` 执行，
//! 这样「谁写库」仍然只有 `Library` / `Db` 一层，UI 不直连 SQL。

use eframe::egui::{self, Color32, Context, RichText, Ui};
use picasa_db::{CacheStats, ExclusionRow};

use crate::theme::Theme;

/// 诊断页上的一个动作，交回 `app` 执行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagAction {
    /// 清空缩略图缓存（释放空间，下次扫描重建）
    ClearThumbs,
    /// VACUUM 回收磁盘
    Vacuum,
    /// 全量重建（清空缩略图 + 重新扫描所有根目录）
    RebuildAll,
    /// 恢复一条排除项
    RestoreExclusion(i64),
    /// 重新打开一个已关闭的根目录
    ReopenRoot(String),
    /// 重新读取统计（缓存占用/失败/排除项可能变了）
    Refresh,
}

/// 一条失败记录（来自 `index_task_failures`）。
#[derive(Clone, Debug, Default)]
pub struct DiagFailure {
    pub scope: String,
    pub path: String,
    pub code: String,
}

/// 诊断页需要展示的全部数据（由 `app` 一次性取好）。
#[derive(Clone, Debug, Default)]
pub struct DiagData {
    pub stats: CacheStats,
    pub failures: Vec<DiagFailure>,
    pub exclusions: Vec<(String, ExclusionRow)>,
    pub closed_roots: Vec<String>,
}

/// 字节数转人类可读（1.2 MB / 340 KB）。
fn human_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    let f = n as f64;
    if f >= KB * KB * KB {
        format!("{:.2} GB", f / KB / KB / KB)
    } else if f >= KB * KB {
        format!("{:.1} MB", f / KB / KB)
    } else if f >= KB {
        format!("{:.0} KB", f / KB)
    } else {
        format!("{n} B")
    }
}

/// 画诊断窗口。返回用户点出的动作（最多一个）。
pub fn show(
    ctx: &Context,
    theme: &Theme,
    open: &mut bool,
    data: &DiagData,
    confirm_rebuild: &mut bool,
) -> Option<DiagAction> {
    let mut action = None;
    egui::Window::new("诊断")
        .open(open)
        .resizable(true)
        .default_width(520.0)
        .default_height(560.0)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;

            // ---------------- 缓存占用 ----------------
            section(ui, "缓存与占用", theme);
            let s = &data.stats;
            grid_row(ui, theme, "索引库文件", &human_bytes(s.db_bytes));
            grid_row(ui, theme, "照片", &format!("{} 张", s.photo_count));
            grid_row(
                ui,
                theme,
                "缩略图",
                &format!("{} 条 · {}", s.thumb_count, human_bytes(s.thumb_bytes)),
            );
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui
                    .button("清理缩略图")
                    .on_hover_text("删掉所有缩略图缓存（不删照片），下次扫描会重建")
                    .clicked()
                {
                    action = Some(DiagAction::ClearThumbs);
                }
                if ui
                    .button("VACUUM 回收")
                    .on_hover_text("物理压缩数据库文件。清理缩略图后点这里才真的变小")
                    .clicked()
                {
                    action = Some(DiagAction::Vacuum);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if *confirm_rebuild {
                        if ui
                            .button(
                                RichText::new("确认重建？")
                                    .color(Color32::from_rgb(0xB0, 0x30, 0x20)),
                            )
                            .clicked()
                        {
                            action = Some(DiagAction::RebuildAll);
                            *confirm_rebuild = false;
                        }
                        if ui.button("取消").clicked() {
                            *confirm_rebuild = false;
                        }
                    } else if ui
                        .button("全量重建…")
                        .on_hover_text("清空全部缩略图并重新扫描所有根目录。耗时很长")
                        .clicked()
                    {
                        *confirm_rebuild = true;
                    }
                });
            });

            ui.add_space(6.0);

            // ---------------- 失败记录 ----------------
            section(ui, &format!("失败记录（{}）", data.failures.len()), theme);
            if data.failures.is_empty() {
                muted(ui, "没有失败项。");
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("diag-failures")
                    .max_height(150.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for f in &data.failures {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(&f.code)
                                        .size(11.0)
                                        .color(Color32::from_rgb(0xB0, 0x30, 0x20)),
                                );
                                ui.label(RichText::new(&f.path).size(11.5));
                                ui.label(
                                    RichText::new(&f.scope)
                                        .size(10.5)
                                        .color(theme.text_secondary),
                                );
                            });
                        }
                    });
            }

            ui.add_space(6.0);

            // ---------------- 排除项 ----------------
            section(
                ui,
                &format!("已关闭的文件夹（{}）", data.exclusions.len()),
                theme,
            );
            if data.exclusions.is_empty() {
                muted(ui, "没有排除项。右键文件夹可以「关闭此文件夹」。");
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("diag-excl")
                    .max_height(130.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (root, e) in &data.exclusions {
                            ui.horizontal(|ui| {
                                if ui.small_button("恢复").clicked() {
                                    action = Some(DiagAction::RestoreExclusion(e.id));
                                }
                                ui.label(RichText::new(&e.relative_path).size(11.5));
                                ui.label(
                                    RichText::new(root).size(10.0).color(theme.text_secondary),
                                );
                            });
                        }
                    });
            }

            ui.add_space(6.0);

            // ---------------- 已关闭的根目录 ----------------
            section(
                ui,
                &format!("已关闭的关注文件夹（{}）", data.closed_roots.len()),
                theme,
            );
            if data.closed_roots.is_empty() {
                muted(ui, "没有已关闭的关注文件夹。");
            } else {
                for root in &data.closed_roots {
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("重新添加")
                            .on_hover_text("复用原有索引，秒开")
                            .clicked()
                        {
                            action = Some(DiagAction::ReopenRoot(root.clone()));
                        }
                        ui.label(RichText::new(root).size(11.0));
                    });
                }
            }

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("刷新统计").clicked() {
                    action = Some(DiagAction::Refresh);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new("只读：本页不做任何写入，除非你点按钮")
                            .size(10.0)
                            .color(theme.text_secondary),
                    );
                });
            });
        });
    action
}

fn section(ui: &mut Ui, title: &str, theme: &Theme) {
    ui.add_space(4.0);
    ui.label(
        RichText::new(title)
            .size(12.5)
            .strong()
            .color(theme.text_primary),
    );
    ui.separator();
}

fn grid_row(ui: &mut Ui, theme: &Theme, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(11.5).color(theme.text_secondary));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).size(11.5));
        });
    });
}

fn muted(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(11.0)
            .color(Color32::from_gray(0x88)),
    );
}

/// 失败记录行的人类可读折行（供日志与测试复用）。
pub fn failure_line(f: &DiagFailure) -> String {
    format!("{} · {} · {}", f.scope, f.path, f.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_switches_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
        assert!(human_bytes(2 * 1024 * 1024 * 1024).ends_with("GB"));
    }

    #[test]
    fn failure_line_joins_fields() {
        let f = DiagFailure {
            scope: "/photos".into(),
            path: "a/b.jpg".into(),
            code: "decode_failed".into(),
        };
        assert_eq!(failure_line(&f), "/photos · a/b.jpg · decode_failed");
    }
}
