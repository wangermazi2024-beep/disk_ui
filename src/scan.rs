//! 目录扫描入口 + 常规遍历 fallback（参考 WinDirStat FinderBasic）。
//!
//! WinDirStat 的常规遍历用 NtQueryDirectoryFile（比 FindFirstFile/FindNextFile 快），
//! 并且对权限问题做了容错：
//! - 用 FILE_OPEN_FOR_BACKUP_INTENT 打开目录（允许读受保护目录）
//! - 对物理大小为 0 的文件用 GetCompressedFileSize 修正
//! - 对逻辑大小为 0 但 AllocationSize>0 的文件用 GetFileSizeEx 修正
//! - 启用 SeBackupPrivilege + SeRestorePrivilege（即使非管理员也能读更多文件）

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use egui::Color32;

use crate::disk_info::DiskInfo;
use crate::model::Node;

pub enum ScanMessage {
    Progress(u64),
    Done(Box<Node>, Option<DiskInfo>),
    Error(String),
}

fn folder_color(depth: usize) -> Color32 {
    const PAL: [Color32; 6] = [
        Color32::from_rgb(0x4C, 0x8B, 0xF5), Color32::from_rgb(0x34, 0xC7, 0x59),
        Color32::from_rgb(0xF5, 0xA6, 0x23), Color32::from_rgb(0xE0, 0x55, 0x5B),
        Color32::from_rgb(0x9C, 0x6A, 0xDE), Color32::from_rgb(0x2E, 0xC4, 0xB6),
    ];
    PAL[depth % PAL.len()]
}
fn file_color() -> Color32 { Color32::from_rgb(0x6C, 0x75, 0x7D) }

fn drive_letter_of(path: &Path) -> Option<char> {
    path.to_string_lossy().chars().next()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
}

pub fn spawn_scan(root: PathBuf, tx: Sender<ScanMessage>) {
    std::thread::spawn(move || {
        let start = SystemTime::now();
        let disk_info = drive_letter_of(&root).and_then(crate::disk_info::query_disk_info);
        eprintln!("[scan] 启动: root={}", root.display());

        #[cfg(windows)]
        {
            if let Some(drive) = as_drive_root(&root) {
                if crate::mft_scan::is_elevated() {
                    eprintln!("[scan] 走 MFT 直读: drive={}", drive);
                    match crate::mft_scan::scan_volume(drive, &tx) {
                        Ok(mut node) => {
                            if let Some(info) = &disk_info {
                                node.name = info.display_name();
                            }
                            eprintln!(
                                "[scan] MFT 完成: files={}, folders={}, logical={}, physical={}, 耗时 {:.1}s",
                                node.file_count, node.folder_count,
                                crate::format::human_size(node.logical_size),
                                crate::format::human_size(node.physical_size),
                                start.elapsed().unwrap_or_default().as_secs_f64()
                            );
                            if let Some(info) = &disk_info {
                                let ratio = if info.used_bytes > 0 {
                                    node.physical_size as f64 / info.used_bytes as f64 * 100.0
                                } else { 0.0 };
                                eprintln!("[scan] 一致性检查: physical={}, 系统已用={}, 比例={:.1}%",
                                    crate::format::human_size(node.physical_size),
                                    crate::format::human_size(info.used_bytes), ratio);
                                if ratio < 60.0 { eprintln!("[scan] ⚠ 可能丢数据"); }
                                else if ratio > 105.0 { eprintln!("[scan] ⚠ 可能含 ADS 或压缩差异"); }
                                else { eprintln!("[scan] ✓ 在合理范围内"); }
                            }
                            let _ = tx.send(ScanMessage::Done(Box::new(node), disk_info));
                            return;
                        }
                        Err(e) => eprintln!("[scan] MFT 失败，回退常规遍历: {e}"),
                    }
                } else {
                    eprintln!("[scan] 非管理员，走常规遍历: drive={}", drive);
                }
            }
        }

        // 常规遍历 fallback
        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        match scan_dir(&root, 0, &counter, &cancel, &tx) {
            Ok(mut node) => {
                if let Some(info) = &disk_info {
                    #[cfg(windows)]
                    if as_drive_root(&root).is_some() { node.name = info.display_name(); }
                    #[cfg(not(windows))]
                    { node.name = info.display_name(); }
                }
                eprintln!("[scan] 常规遍历完成: files={}, folders={}, logical={}",
                    node.file_count, node.folder_count, crate::format::human_size(node.logical_size));
                let _ = tx.send(ScanMessage::Done(Box::new(node), disk_info));
            }
            Err(e) => {
                eprintln!("[scan] 失败: {e}");
                let _ = tx.send(ScanMessage::Error(format!("扫描失败: {e}")));
            }
        }
        eprintln!("[scan] 总耗时: {:.1}s", start.elapsed().unwrap_or_default().as_secs_f64());
    });
}

#[cfg(windows)]
fn as_drive_root(path: &Path) -> Option<char> {
    let s = path.to_string_lossy();
    let b = s.as_bytes();
    if b.len() == 3 && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') {
        let c = b[0] as char;
        if c.is_ascii_alphabetic() { return Some(c.to_ascii_uppercase()); }
    }
    None
}

fn system_time_to_filetime(t: Option<SystemTime>) -> u64 {
    let t = match t { Some(t) => t, None => return 0 };
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            const OFFSET: u64 = 11_644_473_600;
            d.as_secs() * 10_000_000 + (d.subsec_nanos() / 100) as u64 + OFFSET * 10_000_000
        }
        Err(_) => 0,
    }
}

/// 常规遍历：递归扫描目录树。
///
/// 参考 WinDirStat FinderBasic 的设计：
/// - read_dir 失败不中断（权限拒绝等），当作空目录返回
/// - 单个文件 metadata 失败不中断，跳过该文件
/// - 子目录扫描失败不中断父目录
/// - 物理大小 = logical_size（常规遍历不区分，MFT 路径才准）
fn scan_dir(
    path: &Path, depth: usize,
    counter: &Arc<AtomicU64>, cancel: &Arc<AtomicBool>,
    tx: &Sender<ScanMessage>,
) -> std::io::Result<Node> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(Node::new_folder(
            path.file_name().map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            folder_color(depth), Vec::new(),
        ));
    }
    let name = if depth == 0 {
        path.to_string_lossy().into_owned()
    } else {
        path.file_name().map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    };

    // 拿本目录自身的元数据
    let self_meta = std::fs::metadata(path).ok();
    let self_modified = system_time_to_filetime(self_meta.as_ref().and_then(|m| m.modified().ok()));
    #[cfg(windows)]
    let self_attrs = self_meta.as_ref().map(|m| {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes()
    }).unwrap_or(0x10);
    #[cfg(not(windows))]
    let self_attrs: u32 = 0x10;

    let mut children = Vec::new();
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            if depth <= 3 {
                eprintln!("[scan] read_dir 失败 (depth={}, path={}, err={})", depth, path.display(), e);
            }
            return Ok(Node::new_folder_with_meta(name, folder_color(depth), Vec::new(), self_modified, 0, 0, self_attrs, 0, false, String::new()));
        }
    };

    for entry in entries.flatten() {
        let n = counter.fetch_add(1, Ordering::Relaxed);
        if n % 5000 == 0 { let _ = tx.send(ScanMessage::Progress(n)); }
        let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
        let entry_name = entry.file_name().to_string_lossy().into_owned();
        let modified = system_time_to_filetime(meta.modified().ok());
        #[cfg(windows)]
        let (attrs, logical, physical) = {
            use std::os::windows::fs::MetadataExt;
            let attrs = meta.file_attributes();
            let logical = meta.len();
            // 物理大小：对压缩文件用 GetCompressedFileSize，否则用 allocated
            let physical = get_physical_size(&entry.path(), logical, attrs);
            (attrs, logical, physical)
        };
        #[cfg(not(windows))]
        let (attrs, logical, physical) = (if meta.is_dir() { 0x10 } else { 0x80 }, meta.len(), meta.len());

        if meta.is_dir() {
            if let Ok(child) = scan_dir(&entry.path(), depth + 1, counter, cancel, tx) {
                children.push(child);
            }
        } else {
            children.push(Node::new_file_with_meta(entry_name, logical, physical, file_color(), modified, 0, 0, attrs, 0, false, String::new()));
        }
    }
    Ok(Node::new_folder_with_meta(name, folder_color(depth), children, self_modified, 0, 0, self_attrs, 0, false, String::new()))
}

/// Windows 下获取文件的物理大小（占用空间）。
/// 参考 WinDirStat FinderBasic.cpp:177-194：
/// - 如果 AllocationSize==0 且文件较大或压缩/稀疏，用 GetCompressedFileSize 修正
/// - 否则用 metadata 的 allocated 大小
#[cfg(windows)]
fn get_physical_size(path: &Path, logical: u64, attrs: u32) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;

    const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x800;
    const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;

    // 对压缩/稀疏文件，用 GetCompressedFileSize 拿真实占用
    if attrs & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_SPARSE_FILE) != 0 {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut high: u32 = 0;
        let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
        if low != 0xFFFFFFFF || unsafe { windows_sys::Win32::Foundation::GetLastError() } == 0 {
            return ((high as u64) << 32) | (low as u64);
        }
    }
    // 普通文件：logical 向上对齐到簇大小（4096）
    let cluster = 4096u64;
    ((logical + cluster - 1) / cluster) * cluster
}
