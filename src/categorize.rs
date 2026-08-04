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

/// 按扩展名统计每种后缀总共占了多少空间、多少个文件。按总大小从大到小排序。
/// 迭代遍历（显式栈），不管树多深都不会栈溢出。
pub fn compute_extension_breakdown(root: &Node) -> Vec<crate::model::ExtensionStat> {
    use std::collections::HashMap;
    let mut totals: HashMap<String, (u64, u64)> = HashMap::new(); // ext -> (size, count)
    let mut stack: Vec<&Node> = vec![root];
    while let Some(cur) = stack.pop() {
        match cur.kind {
            NodeKind::File => {
                let ext = cur.name.rsplit_once('.')
                    .filter(|(base, e)| !base.is_empty() && !e.is_empty()) // 处理 ".gitignore" 这类"隐藏点文件"，不当成扩展名
                    .map(|(_, e)| e.to_ascii_lowercase())
                    .unwrap_or_else(|| "（无扩展名）".to_string());
                let entry = totals.entry(ext).or_insert((0, 0));
                entry.0 += cur.logical_size;
                entry.1 += 1;
            }
            NodeKind::Folder => stack.extend(cur.children.iter()),
        }
    }
    let mut result: Vec<crate::model::ExtensionStat> = totals.into_iter()
        .map(|(ext, (size, count))| crate::model::ExtensionStat { ext, size, count })
        .collect();
    result.sort_by(|a, b| b.size.cmp(&a.size));
    result
}

/// 按文件大小分组找候选重复文件（同一个大小、出现在 2 个或以上不同位置）。
/// 只按大小分组，不读文件内容比对——这是最便宜的初筛，大小相同不代表内容一定相同，
/// 界面上会标"候选"。按"潜在可省空间"（size × (count-1)）从大到小排序，最值得关注的排前面。
/// 迭代遍历（显式栈），路径随栈一起带着走，不用回头拼接。
pub fn compute_duplicate_candidates(root: &Node, root_path: &str) -> Vec<crate::model::DuplicateGroup> {
    use std::collections::HashMap;
    let mut groups: HashMap<u64, Vec<String>> = HashMap::new();
    let base = root_path.trim_end_matches('\\');
    let mut stack: Vec<(&Node, String)> = vec![(root, base.to_string())];
    while let Some((cur, path)) = stack.pop() {
        match cur.kind {
            NodeKind::File => {
                if cur.logical_size > 0 {
                    // 0 字节文件到处都是、比较没有意义，跳过，避免"候选"列表被一堆空文件淹没
                    groups.entry(cur.logical_size).or_default().push(path);
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
    let mut result: Vec<crate::model::DuplicateGroup> = groups.into_iter()
        .filter(|(_, paths)| paths.len() >= 2)
        .map(|(size, paths)| crate::model::DuplicateGroup { size, paths })
        .collect();
    result.sort_by(|a, b| {
        let wa = a.size * (a.paths.len() as u64 - 1);
        let wb = b.size * (b.paths.len() as u64 - 1);
        wb.cmp(&wa)
    });
    result
}
