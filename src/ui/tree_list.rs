//! 文件列表树（WinDirStat 风格全量列）。
//!
//! 列：名称 | 大小占比条 | 百分比 | Physical Size | Logical Size | 项目 | 文件 | 文件夹
//!      | 修改时间 | 属性 | Reparse | 保留

use std::cell::Cell;
use egui::{Color32, Pos2, Rect, Sense, Vec2};
use crate::disk_info::DiskInfo;
use crate::format::{format_attributes, format_filetime, human_size, human_size_compact};
use crate::model::{Node, NodePath};
use super::TreeAction;

const ROW_H: f32 = 22.0;
const DISK_ROW_H: f32 = 68.0;

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
            .auto_shrink([false, false]);
        builder = builder
            .column(egui_extras::Column::initial(200.0).at_least(120.0).clip(true).resizable(true))  // 名称
            .column(egui_extras::Column::initial(90.0).resizable(true))   // 占比条
            .column(egui_extras::Column::initial(60.0).resizable(true))   // 百分比
            .column(egui_extras::Column::initial(85.0).resizable(true))   // Physical
            .column(egui_extras::Column::initial(85.0).resizable(true))   // Logical
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 项目
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 文件
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 文件夹
            .column(egui_extras::Column::initial(120.0).resizable(true))  // 修改时间
            .column(egui_extras::Column::initial(50.0).resizable(true))   // 属性
            .column(egui_extras::Column::initial(55.0).resizable(true))   // Reparse
            .column(egui_extras::Column::initial(40.0).resizable(true));  // 保留
        builder
            .header(ROW_H, |mut h| {
                let cols = ["名称", "占比", "%", "Physical", "Logical", "项目", "文件", "文件夹", "修改时间", "属性", "Reparse", "保留"];
                for c in cols { h.col(|ui| { ui.label(egui::RichText::new(c).strong().size(12.0).color(Color32::WHITE)); }); }
            })
            .body(|mut body| {
                let mut final_action = TreeAction::None;
                for (pi, partition) in partitions.iter().enumerate() {
                    let info = partition_infos.get(pi).and_then(|i| i.as_ref());
                    let part_path = vec![pi];
                    let part_selected = selected.as_deref() == Some(&[pi]);
                    let total = info.map(|i| i.total_bytes).unwrap_or(partition.logical_size.max(1));
                    let part_pct = if total > 0 { partition.logical_size as f32 / total as f32 } else { 0.0 };
                    let part_clicked = Cell::new(false);

                    body.row(DISK_ROW_H, |mut row| {
                        // 名称
                        row.col(|ui| {
                            let rect = ui.available_rect_before_wrap();
                            let resp = ui.allocate_rect(rect, Sense::click());
                            if part_selected {
                                ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0x4C, 0x8B, 0xF5, 0x40));
                            }
                            let arrow = if partition.expanded { "▼" } else { "▶" };
                            ui.painter().text(Pos2::new(rect.min.x+2.0, rect.center().y), egui::Align2::LEFT_CENTER, arrow, egui::FontId::proportional(10.0), Color32::from_rgb(0xAA,0xCC,0xFF));
                            ui.painter().text(Pos2::new(rect.min.x+18.0, rect.center().y), egui::Align2::LEFT_CENTER, format!("💾 {}", partition.name), egui::FontId::proportional(13.0), if part_selected {Color32::from_rgb(0xFF,0xFF,0x80)} else {Color32::from_rgb(0xFF,0xD7,0x00)});
                            if resp.clicked() { part_clicked.set(true); }
                        });
                        // 占比条
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); draw_bar(ui.painter(),r,part_pct,Color32::from_rgb(0xFF,0xD7,0x00)); if resp.clicked(){part_clicked.set(true);} });
                        // 百分比
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format!("{:.1}%",part_pct*100.0),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        // Physical / Logical / 项目/文件/文件夹 / 修改时间/属性/Reparse/保留
                        // 磁盘行大小列多行显示 总/已分配/未分配/扫描logical/扫描physical
                        row.col(|ui| {
                            // Physical Size 列：磁盘行多行
                            let rect = ui.available_rect_before_wrap();
                            let resp = ui.allocate_rect(rect, Sense::click());
                            let p = ui.painter();
                            let line_h = 12.0;
                            let start_y = rect.min.y + 3.0;
                            let lines: Vec<(Color32,String)> = if let Some(i)=info {
                                vec![
                                    (Color32::from_rgb(0xE0,0xE0,0xE0), human_size_compact(i.total_bytes)),
                                    (Color32::from_rgb(0xF5,0xA6,0x23), human_size_compact(i.used_bytes)),
                                    (Color32::from_rgb(0x34,0xC7,0x59), human_size_compact(i.free_bytes)),
                                    (Color32::from_rgb(0xF5,0xA6,0x23), human_size_compact(partition.physical_size)),
                                ]
                            } else { vec![(Color32::from_rgb(0xE0,0xE0,0xE0), human_size_compact(partition.physical_size))] };
                            for (idx,(c,v)) in lines.iter().enumerate() {
                                p.text(Pos2::new(rect.max.x-4.0, start_y+idx as f32*line_h), egui::Align2::RIGHT_TOP, v, egui::FontId::proportional(9.5), *c);
                            }
                            if resp.clicked(){part_clicked.set(true);}
                        });
                        row.col(|ui| {
                            let rect = ui.available_rect_before_wrap();
                            let resp = ui.allocate_rect(rect, Sense::click());
                            let p = ui.painter();
                            let line_h = 12.0;
                            let start_y = rect.min.y + 3.0;
                            let lines: Vec<(Color32,String)> = if let Some(_)=info {
                                vec![
                                    (Color32::from_rgb(0x4C,0x8B,0xF5), human_size_compact(partition.logical_size)),
                                ]
                            } else { vec![(Color32::from_rgb(0x4C,0x8B,0xF5), human_size_compact(partition.logical_size))] };
                            for (idx,(c,v)) in lines.iter().enumerate() {
                                p.text(Pos2::new(rect.max.x-4.0, start_y+idx as f32*line_h), egui::Align2::RIGHT_TOP, v, egui::FontId::proportional(9.5), *c);
                            }
                            if resp.clicked(){part_clicked.set(true);}
                        });
                        // 项目/文件/文件夹
                        for val in [partition.file_count+partition.folder_count, partition.file_count, partition.folder_count] {
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format!("{}",val),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        }
                        // 修改时间 → 磁盘行显示文件系统
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=info.map(|i|i.file_system.clone()).filter(|s|!s.is_empty()).unwrap_or_else(||format_filetime(partition.modified_ft)); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(11.0),Color32::from_rgb(0xA0,0xC0,0xE0)); if resp.clicked(){part_clicked.set(true);} });
                        // 属性
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format_attributes(partition.attributes),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        // Reparse
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=if partition.reparse_tag!=0 {format!("0x{:X}",partition.reparse_tag)}else{"—".into()}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        // 保留
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=if partition.is_reserved {"是"}else{"—"}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                    });
                    if part_clicked.get() { final_action = TreeAction::ToggleExpand(part_path.clone()); }
                    if partition.expanded {
                        let mut rel_path: NodePath = Vec::new();
                        draw_rows(&mut body, partition, pi, &mut rel_path, 0, selected, &mut final_action, partition.logical_size.max(1));
                    }
                }
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
    painter.text(br.center(), egui::Align2::CENTER_CENTER, format!("{:.1}%", pct*100.0), egui::FontId::proportional(9.5), Color32::WHITE);
}

fn depth_color(depth: u32, is_folder: bool) -> Color32 {
    if !is_folder { return Color32::from_rgb(0x6C,0x75,0x7D); }
    const PAL: [Color32;6] = [Color32::from_rgb(0x4C,0x8B,0xF5),Color32::from_rgb(0x34,0xC7,0x59),Color32::from_rgb(0xF5,0xA6,0x23),Color32::from_rgb(0xE0,0x55,0x5B),Color32::from_rgb(0x9C,0x6A,0xDE),Color32::from_rgb(0x2E,0xC4,0xB6)];
    PAL[depth as usize % PAL.len()]
}

fn draw_rows(
    body: &mut egui_extras::TableBody, node: &Node, pi: usize,
    rel_path: &mut Vec<usize>, depth: u32,
    selected: &Option<NodePath>, action: &mut TreeAction, parent_size: u64,
) {
    let mut order: Vec<usize> = (0..node.children.len()).collect();
    order.sort_by(|&a,&b| node.children[b].logical_size.cmp(&node.children[a].logical_size));
    for i in order {
        let child = &node.children[i];
        let is_folder = child.is_folder();
        rel_path.push(i);
        let mut abs_path = vec![pi];
        abs_path.extend_from_slice(rel_path);
        let is_selected = selected.as_deref() == Some(&abs_path);
        let pct = if parent_size > 0 { child.logical_size as f32 / parent_size as f32 } else { 0.0 };
        let bar_color = depth_color(depth, is_folder);
        let indent = (depth+1) as f32 * 16.0 + 2.0;
        let clicked = Cell::new(false);

        body.row(ROW_H, |mut row| {
            // 名称
            row.col(|ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, Sense::click());
                if is_selected { ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }
                let p = ui.painter();
                if is_folder { p.text(Pos2::new(rect.min.x+indent,rect.center().y),egui::Align2::LEFT_CENTER,if child.expanded{"▼"}else{"▶"},egui::FontId::proportional(10.0),Color32::from_rgb(0xAA,0xCC,0xFF)); }
                let icon = if is_folder {"📁"} else {"📄"};
                let tc = if is_selected {Color32::from_rgb(0xFF,0xFF,0x80)} else if is_folder {Color32::WHITE} else {Color32::from_rgb(0xCC,0xCC,0xCC)};
                p.text(Pos2::new(rect.min.x+indent+16.0,rect.center().y),egui::Align2::LEFT_CENTER,format!("{icon} {}",child.name),egui::FontId::proportional(13.0),tc);
                if resp.clicked(){clicked.set(true);}
            });
            // 占比条
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());draw_bar(ui.painter(),r,pct,bar_color);if resp.clicked(){clicked.set(true);}});
            // 百分比
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format!("{:.1}%",pct*100.0),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
            // Physical
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(egui::pos2(r.max.x-4.0,r.center().y),egui::Align2::RIGHT_CENTER,human_size(child.physical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0xF5,0xA6,0x23));if resp.clicked(){clicked.set(true);}});
            // Logical
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(egui::pos2(r.max.x-4.0,r.center().y),egui::Align2::RIGHT_CENTER,human_size(child.logical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0x4C,0x8B,0xF5));if resp.clicked(){clicked.set(true);}});
            // 项目/文件/文件夹
            for val in [if is_folder{child.file_count+child.folder_count}else{0}, if is_folder{child.file_count}else{0}, if is_folder{child.folder_count}else{0}] {
                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if is_folder{format!("{}",val)}else{"—".into()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
            }
            // 修改时间
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let s=format_filetime(child.modified_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
            // 属性
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format_attributes(child.attributes),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
            // Reparse
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if child.reparse_tag!=0{format!("0x{:X}",child.reparse_tag)}else{"—".into()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
            // 保留
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if child.is_reserved{"是"}else{"—"};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
        });

        let abs_clone = abs_path.clone();
        if clicked.get() {
            *action = if is_folder { TreeAction::ToggleExpand(abs_clone) } else { TreeAction::Select(abs_clone) };
        }
        if child.expanded && is_folder {
            draw_rows(body, child, pi, rel_path, depth+1, selected, action, child.logical_size.max(1));
        }
        rel_path.pop();
    }
}
