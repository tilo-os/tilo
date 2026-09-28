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
use std::collections::HashMap;
use taffy::TaffyTree;

/// The layout node each retained widget registered. A container's
/// [`Widget::children`] need not mirror its layout children: an app may wrap
/// its panes in layout-only rows, or leave a hidden child out of the layout.
/// Pairing the two by position would give a child another node's id, and
/// focus on the real control would then fall outside the order.
pub(crate) struct LayoutNodes {
    nodes: HashMap<*const (), Vec<(NodeId, &'static str)>>,
}

impl LayoutNodes {
    /// # Safety
    ///
    /// Every pointer in `node_map` must point to a live widget for the
    /// duration of this call: the map the host built with the layout it
    /// still holds.
    pub(crate) unsafe fn new<S: std::hash::BuildHasher>(
        node_map: &HashMap<NodeId, *mut dyn Widget, S>,
    ) -> Self {
        let mut nodes: HashMap<*const (), Vec<(NodeId, &'static str)>> = HashMap::new();
        for (&node, &pointer) in node_map {
            // SAFETY: the caller guarantees the pointer is live; reading the
            // concrete type retains no borrow.
            let concrete = unsafe { (&*pointer).concrete_type_name() };
            nodes
                .entry(pointer.cast::<()>().cast_const())
                .or_default()
                .push((node, concrete));
        }
        Self { nodes }
    }

    /// A struct shares its address with its first field, so an address that
    /// two registered widgets share is told apart by concrete type; one that
    /// stays ambiguous has no node here.
    fn get(&self, widget: &dyn Widget) -> Option<NodeId> {
        let entries = self.nodes.get(&std::ptr::from_ref(widget).cast::<()>())?;
        if let [(node, _)] = entries.as_slice() {
            return Some(*node);
        }
        let concrete = widget.concrete_type_name();
        let mut matches = entries.iter().filter(|(_, name)| *name == concrete);
        let (node, _) = matches.next()?;
        matches.next().is_none().then_some(*node)
    }
}

/// `widget`'s children with the layout node each one stands at: the node it
/// registered under `node_id`, else the layout child at its position. A
/// registered node elsewhere (another tree a component lays out itself) is
/// not this layout's.
fn child_nodes<'a>(
    node_id: NodeId,
    widget: &'a dyn Widget,
    taffy: &TaffyTree<()>,
    layout: Option<&LayoutNodes>,
) -> Vec<(&'a dyn Widget, NodeId)> {
    let positional = taffy.children(node_id).unwrap_or_default();
    let mut descendants: Option<std::collections::HashSet<NodeId>> = None;
    widget
        .children()
        .into_iter()
        .enumerate()
        .filter_map(|(index, child)| {
            let at_position = positional.get(index).copied();
            let registered = layout.and_then(|layout| layout.get(child)).filter(|node| {
                Some(*node) == at_position
                    || descendants
                        .get_or_insert_with(|| descendants_of(node_id, taffy))
                        .contains(node)
            });
            registered.or(at_position).map(|node| (child, node))
        })
        .collect()
}

fn descendants_of(node_id: NodeId, taffy: &TaffyTree<()>) -> std::collections::HashSet<NodeId> {
    let mut found = std::collections::HashSet::new();
    let mut pending = taffy.children(node_id).unwrap_or_default();
    while let Some(node) = pending.pop() {
        if found.insert(node) {
            pending.extend(taffy.children(node).unwrap_or_default());
        }
    }
    found
}

fn collect_focusable(
    node_id: NodeId,
    widget: &dyn Widget,
    taffy: &TaffyTree<()>,
    layout: Option<&LayoutNodes>,
    order: &mut Vec<NodeId>,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return;
    };
    if widget.is_focusable() && widget.is_enabled() {
        order.push(node_id);
    }
    for (child_widget, child_node) in child_nodes(node_id, widget, taffy, layout) {
        collect_focusable(child_node, child_widget, taffy, layout, order);
    }
}
// The deepest trapping widget wins (a modal opened from within one).
fn find_trap_focusables(
    node_id: NodeId,
    widget: &dyn Widget,
    taffy: &TaffyTree<()>,
    layout: Option<&LayoutNodes>,
) -> Option<Vec<NodeId>> {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return None;
    };
    // Recurse first so a nested trap takes precedence.
    for (child_widget, child_node) in child_nodes(node_id, widget, taffy, layout) {
        if let Some(inner) = find_trap_focusables(child_node, child_widget, taffy, layout) {
            return Some(inner);
        }
    }
    if widget.traps_focus() {
        let mut order = Vec::new();
        collect_focusable(node_id, widget, taffy, layout, &mut order);
        return Some(order);
    }
    None
}

/// The deepest trapping subtree owns the keyboard order. `layout` names the
/// node each widget registered; without it a child is taken to stand at the
/// layout child in its position.
pub(crate) fn order(
    root: NodeId,
    widget: &dyn Widget,
    taffy: &TaffyTree<()>,
    layout: Option<&LayoutNodes>,
) -> (Vec<NodeId>, bool) {
    if let Some(order) = find_trap_focusables(root, widget, taffy, layout) {
        return (order, true);
    }
    let mut order = vec![];
    collect_focusable(root, widget, taffy, layout, &mut order);
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
