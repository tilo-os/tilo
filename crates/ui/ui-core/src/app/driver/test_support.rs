//! Helpers shared by application tests that drive a root on the display-less [`Driver`].

use super::Driver;
use tilo_ui_base::a11y::AccessNode;
use crate::headless::InspectionNode;

/// Every semantic node of the current frame, paired with the inspection path of the widget that
/// owns it (the owner [`Driver::perform_semantic_named`] routes to).
#[must_use]
pub fn semantic_nodes(driver: &Driver) -> Vec<(String, AccessNode)> {
    fn access(node: &AccessNode, owner: &str, out: &mut Vec<(String, AccessNode)>) {
        out.push((owner.to_owned(), node.clone()));
        for child in &node.children {
            access(child, owner, out);
        }
    }
    fn widget(node: &InspectionNode, out: &mut Vec<(String, AccessNode)>) {
        if let Some(semantics) = &node.semantics {
            access(semantics, &node.path, out);
        }
        for child in &node.children {
            widget(child, out);
        }
    }
    let mut out = Vec::new();
    widget(&driver.inspection().root, &mut out);
    out
}
