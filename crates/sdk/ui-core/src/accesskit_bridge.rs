//! AccessKit bridge: turns the toolkit's per-widget semantic model
//! ([`crate::a11y::AccessNode`], from
//! [`Widget::accessibility`]) into an
//! [`accesskit::TreeUpdate`], published over AT-SPI2.
//!
//! [`build_tree_update_with_resources`] is a pure function of `(root_widget,
//! node_map, focused, resources)`: a depth-first walk that emits one node per widget returning
//! `Some(AccessNode)`, and collapses presentational widgets (`None`) by
//! reparenting their accessible children onto the nearest ancestor —
//! mirroring ARIA `role="presentation"`. Being pure and keyed off the same
//! `node_map` the `App` maintains makes it unit-testable without a live
//! AT-SPI bus.
//!
//! ## Id spaces
//!
//! Five disjoint ranges share the `u64`, so an id says what it addresses
//! without a lookup and [`resolve_node_id`] can invert it:
//!
//! | space | derivation | range |
//! |---|---|---|
//! | root | [`ROOT_ID`] (`0`) | zero |
//! | resource / action | hash of the published address | `1 << 60` / `2 << 60` |
//! | widget | hash of the widget's [`SemanticId`], and of its owner's lifetime inside a bound component | `3 << 60` |
//! | text run | `(1 << 62) \| owner id` | bit 62 |
//! | synthetic | the owner's id chained with each [`AccessNode::key`] | bit 63 |
//!
//! A widget's id is its semantic identity, not its layout slot, so an
//! assistive technology keeps its place across a rebuild of the widget tree
//! exactly as a pick or a recorded step does. Inside a bound component or a
//! legacy record owner the owner's lifetime is hashed in too, so an id aimed
//! at an instance that has gone never lands on its replacement.
//!
//! ## Canvas widgets: focus and actions on synthetic nodes
//!
//! A widget that paints its own contents — a timeline of clips, a table of
//! rows — has no child widgets to expose, so it *synthesises* its accessible
//! structure through [`AccessNode::children`]. Two things make those nodes
//! first-class rather than merely readable:
//!
//! * **Focus.** The widget marks one descendant
//!   [`focused`](crate::a11y::AccessState::focused); while that widget holds
//!   the keyboard, [`build_tree_update`] aims the tree's `focus` at *that* node
//!   instead of the widget. This is ARIA's `aria-activedescendant`: the widget
//!   owns the keyboard, the marked node is where the user is. Keying it off the
//!   App's real focus is deliberate — an unfocused widget cannot steal the
//!   reader's cursor by setting a flag. [`active_descendant_path`] names the
//!   same node for the keyboard path, so Enter activates what the reader just
//!   announced.
//! * **Actions.** [`resolve_node_id`] inverts a synthetic id back to
//!   `(owning widget, key path)` so an incoming `ActionRequest` reaches the
//!   clip the user was on, via
//!   [`Widget::perform_action_at`].
//!   Inversion re-walks the live tree rather than caching an index, so a stale
//!   id from a previous frame resolves to `None` instead of to the wrong clip.

use std::collections::HashMap;
use std::hash::BuildHasher;

use accesskit::{
    Action as AkAction, Live as AkLive, Node as AkNode, NodeId as AkNodeId, Rect as AkRect,
    Role as AkRole, TextPosition as AkTextPosition, TextSelection as AkTextSelection, Toggled,
    TreeInfo, TreeUpdate,
};
use taffy::NodeId as TaffyNodeId;

use crate::a11y::{AccessNode, Action, Role};
use crate::a11y_tree::SemanticId;
use crate::text_break::{grapheme_indices, graphemes};
use crate::widget::Widget;

type HostedWidget<'a> = (Vec<usize>, &'a dyn Widget);
type HostedByIndex<'a> = HashMap<usize, Vec<HostedWidget<'a>>>;

/// Reserved so no widget can collide with it: every widget id carries the
/// `3 << 60` range marker — see `widget_node_id`.
pub const ROOT_ID: AkNodeId = AkNodeId(0);
/// Widget nodes: a stable hash of the widget's semantic id, in the range
/// resource (`1 << 60`) and action (`2 << 60`) nodes leave free.
const WIDGET_ID_BASE: u64 = 3u64 << 60;
const TEXT_RUN_ID_BASE: u64 = 1u64 << 62;
/// Resource and action nodes occupy reserved ranges above synthetic widget
/// nodes and below text-run nodes. The ids are stable hashes of their public
/// addresses, so assistive technology can retain identity across frames.
pub const RESOURCE_ID_BASE: u64 = 1u64 << 60;
const ACTION_ID_BASE: u64 = 2u64 << 60;
const ADDRESS_ID_MASK: u64 = (1u64 << 60) - 1;

fn stable_address_hash(parts: &[&str]) -> u64 {
    // FNV-1a is deliberately fixed here rather than using RandomState: node
    // identity must survive process restarts and Rust upgrades.
    let mut hasher = fnv::FnvHasher::default();
    for part in parts {
        std::hash::Hasher::write(&mut hasher, part.as_bytes());
        std::hash::Hasher::write(&mut hasher, &[0]);
    }
    std::hash::Hasher::finish(&hasher) & ADDRESS_ID_MASK
}

#[must_use]
pub fn resource_node_id(resource: &str) -> AkNodeId {
    AkNodeId(RESOURCE_ID_BASE | stable_address_hash(&[resource]))
}

#[must_use]
pub fn resource_action_node_id(resource: &str, action: &str) -> AkNodeId {
    AkNodeId(ACTION_ID_BASE | stable_address_hash(&[resource, action]))
}
/// Bit that marks an [`accesskit::NodeId`] as belonging to a **synthetic**
/// node — one with no backing [`Widget`]. Widget ids are `taffy_id + 1` or
/// `(1 << 48) + index` and text-run ids sit at bit 62, so nothing else in the
/// tree can reach bit 63 and the space cannot collide.
pub const SYNTHETIC_ID_BIT: u64 = 1u64 << 63;

/// E.g. [`Role::Spinner`] becomes a busy [`AkRole::ProgressIndicator`] and
/// [`Role::ToggleButton`] becomes [`AkRole::Button`] carrying `toggled` state.
pub fn map_role(role: Role) -> AkRole {
    match role {
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

        // No dedicated "toggle button" role; a Button with `toggled` state
        // (applied in `apply_state`) is the AccessKit idiom.
        Role::Button | Role::ToggleButton => AkRole::Button,
        Role::Checkbox => AkRole::CheckBox,
        Role::RadioButton => AkRole::RadioButton,
        Role::Switch => AkRole::Switch,
        Role::Slider => AkRole::Slider,
        Role::SpinButton => AkRole::SpinButton,
        Role::ComboBox => AkRole::ComboBox,
        Role::TextInput => AkRole::TextInput,
        Role::MultilineTextInput => AkRole::MultilineTextInput,
        Role::Link => AkRole::Link,
        // Indeterminate progress indicator; busy flag applied in `apply_state`.
        Role::ProgressBar | Role::Spinner => AkRole::ProgressIndicator,
        Role::Tooltip => AkRole::Tooltip,

        Role::Image => AkRole::Image,
        Role::Label | Role::StaticText => AkRole::Label,
        Role::Heading { .. } => AkRole::Heading,
        Role::Paragraph => AkRole::Paragraph,
        Role::Document => AkRole::Document,
        Role::Terminal => AkRole::Terminal,
        Role::Canvas => AkRole::Canvas,
        Role::Unknown => AkRole::Unknown,
    }
}

/// `ToggleCheck` has no dedicated verb — toggling is a `Click` at the
/// platform level. `SetValue`/`SetSelection` payloads travel in `ActionData`
/// on the round trip ([`accesskit_action_to_ours`]).
pub fn map_action(action: &Action) -> AkAction {
    match action {
        Action::Activate | Action::ToggleCheck => AkAction::Click,
        Action::Focus => AkAction::Focus,
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

/// Inverse of [`map_action`]. `None` for actions we don't model
/// (scroll-by-direction, custom actions, …).
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

/// A widget's AccessKit id: a stable hash of its [`SemanticId`], so an
/// assistive technology keeps its place across layout rebuilds exactly as
/// every other consumer of the semantic tree does.
///
/// Inside a bound component the component's lifetime is part of the hash:
/// the widgets of a new instance get new ids, so an action an assistive
/// technology aims at an instance that has gone never lands on its
/// successor.
fn widget_node_id(id: &SemanticId, lifetime: Option<uuid::Uuid>) -> AkNodeId {
    let lifetime = lifetime
        .map(|lifetime| lifetime.to_string())
        .unwrap_or_default();
    AkNodeId(WIDGET_ID_BASE | stable_address_hash(&[id.as_str(), &lifetime]))
}

fn text_run_node_id(parent: AkNodeId) -> AkNodeId {
    AkNodeId(TEXT_RUN_ID_BASE | parent.0)
}

/// Derive a synthetic node's stable id from its parent's id and its own key.
///
/// Chaining through the parent means the id encodes the whole path from the
/// owning widget down, so two rows that both hold a cell keyed `0` still get
/// distinct cell ids, and a node's id changes only when something on its path
/// changes. Deriving it from the path rather than from a tree-wide walk counter
/// is what stops an unrelated widget gaining a row from renumbering everything
/// after it — an assistive technology holding a node reference would otherwise
/// silently find it pointing somewhere else.
fn synthetic_node_id(parent: AkNodeId, key: u64) -> AkNodeId {
    use std::hash::Hasher;
    let mut h = fnv::FnvHasher::default();
    h.write(&parent.0.to_le_bytes());
    h.write(&key.to_le_bytes());
    // Force the marker bit; everything else is hash.
    AkNodeId(h.finish() | SYNTHETIC_ID_BIT)
}

/// Whether `id` addresses a synthetic node rather than a widget, a text run or
/// the root.
pub fn is_synthetic_id(id: AkNodeId) -> bool {
    id.0 & SYNTHETIC_ID_BIT != 0
}

/// The AccessKit id of the widget laid out at `taffy_id` in the tree rooted
/// at `root`, or `None` when no accessible widget holds that layout node.
///
/// The host tracks focus by `taffy::NodeId` while the exported tree is keyed by
/// AccessKit id, so anything crossing between the two — moving focus to a node,
/// asserting on one in a test — has to translate. It walks the tree because a
/// widget's id comes from its identity, not from its layout slot.
pub fn widget_ak_id<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    taffy_id: TaffyNodeId,
) -> Option<AkNodeId> {
    widget_ak_id_impl::<S, false>(&[root], node_map, taffy_id, None)
}

/// [`widget_ak_id`] for the host's bound presentation roots.
pub(crate) fn widget_ak_id_bound<S: BuildHasher>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    taffy_id: TaffyNodeId,
    custody: &Custody,
) -> Option<AkNodeId> {
    widget_ak_id_impl::<S, true>(roots, node_map, taffy_id, Some(custody))
}

fn widget_ak_id_impl<S: BuildHasher, const BOUND: bool>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    taffy_id: TaffyNodeId,
    custody: Option<&Custody>,
) -> Option<AkNodeId> {
    let (_, targets) = build_with_targets::<S, BOUND>(roots, node_map, None, 1.0, &[], custody);
    targets.into_iter().find_map(|(id, target)| {
        (target == A11yTarget::Widget(taffy_id) && id.0 & TEXT_RUN_ID_BASE == 0).then_some(id)
    })
}

/// The key path to the **active descendant** a widget nominates — the first
/// node in its synthetic subtree marked
/// [`focused`](crate::a11y::AccessState::focused) — or `None` when it nominates
/// none.
///
/// [`build_tree_update`] uses the same rule to aim the exported focus. This is
/// the other half: the host uses it to aim *keyboard* activation at the element
/// the user is on, so a canvas widget implements
/// [`Widget::perform_action_at`] once
/// and gets both the assistive-technology and the Enter/arrow paths, instead of
/// having to implement `perform_action` again for the keyboard.
pub fn active_descendant_path(access: &AccessNode) -> Option<Vec<u64>> {
    fn walk(children: &[AccessNode], path: &mut Vec<u64>) -> bool {
        for (ordinal, child) in children.iter().enumerate() {
            path.push(child.key.unwrap_or(ordinal as u64));
            if child.state.focused || walk(&child.children, path) {
                return true;
            }
            path.pop();
        }
        false
    }
    let mut path = Vec::new();
    walk(&access.children, &mut path).then_some(path)
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
        /// Keys, for a widget that identifies a synthetic child by identity
        /// — `perform_action_at` matches a clip by `hash_key(uuid)`, which is
        /// nothing like a position.
        key_path: Vec<u64>,
        /// Ordinals, for a widget that identifies one by position —
        /// `synthetic_accessibility_rect` indexes into `seg_rects`.
        ///
        /// Both are carried because both are required, and they differ
        /// for any node that sets an explicit key. Handing a positional
        /// implementor a key it never had is a silent mis-target.
        ordinal_path: Vec<usize>,
        /// Opaque stable model identity supplied by the synthetic node. This
        /// lets virtual collections route actions by record identity even
        /// when the row moved since the last frame.
        semantic_target: Option<String>,
    },
}

/// Invert an [`accesskit::NodeId`] back to the thing it addresses, by
/// re-deriving ids over the **current** widget tree.
///
/// Returns `None` when the id belongs to no node this frame — a reference an
/// assistive technology kept across a structural change. Callers must treat
/// that as "gone", never as a fallback to some other node: silently retargeting
/// a stale id is how an AT ends up deleting the wrong clip. The same goes for a
/// widget that was never laid out, and for a synthetic node owned by one: with
/// no taffy id there is no handle to dispatch against.
///
/// A text run resolves to the field that owns it — the run *is* the widget's
/// text, so a verb aimed at it means the widget.
pub fn resolve_node_id<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    id: AkNodeId,
) -> Option<A11yTarget> {
    resolve_node_id_impl::<S, false>(&[root], node_map, id, None)
}
/// Host traversal retaining the originating component through every callback.
///
/// `roots` are the whole presentation the id was exported from: ids are
/// scoped across every root of one walk, so resolving against one root of a
/// multi-root presentation would derive different ids.
pub(crate) fn resolve_node_id_bound<S: BuildHasher>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    id: AkNodeId,
    custody: &Custody,
) -> Option<A11yTarget> {
    resolve_node_id_impl::<S, true>(roots, node_map, id, Some(custody))
}
fn resolve_node_id_impl<S: BuildHasher, const BOUND: bool>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    id: AkNodeId,
    custody: Option<&Custody>,
) -> Option<A11yTarget> {
    if id == ROOT_ID {
        return Some(A11yTarget::Root);
    }
    // One walk derives every id, for export and for resolution alike, so the
    // two can never drift apart.
    let (_, mut targets) = build_with_targets::<S, BOUND>(roots, node_map, None, 1.0, &[], custody);
    targets.remove(&id)
}

/// Reverse of the App's `taffy::NodeId -> *mut dyn Widget` map, so the walk can
/// look up a stable id for any `&dyn Widget` it meets.
struct WidgetNodeMap {
    nodes: HashMap<*const (), Vec<(TaffyNodeId, &'static str)>>,
    aliases: std::collections::HashSet<*const ()>,
}

impl WidgetNodeMap {
    fn get(&self, widget: &dyn Widget) -> Option<TaffyNodeId> {
        let pointer = std::ptr::from_ref(widget);
        let address = pointer.cast::<()>();
        let entries = self.nodes.get(&address)?;
        // A Rust struct can share its address with its first field. Only
        // the registered trait object owns this layout id; the wrapper gets
        // a structural id. For unaliased addresses retain the original rule,
        // independent of possible compiler duplication of trait vtables.
        if !self.aliases.contains(&address) {
            return entries.first().map(|(id, _)| *id);
        }
        let concrete = widget.concrete_type_name();
        let mut matches = entries
            .iter()
            .filter(|(_, registered_type)| *registered_type == concrete);
        let (id, _) = matches.next()?;
        // Multiple nodes with the same address and concrete type are not
        // uniquely addressable. Keep them unavailable instead of retargeting.
        matches.next().is_none().then_some(*id)
    }
}

fn reverse_node_map<S: BuildHasher, const BOUND: bool>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
) -> WidgetNodeMap {
    fn collect<const BOUND: bool>(
        widget: &dyn Widget,
        seen: &mut std::collections::HashSet<*const ()>,
        aliases: &mut std::collections::HashSet<*const ()>,
    ) {
        let Ok(_entry) = crate::bound_dispatch::enter_traversal::<BOUND>(widget) else {
            return;
        };
        let address = std::ptr::from_ref(widget).cast::<()>();
        if !seen.insert(address) {
            aliases.insert(address);
        }
        for child in widget.children() {
            collect::<BOUND>(child, seen, aliases);
        }
    }
    let mut result = WidgetNodeMap {
        nodes: HashMap::new(),
        aliases: std::collections::HashSet::new(),
    };
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        collect::<BOUND>(*root, &mut seen, &mut result.aliases);
    }
    for (&taffy_id, &pointer) in node_map {
        // SAFETY: callers provide the live node map for the duration of this
        // tree build; reading the concrete identity does not retain a borrow.
        let concrete = unsafe { (&*pointer).concrete_type_name() };
        result
            .nodes
            .entry(pointer.cast::<()>())
            .or_default()
            .push((taffy_id, concrete));
    }
    result
}

fn byte_to_grapheme_index(text: &str, byte: usize) -> usize {
    let byte = crate::text_selection::grapheme_boundary_at_or_before(text, byte.min(text.len()));
    grapheme_indices(text)
        .take_while(|(start, _)| *start < byte)
        .map(|(_, grapheme)| grapheme.len().div_ceil(u8::MAX as usize))
        .sum()
}

fn accessible_character_lengths(text: &str) -> Vec<u8> {
    let mut lengths = Vec::new();
    for grapheme in graphemes(text) {
        let mut remaining = grapheme.len();
        while remaining > 0 {
            let chunk = remaining.min(u8::MAX as usize);
            lengths.push(u8::try_from(chunk).unwrap_or(u8::MAX));
            remaining = remaining.saturating_sub(chunk);
        }
    }
    lengths
}

/// `focused` is the taffy id of the focused widget; `focus` falls back to
/// the synthetic root when nothing (or a stale id) matches — AccessKit
/// requires `focus` to be a real node. `scale` multiplies rects into
/// physical-pixel bounds; pass `1.0` for logical coords.
///
/// Always a full tree; AccessKit's adapters suppress events for unchanged
/// nodes, so the cost is processing, not spurious AT chatter.
pub fn build_tree_update<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
) -> TreeUpdate {
    build_tree_update_with_resources(root, node_map, focused, scale, &[])
}

/// Build the widget accessibility tree and append the exact same live
/// resource/action surface used by the palette, shell and collaboration API.
/// Immediate operations expose `Click`; prompted operations expose `SetValue`
/// with the supplied value becoming their argument.
pub fn build_tree_update_with_resources<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    resources: &[tilo_actions::Resource],
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, false>(
        &[root],
        node_map,
        focused,
        scale,
        resources,
        None,
    )
}

/// Adapt the redacted semantic tree to native-provider requirements.
/// AccessKit Windows 0.35 offers `IValueProvider` only when a node has a value.
/// A fixed empty placeholder enables `SetValue` on password fields without
/// disclosing their text, length, selection, or whether they contain anything.
/// The shared semantic tree continues to carry no password value at all.
#[cfg(feature = "native")]
pub(crate) fn native_tree_update(update: TreeUpdate) -> TreeUpdate {
    #[cfg(windows)]
    {
        let mut update = update;
        for (_, node) in &mut update.nodes {
            if node.role() == AkRole::PasswordInput {
                node.set_value("");
            }
        }
        update
    }
    #[cfg(not(windows))]
    {
        update
    }
}
/// Host traversal retaining the originating component through every callback.
pub(crate) fn build_tree_update_with_resources_bound<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    resources: &[tilo_actions::Resource],
    custody: &Custody,
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(
        &[root],
        node_map,
        focused,
        scale,
        resources,
        Some(custody),
    )
}

pub(crate) fn build_tree_update_bound<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    custody: &Custody,
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(
        &[root],
        node_map,
        focused,
        scale,
        &[],
        Some(custody),
    )
}

pub(crate) fn build_presentation_tree_bound<S: BuildHasher>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    custody: &Custody,
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(
        roots,
        node_map,
        focused,
        scale,
        &[],
        Some(custody),
    )
}

/// Multiple presentation roots retain ordinary live resource nodes when their
/// owner is interactive; callers supply only currently presented resources.
pub(crate) fn build_presentation_with_resources_bound<S: BuildHasher>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    resources: &[tilo_actions::Resource],
    custody: &Custody,
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(
        roots,
        node_map,
        focused,
        scale,
        resources,
        Some(custody),
    )
}

fn build_tree_update_with_resources_impl<S: BuildHasher, const BOUND: bool>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    resources: &[tilo_actions::Resource],
    custody: Option<&Custody>,
) -> TreeUpdate {
    build_with_targets::<S, BOUND>(roots, node_map, focused, scale, resources, custody).0
}

/// Whose lifetime each exported layout node answers to: a bound component's
/// or a legacy record owner's, as the host admitted them.
pub(crate) type Custody = HashMap<TaffyNodeId, uuid::Uuid>;

/// Where each exported widget and synthetic node points in the live tree.
type Targets = HashMap<AkNodeId, A11yTarget>;

fn build_with_targets<S: BuildHasher, const BOUND: bool>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    resources: &[tilo_actions::Resource],
    custody: Option<&Custody>,
) -> (TreeUpdate, Targets) {
    // Reverse map so we can look up a stable id for any &dyn Widget during the walk.
    let ptr_to_taffy = reverse_node_map::<S, BOUND>(roots, node_map);
    let mut walk = Walk::<BOUND> {
        ptr_to_taffy: &ptr_to_taffy,
        scale,
        focused,
        identities: crate::semantic_identity::IdentityScope::new::<BOUND>(roots),
        legacy_boundaries: legacy_boundaries(roots),
        custody,
        nodes: Vec::new(),
        targets: HashMap::new(),
        focused_id: None,
        active_descendant: None,
    };

    // The synthetic window root gathers the top-level accessible nodes.
    let mut root_node = AkNode::new(AkRole::Window);
    root_node.set_label("Tilo application window");

    let mut root_children: Vec<AkNodeId> = Vec::new();
    let window = SemanticId::root();
    let mut root_counts = HashMap::new();
    for root in roots {
        walk.visit(
            *root,
            &window,
            "",
            &mut root_counts,
            OwnerScope::default(),
            &mut root_children,
        );
    }
    let Walk {
        mut nodes,
        targets,
        focused_id,
        active_descendant,
        ..
    } = walk;
    let mut resource_ids = HashMap::new();
    append_resource_nodes(resources, &mut nodes, &mut root_children, &mut resource_ids);
    root_node.set_children(root_children);
    nodes.push((ROOT_ID, root_node));

    // AccessKit panics the AT if `focus` references a node absent from this
    // update, so only honour a focus the walk actually emitted.
    //
    // The active descendant wins over the widget itself: a canvas app that
    // tells us which clip the user is on should put the reader on that clip,
    // not on the whole timeline. It is only ever set for the focused widget, so
    // it cannot pull focus away from wherever the keyboard actually is.
    let focus = active_descendant.or(focused_id).unwrap_or(ROOT_ID);

    (
        TreeUpdate {
            nodes,
            tree: Some(TreeInfo::new(ROOT_ID)),
            tree_id: accesskit::TreeId::ROOT,
            focus,
        },
        targets,
    )
}

fn append_resource_nodes(
    resources: &[tilo_actions::Resource],
    nodes: &mut Vec<(AkNodeId, AkNode)>,
    parent_children: &mut Vec<AkNodeId>,
    claimed: &mut HashMap<AkNodeId, String>,
) {
    for resource in resources {
        let id = resource_node_id(resource.id.as_str());
        if let Some(existing) = claimed.insert(id, resource.id.0.clone()) {
            tracing::error!(
                node_id = id.0,
                existing,
                duplicate = resource.id.as_str(),
                "resource accessibility id collision; refusing duplicate"
            );
            continue;
        }
        let mut node = AkNode::new(AkRole::Group);
        node.set_label(resource.label.clone());
        node.set_description(format!("{} resource", resource.kind));
        let mut children = Vec::new();

        for action in &resource.actions {
            let action_id = resource_action_node_id(resource.id.as_str(), &action.id);
            let address = format!(
                "{}{}{}",
                resource.id.as_str(),
                tilo_actions::KEY_SEP,
                action.id
            );
            if let Some(existing) = claimed.insert(action_id, address.clone()) {
                tracing::error!(
                    node_id = action_id.0,
                    existing,
                    duplicate = address,
                    "resource action accessibility id collision; refusing duplicate"
                );
                continue;
            }
            let mut action_node = AkNode::new(AkRole::Button);
            action_node.set_label(action.label.clone());
            let operation = action.kind.as_str();
            match &action.input {
                tilo_actions::ActionInput::Immediate(_)
                | tilo_actions::ActionInput::Deferred { .. } => {
                    action_node.add_action(AkAction::Click);
                    if let Some(description) = &action.description {
                        action_node.set_description(format!("{description} ({operation})"));
                    } else {
                        action_node.set_description(format!("{operation} operation"));
                    }
                }
                tilo_actions::ActionInput::Prompted { prompt, .. } => {
                    action_node.add_action(AkAction::SetValue);
                    let description = action.description.as_deref().unwrap_or(prompt);
                    action_node.set_description(format!("{description} ({operation})"));
                }
            }
            nodes.push((action_id, action_node));
            children.push(action_id);
        }

        append_resource_nodes(&resource.children, nodes, &mut children, claimed);
        node.set_children(children);
        nodes.push((id, node));
        parent_children.push(id);
    }
}

/// DFS helper: if `w.accessibility()` is `None` (presentational), reparent
/// the children's ids straight onto `parent_children` instead of emitting a node.
// The walk threads three accumulators (nodes, the parent's child list, the
// active descendant) alongside the tree it is reading; bundling them would only
// move the arity into a struct.
/// One export walk: the nodes it emits, where each points, and the shared
/// identity derivation that names them.
struct Walk<'a, const BOUND: bool> {
    ptr_to_taffy: &'a WidgetNodeMap,
    scale: f64,
    focused: Option<TaffyNodeId>,
    identities: crate::semantic_identity::IdentityScope,
    /// Layout nodes where a legacy record owner's custody begins, with that
    /// owner's lifetime.
    legacy_boundaries: HashMap<TaffyNodeId, uuid::Uuid>,
    /// The host's own record of whose custody each exported layout node is
    /// in, when the host supplies it. It wins over anything derived here.
    custody: Option<&'a Custody>,
    nodes: Vec<(AkNodeId, AkNode)>,
    targets: Targets,
    /// The focused widget's node, when it was emitted.
    focused_id: Option<AkNodeId>,
    /// Set when the *focused* widget marks one of its synthetic descendants
    /// focused — the active-descendant target (see the module docs).
    active_descendant: Option<AkNodeId>,
}

/// Which owner a widget's callbacks answer to, and so whose lifetime scopes
/// its AccessKit id. Derived exactly as the host derives callback custody
/// (`BoundNodeMap::rebuild_roots_with_retained`): a component binding is
/// inherited until a widget resets it; a legacy record owner's custody
/// begins at the layout node the owner declares — not at the widget that
/// declares it — and ends at any binding or reset below.
#[derive(Clone, Copy, Debug, Default)]
struct OwnerScope {
    binding: Option<uuid::Uuid>,
    legacy: Option<uuid::Uuid>,
}

impl OwnerScope {
    fn enter(self, w: &dyn Widget, legacy_boundary: Option<uuid::Uuid>) -> Self {
        let resets = w.resets_callback_binding();
        let binding = if resets {
            None
        } else {
            w.callback_binding()
                .map(|binding| binding.lifetime_id)
                .or(self.binding)
        };
        let legacy = if binding.is_some() || resets {
            None
        } else {
            legacy_boundary.or(self.legacy)
        };
        Self { binding, legacy }
    }

    /// Custody the host decided for a node, inherited by what below it has
    /// no layout node of its own.
    fn held(lifetime: Option<uuid::Uuid>) -> Self {
        Self {
            binding: lifetime,
            legacy: None,
        }
    }

    fn lifetime(self) -> Option<uuid::Uuid> {
        self.binding.or(self.legacy)
    }
}

/// Every legacy owner boundary declared anywhere under `roots`, keyed by the
/// layout node the owner names.
fn legacy_boundaries(roots: &[&dyn Widget]) -> HashMap<TaffyNodeId, uuid::Uuid> {
    fn collect(widget: &dyn Widget, out: &mut HashMap<TaffyNodeId, uuid::Uuid>) {
        if let Some((node, owner)) = widget.legacy_callback_boundary()
            && let Some(lifetime) = owner.lifetime_id()
        {
            out.insert(node, lifetime);
        }
        for child in widget.children() {
            collect(child, out);
        }
    }
    let mut out = HashMap::new();
    for root in roots {
        collect(*root, &mut out);
    }
    out
}

/// Where a synthetic node sits under the widget that owns it.
struct SyntheticPlace<'p> {
    owner: Option<TaffyNodeId>,
    owner_focused: bool,
    key_path: &'p [u64],
    ordinal_path: &'p [usize],
    /// Whose custody the owning widget is in.
    scope: OwnerScope,
}

impl<const BOUND: bool> Walk<'_, BOUND> {
    /// A presentational widget (`accessibility() == None`) collapses out and
    /// its children reparent onto the nearest accessible ancestor — the
    /// same rule, and the same identities, as the retained semantic tree.
    fn visit(
        &mut self,
        w: &dyn Widget,
        semantic_parent: &SemanticId,
        path_base: &str,
        sibling_counts: &mut HashMap<String, usize>,
        scope: OwnerScope,
        parent_children: &mut Vec<AkNodeId>,
    ) {
        let Ok(_entry) = crate::bound_dispatch::enter_traversal::<BOUND>(w) else {
            return;
        };
        let taffy_id = self.ptr_to_taffy.get(w);
        let scope = match (self.custody, taffy_id) {
            // The host decided this node's custody; nothing is re-derived.
            (Some(custody), Some(node)) => OwnerScope::held(custody.get(&node).copied()),
            (Some(_), None) => scope,
            (None, _) => scope.enter(
                w,
                taffy_id.and_then(|node| self.legacy_boundaries.get(&node).copied()),
            ),
        };
        let lifetime = scope.lifetime();
        let path = crate::semantic_identity::structural_path(w, path_base, sibling_counts);
        let mut child_counts = HashMap::new();
        let Some(access_node) = self.identities.access_of(w) else {
            for child in w.children() {
                self.visit(
                    child,
                    semantic_parent,
                    &path,
                    &mut child_counts,
                    scope,
                    parent_children,
                );
            }
            return;
        };
        let identified = self
            .identities
            .enter(w, &access_node, semantic_parent, path);
        let id = widget_node_id(&identified.id, lifetime);
        if let Some(taffy_id) = taffy_id {
            self.targets.insert(id, A11yTarget::Widget(taffy_id));
            if Some(taffy_id) == self.focused {
                self.focused_id = Some(id);
            }
        }

        let mut ak = AkNode::new(map_role(access_node.role));
        apply_semantics(&mut ak, &access_node, w, self.scale);

        let mut my_children: Vec<AkNodeId> = Vec::new();
        let mut hosted = HostedByIndex::new();
        for (child_index, child) in w.children().into_iter().enumerate() {
            if let Some(mut child_path) = w.accessibility_child_path(child_index)
                && !child_path.is_empty()
            {
                let parent_index = child_path.remove(0);
                hosted
                    .entry(parent_index)
                    .or_default()
                    .push((child_path, child));
            } else {
                self.visit(
                    child,
                    &identified.id,
                    &identified.children_base,
                    &mut child_counts,
                    scope,
                    &mut my_children,
                );
            }
        }
        // Synthetic children with no backing widget — a table's rows/cells, a
        // timeline's clips. Only the widget that actually holds keyboard focus
        // may nominate an active descendant.
        let owner_focused = taffy_id.is_some() && taffy_id == self.focused;
        for (ordinal, synth) in access_node.children.iter().enumerate() {
            let key = synth.key.unwrap_or(ordinal as u64);
            self.visit_synthetic(
                synth,
                &identified.id,
                id,
                ordinal,
                &SyntheticPlace {
                    owner: taffy_id,
                    owner_focused,
                    key_path: &[key],
                    ordinal_path: &[ordinal],
                    scope,
                },
                hosted.remove(&ordinal).unwrap_or_default(),
                &mut my_children,
            );
        }
        self.identities.leave();
        if !access_node.sensitivity.is_secret()
            && let (Some(value), Some(selection)) = (&access_node.value, access_node.text_selection)
        {
            let run_id = text_run_node_id(id);
            if let Some(taffy_id) = taffy_id {
                self.targets.insert(run_id, A11yTarget::Widget(taffy_id));
            }
            let mut run = AkNode::new(AkRole::TextRun);
            run.set_value(value.clone());
            run.set_character_lengths(accessible_character_lengths(value));
            let rect = w.get_rect();
            let scale = self.scale;
            if rect.width() > 0.0 || rect.height() > 0.0 {
                run.set_bounds(AkRect::new(
                    rect.x0 * scale,
                    rect.y0 * scale,
                    rect.x1 * scale,
                    rect.y1 * scale,
                ));
            }
            ak.set_text_selection(AkTextSelection {
                anchor: AkTextPosition {
                    node: run_id,
                    character_index: byte_to_grapheme_index(value, selection.anchor),
                },
                focus: AkTextPosition {
                    node: run_id,
                    character_index: byte_to_grapheme_index(value, selection.focus),
                },
            });
            my_children.insert(0, run_id);
            self.nodes.push((run_id, run));
        }
        ak.set_children(my_children);

        self.nodes.push((id, ak));
        parent_children.push(id);
    }

    /// A node with no backing [`Widget`] — a table row/cell, a clip on a
    /// timeline track. Carries no geometry; everything else is applied as for
    /// a real node.
    ///
    /// The id chains `parent_id` with the node's [`key`](AccessNode::key) — or
    /// its `ordinal` when it has none — so it survives repaints and unrelated
    /// changes elsewhere in the tree (see [`synthetic_node_id`]). The owning
    /// widget is recorded with both paths so an action reaches it.
    fn visit_synthetic(
        &mut self,
        access: &AccessNode,
        semantic_parent: &SemanticId,
        parent_id: AkNodeId,
        ordinal: usize,
        place: &SyntheticPlace<'_>,
        hosted: Vec<HostedWidget<'_>>,
        parent_children: &mut Vec<AkNodeId>,
    ) {
        let scope = place.scope;
        let key = access.key.unwrap_or(ordinal as u64);
        let id = synthetic_node_id(parent_id, key);
        let semantic_id = crate::semantic_identity::synthetic_id(semantic_parent, ordinal, access);
        if let Some(owner) = place.owner {
            self.targets.insert(
                id,
                A11yTarget::Synthetic {
                    owner,
                    key_path: place.key_path.to_vec(),
                    ordinal_path: place.ordinal_path.to_vec(),
                    semantic_target: access.action_target.clone(),
                },
            );
        }

        let mut ak = AkNode::new(map_role(access.role));
        apply_semantics_common(&mut ak, access);
        apply_synthetic_bounds(&mut ak, access, self.scale);

        if place.owner_focused && access.state.focused && self.active_descendant.is_none() {
            self.active_descendant = Some(id);
        }

        let mut my_children: Vec<AkNodeId> = Vec::new();
        let direct = hosted
            .iter()
            .filter(|(path, _)| path.is_empty())
            .map(|(_, child)| *child)
            .collect::<Vec<_>>();
        if direct.is_empty() {
            let mut nested = HostedByIndex::new();
            for (mut path, child) in hosted {
                let child_index = path.remove(0);
                nested.entry(child_index).or_default().push((path, child));
            }
            for (i, child) in access.children.iter().enumerate() {
                let mut key_path = place.key_path.to_vec();
                key_path.push(child.key.unwrap_or(i as u64));
                let mut ordinal_path = place.ordinal_path.to_vec();
                ordinal_path.push(i);
                self.visit_synthetic(
                    child,
                    &semantic_id,
                    id,
                    i,
                    &SyntheticPlace {
                        owner: place.owner,
                        owner_focused: place.owner_focused,
                        key_path: &key_path,
                        ordinal_path: &ordinal_path,
                        scope,
                    },
                    nested.remove(&i).unwrap_or_default(),
                    &mut my_children,
                );
            }
        } else {
            let mut counts = HashMap::new();
            for child in direct {
                self.visit(
                    child,
                    &semantic_id,
                    semantic_id.as_str(),
                    &mut counts,
                    scope,
                    &mut my_children,
                );
            }
        }
        ak.set_children(my_children);

        self.nodes.push((id, ak));
        parent_children.push(id);
    }
}

fn apply_semantics(ak: &mut AkNode, access: &AccessNode, w: &dyn Widget, scale: f64) {
    apply_semantics_common(ak, access);

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

fn apply_synthetic_bounds(ak: &mut AkNode, access: &AccessNode, scale: f64) {
    if let Some(bounds) = access.bounds {
        ak.set_bounds(AkRect::new(
            bounds.x0 * scale,
            bounds.y0 * scale,
            bounds.x1 * scale,
            bounds.y1 * scale,
        ));
    }
}

/// The widget-independent half of [`apply_semantics`], also shared with
/// [`visit_synthetic`] (table rows/cells), which has no geometry.
fn apply_semantics_common(ak: &mut AkNode, access: &AccessNode) {
    let secret = access.sensitivity.is_secret();
    if secret && matches!(access.role, Role::TextInput | Role::MultilineTextInput) {
        ak.set_role(AkRole::PasswordInput);
    }
    // A secret node whose name is its content (a revealed one-time code in a
    // label) keeps that content out of the tree as well; a secret field's
    // name is only its label.
    if let Some(name) = &access.name
        && (!secret || access.presents_value())
    {
        ak.set_label(name.clone());
    }
    if let Some(desc) = &access.description {
        ak.set_description(desc.clone());
    }
    // This must be part of the platform tree, not only the built-in reader's
    // narration pass.  `accesskit_unix` turns these into AT-SPI live-region
    // attributes/events, which is what Orca and braille clients observe when
    // a status message changes without moving keyboard focus.  Atomic is
    // intentional: an AccessNode is already the complete, user-authored
    // announcement rather than a partial DOM fragment.
    if let Some(politeness) = access.live {
        ak.set_live(match politeness {
            crate::a11y::Politeness::Polite => AkLive::Polite,
            crate::a11y::Politeness::Assertive => AkLive::Assertive,
        });
        ak.set_live_atomic();
    }
    // AccessKit derives native Label names from their value, not their label.
    // Our static-text widgets conventionally supply that text through `name`.
    let value = match access.role {
        Role::Label | Role::StaticText => access.value.as_ref().or(access.name.as_ref()),
        _ => access.value.as_ref(),
    };
    // Never expose a secret (password) value, even if the widget mistakenly populated one.
    if !secret && let Some(value) = value {
        ak.set_value(value.clone());
    }

    if let Role::Heading { level } = access.role {
        ak.set_level(level as usize);
    }

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

    if access.action_target.is_some() && access.bounds.is_some() && !access.state.disabled {
        ak.add_action(AkAction::ScrollIntoView);
    }

    if access.state.focusable && !access.state.disabled {
        ak.add_action(AkAction::Focus);
    }
    if access.state.context_menu && !access.state.disabled {
        ak.add_action(AkAction::ShowContextMenu);
    }
    // Click is gated on the roles that can actually be operated. Offering it
    // on every node — which this used to do — makes an AT read "clickable" on
    // a plain label. `ListItem` is in the list because a synthetic segmented
    // control's segments are list items, and without it an AT could see them
    // but not press them.
    if !access.state.disabled
        && matches!(
            access.role,
            Role::Button
                | Role::ToggleButton
                | Role::Checkbox
                | Role::RadioButton
                | Role::Switch
                | Role::Tab
                | Role::MenuItem
                | Role::ListItem
                | Role::TreeItem
                | Role::Link
        )
    {
        ak.add_action(AkAction::Click);
    }
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
        Role::TextInput | Role::MultilineTextInput
            if !access.state.read_only && !access.state.disabled =>
        {
            ak.add_action(AkAction::SetValue);
            ak.add_action(AkAction::SetTextSelection);
        }
        // A terminal transcript is selectable but never replaceable as one
        // editable value. Input is a stream, not a SetValue mutation.
        Role::Terminal if !access.state.disabled => {
            ak.add_action(AkAction::SetTextSelection);
        }
        _ => {}
    }
}

fn apply_state(ak: &mut AkNode, access: &AccessNode) {
    let s = &access.state;

    if s.focused {
        // Focus is carried by the tree's `focus` field, nothing per-node here.
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
    // AccessKit 0.24 has no `multiline` flag; expressed via `MultilineTextInput` (see `map_role`).
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
    // Clear `selected` explicitly (not just omit) for selectable roles so an
    // AT doesn't read "not selected" on unrelated controls.
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

    // Configurable accessibility()/children/rect, to build arbitrary trees
    // without real widgets' layout machinery.
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
        fn sync(&mut self, _s: &mut crate::widget::AppModel, _p: &crate::widget::ModelPath) {}
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
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
        fn children(&self) -> Vec<&dyn Widget> {
            self.kids.iter().map(std::convert::AsRef::as_ref).collect()
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

    #[repr(C)]
    struct AliasWrapper {
        child: TestWidget,
    }

    impl Widget for AliasWrapper {
        fn sync(&mut self, _s: &mut crate::widget::AppModel, _p: &crate::widget::ModelPath) {}
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            theme: &Theme,
            nodes: &mut HashMap<TaffyNodeId, *mut dyn Widget>,
        ) -> TaffyNodeId {
            self.child.build_layout(tree, theme, nodes)
        }
        fn apply_layout(
            &mut self,
            _tree: &TaffyTree<()>,
            _node: TaffyNodeId,
            _origin: vello::kurbo::Point,
        ) {
        }
        fn draw(
            &mut self,
            _scene: &mut vello::Scene,
            _transform: vello::kurbo::Affine,
            _text: &mut crate::text_render::TextRenderer,
            _theme: &Theme,
            _presence: &crate::presence::PresenceCtx,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            vec![&self.child]
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            vec![&mut self.child]
        }
        fn name(&self) -> &str {
            "AliasWrapper"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            self.child.rect
        }
        fn accessibility(&self) -> Option<AccessNode> {
            self.child.access.clone()
        }
    }

    #[test]
    fn node_ids_stay_distinct_and_stable_across_tree_changes() {
        // Case `wrapper_and_first_field_have_distinct_nodes_and_child_keeps_focus`.
        {
            let mut wrapper = AliasWrapper {
                child: TestWidget::new(Some(AccessNode::new(Role::Button))),
            };
            assert_eq!(
                std::ptr::from_ref(&wrapper).cast::<()>(),
                std::ptr::from_ref(&wrapper.child).cast::<()>()
            );
            let (mut tree, mut nodes, child) = lay_out(&mut wrapper);
            let update = build_tree_update(&wrapper, &nodes, Some(child), 1.0);
            let ids: std::collections::HashSet<_> =
                update.nodes.iter().map(|(id, _)| *id).collect();
            assert_eq!(
                ids.len(),
                update.nodes.len(),
                "every semantic node has a distinct id"
            );
            assert_eq!(update.focus, widget_ak_id(&wrapper, &nodes, child).unwrap());
            assert_eq!(
                update.nodes.len(),
                3,
                "window, wrapper and child all remain accessible"
            );
            for (id, node) in &update.nodes {
                assert!(
                    !node.children().contains(id),
                    "node cannot be its own child"
                );
            }
            // If both concrete widgets register their own layout nodes, neither
            // registration may overwrite the other in the reverse lookup.
            let parent = tree.new_leaf(taffy::Style::default()).unwrap();
            nodes.insert(parent, std::ptr::from_mut::<dyn Widget>(&mut wrapper));
            for focused in [parent, child] {
                let update = build_tree_update(&wrapper, &nodes, Some(focused), 1.0);
                assert_eq!(
                    update.focus,
                    widget_ak_id(&wrapper, &nodes, focused).unwrap()
                );
                assert!(
                    update
                        .nodes
                        .iter()
                        .any(|(id, _)| *id == widget_ak_id(&wrapper, &nodes, parent).unwrap())
                );
                assert!(
                    update
                        .nodes
                        .iter()
                        .any(|(id, _)| *id == widget_ak_id(&wrapper, &nodes, child).unwrap())
                );
                let ids: std::collections::HashSet<_> =
                    update.nodes.iter().map(|(id, _)| *id).collect();
                assert_eq!(ids.len(), update.nodes.len());
            }
        }

        // Case `a_child_at_its_parents_address_keeps_its_own_node_id`:
        // The reverse node map must tell a container apart from the child it
        // starts with: keyed by data pointer alone, the two collided and the
        // child was exported under its parent's taffy id, so an activation
        // aimed at the child's own id found no node.
        {
            let mut root = FirstFieldHost {
                child: TestWidget::new(Some(AccessNode::new(Role::Button).with_name("Bump")))
                    .with_rect(0.0, 0.0, 40.0, 20.0),
                pad: 0,
            };
            let (_taffy, node_map, root_node) = lay_out(&mut root);
            assert!(
                std::ptr::eq(
                    std::ptr::from_ref::<dyn Widget>(&root).cast::<()>(),
                    std::ptr::from_ref::<dyn Widget>(&root.child).cast::<()>()
                ),
                "the fixture must reproduce the shared address"
            );
            let child_node = *node_map
                .keys()
                .find(|node| **node != root_node)
                .expect("the child has its own taffy node");
            let child_id = widget_ak_id(&root, &node_map, child_node).expect("child exported");
            let root_id = widget_ak_id(&root, &node_map, root_node).expect("root exported");
            assert_ne!(child_id, root_id);

            let update = build_tree_update(&root, &node_map, None, 1.0);
            let role_of = |wanted: AkNodeId| {
                update
                    .nodes
                    .iter()
                    .find(|(id, _)| *id == wanted)
                    .map(|(_, node)| node.role())
            };
            assert_eq!(
                role_of(child_id),
                Some(AkRole::Button),
                "child exported under its own id"
            );
            assert_eq!(
                role_of(root_id),
                Some(AkRole::GenericContainer),
                "parent exported under its own id"
            );
            assert!(
                matches!(
                    resolve_node_id(&root, &node_map, child_id),
                    Some(A11yTarget::Widget(node)) if node == child_node
                ),
                "the child's id resolves back to the child's taffy node"
            );
        }

        // Case `a_synthetic_nodes_id_survives_changes_elsewhere_in_the_tree`.
        {
            // Two independent widgets with synthetic contents. Adding a clip to the
            // FIRST must not renumber the second's rows: an AT holding a reference
            // to "Beta / two" would otherwise silently start pointing elsewhere.
            let mut first = Box::new(TestWidget::new(Some(canvas(vec![clip("a", false)]))));
            let first_ptr: *mut TestWidget = &raw mut *first;
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

        // Case `an_explicitly_keyed_node_keeps_its_id_when_it_moves`.
        {
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

        // Case `synthetic_ids_never_collide_with_widget_ids`.
        {
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
    }

    #[test]
    fn a_legacy_owners_custody_starts_at_the_node_it_names_not_at_the_declarer() {
        let widget = TestWidget::new(Some(AccessNode::new(Role::Group)));
        let owner = uuid::Uuid::from_u128(7);
        let outside = OwnerScope::default().enter(&widget, None);
        assert_eq!(
            outside.lifetime(),
            None,
            "the declaring chrome is not in custody"
        );
        let boundary = outside.enter(&widget, Some(owner));
        assert_eq!(
            boundary.lifetime(),
            Some(owner),
            "custody begins at the named node"
        );
        let below = boundary.enter(&widget, None);
        assert_eq!(below.lifetime(), Some(owner), "and holds for its subtree");
        let replaced = outside.enter(&widget, Some(uuid::Uuid::from_u128(8)));
        let same_semantic = SemanticId("t:body".into());
        assert_ne!(
            widget_node_id(&same_semantic, boundary.lifetime()),
            widget_node_id(&same_semantic, replaced.lifetime()),
            "a replacement owner's widget never answers to the old id"
        );
        assert_eq!(
            widget_node_id(&same_semantic, outside.lifetime()),
            widget_node_id(&same_semantic, None)
        );
    }

    #[test]
    fn widget_ids_survive_a_rebuild_in_a_fresh_layout_arena() {
        let build = || {
            TestWidget::new(Some(AccessNode::new(Role::Group).with_name("Form")))
                .with_child(Box::new(TestWidget::new(Some(
                    AccessNode::new(Role::Button).with_name("Save"),
                ))))
                .with_child(Box::new(TestWidget::new(Some(
                    AccessNode::new(Role::Button).with_name("Cancel"),
                ))))
        };
        let ids_of = |update: &TreeUpdate| {
            let mut ids: Vec<(Option<String>, AkNodeId)> = update
                .nodes
                .iter()
                .map(|(id, node)| (node.label().map(str::to_owned), *id))
                .collect();
            ids.sort();
            ids
        };
        let mut first = build();
        let (_first_arena, first_map, _) = lay_out(&mut first);
        let before = build_tree_update(&first, &first_map, None, 1.0);

        // A fresh arena with unrelated nodes in front: every layout id moves.
        let mut arena = TaffyTree::<()>::new();
        for _ in 0..9 {
            let _ = arena.new_leaf(taffy::Style::default());
        }
        let mut second = build();
        let mut second_map = HashMap::new();
        let root_node = second.build_layout(&mut arena, &Theme::dark(), &mut second_map);
        assert!(
            second_map.keys().all(|node| !first_map.contains_key(node)),
            "the rebuild really used different layout nodes"
        );
        let after = build_tree_update(&second, &second_map, None, 1.0);
        assert_eq!(
            ids_of(&before),
            ids_of(&after),
            "identity survives the rebuild"
        );

        let save = after
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Save"))
            .map(|(id, _)| *id)
            .expect("save exported");
        let save_node = second_map
            .iter()
            .find_map(|(node, widget)| {
                // SAFETY: the map was built from `second`, alive for this test.
                (unsafe { (**widget).accessibility() })
                    .and_then(|access| access.name)
                    .is_some_and(|name| name == "Save")
                    .then_some(*node)
            })
            .expect("save laid out");
        assert_eq!(
            resolve_node_id(&second, &second_map, save),
            Some(A11yTarget::Widget(save_node)),
            "the stable id resolves to the widget's current layout node"
        );
        assert_ne!(root_node, save_node);
    }

    /// Exactly as the App does before `build_tree_update`.
    fn lay_out(
        root: &mut dyn Widget,
    ) -> (
        TaffyTree<()>,
        HashMap<taffy::NodeId, *mut dyn Widget>,
        taffy::NodeId,
    ) {
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let theme = Theme::dark();
        let root_node = root.build_layout(&mut taffy, &theme, &mut node_map);
        (taffy, node_map, root_node)
    }

    /// A container whose first field is a child widget starts at the very
    /// address of that child; `#[repr(C)]` pins the layout the default
    /// representation may also pick.
    #[repr(C)]
    struct FirstFieldHost {
        child: TestWidget,
        pad: u64,
    }

    impl Widget for FirstFieldHost {
        fn sync(&mut self, _s: &mut crate::widget::AppModel, _p: &crate::widget::ModelPath) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let kid = self.child.build_layout(taffy, theme, node_map);
            let node = taffy
                .new_with_children(taffy::Style::default(), &[kid])
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
        fn children(&self) -> Vec<&dyn Widget> {
            vec![&self.child]
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            vec![&mut self.child]
        }
        fn name(&self) -> &str {
            "first-field-host"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::new(0.0, 0.0, 200.0, 100.0)
        }
        fn accessibility(&self) -> Option<AccessNode> {
            Some(AccessNode::new(Role::Generic))
        }
    }

    #[test]
    fn roles_states_bounds_and_labels_map_to_accesskit() {
        // Case `maps_every_role_to_a_concrete_accesskit_role`.
        {
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
                (Role::Terminal, AkRole::Terminal),
                (Role::Generic, AkRole::GenericContainer),
            ];
            for (ours, theirs) in cases {
                assert_eq!(map_role(ours), theirs, "role {ours:?} mapped wrong");
            }
        }

        // Case `state_flags_map_to_accesskit`.
        {
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

        // Case `heading_level_is_set`.
        {
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

        // Case `bounds_carry_through_with_scale`.
        {
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

        // Case `static_labels_export_the_value_read_by_native_accessibility`.
        {
            for role in [Role::Label, Role::StaticText] {
                let text = "SQLite · Grün 🌍 · 1 rows";
                let mut root = TestWidget::new(Some(AccessNode::new(role).with_name(text)));
                let (_taffy, node_map, _) = lay_out(&mut root);
                let update = build_tree_update(&root, &node_map, None, 1.0);
                let (_, label) = update
                    .nodes
                    .iter()
                    .find(|(_, node)| node.role() == AkRole::Label)
                    .unwrap();
                assert_eq!(
                    label.value(),
                    Some(text),
                    "AccessKit native label names come from their value"
                );
            }
        }

        // Case `every_update_includes_the_window_root_and_a_tree`.
        {
            let mut root = TestWidget::new(None);
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            assert!(update.tree.is_some());
            assert_eq!(update.tree.as_ref().unwrap().root, ROOT_ID);
            assert!(update.nodes.iter().any(|(id, _)| *id == ROOT_ID));
        }
    }

    #[test]
    fn supported_actions_are_advertised_and_round_trip() {
        // Case `action_round_trips_through_accesskit`.
        {
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

        // Case `enabled_focusable_widget_supports_focus_and_click`.
        {
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

        // Case `context_menu_is_advertised_only_when_declared`.
        {
            let row = TestWidget::new(Some(
                AccessNode::new(Role::ListItem)
                    .with_name("Ada Lovelace")
                    .with_state(AccessState::new().focusable(true).context_menu(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(row));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (_, node) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == AkRole::ListItem)
                .expect("list item reaches AccessKit");
            assert!(node.supports_action(AkAction::ShowContextMenu));
        }

        // Case `slider_supports_increment_decrement`.
        {
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

        // Case `live_resources_are_accessible_through_the_same_action_addresses`.
        {
            let immediate = tilo_actions::ActionSpec::operation(
                "refresh",
                "Refresh",
                tilo_actions::OperationKind::Effect,
                "test.refresh",
                &serde_json::json!({}),
            )
            .unwrap();
            let prompted = tilo_actions::ActionSpec::prompted_operation(
                "find",
                "Find",
                "Text to find",
                "query",
                tilo_actions::OperationKind::View,
                "test.find",
                |value| tilo_actions::EncodedAction::encode_operation("test.find", &value).unwrap(),
            );
            let resources =
                [
                    tilo_actions::Resource::new("document:1", "test.document", "Document")
                        .with_action(immediate)
                        .with_action(prompted),
                ];
            let mut root = TestWidget::new(None);
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update_with_resources(&root, &node_map, None, 1.0, &resources);

            let resource_id = resource_node_id("document:1");
            let refresh_id = resource_action_node_id("document:1", "refresh");
            let find_id = resource_action_node_id("document:1", "find");
            let root_node = update.nodes.iter().find(|(id, _)| *id == ROOT_ID).unwrap();
            assert!(root_node.1.children().contains(&resource_id));
            let resource = update
                .nodes
                .iter()
                .find(|(id, _)| *id == resource_id)
                .unwrap();
            assert_eq!(resource.1.role(), AkRole::Group);
            assert_eq!(resource.1.children(), &[refresh_id, find_id]);
            let refresh = update
                .nodes
                .iter()
                .find(|(id, _)| *id == refresh_id)
                .unwrap();
            let find = update.nodes.iter().find(|(id, _)| *id == find_id).unwrap();
            assert!(refresh.1.supports_action(AkAction::Click));
            assert!(!refresh.1.supports_action(AkAction::SetValue));
            assert!(find.1.supports_action(AkAction::SetValue));
            assert!(!find.1.supports_action(AkAction::Click));
            assert_eq!(
                resource_action_node_id("document:1", "refresh"),
                refresh_id,
                "action identity must remain stable between frames"
            );
        }
    }

    #[test]
    fn tree_shape_collapses_presentational_nodes_and_carries_synthetic_and_live_regions() {
        // Case `presentational_containers_collapse_and_reparent`.
        {
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

            // The window root owns BOTH the Group and the reparented Label.
            let (_, window) = update.nodes.iter().find(|(id, _)| *id == ROOT_ID).unwrap();
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

        // Case `synthetic_children_flow_into_the_tree_with_position_and_labels`.
        {
            // Models a DataTable: each row carries its position and two named cells.
            let row = |pos: usize, size: usize| {
                AccessNode::new(Role::Row)
                    .with_position(pos, size)
                    .with_children(vec![
                        AccessNode::new(Role::Cell)
                            .with_name("Name")
                            .with_position(1, 2),
                        AccessNode::new(Role::Cell)
                            .with_name("Age")
                            .with_position(2, 2),
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
                assert!(
                    r.position_in_set().is_some(),
                    "row carries its 1-based position"
                );
                assert_eq!(r.children().len(), 2, "each row owns two cells");
            }

            // Cells are named by their column header.
            let cell_labels: Vec<Option<&str>> = update
                .nodes
                .iter()
                .filter(|(_, n)| n.role() == AkRole::Cell)
                .map(|(_, n)| n.label())
                .collect();
            assert!(
                cell_labels.contains(&Some("Name")),
                "a cell is named for the Name column"
            );
            assert!(
                cell_labels.contains(&Some("Age")),
                "a cell is named for the Age column"
            );
            assert_eq!(cell_labels.len(), 4, "two cells per row across two rows");
        }

        // Case `live_regions_reach_accesskit_with_priority_and_atomic_updates`.
        {
            let polite = TestWidget::new(Some(
                AccessNode::new(Role::StaticText)
                    .with_name("Saved")
                    .with_live(crate::a11y::Politeness::Polite),
            ));
            let assertive = TestWidget::new(Some(
                AccessNode::new(Role::StaticText)
                    .with_name("Connection lost")
                    .with_live(crate::a11y::Politeness::Assertive),
            ));
            let mut root = TestWidget::new(None)
                .with_child(Box::new(polite))
                .with_child(Box::new(assertive));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);

            let node_named = |name: &str| {
                update
                    .nodes
                    .iter()
                    .find(|(_, node)| node.label() == Some(name))
                    .map(|(_, node)| node)
                    .expect("live region is exported")
            };
            let polite = node_named("Saved");
            assert_eq!(polite.live(), Some(AkLive::Polite));
            assert!(polite.is_live_atomic());
            let assertive = node_named("Connection lost");
            assert_eq!(assertive.live(), Some(AkLive::Assertive));
            assert!(assertive.is_live_atomic());
        }
    }

    #[test]
    fn focus_targets_the_focused_widget_or_falls_back_to_root() {
        // Case `focus_targets_the_focused_widget`.
        {
            let btn = TestWidget::new(Some(
                AccessNode::new(Role::Button)
                    .with_name("Save")
                    .with_state(AccessState::new().focusable(true).focused(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(btn));

            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let focused_taffy = node_map
                .iter()
                .find(|(_, ptr)| {
                    let w = unsafe { &***ptr };
                    w.accessibility().is_some_and(|n| n.role == Role::Button)
                })
                .map(|(&id, _)| id);
            assert!(focused_taffy.is_some());

            let update = build_tree_update(&root, &node_map, focused_taffy, 1.0);
            assert_ne!(
                update.focus, ROOT_ID,
                "focus should be the button, not root"
            );
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(id, n)| *id == update.focus && n.role() == AkRole::Button),
                "focus must point at the Button node"
            );
        }

        // Case `focus_falls_back_to_root_when_nothing_focused`.
        {
            let mut root = TestWidget::new(Some(AccessNode::new(Role::Document)));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            assert_eq!(update.focus, ROOT_ID);
        }

        // Case `focus_falls_back_to_root_when_focused_id_is_stale`.
        {
            // A stale taffy id (left over from before a relayout) must not be
            // forwarded — AccessKit panics the AT if `focus` isn't emitted.
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

        // Case `focus_falls_back_to_root_when_the_focused_widget_collapsed_out`.
        {
            // The other half of the emitted-node rule the active-descendant
            // preference is layered on: a live taffy id whose widget is
            // presentational emits no node at all, so forwarding it would name a
            // node absent from this update — which panics the consuming AT.
            let inner = TestWidget::new(None).with_child(Box::new(TestWidget::new(Some(
                AccessNode::new(Role::Button).with_name("Go"),
            ))));
            let mut root = TestWidget::new(None).with_child(Box::new(inner));
            let (_taffy, node_map, _n) = lay_out(&mut root);

            // The presentational wrapper: in the node_map, but never exported.
            let collapsed = node_map
                .iter()
                .find(|(_, ptr)| unsafe { &***ptr }.accessibility().is_none())
                .map(|(&id, _)| id)
                .expect("a presentational widget was laid out");

            let update = build_tree_update(&root, &node_map, Some(collapsed), 1.0);
            assert_eq!(update.focus, ROOT_ID);
            assert!(update.nodes.iter().any(|(id, _)| *id == update.focus));
        }
    }

    #[test]
    fn text_values_expose_editable_runs_but_never_secret_content() {
        // Case `terminal_is_selectable_but_not_an_editable_text_value`.
        {
            let terminal = TestWidget::new(Some(
                AccessNode::new(Role::Terminal)
                    .with_name("Terminal")
                    .with_value("$ cargo test")
                    .with_state(AccessState::new().focusable(true).read_only(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(terminal));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (_, node) = update
                .nodes
                .iter()
                .find(|(_, node)| node.role() == AkRole::Terminal)
                .expect("terminal reaches AccessKit with its native role");
            assert!(node.is_read_only());
            assert!(node.supports_action(AkAction::Focus));
            assert!(node.supports_action(AkAction::SetTextSelection));
            assert!(!node.supports_action(AkAction::SetValue));
            assert_eq!(node.value(), Some("$ cargo test"));
        }

        // Case `secret_value_is_never_exposed`.
        {
            let pw = TestWidget::new(Some(
                AccessNode::new(Role::TextInput)
                    .with_name("Protected input")
                    .with_value("hunter2")
                    .with_sensitivity(crate::a11y::Sensitivity::Secret),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(pw));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (_, node) = update
                .nodes
                .iter()
                .find(|(_, n)| n.label() == Some("Protected input"))
                .unwrap();
            assert_eq!(node.role(), AkRole::PasswordInput);
            assert_eq!(node.value(), None, "secret value must not leak to the AT");
        }

        // Case `secret_label_content_is_never_exposed_but_personal_stays_local`.
        {
            let code = TestWidget::new(Some(
                AccessNode::new(Role::Label)
                    .with_name("493 201")
                    .with_sensitivity(crate::a11y::Sensitivity::Secret),
            ));
            let contact = TestWidget::new(Some(
                AccessNode::new(Role::Label)
                    .with_name("Ada Lovelace")
                    .with_sensitivity(crate::a11y::Sensitivity::Personal),
            ));
            let mut root = TestWidget::new(None)
                .with_child(Box::new(code))
                .with_child(Box::new(contact));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            assert!(
                update
                    .nodes
                    .iter()
                    .all(|(_, n)| n.label() != Some("493 201") && n.value() != Some("493 201")),
                "a secret label's content must not reach the AT"
            );
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, n)| n.label() == Some("Ada Lovelace")),
                "personal content is readable by this session's own screen reader"
            );
        }

        // Case `editable_text_exposes_grapheme_run_and_directional_selection`.
        {
            let family = "👩‍👩‍👧‍👦";
            let value = format!("a{family}b");
            let field = TestWidget::new(Some(
                AccessNode::new(Role::TextInput)
                    .with_value(value.clone())
                    .with_text_selection(1 + family.len(), 1)
                    .with_state(AccessState::new().focusable(true).focused(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(field));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let (field_id, field_node) = update
                .nodes
                .iter()
                .find(|(_, node)| node.role() == AkRole::TextInput)
                .unwrap();
            let (run_id, run) = update
                .nodes
                .iter()
                .find(|(_, node)| node.role() == AkRole::TextRun)
                .unwrap();

            assert_eq!(run.value(), Some(value.as_str()));
            assert_eq!(
                run.character_lengths(),
                [1, u8::try_from(family.len()).unwrap_or(u8::MAX), 1].as_slice(),
                "AccessKit characters match the editor's extended graphemes"
            );
            assert_eq!(field_node.children(), &[*run_id]);
            let selection = field_node.text_selection().unwrap();
            assert_eq!(selection.anchor.node, *run_id);
            assert_eq!(selection.anchor.character_index, 2);
            assert_eq!(selection.focus.node, *run_id);
            assert_eq!(selection.focus.character_index, 1);
            assert_ne!(*field_id, *run_id);
        }
    }

    #[cfg(windows)]
    #[test]
    fn native_password_entry_uses_only_a_fixed_empty_placeholder() {
        for secret in ["", "hunter2", "a much longer password"] {
            let mut root = TestWidget::new(Some(
                AccessNode::new(Role::TextInput)
                    .with_name("Secret")
                    .with_value(secret)
                    .with_sensitivity(crate::a11y::Sensitivity::Secret),
            ));
            let (_, node_map, _) = lay_out(&mut root);
            let semantic = build_tree_update(&root, &node_map, None, 1.0);
            assert!(
                semantic
                    .nodes
                    .iter()
                    .all(|(_, node)| node.value().is_none())
            );
            let native = native_tree_update(semantic);
            let (_, password) = native
                .nodes
                .iter()
                .find(|(_, node)| node.role() == AkRole::PasswordInput)
                .unwrap();
            assert_eq!(password.value(), Some(""));
            assert!(password.supports_action(AkAction::SetValue));
            assert!(password.children().is_empty());
        }
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
                w.accessibility().is_some_and(|n| n.role == role)
            })
            .map(|(&id, _)| id)
            .expect("a widget with that role was laid out")
    }

    fn label_id(update: &TreeUpdate, label: &str) -> AkNodeId {
        update
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some(label))
            .map_or_else(
                || panic!("no node labelled {label} in the update"),
                |(id, _)| *id,
            )
    }

    #[test]
    fn focus_and_active_descendant_follow_the_marked_synthetic_node() {
        // Case `focus_lands_on_the_marked_synthetic_node_not_its_owning_widget`.
        {
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

            let canvas_ak = widget_ak_id(&root, &node_map, canvas_taffy).unwrap();
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

        // Case `an_unfocused_widget_cannot_claim_the_readers_cursor`.
        {
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
                widget_ak_id(&root, &node_map, button_taffy).unwrap(),
                "focus stays on the widget that holds the keyboard"
            );
            let (_, focused) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == update.focus)
                .unwrap();
            assert_eq!(focused.label(), Some("Save"));
        }

        // Case `the_first_marked_descendant_in_document_order_wins`.
        {
            // A widget that marks two nodes focused is buggy, but the reader must
            // still land somewhere deterministic rather than on whichever node the
            // walk happened to reach last.
            let mut root = TestWidget::new(Some(canvas(vec![
                clip("Intro", false),
                clip("Interview", true),
                clip("Outro", true),
            ])));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let canvas_taffy = taffy_id_of(&node_map, Role::Canvas);

            let update = build_tree_update(&root, &node_map, Some(canvas_taffy), 1.0);
            let (_, focused) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == update.focus)
                .expect("the focused id must name a node in this very update");
            assert_eq!(focused.label(), Some("Interview"));
        }

        // Case `active_descendant_path_names_the_marked_node_the_same_way_focus_does`.
        {
            // The keyboard path and the exported focus must agree on which element
            // the user is on; if they disagreed, Enter would activate something
            // other than what the reader just announced.
            let mut root = TestWidget::new(Some(
                AccessNode::new(Role::Canvas)
                    .with_name("Timeline")
                    .with_state(AccessState::new().focusable(true))
                    .with_children(vec![
                        AccessNode::new(Role::Group)
                            .with_name("Track 1")
                            .with_children(vec![clip("Intro", false), clip("Interview", true)]),
                        AccessNode::new(Role::Group).with_name("Track 2"),
                    ]),
            ));
            let access = Widget::accessibility(&root).unwrap();

            // Second clip of the first track: [track 0, clip 1].
            assert_eq!(active_descendant_path(&access), Some(vec![0, 1]));

            // And it names the very node the exported focus lands on.
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let canvas_taffy = taffy_id_of(&node_map, Role::Canvas);
            let update = build_tree_update(&root, &node_map, Some(canvas_taffy), 1.0);
            assert_eq!(
                resolve_node_id(&root, &node_map, update.focus),
                Some(A11yTarget::Synthetic {
                    owner: canvas_taffy,
                    key_path: vec![0, 1],
                    // No explicit keys in this fixture, so the ordinal path
                    // coincides with the key path — spelled out rather than
                    // assumed, because they diverge as soon as one is set.
                    ordinal_path: vec![0, 1],
                    semantic_target: None,
                }),
                "keyboard target and reader focus must be the same element"
            );
        }

        // Case `a_widget_that_marks_nothing_has_no_active_descendant`.
        {
            // An ordinary widget must yield an empty target so the keyboard path
            // keeps addressing the widget itself.
            let plain = AccessNode::new(Role::Button).with_name("Save");
            assert_eq!(active_descendant_path(&plain), None);

            let unmarked = canvas(vec![clip("a", false), clip("b", false)]);
            assert_eq!(active_descendant_path(&unmarked), None);
        }
    }

    #[test]
    fn resolve_node_id_maps_emitted_ids_back_or_reports_them_gone() {
        // Case `resolve_node_id_round_trips_an_emitted_synthetic_id_to_owner_and_key_path`.
        {
            // The action-routing contract: whatever id the bridge put in the tree,
            // an incoming ActionRequest carrying it must resolve back to the widget
            // that owns it plus the path naming the exact element.
            let mut root = TestWidget::new(Some(
                AccessNode::new(Role::Table)
                    .with_name("People")
                    .with_children(vec![
                        AccessNode::new(Role::Row)
                            .with_name("r0")
                            .with_children(vec![
                                AccessNode::new(Role::Cell).with_name("r0c0"),
                                AccessNode::new(Role::Cell).with_name("r0c1"),
                            ]),
                        AccessNode::new(Role::Row)
                            .with_name("r1")
                            .with_children(vec![
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
                    // No explicit keys in this fixture, so the ordinal path
                    // coincides with the key path — spelled out rather than
                    // assumed, because they diverge as soon as one is set.
                    ordinal_path: vec![1],
                    semantic_target: None,
                }),
            );
            // A cell: row, then column.
            assert_eq!(
                resolve_node_id(&root, &node_map, label_id(&update, "r1c0")),
                Some(A11yTarget::Synthetic {
                    owner: table_taffy,
                    key_path: vec![1, 0],
                    // No explicit keys in this fixture, so the ordinal path
                    // coincides with the key path — spelled out rather than
                    // assumed, because they diverge as soon as one is set.
                    ordinal_path: vec![1, 0],
                    semantic_target: None,
                }),
            );
            assert_eq!(
                resolve_node_id(&root, &node_map, label_id(&update, "r0c1")),
                Some(A11yTarget::Synthetic {
                    owner: table_taffy,
                    key_path: vec![0, 1],
                    // No explicit keys in this fixture, so the ordinal path
                    // coincides with the key path — spelled out rather than
                    // assumed, because they diverge as soon as one is set.
                    ordinal_path: vec![0, 1],
                    semantic_target: None,
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

        // Case `resolve_node_id_reports_a_stale_reference_as_gone`.
        {
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

        // Case `resolve_node_id_declines_a_synthetic_node_whose_owner_was_never_laid_out`.
        {
            // Without a taffy id there is no handle to dispatch against, so such a
            // node must report as unaddressable rather than resolve to a wrong owner.
            let root = TestWidget::new(Some(canvas(vec![clip("orphan", false)])));
            let empty_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();

            let update = build_tree_update(&root, &empty_map, None, 1.0);
            let orphan = label_id(&update, "orphan");
            assert!(is_synthetic_id(orphan));
            assert_eq!(resolve_node_id(&root, &empty_map, orphan), None);
        }

        // Case `a_text_run_resolves_to_the_field_that_owns_it`.
        {
            // The run carries the widget's own text, so a verb aimed at it belongs
            // to the widget — dropping it would make an AT's text actions no-ops.
            let field = TestWidget::new(Some(
                AccessNode::new(Role::TextInput)
                    .with_value("hello")
                    .with_text_selection(1, 1)
                    .with_state(AccessState::new().focusable(true)),
            ));
            let mut root = TestWidget::new(None).with_child(Box::new(field));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let field_taffy = taffy_id_of(&node_map, Role::TextInput);
            let update = build_tree_update(&root, &node_map, None, 1.0);

            let (run_id, _) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == AkRole::TextRun)
                .expect("the field exports a text run");
            assert!(!is_synthetic_id(*run_id));
            assert_eq!(
                resolve_node_id(&root, &node_map, *run_id),
                Some(A11yTarget::Widget(field_taffy)),
            );
        }
    }
}
