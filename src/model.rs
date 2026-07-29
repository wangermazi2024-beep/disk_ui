//! 核心数据模型：递归的文件/文件夹树。

use egui::Color32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    File,
    Folder,
}

/// 树节点在树中的位置，用"从根节点出发的子节点下标序列"表示。
/// 例如 `[]` 是根节点自己，`[2, 0]` 是根节点第 3 个孩子的第 1 个孩子。
pub type NodePath = Vec<usize>;

#[derive(Clone)]
pub struct Node {
    pub name: String,
    pub size: u64,
    pub kind: NodeKind,
    pub color: Color32,
    pub children: Vec<Node>,

    /// 该色块是否已展开显示子层（单击的效果）。
    pub expanded: bool,

    /// 递归子树里的文件数（不含自己）。文件夹节点才有意义；文件节点恒为 0。
    pub file_count: u64,
    /// 递归子树里的文件夹数（不含自己）。文件夹节点才有意义；文件节点恒为 0。
    pub folder_count: u64,
    /// 最后修改时间，用 Windows FILETIME 格式（1601-01-01 起 100ns 单位）。
    /// 0 表示未知。在非 Windows 平台上也会被填进去（用 SystemTime 折算），
    /// 这样 UI 层格式化逻辑只有一份。
    pub modified_ft: u64,
    /// Windows 文件属性位（`FILE_ATTRIBUTE_*`）。
    /// 文件夹默认带 `0x10` (DIRECTORY)，文件默认带 `0x80` (NORMAL)。
    pub attributes: u32,
}

impl Node {
    /// 用子节点列表构造一个文件夹节点；大小、文件数、文件夹数会自动汇总。
    /// 修改时间取子节点里最大的；属性默认为 DIRECTORY。
    pub fn new_folder(name: impl Into<String>, color: Color32, children: Vec<Node>) -> Self {
        let size = children.iter().map(|c| c.size).sum();
        let file_count = children.iter().map(|c| c.file_count).sum::<u64>()
            + children.iter().filter(|c| c.is_file()).count() as u64;
        let folder_count = children.iter().map(|c| c.folder_count).sum::<u64>()
            + children.iter().filter(|c| c.is_folder()).count() as u64;
        let modified_ft = children.iter().map(|c| c.modified_ft).max().unwrap_or(0);
        Self {
            name: name.into(),
            size,
            kind: NodeKind::Folder,
            color,
            children,
            expanded: false,
            file_count,
            folder_count,
            modified_ft,
            attributes: 0x10, // FILE_ATTRIBUTE_DIRECTORY
        }
    }

    /// 同上，但允许显式指定本节点自身的修改时间和属性。
    /// 文件夹的"修改时间"在 NTFS 上指的是该目录自身的 LastModificationTime
    ///（不是子节点里的最大值），所以扫描时单独拿到后用这个构造器覆盖。
    pub fn new_folder_with_meta(
        name: impl Into<String>,
        color: Color32,
        children: Vec<Node>,
        modified_ft: u64,
        attributes: u32,
    ) -> Self {
        let mut node = Self::new_folder(name, color, children);
        node.modified_ft = modified_ft.max(node.modified_ft);
        node.attributes = if attributes == 0 { 0x10 } else { attributes };
        node
    }

    pub fn new_file(name: impl Into<String>, size: u64, color: Color32) -> Self {
        Self::new_file_with_meta(name, size, color, 0, 0x80)
    }

    pub fn new_file_with_meta(
        name: impl Into<String>,
        size: u64,
        color: Color32,
        modified_ft: u64,
        attributes: u32,
    ) -> Self {
        Self {
            name: name.into(),
            size,
            kind: NodeKind::File,
            color,
            children: Vec::new(),
            expanded: false,
            file_count: 0,
            folder_count: 0,
            modified_ft,
            attributes: if attributes == 0 { 0x80 } else { attributes },
        }
    }

    pub fn is_folder(&self) -> bool {
        matches!(self.kind, NodeKind::Folder)
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

    /// 递归清空所有节点的展开标记。
    pub fn collapse_all(&mut self) {
        self.expanded = false;
        for c in &mut self.children {
            c.collapse_all();
        }
    }

    /// SpaceSniffer 风格的「独占展开」：
    ///
    /// 单击文件夹 X 时：
    /// 1. 把 X 所在层（父节点的所有 children）全部 collapse_all。
    /// 2. 如果 X 之前是折叠的，展开 X（toggle：如果已展开则折叠）。
    /// 3. X 子树内部的旧展开状态也一并清除（换了视图就重置）。
    pub fn exclusive_toggle(&mut self, path: &[usize]) -> bool {
        if path.is_empty() {
            return false;
        }

        if path.len() == 1 {
            let target_idx = path[0];
            let was_expanded = self
                .children
                .get(target_idx)
                .map(|n| n.expanded)
                .unwrap_or(false);

            for child in &mut self.children {
                child.collapse_all();
            }

            if !was_expanded {
                if let Some(target) = self.children.get_mut(target_idx) {
                    target.expanded = true;
                    return true;
                }
            }
            return false;
        }

        let next = path[0];
        if let Some(child) = self.children.get_mut(next) {
            child.exclusive_toggle(&path[1..])
        } else {
            false
        }
    }
}

/// 文件类型分类统计。
#[derive(Clone)]
pub struct CategoryStat {
    pub label: &'static str,
    pub size: u64,
    pub color: Color32,
}
