//! 递归 Treemap 色块视图，交互方式参考 SpaceSniffer：
//!
//! - **单击**一个色块：在原地把它按 squarify 算法再细分一层，
//!   显示它自己的子节点（嵌套色块）。再单击一次会收起（toggle）。
//! - **双击**一个色块：以它为新的"根"放大铺满整个 treemap 区域
//!   （对应 SpaceSniffer 里双击进入子文件夹的操作），并在顶部面包屑里留痕，
//!   可以随时点面包屑回退。
//!
//! 只有被标记 `expanded` 的节点才会往下递归绘制子色块，
//! 而不是不管有没有点开就无脑画到底——真实目录树可能有几十万个文件，
//! 不加这层限制，帧率会直接崩掉。

use egui::{Color32, FontId, Rounding, RichText, Stroke, Vec2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

/// 手动展开最多允许套多少层嵌套色块，防止用户一路点到底时把帧率拖垮。
const MAX_RENDER_DEPTH: u32 = 6;

/// 绘制 `view_root`（当前被"放大"显示的节点）的子节点铺满 `rect`，
/// 并按各自的 `expanded` 状态继续递归绘制更深层。
///
/// `base_path` 是 `view_root` 相对真实根节点的绝对路径，
/// 用来把内部产生的相对路径换算成 app.rs 能直接使用的绝对路径。
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
        let inset = r.shrink(1.5);
        if inset.width() < 1.0 || inset.height() < 1.0 {
            continue;
        }
        path.push(i);

        let is_selected = selected.as_deref() == Some(path.as_slice());
        let painter = ui.painter_at(inset);
        painter.rect_filled(inset, Rounding::same(4.0), child.color);
        if is_selected {
            painter.rect_stroke(inset, Rounding::same(4.0), Stroke::new(2.0_f32, Color32::WHITE));
        }

        draw_label(ui, &painter, inset, child);

        let id = ui.id().with(("treemap_block", path.clone()));
        let resp = ui.interact(inset, id, egui::Sense::click());

        if ui.rect_contains_pointer(inset) {
            show_tooltip(ui, id, inset, child);
        }

        if resp.double_clicked() {
            *action = TreeAction::ZoomTo(path.clone());
        } else if resp.clicked() {
            if child.children.is_empty() {
                *action = TreeAction::Select(path.clone());
            } else {
                *action = TreeAction::ToggleExpand(path.clone());
            }
        }

        // 只有"已经被单击展开过"的节点，才继续往下递归画嵌套子色块。
        if child.expanded && !child.children.is_empty() && depth + 1 < MAX_RENDER_DEPTH {
            let nested = inset.shrink(3.0);
            if nested.width() > 6.0 && nested.height() > 6.0 {
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
    let name_font = FontId::proportional(12.5);
    let shown_name = truncate_text(ui.ctx(), &node.name, name_font.clone(), text_max_w);
    if !shown_name.is_empty() {
        painter.text(
            inset.left_top() + Vec2::new(pad, 5.0),
            egui::Align2::LEFT_TOP,
            &shown_name,
            name_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 240),
        );
    }
    if inset.height() > 38.0 {
        let size_font = FontId::proportional(11.0);
        let size_text = human_size(node.size);
        let shown_size = truncate_text(ui.ctx(), &size_text, size_font.clone(), text_max_w);
        painter.text(
            inset.left_bottom() + Vec2::new(pad, -5.0),
            egui::Align2::LEFT_BOTTOM,
            &shown_size,
            size_font,
            Color32::from_rgba_unmultiplied(255, 255, 255, 205),
        );
    }
}

// 手动气泡：不用 egui 内置 on_hover_text（按下鼠标键时会被抑制），
// 直接检测鼠标位置，只要在块范围内就立刻显示，跟按没按键完全无关。
fn show_tooltip(ui: &egui::Ui, id: egui::Id, inset: egui::Rect, node: &Node) {
    let tip_pos = ui.ctx().pointer_latest_pos().unwrap_or(inset.left_bottom());
    let hint = if node.children.is_empty() {
        "单击选中".to_owned()
    } else if node.expanded {
        "单击收起 · 双击放大到整层"
    } else {
        "单击展开下一层 · 双击放大到整层"
    }
    .to_string();
    egui::Area::new(id.with("tip"))
        .fixed_pos(tip_pos + Vec2::new(14.0, 0.0))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::default()
                .fill(Color32::from_rgb(0x33, 0x33, 0x38))
                .rounding(4.0)
                .inner_margin(egui::Margin::same(6.0))
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("{} · {}", node.name, human_size(node.size))).color(Color32::WHITE));
                    ui.label(RichText::new(hint).size(11.0).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                });
        });
}
