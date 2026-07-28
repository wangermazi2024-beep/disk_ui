//! Treemap 色块视图（SpaceSniffer 风格）
//!
//! - **单击文件夹**：在当前块内展开子色块（inline 嵌套）。
//! - **单击已展开的文件夹**：收起。
//! - **单击文件**：选中（文件用固定灰色，一眼可识别）。
//! - **双击文件夹**：放大到该层（ZoomTo），铺满整个 treemap 区域。
//!
//! 嵌套点击的核心修复：**先递归子块（消耗点击事件），再处理本级**。
//! 这样点击子块时父块不会被错误地触发展开/收起。

use egui::{Color32, CornerRadius, FontId, Pos2, RichText, Stroke, StrokeKind, Vec2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

/// 色块间距
const BLOCK_PAD: f32 = 2.0;
/// 嵌套内缩（px）— 调大一些让嵌套层次明显可见
const NEST_PAD: f32 = 5.0;
/// 最大嵌套层数
const MAX_DEPTH: u32 = 6;
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
    draw_children(ui, rect, view_root, &mut path, 0, selected, &mut action);
    action
}

#[allow(clippy::too_many_arguments)]
fn draw_children(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    node: &Node,
    path: &mut NodePath,
    depth: u32,
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

        // ── 绘制色块 ──
        let painter = ui.painter_at(inset);
        painter.rect_filled(inset, CornerRadius::same(4), block_color);
        if is_file {
            painter.rect_stroke(inset, CornerRadius::same(4), Stroke::new(1.0, FILE_BORDER), StrokeKind::Inside);
        }
        if is_selected {
            painter.rect_stroke(inset, CornerRadius::same(4), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
        }
        draw_label(ui, &painter, inset, child);

        // ── 注册交互 ──
        let id = ui.id().with(("block", path.clone()));
        let resp = ui.interact(inset, id, egui::Sense::click());
        let was_clicked = resp.clicked();
        let was_dbl = resp.double_clicked();

        // 气泡
        if ui.rect_contains_pointer(inset) {
            show_tooltip(ui, id, child);
        }

        // ── 关键：先递归子块（嵌套色块），再处理本级点击 ──
        if child.expanded && !child.children.is_empty() && depth + 1 < MAX_DEPTH {
            let nested = inset.shrink(NEST_PAD);
            if nested.width() > 8.0 && nested.height() > 8.0 {
                draw_children(ui, nested, child, path, depth + 1, selected, action);
            }
        }

        // 只有子块没有消耗点击事件时，本级才处理
        if was_clicked && matches!(*action, TreeAction::None) {
            if was_dbl {
                if !child.children.is_empty() {
                    *action = TreeAction::ZoomTo(path.clone());
                }
            } else if child.children.is_empty() {
                *action = TreeAction::Select(path.clone());
            } else {
                *action = TreeAction::ToggleExpand(path.clone());
            }
        }

        path.pop();
    }
}

fn draw_label(ui: &egui::Ui, painter: &egui::Painter, inset: egui::Rect, node: &Node) {
    let pad = 5.0;
    let text_max_w = inset.width() - pad * 2.0;
    if inset.width() <= 22.0 || inset.height() <= 18.0 || text_max_w <= 8.0 {
        return;
    }
    let name_font = FontId::proportional(11.5);
    let shown_name = truncate_text(ui.ctx(), &node.name, name_font.clone(), text_max_w);
    if !shown_name.is_empty() {
        painter.text(
            inset.left_top() + Vec2::new(pad, 3.0),
            egui::Align2::LEFT_TOP,
            &shown_name,
            name_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 235),
        );
    }
    if inset.height() > 34.0 {
        let size_font = FontId::proportional(10.0);
        let sz = truncate_text(ui.ctx(), &human_size(node.size), size_font.clone(), text_max_w);
        painter.text(
            inset.left_bottom() + Vec2::new(pad, -3.0),
            egui::Align2::LEFT_BOTTOM,
            &sz,
            size_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 200),
        );
    }
}

fn show_tooltip(ui: &egui::Ui, id: egui::Id, node: &Node) {
    let mouse = ui.ctx().pointer_latest_pos().unwrap_or_default();
    let hint = if node.children.is_empty() {
        "文件 · 单击选中"
    } else {
        "文件夹 · 单击展开/收起 · 双击进入"
    };

    egui::Area::new(id.with("tip"))
        .fixed_pos(mouse + Vec2::new(14.0, -18.0))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::default()
                .fill(Color32::from_rgb(0x2A, 0x2C, 0x32))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0x55, 0x55, 0x60)))
                .corner_radius(CornerRadius::same(5))
                .inner_margin(egui::Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("{} · {}", node.name, human_size(node.size))).color(Color32::WHITE));
                    ui.label(RichText::new(hint).size(10.5).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                });
        });
}
