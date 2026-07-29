//! 递归展开的文件列表树 + 可拖拽表头。
//!
//! - 表头固定不滚动，列宽可拖拽调整。
//! - 文件夹前面带箭头，点箭头/文字展开下一层子节点。
//! - 单击一行：选中该节点。
//! - 双击一行：进入该节点（父节点提升为新视图根）。

use egui::{Color32, Pos2, Rect, RichText, Vec2};

use crate::format::human_size;
use crate::model::{Node, NodePath};

use super::TreeAction;

const TAG_NAME: &str = "col_name";
const SEP_HIT: f32 = 6.0;
const HEADER_H: f32 = 22.0;

/// 画可拖拽表头（固定位置，不随滚动条移动）。
/// 返回 `TreeAction::None`（表头本身不产生交互动作）。
pub fn show_header(ui: &mut egui::Ui) -> TreeAction {
    let id = ui.id();
    let mut name_w = ui.data(|d| d.get_temp::<f32>(id.with(TAG_NAME)).unwrap_or(260.0));
    let avail_w = ui.available_width().max(200.0);
    let min_name = 80.0;
    let min_size = 60.0;
    name_w = name_w.clamp(min_name, (avail_w - min_size).max(min_name));

    let header_bg = Color32::from_rgb(0x2A, 0x2C, 0x30);
    let (hr, _) = ui.allocate_exact_size(Vec2::new(avail_w, HEADER_H), egui::Sense::hover());
    let p = ui.painter_at(hr);
    p.rect_filled(hr, egui::CornerRadius::same(3), header_bg);

    // 名称列
    let name_rect = Rect::from_min_size(hr.min, Vec2::new(name_w, HEADER_H));
    p.text(
        name_rect.left_center() + Vec2::new(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "名称",
        egui::FontId::proportional(12.0),
        Color32::from_rgb(0xF0, 0xF0, 0xF0),
    );

    // 大小列
    let sep_x = hr.min.x + name_w;
    p.text(
        Rect::from_min_size(Pos2::new(sep_x + 6.0, hr.min.y), Vec2::new(avail_w - name_w - 6.0, HEADER_H))
            .right_center(),
        egui::Align2::RIGHT_CENTER,
        "大小",
        egui::FontId::proportional(12.0),
        Color32::from_rgb(0xF0, 0xF0, 0xF0),
    );

    // 分隔条拖拽
    let sep_rect = Rect::from_min_size(Pos2::new(sep_x - SEP_HIT / 2.0, hr.min.y), Vec2::new(SEP_HIT, HEADER_H));
    let sep_resp = ui.interact(sep_rect, id.with("col_drag"), egui::Sense::drag());
    if sep_resp.hovered() || sep_resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
    }
    let sep_color = if sep_resp.dragged() { Color32::WHITE } else { Color32::from_rgb(0x55, 0x55, 0x60) };
    p.rect_filled(Rect::from_min_size(Pos2::new(sep_x - 0.5, hr.min.y), Vec2::new(1.0, HEADER_H)), egui::CornerRadius::ZERO, sep_color);

    let drag = sep_resp.drag_delta();
    if drag.x != 0.0 {
        name_w = (name_w + drag.x).clamp(min_name, avail_w - min_size);
        ui.data_mut(|d| d.insert_temp::<f32>(id.with(TAG_NAME), name_w));
    }

    TreeAction::None
}

/// 读取当前表头的列宽（供渲染树行时使用）。
pub fn get_name_width(ui: &egui::Ui) -> f32 {
    let id = ui.id();
    ui.data(|d| d.get_temp::<f32>(id.with(TAG_NAME)).unwrap_or(260.0))
}

/// 画递归文件列表树（放在可滚动的区域内部）。
pub fn show_body(ui: &mut egui::Ui, view_root: &Node, base_path: &[usize], selected: &Option<NodePath>) -> TreeAction {
    let mut action = TreeAction::None;
    let name_w = get_name_width(ui);
    let mut path = base_path.to_vec();
    draw_children(ui, view_root, &mut path, selected, &mut action, name_w);
    action
}

fn draw_children(
    ui: &mut egui::Ui,
    node: &Node,
    path: &mut NodePath,
    selected: &Option<NodePath>,
    action: &mut TreeAction,
    name_w: f32,
) {
    let mut order: Vec<usize> = (0..node.children.len()).collect();
    order.sort_by(|&a, &b| node.children[b].size.cmp(&node.children[a].size));

    for i in order {
        let child = &node.children[i];
        path.push(i);

        if child.is_folder() && !child.children.is_empty() {
            let coll_id = ui.make_persistent_id(("tree_row", path.clone()));
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), coll_id, false)
                .show_header(ui, |ui| {
                    ui.style_mut().visuals.widgets.inactive.fg_stroke.color = Color32::from_rgb(0xF0, 0xF0, 0xF0);
                    row_contents(ui, child, path, selected, action, name_w);
                })
                .body(|ui| {
                    draw_children(ui, child, path, selected, action, name_w);
                });
        } else {
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                row_contents(ui, child, path, selected, action, name_w);
            });
        }

        path.pop();
    }
}

fn row_contents(
    ui: &mut egui::Ui,
    node: &Node,
    path: &NodePath,
    selected: &Option<NodePath>,
    action: &mut TreeAction,
    name_w: f32,
) {
    ui.horizontal(|ui| {
        let is_selected = selected.as_deref() == Some(path.as_slice());
        // 选中行显示高亮背景
        if is_selected {
            let row_rect = ui.max_rect();
            ui.painter().rect_filled(row_rect, egui::CornerRadius::ZERO, Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 15));
        }

        let icon = if node.is_folder() { "📁" } else { "📄" };

        // 名称列
        let label = RichText::new(format!("{icon} {}", node.name))
            .color(Color32::from_rgb(0xF0, 0xF0, 0xF0))
            .size(13.0);
        let resp = ui.add_sized(
            egui::vec2(name_w - 4.0, ui.available_height()),
            egui::Label::new(label).sense(egui::Sense::click()),
        );
        if resp.double_clicked() {
            *action = TreeAction::EnterNode(path.clone());
        } else if resp.clicked() {
            *action = TreeAction::Select(path.clone());
        }

        // 大小列（右对齐）
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(human_size(node.size)).size(12.0).color(Color32::from_rgb(0xC0, 0xC0, 0xC0)));
        });
    });
}
