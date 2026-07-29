//! 目录扫描。

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

const MAX_ENTRIES: u64 = 300_000;

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

/// 是否是形如 `C:\` / `C:/` 的整盘根路径。只有这种情况才有资格走 MFT 直读路径——
/// 扫描某个子目录时 MFT 直读拿到的是全盘数据，裁剪出子树意义不大，直接走传统遍历更简单可靠。
#[cfg(windows)]
fn as_drive_root(path: &Path) -> Option<char> {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    if bytes.len() == 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
        let c = bytes[0] as char;
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase());
        }
    }
    None
}

pub fn spawn_scan(root: PathBuf, tx: Sender<ScanMessage>) {
    std::thread::spawn(move || {
        // Windows 下，如果目标是一个整盘根目录（如 C:\），且当前有管理员权限、
        // 目标卷是 NTFS，就优先走 $MFT 直读快速路径（WizTree/Everything 同款原理）。
        // 任何一步失败（没权限/非NTFS/记录损坏等）都直接 fallback 回标准目录遍历，
        // 保证功能上始终可用，只是速度上有快慢之分。
        #[cfg(windows)]
        {
            if let Some(drive) = as_drive_root(&root) {
                if crate::mft_scan::mft_scan_available(drive) {
                    match crate::mft_scan::scan_drive_via_mft(drive, &tx) {
                        Ok(result) => {
                            let _ = tx.send(ScanMessage::Done(Box::new(result.root)));
                            return;
                        }
                        Err(e) => {
                            // 不中断，退回传统遍历；把原因打到 stderr 方便排查，
                            // 不打断 UI（UI 只关心最终能不能拿到数据）。
                            eprintln!("MFT 直读失败，回退到标准目录遍历: {e}");
                        }
                    }
                }
            }
        }

        let counter = Arc::new(AtomicU64::new(0));
        match scan_dir(&root, 0, &counter, &tx) {
            Ok(node) => { let _ = tx.send(ScanMessage::Done(Box::new(node))); }
            Err(e)   => { let _ = tx.send(ScanMessage::Error(format!("扫描失败: {e}"))); }
        }
    });
}

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
    for entry in std::fs::read_dir(path)?.flatten() {
        if counter.load(Ordering::Relaxed) > MAX_ENTRIES { break; }
        let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
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

/// 演示数据：C 盘 + D 盘两个分区，各自是独立的根节点。
pub fn demo_partitions() -> Vec<Node> {
    let leaf = |name: &str, size: u64| Node::new_file(name, size, file_color());

    // ── C 盘 ──────────────────────────────────────────────────────
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

    let users_c = Node::new_folder("Users", folder_color(1), vec![
        Node::new_folder("Alex", folder_color(2), vec![
            Node::new_folder("AppData", folder_color(3), vec![
                Node::new_folder("Temp", folder_color(4), vec![
                    leaf("cache.tmp", 1_100_000_000),
                ]),
            ]),
            Node::new_folder("Documents", folder_color(3), vec![
                leaf("thesis.docx", 4_200_000),
            ]),
        ]),
    ]);

    let c_drive = Node::new_folder("C:\\  系统", folder_color(0), vec![
        windows,
        program_files,
        users_c,
        leaf("pagefile.sys", 16_000_000_000),
        leaf("hiberfil.sys", 8_000_000_000),
    ]);

    // ── D 盘 ──────────────────────────────────────────────────────
    let steam = Node::new_folder("Steam", folder_color(1), vec![
        Node::new_folder("steamapps", folder_color(2), vec![
            Node::new_folder("common", folder_color(3), vec![
                Node::new_folder("Cyberpunk2077", folder_color(4), vec![
                    leaf("archive.pak", 68_000_000_000),
                ]),
                Node::new_folder("Elden Ring", folder_color(4), vec![
                    leaf("data.bin", 45_000_000_000),
                ]),
                Node::new_folder("GTA V", folder_color(4), vec![
                    leaf("update.rpf", 36_000_000_000),
                ]),
            ]),
        ]),
    ]);

    let downloads = Node::new_folder("Downloads", folder_color(1), vec![
        leaf("movie_4k.mkv", 18_000_000_000),
        leaf("backup_2024.zip", 9_500_000_000),
        leaf("installer.iso", 4_700_000_000),
    ]);

    let projects = Node::new_folder("Projects", folder_color(1), vec![
        Node::new_folder("my-app", folder_color(2), vec![
            Node::new_folder("node_modules", folder_color(3), vec![
                leaf("packages...", 2_800_000_000),
            ]),
            leaf("dist.tar.gz", 450_000_000),
        ]),
    ]);

    let d_drive = Node::new_folder("D:\\  软件", folder_color(0), vec![
        steam,
        downloads,
        projects,
    ]);

    vec![c_drive, d_drive]
}

/// 兼容旧调用：返回单个 demo 节点（仅 C 盘）。
pub fn demo_tree() -> Node {
    demo_partitions().remove(0)
}
