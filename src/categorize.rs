//! 按文件类型分类统计。

use egui::Color32;
use crate::model::{Node, NodeKind};

const LABELS: [&str; 6] = ["视频", "压缩包", "程序/exe", "文档", "图片", "其他"];
const COLORS: [Color32; 6] = [
    Color32::from_rgb(0xE0, 0x55, 0x5B), Color32::from_rgb(0xF5, 0xA6, 0x23),
    Color32::from_rgb(0x4C, 0x8B, 0xF5), Color32::from_rgb(0x34, 0xC7, 0x59),
    Color32::from_rgb(0x9C, 0x6A, 0xDE), Color32::from_rgb(0x6C, 0x75, 0x7D),
];

fn classify(name: &str) -> usize {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("mp4"|"mkv"|"avi"|"mov"|"wmv"|"flv") => 0,
        Some("zip"|"rar"|"7z"|"tar"|"gz"|"xz") => 1,
        Some("exe"|"msi"|"dll"|"bat"|"sh"|"app") => 2,
        Some("doc"|"docx"|"pdf"|"txt"|"md"|"xlsx"|"pptx") => 3,
        Some("png"|"jpg"|"jpeg"|"gif"|"bmp"|"webp"|"svg") => 4,
        _ => 5,
    }
}

fn accumulate(node: &Node, totals: &mut [u64; 6]) {
    // 迭代版本：用显式栈代替原生递归，不管扫描到的目录树多深，都不会有栈溢出的可能性。
    let mut stack: Vec<&Node> = vec![node];
    while let Some(cur) = stack.pop() {
        match cur.kind {
            NodeKind::File => totals[classify(&cur.name)] += cur.logical_size,
            NodeKind::Folder => stack.extend(cur.children.iter()),
        }
    }
}

pub fn compute_categories(root: &Node) -> Vec<crate::model::CategoryStat> {
    let mut totals = [0u64; 6];
    accumulate(root, &mut totals);
    (0..6).map(|i| crate::model::CategoryStat { label: LABELS[i], size: totals[i], color: COLORS[i] }).collect()
}

/// 按扩展名分类，建一棵"合成树"：每种扩展名一个虚拟文件夹，下面放这个扩展名的
/// 全部真实文件（克隆自原树，带上 full_path_override 记住它们在磁盘上的真实路径）。
/// 建成 Node 树是为了直接复用 tree_list::show() 渲染——和主列表长得一模一样，
/// 可以展开、可以右键复制路径/打开所在文件夹，而不是另外画一套简化表格。
pub fn build_extension_tree(root: &Node, root_path: &str) -> Node {
    use std::collections::HashMap;
    let mut groups: HashMap<String, Vec<Node>> = HashMap::new();
    let mut stack: Vec<(&Node, String)> = vec![(root, root_path.trim_end_matches('\\').to_string())];
    while let Some((cur, path)) = stack.pop() {
        match cur.kind {
            NodeKind::File => {
                let ext = cur.name.rsplit_once('.')
                    .filter(|(base, e)| !base.is_empty() && !e.is_empty())
                    .map(|(_, e)| e.to_ascii_lowercase())
                    .unwrap_or_else(|| "（无扩展名）".to_string());
                let leaf = cur.clone().with_full_path(path.clone());
                groups.entry(ext).or_default().push(leaf);
            }
            NodeKind::Folder => {
                for child in &cur.children {
                    let child_path = if path.is_empty() { child.name.clone() } else { format!("{path}\\{}", child.name) };
                    stack.push((child, child_path));
                }
            }
        }
    }
    let ext_folders: Vec<Node> = groups.into_iter().map(|(ext, files)| {
        let display_ext = if ext.starts_with('（') { ext.clone() } else { format!(".{ext}") };
        let count = files.len();
        Node::new_folder_with_meta(
            format!("{display_ext}（{count} 个文件）"),
            GROUP_COLOR, files, 0, 0, 0, 0x10, 0, false, String::new(),
        )
    }).collect();
    Node::new_folder_with_meta("按扩展名分类".to_string(), GROUP_COLOR, ext_folders, 0, 0, 0, 0x10, 0, false, String::new())
}

/// 遍历树，收集所有可能重复的候选文件（按大小分好组，只保留组内 >= 2 个的）。
/// 这一步只是在内存里走一遍已经扫描好的树、克隆一些 `Node` 出来，不涉及任何
/// 磁盘 I/O，很快，可以放心地在调用方自己的线程（通常是 UI 线程）上同步跑；
/// 真正慢的"读文件内容算哈希"那部分在 `dedup::find_duplicates` 里，那个函数
/// 在后台线程上跑（见下面的 `spawn_duplicate_scan`），不会卡住界面。
fn collect_duplicate_candidates(root: &Node, root_path: &str) -> (Vec<Node>, Vec<String>, Vec<(u64, Vec<usize>)>) {
    use std::collections::HashMap;
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut nodes: Vec<Node> = Vec::new();
    let mut paths: Vec<String> = Vec::new();

    let mut stack: Vec<(&Node, String)> = vec![(root, root_path.trim_end_matches('\\').to_string())];
    while let Some((cur, path)) = stack.pop() {
        match cur.kind {
            NodeKind::File => {
                if cur.logical_size > 0 {
                    // 0 字节文件到处都是、内容比对没有意义（全都一样），跳过，
                    // 避免候选列表被一堆空文件淹没。
                    let idx = nodes.len();
                    // `.with_full_path(path.clone())` 记住真实磁盘路径——合成树里的
                    // 节点是克隆出来的，不再挂在原来的目录结构里，右键菜单的
                    // "打开所在文件夹"/"复制路径"/删除/属性 全靠这个字段才知道
                    // 真实位置在哪。
                    nodes.push(cur.clone().with_full_path(path.clone()));
                    paths.push(path);
                    by_size.entry(cur.logical_size).or_default().push(idx);
                }
            }
            NodeKind::Folder => {
                for child in &cur.children {
                    let child_path = if path.is_empty() { child.name.clone() } else { format!("{path}\\{}", child.name) };
                    stack.push((child, child_path));
                }
            }
        }
    }

    let size_groups: Vec<(u64, Vec<usize>)> = by_size.into_iter().filter(|(_, idxs)| idxs.len() >= 2).collect();
    (nodes, paths, size_groups)
}

/// 后台线程算重复文件期间/算完之后回传给 UI 线程的消息。`Progress` 里的
/// `done`/`total` 见 `dedup::find_duplicates` 的说明——`total` 只统计了
/// header 预筛阶段的文件数，`done` 有可能略微超过它（footer/全文件确认阶段
/// 是在子集上再跑一遍，也会计入 `done`），UI 展示进度条时应该夹一下
/// （`done.min(total)`），避免看起来"超过 100%"。
pub enum DuplicateMessage {
    Progress { done: u64, total: u64 },
    Done(Box<Node>),
}

/// 打开"重复文件查找"标签页的入口。分两段：
///   1. 在调用方线程（通常是 UI 线程）上同步跑 `collect_duplicate_candidates`——
///      只是内存里走一遍树，不碰磁盘，很快，不会让界面卡顿。
///   2. 真正耗时的哈希比对扔进一个新开的后台线程，通过 `tx` 汇报进度、最后
///      把算好的树回传——调用方（`app.rs`）拿到 `tx` 对应的 `Receiver` 之后
///      每帧 `try_recv()` 一下就行，界面全程可以正常交互，不会被卡住。
pub fn spawn_duplicate_scan(root: &Node, root_path: &str, tx: std::sync::mpsc::Sender<DuplicateMessage>) {
    let (nodes, paths, size_groups) = collect_duplicate_candidates(root, root_path);
    let total_candidates: usize = size_groups.iter().map(|(_, idxs)| idxs.len()).sum();

    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let tx_progress = tx.clone();
        let on_progress = move |done: u64, total: u64| {
            let _ = tx_progress.send(DuplicateMessage::Progress { done, total });
        };
        let groups = crate::dedup::find_duplicates(&paths, size_groups, &on_progress);

        // 把结果落盘：一条一条记录 sha256/大小/文件数/路径，方便用别的工具
        // （PowerShell 的 `Get-FileHash -Algorithm SHA256`、`certutil -hashfile`
        // 之类）独立核对"这个算法找出来的是不是真的重复文件"。用 `log_batch`
        // 一次性刷盘，不是每组都单独 flush 一次——组数一多（实测一次 C 盘
        // 扫描能有十几万组"候选"，内容比对之后剩下的"确认"组数量级会小很多，
        // 但依然可能有成千上万组），一条条 flush 本身就会变成新的性能瓶颈。
        let mut log_lines: Vec<String> = Vec::with_capacity(groups.len());
        let mut wasted_total: u64 = 0;
        for g in &groups {
            let wasted = g.size * (g.file_indices.len() as u64 - 1);
            wasted_total += wasted;
            let files_desc: Vec<String> = g.file_indices.iter().map(|&i| paths[i].clone()).collect();
            log_lines.push(format!(
                "[dedup] sha256={} size={} count={} 可省={} 路径: {}",
                g.sha256_hex, g.size, g.file_indices.len(), wasted, files_desc.join(" | "),
            ));
        }
        crate::applog::log_batch(&log_lines);
        crate::applog::log(&format!(
            "[dedup] 完成: 候选 {total_candidates} 个文件 → 确认 {} 组疑似重复，预计可省 {}，耗时 {:.1}s",
            groups.len(), crate::format::human_size(wasted_total), started.elapsed().as_secs_f32(),
        ));

        // 组好展示用的 Node 树，按"潜在可省空间"从大到小排序，最值得关注的排前面。
        let mut pairs: Vec<(u64, Node)> = groups
            .into_iter()
            .map(|g| {
                let wasted = g.size * (g.file_indices.len() as u64 - 1);
                let count = g.file_indices.len();
                let group_files: Vec<Node> = g.file_indices.iter().map(|&i| nodes[i].clone()).collect();
                let name = format!(
                    "{} × {count} 个文件（SHA-256 确认，可省 {}）",
                    crate::format::human_size(g.size), crate::format::human_size(wasted),
                );
                (wasted, Node::new_folder_with_meta(name, GROUP_COLOR, group_files, 0, 0, 0, 0x10, 0, false, String::new()))
            })
            .collect();
        pairs.sort_by(|a, b| b.0.cmp(&a.0));
        let dup_folders: Vec<Node> = pairs.into_iter().map(|(_, n)| n).collect();
        let tree = Node::new_folder_with_meta(
            "疑似重复文件（大小 + 内容哈希确认）".to_string(), GROUP_COLOR, dup_folders, 0, 0, 0, 0x10, 0, false, String::new(),
        );
        let _ = tx.send(DuplicateMessage::Done(Box::new(tree)));
    });
}

const GROUP_COLOR: Color32 = Color32::from_rgb(0xF5, 0xA6, 0x23);
