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
                // 枚举不到任何固定磁盘（极端情况，比如驱动异常）：不再假装存在一个 C 盘，
                // 用空占位代替，root_path 留空，逼用户自己在顶部输入/选择路径，
                // 而不是默默地对着一个不一定存在的 "C:\" 做无意义的展示。
                let demos = vec![Node::new_folder("(未检测到磁盘)", Color32::from_rgb(0x4C,0x8B,0xF5), Vec::new())];
                (demos, vec![None], String::new())
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
                // 拿不到真实磁盘容量（GetDiskFreeSpaceExW 失败，或压根没有分区信息）时，
                // 之前这里用 "已扫描逻辑大小 × 1.25" 瞎猜一个总容量，这个系数没有任何依据，
                // 猜出来的数字和真实容量可能差很远。现在改成诚实地把 total = used（已用/总=100%），
                // 明确告诉用户"这只是已扫描到的大小，真实磁盘总容量未知"，而不是编一个看似合理的数字。
                let used: u64 = self.partitions.iter().map(|p| p.logical_size).sum();
                (used.max(1), 0)
            });
        let used_size = self.partition_infos.first().and_then(|i|i.as_ref())
            .map(|i| i.used_bytes)
            .unwrap_or_else(|| self.partitions.iter().map(|p| p.logical_size).sum());

        let topbar_action = topbar::show(ui, TopbarState {
            root_path: &mut self.root_path, scanning: self.scanning, scanned_count: self.scanned_count,
            scan_error: self.scan_error.as_deref(), used_size, total_size,
            has_result: !self.partitions.is_empty() && self.partitions[0].file_count > 0,
            #[cfg(windows)]
            is_admin: crate::mft_scan::is_elevated(),
        });
        match topbar_action {
            TopbarAction::StartScan => self.start_scan(),
            TopbarAction::ExportCsv => self.export_csv(),
            #[cfg(windows)]
            TopbarAction::RestartAsAdmin => self.restart_as_admin(),
            TopbarAction::None => {}
            #[cfg(not(windows))]
            _ => {}
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
        let trimmed = self.root_path.trim();
        if trimmed.is_empty() {
            self.scan_error = Some("请先输入或选择要扫描的路径".into());
            return;
        }
        let path = PathBuf::from(trimmed);
        let (tx, rx) = mpsc::channel();
        scan::spawn_scan(path, tx);
        self.scan_rx = Some(rx); self.scanning = true; self.scanned_count = 0; self.scan_error = None;
    }

    #[cfg(windows)]
    fn restart_as_admin(&mut self) {
        crate::applog::log("[app] 用户请求以管理员身份重启");
        if let Some(exe) = std::env::current_exe().ok() {
            let exe_str = exe.to_string_lossy().to_string();
            let wide: Vec<u16> = exe_str.encode_utf16().chain(std::iter::once(0)).collect();
            let verb: Vec<u16> = "runas\0".encode_utf16().collect();
            // ShellExecuteW(hwnd, verb, file, params, dir, show_cmd)
            let ret = unsafe {
                windows_sys::Win32::UI::Shell::ShellExecuteW(
                    std::ptr::null_mut(),
                    verb.as_ptr(),
                    wide.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    1, // SW_SHOWNORMAL
                )
            };
            if ret as usize > 32 {
                // 成功启动了管理员实例，退出当前进程
                std::process::exit(0);
            } else {
                crate::applog::log("[app] 管理员重启失败，用户可能取消了 UAC 提示");
                self.scan_error = Some("管理员重启失败（用户可能取消了 UAC 提示）".into());
            }
        }
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
            .unwrap_or_else(|| std::env::temp_dir().join("disklens_export.csv"));
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
                        let now_expanded = if p.len() == 1 {
                            part.expanded = !part.expanded;
                            part.expanded
                        } else {
                            part.exclusive_toggle(&p[1..])
                        };
                        if now_expanded {
                            let root_path = self.root_path.clone();
                            if let Some((full_path, node)) = locate_for_owner(part, &root_path, &p[1..]) {
                                populate_owner_one_level(node, &full_path);
                            }
                        }
                    }
                }
                self.selected = Some(p);
            }
            TreeAction::EnterNode(p) => { self.selected = Some(p); }
        }
    }
}

/// 从分区根节点走到 `indices` 指向的节点，同时拼出它的完整文件系统路径。
/// `indices` 是 NodePath 去掉分区下标之后剩下的部分（分区根本身对应 indices 为空）。
fn locate_for_owner<'a>(root: &'a mut Node, root_path: &str, indices: &[usize]) -> Option<(String, &'a mut Node)> {
    let mut path = root_path.trim_end_matches('\\').to_string();
    let mut cur = root;
    for &i in indices {
        cur = cur.children.get_mut(i)?;
        if path.is_empty() { path = cur.name.clone(); } else { path.push('\\'); path.push_str(&cur.name); }
    }
    Some((path, cur))
}

/// 给 `node` 的直接子项懒加载所有者信息（只查一层，已经查过的跳过），
/// 只在用户点开一个文件夹时触发，不在扫描过程中调用，所以不会拖慢常规扫描/MFT 扫描本身。
/// 之前的问题：常规扫描完全没有所有者数据（一直是空字符串）；MFT 扫描虽然有 `populate_owners`，
/// 但只在扫描刚结束时跑一次，那时候所有节点 `expanded` 都还是 false，实际上只填得到
/// 根目录下第一层，往深了展开就再也没有所有者数据了。这里统一用"点开哪层就查哪层"的懒加载方式，
/// 两种扫描模式都能用，而且只查看得见的那几个条目，开销可以忽略。
#[cfg(windows)]
fn populate_owner_one_level(node: &mut Node, path: &str) {
    for child in &mut node.children {
        if !child.owner.is_empty() { continue; }
        let child_path = if path.is_empty() { child.name.clone() } else { format!("{path}\\{}", child.name) };
        child.owner = crate::mft_scan::get_owner(&child_path);
    }
}
#[cfg(not(windows))]
fn populate_owner_one_level(_node: &mut Node, _path: &str) {}
