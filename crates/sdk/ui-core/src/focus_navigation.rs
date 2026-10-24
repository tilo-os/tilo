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

/// Whether `widget` and what it holds stay out of the focus order: the
/// `hidden` flag of its accessibility node, read through
/// [`Widget::accessibility_hidden`] so traversal does not build each node
/// (and resolve its translated name) for one flag. Debug builds build the
/// node anyway and check that the two agree.
pub(crate) fn hidden(widget: &dyn Widget) -> bool {
    let hidden = widget.accessibility_hidden();
    debug_assert_eq!(
        hidden,
        widget
            .accessibility_root()
            .is_some_and(|node| node.state.hidden),
        "{}: Widget::accessibility_hidden disagrees with the hidden flag of its accessibility node",
        widget.name(),
    );
    hidden
}
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

/// Whether any widget [`order`] would visit traps focus: when none does,
/// `order` reports no trap without its walk having to run.
pub(crate) fn any_trap(widget: &dyn Widget) -> bool {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return false;
    };
    if widget.traps_focus() {
        return true;
    }
    let mut found = false;
    widget.for_each_child(&mut |child| found = found || any_trap(child));
    found
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

/// The same deepest-trap policy through an admitted composition map. History
/// and component roots add layout nodes that are not Widget children.
pub(crate) fn mapped_order(
    root: NodeId,
    taffy: &TaffyTree<()>,
    inspect: impl Fn(NodeId) -> (bool, bool, bool),
) -> (Vec<NodeId>, bool) {
    fn trap(
        node: NodeId,
        taffy: &TaffyTree<()>,
        inspect: &mut impl FnMut(NodeId) -> (bool, bool, bool),
    ) -> Option<NodeId> {
        let (_, traps, hidden) = inspect(node);
        if hidden {
            return None;
        }
        for child in taffy.children(node).unwrap_or_default() {
            if let Some(found) = trap(child, taffy, inspect) {
                return Some(found);
            }
        }
        traps.then_some(node)
    }
    // Trap discovery and order collection describe one immutable composition.
    // Inspect a node once even when both walks reach it.
    let mut inspected = std::collections::HashMap::new();
    let mut inspect_once = |node| *inspected.entry(node).or_insert_with(|| inspect(node));
    let trapped = trap(root, taffy, &mut inspect_once);
    let mut pending = vec![trapped.unwrap_or(root)];
    let mut order = Vec::new();
    while let Some(node) = pending.pop() {
        let (focusable, _, hidden) = inspect_once(node);
        if hidden {
            continue;
        }
        if focusable {
            order.push(node);
        }
        pending.extend(taffy.children(node).unwrap_or_default().into_iter().rev());
    }
    (order, trapped.is_some())
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
        // Focus goes back to where it was before the trap, unless what the
        // trap chose moved it somewhere that outlives the trap (a menu's
        // "Rename" focusing the name it opened for editing).
        (true, false) => {
            let before = before.take();
            if focused.is_some_and(|id| order.contains(&id) && Some(id) != before) {
                None
            } else {
                Some(before.filter(|id| order.contains(id)))
            }
        }
        (true, true) if !focused.is_some_and(|id| order.contains(&id)) => {
            Some(order.first().copied())
        }
        _ => None,
    };
    *active = trapped;
    change
}
/// Keyboard travel (Tab, Shift+Tab, directional focus) outside a trap stays
/// inside the laid-out keyboard scope ([`Widget::scopes_keyboard_focus`]):
/// `order` narrowed to the scope's subtree, empty when the scope holds no
/// control (travel then moves nowhere rather than onto the chrome around
/// it). Without a scope `order` is returned as it is.
pub(crate) fn within_keyboard_scope(
    order: Vec<NodeId>,
    scopes: &[NodeId],
    taffy: &TaffyTree<()>,
) -> Vec<NodeId> {
    let Some(&scope) = scopes.first() else {
        return order;
    };
    order
        .into_iter()
        .filter(|&node| is_within(node, scope, taffy))
        .collect()
}

/// Whether a component boundary (an app hosted in a tile, as a desktop's
/// windows host theirs) sits below `widget`. A component's root lays out
/// nodes that are not its boundary's widget children, so the widget walk
/// of [`order`] cannot reach its controls: focus order then walks the
/// admitted layout map ([`mapped_order`]) instead.
pub(crate) fn hosts_components(widget: &dyn Widget) -> bool {
    let mut found = false;
    widget.for_each_child(&mut |child| {
        found = found || child.callback_binding().is_some() || hosts_components(child);
    });
    found
}

/// Whether a host orders focus over its admitted layout map
/// ([`layout_map_order`]) rather than its widget tree ([`order`]): History,
/// embedded or the host's own, and a component hosted below the root each
/// lay out nodes the widget walk cannot reach. The native window and the
/// frame harness both decide here, so they never order focus apart.
pub(crate) fn walks_layout_map(
    root: &dyn Widget,
    history: bool,
    restrictions: &crate::history_mode::embedded::FocusRestrictions,
) -> bool {
    history || restrictions.has_surfaces() || hosts_components(root)
}

/// What the focus walk asks of one node: whether it takes focus, whether it
/// traps it, and whether it is hidden from assistive technology (which
/// hides its subtree from focus too). Reads the node's own `hidden` flag
/// ([`hidden`]), never its accessibility node or the subtree a document
/// exports.
pub(crate) fn probe(widget: &dyn Widget) -> (bool, bool, bool) {
    (
        widget.is_focusable() && widget.is_enabled(),
        widget.traps_focus(),
        hidden(widget),
    )
}

/// Focus order over the admitted layout map from `root`, for the native
/// window and the frame harness alike: embedded History's restrictions
/// apply, while the host's History captures input only its nodes take
/// focus, and otherwise they come after the app's. `inspect` answers
/// [`probe`] for a mapped node (`None` when it is not mapped). Returns the
/// order and whether something traps focus (an open History review does).
pub(crate) fn layout_map_order(
    root: NodeId,
    taffy: &TaffyTree<()>,
    restrictions: &crate::history_mode::embedded::FocusRestrictions,
    history: Option<&crate::history_mode::host::HistoryHost>,
    inspect: impl Fn(NodeId) -> Option<(bool, bool, bool)>,
) -> (Vec<NodeId>, bool) {
    use tilo_sort::CompactSort as _;
    // History's owned nodes, walked once for this recomputation: both the
    // capture filter and the History-last sort ask it of every node.
    let owned = history
        .map(|history| history.owned_nodes(taffy))
        .unwrap_or_default();
    let captures = history.is_some_and(crate::history_mode::host::HistoryHost::captures_input);
    let (mut order, trapped) = mapped_order(root, taffy, |node| {
        let allowed = restrictions.allows(node) && (!captures || owned.contains(&node));
        let (focusable, traps, hidden) = inspect(node).unwrap_or_default();
        (allowed && focusable, allowed && traps, hidden)
    });
    if !captures {
        order.compact_sort_by_key(|id| owned.contains(id));
    }
    (
        order,
        trapped || history.is_some_and(crate::history_mode::host::HistoryHost::review_is_open),
    )
}

fn is_within(mut node: NodeId, scope: NodeId, taffy: &TaffyTree<()>) -> bool {
    loop {
        if node == scope {
            return true;
        }
        match taffy.parent(node) {
            Some(parent) => node = parent,
            None => return false,
        }
    }
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

#[cfg(test)]
mod trap_tests {
    use super::*;

    #[test]
    fn history_focus_order_inspects_each_node_once() {
        let mut tree = TaffyTree::new();
        let children: Vec<_> = (0..2_000)
            .map(|_| tree.new_leaf(taffy::Style::default()).unwrap())
            .collect();
        let root = tree
            .new_with_children(taffy::Style::default(), &children)
            .unwrap();
        let calls = std::cell::RefCell::new(std::collections::HashMap::new());
        let (order, trapped) = mapped_order(root, &tree, |node| {
            *calls.borrow_mut().entry(node).or_insert(0) += 1;
            (node != root, false, false)
        });
        assert_eq!(order, children, "focus order stays in layout tree order");
        assert!(!trapped);
        let calls = calls.borrow();
        assert_eq!(calls.len(), children.len() + 1);
        assert!(
            calls.values().all(|&count| count == 1),
            "one recomputation must inspect each node once: {} calls for {} nodes",
            calls.values().sum::<usize>(),
            calls.len(),
        );
    }

    #[test]
    fn mapped_history_focus_order_keeps_nested_traps_and_hidden_subtrees() {
        let mut tree = TaffyTree::new();
        let first = tree.new_leaf(taffy::Style::default()).unwrap();
        let second = tree.new_leaf(taffy::Style::default()).unwrap();
        let inner = tree
            .new_with_children(taffy::Style::default(), &[first, second])
            .unwrap();
        let outer = tree
            .new_with_children(taffy::Style::default(), &[inner])
            .unwrap();
        let hidden_trap = tree.new_leaf(taffy::Style::default()).unwrap();
        let hidden = tree
            .new_with_children(taffy::Style::default(), &[hidden_trap])
            .unwrap();
        let later_trap = tree.new_leaf(taffy::Style::default()).unwrap();
        let root = tree
            .new_with_children(taffy::Style::default(), &[hidden, outer, later_trap])
            .unwrap();
        let visited = std::cell::RefCell::new(Vec::new());
        let (order, trapped) = mapped_order(root, &tree, |node| {
            visited.borrow_mut().push(node);
            (
                node == first || node == second || node == later_trap,
                node == outer || node == inner || node == hidden_trap || node == later_trap,
                node == hidden,
            )
        });
        assert!(trapped);
        assert_eq!(order, [first, second]);
        assert!(
            !visited.borrow().contains(&hidden_trap),
            "a hidden ancestor prunes even a nested trap",
        );
    }

    #[test]
    fn a_closing_trap_restores_focus_unless_its_choice_moved_it() {
        let ids: Vec<NodeId> = (0..4_u64).map(NodeId::from).collect();
        let [canvas, label, menu, other] = [ids[0], ids[1], ids[2], ids[3]];
        let page = [canvas, label, other];
        let (mut active, mut before) = (false, None);
        assert_eq!(
            reconcile_trap(&mut active, &mut before, Some(canvas), &[menu], true),
            Some(Some(menu))
        );
        assert_eq!(
            reconcile_trap(&mut active, &mut before, Some(menu), &page, false),
            Some(Some(canvas))
        );
        assert_eq!(
            reconcile_trap(&mut active, &mut before, Some(canvas), &[menu], true),
            Some(Some(menu))
        );
        assert_eq!(
            reconcile_trap(&mut active, &mut before, Some(label), &page, false),
            None
        );
        assert_eq!(before, None);
    }
}

#[cfg(test)]
mod hidden_tests {
    use super::*;
    use crate::widget::command_palette::CommandPalette;
    use crate::widget::popover::Popover;
    use crate::widget::{Column, ContextMenu, ModalDialog, Tooltip};

    /// `widget` answers [`Widget::accessibility_hidden`] as its accessibility
    /// node's `hidden` flag says; returns the flag.
    fn agreed(widget: &dyn Widget) -> bool {
        let hidden = widget.accessibility_hidden();
        assert_eq!(
            hidden,
            widget.accessibility().is_some_and(|node| node.state.hidden),
            "{}",
            widget.name()
        );
        hidden
    }

    #[test]
    fn widgets_that_hide_answer_accessibility_hidden_as_their_node() {
        assert!(agreed(&Tooltip::new("Tip")), "an unrevealed tooltip");

        let mut menu = ContextMenu::new();
        assert!(agreed(&menu), "a closed menu");
        menu.open_at([0.0, 0.0]);
        assert!(!agreed(&menu), "an open menu");

        let mut dialog = ModalDialog::new("Title", Column::new());
        assert!(agreed(&dialog), "a closed dialog");
        dialog.set_open(true);
        assert!(!agreed(&dialog), "an open dialog");

        let mut palette = CommandPalette::new();
        assert!(agreed(&palette), "a closed command palette");
        palette.open();
        assert!(!agreed(&palette), "an open command palette");

        let mut options = Popover::new()
            .with_label("Interval")
            .with_items(["1 s", "2 s"]);
        assert!(agreed(&options), "a closed options menu");
        options.set_open(true);
        assert!(!agreed(&options), "an open options menu");

        let mut panel = Popover::with_content(Column::new()).with_label("Share");
        assert!(agreed(&panel), "a closed labelled panel");
        panel.set_open(true);
        assert!(!agreed(&panel), "an open labelled panel");
        assert!(
            !agreed(&Popover::with_content(Column::new())),
            "an unlabelled panel has no node of its own"
        );

        assert!(!agreed(&Column::new()), "a container is never hidden");
    }
}
