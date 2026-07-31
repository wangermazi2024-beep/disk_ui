//! CSV 导出 — 和列表列完全一致。
//!
//! 列顺序：路径 | 名称 | 父占比 | 总占比 | 逻辑大小 | 修改时间 | 物理大小 | 创建时间 | 访问时间
//!         | 项目 | 文件 | 文件夹 | 属性 | 重解析点 | 保留 | 所有者

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use crate::format::{format_attributes, format_filetime_local as format_filetime};
use crate::model::Node;

fn join_path(parent: &str, name: &str) -> String {
    if parent.ends_with('\\') { format!("{parent}{name}") } else { format!("{parent}\\{name}") }
}

/// 导出整棵树到 CSV。返回 (文件数, 文件夹数)。
pub fn export_tree_csv(root: &Node, root_path: &str, out_path: &Path) -> io::Result<(u64, u64)> {
    let mut f = File::create(out_path)?;
    f.write_all(&[0xEF, 0xBB, 0xBF])?; // UTF-8 BOM
    writeln!(f, "路径,名称,父占比(%),总占比(%),逻辑大小,修改时间,物理大小,创建时间,访问时间,项目,文件,文件夹,属性,重解析点,保留,所有者")?;

    let mut file_count = 0u64;
    let mut folder_count = 0u64;
    let disk_logical = root.logical_size.max(1);

    // 根节点
    write_row(&mut f, root_path, root, 1.0, 1.0, root.logical_size, disk_logical)?;
    walk(root, root_path, &mut f, &mut file_count, &mut folder_count, disk_logical)?;
    Ok((file_count, folder_count))
}

fn write_row(
    f: &mut File, path: &str, node: &Node,
    parent_pct: f64, total_pct: f64,
    _parent_size: u64, _disk_logical: u64,
) -> io::Result<()> {
    let items = if node.is_folder() { node.file_count + node.folder_count } else { 0 };
    let files = if node.is_folder() { node.file_count } else { 0 };
    let folders = if node.is_folder() { node.folder_count } else { 0 };
    let modified = if node.modified_ft > 0 { format_filetime(node.modified_ft) } else { String::new() };
    let created = if node.created_ft > 0 { format_filetime(node.created_ft) } else { String::new() };
    let accessed = if node.accessed_ft > 0 { format_filetime(node.accessed_ft) } else { String::new() };
    let reparse = if node.reparse_tag != 0 { format!("0x{:X}", node.reparse_tag) } else { String::new() };
    let reserved = if node.is_reserved { "是" } else { "" };
    let owner = &node.owner;
    let attrs = format_attributes(node.attributes);

    // CSV 转义：含逗号的字段用双引号包裹，内部双引号转义为两个
    let esc = |s: &str| -> String {
        if s.contains(',') || s.contains('"') || s.contains('\n') {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else { s.to_string() }
    };

    writeln!(f, "{},{},{:.2},{:.2},{},{},{},{},{},{},{},{},{},{},{},{}",
        esc(path), esc(&node.name),
        parent_pct * 100.0, total_pct * 100.0,
        node.logical_size, esc(&modified),
        node.physical_size, esc(&created), esc(&accessed),
        items, files, folders,
        esc(&attrs), esc(&reparse), reserved, esc(owner),
    )?;
    Ok(())
}

fn walk(
    node: &Node, cur_path: &str, f: &mut File,
    file_count: &mut u64, folder_count: &mut u64,
    disk_logical: u64,
) -> io::Result<()> {
    let parent_size = node.logical_size.max(1);
    for child in &node.children {
        let child_path = join_path(cur_path, &child.name);
        let parent_pct = if parent_size > 0 { child.logical_size as f64 / parent_size as f64 } else { 0.0 };
        let total_pct = if disk_logical > 0 { child.logical_size as f64 / disk_logical as f64 } else { 0.0 };
        write_row(f, &child_path, child, parent_pct, total_pct, parent_size, disk_logical)?;
        if child.is_folder() {
            *folder_count += 1;
            walk(child, &child_path, f, file_count, folder_count, disk_logical)?;
        } else {
            *file_count += 1;
        }
    }
    Ok(())
}
