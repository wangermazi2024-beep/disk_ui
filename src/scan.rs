//! 目录扫描入口 + 常规遍历。
//!
//! ## 这一版解决了两个真实问题（不是打补丁，是换算法）：
//!
//! ### 1) 递归深度导致崩溃
//! 旧实现里，`scan_dir` 在 `rayon par_iter` 的闭包里**直接递归调用自己**处理子目录。
//! 这意味着"目录树有多深，原生调用栈就要压多深"——rayon 的并行分裂并不会打破这条调用链，
//! 因为 `join`/`par_iter` 本质上是"父调用等待子调用返回"，子目录的 `scan_dir` 帧
//! 始终摞在父目录 `scan_dir` 帧上面。遇到几千层深的目录（node_modules、备份软件的
//! 版本链、压缩包解压产物等）就会栈溢出崩溃，而且不能靠"调大栈"根治，只是往后拖问题。
//!
//! 真正的修法：**把"递归调用"换成"任务队列 + 显式的父子完成通知"**，用 `rayon::Scope::spawn`
//! 代替直接函数递归——`spawn` 只是把任务丢进工作窃取队列，调用它的函数立刻返回，
//! 不会在原生栈上累积帧。子目录处理完之后，通过一个共享的 `DirTask`（parent 指针 + 剩余
//! 未完成子目录计数）**用循环而不是递归**一路往上通知父目录："我这个子任务做完了"，
//! 计数归零就把父目录也定型、再往上传播。全程没有一次函数对函数的递归调用，
//! 原生栈深度和目录树深度完全解耦，理论上可以扫任意深的目录树。
//!
//! ### 2) 比 WinDirStat 慢（17s vs 10s）
//! 旧实现用 `std::fs::read_dir` 拿目录项（不慢），但对**每一个文件**都额外调用一次
//! `CreateFileW` + `GetFileInformationByHandle`（为了拿硬链接信息），是 79 万次多余的
//! 内核对象创建/销毁；压缩/稀疏文件还要再调一次 `GetCompressedFileSizeW`。这才是真正的瓶颈，
//! 不是"遍历算法"本身慢。
//!
//! 真正的修法：改用 `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`，**每个目录只开
//! 一次 handle**，用一个 64KB 缓冲区循环把该目录下所有条目一次性批量读出——名字、逻辑大小、
//! 物理/占用大小（AllocationSize，压缩稀疏文件同样准确）、属性、三个时间戳全都在这一次调用里，
//! 不再需要逐文件开 handle，也不再需要单独查压缩大小。详见 `dir_enum.rs`。
//!
//! 代价：不再对硬链接做去重（该 API 不返回 nNumberOfLinks，要拿到它必须逐文件开 handle，
//! 这正是我们要去掉的开销）。硬链接在普通用户目录里极少见（主要出现在 WinSxS 这类系统目录），
//! 影响可忽略；如果之后要精确处理，应该做成"可选的深度模式"，而不是让所有文件都为小概率
//! 场景付出逐文件开 handle 的代价。
//!
//! 批量枚举 API 不可用时（极少见：非 NTFS/ReFS、老系统），自动 fallback 到
//! `std::fs::read_dir`，保证兼容性不倒退。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashSet;
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
            // rayon 全局线程池只能初始化一次（官方文档明确：第二次 build_global 会静默失败）。
            // 用 OnceLock 保证只配一次，避免多次扫描时静默失败让用户以为“重新配置生效了”。
            rayon_init_once();
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

        // 常规遍历：非递归工作队列版本
        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        // 动态获取卷簇大小，用于 fallback 路径计算普通文件物理大小（簇对齐）。
        // NTFS 簇大小可以是 512B 到 64K，不能写死 4096。
        #[cfg(windows)]
        let cluster_size = crate::disk_info::get_cluster_size(&root);
        #[cfg(not(windows))]
        let cluster_size = 4096u64;
        eprintln!("[scan] 簇大小: {}", cluster_size);
        // 硬链接去重集合：key 是 NTFS FileId（FileIdBothDirectoryInfo 返回的 64 位
        // 文件参考号，卷内唯一）。同一份文件的所有硬链接位置共享同一个 FileId。
        //
        // 这里和 mft_scan.rs 的去重口径完全一致：
        //   - logical_size 不去重（每次出现都计全值，和 WinDirStat GetSizeLogical 一致）
        //   - physical_size 去重（第一次出现计全值，后续硬链接位置计 0）
        // 这样 root.logical_size 是“所有目录项大小之和”（和 Explorer 一致），
        // root.physical_size 是“磁盘真实占用”（和 WinDirStat/WizTree 一致）。
        let hardlink_seen: Arc<DashSet<u64>> = Arc::new(DashSet::new());

        match run_scan(&root, &counter, &cancel, &tx, &hardlink_seen, cluster_size) {
            Ok(mut node) => {
                if let Some(info) = &disk_info {
                    #[cfg(windows)]
                    if as_drive_root(&root).is_some() { node.name = info.display_name(); }
                    #[cfg(not(windows))]
                    { node.name = info.display_name(); }
                }
                // 常规扫描也填充可见节点的所有者（和 MFT 模式行为对齐）。
                // 以前这里不填，导致同一程序 MFT 模式下"所有者"列有真实数据、
                // 常规模式下永远显示"—"，用户会误以为是 bug。
                #[cfg(windows)]
                crate::mft_scan::populate_owners(&mut node, &root.to_string_lossy());
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

/// rayon 全局线程池只能在进程生命周期里初始化一次（官方文档明确写：
/// "Initialization of the global thread pool happens exactly once. Once started,
/// the configuration cannot be changed."）。多次调用 `build_global` 会静默失败，
/// 让用户误以为"重新配置生效了"。这里用 `OnceLock` 保证只配一次，后续调用直接返回。
fn rayon_init_once() {
    use std::sync::OnceLock;
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        // I/O bound 任务，线程数 = CPU*2，最少 4 个（单核机器也至少 2 个，避免过小开销）。
        let num_threads = num_cpus_get().saturating_mul(2).max(4);
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build_global();
        eprintln!("[scan] rayon 全局线程池已初始化: {} 线程", num_threads);
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

// ---------------------------------------------------------------------------
// 非递归工作队列遍历
// ---------------------------------------------------------------------------

/// 一个目录节点在"定型"之前的中间状态。
/// `parent` 是指向父目录 `DirTask` 的 Arc；`pending` 是"还有多少个子目录没做完"；
/// `children` 收集已经做完的子节点（文件是同步立即放进去的，子目录是异步放进去的）。
struct DirTask {
    name: String,
    color: Color32,
    self_modified: u64,
    self_created: u64,
    self_accessed: u64,
    self_attrs: u32,
    parent: Option<Arc<DirTask>>,
    pending: AtomicUsize,
    children: Mutex<Vec<Node>>,
}

fn run_scan(
    root: &Path,
    counter: &Arc<AtomicU64>,
    cancel: &Arc<AtomicBool>,
    tx: &Sender<ScanMessage>,
    hardlink_seen: &Arc<DashSet<u64>>,
    cluster_size: u64,
) -> std::io::Result<Node> {
    let root_name = root.to_string_lossy().into_owned();
    let self_meta = std::fs::metadata(root).ok();
    let self_modified = system_time_to_filetime(self_meta.as_ref().and_then(|m| m.modified().ok()));
    #[cfg(windows)]
    let self_attrs = self_meta.as_ref().map(|m| {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes()
    }).unwrap_or(0x10);
    #[cfg(not(windows))]
    let self_attrs: u32 = 0x10;

    let root_task = Arc::new(DirTask {
        name: root_name,
        color: folder_color(0),
        self_modified,
        self_created: 0,
        self_accessed: 0,
        self_attrs,
        parent: None,
        pending: AtomicUsize::new(0),
        children: Mutex::new(Vec::new()),
    });

    let root_slot: Arc<Mutex<Option<Node>>> = Arc::new(Mutex::new(None));

    rayon::scope(|s| {
        process_dir(
            s, root.to_path_buf(), 0, root_task, root_slot.clone(),
            counter.clone(), cancel.clone(), tx.clone(), hardlink_seen.clone(), cluster_size,
        );
    });

    match root_slot.lock().unwrap().take() {
        Some(node) => Ok(node),
        None => Ok(Node::new_folder(root.to_string_lossy().into_owned(), folder_color(0), Vec::new())),
    }
}

/// 处理单个目录：批量枚举它的条目，文件直接算完塞进 task.children，
/// 子目录各自 spawn 一个新任务（不是递归调用，只是把工作丢进 rayon 的队列）。
#[allow(clippy::too_many_arguments)]
fn process_dir<'scope>(
    s: &rayon::Scope<'scope>,
    path: PathBuf,
    depth: usize,
    task: Arc<DirTask>,
    root_slot: Arc<Mutex<Option<Node>>>,
    counter: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    tx: Sender<ScanMessage>,
    hardlink_seen: Arc<DashSet<u64>>,
    cluster_size: u64,
) {
    if cancel.load(Ordering::Relaxed) {
        finalize(task, &root_slot);
        return;
    }

    let entries = match read_entries(&path, cluster_size) {
        Ok(e) => e,
        Err(e) => {
            if depth <= 3 {
                eprintln!("[scan] read_dir 失败 (depth={}, path={}, err={})", depth, path.display(), e);
            }
            // 目录打不开不算致命错误：这个目录当空目录处理，继续扫别的。
            finalize(task, &root_slot);
            return;
        }
    };

    let n = counter.fetch_add(entries.len() as u64, Ordering::Relaxed);
    if n / 5000 != (n + entries.len() as u64) / 5000 {
        let _ = tx.send(ScanMessage::Progress(n));
    }

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    let mut subdirs: Vec<crate::dir_enum::RawDirEntry> = Vec::new();
    let mut leaf_nodes: Vec<Node> = Vec::new();
    for e in entries {
        if e.is_dir {
            if e.attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                // 目录 reparse point（junction / symlink / 已知文件夹重定向，如
                // C:\Users\All Users -> C:\ProgramData）。这类目录在文件系统语义上
                // 是"指向别处"，不是真正独立的子树——如果递归进去，会把 target 目录下的
                // 文件在树里重复计入一遍（原始位置一次，junction 里再一次），
                // 逻辑/物理大小之和就会超过磁盘实际占用（>100%）。
                // WinDirStat / WizTree 都不会跟随这类目录，我们照做：只记一个叶子节点，
                // 不递归、不产生子节点。
                #[cfg(windows)]
                let tag = get_reparse_tag(&path.join(&e.name));
                #[cfg(not(windows))]
                let tag = 0u32;
                leaf_nodes.push(Node::new_folder_with_meta(
                    e.name, folder_color(depth + 1), Vec::new(),
                    e.modified_ft, e.created_ft, e.accessed_ft, e.attrs, tag, false, String::new(),
                ));
            } else {
                subdirs.push(e);
            }
        } else {
            // 文件节点。根据 mft_scan.rs 的去重口径：
            //   - logical_size 每次出现都计全值（不去重）
            //   - physical_size 按 FileId 去重（第一次出现计全值，后续硬链接位置计 0）
            //
            // file_id != 0 表示 NTFS/ReFS 返回了有效的 64 位文件参考号（卷内唯一）。
            // file_id == 0 表示卷不支持文件 ID（FAT32 等）或拿不到，不去重。
            let physical = if e.file_id != 0 {
                if hardlink_seen.insert(e.file_id) {
                    e.physical
                } else {
                    0u64
                }
            } else {
                e.physical
            };
            leaf_nodes.push(Node::new_file_with_meta(
                e.name, e.logical, physical, file_color(),
                e.modified_ft, e.created_ft, e.accessed_ft, e.attrs, 0, false, String::new(),
            ));
        }
    }
    if !leaf_nodes.is_empty() {
        task.children.lock().unwrap().extend(leaf_nodes);
    }

    if subdirs.is_empty() {
        finalize(task, &root_slot);
        return;
    }

    task.pending.store(subdirs.len(), Ordering::Release);

    for sub in subdirs {
        let child_path = path.join(&sub.name);
        let child_task = Arc::new(DirTask {
            name: sub.name,
            color: folder_color(depth + 1),
            self_modified: sub.modified_ft,
            self_created: sub.created_ft,
            self_accessed: sub.accessed_ft,
            self_attrs: sub.attrs,
            parent: Some(task.clone()),
            pending: AtomicUsize::new(0),
            children: Mutex::new(Vec::new()),
        });
        let root_slot = root_slot.clone();
        let counter = counter.clone();
        let cancel = cancel.clone();
        let tx = tx.clone();
        let hardlink_seen = hardlink_seen.clone();
        s.spawn(move |s| {
            process_dir(s, child_path, depth + 1, child_task, root_slot, counter, cancel, tx, hardlink_seen, cluster_size);
        });
    }
}

/// 把当前任务定型成 `Node`，并沿着 parent 链**用循环**往上传播完成通知。
/// 这里刻意不用递归：无论目录树多深，这个函数的调用栈深度都是 O(1)。
fn finalize(task: Arc<DirTask>, root_slot: &Arc<Mutex<Option<Node>>>) {
    let mut current = task;
    loop {
        let children = std::mem::take(&mut *current.children.lock().unwrap());
        let node = Node::new_folder_with_meta(
            current.name.clone(), current.color, children,
            current.self_modified, current.self_created, current.self_accessed,
            current.self_attrs, 0, false, String::new(),
        );

        match &current.parent {
            None => {
                *root_slot.lock().unwrap() = Some(node);
                return;
            }
            Some(parent) => {
                parent.children.lock().unwrap().push(node);
                let remaining = parent.pending.fetch_sub(1, Ordering::AcqRel) - 1;
                if remaining == 0 {
                    let next = parent.clone();
                    current = next;
                    continue;
                }
                return;
            }
        }
    }
}

/// 批量读取一个目录的条目：Windows 上优先走 `dir_enum::enum_dir_batch`
/// （每目录一次 handle，一次或几次批量调用拿到全部条目及其大小/时间/属性），
/// 失败时 fallback 到 `std::fs::read_dir` 逐条 stat。
///
/// `cluster_size` 仅在 fallback 路径里用于普通文件簇对齐，主路径拿系统返回的
/// AllocationSize 不受这个参数影响。
fn read_entries(path: &Path, cluster_size: u64) -> std::io::Result<Vec<crate::dir_enum::RawDirEntry>> {
    #[cfg(windows)]
    {
        if let Ok(v) = crate::dir_enum::enum_dir_batch(path) {
            return Ok(v);
        }
    }
    read_entries_fallback(path, cluster_size)
}

fn read_entries_fallback(path: &Path, cluster_size: u64) -> std::io::Result<Vec<crate::dir_enum::RawDirEntry>> {
    let rd = std::fs::read_dir(path)?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let modified_ft = system_time_to_filetime(meta.modified().ok());
        let created_ft = system_time_to_filetime(meta.created().ok());
        let accessed_ft = system_time_to_filetime(meta.accessed().ok());
        let is_dir = meta.is_dir();
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            let attrs = meta.file_attributes();
            let logical = meta.len();
            let physical = if is_dir { 0 } else { get_physical_size(&entry.path(), logical, attrs, cluster_size) };
            out.push(crate::dir_enum::RawDirEntry {
                name, is_dir, logical, physical, attrs, modified_ft, created_ft, accessed_ft, file_id: 0,
            });
        }
        #[cfg(not(windows))]
        {
            let attrs = if is_dir { 0x10 } else { 0x80 };
            let logical = meta.len();
            // Linux 下用 inode 作 file_id（仅用于单元测试验证去重逻辑）。
            // 生产环境（Windows）走的是 enum_dir_batch 主路径，不进这里。
            let file_id = if is_dir {
                0
            } else {
                use std::os::unix::fs::MetadataExt;
                meta.ino()
            };
            out.push(crate::dir_enum::RawDirEntry {
                name, is_dir, logical, physical: logical, attrs, modified_ft, created_ft, accessed_ft, file_id,
            });
        }
    }
    Ok(out)
}

/// 读取一个 reparse point 的 ReparseTag（IO_REPARSE_TAG_MOUNT_POINT / SYMLINK / 等）。
/// 只在真正遇到 reparse 目录（数量很少，通常几十个）时才调用一次，不影响整体性能。
/// REPARSE_DATA_BUFFER 结构体第一个字段就是 ULONG ReparseTag，直接读前 4 字节即可，
/// 不需要引入完整的结构体定义。
#[cfg(windows)]
fn get_reparse_tag(path: &Path) -> u32 {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;

    let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str())
        .chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(), FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(), OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return 0;
    }
    let mut buf = [0u8; 16 * 1024];
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle, FSCTL_GET_REPARSE_POINT,
            std::ptr::null(), 0,
            buf.as_mut_ptr() as *mut _, buf.len() as u32,
            &mut returned, std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 || returned < 4 {
        return 0;
    }
    u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
}

/// Windows 下获取文件的物理大小（仅 fallback 路径使用；批量枚举路径直接拿 AllocationSize）。
///
/// `cluster_size` 是从 `GetDiskFreeSpaceW` 动态获取的卷簇大小——NTFS 簇大小可以是
/// 512B/1K/2K/4K/8K/16K/32K/64K，不能写死 4096。
#[cfg(windows)]
fn get_physical_size(path: &Path, logical: u64, attrs: u32, cluster_size: u64) -> u64 {
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;

    const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x800;
    const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;

    if attrs & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_SPARSE_FILE) != 0 {
        let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str())
            .chain(std::iter::once(0)).collect();
        let mut high: u32 = 0;
        let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
        // MSDN：返回 INVALID_FILE_SIZE(0xFFFFFFFF) 时必须调 GetLastError 判断真失败还是恰好等于 4GB-1。
        // 当前写法：low != 0xFFFFFFFF || GetLastError()==0 都算成功。
        //  - low != 0xFFFFFFFF：肯定成功
        //  - low == 0xFFFFFFFF 且 GetLastError==0：文件大小恰好 4GB-1，也算成功
        //  - low == 0xFFFFFFFF 且 GetLastError!=0：真失败，走 fallback
        if low != 0xFFFFFFFF || unsafe { windows_sys::Win32::Foundation::GetLastError() } == 0 {
            return ((high as u64) << 32) | (low as u64);
        }
    }
    // 普通文件：簇对齐（用动态获取的簇大小，不写死 4096）
    if logical == 0 { 0 } else { ((logical + cluster_size - 1) / cluster_size) * cluster_size }
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

// ============================================================================
// 单元测试（Linux 上跑，验证迭代式工作队列 + 物理大小去重逻辑）
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    fn make_test_tree(base: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("disklens_test_{}", base));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        fs::write(root.join("a.txt"), b"hello").unwrap();
        fs::write(root.join("b.txt"), b"world!").unwrap();

        fs::create_dir_all(root.join("sub1")).unwrap();
        fs::write(root.join("sub1").join("c.txt"), b"abc").unwrap();

        let mut deep = root.clone();
        for i in 0..50 {
            deep = deep.join(format!("d{}", i));
            fs::create_dir_all(&deep).unwrap();
            fs::write(deep.join("file.txt"), format!("content {}", i)).unwrap();
        }
        root
    }

    #[test]
    fn test_scan_basic() {
        let root = make_test_tree("basic");
        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let hardlink_seen = Arc::new(DashSet::new());

        let node = run_scan(&root, &counter, &cancel, &tx, &hardlink_seen, 4096).unwrap();
        assert!(node.is_folder());
        assert!(node.file_count >= 4, "file_count = {}", node.file_count);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_scan_empty_dir() {
        let root = std::env::temp_dir().join("disklens_test_empty");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let hardlink_seen = Arc::new(DashSet::new());

        let node = run_scan(&root, &counter, &cancel, &tx, &hardlink_seen, 4096).unwrap();
        assert_eq!(node.file_count, 0);
        assert_eq!(node.folder_count, 0);

        let _ = fs::remove_dir_all(&root);
    }

    /// 关键测试：硬链接的 physical_size 去重，logical_size 不去重。
    /// 同一份文件 hardlink 到两个位置：
    ///   - root.logical_size = 文件大小 × 2（每个目录项都计）
    ///   - root.physical_size = 文件大小 × 1（去重，只算一次）
    #[test]
    fn test_physical_dedup_logical_not() {
        let root = std::env::temp_dir().join("disklens_test_hardlink_phys");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(root.join("dir_a")).unwrap();
        fs::create_dir_all(root.join("dir_b")).unwrap();

        let content = vec![0xABu8; 4096]; // 1 个簇的大小
        fs::write(root.join("dir_a").join("shared.bin"), &content).unwrap();
        fs::hard_link(
            root.join("dir_a").join("shared.bin"),
            root.join("dir_b").join("shared.bin"),
        ).unwrap();

        // 验证确实硬链接（同 inode）
        let ino_a = fs::metadata(root.join("dir_a").join("shared.bin")).unwrap().ino();
        let ino_b = fs::metadata(root.join("dir_b").join("shared.bin")).unwrap().ino();
        assert_eq!(ino_a, ino_b, "硬链接应该共享同一 inode");

        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let hardlink_seen = Arc::new(DashSet::new());

        let node = run_scan(&root, &counter, &cancel, &tx, &hardlink_seen, 4096).unwrap();

        // logical 不去重：4096 × 2 = 8192
        assert_eq!(node.file_count, 2);
        assert_eq!(
            node.logical_size, 8192,
            "logical 应该不去重: expected=8192, got={}", node.logical_size
        );
        // physical 去重：4096 × 1 = 4096
        assert_eq!(
            node.physical_size, 4096,
            "physical 应该去重: expected=4096, got={}", node.physical_size
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 多个硬链接 + 独立文件混合，验证去重不误伤独立文件。
    #[test]
    fn test_dedup_mixed() {
        let root = std::env::temp_dir().join("disklens_test_hardlink_mixed");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        fs::write(root.join("u1.bin"), vec![1u8; 1000]).unwrap();
        fs::write(root.join("u2.bin"), vec![2u8; 2000]).unwrap();

        fs::write(root.join("h.bin"), vec![0xAAu8; 5000]).unwrap();
        for i in 1..3 {
            fs::hard_link(root.join("h.bin"), root.join(format!("h{}.bin", i))).unwrap();
        }

        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let hardlink_seen = Arc::new(DashSet::new());

        let node = run_scan(&root, &counter, &cancel, &tx, &hardlink_seen, 4096).unwrap();

        // 2 独立 + 3 硬链接 = 5 个文件
        assert_eq!(node.file_count, 5);

        // logical 不去重：1000 + 2000 + 5000 × 3 = 18000
        assert_eq!(
            node.logical_size, 18000,
            "logical 不去重: expected=18000, got={}", node.logical_size
        );
        // physical 去重：1000 + 2000 + 5000 × 1 = 8000
        assert_eq!(
            node.physical_size, 8000,
            "physical 去重: expected=8000, got={}", node.physical_size
        );

        let _ = fs::remove_dir_all(&root);
    }
}
