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
use crate::mft_parse::{RawEntry, ROOT_RECORD_INDEX};

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

/// `mft` 库把 $STANDARD_INFORMATION 里的 FILETIME 转成了 `jiff::Timestamp`（见它的
/// `windows_filetime_to_timestamp`），但我们项目下游（`format.rs::format_filetime_local`）
/// 是按原始 Windows FILETIME（1601-01-01 起 100ns 单位）来格式化的，所以这里转回去。
/// 只做到秒级精度（原逻辑本来也只用来显示到分钟），足够用。
fn jiff_timestamp_to_windows_filetime(ts: jiff::Timestamp) -> u64 {
    const WINDOWS_TO_UNIX_EPOCH_SECS: i64 = 11_644_473_600;
    let unix_secs = ts.as_second();
    let windows_secs = unix_secs + WINDOWS_TO_UNIX_EPOCH_SECS;
    if windows_secs <= 0 {
        0
    } else {
        (windows_secs as u64).saturating_mul(10_000_000)
    }
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

/// 用 mapping pairs（NTFS data runs）手动读取一个 non-resident 属性的完整内容。
///
/// non-resident `$ATTRIBUTE_LIST` 没有对应的文件路径可以 `CreateFileW` 打开去查
/// retrieval pointers（不像 `$MFT` 那样可以按路径打开），只能靠属性头里自带的
/// mapping pairs 编码直接算出物理簇号，然后从卷设备原始读取。
///
/// `vol_handle` 必须是已经用 `GENERIC_READ + FILE_FLAG_NO_BUFFERING` 打开的卷设备句柄。
/// 返回按 VCN 顺序拼接、并截断到 `data_size` 的完整内容；任何一步失败都返回 `None`
/// （调用方应该把这个文件计入"大小解析失败"，而不是静默当成 0）。
/// v14: 这个函数不再被调用——扩展记录现在直接从 `mft` 库解析出的
/// 内存表（`by_record`）里查，不需要再手动读盘重建 non-resident 属性内容。
/// 保留函数体只是为了方便你对照/回滚，可以安全删除。
#[allow(dead_code)]
fn read_nonresident_attribute(
    vol_handle: HANDLE,
    cluster_size: u64,
    sector_size: u32,
    mapping_pairs: &[u8],
    data_size: u64,
) -> Option<Vec<u8>> {
    let runs = crate::mft_parse::parse_data_runs(mapping_pairs);
    if runs.is_empty() || data_size == 0 {
        return None;
    }
    let mut content: Vec<u8> = Vec::with_capacity(data_size as usize);
    for (cluster_count, lcn_opt) in runs {
        if content.len() as u64 >= data_size {
            break;
        }
        let run_bytes = cluster_count * cluster_size;
        match lcn_opt {
            None => {
                // sparse run：没有物理簇，视为全 0（$ATTRIBUTE_LIST 一般不会是 sparse，
                // 但按标准做法兜底处理，避免读越界的物理位置）。
                content.resize(content.len() + run_bytes as usize, 0);
            }
            Some(lcn) if lcn >= 0 => {
                let phys_offset = (lcn as u64) * cluster_size;
                let to_read = ((run_bytes as usize + sector_size as usize - 1)
                    / sector_size as usize)
                    * sector_size as usize;
                let mut buf = vec![0u8; to_read];
                let mut new_pos: i64 = 0;
                let ok = unsafe {
                    SetFilePointerEx(vol_handle, phys_offset as i64, &mut new_pos, FILE_BEGIN)
                };
                if ok == 0 {
                    return None;
                }
                let mut bytes_returned: u32 = 0;
                let ok = unsafe {
                    ReadFile(
                        vol_handle,
                        buf.as_mut_ptr(),
                        to_read as u32,
                        &mut bytes_returned,
                        null_mut(),
                    )
                };
                if ok == 0 || (bytes_returned as usize) < run_bytes.min(to_read as u64) as usize {
                    return None;
                }
                buf.truncate(run_bytes as usize);
                content.extend_from_slice(&buf);
            }
            Some(_negative_lcn) => {
                // LCN 算出来是负数，说明 mapping pairs 解析有问题，放弃这个属性。
                return None;
            }
        }
    }
    if (content.len() as u64) < data_size {
        return None;
    }
    content.truncate(data_size as usize);
    Some(content)
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
    /// 按 **base MFT record** 去重后的物理大小总和（每份数据只算一次，不管
    /// 它有几个硬链接名字/挂在几个目录下）。这是跟"系统报告的已用空间"做
    /// 一致性比较时应该用的数字。
    ///
    /// 注意 `root.size`（树的汇总大小）跟这个数字**不是一回事、也不应该相等**：
    /// `root.size` 是"资源管理器逐目录浏览时看到的大小之和"——同一份硬链接数据
    /// 如果挂在 3 个目录下，会在 3 个目录里各计一次，这是 Explorer/WizTree 的标准
    /// 行为，不是 bug；`dedup_size` 才是"这份数据在磁盘上实际占了多少物理空间"。
    pub dedup_size: u64,
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

    // ── v14：把手撸的 fixup + 属性遍历 + 扩展记录二次读盘，换成
    //    omerbenamram/mft 这个经过审计的库来做 ──────────────────────────
    //
    // 之前的做法分两步：第一遍解析拿不到大小的文件记下来，第二遍重新打开卷设备，
    // 按 $ATTRIBUTE_LIST 里的记录号手动算物理偏移再读一次盘、再手动 fixup。
    // 用库之后不需要这么麻烦：我们一次性把整个 MFT 的字节（含所有 base record
    // 和扩展记录）读到一块连续内存里交给 `mft::MftParser::from_buffer`，
    // 库会解析出**所有**记录（包括扩展记录），我们直接在内存里查表就能拿到
    // 扩展记录里的 $DATA，不用再重新读盘。
    let mut mft_buffer: Vec<u8> = Vec::with_capacity((total_records as usize) * record_size);
    for (run_idx, run) in runs.iter().enumerate() {
        let run_bytes = run.cluster_count * cluster_size;
        let run_offset_bytes = run.start_lcn * cluster_size;
        eprintln!(
            "[mft_scan] 读取 run[{}] 到缓冲区: LCN={}, 字节={} ({:.2} MB)",
            run_idx, run.start_lcn, run_bytes, run_bytes as f64 / 1e6
        );
        unsafe {
            let mut new_pos: i64 = 0;
            let ok = SetFilePointerEx(vol_handle, run_offset_bytes as i64, &mut new_pos, FILE_BEGIN);
            if ok == 0 {
                let e = last_err(&format!("SetFilePointerEx 失败 (run={}, offset={})", run_idx, run_offset_bytes));
                CloseHandle(vol_handle);
                return Err(e);
            }
        }
        // 按 sector 对齐分块读，避免一次性分配/读取过大的缓冲区。
        const READ_CHUNK: usize = 4 * 1024 * 1024; // 4MB 一块
        let mut remaining = run_bytes as usize;
        while remaining > 0 {
            let this_chunk = remaining.min(READ_CHUNK);
            let to_read = ((this_chunk + sector_size as usize - 1) / sector_size as usize) * sector_size as usize;
            let mut buf = vec![0u8; to_read];
            let mut bytes_returned: u32 = 0;
            let ok = unsafe {
                ReadFile(vol_handle, buf.as_mut_ptr(), to_read as u32, &mut bytes_returned, null_mut())
            };
            if ok == 0 || bytes_returned == 0 {
                eprintln!(
                    "[mft_scan] ReadFile 提前结束 (run={}, GetLastError={})",
                    run_idx, unsafe { GetLastError() }
                );
                break;
            }
            buf.truncate(bytes_returned as usize);
            mft_buffer.extend_from_slice(&buf);
            remaining = remaining.saturating_sub(bytes_returned as usize);
        }
        let _ = tx.send(crate::scan::ScanMessage::Progress(
            (mft_buffer.len() / record_size) as u64,
        ));
    }
    unsafe { CloseHandle(vol_handle); }
    eprintln!(
        "[mft_scan] MFT 缓冲区读取完成: {} 字节 (~{} 条记录)",
        mft_buffer.len(),
        mft_buffer.len() / record_size
    );

    // MftParser::from_buffer / iter_entries 的用法已经对着 0.7.0 的实际源码核对过。
    let mut parser = mft::MftParser::from_buffer(mft_buffer)
        .map_err(|e| MftError(format!("mft 库解析 MFT 缓冲区失败: {e}")))?;

    // 先扫一遍，把每条记录的原始信息（含扩展记录）都存下来，方便后面按记录号查表
    // （扩展记录的 $DATA 就是这么查出来的，不用再手动读盘）。
    struct ParsedRec {
        is_dir: bool,
        in_use: bool,
        is_base_record: bool,
        file_names: Vec<(u64, String)>, // (parent_record, name) —— 保留全部，硬链接不再丢
        modified_ft: u64,
        attributes: u32,
        unnamed_data_size: Option<u64>, // 本条记录里未命名 $DATA 的大小（若有）
        attr_list: Vec<(u64, u16)>,     // $ATTRIBUTE_LIST：(扩展记录号, attribute_id)，仅 $DATA 类型
    }
    let mut by_record: HashMap<u64, ParsedRec> = HashMap::new();

    let mut valid_count = 0usize;
    let mut dir_count = 0usize;
    let mut file_count = 0usize;
    let mut fixup_failed = 0u64;
    let mut not_in_use = 0u64;
    let mut not_base = 0u64;
    let mut no_file_name = 0u64;

    for entry_result in parser.iter_entries() {
        let entry = match entry_result {
            Ok(e) => e,
            Err(_) => {
                fixup_failed += 1;
                continue;
            }
        };
        // 下面这些字段名已经对着 omerbenamram/mft 的实际源码（entry.rs / header.rs /
        // attribute/x10.rs / x30.rs / x20.rs）核对过，不再是猜测。
        let record_number = entry.header.record_number;
        // 库自带了这两个便利方法，直接用，不用自己翻 flags 位。
        let in_use = entry.is_allocated();
        let is_dir = entry.is_dir();
        let base_ref = entry.header.base_reference.entry; // MftReference{ entry, sequence }
        let is_base_record = base_ref == 0;

        if !in_use {
            not_in_use += 1;
            continue;
        }

        let mut file_names: Vec<(u8, u64, String)> = Vec::new(); // (namespace, parent, name)
        let mut modified_ft = 0u64;
        let mut attributes = 0u32;
        let mut unnamed_data_size: Option<u64> = None;
        let mut attr_list: Vec<(u64, u16)> = Vec::new();

        for attr in entry.iter_attributes().filter_map(|a| a.ok()) {
            // 未命名 $DATA 的大小在属性头里（resident 用 data_size，non-resident 用
            // file_size，且只有 vnc_first==0 的 extent 才有效），跟内容变体（Resident $DATA
            // 走 AttrX80，non-resident $DATA 其实走的是 DataRun 变体）无关，所以先在这里统一处理，
            // 不用在下面的 match 分支里再判断一次。
            if attr.header.type_code == mft::attribute::MftAttributeType::DATA
                && attr.header.name_size == 0
            {
                use mft::attribute::header::ResidentialHeader;
                unnamed_data_size = match &attr.header.residential_header {
                    ResidentialHeader::Resident(rh) => Some(rh.data_size as u64),
                    ResidentialHeader::NonResident(nrh) => {
                        if nrh.vnc_first == 0 { Some(nrh.file_size) } else { None }
                    }
                };
            }
            match &attr.data {
                mft::attribute::MftAttributeContent::AttrX10(std_info) => {
                    modified_ft = jiff_timestamp_to_windows_filetime(std_info.modified);
                    attributes = std_info.file_flags.bits();
                }
                mft::attribute::MftAttributeContent::AttrX30(fname) => {
                    file_names.push((
                        fname.namespace.clone() as u8, // FileNamespace 是 #[repr(u8)]，可以直接转
                        fname.parent.entry as u64,
                        fname.name.clone(),
                    ));
                }
                mft::attribute::MftAttributeContent::AttrX20(alist) => {
                    // $ATTRIBUTE_LIST：只关心指向别的记录的 $DATA(0x80) 条目。
                    // 注意：库里 `reserved` 字段的文档注释写的是"The attribute's id"
                    // （这是 NTFS 规范里这个字段的真实含义，字段名 reserved 只是历史遗留），
                    // 这就是我们之前叫 instance_id/attribute_id 的东西。
                    for e in &alist.entries {
                        if e.attribute_type == 0x80 {
                            attr_list.push((e.segment_reference.entry as u64, e.reserved));
                        }
                    }
                }
                _ => {}
            }
        }

        // 关键修正：扩展记录（is_base_record=false）通常没有 $FILE_NAME，
        // 但它们必须留在 by_record 表里——不然下面 base record 按 $ATTRIBUTE_LIST
        // 查扩展记录大小的时候会查不到，等于白做了 v14 这次改造。
        // 只有 base record 才需要有 $FILE_NAME，没有的话才真的算"无法显示"要跳过。
        if is_base_record && file_names.is_empty() {
            no_file_name += 1;
            continue;
        }
        if !is_base_record {
            not_base += 1;
        }

        let ns_priority = |ns: u8| -> u32 {
            match ns { 1 => 0, 3 => 1, 0 => 2, 2 => 3, _ => 4 }
        };
        // v14：不再只留一个，全部保留成 (parent, name) 列表——硬链接的每个位置都要显示。
        let mut sorted = file_names.clone();
        sorted.sort_by_key(|(ns, _, _)| ns_priority(*ns));
        let links: Vec<(u64, String)> = sorted.into_iter().map(|(_, p, n)| (p, n)).collect();

        if is_base_record {
            valid_count += 1;
            if is_dir { dir_count += 1 } else { file_count += 1 }
        }

        by_record.insert(record_number, ParsedRec {
            is_dir,
            in_use,
            is_base_record,
            file_names: links,
            modified_ft,
            attributes,
            unnamed_data_size,
            attr_list,
        });
    }

    eprintln!(
        "[mft_scan] 解析完成（mft 库）: 有效base={}, 目录={}, 文件={}, 无法解析={}, 已删除={}, 扩展记录={}, 无$FILE_NAME={}",
        valid_count, dir_count, file_count, fixup_failed, not_in_use, not_base, no_file_name
    );

    // 用 $ATTRIBUTE_LIST 把扩展记录里的 $DATA 大小接到 base record 上——
    // 现在直接查内存里的 by_record 表就行，不用再手动读盘、手动 fixup 了。
    let base_record_numbers: Vec<u64> = by_record
        .iter()
        .filter(|(_, r)| r.is_base_record)
        .map(|(k, _)| *k)
        .collect();
    let mut resolved_count = 0u64;
    let mut resolve_failed = 0u64;
    let mut resolved_sizes: HashMap<u64, u64> = HashMap::new();
    for rec_num in &base_record_numbers {
        let rec = &by_record[rec_num];
        if rec.is_dir || rec.unnamed_data_size.is_some() {
            continue; // 目录没有 $DATA；已经有大小的不用再查扩展记录
        }
        let mut found = None;
        for (ext_rec_num, _instance_id) in &rec.attr_list {
            if let Some(ext) = by_record.get(ext_rec_num) {
                if let Some(sz) = ext.unnamed_data_size {
                    found = Some(sz);
                    break;
                }
            }
        }
        match found {
            Some(sz) => { resolved_sizes.insert(*rec_num, sz); resolved_count += 1; }
            None => resolve_failed += 1,
        }
    }
    eprintln!(
        "[mft_scan] 扩展记录大小解析（查表，无需二次读盘）: 成功 {}, 失败 {}",
        resolved_count, resolve_failed
    );

    // 把 by_record 转换成原来下游代码认识的 entries: Vec<Option<RawEntry>>，
    // 这样后面的邻接表 / build_subtree（含 v14 硬链接修复）完全不用改。
    let max_record = by_record.keys().copied().max().unwrap_or(0);
    let mut entries: Vec<Option<RawEntry>> = vec![None; (max_record + 1) as usize];
    for (rec_num, rec) in &by_record {
        if !rec.is_base_record {
            continue; // 扩展记录本身不在树里显示，和原逻辑一致
        }
        let real_size = rec
            .unnamed_data_size
            .or_else(|| resolved_sizes.get(rec_num).copied())
            .unwrap_or(0);
        let (parent_record, name) = rec.file_names[0].clone();
        let extra_links = rec.file_names[1..].to_vec();
        entries[*rec_num as usize] = Some(RawEntry {
            parent_record,
            name,
            is_dir: rec.is_dir,
            in_use: rec.in_use,
            is_base_record: rec.is_base_record,
            real_size,
            modified_ft: rec.modified_ft,
            attributes: rec.attributes,
            extra_links,
        });
    }

    // 按 base record 去重的物理大小：entries 本来就是 "一个 base record 一个
    // RawEntry"（extra_links 只是额外挂载点，不会产生额外的 RawEntry），所以
    // 这里直接对 entries 求和天然就是去重后的结果——不管一份数据被硬链接到
    // 几个目录下，这里只统计一次，用来跟系统报告的"已用空间"做一致性比较。
    let dedup_size: u64 = entries
        .iter()
        .flatten()
        .filter(|e| !e.is_dir)
        .map(|e| e.real_size)
        .sum();

    // 第二遍：按 parent_record 建邻接表。
    // v14: (child_idx, name_override) —— name_override 用于硬链接场景，同一条记录
    // 在不同父目录下可能用不同的名字挂载；主链接用 None（沿用 entry.name）。
    let mut children_of: HashMap<u64, Vec<(u64, Option<String>)>> = HashMap::new();
    let mut hardlink_extra_count = 0u64;
    for (idx, e) in entries.iter().enumerate() {
        if let Some(e) = e {
            if idx as u64 != ROOT_RECORD_INDEX {
                children_of.entry(e.parent_record).or_default().push((idx as u64, None));
                // v14 关键修正：之前 parse_record 只保留一个 $FILE_NAME，其余硬链接
                // 位置被丢弃，导致文件在那些目录里彻底消失（哪怕文件本身完好）。
                // 这里把每一个额外链接也挂到对应的父目录下。
                //
                // v15 补充：如果额外链接的 parent 和主链接的 parent 完全相同，说明
                // 这不是"另一个目录位置的硬链接"，而是同一个位置的第二条 $FILE_NAME
                // ——典型情况是 WIN32 长名字 + DOS 8.3 短名字（比如
                // "nv_dispig.inf_amd64_..." 和 "NV_DIS~1.INF" 其实是同一个文件夹）。
                // 资源管理器只会显示长名字那一个入口，短名字不是独立可见条目，所以
                // 这种同父目录的额外链接要跳过，否则会在树里多出一个和真实文件夹
                // 同名同父、但内容为空的幽灵条目。
                // 只有 extra_parent 和主 parent 不同时，才是真正"文件/文件夹在另一个
                // 目录位置也可见"的情况，才需要挂到那个目录下面。
                for (extra_parent, extra_name) in &e.extra_links {
                    if *extra_parent == e.parent_record {
                        continue;
                    }
                    children_of
                        .entry(*extra_parent)
                        .or_default()
                        .push((idx as u64, Some(extra_name.clone())));
                    hardlink_extra_count += 1;
                }
            }
        }
    }
    eprintln!(
        "[mft_scan] 邻接表构建完成: {} 个父节点, 额外硬链接挂载 {} 处",
        children_of.len(),
        hardlink_extra_count
    );

    let mut file_paths = Vec::new();
    let mut file_sizes = Vec::new();
    let root_name = format!("{drive_letter}:\\");
    // v15：ancestors 是"当前递归栈（从 root 到当前节点的路径）"上的记录号集合，
    // 用来防御"子孙链兜一圈又指回自己"这种真正的自引用环（损坏卷才会出现）。
    // 注意这不是全局去重表——同一条 record 只要不在当前路径的祖先链上，就算在
    // 树的其它分支里出现过，也会被正常完整展开（比如同一目录同时有 WIN32 长
    // 名字和 DOS 8.3 短名字两条 $FILE_NAME 指向同一 parent 的情况，或者磁盘
    // 损坏导致的重复父引用）。v13/v14 版本用的是全局一次性 visited，会把这些
    // 合法/半合法的重复引用也当成环，导致第二次出现时整棵子树被清空——这是
    // "环警告 + 文件/文件夹消失"的真正原因，已在 v15 修正。
    let mut ancestors: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let root_node = build_subtree(
        ROOT_RECORD_INDEX,
        &root_name,
        &entries,
        &children_of,
        0,
        &PathBuf::from(format!("{drive_letter}:\\")),
        &mut file_paths,
        &mut file_sizes,
        &mut ancestors,
    );

    eprintln!(
        "[mft_scan] 树构建完成: root.size(树汇总,含硬链接重复计入)={}, dedup_size(物理去重)={}, files={}, folders={}",
        crate::format::human_size(root_node.size),
        crate::format::human_size(dedup_size),
        root_node.file_count,
        root_node.folder_count
    );

    Ok(MftScanResult {
        root: root_node,
        file_paths,
        file_sizes,
        dedup_size,
    })
}

fn build_subtree(
    record_idx: u64,
    display_name: &str,
    entries: &[Option<RawEntry>],
    children_of: &HashMap<u64, Vec<(u64, Option<String>)>>,
    depth: usize,
    cur_path: &PathBuf,
    file_paths: &mut Vec<PathBuf>,
    file_sizes: &mut Vec<u64>,
    ancestors: &mut std::collections::HashSet<u64>,
) -> Node {
    // v15 关键修正：之前用的是"全局一次性" visited（record 只要出现过一次就
    // 永久拉黑），这会把"同一目录被两条 $FILE_NAME 指向"（比如 WIN32 长名字 +
    // DOS 8.3 短名字，常见于 DriverStore 那种批量生成短名字的目录树；或者
    // record 在树的两个不相干分支里各被引用一次）也误判成"环"，导致第二次
    // 出现时整棵已经建好、真实存在内容的子树被直接扔掉换成空文件夹——这才是
    // "环警告 + 文件/文件夹凭空消失"的真正原因，不是磁盘真的坏了。
    //
    // 正确的环判定只能看"当前递归栈上的祖先链"：只有 record 是它自己的祖先
    // （子孙链兜了一圈又指回自己）才是真正的自引用环，必须防死循环；record
    // 在树的其它分支里被再次引用是完全正常的情况（哪怕是磁盘损坏产生的重复
    // 父引用），应该照常完整展开，不能被当成环丢弃内容。
    // ancestors 在进入本层时插入、退出前移除，只代表"从 root 到当前节点"这一条
    // 路径上的记录号集合，不是全树累计。
    if !ancestors.insert(record_idx) {
        eprintln!(
            "[mft_scan] 警告：记录号 {} 在当前递归路径上形成了真正的自引用环（磁盘可能有损坏，子孙链指回了自己），跳过其子项避免死循环",
            record_idx
        );
        return Node::new_folder_with_meta(display_name, folder_color(depth), Vec::new(), 0, 0x10);
    }
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
        for (child_idx, name_override) in kids {
            let child_idx = *child_idx;
            let Some(entry) = entries.get(child_idx as usize).and_then(|e| e.as_ref()) else {
                continue;
            };
            // v14: 硬链接场景下同一条记录可能在多个目录下出现，各自可能有不同的
            // 显示名字（name_override），主链接沿用 entry.name。
            let display = name_override.as_deref().unwrap_or(&entry.name);
            let child_path = cur_path.join(display);
            if entry.is_dir {
                // 目录理论上不会有多个硬链接（NTFS 禁止），这里保底仍走正常路径。
                let node = build_subtree(
                    child_idx,
                    display,
                    entries,
                    children_of,
                    depth + 1,
                    &child_path,
                    file_paths,
                    file_sizes,
                    ancestors,
                );
                children_nodes.push(node);
            } else {
                file_paths.push(child_path);
                file_sizes.push(entry.real_size);
                children_nodes.push(Node::new_file_with_meta(
                    display.to_string(),
                    entry.real_size,
                    file_color(),
                    entry.modified_ft,
                    entry.attributes,
                ));
            }
        }
    }
    // 退栈：离开当前节点前必须把自己从祖先链里移除，这样同一条 record 在
    // 树的其它分支（不同祖先路径）里再次出现时，不会被误判成环。
    ancestors.remove(&record_idx);
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
