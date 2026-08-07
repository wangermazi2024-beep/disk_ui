//! "分析视图"专用的树状列表（文件扩展名分类 / 重复文件查找共用）。
//!
//! 和主列表（tree_list.rs）视觉语言一致——能展开、有缩进参考线、隐藏项有徽标、
//! 右键菜单能复制路径/打开所在文件夹——但列不一样：主列表那 15 列里，父占比/总占比/
//! 所有者/重解析点/保留 这些在"按扩展名/大小重新分组"的场景里没有意义（文件不是挂在
//! 真实目录树里的），去掉换成一列"路径"，让用户知道具体是哪个文件、在哪，
//! 不然只看文件名根本不知道要删的是哪一个。
//!
//! 没有直接在 tree_list.rs 里加一个"精简列模式"开关，是刻意的：tree_list.rs 那 15 列
//! 的表头声明和每一行的渲染必须严格一一对应，再加一套完全不同的列会让"这次改了列数、
//! 那次漏改"的风险成倍增加，独立开一个文件、各自的列各自维护，互不影响。

use std::cell::Cell;
use egui::{Color32, Pos2, Sense};
use crate::format::{format_filetime_local, human_size};
use crate::model::{Node, NodePath};
use crate::ui::TreeAction;

const ROW_H: f32 = 24.0;

struct FlatRow {
    node: *const Node,
    abs_path: NodePath,
    depth: u32,
    is_group: bool, // 分组文件夹（扩展名/大小组）还是真实文件
}

/// `root`：合成树的根（它自己不显示，只显示它的直接子项——扩展名/大小分组）。
pub fn show(ui: &mut egui::Ui, root: &Node, selected: &Option<NodePath>) -> TreeAction {
    let action_cell: Cell<TreeAction> = Cell::new(TreeAction::None);

    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        let mut flat_rows: Vec<FlatRow> = Vec::new();
        let mut rel_path: NodePath = vec![0]; // abs_path[0] 固定占位（保持和主列表一样的 [分区下标, ...] 形状）
        collect_rows(root, &mut rel_path, 0, &mut flat_rows);

        let builder = egui_extras::TableBuilder::new(ui)
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .sense(egui::Sense::click())
            .column(egui_extras::Column::initial(280.0).at_least(120.0).clip(true).resizable(true)) // 名称
            .column(egui_extras::Column::remainder().at_least(200.0).clip(true)) // 路径
            .column(egui_extras::Column::initial(90.0).clip(true).resizable(true)) // 大小
            .column(egui_extras::Column::initial(130.0).clip(true).resizable(true)); // 修改时间

        builder
            .header(ROW_H, |mut h| {
                for c in ["名称", "路径", "大小", "修改时间"] {
                    h.col(|ui| { ui.label(egui::RichText::new(c).strong().size(12.0).color(Color32::WHITE)); });
                }
            })
            .body(|body| {
                let clicked_row: Cell<Option<usize>> = Cell::new(None);
                body.rows(ROW_H, flat_rows.len(), |mut row| {
                    let row_idx = row.index();
                    let fr = &flat_rows[row_idx];
                    let n: &Node = unsafe { &*fr.node };
                    let is_selected = selected.as_ref() == Some(&fr.abs_path);
                    let indent = fr.depth as f32 * 16.0 + 4.0;
                    let full_path = n.full_path_override.clone().unwrap_or_default();

                    // 名称
                    row.col(|ui| {
                        let rect = ui.available_rect_before_wrap();
                        let resp = ui.allocate_rect(rect, Sense::click());
                        let hidden = n.is_hidden_or_system();
                        if hidden {
                            ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0xF5, 0xA6, 0x23, 0x1C));
                        }
                        if is_selected {
                            ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0x4C, 0x8B, 0xF5, 0x40));
                        }
                        let guide_color = Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x14);
                        for lvl in 0..fr.depth {
                            let x = rect.min.x + lvl as f32 * 16.0 + 10.0;
                            ui.painter().line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], egui::Stroke::new(1.0, guide_color));
                        }
                        let p = ui.painter();
                        if fr.is_group {
                            p.text(Pos2::new(rect.min.x + indent, rect.center().y), egui::Align2::LEFT_CENTER,
                                if n.expanded { "▼" } else { "▶" }, egui::FontId::proportional(10.0), Color32::from_rgb(0xAA, 0xCC, 0xFF));
                        }
                        let icon = if fr.is_group { "🗀" } else { "📄" };
                        let tc = if is_selected { Color32::from_rgb(0xFF, 0xFF, 0x80) }
                            else if fr.is_group { Color32::WHITE } else { Color32::from_rgb(0xCC, 0xCC, 0xCC) };
                        let mut text_x = rect.min.x + indent + 16.0;
                        if hidden {
                            let badge = egui::Rect::from_min_size(Pos2::new(text_x, rect.center().y - 7.0), egui::vec2(14.0, 14.0));
                            p.rect_filled(badge, 3.0, Color32::from_rgb(0xF5, 0xA6, 0x23));
                            p.text(badge.center(), egui::Align2::CENTER_CENTER, "H", egui::FontId::proportional(9.5), Color32::from_rgb(0x2A, 0x2A, 0x2E));
                            text_x += 18.0;
                        }
                        p.text(Pos2::new(text_x, rect.center().y), egui::Align2::LEFT_CENTER, format!("{icon} {}", n.name), egui::FontId::proportional(13.0), tc);
                        if resp.clicked() || resp.secondary_clicked() { clicked_row.set(Some(row_idx)); }
                        if !fr.is_group {
                            resp.context_menu(|ui| context_menu(ui, &n.name, &full_path));
                        }
                    });
                    // 路径（只有真实文件才有意义，分组行留空）
                    row.col(|ui| {
                        if !fr.is_group {
                            let dir = full_path.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
                            ui.label(egui::RichText::new(dir).size(11.5).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                        }
                    });
                    // 大小
                    row.col(|ui| { ui.label(human_size(n.logical_size)); });
                    // 修改时间
                    row.col(|ui| {
                        if !fr.is_group {
                            ui.label(format_filetime_local(n.modified_ft));
                        }
                    });
                });
                if let Some(idx) = clicked_row.get() {
                    let p = flat_rows[idx].abs_path.clone();
                    if flat_rows[idx].is_group {
                        action_cell.set(TreeAction::ToggleExpand(p));
                    } else {
                        action_cell.set(TreeAction::Select(p));
                    }
                }
            });
    });

    action_cell.into_inner()
}

/// 只有两层：合成根的直接子项（分组）+ 分组展开后的文件。迭代式，栈存相对路径。
fn collect_rows(root: &Node, rel_path: &mut NodePath, _depth: u32, rows: &mut Vec<FlatRow>) {
    struct Item<'a> { node: &'a Node, abs_path: NodePath, depth: u32, is_group: bool }
    let mut stack: Vec<Item> = root.children.iter().enumerate().rev().map(|(i, g)| {
        let mut p = rel_path.clone();
        p.push(i);
        Item { node: g, abs_path: p, depth: 0, is_group: true }
    }).collect();
    while let Some(item) = stack.pop() {
        rows.push(FlatRow { node: item.node as *const Node, abs_path: item.abs_path.clone(), depth: item.depth, is_group: item.is_group });
        if item.is_group && item.node.expanded {
            for (i, f) in item.node.children.iter().enumerate().rev() {
                let mut p = item.abs_path.clone();
                p.push(i);
                stack.push(Item { node: f, abs_path: p, depth: item.depth + 1, is_group: false });
            }
        }
    }
}

/// Windows 下打开资源管理器并选中某个文件；explorer.exe 的 `/select,` 开关对带空格的
/// 路径必须整体带双引号才能正确定位，不然会静默失败、退化成打开默认位置（比如"文档"），
/// 而不是报错——这是 Windows 一个有据可查的老毛病，不是我们这边逻辑错了。
#[cfg(windows)]
fn open_in_explorer_select(path: &str) {
    if path.is_empty() { return; }
    let arg = format!("/select,\"{path}\"");
    crate::applog::log(&format!("[compact_tree] 打开资源管理器: explorer {arg}"));
    if let Err(e) = std::process::Command::new("explorer").arg(arg).spawn() {
        crate::applog::log(&format!("[compact_tree] 打开资源管理器失败 ({path}): {e}"));
    }
}
#[cfg(not(windows))]
fn open_in_explorer_select(_path: &str) {}

fn context_menu(ui: &mut egui::Ui, name: &str, full_path: &str) {
    ui.set_min_width(180.0);
    if ui.button("📂 打开所在文件夹").clicked() {
        open_in_explorer_select(full_path);
        ui.close();
    }
    if ui.button("📋 复制路径").clicked() {
        ui.ctx().copy_text(full_path.to_string());
        ui.close();
    }
    if ui.button("📋 复制名称").clicked() {
        ui.ctx().copy_text(name.to_string());
        ui.close();
    }
    ui.separator();
    ui.add_enabled_ui(false, |ui| {
        let _ = ui.button("🔗 创建符号链接（开发中）");
        let _ = ui.button("🗑 删除（开发中）");
    });
}
