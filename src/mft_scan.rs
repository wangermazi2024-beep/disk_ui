//! 直接读取 NTFS `$MFT`（Master File Table）来枚举整个卷的文件/文件夹，
//! 原理和 WizTree / Everything 一致：
//!
//!   1. 用 `FSCTL_GET_NTFS_VOLUME_DATA` 拿到该卷每条 MFT 记录的字节数；
//!   2. 直接打开 `\\.\X:\$MFT` 这个特殊路径，把整张表顺序读入内存；
//!   3. 逐条解析 FILE 记录：先做 Fixup（每扇区末 2 字节的 USA 修正），
//!      再遍历属性链，取出 `$FILE_NAME`(0x30) 里的父目录引用号和名字、
//!      以及 `$DATA`(0x80) 的真实大小；
//!   4. 用"父记录号 -> 子记录号列表"的邻接表在内存里重建目录树（O(n)，
//!      不发起任何一次逐目录的系统调用）。
//!
//! ## 重要限制（已通过检索官方资料确认，不是这份实现自己猜的）
//! - 打开卷设备 / `$MFT` 需要管理员权限（`SeBackupPrivilege`），
//!   这是 NTFS 原始卷访问的强制要求，不是可选项：普通用户权限下
//!   `CreateFileW("\\\\.\\C:\\$MFT", ...)` 会直接返回 ACCESS_DENIED。
//! - 只对 NTFS 卷有效；FAT/exFAT/ReFS/网络盘会在探测阶段直接失败，
//!   调用方应退回标准目录遍历（见 `scan.rs::scan_dir`）。
//! - 只在 Windows 上编译（`cfg(windows)`），非 Windows 平台这个模块整体不参与构建。

#![cfg(windows)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::FromRawHandle;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::mpsc::Sender;

use egui::Color32;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
    GetVolumeInformationW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::model::Node;

const GENERIC_READ: u32 = 0x8000_0000;
const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_END: u32 = 0xFFFF_FFFF;
const ROOT_RECORD_INDEX: u64 = 5; // NTFS 规定根目录固定是第 5 条 MFT 记录。

pub struct MftError(pub String);
impl std::fmt::Display for MftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
fn last_err(ctx: &str) -> MftError {
    MftError(format!("{ctx} (GetLastError={})", unsafe { GetLastError() }))
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

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 判断某个盘符是否为 NTFS，且当前权限下可以直接读 `$MFT`。
/// 用于扫描前的"能不能走快速路径"探测，不满足条件就应该 fallback。
pub fn mft_scan_available(drive_letter: char) -> bool {
    if !is_elevated() {
        return false;
    }
    let path = wide(&format!(r"\.\{}:", drive_letter));
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
        ok != 0
    }
}

struct VolumeInfo {
    bytes_per_file_record_segment: u32,
    bytes_per_sector: u32,
}

fn get_volume_info(drive_letter: char) -> Result<VolumeInfo, MftError> {
    let path = wide(&format!(r"\.\{}:", drive_letter));
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
fn read_whole_mft(drive_letter: char) -> Result<Vec<u8>, MftError> {
    // 方案：打开卷设备 \\.\C:，用 FSCTL 获取 MFT 位置后直接读取（$MFT 文件路径不可靠）
    let path = wide(&format!(r"\.\{}:", drive_letter));
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
            return Err(last_err("无法打开卷设备（需要管理员权限）"));
        }

        // 获取 MFT 位置
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
        if ok == 0 {
            CloseHandle(h);
            return Err(last_err("FSCTL_GET_NTFS_VOLUME_DATA 失败"));
        }

        let bps = buf.BytesPerSector as u64;
        let bpc = buf.BytesPerCluster as u64;
        let mft_lcn = buf.MftStartLcn as u64;
        let mft_len = buf.MftValidDataLength as u64;
        let rec_size = buf.BytesPerFileRecordSegment;

        // 扇区对齐读取 MFT
        let mft_byte_off = mft_lcn * bpc;
        let sector_mask = bps - 1;
        let aligned_off = mft_byte_off & !sector_mask;
        let read_start = (mft_byte_off - aligned_off) as usize;
        let aligned_sz = ((mft_len as usize + read_start + bps as usize - 1) / bps as usize) * bps as usize;

        // 定位
        let mut raw_buf = vec![0u8; aligned_sz];
        let mut read_bytes = 0u32;

        // SetFilePointerEx
        use windows_sys::Win32::Storage::FileSystem::{SetFilePointerEx, ReadFile, FILE_BEGIN};

        SetFilePointerEx(h, aligned_off as i64, null_mut(), FILE_BEGIN);
        let ok = ReadFile(
            h,
            raw_buf.as_mut_ptr() as *mut _,
            aligned_sz as u32,
            &mut read_bytes,
            null_mut(),
        );
        CloseHandle(h);

        if ok == 0 {
            return Err(last_err("ReadFile MFT 失败"));
        }

        raw_buf.truncate(read_bytes as usize);
        Ok(raw_buf[read_start..read_start + mft_len as usize].to_vec())
    }
}

struct RawEntry {
    parent_record: u64,
    name: String,
    is_dir: bool,
    in_use: bool,
    is_base_record: bool,
    real_size: u64,
    modified: u64,
    attributes: u32,
}

fn apply_fixup(record: &mut [u8], bytes_per_sector: u32) -> bool {
    if record.len() < 8 {
        return false;
    }
    let usa_offset = u16::from_le_bytes([record[4], record[5]]) as usize;
    let usa_count = u16::from_le_bytes([record[6], record[7]]) as usize; // 含 USN 本身
    if usa_count == 0 || usa_offset + usa_count * 2 > record.len() {
        return false;
    }
    let usn = [record[usa_offset], record[usa_offset + 1]];
    let sector = bytes_per_sector.max(512) as usize;
    for i in 1..usa_count {
        let sector_end = i * sector; // 每个扇区最后 2 字节
        if sector_end > record.len() {
            break;
        }
        let check = &record[sector_end - 2..sector_end];
        if check != usn {
            // 该扇区的 USN 不匹配：记录已损坏/不完整，放弃这条记录。
            return false;
        }
        let orig_off = usa_offset + i * 2;
        record[sector_end - 2] = record[orig_off];
        record[sector_end - 1] = record[orig_off + 1];
    }
    true
}

fn parse_record(record: &[u8]) -> Option<RawEntry> {
    if record.len() < 48 || &record[0..4] != b"FILE" {
        return None;
    }
    let flags = u16::from_le_bytes([record[22], record[23]]);
    let in_use = flags & 0x0001 != 0;
    let is_dir = flags & 0x0002 != 0;
    let first_attr_offset = u16::from_le_bytes([record[20], record[21]]) as usize;
    let base_record_ref = u64::from_le_bytes(record[32..40].try_into().unwrap());
    let is_base_record = (base_record_ref & 0x0000_FFFF_FFFF_FFFF) == 0;

    let mut parent_record: u64 = 0;
    let mut name: Option<String> = None;
    let mut best_ns = 255u8;
    let mut real_size: u64 = 0;
    let mut modified_time: u64 = 0;
    let mut file_attributes: u32 = 0;

    let mut off = first_attr_offset;
    while off + 16 <= record.len() {
        let attr_type = u32::from_le_bytes(record[off..off + 4].try_into().unwrap());
        if attr_type == ATTR_END { break; }
        let attr_len = u32::from_le_bytes(record[off + 4..off + 8].try_into().unwrap()) as usize;
        if attr_len == 0 || off + attr_len > record.len() { break; }
        let non_resident = record[off + 8] != 0;

        if attr_type == ATTR_STANDARD_INFORMATION && !non_resident {
            // $STANDARD_INFORMATION: 常驻属性, 包含时间戳和文件属性
            let value_off = u16::from_le_bytes(record[off + 20..off + 22].try_into().unwrap()) as usize;
            let value_len = u32::from_le_bytes(record[off + 16..off + 20].try_into().unwrap()) as usize;
            let v_start = off + value_off;
            if v_start + 36 <= record.len() && value_len >= 36 {
                let c = &record[v_start..v_start + value_len];
                // 偏移 0-7: 创建时间
                // 偏移 8-15: 修改时间
                let modified_ft = u64::from_le_bytes(c[8..16].try_into().unwrap());
                // Windows FILETIME (1601-01-01 epoch, 100ns units) -> unix nanos
                if modified_ft != 0 {
                    // FILETIME → Unix epoch: subtract 11644473600 seconds, multiply by 100
                    modified_time = (modified_ft / 10).saturating_sub(11644473600_0000000);
                }
                // 偏移 32-35: file attributes (DWORD)
                file_attributes = u32::from_le_bytes(c[32..36].try_into().unwrap());
            }
        } else if attr_type == ATTR_FILE_NAME && !non_resident {
            let value_len = u32::from_le_bytes(record[off + 16..off + 20].try_into().unwrap()) as usize;
            let value_off = u16::from_le_bytes(record[off + 20..off + 22].try_into().unwrap()) as usize;
            let content_start = off + value_off;
            if content_start + value_len <= record.len() && value_len >= 66 {
                let c = &record[content_start..content_start + value_len];
                let parent_ref = u64::from_le_bytes(c[0..8].try_into().unwrap());
                let name_len_chars = c[64] as usize;
                let ns = c[65]; // 0=POSIX 1=WIN32 2=DOS(8.3短名) 3=WIN32&DOS
                let name_bytes_len = name_len_chars * 2;
                if 66 + name_bytes_len <= c.len() {
                    // 优先取 WIN32(1) 或 POSIX(0) 名字；纯 DOS 短名(2) 只在没有更好选择时用。
                    let priority = match ns {
                        1 | 0 | 3 => 0,
                        _ => 1,
                    };
                    if priority < best_ns {
                        best_ns = priority;
                        let u16s: Vec<u16> = c[66..66 + name_bytes_len]
                            .chunks_exact(2)
                            .map(|b| u16::from_le_bytes([b[0], b[1]]))
                            .collect();
                        name = Some(OsString::from_wide(&u16s).to_string_lossy().into_owned());
                        parent_record = parent_ref & 0x0000_FFFF_FFFF_FFFF; // 低 48 位是记录号
                        real_size = u64::from_le_bytes(c[48..56].try_into().unwrap());
                    }
                }
            }
        } else if attr_type == ATTR_DATA {
            // 未命名的 $DATA 才代表文件主体大小；命名的是备用数据流(ADS)，跳过。
            let name_len = record[off + 9];
            if name_len == 0 {
                real_size = if non_resident {
                    u64::from_le_bytes(record[off + 48..off + 56].try_into().unwrap())
                } else {
                    u32::from_le_bytes(record[off + 16..off + 20].try_into().unwrap()) as u64
                };
            }
        } else if attr_type == ATTR_STANDARD_INFORMATION {
            // 目前不需要这里的字段（时间戳/常规属性），跳过。
        }

        off += attr_len;
    }

    let name = name?;
    Some(RawEntry {
        parent_record,
        name,
        is_dir,
        in_use,
        is_base_record,
        real_size,
        modified: modified_time,
        attributes: file_attributes,
    })
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
    /// 仅保存文件（非目录）的完整路径，供 verify.rs 做抽测比对用。
    pub file_paths: Vec<PathBuf>,
    /// 与 `file_paths` 一一对应：该文件在 MFT 记录里解析出的大小。
    pub file_sizes: Vec<u64>,
}

/// 核心入口：对给定盘符做一次完整的 `$MFT` 直读扫描，返回内存里重建好的目录树。
///
/// `progress` 用于往界面汇报"已解析记录数"；`tx` 复用 `scan::ScanMessage::Progress`。
pub fn scan_drive_via_mft(
    drive_letter: char,
    tx: &Sender<crate::scan::ScanMessage>,
) -> Result<MftScanResult, MftError> {
    if !is_elevated() {
        return Err(MftError(
            "直读 $MFT 需要管理员权限运行本程序（右键“以管理员身份运行”）".into(),
        ));
    }

    let vol = get_volume_info(drive_letter)?;
    let record_size = vol.bytes_per_file_record_segment.max(1024) as usize;
    let sector_size = vol.bytes_per_sector.max(512);

    let mft_bytes = read_whole_mft(drive_letter)?;
    let total_records = mft_bytes.len() / record_size;

    // 第一遍：解析所有记录。索引 == MFT 记录号。
    let mut entries: Vec<Option<RawEntry>> = Vec::with_capacity(total_records);
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
        entries.push(parsed);

        if i % 20_000 == 0 {
            let _ = tx.send(crate::scan::ScanMessage::Progress(i as u64));
        }
    }

    // 第二遍：按 parent_record 建邻接表。
    let mut children_of: HashMap<u64, Vec<u64>> = HashMap::new();
    for (idx, e) in entries.iter().enumerate() {
        if let Some(e) = e {
            if idx as u64 != ROOT_RECORD_INDEX {
                children_of.entry(e.parent_record).or_default().push(idx as u64);
            }
        }
    }

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
                children_nodes.push(Node::new_file_full(
                    entry.name.clone(),
                    entry.real_size,
                    entry.modified,
                    entry.attributes,
                    file_color(),
                ));
            }
        }
    }
    Node::new_folder(display_name, folder_color(depth), children_nodes)
}

/// 枚举所有固定磁盘（如 C、D、E...），返回盘符列表。
pub fn enum_fixed_drives() -> Vec<char> {
    unsafe {
        let mask = GetLogicalDrives();
        let mut drives = Vec::new();
        for i in 0..26 {
            if mask & (1 << i) != 0 {
                let d = (b'A' + i) as char;
                let root = wide(&format!("{d}:\\"));
                let dt = GetDriveTypeW(root.as_ptr());
                // DRIVE_FIXED = 3
                if dt == 3 {
                    drives.push(d);
                }
            }
        }
        drives
    }
}

/// 获取卷标（如果存在）和 Explorer 友好显示名。
/// 返回 (卷标, 友好名)。
pub fn get_volume_label(drive_letter: char) -> (String, String) {
    use std::ptr;
    unsafe {
        let path = wide(&format!(r"{drive_letter}:\"));
        let mut vol_buf = [0u16; 256];
        let mut fs_buf = [0u16; 256];
        let mut sn = 0u32;
        let mut max_comp = 0u32;
        let mut flags = 0u32;

        let ok = GetVolumeInformationW(
            path.as_ptr(),
            vol_buf.as_mut_ptr(),
            vol_buf.len() as u32,
            &mut sn,
            &mut max_comp,
            &mut flags,
            fs_buf.as_mut_ptr(),
            fs_buf.len() as u32,
        );

        let vol_label = if ok != 0 {
            let len = vol_buf.iter().position(|&c| c == 0).unwrap_or(0);
            String::from_utf16_lossy(&vol_buf[..len])
        } else {
            String::new()
        };

        (vol_label, String::new()) // 友好名后续扩展
    }
}
/// 用来和扫描结果的汇总大小做一个"量级是否合理"的旁证
/// （注意：文件逻辑大小之和天然会小于"已用空间"，因为它不包含簇内部碎片、
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
