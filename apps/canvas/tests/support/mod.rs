//! Locate the real drawn control through its accessibility bounds.
use ui::vello::kurbo::Rect;

pub fn named_bounds(node: &ui::headless::InspectionNode, name: &str) -> Option<Rect> {
    fn semantic(node: &ui::a11y::AccessNode, name: &str) -> Option<Rect> {
        if node.name.as_deref() == Some(name) {
            return node.bounds.map(ui::a11y::AccessBounds::to_rect);
        }
        node.children.iter().find_map(|child| semantic(child, name))
    }
    node.semantics
        .as_ref()
        .and_then(|node| semantic(node, name))
        .or_else(|| {
            node.children
                .iter()
                .find_map(|child| named_bounds(child, name))
        })
}
