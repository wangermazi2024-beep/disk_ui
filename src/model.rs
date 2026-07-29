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
    ///
    /// 这样同一层永远只有一个节点展开，和 SpaceSniffer 行为一致。
    ///
    /// `path`：目标节点相对于 self 的路径（不含 view_root 的 base_path）。
    /// 返回 true 表示节点被展开，false 表示被折叠。
    pub fn exclusive_toggle(&mut self, path: &[usize]) -> bool {
        if path.is_empty() {
            return false;
        }

        if path.len() == 1 {
            // 目标节点就在 self.children[path[0]]
            let target_idx = path[0];
            // 记录目标节点当前状态
            let was_expanded = self.children.get(target_idx)
                .map(|n| n.expanded)
                .unwrap_or(false);

            // 先把同层所有兄弟（包括目标）全部折叠
            for child in &mut self.children {
                child.collapse_all();
            }

            // 如果之前是折叠的，现在展开目标
            if !was_expanded {
                if let Some(target) = self.children.get_mut(target_idx) {
                    target.expanded = true;
                    return true;
                }
            }
            return false;
        }

        // 递归到父节点
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
