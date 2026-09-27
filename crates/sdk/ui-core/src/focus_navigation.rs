//! Spatial focus policy shared by native and portable hosts.
use taffy::NodeId;
use vello::kurbo::Rect;
use winit::keyboard::NamedKey;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Direction {
    Up,
    Down,
    Left,
    Right,
}
impl Direction {
    pub(crate) fn from_key(key: NamedKey) -> Option<Self> {
        match key {
            NamedKey::ArrowUp => Some(Self::Up),
            NamedKey::ArrowDown => Some(Self::Down),
            NamedKey::ArrowLeft => Some(Self::Left),
            NamedKey::ArrowRight => Some(Self::Right),
            _ => None,
        }
    }
}
/// Keep tree order for ties, as the native host always has.
pub(crate) fn next(
    current: (NodeId, Rect),
    candidates: impl IntoIterator<Item = (NodeId, Rect)>,
    direction: Direction,
) -> Option<NodeId> {
    let center = current.1.center();
    let mut best = None;
    let mut best_score = f64::MAX;
    for (id, rect) in candidates {
        if id == current.0 {
            continue;
        }
        let delta = rect.center() - center;
        let (primary, cross, eligible) = match direction {
            Direction::Up => (-delta.y, delta.x.abs(), delta.y < -1.0),
            Direction::Down => (delta.y, delta.x.abs(), delta.y > 1.0),
            Direction::Left => (-delta.x, delta.y.abs(), delta.x < -1.0),
            Direction::Right => (delta.x, delta.y.abs(), delta.x > 1.0),
        };
        let score = primary + cross * 2.0;
        if eligible && score < best_score {
            best_score = score;
            best = Some(id);
        }
    }
    best
}

use crate::widget::Widget;
use taffy::TaffyTree;
fn collect_focusable(
    node_id: NodeId,
    widget: &dyn Widget,
    taffy: &TaffyTree<()>,
    order: &mut Vec<NodeId>,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return;
    };
    if widget.is_focusable() && widget.is_enabled() {
        order.push(node_id);
    }
    let children = widget.children();
    if let Ok(child_nodes) = taffy.children(node_id) {
        for (child_widget, &child_node) in children.iter().zip(child_nodes.iter()) {
            collect_focusable(child_node, *child_widget, taffy, order);
        }
    }
}
// The deepest trapping widget wins (a modal opened from within one).
fn find_trap_focusables(
    node_id: NodeId,
    widget: &dyn Widget,
    taffy: &TaffyTree<()>,
) -> Option<Vec<NodeId>> {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return None;
    };
    let children = widget.children();
    let child_nodes = taffy.children(node_id).ok();
    // Recurse first so a nested trap takes precedence.
    if let Some(child_nodes) = &child_nodes {
        for (child_widget, &child_node) in children.iter().zip(child_nodes.iter()) {
            if let Some(inner) = find_trap_focusables(child_node, *child_widget, taffy) {
                return Some(inner);
            }
        }
    }
    if widget.traps_focus() {
        let mut order = Vec::new();
        collect_focusable(node_id, widget, taffy, &mut order);
        return Some(order);
    }
    None
}

/// The deepest trapping subtree owns the keyboard order.
pub(crate) fn order(
    root: NodeId,
    widget: &dyn Widget,
    taffy: &TaffyTree<()>,
) -> (Vec<NodeId>, bool) {
    if let Some(order) = find_trap_focusables(root, widget, taffy) {
        return (order, true);
    }
    let mut order = vec![];
    collect_focusable(root, widget, taffy, &mut order);
    (order, false)
}
/// None means keep focus; Some(None) means blur all controls.
#[expect(
    clippy::option_option,
    reason = "the outer Option is whether focus changes; the inner one is the new, possibly empty, focus"
)]
pub(crate) fn reconcile_trap(
    active: &mut bool,
    before: &mut Option<NodeId>,
    focused: Option<NodeId>,
    order: &[NodeId],
    trapped: bool,
) -> Option<Option<NodeId>> {
    let change = match (*active, trapped) {
        (false, true) => {
            *before = focused;
            Some(order.first().copied())
        }
        (true, false) => Some(before.take().filter(|id| order.contains(id))),
        (true, true) if !focused.is_some_and(|id| order.contains(&id)) => {
            Some(order.first().copied())
        }
        _ => None,
    };
    *active = trapped;
    change
}
pub(crate) fn keyboard_nav_allowed(modal: Option<Rect>, trapped: bool) -> bool {
    modal.is_none() || trapped
}
pub(crate) fn tab_index(order: &[NodeId], focused: Option<NodeId>, back: bool) -> Option<usize> {
    if order.is_empty() {
        return None;
    }
    Some(
        match focused.and_then(|id| order.iter().position(|&n| n == id)) {
            Some(0) if back => order.len() - 1,
            Some(i) if back => i - 1,
            Some(i) => (i + 1) % order.len(),
            None => 0,
        },
    )
}
