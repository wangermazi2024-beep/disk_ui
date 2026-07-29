//! 目录扫描。
//!
//! 扫描入口在 `spawn_scan`。Windows 下如果是整盘根路径且当前有管理员权限、
//! 目标卷是 NTFS，会优先走 `mft_scan::scan_drive_via_mft` 的 $MFT 直读快速路径
//!（WizTree/Everything 同款原理）；任何一步失败都直接 fallback 到标准目录遍历，
//! 保证功能上始终可用，只是速度上有快慢之分。
//!
//! 扫描线程在开始时会顺带查询一次 `DiskInfo`（盘符、卷标、容量），随扫描结果
//! 一起通过 `ScanMessage::Done` 发回主线程，这样 UI 不需要再单独发一次系统调用。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use egui::Color32;

use crate::disk_info::DiskInfo;
use crate::model::Node;

pub enum ScanMessage {
    Progress(u64),
    /// 扫描完成：节点树 + 该分区对应的 DiskInfo（可能为 None：非 Windows、
    /// 非 NTFS、或扫描的是子目录而非整盘根）。
    Done(Box<Node>, Option<DiskInfo>),
    Error(String),
}

const MAX_ENTRIES: u64 = 300_000;

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

/// 是否是形如 `C:\` / `C:/` 的整盘根路径。只有这种情况才有资格走 MFT 直读路径——
/// 扫描某个子目录时 MFT 直读拿到的是全盘数据，裁剪出子树意义不大，直接走传统遍历更简单可靠。
#[cfg(windows)]
fn as_drive_root(path: &Path) -> Option<char> {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    if bytes.len() == 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
        let c = bytes[0] as char;
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase());
        }
    }
    None
}

/// 从一个路径里推断盘符（取首字符并大写）。非 Windows 上也用得到——
/// 因为 `query_disk_info` 在非 Windows 上直接返回 None，不会真的去调系统 API。
fn drive_letter_of(path: &Path) -> Option<char> {
    path.to_string_lossy()
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
}

pub fn spawn_scan(root: PathBuf, tx: Sender<ScanMessage>) {
    std::thread::spawn(move || {
        // 先查磁盘信息（在非 Windows 上返回 None，不影响后续逻辑）
        let disk_info = drive_letter_of(&root).and_then(crate::disk_info::query_disk_info);
        eprintln!(
            "[scan] 启动扫描: root={}, disk_info={}",
            root.display(),
            disk_info
                .as_ref()
                .map(|d| d.display_name())
                .unwrap_or_else(|| "None".into())
        );

        // 是否是整盘根路径（"C:\"）。只有整盘扫描时才把根节点名字替换成卷标名，
        // 扫描子目录时保留用户输入的路径作为根名字，避免误判。
        #[cfg(windows)]
        let is_drive_root = as_drive_root(&root).is_some();
        #[cfg(not(windows))]
        let is_drive_root = {
            let s = root.to_string_lossy();
            s.len() == 3
                && s.as_bytes()[1] == b':'
                && (s.as_bytes()[2] == b'\\' || s.as_bytes()[2] == b'/')
        };

        // Windows 下，如果目标是一个整盘根目录（如 C:\），且当前有管理员权限、
        // 目标卷是 NTFS，就优先走 $MFT 直读快速路径（WizTree/Everything 同款原理）。
        // 任何一步失败（没权限/非NTFS/记录损坏等）都直接 fallback 回标准目录遍历，
        // 保证功能上始终可用，只是速度上有快慢之分。
        #[cfg(windows)]
        {
            if let Some(drive) = as_drive_root(&root) {
                if crate::mft_scan::mft_scan_available(drive) {
                    eprintln!("[scan] 走 MFT 直读路径: drive={}", drive);
                    match crate::mft_scan::scan_drive_via_mft(drive, &tx) {
                        Ok(result) => {
                            let mut root_node = result.root;
                            // 整盘扫描时，用卷标把根节点名字改得好看一点
                            // （"C:\" -> "本地磁盘C (C:)"）
                            if is_drive_root {
                                if let Some(info) = &disk_info {
                                    root_node.name = info.display_name();
                                }
                            }
                            eprintln!(
                                "[scan] MFT 直读完成: files={}, folders={}, size={:.2}GB",
                                root_node.file_count,
                                root_node.folder_count,
                                root_node.size as f64 / 1e9
                            );
                            let _ = tx.send(ScanMessage::Done(Box::new(root_node), disk_info));
                            return;
                        }
                        Err(e) => {
                            // 不中断，退回传统遍历；把原因打到 stderr 方便排查，
                            // 不打断 UI（UI 只关心最终能不能拿到数据）。
                            eprintln!("[scan] MFT 直读失败，回退到标准目录遍历: {e}");
                        }
                    }
                } else {
                    eprintln!(
                        "[scan] MFT 不可用，走标准目录遍历: drive={}",
                        drive
                    );
                }
            } else {
                eprintln!(
                    "[scan] 目标不是整盘根路径，走标准目录遍历: root={}",
                    root.display()
                );
            }
        }
        #[cfg(not(windows))]
        {
            eprintln!(
                "[scan] 非 Windows 平台，走标准目录遍历: root={}",
                root.display()
            );
        }

        let counter = Arc::new(AtomicU64::new(0));
        match scan_dir(&root, 0, &counter, &tx) {
            Ok(mut node) => {
                if is_drive_root {
                    if let Some(info) = &disk_info {
                        node.name = info.display_name();
                    }
                }
                eprintln!(
                    "[scan] 标准遍历完成: files={}, folders={}, size={:.2}GB",
                    node.file_count,
                    node.folder_count,
                    node.size as f64 / 1e9
                );
                let _ = tx.send(ScanMessage::Done(Box::new(node), disk_info));
            }
            Err(e) => {
                eprintln!("[scan] 标准遍历失败: {e}");
                let _ = tx.send(ScanMessage::Error(format!("扫描失败: {e}")));
            }
        }
    });
}

/// 把 `SystemTime` 折算成 Windows FILETIME（1601-01-01 起 100ns 单位）。
/// 失败返回 0（UI 上会显示成 "—"）。
fn system_time_to_filetime(t: Option<SystemTime>) -> u64 {
    let t = match t {
        Some(t) => t,
        None => return 0,
    };
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            // UNIX epoch (1970-01-01) 比 Windows epoch (1601-01-01) 晚 11644473600 秒
            const UNIX_TO_FILETIME_OFFSET_SECS: u64 = 11_644_473_600;
            let unix_100ns = d.as_secs() * 10_000_000 + (d.subsec_nanos() / 100) as u64;
            unix_100ns + UNIX_TO_FILETIME_OFFSET_SECS * 10_000_000
        }
        Err(_) => 0,
    }
}

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

    // 拿本目录自身的元数据（修改时间、属性）
    let self_meta = std::fs::metadata(path).ok();
    let self_modified = system_time_to_filetime(self_meta.as_ref().and_then(|m| m.modified().ok()));
    #[cfg(windows)]
    let self_attrs = self_meta
        .as_ref()
        .map(|m| {
            use std::os::windows::fs::MetadataExt;
            m.file_attributes()
        })
        .unwrap_or(0x10);
    #[cfg(not(windows))]
    let self_attrs: u32 = 0x10;

    let mut children = Vec::new();
    for entry in std::fs::read_dir(path)?.flatten() {
        if counter.load(Ordering::Relaxed) > MAX_ENTRIES {
            eprintln!(
                "[scan] 达到 MAX_ENTRIES 上限 {}，提前停止",
                MAX_ENTRIES
            );
            break;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let entry_name = entry.file_name().to_string_lossy().into_owned();
        let n = counter.fetch_add(1, Ordering::Relaxed);
        if n % 500 == 0 {
            let _ = tx.send(ScanMessage::Progress(n));
        }

        let modified_ft = system_time_to_filetime(meta.modified().ok());
        #[cfg(windows)]
        let attrs = {
            use std::os::windows::fs::MetadataExt;
            meta.file_attributes()
        };
        #[cfg(not(windows))]
        let attrs: u32 = if meta.is_dir() { 0x10 } else { 0x80 };

        if meta.is_dir() {
            if let Ok(child) = scan_dir(&entry.path(), depth + 1, counter, tx) {
                children.push(child);
            }
        } else {
            children.push(Node::new_file_with_meta(
                entry_name,
                meta.len(),
                file_color(),
                modified_ft,
                attrs,
            ));
        }
    }
    Ok(Node::new_folder_with_meta(
        name,
        folder_color(depth),
        children,
        self_modified,
        self_attrs,
    ))
}

/// 演示数据：C 盘 + D 盘两个分区，各自是独立的根节点。
/// 真实扫描前用作 UI 占位，让用户先看到布局；点击"扫描"后会被替换成真实数据。
pub fn demo_partitions() -> Vec<Node> {
    let leaf_with_meta = |name: &str, size: u64, ft: u64, attr: u32| {
        Node::new_file_with_meta(name, size, file_color(), ft, attr)
    };

    // 一个示意性的 FILETIME：2024-01-15 10:30:00 UTC，对应 133475418000000000
    const DEMO_FT: u64 = 133_475_418_000_000_000;

    // ── C 盘 ──────────────────────────────────────────────────────
    let windows = Node::new_folder_with_meta(
        "Windows",
        folder_color(1),
        vec![
            Node::new_folder_with_meta(
                "System32",
                folder_color(2),
                vec![
                    leaf_with_meta("ntoskrnl.exe", 11_200_000, DEMO_FT, 0xA4),
                    leaf_with_meta("kernel32.dll", 780_000, DEMO_FT, 0xA0),
                    leaf_with_meta("drivers.cab", 640_000_000, DEMO_FT, 0xA0),
                ],
                DEMO_FT,
                0x10,
            ),
            Node::new_folder_with_meta(
                "WinSxS",
                folder_color(2),
                vec![
                    leaf_with_meta("manifest_a.cat", 2_100_000_000, DEMO_FT, 0xA0),
                    leaf_with_meta("manifest_b.cat", 1_800_000_000, DEMO_FT, 0xA0),
                ],
                DEMO_FT,
                0x10,
            ),
            leaf_with_meta("explorer.exe", 5_400_000, DEMO_FT, 0xA0),
        ],
        DEMO_FT,
        0x16, // DIRECTORY | SYSTEM
    );

    let program_files = Node::new_folder_with_meta(
        "Program Files",
        folder_color(1),
        vec![
            Node::new_folder_with_meta(
                "Adobe",
                folder_color(2),
                vec![
                    leaf_with_meta("Photoshop.exe", 2_300_000_000, DEMO_FT, 0xA0),
                    leaf_with_meta("Premiere.exe", 3_100_000_000, DEMO_FT, 0xA0),
                ],
                DEMO_FT,
                0x10,
            ),
            Node::new_folder_with_meta(
                "Microsoft Office",
                folder_color(2),
                vec![
                    leaf_with_meta("WINWORD.EXE", 890_000_000, DEMO_FT, 0xA0),
                    leaf_with_meta("EXCEL.EXE", 760_000_000, DEMO_FT, 0xA0),
                ],
                DEMO_FT,
                0x10,
            ),
        ],
        DEMO_FT,
        0x10,
    );

    let users_c = Node::new_folder_with_meta(
        "Users",
        folder_color(1),
        vec![Node::new_folder_with_meta(
            "Alex",
            folder_color(2),
            vec![
                Node::new_folder_with_meta(
                    "AppData",
                    folder_color(3),
                    vec![Node::new_folder_with_meta(
                        "Temp",
                        folder_color(4),
                        vec![leaf_with_meta("cache.tmp", 1_100_000_000, DEMO_FT, 0xA0)],
                        DEMO_FT,
                        0x10,
                    )],
                    DEMO_FT,
                    0x12, // DIRECTORY | HIDDEN
                ),
                Node::new_folder_with_meta(
                    "Documents",
                    folder_color(3),
                    vec![leaf_with_meta("thesis.docx", 4_200_000, DEMO_FT, 0xA0)],
                    DEMO_FT,
                    0x10,
                ),
            ],
            DEMO_FT,
            0x10,
        )],
        DEMO_FT,
        0x10,
    );

    let c_drive = Node::new_folder_with_meta(
        "本地磁盘C (C:\\)",
        folder_color(0),
        vec![
            windows,
            program_files,
            users_c,
            leaf_with_meta("pagefile.sys", 16_000_000_000, DEMO_FT, 0xA4), // SYSTEM|ARCHIVE
            leaf_with_meta("hiberfil.sys", 8_000_000_000, DEMO_FT, 0xA4),
        ],
        DEMO_FT,
        0x10,
    );

    // ── D 盘 ──────────────────────────────────────────────────────
    let steam = Node::new_folder_with_meta(
        "Steam",
        folder_color(1),
        vec![Node::new_folder_with_meta(
            "steamapps",
            folder_color(2),
            vec![Node::new_folder_with_meta(
                "common",
                folder_color(3),
                vec![
                    Node::new_folder_with_meta(
                        "Cyberpunk2077",
                        folder_color(4),
                        vec![leaf_with_meta("archive.pak", 68_000_000_000, DEMO_FT, 0xA0)],
                        DEMO_FT,
                        0x10,
                    ),
                    Node::new_folder_with_meta(
                        "Elden Ring",
                        folder_color(4),
                        vec![leaf_with_meta("data.bin", 45_000_000_000, DEMO_FT, 0xA0)],
                        DEMO_FT,
                        0x10,
                    ),
                    Node::new_folder_with_meta(
                        "GTA V",
                        folder_color(4),
                        vec![leaf_with_meta("update.rpf", 36_000_000_000, DEMO_FT, 0xA0)],
                        DEMO_FT,
                        0x10,
                    ),
                ],
                DEMO_FT,
                0x10,
            )],
            DEMO_FT,
            0x10,
        )],
        DEMO_FT,
        0x10,
    );

    let downloads = Node::new_folder_with_meta(
        "Downloads",
        folder_color(1),
        vec![
            leaf_with_meta("movie_4k.mkv", 18_000_000_000, DEMO_FT, 0xA0),
            leaf_with_meta("backup_2024.zip", 9_500_000_000, DEMO_FT, 0xA0),
            leaf_with_meta("installer.iso", 4_700_000_000, DEMO_FT, 0xA0),
        ],
        DEMO_FT,
        0x10,
    );

    let projects = Node::new_folder_with_meta(
        "Projects",
        folder_color(1),
        vec![Node::new_folder_with_meta(
            "my-app",
            folder_color(2),
            vec![
                Node::new_folder_with_meta(
                    "node_modules",
                    folder_color(3),
                    vec![leaf_with_meta("packages...", 2_800_000_000, DEMO_FT, 0xA0)],
                    DEMO_FT,
                    0x10,
                ),
                leaf_with_meta("dist.tar.gz", 450_000_000, DEMO_FT, 0xA0),
            ],
            DEMO_FT,
            0x10,
        )],
        DEMO_FT,
        0x10,
    );

    let d_drive = Node::new_folder_with_meta(
        "本地磁盘 (D:\\)",
        folder_color(0),
        vec![steam, downloads, projects],
        DEMO_FT,
        0x10,
    );

    vec![c_drive, d_drive]
}

/// 兼容旧调用：返回单个 demo 节点（仅 C 盘）。
#[allow(dead_code)]
pub fn demo_tree() -> Node {
    demo_partitions().remove(0)
}
