//! 应用主状态 + 顶层编排。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use egui::{Color32, Vec2};

use crate::categorize::compute_categories;
use crate::disk_info::{self, DiskInfo};
use crate::model::{Node, NodePath};
use crate::scan::{self, ScanMessage};
use crate::ui::topbar::{self, TopbarAction, TopbarState};
use crate::ui::{sidebar, tree_list, TreeAction};

pub struct DiskUiApp {
    root_path: String,

    /// 所有磁盘分区，每个分区是独立的根节点。
    /// 多分区时列表顶层显示 C / D / E ... 各一行，用户展开后看子目录。
    partitions: Vec<Node>,

    /// 与 `partitions` 一一对应的磁盘元信息（卷标 / 总容量 / 可用空间）。
    /// `None` 表示没查到（非 Windows、子目录扫描、或 API 失败）。
    partition_infos: Vec<Option<DiskInfo>>,

    /// 系统里枚举到的所有分区。当前 UI 只展示 `partitions` 里的（默认只有 C），
    /// 以后做多盘选择时把这个列表显示给用户即可。
    all_drives: Vec<DiskInfo>,

    selected: Option<NodePath>,

    categories: Vec<crate::model::CategoryStat>,

    scanning: bool,
    scanned_count: u64,
    scan_error: Option<String>,
    scan_rx: Option<Receiver<ScanMessage>>,
}

impl Default for DiskUiApp {
    fn default() -> Self {
        eprintln!("[app] 初始化：枚举磁盘分区...");
        let all_drives = disk_info::enumerate_drives();
        eprintln!("[app] 枚举到 {} 个分区", all_drives.len());

        // 默认只显示 C 盘；找不到 C 盘就退回第一个，再找不到就退回 demo 数据。
        let (partitions, partition_infos, root_path) =
            if let Some(c) = all_drives.iter().find(|d| d.drive_letter == 'C').cloned() {
                eprintln!("[app] 默认显示 C 盘: {}", c.display_name());
                let placeholder = Node::new_folder_with_meta(
                    c.display_name(),
                    folder_color_for_drive(),
                    Vec::new(),
                    0,
                    0x10,
                );
                (vec![placeholder], vec![Some(c.clone())], c.root_path())
            } else if let Some(first) = all_drives.first().cloned() {
                eprintln!(
                    "[app] 没找到 C 盘，退回第一个分区: {}",
                    first.display_name()
                );
                let placeholder = Node::new_folder_with_meta(
                    first.display_name(),
                    folder_color_for_drive(),
                    Vec::new(),
                    0,
                    0x10,
                );
                (
                    vec![placeholder],
                    vec![Some(first.clone())],
                    first.root_path(),
                )
            } else {
                eprintln!("[app] 没枚举到任何分区，使用 demo 数据");
                let demos = scan::demo_partitions();
                let infos = demos.iter().map(|_| None).collect();
                let root = demos
                    .first()
                    .map(|_| r"C:\".to_string())
                    .unwrap_or_default();
                (demos, infos, root)
            };

        let categories = compute_categories_multi(&partitions);
        Self {
            root_path,
            partitions,
            partition_infos,
            all_drives,
            selected: None,
            categories,
            scanning: false,
            scanned_count: 0,
            scan_error: None,
            scan_rx: None,
        }
    }
}

fn folder_color_for_drive() -> Color32 {
    Color32::from_rgb(0x4C, 0x8B, 0xF5)
}

/// 合并所有分区的分类统计。
fn compute_categories_multi(partitions: &[Node]) -> Vec<crate::model::CategoryStat> {
    if partitions.is_empty() {
        return Vec::new();
    }
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

        // 真实磁盘容量：优先用 DiskInfo，没拿到时再退回到旧的"已用 * 1.25"估算。
        let (total_size, _free_size) = self
            .partition_infos
            .first()
            .and_then(|i| i.as_ref())
            .map(|i| (i.total_bytes, i.free_bytes))
            .unwrap_or_else(|| {
                let used: u64 = self.partitions.iter().map(|p| p.size).sum();
                let total = ((used as f64) * 1.25).max(1.0) as u64;
                (total, total.saturating_sub(used))
            });
        // 已用空间：优先用系统报告的 used_bytes（更准），没拿到时退回扫描汇总。
        let used_size = self
            .partition_infos
            .first()
            .and_then(|i| i.as_ref())
            .map(|i| i.used_bytes)
            .unwrap_or_else(|| self.partitions.iter().map(|p| p.size).sum());

        let topbar_action = topbar::show(
            ui,
            TopbarState {
                root_path: &mut self.root_path,
                scanning: self.scanning,
                scanned_count: self.scanned_count,
                scan_error: self.scan_error.as_deref(),
                used_size,
                total_size,
            },
        );
        if matches!(topbar_action, TopbarAction::StartScan) {
            self.start_scan();
        }

        sidebar::show(
            ui,
            used_size,
            total_size,
            total_size.saturating_sub(used_size),
            &self.categories,
            self.partition_infos
                .first()
                .and_then(|i| i.as_ref())
                .map(|i| i.file_system.as_str())
                .unwrap_or(""),
        );

        let action = self.show_central_panel(ui);
        self.apply_action(action);

        ui.ctx().request_repaint();
    }
}

impl DiskUiApp {
    fn apply_dark_theme(&self, ctx: &egui::Context) {
        ctx.set_visuals_of(egui::Theme::Dark, egui::Visuals {
            window_fill: Color32::from_rgb(0x36, 0x36, 0x3A),
            panel_fill: Color32::from_rgb(0x36, 0x36, 0x3A),
            ..Default::default()
        });
        ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.spacing.item_spacing = Vec2::new(10.0, 8.0);
            style.spacing.button_padding = Vec2::new(12.0, 6.0);
            style.interaction.tooltip_delay = 0.05;
        });
    }

    fn start_scan(&mut self) {
        let path = PathBuf::from(self.root_path.trim());
        let (tx, rx) = mpsc::channel();
        eprintln!("[app] 用户点击扫描，启动后台线程: root={}", path.display());
        scan::spawn_scan(path, tx);
        self.scan_rx = Some(rx);
        self.scanning = true;
        self.scanned_count = 0;
        self.scan_error = None;
    }

    fn poll_scan(&mut self) {
        let Some(rx) = &self.scan_rx else {
            return;
        };
        let mut finished = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ScanMessage::Progress(n) => self.scanned_count = n,
                ScanMessage::Done(node, disk_info) => {
                    // 扫描单个路径时，替换第一个分区（或唯一分区）
                    if self.partitions.is_empty() {
                        self.partitions.push(*node);
                        self.partition_infos.push(disk_info);
                    } else {
                        self.partitions[0] = *node;
                        self.partition_infos[0] = disk_info;
                    }
                    self.categories = compute_categories_multi(&self.partitions);
                    self.selected = None;
                    self.scanning = false;
                    finished = true;
                    eprintln!("[app] 扫描完成，已更新 partitions[0]");
                }
                ScanMessage::Error(e) => {
                    self.scan_error = Some(e);
                    self.scanning = false;
                    finished = true;
                    eprintln!("[app] 扫描报错: {}", self.scan_error.as_deref().unwrap_or(""));
                }
            }
        }
        if finished {
            self.scan_rx = None;
        }
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
                tree_list::show(ui, &self.partitions, &self.partition_infos, &self.selected)
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
