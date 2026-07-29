//! Treemap 色块视图（SpaceSniffer 风格）
//!
//! - **单击文件夹**：在当前块内展开子色块（inline 嵌套）。
//! - **单击已展开的文件夹**：收起。
//! - **单击文件**：选中。
//! - **双击文件夹**：ZoomTo，把 `zoom_path` 设到该文件夹。
//!   下一帧渲染时，该文件夹的父节点作为唯一色块占满画面并强制展开，
//!   于是自然长出"上一层 → 当前层 → 子层"三层，且上层色块本身可交互。
//!
//! 没有为"上一层"单独写背景绘制——它走 `draw_block`，和普通色块同一份代码。
//! 双击"上一层"色块 = 双击普通文件夹，ZoomTo 到它自己的路径，一路返回根。

use egui::{Color32, CornerRadius, FontId, Pos2, Rect, RichText, Stroke, StrokeKind, Vec2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

const MAX_DEPTH: u32 = 6;
const MIN_RENDER_W: f32 = 6.0;
const MIN_RENDER_H: f32 = 6.0;
const MIN_EXPAND_W: f32 = 36.0;
const MIN_EXPAND_H: f32 = 28.0;
const NEST_TOP: f32 = 14.0;

const FILE_COLOR: Color32 = Color32::from_rgb(0x5A, 0x6B, 0x7C);
const FILE_BORDER: Color32 = Color32::from_rgb(0x6A, 0x7B, 0x8C);

pub fn show(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    root: &Node,
    zoom_path: &[usize],
    selected: &Option<NodePath>,
) -> TreeAction {
    let mut action = TreeAction::None;

    if zoom_path.is_empty() {
        // 顶层视图：直接画 root 的孩子
        let mut path = Vec::new();
        draw_children(ui, rect, root, &mut path, 0, selected, &mut action, None);
        return action;
    }

    // 有 zoom_path：把 zoom_path 的父节点画成一个占满全区的色块（强制展开），
    // 该色块内部显示它的所有孩子（含目标节点及其兄弟）。
    // 目标节点同样被强制展开——这样"上一层→当前层→子层"就都可见了。
    let Some((&target_idx, parent_path)) = zoom_path.split_last() else {
        let mut path = Vec::new();
        draw_children(ui, rect, root, &mut path, 0, selected, &mut action, None);
        return action;
    };
    let Some(parent) = root.navigate(parent_path) else {
        let mut path = Vec::new();
        draw_children(ui, rect, root, &mut path, 0, selected, &mut action, None);
        return action;
    };

    // 父节点占满全区
    let mut path = parent_path.to_vec();
    // 父节点背景 + 标签
    {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(2), parent.color);
        painter.rect_stroke(rect, CornerRadius::same(2), Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)), StrokeKind::Inside);
        let shown = truncate_text(ui.ctx(), &parent.name, FontId::proportional(10.0), (rect.width() - 12.0).max(20.0));
        painter.text(
            rect.left_top() + Vec2::new(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            &shown,
            FontId::proportional(10.0),
            Color32::from_rgba_unmultiplied(255, 255, 255, 200),
        );
    }

    // 子层区域（强制展开目标子项，以露出"当前层→子层"）
    let nested = Rect::from_min_max(
        Pos2::new(rect.min.x, rect.min.y + NEST_TOP + 2.0),
        rect.max,
    );
    if nested.width() > 4.0 && nested.height() > 4.0 {
        draw_children(ui, nested, parent, &mut path, 0, selected, &mut action, Some(target_idx));
    }

    // 父节点本身的交互（单击/双击），在子节点处理完之后再检查，
    // 如果子节点已消费事件则跳过。
    if matches!(action, TreeAction::None) {
        let id = ui.id().with(("block", path.clone()));
        let resp = ui.interact(rect, id, egui::Sense::click());
        if resp.clicked() {
            if resp.double_clicked() && !parent.children.is_empty() {
                action = TreeAction::ZoomTo(path);
            } else {
                action = TreeAction::ToggleExpand(path);
            }
        }
    }

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
    force_index: Option<usize>,
) {
    if node.children.is_empty() { return; }
    if rect.width() < 2.0 || rect.height() < 2.0 { return; }

    let sizes: Vec<u64> = node.children.iter().map(|c| c.size).collect();
    let rects = compute_treemap(&sizes, rect);

    for (i, (r, child)) in rects.iter().zip(node.children.iter()).enumerate() {
        if r.width() < MIN_RENDER_W || r.height() < MIN_RENDER_H {
            continue;
        }

        path.push(i);

        let is_file = child.children.is_empty();
        let block_color = if is_file { FILE_COLOR } else { child.color };
        let is_selected = selected.as_deref() == Some(path.as_slice());

        let painter = ui.painter_at(*r);
        painter.rect_filled(*r, CornerRadius::same(2), block_color);
        let border_color = if is_file { FILE_BORDER } else { Color32::from_rgba_unmultiplied(0, 0, 0, 40) };
        painter.rect_stroke(*r, CornerRadius::same(2), Stroke::new(1.0, border_color), StrokeKind::Inside);
        if is_selected {
            painter.rect_stroke(*r, CornerRadius::same(2), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
        }

        // 如果是被强制展开的子项（当前层），假装 expanded=true
        let is_forced = force_index == Some(i);
        let expanded = child.expanded || is_forced;
        let can_inline_expand = !is_file
            && expanded
            && depth + 1 < MAX_DEPTH
            && r.width() > MIN_EXPAND_W
            && r.height() > MIN_EXPAND_H;

        draw_label(ui, &painter, *r, child);

        let id = ui.id().with(("block", path.clone()));
        let resp = ui.interact(*r, id, egui::Sense::click());

        if ui.rect_contains_pointer(*r) {
            show_tooltip(ui, id, child, !can_inline_expand && expanded && !is_file);
        }

        if can_inline_expand {
            let nested = Rect::from_min_max(
                Pos2::new(r.min.x, r.min.y + NEST_TOP),
                r.max,
            );
            if nested.width() > 4.0 && nested.height() > 4.0 {
                // 强制展开只沿 zoom_path 走一层：递归下去不再传递 force_index
                draw_children(ui, nested, child, path, depth + 1, selected, action, None);
            }
        }

        // 单击/双击处理（double_clicked 优先）
        if resp.clicked() && matches!(*action, TreeAction::None) {
            if resp.double_clicked() {
                if !child.children.is_empty() {
                    *action = TreeAction::ZoomTo(path.clone());
                }
            } else if is_file {
                *action = TreeAction::Select(path.clone());
            } else if r.width() < MIN_EXPAND_W || r.height() < MIN_EXPAND_H {
                *action = TreeAction::Select(path.clone());
            } else {
                *action = TreeAction::ToggleExpand(path.clone());
            }
        }

        path.pop();
    }
}

fn draw_label(ui: &egui::Ui, painter: &egui::Painter, r: egui::Rect, node: &Node) {
    let pad = 3.0;
    let text_max_w = (r.width() - pad * 2.0).max(0.0);
    if r.width() <= 14.0 || text_max_w <= 4.0 { return; }
    let name_font = FontId::proportional(9.0);
    let shown = truncate_text(ui.ctx(), &node.name, name_font.clone(), text_max_w);
    if !shown.is_empty() && r.height() > 11.0 {
        painter.text(
            r.left_top() + Vec2::new(pad, 2.0),
            egui::Align2::LEFT_TOP,
            &shown,
            name_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 220),
        );
    }
    if r.height() > 22.0 {
        let size_font = FontId::proportional(8.0);
        let sz = truncate_text(ui.ctx(), &human_size(node.size), size_font.clone(), text_max_w);
        if !sz.is_empty() {
            painter.text(
                r.left_bottom() + Vec2::new(pad, -2.0),
                egui::Align2::LEFT_BOTTOM,
                &sz,
                size_font,
                Color32::from_rgba_unmultiplied(255, 255, 255, 180),
            );
        }
    }
}

fn show_tooltip(ui: &egui::Ui, id: egui::Id, node: &Node, too_small: bool) {
    let mouse = ui.ctx().pointer_latest_pos().unwrap_or_default();
    let hint = if node.children.is_empty() {
        "文件 · 单击选中"
    } else if too_small {
        "文件夹 · 块太小，请双击进入"
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
                    ui.label(
                        RichText::new(format!("{} · {}", node.name, human_size(node.size)))
                            .color(Color32::WHITE),
                    );
                    ui.label(
                        RichText::new(hint)
                            .size(10.5)
                            .color(Color32::from_rgb(0xA0, 0xA0, 0xA0)),
                    );
                });
        });
}
