//! 文件列表树（全量列，中文表头，全部居中，heterogeneous_rows 虚拟化）。
//!
//! 列顺序：名称 | 父占比 | 总占比 | 逻辑大小 | 修改时间 | 物理大小 | 创建时间 | 访问时间
//!         | 项目 | 文件 | 文件夹 | 属性 | 重解析点 | 保留 | 所有者
//!
//! 性能：用 `TableBody::heterogeneous_rows()` 做真正的虚拟滚动——
//! egui_extras 源码确认 `row()` 不虚拟化（每帧渲染所有行），
//! 只有 `rows()` / `heterogeneous_rows()` 才跳过不可见行。
//! 磁盘行和子行合并到同一个 heterogeneous_rows 调用里。

use std::cell::Cell;
use egui::{Color32, Pos2, Rect, Sense, Vec2};
use crate::disk_info::DiskInfo;
use crate::format::{format_attributes, format_filetime_local as format_filetime, human_size, human_size_compact};
use crate::model::{Node, NodePath};
use super::{SortDir, SortKey, SortState, TreeAction};

const ROW_H: f32 = 22.0;
const DISK_ROW_H: f32 = 68.0;

/// 扁平化的可见行。磁盘行 + 子行统一处理。
#[derive(Clone)]
enum RowKind {
    Disk { pi: usize },
    Child { pi: usize, node: *const Node, abs_path: NodePath, indent: f32, depth: u32, parent_logical: u64 },
}

#[derive(Clone)]
struct FlatRow {
    height: f32,
    kind: RowKind,
}

/// 排序/展开状态 + 上一次算好的可见行缓存。
///
/// 背景：以前 children 是构建时排好序的（改一次、用到天荒地老），点表头排序后
/// 变成"每帧都要现算"，而 `collect_rows` 本来就是每帧都会重新走一遍整棵树
/// （因为要响应展开/折叠）。两者叠一起 = 每一帧、每一层可见节点都做一次
/// `sort_by`，稍微大一点的目录/大量重复文件时这个开销肉眼可见（debug 构建下
/// 尤其明显）。所以这里把算好的 `Vec<FlatRow>` 缓存起来，只有下面几种情况
/// 真正变化了才重算：排序变了、展开状态变了（`expand_version`）、
/// "显示全部信息"开关变了、或者分区数量/整个 `partitions` 数组的内存地址变了
/// （后者用来兜底探测"扫描线程往 self.partitions 里 push 了新分区，Vec 扩容
/// 导致所有元素搬家"这种会让缓存里的裸指针失效的情况——地址一变就强制重建，
/// 不会有 use-after-free 风险）。
///
/// `expand_version` 由 app.rs 在真正修改 `partitions` 树结构/展开状态的两处
/// （ToggleExpand 的处理分支、scan 完成 push 新分区）负责 +1；这里只管比较。
#[derive(Default)]
pub struct ListState {
    pub sort: SortState,
    pub expand_version: u64,
    cache: Option<(CacheKey, Vec<FlatRow>)>,
}

#[derive(PartialEq, Eq, Clone, Copy)]
struct CacheKey {
    sort: SortState,
    expand_version: u64,
    show_all: bool,
    partitions_len: usize,
    partitions_ptr: usize,
}

/// 大小写不敏感比较，不分配新 String（`to_lowercase()` 每次比较都要堆分配一次，
/// 排序是 O(n log n) 次比较，n 一大——比如上万个文件——每帧都这么分配一遍，
/// 就是列表变卡的主因之一）。
fn cmp_ignore_ascii_case(a: &str, b: &str) -> std::cmp::Ordering {
    a.bytes().map(|c| c.to_ascii_lowercase()).cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// 按当前排序状态给一层 children 排出显示顺序，返回的是 `children` 里的下标
/// （不改变 children 本身的存储顺序——那是 abs_path 依赖的"真实"下标，
/// 排序只影响遍历/显示时按什么顺序走这些下标）。
fn sorted_child_order(children: &[Node], sort: SortState) -> Vec<usize> {
    let mut order: Vec<usize> = (0..children.len()).collect();
    order.sort_by(|&a, &b| compare_nodes(&children[a], &children[b], sort.key));
    if sort.dir == SortDir::Desc { order.reverse(); }
    order
}

fn compare_nodes(a: &Node, b: &Node, key: SortKey) -> std::cmp::Ordering {
    match key {
        SortKey::Name => cmp_ignore_ascii_case(&a.name, &b.name),
        SortKey::Size => a.logical_size.cmp(&b.logical_size),
        SortKey::Physical => a.physical_size.cmp(&b.physical_size),
        SortKey::Modified => a.modified_ft.cmp(&b.modified_ft),
        SortKey::Created => a.created_ft.cmp(&b.created_ft),
        SortKey::Accessed => a.accessed_ft.cmp(&b.accessed_ft),
        SortKey::Items => (a.file_count + a.folder_count).cmp(&(b.file_count + b.folder_count)),
        SortKey::Files => a.file_count.cmp(&b.file_count),
        SortKey::Folders => a.folder_count.cmp(&b.folder_count),
        SortKey::Attributes => a.attributes.cmp(&b.attributes),
        SortKey::Reparse => a.reparse_tag.cmp(&b.reparse_tag),
        SortKey::Reserved => a.is_reserved.cmp(&b.is_reserved),
        SortKey::Owner => cmp_ignore_ascii_case(&a.owner, &b.owner),
    }
}

/// 表头文字对应的排序键：父占比/总占比两列本质上和逻辑大小同序，都映射到 `Size`。
const HEADER_COLS: [(&str, SortKey); 15] = [
    ("名称", SortKey::Name),
    ("父占比", SortKey::Size),
    ("总占比", SortKey::Size),
    ("逻辑大小", SortKey::Size),
    ("修改时间", SortKey::Modified),
    ("物理大小", SortKey::Physical),
    ("创建时间", SortKey::Created),
    ("访问时间", SortKey::Accessed),
    ("项目", SortKey::Items),
    ("文件", SortKey::Files),
    ("文件夹", SortKey::Folders),
    ("属性", SortKey::Attributes),
    ("重解析点", SortKey::Reparse),
    ("保留", SortKey::Reserved),
    ("所有者", SortKey::Owner),
];

/// 收集子行（含所有已展开的更深层级）。每一层按 `sort` 现算显示顺序
/// （不改变 `node.children` 的实际存储顺序，abs_path 里存的还是真实下标）。
/// `show_reserved`：为 false 时跳过 NTFS 保留的元数据文件（is_reserved，如 $MFT/$LogFile），
/// 对应"视图 > 显示全部信息"关闭的情况。
/// 迭代版本：用显式栈代替原生递归，栈里存"待处理节点 + 它的相对路径/深度/父 logical_size"，
/// 子节点按倒序入栈，保证出栈顺序（先序、从左到右）和排好的显示顺序完全一致。
fn collect_rows(
    node: &Node, pi: usize, rel_path: &mut Vec<usize>, depth: u32,
    parent_logical: u64, show_reserved: bool, sort: SortState, rows: &mut Vec<FlatRow>,
) {
    struct Item<'a> { node: &'a Node, rel_path: Vec<usize>, depth: u32, parent_logical: u64 }
    let order = sorted_child_order(&node.children, sort);
    let mut stack: Vec<Item> = order.into_iter().rev()
        .filter(|&i| show_reserved || !node.children[i].is_reserved)
        .map(|i| {
            let mut rp = rel_path.clone();
            rp.push(i);
            Item { node: &node.children[i], rel_path: rp, depth, parent_logical }
        }).collect();
    while let Some(item) = stack.pop() {
        let mut abs_path = vec![pi];
        abs_path.extend_from_slice(&item.rel_path);
        let indent = (item.depth + 1) as f32 * 16.0 + 2.0;
        rows.push(FlatRow {
            height: ROW_H,
            kind: RowKind::Child {
                pi,
                node: item.node as *const Node,
                abs_path,
                indent,
                depth: item.depth,
                parent_logical: item.parent_logical,
            },
        });
        if item.node.is_folder() && item.node.expanded {
            let child_parent_logical = item.node.logical_size.max(1);
            let child_order = sorted_child_order(&item.node.children, sort);
            for i in child_order.into_iter().rev()
                .filter(|&i| show_reserved || !item.node.children[i].is_reserved)
            {
                let mut rp = item.rel_path.clone();
                rp.push(i);
                stack.push(Item { node: &item.node.children[i], rel_path: rp, depth: item.depth + 1, parent_logical: child_parent_logical });
            }
        }
    }
}

pub fn show(
    ui: &mut egui::Ui,
    partitions: &[Node],
    partition_infos: &[Option<DiskInfo>],
    root_paths: &[String],
    selected: &Option<NodePath>,
    show_all: bool,
    state: &mut ListState,
) -> TreeAction {
    let action_cell: Cell<TreeAction> = Cell::new(TreeAction::None);
    // "关键列"始终保持正常宽度；非关键列在 show_all=false 时收缩到 0 宽度
    // （而不是真的减少 .column()/col() 调用次数）。egui_extras::TableBuilder 要求
    // 表头、每一行声明的列数必须严格一致，三处手写的列数只要有一处漏改就会在运行时
    // 出错/错位，且这里没有编译器能提前发现这种不匹配。用"宽度收缩到 0"来实现
    // "非关键列隐藏"，可以保证列数在任何开关状态下都完全不变，从根上排除这类风险。
    let extra_w = |normal: f32| if show_all { normal } else { 0.0 };
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        // 表格默认的 item_spacing 会在行与行、列与列之间留出几像素的间距——这段间距
        // 不属于任何一个单元格，我们手画的高亮背景、手动建的点击感应区都不会覆盖到它，
        // 于是就成了"看着是空白、点了没反应"的死区。这里直接把间距清零，行与行之间
        // 紧挨着，不会再有这种缝隙。
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
        let mut builder = egui_extras::TableBuilder::new(ui)
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .auto_shrink([false, false])
            .column(egui_extras::Column::initial(200.0).at_least(80.0).clip(true).resizable(true))  // 名称
            .column(egui_extras::Column::initial(85.0).clip(true).resizable(true))   // 父占比
            .column(egui_extras::Column::initial(85.0).clip(true).resizable(true))   // 总占比
            .column(egui_extras::Column::initial(85.0).clip(true).resizable(true))   // 逻辑大小
            .column(egui_extras::Column::initial(120.0).clip(true).resizable(true))  // 修改时间
            .column(egui_extras::Column::initial(85.0).clip(true).resizable(true))   // 物理大小
            .column(egui_extras::Column::initial(extra_w(120.0)).clip(true).resizable(show_all))  // 创建时间
            .column(egui_extras::Column::initial(extra_w(120.0)).clip(true).resizable(show_all))  // 访问时间
            .column(egui_extras::Column::initial(extra_w(55.0)).clip(true).resizable(show_all))   // 项目
            .column(egui_extras::Column::initial(extra_w(55.0)).clip(true).resizable(show_all))   // 文件
            .column(egui_extras::Column::initial(extra_w(55.0)).clip(true).resizable(show_all))   // 文件夹
            .column(egui_extras::Column::initial(extra_w(50.0)).clip(true).resizable(show_all))   // 属性
            .column(egui_extras::Column::initial(extra_w(55.0)).clip(true).resizable(show_all))   // 重解析点
            .column(egui_extras::Column::initial(extra_w(40.0)).clip(true).resizable(show_all))   // 保留
            .column(egui_extras::Column::initial(extra_w(80.0)).clip(true).resizable(show_all).at_least(0.0)); // 所有者

        builder = builder.sense(egui::Sense::click());
        let sort_clicked_cell: Cell<Option<SortKey>> = Cell::new(None);
        let table = builder.header(ROW_H, |mut h| {
            for (label, key) in HEADER_COLS {
                h.col(|ui| {
                    let active = state.sort.key == key;
                    let arrow = if active {
                        if state.sort.dir == SortDir::Asc { " ▲" } else { " ▼" }
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
        // 表头点击立刻生效（排序状态在渲染 body 之前就更新），不用等下一帧。
        if let Some(key) = sort_clicked_cell.get() { state.sort.click(key); }

        // 只有排序/展开/显示全部信息/分区数组真的变了才重新扫一遍树、重新排序；
        // 否则复用上一帧缓存的可见行列表，避免每一帧都对（可能很大的）子树做
        // sort_by。`partitions.as_ptr()` 兜底探测扫描线程往 `partitions` push
        // 新分区导致 Vec 扩容、所有元素搬家的情况——地址一变就强制重建，
        // 缓存里存的裸指针绝不会指向已经失效的内存。
        let cache_key = CacheKey {
            sort: state.sort,
            expand_version: state.expand_version,
            show_all,
            partitions_len: partitions.len(),
            partitions_ptr: partitions.as_ptr() as usize,
        };
        let need_rebuild = state.cache.as_ref().map_or(true, |(k, _)| *k != cache_key);
        if need_rebuild {
            let mut flat_rows: Vec<FlatRow> = Vec::new();
            let partition_order = sorted_child_order(partitions, state.sort);
            for pi in partition_order {
                let partition = &partitions[pi];
                flat_rows.push(FlatRow { height: DISK_ROW_H, kind: RowKind::Disk { pi } });
                if partition.expanded {
                    let mut rel_path: NodePath = Vec::new();
                    collect_rows(partition, pi, &mut rel_path, 0, partition.logical_size.max(1), show_all, state.sort, &mut flat_rows);
                }
            }
            state.cache = Some((cache_key, flat_rows));
        }
        let flat_rows = &state.cache.as_ref().unwrap().1;

        table
            .body(|body| {
                let mut final_action = TreeAction::None;

                // ── 用 heterogeneous_rows 做虚拟化渲染 ──
                let heights: Vec<f32> = flat_rows.iter().map(|r| r.height).collect();
                let clicked_row: Cell<usize> = Cell::new(usize::MAX);
                // 右键菜单点"删除到回收站"时，通过这个 Cell 把请求带出 body 闭包
                // （菜单是嵌在 row.col 更深一层的闭包里画的，够不着外层的 final_action）。
                let delete_request: Cell<Option<TreeAction>> = Cell::new(None);

                body.heterogeneous_rows(heights.into_iter(), |mut row| {
                    let row_idx = row.index();
                    if row_idx >= flat_rows.len() { return; }
                    let fr = &flat_rows[row_idx];

                    match &fr.kind {
                        RowKind::Disk { pi } => {
                            let partition = &partitions[*pi];
                            let info = partition_infos.get(*pi).and_then(|i| i.as_ref());
                            let part_selected = selected.as_deref() == Some(&[*pi]);
                            let total = info.map(|i| i.total_bytes).unwrap_or(partition.logical_size.max(1));
                            let part_pct = if total > 0 { partition.logical_size as f32 / total as f32 } else { 0.0 };
                            let p = partition;
                            let info_ref = info;
                            let root_path = root_paths.get(*pi).cloned().unwrap_or_default();

                            // 名称
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                if part_selected { ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }
                                let arrow = if p.expanded { "▼" } else { "▶" };
                                ui.painter().text(Pos2::new(rect.min.x+2.0, rect.min.y+6.0), egui::Align2::LEFT_TOP, arrow, egui::FontId::proportional(10.0), Color32::from_rgb(0xAA,0xCC,0xFF));
                                ui.painter().text(Pos2::new(rect.min.x+18.0, rect.min.y+4.0), egui::Align2::LEFT_TOP, format!("💾 {}", p.name), egui::FontId::proportional(13.0), if part_selected {Color32::from_rgb(0xFF,0xFF,0x80)} else {Color32::from_rgb(0xFF,0xD7,0x00)});
                                if let Some(i) = info_ref {
                                    ui.painter().text(Pos2::new(rect.min.x+18.0, rect.min.y+22.0), egui::Align2::LEFT_TOP, format!("总: {}  已用: {}  可用: {}", human_size_compact(i.total_bytes), human_size_compact(i.used_bytes), human_size_compact(i.free_bytes)), egui::FontId::proportional(10.0), Color32::from_rgb(0xA0,0xC0,0xE0));
                                    ui.painter().text(Pos2::new(rect.min.x+18.0, rect.min.y+36.0), egui::Align2::LEFT_TOP, format!("扫描: 逻辑={}  物理={}", human_size_compact(p.logical_size), human_size_compact(p.physical_size)), egui::FontId::proportional(10.0), Color32::from_rgb(0xA0,0xA0,0xA0));
                                }
                                if resp.clicked() || resp.secondary_clicked() { clicked_row.set(row_idx); }
                                resp.context_menu(|ui| context_menu_disk(ui, &root_path));
                            });
                            // 父占比
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } draw_bar(ui.painter(),r,1.0,Color32::from_rgb(0xFF,0xD7,0x00)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                            // 总占比
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } draw_bar(ui.painter(),r,part_pct,Color32::from_rgb(0x4C,0x8B,0xF5)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                            // 逻辑大小
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(p.logical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0x4C,0x8B,0xF5)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                            // 修改时间
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } let t=info_ref.map(|i|i.file_system.clone()).filter(|s|!s.is_empty()).unwrap_or_else(||format_filetime(p.modified_ft)); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(11.0),Color32::from_rgb(0xA0,0xC0,0xE0)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                            // 物理大小
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(p.physical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0xF5,0xA6,0x23)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                            // 创建时间
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } let s=format_filetime(p.created_ft); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                            // 访问时间
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } let s=format_filetime(p.accessed_ft); ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                            // 项目/文件/文件夹
                            for val in [p.file_count+p.folder_count, p.file_count, p.folder_count] {
                                row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format!("{}",val),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                            }
                            // 属性
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format_attributes(p.attributes),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                            // 重解析点
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } let t=if p.reparse_tag!=0 {format!("0x{:X}",p.reparse_tag)}else{"—".into()}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                            // 保留
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } let t=if p.is_reserved {"是"}else{"—"}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                            // 所有者
                            row.col(|ui| { let r=ui.available_rect_before_wrap(); let resp=ui.allocate_rect(r,Sense::click()); if part_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); } let t=if p.owner.is_empty(){"—".into()}else{p.owner.clone()}; ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0)); if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu_disk(ui, &root_path)); });
                        }
                        RowKind::Child { pi, node, abs_path, indent, depth, parent_logical } => {
                            let child = unsafe { &**node };
                            let is_folder = child.is_folder();
                            let is_selected = selected.as_deref() == Some(abs_path);
                            let disk_logical = partitions[*pi].logical_size.max(1);
                            let pct = if *parent_logical > 0 { child.logical_size as f32 / *parent_logical as f32 } else { 0.0 };
                            let total_pct = if disk_logical > 0 { child.logical_size as f32 / disk_logical as f32 } else { 0.0 };
                            let bar_color = depth_color(*depth, is_folder);
                            let c = child;
                            let full_path = build_full_path(partitions, root_paths, abs_path);
                            let hidden = c.is_hidden_or_system();

                            // 名称
                            row.col(|ui| {
                                let rect = ui.available_rect_before_wrap();
                                let resp = ui.allocate_rect(rect, Sense::click());
                                if hidden {
                                    // 隐藏/系统项：整个名称格淡橙色打底，一眼就能扫到，不需要盯着看图标
                                    ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0xF5, 0xA6, 0x23, 0x1C));
                                }
                                if is_selected { ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }
                                // 缩进参考线：每一层级画一条竖线贯穿整行，展开层级多的时候
                                // 能顺着线看清楚某一项到底属于哪一层，而不是只能数缩进空格数。
                                let guide_color = Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x14);
                                for lvl in 0..=*depth {
                                    let x = rect.min.x + lvl as f32 * 16.0 + 10.0;
                                    ui.painter().line_segment(
                                        [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                                        egui::Stroke::new(1.0, guide_color),
                                    );
                                }
                                let p = ui.painter();
                                if is_folder { p.text(Pos2::new(rect.min.x+indent,rect.center().y),egui::Align2::LEFT_CENTER,if c.expanded{"▼"}else{"▶"},egui::FontId::proportional(10.0),Color32::from_rgb(0xAA,0xCC,0xFF)); }
                                let icon = if is_folder {"📁"} else {"📄"};
                                // 名称文字颜色不再因为"隐藏"而整体淡化——那样会把文件夹（白）和
                                // 文件（浅灰）的区别也一起冲淡，反而更难分辨谁是谁。现在文字颜色
                                // 只由"是文件夹还是文件"决定，"隐藏"改用独立的橙色 H 徽标 + 上面
                                // 的行底色来标记，两条视觉线索互不干扰。
                                let tc = if is_selected {Color32::from_rgb(0xFF,0xFF,0x80)}
                                    else if is_folder {Color32::WHITE} else {Color32::from_rgb(0xCC,0xCC,0xCC)};
                                let mut text_x = rect.min.x + indent + 16.0;
                                if hidden {
                                    let badge = Rect::from_min_size(Pos2::new(text_x, rect.center().y - 7.0), Vec2::new(14.0, 14.0));
                                    p.rect_filled(badge, 3.0, Color32::from_rgb(0xF5, 0xA6, 0x23));
                                    p.text(badge.center(), egui::Align2::CENTER_CENTER, "H", egui::FontId::proportional(9.5), Color32::from_rgb(0x2A,0x2A,0x2E));
                                    text_x += 18.0;
                                }
                                p.text(Pos2::new(text_x,rect.center().y),egui::Align2::LEFT_CENTER,format!("{icon} {}",c.name),egui::FontId::proportional(13.0),tc);
                                if resp.clicked() || resp.secondary_clicked() {clicked_row.set(row_idx);}
                                resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request));
                            });
                            // 父占比
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }draw_bar(ui.painter(),r,pct,bar_color);if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                            // 总占比
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }draw_bar(ui.painter(),r,total_pct,Color32::from_rgb(0x4C,0x8B,0xF5));if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                            // 逻辑大小
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(c.logical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0x4C,0x8B,0xF5));if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                            // 修改时间
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }let s=format_filetime(c.modified_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                            // 物理大小
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,human_size(c.physical_size),egui::FontId::proportional(11.0),Color32::from_rgb(0xF5,0xA6,0x23));if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                            // 创建时间
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }let s=format_filetime(c.created_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                            // 访问时间
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }let s=format_filetime(c.accessed_ft);ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,if s.is_empty(){"—".into()}else{s},egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                            // 项目/文件/文件夹
                            for val in [if is_folder{c.file_count+c.folder_count}else{0}, if is_folder{c.file_count}else{0}, if is_folder{c.folder_count}else{0}] {
                                row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }let t=if is_folder{format!("{}",val)}else{"—".into()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                            }
                            // 属性
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,format_attributes(c.attributes),egui::FontId::proportional(11.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                            // 重解析点
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }let t=if c.reparse_tag!=0{format!("0x{:X}",c.reparse_tag)}else{"—".into()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                            // 保留
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }let t=if c.is_reserved{"是"}else{"—"};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                            // 所有者
                            row.col(|ui|{let r=ui.available_rect_before_wrap();let resp=ui.allocate_rect(r,Sense::click()); if is_selected { ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x4C,0x8B,0xF5,0x40)); }let t=if c.owner.is_empty(){"—".into()}else{c.owner.clone()};ui.painter().text(r.center(),egui::Align2::CENTER_CENTER,t,egui::FontId::proportional(10.0),Color32::from_rgb(0xC0,0xC0,0xC0));if resp.clicked()||resp.secondary_clicked(){clicked_row.set(row_idx);} resp.context_menu(|ui| context_menu(ui, is_folder, &c.name, &full_path, abs_path, &delete_request)); });
                        }
                    }
                });

                // 处理点击
                let clicked_idx = clicked_row.into_inner();
                if clicked_idx != usize::MAX && clicked_idx < flat_rows.len() {
                    let fr = &flat_rows[clicked_idx];
                    match &fr.kind {
                        RowKind::Disk { pi } => {
                            final_action = TreeAction::ToggleExpand(vec![*pi]);
                        }
                        RowKind::Child { node, abs_path, .. } => {
                            let child = unsafe { &**node };
                            let abs = abs_path.clone();
                            final_action = if child.is_folder() {
                                TreeAction::ToggleExpand(abs)
                            } else {
                                TreeAction::Select(abs)
                            };
                        }
                    }
                }
                let _ = &mut final_action;
                // 删除请求优先：正常行点击（左键选中/右键仅弹出菜单）不会同时设置
                // delete_request，两者不会真的抢——这里只是给"万一"兜个底。
                if let Some(action) = delete_request.into_inner() {
                    final_action = action;
                }
                action_cell.set(final_action);
            });
    });
    action_cell.into_inner()
}

fn draw_bar(painter: &egui::Painter, cell: Rect, pct: f32, color: Color32) {
    let pad = 4.0; let bar_h = 10.0;
    let bar_w = (cell.width() - pad*2.0).max(0.0);
    let br = Rect::from_min_size(Pos2::new(cell.min.x+pad, cell.center().y-bar_h/2.0), Vec2::new(bar_w, bar_h));
    painter.rect_filled(br, 2.0, Color32::from_rgb(0x48,0x48,0x52));
    let fill_w = (bar_w * pct.clamp(0.0,1.0)).max(0.0);
    if fill_w > 0.5 { painter.rect_filled(Rect::from_min_size(br.min, Vec2::new(fill_w, bar_h)), 2.0, color); }
    painter.text(br.center(), egui::Align2::CENTER_CENTER, format!("{:.2}%", pct*100.0), egui::FontId::proportional(9.5), Color32::WHITE);
}

fn depth_color(depth: u32, is_folder: bool) -> Color32 {
    if !is_folder { return Color32::from_rgb(0x6C,0x75,0x7D); }
    const PAL: [Color32;6] = [Color32::from_rgb(0x4C,0x8B,0xF5),Color32::from_rgb(0x34,0xC7,0x59),Color32::from_rgb(0xF5,0xA6,0x23),Color32::from_rgb(0xE0,0x55,0x5B),Color32::from_rgb(0x9C,0x6A,0xDE),Color32::from_rgb(0x2E,0xC4,0xB6)];
    PAL[depth as usize % PAL.len()]
}

/// 从 abs_path（第一个元素是分区下标，后面是逐层子节点下标）拼出完整文件系统路径。
/// 只在右键菜单打开的那一刻才调用（不是每帧都算），开销可以忽略。
fn build_full_path(partitions: &[Node], root_paths: &[String], abs_path: &[usize]) -> String {
    let Some(&pi) = abs_path.first() else { return String::new() };
    let mut path = root_paths.get(pi).cloned().unwrap_or_default().trim_end_matches('\\').to_string();
    let Some(mut cur) = partitions.get(pi) else { return path };
    for &i in &abs_path[1..] {
        let Some(n) = cur.children.get(i) else { break };
        cur = n;
        if path.is_empty() { path = cur.name.clone(); } else { path.push('\\'); path.push_str(&cur.name); }
    }
    // 分析视图（扩展名分类/重复文件查找）里的合成节点：它们在合成树里的位置和真实磁盘
    // 目录结构对不上，沿祖先名字拼出来的 path 是错的，有 full_path_override 就用它。
    if let Some(real) = &cur.full_path_override {
        return real.clone();
    }
    path
}

/// Windows 下打开资源管理器并选中某个文件/文件夹；`select_self` 为 true 时定位到这一项本身，
/// 否则是"打开这个文件夹"（用于文件的"打开所在文件夹"——选中文件本身，而不是钻进它内部，
/// 因为文件打不开"进入"）。
///
/// 路径必须整体带双引号：`explorer /select,C:\some path\file.txt`（不带引号）在路径带空格时
/// 会静默失败，退化成打开资源管理器的默认位置（很多机器上是"文档"），而不是报错或者什么都
/// 不做——这是 Windows 一个有据可查的老毛病，不是这边逻辑写错了。之前就是漏了这层引号，
/// 导致"有的文件用资源管理器打开会跳到 Documents"。
#[cfg(windows)]
/// 关键点：必须用 `raw_arg` 而不是普通的 `arg`。Rust 标准库的 `Command::arg()` 在
/// Windows 上会对参数里的引号做自己的转义（比如把 `"` 转成 `\"`），这是为了让参数能被
/// 标准的 C 运行时命令行解析器正确还原成"一个完整参数"。但 explorer.exe 对 `/select,`
/// 这种开关根本不走那套标准解析逻辑，它想看到的就是命令行里字面意义上的引号字符。
/// Rust 加了转义之后，explorer.exe 解析不出真正的路径，会静默失败、退化成打开默认位置
/// （很多机器上是"文档"）——这正是"用资源管理器打开却跳到 Documents"的真正原因。
fn open_in_explorer(path: &str, select_self: bool) {
    use std::os::windows::process::CommandExt;
    if path.is_empty() { return; }
    let (cmd_desc, result) = if select_self {
        let arg = format!("/select,\"{path}\"");
        let r = std::process::Command::new("explorer").raw_arg(&arg).spawn();
        (format!("explorer {arg}"), r)
    } else {
        let r = std::process::Command::new("explorer").arg(path).spawn();
        (format!("explorer {path}"), r)
    };
    crate::applog::log(&format!("[tree_list] 打开资源管理器: {cmd_desc}"));
    if let Err(e) = result {
        crate::applog::log(&format!("[tree_list] 打开资源管理器失败 ({path}): {e}"));
    }
}
#[cfg(not(windows))]
fn open_in_explorer(_path: &str, _select_self: bool) {}

/// 文件/文件夹右键菜单。
fn context_menu(
    ui: &mut egui::Ui, is_folder: bool, name: &str, full_path: &str,
    abs_path: &NodePath, delete_request: &Cell<Option<TreeAction>>,
) {
    ui.set_min_width(180.0);
    let open_label = if is_folder { "📂 在资源管理器中打开" } else { "📂 打开所在文件夹" };
    if ui.button(open_label).clicked() {
        open_in_explorer(full_path, !is_folder);
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
    // "属性" 直接调系统原生对话框，不用二次确认——纯只读操作。
    if ui.button("ℹ 属性").clicked() {
        crate::file_ops::open_properties(full_path);
        ui.close();
    }
    // "删除"只在这里发个请求（红色强调，提醒这是破坏性操作），真正执行前
    // app.rs 会弹一个确认框——回收站虽然能找回，但"点错就没了"这种事故
    // 不该一次点击就直接生效。
    if ui.add(egui::Button::new(egui::RichText::new("🗑 删除到回收站").color(Color32::from_rgb(0xE0, 0x60, 0x60)))).clicked() {
        delete_request.set(Some(TreeAction::RequestDelete {
            abs_path: abs_path.clone(),
            name: name.to_string(),
            full_path: full_path.to_string(),
            is_folder,
        }));
        ui.close();
    }
}

/// 磁盘/根目录行的右键菜单。删除到回收站不适用于整个分区/扫描根目录，这里不放；
/// "属性"看的是这个根目录本身（比如某个磁盘分区），一样直接调系统对话框。
fn context_menu_disk(ui: &mut egui::Ui, root_path: &str) {
    ui.set_min_width(180.0);
    if ui.button("📂 在资源管理器中打开").clicked() {
        open_in_explorer(root_path, false);
        ui.close();
    }
    if ui.button("📋 复制路径").clicked() {
        ui.ctx().copy_text(root_path.to_string());
        ui.close();
    }
    if ui.button("ℹ 属性").clicked() {
        crate::file_ops::open_properties(root_path);
        ui.close();
    }
    ui.separator();
    ui.add_enabled_ui(false, |ui| {
        let _ = ui.button("🔄 重新扫描（开发中）");
        let _ = ui.button("✖ 从列表移除（开发中）");
    });
}
