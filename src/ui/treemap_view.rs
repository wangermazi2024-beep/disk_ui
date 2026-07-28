//! Treemap 色块视图（SpaceSniffer 风格）
//!
//! - **单击文件夹**：在当前块内展开子色块（inline 嵌套）。
//! - **单击已展开的文件夹**：收起。
//! - **单击文件**：选中。
//! - **双击文件夹**：ZoomTo，画面保留父层背景，当前层作为父层内展开的一个大子块。
//!
//! 布局间距由 treemap::LAYOUT_PAD 在算法层统一处理，渲染时不再二次 shrink。

use egui::{Color32, CornerRadius, FontId, Pos2, Rect, RichText, Stroke, StrokeKind, Vec2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

const MAX_DEPTH: u32 = 6;
/// 子块小于此尺寸直接不渲染
const MIN_RENDER_W: f32 = 6.0;
const MIN_RENDER_H: f32 = 6.0;
/// 子块小于此尺寸不允许 inline 嵌套展开
const MIN_EXPAND_W: f32 = 36.0;
const MIN_EXPAND_H: f32 = 28.0;
/// 展开子块时顶部预留的标签高度
const NEST_TOP: f32 = 14.0;

const FILE_COLOR: Color32 = Color32::from_rgb(0x5A, 0x6B, 0x7C);
const FILE_BORDER: Color32 = Color32::from_rgb(0x6A, 0x7B, 0x8C);

pub fn show(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    view_root: &Node,
    base_path: &[usize],
    selected: &Option<NodePath>,
    parent_node: Option<&Node>,
) -> TreeAction {
    let mut action = TreeAction::None;

    // 双击放大后，上层作为整个 treemap 的背景填充，当前层子色块绘制在内部。
    // 就像 inline 展开一样，只是父层变成了整个画面。
    // 返回上层由面包屑处理，treemap 本身不渲染返回按钮。
    let draw_rect = if let Some(parent) = parent_node {
        // 父层背景 + 名称（像普通色块一样在左上角画标签）
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(2), parent.color);
        let shown = truncate_text(ui.ctx(), &parent.name, FontId::proportional(10.0), (rect.width() - 12.0).max(20.0));
        painter.text(
            rect.left_top() + Vec2::new(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            &shown,
            FontId::proportional(10.0),
            Color32::from_rgba_unmultiplied(255, 255, 255, 200),
        );
        // 子层区域：父层内部去掉顶部标签空间
        Rect::from_min_max(
            Pos2::new(rect.min.x, rect.min.y + NEST_TOP + 2.0),
            rect.max,
        )
    } else {
        rect
    };

    let mut path = base_path.to_vec();
    draw_children(ui, draw_rect, view_root, &mut path, 0, selected, &mut action);
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
    if node.children.is_empty() { return; }
    if rect.width() < 2.0 || rect.height() < 2.0 { return; }

    let sizes: Vec<u64> = node.children.iter().map(|c| c.size).collect();
    // compute_treemap 已包含 LAYOUT_PAD 间距，直接用，不再 shrink
    let rects = compute_treemap(&sizes, rect);

    for (i, (r, child)) in rects.iter().zip(node.children.iter()).enumerate() {
        // 太小不渲染
        if r.width() < MIN_RENDER_W || r.height() < MIN_RENDER_H {
            continue;
        }

        path.push(i);

        let is_file = child.children.is_empty();
        let block_color = if is_file { FILE_COLOR } else { child.color };
        let is_selected = selected.as_deref() == Some(path.as_slice());

        let painter = ui.painter_at(*r);
        painter.rect_filled(*r, CornerRadius::same(2), block_color);
        // 每个色块都画边框线，无间隙时靠边框区分相邻块
        let border_color = if is_file { FILE_BORDER } else { Color32::from_rgba_unmultiplied(0, 0, 0, 40) };
        painter.rect_stroke(*r, CornerRadius::same(2), Stroke::new(1.0, border_color), StrokeKind::Inside);
        if is_selected {
            painter.rect_stroke(*r, CornerRadius::same(2), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
        }

        let can_inline_expand = !is_file
            && child.expanded
            && depth + 1 < MAX_DEPTH
            && r.width() > MIN_EXPAND_W
            && r.height() > MIN_EXPAND_H;

        draw_label(ui, &painter, *r, child);

        let id = ui.id().with(("block", path.clone()));
        let resp = ui.interact(*r, id, egui::Sense::click());
        let was_clicked = resp.clicked();
        let was_dbl = resp.double_clicked();

        if ui.rect_contains_pointer(*r) {
            show_tooltip(ui, id, child, !can_inline_expand && child.expanded && !is_file);
        }

        if can_inline_expand {
            let nested = Rect::from_min_max(
                Pos2::new(r.min.x, r.min.y + NEST_TOP),
                r.max,
            );
            if nested.width() > 4.0 && nested.height() > 4.0 {
                draw_children(ui, nested, child, path, depth + 1, selected, action);
            }
        }

        if was_clicked && matches!(*action, TreeAction::None) {
            if was_dbl {
                if !child.children.is_empty() {
                    *action = TreeAction::ZoomTo(path.clone());
                }
            } else if is_file {
                *action = TreeAction::Select(path.clone());
            } else if r.width() < MIN_EXPAND_W || r.height() < MIN_EXPAND_H {
                *action = TreeAction::Select(path.clone());
            } else {
                // 文件夹单击：先选中，下一帧再展开（避免双击第一帧触发展开）
                *action = TreeAction::PrepareToggle(path.clone());
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
