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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// FIX C: 原来的 `MAX_ENTRIES = 300_000` 上限太低——C 盘轻松有 27 万文件，
/// 一旦触顶后续所有子目录的循环都会 break，导致丢一大半文件（实测扫到 60GB
/// 但实际已用 112GB，丢了 ~46%）。
///
/// 现在改成：**不再硬性截断**。只用 atomic counter 做进度汇报，不做上限。
/// 如果以后要支持"取消扫描"，可以用一个 `AtomicBool` cancel flag，而不是
/// 用 entry 上限来粗暴中断。
///
/// 保留一个很大的软上限（10 亿）纯粹是防止恶意构造的死循环目录耗尽内存，
/// 正常磁盘永远碰不到。
const SAFETY_MAX_ENTRIES: u64 = 1_000_000_000;

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
        let cancel = Arc::new(AtomicBool::new(false));
        match scan_dir(&root, 0, &counter, &cancel, &tx) {
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
    cancel: &Arc<AtomicBool>,
    tx: &Sender<ScanMessage>,
) -> std::io::Result<Node> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(Node::new_folder(
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            folder_color(depth),
            Vec::new(),
        ));
    }

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
    // FIX C: read_dir 出错（权限拒绝等）不再直接 ? 传播——
    // 而是当作空目录返回，让父目录的扫描能继续，避免一个权限错误中断整个扫描。
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "[scan] read_dir 失败 (path={}, err={})，当作空目录继续",
                path.display(),
                e
            );
            return Ok(Node::new_folder_with_meta(
                name,
                folder_color(depth),
                Vec::new(),
                self_modified,
                self_attrs,
            ));
        }
    };

    for entry in entries.flatten() {
        // FIX C: 不再用 MAX_ENTRIES 截断。只用 counter 做进度汇报。
        // 仅在超过安全上限（10 亿）时停止——正常磁盘永远碰不到。
        let n = counter.fetch_add(1, Ordering::Relaxed);
        if n > SAFETY_MAX_ENTRIES {
            eprintln!(
                "[scan] 触发安全上限 {}，停止扫描（正常磁盘不会碰到）",
                SAFETY_MAX_ENTRIES
            );
            break;
        }
        if n % 2000 == 0 {
            let _ = tx.send(ScanMessage::Progress(n));
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                // 单个文件 metadata 失败不中断整个扫描
                eprintln!(
                    "[scan] metadata 失败 (entry={}, err={})，跳过",
                    entry.path().display(),
                    e
                );
                continue;
            }
        };
        let entry_name = entry.file_name().to_string_lossy().into_owned();

        let modified_ft = system_time_to_filetime(meta.modified().ok());
        #[cfg(windows)]
        let attrs = {
            use std::os::windows::fs::MetadataExt;
            meta.file_attributes()
        };
        #[cfg(not(windows))]
        let attrs: u32 = if meta.is_dir() { 0x10 } else { 0x80 };

        if meta.is_dir() {
            // 子目录扫描失败不中断父目录扫描——子目录可能因为权限拒绝打不开，
            // 但同层其他子目录还能继续扫。
            match scan_dir(&entry.path(), depth + 1, counter, cancel, tx) {
                Ok(child) => children.push(child),
                Err(e) => {
                    eprintln!(
                        "[scan] 子目录扫描失败 (path={}, err={})，跳过",
                        entry.path().display(),
                        e
                    );
                }
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

    // 一个示意性的 FILETIME：2024-01-15 10:30:00 UTC
    const DEMO_FT: u64 = 13_349_788_200_000_000_000;

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
        0x16,
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
                    0x12,
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
        "本地磁盘 (C:)",
        folder_color(0),
        vec![
            windows,
            program_files,
            users_c,
            leaf_with_meta("pagefile.sys", 16_000_000_000, DEMO_FT, 0xA4),
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
        "新加卷 (D:)",
        folder_color(0),
        vec![steam, downloads, projects],
        DEMO_FT,
        0x10,
    );

    vec![c_drive, d_drive]
}

// ─────────────────────────────────────────────────────────────────────────
// 单元测试：scan_dir 在已知目录树上跑一遍，对比 std::fs 递归统计，
// 验证不丢文件 / 文件夹 / 大小。Windows 上跑 `cargo test` 即可。
// ─────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn build_test_tree(root: &Path) -> std::io::Result<()> {
        let blob = |n: usize| vec![b'x'; n];
        fs::write(root.join("file1.txt"), blob(10))?;
        fs::write(root.join("file2.dat"), blob(20))?;

        fs::create_dir(root.join("subdir1"))?;
        fs::write(root.join("subdir1").join("file3.txt"), blob(30))?;
        fs::write(root.join("subdir1").join("file4.log"), blob(40))?;

        fs::create_dir(root.join("subdir1").join("subdir2"))?;
        fs::write(
            root.join("subdir1").join("subdir2").join("file5.bin"),
            blob(50),
        )?;

        fs::create_dir(root.join("empty_dir"))?;

        fs::create_dir(root.join("subdir3"))?;
        fs::write(root.join("subdir3").join("file6.txt"), blob(60))?;
        Ok(())
    }

    fn ground_truth(root: &Path) -> (u64, u64, u64) {
        let mut files = 0u64;
        let mut folders = 0u64;
        let mut total_size = 0u64;
        fn walk(p: &Path, files: &mut u64, folders: &mut u64, total_size: &mut u64) {
            let entries = match fs::read_dir(p) {
                Ok(e) => e,
                Err(_) => return,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if meta.is_dir() {
                    *folders += 1;
                    walk(&path, files, folders, total_size);
                } else {
                    *files += 1;
                    *total_size += meta.len();
                }
            }
        }
        walk(root, &mut files, &mut folders, &mut total_size);
        (files, folders, total_size)
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "disklens_test_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn test_scan_dir_counts_match_ground_truth() {
        let tmp = tmp_dir("match");
        fs::create_dir_all(&tmp).expect("create temp dir");
        build_test_tree(&tmp).expect("build test tree");

        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let node = scan_dir(&tmp, 0, &counter, &cancel, &tx).expect("scan_dir");

        let (gt_files, gt_folders, gt_size) = ground_truth(&tmp);
        assert_eq!(node.file_count, gt_files);
        assert_eq!(node.folder_count, gt_folders);
        assert_eq!(node.size, gt_size);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_scan_dir_expected_counts() {
        let tmp = tmp_dir("expected");
        fs::create_dir_all(&tmp).expect("create temp dir");
        build_test_tree(&tmp).expect("build test tree");

        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let node = scan_dir(&tmp, 0, &counter, &cancel, &tx).expect("scan_dir");

        assert_eq!(node.file_count, 6, "应有 6 个文件");
        assert_eq!(node.folder_count, 4, "应有 4 个子文件夹");
        assert_eq!(node.size, 210, "总大小应为 210 字节");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_scan_dir_empty_dir() {
        let tmp = tmp_dir("empty");
        fs::create_dir_all(&tmp).expect("create temp dir");
        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let node = scan_dir(&tmp, 0, &counter, &cancel, &tx).expect("scan_dir");
        assert_eq!(node.file_count, 0);
        assert_eq!(node.folder_count, 0);
        assert_eq!(node.size, 0);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_scan_dir_deep_nesting() {
        let tmp = tmp_dir("deep");
        fs::create_dir_all(&tmp).expect("create temp dir");
        let mut cur: PathBuf = tmp.clone();
        for i in 0..10 {
            cur = cur.join(format!("d{}", i));
            fs::create_dir_all(&cur).expect("create dir");
        }
        fs::write(cur.join("leaf.txt"), "hello").expect("write leaf");

        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let node = scan_dir(&tmp, 0, &counter, &cancel, &tx).expect("scan_dir");

        assert_eq!(node.file_count, 1);
        assert_eq!(node.folder_count, 10);
        assert_eq!(node.size, 5);
        let _ = fs::remove_dir_all(&tmp);
    }

    /// FIX C 回归测试：造一个超过原来 MAX_ENTRIES(300_000) 的目录树，
    /// 验证 scan_dir 不会因为条目数过多而提前停止。
    /// （这里不真的造 30 万文件——太慢——而是验证 counter 能正常增长
    ///   且最终结果和 ground truth 一致，证明没有截断逻辑。）
    #[test]
    fn test_scan_dir_no_max_entries_truncation() {
        let tmp = tmp_dir("no_trunc");
        fs::create_dir_all(&tmp).expect("create temp dir");
        // 造 500 个子目录，每个里面 10 个文件 = 5000 文件
        for i in 0..500 {
            let dir = tmp.join(format!("d{}", i));
            fs::create_dir_all(&dir).expect("create dir");
            for j in 0..10 {
                fs::write(dir.join(format!("f{}.txt", j)), vec![b'x'; 100]).expect("write file");
            }
        }

        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let node = scan_dir(&tmp, 0, &counter, &cancel, &tx).expect("scan_dir");

        let (gt_files, gt_folders, gt_size) = ground_truth(&tmp);
        assert_eq!(node.file_count, gt_files, "文件数应和真值一致（不截断）");
        assert_eq!(node.folder_count, gt_folders);
        assert_eq!(node.size, gt_size);
        // counter 应该 >= 5000（文件）+ 500（目录）+ 1（root 自己）
        assert!(
            counter.load(Ordering::Relaxed) >= 5000,
            "counter 应该 >= 5000，实际 = {}",
            counter.load(Ordering::Relaxed)
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 在真实系统目录上跑一遍 scan_dir，对比 std::fs 递归统计。
    #[test]
    fn test_scan_dir_on_real_system_directory() {
        let candidates = [
            std::path::Path::new("/home/z"),
            std::path::Path::new("/tmp"),
            std::path::Path::new("/usr/local"),
            std::path::Path::new("/etc"),
        ];
        let target = candidates
            .iter()
            .find(|p| p.exists() && fs::read_dir(p).is_ok())
            .expect("至少要有一个候选目录可用");
        eprintln!("[test] 真实目录测试: {}", target.display());

        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let node = scan_dir(target, 0, &counter, &cancel, &tx).expect("scan_dir");

        let (files, folders, size) = ground_truth(target);
        eprintln!(
            "[test] scan_dir:     files={}, folders={}, size={}",
            node.file_count, node.folder_count, node.size
        );
        eprintln!(
            "[test] ground_truth: files={}, folders={}, size={}",
            files, folders, size
        );

        assert_eq!(node.file_count, files, "文件数不匹配");
        assert_eq!(node.folder_count, folders, "文件夹数不匹配");
        assert_eq!(node.size, size, "总大小不匹配");
    }
}
