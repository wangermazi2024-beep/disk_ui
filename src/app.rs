//! 应用主状态 + 顶层编排。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use egui::{Color32, Vec2};

use crate::categorize::compute_categories;
use crate::model::{Node, NodePath};
use crate::scan::{self, ScanMessage};
use crate::ui::topbar::{self, TopbarAction, TopbarState};
use crate::ui::{sidebar, tree_list, TreeAction};

pub struct DiskUiApp {
    root_path: String,

    partitions: Vec<Node>,

    selected: Option<NodePath>,

    categories: Vec<crate::model::CategoryStat>,

    scanning: bool,
    scanned_count: u64,
    scan_error: Option<String>,
    scan_rx: Option<Receiver<ScanMessage>>,

    /// 实际磁盘总容量（从 GetDiskFreeSpaceExW 获取）
    disk_total: u64,
    /// 实际磁盘剩余空间
    disk_free: u64,
}

impl Default for DiskUiApp {
    fn default() -> Self {
        let partitions = scan::demo_partitions();
        let categories = compute_categories_multi(&partitions);
        Self {
            root_path: r"C:\".into(),
            partitions,
            selected: None,
            categories,
            scanning: false,
            scanned_count: 0,
            scan_error: None,
            scan_rx: None,
            disk_total: 0,
            disk_free: 0,
        }
    }
}

/// 合并所有分区的分类统计。
fn compute_categories_multi(partitions: &[Node]) -> Vec<crate::model::CategoryStat> {
    if partitions.is_empty() { return Vec::new(); }
    // 用第一个分区做基础，后续分区累加 size
    let mut stats = compute_categories(&partitions[0]);
    for p in &partitions[1..] {
        let more = compute_categories(p);
        for (s, m) in stats.iter_mut().zip(more.iter()) {
            s.size += m.size;
        }
    }
    stats
}

impl eframe::App for DiskUiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.apply_dark_theme(ui.ctx());
        self.poll_scan();

        let total_used: u64 = self.partitions.iter().map(|p| p.size).sum();
        // 尝试获取真实磁盘容量
        #[cfg(windows)]
        if self.disk_total == 0 {
            if let Some((total, free)) = crate::mft_scan::get_disk_space('C') {
                self.disk_total = total;
                self.disk_free = free;
            }
        }
        let total_size = if self.disk_total > 0 { self.disk_total } else { ((total_used as f64) * 1.25).max(1.0) as u64 };

        let topbar_action = topbar::show(
            ui,
            TopbarState {
                root_path: &mut self.root_path,
                scanning: self.scanning,
                scanned_count: self.scanned_count,
                scan_error: self.scan_error.as_deref(),
                used_size: total_used,
                total_size,
            },
        );
        if matches!(topbar_action, TopbarAction::StartScan) {
            self.start_scan();
        }

        sidebar::show(ui, total_used, total_size,
            if self.disk_free > 0 { self.disk_free } else { total_size.saturating_sub(total_used) }, &self.categories);

        let action = self.show_central_panel(ui);
        self.apply_action(action);

        ui.ctx().request_repaint();
    }
}

impl DiskUiApp {
    fn apply_dark_theme(&self, ctx: &egui::Context) {
        ctx.set_visuals_of(egui::Theme::Dark, egui::Visuals {
            window_fill: Color32::from_rgb(0x36, 0x36, 0x3A),
            panel_fill:  Color32::from_rgb(0x36, 0x36, 0x3A),
            ..Default::default()
        });
        ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.spacing.item_spacing   = Vec2::new(10.0, 8.0);
            style.spacing.button_padding = Vec2::new(12.0, 6.0);
            style.interaction.tooltip_delay = 0.05;
        });
    }

    fn start_scan(&mut self) {
        let path = PathBuf::from(self.root_path.trim());
        let (tx, rx) = mpsc::channel();
        scan::spawn_scan(path, tx);
        self.scan_rx   = Some(rx);
        self.scanning  = true;
        self.scanned_count = 0;
        self.scan_error    = None;
    }

    fn poll_scan(&mut self) {
        let Some(rx) = &self.scan_rx else { return };
        let mut finished = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ScanMessage::Progress(n) => self.scanned_count = n,
                ScanMessage::Done(node) => {
                    // 扫描单个路径时，替换第一个分区（或唯一分区）
                    if self.partitions.is_empty() {
                        self.partitions.push(*node);
                    } else {
                        self.partitions[0] = *node;
                    }
                    self.categories = compute_categories_multi(&self.partitions);
                    self.selected   = None;
                    self.scanning   = false;
                    finished = true;
                }
                ScanMessage::Error(e) => {
                    self.scan_error = Some(e);
                    self.scanning   = false;
                    finished = true;
                }
            }
        }
        if finished { self.scan_rx = None; }
    }

    fn show_central_panel(&self, ui: &mut egui::Ui) -> TreeAction {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(0x36, 0x36, 0x3A))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ui, |ui| {
                ui.add_space(4.0);
                tree_list::show(ui, &self.partitions, &self.selected, self.disk_total, self.disk_free)
            })
            .inner
    }

    fn apply_action(&mut self, action: TreeAction) {
        match action {
            TreeAction::None => {}

            TreeAction::Select(path) => {
                self.selected = Some(path);
            }

            TreeAction::ToggleExpand(path) => {
                // path[0] = 分区索引，path[1..] = 分区内相对路径
                if let Some(&pi) = path.first() {
                    if let Some(partition) = self.partitions.get_mut(pi) {
                        if path.len() == 1 {
                            // 点击分区根节点行
                            partition.expanded = !partition.expanded;
                        } else {
                            // 点击分区内的子节点
                            partition.exclusive_toggle(&path[1..]);
                        }
                    }
                }
                self.selected = Some(path);
            }

            TreeAction::EnterNode(path) => {
                self.selected = Some(path);
            }

            TreeAction::NavigateTo(path) => {
                self.selected = None;
                let _ = path; // 当前版本 tree_list 不发送此动作
            }
        }
    }
}
