//! 目录扫描。
//!
//! 两部分：
//! 1. `spawn_scan`：真正遍历磁盘的后台线程实现，通过 channel 把进度/结果
//!    发回 UI 线程，避免扫描大目录时把界面卡死。
//! 2. `demo_tree`：跨平台可运行的多层级演示数据（原来 main.rs 里那份写死的
//!    示例数字的"递归版"），在扫描路径不存在（比如非 Windows 环境，或者
//!    还没点"扫描"按钮）时兜底展示，用来演示色块递归/文件树递归的效果。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use egui::Color32;

use crate::model::Node;

pub enum ScanMessage {
    /// 已扫描的文件/文件夹计数，用于顶部进度条粗略展示"正在扫描"。
    Progress(u64),
    Done(Box<Node>),
    Error(String),
}

/// 安全上限：防止在一个异常庞大的目录（比如 `/`）上跑到天荒地老，
/// 超过这个条目数就提前收尾，返回"扫描到目前为止"的结果。
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

/// 在后台线程递归扫描 `root`，通过 `tx` 把结果发回来。
/// 调用者（UI 线程）负责在每一帧 `try_recv` 这个 channel。
pub fn spawn_scan(root: PathBuf, tx: Sender<ScanMessage>) {
    std::thread::spawn(move || {
        let counter = Arc::new(AtomicU64::new(0));
        match scan_dir(&root, 0, &counter, &tx) {
            Ok(node) => {
                let _ = tx.send(ScanMessage::Done(Box::new(node)));
            }
            Err(e) => {
                let _ = tx.send(ScanMessage::Error(format!("扫描失败: {e}")));
            }
        }
    });
}

fn scan_dir(
    path: &Path,
    depth: usize,
    counter: &Arc<AtomicU64>,
    tx: &Sender<ScanMessage>,
) -> std::io::Result<Node> {
    // 根节点（depth==0）显示完整路径（如 "C:\" / "/home"），子目录只显示文件夹名
    let name = if depth == 0 {
        path.to_string_lossy().into_owned()
    } else {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    };

    let mut children = Vec::new();
    let entries = std::fs::read_dir(path)?;

    for entry in entries.flatten() {
        if counter.load(Ordering::Relaxed) > MAX_ENTRIES {
            break;
        }
        // 用 symlink_metadata 而不是 metadata：不跟随符号链接，
        // 避免软链接成环导致递归死循环。
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue, // 权限不足等场景直接跳过，不让一个文件挡住整个扫描
        };
        let entry_name = entry.file_name().to_string_lossy().into_owned();
        let n = counter.fetch_add(1, Ordering::Relaxed);
        if n % 500 == 0 {
            let _ = tx.send(ScanMessage::Progress(n));
        }

        if meta.is_dir() {
            match scan_dir(&entry.path(), depth + 1, counter, tx) {
                Ok(child) => children.push(child),
                Err(_) => continue, // 单个子目录扫描失败（如权限问题）不影响其余部分
            }
        } else {
            children.push(Node::new_file(entry_name, meta.len(), file_color()));
        }
    }

    Ok(Node::new_folder(name, folder_color(depth), children))
}

/// 生成一份多层级的演示数据，结构上和真实扫描结果一致（带 children 递归），
/// 用来在没有真实扫描结果时，展示色块递归展开/文件树递归展开的交互效果。
pub fn demo_tree() -> Node {
    let leaf = |name: &str, size: u64| Node::new_file(name, size, file_color());

    let windows = Node::new_folder(
        "Windows",
        folder_color(0),
        vec![
            Node::new_folder(
                "System32",
                folder_color(1),
                vec![leaf("ntoskrnl.exe", 11_200_000), leaf("kernel32.dll", 780_000), leaf("drivers.cab", 640_000_000)],
            ),
            Node::new_folder(
                "WinSxS",
                folder_color(1),
                vec![leaf("manifest_a.cat", 2_100_000_000), leaf("manifest_b.cat", 1_800_000_000)],
            ),
            leaf("explorer.exe", 5_400_000),
        ],
    );

    let program_files = Node::new_folder(
        "Program Files",
        folder_color(0),
        vec![
            Node::new_folder(
                "Steam",
                folder_color(1),
                vec![
                    Node::new_folder(
                        "steamapps",
                        folder_color(2),
                        vec![
                            Node::new_folder("common", folder_color(3), vec![
                                Node::new_folder("Cyberpunk2077", folder_color(4), vec![leaf("archive.pak", 68_000_000_000)]),
                                Node::new_folder("Elden Ring", folder_color(4), vec![leaf("data.bin", 45_000_000_000)]),
                            ]),
                        ],
                    ),
                ],
            ),
            Node::new_folder(
                "Adobe",
                folder_color(1),
                vec![leaf("Photoshop.exe", 2_300_000_000), leaf("Premiere.exe", 3_100_000_000)],
            ),
        ],
    );

    let users = Node::new_folder(
        "Users",
        folder_color(0),
        vec![Node::new_folder(
            "Alex",
            folder_color(1),
            vec![
                Node::new_folder(
                    "Documents",
                    folder_color(2),
                    vec![leaf("thesis_final_v3.docx", 4_200_000), leaf("budget.xlsx", 900_000)],
                ),
                Node::new_folder(
                    "Downloads",
                    folder_color(2),
                    vec![leaf("big_video.mp4", 7_400_000_000), leaf("installer.msi", 320_000_000)],
                ),
                Node::new_folder(
                    "AppData",
                    folder_color(2),
                    vec![
                        Node::new_folder(
                            "node_modules",
                            folder_color(3),
                            vec![
                                Node::new_folder("react", folder_color(4), vec![leaf("index.js", 1_200_000)]),
                                Node::new_folder("webpack", folder_color(4), vec![leaf("bundle.js", 8_900_000)]),
                                Node::new_folder("lodash", folder_color(4), vec![leaf("lodash.js", 1_500_000)]),
                            ],
                        ),
                        Node::new_folder("Temp", folder_color(3), vec![leaf("cache.tmp", 1_100_000_000)]),
                    ],
                ),
            ],
        )],
    );

    let pagefile = leaf("pagefile.sys", 16_000_000_000);

    Node::new_folder("C:\\", folder_color(usize::MAX), vec![windows, program_files, users, pagefile])
}
