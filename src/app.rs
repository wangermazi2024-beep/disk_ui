//! 应用主状态 + 顶层编排。
//!
//! 这一层只做"粘合"：拿各个子模块（treemap_view/tree_list/topbar/sidebar）
//! 产生的动作，统一更新状态；不在这里画细节 UI，也不在这里放算法逻辑，
//! 方便以后单独替换某一块（比如换一种色块渲染方式）而不动其它模块。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use egui::{Color32, RichText, Vec2};

use crate::categorize::compute_categories;
use crate::model::{Node, NodePath};
use crate::scan::{self, ScanMessage};
use crate::ui::topbar::{self, TopbarAction, TopbarState};
use crate::ui::{sidebar, tree_list, treemap_view, TreeAction};

pub struct DiskUiApp {
    root_path: String,
    /// 模拟的磁盘总容量。真实的"剩余空间"需要平台相关的 API（statvfs / GetDiskFreeSpaceEx），
    /// 为了不引入额外的平台依赖，这里按已用空间估算一个总容量用于展示比例条，
    /// 并在界面上如实叫它"演示值"，不假装是真实磁盘剩余空间。
    total_size: u64,

    root: Node,
    /// 当前 treemap "放大"显示到的节点路径（双击导航的结果），空路径代表显示根节点。
    zoom_path: NodePath,
    /// 当前选中节点，treemap 色块和文件列表树共用同一个选中状态实现联动高亮。
    selected: Option<NodePath>,

    categories: Vec<crate::model::CategoryStat>,

    scanning: bool,
    scanned_count: u64,
    scan_error: Option<String>,
    scan_rx: Option<Receiver<ScanMessage>>,
}

impl Default for DiskUiApp {
    fn default() -> Self {
        let root = scan::demo_tree();
        let categories = compute_categories(&root);
        let total_size = estimate_total(root.size);
        Self {
            root_path: r"C:\".into(),
            total_size,
            root,
            zoom_path: Vec::new(),
            selected: None,
            categories,
            scanning: false,
            scanned_count: 0,
            scan_error: None,
            scan_rx: None,
        }
    }
}

/// 演示用的"总容量"估算：留出大约 20% 余量，只是为了让概览条看起来合理，
/// 不代表真实磁盘容量。
fn estimate_total(used: u64) -> u64 {
    ((used as f64) * 1.25).max(1.0) as u64
}

impl eframe::App for DiskUiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.apply_dark_theme(ui.ctx());
        self.poll_scan();

        let topbar_action = topbar::show(
            ui,
            TopbarState {
                root_path: &mut self.root_path,
                scanning: self.scanning,
                scanned_count: self.scanned_count,
                scan_error: self.scan_error.as_deref(),
                used_size: self.root.size,
                total_size: self.total_size,
            },
        );
        if matches!(topbar_action, TopbarAction::StartScan) {
            self.start_scan();
        }

        sidebar::show(ui, self.root.size, self.total_size, self.total_size.saturating_sub(self.root.size), &self.categories);

        let action = self.show_central_panel(ui);
        self.apply_action(action);

        ui.ctx().request_repaint();
    }
}

impl DiskUiApp {
    fn apply_dark_theme(&self, ctx: &egui::Context) {
        // 从 egui 内置的暗色主题起步，而不是零散地强制文字颜色。
        // 之前用 override_text_color 只对"没有显式设色"的文字生效，
        // .strong() 这类样式会绕过它，导致暗底配深色字看不清。
        ctx.set_visuals_of(egui::Theme::Dark, egui::Visuals {
            window_fill: Color32::from_rgb(0x1E, 0x1F, 0x22),
            panel_fill: Color32::from_rgb(0x1E, 0x1F, 0x22),
            ..Default::default()
        });
        ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.spacing.item_spacing = Vec2::new(10.0, 8.0);
            style.spacing.button_padding = Vec2::new(12.0, 6.0);
            style.interaction.tooltip_delay = 0.05; // 即时显示气泡（默认 0.5s 太慢）
        });
    }

    fn start_scan(&mut self) {
        let path = PathBuf::from(self.root_path.trim());
        let (tx, rx) = mpsc::channel();
        scan::spawn_scan(path, tx);
        self.scan_rx = Some(rx);
        self.scanning = true;
        self.scanned_count = 0;
        self.scan_error = None;
    }

    fn poll_scan(&mut self) {
        let Some(rx) = &self.scan_rx else { return };
        let mut finished = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ScanMessage::Progress(n) => self.scanned_count = n,
                ScanMessage::Done(node) => {
                    self.root = *node;
                    self.categories = compute_categories(&self.root);
                    self.total_size = estimate_total(self.root.size);
                    self.zoom_path.clear();
                    self.selected = None;
                    self.scanning = false;
                    finished = true;
                }
                ScanMessage::Error(e) => {
                    self.scan_error = Some(e);
                    self.scanning = false;
                    finished = true;
                }
            }
        }
        if finished {
            self.scan_rx = None;
        }
    }

    /// 中央面板：面包屑导航 + treemap 色块 + 递归文件列表树。
    /// 所有交互统一收敛成一个 `TreeAction`，画完之后再统一应用到状态上，
    /// 避免在 egui 的画图闭包内部同时持有 `self` 的多处可变借用。
    fn show_central_panel(&self, ui: &mut egui::Ui) -> TreeAction {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(0x1E, 0x1F, 0x22))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ui, |ui| {
                let mut action = TreeAction::None;

                // 当前层名称（一行截断显示）
                let cur_name = self.root.navigate(&self.zoom_path)
                    .map(|n| n.name.as_str()).unwrap_or(&self.root.name);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("📂 {}", cur_name)).strong().size(15.0));
                    ui.add_space(10.0);
                    if let Some(a) = self.breadcrumb_ui(ui) {
                        action = a;
                    }
                });
                ui.add_space(8.0);

                let total_h = ui.available_height();
                let treemap_h = (total_h * 0.5).clamp(220.0, 420.0);
                let (rect, _resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), treemap_h), egui::Sense::hover());

                let view_root = self.root.navigate(&self.zoom_path).unwrap_or(&self.root);
                let tm_action = treemap_view::show(ui, rect, view_root, &self.zoom_path, &self.selected);
                action.merge(tm_action);

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                ui.label(RichText::new("文件列表").strong().size(14.0));
                ui.add_space(4.0);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let list_action = tree_list::show(ui, &self.root, &[], &self.selected);
                    action.merge(list_action);
                });
                action
            })
            .inner
    }

    /// 顶部面包屑（可点击路径）：`C: / Windows / System32 / drivers`。
    ///
    /// 截断算法（Middle Ellipsis）：
    /// - 始终保留最左一段（根）和最右一段（当前层）。
    /// - 从左往右贪心地加入中间段，直到剩余宽度不足以再加下一段 + " / … / " + 当前层。
    /// - 被省略的中间段统一用不可点击的 "…" 替代。
    /// - 这样用户随时能看到"我在哪"和"从哪来"，也能点击根或当前层的父级回溯。
    fn breadcrumb_ui(&self, ui: &mut egui::Ui) -> Option<TreeAction> {
        let mut clicked: Option<NodePath> = None;

        if !self.zoom_path.is_empty() {
            if ui.small_button("⬆ 上一级").clicked() {
                let mut p = self.zoom_path.clone();
                p.pop();
                clicked = Some(p);
            }
        }

        // 收集所有路径段
        struct Seg {
            name: String,
            path: Vec<usize>,
        }
        let mut segs = vec![Seg { name: self.root.name.clone(), path: Vec::new() }];
        let mut cursor = &self.root;
        let mut prefix = Vec::new();
        for &i in &self.zoom_path {
            let Some(child) = cursor.children.get(i) else { break };
            prefix.push(i);
            segs.push(Seg { name: child.name.clone(), path: prefix.clone() });
            cursor = child;
        }

        // 用 egui 测量字符串像素宽度
        let measure_str = |s: &str| -> f32 {
            let font = egui::FontId::proportional(12.5);
            ui.ctx().fonts_mut(|f| f.layout_no_wrap(s.to_owned(), font, Color32::WHITE).size().x)
        };
        let sep_w = measure_str(" / ");
        let ellipsis_w = measure_str("…");

        // 可用宽度（留 8px 安全边距）
        let avail_w = (ui.available_width() - 8.0).max(60.0);

        // Middle-Ellipsis 算法（用下标避免 lifetime 问题）：
        // - 始终显示首段（index 0）和尾段（index n-1）
        // - 中间段从左向右贪心填入，装不下就用 None 表示省略
        // slot = Some(seg_index) 表示显示该段，None 表示"…"
        let slots: Vec<Option<usize>> = if segs.len() <= 2 {
            (0..segs.len()).map(Some).collect()
        } else {
            let first_w = measure_str(&segs[0].name);
            let last_w  = measure_str(&segs[segs.len() - 1].name);
            // 最少空间：first + sep + … + sep + last
            let min_w = first_w + sep_w + ellipsis_w + sep_w + last_w;
            let mut remaining = avail_w - min_w;

            let mut middle: Vec<Option<usize>> = Vec::new();
            let mut need_ellipsis = false;
            for idx in 1..segs.len() - 1 {
                let cost = sep_w + measure_str(&segs[idx].name);
                if remaining >= cost {
                    middle.push(Some(idx));
                    remaining -= cost;
                } else {
                    need_ellipsis = true;
                    break;
                }
            }

            let mut result = vec![Some(0_usize)];
            result.extend(middle);
            if need_ellipsis {
                result.push(None); // "…"
            }
            result.push(Some(segs.len() - 1));
            result
        };

        ui.horizontal(|ui| {
            let total = slots.len();
            for (slot_idx, slot) in slots.iter().enumerate() {
                if slot_idx > 0 {
                    ui.label(RichText::new(" / ").color(Color32::from_rgb(0x65, 0x65, 0x70)).size(12.5));
                }
                match slot {
                    None => {
                        ui.label(RichText::new("…").color(Color32::from_rgb(0x65, 0x65, 0x70)).size(12.5));
                    }
                    Some(seg_idx) => {
                        let seg = &segs[*seg_idx];
                        let is_last = slot_idx == total - 1;
                        let label = RichText::new(&seg.name).size(12.5);
                        if ui.selectable_label(is_last, label).clicked() && !is_last {
                            clicked = Some(seg.path.clone());
                        }
                    }
                }
            }
        });

        clicked.map(TreeAction::ZoomTo)
    }

    fn apply_action(&mut self, action: TreeAction) {
        match action {
            TreeAction::None => {}
            TreeAction::Select(path) => {
                self.selected = Some(path);
            }
            TreeAction::ToggleExpand(path) => {
                // SpaceSniffer 独占展开：
                // path 是绝对路径（从 root 出发），zoom_path 是当前视图根。
                // exclusive_toggle 需要「相对于视图根」的路径，这样才能
                // 把同层兄弟节点的展开状态一并清理。
                //
                // 例如：zoom_path=[0]，path=[0,2,1]
                // → 相对路径 = [2,1]，视图根节点 = root.children[0]
                // exclusive_toggle 在 root.children[0].children[2] 这一层
                // 折叠所有兄弟，只展开 [2]，再递归进去。
                if path.starts_with(&self.zoom_path) {
                    let rel = &path[self.zoom_path.len()..];
                    if let Some(view_root) = self.root.navigate_mut(&self.zoom_path) {
                        view_root.exclusive_toggle(rel);
                    }
                }
                self.selected = Some(path);
            }
            TreeAction::ZoomTo(path) => {
                self.zoom_path = path.clone();
                // 清理整棵树所有节点的 inline 展开状态：
                // 用户导航到新层级（无论是双击 ZoomTo 还是面包屑跳转），
                // 希望看到干净视图，不保留之前在其他层展开的残留子块。
                self.root.collapse_all();
                self.selected = Some(path);
            }
        }
    }
}
