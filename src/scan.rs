//! 磁盘扫描。
//!
//! 主策略：MFT 直读（NTFS 专属，管理员权限）。
//!   打开卷设备 `\\.\C:` → FSCTL 定位 MFT 位置 → 扇区对齐读取 → 自解析 FILE 记录。
//! 备选：jwalk 并行目录遍历。

use std::collections::HashMap;
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

const MAX_ENTRIES: u64 = 5_000_000;

/// 启动扫描线程。
pub fn spawn_scan(path: PathBuf, tx: Sender<ScanMessage>) {
    std::thread::spawn(move || {
        let drive = path.to_string_lossy().chars().next().unwrap_or('C');
        match scan_via_mft(drive, &tx) {
            Ok(node) => { let _ = tx.send(ScanMessage::Done(Box::new(node))); }
            Err(e) => {
                eprintln!("MFT 直读失败: {}; 降级到 jwalk 遍历", e);
                scan_fallback(&path, &tx);
            }
        }
    });
}

// ── MFT 直读 ─────────────────────────────────────────────────────────

#[cfg(windows)]
fn scan_via_mft(drive: char, tx: &Sender<ScanMessage>) -> Result<Node, Box<dyn std::error::Error>> {
    use std::ptr;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle;

    // 1. 打开卷设备
    let vol_path = format!(r"\\.\{}:", drive);
    let vol = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x7)
        .custom_flags(0x02000000 | 0x20000000) // BACKUP_SEMANTICS | NO_BUFFERING
        .open(&vol_path)?;
    let handle = vol.as_raw_handle() as isize;

    // 2. FSCTL 获取 NTFS 卷元数据
    let nvdb = get_volume_data(handle)?;
    let bpc = nvdb.BytesPerCluster as u64;
    let bps = nvdb.BytesPerSector as u64;
    let mft_lcn = nvdb.MftStartLcn as u64;
    let mft_len = nvdb.MftValidDataLength as u64;

    let mft_byte_off = mft_lcn * bpc;
    let mft_byte_len = mft_len as usize;

    // 3. 扇区对齐读 MFT
    let sector_mask = bps - 1;
    let aligned_off = mft_byte_off & !sector_mask;
    let read_start = (mft_byte_off - aligned_off) as usize;
    let aligned_size = ((mft_byte_len + read_start + bps as usize - 1) / bps as usize) * bps as usize;

    let raw = read_raw(handle, aligned_off as i64, aligned_size)?;

    let _ = tx.send(ScanMessage::Progress(1));

    // 4. 自解析 MFT 记录（跳过损坏记录，不 panic）
    let record_size = 1024;
    let records_count = mft_byte_len / record_size;
    let data = &raw[read_start..];

    let mut raw_entries: Vec<RawEntry> = Vec::with_capacity(records_count);
    let mut total = 0u64;

    for i in 0..records_count {
        let off = i * record_size;
        if off + record_size > data.len() { break; }
        let entry = &data[off..off + record_size];
        total += 1;
        if total % 100_000 == 0 { let _ = tx.send(ScanMessage::Progress(total)); }
        if total > MAX_ENTRIES { break; }

        // 解析 FILE 记录
        let Some(e) = parse_mft_record(entry, i as u64) else { continue };
        raw_entries.push(e);
    }

    let _ = tx.send(ScanMessage::Progress(2));

    // 5. 重建目录树
    build_tree(raw_entries, &format!("{}:", drive))
}

/// 解析单条 MFT FILE 记录，提取 parent FRN、name、size、目录标记。
/// 遇到损坏记录返回 None（不 panic）。
fn parse_mft_record(data: &[u8], rec_num: u64) -> Option<RawEntry> {
    // 最小长度: 48 字节 header + 至少一个属性头
    if data.len() < 48 { return None; }

    // 检查 FILE 签名
    if &data[0..4] != b"FILE" { return None; }

    // 解析 header 关键字段 (小端)
    let usa_offset = u16::from_le_bytes([data[4], data[5]]) as usize;
    let usa_size = u16::from_le_bytes([data[6], data[7]]) as usize;
    // first_attribute_record_offset
    let attr_off = u16::from_le_bytes([data[20], data[21]]) as usize;
    // flags (0x01=inuse, 0x02=directory)
    let flags = u16::from_le_bytes([data[22], data[23]]);
    let allocated_size = u32::from_le_bytes([data[24], data[25], data[26], data[27]]) as usize;
    let real_size = u32::from_le_bytes([data[28], data[29], data[30], data[31]]) as usize;

    let in_use = (flags & 0x01) != 0;
    if !in_use { return None; }
    let is_dir = (flags & 0x02) != 0;

    // 如果 attr_off 不合理，跳过
    if attr_off < 48 || attr_off >= real_size.min(allocated_size).min(data.len()) {
        return None;
    }

    // 遍历属性链找 $FILE_NAME (0x30)
    let mut pos = attr_off;
    let max_pos = real_size.min(allocated_size).min(data.len());

    while pos + 24 <= max_pos {
        let attr_type = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]);
        let attr_len = u32::from_le_bytes([data[pos+4], data[pos+5], data[pos+6], data[pos+7]]) as usize;
        if attr_len < 24 { break; }
        if pos + attr_len > max_pos { break; }

        // 检查是否为 resident 属性
        let non_resident = data[pos + 8];
        if attr_type == 0x30 && non_resident == 0 {
            // 解析 resident 属性头找 value 偏移
            let value_off = u16::from_le_bytes([data[pos + 20], data[pos + 21]]) as usize;
            let value_len = u32::from_le_bytes([data[pos + 16], data[pos + 17], data[pos + 18], data[pos + 19]]) as usize;
            let v_start = pos + value_off;
            if v_start + 66 > pos + attr_len { return None; }

            // $FILE_NAME value layout (from v_start):
            // [0..7] parent FRN (6-byte FRN low + 2-byte sequence)
            let parent_frn = u64::from_le_bytes([
                data[v_start], data[v_start+1], data[v_start+2], data[v_start+3],
                data[v_start+4], data[v_start+5], 0, 0,
            ]);
            // [40..47] logical_size
            let logical_size = u64::from_le_bytes([
                data[v_start+40], data[v_start+41], data[v_start+42], data[v_start+43],
                data[v_start+44], data[v_start+45], data[v_start+46], data[v_start+47],
            ]);
            // [48..55] physical_size
            // [56..63] flags + reparse
            // [64] name_length (1 byte)
            // [65] namespace (1 byte)
            // [66+] name (UTF-16LE)
            let name_len = data[v_start + 64] as usize;
            let name_byte_off = v_start + 66;
            if name_len > 0 && name_byte_off + name_len * 2 <= pos + attr_len {
                let name_bytes = &data[name_byte_off..name_byte_off + name_len * 2];
                let utf16: Vec<u16> = name_bytes.chunks(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                let name: String = std::char::decode_utf16(utf16.into_iter())
                    .filter_map(|r| r.ok())
                    .collect();
                if name.is_empty() || name.starts_with('$') { return None; }
                return Some(RawEntry {
                    rec: rec_num,
                    parent: parent_frn,
                    name,
                    size: logical_size,
                    is_dir,
                });
            }
            return None;
        }

        if attr_len == 0 { break; }
        pos += attr_len;
    }

    None
}

/// 保存解析结果的结构
#[derive(Clone)]
struct RawEntry {
    rec: u64,
    parent: u64,
    name: String,
    size: u64,
    is_dir: bool,
}

/// 从 parent 引用重建目录树
fn build_tree(raw_entries: Vec<RawEntry>, root_name: &str) -> Result<Node, Box<dyn std::error::Error>> {
    use std::collections::HashSet;

    if raw_entries.is_empty() {
        return Err("MFT 未包含有效条目".into());
    }

    let rec_set: HashSet<u64> = raw_entries.iter().map(|e| e.rec).collect();
    let mut children_of: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut root_indices: Vec<usize> = Vec::new();

    for (i, e) in raw_entries.iter().enumerate() {
        if e.parent == e.rec {
            root_indices.push(i);
        } else if !rec_set.contains(&e.parent) {
            root_indices.push(i);
        } else {
            children_of.entry(e.parent).or_default().push(i);
        }
    }

    fn build_node(raw: &[RawEntry], children_of: &HashMap<u64, Vec<usize>>, idx: usize, depth: usize) -> Node {
        let e = &raw[idx];
        if e.is_dir {
            let mut kids = Vec::new();
            if let Some(ci) = children_of.get(&e.rec) {
                for &c in ci {
                    kids.push(build_node(raw, children_of, c, depth + 1));
                }
            }
            Node::new_folder(&e.name, folder_color(depth), kids)
        } else {
            Node::new_file(&e.name, e.size, file_color())
        }
    }

    if root_indices.is_empty() {
        Err("没有根节点".into())
    } else if root_indices.len() == 1 {
        let mut root = build_node(&raw_entries, &children_of, root_indices[0], 0);
        root.expanded = true;
        Ok(root)
    } else {
        let mut kids = Vec::new();
        for &ri in &root_indices {
            kids.push(build_node(&raw_entries, &children_of, ri, 1));
        }
        Ok(Node::new_folder(root_name, folder_color(0), kids))
    }
}

// ── Win32 FFI 辅助 ──────────────────────────────────────────────────

#[cfg(windows)]
fn get_volume_data(handle: isize) -> Result<NtfsVolumeData, Box<dyn std::error::Error>> {
    use std::mem;
    use std::ptr;

    type DWORD = u32;
    type BOOL = i32;

    unsafe extern "system" {
        fn DeviceIoControl(
            h: isize, code: DWORD,
            in_buf: *const std::ffi::c_void, in_sz: DWORD,
            out_buf: *mut std::ffi::c_void, out_sz: DWORD,
            ret: *mut DWORD, overlapped: *mut std::ffi::c_void,
        ) -> BOOL;
        fn GetLastError() -> DWORD;
    }

    #[repr(C)]
    #[allow(non_snake_case)]
    struct NVDB {
        VolumeSerialNumber: u64,
        NumberSectors: i64,
        TotalClusters: i64,
        FreeClusters: i64,
        TotalReserved: i64,
        BytesPerSector: DWORD,
        BytesPerCluster: DWORD,
        BytesPerFileRecordSegment: DWORD,
        ClustersPerFileRecordSegment: DWORD,
        MftValidDataLength: i64,
        MftStartLcn: i64,
        Mft2StartLcn: i64,
        MftZoneStart: i64,
        MftZoneEnd: i64,
    }

    const FSCTL_GET_NTFS_VOLUME_DATA: DWORD = 0x00090064;

    let mut nvdb: NVDB = unsafe { mem::zeroed() };
    let mut ret: DWORD = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle, FSCTL_GET_NTFS_VOLUME_DATA,
            ptr::null(), 0,
            &mut nvdb as *mut _ as *mut std::ffi::c_void,
            mem::size_of::<NVDB>() as DWORD,
            &mut ret, ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(format!("FSCTL 失败 win32={}", unsafe { GetLastError() }).into());
    }

    Ok(NtfsVolumeData {
        BytesPerCluster: nvdb.BytesPerCluster,
        BytesPerSector: nvdb.BytesPerSector,
        MftStartLcn: nvdb.MftStartLcn,
        MftValidDataLength: nvdb.MftValidDataLength,
    })
}

#[cfg(windows)]
struct NtfsVolumeData {
    BytesPerCluster: u32,
    BytesPerSector: u32,
    MftStartLcn: i64,
    MftValidDataLength: i64,
}

#[cfg(windows)]
fn read_raw(handle: isize, offset: i64, size: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use std::ptr;

    type DWORD = u32;
    type BOOL = i32;

    unsafe extern "system" {
        fn SetFilePointerEx(
            h: isize, dist: i64, new_ptr: *mut i64, method: DWORD,
        ) -> BOOL;
        fn ReadFile(
            h: isize, buf: *mut std::ffi::c_void, sz: DWORD, read: *mut DWORD,
            overlapped: *mut std::ffi::c_void,
        ) -> BOOL;
        fn GetLastError() -> DWORD;
    }

    const FILE_BEGIN: DWORD = 0;

    unsafe {
        SetFilePointerEx(handle, offset, ptr::null_mut(), FILE_BEGIN);
    }

    let mut buf = vec![0u8; size];
    let mut read_bytes: DWORD = 0;
    let ok = unsafe {
        ReadFile(
            handle, buf.as_mut_ptr() as *mut std::ffi::c_void,
            size as DWORD, &mut read_bytes, ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(format!("ReadFile 失败 win32={}", unsafe { GetLastError() }).into());
    }
    buf.truncate(read_bytes as usize);
    Ok(buf)
}

// ── 非 Windows 存根 ─────────────────────────────────────────────────

#[cfg(not(windows))]
fn scan_via_mft(_drive: char, _tx: &Sender<ScanMessage>) -> Result<Node, Box<dyn std::error::Error>> {
    Err("MFT 直读仅 Windows".into())
}

// ── 降级方案：jwalk ──────────────────────────────────────────────────

fn scan_fallback(path: &Path, tx: &Sender<ScanMessage>) {
    let counter = Arc::new(AtomicU64::new(0));
    match jwalk_scan(path, &counter, tx) {
        Ok(node) => { let _ = tx.send(ScanMessage::Done(Box::new(node))); }
        Err(e)   => { let _ = tx.send(ScanMessage::Error(format!("扫描失败: {e}"))); }
    }
}

fn jwalk_scan(path: &Path, counter: &Arc<AtomicU64>, tx: &Sender<ScanMessage>) -> std::io::Result<Node> {
    let name = path.to_string_lossy().into_owned();
    let mut children = Vec::new();
    let entries: Vec<_> = jwalk::WalkDir::new(path)
        .max_depth(1).sort(true)
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
            if let Ok(child) = jwalk_scan(&entry.path(), counter, tx) {
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
    let leaf = |n: &str, s: u64| Node::new_file(n, s, file_color());
    let c = Node::new_folder("C:\\", folder_color(0), vec![
        Node::new_folder("Windows", folder_color(1), vec![
            Node::new_folder("System32", folder_color(2), vec![
                leaf("ntoskrnl.exe", 11_200_000),
            ]),
            leaf("explorer.exe", 5_400_000),
        ]),
        Node::new_folder("Program Files", folder_color(1), vec![
            leaf("Photoshop.exe", 2_300_000_000),
        ]),
        leaf("pagefile.sys", 16_000_000_000),
    ]);
    vec![c]
}

pub fn demo_tree() -> Node {
    demo_partitions().remove(0)
}
