//! Treemap 色块视图（SpaceSniffer 风格）
//!
//! - **单击文件夹**：在当前块内展开子色块（inline 嵌套）。
//! - **单击已展开的文件夹**：收起。
//! - **单击文件**：选中。
//! - **双击文件夹**：产生 `TreeAction::EnterNode`，`app.rs` 据此把该节点的父节点
//!   设为新的"当前视图根"（只是切换 `view_path` 这个只读导航索引，不修改、
//!   不复制、也不丢弃树里任何一个节点）。下一帧本视图用 `root.navigate(view_path)`
//!   重新定位到新的视图根，画法和最外层视图完全一样，没有任何特殊分支。
//! - **顶部"⬆ 上级目录"标题条**：代表"当前视图根自己"这一个色块。双击它，
//!   产生的 `TreeAction::EnterNode` 携带的正是当前视图根自己的绝对路径——
//!   跟双击普通子色块走的是完全同一条处理逻辑（取父路径设为新根），
//!   所以效果是"再上一级的目录"变成新的最外层，用户可以一路双击这个标题条
//!   逐层返回，直到回到真正的根目录（比如 C 盘）。

use egui::{Color32, CornerRadius, FontId, Pos2, Rect, RichText, Stroke, StrokeKind, Vec2};

use crate::format::{human_size, truncate_text};
use crate::model::{Node, NodePath};
use crate::treemap::compute_treemap;

use super::TreeAction;

const MAX_DEPTH: u32 = 6;
const MIN_RENDER_W: f32 = 6.0;
const MIN_RENDER_H: f32 = 6.0;
const MIN_EXPAND_W: f32 = 36.0;
const MIN_EXPAND_H: f32 = 28.0;
const NEST_TOP: f32 = 14.0;
/// 顶部"当前视图根"标题条的高度，双击它等价于双击一个"代表当前目录自己"的色块。
const ROOT_HEADER_H: f32 = 22.0;

const FILE_COLOR: Color32 = Color32::from_rgb(0x5A, 0x6B, 0x7C);
const FILE_BORDER: Color32 = Color32::from_rgb(0x6A, 0x7B, 0x8C);

/// 当前悬停到的（最深一层）色块信息，整帧只保留最后一次写入，
/// 最后统一画一个气泡，避免父子多层色块同时弹出好几个气泡叠在一起。
struct HoverInfo {
    name: String,
    size: u64,
    hint: &'static str,
}

pub fn show(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    root: &Node,
    view_path: &[usize],
    selected: &Option<NodePath>,
    // 上一帧"展开/进入/跳转"操作的目标层路径：如果非空，说明这一层的子色块
    // 是刚刚才出现在画面上的，需要自动选中其中"第一个真正被渲染出来"的色块，
    // 避免旧的白色选中边框停留在一个现在看起来毫不相关的位置上。
    auto_select: Option<&NodePath>,
) -> TreeAction {
    let mut action = TreeAction::None;
    let mut hover: Option<HoverInfo> = None;

    // 只读导航到当前视图根：view_path 失效时（比如目标节点被重新扫描后没了）
    // 退回真正的根节点，不 panic、不 crash。
    let (view_root, root_path) = match root.navigate(view_path) {
        Some(n) => (n, view_path.to_vec()),
        None => (root, Vec::new()),
    };

    // 只有当前视图根不是"真正的根"时，才画这条"⬆ 上级目录"标题条——
    // 已经是真正根目录的话，再往上没有地方可去了。
    let children_rect = if !root_path.is_empty() {
        let header_rect = Rect::from_min_max(
            rect.min,
            Pos2::new(rect.max.x, (rect.min.y + ROOT_HEADER_H).min(rect.max.y)),
        );
        draw_root_header(ui, header_rect, view_root, &root_path, &mut action, &mut hover);
        Rect::from_min_max(
            Pos2::new(rect.min.x, (rect.min.y + ROOT_HEADER_H).min(rect.max.y)),
            rect.max,
        )
    } else {
        rect
    };

    let mut path = root_path;
    draw_children(ui, children_rect, view_root, &mut path, 0, selected, auto_select, &mut action, &mut hover);

    if let Some(h) = hover {
        show_tooltip(ui, h);
    }

    action
}

/// 顶部"⬆ 上级目录"标题条：代表当前视图根自己的一个可交互色块。
/// 双击它和双击普通子色块走同一个 `TreeAction::EnterNode`，只是携带的路径
/// 是"自己"而不是某个孩子——`app.rs` 一律取其父路径作为新的视图根，
/// 所以效果自然就是"再上一级目录变成新的最外层"，可以一路双击点回真正的根。
fn draw_root_header(
    ui: &egui::Ui,
    rect: Rect,
    node: &Node,
    path: &NodePath,
    action: &mut TreeAction,
    hover: &mut Option<HoverInfo>,
) {
    if rect.height() < 4.0 || rect.width() < 4.0 {
        return;
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(2), Color32::from_rgb(0x40, 0x42, 0x48));
    painter.rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0, Color32::from_rgb(0x58, 0x5B, 0x64)),
        StrokeKind::Inside,
    );

    let font = FontId::proportional(11.0);
    let max_w = (rect.width() - 12.0).max(0.0);
    let label = truncate_text(ui.ctx(), &format!("⬆ {}", node.name), font.clone(), max_w);
    painter.text(
        rect.left_center() + Vec2::new(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        font,
        Color32::from_rgba_unmultiplied(255, 255, 255, 230),
    );

    let id = ui.id().with(("root_header", path.clone()));
    let resp = ui.interact(rect, id, egui::Sense::click());

    if ui.rect_contains_pointer(rect) {
        *hover = Some(HoverInfo {
            name: node.name.clone(),
            size: node.size,
            hint: "双击返回上一级目录",
        });
    }

    if resp.double_clicked() && matches!(*action, TreeAction::None) {
        *action = TreeAction::EnterNode(path.clone());
    }
}

fn draw_children(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    node: &Node,
    path: &mut NodePath,
    depth: u32,
    selected: &Option<NodePath>,
    auto_select: Option<&NodePath>,
    action: &mut TreeAction,
    hover: &mut Option<HoverInfo>,
) {
    if node.children.is_empty() { return; }
    if rect.width() < 2.0 || rect.height() < 2.0 { return; }

    let sizes: Vec<u64> = node.children.iter().map(|c| c.size).collect();
    let rects = compute_treemap(&sizes, rect);

    // 如果这一层正是刚被"展开/跳转"的目标层，自动选中第一个真正被渲染出来的
    // 色块——不是数据里下标 0 的孩子，因为它可能因为面积太小而被跳过没画出来。
    if matches!(*action, TreeAction::None) {
        if let Some(target) = auto_select {
            if target.as_slice() == path.as_slice() {
                if let Some(i) = rects.iter().position(|r| r.width() >= MIN_RENDER_W && r.height() >= MIN_RENDER_H) {
                    let mut sel = path.clone();
                    sel.push(i);
                    *action = TreeAction::Select(sel);
                }
            }
        }
    }

    for (i, (r, child)) in rects.iter().zip(node.children.iter()).enumerate() {
        if r.width() < MIN_RENDER_W || r.height() < MIN_RENDER_H {
            continue;
        }

        path.push(i);

        let is_file = child.children.is_empty();
        let block_color = if is_file { FILE_COLOR } else { child.color };
        let is_selected = selected.as_deref() == Some(path.as_slice());

        let painter = ui.painter_at(*r);
        painter.rect_filled(*r, CornerRadius::same(2), block_color);
        let border_color = if is_file { FILE_BORDER } else { Color32::from_rgba_unmultiplied(0, 0, 0, 40) };
        painter.rect_stroke(*r, CornerRadius::same(2), Stroke::new(1.0, border_color), StrokeKind::Inside);
        if is_selected {
            painter.rect_stroke(*r, CornerRadius::same(2), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
        }

        let expanded = child.expanded;
        let can_inline_expand = !is_file
            && expanded
            && depth + 1 < MAX_DEPTH
            && r.width() > MIN_EXPAND_W
            && r.height() > MIN_EXPAND_H;

        draw_label(ui, &painter, *r, child);

        let id = ui.id().with(("block", path.clone()));
        let resp = ui.interact(*r, id, egui::Sense::click());

        // 只记录"当前悬停到的最深一层"是谁：同一像素点，父色块和它内部嵌套的
        // 子色块的矩形是重叠包含关系，这里先记父级、递归画子级时如果指针确实
        // 落在子矩形里会再覆盖一次——最终只剩下最深那一层的信息，帧末统一只画
        // 一个气泡，不会叠出两三个。
        if ui.rect_contains_pointer(*r) {
            let too_small_hint = !can_inline_expand && expanded && !is_file;
            *hover = Some(HoverInfo {
                name: child.name.clone(),
                size: child.size,
                hint: if is_file {
                    "文件 · 单击选中"
                } else if too_small_hint {
                    "文件夹 · 块太小，请双击进入"
                } else {
                    "文件夹 · 单击展开/收起 · 双击进入"
                },
            });
        }

        if can_inline_expand {
            let nested = Rect::from_min_max(
                Pos2::new(r.min.x, r.min.y + NEST_TOP),
                r.max,
            );
            if nested.width() > 4.0 && nested.height() > 4.0 {
                draw_children(ui, nested, child, path, depth + 1, selected, auto_select, action, hover);
            }
        }

        // 单击/双击处理（double_clicked 优先）。
        // 注意：如果这一帧已经被"自动选中第一个色块"占用了 action，
        // 这里的 `matches!(*action, TreeAction::None)` 会让真实点击让位——
        // 这种情况只会发生在"刚展开/跳转的那一帧"，概率极低，不影响正常操作。
        if resp.clicked() && matches!(*action, TreeAction::None) {
            if resp.double_clicked() {
                if !child.children.is_empty() {
                    *action = TreeAction::EnterNode(path.clone());
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

/// 整帧只调用一次：画出当前悬停目标的气泡，位置经过计算，
/// 保证气泡矩形不会盖住鼠标指针所在的那个点（否则用户想点别处会先点到气泡上）。
fn show_tooltip(ui: &egui::Ui, info: HoverInfo) {
    let mouse = ui.ctx().pointer_latest_pos().unwrap_or_default();
    let line1 = format!("{} · {}", info.name, human_size(info.size));
    let pos = tooltip_pos(ui, mouse, &line1, info.hint);

    egui::Area::new(ui.id().with("treemap_hover_tip"))
        .fixed_pos(pos)
        .order(egui::Order::Tooltip)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::default()
                .fill(Color32::from_rgb(0x2A, 0x2C, 0x32))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0x55, 0x55, 0x60)))
                .corner_radius(CornerRadius::same(5))
                .inner_margin(egui::Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.label(RichText::new(line1).color(Color32::WHITE));
                    ui.label(
                        RichText::new(info.hint)
                            .size(10.5)
                            .color(Color32::from_rgb(0xA0, 0xA0, 0xA0)),
                    );
                });
        });
}

/// 根据鼠标位置和屏幕可用范围，选一个"背对鼠标"的方向摆放气泡：
/// - 水平方向优先放右边；右边放不下（比如鼠标在屏幕右下角）就放左边。
/// - 垂直方向优先放上边；上边放不下（比如鼠标在屏幕顶部）就放下边。
/// 因为水平方向必定往鼠标 x 坐标的某一侧偏移至少 GAP 像素，
/// 气泡矩形的 x 范围永远不包含鼠标 x 坐标本身，所以不管垂直方向怎么选，
/// 气泡都不可能盖住鼠标指针所在的那个点。
fn tooltip_pos(ui: &egui::Ui, mouse: Pos2, line1: &str, line2: &str) -> Pos2 {
    let screen = ui.ctx().screen_rect();

    let measure = |s: &str, size: f32| -> f32 {
        let font = FontId::proportional(size);
        ui.ctx().fonts_mut(|f| f.layout_no_wrap(s.to_owned(), font, Color32::WHITE).size().x)
    };
    let w1 = measure(line1, 13.0);
    let w2 = measure(line2, 10.5);
    // 粗略估算气泡尺寸（含 padding），不需要像素级精确，够用来判断往哪边摆就行。
    let est_w = w1.max(w2) + 18.0;
    let est_h = 46.0;
    const GAP: f32 = 16.0;

    let place_right = mouse.x + GAP + est_w <= screen.right();
    let x = if place_right {
        mouse.x + GAP
    } else {
        (mouse.x - GAP - est_w).max(screen.left())
    };

    let place_above = mouse.y - GAP - est_h >= screen.top();
    let y = if place_above {
        mouse.y - GAP - est_h
    } else {
        mouse.y + GAP
    };

    Pos2::new(x, y)
}
