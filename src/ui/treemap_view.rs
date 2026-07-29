//! Treemap 色块视图（SpaceSniffer 风格）
//!
//! - **单击文件夹**：在当前块内展开子色块（inline 嵌套）。
//! - **单击已展开的文件夹**：收起。
//! - **单击文件**：选中（文件用固定灰色，一眼可识别）。
//! - **双击文件夹**：放大到该层（ZoomTo），铺满整个 treemap 区域。
//!
//! 嵌套点击的核心修复：**先递归子块（消耗点击事件），再处理本级**。

use egui::{Color32, CornerRadius, FontId, Pos2, Rect, RichText, Stroke, StrokeKind, Vec2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

const BLOCK_PAD: f32 = 0.5;
/// 嵌套时只在顶部留空间显示文字（左右下不缩）
const NEST_TOP: f32 = 14.0;
const MAX_DEPTH: u32 = 6;
/// 子块小于此尺寸时不展开嵌套（提示双击进入）
const MIN_INLINE_SIZE: f32 = 30.0;
const FILE_COLOR: Color32 = Color32::from_rgb(0x5A, 0x6B, 0x7C);
const FILE_BORDER: Color32 = Color32::from_rgb(0x6A, 0x7B, 0x8C);
const UP_COLOR: Color32 = Color32::from_rgb(0x40, 0x42, 0x46);

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

    if let (Some(parent), Some(parent_base)) = (parent_node, parent_base_path) {
        let up_h = 22.0_f32;
        let up_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), up_h));
        let painter = ui.painter_at(up_rect);
        painter.rect_filled(up_rect, CornerRadius::same(3), UP_COLOR);
        painter.text(
            up_rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("⬆ 返回  {}", parent.name),
            FontId::proportional(11.0),
            Color32::from_rgb(0xBB, 0xBB, 0xCC),
        );
        let up_id = ui.id().with("go_up");
        let up_resp = ui.interact(up_rect, up_id, egui::Sense::click());
        if up_resp.clicked() && !up_resp.double_clicked() {
            action = TreeAction::ZoomTo(parent_base.to_vec());
        }
        // 剩余空间给子色块
        let remain = Rect::from_min_max(
            Pos2::new(rect.min.x, up_rect.max.y + 3.0),
            rect.max,
        );
        let mut path = base_path.to_vec();
        draw_children(ui, remain, view_root, &mut path, 0, selected, &mut action);
    } else {
        let mut path = base_path.to_vec();
        draw_children(ui, rect, view_root, &mut path, 0, selected, &mut action);
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
        painter.rect_filled(inset, CornerRadius::same(3), block_color);
        if is_file {
            painter.rect_stroke(inset, CornerRadius::same(3), Stroke::new(1.0, FILE_BORDER), StrokeKind::Inside);
        }
        if is_selected {
            painter.rect_stroke(inset, CornerRadius::same(3), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
        }
        draw_label(ui, &painter, inset, child, child.expanded);

        let id = ui.id().with(("block", path.clone()));
        let resp = ui.interact(inset, id, egui::Sense::click());
        let was_clicked = resp.clicked();
        let was_dbl = resp.double_clicked();

        if ui.rect_contains_pointer(inset) {
            show_tooltip(ui, id, child, false);
        }

        // 判断是否适合展开嵌套：有子节点、已展开、depth 未超限
        let can_nest = child.expanded
            && !child.children.is_empty()
            && depth + 1 < MAX_DEPTH
            && inset.width() > MIN_INLINE_SIZE
            && inset.height() > MIN_INLINE_SIZE;

        if can_nest {
            // 只在顶部留间距，左右下保持原样，节约空间
            let nested = Rect::from_min_max(
                Pos2::new(inset.min.x, inset.min.y + NEST_TOP),
                inset.max,
            );
            if nested.width() > 10.0 && nested.height() > 10.0 {
                draw_children(ui, nested, child, path, depth + 1, selected, action);
            }
        } else if child.expanded && !child.children.is_empty() {
            // 块太小展开不了 → 画一个小提示
            let painter = ui.painter_at(inset);
            painter.text(
                inset.center(),
                egui::Align2::CENTER_CENTER,
                "双击进入",
                FontId::proportional(8.0),
                Color32::from_rgba_unmultiplied(255, 255, 255, 120),
            );
        }

        // 子块消耗了点击事件就不再处理本级
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

fn draw_label(ui: &egui::Ui, painter: &egui::Painter, inset: egui::Rect, node: &Node, expanded: bool) {
    let pad = 3.0;
    let text_max_w = inset.width() - pad * 2.0;
    if inset.width() <= 18.0 || text_max_w <= 4.0 {
        return;
    }
    // 名字用小字体
    let name_font = FontId::proportional(9.0);
    let shown = truncate_text(ui.ctx(), &node.name, name_font.clone(), text_max_w);
    if !shown.is_empty() && inset.height() > 12.0 {
        let y_top = if expanded { NEST_TOP - 2.0 } else { 2.0 };
        painter.text(
            inset.left_top() + Vec2::new(pad, y_top),
            egui::Align2::LEFT_TOP,
            &shown,
            name_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 220),
        );
    }
    // 大小
    if inset.height() > 24.0 {
        let size_font = FontId::proportional(8.0);
        let sz = truncate_text(ui.ctx(), &human_size(node.size), size_font.clone(), text_max_w);
        painter.text(
            inset.left_bottom() + Vec2::new(pad, -2.0),
            egui::Align2::LEFT_BOTTOM,
            &sz,
            size_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 180),
        );
    }
}

fn show_tooltip(ui: &egui::Ui, id: egui::Id, node: &Node, _too_small: bool) {
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
