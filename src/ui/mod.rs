pub mod sidebar;
pub mod topbar;
pub mod tree_list;

use crate::model::NodePath;

/// treemap 色块和文件列表树两种视图，产生的用户操作在语义上是一样的：
/// 选中一个节点 / 原地展开下一层 / 双击进入某个节点 / 面包屑跳转。
/// 用同一个枚举承载，`app.rs` 里就只需要一处分支处理逻辑，
/// 不用给两个视图各写一遍重复的状态更新代码。
///
/// 这里的 `NodePath` 一律是"从真正的根节点出发"的绝对路径——
/// 即使 treemap 当前只显示某个子目录（`view_path` 非空），
/// 它构造的路径也是从真正根节点算起的绝对路径，这样才能和
/// 文件列表树（永远从真正根节点展示完整树）共用同一个 `selected`。
#[derive(Debug, Clone)]
pub enum TreeAction {
    None,
    Select(NodePath),
    ToggleExpand(NodePath),
    /// 双击某个节点：把它的父节点作为新的“当前视图根”。
    /// 这里携带的是被双击节点自身的绝对路径，`app.rs` 会取其父路径
    /// 赋给 `view_path`——这只是切换"当前展示哪一段"的视图状态，
    /// 不会修改、复制或丢弃树里的任何数据。
    EnterNode(NodePath),
    /// 直接跳转到某个绝对路径作为新的"当前视图根"（面包屑 / "上级目录"按钮用）。
    NavigateTo(NodePath),
}

impl TreeAction {
    /// 合并动作：保留第一个非 None 的动作（后面的不覆盖前面）。
    pub fn merge(&mut self, other: TreeAction) {
        if matches!(*self, TreeAction::None) && !matches!(other, TreeAction::None) {
            *self = other;
        }
    }
}
