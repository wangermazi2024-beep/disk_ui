//! 文件列表树（扁平渲染 + `egui_extras::TableBuilder` 原生表头）。
//!
//! 修复：
//! - 拖动名称列不影响整体宽度（min_scrolled_width + initial 列）。
//! - 根节点整行可点击展开/收缩。
//! - 子节点整行（含名称列空白处）均可点击展开/收缩。
//! - 每一行都有占比进度条，用 painter 直接绘制，不会超出列边界。

use std::cell::Cell;

use egui::{Color32, Rect, Sense, Vec2, Pos2};

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
    // 用 Cell 在 ScrollArea / TableBuilder 闭包链之间传递最终动作
    let action_cell: Cell<TreeAction> = Cell::new(TreeAction::None);
    let total_size = view_root.size.max(1);

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui_extras::TableBuilder::new(ui)
                .striped(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .auto_shrink([false, false])
                .min_scrolled_width(400.0)          // 防止拖动列宽缩小整个表格
                .column(                             // 名称列
                    egui_extras::Column::initial(260.0)
                        .at_least(120.0)
                        .clip(true)
                        .resizable(true),
                )
                .column(                             // 大小列
                    egui_extras::Column::initial(90.0)
                        .at_least(50.0)
                        .resizable(true),
                )
                .column(                             // 占比列
                    egui_extras::Column::initial(130.0)
                        .at_least(80.0)
                        .resizable(true),
                )
                .header(ROW_H, |mut header| {
                    header.col(|ui| {
                        ui.label(egui::RichText::new("名称").strong().size(12.0).color(Color32::WHITE));
                    });
                    header.col(|ui| {
                        ui.label(egui::RichText::new("大小").strong().size(12.0).color(Color32::WHITE));
                    });
                    header.col(|ui| {
                        ui.label(egui::RichText::new("占比").strong().size(12.0).color(Color32::WHITE));
                    });
                })
                .body(|mut body| {
                    // ── 根节点行（磁盘分区）──────────────────────────
                    let root_clicked  = Cell::new(false);
                    let root_dbl      = Cell::new(false);

                    body.row(ROW_H, |mut row| {
                        // 名称列：整列感应
                        row.col(|ui| {
                            let rect = ui.available_rect_before_wrap();
                            let resp = ui.allocate_rect(rect, Sense::click());
                            let p = ui.painter();
                            // 箭头
                            p.text(
                                Pos2::new(rect.min.x + 2.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                if view_root.expanded { "▼" } else { "▶" },
                                egui::FontId::proportional(10.0),
                                Color32::from_rgb(0xAA, 0xCC, 0xFF),
                            );
                            // 图标 + 标签
                            p.text(
                                Pos2::new(rect.min.x + 18.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                format!("💾 {root_label}"),
                                egui::FontId::proportional(13.0),
                                Color32::from_rgb(0xFF, 0xD7, 0x00),
                            );
                            if resp.double_clicked() { root_dbl.set(true); }
                            else if resp.clicked()   { root_clicked.set(true); }
                        });
                        // 大小列
                        row.col(|ui| {
                            let rect = ui.available_rect_before_wrap();
                            let resp = ui.allocate_rect(rect, Sense::click());
                            ui.painter().text(
                                egui::pos2(rect.max.x - 4.0, rect.center().y),
                                egui::Align2::RIGHT_CENTER,
                                human_size(view_root.size),
                                egui::FontId::proportional(12.0),
                                Color32::from_rgb(0xC0, 0xC0, 0xC0),
                            );
                            if resp.double_clicked() { root_dbl.set(true); }
                            else if resp.clicked()   { root_clicked.set(true); }
                        });
                        // 占比列（100%）
                        row.col(|ui| {
                            let rect = ui.available_rect_before_wrap();
                            let resp = ui.allocate_rect(rect, Sense::click());
                            draw_bar(ui.painter(), rect, 1.0, Color32::from_rgb(0xFF, 0xD7, 0x00));
                            if resp.double_clicked() { root_dbl.set(true); }
                            else if resp.clicked()   { root_clicked.set(true); }
                        });
                    });

                    // 根节点交互处理
                    if root_dbl.get() {
                        action_cell.set(TreeAction::EnterNode(vec![]));
                    } else if root_clicked.get() {
                        action_cell.set(TreeAction::ToggleExpand(vec![]));
                    }

                    // ── 子节点递归行 ─────────────────────────────────
                    let mut path: NodePath = Vec::new();
                    let mut child_action = TreeAction::None;
                    draw_rows(
                        &mut body,
                        view_root,
                        &mut path,
                        0,
                        selected,
                        &mut child_action,
                        total_size,
                    );
                    // 子节点动作优先于根节点动作（后发生的覆盖）
                    if !matches!(child_action, TreeAction::None) {
                        action_cell.set(child_action);
                    }
                });
        });

    action_cell.into_inner()
}

/// 用 `Painter` 在给定 cell rect 内绘制进度条，完全不消耗 UI 布局空间。
fn draw_bar(painter: &egui::Painter, cell_rect: Rect, pct: f32, color: Color32) {
    let pad = 4.0;
    let bar_h = 10.0;
    let bar_w = (cell_rect.width() - pad * 2.0).max(0.0);
    let bar_rect = Rect::from_min_size(
        Pos2::new(cell_rect.min.x + pad, cell_rect.center().y - bar_h / 2.0),
        Vec2::new(bar_w, bar_h),
    );

    // 背景槽
    painter.rect_filled(bar_rect, 2.0, Color32::from_rgb(0x48, 0x48, 0x52));
    // 前景
    let fill_w = (bar_w * pct.clamp(0.0, 1.0)).max(0.0);
    if fill_w > 0.5 {
        painter.rect_filled(
            Rect::from_min_size(bar_rect.min, Vec2::new(fill_w, bar_h)),
            2.0,
            color,
        );
    }
    // 百分比文字（绘制在 bar_rect 中央，clip 由列宽保证不越界）
    painter.text(
        bar_rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{:.1}%", pct * 100.0),
        egui::FontId::proportional(9.5),
        Color32::WHITE,
    );
}

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

        let current_path  = path.clone();
        let is_selected   = selected.as_deref() == Some(path.as_slice());
        let pct           = child.size as f32 / total_size as f32;
        let bar_color     = depth_color(depth, is_folder);

        // Cell 传递闭包间信号
        let clicked       = Cell::new(false);
        let dbl_clicked   = Cell::new(false);
        let arrow_clicked = Cell::new(false);

        body.row(ROW_H, |mut row| {
            // ── 名称列 ───────────────────────────────────────────
            row.col(|ui| {
                let indent    = depth as f32 * 16.0 + 2.0;
                let cell_rect = ui.available_rect_before_wrap();

                // 整列底层感应区（箭头在上层覆盖它）
                let full_resp = ui.allocate_rect(cell_rect, Sense::click());

                // 选中高亮背景
                if is_selected {
                    ui.painter().rect_filled(
                        cell_rect,
                        0.0,
                        Color32::from_rgba_unmultiplied(0x4C, 0x8B, 0xF5, 0x40),
                    );
                }

                let p = ui.painter();

                if is_folder {
                    // 箭头：独立感应区，覆盖在底层感应区之上
                    let arrow_rect = Rect::from_min_size(
                        Pos2::new(cell_rect.min.x + indent, cell_rect.min.y),
                        Vec2::new(16.0, ROW_H),
                    );
                    let arrow_resp = ui.allocate_rect(arrow_rect, Sense::click());
                    p.text(
                        arrow_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        if child.expanded { "▼" } else { "▶" },
                        egui::FontId::proportional(10.0),
                        Color32::from_rgb(0xAA, 0xCC, 0xFF),
                    );
                    if arrow_resp.clicked() {
                        arrow_clicked.set(true);
                    }
                }

                // 图标 + 文件名（painter 绘制，不产生额外感应区）
                let icon = if is_folder { "📁" } else { "📄" };
                let text_color = if is_selected {
                    Color32::from_rgb(0xFF, 0xFF, 0x80)
                } else if is_folder {
                    Color32::WHITE
                } else {
                    Color32::from_rgb(0xCC, 0xCC, 0xCC)
                };
                ui.painter().text(
                    Pos2::new(cell_rect.min.x + indent + 18.0, cell_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{icon} {}", child.name),
                    egui::FontId::proportional(13.0),
                    text_color,
                );

                // 整行感应（箭头区域已经有自己的 sense，这里是其余空白区域）
                if full_resp.double_clicked() { dbl_clicked.set(true); }
                else if full_resp.clicked()   { clicked.set(true); }
            });

            // ── 大小列 ───────────────────────────────────────────
            row.col(|ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, Sense::click());
                ui.painter().text(
                    egui::pos2(rect.max.x - 4.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    human_size(child.size),
                    egui::FontId::proportional(12.0),
                    Color32::from_rgb(0xC0, 0xC0, 0xC0),
                );
                if resp.double_clicked() { dbl_clicked.set(true); }
                else if resp.clicked()   { clicked.set(true); }
            });

            // ── 占比列 ───────────────────────────────────────────
            row.col(|ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, Sense::click());
                // painter 绘制，绝对不会超出列宽
                draw_bar(ui.painter(), rect, pct, bar_color);
                if resp.double_clicked() { dbl_clicked.set(true); }
                else if resp.clicked()   { clicked.set(true); }
            });
        });

        // ── 交互处理（闭包外）────────────────────────────────────
        if arrow_clicked.get() {
            *action = TreeAction::ToggleExpand(current_path);
        } else if dbl_clicked.get() {
            *action = TreeAction::EnterNode(current_path);
        } else if clicked.get() {
            *action = if is_folder {
                TreeAction::ToggleExpand(current_path)
            } else {
                TreeAction::Select(current_path)
            };
        }

        if child.expanded && is_folder {
            draw_rows(body, child, path, depth + 1, selected, action, total_size);
        }

        path.pop();
    }
}
