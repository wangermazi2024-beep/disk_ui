//! WinDirStat 风格的 NTFS MFT 直读扫描（v2 完全重构）。
//!
//! ## 算法（参考 github.com/windirstat/windirstat FinderNtfs.cpp）
//!
//! **核心设计：两阶段 + 哈希表聚合**
//!
//! 1. **第一阶段（load_volume）**：读整张 MFT，对每条记录解析属性，
//!    把属性聚合到 **base record** 的哈希表条目里（扩展记录的属性自动写到 base record）。
//!    - `base_file_records: HashMap<record_number, FileRecordBase>` — 每个文件的属性
//!    - `parent_to_children: HashMap<parent_record_number, Vec<(name, base_record)>>` — 父→子映射
//!
//! 2. **第二阶段（build_tree）**：从根目录（record 5）递归，用 `parent_to_children` 找子项，
//!    用 `base_file_records` 拿属性，构建 Node 树。
//!
//! **关键**：不需要解析 `$ATTRIBUTE_LIST`！因为扩展记录的 `$DATA` 属性在第一阶段
//! 就已经聚合到 base record 的 `FileRecordBase` 里了。
//!
//! ## MFT 物理读取
//! - 打开卷设备 `\\.\C:`（`FILE_READ_DATA | FILE_READ_ATTRIBUTES`，`FILE_FLAG_NO_BUFFERING`）
//! - `FSCTL_GET_NTFS_VOLUME_DATA` 拿卷信息
//! - 打开 `\\.\C:\$MFT::$DATA`（`FILE_READ_ATTRIBUTES`）+ `FSCTL_GET_RETRIEVAL_POINTERS` 拿 MFT 簇映射
//! - 按 run 顺序读 MFT
//!
//! **不需要 SeBackupPrivilege**，只要管理员身份。

#![cfg(windows)]

use std::collections::HashMap;
use std::ptr::null_mut;
use std::sync::mpsc::Sender;

use egui::Color32;
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, ReadFile, SetFilePointerEx, FILE_BEGIN,
    FILE_FLAG_NO_BUFFERING, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_GET_RETRIEVAL_POINTERS, NTFS_VOLUME_DATA_BUFFER,
    RETRIEVAL_POINTERS_BUFFER, STARTING_VCN_INPUT_BUFFER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::model::Node;

/// NTFS 根目录的 MFT 记录号固定是 5。
const NTFS_ROOT_RECORD: u64 = 5;
/// record < 16 是 NTFS 保留系统文件（$MFT/$LogFile/$Bitmap 等）。
const NTFS_RESERVED_MAX: u64 = 16;

/// 属性类型码
const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
#[allow(dead_code)]
const ATTR_INDEX_ALLOCATION: u32 = 0xA0;
const ATTR_REPARSE_POINT: u32 = 0xC0;
const ATTR_END: u32 = 0xFFFF_FFFF;

/// FILE_ATTRIBUTE_* 常量
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x800;
#[allow(dead_code)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// IO_REPARSE_TAG_*（部分）
#[allow(dead_code)]
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA0000003;
#[allow(dead_code)]
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000000C;
const IO_REPARSE_TAG_WOF: u32 = 0x80000017;

pub struct MftError(pub String);
impl std::fmt::Debug for MftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{:?}", self.0) }
}
impl std::fmt::Display for MftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 一个文件的聚合属性（来自 base record + 所有扩展记录）。
#[derive(Clone, Debug, Default)]
pub struct FileRecordBase {
    pub logical_size: u64,
    pub physical_size: u64,
    pub last_modified_ft: u64,
    pub created_ft: u64,
    pub accessed_ft: u64,
    pub attributes: u32,
    pub reparse_tag: u32,
}

/// 一条 $FILE_NAME 属性解析结果（一个文件可能有多个 $FILE_NAME：长名+短名+硬链接）。
#[derive(Clone, Debug)]
pub struct FileRecordName {
    pub name: String,
    pub base_record: u64,
}

/// NTFS 上下文：两个哈希表 + 卷信息。
pub struct NtfsContext {
    pub base_file_records: HashMap<u64, FileRecordBase>,
    pub parent_to_children: HashMap<u64, Vec<FileRecordName>>,
    pub bytes_per_cluster: u32,
    pub bytes_per_record: u32,
}

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

fn folder_color(depth: usize) -> Color32 {
    const PAL: [Color32; 6] = [
        Color32::from_rgb(0x4C, 0x8B, 0xF5),
        Color32::from_rgb(0x34, 0xC7, 0x59),
        Color32::from_rgb(0xF5, 0xA6, 0x23),
        Color32::from_rgb(0xE0, 0x55, 0x5B),
        Color32::from_rgb(0x9C, 0x6A, 0xDE),
        Color32::from_rgb(0x2E, 0xC4, 0xB6),
    ];
    PAL[depth % PAL.len()]
}

fn file_color() -> Color32 {
    Color32::from_rgb(0x6C, 0x75, 0x7D)
}

/// 主入口：扫描一个 NTFS 卷，返回建好的目录树。
pub fn scan_volume(
    drive_letter: char,
    tx: &Sender<crate::scan::ScanMessage>,
) -> Result<Node, MftError> {
    eprintln!("[mft_scan] 开始扫描 drive={}", drive_letter);
    let ctx = load_volume(drive_letter, tx)?;
    eprintln!(
        "[mft_scan] MFT 加载完成: {} 个文件记录, {} 个父目录",
        ctx.base_file_records.len(),
        ctx.parent_to_children.len()
    );

    let root_name = format!("{}:\\", drive_letter);
    let mut size_counted: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut root_node = build_tree(&ctx, NTFS_ROOT_RECORD, &root_name, 0, &mut size_counted);
    eprintln!(
        "[mft_scan] 树构建完成: logical={}, physical={}, files={}, folders={}",
        crate::format::human_size(root_node.logical_size),
        crate::format::human_size(root_node.physical_size),
        root_node.file_count,
        root_node.folder_count
    );

    // 填充根级子项的 Owner（和 WinDirStat 一样用 GetNamedSecurityInfo）
    let root_path = format!("{}:\\", drive_letter);
    populate_owners(&mut root_node, &root_path);
    eprintln!("[mft_scan] Owner 填充完成");

    Ok(root_node)
}

/// 递归填充 Owner（用 GetNamedSecurityInfo + LookupAccountSid）。
/// 只填充可见的（已展开的）节点的直接子项，避免全量遍历太慢。
fn populate_owners(node: &mut Node, path: &str) {
    for child in &mut node.children {
        let child_path = if path.ends_with('\\') {
            format!("{}{}", path, child.name)
        } else {
            format!("{}\\{}", path, child.name)
        };
        child.owner = get_owner(&child_path);
        // 只递归已展开的文件夹
        if child.is_folder() && child.expanded {
            populate_owners(child, &child_path);
        }
    }
}

/// 用 Win32 API 获取文件所有者名称（和 WinDirStat 的 GetOwner 一致）。
fn get_owner(path: &str) -> String {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::{
        LookupAccountSidW, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SID_NAME_USE,
    };
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};

    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut sid: PSID = std::ptr::null_mut();
    let ok = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut sid,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if ok != 0 {
        return String::new();
    }

    let mut name_buf = [0u16; 260];
    let mut name_len: u32 = 260;
    let mut domain_buf = [0u16; 260];
    let mut domain_len: u32 = 260;
    let mut sid_type: SID_NAME_USE = 0;
    let ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid,
            name_buf.as_mut_ptr(),
            &mut name_len,
            domain_buf.as_mut_ptr(),
            &mut domain_len,
            &mut sid_type,
        )
    };
    let result = if ok != 0 && name_len > 0 {
        let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        if domain_len > 0 {
            let domain = String::from_utf16_lossy(&domain_buf[..domain_len as usize]);
            format!("{}\\{}", domain, name)
        } else {
            name
        }
    } else {
        String::new()
    };

    unsafe {
        if !sd.is_null() {
            LocalFree(sd as *mut _);
        }
    }
    result
}

/// 第一阶段：读 MFT，填充两个哈希表。
fn load_volume(
    drive_letter: char,
    tx: &Sender<crate::scan::ScanMessage>,
) -> Result<NtfsContext, MftError> {
    let vol_path = wide(&format!(r"\\.\{drive_letter}:"));

    // 打开卷设备
    let vol_handle = unsafe {
        let h = CreateFileW(
            vol_path.as_ptr(),
            FILE_READ_DATA | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING,
            null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return Err(MftError(format!(
                "无法打开卷设备 \\\\.\\{drive_letter}:（需要管理员权限）"
            )));
        }
        h
    };
    eprintln!("[mft_scan] 卷设备已打开: \\\\.\\{drive_letter}:");

    // 拿卷信息
    let mut vol_info: NTFS_VOLUME_DATA_BUFFER = unsafe { std::mem::zeroed() };
    let mut bytes_returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            vol_handle,
            FSCTL_GET_NTFS_VOLUME_DATA,
            null_mut(),
            0,
            &mut vol_info as *mut _ as *mut _,
            std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            &mut bytes_returned,
            null_mut(),
        )
    };
    if ok == 0 {
        unsafe { CloseHandle(vol_handle); }
        return Err(MftError(format!(
            "FSCTL_GET_NTFS_VOLUME_DATA 失败（{} 可能不是 NTFS）",
            drive_letter
        )));
    }
    let bytes_per_cluster = vol_info.BytesPerCluster;
    let bytes_per_record = vol_info.BytesPerFileRecordSegment.max(1024);
    eprintln!(
        "[mft_scan] 卷信息: BytesPerCluster={}, BytesPerFileRecordSegment={}, MftStartLcn={}, MftValidDataLength={}",
        bytes_per_cluster, bytes_per_record, vol_info.MftStartLcn, vol_info.MftValidDataLength
    );

    // 拿 MFT 的 retrieval pointers（打开 $MFT::$DATA 用 FILE_READ_ATTRIBUTES）
    let mft_path = wide(&format!(r"\\.\{drive_letter}:\$MFT::$DATA"));
    let mft_handle = unsafe {
        let h = CreateFileW(
            mft_path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING,
            null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            // fallback: 不打开 $MFT 文件，直接用 MftStartLcn 做单 run
            eprintln!("[mft_scan] 打开 $MFT::$DATA 失败，用 MftStartLcn 单 run");
            INVALID_HANDLE_VALUE
        } else {
            h
        }
    };

    // 收集 MFT 的物理 run 列表
    let mft_runs: Vec<(u64, i64, u64)> = if mft_handle != INVALID_HANDLE_VALUE {
        // 用 FSCTL_GET_RETRIEVAL_POINTERS
        let mut input = STARTING_VCN_INPUT_BUFFER { StartingVcn: 0 };
        let mut buf_size = std::mem::size_of::<RETRIEVAL_POINTERS_BUFFER>() + 32 * 16;
        let mut buf = vec![0u8; buf_size];
        let mut ok;
        loop {
            ok = unsafe {
                DeviceIoControl(
                    mft_handle,
                    FSCTL_GET_RETRIEVAL_POINTERS,
                    &mut input as *mut _ as *mut _,
                    std::mem::size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
                    buf.as_mut_ptr() as *mut _,
                    buf_size as u32,
                    &mut bytes_returned,
                    null_mut(),
                )
            };
            if ok != 0 {
                break;
            }
            if unsafe { windows_sys::Win32::Foundation::GetLastError() } != 234 {
                // ERROR_MORE_DATA = 234
                break;
            }
            buf_size *= 2;
            buf.resize(buf_size, 0);
        }
        unsafe { CloseHandle(mft_handle); }
        if ok == 0 {
            eprintln!("[mft_scan] FSCTL_GET_RETRIEVAL_POINTERS 失败，用 MftStartLcn 单 run");
            let total_clusters = vol_info.MftValidDataLength as u64 / bytes_per_cluster as u64;
            vec![(0, vol_info.MftStartLcn, total_clusters)]
        } else {
            parse_retrieval_pointers(&buf, vol_info.MftStartLcn as i64)
        }
    } else {
        // 单 run fallback
        let total_clusters = vol_info.MftValidDataLength as u64 / bytes_per_cluster as u64;
        vec![(0, vol_info.MftStartLcn, total_clusters)]
    };
    eprintln!("[mft_scan] MFT 有 {} 个物理 run", mft_runs.len());

    let mut ctx = NtfsContext {
        base_file_records: HashMap::new(),
        parent_to_children: HashMap::new(),
        bytes_per_cluster,
        bytes_per_record,
    };

    let cluster_size = bytes_per_cluster as u64;
    let record_size = bytes_per_record as usize;
    let mut records_processed: u64 = 0;

    // 按 run 顺序读 MFT
    let chunk_size = 4 * 1024 * 1024; // 4MB 一块
    let mut chunk_buf: Vec<u8> = vec![0u8; chunk_size];

    for (run_idx, &(run_vcn_start, cluster_start, cluster_count)) in mft_runs.iter().enumerate() {
        let run_bytes = cluster_count * cluster_size;
        let mut file_offset = cluster_start * cluster_size as i64;
        let mft_run_offset = run_vcn_start * cluster_size;
        let mut bytes_read_from_run: u64 = 0;
        let mut bytes_to_read = run_bytes;

        eprintln!(
            "[mft_scan] run[{}]: VCN={}, LCN={}, {}MB",
            run_idx, run_vcn_start, cluster_start, run_bytes as f64 / 1e6
        );

        while bytes_to_read > 0 {
            let bytes_this = (bytes_to_read as usize).min(chunk_size);
            let mut new_pos: i64 = 0;
            let ok = unsafe {
                SetFilePointerEx(vol_handle, file_offset, &mut new_pos, FILE_BEGIN)
            };
            if ok == 0 {
                eprintln!("[mft_scan] SetFilePointerEx 失败");
                break;
            }
            let mut bytes_returned: u32 = 0;
            let ok = unsafe {
                ReadFile(vol_handle, chunk_buf.as_mut_ptr(), bytes_this as u32, &mut bytes_returned, null_mut())
            };
            if ok == 0 || bytes_returned == 0 {
                eprintln!("[mft_scan] ReadFile 结束: bytes={}", bytes_returned);
                break;
            }
            let bytes_read = bytes_returned as usize;
            // 逐条记录解析
            let mut off = 0usize;
            while off + record_size <= bytes_read {
                let rec = &mut chunk_buf[off..off + record_size];
                let rec_offset_in_mft = mft_run_offset + bytes_read_from_run + off as u64;
                let current_record = rec_offset_in_mft / record_size as u64;
                process_record(rec, current_record, &mut ctx);
                records_processed += 1;
                off += record_size;
            }
            bytes_read_from_run += bytes_read as u64;
            bytes_to_read -= bytes_read as u64;
            file_offset += bytes_read as i64;

            if records_processed % 50_000 < (bytes_read / record_size) as u64 {
                let _ = tx.send(crate::scan::ScanMessage::Progress(records_processed));
            }
        }
    }
    unsafe { CloseHandle(vol_handle); }

    eprintln!("[mft_scan] 共处理 {} 条 MFT 记录", records_processed);
    Ok(ctx)
}

/// 解析 RETRIEVAL_POINTERS_BUFFER，返回 (start_vcn, lcn, cluster_count) 列表。
fn parse_retrieval_pointers(buf: &[u8], mft_start_lcn: i64) -> Vec<(u64, i64, u64)> {
    let rp = unsafe { &*(buf.as_ptr() as *const RETRIEVAL_POINTERS_BUFFER) };
    let extent_count = rp.ExtentCount as usize;
    let mut runs = Vec::with_capacity(extent_count);
    let extents_ptr: *const windows_sys::Win32::System::Ioctl::RETRIEVAL_POINTERS_BUFFER_0 = &rp.Extents[0];
    let mut vcn_start = rp.StartingVcn;
    for i in 0..extent_count {
        let ext = unsafe { &*extents_ptr.add(i) };
        let vcn_next = ext.NextVcn;
        let lcn = ext.Lcn;
        let count = (vcn_next - vcn_start) as u64;
        // lcn == -1 表示 sparse，跳过（MFT 理论上不 sparse，但容错）
        if lcn >= 0 {
            runs.push((vcn_start as u64, lcn, count));
        }
        vcn_start = vcn_next;
    }
    // 如果没拿到 run，fallback 到 MftStartLcn
    if runs.is_empty() {
        runs.push((0, mft_start_lcn, 0));
    }
    runs
}

/// 处理单条 MFT 记录：做 USA fixup，解析属性，聚合到 base record。
fn process_record(rec: &mut [u8], current_record: u64, ctx: &mut NtfsContext) {
    if rec.len() < 48 || &rec[0..4] != b"FILE" {
        return;
    }
    let usa_offset = u16::from_le_bytes([rec[4], rec[5]]) as usize;
    let usa_count = u16::from_le_bytes([rec[6], rec[7]]) as usize;
    let flags = u16::from_le_bytes([rec[22], rec[23]]);
    let in_use = flags & 0x0001 != 0;
    let is_dir = flags & 0x0002 != 0;
    let first_attr_offset = u16::from_le_bytes([rec[20], rec[21]]) as usize;
    let base_file_record = u64::from_le_bytes(rec[32..40].try_into().unwrap());
    let base_record_index = if base_file_record > 0 {
        base_file_record & 0x0000_FFFF_FFFF_FFFF
    } else {
        current_record
    };

    // USA fixup
    if usa_count > 0 {
        let _sector_words = 256usize; // 512/2
        if usa_offset + usa_count * 2 <= rec.len() {
            let usn = [rec[usa_offset], rec[usa_offset + 1]];
            for i in 1..usa_count {
                let sector_end = i * 512; // 每扇区最后 2 字节
                if sector_end > rec.len() {
                    break;
                }
                let check = &rec[sector_end - 2..sector_end];
                if check != usn {
                    return; // fixup 失败，跳过
                }
                let orig_off = usa_offset + i * 2;
                rec[sector_end - 2] = rec[orig_off];
                rec[sector_end - 1] = rec[orig_off + 1];
            }
        }
    }

    if !in_use {
        return;
    }

    // 获取或创建 base record 条目
    let base_entry = ctx
        .base_file_records
        .entry(base_record_index)
        .or_default();

    // 遍历属性
    let mut off = first_attr_offset;
    while off + 16 <= rec.len() {
        let attr_type = u32::from_le_bytes(rec[off..off + 4].try_into().unwrap());
        if attr_type == ATTR_END {
            break;
        }
        let attr_len = u32::from_le_bytes(rec[off + 4..off + 8].try_into().unwrap()) as usize;
        if attr_len == 0 || off + attr_len > rec.len() {
            break;
        }
        let non_resident = rec[off + 8] != 0;
        let name_len = rec[off + 9];
        // let name_offset = u16::from_le_bytes([rec[off + 10], rec[off + 11]]) as usize;
        let attr_flags = u16::from_le_bytes([rec[off + 12], rec[off + 13]]);

        if attr_type == ATTR_STANDARD_INFORMATION && !non_resident {
            // $STANDARD_INFORMATION（resident）
            let value_off = u16::from_le_bytes([rec[off + 20], rec[off + 21]]) as usize;
            let value_len = u32::from_le_bytes([rec[off + 16], rec[off + 17], rec[off + 18], rec[off + 19]]) as usize;
            let content = off + value_off;
            // 布局：CreationTime(8) + LastModificationTime(8) + MftChangeTime(8) + AccessTime(8) + Flags(4)
            if content + 0x24 <= rec.len() && value_len >= 0x24 {
                base_entry.created_ft = u64::from_le_bytes(
                    rec[content + 0x00..content + 0x08].try_into().unwrap(),
                );
                base_entry.last_modified_ft = u64::from_le_bytes(
                    rec[content + 0x08..content + 0x10].try_into().unwrap(),
                );
                base_entry.accessed_ft = u64::from_le_bytes(
                    rec[content + 0x18..content + 0x20].try_into().unwrap(),
                );
                base_entry.attributes = u32::from_le_bytes(
                    rec[content + 0x20..content + 0x24].try_into().unwrap(),
                );
                if is_dir {
                    base_entry.attributes |= FILE_ATTRIBUTE_DIRECTORY;
                }
                if base_entry.attributes == 0 {
                    base_entry.attributes = FILE_ATTRIBUTE_NORMAL;
                }
            }
        } else if attr_type == ATTR_FILE_NAME && !non_resident {
            // $FILE_NAME（resident）
            let value_off = u16::from_le_bytes([rec[off + 20], rec[off + 21]]) as usize;
            let value_len = u32::from_le_bytes([rec[off + 16], rec[off + 17], rec[off + 18], rec[off + 19]]) as usize;
            let content = off + value_off;
            if content + 0x42 <= rec.len() && value_len >= 0x42 {
                let parent_ref = u64::from_le_bytes(rec[content..content + 8].try_into().unwrap());
                let parent_dir = parent_ref & 0x0000_FFFF_FFFF_FFFF;
                let ns = rec[content + 0x41]; // namespace
                let name_len_chars = rec[content + 0x40] as usize;
                // 跳过短名（ns==2 = DOS 8.3）
                if ns == 0x02 {
                    off += attr_len;
                    continue;
                }
                let name_bytes_len = name_len_chars * 2;
                if content + 0x42 + name_bytes_len <= rec.len() && name_len_chars > 0 {
                    let name_u16: Vec<u16> = rec[content + 0x42..content + 0x42 + name_bytes_len]
                        .chunks_exact(2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    let name = String::from_utf16_lossy(&name_u16);
                    // 跳过 . 和 ..
                    if name == "." || name == ".." {
                        off += attr_len;
                        continue;
                    }
                    ctx.parent_to_children
                        .entry(parent_dir)
                        .or_default()
                        .push(FileRecordName {
                            name,
                            base_record: base_record_index,
                        });
                }
            }
        } else if attr_type == ATTR_DATA {
            // $DATA
            if name_len > 0 {
                // 命名 $DATA（ADS）：检查 WofCompressedData
                let name_off = u16::from_le_bytes([rec[off + 10], rec[off + 11]]) as usize;
                let name_start = off + name_off;
                if name_start + (name_len as usize) * 2 <= rec.len() {
                    let stream_u16: Vec<u16> = rec[name_start..name_start + (name_len as usize) * 2]
                        .chunks_exact(2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    let stream_name = String::from_utf16_lossy(&stream_u16);
                    if stream_name == "WofCompressedData" {
                        if !non_resident {
                            // resident WofCompressedData
                            let value_len = u32::from_le_bytes([rec[off + 16], rec[off + 17], rec[off + 18], rec[off + 19]]) as u64;
                            base_entry.physical_size = (value_len + 7) & !7;
                        } else {
                            // non-resident WofCompressedData：检查 LowestVcn==0
                            let lowest_vcn = u64::from_le_bytes(rec[off + 16..off + 24].try_into().unwrap());
                            if lowest_vcn == 0 {
                                let alloc_len = u64::from_le_bytes(rec[off + 0x28..off + 0x30].try_into().unwrap());
                                base_entry.physical_size = alloc_len;
                            }
                        }
                    }
                }
                off += attr_len;
                continue;
            }
            // 未命名 $DATA
            if !non_resident {
                // resident：ValueLength = logical size，physical = (len+7)&~7
                let value_len = u32::from_le_bytes([rec[off + 16], rec[off + 17], rec[off + 18], rec[off + 19]]) as u64;
                base_entry.logical_size = value_len;
                base_entry.physical_size = (value_len + 7) & !7;
            } else {
                // non-resident：只在 LowestVcn==0 时有效
                let lowest_vcn = u64::from_le_bytes(rec[off + 16..off + 24].try_into().unwrap());
                if lowest_vcn == 0 {
                    let file_size = u64::from_le_bytes(rec[off + 0x30..off + 0x38].try_into().unwrap());
                    base_entry.logical_size = file_size;
                    // physical size：压缩/稀疏用 Compressed(0x40)，否则 AllocatedLength(0x28)
                    let is_compressed = attr_flags & 0x0001 != 0;
                    let is_sparse = attr_flags & 0x8000 != 0;
                    let phys = if is_compressed || is_sparse {
                        if off + 0x48 <= rec.len() {
                            u64::from_le_bytes(rec[off + 0x40..off + 0x48].try_into().unwrap())
                        } else {
                            0
                        }
                    } else {
                        u64::from_le_bytes(rec[off + 0x28..off + 0x30].try_into().unwrap())
                    };
                    if phys > 0 {
                        base_entry.physical_size = phys;
                    }
                }
            }
        } else if attr_type == ATTR_REPARSE_POINT && !non_resident {
            // $REPARSE_POINT（resident）
            let value_off = u16::from_le_bytes([rec[off + 20], rec[off + 21]]) as usize;
            let content = off + value_off;
            if content + 4 <= rec.len() {
                base_entry.reparse_tag = u32::from_le_bytes(rec[content..content + 4].try_into().unwrap());
                if base_entry.reparse_tag == IO_REPARSE_TAG_WOF {
                    base_entry.attributes |= FILE_ATTRIBUTE_COMPRESSED;
                }
            }
        }
        off += attr_len;
    }
}

/// 第二阶段：从指定 record 递归建树。
fn build_tree(
    ctx: &NtfsContext,
    record: u64,
    display_name: &str,
    depth: usize,
    size_counted: &mut std::collections::HashSet<u64>,
) -> Node {
    let is_reserved = record < NTFS_RESERVED_MAX;
    let base = ctx.base_file_records.get(&record);
    let (logical, physical, modified_ft, created_ft, accessed_ft, attributes, reparse_tag) = match base {
        Some(b) => (b.logical_size, b.physical_size, b.last_modified_ft, b.created_ft, b.accessed_ft, b.attributes, b.reparse_tag),
        None => (0, 0, 0, 0, 0, FILE_ATTRIBUTE_DIRECTORY, 0),
    };
    let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;

    // 硬链接处理：只有 Physical Size 去重，Logical Size 不去重
    //（和 WinDirStat 一致：GetSizePhysical() 对硬链接返回 0，GetSizeLogical() 总是返回完整值）
    let physical_to_use = if is_dir {
        physical // 目录不去重
    } else if size_counted.insert(record) {
        physical // 第一次遇到这个文件，physical 计入
    } else {
        0u64 // 硬链接的后续实例，physical=0（logical 仍然计入）
    };

    let mut children = Vec::new();
    if let Some(child_names) = ctx.parent_to_children.get(&record) {
        for cn in child_names {
            let child_base = ctx.base_file_records.get(&cn.base_record);
            let (c_logical, c_physical, c_modified, c_created, c_accessed, c_attrs, c_reparse) = match child_base {
                Some(b) => (b.logical_size, b.physical_size, b.last_modified_ft, b.created_ft, b.accessed_ft, b.attributes, b.reparse_tag),
                None => (0, 0, 0, 0, 0, 0, 0),
            };
            let c_is_dir = c_attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
            let c_is_reserved = cn.base_record < NTFS_RESERVED_MAX;
            if c_is_dir {
                let child_node = build_tree(ctx, cn.base_record, &cn.name, depth + 1, size_counted);
                children.push(child_node);
            } else {
                // 硬链接去重：只有 physical 去重，logical 不去重
                let cp = if size_counted.insert(cn.base_record) {
                    c_physical
                } else {
                    0u64
                };
                children.push(Node::new_file_with_meta(
                    cn.name.clone(),
                    c_logical,  // logical 总是完整值
                    cp,         // physical 只第一次计入
                    file_color(),
                    c_modified,
                    c_created,
                    c_accessed,
                    c_attrs,
                    c_reparse,
                    c_is_reserved,
                    String::new(),
                ));
            }
        }
    }

    if is_dir {
        Node::new_folder_with_meta(
            display_name,
            folder_color(depth),
            children,
            modified_ft,
            created_ft,
            accessed_ft,
            attributes,
            reparse_tag,
            is_reserved,
            String::new(),
        )
    } else {
        Node::new_file_with_meta(
            display_name.to_string(),
            logical,         // logical 总是完整值
            physical_to_use, // physical 只第一次计入
            file_color(),
            modified_ft,
            created_ft,
            accessed_ft,
            attributes,
            reparse_tag,
            is_reserved,
            String::new(),
        )
    }
}

/// 系统报告的磁盘空间（GetDiskFreeSpaceExW）。
pub fn get_disk_space(drive_letter: char) -> Option<(u64, u64)> {
    let path = wide(&format!("{drive_letter}:\\"));
    unsafe {
        let mut free_bytes = 0u64;
        let mut total_bytes = 0u64;
        let ok = GetDiskFreeSpaceExW(path.as_ptr(), null_mut(), &mut total_bytes, &mut free_bytes);
        if ok == 0 { None } else { Some((total_bytes, free_bytes)) }
    }
}
