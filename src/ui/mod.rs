pub mod sidebar;
pub mod topbar;
pub mod tree_list;
pub mod treemap_view;

use crate::model::NodePath;

/// treemap 色块和文件列表树两种视图，产生的用户操作在语义上是一样的：
/// 选中一个节点 / 原地展开下一层 / 放大导航到某个节点。
/// 用同一个枚举承载，`app.rs` 里就只需要一处分支处理逻辑，
/// 不用给两个视图各写一遍重复的状态更新代码。
///
/// 这里的 `NodePath` 一律是"从真实根节点出发"的绝对路径,
/// 两个视图内部各自负责把相对路径换算成绝对路径再返回。
#[derive(Debug, Clone)]
pub enum TreeAction {
    None,
    Select(NodePath),
    ToggleExpand(NodePath),
    ZoomTo(NodePath),
}

impl TreeAction {
    pub fn or(self, other: TreeAction) -> TreeAction {
        match self {
            TreeAction::None => other,
            _ => self,
        }
    }
}
