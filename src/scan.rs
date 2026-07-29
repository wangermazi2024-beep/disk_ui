//! 磁盘扫描。
//!
//! 使用 `jwalk` 并行目录遍历，比单线程 `std::fs::read_dir` 快 3-5 倍。

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

/// 最多扫描多少条目（防止内存爆炸）。
const MAX_ENTRIES: u64 = 5_000_000;

/// 启动扫描线程。
/// `path` 是被扫描的根路径（如 `C:\`）。
pub fn spawn_scan(path: PathBuf, tx: Sender<ScanMessage>) {
    std::thread::spawn(move || {
        let counter = Arc::new(AtomicU64::new(0));
        match scan_dir(&path, &counter, &tx) {
            Ok(node) => { let _ = tx.send(ScanMessage::Done(Box::new(node))); }
            Err(e)   => { let _ = tx.send(ScanMessage::Error(format!("扫描失败: {e}"))); }
        }
    });
}

/// 使用 `jwalk` 并行遍历目录，递归构建 Node 树。
fn scan_dir(
    path: &Path,
    counter: &Arc<AtomicU64>,
    tx: &Sender<ScanMessage>,
) -> std::io::Result<Node> {
    let name = path.to_string_lossy().into_owned();

    let mut children = Vec::new();
    // jwalk 自动多线程遍历
    let entries: Vec<_> = jwalk::WalkDir::new(path)
        .max_depth(1)
        .sort(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.depth() == 1)
        .collect();

    for entry in entries {
        if counter.load(Ordering::Relaxed) > MAX_ENTRIES { break; }
        let ft = entry.file_type();
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let n = counter.fetch_add(1, Ordering::Relaxed);
        if n % 1000 == 0 { let _ = tx.send(ScanMessage::Progress(n)); }

        if ft.is_dir() {
            if let Ok(child) = scan_dir(&entry.path(), counter, tx) {
                children.push(child);
            }
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            children.push(Node::new_file(file_name, size, file_color()));
        }
    }
    Ok(Node::new_folder(name, folder_color(0), children))
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

pub fn demo_tree() -> Node {
    demo_partitions().remove(0)
}
