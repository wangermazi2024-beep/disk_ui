//! Treemap 色块视图（SpaceSniffer 风格）
//!
//! - **单击文件夹**：在当前块内展开子色块（inline 嵌套）。
//! - **单击已展开的文件夹**：收起。
//! - **单击文件**：选中。
//! - **双击文件夹**：ZoomTo，画面变成三层——
//!   上一层（parent）→ 当前层（被双击的节点自己）→ 子层（当前层的孩子）。
//!
//! 三层用的是同一套色块渲染 + 单击/双击判定逻辑（`draw_block` / `resolve_click`），
//! 没有为"上一层"单独写一套背景绘制代码：它只是被"强制当成已展开"来画的一个普通色块。
//! 强制展开只沿着 `zoom_path` 这一条链走一层（上一层 → 当前层），
//! 再往里（当前层的子层）就回到正常的 `expanded` 状态驱动，和一直以来的行为一致。
//! 这样双击"上一层"色块和双击任何普通文件夹色块走的是完全相同的代码路径：
//! 它自己变成新的当前层，于是可以连续双击一路返回根目录（根目录没有上一层，自动停止）。
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

/// `root`：整棵树的真正根节点（不是 view_root），因为"上一层"要能一路双击回到根目录，
/// 需要能从任意 parent 继续往上导航。
/// `zoom_path`：当前双击放大到的节点路径（相对 root 的绝对路径），空路径代表显示根节点，
/// 此时没有"上一层"，直接铺满 root 的孩子（和原来的顶层视图完全一样）。
pub fn show(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    root: &Node,
    zoom_path: &[usize],
    selected: &Option<NodePath>,
) -> TreeAction {
    let mut action = TreeAction::None;

    // 顶层（没有双击进入过任何文件夹）：和以前一样，直接把 root 的孩子铺满整个画布，
    // 不需要任何"上一层"包装。
    let Some((&target_idx, parent_path)) = zoom_path.split_last() else {
        let mut path = Vec::new();
        draw_children(ui, rect, root, &mut path, 0, selected, None, &mut action);
        return action;
    };

    let Some(parent) = root.navigate(parent_path) else {
        // 路径失效（比如扫描结果变了）：退回顶层渲染，不崩溃。
        let mut path = Vec::new();
        draw_children(ui, rect, root, &mut path, 0, selected, None, &mut action);
        return action;
    };

    // "上一层"就是一个普通色块，只是强制当成"已展开"来画——
    // 复用 draw_block，不为它单独写背景/标签绘制代码。
    let mut path = parent_path.to_vec();
    let (resp, can_inline_expand) = draw_block(ui, rect, parent, &path, 0, selected, true);

    if can_inline_expand {
        // 强制展开进 parent 内部：画出 parent 的所有孩子（含"当前层"和它的兄弟节点，
        // 兄弟节点作为上下文正常显示，不强制展开）。其中 zoom_path 最后一位对应的
        // 那个孩子——也就是被双击进来的"当前层"——同样被强制展开，
        // 这样"上一层 → 当前层 → 子层"三层色块就都露出来了。
        let nested = Rect::from_min_max(Pos2::new(rect.min.x, rect.min.y + NEST_TOP), rect.max);
        if nested.width() > 4.0 && nested.height() > 4.0 {
            draw_children(ui, nested, parent, &mut path, 0, selected, Some(target_idx), &mut action);
        }
    }

    if let Some(a) = resolve_click(&resp, false, false, !parent.children.is_empty(), &path) {
        if matches!(action, TreeAction::None) {
            action = a;
        }
    }

    action
}

/// 画一个色块的通用逻辑（填色、描边、选中高亮、标签、tooltip），
/// 并算出它是否应该 inline 展开画出子层。
/// `force_expand`：不看 `node.expanded`，强制当成已展开来画——"上一层"色块和
/// 被强制展开的"当前层"色块都用这个参数复用同一份渲染代码。
/// 返回 `(response, can_inline_expand)`，调用方自己决定要不要递归画子层、
/// 要不要把点击结果写进 action（用 `resolve_click`，同样是共用逻辑）。
fn draw_block(
    ui: &mut egui::Ui,
    r: egui::Rect,
    node: &Node,
    path: &NodePath,
    depth: u32,
    selected: &Option<NodePath>,
    force_expand: bool,
) -> (egui::Response, bool) {
    let is_file = node.children.is_empty();
    let block_color = if is_file { FILE_COLOR } else { node.color };
    let is_selected = selected.as_deref() == Some(path.as_slice());

    let painter = ui.painter_at(r);
    painter.rect_filled(r, CornerRadius::same(2), block_color);
    // 每个色块都画边框线，无间隙时靠边框区分相邻块
    let border_color = if is_file { FILE_BORDER } else { Color32::from_rgba_unmultiplied(0, 0, 0, 40) };
    painter.rect_stroke(r, CornerRadius::same(2), Stroke::new(1.0, border_color), StrokeKind::Inside);
    if is_selected {
        painter.rect_stroke(r, CornerRadius::same(2), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
    }

    let expanded = node.expanded || force_expand;
    let can_inline_expand = !is_file
        && expanded
        && depth < MAX_DEPTH
        && r.width() > MIN_EXPAND_W
        && r.height() > MIN_EXPAND_H;

    draw_label(ui, &painter, r, node);

    let id = ui.id().with(("block", path.clone()));
    let resp = ui.interact(r, id, egui::Sense::click());

    if ui.rect_contains_pointer(r) {
        show_tooltip(ui, id, node, !can_inline_expand && expanded && !is_file);
    }

    (resp, can_inline_expand)
}

/// 单击/双击的动作判定，供 `draw_children` 遍历兄弟节点和 `show` 里的"上一层"色块共用：
/// 双击 = 放大成为新的当前层（ZoomTo，前提是有孩子）；
/// 单击文件 = 选中；单击块太小的文件夹 = 选中（避免展开出看不清的内容）；
/// 单击正常大小的文件夹 = 原地展开/收起（ToggleExpand）。
fn resolve_click(
    resp: &egui::Response,
    is_file: bool,
    too_small: bool,
    has_children: bool,
    path: &NodePath,
) -> Option<TreeAction> {
    if !resp.clicked() {
        return None;
    }
    if resp.double_clicked() {
        if has_children {
            Some(TreeAction::ZoomTo(path.clone()))
        } else {
            None
        }
    } else if is_file || too_small {
        Some(TreeAction::Select(path.clone()))
    } else {
        Some(TreeAction::ToggleExpand(path.clone()))
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_children(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    node: &Node,
    path: &mut NodePath,
    depth: u32,
    selected: &Option<NodePath>,
    forced_index: Option<usize>,
    action: &mut TreeAction,
) {
    if node.children.is_empty() { return; }
    if rect.width() < 2.0 || rect.height() < 2.0 { return; }

    let mut sizes: Vec<u64> = node.children.iter().map(|c| c.size).collect();
    // 被强制展开的那个孩子（"当前层"）无论实际占用多少磁盘空间，
    // 都必须在画面上有足够大的地盘可见——否则如果它刚好是一个很小的文件夹，
    // 会被排布算法挤成几个像素宽的细条，等于又变相"消失"了。
    // 这里把它的权重提到至少等于其余兄弟节点权重之和，保证至少拿到一半版面。
    if let Some(fi) = forced_index {
        if let Some(slot) = sizes.get(fi).copied() {
            let total: u64 = sizes.iter().sum();
            let others = total.saturating_sub(slot);
            if let Some(s) = sizes.get_mut(fi) {
                *s = slot.max(others).max(1);
            }
        }
    }

    // compute_treemap 已包含 LAYOUT_PAD 间距，直接用，不再 shrink
    let rects = compute_treemap(&sizes, rect);

    for (i, (r, child)) in rects.iter().zip(node.children.iter()).enumerate() {
        // 太小不渲染
        if r.width() < MIN_RENDER_W || r.height() < MIN_RENDER_H {
            continue;
        }

        path.push(i);

        let force_this = forced_index == Some(i);
        let (resp, can_inline_expand) = draw_block(ui, *r, child, path, depth + 1, selected, force_this);

        if can_inline_expand {
            let nested = Rect::from_min_max(
                Pos2::new(r.min.x, r.min.y + NEST_TOP),
                r.max,
            );
            if nested.width() > 4.0 && nested.height() > 4.0 {
                // 强制展开只沿 zoom_path 走一层：往更深处递归时不再传递 forced_index，
                // 子层内部的展开/收起完全交回 node.expanded 正常驱动。
                draw_children(ui, nested, child, path, depth + 1, selected, None, action);
            }
        }

        let is_file = child.children.is_empty();
        let too_small = !is_file && (r.width() < MIN_EXPAND_W || r.height() < MIN_EXPAND_H);
        if let Some(a) = resolve_click(&resp, is_file, too_small, !child.children.is_empty(), path) {
            if matches!(*action, TreeAction::None) {
                *action = a;
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
