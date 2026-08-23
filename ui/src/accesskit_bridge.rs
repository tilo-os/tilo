//! AccessKit bridge.
//!
//! This module turns the toolkit's per-widget semantic model
//! ([`crate::a11y::AccessNode`], exposed via
//! [`Widget::accessibility`](crate::widget::Widget::accessibility)) into an
//! [`accesskit::TreeUpdate`]. That `TreeUpdate` is what
//! [`accesskit_winit::Adapter`] hands to `accesskit_unix`, which publishes it
//! over **AT-SPI2** so standard assistive technologies (Orca, braille via
//! BrlTTY) read every `ui` app with zero qOS-specific knowledge.
//!
//! ## Design
//!
//! The single load-bearing entry point is [`build_tree_update`] — a **pure
//! function** of `(root_widget, node_map, focused)`. It performs a depth-first
//! walk of the widget tree, and for every widget that returns
//! `Some(AccessNode)` it emits one `accesskit::Node`. Presentational widgets
//! (those returning `None` from `accessibility()`) are **collapsed**: their
//! accessible children are reparented onto the nearest accessible ancestor,
//! exactly mirroring ARIA `role="presentation"`. This keeps the exported tree
//! free of "group, group, group" noise from layout-only `Row`/`Column`/
//! `Container`/`Stack`/`Spacer` widgets.
//!
//! Because it is pure and takes the widget tree + the same `node_map`
//! (`taffy::NodeId -> *mut dyn Widget`) the `App` already maintains, it is
//! **unit-testable without a live AT-SPI bus or a winit window** — the tests at
//! the bottom build a small tree and assert on roles, hierarchy, focus, bounds,
//! and an action round-trip.
//!
//! ## Stable ids
//!
//! AccessKit needs a stable [`accesskit::NodeId`] per node so incremental
//! updates and focus events line up frame-to-frame. Three disjoint id spaces
//! share the `u64`:
//!
//! | space | derivation | bit 63 |
//! |---|---|---|
//! | root | [`ROOT_ID`] (`0`) | 0 |
//! | widget | `taffy::NodeId + 1` (`0` reserved for the root) | 0 |
//! | widget, never laid out | `(1 << 48) + walk_index` | 0 |
//! | synthetic | hash of the owner's id chained with each [`AccessNode::key`] | **1** |
//!
//! Synthetic ids set the top bit, so [`is_synthetic_id`] tells the three apart
//! with no lookup, and a widget id can never be mistaken for one.
//!
//! ## Canvas widgets: focus and actions on synthetic nodes
//!
//! A widget that paints its own contents — a timeline of clips, a spreadsheet
//! grid — has no child widgets to expose, so it *synthesises* its accessible
//! structure through [`AccessNode::children`]. Two things make those nodes
//! first-class rather than merely readable:
//!
//! * **Focus.** The widget marks one descendant
//!   [`focused`](crate::a11y::AccessState::focused); when that widget is the one
//!   holding keyboard focus, [`build_tree_update`] points the tree's `focus` at
//!   *that* node instead of the widget. This is exactly ARIA's
//!   `aria-activedescendant`: the widget owns the keyboard, the marked node is
//!   where the user is. Keying it off the app's real focus is deliberate — an
//!   unfocused widget cannot steal the reader's cursor by setting a flag.
//! * **Actions.** [`resolve_node_id`] inverts a synthetic id back to
//!   `(owning widget, key path)` so an incoming `ActionRequest` reaches the
//!   clip the user was on, via
//!   [`Widget::perform_action_at`](crate::widget::Widget::perform_action_at).
//!   Inversion re-walks the live tree rather than caching an index, so a stale
//!   id from a previous frame resolves to `None` instead of to the wrong clip.
//!
//! ## Coordinate space
//!
//! [`Widget::get_rect`](crate::widget::Widget::get_rect) returns **logical**
//! coordinates (the `App` applies the scale-factor transform at composite
//! time). AccessKit wants bounds in the tree container's coordinate space; we
//! emit logical coords and let the adapter/`set_root_window_bounds` carry the
//! window placement. `build_tree_update` takes an optional `scale` so a caller
//! that wants physical pixels can pass the window scale factor; the default
//! (`1.0`) keeps logical coords, which is what the unit tests assert on.

use std::collections::HashMap;

use accesskit::{
    Action as AkAction, Node as AkNode, NodeId as AkNodeId, Rect as AkRect, Role as AkRole,
    Toggled, Tree, TreeUpdate,
};
use taffy::NodeId as TaffyNodeId;

use crate::a11y::{AccessNode, Action, FnvHasher, Role};
use crate::widget::Widget;

/// The AccessKit id of the synthetic window/root node. Reserved so no widget
/// can collide with it: widget ids are derived from their (non-zero in
/// practice, but we offset regardless) taffy node id + 1, and the structural
/// fallback ids are large offsets — see [`widget_node_id`].
pub const ROOT_ID: AkNodeId = AkNodeId(0);

/// Map our compact [`a11y::Role`](Role) onto AccessKit's far richer role set.
/// We keep the toolkit surface small and map liberally:
/// e.g. our [`Role::Spinner`] becomes a busy [`AkRole::ProgressIndicator`] and
/// our [`Role::ToggleButton`] becomes a [`AkRole::Button`] carrying the
/// `toggled` state. Heading depth rides along as the AccessKit heading level
/// (set by the caller, not here, since `AkRole::Heading` carries no level in
/// the enum itself).
pub fn map_role(role: Role) -> AkRole {
    match role {
        // Structural / containers
        Role::Window => AkRole::Window,
        Role::Dialog => AkRole::Dialog,
        Role::Group => AkRole::Group,
        Role::Generic => AkRole::GenericContainer,
        Role::Toolbar => AkRole::Toolbar,
        Role::TabBar => AkRole::TabList,
        Role::Tab => AkRole::Tab,
        Role::TabPanel => AkRole::TabPanel,
        Role::List => AkRole::List,
        Role::ListItem => AkRole::ListItem,
        Role::Table => AkRole::Table,
        Role::Row => AkRole::Row,
        Role::Cell => AkRole::Cell,
        Role::Tree => AkRole::Tree,
        Role::TreeItem => AkRole::TreeItem,
        Role::Menu => AkRole::Menu,
        Role::MenuItem => AkRole::MenuItem,

        // Controls
        Role::Button => AkRole::Button,
        // No dedicated "toggle button" role; a Button with `toggled` state is
        // the AccessKit idiom (the state is applied in `apply_state`).
        Role::ToggleButton => AkRole::Button,
        Role::Checkbox => AkRole::CheckBox,
        Role::RadioButton => AkRole::RadioButton,
        Role::Switch => AkRole::Switch,
        Role::Slider => AkRole::Slider,
        Role::SpinButton => AkRole::SpinButton,
        Role::ComboBox => AkRole::ComboBox,
        Role::TextInput => AkRole::TextInput,
        Role::MultilineTextInput => AkRole::MultilineTextInput,
        Role::Link => AkRole::Link,
        Role::ProgressBar => AkRole::ProgressIndicator,
        // A spinner is an indeterminate progress indicator (busy flag applied
        // in `apply_state`).
        Role::Spinner => AkRole::ProgressIndicator,
        Role::Tooltip => AkRole::Tooltip,

        // Content
        Role::Image => AkRole::Image,
        Role::Label => AkRole::Label,
        Role::StaticText => AkRole::Label,
        Role::Heading { .. } => AkRole::Heading,
        Role::Paragraph => AkRole::Paragraph,
        Role::Document => AkRole::Document,
        Role::Canvas => AkRole::Canvas,
        Role::Unknown => AkRole::Unknown,
    }
}

/// Map our [`a11y::Action`](Action) onto an [`accesskit::Action`]. Our
/// `ToggleCheck` has no dedicated AccessKit verb — toggling a checkbox/switch
/// is a `Click` at the platform level — so it maps to [`AkAction::Click`].
/// `SetValue`/`SetSelection` carry a payload in our enum; that payload travels
/// in the `ActionData` on the round trip ([`accesskit_action_to_ours`]), so the
/// forward map here just names the verb.
pub fn map_action(action: &Action) -> AkAction {
    match action {
        Action::Activate => AkAction::Click,
        Action::Focus => AkAction::Focus,
        Action::ToggleCheck => AkAction::Click,
        Action::Increment => AkAction::Increment,
        Action::Decrement => AkAction::Decrement,
        Action::Expand => AkAction::Expand,
        Action::Collapse => AkAction::Collapse,
        Action::SetValue(_) => AkAction::SetValue,
        Action::SetSelection { .. } => AkAction::SetTextSelection,
        Action::ScrollIntoView => AkAction::ScrollIntoView,
        Action::ShowContextMenu => AkAction::ShowContextMenu,
    }
}

/// Inverse of [`map_action`] for an incoming
/// [`accesskit::ActionRequest`](accesskit::ActionRequest): turn the AccessKit
/// verb (+ optional [`accesskit::ActionData`]) back into our [`Action`] so the
/// `App` can dispatch it via [`Widget::perform_action`]. Returns `None` for
/// AccessKit actions we don't model (scroll-by-direction, custom actions, …).
pub fn accesskit_action_to_ours(
    action: AkAction,
    data: Option<&accesskit::ActionData>,
) -> Option<Action> {
    use accesskit::ActionData;
    Some(match action {
        AkAction::Click => Action::Activate,
        AkAction::Focus => Action::Focus,
        AkAction::Increment => Action::Increment,
        AkAction::Decrement => Action::Decrement,
        AkAction::Expand => Action::Expand,
        AkAction::Collapse => Action::Collapse,
        AkAction::ScrollIntoView => Action::ScrollIntoView,
        AkAction::ShowContextMenu => Action::ShowContextMenu,
        AkAction::SetValue => match data {
            Some(ActionData::Value(v)) => Action::SetValue(v.to_string()),
            _ => Action::SetValue(String::new()),
        },
        AkAction::SetTextSelection => match data {
            Some(ActionData::SetTextSelection(sel)) => Action::SetSelection {
                anchor: sel.anchor.character_index,
                focus: sel.focus.character_index,
            },
            _ => return None,
        },
        _ => return None,
    })
}

/// Derive the stable AccessKit id for a widget. Preference order:
///   1. its `taffy::NodeId` (a `u64`) offset by 1 so it can never be
///      [`ROOT_ID`] (taffy ids start at 0);
///   2. a structural walk index offset into a high range, for widgets that
///      were never laid out (no entry in `node_map`).
fn widget_node_id(taffy_id: Option<TaffyNodeId>, walk_index: u64) -> AkNodeId {
    match taffy_id {
        Some(id) => AkNodeId(u64::from(id) + 1),
        // 1<<48 keeps fallback ids well clear of any realistic taffy id range.
        None => AkNodeId((1u64 << 48) + walk_index),
    }
}

/// Reverse of the `App`'s `taffy::NodeId -> *mut dyn Widget` map: given a
/// `&dyn Widget` encountered during the walk, which taffy node is it?
///
/// Matching on the raw `*const dyn Widget` alone is not enough. A widget
/// registers itself in `build_layout` as `self as *mut dyn Widget`, and the walk
/// sees it through a `&dyn Widget` coerced somewhere else entirely; those are
/// *fat* pointers, and Rust does not promise that two coercions of the same type
/// carry the same vtable address — for a generic widget like
/// [`DataTable`](crate::widget::DataTable) they routinely don't. A fat-pointer
/// lookup then misses, the widget silently falls back to a structural id, and
/// everything hanging off it (its synthetic rows, and any action aimed at them)
/// becomes unaddressable.
///
/// So match on the **data address**, which uniquely identifies a live object,
/// and keep the fat-pointer map as the precise first pass. The one case a data
/// address cannot separate is a widget stored inline as another widget's first
/// field, where parent and child share an address; that is recorded as
/// ambiguous and resolved only if the fat pointers happen to disambiguate it,
/// rather than guessing and handing two nodes the same id.
struct WidgetIndex {
    by_fat: HashMap<*const dyn Widget, TaffyNodeId>,
    /// `None` = two or more widgets share this address; do not guess.
    by_addr: HashMap<*const (), Option<TaffyNodeId>>,
}

impl WidgetIndex {
    fn build(node_map: &HashMap<TaffyNodeId, *mut dyn Widget>) -> Self {
        let mut by_fat: HashMap<*const dyn Widget, TaffyNodeId> = HashMap::new();
        let mut by_addr: HashMap<*const (), Option<TaffyNodeId>> = HashMap::new();
        for (&taffy_id, &w_ptr) in node_map {
            let fat = w_ptr as *const dyn Widget;
            by_fat.insert(fat, taffy_id);
            by_addr
                .entry(fat.cast::<()>())
                .and_modify(|slot| {
                    if *slot != Some(taffy_id) {
                        *slot = None;
                    }
                })
                .or_insert(Some(taffy_id));
        }
        Self { by_fat, by_addr }
    }

    fn taffy_id_of(&self, w: &dyn Widget) -> Option<TaffyNodeId> {
        let fat = w as *const dyn Widget;
        if let Some(&id) = self.by_fat.get(&fat) {
            return Some(id);
        }
        self.by_addr.get(&fat.cast::<()>()).copied().flatten()
    }
}

/// Bit that marks an [`accesskit::NodeId`] as belonging to a **synthetic**
/// node (one with no backing [`Widget`]). Widget ids are `taffy_id + 1` or
/// `(1 << 48) + index`, neither of which can reach bit 63 for any tree that
/// fits in memory, so the two spaces cannot collide.
pub const SYNTHETIC_ID_BIT: u64 = 1u64 << 63;

/// Whether `id` addresses a synthetic node rather than a widget or the root.
pub fn is_synthetic_id(id: AkNodeId) -> bool {
    id.0 & SYNTHETIC_ID_BIT != 0
}

/// Derive a synthetic node's stable id from its parent's id and its own key.
///
/// Chaining through the parent means the id encodes the whole path from the
/// owning widget down, so two rows that both hold a cell keyed `0` still get
/// distinct cell ids, and a node's id changes only when something on its path
/// changes.
fn synthetic_node_id(parent: AkNodeId, key: u64) -> AkNodeId {
    use std::hash::Hasher;
    let mut h = FnvHasher::new();
    h.write(&parent.0.to_le_bytes());
    h.write(&key.to_le_bytes());
    // Force the marker bit; everything else is hash.
    AkNodeId(h.finish() | SYNTHETIC_ID_BIT)
}

/// Where an [`accesskit::NodeId`] points in the live widget tree. Produced by
/// [`resolve_node_id`], which is how an incoming `ActionRequest` is turned back
/// into something the app can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum A11yTarget {
    /// The synthetic window root — no widget owns it.
    Root,
    /// A real widget, addressed by the `taffy::NodeId` it was laid out with.
    Widget(TaffyNodeId),
    /// A synthetic node inside `owner`'s [`AccessNode::children`], addressed by
    /// the key path from `owner` down to it (see [`AccessNode::key`]). The path
    /// has one element per level: `[row]` for a table row, `[row, cell]` for a
    /// cell inside it.
    Synthetic {
        /// The widget that synthesised the node.
        owner: TaffyNodeId,
        /// Keys from `owner`'s direct synthetic child down to the target.
        key_path: Vec<u64>,
    },
}

/// Invert an [`accesskit::NodeId`] back to the thing it addresses, by
/// re-deriving ids over the **current** widget tree.
///
/// Returns `None` when the id belongs to no node this frame — a reference an
/// assistive technology kept across a structural change. Callers must treat
/// that as "gone", never as a fallback to some other node: silently retargeting
/// a stale id is how an AT ends up deleting the wrong clip.
///
/// A synthetic node owned by a widget that was never laid out (no entry in
/// `node_map`) is not addressable, and also resolves to `None` — such a widget
/// has no stable handle for the app to dispatch against.
pub fn resolve_node_id(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget>,
    id: AkNodeId,
) -> Option<A11yTarget> {
    if id == ROOT_ID {
        return Some(A11yTarget::Root);
    }
    if !is_synthetic_id(id) {
        // Widget space: `taffy_id + 1`, but only if that taffy node is live.
        let taffy_id = TaffyNodeId::from(id.0 - 1);
        return node_map
            .contains_key(&taffy_id)
            .then_some(A11yTarget::Widget(taffy_id));
    }

    let index = WidgetIndex::build(node_map);
    let mut walk_counter: u64 = 0;
    search_synthetic(root, &index, id, &mut walk_counter)
}

/// Depth-first hunt for the widget whose synthetic subtree contains `wanted`,
/// mirroring [`visit`]'s traversal (and its `walk_counter` bookkeeping) so the
/// ids line up with the ones [`build_tree_update`] emitted.
fn search_synthetic(
    w: &dyn Widget,
    index: &WidgetIndex,
    wanted: AkNodeId,
    walk_counter: &mut u64,
) -> Option<A11yTarget> {
    let access = w.accessibility();
    let taffy_id = index.taffy_id_of(w);

    match access {
        Some(access_node) => {
            let owner_id = widget_node_id(taffy_id, *walk_counter);
            *walk_counter += 1;
            for child in w.children() {
                if let Some(hit) = search_synthetic(child, index, wanted, walk_counter) {
                    return Some(hit);
                }
            }
            // Only a laid-out widget can own an addressable synthetic subtree —
            // without a taffy id the app has nothing to dispatch against.
            if let Some(owner) = taffy_id {
                let mut key_path = Vec::new();
                if find_synthetic_path(&access_node.children, owner_id, wanted, &mut key_path) {
                    return Some(A11yTarget::Synthetic { owner, key_path });
                }
            }
            None
        }
        None => {
            *walk_counter += 1;
            for child in w.children() {
                if let Some(hit) = search_synthetic(child, index, wanted, walk_counter) {
                    return Some(hit);
                }
            }
            None
        }
    }
}

/// Walk one widget's synthetic forest looking for `wanted`, accumulating the
/// key path as it descends. Returns `true` (with `path` filled) on a hit.
fn find_synthetic_path(
    children: &[AccessNode],
    parent_id: AkNodeId,
    wanted: AkNodeId,
    path: &mut Vec<u64>,
) -> bool {
    for (ordinal, child) in children.iter().enumerate() {
        let key = child.key.unwrap_or(ordinal as u64);
        let id = synthetic_node_id(parent_id, key);
        path.push(key);
        if id == wanted || find_synthetic_path(&child.children, id, wanted, path) {
            return true;
        }
        path.pop();
    }
    false
}

/// Build a complete [`accesskit::TreeUpdate`] from the live widget tree.
///
/// * `root` — the App's root widget.
/// * `node_map` — the App's `taffy::NodeId -> *mut dyn Widget` map, used in
///   reverse to assign each widget a stable AccessKit id (see
///   [`widget_node_id`]). A widget not present in the map gets a structural
///   fallback id.
/// * `focused` — the `taffy::NodeId` of the focused widget, if any. The
///   resulting tree's `focus` points at the matching AccessKit node, or at the
///   synthetic root window when nothing is focused (AccessKit requires `focus`
///   to be a real node id).
/// * `scale` — multiply each widget rect by this to emit physical-pixel bounds.
///   Pass `1.0` for logical coords (what the tests assert on).
///
/// The returned update is **always a full tree** (every accessible node plus
/// the synthetic window root). Pushing a full tree each frame is correct;
/// AccessKit's platform adapters suppress events for unchanged nodes, so the
/// cost is processing, not spurious AT chatter. (A future optimization can diff
/// against the previous frame.)
pub fn build_tree_update(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget>,
    focused: Option<TaffyNodeId>,
    scale: f64,
) -> TreeUpdate {
    let index = WidgetIndex::build(node_map);

    let focused_ak = focused.map(|t| AkNodeId(u64::from(t) + 1));

    let mut nodes: Vec<(AkNodeId, AkNode)> = Vec::new();
    let mut walk_counter: u64 = 0;
    // Set by the walk when the *focused* widget marks one of its synthetic
    // descendants focused — the active-descendant target (see the module docs).
    let mut active_descendant: Option<AkNodeId> = None;

    // The synthetic window root gathers the top-level accessible nodes.
    let mut root_node = AkNode::new(AkRole::Window);
    root_node.set_label("qOS application window");

    let mut root_children: Vec<AkNodeId> = Vec::new();
    visit(
        root,
        &index,
        scale,
        focused,
        &mut walk_counter,
        &mut nodes,
        &mut root_children,
        &mut active_descendant,
    );
    root_node.set_children(root_children);
    nodes.push((ROOT_ID, root_node));

    // AccessKit requires `focus` to reference a node that exists in this update,
    // or it panics the consuming assistive tech. The focused taffy id can be
    // stale (taffy reassigns ids across relayouts) or point at a node the walk
    // collapsed, so only honor it when that node was actually emitted; otherwise
    // fall back to the synthetic window root.
    //
    // The active descendant wins over the widget itself: a canvas app that
    // tells us which clip the user is on should put the reader on that clip,
    // not on the whole timeline. It is only ever set for the focused widget, so
    // it cannot pull focus away from wherever the keyboard actually is.
    let focus = match (active_descendant, focused_ak) {
        (Some(id), _) if nodes.iter().any(|(nid, _)| *nid == id) => id,
        (_, Some(id)) if nodes.iter().any(|(nid, _)| *nid == id) => id,
        _ => ROOT_ID,
    };

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(ROOT_ID)),
        tree_id: accesskit::TreeId::ROOT,
        focus,
    }
}

/// DFS helper. For the widget `w`:
///   - if `w.accessibility()` is `Some`, emit a node, recurse to gather *its*
///     accessible children, and append the node's id to `parent_children`;
///   - if `None` (presentational), recurse and append the children's ids
///     directly to `parent_children` (reparenting — the widget collapses out).
#[allow(clippy::too_many_arguments)]
fn visit(
    w: &dyn Widget,
    index: &WidgetIndex,
    scale: f64,
    focused: Option<TaffyNodeId>,
    walk_counter: &mut u64,
    nodes: &mut Vec<(AkNodeId, AkNode)>,
    parent_children: &mut Vec<AkNodeId>,
    active_descendant: &mut Option<AkNodeId>,
) {
    let access = w.accessibility();
    let taffy_id = index.taffy_id_of(w);

    match access {
        Some(access_node) => {
            let id = widget_node_id(taffy_id, *walk_counter);
            *walk_counter += 1;

            let mut ak = AkNode::new(map_role(access_node.role));
            apply_semantics(&mut ak, &access_node, w, scale);

            // Gather this node's accessible children (collapsing presentational
            // descendants along the way).
            let mut my_children: Vec<AkNodeId> = Vec::new();
            for child in w.children() {
                visit(
                    child,
                    index,
                    scale,
                    focused,
                    walk_counter,
                    nodes,
                    &mut my_children,
                    active_descendant,
                );
            }
            // Synthetic children with no backing widget — a table's rows/cells,
            // a timeline's clips — described directly by the widget's
            // AccessNode. Only the widget that actually holds keyboard focus may
            // nominate an active descendant.
            let owns_focus = taffy_id.is_some() && taffy_id == focused;
            for (ordinal, synth) in access_node.children.iter().enumerate() {
                visit_synthetic(
                    synth,
                    id,
                    ordinal,
                    owns_focus,
                    nodes,
                    &mut my_children,
                    active_descendant,
                );
            }
            ak.set_children(my_children);

            nodes.push((id, ak));
            parent_children.push(id);
        }
        None => {
            // Presentational: collapse out, reparent children onto the parent.
            *walk_counter += 1;
            for child in w.children() {
                visit(
                    child,
                    index,
                    scale,
                    focused,
                    walk_counter,
                    nodes,
                    parent_children,
                    active_descendant,
                );
            }
        }
    }
}

/// Emit an [`AccessNode`] that has no backing [`Widget`] — a table row/cell, a
/// clip on a timeline track. Recurses into its own synthetic children. Carries
/// no geometry (these elements aren't laid out as widgets); everything else
/// (role, name/description/value, state, position, actions) is applied just as
/// for a real widget node.
///
/// The id is derived from `parent_id` chained with the node's
/// [`key`](AccessNode::key) — or its `ordinal` when it has none — so it survives
/// repaints and unrelated changes elsewhere in the tree. (The previous scheme
/// keyed off a tree-wide walk counter, which shifted every synthetic id
/// whenever *any* earlier widget appeared or vanished — an assistive technology
/// holding a node reference would silently find it pointing somewhere else.)
///
/// `in_focused_widget` says whether the owning widget holds keyboard focus;
/// only then may a node marked [`focused`](crate::a11y::AccessState::focused)
/// claim `active_descendant`. The first such node in document order wins.
fn visit_synthetic(
    access: &AccessNode,
    parent_id: AkNodeId,
    ordinal: usize,
    in_focused_widget: bool,
    nodes: &mut Vec<(AkNodeId, AkNode)>,
    parent_children: &mut Vec<AkNodeId>,
    active_descendant: &mut Option<AkNodeId>,
) {
    let id = synthetic_node_id(parent_id, access.key.unwrap_or(ordinal as u64));

    let mut ak = AkNode::new(map_role(access.role));
    apply_semantics_common(&mut ak, access);

    if in_focused_widget && access.state.focused && active_descendant.is_none() {
        *active_descendant = Some(id);
    }

    let mut my_children: Vec<AkNodeId> = Vec::new();
    for (i, child) in access.children.iter().enumerate() {
        visit_synthetic(
            child,
            id,
            i,
            in_focused_widget,
            nodes,
            &mut my_children,
            active_descendant,
        );
    }
    ak.set_children(my_children);

    nodes.push((id, ak));
    parent_children.push(id);
}

/// Copy an [`AccessNode`]'s name/description/value/state plus the widget's
/// bounds and supported actions onto an [`accesskit::Node`].
fn apply_semantics(ak: &mut AkNode, access: &AccessNode, w: &dyn Widget, scale: f64) {
    apply_semantics_common(ak, access);

    // Geometry: logical rect → (optionally scaled) AccessKit Rect.
    let r = w.get_rect();
    if r.width() > 0.0 || r.height() > 0.0 {
        ak.set_bounds(AkRect::new(
            r.x0 * scale,
            r.y0 * scale,
            r.x1 * scale,
            r.y1 * scale,
        ));
    }
}

/// The widget-independent half of [`apply_semantics`]: name/description/value,
/// heading level, collection position, state, and implicit/role actions.
/// Shared with [`visit_synthetic`] (table rows/cells), which has no geometry.
fn apply_semantics_common(ak: &mut AkNode, access: &AccessNode) {
    if let Some(name) = &access.name {
        ak.set_label(name.clone());
    }
    if let Some(desc) = &access.description {
        ak.set_description(desc.clone());
    }
    // Never expose a protected (password) value, even if the widget mistakenly
    // populated one.
    if !access.state.protected {
        if let Some(value) = &access.value {
            ak.set_value(value.clone());
        }
    }

    // Heading level rides on the role.
    if let Role::Heading { level } = access.role {
        ak.set_level(level as usize);
    }

    // Collection position ("row 2 of 4", "tab 3 of 5") and explicit nesting
    // depth, so an AT can place the element within its set.
    if let Some(pos) = access.pos_in_set {
        ak.set_position_in_set(pos);
    }
    if let Some(size) = access.set_size {
        ak.set_size_of_set(size);
    }
    if let Some(level) = access.level {
        ak.set_level(level);
    }

    apply_state(ak, access);

    // Implicit actions every accessible, enabled widget supports: Focus (if
    // focusable) and the default Click activation.
    if access.state.focusable && !access.state.disabled {
        ak.add_action(AkAction::Focus);
    }
    if !access.state.disabled {
        ak.add_action(AkAction::Click);
    }
    // Role-specific actions.
    match access.role {
        Role::Slider | Role::SpinButton => {
            if !access.state.disabled {
                ak.add_action(AkAction::Increment);
                ak.add_action(AkAction::Decrement);
            }
        }
        Role::ComboBox | Role::TreeItem => {
            // Expand/collapse only when the control models the state.
            if let Some(expanded) = access.state.expanded {
                ak.add_action(if expanded {
                    AkAction::Collapse
                } else {
                    AkAction::Expand
                });
            }
        }
        Role::TextInput | Role::MultilineTextInput => {
            if !access.state.read_only && !access.state.disabled {
                ak.add_action(AkAction::SetValue);
                ak.add_action(AkAction::SetTextSelection);
            }
        }
        _ => {}
    }
}

/// Translate our [`AccessState`](crate::a11y::AccessState) onto the relevant
/// AccessKit flags/enums.
fn apply_state(ak: &mut AkNode, access: &AccessNode) {
    let s = &access.state;

    if s.focused {
        // Focus is carried by the tree's `focus` field; nothing per-node here,
        // but disabled/hidden/etc. below still apply.
    }
    if s.disabled {
        ak.set_disabled();
    }
    if s.hidden {
        ak.set_hidden();
    }
    if s.read_only {
        ak.set_read_only();
    }
    if s.required {
        ak.set_required();
    }
    if s.busy {
        ak.set_busy();
    }
    if s.modal {
        ak.set_modal();
    }
    // NOTE: AccessKit 0.24 has no `multiline` flag — a multi-line text field is
    // expressed by the `MultilineTextInput` *role* (see `map_role`), which the
    // widget supplies. So `s.multiline` needs no per-node flag here.
    if let Some(checked) = s.checked {
        ak.set_toggled(if s.mixed {
            Toggled::Mixed
        } else if checked {
            Toggled::True
        } else {
            Toggled::False
        });
    } else if s.mixed {
        ak.set_toggled(Toggled::Mixed);
    }
    if let Some(expanded) = s.expanded {
        ak.set_expanded(expanded);
    }
    // `selected` is only meaningful inside a multi-selectable container; emit it
    // when the widget says it is selected, and explicitly clear it (Some(false))
    // for selectable items so an AT doesn't say "not selected" for unrelated
    // controls.
    if s.selected {
        ak.set_selected(true);
    } else if matches!(
        access.role,
        Role::ListItem | Role::TreeItem | Role::Tab | Role::Row | Role::Cell | Role::MenuItem
    ) {
        ak.set_selected(false);
    }
    if s.invalid {
        ak.set_invalid(accesskit::Invalid::True);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::{AccessNode, AccessState, Role};
    use crate::theme::Theme;
    use crate::widget::Widget;
    use std::collections::HashMap;
    use taffy::TaffyTree;

    // A minimal test widget: a configurable accessibility() + children, with a
    // rect we can assert on. Lets us build arbitrary trees without dragging in
    // real widgets' layout machinery.
    struct TestWidget {
        access: Option<AccessNode>,
        rect: vello::kurbo::Rect,
        kids: Vec<Box<dyn Widget>>,
        name: String,
    }

    impl TestWidget {
        fn new(access: Option<AccessNode>) -> Self {
            Self {
                access,
                rect: vello::kurbo::Rect::new(0.0, 0.0, 0.0, 0.0),
                kids: vec![],
                name: "test".into(),
            }
        }
        fn with_rect(mut self, x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
            self.rect = vello::kurbo::Rect::new(x0, y0, x1, y1);
            self
        }
        fn with_child(mut self, c: Box<dyn Widget>) -> Self {
            self.kids.push(c);
            self
        }
    }

    impl Widget for TestWidget {
        fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let child_nodes: Vec<taffy::NodeId> = self
                .kids
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            let node = taffy
                .new_with_children(taffy::Style::default(), &child_nodes)
                .unwrap();
            node_map.insert(node, self as *mut dyn Widget);
            node
        }
        fn apply_layout(
            &mut self,
            _taffy: &TaffyTree<()>,
            _node: taffy::NodeId,
            _origin: vello::kurbo::Point,
        ) {
        }
        fn draw(
            &mut self,
            _scene: &mut vello::Scene,
            _transform: vello::kurbo::Affine,
            _text_renderer: &mut crate::text_render::TextRenderer,
            _theme: &Theme,
            _presence: &crate::presence::PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _event: &winit::event::WindowEvent,
            _scale_factor: f64,
            _modifiers: &winit::event::Modifiers,
            _state: &mut automerge::AutoCommit,
            _path: &automerge::ObjId,
            _handled: &mut bool,
            _focus_target: &mut Option<taffy::NodeId>,
            _node_id: taffy::NodeId,
            _taffy: &TaffyTree<()>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.kids.iter().map(|c| c.as_ref()).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.kids
                .iter_mut()
                .map(|c| &mut **c as &mut (dyn Widget + '_))
                .collect()
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            self.rect
        }
        fn accessibility(&self) -> Option<AccessNode> {
            self.access.clone()
        }
    }

    /// Lay a tree out so every widget gets a real `taffy::NodeId` in the
    /// node_map, exactly as the App does before `build_tree_update`.
    fn lay_out(
        root: &mut dyn Widget,
    ) -> (TaffyTree<()>, HashMap<taffy::NodeId, *mut dyn Widget>, taffy::NodeId) {
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let theme = Theme::dark();
        let root_node = root.build_layout(&mut taffy, &theme, &mut node_map);
        (taffy, node_map, root_node)
    }

    #[test]
    fn maps_every_role_to_a_concrete_accesskit_role() {
        // A non-exhaustive smoke set covering the interesting many-to-one maps.
        let cases = [
            (Role::Window, AkRole::Window),
            (Role::Dialog, AkRole::Dialog),
            (Role::TabBar, AkRole::TabList),
            (Role::ToggleButton, AkRole::Button),
            (Role::Checkbox, AkRole::CheckBox),
            (Role::Switch, AkRole::Switch),
            (Role::Spinner, AkRole::ProgressIndicator),
            (Role::ProgressBar, AkRole::ProgressIndicator),
            (Role::StaticText, AkRole::Label),
            (Role::Heading { level: 2 }, AkRole::Heading),
            (Role::MultilineTextInput, AkRole::MultilineTextInput),
            (Role::Generic, AkRole::GenericContainer),
        ];
        for (ours, theirs) in cases {
            assert_eq!(map_role(ours), theirs, "role {ours:?} mapped wrong");
        }
    }

    #[test]
    fn action_round_trips_through_accesskit() {
        // Forward then back for the simple verbs.
        for a in [
            Action::Activate,
            Action::Focus,
            Action::Increment,
            Action::Decrement,
            Action::Expand,
            Action::Collapse,
            Action::ScrollIntoView,
            Action::ShowContextMenu,
        ] {
            let ak = map_action(&a);
            let back = accesskit_action_to_ours(ak, None).unwrap();
            // Activate and ToggleCheck both map to Click; round-trip lands on
            // Activate, which is the canonical "default activation".
            assert_eq!(back, a, "action {a:?} did not round-trip");
        }

        // SetValue carries its payload through ActionData::Value.
        let set = Action::SetValue("hello".into());
        let ak = map_action(&set);
        assert_eq!(ak, AkAction::SetValue);
        let data = accesskit::ActionData::Value("hello".into());
        assert_eq!(
            accesskit_action_to_ours(ak, Some(&data)),
            Some(Action::SetValue("hello".into()))
        );

        // ToggleCheck maps to Click (no dedicated AccessKit verb).
        assert_eq!(map_action(&Action::ToggleCheck), AkAction::Click);
    }

    #[test]
    fn presentational_containers_collapse_and_reparent() {
        // root(None) -> [ group(Some Group) -> [ btn(Some Button) ], None-wrapper -> [ text(Some StaticText) ] ]
        let btn = TestWidget::new(Some(AccessNode::new(Role::Button).with_name("Go")));
        let group = TestWidget::new(Some(AccessNode::new(Role::Group).with_name("Form")))
            .with_child(Box::new(btn));
        let text = TestWidget::new(Some(AccessNode::new(Role::StaticText).with_name("Hi")));
        let wrapper = TestWidget::new(None).with_child(Box::new(text)); // presentational
        let mut root = TestWidget::new(None) // presentational root
            .with_child(Box::new(group))
            .with_child(Box::new(wrapper));

        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);

        // Roles present: Window (root), Group, Button, Label (StaticText).
        let roles: Vec<AkRole> = update.nodes.iter().map(|(_, n)| n.role()).collect();
        assert!(roles.contains(&AkRole::Window));
        assert!(roles.contains(&AkRole::Group));
        assert!(roles.contains(&AkRole::Button));
        assert!(roles.contains(&AkRole::Label));
        // No GenericContainer — both presentational widgets collapsed out.
        assert!(
            !roles.contains(&AkRole::GenericContainer),
            "presentational widgets must not emit a node"
        );

        // The window root must own BOTH the Group and the reparented Label,
        // since their presentational parents collapsed.
        let (_, window) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == ROOT_ID)
            .unwrap();
        assert_eq!(
            window.children().len(),
            2,
            "window should own Group + reparented Label"
        );

        // The Button hangs off the Group, not the window.
        let group_id = window
            .children()
            .iter()
            .copied()
            .find(|cid| {
                update
                    .nodes
                    .iter()
                    .any(|(id, n)| id == cid && n.role() == AkRole::Group)
            })
            .expect("group child of window");
        let (_, group_node) = update.nodes.iter().find(|(id, _)| *id == group_id).unwrap();
        assert_eq!(group_node.children().len(), 1, "group owns the button");
    }

    #[test]
    fn synthetic_children_flow_into_the_tree_with_position_and_labels() {
        // A widget that owns rows/cells with no backing widgets — modelling a
        // DataTable. Each row is a Row node carrying its position, with two
        // header-named Cell children.
        let row = |pos: usize, size: usize| {
            AccessNode::new(Role::Row)
                .with_position(pos, size)
                .with_children(vec![
                    AccessNode::new(Role::Cell).with_name("Name").with_position(1, 2),
                    AccessNode::new(Role::Cell).with_name("Age").with_position(2, 2),
                ])
        };
        let table_access = AccessNode::new(Role::Table)
            .with_name("People")
            .with_children(vec![row(1, 2), row(2, 2)]);
        let mut root = TestWidget::new(Some(table_access));

        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);

        // The table node owns two synthetic rows.
        let (_, table) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::Table)
            .expect("table node present");
        assert_eq!(table.children().len(), 2, "table owns two synthetic rows");

        // Rows carry their 1-based position and own two header-named cells.
        let row_nodes: Vec<_> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == AkRole::Row)
            .collect();
        assert_eq!(row_nodes.len(), 2, "two row nodes reached the tree");
        for (_, r) in &row_nodes {
            assert_eq!(r.size_of_set(), Some(2), "row knows the table has 2 rows");
            assert!(r.position_in_set().is_some(), "row carries its 1-based position");
            assert_eq!(r.children().len(), 2, "each row owns two cells");
        }

        // Cells are named by their column header.
        let cell_labels: Vec<Option<&str>> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == AkRole::Cell)
            .map(|(_, n)| n.label())
            .collect();
        assert!(cell_labels.contains(&Some("Name")), "a cell is named for the Name column");
        assert!(cell_labels.contains(&Some("Age")), "a cell is named for the Age column");
        assert_eq!(cell_labels.len(), 4, "two cells per row across two rows");
    }

    #[test]
    fn focus_targets_the_focused_widget() {
        let btn = TestWidget::new(Some(
            AccessNode::new(Role::Button)
                .with_name("Save")
                .with_state(AccessState::new().focusable(true).focused(true)),
        ));
        let mut root = TestWidget::new(None).with_child(Box::new(btn));

        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        // Find the taffy id of the button (the widget whose accessibility role
        // is Button).
        let focused_taffy = node_map
            .iter()
            .find(|(_, ptr)| {
                let w = unsafe { &***ptr };
                w.accessibility()
                    .map(|n| n.role == Role::Button)
                    .unwrap_or(false)
            })
            .map(|(&id, _)| id);
        assert!(focused_taffy.is_some());

        let update = build_tree_update(&root, &node_map, focused_taffy, 1.0);
        // The focused AccessKit id must equal the button's derived id, and that
        // id must exist in the node list.
        assert_ne!(update.focus, ROOT_ID, "focus should be the button, not root");
        assert!(
            update.nodes.iter().any(|(id, n)| *id == update.focus && n.role() == AkRole::Button),
            "focus must point at the Button node"
        );
    }

    #[test]
    fn focus_falls_back_to_root_when_nothing_focused() {
        let mut root = TestWidget::new(Some(AccessNode::new(Role::Document)));
        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        assert_eq!(update.focus, ROOT_ID);
    }

    #[test]
    fn focus_falls_back_to_root_when_focused_id_is_stale() {
        // A focused taffy id that isn't in the current tree (e.g. left over
        // from before a relayout, since taffy reassigns ids) must NOT be
        // forwarded as the tree focus — AccessKit panics the assistive tech if
        // `focus` references a node that wasn't emitted.
        let btn = TestWidget::new(Some(
            AccessNode::new(Role::Button)
                .with_name("Save")
                .with_state(AccessState::new().focusable(true)),
        ));
        let mut root = TestWidget::new(None).with_child(Box::new(btn));
        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let stale = taffy::NodeId::from(u64::MAX - 7);
        let update = build_tree_update(&root, &node_map, Some(stale), 1.0);
        assert_eq!(update.focus, ROOT_ID);
        // The focus is always a node that exists in the update.
        assert!(update.nodes.iter().any(|(id, _)| *id == update.focus));
    }

    #[test]
    fn bounds_carry_through_with_scale() {
        let btn = TestWidget::new(Some(AccessNode::new(Role::Button).with_name("B")))
            .with_rect(10.0, 20.0, 110.0, 60.0);
        let mut root = TestWidget::new(None).with_child(Box::new(btn));
        let (_taffy, node_map, _root_node) = lay_out(&mut root);

        // Logical (scale 1.0).
        let update = build_tree_update(&root, &node_map, None, 1.0);
        let (_, button) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::Button)
            .unwrap();
        let b = button.bounds().expect("button has bounds");
        assert_eq!((b.x0, b.y0, b.x1, b.y1), (10.0, 20.0, 110.0, 60.0));

        // Physical (scale 2.0).
        let update2 = build_tree_update(&root, &node_map, None, 2.0);
        let (_, button2) = update2
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::Button)
            .unwrap();
        let b2 = button2.bounds().expect("button has bounds");
        assert_eq!((b2.x0, b2.y0, b2.x1, b2.y1), (20.0, 40.0, 220.0, 120.0));
    }

    #[test]
    fn state_flags_map_to_accesskit() {
        let checkbox = TestWidget::new(Some(
            AccessNode::new(Role::Checkbox).with_state(
                AccessState::new()
                    .focusable(true)
                    .checked(true)
                    .disabled(true),
            ),
        ));
        let mut root = TestWidget::new(None).with_child(Box::new(checkbox));
        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::CheckBox)
            .unwrap();
        assert_eq!(node.toggled(), Some(Toggled::True));
        assert!(node.is_disabled());
        // Disabled → no Focus/Click actions added.
        assert!(!node.supports_action(AkAction::Focus));
        assert!(!node.supports_action(AkAction::Click));
    }

    #[test]
    fn enabled_focusable_widget_supports_focus_and_click() {
        let btn = TestWidget::new(Some(
            AccessNode::new(Role::Button)
                .with_name("Go")
                .with_state(AccessState::new().focusable(true)),
        ));
        let mut root = TestWidget::new(None).with_child(Box::new(btn));
        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::Button)
            .unwrap();
        assert!(node.supports_action(AkAction::Focus));
        assert!(node.supports_action(AkAction::Click));
    }

    #[test]
    fn slider_supports_increment_decrement() {
        let slider = TestWidget::new(Some(
            AccessNode::new(Role::Slider)
                .with_value("50%")
                .with_state(AccessState::new().focusable(true)),
        ));
        let mut root = TestWidget::new(None).with_child(Box::new(slider));
        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::Slider)
            .unwrap();
        assert!(node.supports_action(AkAction::Increment));
        assert!(node.supports_action(AkAction::Decrement));
        assert_eq!(node.value(), Some("50%"));
    }

    #[test]
    fn protected_value_is_never_exposed() {
        let pw = TestWidget::new(Some(
            AccessNode::new(Role::TextInput)
                .with_value("hunter2")
                .with_state(AccessState::new().protected(true)),
        ));
        let mut root = TestWidget::new(None).with_child(Box::new(pw));
        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::TextInput)
            .unwrap();
        assert_eq!(node.value(), None, "protected value must not leak to the AT");
    }

    #[test]
    fn heading_level_is_set() {
        let h = TestWidget::new(Some(
            AccessNode::new(Role::Heading { level: 3 }).with_name("Section"),
        ));
        let mut root = TestWidget::new(None).with_child(Box::new(h));
        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::Heading)
            .unwrap();
        assert_eq!(node.level(), Some(3));
    }

    // ---- Synthetic nodes: focus, stable ids, action routing --------------

    /// A canvas-style widget: one accessible node whose contents are entirely
    /// synthetic (no child widgets), the shape the video editor / sheets use.
    fn canvas(children: Vec<AccessNode>) -> AccessNode {
        AccessNode::new(Role::Canvas)
            .with_name("Timeline")
            .with_state(AccessState::new().focusable(true))
            .with_children(children)
    }

    fn clip(name: &str, focused: bool) -> AccessNode {
        AccessNode::new(Role::ListItem)
            .with_name(name)
            .with_state(AccessState::new().focusable(true).focused(focused))
    }

    /// The taffy id of the single widget in `node_map` whose accessible role
    /// matches `role`.
    fn taffy_id_of(
        node_map: &HashMap<taffy::NodeId, *mut dyn Widget>,
        role: Role,
    ) -> taffy::NodeId {
        node_map
            .iter()
            .find(|(_, ptr)| {
                let w = unsafe { &***ptr };
                w.accessibility().map(|n| n.role == role).unwrap_or(false)
            })
            .map(|(&id, _)| id)
            .expect("a widget with that role was laid out")
    }

    fn label_id(update: &TreeUpdate, label: &str) -> AkNodeId {
        update
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some(label))
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("no node labelled {label} in the update"))
    }

    #[test]
    fn focus_lands_on_the_marked_synthetic_node_not_its_owning_widget() {
        // A canvas widget holds keyboard focus and nominates its second clip as
        // the active descendant. The reader must land on THAT clip — a canvas
        // whose focus can only ever be the whole canvas is precisely why a
        // screen-reader user cannot navigate one.
        let mut root = TestWidget::new(Some(canvas(vec![
            clip("Intro", false),
            clip("Interview", true),
            clip("Outro", false),
        ])));
        let (_taffy, node_map, _n) = lay_out(&mut root);
        let canvas_taffy = taffy_id_of(&node_map, Role::Canvas);

        let update = build_tree_update(&root, &node_map, Some(canvas_taffy), 1.0);

        let canvas_ak = AkNodeId(u64::from(canvas_taffy) + 1);
        assert_ne!(
            update.focus, canvas_ak,
            "focus must not fall back to the whole canvas when a descendant is marked"
        );
        assert_ne!(update.focus, ROOT_ID);
        let (_, focused) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == update.focus)
            .expect("the focused id must name a node in this very update");
        assert_eq!(
            focused.label(),
            Some("Interview"),
            "focus must be the clip the widget marked, not one of its siblings"
        );
        assert!(
            is_synthetic_id(update.focus),
            "the focused node is synthetic, so its id must live in the synthetic id space"
        );
    }

    #[test]
    fn an_unfocused_widget_cannot_claim_the_readers_cursor() {
        // Two widgets; the one WITHOUT keyboard focus marks a descendant
        // focused. Honouring that would let any offscreen widget yank the reader
        // away from where the keyboard actually is.
        let idle = TestWidget::new(Some(canvas(vec![clip("Ghost", true)])));
        let btn = TestWidget::new(Some(
            AccessNode::new(Role::Button)
                .with_name("Save")
                .with_state(AccessState::new().focusable(true)),
        ));
        let mut root = TestWidget::new(None)
            .with_child(Box::new(idle))
            .with_child(Box::new(btn));
        let (_taffy, node_map, _n) = lay_out(&mut root);
        let button_taffy = taffy_id_of(&node_map, Role::Button);

        let update = build_tree_update(&root, &node_map, Some(button_taffy), 1.0);

        assert_eq!(
            update.focus,
            AkNodeId(u64::from(button_taffy) + 1),
            "focus stays on the widget that holds the keyboard"
        );
        let (_, focused) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == update.focus)
            .unwrap();
        assert_eq!(focused.label(), Some("Save"));
    }

    #[test]
    fn a_synthetic_nodes_id_survives_changes_elsewhere_in_the_tree() {
        // Two independent widgets with synthetic contents. Adding a clip to the
        // FIRST must not renumber the second's rows: an AT holding a reference
        // to "Beta / two" would otherwise silently start pointing elsewhere.
        let mut first = Box::new(TestWidget::new(Some(canvas(vec![clip("a", false)]))));
        let first_ptr: *mut TestWidget = &mut *first;
        let second = Box::new(TestWidget::new(Some(
            AccessNode::new(Role::Table)
                .with_name("Beta")
                .with_children(vec![
                    AccessNode::new(Role::Row).with_name("one"),
                    AccessNode::new(Role::Row).with_name("two"),
                ]),
        )));
        let mut root = TestWidget::new(None).with_child(first).with_child(second);
        let (_taffy, node_map, _n) = lay_out(&mut root);

        let before = build_tree_update(&root, &node_map, None, 1.0);
        let two_before = label_id(&before, "two");

        // Grow the first widget's synthetic list. Same widgets, same layout,
        // same taffy ids — only an unrelated neighbour's content changed.
        unsafe {
            (*first_ptr).access = Some(canvas(vec![
                clip("a", false),
                clip("b", false),
                clip("c", false),
            ]));
        }
        let after = build_tree_update(&root, &node_map, None, 1.0);

        assert_eq!(
            label_id(&after, "two"),
            two_before,
            "a synthetic id must not depend on how many synthetic nodes precede it in the tree"
        );
        assert_ne!(
            label_id(&after, "one"),
            two_before,
            "sibling ids must still be distinct"
        );
    }

    #[test]
    fn an_explicitly_keyed_node_keeps_its_id_when_it_moves() {
        // Ordinal keying is right for a table row ("row 3" is the identity the
        // user means), but wrong for a clip that can be dragged along a track.
        // An explicit key pins the id to the element instead of the slot.
        let keyed = |name: &str| {
            AccessNode::new(Role::ListItem)
                .with_name(name)
                .with_hashed_key(name)
        };
        let mut root = TestWidget::new(Some(canvas(vec![keyed("intro"), keyed("outro")])));
        let (_taffy, node_map, _n) = lay_out(&mut root);

        let before = build_tree_update(&root, &node_map, None, 1.0);
        let outro_before = label_id(&before, "outro");

        // Drag "outro" to the front — same elements, new order.
        root.access = Some(canvas(vec![keyed("outro"), keyed("intro")]));
        let after = build_tree_update(&root, &node_map, None, 1.0);

        assert_eq!(
            label_id(&after, "outro"),
            outro_before,
            "an explicitly keyed node keeps its id across a reorder"
        );
        assert_ne!(
            label_id(&after, "intro"),
            outro_before,
            "and does not inherit its new neighbour's id"
        );
    }

    #[test]
    fn synthetic_ids_never_collide_with_widget_ids() {
        let mut root = TestWidget::new(Some(canvas(vec![clip("a", false), clip("b", false)])));
        let (_taffy, node_map, _n) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);

        // The id spaces partition cleanly: the root and every widget node sit
        // below the marker bit, every synthetic node above it.
        assert!(!is_synthetic_id(ROOT_ID));
        for (id, node) in &update.nodes {
            let synthetic = node.role() == AkRole::ListItem;
            assert_eq!(
                is_synthetic_id(*id),
                synthetic,
                "id {id:?} ({:?}) is in the wrong id space",
                node.role()
            );
        }
        // And no id is emitted twice.
        let mut ids: Vec<u64> = update.nodes.iter().map(|(id, _)| id.0).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(count, ids.len(), "duplicate node id in the tree update");
    }

    #[test]
    fn resolve_node_id_round_trips_an_emitted_synthetic_id_to_owner_and_key_path() {
        // The action-routing contract: whatever id the bridge put in the tree,
        // an incoming ActionRequest carrying it must resolve back to the widget
        // that owns it plus the path naming the exact element.
        let mut root = TestWidget::new(Some(
            AccessNode::new(Role::Table)
                .with_name("People")
                .with_children(vec![
                    AccessNode::new(Role::Row).with_name("r0").with_children(vec![
                        AccessNode::new(Role::Cell).with_name("r0c0"),
                        AccessNode::new(Role::Cell).with_name("r0c1"),
                    ]),
                    AccessNode::new(Role::Row).with_name("r1").with_children(vec![
                        AccessNode::new(Role::Cell).with_name("r1c0"),
                        AccessNode::new(Role::Cell).with_name("r1c1"),
                    ]),
                ]),
        ));
        let (_taffy, node_map, _n) = lay_out(&mut root);
        let table_taffy = taffy_id_of(&node_map, Role::Table);
        let update = build_tree_update(&root, &node_map, None, 1.0);

        // A row: one path element, its ordinal.
        assert_eq!(
            resolve_node_id(&root, &node_map, label_id(&update, "r1")),
            Some(A11yTarget::Synthetic {
                owner: table_taffy,
                key_path: vec![1],
            }),
        );
        // A cell: row, then column.
        assert_eq!(
            resolve_node_id(&root, &node_map, label_id(&update, "r1c0")),
            Some(A11yTarget::Synthetic {
                owner: table_taffy,
                key_path: vec![1, 0],
            }),
        );
        assert_eq!(
            resolve_node_id(&root, &node_map, label_id(&update, "r0c1")),
            Some(A11yTarget::Synthetic {
                owner: table_taffy,
                key_path: vec![0, 1],
            }),
        );
        // The widget itself and the window root still resolve to themselves.
        assert_eq!(
            resolve_node_id(&root, &node_map, label_id(&update, "People")),
            Some(A11yTarget::Widget(table_taffy)),
        );
        assert_eq!(
            resolve_node_id(&root, &node_map, ROOT_ID),
            Some(A11yTarget::Root),
        );
    }

    #[test]
    fn resolve_node_id_reports_a_stale_reference_as_gone() {
        // An AT can hold an id across a structural change. Resolving it to
        // "whatever now sits there" would operate on the wrong element, so an
        // unknown id must resolve to nothing at all.
        let mut root = TestWidget::new(Some(canvas(vec![clip("only", false)])));
        let (_taffy, node_map, _n) = lay_out(&mut root);

        let bogus_synthetic = AkNodeId(0x1234_5678_9abc_def0 | SYNTHETIC_ID_BIT);
        assert_eq!(resolve_node_id(&root, &node_map, bogus_synthetic), None);

        let bogus_widget = AkNodeId(u64::from(u32::MAX));
        assert_eq!(resolve_node_id(&root, &node_map, bogus_widget), None);
    }

    #[test]
    fn resolve_node_id_declines_a_synthetic_node_whose_owner_was_never_laid_out() {
        // Without a taffy id there is no handle to dispatch against, so such a
        // node must report as unaddressable rather than resolve to a wrong owner.
        let mut root = TestWidget::new(Some(canvas(vec![clip("orphan", false)])));
        let empty_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();

        let update = build_tree_update(&root, &empty_map, None, 1.0);
        let orphan = label_id(&update, "orphan");
        assert!(is_synthetic_id(orphan));
        assert_eq!(resolve_node_id(&root, &empty_map, orphan), None);
    }

    #[test]
    fn every_update_includes_the_window_root_and_a_tree() {
        let mut root = TestWidget::new(None);
        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        assert!(update.tree.is_some());
        assert_eq!(update.tree.as_ref().unwrap().root, ROOT_ID);
        assert!(update.nodes.iter().any(|(id, _)| *id == ROOT_ID));
    }
}
