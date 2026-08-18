//! 应用主状态：启动即显示主界面（空的），弹窗选分区/目录 → 顺序批量扫描 → 结果树。
//! 主区域是标签页：默认"主列表"一个标签，点"文件扩展名分类"/"重复文件查找"
//! 会各自开一个新标签页。这两个分析标签页内部用 `ui::compact_tree` 渲染（和主列表
//! 视觉语言一致——能展开、有缩进参考线、右键菜单——但列不一样：数据先在 categorize.rs
//! 里重新组织成一棵"合成树"，按扩展名/大小分组当文件夹，真实文件当叶子）。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use egui::{Color32, RichText};

use crate::categorize;
use crate::disk_info::{self, DiskInfo};
use crate::export;
use crate::model::{CategoryStat, Node, NodePath};
use crate::scan::{self, ScanMessage};
use crate::ui::topbar::{self, TopbarAction, TopbarState};
use crate::ui::{sidebar, startup, tree_list, TreeAction};

/// 主区域的一个标签页。"主列表"永远是第一个、不能关闭；
/// 扩展名分类/重复文件查找是按需打开的，数据（合成树）在打开的那一刻算一次，存在标签页里。
/// 每个分析标签页有自己独立的 `selected`（展开/选中状态），不会和主列表互相干扰。
enum Tab {
    Main,
    Extensions { partition_idx: usize, title: String, root: Node, selected: Option<NodePath>, view: crate::ui::compact_tree::ViewState },
    Duplicates {
        partition_idx: usize, title: String, root: Node, selected: Option<NodePath>, view: crate::ui::compact_tree::ViewState,
        /// 后台线程还在跑内容哈希比对的时候是 `Some((阶段, done, total))`；
        /// 算完变成 `None`，这时候 `root` 才是真正算好的结果树。在那之前
        /// `root` 只是一棵空占位树（不是 `Option<Node>`，是为了让上面那几处
        /// `Tab::Extensions { root, .. } | Tab::Duplicates { root, .. }` 合并
        /// 匹配的地方不用跟着改类型），UI 层看到 `loading.is_some()` 就知道
        /// 该显示"正在比对内容…"的进度提示，而不是把这棵空树渲染成"一个重复
        /// 文件都没找到"。阶段（`dedup::HashPhase`）单独带着，不能省——两个
        /// 阶段（预筛/最终确认）的 done/total 是分开计数的，各自 0~各自的
        /// 100%，UI 上不分阶段直接展示一条进度会在切换阶段时看起来"卡在
        /// 100% 不动"或者"进度突然归零往回跳"，两种观感都会让人以为程序
        /// 卡死了。
        loading: Option<(crate::dedup::HashPhase, u64, u64)>,
    },
}

/// 右键菜单点了"删除到回收站"之后、用户在确认框里点"确定"之前的中间状态。
/// 单独存一份 name/full_path/is_folder（而不是等确认时再重新沿 abs_path 走一遍树）是
/// 因为确认框要立刻展示这些信息，且这段时间里树本身理论上可能变（虽然当前 UI 下
/// 用户点了确认框就基本被这个模态挡住了，操作不了别的，但直接存一份更稳妥、也省事）。
///
/// `source` 记的是这个删除请求是从哪棵树发起的：主列表（`self.partitions`）还是
/// 某个分析标签页自己的合成树（`Tab::Extensions`/`Tab::Duplicates` 里的 `root`）。
/// 这两棵树的节点是各自独立的 `Node` 拷贝（`categorize.rs` 建合成树时是克隆的，
/// 不是共享引用），所以"从哪棵树来的就摘哪棵树"，不能用同一份 abs_path 去两边都摘——
/// 下标含义完全不是一回事。已知的权衡：如果同一个文件在主列表和某个分析标签页里
/// 都能看到，从分析标签页删除后，主列表那边在下次重新扫描之前还会显示这个已经不存在
/// 的文件（磁盘上已经真删了，只是内存里那棵没同步更新）——分析标签页本来就是"打开那一刻
/// 拍的快照"，这个限制和它本来的语义是一致的。
struct PendingDelete {
    source: DeleteSource,
    abs_path: NodePath,
    name: String,
    full_path: String,
    is_folder: bool,
}

#[derive(Clone, Copy)]
enum DeleteSource {
    Main,
    Tab(usize),
}

pub struct DiskUiApp {
    partitions: Vec<Node>,
    partition_infos: Vec<Option<DiskInfo>>,
    /// 分类统计缓存：扫描完成时算一次存起来，不在每一帧里现算——分类统计要遍历
    /// 整棵树，几十万个文件的情况下每帧都重算一遍是明显能感觉到卡顿的（如果界面按
    /// 60fps 刷新，就是一秒钟内把整棵树重新遍历 60 次），缓存下来后侧边栏只是读一个
    /// 现成的 Vec，不用每帧都现算。
    partition_categories: Vec<Vec<CategoryStat>>,
    /// 和 `partitions`/`partition_infos` 一一对应：这个分区/目录当初是从哪个路径扫的，
    /// 右键菜单拼完整路径、打开扩展名/重复文件分析都要用到。
    partition_root_paths: Vec<String>,
    selected: Option<NodePath>,

    tabs: Vec<Tab>,
    active_tab: usize,

    scanning: bool,
    scanned_count: u64,
    scan_error: Option<String>,
    scan_rx: Option<Receiver<ScanMessage>>,
    /// 一次选了多个分区/目录时，排队按顺序一个个扫，扫完一个再扫下一个。
    scan_queue: VecDeque<PathBuf>,
    current_scan_path: Option<PathBuf>,

    /// 选择分区/目录的弹窗。启动时就是 `Some(..)`（软件一打开就弹出来选），
    /// 背景仍然是（空的）主界面；"文件 > 添加扫描…"也是把这个重新置为 `Some`。
    picker: Option<startup::PickerState>,

    /// 视图 > 显示全部信息：开=全部列 + 元数据文件；关=只留关键列、隐藏元数据文件。
    show_all_details: bool,

    /// 主列表当前的排序列 + 方向 + 展开版本号 + 可见行缓存，点表头改；
    /// 默认和构建时的排序规则一致（按逻辑大小降序），不点表头的话行为和以前完全一样。
    list_state: tree_list::ListState,

    /// 右键菜单"删除到回收站"点了之后、确认框点"确定"/"取消"之前的等待状态；
    /// `None` 时不显示确认框。
    pending_delete: Option<PendingDelete>,

    /// 正在后台跑"重复文件查找"内容哈希比对的分区，`(分区下标, 结果通道)`。
    /// 一个 `Vec` 是因为可能同时有好几个分区的比对在并行跑（用户开了多个
    /// 重复文件标签页）；每帧在 `poll_duplicate_scan` 里收一遍。
    duplicate_rx: Vec<(usize, Receiver<categorize::DuplicateMessage>)>,

    /// 真正在后台线程执行"删除到回收站"（含占用重试）期间，保留一份
    /// `PendingDelete`（等结果回来了还要用它去更新树）+ 结果通道。
    /// 只会同时有一个在跑——确认框是模态的，没删完之前弹不出第二个。
    delete_rx: Option<(PendingDelete, Receiver<Result<(), String>>)>,
}

impl Default for DiskUiApp {
    fn default() -> Self {
        let drives = disk_info::list_fixed_drives_with_labels();
        Self {
            partitions: Vec::new(),
            partition_infos: Vec::new(),
            partition_categories: Vec::new(),
            partition_root_paths: Vec::new(),
            selected: None,
            tabs: vec![Tab::Main],
            active_tab: 0,
            scanning: false,
            scanned_count: 0,
            scan_error: None,
            scan_rx: None,
            scan_queue: VecDeque::new(),
            current_scan_path: None,
            picker: Some(startup::PickerState::new(drives)),
            show_all_details: true,
            list_state: tree_list::ListState::default(),
            pending_delete: None,
            duplicate_rx: Vec::new(),
            delete_rx: None,
        }
    }
}

impl eframe::App for DiskUiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().set_visuals(egui::Visuals::dark());
        self.poll_scan();
        self.poll_duplicate_scan();
        self.poll_delete();

        self.show_main_screen(ui);
        if self.picker.is_some() {
            self.show_picker_modal(ui.ctx());
        }
        if self.pending_delete.is_some() {
            self.show_delete_confirm_modal(ui.ctx());
        }

        // 扫描中：进度数字和转圈动画要持续刷新，这时候需要强制重绘。
        // 空闲时不再无条件 request_repaint()：那样等于强制 egui 一直按屏幕刷新率
        // （通常 60Hz）重绘，不管界面有没有变化都要重新布局一遍，是 egui 官方
        // GitHub 讨论区里明确点出来的"不必要 CPU 占用"反模式，鼠标移动/点击/
        // 菜单展开这些交互 egui 自己就会触发重绘，不需要每帧手动催一次。
        if self.scanning || !self.duplicate_rx.is_empty() || self.delete_rx.is_some() {
            ui.ctx().request_repaint();
        }
    }
}

impl DiskUiApp {
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

        let focused_idx = self.selected.as_ref().and_then(|p| p.first().copied())
            .or(if self.partitions.is_empty() { None } else { Some(0) });

        match action {
            TopbarAction::AddScan => {
                let drives = disk_info::list_fixed_drives_with_labels();
                self.picker = Some(startup::PickerState::new(drives));
            }
            TopbarAction::ExportCsv => self.export_csv(),
            TopbarAction::ToggleShowAll => self.show_all_details = !self.show_all_details,
            TopbarAction::ShowExtensionBreakdown => { if let Some(pi) = focused_idx { self.open_extension_tab(pi); } }
            TopbarAction::ShowDuplicateFinder => { if let Some(pi) = focused_idx { self.open_duplicate_tab(pi); } }
            #[cfg(windows)]
            TopbarAction::RestartAsAdmin => self.restart_as_admin(),
            TopbarAction::None => {}
        }

        self.show_branding_bar(ui);
        self.show_tab_bar(ui);

        // 弹窗打开的时候，背景内容（侧边栏 + 主区域）整体禁用，提示用户先处理弹窗——
        // 但仍然可见，不是替换成另一个界面。
        let background_enabled = self.picker.is_none();
        let focused_node = focused_idx.and_then(|i| self.partitions.get(i));
        let focused_info = focused_idx.and_then(|i| self.partition_infos.get(i)).and_then(|o| o.as_ref());
        let focused_categories = focused_idx.and_then(|i| self.partition_categories.get(i)).map(|v| v.as_slice());

        let mut sidebar_action = sidebar::SidebarAction::None;
        egui::Panel::left("sidebar").exact_size(220.0)
            .frame(egui::Frame::default().fill(Color32::from_rgb(0x2A, 0x2A, 0x2E)).inner_margin(egui::Margin::symmetric(12, 4)))
            .show(ui, |ui| {
                ui.add_enabled_ui(background_enabled, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        sidebar_action = sidebar::show(ui, focused_node, focused_info, focused_categories);
                    });
                });
            });
        match sidebar_action {
            sidebar::SidebarAction::OpenExtensions => { if let Some(pi) = focused_idx { self.open_extension_tab(pi); } }
            sidebar::SidebarAction::OpenDuplicates => { if let Some(pi) = focused_idx { self.open_duplicate_tab(pi); } }
            sidebar::SidebarAction::None => {}
        }

        let tab_idx = self.active_tab.min(self.tabs.len().saturating_sub(1));
        let tree_action = egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(Color32::from_rgb(0x24, 0x24, 0x28)).inner_margin(egui::Margin::same(4)))
            .show(ui, |ui| {
                ui.add_enabled_ui(background_enabled, |ui| {
                    match self.tabs.get_mut(tab_idx) {
                        Some(Tab::Main) | None => {
                            tree_list::show(ui, &self.partitions, &self.partition_infos, &self.partition_root_paths, &self.selected, self.show_all_details, &mut self.list_state)
                        }
                        Some(Tab::Duplicates { loading: Some((phase, done, total)), title, .. }) => {
                            show_duplicate_loading(ui, title, *phase, *done, *total);
                            TreeAction::None
                        }
                        Some(Tab::Extensions { root, selected, view, .. }) | Some(Tab::Duplicates { root, selected, view, .. }) => {
                            crate::ui::compact_tree::show(ui, root, selected, view)
                        }
                    }
                }).inner
            })
            .inner;
        self.apply_tree_action(tree_action);
    }

    /// 窗口左下角的小品牌条：软件名 + 开发者。
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

    /// 标签栏：只有"主列表"一个标签时不画，省地方（和之前单标签页的观感一致）。
    fn show_tab_bar(&mut self, ui: &mut egui::Ui) {
        if self.tabs.len() <= 1 { return; }
        let mut select_idx: Option<usize> = None;
        let mut close_idx: Option<usize> = None;
        egui::Panel::top("tab_bar").exact_size(30.0)
            .frame(egui::Frame::default().fill(Color32::from_rgb(0x26, 0x26, 0x2A)).inner_margin(egui::Margin::symmetric(6, 3)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let title = match tab {
                            Tab::Main => "📋 主列表".to_string(),
                            Tab::Extensions { title, .. } => format!("🗐 {title}"),
                            Tab::Duplicates { title, .. } => format!("🔍 {title}"),
                        };
                        if ui.selectable_label(i == self.active_tab, title).clicked() {
                            select_idx = Some(i);
                        }
                        if !matches!(tab, Tab::Main) && ui.small_button("×").clicked() {
                            close_idx = Some(i);
                        }
                        ui.add_space(4.0);
                    }
                });
            });
        if let Some(i) = select_idx { self.active_tab = i; }
        if let Some(i) = close_idx {
            self.tabs.remove(i);
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len().saturating_sub(1);
            } else if self.active_tab > i {
                self.active_tab -= 1;
            }
        }
    }

    /// 打开（或切换到已经开着的）某个分区的"文件扩展名分类"标签页。
    fn open_extension_tab(&mut self, pi: usize) {
        if let Some(idx) = self.tabs.iter().position(|t| matches!(t, Tab::Extensions { partition_idx, .. } if *partition_idx == pi)) {
            self.active_tab = idx;
            return;
        }
        let Some(node) = self.partitions.get(pi) else { return };
        let root_path = self.partition_root_paths.get(pi).cloned().unwrap_or_default();
        let root = categorize::build_extension_tree(node, &root_path);
        let title = node.name.clone();
        crate::applog::log(&format!("[app] 打开扩展名分类标签页: {title}（{} 种扩展名）", root.children.len()));
        self.tabs.push(Tab::Extensions { partition_idx: pi, title, root, selected: None, view: crate::ui::compact_tree::ViewState::default() });
        self.active_tab = self.tabs.len() - 1;
    }

    /// 打开（或切换到已经开着的）某个分区的"重复文件查找"标签页。
    ///
    /// `categorize::spawn_duplicate_scan` 会真的读文件内容算哈希做确认
    /// （见 dedup.rs 的说明），这一步有实打实的磁盘 I/O——但现在是在后台线程上
    /// 跑的：这里立刻插入一个 `loading: Some(...)` 的占位标签页就返回，真正的
    /// 计算通过 `duplicate_rx` 异步收结果，界面全程可以正常操作，不会卡住。
    fn open_duplicate_tab(&mut self, pi: usize) {
        if let Some(idx) = self.tabs.iter().position(|t| matches!(t, Tab::Duplicates { partition_idx, .. } if *partition_idx == pi)) {
            self.active_tab = idx;
            return;
        }
        let Some(node) = self.partitions.get(pi) else { return };
        let root_path = self.partition_root_paths.get(pi).cloned().unwrap_or_default();
        let title = node.name.clone();
        crate::applog::log(&format!("[app] 开始比对重复文件: {title}"));

        let (tx, rx) = mpsc::channel();
        categorize::spawn_duplicate_scan(node, &root_path, tx);
        self.duplicate_rx.push((pi, rx));

        self.tabs.push(Tab::Duplicates {
            partition_idx: pi, title, root: Node::new_folder("", Color32::WHITE, Vec::new()),
            selected: None, view: crate::ui::compact_tree::ViewState::default(), loading: Some((crate::dedup::HashPhase::Prefilter, 0, 0)),
        });
        self.active_tab = self.tabs.len() - 1;
    }

    /// 每帧调用：收后台"重复文件查找"线程的进度/结果消息，更新对应标签页。
    /// 同一时间可能有好几个分区的比对在并行跑（用户开了好几个不同分区的重复
    /// 文件标签页），所以是一个 `Vec`，不是单个 `Option<Receiver<_>>`。
    ///
    /// 标签页有可能在后台还没算完的时候就被用户关掉了（当前没有做"取消正在跑
    /// 的计算"这件事，关掉标签页只是不再展示结果，后台线程会自己跑到底）——
    /// 这种情况下消息直接丢弃，不去找已经不存在的标签页；`duplicate_rx` 里的
    /// 记录要等对应的发送端彻底断开（后台线程跑完、`tx` 被 drop）才清掉，
    /// 不然会有一条永远不会再收到消息、但也一直留在 `Vec` 里的僵尸记录。
    fn poll_duplicate_scan(&mut self) {
        if self.duplicate_rx.is_empty() {
            return;
        }
        let mut done_pi: Vec<usize> = Vec::new();
        for (pi, rx) in &self.duplicate_rx {
            let pi = *pi;
            loop {
                match rx.try_recv() {
                    Ok(msg) => {
                        let tab = self.tabs.iter_mut().find(|t| matches!(t, Tab::Duplicates { partition_idx, .. } if *partition_idx == pi));
                        if let Some(Tab::Duplicates { root, loading, .. }) = tab {
                            match msg {
                                categorize::DuplicateMessage::Progress { phase, done, total } => *loading = Some((phase, done, total)),
                                categorize::DuplicateMessage::Done(tree) => { *root = *tree; *loading = None; }
                            }
                        }
                        // 标签页已经被关掉的情况：消息直接丢弃，什么都不做。
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => { done_pi.push(pi); break; }
                }
            }
        }
        if !done_pi.is_empty() {
            self.duplicate_rx.retain(|(pi, _)| !done_pi.contains(pi));
        }
    }

    /// 选分区/目录的弹窗。用 `egui::Window` 模拟模态：不可缩放、居中，
    /// 背景内容在 `show_main_screen` 里已经被 `add_enabled_ui(false, ..)` 整体禁用了。
    fn show_picker_modal(&mut self, ctx: &egui::Context) {
        let show_cancel = !self.partitions.is_empty() || self.scanning || !self.scan_queue.is_empty();
        let Some(picker) = &mut self.picker else { return };
        let mut picker_action = startup::PickerAction::None;
        egui::Window::new("选择要扫描的分区/目录")
            .id(egui::Id::new("scan_picker_modal"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                picker_action = startup::show(ui, picker, show_cancel);
            });
        match picker_action {
            startup::PickerAction::Confirm => {
                let paths = build_scan_paths(picker);
                self.picker = None;
                self.start_scan_batch(paths);
            }
            startup::PickerAction::Cancel => { self.picker = None; }
            startup::PickerAction::None => {}
        }
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
                    let categories = categorize::compute_categories(&node);
                    self.partitions.push(node);
                    self.partition_infos.push(info);
                    self.partition_categories.push(categories);
                    self.partition_root_paths.push(path);
                    self.selected = None;
                    // `partitions.push` 可能触发 Vec 扩容、所有元素搬家，主列表缓存里
                    // 存的裸指针会跟着失效——版本号 +1 强制下一帧重新收集可见行。
                    self.list_state.expand_version += 1;
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

    /// 把展开/选中操作应用到"当前激活的那个标签页"自己的树/选中状态上——
    /// 主列表用 `self.selected` + `self.partitions`，每个分析标签页有自己独立的
    /// `selected` + 合成 `root`，互不干扰（切标签页不会互相影响展开状态）。
    fn apply_tree_action(&mut self, action: TreeAction) {
        let tab_idx = self.active_tab.min(self.tabs.len().saturating_sub(1));
        let is_view_tab = matches!(self.tabs.get(tab_idx), Some(Tab::Extensions { .. }) | Some(Tab::Duplicates { .. }));
        match action {
            TreeAction::None => {}
            TreeAction::Select(p) | TreeAction::EnterNode(p) => {
                if is_view_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let selected = match tab {
                            Tab::Extensions { selected, .. } | Tab::Duplicates { selected, .. } => selected,
                            Tab::Main => return,
                        };
                        *selected = Some(p);
                    }
                } else {
                    self.selected = Some(p);
                }
            }
            TreeAction::ToggleExpand(p) => {
                if is_view_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let (root, selected, view) = match tab {
                            Tab::Extensions { root, selected, view, .. } | Tab::Duplicates { root, selected, view, .. } => (root, selected, view),
                            Tab::Main => return,
                        };
                        // 合成树只有一个根（tree_list 里传的是单元素切片），abs_path[0] 恒为 0，
                        // 真正要用来定位/展开的是 p[1..]。
                        if p.len() == 1 { root.expanded = !root.expanded; } else { root.exclusive_toggle(&p[1..]); }
                        *selected = Some(p);
                        // 展开状态变了，这个标签页缓存的可见行列表要重算。
                        view.expand_version += 1;
                    }
                } else {
                    if let Some(&pi) = p.first() {
                        if let Some(part) = self.partitions.get_mut(pi) {
                            if p.len() == 1 { part.expanded = !part.expanded; } else { part.exclusive_toggle(&p[1..]); }
                        }
                    }
                    // 展开状态变了，主列表缓存的可见行列表要重算。
                    self.list_state.expand_version += 1;
                    self.selected = Some(p);
                }
            }
            TreeAction::RequestDelete { abs_path, name, full_path, is_folder } => {
                // 只是记下来，真正删除要等用户在确认框里点"确定"——见 show_delete_confirm_modal。
                // is_view_tab 已经在函数开头算好了：决定这个 abs_path 应该去哪棵树上摘。
                let source = if is_view_tab { DeleteSource::Tab(tab_idx) } else { DeleteSource::Main };
                self.pending_delete = Some(PendingDelete { source, abs_path, name, full_path, is_folder });
            }
        }
    }

    /// "删除到回收站"确认框：点右键菜单只是记了个 `pending_delete`，这里才是用户
    /// 点"确定"之后真正调用 Win32 API 的地方。
    fn show_delete_confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.pending_delete else { return };
        let kind = if pending.is_folder { "文件夹" } else { "文件" };
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("确认删除")
            .id(egui::Id::new("delete_confirm_modal"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                ui.label(format!("确定要把这个{kind}删除到回收站吗？"));
                ui.add_space(4.0);
                ui.label(egui::RichText::new(&pending.name).strong());
                ui.label(egui::RichText::new(&pending.full_path).small().color(egui::Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                if pending.is_folder {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("文件夹里的所有内容都会一起被删除。").small().color(egui::Color32::from_rgb(0xF5, 0xA6, 0x23)));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() { cancel = true; }
                    if ui.add(egui::Button::new(egui::RichText::new("删除到回收站").color(egui::Color32::WHITE))
                        .fill(egui::Color32::from_rgb(0xC0, 0x40, 0x40))).clicked()
                    {
                        confirm = true;
                    }
                });
            });
        if cancel {
            self.pending_delete = None;
        } else if confirm {
            self.execute_pending_delete();
        }
    }

    /// 用户在确认框里点了"删除到回收站"：真正的删除（含占用重试，最多可能
    /// 要等接近 2 秒——见 `file_ops::delete_to_recycle_bin_with_retry`）挪到
    /// 后台线程做，不在 UI 线程上等，弹窗立刻关掉，界面照常能操作；结果通过
    /// `delete_rx` 在 `poll_delete` 里收。
    fn execute_pending_delete(&mut self) {
        let Some(pending) = self.pending_delete.take() else { return };
        let full_path = pending.full_path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::file_ops::delete_to_recycle_bin_with_retry(&full_path));
        });
        self.delete_rx = Some((pending, rx));
    }

    /// 每帧调用：收后台删除线程的结果。
    fn poll_delete(&mut self) {
        let Some((_, rx)) = &self.delete_rx else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return, // 还没删完，下一帧再看
            // 发送端断开却没收到消息，理论上只会是后台线程 panic 了——正常
            // 逻辑下 `delete_to_recycle_bin_with_retry` 总会返回一个
            // Ok/Err，不会出现"什么都不发就跑没了"，这里兜个底不留僵尸记录。
            Err(mpsc::TryRecvError::Disconnected) => Err("删除线程异常退出".to_string()),
        };
        let (pending, _) = self.delete_rx.take().unwrap();
        match result {
            Ok(()) => {
                match pending.source {
                    DeleteSource::Main => {
                        // 从内存里的树上把这一项摘掉（同时更新沿途所有祖先的聚合统计），
                        // 不用为了这一个文件重新扫一遍整个分区。abs_path 至少是
                        // [分区下标, 子节点下标, ...]——右键菜单的"删除"目前只挂在文件/
                        // 文件夹行上，不挂在磁盘/根目录行上，所以这里长度必然 >= 2；
                        // 如果哪天误传了长度 1 的路径，宁可什么都不做也不去动
                        // partitions/partition_infos/partition_categories/partition_root_paths
                        // 这几个下标必须一一对应的并行数组——只删 partitions 一个会把它们全错位。
                        if pending.abs_path.len() >= 2 {
                            if let Some(&pi) = pending.abs_path.first() {
                                if let Some(part) = self.partitions.get_mut(pi) {
                                    part.remove_at_path(&pending.abs_path[1..]);
                                }
                            }
                        }
                        if self.selected.as_ref() == Some(&pending.abs_path) { self.selected = None; }
                        // 树结构变了（对应节点及其祖先的聚合统计都变了），主列表缓存要重算。
                        self.list_state.expand_version += 1;
                    }
                    DeleteSource::Tab(tab_idx) => {
                        // 同样的道理，只是摘的是这个标签页自己的合成树，不是 self.partitions。
                        // 如果这时候标签页已经不在了、或者被切换成了别的类型（用户在删除
                        // 期间关掉了那个标签页），就什么都不做，不去动任何数据。
                        let Some(tab) = self.tabs.get_mut(tab_idx) else { return };
                        let (root, selected, view) = match tab {
                            Tab::Extensions { root, selected, view, .. } | Tab::Duplicates { root, selected, view, .. } => (root, selected, view),
                            Tab::Main => return,
                        };
                        if pending.abs_path.len() >= 2 {
                            root.remove_at_path(&pending.abs_path[1..]);
                        }
                        if selected.as_ref() == Some(&pending.abs_path) { *selected = None; }
                        view.expand_version += 1;
                    }
                }
                self.scan_error = Some(format!("已删除到回收站: {}", pending.name));
            }
            Err(e) => {
                // `delete_to_recycle_bin_with_retry` 失败之后已经尝试查过占用
                // 进程、把结果拼进了错误信息里（见 file_ops.rs），这里直接
                // 展示，不用再单独查一遍。
                self.scan_error = Some(format!("删除失败 ({}): {e}", pending.name));
            }
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

/// "重复文件查找"标签页在后台线程还没算完时显示的占位内容：转圈 + 进度条。
/// 两个阶段（`dedup::HashPhase::Prefilter`/`Confirm`）分开展示，各自的
/// `done`/`total` 都是这个阶段自己的数字，不用再夹 `min` 防止"超过 100%"——
/// 见 `dedup.rs`/`app.rs` 里 `Tab::Duplicates.loading` 字段上的说明：不分开
/// 展示的话，切换到第二阶段时要么看起来"卡在 100% 不动"要么"进度突然归零
/// 往回跳"，两种都会让人误以为程序卡死了。
fn show_duplicate_loading(ui: &mut egui::Ui, title: &str, phase: crate::dedup::HashPhase, done: u64, total: u64) {
    let (step_label, step_desc) = match phase {
        crate::dedup::HashPhase::Prefilter => (
            "第 1 步 / 共 2 步：快速预筛",
            "读取每个候选文件开头一小段内容，先排除大小相同但内容一开始就不一样的文件。",
        ),
        crate::dedup::HashPhase::Confirm => (
            "第 2 步 / 共 2 步：逐字节确认",
            "逐字节比较文件内容，确认是不是真的一模一样（不是靠哈希碰巧相同）——这一步的文件数取决于实际重复率，重复率越高这一步越慢。",
        ),
    };
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() / 2.0 - 56.0).max(0.0));
        ui.spinner();
        ui.add_space(8.0);
        ui.label(egui::RichText::new(format!("正在比对内容：{title}")).strong().size(15.0));
        ui.add_space(2.0);
        ui.label(egui::RichText::new(step_label).strong().color(Color32::from_rgb(0x4C, 0x8B, 0xF5)));
        ui.add_space(4.0);
        if total == 0 {
            ui.label("正在收集候选文件…");
        } else {
            ui.label(format!("已处理 {done} / {total} 个文件"));
            let frac = (done as f32 / total as f32).clamp(0.0, 1.0);
            ui.add(egui::ProgressBar::new(frac).desired_width(320.0));
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new(step_desc).small().color(Color32::from_rgb(0x90, 0x90, 0x90)));
        ui.add_space(2.0);
        ui.label(egui::RichText::new("界面这段时间可以正常操作其它标签页。")
            .small().color(Color32::from_rgb(0x90, 0x90, 0x90)));
    });
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

/// 扫描完成时把完整统计打到日志里（替代原来只针对单个分区的侧边栏"空间统计"文字块——
/// 现在可以同时扫多个分区/目录，塞进侧边栏既放不下也不合适，这些数字列表里也都能看到，
/// 日志留一份方便事后核对/排查）。
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
    for c in categorize::compute_categories(node) {
        if c.size > 0 {
            crate::applog::log(&format!("  分类[{}]: {}", c.label, crate::format::human_size(c.size)));
        }
    }
}
