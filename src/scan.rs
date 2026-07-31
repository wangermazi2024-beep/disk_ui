//! 目录扫描入口 + 常规遍历（rayon 并行 + 硬链接去重 + 权限优化）。
//!
//! 参考 WinDirStat FinderBasic + 现代 Rust 并行实践：
//! - rayon 并行处理子目录（I/O bound，线程数 = CPU*2）
//! - dashmap 做多线程安全硬链接去重
//! - 启用 SeBackupPrivilege + SeRestorePrivilege
//! - 权限失败不中断
//! - 压缩/稀疏文件用 GetCompressedFileSizeW
//! - 只对 GetFileInformationByHandle 返回 nLinkCount>1 的文件做去重
//! - 合并系统调用：一次 CreateFileW 同时拿 nLinkCount + FileIndex + 分配大小

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashSet;
use egui::Color32;
use rayon::prelude::*;

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
            let num_threads = num_cpus_get().saturating_mul(2).max(4);
            let _ = rayon::ThreadPoolBuilder::new()
                .num_threads(num_threads)
                .build_global();
            eprintln!("[scan] rayon 线程池: {} 线程", num_threads);

            enable_read_privileges();

            if let Some(drive) = as_drive_root(&root) {
                if crate::mft_scan::is_elevated() {
                    eprintln!("[scan] 走 MFT 直读: drive={}", drive);
                    match crate::mft_scan::scan_volume(drive, &tx) {
                        Ok(mut node) => {
                            if let Some(info) = &disk_info { node.name = info.display_name(); }
                            eprintln!("[scan] MFT 完成: files={}, folders={}, logical={}, physical={}, 耗时 {:.1}s",
                                node.file_count, node.folder_count,
                                crate::format::human_size(node.logical_size),
                                crate::format::human_size(node.physical_size),
                                start.elapsed().unwrap_or_default().as_secs_f64());
                            if let Some(info) = &disk_info {
                                let ratio = if info.used_bytes > 0 { node.physical_size as f64 / info.used_bytes as f64 * 100.0 } else { 0.0 };
                                eprintln!("[scan] 一致性检查: physical={}, 系统已用={}, 比例={:.1}%",
                                    crate::format::human_size(node.physical_size), crate::format::human_size(info.used_bytes), ratio);
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

        // 常规遍历
        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let hardlink_seen: Arc<DashSet<u64>> = Arc::new(DashSet::new());

        match scan_dir(&root, 0, None, &counter, &cancel, &tx, &hardlink_seen) {
            Ok(mut node) => {
                if let Some(info) = &disk_info {
                    #[cfg(windows)]
                    if as_drive_root(&root).is_some() { node.name = info.display_name(); }
                    #[cfg(not(windows))]
                    { node.name = info.display_name(); }
                }
                eprintln!("[scan] 常规遍历完成: files={}, folders={}, logical={}, 耗时 {:.1}s",
                    node.file_count, node.folder_count,
                    crate::format::human_size(node.logical_size),
                    start.elapsed().unwrap_or_default().as_secs_f64());
                let _ = tx.send(ScanMessage::Done(Box::new(node), disk_info));
            }
            Err(e) => {
                eprintln!("[scan] 失败: {e}");
                let _ = tx.send(ScanMessage::Error(format!("扫描失败: {e}")));
            }
        }
    });
}

fn num_cpus_get() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
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

/// 并行递归扫描目录。
///
/// 关键优化：
/// - parent_meta: 父级已经 stat 过的 metadata 传下来复用，避免重复 stat
/// - 硬链接去重：一次 CreateFileW + GetFileInformationByHandle 同时拿 nLinkCount + FileIndex
///   只对 nLinkCount>1 的文件做去重（99% 文件是单链接，零额外开销）
fn scan_dir(
    path: &Path, depth: usize, parent_meta: Option<std::fs::Metadata>,
    counter: &Arc<AtomicU64>, cancel: &Arc<AtomicBool>,
    tx: &Sender<ScanMessage>,
    hardlink_seen: &Arc<DashSet<u64>>,
) -> std::io::Result<Node> {
    const MAX_DEPTH: usize = 256;
    if depth > MAX_DEPTH {
        eprintln!("[scan] 超过最大递归深度 {} (path={})", MAX_DEPTH, path.display());
        return Ok(Node::new_folder(
            path.file_name().map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            folder_color(depth), Vec::new(),
        ));
    }
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

    // 复用父级传下来的 metadata（避免重复 stat）
    let self_meta = parent_meta.or_else(|| std::fs::metadata(path).ok());
    let self_modified = system_time_to_filetime(self_meta.as_ref().and_then(|m| m.modified().ok()));
    #[cfg(windows)]
    let self_attrs = self_meta.as_ref().map(|m| {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes()
    }).unwrap_or(0x10);
    #[cfg(not(windows))]
    let self_attrs: u32 = 0x10;

    let entries: Vec<_> = match std::fs::read_dir(path) {
        Ok(e) => e.flatten().collect(),
        Err(e) => {
            if depth <= 3 {
                eprintln!("[scan] read_dir 失败 (depth={}, path={}, err={})", depth, path.display(), e);
            }
            return Ok(Node::new_folder_with_meta(name, folder_color(depth), Vec::new(), self_modified, 0, 0, self_attrs, 0, false, String::new()));
        }
    };

    let children: Vec<Node> = entries
        .par_iter()
        .filter_map(|entry| {
            let n = counter.fetch_add(1, Ordering::Relaxed);
            if n % 5000 == 0 { let _ = tx.send(ScanMessage::Progress(n)); }

            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => return None,
            };
            let entry_name = entry.file_name().to_string_lossy().into_owned();
            let modified = system_time_to_filetime(meta.modified().ok());
            let child_path = entry.path();

            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                let attrs = meta.file_attributes();
                let mut logical = meta.len();

                // 逻辑大小修正（参考 WinDirStat FinderBasic.cpp:197-208）
                if logical == 0 {
                    logical = get_logical_size_fixup(&child_path);
                }

                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if meta.is_dir() {
                    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                        // 重解析点目录：不递归
                        Some(Node::new_folder_with_meta(
                            entry_name, folder_color(depth + 1), Vec::new(),
                            modified, 0, 0, attrs, 0, false, String::new(),
                        ))
                    } else {
                        // 把已拿到的 meta 传下去，子级不用再 stat 一次
                        match scan_dir(&child_path, depth + 1, Some(meta), counter, cancel, tx, hardlink_seen) {
                            Ok(child) => Some(child),
                            Err(e) => {
                                if depth <= 3 {
                                    eprintln!("[scan] 子目录扫描失败 (path={}, err={})", child_path.display(), e);
                                }
                                None
                            }
                        }
                    }
                } else {
                    // 文件：物理大小 + 硬链接去重（合并到一次系统调用）
                    let physical = compute_physical(&child_path, logical, attrs, hardlink_seen);
                    Some(Node::new_file_with_meta(
                        entry_name, logical, physical, file_color(),
                        modified, 0, 0, attrs, 0, false, String::new(),
                    ))
                }
            }

            #[cfg(not(windows))]
            {
                let (attrs, logical, physical) = (if meta.is_dir() { 0x10 } else { 0x80 }, meta.len(), meta.len());
                if meta.is_dir() {
                    match scan_dir(&child_path, depth + 1, Some(meta), counter, cancel, tx, hardlink_seen) {
                        Ok(child) => Some(child),
                        Err(_) => None,
                    }
                } else {
                    Some(Node::new_file_with_meta(entry_name, logical, physical, file_color(), modified, 0, 0, attrs, 0, false, String::new()))
                }
            }
        })
        .collect();

    Ok(Node::new_folder_with_meta(name, folder_color(depth), children, self_modified, 0, 0, self_attrs, 0, false, String::new()))
}

/// 计算文件物理大小 + 硬链接去重。
///
/// 关键优化：只对需要去重的文件（压缩/稀疏 或 可能硬链接）才打开句柄。
/// 普通文件用簇对齐估算，零额外系统调用。
#[cfg(windows)]
fn compute_physical(
    path: &Path, logical: u64, attrs: u32,
    hardlink_seen: &Arc<DashSet<u64>>,
) -> u64 {
    const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x800;
    const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;

    // 压缩/稀疏文件：必须用 GetCompressedFileSizeW（需要路径，但只对少数文件）
    if attrs & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_SPARSE_FILE) != 0 {
        let phys = get_compressed_size(path);
        if phys > 0 {
            // 压缩文件也可能有硬链接，但概率极低，跳过去重避免额外开销
            return phys;
        }
    }

    // 普通文件：簇对齐估算（零系统调用）
    let cluster = get_cluster_size(path);
    let physical = if logical == 0 { 0 } else { ((logical + cluster - 1) / cluster) * cluster };
    if physical == 0 { return 0; }

    // 硬链接去重：只对大文件（>1MB）才查 FileIndex
    // 小文件硬链接去重收益极低，但 CreateFileW 开销固定
    if logical < 1_048_576 {
        return physical; // 小文件跳过去重
    }

    // 一次 CreateFileW + GetFileInformationByHandle 拿 nLinkCount + FileIndex
    if let Some((nlinks, key)) = get_file_info_for_dedup(path) {
        if nlinks > 1 {
            if hardlink_seen.insert(key) {
                physical
            } else {
                0
            }
        } else {
            physical
        }
    } else {
        physical
    }
}

/// 用 GetCompressedFileSizeW 获取压缩/稀疏文件的真实占用。
#[cfg(windows)]
fn get_compressed_size(path: &Path) -> u64 {
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;
    let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str())
        .chain(std::iter::once(0)).collect();
    let mut high: u32 = 0;
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    if low != 0xFFFFFFFF || unsafe { windows_sys::Win32::Foundation::GetLastError() } == 0 {
        ((high as u64) << 32) | (low as u64)
    } else {
        0
    }
}

/// 参考 WinDirStat FinderBasic.cpp:197-208：
/// 如果 EndOfFile==0 但文件可能有分配空间，用 GetFileSizeEx 修正逻辑大小。
#[cfg(windows)]
fn get_logical_size_fixup(path: &Path) -> u64 {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileSizeEx, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_SHARE_DELETE,
        OPEN_EXISTING, FILE_READ_ATTRIBUTES,
    };

    let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str())
        .chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(), FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE { return 0; }
    let mut size: i64 = 0;
    let ok = unsafe { GetFileSizeEx(handle, &mut size) };
    unsafe { CloseHandle(handle); }
    if ok != 0 && size > 0 { size as u64 } else { 0 }
}

/// 获取路径所在卷的簇大小（参考 WinDirStat FinderBasic.cpp:64-68）。
#[cfg(windows)]
fn get_cluster_size(path: &Path) -> u64 {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceW;
    let root = path.to_string_lossy();
    let root_path: String = if root.len() >= 2 && root.as_bytes()[1] == b':' {
        format!("{}:\\", root.chars().next().unwrap())
    } else {
        return 4096;
    };
    let wide: Vec<u16> = root_path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sectors_per_cluster: u32 = 0;
    let mut bytes_per_sector: u32 = 0;
    let mut free_clusters: u32 = 0;
    let mut total_clusters: u32 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceW(wide.as_ptr(), &mut sectors_per_cluster, &mut bytes_per_sector,
            &mut free_clusters, &mut total_clusters)
    };
    if ok != 0 && sectors_per_cluster > 0 && bytes_per_sector > 0 {
        (sectors_per_cluster as u64) * (bytes_per_sector as u64)
    } else {
        4096
    }
}

/// 用 GetFileInformationByHandle 拿 nNumberOfLinks + FileIndex。
/// 只对大文件（>1MB）调用，避免 99% 小文件的额外开销。
#[cfg(windows)]
fn get_file_info_for_dedup(path: &Path) -> Option<(u32, u64)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING, FILE_READ_ATTRIBUTES,
    };

    let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str())
        .chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(), FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE { return None; }

    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    unsafe { CloseHandle(handle); }

    if ok == 0 { return None; }

    let nlinks = info.nNumberOfLinks;
    let file_index = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
    let key = ((info.dwVolumeSerialNumber as u64) << 32) | (file_index & 0xFFFFFFFF);
    Some((nlinks, key))
}

/// 启用 SeBackupPrivilege + SeRestorePrivilege。
#[cfg(windows)]
fn enable_read_privileges() {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LUID};
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW,
        SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_QUERY,
        TOKEN_PRIVILEGES, LUID_AND_ATTRIBUTES,
        SE_BACKUP_NAME, SE_RESTORE_NAME,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token) == 0 {
            return;
        }
        for priv_name in [SE_BACKUP_NAME, SE_RESTORE_NAME] {
            let mut luid = LUID { LowPart: 0, HighPart: 0 };
            if LookupPrivilegeValueW(std::ptr::null(), priv_name, &mut luid) == 0 { continue; }
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
            };
            AdjustTokenPrivileges(
                token, 0, &tp as *const _ as *const TOKEN_PRIVILEGES,
                std::mem::size_of::<TOKEN_PRIVILEGES>() as u32,
                std::ptr::null_mut(), std::ptr::null_mut(),
            );
        }
        CloseHandle(token);
    }
    eprintln!("[scan] 已尝试启用 SeBackupPrivilege + SeRestorePrivilege");
}
