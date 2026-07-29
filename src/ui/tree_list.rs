//! 文件列表树（扁平渲染 + `egui_extras::TableBuilder` 原生表头）。
//!
//! 修复：
//! - 拖动名称列不再影响整个列表宽度（改用 initial 列 + clip，不用 remainder）。
//! - 展开/收起支持整行任意位置点击（箭头 / 名称文字 / 大小列 / 进度条列均响应）。
//! - 新增"占比"列，显示当前行容量占根节点总容量的百分比彩色进度条。
//! - 根节点行显示磁盘分区路径（root_label 由调用方传入）。

use egui::{Color32, RichText, Sense, Rect, Vec2, Pos2};

use crate::format::human_size;
use crate::model::{Node, NodePath};

use super::TreeAction;

const ROW_H: f32 = 22.0;

pub fn show(
    ui: &mut egui::Ui,
    view_root: &Node,
    selected: &Option<NodePath>,
    root_label: &str,
) -> TreeAction {
    let mut action = TreeAction::None;
    let total_size = view_root.size.max(1);

    // ScrollArea 防止表格 auto-shrink 压缩父容器
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui_extras::TableBuilder::new(ui)
                .striped(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .auto_shrink([false, false])
                // 名称列：initial 宽度 + clip，拖动时不影响其他列或整体宽度
                .column(
                    egui_extras::Column::initial(260.0)
                        .at_least(120.0)
                        .clip(true)
                        .resizable(true),
                )
                // 大小列
                .column(
                    egui_extras::Column::initial(90.0)
                        .at_least(50.0)
                        .resizable(true),
                )
                // 占比进度条列
                .column(
                    egui_extras::Column::initial(120.0)
                        .at_least(60.0)
                        .resizable(true),
                )
                .header(ROW_H, |mut header| {
                    header.col(|ui| {
                        ui.label(RichText::new("名称").strong().size(12.0).color(Color32::WHITE));
                    });
                    header.col(|ui| {
                        ui.label(RichText::new("大小").strong().size(12.0).color(Color32::WHITE));
                    });
                    header.col(|ui| {
                        ui.label(RichText::new("占比").strong().size(12.0).color(Color32::WHITE));
                    });
                })
                .body(|mut body| {
                    // 根节点行（磁盘分区）
                    body.row(ROW_H, |mut row| {
                        row.col(|ui| {
                            ui.colored_label(
                                Color32::from_rgb(0xAA, 0xCC, 0xFF),
                                RichText::new("▼").size(10.0),
                            );
                            let icon = "💾";
                            ui.label(
                                RichText::new(format!("{icon} {root_label}"))
                                    .color(Color32::from_rgb(0xFF, 0xD7, 0x00))
                                    .size(13.0)
                                    .strong(),
                            );
                        });
                        row.col(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(
                                    RichText::new(human_size(view_root.size))
                                        .size(12.0)
                                        .color(Color32::from_rgb(0xC0, 0xC0, 0xC0)),
                                );
                            });
                        });
                        row.col(|ui| {
                            draw_bar(ui, 1.0, Color32::from_rgb(0xFF, 0xD7, 0x00));
                        });
                    });

                    let mut path: NodePath = Vec::new();
                    draw_rows(
                        &mut body,
                        view_root,
                        &mut path,
                        0,
                        selected,
                        &mut action,
                        total_size,
                    );
                });
        });

    action
}

/// 绘制容量占比进度条（含百分比文字）
fn draw_bar(ui: &mut egui::Ui, pct: f32, color: Color32) {
    let avail = ui.available_rect_before_wrap();
    let bar_h = 10.0;
    let bar_w = (avail.width() - 6.0).max(0.0);
    let bar_rect = Rect::from_min_size(
        Pos2::new(avail.min.x + 3.0, avail.center().y - bar_h / 2.0),
        Vec2::new(bar_w, bar_h),
    );

    let painter = ui.painter();
    // 背景槽
    painter.rect_filled(bar_rect, 2.0, Color32::from_rgb(0x48, 0x48, 0x52));
    // 前景填充
    let fill_w = (bar_w * pct.clamp(0.0, 1.0)).max(0.0);
    if fill_w > 0.5 {
        let fill_rect = Rect::from_min_size(bar_rect.min, Vec2::new(fill_w, bar_h));
        painter.rect_filled(fill_rect, 2.0, color);
    }
    // 百分比文字
    painter.text(
        bar_rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{:.1}%", pct * 100.0),
        egui::FontId::proportional(9.5),
        Color32::WHITE,
    );
}

/// 深度调色板
fn depth_color(depth: u32, is_folder: bool) -> Color32 {
    if !is_folder {
        return Color32::from_rgb(0x6C, 0x75, 0x7D);
    }
    const PAL: [Color32; 6] = [
        Color32::from_rgb(0x4C, 0x8B, 0xF5),
        Color32::from_rgb(0x34, 0xC7, 0x59),
        Color32::from_rgb(0xF5, 0xA6, 0x23),
        Color32::from_rgb(0xE0, 0x55, 0x5B),
        Color32::from_rgb(0x9C, 0x6A, 0xDE),
        Color32::from_rgb(0x2E, 0xC4, 0xB6),
    ];
    PAL[depth as usize % PAL.len()]
}

fn draw_rows(
    body: &mut egui_extras::TableBody,
    node: &Node,
    path: &mut NodePath,
    depth: u32,
    selected: &Option<NodePath>,
    action: &mut TreeAction,
    total_size: u64,
) {
    let mut order: Vec<usize> = (0..node.children.len()).collect();
    order.sort_by(|&a, &b| node.children[b].size.cmp(&node.children[a].size));

    for i in order {
        let child = &node.children[i];
        let is_folder = !child.children.is_empty();
        path.push(i);

        let current_path = path.clone();
        let is_selected = selected.as_deref() == Some(path.as_slice());
        let pct = child.size as f32 / total_size as f32;
        let bar_color = depth_color(depth, is_folder);

        // 收集三列的交互信号
        let mut any_clicked = false;
        let mut any_dbl = false;
        let mut arrow_clicked = false;

        body.row(ROW_H, |mut row| {
            // ── 名称列 ─────────────────────────────────────────────
            row.col(|ui| {
                let indent = depth as f32 * 16.0;
                if indent > 0.0 {
                    ui.add_space(indent);
                }

                if is_folder {
                    let arrow = if child.expanded { "▼" } else { "▶" };
                    let arrow_resp = ui.add(
                        egui::Button::new(
                            RichText::new(arrow)
                                .size(10.0)
                                .color(Color32::from_rgb(0xAA, 0xCC, 0xFF)),
                        )
                        .frame(false)
                        .min_size(Vec2::new(16.0, ROW_H)),
                    );
                    if arrow_resp.clicked() {
                        arrow_clicked = true;
                    }
                } else {
                    ui.add_space(16.0);
                }

                let icon = if is_folder { "📁" } else { "📄" };
                let text_color = if is_folder {
                    Color32::WHITE
                } else {
                    Color32::from_rgb(0xCC, 0xCC, 0xCC)
                };
                let label = ui.selectable_label(
                    is_selected,
                    RichText::new(format!("{icon} {}", child.name))
                        .color(text_color)
                        .size(13.0),
                );
                if label.double_clicked() {
                    any_dbl = true;
                } else if label.clicked() {
                    any_clicked = true;
                }
            });

            // ── 大小列（整列响应点击）──────────────────────────────
            row.col(|ui| {
                // allocate 整列区域来捕获点击，再在其中绘制文字
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, Sense::click());
                ui.painter().text(
                    egui::pos2(rect.max.x - 4.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    human_size(child.size),
                    egui::FontId::proportional(12.0),
                    Color32::from_rgb(0xC0, 0xC0, 0xC0),
                );
                if resp.double_clicked() {
                    any_dbl = true;
                } else if resp.clicked() {
                    any_clicked = true;
                }
            });

            // ── 占比进度条列（整列响应点击）───────────────────────
            row.col(|ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, Sense::click());
                draw_bar(ui, pct, bar_color);
                if resp.double_clicked() {
                    any_dbl = true;
                } else if resp.clicked() {
                    any_clicked = true;
                }
            });
        });

        // ── 处理交互 ──────────────────────────────────────────────
        if arrow_clicked {
            *action = TreeAction::ToggleExpand(current_path);
        } else if any_dbl {
            *action = TreeAction::EnterNode(current_path);
        } else if any_clicked {
            if is_folder {
                *action = TreeAction::ToggleExpand(current_path);
            } else {
                *action = TreeAction::Select(current_path);
            }
        }

        if child.expanded && is_folder {
            draw_rows(body, child, path, depth + 1, selected, action, total_size);
        }

        path.pop();
    }
}
