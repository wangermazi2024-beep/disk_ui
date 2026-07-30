pub mod sidebar;
pub mod topbar;
pub mod tree_list;

use crate::model::NodePath;

#[derive(Debug, Clone)]
pub enum TreeAction {
    None,
    Select(NodePath),
    ToggleExpand(NodePath),
    EnterNode(NodePath),
}
