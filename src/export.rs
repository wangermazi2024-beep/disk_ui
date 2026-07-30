//! 把扫描结果（`Node` 树）导出成 CSV，方便和 WizTree 的导出结果做逐行对比，
//! 定位到底是哪些具体文件/文件夹丢了、或者大小算错了。
//!
//! 格式故意做得跟 WizTree 导出的 CSV 尽量像：一行一个文件/文件夹，带完整路径
//! 和大小，这样可以直接用 Excel/脚本按路径 VLOOKUP 对比两份表。

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::model::Node;

/// 手动拼路径字符串，而不是用 `PathBuf::join`。
///
/// **关键原因**：`std::path::PathBuf` 在 Windows 下对"以句点/空格结尾"的路径
/// 分量处理不可靠（这类分量在 Win32 层面本来就是被 `GetFullPathNameW` 之类的
/// API 特殊对待的边角情况，NTFS 底层允许，但路径库这一层会把结尾的点/空格
/// 悄悄吃掉）。这份导出只是纯文本 CSV，不需要真的操作文件系统路径语义，
/// 用字符串拼接可以完整保留原始文件名（比如 `92b.` 这种结尾带点的合法文件名），
/// 之前用 `PathBuf::join` + `.display()` 时就是在这一步把结尾的 `.` 弄丢的——
/// 扫描引擎本身拿到的名字和大小其实都是对的，只是导出这步把名字写错了。
fn join_path(parent: &str, name: &str) -> String {
    if parent.ends_with('\\') {
        format!("{parent}{name}")
    } else {
        format!("{parent}\\{name}")
    }
}

/// 导出整棵树到 `out_path`。`root_path` 是这棵树对应的盘符根路径（比如 `"C:\\"`），
/// 用来拼出每一行的完整路径——`Node` 本身只存相对的子节点名字，不存完整路径。
///
/// 返回 `(导出的文件数, 导出的文件夹数)`，跟 UI 上显示的"总扫描数"对一下账，
/// 确认导出没有中途漏行。
pub fn export_tree_csv(root: &Node, root_path: &str, out_path: &Path) -> io::Result<(u64, u64)> {
    let mut f = File::create(out_path)?;
    // UTF-8 BOM，跟 WizTree 导出的编码一致，Excel 打开中文路径不会乱码。
    f.write_all(&[0xEF, 0xBB, 0xBF])?;
    writeln!(f, "路径,大小,类型")?;

    let mut file_count = 0u64;
    let mut folder_count = 0u64;
    let base = root_path.to_string();

    // 根节点自己也写一行（大小=整个树汇总，方便跟 WizTree CSV 里 "C:\" 那一行对比）。
    write_row(&mut f, &base, root.size, root.is_folder())?;
    walk(root, &base, &mut f, &mut file_count, &mut folder_count)?;

    Ok((file_count, folder_count))
}

fn write_row(f: &mut File, path: &str, size: u64, is_folder: bool) -> io::Result<()> {
    let path_str = path.replace('"', "\"\"");
    let kind = if is_folder { "D" } else { "F" };
    writeln!(f, "\"{path_str}\",{size},{kind}")
}

fn walk(
    node: &Node,
    cur_path: &str,
    f: &mut File,
    file_count: &mut u64,
    folder_count: &mut u64,
) -> io::Result<()> {
    for child in &node.children {
        let child_path = join_path(cur_path, &child.name);
        write_row(f, &child_path, child.size, child.is_folder())?;
        if child.is_folder() {
            *folder_count += 1;
            walk(child, &child_path, f, file_count, folder_count)?;
        } else {
            *file_count += 1;
        }
    }
    Ok(())
}

/// 导出文件默认放的位置：优先桌面（`%USERPROFILE%\Desktop`），拿不到就放当前目录。
/// 文件名带时间戳，多次导出不会互相覆盖，方便留多份跟不同时间点的 WizTree 导出对比。
pub fn default_export_path(drive_letter: char) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let filename = format!("DiskLens_export_{drive_letter}_{ts}.csv");
    let desktop = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|p| p.join("Desktop"))
        .filter(|p| p.is_dir());
    match desktop {
        Some(dir) => dir.join(filename),
        None => PathBuf::from(filename),
    }
}
