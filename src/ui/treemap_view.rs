//! Treemap 色块视图（SpaceSniffer 风格）
//!
//! - **单击文件夹**：在当前块内展开子色块（inline 嵌套）。
//! - **单击已展开的文件夹**：收起。
//! - **单击文件**：选中。
//! - **双击文件夹**：ZoomTo，把它变成新的"当前层"。
//! - **双击"上一层"色块**：等价于双击任意普通色块——ZoomTo 到它自己的路径，
//!   于是它变成新的"当前层"，它的父节点又变成新的"上一层"，一路双击就能
//!   逐级返回到根目录。这里没有任何特殊分支：上一层色块和其它色块
//!   走的是完全同一份渲染 + 交互代码（`draw_block`）。
//!
//! 三层可见的实现方式：双击进入某个节点后，实际画的是"它的父节点"这一个块
//! （占满整个 rect，就像它是唯一的兄弟节点一样），并且强制把父节点、
//! 目标节点这两级标记为"展开"，于是父节点（上一层）、目标节点（当前层）、
//! 目标节点的子节点（子层）就会像正常的 inline 展开一样层层嵌套画出来。
//! "强制展开"只影响这一帧的渲染，不修改节点真实的 `expanded` 字段，
//! 也不会连带强制展开更深的层级（子层内部该收着还是收着）。

use egui::{Color32, CornerRadius, FontId, Pos2, Rect, RichText, Stroke, StrokeKind, Vec2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

const MAX_DEPTH: u32 = 8;
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
    zoom_path: &NodePath,
    selected: &Option<NodePath>,
) -> TreeAction {
    let mut action = TreeAction::None;

    if zoom_path.is_empty() {
        // 没有上一层：就是根目录本身，直接画它的子节点。
        let mut path: NodePath = Vec::new();
        draw_children(ui, rect, &root.children, &mut path, 0, selected, &mut action, &[]);
    } else {
        // 有上一层：把"目标节点的父节点"当成唯一的一个色块画满整个 rect，
        // 强制展开父节点和目标节点这两级，天然长出"上一层/当前层/子层"。
        let parent_path = zoom_path[..zoom_path.len() - 1].to_vec();
        let parent = root.navigate(&parent_path).unwrap_or(root);
        let target_index = zoom_path[zoom_path.len() - 1];
        let mut path = parent_path;
        draw_block(ui, rect, parent, &mut path, 0, selected, &mut action, true, &[target_index]);
    }

    action
}

/// 画一组同级节点（正常 squarified 布局），每个子块各自调用 `draw_block`。
/// `force_open_index`：如果某个子节点的下标等于这个值，就强制展开它
/// （用来撑出双击导航所需要的"上一层→当前层"这一级嵌套）。
#[allow(clippy::too_many_arguments)]
fn draw_children(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    children: &[Node],
    path: &mut NodePath,
    depth: u32,
    selected: &Option<NodePath>,
    action: &mut TreeAction,
    force_open_index: &[usize],
) {
    if children.is_empty() || rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    let sizes: Vec<u64> = children.iter().map(|c| c.size).collect();
    let rects = compute_treemap(&sizes, rect);

    for (i, (r, child)) in rects.iter().zip(children.iter()).enumerate() {
        if r.width() < MIN_RENDER_W || r.height() < MIN_RENDER_H {
            continue;
        }
        path.push(i);
        let force_open = force_open_index.first() == Some(&i);
        draw_block(ui, *r, child, path, depth, selected, action, force_open, &[]);
        path.pop();
    }
}

/// 画单独一个色块：填色、边框、标签、tooltip、单击/双击交互，
/// 以及（如果需要）递归嵌套画它自己的子节点。
/// 这是整个视图唯一的"画一个块"的地方——普通色块、"上一层"色块，
/// 用的都是这一份代码，因此交互行为天然保持一致，不需要分别处理。
#[allow(clippy::too_many_arguments)]
fn draw_block(
    ui: &mut egui::Ui,
    r: egui::Rect,
    child: &Node,
    path: &mut NodePath,
    depth: u32,
    selected: &Option<NodePath>,
    action: &mut TreeAction,
    force_open: bool,
    force_open_child: &[usize],
) {
    let is_file = child.children.is_empty();
    let block_color = if is_file { FILE_COLOR } else { child.color };
    let is_selected = selected.as_deref() == Some(path.as_slice());

    let painter = ui.painter_at(r);
    painter.rect_filled(r, CornerRadius::same(2), block_color);
    let border_color = if is_file { FILE_BORDER } else { Color32::from_rgba_unmultiplied(0, 0, 0, 40) };
    painter.rect_stroke(r, CornerRadius::same(2), Stroke::new(1.0, border_color), StrokeKind::Inside);
    if is_selected {
        painter.rect_stroke(r, CornerRadius::same(2), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
    }

    let should_expand = child.expanded || force_open;
    let can_inline_expand = !is_file
        && should_expand
        && depth + 1 < MAX_DEPTH
        && r.width() > MIN_EXPAND_W
        && r.height() > MIN_EXPAND_H;

    draw_label(ui, &painter, r, child);

    let id = ui.id().with(("block", path.clone()));
    let resp = ui.interact(r, id, egui::Sense::click());
    let was_clicked = resp.clicked();
    let was_dbl = resp.double_clicked();

    if ui.rect_contains_pointer(r) {
        show_tooltip(ui, id, child, !can_inline_expand && should_expand && !is_file);
    }

    if can_inline_expand {
        let nested = Rect::from_min_max(Pos2::new(r.min.x, r.min.y + NEST_TOP), r.max);
        if nested.width() > 4.0 && nested.height() > 4.0 {
            draw_children(ui, nested, &child.children, path, depth + 1, selected, action, force_open_child);
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
            *action = TreeAction::ToggleExpand(path.clone());
        }
    }
}

fn draw_label(ui: &egui::Ui, painter: &egui::Painter, r: egui::Rect, node: &Node) {
    let pad = 3.0;
    let text_max_w = (r.width() - pad * 2.0).max(0.0);
    if r.width() <= 14.0 || text_max_w <= 4.0 {
        return;
    }
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
