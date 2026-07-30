//! 文件列表树（全量列，中文表头，全部居中，heterogeneous_rows 虚拟化）。
//!
//! 列顺序：名称 | 父占比 | 总占比 | 逻辑大小 | 修改时间 | 物理大小 | 创建时间 | 访问时间
//!         | 项目 | 文件 | 文件夹 | 属性 | 重解析点 | 保留 | 所有者
//!
//! 性能：用 `TableBody::heterogeneous_rows()` 做真正的虚拟滚动——
//! egui_extras 源码确认 `row()` 不虚拟化（每帧渲染所有行），
//! 只有 `rows()` / `heterogeneous_rows()` 才跳过不可见行。
//! 磁盘行和子行合并到同一个 heterogeneous_rows 调用里。

use std::cell::Cell;
use egui::{Color32, Pos2, Rect, Sense, Vec2};
use crate::disk_info::DiskInfo;
use crate::format::{format_attributes, format_filetime, human_size, human_size_compact};
use crate::model::{Node, NodePath};
use super::TreeAction;

const ROW_H: f32 = 22.0;
const DISK_ROW_H: f32 = 68.0;

/// 扁平化的可见行。磁盘行 + 子行统一处理。
enum RowKind {
    Disk { pi: usize },
    Child { pi: usize, node: *const Node, abs_path: NodePath, indent: f32, depth: u32, parent_logical: u64 },
}

struct FlatRow {
    height: f32,
    kind: RowKind,
}

/// 递归收集子行。children 已在构建时排序。
fn collect_rows(
    node: &Node, pi: usize, rel_path: &mut Vec<usize>, depth: u32,
    parent_logical: u64, rows: &mut Vec<FlatRow>,
) {
    for (i, child) in node.children.iter().enumerate() {
        rel_path.push(i);
        let mut abs_path = vec![pi];
        abs_path.extend_from_slice(rel_path);
        let indent = (depth + 1) as f32 * 16.0 + 2.0;
        rows.push(FlatRow {
            height: ROW_H,
            kind: RowKind::Child {
                pi,
                node: child as *const Node,
                abs_path,
                indent,
                depth,
                parent_logical,
            },
        });
        if child.is_folder() && child.expanded {
            collect_rows(child, pi, rel_path, depth + 1, child.logical_size.max(1), rows);
        }
        rel_path.pop();
    }
}

pub fn show(
    ui: &mut egui::Ui,
    partitions: &[Node],
    partition_infos: &[Option<DiskInfo>],
    selected: &Option<NodePath>,
) -> TreeAction {
    let action_cell: Cell<TreeAction> = Cell::new(TreeAction::None);
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        let mut builder = egui_extras::TableBuilder::new(ui)
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .auto_shrink([false, false])
            .column(egui_extras::Column::initial(200.0).at_least(120.0).clip(true).resizable(true))
            .column(egui_extras::Column::initial(85.0).resizable(true))
            .column(egui_extras::Column::initial(85.0).resizable(true))
            .column(egui_extras::Column::initial(85.0).resizable(true))
            .column(egui_extras::Column::initial(120.0).resizable(true))
            .column(egui_extras::Column::initial(85.0).resizable(true))
            .column(egui_extras::Column::initial(120.0).resizable(true))
            .column(egui_extras::Column::initial(120.0).resizable(true))
            .column(egui_extras::Column::initial(55.0).resizable(true))
            .column(egui_extras::Column::initial(55.0).resizable(true))
            .column(egui_extras::Column::initial(55.0).resizable(true))
            .column(egui_extras::Column::initial(50.0).resizable(true))
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 属性
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 重解析点
            .column(egui_extras::Column::initial(40.0).resizable(true))   // 保留
            .column(egui_extras::Column::initial(80.0).resizable(true).at_least(50.0).clip(true)) // 所有者——可拖动，at_least 防止被挤到0
            .column(egui_extras::Column::remainder()); // 空列——填满剩余空间，无空白

        builder = builder.sense(egui::Sense::click());
        builder
            .header(ROW_H, |mut h| {
                let cols = ["名称", "父占比", "总占比", "逻辑大小", "修改时间", "物理大小", "创建时间", "访问时间", "项目", "文件", "文件夹", "属性", "重解析点", "保留", "所有者", ""];
                for c in cols { h.col(|ui| { ui.label(egui::RichText::new(c).strong().size(12.0).color(Color32::WHITE)); }); }
            })
            .body(|mut body| {
                let mut final_action = TreeAction::None;
                // ── 先收集所有可见行（磁盘行 + 子行） ──
                let mut flat_rows: Vec<FlatRow> = Vec::new();
                for (pi, partition) in partitions.iter().enumerate() {
                    flat_rows.push(FlatRow { height: DISK_ROW_H, kind: RowKind::Disk { pi } });
                    if partition.expanded {
                        let mut rel_path: NodePath = Vec::new();
                        collect_rows(partition, pi, &mut rel_path, 0, partition.logical_size.max(1), &mut flat_rows);
                    }
                }

                // ── 用 heterogeneous_rows 做虚拟化渲染 ──
                let heights: Vec<f32> = flat_rows.iter().map(|r| r.height).collect();
                let clicked_row: Cell<usize> = Cell::new(usize::MAX);

                body.heterogeneous_rows(heights.into_iter(), |mut row| {
                    let row_idx = row.index();
                    if row_idx >= flat_rows.len() { return; }
                    let fr = &flat_rows[row_idx];

                    match &fr.kind {
                        RowKind::Disk { pi } => {
                            let partition = &partitions[*pi];
                            let info = partition_infos.get(*pi).and_then(|i| i.as_ref());
                            let part_selected = selected.as_deref() == Some(&[*pi]);
                            let total = info.map(|i| i.total_bytes).unwrap_or(partition.logical_size.max(1));
                            let part_pct = if total > 0 { partition.logical_size as f32 / total as f32 } else { 0.0 };
                            let p = partition;
                            let info_ref = info;

                            // 名称
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                if part_selected { ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }
                                let arrow = if p.expanded { "▼" } else { "▶" };
                                ui.painter().text(Pos2::new(rect.min.x+2.0, rect.min.y+6.0), egui::Align2::LEFT_TOP, arrow, egui::FontId::proportional(10.0), Color32::from_rgb(0xAA,0xCC,0xFF));
                                ui.painter().text(Pos2::new(rect.min.x+18.0, rect.min.y+4.0), egui::Align2::LEFT_TOP, format!("💾 {}", p.name), egui::FontId::proportional(13.0), if part_selected {Color32::from_rgb(0xFF,0xFF,0x80)} else {Color32::from_rgb(0xFF,0xD7,0x00)});
                                if let Some(i) = info_ref {
                                    ui.painter().text(Pos2::new(rect.min.x+18.0, rect.min.y+22.0), egui::Align2::LEFT_TOP, format!("总: {}  已用: {}  可用: {}", human_size_compact(i.total_bytes), human_size_compact(i.used_bytes), human_size_compact(i.free_bytes)), egui::FontId::proportional(10.0), Color32::from_rgb(0xA0,0xC0,0xE0));
                                    ui.painter().text(Pos2::new(rect.min.x+18.0, rect.min.y+36.0), egui::Align2::LEFT_TOP, format!("扫描: 逻辑={}  物理={}", human_size_compact(p.logical_size), human_size_compact(p.physical_size)), egui::FontId::proportional(10.0), Color32::from_rgb(0xA0,0xA0,0xA0));
                                }
                                if resp.clicked() { clicked_row.set(row_idx); }
                            });
                            // 父占比
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); draw_bar(ui.painter(),r,1.0,Color32::from_rgb(0xFF,0xD7,0x00)); if resp.clicked(){clicked_row.set(row_idx);} });
                            // 总占比
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); draw_bar(ui.painter(),r,part_pct,Color32::from_rgb(0x4C,0x8B,0xF5)); if resp.clicked(){clicked_row.set(row_idx);} });
                            // 逻辑大小
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(p.logical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0x4C,0x8B,0xF5)); if resp.clicked(){clicked_row.set(row_idx);} });
                            // 修改时间
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=info_ref.map(|i|i.file_system.clone()).filter(|s|!s.is_empty()).unwrap_or_else(||format_filetime(p.modified_ft)); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(11.0),Color32::from_rgb(0xA0,0xC0,0xE0)); if resp.clicked(){clicked_row.set(row_idx);} });
                            // 物理大小
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(p.physical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0xF5,0xA6,0x23)); if resp.clicked(){clicked_row.set(row_idx);} });
                            // 创建时间
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let s=format_filetime(p.created_ft); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){clicked_row.set(row_idx);} });
                            // 访问时间
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let s=format_filetime(p.accessed_ft); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){clicked_row.set(row_idx);} });
                            // 项目/文件/文件夹
                            for val in [p.file_count+p.folder_count, p.file_count, p.folder_count] {
                                row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format!("{}",val),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){clicked_row.set(row_idx);} });
                            }
                            // 属性
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format_attributes(p.attributes),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){clicked_row.set(row_idx);} });
                            // 重解析点
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=if p.reparse_tag!=0 {format!("0x{:X}",p.reparse_tag)}else{"—".into()}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){clicked_row.set(row_idx);} });
                            // 保留
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=if p.is_reserved {"是"}else{"—"}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){clicked_row.set(row_idx);} });
                            // 所有者
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=if p.owner.is_empty(){"—".into()}else{p.owner.clone()}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){clicked_row.set(row_idx);} });
                            // 空列（remainder 填充）
                            row.col(|_ui| {});
                        }
                        RowKind::Child { pi, node, abs_path, indent, depth, parent_logical } => {
                            let child = unsafe { &**node };
                            let is_folder = child.is_folder();
                            let is_selected = selected.as_deref() == Some(abs_path);
                            let disk_logical = partitions[*pi].logical_size.max(1);
                            let pct = if *parent_logical > 0 { child.logical_size as f32 / *parent_logical as f32 } else { 0.0 };
                            let total_pct = if disk_logical > 0 { child.logical_size as f32 / disk_logical as f32 } else { 0.0 };
                            let bar_color = depth_color(*depth, is_folder);
                            let c = child;

                            // 名称
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                if is_selected { ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }
                                let p = ui.painter();
                                if is_folder { p.text(Pos2::new(rect.min.x+indent,rect.center().y),egui::Align2::LEFT_CENTER,if c.expanded{"▼"}else{"▶"},egui::FontId::proportional(10.0),Color32::from_rgb(0xAA,0xCC,0xFF)); }
                                let icon = if is_folder {"📁"} else {"📄"};
                                let tc = if is_selected {Color32::from_rgb(0xFF,0xFF,0x80)} else if is_folder {Color32::WHITE} else {Color32::from_rgb(0xCC,0xCC,0xCC)};
                                p.text(Pos2::new(rect.min.x+indent+16.0,rect.center().y),egui::Align2::LEFT_CENTER,format!("{icon} {}",c.name),egui::FontId::proportional(13.0),tc);
                                if resp.clicked(){clicked_row.set(row_idx);}
                            });
                            // 父占比
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());draw_bar(ui.painter(),r,pct,bar_color);if resp.clicked(){clicked_row.set(row_idx);}});
                            // 总占比
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());draw_bar(ui.painter(),r,total_pct,Color32::from_rgb(0x4C,0x8B,0xF5));if resp.clicked(){clicked_row.set(row_idx);}});
                            // 逻辑大小
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(c.logical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0x4C,0x8B,0xF5));if resp.clicked(){clicked_row.set(row_idx);}});
                            // 修改时间
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let s=format_filetime(c.modified_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked_row.set(row_idx);}});
                            // 物理大小
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(c.physical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0xF5,0xA6,0x23));if resp.clicked(){clicked_row.set(row_idx);}});
                            // 创建时间
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let s=format_filetime(c.created_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked_row.set(row_idx);}});
                            // 访问时间
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let s=format_filetime(c.accessed_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked_row.set(row_idx);}});
                            // 项目/文件/文件夹
                            for val in [if is_folder{c.file_count+c.folder_count}else{0}, if is_folder{c.file_count}else{0}, if is_folder{c.folder_count}else{0}] {
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if is_folder{format!("{}",val)}else{"—".into()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked_row.set(row_idx);}});
                            }
                            // 属性
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format_attributes(c.attributes),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked_row.set(row_idx);}});
                            // 重解析点
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if c.reparse_tag!=0{format!("0x{:X}",c.reparse_tag)}else{"—".into()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked_row.set(row_idx);}});
                            // 保留
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if c.is_reserved{"是"}else{"—"};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked_row.set(row_idx);}});
                            // 所有者
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if c.owner.is_empty(){"—".into()}else{c.owner.clone()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked_row.set(row_idx);}});
                            // 空列（remainder 填充）
                            row.col(|_ui|{});
                        }
                    }
                });

                // 处理点击
                let clicked_idx = clicked_row.into_inner();
                if clicked_idx != usize::MAX && clicked_idx < flat_rows.len() {
                    let fr = &flat_rows[clicked_idx];
                    match &fr.kind {
                        RowKind::Disk { pi } => {
                            final_action = TreeAction::ToggleExpand(vec![*pi]);
                        }
                        RowKind::Child { node, abs_path, .. } => {
                            let child = unsafe { &**node };
                            let abs = abs_path.clone();
                            final_action = if child.is_folder() {
                                TreeAction::ToggleExpand(abs)
                            } else {
                                TreeAction::Select(abs)
                            };
                        }
                    }
                }
                let _ = &mut final_action;
                action_cell.set(final_action);
            });
    });
    action_cell.into_inner()
}

fn draw_bar(painter: &egui::Painter, cell: Rect, pct: f32, color: Color32) {
    let pad = 4.0; let bar_h = 10.0;
    let bar_w = (cell.width() - pad*2.0).max(0.0);
    let br = Rect::from_min_size(Pos2::new(cell.min.x+pad, cell.center().y-bar_h/2.0), Vec2::new(bar_w, bar_h));
    painter.rect_filled(br, 2.0, Color32::from_rgb(0x48,0x48,0x52));
    let fill_w = (bar_w * pct.clamp(0.0,1.0)).max(0.0);
    if fill_w > 0.5 { painter.rect_filled(Rect::from_min_size(br.min, Vec2::new(fill_w, bar_h)), 2.0, color); }
    painter.text(br.center(), egui::Align2::CENTER_CENTER, format!("{:.2}%", pct*100.0), egui::FontId::proportional(9.5), Color32::WHITE);
}

fn depth_color(depth: u32, is_folder: bool) -> Color32 {
    if !is_folder { return Color32::from_rgb(0x6C,0x75,0x7D); }
    const PAL: [Color32;6] = [Color32::from_rgb(0x4C,0x8B,0xF5),Color32::from_rgb(0x34,0xC7,0x59),Color32::from_rgb(0xF5,0xA6,0x23),Color32::from_rgb(0xE0,0x55,0x5B),Color32::from_rgb(0x9C,0x6A,0xDE),Color32::from_rgb(0x2E,0xC4,0xB6)];
    PAL[depth as usize % PAL.len()]
}
