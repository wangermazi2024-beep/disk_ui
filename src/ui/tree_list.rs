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
            .column(egui_extras::Column::initial(70.0).resizable(true))   // 总占比
            .column(egui_extras::Column::initial(85.0).resizable(true))   // Physical
            .column(egui_extras::Column::initial(85.0).resizable(true))   // Logical
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 项目
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 文件
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 文件夹
            .column(egui_extras::Column::initial(120.0).resizable(true))  // 修改时间
            .column(egui_extras::Column::initial(50.0).resizable(true))   // 属性
            .column(egui_extras::Column::initial(55.0).resizable(true))   // Reparse
            .column(egui_extras::Column::initial(40.0).resizable(true))   // 保留
            .column(egui_extras::Column::initial(120.0).resizable(true))  // 创建时间
            .column(egui_extras::Column::initial(120.0).resizable(true))  // 访问时间
            .column(egui_extras::Column::initial(80.0).resizable(true));  // Owner
        builder
            .header(ROW_H, |mut h| {
                let cols = ["名称", "父占比", "总占比", "Physical", "Logical", "项目", "文件", "文件夹", "修改时间", "属性", "Reparse", "保留", "创建时间", "访问时间", "Owner"];
                for c in cols { h.col(|ui| { ui.label(egui::RichText::new(c).strong().size(12.0).color(Color32::WHITE)); }); }
            })
            .body(|mut body| {
                let mut final_action = TreeAction::None;
                for (pi, partition) in partitions.iter().enumerate() {
                    let info = partition_infos.get(pi).and_then(|i| i.as_ref());
                    let part_path = vec![pi];
                    let part_selected = selected.as_deref() == Some(&[pi]);
                    let total = info.map(|i| i.total_bytes).unwrap_or(partition.logical_size.max(1));
                    let disk_logical = partition.logical_size.max(1); // 用于子节点的总占比计算
                    let part_pct = if total > 0 { partition.logical_size as f32 / total as f32 } else { 0.0 };
                    let part_clicked = Cell::new(false);

                    body.row(DISK_ROW_H, |mut row| {
                        // 名称（含磁盘容量信息：总大小/已用/可用）
                        row.col(|ui| {
                            let rect = ui.available_rect_before_wrap();
                            let resp = ui.allocate_rect(rect, Sense::click());
                            if part_selected {
                                ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0x4C, 0x8B, 0xF5, 0x40));
                            }
                            let arrow = if partition.expanded { "▼" } else { "▶" };
                            ui.painter().text(Pos2::new(rect.min.x+2.0, rect.min.y+6.0), egui::Align2::LEFT_TOP, arrow, egui::FontId::proportional(10.0), Color32::from_rgb(0xAA,0xCC,0xFF));
                            ui.painter().text(Pos2::new(rect.min.x+18.0, rect.min.y+4.0), egui::Align2::LEFT_TOP, format!("💾 {}", partition.name), egui::FontId::proportional(13.0), if part_selected {Color32::from_rgb(0xFF,0xFF,0x80)} else {Color32::from_rgb(0xFF,0xD7,0x00)});
                            // 磁盘容量信息放在名称下方
                            if let Some(i) = info {
                                let y2 = rect.min.y + 22.0;
                                ui.painter().text(Pos2::new(rect.min.x+18.0, y2), egui::Align2::LEFT_TOP,
                                    format!("总: {}  已用: {}  可用: {}", human_size_compact(i.total_bytes), human_size_compact(i.used_bytes), human_size_compact(i.free_bytes)),
                                    egui::FontId::proportional(10.0), Color32::from_rgb(0xA0,0xC0,0xE0));
                                let y3 = rect.min.y + 36.0;
                                ui.painter().text(Pos2::new(rect.min.x+18.0, y3), egui::Align2::LEFT_TOP,
                                    format!("扫描: logical={}  physical={}", human_size_compact(partition.logical_size), human_size_compact(partition.physical_size)),
                                    egui::FontId::proportional(10.0), Color32::from_rgb(0xA0,0xA0,0xA0));
                            }
                            if resp.clicked() { part_clicked.set(true); }
                        });
                        // 占比条（磁盘行：100%，因为磁盘是根）
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); draw_bar(ui.painter(),r,1.0,Color32::from_rgb(0xFF,0xD7,0x00)); if resp.clicked(){part_clicked.set(true);} });
                        // 百分比（总占比 = 扫描logical / 磁盘总容量）
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format!("{:.1}%",part_pct*100.0),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        // Physical（磁盘行只显示扫描汇总）
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(egui::pos2(r.max.x-4.0,r.center().y),egui::Align2::RIGHT_CENTER,human_size(partition.physical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0xF5,0xA6,0x23)); if resp.clicked(){part_clicked.set(true);} });
                        // Logical（磁盘行只显示扫描汇总，居中）
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(partition.logical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0x4C,0x8B,0xF5)); if resp.clicked(){part_clicked.set(true);} });
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
                        // 创建时间
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let s=format_filetime(partition.created_ft); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        // 访问时间
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let s=format_filetime(partition.accessed_ft); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        // Owner
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=if partition.owner.is_empty(){"—".into()}else{partition.owner.clone()}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                    });
                    if part_clicked.get() { final_action = TreeAction::ToggleExpand(part_path.clone()); }
                    if partition.expanded {
                        let mut rel_path: NodePath = Vec::new();
                        draw_rows(&mut body, partition, pi, &mut rel_path, 0, selected, &mut final_action, partition.logical_size.max(1), disk_logical);
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
    painter.text(br.center(), egui::Align2::CENTER_CENTER, format!("{:.2}%", pct*100.0), egui::FontId::proportional(9.5), Color32::WHITE);
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
    disk_logical: u64,
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
        let pct = if parent_size > 0 { child.logical_size as f32 / parent_size as f32 } else { 0.0 }; // 父占比
        let total_pct = if disk_logical > 0 { child.logical_size as f32 / disk_logical as f32 } else { 0.0 }; // 总占比
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
            // 占比条（父占比）
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());draw_bar(ui.painter(),r,pct,bar_color);if resp.clicked(){clicked.set(true);}});
            // 百分比（总占比 = 相对磁盘根）
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format!("{:.2}%",total_pct*100.0),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
            // Physical（居中）
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(child.physical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0xF5,0xA6,0x23));if resp.clicked(){clicked.set(true);}});
            // Logical（居中）
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(child.logical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0x4C,0x8B,0xF5));if resp.clicked(){clicked.set(true);}});
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
            // 创建时间
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let s=format_filetime(child.created_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
            // 访问时间
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let s=format_filetime(child.accessed_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
            // Owner
            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if child.owner.is_empty(){"—".into()}else{child.owner.clone()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
        });

        let abs_clone = abs_path.clone();
        if clicked.get() {
            *action = if is_folder { TreeAction::ToggleExpand(abs_clone) } else { TreeAction::Select(abs_clone) };
        }
        if child.expanded && is_folder {
            draw_rows(body, child, pi, rel_path, depth+1, selected, action, child.logical_size.max(1), disk_logical);
        }
        rel_path.pop();
    }
}
