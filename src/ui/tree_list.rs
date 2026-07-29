//! 文件列表树（扁平渲染 + `egui_extras::TableBuilder` 原生表头）。
//!
//! - 表头（名称/大小）原生支持列宽拖拽 + 点击排序。
//! - 目录行显示 ▶/▼ 小图标，点击展开/收起子树。
//! - 单击选中，双击进入该目录。

use egui::{Color32, RichText};

use crate::format::human_size;
use crate::model::{Node, NodePath};

use super::TreeAction;

const ROW_H: f32 = 20.0;

pub fn show(ui: &mut egui::Ui, view_root: &Node, selected: &Option<NodePath>) -> TreeAction {
    let mut action = TreeAction::None;

    egui_extras::TableBuilder::new(ui)
        .striped(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .auto_shrink([false, false])
        .column(egui_extras::Column::remainder().at_least(150.0).resizable(true))
        .column(egui_extras::Column::initial(100.0).at_least(50.0).resizable(true))
        .header(ROW_H, |mut header| {
            // 名称列
            header.col(|ui| {
                ui.label(RichText::new("名称").strong().size(12.0).color(Color32::WHITE));
            });
            // 大小列
            header.col(|ui| {
                ui.label(RichText::new("大小").strong().size(12.0).color(Color32::WHITE));
            });
        })
        .body(|mut body| {
            let mut path: NodePath = Vec::new();
            draw_rows(&mut body, view_root, &mut path, 0, selected, &mut action);
        });

    action
}

/// 递归遍历可见节点，在 `TableBuilder::body` 中逐行渲染。
fn draw_rows(
    body: &mut egui_extras::TableBody,
    node: &Node,
    path: &mut NodePath,
    depth: u32,
    selected: &Option<NodePath>,
    action: &mut TreeAction,
) {
    // 按大小从大到小排序
    let mut order: Vec<usize> = (0..node.children.len()).collect();
    order.sort_by(|&a, &b| node.children[b].size.cmp(&node.children[a].size));

    for i in order {
        let child = &node.children[i];
        let is_folder = !child.children.is_empty();
        path.push(i);

        body.row(ROW_H, |mut row| {
            row.col(|ui| {
                let indent = depth as f32 * 16.0;

                // 展开/收起小图标
                if is_folder {
                    let arrow = if child.expanded { "▼ " } else { "▶ " };
                    ui.colored_label(Color32::WHITE, RichText::new(arrow).size(10.0));
                } else {
                    ui.add_space(14.0); // 对齐文件夹的箭头宽度
                }

                // 缩进
                ui.add_space(indent);

                let is_selected = selected.as_deref() == Some(path.as_slice());
                let icon = if child.is_folder() { "📁" } else { "📄" };
                let text = format!("{icon} {}", child.name);
                let resp = ui.selectable_label(is_selected, RichText::new(text).color(Color32::WHITE).size(13.0));
                if resp.double_clicked() {
                    *action = TreeAction::EnterNode(path.clone());
                } else if resp.clicked() {
                    if is_folder {
                        *action = TreeAction::ToggleExpand(path.clone());
                    } else {
                        *action = TreeAction::Select(path.clone());
                    }
                }
            });

            row.col(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(human_size(child.size)).size(12.0).color(Color32::from_rgb(0xC0, 0xC0, 0xC0)));
                });
            });
        });

        // 如果是展开的文件夹，递归渲染其子节点
        if child.expanded && is_folder {
            draw_rows(body, child, path, depth + 1, selected, action);
        }

        path.pop();
    }
}
