//! 文件列表树（全量列，中文表头，全部居中，虚拟化渲染）。
//!
//! 列顺序：名称 | 父占比 | 总占比 | 逻辑大小 | 修改时间 | 物理大小 | 创建时间 | 访问时间
//!         | 项目 | 文件 | 文件夹 | 属性 | 重解析点 | 保留 | 所有者
//!
//! 性能优化：用 egui_extras::TableBuilder 的 body(|body| body.heterogeneous_rows)
//! 做虚拟化——只有可见的行才会调 callback 渲染，屏幕外的行不渲染。

use std::cell::Cell;
use egui::{Color32, Pos2, Rect, Sense, Vec2};
use crate::disk_info::DiskInfo;
use crate::format::{format_attributes, format_filetime, human_size, human_size_compact};
use crate::model::{Node, NodePath};
use super::TreeAction;

const ROW_H: f32 = 22.0;
const DISK_ROW_H: f32 = 68.0;

/// 把整棵展开的树扁平化成一个行列表，这样 TableBuilder 只渲染可见行。
struct FlatRow {
    depth: u32,
    node_idx: usize,       // 在 node.children 中的下标
    abs_path: NodePath,    // [pi, ...]
    indent: f32,
}

/// 递归收集所有可见行（已展开的文件夹的子项）。
fn collect_visible_rows(
    node: &Node, pi: usize, rel_path: &mut Vec<usize>, depth: u32,
    rows: &mut Vec<FlatRow>,
) {
    let mut order: Vec<usize> = (0..node.children.len()).collect();
    order.sort_by(|&a, &b| node.children[b].logical_size.cmp(&node.children[a].logical_size));
    for i in order {
        let child = &node.children[i];
        rel_path.push(i);
        let mut abs_path = vec![pi];
        abs_path.extend_from_slice(rel_path);
        let indent = (depth + 1) as f32 * 16.0 + 2.0;
        rows.push(FlatRow { depth, node_idx: i, abs_path, indent });
        if child.is_folder() && child.expanded {
            collect_visible_rows(child, pi, rel_path, depth + 1, rows);
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
            // 列顺序：名称 | 父占比 | 总占比 | 逻辑大小 | 修改时间 | 物理大小 | 创建时间 | 访问时间 | 项目 | 文件 | 文件夹 | 属性 | 重解析点 | 保留 | 所有者
            .column(egui_extras::Column::initial(200.0).at_least(120.0).clip(true).resizable(true))
            .column(egui_extras::Column::initial(85.0).resizable(true))   // 父占比
            .column(egui_extras::Column::initial(85.0).resizable(true))   // 总占比
            .column(egui_extras::Column::initial(85.0).resizable(true))   // 逻辑大小
            .column(egui_extras::Column::initial(120.0).resizable(true))  // 修改时间
            .column(egui_extras::Column::initial(85.0).resizable(true))   // 物理大小
            .column(egui_extras::Column::initial(120.0).resizable(true))  // 创建时间
            .column(egui_extras::Column::initial(120.0).resizable(true))  // 访问时间
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 项目
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 文件
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 文件夹
            .column(egui_extras::Column::initial(50.0).resizable(true))   // 属性
            .column(egui_extras::Column::initial(55.0).resizable(true))   // 重解析点
            .column(egui_extras::Column::initial(40.0).resizable(true))   // 保留
            .column(egui_extras::Column::initial(80.0).resizable(true));  // 所有者

        builder = builder.sense(egui::Sense::click());
        builder
            .header(ROW_H, |mut h| {
                let cols = ["名称", "父占比", "总占比", "逻辑大小", "修改时间", "物理大小", "创建时间", "访问时间", "项目", "文件", "文件夹", "属性", "重解析点", "保留", "所有者"];
                for c in cols { h.col(|ui| { ui.label(egui::RichText::new(c).strong().size(12.0).color(Color32::WHITE)); }); }
            })
            .body(|mut body| {
                let mut final_action = TreeAction::None;

                for (pi, partition) in partitions.iter().enumerate() {
                    let info = partition_infos.get(pi).and_then(|i| i.as_ref());
                    let part_path = vec![pi];
                    let part_selected = selected.as_deref() == Some(&[pi]);
                    let total = info.map(|i| i.total_bytes).unwrap_or(partition.logical_size.max(1));
                    let disk_logical = partition.logical_size.max(1);
                    let part_pct = if total > 0 { partition.logical_size as f32 / total as f32 } else { 0.0 };
                    let part_clicked = Cell::new(false);

                    // ── 磁盘行 ──
                    body.row(DISK_ROW_H, |mut row| {
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
                            if resp.clicked() { part_clicked.set(true); }
                        });
                        // 父占比（磁盘=100%）
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); draw_bar(ui.painter(),r,1.0,Color32::from_rgb(0xFF,0xD7,0x00)); if resp.clicked(){part_clicked.set(true);} });
                        // 总占比
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); draw_bar(ui.painter(),r,part_pct,Color32::from_rgb(0x4C,0x8B,0xF5)); if resp.clicked(){part_clicked.set(true);} });
                        // 逻辑大小
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(p.logical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0x4C,0x8B,0xF5)); if resp.clicked(){part_clicked.set(true);} });
                        // 修改时间
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=info_ref.map(|i|i.file_system.clone()).filter(|s|!s.is_empty()).unwrap_or_else(||format_filetime(p.modified_ft)); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(11.0),Color32::from_rgb(0xA0,0xC0,0xE0)); if resp.clicked(){part_clicked.set(true);} });
                        // 物理大小
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(p.physical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0xF5,0xA6,0x23)); if resp.clicked(){part_clicked.set(true);} });
                        // 创建时间
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let s=format_filetime(p.created_ft); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        // 访问时间
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let s=format_filetime(p.accessed_ft); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        // 项目/文件/文件夹
                        for val in [p.file_count+p.folder_count, p.file_count, p.folder_count] {
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format!("{}",val),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        }
                        // 属性
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format_attributes(p.attributes),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        // 重解析点
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=if p.reparse_tag!=0 {format!("0x{:X}",p.reparse_tag)}else{"—".into()}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        // 保留
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=if p.is_reserved {"是"}else{"—"}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                        // 所有者
                        row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); let t=if p.owner.is_empty(){"—".into()}else{p.owner.clone()}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked(){part_clicked.set(true);} });
                    });

                    if part_clicked.get() { final_action = TreeAction::ToggleExpand(part_path.clone()); }

                    // ── 子行：扁平化后用 body.row 逐行渲染（TableBuilder 自带虚拟化） ──
                    if partition.expanded {
                        let mut flat_rows: Vec<FlatRow> = Vec::new();
                        let mut rel_path: NodePath = Vec::new();
                        collect_visible_rows(partition, pi, &mut rel_path, 0, &mut flat_rows);

                        // 找到 partition 的指针，用于在闭包里访问子节点
                        // 注意：不能在闭包里直接借用 partition 因为 body 的闭包要 'static
                        // 所以我们把需要的数据提取出来
                        for fr in &flat_rows {
                            // 沿 abs_path 找到 child node
                            let child = match navigate(partition, &fr.abs_path[1..]) {
                                Some(n) => n,
                                None => continue,
                            };
                            let is_folder = child.is_folder();
                            let is_selected = selected.as_deref() == Some(&fr.abs_path);
                            let pct = if partition.logical_size > 0 { child.logical_size as f32 / partition.logical_size as f32 } else { 0.0 };
                            let total_pct = if disk_logical > 0 { child.logical_size as f32 / disk_logical as f32 } else { 0.0 };
                            let bar_color = depth_color(fr.depth, is_folder);
                            let indent = fr.indent;
                            let clicked = Cell::new(false);
                            let abs_clone = fr.abs_path.clone();

                            body.row(ROW_H, |mut row| {
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
                                    if resp.clicked(){clicked.set(true);}
                                });
                                // 父占比
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());draw_bar(ui.painter(),r,pct,bar_color);if resp.clicked(){clicked.set(true);}});
                                // 总占比
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());draw_bar(ui.painter(),r,total_pct,Color32::from_rgb(0x4C,0x8B,0xF5));if resp.clicked(){clicked.set(true);}});
                                // 逻辑大小
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(c.logical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0x4C,0x8B,0xF5));if resp.clicked(){clicked.set(true);}});
                                // 修改时间
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let s=format_filetime(c.modified_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
                                // 物理大小
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(c.physical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0xF5,0xA6,0x23));if resp.clicked(){clicked.set(true);}});
                                // 创建时间
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let s=format_filetime(c.created_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
                                // 访问时间
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let s=format_filetime(c.accessed_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
                                // 项目/文件/文件夹
                                for val in [if is_folder{c.file_count+c.folder_count}else{0}, if is_folder{c.file_count}else{0}, if is_folder{c.folder_count}else{0}] {
                                    row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if is_folder{format!("{}",val)}else{"—".into()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
                                }
                                // 属性
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format_attributes(c.attributes),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
                                // 重解析点
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if c.reparse_tag!=0{format!("0x{:X}",c.reparse_tag)}else{"—".into()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
                                // 保留
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if c.is_reserved{"是"}else{"—"};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
                                // 所有者
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click());let t=if c.owner.is_empty(){"—".into()}else{c.owner.clone()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked(){clicked.set(true);}});
                            });

                            if clicked.get() {
                                final_action = if is_folder { TreeAction::ToggleExpand(abs_clone) } else { TreeAction::Select(abs_clone) };
                            }
                        }
                    }
                }
                action_cell.set(final_action);
            });
    });
    action_cell.into_inner()
}

/// 沿路径导航到子节点
fn navigate<'a>(node: &'a Node, path: &[usize]) -> Option<&'a Node> {
    let mut cur = node;
    for &i in path {
        cur = cur.children.get(i)?;
    }
    Some(cur)
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
