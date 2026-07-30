//! 目录扫描。
//!
//! 扫描入口在 `spawn_scan`。Windows 下如果是整盘根路径且当前有管理员权限、
//! 目标卷是 NTFS，会优先走 `mft_scan::scan_drive_via_mft` 的 MFT 直读快速路径
//!（WizTree/Everything 同款原理）；任何一步失败都直接 fallback 到标准目录遍历，
//! 保证功能上始终可用，只是速度上有快慢之分。
//!
//! ## 关于条目上限
//! **没有任何上限**。用户要求"全量扫出来，有多少扫多少"。原来 MAX_ENTRIES=300_000
//! 的截断已经被移除（那个上限导致 C 盘 27 万文件就触顶，丢了 ~46%）。
//! 如果以后要支持"取消扫描"，用一个 `AtomicBool` cancel flag，不要用条目上限。

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
    /// 扫描完成：节点树 + 该分区对应的 DiskInfo + 按物理记录去重后的大小。
    ///
    /// `dedup_size` 是"扫描汇总 vs 系统已用空间"一致性判断应该用的数字——
    /// 树里的 `Node::size` 故意允许硬链接在每个出现的目录下都计一次（匹配
    /// Explorer 逐目录浏览的展示方式），拿它去跟系统已用比会被硬链接场景
    /// 误判成"数据异常"，所以两个数字分开传，UI 层展示各自该展示的东西：
    /// 树/treemap 用 `Node::size`，"一致性"这个统计数字用 `dedup_size`。
    Done(Box<Node>, Option<DiskInfo>, u64),
    Error(String),
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

/// 是否是形如 `C:\` / `C:/` 的整盘根路径。
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

/// 从一个路径里推断盘符（取首字符并大写）。
fn drive_letter_of(path: &Path) -> Option<char> {
    path.to_string_lossy()
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
}

pub fn spawn_scan(root: PathBuf, tx: Sender<ScanMessage>) {
    std::thread::spawn(move || {
        let scan_start = SystemTime::now();
        // 先查磁盘信息
        let disk_info = drive_letter_of(&root).and_then(crate::disk_info::query_disk_info);
        eprintln!(
            "[scan] 启动扫描: root={}, disk_info={}",
            root.display(),
            disk_info
                .as_ref()
                .map(|d| d.display_name())
                .unwrap_or_else(|| "None".into())
        );

        // 是否是整盘根路径
        #[cfg(windows)]
        let is_drive_root = as_drive_root(&root).is_some();
        #[cfg(not(windows))]
        let is_drive_root = {
            let s = root.to_string_lossy();
            s.len() == 3
                && s.as_bytes()[1] == b':'
                && (s.as_bytes()[2] == b'\\' || s.as_bytes()[2] == b'/')
        };

        #[cfg(windows)]
        {
            if let Some(drive) = as_drive_root(&root) {
                if crate::mft_scan::mft_scan_available(drive) {
                    eprintln!("[scan] 走 MFT 直读路径: drive={}", drive);
                    let mft_start = SystemTime::now();
                    match crate::mft_scan::scan_drive_via_mft(drive, &tx) {
                        Ok(result) => {
                            let mut root_node = result.root;
                            if is_drive_root {
                                if let Some(info) = &disk_info {
                                    root_node.name = info.display_name();
                                }
                            }
                            let mft_elapsed = mft_start.elapsed().unwrap_or_default();
                            eprintln!(
                                "[scan] MFT 直读完成: files={}, folders={}, size={}, 耗时 {:.1}s",
                                root_node.file_count,
                                root_node.folder_count,
                                crate::format::human_size(root_node.size),
                                mft_elapsed.as_secs_f64()
                            );
                            let dedup_size = result.dedup_size;
                            // 一致性检查：dedup_size（物理去重总量，每份数据只算一次）
                            // vs 系统已用空间。注意故意不用 root_node.size —— 那是"树汇总"，
                            // 同一份硬链接数据挂在几个目录下就会被计几次，是 Explorer/WizTree
                            // 的标准展示行为，拿它去跟系统已用比会被硬链接场景误报成"异常"。
                            if let Some(info) = &disk_info {
                                let scanned_str = crate::format::human_size(dedup_size);
                                let used_str = crate::format::human_size(info.used_bytes);
                                let ratio = if info.used_bytes > 0 {
                                    dedup_size as f64 / info.used_bytes as f64 * 100.0
                                } else {
                                    0.0
                                };
                                eprintln!(
                                    "[scan] 一致性检查（物理去重口径）: 扫描汇总={}, 系统已用={}, 比例={:.1}%",
                                    scanned_str, used_str, ratio
                                );
                                if ratio < 60.0 {
                                    eprintln!(
                                        "[scan] ⚠ 扫描汇总不到系统已用的 60%，可能有丢数据（正常 70%~95%，差值含 $MFT/簇碎片/VSS/USN日志）"
                                    );
                                } else if ratio > 105.0 {
                                    eprintln!(
                                        "[scan] ⚠ 扫描汇总超过系统已用 105%，即使已按 base record 去重仍偏高，可能含 ADS（备用数据流）未单独计入或压缩/稀疏文件的逻辑大小与占用不一致"
                                    );
                                } else {
                                    eprintln!(
                                        "[scan] ✓ 扫描汇总在合理范围内（差值 = $MFT + 簇碎片 + VSS + NTFS元数据）"
                                    );
                                }
                            }
                            let _ = tx.send(ScanMessage::Done(Box::new(root_node), disk_info, dedup_size));
                            let total = scan_start.elapsed().unwrap_or_default();
                            eprintln!("[scan] 扫描总耗时: {:.1}s", total.as_secs_f64());
                            return;
                        }
                        Err(e) => {
                            eprintln!("[scan] MFT 直读失败，回退到标准目录遍历: {e}");
                        }
                    }
                } else {
                    eprintln!("[scan] MFT 不可用，走标准目录遍历: drive={}", drive);
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
                    "[scan] 标准遍历完成: files={}, folders={}, size={}",
                    node.file_count,
                    node.folder_count,
                    crate::format::human_size(node.size)
                );
                let dedup_size_fallback = node.size;
                // 标准目录遍历（非 MFT 直读的回退路径）没有 MFT record 号可以
                // 拿来去重，没法像 MFT 路径那样精确区分"同一份数据的多个硬链接
                // 位置"，这里退化成直接用 node.size 本身（即不做去重）。这个
                // 路径本来就只在拿不到管理员权限/非 NTFS 卷时才会走到，硬链接
                // 密集的场景（DriverStore 那种）基本只出现在系统盘、走的是 MFT
                // 直读路径，所以这里的近似不影响实际使用场景。
                let _ = tx.send(ScanMessage::Done(Box::new(node), disk_info, dedup_size_fallback));
            }
            Err(e) => {
                eprintln!("[scan] 标准遍历失败: {e}");
                let _ = tx.send(ScanMessage::Error(format!("扫描失败: {e}")));
            }
        }
        let total = scan_start.elapsed().unwrap_or_default();
        eprintln!("[scan] 扫描总耗时: {:.1}s", total.as_secs_f64());
    });
}

/// 把 `SystemTime` 折算成 Windows FILETIME（1601-01-01 起 100ns 单位）。
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

    // 顶层目录开始时打一行日志（深度 0/1/2），方便定位卡在哪
    if depth <= 2 {
        eprintln!("[scan] 扫描目录 (depth={}): {}", depth, path.display());
    }

    // 拿本目录自身的元数据
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
    // read_dir 出错（权限拒绝等）不中断，当作空目录返回
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            // 只在浅层打这个警告，深层目录太多会刷屏
            if depth <= 3 {
                eprintln!(
                    "[scan] read_dir 失败 (depth={}, path={}, err={})，当作空目录继续",
                    depth,
                    path.display(),
                    e
                );
            }
            return Ok(Node::new_folder_with_meta(
                name,
                folder_color(depth),
                Vec::new(),
                self_modified,
                self_attrs,
            ));
        }
    };

    let mut entries_iterated: u64 = 0;
    for entry in entries.flatten() {
        // 没有上限——全量扫
        let n = counter.fetch_add(1, Ordering::Relaxed);
        // 进度汇报：每 5000 项发一次，避免 channel 拥塞
        if n % 5000 == 0 {
            let _ = tx.send(ScanMessage::Progress(n));
        }
        entries_iterated += 1;

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                if depth <= 3 {
                    eprintln!(
                        "[scan] metadata 失败 (entry={}, err={})，跳过",
                        entry.path().display(),
                        e
                    );
                }
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
            match scan_dir(&entry.path(), depth + 1, counter, cancel, tx) {
                Ok(child) => children.push(child),
                Err(e) => {
                    if depth <= 3 {
                        eprintln!(
                            "[scan] 子目录扫描失败 (path={}, err={})，跳过",
                            entry.path().display(),
                            e
                        );
                    }
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
    // 顶层目录扫完时打一行汇总
    if depth <= 1 {
        eprintln!(
            "[scan] 目录扫描完成 (depth={}, path={}, 本层 {} 项)",
            depth,
            path.display(),
            entries_iterated
        );
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
pub fn demo_partitions() -> Vec<Node> {
    let leaf_with_meta = |name: &str, size: u64, ft: u64, attr: u32| {
        Node::new_file_with_meta(name, size, file_color(), ft, attr)
    };

    const DEMO_FT: u64 = 13_349_788_200_000_000_000;

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
// 单元测试
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
        fs::write(root.join("subdir1").join("subdir2").join("file5.bin"), blob(50))?;
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
        fs::create_dir_all(&tmp).unwrap();
        build_test_tree(&tmp).unwrap();
        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let node = scan_dir(&tmp, 0, &counter, &cancel, &tx).unwrap();
        let (gt_files, gt_folders, gt_size) = ground_truth(&tmp);
        assert_eq!(node.file_count, gt_files);
        assert_eq!(node.folder_count, gt_folders);
        assert_eq!(node.size, gt_size);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_scan_dir_expected_counts() {
        let tmp = tmp_dir("expected");
        fs::create_dir_all(&tmp).unwrap();
        build_test_tree(&tmp).unwrap();
        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let node = scan_dir(&tmp, 0, &counter, &cancel, &tx).unwrap();
        assert_eq!(node.file_count, 6);
        assert_eq!(node.folder_count, 4);
        assert_eq!(node.size, 210);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_scan_dir_empty_dir() {
        let tmp = tmp_dir("empty");
        fs::create_dir_all(&tmp).unwrap();
        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let node = scan_dir(&tmp, 0, &counter, &cancel, &tx).unwrap();
        assert_eq!(node.file_count, 0);
        assert_eq!(node.folder_count, 0);
        assert_eq!(node.size, 0);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_scan_dir_deep_nesting() {
        let tmp = tmp_dir("deep");
        fs::create_dir_all(&tmp).unwrap();
        let mut cur: PathBuf = tmp.clone();
        for i in 0..10 {
            cur = cur.join(format!("d{}", i));
            fs::create_dir_all(&cur).unwrap();
        }
        fs::write(cur.join("leaf.txt"), "hello").unwrap();
        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let node = scan_dir(&tmp, 0, &counter, &cancel, &tx).unwrap();
        assert_eq!(node.file_count, 1);
        assert_eq!(node.folder_count, 10);
        assert_eq!(node.size, 5);
        let _ = fs::remove_dir_all(&tmp);
    }

    /// 回归测试：5000 文件不截断（验证没有 MAX_ENTRIES 上限）
    #[test]
    fn test_scan_dir_no_truncation() {
        let tmp = tmp_dir("no_trunc");
        fs::create_dir_all(&tmp).unwrap();
        for i in 0..500 {
            let dir = tmp.join(format!("d{}", i));
            fs::create_dir_all(&dir).unwrap();
            for j in 0..10 {
                fs::write(dir.join(format!("f{}.txt", j)), vec![b'x'; 100]).unwrap();
            }
        }
        let counter = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let node = scan_dir(&tmp, 0, &counter, &cancel, &tx).unwrap();
        let (gt_files, gt_folders, gt_size) = ground_truth(&tmp);
        assert_eq!(node.file_count, gt_files, "文件数应和真值一致（不截断）");
        assert_eq!(node.folder_count, gt_folders);
        assert_eq!(node.size, gt_size);
        assert!(counter.load(Ordering::Relaxed) >= 5000);
        let _ = fs::remove_dir_all(&tmp);
    }

    /// 真实系统目录端到端测试
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
        let node = scan_dir(target, 0, &counter, &cancel, &tx).unwrap();
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
