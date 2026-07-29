//! 文件列表树。
//!
//! 顶层显示多个磁盘分区（每个分区是独立根节点），用户展开某个分区后
//! 递归显示其子目录/文件。整行任意位置点击均可触发展开/收缩或选中。

use std::cell::Cell;

use egui::{Color32, Rect, Sense, Vec2, Pos2};

use crate::format::human_size;
use crate::model::{Node, NodePath};

use super::TreeAction;

const ROW_H: f32 = 22.0;

/// `path` 编码规则：`[partition_index, child0, child1, ...]`
/// 顶层分区行对应 `[i]`（长度为 1），其子节点对应 `[i, j, ...]`。
pub fn show(
    ui: &mut egui::Ui,
    partitions: &[Node],
    selected: &Option<NodePath>,
) -> TreeAction {
    let action_cell: Cell<TreeAction> = Cell::new(TreeAction::None);

    // 总容量 = 所有分区大小之和，用于计算占比
    let total_size: u64 = partitions.iter().map(|p| p.size).sum::<u64>().max(1);

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui_extras::TableBuilder::new(ui)
                .striped(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .auto_shrink([false, false])
                .min_scrolled_width(400.0)
                .column(                         // 名称列
                    egui_extras::Column::initial(260.0)
                        .at_least(120.0)
                        .clip(true)
                        .resizable(true),
                )
                .column(                         // 大小列
                    egui_extras::Column::initial(90.0)
                        .at_least(50.0)
                        .resizable(true),
                )
                .column(                         // 占比列
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
                    let mut final_action = TreeAction::None;

                    for (pi, partition) in partitions.iter().enumerate() {
                        // ── 分区根节点行 ─────────────────────────────
                        let part_path = vec![pi];
                        let part_selected = selected.as_deref() == Some(&[pi]);
                        let part_pct = partition.size as f32 / total_size as f32;

                        let part_clicked = Cell::new(false);

                        body.row(ROW_H, |mut row| {
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                if part_selected {
                                    ui.painter().rect_filled(
                                        rect, 0.0,
                                        Color32::from_rgba_unmultiplied(0x4C, 0x8B, 0xF5, 0x40),
                                    );
                                }
                                let arrow = if partition.expanded { "▼" } else { "▶" };
                                ui.painter().text(
                                    Pos2::new(rect.min.x + 2.0, rect.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    arrow,
                                    egui::FontId::proportional(10.0),
                                    Color32::from_rgb(0xAA, 0xCC, 0xFF),
                                );
                                ui.painter().text(
                                    Pos2::new(rect.min.x + 18.0, rect.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    format!("💾 {}", partition.name),
                                    egui::FontId::proportional(13.0),
                                    if part_selected {
                                        Color32::from_rgb(0xFF, 0xFF, 0x80)
                                    } else {
                                        Color32::from_rgb(0xFF, 0xD7, 0x00)
                                    },
                                );
                                if resp.clicked() { part_clicked.set(true); }
                            });
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                ui.painter().text(
                                    egui::pos2(rect.max.x - 4.0, rect.center().y),
                                    egui::Align2::RIGHT_CENTER,
                                    human_size(partition.size),
                                    egui::FontId::proportional(12.0),
                                    Color32::from_rgb(0xC0, 0xC0, 0xC0),
                                );
                                if resp.clicked() { part_clicked.set(true); }
                            });
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                draw_bar(ui.painter(), rect, part_pct,
                                    Color32::from_rgb(0xFF, 0xD7, 0x00));
                                if resp.clicked() { part_clicked.set(true); }
                            });
                        });

                        if part_clicked.get() {
                            final_action = TreeAction::ToggleExpand(part_path.clone());
                        }

                        // ── 分区展开后的子节点 ───────────────────────
                        if partition.expanded {
                            let mut rel_path: NodePath = Vec::new();
                            draw_rows(
                                &mut body,
                                partition,
                                pi,
                                &mut rel_path,
                                0,
                                selected,
                                &mut final_action,
                                partition.size.max(1), // 占比相对于本分区总大小
                            );
                        }
                    }

                    action_cell.set(final_action);
                });
        });

    action_cell.into_inner()
}

/// 在 cell rect 内用 Painter 绘制进度条（不消耗布局空间，不超出列边界）。
fn draw_bar(painter: &egui::Painter, cell_rect: Rect, pct: f32, color: Color32) {
    let pad = 4.0;
    let bar_h = 10.0;
    let bar_w = (cell_rect.width() - pad * 2.0).max(0.0);
    let bar_rect = Rect::from_min_size(
        Pos2::new(cell_rect.min.x + pad, cell_rect.center().y - bar_h / 2.0),
        Vec2::new(bar_w, bar_h),
    );
    painter.rect_filled(bar_rect, 2.0, Color32::from_rgb(0x48, 0x48, 0x52));
    let fill_w = (bar_w * pct.clamp(0.0, 1.0)).max(0.0);
    if fill_w > 0.5 {
        painter.rect_filled(
            Rect::from_min_size(bar_rect.min, Vec2::new(fill_w, bar_h)),
            2.0, color,
        );
    }
    painter.text(
        bar_rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{:.1}%", pct * 100.0),
        egui::FontId::proportional(9.5),
        Color32::WHITE,
    );
}

fn depth_color(depth: u32, is_folder: bool) -> Color32 {
    if !is_folder { return Color32::from_rgb(0x6C, 0x75, 0x7D); }
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

/// 递归绘制分区内的子节点。
/// `rel_path`：相对于 partition 的路径（不含 partition_index）。
/// 最终写入 `action` 的路径格式为 `[partition_index, child0, child1, ...]`。
fn draw_rows(
    body: &mut egui_extras::TableBody,
    node: &Node,
    partition_index: usize,
    rel_path: &mut Vec<usize>,   // 相对于 partition 根的路径
    depth: u32,
    selected: &Option<NodePath>,
    action: &mut TreeAction,
    partition_size: u64,         // 当前分区总大小，用于计算占比
) {
    let mut order: Vec<usize> = (0..node.children.len()).collect();
    order.sort_by(|&a, &b| node.children[b].size.cmp(&node.children[a].size));

    for i in order {
        let child = &node.children[i];
        let is_folder = !child.children.is_empty();
        rel_path.push(i);

        // 绝对路径 = [partition_index] + rel_path
        let mut abs_path = vec![partition_index];
        abs_path.extend_from_slice(rel_path);

        let is_selected = selected.as_deref() == Some(&abs_path);
        let pct = child.size as f32 / partition_size as f32;
        let bar_color = depth_color(depth, is_folder);
        let indent = (depth + 1) as f32 * 16.0 + 2.0; // +1 因为分区行占第0层

        let clicked = Cell::new(false);
        let dbl_clicked = Cell::new(false);

        body.row(ROW_H, |mut row| {
            // ── 名称列：整行一个 allocate_rect ───────────────────
            row.col(|ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, Sense::click());
                if is_selected {
                    ui.painter().rect_filled(
                        rect, 0.0,
                        Color32::from_rgba_unmultiplied(0x4C, 0x8B, 0xF5, 0x40),
                    );
                }
                let p = ui.painter();
                // 箭头（文件夹）或空白（文件），直接用 painter 画，不单独处理点击
                if is_folder {
                    p.text(
                        Pos2::new(rect.min.x + indent, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        if child.expanded { "▼" } else { "▶" },
                        egui::FontId::proportional(10.0),
                        Color32::from_rgb(0xAA, 0xCC, 0xFF),
                    );
                }
                let icon = if is_folder { "📁" } else { "📄" };
                let text_color = if is_selected {
                    Color32::from_rgb(0xFF, 0xFF, 0x80)
                } else if is_folder {
                    Color32::WHITE
                } else {
                    Color32::from_rgb(0xCC, 0xCC, 0xCC)
                };
                p.text(
                    Pos2::new(rect.min.x + indent + 16.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{icon} {}", child.name),
                    egui::FontId::proportional(13.0),
                    text_color,
                );
                if resp.double_clicked() { dbl_clicked.set(true); }
                else if resp.clicked()   { clicked.set(true); }
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
                draw_bar(ui.painter(), rect, pct, bar_color);
                if resp.double_clicked() { dbl_clicked.set(true); }
                else if resp.clicked()   { clicked.set(true); }
            });
        });

        // ── 交互处理 ─────────────────────────────────────────────
        let abs_clone = abs_path.clone();
        if dbl_clicked.get() {
            *action = TreeAction::EnterNode(abs_clone);
        } else if clicked.get() {
            *action = if is_folder {
                TreeAction::ToggleExpand(abs_clone)
            } else {
                TreeAction::Select(abs_clone)
            };
        }

        if child.expanded && is_folder {
            draw_rows(body, child, partition_index, rel_path, depth + 1,
                selected, action, partition_size);
        }

        rel_path.pop();
    }
}
