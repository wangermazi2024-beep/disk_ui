//! 磁盘扫描。
//!
//! 使用双策略：
//! 1. **MFT 直读**（NTFS 专属，需管理员权限）—— 通过 `mft` crate 解析 \$MFT，秒级扫描整个分区。
//! 2. **传统 API 遍历**（fallback）—— 无管理员权限时使用 `std::fs::read_dir` 逐目录递归。

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

const MAX_ENTRIES: u64 = 1_000_000;

/// 启动扫描线程。
/// `path` 是被扫描的根路径（如 `C:\`）。
pub fn spawn_scan(path: PathBuf, tx: Sender<ScanMessage>) {
    std::thread::spawn(move || {
        // 先尝试 MFT 直读（仅适用于 NTFS 卷）
        if path.starts_with(r"C:\") || path.starts_with("C:") || path.starts_with(r"D:\") || path.starts_with("D:") {
            let drive_letter = path.to_string_lossy().chars().next().unwrap_or('C');
            let _ = format!(r"\\.\{}:\$MFT", drive_letter); // 保持旧路径引用（无用但无害）
            match scan_via_mft(drive_letter, &tx) {
                Ok(node) => {
                    let _ = tx.send(ScanMessage::Done(Box::new(node)));
                    return;
                }
                Err(e) => {
                    // MFT 读取失败，降级到传统遍历
                    let _ = tx.send(ScanMessage::Progress(0));
                    eprintln!("MFT 扫描失败 ({}), 降级到传统遍历", e);
                }
            }
        }

        // Fallback: 传统目录遍历
        let counter = Arc::new(AtomicU64::new(0));
        match scan_dir(&path, 0, &counter, &tx) {
            Ok(node) => { let _ = tx.send(ScanMessage::Done(Box::new(node))); }
            Err(e)   => { let _ = tx.send(ScanMessage::Error(format!("扫描失败: {e}"))); }
        }
    });
}

// ── MFT 直读 ──────────────────────────────────────────────────────────
// 仅在 Windows NTFS 卷上可用，需要管理员权限。

/// 通过 `mft`  crate 直接解析 `$MFT`，重建目录树。
/// 需要管理员权限运行。
#[cfg(windows)]
fn scan_via_mft(drive_letter: char, tx: &Sender<ScanMessage>) -> Result<Node, Box<dyn std::error::Error>> {
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt as _;

    let _ = tx.send(ScanMessage::Progress(1));

    // 启用 SeBackupPrivilege（让管理员能绕过部分 ACL 访问 $MFT）
    enable_backup_privilege();

    let mft_path = format!(r"\\.\{}:\$MFT", drive_letter);

    // 尝试直接打开 $MFT 文件
    let mft_data = match std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x7) // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        .custom_flags(0x02000000) // FILE_FLAG_BACKUP_SEMANTICS
        .open(&mft_path)
    {
        Ok(mut file) => {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            buf
        }
        // $MFT 直接打开失败, 尝试从卷设备读取
        Err(_e) => {
            read_mft_from_volume(drive_letter)?
        }
    };

    let _ = tx.send(ScanMessage::Progress(2));
    parse_mft_records(&mft_data, tx)
}

/// 启用 SeBackupPrivilege，使管理员能打开受保护的系统文件。
#[cfg(windows)]
fn enable_backup_privilege() {
    use std::ptr;

    type HANDLE = isize;
    type BOOL = i32;

    unsafe extern "system" {
        fn GetCurrentProcess() -> HANDLE;
        fn OpenProcessToken(
            ProcessHandle: HANDLE,
            DesiredAccess: u32,
            TokenHandle: *mut HANDLE,
        ) -> BOOL;
        fn AdjustTokenPrivileges(
            TokenHandle: HANDLE,
            DisableAllPrivileges: BOOL,
            NewState: *const TOKEN_PRIVILEGES,
            BufferLength: u32,
            PreviousState: *mut TOKEN_PRIVILEGES,
            ReturnLength: *mut u32,
        ) -> BOOL;
        fn LookupPrivilegeValueW(
            lpSystemName: *const u16,
            lpName: *const u16,
            lpLuid: *mut u64,
        ) -> BOOL;
        fn CloseHandle(hObject: HANDLE) -> BOOL;
    }

    const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
    const TOKEN_QUERY: u32 = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 2;

    #[repr(C)]
    struct LUID_AND_ATTRIBUTES {
        luid: u64,
        attributes: u32,
    }
    #[repr(C)]
    struct TOKEN_PRIVILEGES {
        privilege_count: u32,
        privileges: [LUID_AND_ATTRIBUTES; 1],
    }

    unsafe {
        let cur_proc = GetCurrentProcess();
        let mut token: HANDLE = 0;
        let ret = OpenProcessToken(cur_proc, TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token);
        if ret == 0 || token == 0 {
            return;
        }

        let mut luid: u64 = 0;
        let name: Vec<u16> = "SeBackupPrivilege\0".encode_utf16().collect();
        let ret = LookupPrivilegeValueW(ptr::null(), name.as_ptr(), &mut luid);
        if ret == 0 {
            let _ = CloseHandle(token);
            return;
        }

        let tp = TOKEN_PRIVILEGES {
            privilege_count: 1,
            privileges: [LUID_AND_ATTRIBUTES {
                luid,
                attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let _ = AdjustTokenPrivileges(token, 0, &tp, 0, ptr::null_mut(), ptr::null_mut());
        let _ = CloseHandle(token);
    }
}

/// 从卷设备（\\.\C:）直接读取 $MFT 的原始字节。
#[cfg(windows)]
fn read_mft_from_volume(drive_letter: char) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use std::mem;
    use std::ptr;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle;

    type HANDLE = isize;
    type BOOL = i32;
    type DWORD = u32;

    unsafe extern "system" {
        fn DeviceIoControl(
            hDevice: HANDLE,
            dwIoControlCode: DWORD,
            lpInBuffer: *const std::ffi::c_void,
            nInBufferSize: DWORD,
            lpOutBuffer: *mut std::ffi::c_void,
            nOutBufferSize: DWORD,
            lpBytesReturned: *mut DWORD,
            lpOverlapped: *mut std::ffi::c_void,
        ) -> BOOL;
        fn SetFilePointerEx(
            hFile: HANDLE,
            liDistanceToMove: i64,
            lpNewFilePointer: *mut i64,
            dwMoveMethod: DWORD,
        ) -> BOOL;
        fn ReadFile(
            hFile: HANDLE,
            lpBuffer: *mut std::ffi::c_void,
            nNumberOfBytesToRead: DWORD,
            lpNumberOfBytesRead: *mut DWORD,
            lpOverlapped: *mut std::ffi::c_void,
        ) -> BOOL;
        fn GetLastError() -> DWORD;
    }

    const FSCTL_GET_NTFS_VOLUME_DATA: DWORD = 0x00090064; // CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 28, METHOD_BUFFERED, FILE_ANY_ACCESS)
    const FILE_BEGIN: DWORD = 0;
    const FILE_FLAG_NO_BUFFERING: u32 = 0x20000000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x02000000;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct NTFS_VOLUME_DATA_BUFFER {
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

    let vol_path = format!(r"\\.\{}:", drive_letter);

    // 打开卷设备（需要管理员权限）
    let vol_file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x7)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_NO_BUFFERING)
        .open(&vol_path)
        .map_err(|e| format!("打开卷设备失败: {}", e))?;

    let handle = vol_file.as_raw_handle() as HANDLE;

    // 获取 NTFS 卷元信息
    let mut nvdb: NTFS_VOLUME_DATA_BUFFER = unsafe { mem::zeroed() };
    let mut bytes_ret: DWORD = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle as HANDLE,
            FSCTL_GET_NTFS_VOLUME_DATA,
            ptr::null(),
            0,
            &mut nvdb as *mut _ as *mut std::ffi::c_void,
            mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as DWORD,
            &mut bytes_ret,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(format!("FSCTL_GET_NTFS_VOLUME_DATA 失败 (win32={})", err).into());
    }

    let bytes_per_cluster = nvdb.BytesPerCluster as u64;
    let bytes_per_sector = nvdb.BytesPerSector as u64;
    let _bytes_per_record = nvdb.BytesPerFileRecordSegment as usize;
    let mft_start_lcn = nvdb.MftStartLcn as u64;
    let mft_valid_len = nvdb.MftValidDataLength as u64;

    // MFT 字节偏移
    let mft_byte_off = mft_start_lcn * bytes_per_cluster;
    let mft_size = mft_valid_len as usize;

    // 扇区对齐（FILE_FLAG_NO_BUFFERING 要求）
    let sector_mask = bytes_per_sector as u64 - 1;
    let aligned_off = mft_byte_off & !sector_mask;
    let read_start = (mft_byte_off - aligned_off) as usize;
    let aligned_size = ((mft_size + read_start + bytes_per_sector as usize - 1)
        / bytes_per_sector as usize)
        * bytes_per_sector as usize;

    // 定位到对齐的偏移
    unsafe {
        SetFilePointerEx(
            handle,
            aligned_off as i64,
            ptr::null_mut(),
            FILE_BEGIN,
        );
    }

    // 读取（必须用 ReadFile, 因为 FILE_FLAG_NO_BUFFERING 不允许 std Read）
    let mut raw = vec![0u8; aligned_size];
    let mut read_bytes: DWORD = 0;
    let ok = unsafe {
        ReadFile(
            handle,
            raw.as_mut_ptr() as *mut std::ffi::c_void,
            aligned_size as DWORD,
            &mut read_bytes,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(format!("ReadFile MFT 失败 (win32={})", err).into());
    }

    Ok(raw[read_start..read_start + mft_size].to_vec())
}

/// 解析 MFT 记录并重建目录树。
#[cfg(windows)]
fn parse_mft_records(mft_data: &[u8], tx: &Sender<ScanMessage>) -> Result<Node, Box<dyn std::error::Error>> {
    use mft::MftParser;

    let mut parser = MftParser::from_buffer(mft_data.to_vec())?;

    // 第一遍：收集所有有效条目
    // key = MFT record number, value = parsed entry data
    struct RawEntry {
        record_number: u64,
        parent_record: u64,
        name: String,
        size: u64,
        is_dir: bool,
    }

    let mut entries: Vec<RawEntry> = Vec::new();
    let mut total = 0u64;

    for result in parser.iter_entries() {
        total += 1;
        if total % 100_000 == 0 {
            let _ = tx.send(ScanMessage::Progress(total));
        }
        if total > MAX_ENTRIES * 10 {
            break; // 防止无限增长
        }

        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };

        // 只处理已分配的条目（跳过已删除的）
        if !entry.is_allocated() {
            continue;
        }

        let attr = match entry.find_best_name_attribute() {
            Some(a) => a,
            None => continue,
        };

        // 跳过特殊系统文件（$MFT, $Secure 等）
        let name = attr.name.trim().to_string();
        if name.is_empty() || name.starts_with('$') {
            continue;
        }

        // 特殊目录: 卷根目录 "." 需要处理 - 它的 parent_record 指向自己
        // 卷根目录的 name 可能为空或 "."，我们用驱动器号代替
        entries.push(RawEntry {
            record_number: entry.header.record_number,
            parent_record: attr.parent.entry,
            name,
            size: if entry.is_dir() { 0 } else { attr.logical_size },
            is_dir: entry.is_dir(),
        });
    }

    let _ = tx.send(ScanMessage::Progress(total / 2 + 1));

    // 重建树结构
    // 用 HashMap 建立 record_number -> children 的映射
    // 根节点是所有 parent_record 不在 entries 中的条目（它们的父节点在树外）
    let mut children_of: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut root_indices: Vec<usize> = Vec::new();

    // 先收集所有 record_number
    let record_numbers: Vec<u64> = entries.iter().map(|e| e.record_number).collect();
    let rec_set: std::collections::HashSet<u64> = record_numbers.iter().cloned().collect();

    for (i, entry) in entries.iter().enumerate() {
        if entry.parent_record == entry.record_number {
            // 自引用 = 卷根目录
            root_indices.push(i);
        } else if !rec_set.contains(&entry.parent_record) {
            // 父节点不在当前集合中，也是根
            root_indices.push(i);
        } else {
            children_of.entry(entry.parent_record).or_default().push(i);
        }
    }

    let _ = tx.send(ScanMessage::Progress(total / 2 + 2));

    // 递归构建 Node 树
    fn build_node(
        raw: &[RawEntry],
        children_of: &HashMap<u64, Vec<usize>>,
        idx: usize,
        depth: usize,
    ) -> Node {
        let entry = &raw[idx];
        let mut node = if entry.is_dir {
            let mut children = Vec::new();
            if let Some(child_indices) = children_of.get(&entry.record_number) {
                for &ci in child_indices {
                    children.push(build_node(raw, children_of, ci, depth + 1));
                }
            }
            Node::new_folder(&entry.name, folder_color(depth), children)
        } else {
            Node::new_file(&entry.name, entry.size, file_color())
        };
        // 根目录默认展开
        if depth == 0 {
            node.expanded = true;
        }
        node
    }

    // 合并多个根节点到一个虚拟根
    if root_indices.len() == 1 {
        Ok(build_node(&entries, &children_of, root_indices[0], 0))
    } else if root_indices.is_empty() {
        Err("未找到任何文件条目".into())
    } else {
        let mut children = Vec::new();
        for &ri in &root_indices {
            children.push(build_node(&entries, &children_of, ri, 1));
        }
        // 使用 C: 作为默认驱动器名
        Ok(Node::new_folder("C:\\\\".to_string(), folder_color(0), children))
    }
}

#[cfg(not(windows))]
fn scan_via_mft(_drive_letter: char, _tx: &Sender<ScanMessage>) -> Result<Node, Box<dyn std::error::Error>> {
    Err("MFT 扫描仅在 Windows 上可用".into())
}

// ── 传统 API 遍历（fallback） ────────────────────────────────────────

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
    let dir = match std::fs::read_dir(path) {
        Ok(d) => d,
        Err(e) => {
            // 权限拒绝等错误跳过该目录
            eprintln!("跳过 {}: {}", path.display(), e);
            return Ok(Node::new_folder(name, folder_color(depth), vec![]));
        }
    };

    for entry in dir.flatten() {
        if counter.load(Ordering::Relaxed) > MAX_ENTRIES { break; }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
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

/// 演示数据：C 盘 + D 盘两个分区，各自是独立的根节点。
pub fn demo_partitions() -> Vec<Node> {
    let leaf = |name: &str, size: u64| Node::new_file(name, size, file_color());

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

    let users = Node::new_folder("Users", folder_color(1), vec![
        Node::new_folder("Default", folder_color(2), vec![
            Node::new_folder("AppData", folder_color(3), vec![
                Node::new_folder("Temp", folder_color(4), vec![
                    leaf("cache.tmp", 1_100_000_000),
                ]),
            ]),
        ]),
    ]);

    let c_drive = Node::new_folder("C:\\\\  系统", folder_color(0), vec![
        windows, program_files, users,
        leaf("pagefile.sys", 16_000_000_000),
        leaf("hiberfil.sys", 8_000_000_000),
    ]);

    let d_drive = Node::new_folder("D:\\\\  软件", folder_color(0), vec![
        Node::new_folder("Steam", folder_color(1), vec![
            leaf("steamapps", 0),
        ]),
        Node::new_folder("Downloads", folder_color(1), vec![
            leaf("movie_4k.mkv", 18_000_000_000),
        ]),
    ]);

    vec![c_drive, d_drive]
}

/// 兼容旧调用：返回单个 demo 节点（仅 C 盘）。
pub fn demo_tree() -> Node {
    demo_partitions().remove(0)
}
