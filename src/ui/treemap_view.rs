//! Treemap 色块视图（SpaceSniffer 风格交互）
//!
//! - **单击**文件夹：在当前视图里展开/折叠其子色块（递归嵌套）。
//! - **单击**文件：选中它。
//! - **双击**文件夹：放大到该层（铺满整个 treemap 区域）。
//! - **双击**文件：选中它（和单击一样）。
//!
//! 文件色块使用固定颜色（灰色系），文件夹使用各自的 type 颜色，
//! 一眼就能区分哪些是可继续点击的文件夹、哪些是终点文件。

use egui::{Color32, FontId, Rounding, RichText, Stroke, Vec2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

/// 色块之间的间距
const BLOCK_PAD: f32 = 2.5;
/// 嵌套展开的最大层数（防止递归过深拖垮帧率）
const MAX_DEPTH: u32 = 5;
/// 嵌套内缩进（px）
const NEST_PAD: f32 = 4.0;

/// 文件色块固定使用的灰色，方便一眼识别
const FILE_COLOR: Color32 = Color32::from_rgb(0x5A, 0x6B, 0x7C);

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

        // 文件用固定灰色，文件夹用各自颜色
        let block_color = if !child.is_folder() { FILE_COLOR } else { child.color };

        let is_selected = selected.as_deref() == Some(path.as_slice());
        let painter = ui.painter_at(inset);
        painter.rect_filled(inset, Rounding::same(4.0), block_color);
        if is_selected {
            painter.rect_stroke(inset, Rounding::same(4.0), Stroke::new(2.0_f32, Color32::WHITE));
        }

        draw_label(ui, &painter, inset, child);

        let id = ui.id().with(("block", path.clone()));
        let resp = ui.interact(inset, id, egui::Sense::click());

        // 气泡：定位在鼠标上方，避免遮住点击目标
        if ui.rect_contains_pointer(inset) {
            show_tooltip(ui, id, inset, child);
        }

        // 单击/双击统一处理：先判断 clicked，在内部分 double_clicked
        if resp.clicked() {
            if resp.double_clicked() {
                if !child.children.is_empty() {
                    *action = TreeAction::ZoomTo(path.clone());
                }
            } else if child.children.is_empty() {
                *action = TreeAction::Select(path.clone());
            } else {
                *action = TreeAction::ToggleExpand(path.clone());
            }
        }

        // 已展开的文件夹 → 递归画嵌套子色块
        if child.expanded && !child.children.is_empty() && depth + 1 < MAX_DEPTH {
            let nested = inset.shrink(NEST_PAD);
            if nested.width() > 8.0 && nested.height() > 8.0 {
                draw_children(ui, nested, child, path, depth + 1, selected, action);
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

/// 气泡：定位在鼠标正上方，避免遮住点击目标。
/// 用 `egui::Order::Tooltip` 确保在最上层。
fn show_tooltip(ui: &egui::Ui, id: egui::Id, inset: egui::Rect, node: &Node) {
    let tip_pos = ui.ctx().pointer_latest_pos().unwrap_or(inset.center());
    // 向上偏移 24px 让气泡在鼠标上方，不遮挡点击
    let offset = Vec2::new(0.0, -24.0);

    let hint = if node.children.is_empty() {
        "文件 · 单击选中"
    } else if node.expanded {
        "文件夹 · 单击收起 · 双击进入"
    } else {
        "文件夹 · 单击展开 · 双击进入"
    };

    egui::Area::new(id.with("tooltip"))
        .fixed_pos(tip_pos + offset)
        .anchor(egui::Align2::LEFT_BOTTOM, Vec2::new(8.0, 0.0))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::default()
                .fill(Color32::from_rgb(0x33, 0x33, 0x38))
                .rounding(4.0)
                .inner_margin(egui::Margin::same(6.0))
                .show(ui, |ui| {
                    ui.label(RichText::new(
                        format!("{} · {}", node.name, human_size(node.size))
                    ).color(Color32::WHITE));
                    ui.label(RichText::new(hint).size(10.5).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                });
        });
}
