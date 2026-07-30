//! 应用主状态。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use egui::{Color32, Vec2};
use crate::categorize::compute_categories;
use crate::disk_info::{self, DiskInfo};
use crate::export;
use crate::model::{Node, NodePath};
use crate::scan::{self, ScanMessage};
use crate::ui::topbar::{self, TopbarAction, TopbarState};
use crate::ui::{sidebar, tree_list, TreeAction};

pub struct DiskUiApp {
    root_path: String,
    partitions: Vec<Node>,
    partition_infos: Vec<Option<DiskInfo>>,
    selected: Option<NodePath>,
    categories: Vec<crate::model::CategoryStat>,
    scanning: bool,
    scanned_count: u64,
    scan_error: Option<String>,
    scan_rx: Option<Receiver<ScanMessage>>,
}

impl Default for DiskUiApp {
    fn default() -> Self {
        let all_drives = disk_info::enumerate_drives();
        let (partitions, partition_infos, root_path) =
            if let Some(c) = all_drives.iter().find(|d| d.drive_letter == 'C').cloned() {
                let placeholder = Node::new_folder_with_meta(c.display_name(), Color32::from_rgb(0x4C,0x8B,0xF5), Vec::new(), 0, 0, 0, 0x10, 0, false, String::new());
                (vec![placeholder], vec![Some(c.clone())], c.root_path())
            } else if let Some(first) = all_drives.first().cloned() {
                let placeholder = Node::new_folder_with_meta(first.display_name(), Color32::from_rgb(0x4C,0x8B,0xF5), Vec::new(), 0, 0, 0, 0x10, 0, false, String::new());
                (vec![placeholder], vec![Some(first.clone())], first.root_path())
            } else {
                let demos = vec![Node::new_folder("本地磁盘 (C:)", Color32::from_rgb(0x4C,0x8B,0xF5), Vec::new())];
                (demos, vec![None], r"C:\".into())
            };
        let categories = compute_categories(&partitions[0]);
        Self { root_path, partitions, partition_infos, selected: None, categories, scanning: false, scanned_count: 0, scan_error: None, scan_rx: None }
    }
}

impl eframe::App for DiskUiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.apply_theme(ui.ctx());
        self.poll_scan();
        let (total_size, _free) = self.partition_infos.first().and_then(|i|i.as_ref())
            .map(|i|(i.total_bytes, i.free_bytes))
            .unwrap_or_else(|| {
                let used: u64 = self.partitions.iter().map(|p| p.logical_size).sum();
                (((used as f64)*1.25).max(1.0) as u64, 0)
            });
        let used_size = self.partition_infos.first().and_then(|i|i.as_ref())
            .map(|i| i.used_bytes)
            .unwrap_or_else(|| self.partitions.iter().map(|p| p.logical_size).sum());

        let topbar_action = topbar::show(ui, TopbarState {
            root_path: &mut self.root_path, scanning: self.scanning, scanned_count: self.scanned_count,
            scan_error: self.scan_error.as_deref(), used_size, total_size,
            has_result: !self.partitions.is_empty() && self.partitions[0].file_count > 0,
        });
        match topbar_action {
            TopbarAction::StartScan => self.start_scan(),
            TopbarAction::ExportCsv => self.export_csv(),
            TopbarAction::None => {}
        }

        let p = self.partitions.first();
        sidebar::show(ui, used_size, total_size, total_size.saturating_sub(used_size), &self.categories,
            self.partition_infos.first().and_then(|i|i.as_ref()).map(|i|i.file_system.as_str()).unwrap_or(""),
            p.map(|p|p.logical_size).unwrap_or(0), p.map(|p|p.physical_size).unwrap_or(0),
            p.map(|p|p.file_count).unwrap_or(0), p.map(|p|p.folder_count).unwrap_or(0));

        let action = self.show_central(ui);
        self.apply_action(action);
        ui.ctx().request_repaint();
    }
}

impl DiskUiApp {
    fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_visuals_of(egui::Theme::Dark, egui::Visuals {
            window_fill: Color32::from_rgb(0x36,0x36,0x3A), panel_fill: Color32::from_rgb(0x36,0x36,0x3A), ..Default::default()
        });
        ctx.style_mut_of(egui::Theme::Dark, |s| {
            s.spacing.item_spacing = Vec2::new(10.0, 8.0);
            s.spacing.button_padding = Vec2::new(12.0, 6.0);
        });
    }
    fn start_scan(&mut self) {
        let path = PathBuf::from(self.root_path.trim());
        let (tx, rx) = mpsc::channel();
        scan::spawn_scan(path, tx);
        self.scan_rx = Some(rx); self.scanning = true; self.scanned_count = 0; self.scan_error = None;
    }
    fn export_csv(&mut self) {
        if self.partitions.is_empty() { return; }
        let root = &self.partitions[0];
        let root_path = self.partition_infos.first()
            .and_then(|i| i.as_ref())
            .map(|i| i.root_path())
            .unwrap_or_else(|| self.root_path.clone());
        let out = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("disklens_export.csv")))
            .unwrap_or_else(|| std::path::PathBuf::from("disklens_export.csv"));
        match export::export_tree_csv(root, &root_path, &out) {
            Ok((f, d)) => {
                crate::applog::log(&format!(
                    "[app] CSV 导出成功: {} (文件={}, 文件夹={})",
                    out.display(), f, d
                ));
                self.scan_error = Some(format!("CSV 已导出到: {}", out.display()));
            }
            Err(e) => {
                crate::applog::log(&format!("[app] CSV 导出失败: {e}"));
            }
        }
    }
    fn poll_scan(&mut self) {
        let Some(rx) = &self.scan_rx else { return };
        let mut finished = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ScanMessage::Progress(n) => self.scanned_count = n,
                ScanMessage::Done(node, info) => {
                    if self.partitions.is_empty() { self.partitions.push(*node); self.partition_infos.push(info); }
                    else { self.partitions[0] = *node; self.partition_infos[0] = info; }
                    self.categories = compute_categories(&self.partitions[0]);
                    self.selected = None; self.scanning = false; finished = true;
                }
                ScanMessage::Error(e) => { self.scan_error = Some(e); self.scanning = false; finished = true; }
            }
        }
        if finished { self.scan_rx = None; }
    }
    fn show_central(&self, ui: &mut egui::Ui) -> TreeAction {
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(Color32::from_rgb(0x36,0x36,0x3A)).inner_margin(egui::Margin::same(16)))
            .show(ui, |ui| { ui.add_space(4.0); tree_list::show(ui, &self.partitions, &self.partition_infos, &self.selected) })
            .inner
    }
    fn apply_action(&mut self, action: TreeAction) {
        match action {
            TreeAction::None => {}
            TreeAction::Select(p) => { self.selected = Some(p); }
            TreeAction::ToggleExpand(p) => {
                if let Some(&pi) = p.first() {
                    if let Some(part) = self.partitions.get_mut(pi) {
                        if p.len() == 1 { part.expanded = !part.expanded; }
                        else { part.exclusive_toggle(&p[1..]); }
                    }
                }
                self.selected = Some(p);
            }
            TreeAction::EnterNode(p) => { self.selected = Some(p); }
        }
    }
}
