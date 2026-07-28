//! Treemap 色块视图（SpaceSniffer 风格）
//!
//! - **单击文件夹**：在当前块内展开子色块（inline 嵌套）。
//! - **单击已展开的文件夹**：收起。
//! - **单击文件**：选中。
//! - **双击文件夹**：放大到该层（ZoomTo），上层色块保留在顶部作为返回入口。
//!
//! 嵌套点击的核心：先递归子块（消耗点击事件），再处理本级。
//!
//! 文字位置：色块名字永远画在左上角（y=2），不因 expanded 状态变化。
//! 嵌套区域从 y=NEST_TOP 开始，与标签留出 2px 间隔避免覆盖。

use egui::{Color32, CornerRadius, FontId, Pos2, Rect, RichText, Stroke, StrokeKind, Vec2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

/// 色块之间的间距（像素）
const BLOCK_PAD: f32 = 1.0;
/// 嵌套展开时顶部预留的高度（容纳标签）
const NEST_TOP: f32 = 14.0;
const MAX_DEPTH: u32 = 6;
/// 子块小于此尺寸直接不渲染（看不见也点不到，需双击上层进入查看）
const MIN_RENDER_W: f32 = 6.0;
const MIN_RENDER_H: f32 = 6.0;
/// 子块小于此尺寸不允许 inline 嵌套展开（单击只 select，tooltip 提示双击）
const MIN_EXPAND_W: f32 = 36.0;
const MIN_EXPAND_H: f32 = 28.0;
/// 双击进入后，顶部"上层色块"的高度
const HEADER_H: f32 = 22.0;

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

    // 1) 先用 view_root 自己的颜色填满整个 rect ——
    //    这样不渲染的小色块区域露出的是"当前层"的颜色（而非上层），
    //    避免出现"上层背景色"造成的误解。
    {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(2), view_root.color);
    }

    // 2) 如果有上层，在顶部画"上层色块"作为返回入口
    //    （保留上层颜色 + 左上角显示名字，参考 SpaceSniffer）
    let children_rect = if let (Some(parent), Some(parent_base)) = (parent_node, parent_base_path) {
        let header_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER_H));
        let painter = ui.painter_at(header_rect);
        painter.rect_filled(header_rect, CornerRadius::same(2), parent.color);
        // 上层名字（左上角）
        let name_font = FontId::proportional(11.0);
        let text_max = (header_rect.width() - 28.0).max(20.0);
        let shown = truncate_text(ui.ctx(), &parent.name, name_font.clone(), text_max);
        let label = format!("⬆ {}", shown);
        painter.text(
            header_rect.left_top() + Vec2::new(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            &label,
            name_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 235),
        );
        // 点击上层色块 = 返回上一层
        let up_id = ui.id().with("go_up");
        let up_resp = ui.interact(header_rect, up_id, egui::Sense::click());
        if up_resp.clicked() && !up_resp.double_clicked() {
            action = TreeAction::ZoomTo(parent_base.to_vec());
        }
        if ui.rect_contains_pointer(header_rect) {
            show_up_tooltip(ui, up_id, parent);
        }
        // 子色块区域 = header 下方（留 1px 间距做视觉分割）
        Rect::from_min_max(
            Pos2::new(rect.min.x, header_rect.max.y + BLOCK_PAD),
            rect.max,
        )
    } else {
        rect
    };

    // 3) 画当前层的子色块
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
                    ui.label(
                        RichText::new(format!("⬆ 返回 {}", parent.name))
                            .color(Color32::WHITE),
                    );
                    ui.label(
                        RichText::new("单击 = 返回上一层")
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
    if node.children.is_empty() {
        return;
    }
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }

    let sizes: Vec<u64> = node.children.iter().map(|c| c.size).collect();
    let rects = compute_treemap(&sizes, rect);

    for (i, (r, child)) in rects.iter().zip(node.children.iter()).enumerate() {
        let inset = r.shrink(BLOCK_PAD);

        // 最小渲染阈值：太小不画、不交互
        if inset.width() < MIN_RENDER_W || inset.height() < MIN_RENDER_H {
            continue;
        }

        path.push(i);

        let is_file = child.children.is_empty();
        let block_color = if is_file { FILE_COLOR } else { child.color };
        let is_selected = selected.as_deref() == Some(path.as_slice());

        let painter = ui.painter_at(inset);
        painter.rect_filled(inset, CornerRadius::same(2), block_color);
        if is_file {
            painter.rect_stroke(
                inset,
                CornerRadius::same(2),
                Stroke::new(1.0, FILE_BORDER),
                StrokeKind::Inside,
            );
        }
        if is_selected {
            painter.rect_stroke(
                inset,
                CornerRadius::same(2),
                Stroke::new(2.0, Color32::WHITE),
                StrokeKind::Inside,
            );
        }

        // 判断是否适合 inline 展开
        let can_inline_expand = !is_file
            && child.expanded
            && depth + 1 < MAX_DEPTH
            && inset.width() > MIN_EXPAND_W
            && inset.height() > MIN_EXPAND_H;

        // 标签：永远左上角（y=2），不因 expanded 状态变化
        draw_label(ui, &painter, inset, child);

        let id = ui.id().with(("block", path.clone()));
        let resp = ui.interact(inset, id, egui::Sense::click());
        let was_clicked = resp.clicked();
        let was_dbl = resp.double_clicked();

        if ui.rect_contains_pointer(inset) {
            show_tooltip(ui, id, child, !can_inline_expand && child.expanded && !is_file);
        }

        if can_inline_expand {
            // 嵌套区域从 NEST_TOP 开始，与标签留 2px 间隔避免覆盖
            let nested = Rect::from_min_max(
                Pos2::new(inset.min.x, inset.min.y + NEST_TOP),
                inset.max,
            );
            if nested.width() > 4.0 && nested.height() > 4.0 {
                draw_children(ui, nested, child, path, depth + 1, selected, action);
            }
        }

        // 子块消耗了点击事件就不再处理本级
        if was_clicked && matches!(*action, TreeAction::None) {
            if was_dbl {
                if !child.children.is_empty() {
                    *action = TreeAction::ZoomTo(path.clone());
                }
            } else if is_file {
                *action = TreeAction::Select(path.clone());
            } else {
                // 文件夹单击：太小不允许 inline 展开，仅 select（tooltip 提示双击）
                if inset.width() < MIN_EXPAND_W || inset.height() < MIN_EXPAND_H {
                    *action = TreeAction::Select(path.clone());
                } else {
                    *action = TreeAction::ToggleExpand(path.clone());
                }
            }
        }

        path.pop();
    }
}

/// 色块标签：名字 + 大小，都画在左上角附近。
/// 名字 y=2，大小 y=底部。永远左上角对齐，不因 expanded 状态变化。
fn draw_label(ui: &egui::Ui, painter: &egui::Painter, inset: egui::Rect, node: &Node) {
    let pad = 3.0;
    let text_max_w = (inset.width() - pad * 2.0).max(0.0);
    if inset.width() <= 14.0 || text_max_w <= 4.0 {
        return;
    }
    let name_font = FontId::proportional(9.0);
    let shown = truncate_text(ui.ctx(), &node.name, name_font.clone(), text_max_w);
    if !shown.is_empty() && inset.height() > 11.0 {
        painter.text(
            inset.left_top() + Vec2::new(pad, 2.0),
            egui::Align2::LEFT_TOP,
            &shown,
            name_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 220),
        );
    }
    if inset.height() > 22.0 {
        let size_font = FontId::proportional(8.0);
        let sz = truncate_text(ui.ctx(), &human_size(node.size), size_font.clone(), text_max_w);
        if !sz.is_empty() {
            painter.text(
                inset.left_bottom() + Vec2::new(pad, -2.0),
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
