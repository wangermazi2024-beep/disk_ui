//! 直接读取 NTFS `$MFT`（Master File Table）来枚举整个卷的文件/文件夹，
//! 原理和 WizTree / Everything 一致：
//!
//! ## 正确的 MFT 读取方法（关键！）
//!
//! **不要打开 `\\.\X:\$MFT` 文件**——NTFS 驱动在 `NtfsCommonCreate` 里硬性
//! 禁止用户态 `ReadFile` 读 `$MFT`，这是驱动层的检查（不是 ACL 检查），
//! 连 SYSTEM + SeBackupPrivilege 都绕不过去，会返回 `ACCESS_DENIED (5)`。
//!
//! 正确做法（Everything / WizTree / `ntfs-reader` 都用的方法）：
//! 1. 打开**卷设备** `\\.\X:`（用 GENERIC_READ + FILE_FLAG_NO_BUFFERING），
//!    这个句柄能成功拿到——和 `FSCTL_GET_NTFS_VOLUME_DATA` 用的是同一个。
//! 2. 用 `FSCTL_GET_NTFS_VOLUME_DATA` 拿 `MftStartLcn`（MFT 在卷上的起始簇）。
//! 3. **处理 MFT 碎片**：用 `FILE_READ_ATTRIBUTES`（这个能成功）打开 `X:\$MFT`
//!    作为文件，调 `FSCTL_GET_RETRIEVAL_POINTERS` 拿 MFT 的簇映射表（run list）。
//!    如果 MFT 是单个连续 run（常见情况），就直接用 `MftStartLcn`。
//! 4. 在卷设备句柄上 `SetFilePointerEx` 定位到 MFT 的物理偏移，`ReadFile`
//!    按 128 条记录的块读取。
//! 5. 每条记录做 USA Fixup，遍历属性链提取 `$FILE_NAME` / `$DATA` / `$STANDARD_INFORMATION`。
//!
//! 这个方法不需要 `SeBackupPrivilege`（因为不读 `$MFT` 文件，只读卷设备），
//! 也不需要 `SeManageVolumePrivilege`。只要管理员身份 + 卷设备 GENERIC_READ。
//!
//! ## 模块拆分
//! 纯字节解析逻辑在 `crate::mft_parse`，本模块只剩 Windows 专有的 I/O。

#![cfg(windows)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::mpsc::Sender;

use egui::Color32;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, ReadFile, SetFilePointerEx, FILE_BEGIN,
    FILE_FLAG_NO_BUFFERING, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_SHARE_DELETE, FILE_FLAG_BACKUP_SEMANTICS, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_GET_RETRIEVAL_POINTERS,
    NTFS_VOLUME_DATA_BUFFER, RETRIEVAL_POINTERS_BUFFER, STARTING_VCN_INPUT_BUFFER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::model::Node;
use crate::mft_parse::{
    apply_fixup, find_attribute_list_content, find_unnamed_data_size, parse_attribute_list,
    parse_record, RawEntry, ROOT_RECORD_INDEX,
};

/// `CreateFileW` 的访问模式常量。`GENERIC_READ` 在 windows-sys 0.59 里是
/// `GENERIC_ACCESS_RIGHTS`（u32 的新类型），不能直接传给 `CreateFileW` 的
/// `dwDesiredAccess: u32` 参数，这里转成裸 u32。
const GENERIC_READ_U32: u32 = GENERIC_READ;

pub struct MftError(pub String);
impl std::fmt::Display for MftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
fn last_err(ctx: &str) -> MftError {
    MftError(format!("{} (GetLastError={})", ctx, unsafe { GetLastError() }))
}

/// 当前进程是否以管理员身份提升运行。读卷设备 `$MFT` 区域需要这个。
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

/// 判断某个盘符是否为 NTFS，且当前权限下可以直接读卷设备。
///
/// **注意**：这个检查只验证"管理员身份 + 卷是 NTFS + 能打开卷设备"。
/// 真正读 MFT 是通过卷设备的 raw read，不需要 `SeBackupPrivilege`。
pub fn mft_scan_available(drive_letter: char) -> bool {
    if !is_elevated() {
        eprintln!(
            "[mft_scan] 不可用：当前进程非管理员 (drive={})",
            drive_letter
        );
        return false;
    }
    let path = wide(&format!(r"\\.\{drive_letter}:"));
    unsafe {
        // 注意：这里用 FILE_FLAG_NO_BUFFERING，和真正读 MFT 时一致。
        let h = CreateFileW(
            path.as_ptr(),
            GENERIC_READ_U32,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING,
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
            "[mft_scan] 可用：drive={} 是 NTFS，BytesPerCluster={}, BytesPerFileRecordSegment={}, BytesPerSector={}, MftStartLcn={}, MftValidDataLength={}",
            drive_letter,
            buf.BytesPerCluster,
            buf.BytesPerFileRecordSegment,
            buf.BytesPerSector,
            buf.MftStartLcn,
            buf.MftValidDataLength
        );
        true
    }
}

struct VolumeInfo {
    bytes_per_cluster: u64,
    bytes_per_sector: u32,
    bytes_per_file_record_segment: u32,
    mft_start_lcn: u64,
    mft_valid_data_length: u64,
}

/// 打开卷设备 `\\.\X:`，拿 `NTFS_VOLUME_DATA_BUFFER`，返回句柄 + 卷信息。
///
/// 句柄用 `GENERIC_READ + FILE_FLAG_NO_BUFFERING` 打开，后续用来 raw-read MFT。
fn open_volume_and_get_info(drive_letter: char) -> Result<(HANDLE, VolumeInfo), MftError> {
    let path = wide(&format!(r"\\.\{drive_letter}:"));
    unsafe {
        eprintln!("[mft_scan] 打开卷设备: \\\\.\\{drive_letter}:");
        let h = CreateFileW(
            path.as_ptr(),
            GENERIC_READ_U32,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING,
            null_mut(),
        );
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            return Err(last_err(&format!(
                "无法打开卷设备 \\\\.\\{drive_letter}:（需要管理员权限）"
            )));
        }
        eprintln!("[mft_scan] 卷设备句柄已打开: handle={:p}", h as *const ());

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
            let e = last_err("FSCTL_GET_NTFS_VOLUME_DATA 失败（该卷可能不是 NTFS）");
            CloseHandle(h);
            return Err(e);
        }
        let info = VolumeInfo {
            bytes_per_cluster: buf.BytesPerCluster as u64,
            bytes_per_sector: buf.BytesPerSector,
            bytes_per_file_record_segment: buf.BytesPerFileRecordSegment,
            mft_start_lcn: buf.MftStartLcn as u64,
            mft_valid_data_length: buf.MftValidDataLength as u64,
        };
        eprintln!(
            "[mft_scan] 卷信息: BytesPerCluster={}, BytesPerSector={}, BytesPerFileRecordSegment={}, MftStartLcn={}, MftValidDataLength={} ({:.2} MB)",
            info.bytes_per_cluster,
            info.bytes_per_sector,
            info.bytes_per_file_record_segment,
            info.mft_start_lcn,
            info.mft_valid_data_length,
            info.mft_valid_data_length as f64 / 1e6
        );
        Ok((h, info))
    }
}

/// MFT 的一个物理连续段（run）：起始 VCN、起始 LCN、簇数。
struct MftRun {
    start_vcn: u64,
    start_lcn: u64,
    cluster_count: u64,
}

/// 用 `FSCTL_GET_RETRIEVAL_POINTERS` 拿 MFT 的簇映射表。
///
/// 这是处理 MFT 碎片化的正确方法——MFT 本身可能被分到多个不连续的物理区域。
/// 打开 `X:\$MFT` 用 `FILE_READ_ATTRIBUTES`（这个能成功，不像 GENERIC_READ 会被拒），
/// 然后查它的 retrieval pointers。
///
/// 失败时返回空 vec，调用方应该退回到"单 run 假设"（用 `MftStartLcn`）。
fn get_mft_runs(drive_letter: char, info: &VolumeInfo) -> Vec<MftRun> {
    let mft_path = wide(&format!(r"{}:\$MFT", drive_letter));
    unsafe {
        eprintln!("[mft_scan] 打开 $MFT 文件（FILE_READ_ATTRIBUTES）拿 retrieval pointers");
        let h = CreateFileW(
            mft_path.as_ptr(),
            FILE_READ_ATTRIBUTES as u32,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        );
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            eprintln!(
                "[mft_scan] 打开 $MFT 失败 (GetLastError={})，退回到单 run 假设（用 MftStartLcn）",
                GetLastError()
            );
            // fallback：单 run，从 MftStartLcn 开始，覆盖整个 MftValidDataLength
            let cluster_count = info.mft_valid_data_length / info.bytes_per_cluster;
            return vec![MftRun {
                start_vcn: 0,
                start_lcn: info.mft_start_lcn,
                cluster_count,
            }];
        }

        let mut input = STARTING_VCN_INPUT_BUFFER { StartingVcn: 0 };
        // 给一个足够大的输出缓冲区。每个 extent 占 16 字节，1024 个 extent 足够大多数卷。
        const MAX_EXTENTS: usize = 1024;
        let out_size = std::mem::size_of::<u32>() + std::mem::size_of::<i64>()
            + MAX_EXTENTS * (std::mem::size_of::<i64>() + std::mem::size_of::<i64>());
        let mut out_buf: Vec<u8> = vec![0u8; out_size];
        let mut ret = 0u32;
        let ok = DeviceIoControl(
            h,
            FSCTL_GET_RETRIEVAL_POINTERS,
            &mut input as *mut _ as *mut _,
            std::mem::size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
            out_buf.as_mut_ptr() as *mut _,
            out_size as u32,
            &mut ret,
            null_mut(),
        );
        CloseHandle(h);

        if ok == 0 {
            eprintln!(
                "[mft_scan] FSCTL_GET_RETRIEVAL_POINTERS 失败 (GetLastError={})，退回到单 run 假设",
                GetLastError()
            );
            let cluster_count = info.mft_valid_data_length / info.bytes_per_cluster;
            return vec![MftRun {
                start_vcn: 0,
                start_lcn: info.mft_start_lcn,
                cluster_count,
            }];
        }

        // 解析 RETRIEVAL_POINTERS_BUFFER
        let rp = &*(out_buf.as_ptr() as *const RETRIEVAL_POINTERS_BUFFER);
        let extent_count = rp.ExtentCount as usize;
        eprintln!(
            "[mft_scan] MFT 有 {} 个 extent（连续段），StartingVcn={}",
            extent_count, rp.StartingVcn
        );

        if extent_count == 0 {
            eprintln!("[mft_scan] MFT 没有 extent？退回到单 run 假设");
            let cluster_count = info.mft_valid_data_length / info.bytes_of_cluster();
            return vec![MftRun {
                start_vcn: 0,
                start_lcn: info.mft_start_lcn,
                cluster_count,
            }];
        }

        // Extents 是 [RETRIEVAL_POINTERS_BUFFER_0; 1]，但实际是柔性数组。
        // 用指针偏移读出所有 extent。
        use windows_sys::Win32::System::Ioctl::RETRIEVAL_POINTERS_BUFFER_0;
        let extents_ptr: *const RETRIEVAL_POINTERS_BUFFER_0 = &rp.Extents[0];
        let mut runs = Vec::with_capacity(extent_count);
        let mut prev_vcn = rp.StartingVcn as u64;
        for i in 0..extent_count {
            let ext = &*extents_ptr.add(i);
            let next_vcn = ext.NextVcn as u64;
            let lcn = ext.Lcn as u64;
            // Lcn == -1 表示 sparse（洞），跳过
            if lcn != u64::MAX {
                let count = next_vcn.saturating_sub(prev_vcn);
                runs.push(MftRun {
                    start_vcn: prev_vcn,
                    start_lcn: lcn,
                    cluster_count: count,
                });
                eprintln!(
                    "[mft_scan]   extent[{}]: VCN={} LCN={} clusters={}",
                    i, prev_vcn, lcn, count
                );
            } else {
                eprintln!(
                    "[mft_scan]   extent[{}]: VCN={} SPARSE (跳过)",
                    i, prev_vcn
                );
            }
            prev_vcn = next_vcn;
        }
        if runs.is_empty() {
            eprintln!("[mft_scan] 所有 extent 都是 sparse？退回到单 run 假设");
            let cluster_count = info.mft_valid_data_length / info.bytes_per_cluster;
            return vec![MftRun {
                start_vcn: 0,
                start_lcn: info.mft_start_lcn,
                cluster_count,
            }];
        }
        runs
    }
}

impl VolumeInfo {
    fn bytes_per_cluster(&self) -> u64 {
        self.bytes_per_cluster
    }
    fn bytes_of_cluster(&self) -> u64 {
        self.bytes_per_cluster
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
    pub file_paths: Vec<PathBuf>,
    pub file_sizes: Vec<u64>,
}

/// 核心入口：对给定盘符做一次完整的 MFT 直读扫描。
///
/// 步骤：
/// 1. 检查管理员权限
/// 2. 打开卷设备 `\\.\X:` + 拿 `NTFS_VOLUME_DATA_BUFFER`
/// 3. 拿 MFT 的簇映射表（处理碎片化）
/// 4. 按 run 逐块 ReadFile，解析每条记录
/// 5. 建邻接表 + 从根目录（记录 5）递归建树
pub fn scan_drive_via_mft(
    drive_letter: char,
    tx: &Sender<crate::scan::ScanMessage>,
) -> Result<MftScanResult, MftError> {
    if !is_elevated() {
        return Err(MftError(
            "直读 MFT 需要管理员权限运行本程序（右键\"以管理员身份运行\"）".into(),
        ));
    }

    let (vol_handle, info) = open_volume_and_get_info(drive_letter)?;
    let record_size = info.bytes_per_file_record_segment.max(1024) as usize;
    let sector_size = info.bytes_per_sector.max(512) as u64;
    let cluster_size = info.bytes_per_cluster.max(sector_size);

    let total_records = info.mft_valid_data_length / record_size as u64;
    eprintln!(
        "[mft_scan] 开始读 MFT: total_records={}, record_size={}B, sector_size={}B, cluster_size={}B",
        total_records, record_size, sector_size, cluster_size
    );

    // 拿 MFT 的簇映射表（处理碎片化）
    let runs = get_mft_runs(drive_letter, &info);
    eprintln!("[mft_scan] MFT 共 {} 个物理 run", runs.len());

    // 第一遍：按 run 顺序读 MFT 数据，解析所有记录。
    // MFT 记录号 = (已读字节数 / record_size)。因为 run 是按 VCN 升序的，
    // VCN 0 对应记录 0，所以连续读下来记录号是连续递增的。
    let mut entries: Vec<Option<RawEntry>> = Vec::with_capacity(total_records as usize);
    // 保存需要二次解析大小的记录的原始字节（real_size==0 的文件，其 $DATA 在扩展记录里）
    // key = 记录号，value = fixup 后的记录字节
    let mut records_needing_size: HashMap<u64, Vec<u8>> = HashMap::new();
    let mut valid_count = 0usize;
    let mut dir_count = 0usize;
    let mut file_count = 0usize;
    let mut records_read: u64 = 0;
    let mut files_needing_size_resolve = 0u64; // real_size==0 的文件数
    // 诊断计数器
    let mut fixup_failed = 0u64;
    let mut bad_magic = 0u64;
    let mut not_in_use = 0u64;
    let mut not_base = 0u64;
    let mut no_file_name = 0u64;

    // 一次读 128 条记录（约 128KB），按 sector 对齐
    const RECORDS_PER_CHUNK: usize = 128;
    let chunk_records = RECORDS_PER_CHUNK;
    let chunk_bytes_unaligned = chunk_records * record_size;
    let chunk_bytes = (chunk_bytes_unaligned + sector_size as usize - 1)
        / sector_size as usize
        * sector_size as usize;
    let mut chunk_buf: Vec<u8> = vec![0u8; chunk_bytes];
    eprintln!(
        "[mft_scan] 读取块大小: {} 字节 ({} 条记录，sector 对齐)",
        chunk_bytes, chunk_records
    );

    for (run_idx, run) in runs.iter().enumerate() {
        let run_bytes = run.cluster_count * cluster_size;
        let run_records = run_bytes / record_size as u64;
        let run_offset_bytes = run.start_lcn * cluster_size;
        eprintln!(
            "[mft_scan] 处理 run[{}]: 起始记录={}, LCN={}, 字节={} ({:.2} MB), 记录数={}",
            run_idx,
            records_read,
            run.start_lcn,
            run_bytes,
            run_bytes as f64 / 1e6,
            run_records
        );

        // 定位到 run 的物理偏移
        unsafe {
            let mut new_pos: i64 = 0;
            let ok = SetFilePointerEx(
                vol_handle,
                run_offset_bytes as i64,
                &mut new_pos,
                FILE_BEGIN,
            );
            if ok == 0 {
                let e = last_err(&format!(
                    "SetFilePointerEx 失败 (run={}, offset={})",
                    run_idx, run_offset_bytes
                ));
                CloseHandle(vol_handle);
                return Err(e);
            }
        }

        // 逐块读
        let mut records_in_run_left = run_records as usize;
        while records_in_run_left > 0 {
            let recs_this = records_in_run_left.min(chunk_records);
            let to_read_unaligned = recs_this * record_size;
            let to_read = (to_read_unaligned + sector_size as usize - 1)
                / sector_size as usize
                * sector_size as usize;
            let mut bytes_returned: u32 = 0;
            let ok = unsafe {
                ReadFile(
                    vol_handle,
                    chunk_buf.as_mut_ptr(),
                    to_read as u32,
                    &mut bytes_returned,
                    null_mut(),
                )
            };
            if ok == 0 || bytes_returned == 0 {
                eprintln!(
                    "[mft_scan] ReadFile 提前结束 (run={}, records_read={}, bytes_returned={}, GetLastError={})",
                    run_idx, records_read, bytes_returned, unsafe { GetLastError() }
                );
                break;
            }
            // 实际读到的记录数（可能比请求的少，比如最后一个块）
            let actual_recs = (bytes_returned as usize / record_size).min(recs_this);
            for i in 0..actual_recs {
                let start = i * record_size;
                let end = start + record_size;
                let rec = &mut chunk_buf[start..end];
                // 先检查 magic，区分"零填充/空洞"和"真实损坏记录"
                if rec.len() < 4 || &rec[0..4] != b"FILE" {
                    bad_magic += 1;
                    entries.push(None);
                    records_read += 1;
                    continue;
                }
                // apply_fixup 会修改 rec，但 chunk_buf 下一轮会被覆盖，所以直接改没问题
                if !apply_fixup(rec, sector_size as u32) {
                    fixup_failed += 1;
                    entries.push(None);
                    records_read += 1;
                    continue;
                }
                // v11: 用 parse_record（不再用 diag 版，no_file_name 记录直接返回 None）
                let parsed_opt = parse_record(rec);
                let parsed = match parsed_opt {
                    None => {
                        // parse_record 返回 None 的原因：magic 不对 / 长度不够 / 没 $FILE_NAME
                        // 区分一下：如果 magic 是 FILE 但没 $FILE_NAME，算 no_file_name
                        if rec.len() >= 4 && &rec[0..4] == b"FILE" {
                            no_file_name += 1;
                        } else {
                            fixup_failed += 1;
                        }
                        entries.push(None);
                        records_read += 1;
                        continue;
                    }
                    Some(e) => {
                        if !e.in_use {
                            not_in_use += 1;
                            entries.push(None);
                            records_read += 1;
                            continue;
                        }
                        if !e.is_base_record {
                            not_base += 1;
                            entries.push(None);
                            records_read += 1;
                            continue;
                        }
                        e
                    }
                };
                // v11: 如果是文件（非目录）且 real_size==0，保存记录字节用于二次解析
                // （大文件的 $DATA 可能在扩展记录里，需要跟 $ATTRIBUTE_LIST 去拿）
                if !parsed.is_dir && parsed.real_size == 0 {
                    files_needing_size_resolve += 1;
                    records_needing_size.insert(records_read, rec.to_vec());
                }
                valid_count += 1;
                if parsed.is_dir {
                    dir_count += 1;
                } else {
                    file_count += 1;
                }
                entries.push(Some(parsed));
                records_read += 1;
            }
            records_in_run_left -= actual_recs;

            if records_read % 20_000 < chunk_records as u64 {
                let _ = tx.send(crate::scan::ScanMessage::Progress(records_read));
            }
        }
    }
    // 关闭卷设备句柄
    unsafe { CloseHandle(vol_handle); }

    eprintln!(
        "[mft_scan] 解析完成: 总记录={}, 有效={}, 目录={}, 文件={}",
        entries.len(),
        valid_count,
        dir_count,
        file_count
    );
    eprintln!(
        "[mft_scan] 过滤统计: bad_magic={} (零填充/空洞), fixup_failed={} (USA损坏), not_in_use={} (已删除), not_base={} (扩展记录), no_file_name={} (无$FILE_NAME,已跳过)",
        bad_magic, fixup_failed, not_in_use, not_base, no_file_name
    );
    eprintln!(
        "[mft_scan] v11: 有 {} 个文件 real_size==0（$DATA 可能在扩展记录，需二次解析）",
        files_needing_size_resolve
    );

    // v11 第二阶段：解析 real_size==0 的文件的真实大小。
    // 这些文件的 $DATA 在扩展记录里，需要：
    //   1. 从 base record 拿 $ATTRIBUTE_LIST
    //   2. 遍历 $ATTRIBUTE_LIST 找 type==0x80($DATA) && lowest_vcn==0 的条目
    //   3. 读对应扩展记录，从中找未命名 $DATA 的 data_size
    //
    // 同时，扩展记录本身也在我们的 entries 里（is_base_record=false，被跳过），
    // 但我们这里需要按记录号随机读，所以要重新打开卷设备。
    if !records_needing_size.is_empty() {
        eprintln!(
            "[mft_scan] v11: 开始二次解析 {} 个文件的大小（跟 $ATTRIBUTE_LIST 读扩展记录）",
            records_needing_size.len()
        );
        let size_resolve_start = std::time::Instant::now();
        let mut resolved_count = 0u64;
        let mut resolve_failed = 0u64;

        // 重新打开卷设备读扩展记录
        let vol_path = wide(&format!(r"\\.\{drive_letter}:"));
        let h2 = unsafe {
            CreateFileW(
                vol_path.as_ptr(),
                GENERIC_READ_U32,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_NO_BUFFERING,
                null_mut(),
            )
        };
        if h2 == INVALID_HANDLE_VALUE || h2.is_null() {
            eprintln!(
                "[mft_scan] v11: 二次解析时打开卷设备失败 (GetLastError={})，跳过大小解析",
                unsafe { GetLastError() }
            );
        } else {
            // 需要读的扩展记录号集合（从 $ATTRIBUTE_LIST 里解析出来）
            // 为了减少 SetFilePointerEx 次数，先收集所有要读的记录号，再批量读
            let mut ext_records_to_read: std::collections::HashSet<u64> = std::collections::HashSet::new();
            // (base_record_number, ext_record_numbers[]) 映射
            let mut base_to_ext: HashMap<u64, Vec<u64>> = HashMap::new();
            for (base_rec_num, rec_bytes) in &records_needing_size {
                if let Some(alist_content) = find_attribute_list_content(rec_bytes) {
                    let alist_entries = parse_attribute_list(alist_content);
                    for entry in &alist_entries {
                        // 找 type==0x80($DATA) && lowest_vcn==0 的条目
                        //（lowest_vcn==0 的 extent 才有完整 data_size）
                        if entry.attr_type == crate::mft_parse::ATTR_DATA && entry.lowest_vcn == 0 {
                            ext_records_to_read.insert(entry.record_number);
                            base_to_ext.entry(*base_rec_num).or_default().push(entry.record_number);
                        }
                    }
                }
            }
            eprintln!(
                "[mft_scan] v11: 需要读 {} 个扩展记录来解析大小",
                ext_records_to_read.len()
            );

            // 读所有需要的扩展记录到内存（record_number -> fixup 后的字节）
            let mut ext_record_bytes: HashMap<u64, Vec<u8>> = HashMap::new();
            for ext_rec_num in &ext_records_to_read {
                // 用 MftStartLcn + record_size * record_number 计算偏移
                // 但 MFT 是碎片化的，要按 VCN -> LCN 映射找物理位置
                // record_number 对应 VCN = record_number * record_size / cluster_size
                let vcn = ext_rec_num * record_size as u64 / cluster_size;
                // 在 runs 里找包含这个 VCN 的 run
                let mut phys_lcn: Option<u64> = None;
                let mut run_cluster_offset: u64 = 0;
                for run in &runs {
                    let run_vcn_start = run.start_vcn;
                    let run_vcn_end = run.start_vcn + run.cluster_count;
                    if vcn >= run_vcn_start && vcn < run_vcn_end {
                        phys_lcn = Some(run.start_lcn + (vcn - run_vcn_start));
                        run_cluster_offset = vcn - run_vcn_start;
                        break;
                    }
                }
                let Some(lcn) = phys_lcn else { continue };
                let phys_offset = lcn * cluster_size;
                // 记录在 cluster 里的偏移
                let record_offset_in_cluster = (ext_rec_num * record_size as u64) % cluster_size;
                let read_offset = phys_offset + record_offset_in_cluster;

                // 定位 + 读一个 record_size（按 sector 对齐）
                let mut new_pos: i64 = 0;
                let ok = unsafe { SetFilePointerEx(h2, read_offset as i64, &mut new_pos, FILE_BEGIN) };
                if ok == 0 { continue; }
                let to_read = ((record_size + sector_size as usize - 1) / sector_size as usize) * sector_size as usize;
                let mut buf = vec![0u8; to_read];
                let mut bytes_returned: u32 = 0;
                let ok = unsafe { ReadFile(h2, buf.as_mut_ptr(), to_read as u32, &mut bytes_returned, null_mut()) };
                if ok == 0 || (bytes_returned as usize) < record_size { continue; }
                let mut rec = buf[..record_size].to_vec();
                if !apply_fixup(&mut rec, sector_size as u32) { continue; }
                ext_record_bytes.insert(*ext_rec_num, rec);
            }
            unsafe { CloseHandle(h2); }
            eprintln!("[mft_scan] v11: 已读入 {} 个扩展记录", ext_record_bytes.len());

            // 现在用扩展记录解析每个文件的大小
            for (base_rec_num, _) in &records_needing_size {
                let Some(entry_opt) = entries.get_mut(*base_rec_num as usize) else { continue };
                let Some(entry) = entry_opt.as_mut() else { continue };
                let mut found_size: Option<u64> = None;
                if let Some(ext_recs) = base_to_ext.get(base_rec_num) {
                    for ext_rec_num in ext_recs {
                        if let Some(ext_bytes) = ext_record_bytes.get(ext_rec_num) {
                            // 在扩展记录里找未命名 $DATA 的 data_size
                            if let Some(size) = find_unnamed_data_size(ext_bytes) {
                                found_size = Some(size);
                                break;
                            }
                        }
                    }
                }
                if let Some(size) = found_size {
                    entry.real_size = size;
                    resolved_count += 1;
                } else {
                    resolve_failed += 1;
                }
            }
        }
        let elapsed = size_resolve_start.elapsed();
        eprintln!(
            "[mft_scan] v11: 大小解析完成: 成功 {}, 失败 {}, 耗时 {:.1}s",
            resolved_count, resolve_failed, elapsed.as_secs_f64()
        );
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
    let mut self_modified: u64 = 0;
    let mut self_attrs: u32 = 0x10;
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

/// 用 `GetDiskFreeSpaceExW` 拿该盘符官方报告的总容量/可用空间。
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
