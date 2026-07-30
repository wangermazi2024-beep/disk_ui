//! 文件列表树。
//!
//! 顶层显示多个磁盘分区（每个分区是独立根节点），用户展开某个分区后
//! 递归显示其子目录/文件。整行任意位置点击均可触发展开/收缩或选中。
//!
//! 列布局：
//!   名称 | 大小 | 占比 | 项目 | 文件 | 文件夹 | 修改时间 | 属性
//!
//! - 磁盘根行：高度更大，大小列多行展示 总/已分配/未分配/占用 四个值；
//!   占比按 "扫描汇总大小 / 磁盘总容量" 计算。
//! - 普通子节点：高度 22px，占比按 "本节点大小 / 父节点大小" 计算
//!   （即"相对父级的占用百分比"）。

use std::cell::Cell;

use egui::{Color32, Pos2, Rect, Sense, Vec2};

use crate::disk_info::DiskInfo;
use crate::format::{format_attributes, format_filetime_local, human_size, human_size_compact};
use crate::model::{Node, NodePath};

use super::TreeAction;

const ROW_H: f32 = 22.0;
/// 磁盘根行更高，留出多行展示 总/已分配/未分配/扫描汇总/一致性 五个值。
const DISK_ROW_H: f32 = 68.0;

pub fn show(
    ui: &mut egui::Ui,
    partitions: &[Node],
    partition_infos: &[Option<DiskInfo>],
    selected: &Option<NodePath>,
) -> TreeAction {
    let action_cell: Cell<TreeAction> = Cell::new(TreeAction::None);

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui_extras::TableBuilder::new(ui)
                .striped(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .auto_shrink([false, false])
                .column(egui_extras::Column::initial(220.0).at_least(140.0).clip(true).resizable(true))
                .column(egui_extras::Column::initial(110.0).at_least(80.0).resizable(true))
                .column(egui_extras::Column::initial(95.0).at_least(70.0).resizable(true))
                .column(egui_extras::Column::initial(55.0).at_least(40.0).resizable(true))
                .column(egui_extras::Column::initial(55.0).at_least(40.0).resizable(true))
                .column(egui_extras::Column::initial(55.0).at_least(40.0).resizable(true))
                .column(egui_extras::Column::initial(125.0).at_least(95.0).resizable(true))
                .column(egui_extras::Column::initial(70.0).at_least(50.0).resizable(true))
                .header(ROW_H, |mut header| {
                    header.col(|ui| { ui.label(egui::RichText::new("名称").strong().size(12.0).color(Color32::WHITE)); });
                    header.col(|ui| { ui.label(egui::RichText::new("大小").strong().size(12.0).color(Color32::WHITE)); });
                    header.col(|ui| { ui.label(egui::RichText::new("占比").strong().size(12.0).color(Color32::WHITE)); });
                    header.col(|ui| { ui.label(egui::RichText::new("项目").strong().size(12.0).color(Color32::WHITE)); });
                    header.col(|ui| { ui.label(egui::RichText::new("文件").strong().size(12.0).color(Color32::WHITE)); });
                    header.col(|ui| { ui.label(egui::RichText::new("文件夹").strong().size(12.0).color(Color32::WHITE)); });
                    header.col(|ui| { ui.label(egui::RichText::new("修改时间").strong().size(12.0).color(Color32::WHITE)); });
                    header.col(|ui| { ui.label(egui::RichText::new("属性").strong().size(12.0).color(Color32::WHITE)); });
                })
                .body(|mut body| {
                    let mut final_action = TreeAction::None;

                    for (pi, partition) in partitions.iter().enumerate() {
                        let info = partition_infos.get(pi).and_then(|i| i.as_ref());
                        let part_path = vec![pi];
                        let part_selected = selected.as_deref() == Some(&[pi]);
                        let total = info.map(|i| i.total_bytes).unwrap_or(partition.size.max(1));
                        let part_pct = if total > 0 {
                            partition.size as f32 / total as f32
                        } else {
                            0.0
                        };

                        let part_clicked = Cell::new(false);

                        body.row(DISK_ROW_H, |mut row| {
                            // ── 名称列 ─────────────────────────────────
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                if part_selected {
                                    ui.painter().rect_filled(rect, 0.0,
                                        Color32::from_rgba_unmultiplied(0x4C, 0x8B, 0xF5, 0x40));
                                }
                                let arrow = if partition.expanded { "▼" } else { "▶" };
                                ui.painter().text(
                                    Pos2::new(rect.min.x + 2.0, rect.center().y),
                                    egui::Align2::LEFT_CENTER, arrow,
                                    egui::FontId::proportional(10.0),
                                    Color32::from_rgb(0xAA, 0xCC, 0xFF),
                                );
                                ui.painter().text(
                                    Pos2::new(rect.min.x + 18.0, rect.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    format!("💾 {}", partition.name),
                                    egui::FontId::proportional(13.0),
                                    if part_selected { Color32::from_rgb(0xFF, 0xFF, 0x80) }
                                    else { Color32::from_rgb(0xFF, 0xD7, 0x00) },
                                );
                                if resp.clicked() { part_clicked.set(true); }
                            });

                            // ── 大小列：磁盘行多行展示 总/已分配/未分配/扫描汇总/一致性 ─
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                let p = ui.painter();
                                let line_h = 12.0;
                                let start_y = rect.min.y + 3.0;
                                let x_label = rect.min.x + 2.0;
                                let x_value = rect.max.x - 4.0;

                                // 计算一致性比例 = 扫描汇总 / 系统已用
                                let (ratio_str, ratio_color) = if let Some(i) = info {
                                    if i.used_bytes > 0 {
                                        let r = partition.size as f64 / i.used_bytes as f64 * 100.0;
                                        let color = if r < 60.0 {
                                            Color32::from_rgb(0xE0, 0x55, 0x5B) // 红：可能丢数据
                                        } else if r > 105.0 {
                                            Color32::from_rgb(0xF5, 0xA6, 0x23) // 橙：可能重复计算
                                        } else {
                                            Color32::from_rgb(0x34, 0xC7, 0x59) // 绿：正常
                                        };
                                        (format!("{:.0}%", r), color)
                                    } else {
                                        ("—".into(), Color32::from_rgb(0xA0, 0xA0, 0xA0))
                                    }
                                } else {
                                    ("—".into(), Color32::from_rgb(0xA0, 0xA0, 0xA0))
                                };

                                let lines: Vec<(Color32, &str, String)> = if let Some(i) = info {
                                    vec![
                                        (Color32::from_rgb(0xE0, 0xE0, 0xE0), "总大小", human_size_compact(i.total_bytes)),
                                        (Color32::from_rgb(0xF5, 0xA6, 0x23), "已分配", human_size_compact(i.used_bytes)),
                                        (Color32::from_rgb(0x34, 0xC7, 0x59), "未分配", human_size_compact(i.free_bytes)),
                                        (Color32::from_rgb(0x4C, 0x8B, 0xF5), "扫描",   human_size_compact(partition.size)),
                                        (ratio_color,                      "一致性", ratio_str),
                                    ]
                                } else {
                                    vec![
                                        (Color32::from_rgb(0xE0, 0xE0, 0xE0), "总大小", human_size_compact(partition.size)),
                                        (Color32::from_rgb(0xA0, 0xA0, 0xA0), "已分配", "—".into()),
                                        (Color32::from_rgb(0xA0, 0xA0, 0xA0), "未分配", "—".into()),
                                        (Color32::from_rgb(0x4C, 0x8B, 0xF5), "扫描",   human_size_compact(partition.size)),
                                        (Color32::from_rgb(0xA0, 0xA0, 0xA0), "一致性", "—".into()),
                                    ]
                                };
                                for (i, (color, label, value)) in lines.iter().enumerate() {
                                    let y = start_y + (i as f32) * line_h;
                                    p.text(Pos2::new(x_label, y), egui::Align2::LEFT_TOP,
                                        label, egui::FontId::proportional(9.5),
                                        Color32::from_rgb(0xA0, 0xA0, 0xA0));
                                    p.text(Pos2::new(x_value, y), egui::Align2::RIGHT_TOP,
                                        value, egui::FontId::proportional(9.5), *color);
                                }
                                if resp.clicked() { part_clicked.set(true); }
                            });

                            // ── 占比列 ─────────────────────────────────
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                draw_bar(ui.painter(), rect, part_pct, Color32::from_rgb(0xFF, 0xD7, 0x00));
                                if resp.clicked() { part_clicked.set(true); }
                            });

                            // ── 项目/文件/文件夹 ───────────────────────
                            for value in [partition.file_count + partition.folder_count,
                                          partition.file_count,
                                          partition.folder_count] {
                                row.col(|ui| {
                                    let rect = ui.available_rect_before_wrap();
                                    let resp = ui.allocate_rect(rect, Sense::click());
                                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                                        format!("{}", value), egui::FontId::proportional(11.0),
                                        Color32::from_rgb(0xC0, 0xC0, 0xC0));
                                    if resp.clicked() { part_clicked.set(true); }
                                });
                            }

                            // ── 修改时间：磁盘行显示文件系统 ───────────
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                let text = info
                                    .map(|i| i.file_system.clone())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| format_filetime_local(partition.modified_ft));
                                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                                    text, egui::FontId::proportional(11.0),
                                    Color32::from_rgb(0xA0, 0xC0, 0xE0));
                                if resp.clicked() { part_clicked.set(true); }
                            });

                            // ── 属性 ──────────────────────────────────
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                                    format_attributes(partition.attributes),
                                    egui::FontId::proportional(11.0),
                                    Color32::from_rgb(0xC0, 0xC0, 0xC0));
                                if resp.clicked() { part_clicked.set(true); }
                            });
                        });

                        if part_clicked.get() {
                            final_action = TreeAction::ToggleExpand(part_path.clone());
                        }

                        if partition.expanded {
                            let mut rel_path: NodePath = Vec::new();
                            draw_rows(&mut body, partition, pi, &mut rel_path, 0,
                                selected, &mut final_action, partition.size.max(1));
                        }
                    }

                    action_cell.set(final_action);
                });
        });

    action_cell.into_inner()
}

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
        bar_rect.center(), egui::Align2::CENTER_CENTER,
        format!("{:.1}%", pct * 100.0),
        egui::FontId::proportional(9.5), Color32::WHITE,
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

/// 递归绘制分区内的子节点。`parent_size` 用于计算"相对父级的占用百分比"。
fn draw_rows(
    body: &mut egui_extras::TableBody,
    node: &Node,
    partition_index: usize,
    rel_path: &mut Vec<usize>,
    depth: u32,
    selected: &Option<NodePath>,
    action: &mut TreeAction,
    parent_size: u64,
) {
    let mut order: Vec<usize> = (0..node.children.len()).collect();
    order.sort_by(|&a, &b| node.children[b].size.cmp(&node.children[a].size));

    for i in order {
        let child = &node.children[i];
        let is_folder = child.is_folder();
        rel_path.push(i);

        let mut abs_path = vec![partition_index];
        abs_path.extend_from_slice(rel_path);

        let is_selected = selected.as_deref() == Some(&abs_path);
        let pct = if parent_size > 0 {
            child.size as f32 / parent_size as f32
        } else { 0.0 };
        let bar_color = depth_color(depth, is_folder);
        let indent = (depth + 1) as f32 * 16.0 + 2.0;

        let clicked = Cell::new(false);
        let dbl_clicked = Cell::new(false);

        body.row(ROW_H, |mut row| {
            // 名称列
            row.col(|ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, Sense::click());
                if is_selected {
                    ui.painter().rect_filled(rect, 0.0,
                        Color32::from_rgba_unmultiplied(0x4C, 0x8B, 0xF5, 0x40));
                }
                let p = ui.painter();
                if is_folder {
                    p.text(Pos2::new(rect.min.x + indent, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        if child.expanded { "▼" } else { "▶" },
                        egui::FontId::proportional(10.0),
                        Color32::from_rgb(0xAA, 0xCC, 0xFF));
                }
                let icon = if is_folder { "📁" } else { "📄" };
                let text_color = if is_selected { Color32::from_rgb(0xFF, 0xFF, 0x80) }
                    else if is_folder { Color32::WHITE }
                    else { Color32::from_rgb(0xCC, 0xCC, 0xCC) };
                p.text(Pos2::new(rect.min.x + indent + 16.0, rect.center().y),
                    egui::Align2::LEFT_CENTER, format!("{icon} {}", child.name),
                    egui::FontId::proportional(13.0), text_color);
                if resp.double_clicked() { dbl_clicked.set(true); }
                else if resp.clicked() { clicked.set(true); }
            });

            // 大小列
            row.col(|ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, Sense::click());
                ui.painter().text(egui::pos2(rect.max.x - 4.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER, human_size(child.size),
                    egui::FontId::proportional(12.0), Color32::from_rgb(0xC0, 0xC0, 0xC0));
                if resp.double_clicked() { dbl_clicked.set(true); }
                else if resp.clicked() { clicked.set(true); }
            });

            // 占比列
            row.col(|ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, Sense::click());
                draw_bar(ui.painter(), rect, pct, bar_color);
                if resp.double_clicked() { dbl_clicked.set(true); }
                else if resp.clicked() { clicked.set(true); }
            });

            // 项目数 / 文件数 / 文件夹数
            for (idx, value) in [child.file_count + child.folder_count,
                                  child.file_count,
                                  child.folder_count].iter().enumerate() {
                row.col(|ui| {
                    let rect = ui.available_rect_before_wrap();
                    let resp = ui.allocate_rect(rect, Sense::click());
                    let text = if is_folder { format!("{}", value) } else { "—".into() };
                    let _ = idx;
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                        text, egui::FontId::proportional(11.0),
                        Color32::from_rgb(0xC0, 0xC0, 0xC0));
                    if resp.double_clicked() { dbl_clicked.set(true); }
                    else if resp.clicked() { clicked.set(true); }
                });
            }

            // 修改时间
            row.col(|ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, Sense::click());
                let s = format_filetime_local(child.modified_ft);
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                    if s.is_empty() { "—".into() } else { s },
                    egui::FontId::proportional(11.0), Color32::from_rgb(0xC0, 0xC0, 0xC0));
                if resp.double_clicked() { dbl_clicked.set(true); }
                else if resp.clicked() { clicked.set(true); }
            });

            // 属性
            row.col(|ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, Sense::click());
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                    format_attributes(child.attributes),
                    egui::FontId::proportional(11.0), Color32::from_rgb(0xC0, 0xC0, 0xC0));
                if resp.double_clicked() { dbl_clicked.set(true); }
                else if resp.clicked() { clicked.set(true); }
            });
        });

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
                selected, action, child.size.max(1));
        }

        rel_path.pop();
    }
}
