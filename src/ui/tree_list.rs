//! 递归展开的文件列表树，交互上参考 Everything.exe / WizTree 左侧的目录树：
//!
//! - 文件夹前面带箭头，点箭头/文字展开下一层子节点（`egui::CollapsingState` 自带的
//!   折叠状态是持久化的，靠 `path` 生成的 `Id` 区分每一行，重新渲染也不会丢展开状态）。
//! - 单击一行：选中该节点（跟 treemap 那边的高亮联动）。
//! - 双击一行：等价于在 treemap 里双击对应色块——把该节点的父节点提升为新的根节点。
//!
//! 这个视图始终从当前根节点展示完整树，和 treemap 是两种独立但共享同一份数据模型的浏览方式。

use egui::{Color32, RichText};

use crate::format::human_size;
use crate::model::{Node, NodePath};

use super::TreeAction;

pub fn show(ui: &mut egui::Ui, view_root: &Node, base_path: &[usize], selected: &Option<NodePath>) -> TreeAction {
    let mut action = TreeAction::None;
    let mut path = base_path.to_vec();
    draw_children(ui, view_root, &mut path, selected, &mut action);
    action
}

fn draw_children(ui: &mut egui::Ui, node: &Node, path: &mut NodePath, selected: &Option<NodePath>, action: &mut TreeAction) {
    // 按大小从大到小展示，符合"看占用空间"的使用场景，这也是 WizTree 的默认顺序。
    let mut order: Vec<usize> = (0..node.children.len()).collect();
    order.sort_by(|&a, &b| node.children[b].size.cmp(&node.children[a].size));

    for i in order {
        let child = &node.children[i];
        path.push(i);

        if child.is_folder() && !child.children.is_empty() {
            let id = ui.make_persistent_id(("tree_row", path.clone()));
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                .show_header(ui, |ui| {
                    row_contents(ui, child, path, selected, action);
                })
                .body(|ui| {
                    draw_children(ui, child, path, selected, action);
                });
        } else {
            ui.horizontal(|ui| {
                ui.add_space(18.0); // 没有展开箭头的行，用空格对齐到同一列
                row_contents(ui, child, path, selected, action);
            });
        }

        path.pop();
    }
}

fn row_contents(ui: &mut egui::Ui, node: &Node, path: &NodePath, selected: &Option<NodePath>, action: &mut TreeAction) {
    ui.horizontal(|ui| {
        let is_selected = selected.as_deref() == Some(path.as_slice());
        let icon = if node.is_folder() { "📁" } else { "📄" };
        let resp = ui.selectable_label(is_selected, RichText::new(format!("{icon} {}", node.name)).color(Color32::from_rgb(0xF0, 0xF0, 0xF0)).size(13.0));
        if resp.double_clicked() {
            *action = TreeAction::EnterNode(path.clone());
        } else if resp.clicked() {
            *action = TreeAction::Select(path.clone());
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(human_size(node.size)).size(12.0).color(Color32::from_rgb(0xC0, 0xC0, 0xC0)));
        });
    });
}
