use tilo_ui_base::widget::{keyboard_entry_rank, Widget};

/// An editor whose whole accessibility subtree (a node per block of a long
/// note) is too costly to build only to read its role.
struct Editor;

impl Widget for Editor {
    crate::testing::inert_widget_methods!(leaf_layout, name = "Editor");
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        panic!("reading the keyboard entry built the editor's whole subtree")
    }
    fn accessibility_root(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        Some(tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Document))
    }
}

#[test]
fn the_keyboard_entry_reads_a_controls_own_node() {
    assert_eq!(keyboard_entry_rank(&Editor), Some(1));
}
