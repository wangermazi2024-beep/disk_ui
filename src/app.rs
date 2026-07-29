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
use crate::ui::{sidebar, tree_list, TreeAction};

pub struct DiskUiApp {
    root_path: String,
    /// 模拟的磁盘总容量。真实的"剩余空间"需要平台相关的 API（statvfs / GetDiskFreeSpaceEx），
    /// 为了不引入额外的平台依赖，这里按已用空间估算一个总容量用于展示比例条，
    /// 并在界面上如实叫它"演示值"，不假装是真实磁盘剩余空间。
    total_size: u64,

    root: Node,
    /// 当前 treemap 显示到的视图根节点路径（双击导航的结果），
    /// 是"从真正根节点出发"的绝对路径，空路径代表显示真正的根节点。
    /// 这只是一个只读的导航索引：渲染时用 `root.navigate(&view_path)` 现查，
    /// 不会修改、复制或丢弃 `root` 里的任何数据——文件列表树永远能看到完整的真实数据。
    view_path: NodePath,
    /// 当前选中节点，treemap 色块和文件列表树共用同一个选中状态实现联动高亮。
    selected: Option<NodePath>,
    /// 只在"刚展开某层/刚跳转到新视图根"的那一帧有值：告诉 treemap_view，
    /// 这一层的子色块是新出现的，需要自动选中其中第一个真正被渲染出来的色块，
    /// 而不是让旧的白色选中框停留在一个现在已经不相关的位置上。
    /// 用一次就清空（在 apply_action 开头清掉），不会一直生效。
    pending_auto_select: Option<NodePath>,

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
            view_path: Vec::new(),
            selected: None,
            pending_auto_select: None,
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
            window_fill: Color32::from_rgb(0x36, 0x36, 0x3A),
            panel_fill: Color32::from_rgb(0x36, 0x36, 0x3A),
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
                    self.view_path.clear();
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

    /// 中央面板：当前根节点名称 + treemap 色块 + 递归文件列表树。
    /// 所有交互统一收敛成一个 `TreeAction`，画完之后再统一应用到状态上，
    /// 避免在 egui 的画图闭包内部同时持有 `self` 的多处可变借用。
    fn show_central_panel(&self, ui: &mut egui::Ui) -> TreeAction {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(0x36, 0x36, 0x3A))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ui, |ui| {
                let mut action = TreeAction::None;

                ui.add_space(4.0);
                // 表头（固定，不滚动）
                let header_action = tree_list::show_header(ui);
                action.merge(header_action);
                ui.add_space(2.0);
                ui.separator();
                // 树行（可滚动）
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
                    .show(ui, |ui| {
                    let list_action = tree_list::show_body(ui, &self.root, &[], &self.selected);
                    action.merge(list_action);
                });
                action
            })
            .inner
    }

    fn apply_action(&mut self, action: TreeAction) {
        // pending_auto_select 只对"刚刚渲染的那一帧"有效，这一帧渲染时已经被
        // treemap_view 读取/使用过了（如果适用的话会转化成下面的 Select 分支），
        // 所以这里统一先清空，避免它一直生效、每帧都强制改选中项。
        self.pending_auto_select = None;

        match action {
            TreeAction::None => {}
            TreeAction::Select(path) => {
                self.selected = Some(path);
            }
            TreeAction::ToggleExpand(path) => {
                // path 已经是"从真正根节点出发"的绝对路径，直接在真实数据上操作即可：
                // exclusive_toggle 会自己递归到目标节点所在层，折叠兄弟、展开目标；
                // 返回 true 表示这次操作的结果是"展开"，false 表示"收起"。
                let expanded_now = self.root.exclusive_toggle(&path);
                self.selected = Some(path.clone());
                if expanded_now {
                    // 新展开出来的子色块里还没有天然合理的选中项，标记这一层，
                    // 下一帧交给 treemap_view 自动选中第一个真正渲染出来的色块。
                    self.pending_auto_select = Some(path);
                }
            }
            TreeAction::EnterNode(path) => {
                // 双击某个子色块（或文件列表树里的某一行）：把它的父节点设为
                // 新的"当前视图根"。view_path 只是一个只读的导航索引（下次
                // 渲染时用它去 navigate），不修改、不复制、也不丢弃 root 里的
                // 任何数据——文件列表树用的还是同一份完整的真实数据。
                //
                // 新画面里子色块的布局跟顶层视图完全一样、没有任何特殊标记，
                // 看起来就像直接从这个上级目录重新扫描出来的一样。往上回退
                // 用的是面包屑 / "⬆ 上级目录" 按钮，一路点回真正的根目录。
                //
                // 这里不需要 pending_auto_select 兜底：`path` 本身就是刚被
                // 双击进入的那个节点，它必定会作为新画面里的一个子色块存在，
                // 直接高亮它就是明确、有意义的选中状态。
                if let Some((_, parent_path)) = path.split_last() {
                    self.view_path = parent_path.to_vec();
                }
                self.selected = Some(path);
            }
            TreeAction::NavigateTo(path) => {
                // 面包屑 / "⬆ 上级目录"按钮：直接把视图根跳到给定的绝对路径，
                // 可能一次跳好几层，旧的选中项大概率已经不在新画面里了，
                // 交给下一帧自动选中新视图根下第一个真正渲染出来的色块。
                self.view_path = path.clone();
                self.selected = None;
                self.pending_auto_select = Some(path);
            }
        }
    }
}
