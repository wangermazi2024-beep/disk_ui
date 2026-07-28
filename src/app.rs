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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_dark_theme(ctx);
        self.poll_scan();

        let topbar_action = topbar::show(
            ctx,
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

        sidebar::show(ctx, self.root.size, self.total_size, self.total_size.saturating_sub(self.root.size), &self.categories);

        let action = self.show_central_panel(ctx);
        self.apply_action(action);

        ctx.request_repaint();
    }
}

impl DiskUiApp {
    fn apply_dark_theme(&self, ctx: &egui::Context) {
        // 从 egui 内置的暗色主题起步，而不是零散地强制文字颜色。
        // 之前用 override_text_color 只对"没有显式设色"的文字生效，
        // .strong() 这类样式会绕过它，导致暗底配深色字看不清。
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = Color32::from_rgb(0x1E, 0x1F, 0x22);
        visuals.panel_fill = Color32::from_rgb(0x1E, 0x1F, 0x22);
        let mut style = (*ctx.style()).clone();
        style.visuals = visuals;
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.interaction.tooltip_delay = 0.05; // 即时显示气泡（默认 0.5s 太慢）
        ctx.set_style(style);
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
    fn show_central_panel(&self, ctx: &egui::Context) -> TreeAction {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(0x1E, 0x1F, 0x22))
                    .inner_margin(egui::Margin::same(16.0)),
            )
            .show(ctx, |ui| {
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

    /// 顶部面包屑（一行截断）：`根目录 / 子目录 / ...`，路径太长时自动截断。
    /// 点击任意一段跳转回对应层级。
    fn breadcrumb_ui(&self, ui: &mut egui::Ui) -> Option<TreeAction> {
        let mut clicked: Option<NodePath> = None;

        if !self.zoom_path.is_empty() {
            if ui.small_button("⬆ 上一级").clicked() {
                let mut p = self.zoom_path.clone();
                p.pop();
                clicked = Some(p);
            }
        }

        // 构建完整路径文本
        let mut path_parts: Vec<&str> = vec![&self.root.name];
        let mut cursor = &self.root;
        let mut prefix = Vec::new();
        for &i in &self.zoom_path {
            let Some(child) = cursor.children.get(i) else { break };
            prefix.push(i);
            path_parts.push(&child.name);
            cursor = child;
        }
        // 路径太长时中间截断：保留首尾
        let max_segments = 4;
        let path_text = if path_parts.len() > max_segments {
            let head = path_parts[0];
            let tail: Vec<&&str> = path_parts[path_parts.len()-2..].iter().collect();
            format!("{} / … / {}", head, tail.join(" / "))
        } else {
            path_parts.join(" / ")
        };

        ui.horizontal(|ui| {
            ui.set_max_width(ui.available_width() - 10.0);
            // 根节点可点击
            if ui.selectable_label(self.zoom_path.is_empty(), RichText::new(&self.root.name).size(12.5)).clicked() {
                clicked = Some(Vec::new());
            }
            let mut cursor = &self.root;
            let mut prefix = Vec::new();
            for &i in &self.zoom_path {
                let Some(child) = cursor.children.get(i) else { break };
                prefix.push(i);
                ui.label(RichText::new(" / ").color(Color32::from_rgb(0x70, 0x70, 0x70)).size(12.5));
                let is_current = prefix == self.zoom_path;
                if ui.selectable_label(is_current, RichText::new(&child.name).size(12.5)).clicked() {
                    clicked = Some(prefix.clone());
                }
                cursor = child;
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
                if let Some(node) = self.root.navigate_mut(&path) {
                    node.expanded = !node.expanded;
                }
                self.selected = Some(path);
            }
            TreeAction::ZoomTo(path) => {
                self.zoom_path = path.clone();
                // 放大到新层级时重置该路径上所有节点的 inline 展开状态，
                // 避免之前手动展开的子块在新视图里继续显示嵌套色块。
                if let Some(n) = self.root.navigate_mut(&path) {
                    n.collapse_all();
                }
                self.selected = Some(path);
            }
        }
    }
}
