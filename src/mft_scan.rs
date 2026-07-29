//! 直接读取 NTFS `$MFT`（Master File Table）来枚举整个卷的文件/文件夹，
//! 原理和 WizTree / Everything 一致：
//!
//!   1. 用 `FSCTL_GET_NTFS_VOLUME_DATA` 拿到该卷每条 MFT 记录的字节数；
//!   2. 直接打开 `\\.\X:\$MFT` 这个特殊路径，把整张表顺序读入内存；
//!   3. 逐条解析 FILE 记录：先做 Fixup（每扇区末 2 字节的 USA 修正），
//!      再遍历属性链，取出 `$FILE_NAME`(0x30) 里的父目录引用号和名字、
//!      `$DATA`(0x80) 的真实大小，以及 `$STANDARD_INFORMATION`(0x10) 里
//!      的修改时间和文件属性位；
//!   4. 用"父记录号 -> 子记录号列表"的邻接表在内存里重建目录树（O(n)，
//!      不发起任何一次逐目录的系统调用）。
//!
//! ## 重要限制（已通过检索官方资料确认，不是这份实现自己猜的）
//! - 打开 `$MFT` 需要 **管理员权限 + 启用 `SeBackupPrivilege`**。
//!   光是管理员身份还不够：`SeBackupPrivilege` 在 token 里默认存在但**未启用**，
//!   必须用 `AdjustTokenPrivileges` 显式 enable，否则
//!   `CreateFileW(r"\\.\C:\$MFT", GENERIC_READ, ...)` 会返回
//!   `ERROR_ACCESS_DENIED (GetLastError=5)`。WizTree / Everything 都是这么做的。
//!   （上一版 bug 就是：`mft_scan_available` 只检查了管理员身份 + 打开卷设备
//!    `\\.\C:`，没启用 SeBackupPrivilege，结果"可用"检查通过但实际读 `$MFT`
//!    时 ACCESS_DENIED，被错误地 fallback 到标准遍历。）
//! - 只对 NTFS 卷有效；FAT/exFAT/ReFS/网络盘会在探测阶段直接失败，
//!   调用方应退回标准目录遍历（见 `scan.rs::scan_dir`）。
//! - 只在 Windows 上编译（`cfg(windows)`），非 Windows 平台这个模块整体不参与构建。
//!
//! ## 模块拆分
//! 纯字节解析逻辑（`apply_fixup` / `parse_record` / `RawEntry`）已经抽到
//! `crate::mft_parse` 模块，那里没有 `cfg(windows)` 限制，可以在 Linux 上
//! 跑单元测试。本模块只剩 Windows 专有的 I/O 代码。

#![cfg(windows)]

use std::collections::HashMap;
use std::os::windows::io::FromRawHandle;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::mpsc::Sender;

use egui::Color32;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LUID};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LookupPrivilegeValueW, SE_BACKUP_NAME,
    SE_PRIVILEGE_ENABLED, TokenElevation, TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::model::Node;
use crate::mft_parse::{apply_fixup, parse_record, RawEntry, ROOT_RECORD_INDEX};

const GENERIC_READ: u32 = 0x8000_0000;

pub struct MftError(pub String);
impl std::fmt::Display for MftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
fn last_err(ctx: &str) -> MftError {
    MftError(format!("{} (GetLastError={})", ctx, unsafe { GetLastError() }))
}

/// 当前进程是否以管理员身份提升运行。读 `$MFT` 强制要求这个，
/// 提前检测出来可以给用户一个明确提示，而不是让它在打开文件时才报错。
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret_len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// **FIX B 的核心**：在当前进程的 token 上启用 `SeBackupPrivilege`。
///
/// 这是读 `$MFT` 的强制前置步骤——光是管理员身份不够，必须在 token 上
/// 显式 enable 这个 privilege，否则 `CreateFileW(r"\\.\C:\$MFT", ...)`
/// 会返回 `ERROR_ACCESS_DENIED (5)`。
///
/// 返回 true 表示成功启用（或已经启用）。失败时打印 GetLastError 并返回 false。
fn enable_backup_privilege() -> bool {
    unsafe {
        let mut token: HANDLE = null_mut();
        // 需要 TOKEN_ADJUST_PRIVILEGES 才能改 token；同时带 QUERY 以便排查。
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            eprintln!(
                "[mft_scan] enable_backup_privilege: OpenProcessToken 失败 GetLastError={}",
                GetLastError()
            );
            return false;
        }

        // 查 SeBackupPrivilege 的 LUID
        // SE_BACKUP_NAME 是 PCWSTR（*const u16），直接传，不用 encode_utf16
        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        if LookupPrivilegeValueW(null_mut(), SE_BACKUP_NAME, &mut luid) == 0 {
            eprintln!(
                "[mft_scan] enable_backup_privilege: LookupPrivilegeValueW(SeBackupPrivilege) 失败 GetLastError={}",
                GetLastError()
            );
            CloseHandle(token);
            return false;
        }

        // 构造 TOKEN_PRIVILEGES：1 个 privilege，启用 SE_PRIVILEGE_ENABLED
        let mut tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [std::mem::MaybeUninit::zeroed().assume_init()],
        };
        tp.Privileges[0].Luid = luid;
        tp.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;

        let ok = AdjustTokenPrivileges(
            token,
            0, // FALSE = 启用（不是禁用）
            &mut tp,
            0,
            null_mut(),
            null_mut(),
        );
        let last_err = GetLastError();
        CloseHandle(token);

        if ok == 0 {
            eprintln!(
                "[mft_scan] enable_backup_privilege: AdjustTokenPrivileges 失败 GetLastError={}",
                last_err
            );
            return false;
        }
        // 注意：AdjustTokenPrivileges 即使部分 privilege 不存在也会返回 TRUE，
        // 必须 GetLastError == ERROR_SUCCESS (0) 才是真的全部成功。
        // ERROR_NOT_ALL_ASSIGNED (1300) 表示至少有一个没启用。
        if last_err != 0 {
            eprintln!(
                "[mft_scan] enable_backup_privilege: AdjustTokenPrivileges 返回 TRUE 但 GetLastError={}（可能 token 里没有 SeBackupPrivilege，需要管理员身份）",
                last_err
            );
            return false;
        }
        eprintln!("[mft_scan] enable_backup_privilege: SeBackupPrivilege 已启用");
        true
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 判断某个盘符是否为 NTFS，且当前权限下可以直接读 `$MFT`。
///
/// **注意**：这里只检查"管理员身份 + 卷是 NTFS"。真正的 `$MFT` 读取权限
/// 要靠 `enable_backup_privilege()` 启用 SeBackupPrivilege 后才能拿到，
/// 那一步在 `scan_drive_via_mft` 入口做。如果在这里返回 true 但实际读
/// `$MFT` 时 ACCESS_DENIED，说明 enable_backup_privilege 失败了
///（常见于 token 里没有 SeBackupPrivilege，需要管理员身份运行）。
pub fn mft_scan_available(drive_letter: char) -> bool {
    if !is_elevated() {
        eprintln!(
            "[mft_scan] 不可用：当前进程非管理员，无法读 $MFT (drive={})",
            drive_letter
        );
        return false;
    }
    let path = wide(&format!(r"\\.\{drive_letter}:"));
    unsafe {
        let h = CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        );
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            eprintln!(
                "[mft_scan] 不可用：无法打开卷设备 (drive={}, GetLastError={})",
                drive_letter,
                GetLastError()
            );
            return false;
        }
        let mut buf: NTFS_VOLUME_DATA_BUFFER = std::mem::zeroed();
        let mut ret = 0u32;
        let ok = DeviceIoControl(
            h,
            FSCTL_GET_NTFS_VOLUME_DATA,
            null_mut(),
            0,
            &mut buf as *mut _ as *mut _,
            std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            &mut ret,
            null_mut(),
        );
        CloseHandle(h);
        if ok == 0 {
            eprintln!(
                "[mft_scan] 不可用：FSCTL_GET_NTFS_VOLUME_DATA 失败 (drive={}, GetLastError={})，可能不是 NTFS",
                drive_letter,
                GetLastError()
            );
            return false;
        }
        eprintln!(
            "[mft_scan] 可用：drive={} 是 NTFS，BytesPerFileRecordSegment={}, BytesPerSector={}",
            drive_letter, buf.BytesPerFileRecordSegment, buf.BytesPerSector
        );
        true
    }
}

struct VolumeInfo {
    bytes_per_file_record_segment: u32,
    bytes_per_sector: u32,
}

fn get_volume_info(drive_letter: char) -> Result<VolumeInfo, MftError> {
    let path = wide(&format!(r"\\.\{drive_letter}:"));
    unsafe {
        let h = CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        );
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            return Err(last_err("无法打开卷设备句柄（需要管理员权限）"));
        }
        let mut buf: NTFS_VOLUME_DATA_BUFFER = std::mem::zeroed();
        let mut ret = 0u32;
        let ok = DeviceIoControl(
            h,
            FSCTL_GET_NTFS_VOLUME_DATA,
            null_mut(),
            0,
            &mut buf as *mut _ as *mut _,
            std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            &mut ret,
            null_mut(),
        );
        CloseHandle(h);
        if ok == 0 {
            return Err(last_err("FSCTL_GET_NTFS_VOLUME_DATA 失败（该卷可能不是 NTFS）"));
        }
        Ok(VolumeInfo {
            bytes_per_file_record_segment: buf.BytesPerFileRecordSegment,
            bytes_per_sector: buf.BytesPerSector,
        })
    }
}

/// 把整张 `$MFT` 顺序读入内存。这是一次大块顺序 I/O，而不是逐文件调用系统 API，
/// 这也是这条路径比标准目录遍历快一个数量级的根本原因。
///
/// **调用此函数前必须已经 `enable_backup_privilege()`**，否则 CreateFileW 会 ACCESS_DENIED。
fn read_whole_mft(drive_letter: char) -> Result<Vec<u8>, MftError> {
    let path = wide(&format!(r"\\.\{drive_letter}:\$MFT"));
    unsafe {
        let h = CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        );
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            return Err(last_err("无法打开 \\\\.\\X:\\$MFT（需要管理员权限 + SeBackupPrivilege）"));
        }
        let mut file = std::fs::File::from_raw_handle(h as *mut _);
        use std::io::Read;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .map_err(|e| MftError(format!("读取 $MFT 失败: {e}")))?;
        eprintln!(
            "[mft_scan] $MFT 已读入内存: drive={}, {} 字节 ({:.2} MB)",
            drive_letter,
            buf.len(),
            buf.len() as f64 / 1e6
        );
        // `file` 在这里 drop 会自动 CloseHandle。
        Ok(buf)
    }
}

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

/// 扫描结果：树 + 用于抽测/校验的辅助信息。
pub struct MftScanResult {
    pub root: Node,
    /// 仅保存文件（非目录）的完整路径，供 verify_mft 做抽测比对用。
    pub file_paths: Vec<PathBuf>,
    /// 与 `file_paths` 一一对应：该文件在 MFT 记录里解析出的大小。
    pub file_sizes: Vec<u64>,
}

/// 核心入口：对给定盘符做一次完整的 `$MFT` 直读扫描，返回内存里重建好的目录树。
///
/// 步骤：
/// 1. 检查管理员权限
/// 2. **启用 SeBackupPrivilege**（FIX B）
/// 3. 拿卷信息（记录大小 / 扇区大小）
/// 4. 读整张 $MFT 到内存
/// 5. 逐条解析 + 建邻接表
/// 6. 从根目录（记录 5）递归建树
pub fn scan_drive_via_mft(
    drive_letter: char,
    tx: &Sender<crate::scan::ScanMessage>,
) -> Result<MftScanResult, MftError> {
    if !is_elevated() {
        return Err(MftError(
            "直读 $MFT 需要管理员权限运行本程序（右键\"以管理员身份运行\"）".into(),
        ));
    }

    // FIX B: 必须在打开 $MFT 之前启用 SeBackupPrivilege，否则 ACCESS_DENIED。
    if !enable_backup_privilege() {
        return Err(MftError(
            "启用 SeBackupPrivilege 失败：请确保以管理员身份运行（右键 → 以管理员身份运行）".into(),
        ));
    }

    let vol = get_volume_info(drive_letter)?;
    let record_size = vol.bytes_per_file_record_segment.max(1024) as usize;
    let sector_size = vol.bytes_per_sector.max(512);

    let mft_bytes = read_whole_mft(drive_letter)?;
    let total_records = mft_bytes.len() / record_size;
    eprintln!(
        "[mft_scan] 开始解析 MFT 记录: total_records={}, record_size={}B",
        total_records, record_size
    );

    // 第一遍：解析所有记录。索引 == MFT 记录号。
    let mut entries: Vec<Option<RawEntry>> = Vec::with_capacity(total_records);
    let mut valid_count = 0usize;
    let mut dir_count = 0usize;
    let mut file_count = 0usize;
    for i in 0..total_records {
        let start = i * record_size;
        let end = start + record_size;
        if end > mft_bytes.len() {
            break;
        }
        let mut rec = mft_bytes[start..end].to_vec();
        if !apply_fixup(&mut rec, sector_size) {
            entries.push(None);
            continue;
        }
        let parsed = parse_record(&rec).filter(|e| e.in_use && e.is_base_record);
        if let Some(p) = &parsed {
            valid_count += 1;
            if p.is_dir {
                dir_count += 1;
            } else {
                file_count += 1;
            }
        }
        entries.push(parsed);

        if i % 20_000 == 0 {
            let _ = tx.send(crate::scan::ScanMessage::Progress(i as u64));
        }
    }
    eprintln!(
        "[mft_scan] 解析完成: 总记录={}, 有效={}, 目录={}, 文件={}",
        total_records, valid_count, dir_count, file_count
    );

    // 第二遍：按 parent_record 建邻接表。
    let mut children_of: HashMap<u64, Vec<u64>> = HashMap::new();
    for (idx, e) in entries.iter().enumerate() {
        if let Some(e) = e {
            if idx as u64 != ROOT_RECORD_INDEX {
                children_of
                    .entry(e.parent_record)
                    .or_default()
                    .push(idx as u64);
            }
        }
    }
    eprintln!(
        "[mft_scan] 邻接表构建完成: {} 个父节点",
        children_of.len()
    );

    let mut file_paths = Vec::new();
    let mut file_sizes = Vec::new();
    let root_name = format!("{drive_letter}:\\");
    let root_node = build_subtree(
        ROOT_RECORD_INDEX,
        &root_name,
        &entries,
        &children_of,
        0,
        &PathBuf::from(format!("{drive_letter}:\\")),
        &mut file_paths,
        &mut file_sizes,
    );

    eprintln!(
        "[mft_scan] 树构建完成: root.size={:.2}GB, files={}, folders={}",
        root_node.size as f64 / 1e9,
        root_node.file_count,
        root_node.folder_count
    );

    Ok(MftScanResult {
        root: root_node,
        file_paths,
        file_sizes,
    })
}

fn build_subtree(
    record_idx: u64,
    display_name: &str,
    entries: &[Option<RawEntry>],
    children_of: &HashMap<u64, Vec<u64>>,
    depth: usize,
    cur_path: &PathBuf,
    file_paths: &mut Vec<PathBuf>,
    file_sizes: &mut Vec<u64>,
) -> Node {
    let mut children_nodes = Vec::new();
    // 本目录自身的元信息：默认 0，如果 MFT 记录里有就从记录里取。
    let mut self_modified: u64 = 0;
    let mut self_attrs: u32 = 0x10; // DIRECTORY
    if let Some(Some(entry)) = entries.get(record_idx as usize) {
        self_modified = entry.modified_ft;
        self_attrs = if entry.attributes == 0 {
            0x10
        } else {
            entry.attributes
        };
    }

    if let Some(kids) = children_of.get(&record_idx) {
        for &child_idx in kids {
            let Some(entry) = entries.get(child_idx as usize).and_then(|e| e.as_ref()) else {
                continue;
            };
            let child_path = cur_path.join(&entry.name);
            if entry.is_dir {
                let node = build_subtree(
                    child_idx,
                    &entry.name,
                    entries,
                    children_of,
                    depth + 1,
                    &child_path,
                    file_paths,
                    file_sizes,
                );
                children_nodes.push(node);
            } else {
                file_paths.push(child_path);
                file_sizes.push(entry.real_size);
                children_nodes.push(Node::new_file_with_meta(
                    entry.name.clone(),
                    entry.real_size,
                    file_color(),
                    entry.modified_ft,
                    entry.attributes,
                ));
            }
        }
    }
    Node::new_folder_with_meta(
        display_name,
        folder_color(depth),
        children_nodes,
        self_modified,
        self_attrs,
    )
}

/// 用 `GetDiskFreeSpaceExW` 拿该盘符官方报告的总容量/可用空间，
/// 用来和扫描结果的汇总大小做一个"量级是否合理"的旁证
///（注意：文件逻辑大小之和天然会小于"已用空间"，因为它不包含簇内部碎片、
/// NTFS 元数据本身、卷影副本等——这是预期内的差异，不是 bug）。
pub fn get_disk_space(drive_letter: char) -> Option<(u64, u64)> {
    let path = wide(&format!("{drive_letter}:\\"));
    unsafe {
        let mut free_bytes = 0u64;
        let mut total_bytes = 0u64;
        let ok = GetDiskFreeSpaceExW(
            path.as_ptr(),
            null_mut(),
            &mut total_bytes,
            &mut free_bytes,
        );
        if ok == 0 {
            None
        } else {
            Some((total_bytes, free_bytes))
        }
    }
}
