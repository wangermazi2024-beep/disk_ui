pub mod sidebar;
pub mod topbar;
pub mod tree_list;
pub mod treemap_view;

use crate::model::NodePath;

/// treemap 色块和文件列表树两种视图，产生的用户操作在语义上是一样的：
/// 选中一个节点 / 原地展开下一层 / 双击进入某个节点。
/// 用同一个枚举承载，`app.rs` 里就只需要一处分支处理逻辑，
/// 不用给两个视图各写一遍重复的状态更新代码。
///
/// 这里的 `NodePath` 一律是"从当前根节点出发"的路径,
/// 两个视图内部各自负责把相对路径换算好再返回。
#[derive(Debug, Clone)]
pub enum TreeAction {
    None,
    Select(NodePath),
    ToggleExpand(NodePath),
    /// 双击某个节点：把它的父节点提升为新的根节点（数据结构变化，不是视图缩放）。
    /// 提升后根到父节点只剩一层，父节点原有的孩子（含被双击节点及其兄弟）不变。
    EnterNode(NodePath),
}

impl TreeAction {
    /// 合并动作：保留第一个非 None 的动作（后面的不覆盖前面）。
    pub fn merge(&mut self, other: TreeAction) {
        if matches!(*self, TreeAction::None) && !matches!(other, TreeAction::None) {
            *self = other;
        }
    }
}
