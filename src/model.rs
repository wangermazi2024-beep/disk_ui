//! 核心数据模型：递归的文件/文件夹树（v2 — WinDirStat 风格）。
//!
//! 参考 WinDirStat 的 CItem，每个节点同时保存 Logical Size 和 Physical Size。
//! UI 默认以 Logical Size 为准（和 Explorer / WizTree 一致）。

use egui::Color32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    File,
    Folder,
}

/// 树节点在树中的位置，用"从根节点出发的子节点下标序列"表示。
pub type NodePath = Vec<usize>;

#[derive(Clone)]
pub struct Node {
    pub name: String,
    /// Logical Size（逻辑大小 = Explorer "大小" = $DATA.FileSize）。
    /// UI 默认显示这个。向后兼容字段，等于 `logical_size`。
    pub size: u64,
    /// Logical Size（和 `size` 相同，显式保留方便区分）。
    pub logical_size: u64,
    /// Physical Size（物理大小 = Explorer "占用空间" = $DATA.AllocatedLength/Compressed）。
    pub physical_size: u64,
    pub kind: NodeKind,
    pub color: Color32,
    pub children: Vec<Node>,
    pub expanded: bool,

    pub file_count: u64,
    pub folder_count: u64,
    /// 最后修改时间（FILETIME，1601-01-01 起 100ns）。0=未知。
    pub modified_ft: u64,
    /// 创建时间（FILETIME）。0=未知。
    pub created_ft: u64,
    /// 最后访问时间（FILETIME）。0=未知。
    pub accessed_ft: u64,
    /// Windows 文件属性位（FILE_ATTRIBUTE_*）。
    pub attributes: u32,
    /// Reparse point tag（0=普通文件，IO_REPARSE_TAG_*=reparse point）。
    pub reparse_tag: u32,
    /// 是否是 NTFS 保留系统文件（record < 16，如 $MFT/$LogFile/$Bitmap）。
    pub is_reserved: bool,
    /// 所有者（SID 或用户名，可能为空）。
    pub owner: String,
}

impl Node {
    pub fn new_folder(name: impl Into<String>, color: Color32, children: Vec<Node>) -> Self {
        Self::new_folder_with_meta(name, color, children, 0, 0, 0, 0x10, 0, false, String::new())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_folder_with_meta(
        name: impl Into<String>,
        color: Color32,
        mut children: Vec<Node>,
        modified_ft: u64,
        created_ft: u64,
        accessed_ft: u64,
        attributes: u32,
        reparse_tag: u32,
        is_reserved: bool,
        owner: String,
    ) -> Self {
        // 排序一次：按 logical_size 降序，文件夹优先
        children.sort_by(|a, b| {
            b.logical_size.cmp(&a.logical_size)
                .then_with(|| b.is_folder().cmp(&a.is_folder()))
        });
        let logical_size = children.iter().map(|c| c.logical_size).sum();
        let physical_size = children.iter().map(|c| c.physical_size).sum();
        let file_count = children.iter().map(|c| c.file_count).sum::<u64>()
            + children.iter().filter(|c| c.is_file()).count() as u64;
        let folder_count = children.iter().map(|c| c.folder_count).sum::<u64>()
            + children.iter().filter(|c| c.is_folder()).count() as u64;
        let modified_ft = modified_ft.max(children.iter().map(|c| c.modified_ft).max().unwrap_or(0));
        let attributes = if attributes == 0 { 0x10 } else { attributes };
        Self {
            name: name.into(),
            size: logical_size,
            logical_size,
            physical_size,
            kind: NodeKind::Folder,
            color,
            children,
            expanded: false,
            file_count,
            folder_count,
            modified_ft,
            created_ft,
            accessed_ft,
            attributes,
            reparse_tag,
            is_reserved,
            owner,
        }
    }

    pub fn new_file(name: impl Into<String>, logical: u64, color: Color32) -> Self {
        Self::new_file_with_meta(name, logical, logical, color, 0, 0, 0, 0x80, 0, false, String::new())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_file_with_meta(
        name: impl Into<String>,
        logical_size: u64,
        physical_size: u64,
        color: Color32,
        modified_ft: u64,
        created_ft: u64,
        accessed_ft: u64,
        attributes: u32,
        reparse_tag: u32,
        is_reserved: bool,
        owner: String,
    ) -> Self {
        Self {
            name: name.into(),
            size: logical_size,
            logical_size,
            physical_size,
            kind: NodeKind::File,
            color,
            children: Vec::new(),
            expanded: false,
            file_count: 0,
            folder_count: 0,
            modified_ft,
            created_ft,
            accessed_ft,
            attributes: if attributes == 0 { 0x80 } else { attributes },
            reparse_tag,
            is_reserved,
            owner,
        }
    }

    pub fn is_folder(&self) -> bool {
        matches!(self.kind, NodeKind::Folder)
    }

    /// 是否带有"隐藏"或"系统"属性（FILE_ATTRIBUTE_HIDDEN=0x02 / FILE_ATTRIBUTE_SYSTEM=0x04）。
    /// Windows 资源管理器对这类项目的做法是图标和文字都做半透明/淡化处理，用来提示"这是隐藏项"，
    /// 而不是直接不显示——我们扫描器本来就没有 Explorer 那个"隐藏文件"开关的过滤逻辑，
    /// 所有文件都会显示，所以用同样的"淡化"视觉提示替代"完全不显示"。
    pub fn is_hidden_or_system(&self) -> bool {
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x02;
        const FILE_ATTRIBUTE_SYSTEM: u32 = 0x04;
        self.attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
    }

    pub fn is_file(&self) -> bool {
        matches!(self.kind, NodeKind::File)
    }

    pub fn navigate(&self, path: &[usize]) -> Option<&Node> {
        let mut cur = self;
        for &i in path {
            cur = cur.children.get(i)?;
        }
        Some(cur)
    }

    pub fn navigate_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        let mut cur = self;
        for &i in path {
            cur = cur.children.get_mut(i)?;
        }
        Some(cur)
    }

    pub fn collapse_all(&mut self) {
        // 迭代版本：和 mft_scan.rs 的 populate_owners 用同一套写法——栈里存相对 NodePath，
        // 每次用 navigate_mut 重新定位，不持有多个 &mut Node 引用，也不用原生递归。
        let mut stack: Vec<Vec<usize>> = vec![Vec::new()];
        while let Some(rel) = stack.pop() {
            let Some(cur) = (if rel.is_empty() { Some(&mut *self) } else { self.navigate_mut(&rel) }) else { continue };
            cur.expanded = false;
            for i in 0..cur.children.len() {
                let mut child_rel = rel.clone();
                child_rel.push(i);
                stack.push(child_rel);
            }
        }
    }

    pub fn exclusive_toggle(&mut self, path: &[usize]) -> bool {
        if path.is_empty() {
            return false;
        }
        // 迭代地走到 path 指向的父节点（除最后一段外都只是导航，和原递归版
        // "path.len()>1 时只是往下一层再调自己"完全等价，只是不再用调用栈）。
        let mut cur = self;
        for &i in &path[..path.len() - 1] {
            match cur.children.get_mut(i) {
                Some(next) => cur = next,
                None => return false,
            }
        }
        let target_idx = path[path.len() - 1];
        let was_expanded = cur.children.get(target_idx).map(|n| n.expanded).unwrap_or(false);
        for child in &mut cur.children {
            child.collapse_all();
        }
        if !was_expanded {
            if let Some(target) = cur.children.get_mut(target_idx) {
                target.expanded = true;
                return true;
            }
        }
        false
    }
}

#[derive(Clone)]
pub struct CategoryStat {
    pub label: &'static str,
    pub size: u64,
    pub color: Color32,
}
