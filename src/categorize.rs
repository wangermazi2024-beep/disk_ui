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

/// 按文件大小分组、再用内容哈希确认，找出真正内容相同的重复文件，建一棵合成树
/// （每组一个虚拟文件夹）。具体的"内容比对"逻辑在 `dedup.rs`——那边有详细注释
/// 说明为什么不能只按大小分组（大小相同不代表内容相同）、两阶段哈希（局部预筛
/// + 全文件确认）是怎么工作的，这边只管调用、把结果拼成 UI 要的 `Node` 树。
///
/// 即便走完这套内容哈希流程，文件夹名字里依然写"疑似"——理论上仍有极小概率的
/// 哈希碰撞（`dedup.rs` 里有具体说明），界面上不应该让人误以为这是"绝对保证
/// 相同"的确认结果；真正要执行删除/建符号链接这类不可逆操作之前，还需要再加
/// 一道逐字节比较的保险（这一步留给以后接符号链接功能的时候做，`dedup.rs`
/// 顶部的注释里也提了这一点）。
///
/// 按"潜在可省空间"（size × (count-1)）从大到小排序，最值得关注的排前面。
pub fn build_duplicate_tree(root: &Node, root_path: &str) -> Node {
    use std::collections::HashMap;

    // 阶段一：按大小分组（和以前一样，免费的第一轮筛选，大小都不同直接排除）。
    let mut by_size: HashMap<u64, Vec<(Node, String)>> = HashMap::new();
    let mut stack: Vec<(&Node, String)> = vec![(root, root_path.trim_end_matches('\\').to_string())];
    while let Some((cur, path)) = stack.pop() {
        match cur.kind {
            NodeKind::File => {
                if cur.logical_size > 0 {
                    // 0 字节文件到处都是、内容比对没有意义（全都一样），跳过，
                    // 避免候选列表被一堆空文件淹没。
                    // `.with_full_path(path.clone())` 记住真实磁盘路径——合成树里的
                    // 节点是克隆出来的，不再挂在原来的目录结构里，右键菜单的
                    // "打开所在文件夹"/"复制路径"/删除/属性 全靠这个字段才知道
                    // 真实位置在哪。
                    let leaf = cur.clone().with_full_path(path.clone());
                    by_size.entry(cur.logical_size).or_default().push((leaf, path.clone()));
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

    // 阶段二/三（在 dedup.rs 里）：大小相同的这些文件，内容是不是真的相同。
    let mut pairs: Vec<(u64, Node)> = Vec::new();
    for (size, files) in by_size {
        if files.len() < 2 {
            continue; // 这个大小全世界就这一个文件，不用比。
        }
        let paths: Vec<String> = files.iter().map(|(_, p)| p.clone()).collect();
        for idxs in crate::dedup::group_by_content(&paths) {
            let count = idxs.len() as u64;
            let wasted = size * (count - 1);
            let group_files: Vec<Node> = idxs.iter().map(|&i| files[i].0.clone()).collect();
            let name = format!(
                "{} × {count} 个文件（疑似重复，可省 {}）",
                crate::format::human_size(size), crate::format::human_size(wasted),
            );
            pairs.push((wasted, Node::new_folder_with_meta(name, GROUP_COLOR, group_files, 0, 0, 0, 0x10, 0, false, String::new())));
        }
    }
    pairs.sort_by(|a, b| b.0.cmp(&a.0));
    let dup_folders: Vec<Node> = pairs.into_iter().map(|(_, n)| n).collect();
    Node::new_folder_with_meta("疑似重复文件（大小 + 内容哈希确认）".to_string(), GROUP_COLOR, dup_folders, 0, 0, 0, 0x10, 0, false, String::new())
}

const GROUP_COLOR: Color32 = Color32::from_rgb(0xF5, 0xA6, 0x23);
