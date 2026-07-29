//! 磁盘扫描。
//!
//! 使用双策略：
//! 1. **MFT 直读**（NTFS 专属，需管理员权限）—— 通过 `mft` crate 解析 \$MFT，秒级扫描整个分区。
//! 2. **传统 API 遍历**（fallback）—— 无管理员权限时使用 `std::fs::read_dir` 逐目录递归。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use egui::Color32;

use crate::model::Node;

pub enum ScanMessage {
    Progress(u64),
    Done(Box<Node>),
    Error(String),
}

const MAX_ENTRIES: u64 = 1_000_000;

/// 启动扫描线程。
/// `path` 是被扫描的根路径（如 `C:\`）。
pub fn spawn_scan(path: PathBuf, tx: Sender<ScanMessage>) {
    std::thread::spawn(move || {
        // 先尝试 MFT 直读（仅适用于 NTFS 卷）
        if path.starts_with(r"C:\") || path.starts_with("C:") || path.starts_with(r"D:\") || path.starts_with("D:") {
            let drive_letter = path.to_string_lossy().chars().next().unwrap_or('C');
            let mft_path = format!(r"\\.\{}:\$MFT", drive_letter);
            match scan_via_mft(&mft_path, &tx) {
                Ok(node) => {
                    let _ = tx.send(ScanMessage::Done(Box::new(node)));
                    return;
                }
                Err(e) => {
                    // MFT 读取失败，降级到传统遍历
                    let _ = tx.send(ScanMessage::Progress(0));
                    eprintln!("MFT 扫描失败 ({}), 降级到传统遍历", e);
                }
            }
        }

        // Fallback: 传统目录遍历
        let counter = Arc::new(AtomicU64::new(0));
        match scan_dir(&path, 0, &counter, &tx) {
            Ok(node) => { let _ = tx.send(ScanMessage::Done(Box::new(node))); }
            Err(e)   => { let _ = tx.send(ScanMessage::Error(format!("扫描失败: {e}"))); }
        }
    });
}

// ── MFT 直读 ──────────────────────────────────────────────────────────
// 仅在 Windows NTFS 卷上可用，需要管理员权限。

/// 通过 `mft`  crate 直接解析 `$MFT`，重建目录树。
#[cfg(windows)]
fn scan_via_mft(mft_path: &str, tx: &Sender<ScanMessage>) -> Result<Node, Box<dyn std::error::Error>> {
    use mft::MftParser;

    let _ = tx.send(ScanMessage::Progress(1));

    // 打开 \$MFT 文件（需要管理员权限）
    let mut parser = MftParser::from_path(mft_path)?;
    let _ = tx.send(ScanMessage::Progress(2));

    // 第一遍：收集所有有效条目
    // key = MFT record number, value = parsed entry data
    struct RawEntry {
        record_number: u64,
        parent_record: u64,
        name: String,
        size: u64,
        is_dir: bool,
    }

    let mut entries: Vec<RawEntry> = Vec::new();
    let mut total = 0u64;

    for result in parser.iter_entries() {
        total += 1;
        if total % 100_000 == 0 {
            let _ = tx.send(ScanMessage::Progress(total));
        }
        if total > MAX_ENTRIES * 10 {
            break; // 防止无限增长
        }

        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };

        // 只处理已分配的条目（跳过已删除的）
        if !entry.is_allocated() {
            continue;
        }

        let attr = match entry.find_best_name_attribute() {
            Some(a) => a,
            None => continue,
        };

        // 跳过特殊系统文件（$MFT, $Secure 等）
        let name = attr.name.trim().to_string();
        if name.is_empty() || name.starts_with('$') {
            continue;
        }

        // 特殊目录: 卷根目录 "." 需要处理 - 它的 parent_record 指向自己
        // 卷根目录的 name 可能为空或 "."，我们用驱动器号代替
        entries.push(RawEntry {
            record_number: entry.header.record_number,
            parent_record: attr.parent.entry,
            name,
            size: if entry.is_dir() { 0 } else { attr.logical_size },
            is_dir: entry.is_dir(),
        });
    }

    let _ = tx.send(ScanMessage::Progress(total / 2 + 1));

    // 重建树结构
    // 用 HashMap 建立 record_number -> children 的映射
    // 根节点是所有 parent_record 不在 entries 中的条目（它们的父节点在树外）
    let mut children_of: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut root_indices: Vec<usize> = Vec::new();

    // 先收集所有 record_number
    let record_numbers: Vec<u64> = entries.iter().map(|e| e.record_number).collect();
    let rec_set: std::collections::HashSet<u64> = record_numbers.iter().cloned().collect();

    for (i, entry) in entries.iter().enumerate() {
        if entry.parent_record == entry.record_number {
            // 自引用 = 卷根目录
            root_indices.push(i);
        } else if !rec_set.contains(&entry.parent_record) {
            // 父节点不在当前集合中，也是根
            root_indices.push(i);
        } else {
            children_of.entry(entry.parent_record).or_default().push(i);
        }
    }

    let _ = tx.send(ScanMessage::Progress(total / 2 + 2));

    // 递归构建 Node 树
    fn build_node(
        raw: &[RawEntry],
        children_of: &HashMap<u64, Vec<usize>>,
        idx: usize,
        depth: usize,
    ) -> Node {
        let entry = &raw[idx];
        let mut node = if entry.is_dir {
            let mut children = Vec::new();
            if let Some(child_indices) = children_of.get(&entry.record_number) {
                for &ci in child_indices {
                    children.push(build_node(raw, children_of, ci, depth + 1));
                }
            }
            Node::new_folder(&entry.name, folder_color(depth), children)
        } else {
            Node::new_file(&entry.name, entry.size, file_color())
        };
        // 根目录默认展开
        if depth == 0 {
            node.expanded = true;
        }
        node
    }

    // 合并多个根节点到一个虚拟根
    if root_indices.len() == 1 {
        Ok(build_node(&entries, &children_of, root_indices[0], 0))
    } else if root_indices.is_empty() {
        Err("未找到任何文件条目".into())
    } else {
        let mut children = Vec::new();
        for &ri in &root_indices {
            children.push(build_node(&entries, &children_of, ri, 1));
        }
        let drive_letter = std::path::Path::new(mft_path)
            .to_string_lossy()
            .chars()
            .next()
            .unwrap_or('C');
        Ok(Node::new_folder(format!("{}:\\\\", drive_letter), folder_color(0), children))
    }
}

#[cfg(not(windows))]
fn scan_via_mft(_mft_path: &str, _tx: &Sender<ScanMessage>) -> Result<Node, Box<dyn std::error::Error>> {
    Err("MFT 扫描仅在 Windows 上可用".into())
}

// ── 传统 API 遍历（fallback） ────────────────────────────────────────

fn scan_dir(
    path: &Path,
    depth: usize,
    counter: &Arc<AtomicU64>,
    tx: &Sender<ScanMessage>,
) -> std::io::Result<Node> {
    let name = if depth == 0 {
        path.to_string_lossy().into_owned()
    } else {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    };

    let mut children = Vec::new();
    let dir = match std::fs::read_dir(path) {
        Ok(d) => d,
        Err(e) => {
            // 权限拒绝等错误跳过该目录
            eprintln!("跳过 {}: {}", path.display(), e);
            return Ok(Node::new_folder(name, folder_color(depth), vec![]));
        }
    };

    for entry in dir.flatten() {
        if counter.load(Ordering::Relaxed) > MAX_ENTRIES { break; }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let entry_name = entry.file_name().to_string_lossy().into_owned();
        let n = counter.fetch_add(1, Ordering::Relaxed);
        if n % 500 == 0 { let _ = tx.send(ScanMessage::Progress(n)); }

        if meta.is_dir() {
            if let Ok(child) = scan_dir(&entry.path(), depth + 1, counter, tx) {
                children.push(child);
            }
        } else {
            children.push(Node::new_file(entry_name, meta.len(), file_color()));
        }
    }
    Ok(Node::new_folder(name, folder_color(depth), children))
}

// ── 颜色 ─────────────────────────────────────────────────────────────

fn folder_color(depth: usize) -> Color32 {
    const PALETTE: [Color32; 6] = [
        Color32::from_rgb(0x4C, 0x8B, 0xF5),
        Color32::from_rgb(0x34, 0xC7, 0x59),
        Color32::from_rgb(0xF5, 0xA6, 0x23),
        Color32::from_rgb(0xE0, 0x55, 0x5B),
        Color32::from_rgb(0x9C, 0x6A, 0xDE),
        Color32::from_rgb(0x2E, 0xC4, 0xB6),
    ];
    PALETTE[depth % PALETTE.len()]
}

fn file_color() -> Color32 {
    Color32::from_rgb(0x6C, 0x75, 0x7D)
}

// ── 演示数据 ──────────────────────────────────────────────────────────

/// 演示数据：C 盘 + D 盘两个分区，各自是独立的根节点。
pub fn demo_partitions() -> Vec<Node> {
    let leaf = |name: &str, size: u64| Node::new_file(name, size, file_color());

    let windows = Node::new_folder("Windows", folder_color(1), vec![
        Node::new_folder("System32", folder_color(2), vec![
            leaf("ntoskrnl.exe", 11_200_000),
            leaf("kernel32.dll", 780_000),
            leaf("drivers.cab", 640_000_000),
        ]),
        Node::new_folder("WinSxS", folder_color(2), vec![
            leaf("manifest_a.cat", 2_100_000_000),
            leaf("manifest_b.cat", 1_800_000_000),
        ]),
        leaf("explorer.exe", 5_400_000),
    ]);

    let program_files = Node::new_folder("Program Files", folder_color(1), vec![
        Node::new_folder("Adobe", folder_color(2), vec![
            leaf("Photoshop.exe", 2_300_000_000),
            leaf("Premiere.exe", 3_100_000_000),
        ]),
        Node::new_folder("Microsoft Office", folder_color(2), vec![
            leaf("WINWORD.EXE", 890_000_000),
            leaf("EXCEL.EXE", 760_000_000),
        ]),
    ]);

    let users = Node::new_folder("Users", folder_color(1), vec![
        Node::new_folder("Default", folder_color(2), vec![
            Node::new_folder("AppData", folder_color(3), vec![
                Node::new_folder("Temp", folder_color(4), vec![
                    leaf("cache.tmp", 1_100_000_000),
                ]),
            ]),
        ]),
    ]);

    let c_drive = Node::new_folder("C:\\\\  系统", folder_color(0), vec![
        windows, program_files, users,
        leaf("pagefile.sys", 16_000_000_000),
        leaf("hiberfil.sys", 8_000_000_000),
    ]);

    let d_drive = Node::new_folder("D:\\\\  软件", folder_color(0), vec![
        Node::new_folder("Steam", folder_color(1), vec![
            leaf("steamapps", 0),
        ]),
        Node::new_folder("Downloads", folder_color(1), vec![
            leaf("movie_4k.mkv", 18_000_000_000),
        ]),
    ]);

    vec![c_drive, d_drive]
}

/// 兼容旧调用：返回单个 demo 节点（仅 C 盘）。
pub fn demo_tree() -> Node {
    demo_partitions().remove(0)
}
