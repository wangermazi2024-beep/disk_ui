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
/// `phase`/`done`/`total` 见 `dedup::HashPhase`/`dedup::find_duplicates` 的
/// 说明——两个阶段（预筛/最终确认）各自独立计数，`done`/`total` 都是"当前
/// 这个阶段"的数字，不用再夹 `min` 防止"超过 100%"，UI 上应该按 `phase`
/// 分别展示成"第一步：xxx"/"第二步：xxx"，不要合并成一条进度，不然又会
/// 变回"看起来卡在 100%"的老问题（切换到第二阶段时数字会从 0 重新开始，
/// 不提示清楚"现在换阶段了"的话，用户会以为进度条自己倒退了）。
pub enum DuplicateMessage {
    Progress { phase: crate::dedup::HashPhase, done: u64, total: u64 },
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
        let on_progress = move |phase: crate::dedup::HashPhase, done: u64, total: u64| {
            let _ = tx_progress.send(DuplicateMessage::Progress { phase, done, total });
        };
        let groups = crate::dedup::find_duplicates(&paths, size_groups, &on_progress);

        // 每组重复文件的哈希/路径不再整批写日志了——之前那样做（每组一行、
        // `log_batch` 一次性落盘）实测就是"进度条已经走到 100%、界面却还在
        // 卡住不动"的真正原因：确认阶段之后如果还剩下几千甚至上万个分组，
        // 每组的路径列表拼接（`format!`、`join`）全部堆在一起做，是这段时间
        // 里唯一还在跑、但完全不出现在进度条里的工作，看起来就像"卡死了"。
        // 现在只挑第一组的第一个文件记一行日志，纯粹是给"想快速确认一下哈希
        // 对不对"的场景留个样例，不会有性能影响（就一行，不随分组数量增长）。
        // TODO(以后想在界面上看到完整结果的时候)：更好的位置是在重复文件
        // 列表里加一列"哈希"直接展示（`DuplicateGroup.hash_hex` 已经带着这个
        // 值了），可以直接在界面上复制/核对，比翻日志文件好用得多。
        if let Some(first) = groups.first() {
            if let Some(&first_file_idx) = first.file_indices.first() {
                crate::applog::log(&format!(
                    "[dedup] 示例（仅记第 1 组第 1 个文件，其余不再逐条记录）: hash={} size={} 路径={}",
                    first.hash_hex, first.size, paths[first_file_idx],
                ));
            }
        }

        let wasted_total: u64 = groups.iter().map(|g| g.size * (g.file_indices.len() as u64 - 1)).sum();
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
                    "{} × {count} 个文件（哈希确认，可省 {}）",
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
