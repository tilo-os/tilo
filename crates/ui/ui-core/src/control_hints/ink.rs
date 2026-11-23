//! Where a window's content shows text or a control: what a hint's card laid over the window must
//! not cover. Read from what the content tells assistive technology, which names exactly the text
//! a person reads and the controls they use, each with its bounds.

use crate::a11y::{AccessNode, Role};
use crate::widget::Widget;
use vello::kurbo::Rect;

/// The most places one window's content is read for: content that shows more counts whole.
const MOST: usize = 4096;

/// Every place `root`'s content shows text or a control, in its layout coordinates; `None` when
/// it shows more than can be read for a hint.
pub(crate) fn of(root: &dyn Widget) -> Option<Vec<Rect>> {
    let mut out = Vec::new();
    walk(root, &mut out).then_some(out)
}

fn walk(widget: &dyn Widget, out: &mut Vec<Rect>) -> bool {
    let children = widget.children();
    if let Some(node) = widget.accessibility() {
        node_ink(&node, children.is_empty(), out);
    }
    if out.len() > MOST {
        return false;
    }
    children
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !widget.accessibility_represents_child(*index))
        .all(|(_, child)| walk(child, out))
}

/// What `node` and the nodes under it show. A container (a pane, a list, a toolbar) is only where
/// its parts are; a part with no parts of its own is ink whole: a line of text, a button, a field,
/// an image, an editor. `leaf` says the widget the node is from has no widgets under it.
fn node_ink(node: &AccessNode, leaf: bool, out: &mut Vec<Rect>) {
    let shows_words = node.name.is_some() || node.description.is_some() || node.value.is_some();
    let ink = !container(node.role) || (leaf && node.children.is_empty() && shows_words);
    if ink && let Some(bounds) = node.bounds {
        let rect = bounds.to_rect();
        if !rect.is_zero_area() {
            out.push(rect);
        }
    }
    for child in &node.children {
        node_ink(child, true, out);
    }
}

/// Roles that only hold other parts.
const fn container(role: Role) -> bool {
    matches!(
        role,
        Role::Window
            | Role::Dialog
            | Role::Group
            | Role::Generic
            | Role::Toolbar
            | Role::TabBar
            | Role::TabPanel
            | Role::List
            | Role::Table
            | Role::Tree
            | Role::Menu
            | Role::ListBox
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::AccessNode;

    #[test]
    fn a_pane_is_ink_only_where_its_text_and_controls_are() {
        let pane = AccessNode::new(Role::Group)
            .with_name("Messages")
            .with_bounds(Rect::new(0.0, 0.0, 600.0, 800.0))
            .with_children(vec![
                AccessNode::new(Role::Heading { level: 2 })
                    .with_name("No messages yet")
                    .with_bounds(Rect::new(200.0, 380.0, 400.0, 404.0)),
                AccessNode::new(Role::Button)
                    .with_name("Send")
                    .with_bounds(Rect::new(540.0, 740.0, 580.0, 780.0)),
            ]);
        let mut out = Vec::new();
        node_ink(&pane, false, &mut out);
        assert_eq!(
            out,
            vec![
                Rect::new(200.0, 380.0, 400.0, 404.0),
                Rect::new(540.0, 740.0, 580.0, 780.0)
            ],
            "the pane itself is no ink"
        );
        let notice = AccessNode::new(Role::Group)
            .with_name("Nothing here")
            .with_bounds(Rect::new(0.0, 0.0, 600.0, 800.0));
        let mut out = Vec::new();
        node_ink(&notice, true, &mut out);
        assert_eq!(out.len(), 1, "a group that says something itself is ink");
    }
}
