//! Treemap 色块视图（SpaceSniffer 风格交互）
//!
//! 交互模型：
//! - **单击**文件夹：在原地内联展开子色块（嵌套）。再次单击收起。
//! - **双击**文件夹：Zoom 进入（铺满整个 treemap 区域）。
//! - **单击**文件：选中（固定灰色，表示不可继续展开）。
//!
//! Tooltip 逻辑：
//! - 在所有 interact 完成后，找到最深命中（鼠标位置下面最内层的块），
//!   只显示那一个气泡，避免父子同帧双气泡。
//!
//! 点击命中逻辑：
//! - 从最深层往外找：最里层的子块优先响应点击，父块只在未被子块消费时响应。
//!   用 `action` 的"一旦非 None 就不再覆盖"来实现优先级。

use egui::{Color32, CornerRadius, FontId, RichText, Stroke, StrokeKind, Vec2, Pos2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

const BLOCK_PAD: f32 = 2.5;
/// 嵌套内缩进（px）—— 给子层留出标题行高度
const NEST_PAD: f32 = 4.0;
const NEST_HEADER_H: f32 = 18.0;
/// 最大渲染嵌套深度，超过就不再递归画（性能保护）
/// 注意：这只是「渲染」深度，用户通过双击 ZoomTo 可以无限深入
const MAX_RENDER_DEPTH: u32 = 8;

/// 文件色块固定灰色，一眼识别"不可继续展开"
const FILE_COLOR: Color32 = Color32::from_rgb(0x4A, 0x55, 0x60);
const FILE_BORDER: Color32 = Color32::from_rgb(0x38, 0x42, 0x4C);

/// 传递 tooltip 候选：记录鼠标命中的「最深」一个节点信息
struct TooltipCandidate<'a> {
    id: egui::Id,
    node: &'a Node,
    depth: u32,
}

pub fn show(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    view_root: &Node,
    base_path: &[usize],
    selected: &Option<NodePath>,
) -> TreeAction {
    let mut action = TreeAction::None;

    let painter = ui.painter_at(rect);
    let clip = painter.clip_rect();
    let draw_rect = rect.intersect(clip);
    if draw_rect.width() < 2.0 || draw_rect.height() < 2.0 {
        return action;
    }

    let mut tooltip: Option<TooltipCandidate> = None;
    let mut path = base_path.to_vec();

    draw_children(
        ui,
        draw_rect,
        view_root,
        &mut path,
        0,
        selected,
        &mut action,
        &mut tooltip,
    );

    // 统一在最后画气泡，保证只有一个（最深层命中的）
    if let Some(tip) = tooltip {
        show_tooltip(ui, tip.id, tip.node);
    }

    action
}

#[allow(clippy::too_many_arguments)]
fn draw_children<'a>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    node: &'a Node,
    path: &mut NodePath,
    depth: u32,
    selected: &Option<NodePath>,
    action: &mut TreeAction,
    tooltip: &mut Option<TooltipCandidate<'a>>,
) {
    if node.children.is_empty() || depth >= MAX_RENDER_DEPTH {
        return;
    }

    let sizes: Vec<u64> = node.children.iter().map(|c| c.size).collect();
    let rects = compute_treemap(&sizes, rect);

    for (i, (r, child)) in rects.iter().zip(node.children.iter()).enumerate() {
        let inset = r.shrink(BLOCK_PAD).intersect(rect);
        if inset.width() < 2.0 || inset.height() < 2.0 {
            continue;
        }
        path.push(i);

        let is_file = !child.is_folder();
        let block_color = if is_file { FILE_COLOR } else { child.color };
        let is_selected = selected.as_deref() == Some(path.as_slice());

        // ── 绘制背景 ───────────────────────────────────────────
        let painter = ui.painter_at(inset);
        painter.rect_filled(inset, CornerRadius::same(4), block_color);
        if is_file {
            painter.rect_stroke(inset, CornerRadius::same(4), Stroke::new(1.0_f32, FILE_BORDER), StrokeKind::Inside);
        }
        if is_selected {
            painter.rect_stroke(inset, CornerRadius::same(4), Stroke::new(2.0_f32, Color32::WHITE), StrokeKind::Inside);
        }
        draw_label(ui, &painter, inset, child);

        // ── 嵌套子块（先画，再注册当前层 interact）───────────
        // 先递归画子层，子层的 interact 会先注册到 egui 的命中列表里，
        // 这样子层的点击就能优先于父层响应（egui 按注册顺序，后注册优先）。
        // 同时子层的 tooltip 候选会先写入，父层只在子层没命中时才覆盖。
        if child.expanded && !child.children.is_empty() {
            let header_h = if inset.height() > NEST_HEADER_H + NEST_PAD * 2.0 {
                NEST_HEADER_H
            } else {
                0.0
            };
            let nested = egui::Rect::from_min_size(
                inset.min + Vec2::new(NEST_PAD, header_h + NEST_PAD),
                egui::vec2(
                    (inset.width() - NEST_PAD * 2.0).max(0.0),
                    (inset.height() - header_h - NEST_PAD * 2.0).max(0.0),
                ),
            );
            if nested.width() > 8.0 && nested.height() > 8.0 {
                draw_children(ui, nested, child, path, depth + 1, selected, action, tooltip);
            }
        }

        // ── 注册当前块的 interact（在子层之后，优先级低于子层）
        let id = ui.id().with(("block", path.clone()));
        let resp = ui.interact(inset, id, egui::Sense::click());

        // 点击：只在 action 尚未被更深层消费时才处理
        if matches!(action, TreeAction::None) {
            if resp.double_clicked() {
                if !child.children.is_empty() {
                    *action = TreeAction::ZoomTo(path.clone());
                }
            } else if resp.clicked() {
                if child.children.is_empty() {
                    *action = TreeAction::Select(path.clone());
                } else {
                    *action = TreeAction::ToggleExpand(path.clone());
                }
            }
        }

        // ── Tooltip：取「最深」命中（深度更大的优先）───────────
        // 鼠标在当前块内 → 记录候选；如果已有候选且更深，则不覆盖。
        if ui.rect_contains_pointer(inset) {
            let replace = match &tooltip {
                None => true,
                Some(prev) => depth > prev.depth, // 更深的子块优先
            };
            if replace {
                *tooltip = Some(TooltipCandidate { id, node: child, depth });
            }
        }

        path.pop();
    }
}

fn draw_label(ui: &egui::Ui, painter: &egui::Painter, inset: egui::Rect, node: &Node) {
    let pad = 5.0;
    let text_max_w = inset.width() - pad * 2.0;
    if inset.width() <= 20.0 || inset.height() <= 14.0 || text_max_w <= 6.0 {
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
            Color32::from_rgba_unmultiplied(255, 255, 255, 230),
        );
    }
    if inset.height() > 34.0 {
        let size_font = FontId::proportional(10.0);
        let size_text = human_size(node.size);
        let shown_size = truncate_text(ui.ctx(), &size_text, size_font.clone(), text_max_w);
        if !shown_size.is_empty() {
            painter.text(
                inset.left_bottom() + Vec2::new(pad, -3.0),
                egui::Align2::LEFT_BOTTOM,
                &shown_size,
                size_font,
                Color32::from_rgba_unmultiplied(255, 255, 255, 190),
            );
        }
    }
}

/// 气泡显示在鼠标右上方，超出屏幕边界时自动翻转方向。
/// interactable(false) 确保不拦截任何鼠标事件。
fn show_tooltip(ui: &egui::Ui, id: egui::Id, node: &Node) {
    let ctx = ui.ctx();
    let mouse_pos = match ctx.pointer_latest_pos() {
        Some(p) => p,
        None => return,
    };

    let hint = if node.children.is_empty() {
        "文件 · 单击选中"
    } else if node.expanded {
        "文件夹 · 单击收起 · 双击进入"
    } else {
        "文件夹 · 单击展开 · 双击进入"
    };

    let tip_w = 240.0_f32;
    let tip_h = 52.0_f32;
    let screen = ctx.content_rect();

    // 默认右上方
    let mut tip_x = mouse_pos.x + 16.0;
    let mut tip_y = mouse_pos.y - tip_h - 8.0;

    // 右侧超界 → 左侧
    if tip_x + tip_w > screen.right() - 8.0 {
        tip_x = mouse_pos.x - tip_w - 16.0;
    }
    // 上方超界 → 下方
    if tip_y < screen.top() + 8.0 {
        tip_y = mouse_pos.y + 20.0;
    }
    // 左侧超界兜底
    if tip_x < screen.left() + 4.0 {
        tip_x = screen.left() + 4.0;
    }

    egui::Area::new(id.with("tooltip"))
        .fixed_pos(Pos2::new(tip_x, tip_y))
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
                            .color(Color32::WHITE)
                            .size(12.5),
                    );
                    ui.label(
                        RichText::new(hint)
                            .size(10.5)
                            .color(Color32::from_rgb(0x90, 0x90, 0x98)),
                    );
                });
        });
}
