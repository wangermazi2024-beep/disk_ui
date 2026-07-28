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
use crate::treemap::{compute_treemap, LAYOUT_PAD};

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
    parent_base_path: Option<&[usize]>,
) -> TreeAction {
    let mut action = TreeAction::None;

    // 1) 父层背景：如果有父层，先画父层颜色铺满整个 rect，
    //    然后在父层中为当前层找一个合适的占据位置（铺满父层去掉标签后的区域）。
    //    这样实现"双击后保留上层，当前层作为内嵌展开"的效果。
    let children_rect = if let (Some(parent), Some(parent_base)) = (parent_node, parent_base_path) {
        // 画父层背景
        {
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, CornerRadius::same(2), parent.color);
        }

        // 父层标签（左上角）
        let name_font = FontId::proportional(11.0);
        let text_max = (rect.width() - 28.0).max(20.0);
        let shown = truncate_text(ui.ctx(), &parent.name, name_font.clone(), text_max);
        let label = format!("⬆ {}", shown);
        let painter = ui.painter_at(rect);
        painter.text(
            rect.left_top() + Vec2::new(6.0, 3.0),
            egui::Align2::LEFT_TOP,
            &label,
            name_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 235),
        );

        // 点击父层背景 = 返回上一层
        let up_id = ui.id().with("go_up");
        let up_resp = ui.interact(rect, up_id, egui::Sense::click());
        if up_resp.clicked() && !up_resp.double_clicked() {
            // 只有点击父层空白区（不被子块消耗）才触发
            if matches!(action, TreeAction::None) {
                action = TreeAction::ZoomTo(parent_base.to_vec());
            }
        }
        if ui.rect_contains_pointer(rect) {
            show_up_tooltip(ui, up_id, parent);
        }

        // 当前层区域 = 父层 rect 去掉顶部标签高度，并留内边距
        let pad = LAYOUT_PAD;
        Rect::from_min_max(
            Pos2::new(rect.min.x + pad, rect.min.y + NEST_TOP + pad),
            Pos2::new(rect.max.x - pad, rect.max.y - pad),
        )
    } else {
        rect
    };

    // 2) 画当前层背景色
    {
        let painter = ui.painter_at(children_rect);
        painter.rect_filled(children_rect, CornerRadius::same(2), view_root.color);
    }

    // 3) 当前层子色块
    let mut path = base_path.to_vec();
    draw_children(ui, children_rect, view_root, &mut path, 0, selected, &mut action);
    action
}

fn show_up_tooltip(ui: &egui::Ui, id: egui::Id, parent: &Node) {
    let mouse = ui.ctx().pointer_latest_pos().unwrap_or_default();
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
                    ui.label(RichText::new(format!("⬆ 返回 {}", parent.name)).color(Color32::WHITE));
                    ui.label(
                        RichText::new("单击父层区域 = 返回上一层")
                            .size(10.5)
                            .color(Color32::from_rgb(0xA0, 0xA0, 0xA0)),
                    );
                });
        });
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
        if is_file {
            painter.rect_stroke(*r, CornerRadius::same(2), Stroke::new(1.0, FILE_BORDER), StrokeKind::Inside);
        }
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
