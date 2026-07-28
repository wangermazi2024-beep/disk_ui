//! Treemap 色块视图（SpaceSniffer 风格交互）
//!
//! - **单击**：选中色块（高亮边框）。
//! - **双击文件夹**：放大到该层（zoom），铺满整个 treemap 区域。
//! - **双击文件**：无效果（文件没有子节点）。
//!
//! 展开/折叠由右侧文件列表树的 ▶/▼ 完成，treemap 本身不做 inline 展开，
//! 避免嵌套点击路径混乱的问题。

use egui::{Color32, CornerRadius, FontId, RichText, Stroke, StrokeKind, Vec2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

/// 色块间距（px）
const BLOCK_PAD: f32 = 2.0;
/// 文件色块固定灰色
const FILE_COLOR: Color32 = Color32::from_rgb(0x5A, 0x6B, 0x7C);
const FILE_BORDER: Color32 = Color32::from_rgb(0x6A, 0x7B, 0x8C);

pub fn show(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    view_root: &Node,
    base_path: &[usize],
    selected: &Option<NodePath>,
) -> TreeAction {
    let mut action = TreeAction::None;
    let mut path = base_path.to_vec();
    draw_children(ui, rect, view_root, &mut path, selected, &mut action);
    action
}

fn draw_children(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    node: &Node,
    path: &mut NodePath,
    selected: &Option<NodePath>,
    action: &mut TreeAction,
) {
    if node.children.is_empty() {
        return;
    }
    let sizes: Vec<u64> = node.children.iter().map(|c| c.size).collect();
    let rects = compute_treemap(&sizes, rect);

    for (i, (r, child)) in rects.iter().zip(node.children.iter()).enumerate() {
        let inset = r.shrink(BLOCK_PAD);
        if inset.width() < 2.0 || inset.height() < 2.0 {
            continue;
        }
        path.push(i);

        let is_file = child.children.is_empty();
        let block_color = if is_file { FILE_COLOR } else { child.color };
        let is_selected = selected.as_deref() == Some(path.as_slice());

        let painter = ui.painter_at(inset);
        painter.rect_filled(inset, CornerRadius::same(4), block_color);
        if is_file {
            painter.rect_stroke(inset, CornerRadius::same(4), Stroke::new(1.0, FILE_BORDER), StrokeKind::Inside);
        }
        if is_selected {
            painter.rect_stroke(inset, CornerRadius::same(4), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
        }

        draw_label(ui, &painter, inset, child);

        let id = ui.id().with(("block", path.clone()));
        let resp = ui.interact(inset, id, egui::Sense::click());

        if ui.rect_contains_pointer(inset) {
            show_tooltip(ui, id, child);
        }

        // 单击/双击分离
        if resp.clicked() {
            if resp.double_clicked() {
                if !child.children.is_empty() {
                    *action = TreeAction::ZoomTo(path.clone());
                }
            } else if is_file {
                *action = TreeAction::Select(path.clone());
            } else {
                *action = TreeAction::Select(path.clone());
            }
        }

        path.pop();
    }
}

fn draw_label(ui: &egui::Ui, painter: &egui::Painter, inset: egui::Rect, node: &Node) {
    let pad = 6.0;
    let text_max_w = inset.width() - pad * 2.0;
    if inset.width() <= 22.0 || inset.height() <= 18.0 || text_max_w <= 8.0 {
        return;
    }
    let name_font = FontId::proportional(12.0);
    let shown_name = truncate_text(ui.ctx(), &node.name, name_font.clone(), text_max_w);
    if !shown_name.is_empty() {
        painter.text(
            inset.left_top() + Vec2::new(pad, 4.0),
            egui::Align2::LEFT_TOP,
            &shown_name,
            name_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 235),
        );
    }
    if inset.height() > 36.0 {
        let size_font = FontId::proportional(10.5);
        let size_text = human_size(node.size);
        let shown_size = truncate_text(ui.ctx(), &size_text, size_font.clone(), text_max_w);
        painter.text(
            inset.left_bottom() + Vec2::new(pad, -4.0),
            egui::Align2::LEFT_BOTTOM,
            &shown_size,
            size_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 200),
        );
    }
}

/// 气泡固定在鼠标右上方，避开光标位置。
fn show_tooltip(ui: &egui::Ui, id: egui::Id, node: &Node) {
    let ctx = ui.ctx();
    let mouse = ctx.pointer_latest_pos().unwrap_or_default();
    let hint = if node.children.is_empty() {
        "文件 · 单击选中"
    } else {
        "文件夹 · 单击选中 · 双击进入"
    };

    egui::Area::new(id.with("tip"))
        .fixed_pos(mouse + Vec2::new(14.0, -18.0))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::default()
                .fill(Color32::from_rgb(0x2A, 0x2C, 0x32))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0x55, 0x55, 0x60)))
                .corner_radius(CornerRadius::same(5))
                .inner_margin(egui::Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!("{} · {}", node.name, human_size(node.size)))
                            .color(Color32::WHITE),
                    );
                    ui.label(RichText::new(hint).size(10.5).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                });
        });
}
