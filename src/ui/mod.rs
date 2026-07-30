pub mod sidebar;
pub mod topbar;
pub mod tree_list;

use crate::model::NodePath;

#[derive(Debug, Clone)]
pub enum TreeAction {
    None,
    Select(NodePath),
    ToggleExpand(NodePath),
    #[allow(dead_code)] // 预留：双击进入文件夹功能尚未接入
    EnterNode(NodePath),
}
