//! WinDirStat 风格 MFT 扫描引擎：自解析二进制，base record 折叠，两表法建树。
//!
//! 算法来源：WinDirStat (FinderNtfs.cpp)
//! 核心差异（对比旧 mft crate 方案）：
//! 1. 直接读卷设备 + FSCTL 定位 MFT → 自解析，无外部库依赖
//! 2. Base record 折叠：扩展记录通过 base_ref 自动归入基记录，无 ATTRIBUTE_LIST 遍历
//! 3. 两表法：HashMap<FRN, FileRecordBase> + HashMap<ParentFRN, Vec<(FRN, Name)>>
//! 4. PhysicalSize 来自 $DATA 的 AllocatedLength，LogicalSize 来自 FileSize
//! 5. DOS 短名在 $FILE_NAME 解析时直接跳过

use std::collections::HashMap;
use std::mem::zeroed;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, SetFilePointerEx, FILE_BEGIN,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    FILE_FLAG_NO_BUFFERING, FILE_READ_DATA, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_GET_RETRIEVAL_POINTERS,
    RETRIEVAL_POINTERS_BUFFER, STARTING_VCN_INPUT_BUFFER,
};

use egui::Color32;
use crate::format::human_size;
use crate::model::{Node, NodeKind};
use crate::scan::ScanMessage;

pub fn mft_scan_available(_drive: char) -> bool { true }

/// 获取磁盘已用/空闲空间（向下兼容 verify_mft）
pub fn get_disk_space(drive_letter: char) -> Result<(u64, u64), String> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let path: Vec<u16> = format!(r"{}:\", drive_letter).encode_utf16().chain([0]).collect();
    let mut free = 0u64;
    let mut total = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) };
    if ok == 0 { return Err("GetDiskFreeSpaceExW 失败".into()); }
    Ok((total, total - free))
}

/// 是否以管理员权限运行（向下兼容 verify_mft）
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows_sys::Win32::Foundation::HANDLE;
    let mut token: HANDLE = std::ptr::null_mut();
    let ok = unsafe {
        windows_sys::Win32::System::Threading::OpenProcessToken(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            TOKEN_QUERY, &mut token)
    };
    if ok == 0 { return false; }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut ret_len = 0u32;
    unsafe {
        GetTokenInformation(token, windows_sys::Win32::Security::TokenElevation,
            &mut elevation as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut ret_len);
        CloseHandle(token);
    }
    elevation.TokenIsElevated != 0
}

const ATTR_SI: u32 = 0x10;
const ATTR_FN: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_RP: u32 = 0xC0;
const ATTR_END: u32 = 0xFFFF_FFFF;
const ROOT_FRN: u64 = 5;
const FILE_SIG: u32 = 0x454C4946;

// ── 裸 MFT 文件头 (packed, 同 WinDirStat FinderNtfs.cpp) ──
#[repr(C, packed)]
struct FILE_RECORD {
    signature: u32,
    usa_offset: u16,
    usa_count: u16,
    _lsn: [u8; 8],
    _seq: u16,
    _link_cnt: u16,
    first_attr_off: u16,
    flags: u16,
    _free: [u8; 8],
    base_ref: u64,
    _next_attr: u16,
    _high: u16,
    _low: u32,
}

#[repr(C, packed)]
struct ATTR_HDR {
    type_code: u32,
    rec_len: u32,
    form_code: u8,     // 0=resident, 1=non-resident
    name_len: u8,
    name_off: u16,
    flags: u16,
    instance: u16,
}

#[repr(C, packed)]
struct NR_BODY {
    lowest_vcn: i64,
    highest_vcn: i64,
    datarun_off: u16,
    comp_size: u16,
    _pad: [u8; 4],
    allocated_len: u64,
    file_size: u64,
    valid_data: u64,
    _total_alloc: u64,
}

#[repr(C, packed)]
struct RES_BODY {
    val_len: u32,
    val_off: u16,
    _pad: [u8; 2],
}

struct FN_BODY {
    // We handle this via byte offsets for portability
}

/// WinDirStat 风格的 MFT 记录聚合数据
#[derive(Default, Clone)]
struct FileRecordBase {
    logical_size: u64,
    physical_size: u64,
    modified_ft: u64,    // FILETIME (100ns since 1601)
    attributes: u32,
    has_data: bool,
}

/// 扫描结果
pub struct MftScanResult {
    pub root: Node,
    pub dedup_size: u64,
    pub file_count: u64,
    pub folder_count: u64,
    pub file_paths: Vec<PathBuf>,
    pub file_sizes: Vec<u64>,
}

// ── 工具函数 ──────────────────────────────────────────
fn apply_fixup(data: &mut [u8], record_size: usize) -> bool {
    if data.len() < 8 { return false; }
    let uo = u16::from_le_bytes([data[4], data[5]]) as usize;
    let uc = u16::from_le_bytes([data[6], data[7]]) as usize;
    if uc == 0 || uo + uc * 2 > data.len() { return false; }
    let usn = [data[uo], data[uo + 1]];
    let words_per_sector = 512 / 2;
    let ws = unsafe { std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u16, record_size / 2) };
    for i in 1..uc {
        let se = i * words_per_sector - 1;
        if ws[se] == u16::from_le_bytes(usn) {
            let fv = u16::from_le_bytes([data[uo + i * 2], data[uo + i * 2 + 1]]);
            ws[se] = fv;
        } else { return false; }
    }
    true
}

/// WinDirStat: NTFS_VOLUME_DATA_BUFFER (48 bytes)
#[repr(C)]
struct VOL_INFO {
    _serial: i64,
    _sectors: i64,
    _total_clusters: i64,
    _free_clusters: i64,
    _reserved: i64,
    _sec_size: u32,
    clus_size: u32,
    rec_size: u32,
    _clus_per_rec: u32,
    _mft_valid: i64,
    _mft_lcn: i64,
    _mft2_lcn: i64,
    _mft_zone_start: i64,
    _mft_zone_end: i64,
}

/// 打开卷 → 读 MFT → 自解析 → 建两表 → 建树
pub fn scan_drive_via_mft(drive_letter: char, _tx: &Sender<ScanMessage>) -> Result<MftScanResult, String> {
    let vol_ws: Vec<u16> = format!(r"\\.\{}:", drive_letter).encode_utf16().chain([0]).collect();

    let vol = unsafe { CreateFileW(vol_ws.as_ptr(), FILE_READ_DATA,
        FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null_mut(), OPEN_EXISTING,
        FILE_FLAG_NO_BUFFERING, std::ptr::null_mut()) };
    if vol == INVALID_HANDLE_VALUE {
        return Err("打开卷失败（需管理员权限）".into());
    }

    // 卷信息
    let mut vi: VOL_INFO = unsafe { zeroed() };
    let mut br = 0u32;
    if unsafe { DeviceIoControl(vol, FSCTL_GET_NTFS_VOLUME_DATA, std::ptr::null(), 0,
        &mut vi as *mut _ as *mut _, std::mem::size_of::<VOL_INFO>() as u32, &mut br, std::ptr::null_mut()) } == 0
    {
        unsafe { CloseHandle(vol); }
        return Err("FSCTL_GET_NTFS_VOLUME_DATA 失败".into());
    }
    let cls = vi.clus_size as u64;
    let rec_sz = vi.rec_size as usize;
    crate::dlog!("[mft] 簇={} 记录={}", human_size(cls), rec_sz);

    // 获取 $MFT data runs via FSCTL_GET_RETRIEVAL_POINTERS (WinDirStat 风格)
    let mft_ws: Vec<u16> = format!(r"\\.\{}:\$MFT", drive_letter).encode_utf16().chain([0]).collect();
    let fh = unsafe { CreateFileW(mft_ws.as_ptr(), FILE_READ_DATA,
        FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null_mut(), OPEN_EXISTING, 0x200000, std::ptr::null_mut()) };
    if fh == INVALID_HANDLE_VALUE { unsafe { CloseHandle(vol); } return Err("打开 $MFT 失败".into()); }

    let mut runs: Vec<(u64, i64, u64)> = Vec::new();
    let mut sv = 0i64;
    loop {
        let inp = STARTING_VCN_INPUT_BUFFER { StartingVcn: sv };
        let mut buf = vec![0u8; std::mem::size_of::<RETRIEVAL_POINTERS_BUFFER>() + 4096];
        let mut br2 = 0u32;
        let ok = unsafe { DeviceIoControl(fh, FSCTL_GET_RETRIEVAL_POINTERS,
            &inp as *const _ as *const _, std::mem::size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
            buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut br2, std::ptr::null_mut()) };
        let rp = unsafe { &*(buf.as_ptr() as *const RETRIEVAL_POINTERS_BUFFER) };
        let ec = rp.ExtentCount as usize;
        let ext_ptr: *const _ = &rp.Extents[0];
        let exts = unsafe { std::slice::from_raw_parts(ext_ptr, ec) };
        let mut vcn = rp.StartingVcn;
        for ext in exts {
            let nv = ext.NextVcn;
            runs.push((vcn as u64, ext.Lcn, (nv - vcn) as u64));
            vcn = nv;
        }
        if ok != 0 { break; }
        sv = vcn;
    }
    unsafe { CloseHandle(fh); }
    crate::dlog!("[mft] {} 个 data run", runs.len());

    // ── WinDirStat 两表 ──
    let mut bases: HashMap<u64, FileRecordBase> = HashMap::new();
    let mut ptree: HashMap<u64, Vec<(u64, String)>> = HashMap::new();
    bases.insert(ROOT_FRN, FileRecordBase::default());

    let mut fc = 0u64; let mut dc = 0u64; let mut fixup_fail = 0u64; let mut not_used = 0u64;

    // 4MB aligned buffer for overlapped I/O
    let bufsz: usize = 4 * 1024 * 1024;
    let layout = std::alloc::Layout::from_size_align(bufsz, 4096).unwrap();
    let raw_buf = unsafe { std::alloc::alloc_zeroed(layout) };
    if raw_buf.is_null() { unsafe { CloseHandle(vol); } return Err("分配缓冲区失败".into()); }

    for (run_vcn, run_lcn, run_cls) in &runs {
        let mut remain = run_cls * cls;
        let mut off = (run_lcn * cls as i64) as i64;
        let run_rec_off = run_vcn * rec_sz as u64;

        while remain > 0 {
            let chunk = remain.min(bufsz as u64);
            unsafe { SetFilePointerEx(vol, off, std::ptr::null_mut(), FILE_BEGIN); }
            let mut rd = 0u32;
            let ok = unsafe { ReadFile(vol, raw_buf as *mut _, chunk as u32, &mut rd, std::ptr::null_mut()) };
            if ok == 0 {
                crate::dlog!("[mft] 读盘错误: {}", unsafe { GetLastError() });
                break;
            }

            let recs = rd as usize / rec_sz;
            for ri in 0..recs {
                let p = unsafe { &mut *(raw_buf.add(ri * rec_sz) as *mut FILE_RECORD) };
                if p.signature != FILE_SIG { continue; }
                if !apply_fixup(unsafe { std::slice::from_raw_parts_mut(raw_buf.add(ri * rec_sz), rec_sz) }, rec_sz) {
                    fixup_fail += 1; continue;
                }
                if (p.flags & 1) == 0 { not_used += 1; continue; }

                let cr = run_rec_off + (ri * rec_sz) as u64 / rec_sz as u64;
                let bref = p.base_ref & 0xFFFF_FFFF_FFFF;
                let bi = if bref > 0 { bref } else { cr };
                let base_base = bref == 0;

                if base_base && (p.flags & 2) != 0 { dc += 1; } else if base_base { fc += 1; }

                let b = bases.entry(bi).or_default();

                let fao = p.first_attr_off as usize;
                if fao >= rec_sz { continue; }
                let rd = unsafe { std::slice::from_raw_parts(raw_buf.add(ri * rec_sz), rec_sz) };
                let mut ao = fao;
                while ao + 16 <= rec_sz {
                    let at = u32::from_le_bytes(rd[ao..ao + 4].try_into().unwrap_or([0xFF; 4]));
                    if at == ATTR_END { break; }
                    let al = u32::from_le_bytes(rd[ao + 4..ao + 8].try_into().unwrap_or([0; 4])) as usize;
                    if al == 0 || ao + al > rec_sz { break; }
                    let nr = rd[ao + 8] & 1;
                    let nl = rd[ao + 9];

                    match at {
                        ATTR_SI if nr == 0 => {
                            let vo = u16::from_le_bytes(rd[ao + 0x14..ao + 0x16].try_into().unwrap_or([0; 2])) as usize;
                            let sp = ao + vo;
                            if sp + 36 <= ao + al {
                                let mt = i64::from_le_bytes(rd[sp + 8..sp + 16].try_into().unwrap_or([0; 8]));
                                let attrs = u32::from_le_bytes(rd[sp + 32..sp + 36].try_into().unwrap_or([0; 4]));
                                b.modified_ft = mt as u64;
                                b.attributes = if (p.flags & 2) != 0 { attrs | FILE_ATTRIBUTE_DIRECTORY } else { attrs };
                                if b.attributes == 0 { b.attributes = FILE_ATTRIBUTE_NORMAL; }
                            }
                        }

                        ATTR_FN if nr == 0 && nl == 0 => { // unnamed $FILE_NAME only
                            let vo = u16::from_le_bytes(rd[ao + 0x14..ao + 0x16].try_into().unwrap_or([0; 2])) as usize;
                            let fp = ao + vo;
                            if fp + 66 <= ao + al {
                                let fd = &rd[fp..];
                                let pr = u64::from_le_bytes(fd[0..8].try_into().unwrap_or([0; 8])) & 0xFFFF_FFFF_FFFF;
                                let ns = fd[65];
                                let nlb = fd[64];
                                // WinDirStat: 跳过短名 (DOS namespace == 2) 和 . / ..
                                if ns == 2 { break; } // skip short name
                                if nlb == 1 && fd[66] == b'.' { break; }
                                if nlb == 2 && fd[66] == b'.' && fd[68] == b'.' { break; }

                                let name_utf16 = unsafe { std::slice::from_raw_parts(fd.as_ptr().add(66) as *const u16, nlb as usize) };
                                let name = String::from_utf16_lossy(name_utf16);
                                // WinDirStat: 每个 $FILE_NAME 生成一条父子记录
                                ptree.entry(pr).or_default().push((bi, name));
                                // 注意：这个 fn 也提供 cached logical_size (file_size at +56) 和
                                // physical_size (allocated_length at +48)。但它们可能过期，
                                // 只在新代码找不到 $DATA 时才用做兜底。
                            }
                        }

                        ATTR_DATA if nl == 0 => { // unnamed $DATA
                            if nr != 0 {
                                // non-resident
                                if rec_sz < ao + 0x38 { break; }
                                let lvcn = i64::from_le_bytes(rd[ao + 0x10..ao + 0x18].try_into().unwrap_or([0xFF; 8]));
                                // WinDirStat: 只有 lowest_vcn==0 的 extent 才含完整大小信息
                                if lvcn != 0 { break; }
                                let fs = u64::from_le_bytes(rd[ao + 0x30..ao + 0x38].try_into().unwrap_or([0; 8]));
                                let al = u64::from_le_bytes(rd[ao + 0x28..ao + 0x30].try_into().unwrap_or([0; 8]));
                                let compressed = (u16::from_le_bytes(rd[ao + 0x0C..ao + 0x0E].try_into().unwrap_or([0; 2])) & 1) != 0;
                                let sparse = (u16::from_le_bytes(rd[ao + 0x0C..ao + 0x0E].try_into().unwrap_or([0; 2])) & 0x8000) != 0;
                                b.logical_size = fs;
                                b.physical_size = if compressed || sparse {
                                    // Compressed/sparse: 使用 total_allocated (+0x40)
                                    if ao + 0x48 <= rec_sz {
                                        u64::from_le_bytes(rd[ao + 0x40..ao + 0x48].try_into().unwrap_or([0; 8]))
                                    } else { al }
                                } else { al };
                                b.has_data = true;
                            } else {
                                // resident
                                let vl = u32::from_le_bytes(rd[ao + 0x10..ao + 0x14].try_into().unwrap_or([0; 4])) as u64;
                                b.logical_size = vl;
                                b.physical_size = (vl + 7) & !7;
                                b.has_data = true;
                            }
                        }

                        ATTR_RP if nr == 0 && nl == 0 => {
                            // reparse point: 可选，WinDirStat 用来标记 junction/symlink
                            let vo = u16::from_le_bytes(rd[ao + 0x14..ao + 0x16].try_into().unwrap_or([0; 2])) as usize;
                            if vo + 4 <= al {
                                let tag = u32::from_le_bytes(rd[ao + vo..ao + vo + 4].try_into().unwrap_or([0; 4]));
                                b.attributes |= FILE_ATTRIBUTE_REPARSE_POINT;
                            }
                        }

                        _ => {}
                    }

                    ao += al;
                }
            }

            off += rd as i64;
            remain -= rd as u64;
        }
    }

    unsafe { std::alloc::dealloc(raw_buf, layout); }
    unsafe { CloseHandle(vol); }

    crate::dlog!("[mft] 解析: base={} 目录={} 文件={} fixup_fail={} 已删={}",
        bases.len(), dc, fc, fixup_fail, not_used);

    // ── 建树（从 FRN 5 开始递归遍历 ptree） ──
    let root_name = format!(r"{}:\", drive_letter);
    let root_node = build_subtree(ROOT_FRN, &root_name, &bases, &ptree, 0, &mut HashMap::new());

    // dedup = 去重后的物理大小（每份数据只计一次）
    let dedup_size: u64 = bases.values()
        .filter(|r| (r.attributes & FILE_ATTRIBUTE_DIRECTORY) == 0 && r.has_data)
        .map(|r| r.physical_size)
        .sum();

    crate::dlog!("[mft] 树: root.size={} dedup={}",
        human_size(root_node.size), human_size(dedup_size));

    Ok(MftScanResult {
        root: root_node,
        dedup_size,
        file_count: fc,
        folder_count: dc,
        file_paths: Vec::new(),
        file_sizes: Vec::new(),
    })
}

fn folder_color(depth: usize) -> Color32 {
    const P: [Color32; 6] = [
        Color32::from_rgb(0x4C, 0x8B, 0xF5),
        Color32::from_rgb(0x34, 0xC7, 0x59),
        Color32::from_rgb(0xF5, 0xA6, 0x23),
        Color32::from_rgb(0xE0, 0x55, 0x5B),
        Color32::from_rgb(0x9C, 0x6A, 0xDE),
        Color32::from_rgb(0x2E, 0xC4, 0xB6),
    ];
    P[depth % P.len()]
}
fn file_color() -> Color32 { Color32::from_rgb(0x6C, 0x75, 0x7D) }

/// 从 ptree 构建递归 Node 树（WinDirStat 风格）
fn build_subtree(
    frn: u64, name: &str, bases: &HashMap<u64, FileRecordBase>,
    ptree: &HashMap<u64, Vec<(u64, String)>>, depth: usize,
    visited: &mut HashMap<u64, bool>,
) -> Node {
    if visited.contains_key(&frn) {
        return Node::new_folder(name, folder_color(depth), Vec::new());
    }
    visited.insert(frn, true);

    let mut children = Vec::new();
    if let Some(entries) = ptree.get(&frn) {
        for (child_frn, child_name) in entries {
            if let Some(base) = bases.get(child_frn) {
                if (base.attributes & FILE_ATTRIBUTE_DIRECTORY) != 0 {
                    // Directory
                    let child = build_subtree(*child_frn, child_name, bases, ptree, depth + 1, visited);
                    children.push(child);
                } else {
                    // File
                    children.push(Node {
                        name: child_name.clone(),
                        size: base.logical_size,
                        allocated: base.physical_size,
                        kind: NodeKind::File,
                        color: file_color(),
                        children: Vec::new(),
                        expanded: false,
                        file_count: 0,
                        folder_count: 0,
                        modified_ft: base.modified_ft,
                        attributes: base.attributes,
                    });
                }
            }
        }
    }

    visited.remove(&frn);
    let mut folder = Node::new_folder(name, folder_color(depth), children);
    // 可选：从 base_records[frn] 获取目录自身的时间
    if let Some(b) = bases.get(&frn) {
        if b.modified_ft > 0 {
            folder.modified_ft = b.modified_ft;
        }
    }
    folder
}
