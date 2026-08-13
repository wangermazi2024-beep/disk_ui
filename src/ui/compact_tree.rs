//! "分析视图"专用的树状列表（文件扩展名分类 / 重复文件查找共用）。
//!
//! 和主列表（tree_list.rs）视觉语言一致——能展开、有缩进参考线、隐藏项有徽标、
//! 右键菜单能复制路径/打开所在文件夹——但列不一样：主列表那 15 列里，父占比/总占比/
//! 所有者/重解析点/保留 这些在"按扩展名/大小重新分组"的场景里没有意义（文件不是挂在
//! 真实目录树里的），去掉换成一列"路径"，让用户知道具体是哪个文件、在哪，
//! 不然只看文件名根本不知道要删的是哪一个。
//!
//! 没有直接在 tree_list.rs 里加一个"精简列模式"开关，是刻意的：tree_list.rs 那 15 列
//! 的表头声明和每一行的渲染必须严格一一对应，再加一套完全不同的列会让"这次改了列数、
//! 那次漏改"的风险成倍增加，独立开一个文件、各自的列各自维护，互不影响。

use std::cell::Cell;
use egui::{Color32, Pos2, Sense};
use crate::format::{format_filetime_local, human_size};
use crate::model::{Node, NodePath};
use crate::ui::TreeAction;

const ROW_H: f32 = 24.0;

/// 分析视图（扩展名分类/重复文件查找）可排序的字段。列比主列表少很多——只有
/// 名称/大小/修改时间/路径，单独定义一套，不复用主列表 `ui::SortKey`
/// （字段集合不一样，硬凑共用类型只会让两边都变得别扭）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    fn toggled(self) -> Self {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }
}

/// 当前排序状态。默认按大小降序——和这两个分析视图原来的构建顺序
/// （categorize.rs 里已经按大小分组排好）保持一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub key: SortKey,
    pub dir: SortDir,
}

impl Default for SortState {
    fn default() -> Self {
        Self { key: SortKey::Size, dir: SortDir::Desc }
    }
}

impl SortState {
    pub fn click(&mut self, key: SortKey) {
        if self.key == key {
            self.dir = self.dir.toggled();
        } else {
            self.key = key;
            self.dir = SortDir::Desc;
        }
    }
}

fn compare_nodes(a: &Node, b: &Node, key: SortKey) -> std::cmp::Ordering {
    match key {
        SortKey::Name => cmp_ignore_ascii_case(&a.name, &b.name),
        SortKey::Size => a.logical_size.cmp(&b.logical_size),
        SortKey::Modified => a.modified_ft.cmp(&b.modified_ft),
        // 分组行（扩展名/大小类目）没有 full_path_override，路径排序时统一当空串，
        // 分组会在路径排序下聚成一堆并列——这一列本来就是给"真实文件"用的，
        // 分组行点这一列排不出什么意义在意料之中。
        SortKey::Path => {
            let pa = a.full_path_override.as_deref().unwrap_or("");
            let pb = b.full_path_override.as_deref().unwrap_or("");
            cmp_ignore_ascii_case(pa, pb)
        }
    }
}

/// 大小写不敏感比较，不分配新 String——原来用 `to_lowercase()` 每次比较都堆分配一次，
/// 排序 O(n log n) 次比较、外加每帧都要重排（见下面 `ViewState` 的说明），
/// 扩展名分类/重复文件这种单个分组能有几千上万个文件的场景下，就是明显卡顿的元凶。
fn cmp_ignore_ascii_case(a: &str, b: &str) -> std::cmp::Ordering {
    a.bytes().map(|c| c.to_ascii_lowercase()).cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// 按当前排序状态给一层 children 排出显示顺序，返回下标（不改变存储顺序，
/// 道理和 tree_list.rs 里的同名函数一样：abs_path 依赖"真实"下标）。
fn sorted_child_order(children: &[Node], sort: SortState) -> Vec<usize> {
    let mut order: Vec<usize> = (0..children.len()).collect();
    order.sort_by(|&a, &b| compare_nodes(&children[a], &children[b], sort.key));
    if sort.dir == SortDir::Desc { order.reverse(); }
    order
}

#[derive(Clone)]
struct FlatRow {
    node: *const Node,
    abs_path: NodePath,
    depth: u32,
    is_group: bool, // 分组文件夹（扩展名/大小组）还是真实文件
}

/// 排序/展开状态 + 上一次算好的可见行缓存。
///
/// `root` 这棵合成树在标签页打开时构建一次，之后只有 `.expanded` 这个 bool
/// 会被原地翻转（`Node::exclusive_toggle`，不涉及任何 Vec 重新分配），
/// 树的其余部分和内存地址终生不变，所以缓存里存的裸指针放心跨帧复用，
/// 没有 tree_list.rs 那边"扫描线程还在长树"的顾虑。
///
/// 只有排序或展开状态真的变了（`expand_version`，由 app.rs 在处理
/// `TreeAction::ToggleExpand` 时 +1）才重新走一遍 `collect_rows`，
/// 否则复用上一帧的 `Vec<FlatRow>`——这才是这次真正要修的地方：以前每一帧
/// 都对每一层可见节点重新 `sort_by` 一次，一个分组几千个文件、
/// 每秒 60 帧地排，明显卡。
#[derive(Default)]
pub struct ViewState {
    pub sort: SortState,
    pub expand_version: u64,
    cache: Option<(SortState, u64, Vec<FlatRow>)>,
}

const HEADER_COLS: [(&str, SortKey); 4] = [
    ("名称", SortKey::Name),
    ("大小", SortKey::Size),
    ("修改时间", SortKey::Modified),
    ("路径", SortKey::Path),
];

/// `root`：合成树的根（它自己不显示，只显示它的直接子项——扩展名/大小分组）。
pub fn show(ui: &mut egui::Ui, root: &Node, selected: &Option<NodePath>, view: &mut ViewState) -> TreeAction {
    let action_cell: Cell<TreeAction> = Cell::new(TreeAction::None);

    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        // 名称 | 大小 | 修改时间 | 路径（放最后，用 remainder，避免中间夹在两个固定宽度列
        // 之间时缺一条可视的拖拽分隔线，看起来不像两个独立的列）。
        // 表格默认的 item_spacing 会在行之间留出几像素间距，手画的高亮/点击感应区都盖不到，
        // 就成了"点了没反应"的死区，这里清零，行与行紧挨着。
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
        let builder = egui_extras::TableBuilder::new(ui)
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .sense(egui::Sense::click())
            .column(egui_extras::Column::initial(280.0).at_least(120.0).clip(true).resizable(true)) // 名称
            .column(egui_extras::Column::initial(90.0).clip(true).resizable(true)) // 大小
            .column(egui_extras::Column::initial(130.0).clip(true).resizable(true)) // 修改时间
            .column(egui_extras::Column::remainder().at_least(150.0).clip(true).resizable(true)); // 路径

        let sort_clicked_cell: Cell<Option<SortKey>> = Cell::new(None);
        let table = builder
            .header(ROW_H, |mut h| {
                for (label, key) in HEADER_COLS {
                    h.col(|ui| {
                        let active = view.sort.key == key;
                        let arrow = if active {
                            if view.sort.dir == SortDir::Asc { " ▲" } else { " ▼" }
                        } else { "" };
                        let color = if active { Color32::from_rgb(0xFF, 0xD7, 0x00) } else { Color32::WHITE };
                        let text = egui::RichText::new(format!("{label}{arrow}")).strong().size(12.0).color(color);
                        let resp = ui.add(egui::Label::new(text).sense(Sense::click()));
                        let resp = resp.on_hover_text("点击排序，再次点击切换升/降序");
                        if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                        if resp.clicked() { sort_clicked_cell.set(Some(key)); }
                    });
                }
            });
        // 表头点击立刻生效（渲染表体之前先更新排序状态、重算行顺序），不用等下一帧。
        if let Some(key) = sort_clicked_cell.get() { view.sort.click(key); }

        // 只有排序或展开状态真的变了才重新收集/排序一遍，否则复用上一帧缓存
        // 的可见行（见 ViewState 上的说明——这是这次真正要修的性能问题）。
        let need_rebuild = view.cache.as_ref()
            .map_or(true, |(s, v, _)| *s != view.sort || *v != view.expand_version);
        if need_rebuild {
            let mut flat_rows: Vec<FlatRow> = Vec::new();
            let rel_path: NodePath = vec![0]; // abs_path[0] 固定占位（保持和主列表一样的 [分区下标, ...] 形状）
            collect_rows(root, &rel_path, view.sort, &mut flat_rows);
            view.cache = Some((view.sort, view.expand_version, flat_rows));
        }
        let flat_rows = &view.cache.as_ref().unwrap().2;

        table
            .body(|body| {
                let clicked_row: Cell<Option<usize>> = Cell::new(None);
                // 右键菜单点"删除到回收站"时用来把请求带出这层闭包，道理和 tree_list.rs
                // 里的同名 Cell 一样。
                let delete_request: Cell<Option<TreeAction>> = Cell::new(None);
                body.rows(ROW_H, flat_rows.len(), |mut row| {
                    let row_idx = row.index();
                    let fr = &flat_rows[row_idx];
                    let n: &Node = unsafe { &*fr.node };
                    let is_selected = selected.as_ref() == Some(&fr.abs_path);
                    let hidden = n.is_hidden_or_system();
                    let indent = fr.depth as f32 * 16.0 + 4.0;
                    let full_path = n.full_path_override.clone().unwrap_or_default();
                    let is_group = fr.is_group;

                    // 每一列都要能点击（选中/右键菜单），不是只有名称那一列能点——
                    // 统一在每个 row.col() 开头画背景（选中蓝底/隐藏淡橙底）+ 建立点击感应区，
                    // 四列各自画一段，视觉上连起来就是一整行的高亮，而不是只有名称那一小块。
                    let paint_bg_and_sense = |ui: &mut egui::Ui| -> (egui::Rect, egui::Response) {
                        let rect = ui.available_rect_before_wrap();
                        let resp = ui.allocate_rect(rect, Sense::click());
                        if hidden {
                            ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0xF5, 0xA6, 0x23, 0x1C));
                        }
                        if is_selected {
                            ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0x4C, 0x8B, 0xF5, 0x40));
                        }
                        (rect, resp)
                    };
                    let handle_click = |resp: &egui::Response| {
                        if resp.clicked() || resp.secondary_clicked() { clicked_row.set(Some(row_idx)); }
                    };

                    // 名称
                    row.col(|ui| {
                        let (rect, resp) = paint_bg_and_sense(ui);
                        let guide_color = Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x14);
                        for lvl in 0..fr.depth {
                            let x = rect.min.x + lvl as f32 * 16.0 + 10.0;
                            ui.painter().line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], egui::Stroke::new(1.0, guide_color));
                        }
                        let p = ui.painter();
                        if is_group {
                            p.text(Pos2::new(rect.min.x + indent, rect.center().y), egui::Align2::LEFT_CENTER,
                                if n.expanded { "▼" } else { "▶" }, egui::FontId::proportional(10.0), Color32::from_rgb(0xAA, 0xCC, 0xFF));
                        }
                        let icon = if is_group { "🗀" } else { "📄" };
                        let tc = if is_selected { Color32::from_rgb(0xFF, 0xFF, 0x80) }
                            else if is_group { Color32::WHITE } else { Color32::from_rgb(0xCC, 0xCC, 0xCC) };
                        let mut text_x = rect.min.x + indent + 16.0;
                        if hidden {
                            let badge = egui::Rect::from_min_size(Pos2::new(text_x, rect.center().y - 7.0), egui::vec2(14.0, 14.0));
                            p.rect_filled(badge, 3.0, Color32::from_rgb(0xF5, 0xA6, 0x23));
                            p.text(badge.center(), egui::Align2::CENTER_CENTER, "H", egui::FontId::proportional(9.5), Color32::from_rgb(0x2A, 0x2A, 0x2E));
                            text_x += 18.0;
                        }
                        p.text(Pos2::new(text_x, rect.center().y), egui::Align2::LEFT_CENTER, format!("{icon} {}", n.name), egui::FontId::proportional(13.0), tc);
                        handle_click(&resp);
                        if !is_group { resp.context_menu(|ui| context_menu(ui, &n.name, &full_path, &fr.abs_path, &delete_request)); }
                    });
                    // 大小
                    row.col(|ui| {
                        let (rect, resp) = paint_bg_and_sense(ui);
                        ui.painter().text(rect.left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER,
                            human_size(n.logical_size), egui::FontId::proportional(12.0), Color32::from_rgb(0xD0, 0xD0, 0xD0));
                        handle_click(&resp);
                        if !is_group { resp.context_menu(|ui| context_menu(ui, &n.name, &full_path, &fr.abs_path, &delete_request)); }
                    });
                    // 修改时间
                    row.col(|ui| {
                        let (rect, resp) = paint_bg_and_sense(ui);
                        if !is_group {
                            ui.painter().text(rect.left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER,
                                format_filetime_local(n.modified_ft), egui::FontId::proportional(11.0), Color32::from_rgb(0xC0, 0xC0, 0xC0));
                        }
                        handle_click(&resp);
                        if !is_group { resp.context_menu(|ui| context_menu(ui, &n.name, &full_path, &fr.abs_path, &delete_request)); }
                    });
                    // 路径（只有真实文件才有意义，分组行留空）
                    row.col(|ui| {
                        let (rect, resp) = paint_bg_and_sense(ui);
                        if !is_group {
                            let dir = full_path.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
                            ui.painter().text(rect.left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER,
                                dir, egui::FontId::proportional(11.5), Color32::from_rgb(0xA0, 0xA0, 0xA0));
                        }
                        handle_click(&resp);
                        if !is_group { resp.context_menu(|ui| context_menu(ui, &n.name, &full_path, &fr.abs_path, &delete_request)); }
                    });
                });
                if let Some(idx) = clicked_row.get() {
                    let p = flat_rows[idx].abs_path.clone();
                    if flat_rows[idx].is_group {
                        action_cell.set(TreeAction::ToggleExpand(p));
                    } else {
                        action_cell.set(TreeAction::Select(p));
                    }
                }
                // 删除请求优先于普通行点击——道理和 tree_list.rs 一样，正常情况下
                // 二者不会同时触发，这里只是兜底。
                if let Some(action) = delete_request.into_inner() {
                    action_cell.set(action);
                }
            });
    });

    action_cell.into_inner()
}

/// 只有两层：合成根的直接子项（分组）+ 分组展开后的文件。迭代式，栈存相对路径。
/// 每一层都按 `sort` 现算显示顺序（分组之间排一次，每个展开分组内部的文件各排一次）。
fn collect_rows(root: &Node, rel_path: &NodePath, sort: SortState, rows: &mut Vec<FlatRow>) {
    struct Item<'a> { node: &'a Node, abs_path: NodePath, depth: u32, is_group: bool }
    let order = sorted_child_order(&root.children, sort);
    let mut stack: Vec<Item> = order.into_iter().rev().map(|i| {
        let mut p = rel_path.clone();
        p.push(i);
        Item { node: &root.children[i], abs_path: p, depth: 0, is_group: true }
    }).collect();
    while let Some(item) = stack.pop() {
        rows.push(FlatRow { node: item.node as *const Node, abs_path: item.abs_path.clone(), depth: item.depth, is_group: item.is_group });
        if item.is_group && item.node.expanded {
            let child_order = sorted_child_order(&item.node.children, sort);
            for i in child_order.into_iter().rev() {
                let mut p = item.abs_path.clone();
                p.push(i);
                stack.push(Item { node: &item.node.children[i], abs_path: p, depth: item.depth + 1, is_group: false });
            }
        }
    }
}

/// Windows 下打开资源管理器并选中某个文件。
///
/// 关键点：必须用 `raw_arg` 而不是普通的 `arg`。Rust 标准库的 `Command::arg()` 在
/// Windows 上会对参数里的引号做自己的转义（比如把 `"` 转成 `\"`），这是为了让参数能被
/// 标准的 C 运行时命令行解析器正确还原成"一个完整参数"。但 explorer.exe 对 `/select,`
/// 这种开关根本不走那套标准解析逻辑，它想看到的就是命令行里字面意义上的引号字符。
/// Rust 加了转义之后，explorer.exe 解析不出真正的路径，会静默失败、退化成打开默认位置
/// （很多机器上是"文档"）——这正是"用资源管理器打开却跳到 Documents"的真正原因，
/// 不是我们拼的路径本身有问题（日志和命令行手动跑都是对的，只是 Rust 在发给
/// explorer.exe 之前，偷偷改了一遍我们没让它改的字符）。`raw_arg` 会原样追加文本，
/// 不做任何转义，这样 explorer.exe 收到的命令行就和你在 cmd.exe 里手动敲的完全一样。
#[cfg(windows)]
fn open_in_explorer_select(path: &str) {
    if path.is_empty() { return; }
    use std::os::windows::process::CommandExt;
    let arg = format!("/select,\"{path}\"");
    crate::applog::log(&format!("[compact_tree] 打开资源管理器: explorer {arg}"));
    if let Err(e) = std::process::Command::new("explorer").raw_arg(&arg).spawn() {
        crate::applog::log(&format!("[compact_tree] 打开资源管理器失败 ({path}): {e}"));
    }
}
#[cfg(not(windows))]
fn open_in_explorer_select(_path: &str) {}

fn context_menu(ui: &mut egui::Ui, name: &str, full_path: &str, abs_path: &NodePath, delete_request: &Cell<Option<TreeAction>>) {
    ui.set_min_width(180.0);
    if ui.button("📂 打开所在文件夹").clicked() {
        open_in_explorer_select(full_path);
        ui.close();
    }
    if ui.button("📋 复制路径").clicked() {
        ui.ctx().copy_text(full_path.to_string());
        ui.close();
    }
    if ui.button("📋 复制名称").clicked() {
        ui.ctx().copy_text(name.to_string());
        ui.close();
    }
    ui.separator();
    if ui.button("ℹ 属性").clicked() {
        crate::file_ops::open_properties(full_path);
        ui.close();
    }
    // 这里列出来的都是真实文件（分组行不会调这个菜单），所以 is_folder 恒为 false。
    if ui.add(egui::Button::new(egui::RichText::new("🗑 删除到回收站").color(Color32::from_rgb(0xE0, 0x60, 0x60)))).clicked() {
        delete_request.set(Some(TreeAction::RequestDelete {
            abs_path: abs_path.clone(),
            name: name.to_string(),
            full_path: full_path.to_string(),
            is_folder: false,
        }));
        ui.close();
    }
    ui.separator();
    ui.add_enabled_ui(false, |ui| {
        let _ = ui.button("🔗 创建符号链接（开发中）");
    });
}
