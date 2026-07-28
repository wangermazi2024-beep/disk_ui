//! 递归 Treemap 色块视图，交互方式参考 SpaceSniffer：
//!
//! - **单击**一个色块：选中它（高亮边框）。
//! - **双击**一个文件夹色块：以它为新的"根"放大铺满整个 treemap 区域
//!   （对应 SpaceSniffer 里双击进入子文件夹的操作），并在顶部面包屑里留痕，
//!   可以随时点面包屑回退。
//! - 单击文件色块：选中它。
//!
//! 展开/折叠操作由右侧文件列表树的 ▶/▼ 按钮完成，treemap 色块不参与展开/折叠，
//! 避免单击/双击冲突。

use egui::{Color32, FontId, Rounding, RichText, Stroke, Vec2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

/// 色块之间的间距（px）
const BLOCK_PAD: f32 = 3.0;

/// 绘制 `view_root`（当前被"放大"显示的节点）的子节点铺满 `rect`。
pub fn show(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    view_root: &Node,
    base_path: &[usize],
    selected: &Option<NodePath>,
) -> TreeAction {
    let mut action = TreeAction::None;
    let mut path = base_path.to_vec();
    draw_children(ui, rect, view_root, &mut path, selected, &mut action);
    action
}

fn draw_children(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    node: &Node,
    path: &mut NodePath,
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

        // ✅ 单击/双击分离：先判断 clicked，再在里面判断 double_clicked
        // 避免第一次单击触发选中，第二次才触发双击的问题
        if resp.clicked() {
            if resp.double_clicked() {
                if !child.children.is_empty() {
                    *action = TreeAction::ZoomTo(path.clone());
                }
            } else {
                *action = TreeAction::Select(path.clone());
            }
        }

        path.pop();
    }
}

fn draw_label(ui: &egui::Ui, painter: &egui::Painter, inset: egui::Rect, node: &Node) {
    let pad = 8.0;
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

// 手动气泡，不用 egui 内置 on_hover_text（按下鼠标键时会被抑制）
fn show_tooltip(ui: &egui::Ui, id: egui::Id, inset: egui::Rect, node: &Node) {
    let tip_pos = ui.ctx().pointer_latest_pos().unwrap_or(inset.left_bottom());
    let hint = if node.children.is_empty() {
        "单击选中"
    } else {
        "双击进入"
    };
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
