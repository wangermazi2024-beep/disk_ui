//! 核心数据模型：递归的文件/文件夹树。

use egui::Color32;
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum NodeKind {
    File,
    Folder,
}

pub type NodePath = Vec<usize>;

#[derive(Clone)]
pub struct Node {
    pub name: String,
    pub size: u64,
    pub kind: NodeKind,
    pub color: Color32,
    pub children: Vec<Node>,
    pub expanded: bool,
    /// 该节点包含的文件数量（自身不算，仅子项中文件数；目录为子树总和）
    pub file_count: u64,
    /// 该节点包含的文件夹数量（自身不算，仅子项中文件夹数）
    pub folder_count: u64,
    /// 最近修改时间（unix epoch nanos, 0 = 未知）
    pub modified: u64,
    /// Windows 文件属性标记（FILE_ATTRIBUTE_*）
    pub attributes: u32,
}

impl Node {
    pub fn new_folder(
        name: impl Into<String>,
        color: Color32,
        children: Vec<Node>,
    ) -> Self {
        let size = children.iter().map(|c| c.size).sum();
        let file_count = children.iter().map(|c| c.file_count + if c.is_file() { 1 } else { 0 }).sum::<u64>();
        let folder_count = children.iter().map(|c| c.folder_count + if c.is_folder() { 1 } else { 0 }).sum::<u64>();
        Self {
            name: name.into(),
            size,
            kind: NodeKind::Folder,
            color,
            children,
            expanded: false,
            file_count,
            folder_count,
            modified: 0,
            attributes: 0,
        }
    }

    pub fn new_file(
        name: impl Into<String>,
        size: u64,
        color: Color32,
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
            modified: 0,
            attributes: 0,
        }
    }

    pub fn new_file_full(
        name: impl Into<String>,
        size: u64,
        modified: u64,
        attributes: u32,
        color: Color32,
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
            modified,
            attributes,
        }
    }

    pub fn is_file(&self) -> bool {
        matches!(self.kind, NodeKind::File)
    }

    pub fn is_folder(&self) -> bool {
        matches!(self.kind, NodeKind::Folder)
    }

    pub fn navigate(&self, path: &[usize]) -> Option<&Node> {
        let mut cur = self;
        for &i in path {
            cur = cur.children.get(i)?;
        }
        Some(cur)
    }

    pub fn collapse_all(&mut self) {
        self.expanded = false;
        for c in &mut self.children {
            c.collapse_all();
        }
    }

    pub fn exclusive_toggle(&mut self, path: &[usize]) -> bool {
        if path.is_empty() { return false; }
        if path.len() == 1 {
            let target_idx = path[0];
            let was_expanded = self.children.get(target_idx)
                .map(|n| n.expanded).unwrap_or(false);
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
        } else { false }
    }
}

/// 文件类型分类统计。
#[derive(Clone)]
pub struct CategoryStat {
    pub label: &'static str,
    pub size: u64,
    pub color: Color32,
}
