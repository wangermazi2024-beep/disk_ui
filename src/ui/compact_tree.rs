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
        let rel_path: NodePath = vec![0]; // abs_path[0] 固定占位（保持和主列表一样的 [分区下标, ...] 形状）
        collect_rows(root, &rel_path, &mut flat_rows);

        // 名称 | 大小 | 修改时间 | 路径（放最后，用 remainder，避免中间夹在两个固定宽度列
        // 之间时缺一条可视的拖拽分隔线，看起来不像两个独立的列）。
        let builder = egui_extras::TableBuilder::new(ui)
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .sense(egui::Sense::click())
            .column(egui_extras::Column::initial(280.0).at_least(120.0).clip(true).resizable(true)) // 名称
            .column(egui_extras::Column::initial(90.0).clip(true).resizable(true)) // 大小
            .column(egui_extras::Column::initial(130.0).clip(true).resizable(true)) // 修改时间
            .column(egui_extras::Column::remainder().at_least(150.0).clip(true).resizable(true)); // 路径

        builder
            .header(ROW_H, |mut h| {
                for c in ["名称", "大小", "修改时间", "路径"] {
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
                    let hidden = n.is_hidden_or_system();
                    let indent = fr.depth as f32 * 16.0 + 4.0;
                    let full_path = n.full_path_override.clone().unwrap_or_default();
                    let is_group = fr.is_group;

                    // 每一列都要能点击（选中/右键菜单），不是只有名称那一列能点——
                    // 统一在每个 row.col() 开头画背景（选中蓝底/隐藏淡橙底）+ 建立点击感应区，
                    // 四列各自画一段，视觉上连起来就是一整行的高亮，而不是只有名称那一小块。
                    let paint_bg_and_sense = |ui: &mut egui::Ui| -> egui::Response {
                        let rect = ui.available_rect_before_wrap();
                        let resp = ui.allocate_rect(rect, Sense::click());
                        if hidden {
                            ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0xF5, 0xA6, 0x23, 0x1C));
                        }
                        if is_selected {
                            ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0x4C, 0x8B, 0xF5, 0x40));
                        }
                        resp
                    };
                    let handle_click = |resp: &egui::Response| {
                        if resp.clicked() || resp.secondary_clicked() { clicked_row.set(Some(row_idx)); }
                    };

                    // 名称
                    row.col(|ui| {
                        let rect = ui.available_rect_before_wrap();
                        let resp = paint_bg_and_sense(ui);
                        let guide_color = Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x14);
                        for lvl in 0..fr.depth {
                            let x = rect.min.x + lvl as f32 * 16.0 + 10.0;
                            ui.painter().line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], egui::Stroke::new(1.0, guide_color));
                        }
                        let p = ui.painter();
                        if is_group {
                            p.text(Pos2::new(rect.min.x + indent, rect.center().y), egui::Align2::LEFT_CENTER,
                                if n.expanded { "▼" } else { "▶" }, egui::FontId::proportional(10.0), Color32::from_rgb(0xAA, 0xCC, 0xFF));
                        }
                        let icon = if is_group { "🗀" } else { "📄" };
                        let tc = if is_selected { Color32::from_rgb(0xFF, 0xFF, 0x80) }
                            else if is_group { Color32::WHITE } else { Color32::from_rgb(0xCC, 0xCC, 0xCC) };
                        let mut text_x = rect.min.x + indent + 16.0;
                        if hidden {
                            let badge = egui::Rect::from_min_size(Pos2::new(text_x, rect.center().y - 7.0), egui::vec2(14.0, 14.0));
                            p.rect_filled(badge, 3.0, Color32::from_rgb(0xF5, 0xA6, 0x23));
                            p.text(badge.center(), egui::Align2::CENTER_CENTER, "H", egui::FontId::proportional(9.5), Color32::from_rgb(0x2A, 0x2A, 0x2E));
                            text_x += 18.0;
                        }
                        p.text(Pos2::new(text_x, rect.center().y), egui::Align2::LEFT_CENTER, format!("{icon} {}", n.name), egui::FontId::proportional(13.0), tc);
                        handle_click(&resp);
                        if !is_group { resp.context_menu(|ui| context_menu(ui, &n.name, &full_path)); }
                    });
                    // 大小
                    row.col(|ui| {
                        let resp = paint_bg_and_sense(ui);
                        ui.painter().text(ui.available_rect_before_wrap().left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER,
                            human_size(n.logical_size), egui::FontId::proportional(12.0), Color32::from_rgb(0xD0, 0xD0, 0xD0));
                        handle_click(&resp);
                        if !is_group { resp.context_menu(|ui| context_menu(ui, &n.name, &full_path)); }
                    });
                    // 修改时间
                    row.col(|ui| {
                        let resp = paint_bg_and_sense(ui);
                        if !is_group {
                            ui.painter().text(ui.available_rect_before_wrap().left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER,
                                format_filetime_local(n.modified_ft), egui::FontId::proportional(11.0), Color32::from_rgb(0xC0, 0xC0, 0xC0));
                        }
                        handle_click(&resp);
                        if !is_group { resp.context_menu(|ui| context_menu(ui, &n.name, &full_path)); }
                    });
                    // 路径（只有真实文件才有意义，分组行留空）
                    row.col(|ui| {
                        let resp = paint_bg_and_sense(ui);
                        if !is_group {
                            let dir = full_path.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
                            ui.painter().text(ui.available_rect_before_wrap().left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER,
                                dir, egui::FontId::proportional(11.5), Color32::from_rgb(0xA0, 0xA0, 0xA0));
                        }
                        handle_click(&resp);
                        if !is_group { resp.context_menu(|ui| context_menu(ui, &n.name, &full_path)); }
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
fn collect_rows(root: &Node, rel_path: &NodePath, rows: &mut Vec<FlatRow>) {
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

/// Windows 下打开资源管理器并选中某个文件。
///
/// 关键点：必须用 `raw_arg` 而不是普通的 `arg`。Rust 标准库的 `Command::arg()` 在
/// Windows 上会对参数里的引号做自己的转义（比如把 `"` 转成 `\"`），这是为了让参数能被
/// 标准的 C 运行时命令行解析器正确还原成"一个完整参数"。但 explorer.exe 对 `/select,`
/// 这种开关根本不走那套标准解析逻辑，它想看到的就是命令行里字面意义上的引号字符。
/// Rust 加了转义之后，explorer.exe 解析不出真正的路径，会静默失败、退化成打开默认位置
/// （很多机器上是"文档"）——这正是"用资源管理器打开却跳到 Documents"的真正原因，
/// 不是我们拼的路径本身有问题（日志和命令行手动跑都是对的，只是 Rust 在发给
/// explorer.exe 之前，偷偷改了一遍我们没让它改的字符）。`raw_arg` 会原样追加文本，
/// 不做任何转义，这样 explorer.exe 收到的命令行就和你在 cmd.exe 里手动敲的完全一样。
#[cfg(windows)]
fn open_in_explorer_select(path: &str) {
    if path.is_empty() { return; }
    use std::os::windows::process::CommandExt;
    let arg = format!("/select,\"{path}\"");
    crate::applog::log(&format!("[compact_tree] 打开资源管理器: explorer {arg}"));
    if let Err(e) = std::process::Command::new("explorer").raw_arg(&arg).spawn() {
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
