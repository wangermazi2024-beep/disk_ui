//! 按文件类型统计大小（对应 WizTree/TreeSize 左侧的"按类型"视图）。
//!
//! 原来这份数据是写死的示例数字，和 `nodes` 完全脱节。
//! 现在改成真正遍历一遍树、按扩展名分类累加，这样左侧统计和
//! treemap/文件树三处展示的是同一份数据，不会出现"改一处忘了改另一处"的耦合问题。

use egui::Color32;

use crate::model::{Node, NodeKind};

const LABELS: [&str; 6] = ["视频", "压缩包", "程序/exe", "文档", "图片", "其他"];
const COLORS: [Color32; 6] = [
    Color32::from_rgb(0xE0, 0x55, 0x5B), // 视频
    Color32::from_rgb(0xF5, 0xA6, 0x23), // 压缩包
    Color32::from_rgb(0x4C, 0x8B, 0xF5), // 程序/exe
    Color32::from_rgb(0x34, 0xC7, 0x59), // 文档
    Color32::from_rgb(0x9C, 0x6A, 0xDE), // 图片
    Color32::from_rgb(0x6C, 0x75, 0x7D), // 其他
];

fn classify_ext(name: &str) -> usize {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv") => 0,
        Some("zip" | "rar" | "7z" | "tar" | "gz" | "xz") => 1,
        Some("exe" | "msi" | "dll" | "bat" | "sh" | "app") => 2,
        Some("doc" | "docx" | "pdf" | "txt" | "md" | "xlsx" | "pptx") => 3,
        Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg") => 4,
        _ => 5,
    }
}

/// 深度优先遍历整棵树，把所有"文件"叶子节点按扩展名归类累加。
fn accumulate(node: &Node, totals: &mut [u64; 6]) {
    match node.kind {
        NodeKind::File => {
            totals[classify_ext(&node.name)] += node.size;
        }
        NodeKind::Folder => {
            for c in &node.children {
                accumulate(c, totals);
            }
        }
    }
}

pub fn compute_categories(root: &Node) -> Vec<crate::model::CategoryStat> {
    let mut totals = [0u64; 6];
    accumulate(root, &mut totals);
    (0..6)
        .map(|i| crate::model::CategoryStat {
            label: LABELS[i],
            size: totals[i],
            color: COLORS[i],
        })
        .collect()
}
