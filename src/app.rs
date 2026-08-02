//! 应用主状态：启动选盘 → 顺序批量扫描 → 结果树 + 菜单栏。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use egui::{Color32, RichText};

use crate::categorize::compute_categories;
use crate::disk_info::{self, DiskInfo};
use crate::export;
use crate::model::{Node, NodePath};
use crate::scan::{self, ScanMessage};
use crate::ui::topbar::{self, TopbarAction, TopbarState};
use crate::ui::{startup, tree_list, TreeAction};

/// 界面所处的大阶段。
enum AppMode {
    /// 首次启动：占满整个窗口，让用户选分区/目录，点"开始扫描"之前不查询任何数据。
    Startup(startup::PickerState),
    /// 已经有扫描结果（或正在扫描），显示菜单栏 + 结果列表。
    Main,
    /// 在 Main 基础上用户点了"添加扫描…"，重新显示一次选择界面（可以取消回到 Main）。
    AddingScan(startup::PickerState),
}

pub struct DiskUiApp {
    mode: AppMode,
    partitions: Vec<Node>,
    partition_infos: Vec<Option<DiskInfo>>,
    /// 和 `partitions`/`partition_infos` 一一对应：这个分区/目录当初是从哪个路径扫的，
    /// 展开文件夹时懒加载所有者要用到。
    partition_root_paths: Vec<String>,
    selected: Option<NodePath>,

    scanning: bool,
    scanned_count: u64,
    scan_error: Option<String>,
    scan_rx: Option<Receiver<ScanMessage>>,
    /// 用户一次选了多个分区/目录时，排队按顺序一个个扫，扫完一个再扫下一个。
    scan_queue: VecDeque<PathBuf>,
    current_scan_path: Option<PathBuf>,

    /// 视图 > 显示全部信息：开=全部列 + 元数据文件；关=只留关键列、隐藏元数据文件。
    show_all_details: bool,
}

impl Default for DiskUiApp {
    fn default() -> Self {
        let drives = disk_info::list_fixed_drive_letters();
        Self {
            mode: AppMode::Startup(startup::PickerState::new(drives)),
            partitions: Vec::new(),
            partition_infos: Vec::new(),
            partition_root_paths: Vec::new(),
            selected: None,
            scanning: false,
            scanned_count: 0,
            scan_error: None,
            scan_rx: None,
            scan_queue: VecDeque::new(),
            current_scan_path: None,
            show_all_details: true,
        }
    }
}

impl eframe::App for DiskUiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.apply_theme(ui.ctx());
        self.poll_scan();

        match &self.mode {
            AppMode::Startup(_) | AppMode::AddingScan(_) => self.show_picker_screen(ui),
            AppMode::Main => self.show_main_screen(ui),
        }
        ui.ctx().request_repaint();
    }
}

impl DiskUiApp {
    fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_visuals(egui::Visuals::dark());
    }

    /// 首次启动界面 / "添加扫描" 界面，两者共用同一套渲染逻辑，只是要不要"取消"按钮不同。
    fn show_picker_screen(&mut self, ui: &mut egui::Ui) {
        let show_cancel = matches!(self.mode, AppMode::AddingScan(_));
        // 把 picker 状态从 self.mode 里换出来单独处理，用完再放回去，
        // 避免闭包里同时借用 self 的好几个字段让借用检查器为难。
        let mut picker = match std::mem::replace(&mut self.mode, AppMode::Main) {
            AppMode::Startup(p) => p,
            AppMode::AddingScan(p) => p,
            AppMode::Main => return, // 不会走到这，这个函数只在 Startup/AddingScan 时被调用
        };
        let mut picker_action = startup::PickerAction::None;
        egui::CentralPanel::default()
            .frame(egui::Frame::default()
                .fill(Color32::from_rgb(0x36, 0x36, 0x3A))
                .inner_margin(egui::Margin::same(24)))
            .show(ui, |ui| {
                picker_action = startup::show(ui, &mut picker, show_cancel);
            });
        match picker_action {
            startup::PickerAction::Confirm => {
                let paths = build_scan_paths(&picker);
                self.mode = AppMode::Main;
                self.start_scan_batch(paths);
            }
            startup::PickerAction::Cancel => {
                self.mode = AppMode::Main;
            }
            startup::PickerAction::None => {
                self.mode = if show_cancel { AppMode::AddingScan(picker) } else { AppMode::Startup(picker) };
            }
        }
    }

    fn show_main_screen(&mut self, ui: &mut egui::Ui) {
        let action = topbar::show(ui, TopbarState {
            scanning: self.scanning,
            scanned_count: self.scanned_count,
            scan_error: self.scan_error.as_deref(),
            has_result: !self.partitions.is_empty(),
            show_all_details: self.show_all_details,
            #[cfg(windows)]
            is_admin: crate::mft_scan::is_elevated(),
        });
        match action {
            TopbarAction::AddScan => {
                let drives = disk_info::list_fixed_drive_letters();
                self.mode = AppMode::AddingScan(startup::PickerState::new(drives));
            }
            TopbarAction::ExportCsv => self.export_csv(),
            TopbarAction::ToggleShowAll => self.show_all_details = !self.show_all_details,
            #[cfg(windows)]
            TopbarAction::RestartAsAdmin => self.restart_as_admin(),
            TopbarAction::None => {}
        }

        self.show_branding_bar(ui);

        let tree_action = egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(Color32::from_rgb(0x24, 0x24, 0x28)).inner_margin(egui::Margin::same(4)))
            .show(ui, |ui| {
                tree_list::show(ui, &self.partitions, &self.partition_infos, &self.selected, self.show_all_details)
            })
            .inner;
        self.apply_tree_action(tree_action);
    }

    /// 窗口左下角的小品牌条：软件名 + 开发者，不再占用顶部菜单栏的空间。
    fn show_branding_bar(&self, ui: &mut egui::Ui) {
        egui::Panel::bottom("branding_bar")
            .exact_size(22.0)
            .frame(egui::Frame::default().fill(Color32::from_rgb(0x2E, 0x2E, 0x32)).inner_margin(egui::Margin::symmetric(10, 3)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⛁ DiskForge WMS").size(11.0).color(Color32::from_rgb(0x6F, 0xA8, 0xFF)));
                    ui.label(RichText::new("· 由 WMS 开发").size(10.0).color(Color32::from_rgb(0x80, 0x80, 0x80)));
                });
            });
    }

    fn start_scan_batch(&mut self, paths: Vec<PathBuf>) {
        self.scan_queue.extend(paths);
        if !self.scanning {
            self.dequeue_next_scan();
        }
    }

    fn dequeue_next_scan(&mut self) {
        if let Some(path) = self.scan_queue.pop_front() {
            crate::applog::log(&format!("[app] 开始扫描: {}", path.display()));
            let (tx, rx) = mpsc::channel();
            scan::spawn_scan(path.clone(), tx);
            self.current_scan_path = Some(path);
            self.scan_rx = Some(rx);
            self.scanning = true;
            self.scanned_count = 0;
            self.scan_error = None;
        }
    }

    fn poll_scan(&mut self) {
        let Some(rx) = &self.scan_rx else { return };
        let mut finished = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ScanMessage::Progress(n) => self.scanned_count = n,
                ScanMessage::Done(node, info) => {
                    let node = *node;
                    log_scan_summary(&node, info.as_ref());
                    let path = self.current_scan_path.take()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.partitions.push(node);
                    self.partition_infos.push(info);
                    self.partition_root_paths.push(path);
                    self.selected = None;
                    finished = true;
                }
                ScanMessage::Error(e) => {
                    crate::applog::log(&format!("[app] 扫描失败: {e}"));
                    self.scan_error = Some(e);
                    self.current_scan_path = None;
                    finished = true;
                }
            }
        }
        if finished {
            self.scan_rx = None;
            self.scanning = false;
            self.dequeue_next_scan();
        }
    }

    fn apply_tree_action(&mut self, action: TreeAction) {
        match action {
            TreeAction::None => {}
            TreeAction::Select(p) => { self.selected = Some(p); }
            TreeAction::ToggleExpand(p) => {
                if let Some(&pi) = p.first() {
                    let root_path = self.partition_root_paths.get(pi).cloned();
                    if let (Some(part), Some(root_path)) = (self.partitions.get_mut(pi), root_path) {
                        let now_expanded = if p.len() == 1 {
                            part.expanded = !part.expanded;
                            part.expanded
                        } else {
                            part.exclusive_toggle(&p[1..])
                        };
                        if now_expanded {
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

    /// 导出全部已扫描的分区/目录，每个各自一个 CSV 文件。
    fn export_csv(&mut self) {
        if self.partitions.is_empty() { return; }
        let dir = std::env::current_exe().ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(std::env::temp_dir);
        let mut ok_count = 0usize;
        for (i, root) in self.partitions.iter().enumerate() {
            let root_path = self.partition_root_paths.get(i).cloned().unwrap_or_default();
            let safe_name: String = root.name.chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect();
            let out = dir.join(format!("diskforge_export_{}_{}.csv", i + 1, safe_name));
            match export::export_tree_csv(root, &root_path, &out) {
                Ok((f, d)) => {
                    ok_count += 1;
                    crate::applog::log(&format!("[app] CSV 导出成功: {} (文件={}, 文件夹={})", out.display(), f, d));
                }
                Err(e) => crate::applog::log(&format!("[app] CSV 导出失败 ({}): {e}", root.name)),
            }
        }
        self.scan_error = Some(if ok_count == 0 {
            "CSV 导出失败，详情见日志".to_string()
        } else {
            format!("已导出 {ok_count} 个 CSV 文件到程序所在目录")
        });
    }

    #[cfg(windows)]
    fn restart_as_admin(&mut self) {
        use std::os::windows::ffi::OsStrExt;
        let Ok(exe) = std::env::current_exe() else { return };
        let verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
        let file: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // ShellExecuteW(hwnd, verb, file, params, dir, show_cmd)
        let result = unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(),
                std::ptr::null(), std::ptr::null(), 1,
            )
        };
        // ShellExecuteW 返回值 > 32 表示成功
        if (result as isize) > 32 {
            std::process::exit(0);
        } else {
            crate::applog::log(&format!("[app] 以管理员身份重启失败 (ShellExecuteW={result:?})"));
            self.scan_error = Some("以管理员身份重启失败，请手动以管理员运行".to_string());
        }
    }
}

/// 从选择界面的状态构造出这一批要扫描的路径列表：已选中的固定分区（按盘符排序，保证
/// 每次顺序确定）+ 用户手动添加的自定义目录。
fn build_scan_paths(picker: &startup::PickerState) -> Vec<PathBuf> {
    let mut drives: Vec<char> = picker.selected_drives.iter().copied().collect();
    drives.sort_unstable();
    let mut paths: Vec<PathBuf> = drives.iter().map(|&l| PathBuf::from(format!("{l}:\\"))).collect();
    paths.extend(picker.custom_paths.iter().map(PathBuf::from));
    paths
}

/// 扫描完成时把完整统计打到日志里（替代原来只针对单个分区的侧边栏"空间统计"面板——
/// 现在可以同时扫多个分区/目录，塞进侧边栏既放不下也不合适，这些信息本来
/// 列表里也都能看到，日志留一份方便事后核对/排查）。
fn log_scan_summary(node: &Node, info: Option<&DiskInfo>) {
    let free = info.map(|i| i.free_bytes);
    let used_by_system = info.map(|i| i.used_bytes);
    crate::applog::log(&format!(
        "[app] 扫描完成: {}\n  逻辑大小: {}\n  物理大小: {}\n  系统已用: {}\n  剩余空间: {}\n  文件: {}  文件夹: {}",
        node.name,
        crate::format::human_size(node.logical_size),
        crate::format::human_size(node.physical_size),
        used_by_system.map(crate::format::human_size).unwrap_or_else(|| "未知".to_string()),
        free.map(crate::format::human_size).unwrap_or_else(|| "未知".to_string()),
        node.file_count, node.folder_count,
    ));
    for c in compute_categories(node) {
        if c.size > 0 {
            crate::applog::log(&format!("  分类[{}]: {}", c.label, crate::format::human_size(c.size)));
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
