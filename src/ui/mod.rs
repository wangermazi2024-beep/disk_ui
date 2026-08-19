pub mod compact_tree;
pub mod sidebar;
pub mod startup;
pub mod topbar;
pub mod tree_list;

use crate::model::NodePath;

#[derive(Debug, Clone)]
pub enum TreeAction {
    None,
    Select(NodePath),
    ToggleExpand(NodePath),
    #[allow(dead_code)]
    EnterNode(NodePath),
    /// 右键菜单点了"删除到回收站"：只是发出请求，真正执行前 app.rs 要弹确认框。
    /// 带上名称/完整路径/是否文件夹，是因为确认框要展示这些信息，而 app.rs
    /// 侧不方便（也没必要）重新从 abs_path 沿树走一遍去拿。
    RequestDelete { abs_path: NodePath, name: String, full_path: String, is_folder: bool },
    /// 右键菜单点了"检测占用"：查一下这个文件/文件夹当前有没有被别的进程/
    /// 服务占用。这是纯只读查询（不会像删除那样需要弹确认框），app.rs 收到
    /// 之后直接查、弹一个结果窗口。
    RequestCheckLock { abs_path: NodePath, name: String, full_path: String, is_folder: bool },
}

/// 主列表（tree_list）可排序的字段。父占比/总占比两列本质上和逻辑大小同序
/// （同一层级内 parent_logical 相同，全树内 disk_logical 也相同），
/// 所以这两列点击时都映射到 `Size`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Physical,
    Created,
    Accessed,
    Items,
    Files,
    Folders,
    Attributes,
    Reparse,
    Reserved,
    Owner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    fn toggled(self) -> Self {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }
}

/// 当前排序状态：排哪一列 + 升/降序。默认和原来"构建时排序"的规则一致
/// （按逻辑大小降序），保证不带排序状态的旧行为不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub key: SortKey,
    pub dir: SortDir,
}

impl Default for SortState {
    fn default() -> Self {
        Self { key: SortKey::Size, dir: SortDir::Desc }
    }
}

impl SortState {
    /// 表头被点击：点同一列切换方向；点新列换到新列，默认降序
    /// （体积/时间/数量类列一般更想先看"最大/最新"的，降序更符合直觉）。
    pub fn click(&mut self, key: SortKey) {
        if self.key == key {
            self.dir = self.dir.toggled();
        } else {
            self.key = key;
            self.dir = SortDir::Desc;
        }
    }
}
