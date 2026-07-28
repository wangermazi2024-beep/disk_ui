//! Treemap 色块视图（SpaceSniffer 风格交互）
//!
//! - **单击**文件夹：在当前视图里展开/折叠其子色块（递归嵌套）。
//! - **单击**文件：选中它。
//! - **双击**文件夹：放大到该层（铺满整个 treemap 区域）。
//!
//! 文件色块使用固定灰色，文件夹使用各自的 type 颜色，
//! 一眼就能区分可继续点击的文件夹和终点文件。

use egui::{Color32, FontId, Rounding, RichText, Stroke, Vec2, Pos2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

const BLOCK_PAD: f32 = 2.5;
const MAX_DEPTH: u32 = 5;
const NEST_PAD: f32 = 4.0;

/// 文件色块固定灰色，一眼识别"不可继续展开"
const FILE_COLOR: Color32 = Color32::from_rgb(0x4A, 0x55, 0x60);
/// 文件色块边框，进一步强调"终点"感
const FILE_BORDER: Color32 = Color32::from_rgb(0x38, 0x42, 0x4C);

pub fn show(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    view_root: &Node,
    base_path: &[usize],
    selected: &Option<NodePath>,
) -> TreeAction {
    let mut action = TreeAction::None;
    // 用 clip_rect 确保色块不会渲染到分配区域之外
    let painter = ui.painter_at(rect);
    let clip = painter.clip_rect();
    let draw_rect = rect.intersect(clip);
    if draw_rect.width() < 2.0 || draw_rect.height() < 2.0 {
        return action;
    }

    // tooltip 状态：只允许最顶层（depth=0）的命中节点显示 tooltip，
    // 收集到这里统一在最后画，避免嵌套层级重复触发。
    let mut tooltip_info: Option<(egui::Id, egui::Rect, &Node)> = None;

    let mut path = base_path.to_vec();
    draw_children(
        ui,
        draw_rect,
        view_root,
        &mut path,
        0,
        selected,
        &mut action,
        &mut tooltip_info,
    );

    // 统一在所有色块之后画 tooltip，保证只有一个
    if let Some((id, inset, node)) = tooltip_info {
        show_tooltip(ui, id, inset, node);
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
    tooltip_info: &mut Option<(egui::Id, egui::Rect, &'a Node)>,
) {
    if node.children.is_empty() {
        return;
    }
    let sizes: Vec<u64> = node.children.iter().map(|c| c.size).collect();
    let rects = compute_treemap(&sizes, rect);

    for (i, (r, child)) in rects.iter().zip(node.children.iter()).enumerate() {
        let inset = r.shrink(BLOCK_PAD);
        // 裁剪到父级 rect，防止超界
        let inset = inset.intersect(rect);
        if inset.width() < 2.0 || inset.height() < 2.0 {
            path.push(i);
            path.pop();
            continue;
        }
        path.push(i);

        let is_file = !child.is_folder();
        // 文件：固定灰色；文件夹：用各自颜色
        let block_color = if is_file { FILE_COLOR } else { child.color };
        let is_selected = selected.as_deref() == Some(path.as_slice());

        let painter = ui.painter_at(inset);
        painter.rect_filled(inset, Rounding::same(4.0), block_color);

        // 文件额外描边，强调"不可展开"
        if is_file {
            painter.rect_stroke(inset, Rounding::same(4.0), Stroke::new(1.0_f32, FILE_BORDER));
        }
        if is_selected {
            painter.rect_stroke(inset, Rounding::same(4.0), Stroke::new(2.0_f32, Color32::WHITE));
        }

        draw_label(ui, &painter, inset, child);

        let id = ui.id().with(("block", path.clone()));
        // 使用 click + double_click 两个独立的 Sense
        let resp = ui.interact(inset, id, egui::Sense::click());

        // ── 点击处理 ────────────────────────────────────────────
        // egui 0.28: double_clicked() 是独立事件，不走 clicked()，
        // 所以要分别检测，而不是嵌套。
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

        // ── Tooltip：只记录 depth==0 且鼠标命中的节点 ───────────
        // depth>0 的嵌套子块不单独显示 tooltip，避免父子同帧双气泡。
        if depth == 0 && tooltip_info.is_none() && ui.rect_contains_pointer(inset) {
            *tooltip_info = Some((id, inset, child));
        }

        // ── 嵌套展开 ────────────────────────────────────────────
        if child.expanded && !child.children.is_empty() && depth + 1 < MAX_DEPTH {
            // 内缩后留给子层的空间
            let header_h = if inset.height() > 28.0 { 18.0 } else { 0.0 };
            let nested = egui::Rect::from_min_size(
                inset.min + Vec2::new(NEST_PAD, header_h + NEST_PAD),
                egui::vec2(
                    (inset.width() - NEST_PAD * 2.0).max(0.0),
                    (inset.height() - header_h - NEST_PAD * 2.0).max(0.0),
                ),
            );
            if nested.width() > 8.0 && nested.height() > 8.0 {
                draw_children(ui, nested, child, path, depth + 1, selected, action, tooltip_info);
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

/// 气泡：定位策略参考 SpaceSniffer——
/// 1. 默认显示在鼠标右上方（偏移 +16, -8）。
/// 2. 如果右边放不下，改到鼠标左侧。
/// 3. 如果上边放不下，改到鼠标下方。
/// 4. interactable(false) 确保气泡不会拦截鼠标事件。
fn show_tooltip(ui: &egui::Ui, id: egui::Id, _inset: egui::Rect, node: &Node) {
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

    // 先用一个估算宽度（实际宽度由内容决定），足够做碰撞检测
    let tip_w_estimate = 220.0_f32;
    let tip_h_estimate = 52.0_f32;
    let screen = ctx.screen_rect();

    // 默认：鼠标右上方
    let mut tip_x = mouse_pos.x + 16.0;
    let mut tip_y = mouse_pos.y - tip_h_estimate - 8.0;

    // 右侧放不下 → 改到左侧
    if tip_x + tip_w_estimate > screen.right() - 8.0 {
        tip_x = mouse_pos.x - tip_w_estimate - 16.0;
    }
    // 上方放不下 → 改到鼠标下方
    if tip_y < screen.top() + 8.0 {
        tip_y = mouse_pos.y + 20.0;
    }

    egui::Area::new(id.with("tooltip"))
        .fixed_pos(Pos2::new(tip_x, tip_y))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::default()
                .fill(Color32::from_rgb(0x2A, 0x2C, 0x32))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0x55, 0x55, 0x60)))
                .rounding(5.0)
                .inner_margin(egui::Margin::symmetric(8.0, 5.0))
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
