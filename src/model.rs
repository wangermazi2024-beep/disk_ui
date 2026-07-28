//! 核心数据模型：递归的文件/文件夹树。
//!
//! 这是这次扩展的关键改动：原来的 `FileNode` 是一个"扁平"的单层列表，
//! 现在换成了带 `children` 的树结构，treemap 色块递归、文件列表树递归展开，
//! 都建立在同一份数据模型之上，避免出现"色块一套数据、列表又一套数据"的重复。

use egui::Color32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    File,
    Folder,
}

/// 树节点在树中的位置，用"从根节点出发的子节点下标序列"表示。
/// 例如 `[]` 是根节点自己，`[2, 0]` 是根节点第 3 个孩子的第 1 个孩子。
///
/// 用路径而不是指针/引用当 ID，是为了避开 Rust 里"边遍历树边可变修改树"
/// 的借用检查难题：UI 每帧只需要算出"点了哪条路径"，再统一按路径去改状态。
pub type NodePath = Vec<usize>;

#[derive(Clone)]
pub struct Node {
    pub name: String,
    pub size: u64,
    pub kind: NodeKind,
    pub color: Color32,
    pub children: Vec<Node>,

    /// UI 状态：这个色块是否已经"在原地展开下一层"（单击的效果）。
    /// 是否绘制 children 的嵌套子色块，由这个字段控制，
    /// 而不是无脑一次性画到底——文件树可能有几十万个节点，
    /// 不加这道开关，treemap 会直接卡死。
    pub expanded: bool,
}

impl Node {
    pub fn new_folder(name: impl Into<String>, color: Color32, children: Vec<Node>) -> Self {
        let size = children.iter().map(|c| c.size).sum();
        Self {
            name: name.into(),
            size,
            kind: NodeKind::Folder,
            color,
            children,
            expanded: false,
        }
    }

    pub fn new_file(name: impl Into<String>, size: u64, color: Color32) -> Self {
        Self {
            name: name.into(),
            size,
            kind: NodeKind::File,
            color,
            children: Vec::new(),
            expanded: false,
        }
    }

    pub fn is_folder(&self) -> bool {
        matches!(self.kind, NodeKind::Folder)
    }

    /// 按路径只读导航到子节点；路径非法时返回 None（而不是 panic），
    /// 因为扫描线程随时可能替换掉整棵树，UI 侧保存的旧路径可能已经失效。
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

    /// 递归清空所有节点的 `expanded` 标记（比如换根扫描之后需要重置 UI 展开状态）。
    pub fn collapse_all(&mut self) {
        self.expanded = false;
        for c in &mut self.children {
            c.collapse_all();
        }
    }
}

/// 文件类型分类统计（对应 WizTree/TreeSize 的"按类型统计"视图）。
#[derive(Clone)]
pub struct CategoryStat {
    pub label: &'static str,
    pub size: u64,
    pub color: Color32,
}
