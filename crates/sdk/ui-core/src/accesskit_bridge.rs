//! AccessKit bridge: turns the toolkit's per-widget semantic model ([`crate::a11y::AccessNode`], from
//! [`Widget::accessibility`]) into an [`accesskit::TreeUpdate`], published over AT-SPI2.

use std::collections::HashMap;
use std::hash::BuildHasher;

use accesskit::{
    Action as AkAction, Live as AkLive, Node as AkNode, NodeId as AkNodeId, Rect as AkRect,
    Role as AkRole, TextPosition as AkTextPosition, TextSelection as AkTextSelection, Toggled,
    TreeInfo, TreeUpdate,
};
use taffy::NodeId as TaffyNodeId;

use crate::a11y::{AccessNode, AccessibleTextSelection, Action, Role};
use crate::a11y_child_route::{ChildRoute, Routes, child_route};
use crate::a11y_tree::HostedByIndex;
use crate::a11y_tree::walk::{A11yVisitor, Visit, walk, walk_synthetic_children};
use crate::text_break::{grapheme_indices, graphemes};
use crate::widget::Widget;

#[cfg(any(feature = "native", test))]
mod floating_surface;
#[cfg(feature = "native")]
pub(crate) use floating_surface::{publish_host_part, surface_action_target, surface_tree_update};

/// Reserved so no widget can collide with it: widget ids are offset by +1
/// from their taffy node id, and structural fallback ids are large offsets
/// — see `widget_node_id`.
pub const ROOT_ID: AkNodeId = AkNodeId(0);
const WALK_ID_BASE: u64 = 1u64 << 48;
const TEXT_RUN_ID_BASE: u64 = 1u64 << 62;
/// Resource and action nodes occupy reserved ranges above synthetic widget nodes and below text-run
/// nodes.
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

/// E.g.
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
        Role::ListBox => AkRole::ListBox,
        Role::ListBoxOption => AkRole::ListBoxOption,

        Role::Button | Role::ToggleButton => AkRole::Button,
        Role::Checkbox => AkRole::CheckBox,
        Role::RadioButton => AkRole::RadioButton,
        Role::Switch => AkRole::Switch,
        Role::Slider => AkRole::Slider,
        Role::Separator => AkRole::Splitter,
        Role::SpinButton => AkRole::SpinButton,
        Role::ComboBox => AkRole::ComboBox,
        Role::TextInput => AkRole::TextInput,
        Role::MultilineTextInput => AkRole::MultilineTextInput,
        Role::Link => AkRole::Link,
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

/// `ToggleCheck` has no dedicated verb — toggling is a `Click` at the platform level.
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

/// Inverse of [`map_action`]. `None` for actions we don't model (scroll-by-direction, custom actions,
/// …).
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

/// Prefers `taffy_id` (offset by 1 so it can never be [`ROOT_ID`], since
/// taffy ids start at 0); falls back to a high-range walk index for widgets
/// with no `node_map` entry.
fn widget_node_id(taffy_id: Option<TaffyNodeId>, walk_index: u64) -> AkNodeId {
    match taffy_id {
        Some(id) => AkNodeId(u64::from(id).saturating_add(1)),
        None => AkNodeId(WALK_ID_BASE.saturating_add(walk_index)),
    }
}

fn text_run_node_id(parent: AkNodeId) -> AkNodeId {
    AkNodeId(TEXT_RUN_ID_BASE | parent.0)
}

const SYNTHETIC_TEXT_RUN_KEY: u64 = u64::MAX;

fn synthetic_text_run_node_id(parent: AkNodeId) -> AkNodeId {
    synthetic_node_id(parent, SYNTHETIC_TEXT_RUN_KEY)
}

/// What a readable text node's run has: its value and caret or selection,
/// or `None` when the node exports no run.
///
/// A protected (password) field still gets a run: AccessKit offers AT-SPI
/// Text and EditableText only on a node with one, and without them a screen
/// reader cannot treat the field as editable text or type a passphrase into
/// it. That run is always empty with the caret at its start, so it discloses
/// neither the value, nor its length, nor whether anything was typed.
fn text_run_content(access: &AccessNode) -> Option<(&str, AccessibleTextSelection)> {
    if access.state.protected {
        let editable = matches!(access.role, Role::TextInput | Role::MultilineTextInput);
        let start = AccessibleTextSelection {
            anchor: 0,
            focus: 0,
        };
        return editable.then_some(("", start));
    }
    match (&access.value, access.text_selection) {
        (Some(value), Some(selection)) => Some((value.as_str(), selection)),
        (Some(value), None) if access.role == Role::Terminal => Some((
            value.as_str(),
            AccessibleTextSelection {
                anchor: 0,
                focus: 0,
            },
        )),
        _ => None,
    }
}

/// The text run that gives an editable node its AT-SPI Text and EditableText
/// interfaces: the value as one run, and the caret/selection as positions in
/// it. A protected (password) node never has its value here.
fn text_run_for(
    ak: &mut AkNode,
    access: &AccessNode,
    run_id: AkNodeId,
    bounds: Option<vello::kurbo::Rect>,
    scale: f64,
) -> Option<AkNode> {
    let (value, selection) = text_run_content(access)?;
    let mut run = AkNode::new(AkRole::TextRun);
    run.set_value(value);
    run.set_character_lengths(accessible_character_lengths(value));
    if let Some(rect) = bounds.filter(|rect| rect.width() > 0.0 || rect.height() > 0.0) {
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
    Some(run)
}

/// Derive a synthetic node's stable id from its parent's id and its key.
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
    AkNodeId(h.finish() | SYNTHETIC_ID_BIT)
}

/// Whether `id` addresses a synthetic node rather than a widget, a text run or the root.
pub fn is_synthetic_id(id: AkNodeId) -> bool {
    id.0 & SYNTHETIC_ID_BIT != 0
}

/// What a child index is hashed with on the way down to a retained widget
/// with no layout node of it, so it never meets a synthetic child's key.
const CHILD_ROUTE_KEY: u64 = 0x7469_6c6f_2e63_6800;

fn child_route_node_id(owner: AkNodeId, path: &[usize]) -> AkNodeId {
    path.iter().fold(owner, |id, &index| {
        synthetic_node_id(id, CHILD_ROUTE_KEY ^ index as u64)
    })
}

/// The AccessKit id of the widget laid out at `taffy_id`, or `None` when that
/// taffy node has no live widget.
///
/// The host tracks focus by `taffy::NodeId` while the exported tree is keyed by
/// AccessKit id, so anything crossing between the two — moving focus to a node,
/// asserting on one in a test — has to translate. Exposed rather than
/// open-coded so a caller cannot drift from `widget_node_id`.
pub fn widget_ak_id<S: BuildHasher>(
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    taffy_id: TaffyNodeId,
) -> Option<AkNodeId> {
    node_map
        .contains_key(&taffy_id)
        .then(|| widget_node_id(Some(taffy_id), 0))
}

/// Where an [`accesskit::NodeId`] points in the live widget tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum A11yTarget {
    /// The synthetic window root — no widget owns it.
    Root,
    /// A real widget, addressed by the `taffy::NodeId` it was laid out with.
    Widget(TaffyNodeId),
    /// A synthetic node inside `owner`'s [`AccessNode::children`], addressed by the key path from
    /// `owner` down to it (see [`AccessNode::key`]).
    Synthetic {
        owner: TaffyNodeId,
        /// Keys from `owner`'s direct synthetic child down to the target.
        key_path: Vec<u64>,
        /// Ordinals, for a widget that identifies one by position —
        /// `synthetic_accessibility_rect` indexes into `seg_rects`.
        ///
        /// Both are carried because both are required, and they differ
        /// for any node that sets an explicit key. Handing a positional
        /// implementor a key it never had is a silent mis-target.
        ordinal_path: Vec<usize>,
        /// Opaque stable model identity supplied by the synthetic node.
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
    resolve_node_id_impl::<S, false>(root, node_map, id)
}
/// Host traversal retaining the originating component through every callback.
pub(crate) fn resolve_node_id_bound<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    id: AkNodeId,
) -> Option<A11yTarget> {
    resolve_node_id_impl::<S, true>(root, node_map, id)
}
fn resolve_node_id_impl<S: BuildHasher, const BOUND: bool>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    id: AkNodeId,
) -> Option<A11yTarget> {
    if id == ROOT_ID {
        return Some(A11yTarget::Root);
    }
    if is_synthetic_id(id) {
        return search_synthetic::<BOUND>(
            root,
            &reverse_node_map::<S, BOUND>(&[root], node_map),
            id,
        );
    }
    let widget_id = id.0 & !TEXT_RUN_ID_BASE;
    if widget_id >= WALK_ID_BASE {
        return None;
    }
    let taffy_id = TaffyNodeId::from(widget_id.checked_sub(1)?);
    node_map
        .contains_key(&taffy_id)
        .then_some(A11yTarget::Widget(taffy_id))
}

fn search_synthetic<const BOUND: bool>(
    w: &dyn Widget,
    ptr_to_taffy: &WidgetNodeMap,
    wanted: AkNodeId,
) -> Option<A11yTarget> {
    search_synthetic_routed::<BOUND>(w, ptr_to_taffy, wanted, None)
}

fn search_synthetic_routed<const BOUND: bool>(
    w: &dyn Widget,
    ptr_to_taffy: &WidgetNodeMap,
    wanted: AkNodeId,
    route: Option<ChildRoute>,
) -> Option<A11yTarget> {
    let Ok(_entry) = crate::bound_dispatch::enter_traversal::<BOUND>(w) else {
        return None;
    };
    let taffy = ptr_to_taffy.get(w);
    let route = if taffy.is_some() { None } else { route };
    for (index, child) in w.children().into_iter().enumerate() {
        let child_route = child_route(w, taffy, route.as_ref(), index);
        if let Some(hit) =
            search_synthetic_routed::<BOUND>(child, ptr_to_taffy, wanted, child_route)
        {
            return Some(hit);
        }
    }
    let access_node = w.accessibility()?;
    let (owner, owner_id, path) = match (taffy, route) {
        (Some(owner), _) => (owner, widget_node_id(Some(owner), 0), None),
        (None, Some((owner, path))) => {
            let id = child_route_node_id(widget_node_id(Some(owner), 0), &path);
            if id == wanted || text_run_node_id(id) == wanted {
                return Some(A11yTarget::Synthetic {
                    owner,
                    key_path: Vec::new(),
                    ordinal_path: Vec::new(),
                    semantic_target: Some(crate::a11y_child_route::encode(&path, None)),
                });
            }
            (owner, id, Some(path))
        }
        (None, None) => return None,
    };

    let mut key_path = Vec::new();
    let mut ordinal_path = Vec::new();
    let mut semantic_target = None;
    if !find_synthetic_path(
        &access_node.children,
        owner_id,
        wanted,
        &mut key_path,
        &mut ordinal_path,
        &mut semantic_target,
    ) {
        return None;
    }
    if let Some(path) = path {
        let inner = semantic_target.unwrap_or_else(|| crate::a11y_child_route::keys(&key_path));
        return Some(A11yTarget::Synthetic {
            owner,
            key_path: Vec::new(),
            ordinal_path: Vec::new(),
            semantic_target: Some(crate::a11y_child_route::encode(&path, Some(&inner))),
        });
    }
    Some(A11yTarget::Synthetic {
        owner,
        key_path,
        ordinal_path,
        semantic_target,
    })
}

/// Walk one widget's synthetic forest looking for `wanted`, accumulating the
/// path as it descends. Returns `true` (with `path` filled) on a hit.
///
/// The id is derived from each node's **key**, so it stays stable when a
/// sibling appears or the list is reordered — that is the whole point of
/// hashing the path rather than counting a tree-wide walk. The path handed
/// back is the **ordinal**, because that is what a widget indexes by:
/// `synthetic_accessibility_rect(&[segment])` reaches into `seg_rects`. Using
/// the key for both would hand a positional implementor an index it never
/// had, for any widget that sets an explicit key.
fn find_synthetic_path(
    children: &[AccessNode],
    parent_id: AkNodeId,
    wanted: AkNodeId,
    keys: &mut Vec<u64>,
    ordinals: &mut Vec<usize>,
    semantic_target: &mut Option<String>,
) -> bool {
    for (ordinal, child) in children.iter().enumerate() {
        let key = child.key.unwrap_or(ordinal as u64);
        let id = synthetic_node_id(parent_id, key);
        keys.push(key);
        ordinals.push(ordinal);
        if id == wanted
            || (text_run_content(child).is_some() && synthetic_text_run_node_id(id) == wanted)
        {
            semantic_target.clone_from(&child.action_target);
            return true;
        }
        if find_synthetic_path(&child.children, id, wanted, keys, ordinals, semantic_target) {
            return true;
        }
        keys.pop();
        ordinals.pop();
    }
    false
}

struct WidgetNodeMap {
    nodes: HashMap<*const (), Vec<(TaffyNodeId, &'static str)>>,
    aliases: std::collections::HashSet<*const ()>,
}

impl WidgetNodeMap {
    fn get(&self, widget: &dyn Widget) -> Option<TaffyNodeId> {
        let pointer = std::ptr::from_ref(widget);
        let address = pointer.cast::<()>();
        let entries = self.nodes.get(&address)?;
        if !self.aliases.contains(&address) {
            return entries.first().map(|(id, _)| *id);
        }
        let concrete = widget.concrete_type_name();
        let mut matches = entries
            .iter()
            .filter(|(_, registered_type)| *registered_type == concrete);
        let (id, _) = matches.next()?;
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

pub fn build_tree_update<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
) -> TreeUpdate {
    build_tree_update_with_resources(root, node_map, focused, scale, &[])
}

/// Name the window `update` belongs to: what an assistive technology reads as the window's title.
pub fn name_window(update: &mut TreeUpdate, title: &str) {
    let root = update.tree.as_ref().map_or(ROOT_ID, |tree| tree.root);
    if let Some((_, node)) = update.nodes.iter_mut().find(|(id, _)| *id == root) {
        node.set_label(title);
    }
}

/// Build the widget accessibility tree and append the exact same live resource/action surface used by
/// the palette, shell and collaboration API.
pub fn build_tree_update_with_resources<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    resources: &[tilo_actions::ResourceSpec],
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, false>(&[root], node_map, focused, scale, resources)
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
    resources: &[tilo_actions::ResourceSpec],
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(&[root], node_map, focused, scale, resources)
}

pub(crate) fn build_tree_update_bound<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(&[root], node_map, focused, scale, &[])
}

pub(crate) fn build_presentation_tree_bound<S: BuildHasher>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(roots, node_map, focused, scale, &[])
}

/// Multiple presentation roots retain ordinary live resource nodes when their
/// owner is interactive; callers supply only currently presented resources.
pub(crate) fn build_presentation_with_resources_bound<S: BuildHasher>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    resources: &[tilo_actions::ResourceSpec],
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(roots, node_map, focused, scale, resources)
}

fn build_tree_update_with_resources_impl<S: BuildHasher, const BOUND: bool>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    resources: &[tilo_actions::ResourceSpec],
) -> TreeUpdate {
    let ptr_to_taffy = reverse_node_map::<S, BOUND>(roots, node_map);

    let focused_ak = focused.map(|t| AkNodeId(u64::from(t).saturating_add(1)));

    let mut tree_walk = TreeWalk::<BOUND> {
        ptr_to_taffy: &ptr_to_taffy,
        scale,
        focused,
        walk_counter: 0,
        nodes: Vec::new(),
        active_descendant: None,
        owned_active_descendant: None,
        routes: Routes::default(),
    };

    let mut root_node = AkNode::new(AkRole::Window);
    root_node.set_label(crate::i18n::widget_format("window-root-name", &[]));

    let mut top = AkScope::under(ROOT_ID, false);
    for root in roots {
        walk::<BOUND, _>(&mut tree_walk, *root, None, &mut top);
    }
    let mut root_children = top.children;
    let TreeWalk {
        mut nodes,
        active_descendant,
        ..
    } = tree_walk;
    let mut resource_ids = HashMap::new();
    append_resource_nodes(resources, &mut nodes, &mut root_children, &mut resource_ids);
    root_node.set_children(root_children);
    nodes.push((ROOT_ID, root_node));

    // AccessKit panics the AT if `focus` references a node absent from this
    // update. The taffy id can be stale (relayout reassigns ids) or point
    // at a collapsed node, so only honour it when actually emitted.
    //
    // The active descendant wins over the widget itself: a canvas app that
    // tells us which clip the user is on should put the reader on that clip,
    // not on the whole timeline. It is only ever set for the focused widget, so
    // it cannot pull focus away from wherever the keyboard actually is — and it
    // is subject to the same emitted-node check, never instead of it.
    let focus = match (active_descendant, focused_ak) {
        (Some(id), _) if nodes.iter().any(|(nid, _)| *nid == id) => id,
        (_, Some(id)) if nodes.iter().any(|(nid, _)| *nid == id) => id,
        _ => ROOT_ID,
    };

    TreeUpdate {
        nodes,
        tree: Some(TreeInfo::new(ROOT_ID)),
        tree_id: accesskit::TreeId::ROOT,
        focus,
    }
}

fn append_resource_nodes(
    resources: &[tilo_actions::ResourceSpec],
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
            match action.input() {
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

struct TreeWalk<'m, const BOUND: bool> {
    ptr_to_taffy: &'m WidgetNodeMap,
    scale: f64,
    focused: Option<TaffyNodeId>,
    walk_counter: u64,
    nodes: Vec<(AkNodeId, AkNode)>,
    active_descendant: Option<AkNodeId>,
    owned_active_descendant: Option<AkNodeId>,
    routes: Routes,
}

struct AkScope {
    id: AkNodeId,
    in_focused_widget: bool,
    children: Vec<AkNodeId>,
    /// The keys this scope's synthetic children have been given ids by: a widget that draws several
    /// windows names each window's control alike ("Show sidebar" in every window), and the second of a
    /// key takes its place among the children instead, so both keep a node a reader reaches.
    keys: std::collections::HashSet<u64>,
}

impl AkScope {
    fn under(id: AkNodeId, in_focused_widget: bool) -> Self {
        Self {
            id,
            in_focused_widget,
            children: Vec::new(),
            keys: std::collections::HashSet::new(),
        }
    }
}

struct PendingWidget {
    ak: AkNode,
    access: AccessNode,
    taffy_id: Option<TaffyNodeId>,
}

impl<const BOUND: bool> A11yVisitor for TreeWalk<'_, BOUND> {
    type Scope = AkScope;
    type Pending = PendingWidget;
    type SyntheticPending = AkNode;

    fn open(
        &mut self,
        w: &dyn Widget,
        slot: Option<(&dyn Widget, usize)>,
        _parent: &mut AkScope,
    ) -> Visit<PendingWidget, AkScope> {
        let taffy_id = self.ptr_to_taffy.get(w);
        let route = self.routes.enter(w, taffy_id);
        let Some(mut access) = w.accessibility() else {
            self.walk_counter = self.walk_counter.saturating_add(1);
            return Visit::Collapse;
        };
        crate::a11y_tree::walk::with_slot_words(&mut access, slot.as_ref());
        let id = match (&route, taffy_id) {
            (Some((owner, path)), None) => {
                child_route_node_id(widget_node_id(Some(*owner), 0), path)
            }
            _ => widget_node_id(taffy_id, self.walk_counter),
        };
        self.walk_counter = self.walk_counter.saturating_add(1);

        let mut ak = AkNode::new(map_role(access.role));
        apply_semantics(&mut ak, &access, w, self.scale);
        Visit::Node(
            PendingWidget {
                ak,
                access,
                taffy_id,
            },
            AkScope::under(id, false),
        )
    }

    fn close<'w>(
        &mut self,
        w: &'w dyn Widget,
        PendingWidget {
            mut ak,
            access,
            taffy_id,
        }: PendingWidget,
        mut scope: AkScope,
        hosted: HostedByIndex<'w>,
        represented: Vec<&'w dyn Widget>,
        parent: &mut AkScope,
    ) {
        let represents_focus = self.focused.is_some()
            && represented
                .into_iter()
                .any(|child| self.ptr_to_taffy.get(child) == self.focused);
        scope.in_focused_widget =
            (taffy_id.is_some() && taffy_id == self.focused) || represents_focus;
        let outer_active = self.owned_active_descendant.take();
        walk_synthetic_children::<BOUND, Self>(self, w, &access.children, hosted, &mut scope);
        if let Some(active) = std::mem::replace(&mut self.owned_active_descendant, outer_active) {
            ak.set_active_descendant(active);
        }
        let AkScope {
            id,
            children: mut my_children,
            ..
        } = scope;
        let run_id = text_run_node_id(id);
        if let Some(run) = text_run_for(&mut ak, &access, run_id, Some(w.get_rect()), self.scale) {
            my_children.insert(0, run_id);
            self.nodes.push((run_id, run));
        }
        ak.set_children(my_children);

        self.nodes.push((id, ak));
        parent.children.push(id);
    }

    fn open_synthetic(
        &mut self,
        owner: &dyn Widget,
        path: &[usize],
        access: &AccessNode,
        parent: &mut AkScope,
    ) -> (AkNode, AkScope) {
        let ordinal = path.last().copied().unwrap_or_default();
        let key = access.key.unwrap_or(ordinal as u64);
        // Two windows' controls of one name, drawn by the widget that has both: the first keeps
        // the id its key names, the next its place among the children, so neither is lost to
        // the other. A repeated key is a node the widget describes twice, never a node dropped.
        let id = if parent.keys.insert(key) {
            synthetic_node_id(parent.id, key)
        } else {
            synthetic_node_id(parent.id, ordinal as u64)
        };

        let mut ak = AkNode::new(map_role(access.role));
        apply_semantics_common(&mut ak, access);
        apply_synthetic_bounds(&mut ak, access, owner, path, self.scale);

        let in_focused_widget = parent.in_focused_widget;
        if in_focused_widget && access.state.focused && self.active_descendant.is_none() {
            self.active_descendant = Some(id);
        }
        if in_focused_widget
            && access.state.active_descendant
            && self.owned_active_descendant.is_none()
        {
            self.owned_active_descendant = Some(id);
        }
        (ak, AkScope::under(id, in_focused_widget))
    }

    fn close_synthetic(
        &mut self,
        owner: &dyn Widget,
        path: &[usize],
        access: &AccessNode,
        mut ak: AkNode,
        AkScope {
            id,
            children: mut my_children,
            ..
        }: AkScope,
        parent: &mut AkScope,
    ) {
        let run_id = synthetic_text_run_node_id(id);
        let run_bounds = access
            .bounds
            .map(crate::a11y::AccessBounds::to_rect)
            .or_else(|| owner.synthetic_accessibility_rect(path));
        if let Some(run) = text_run_for(&mut ak, access, run_id, run_bounds, self.scale) {
            my_children.insert(0, run_id);
            self.nodes.push((run_id, run));
        }
        ak.set_children(my_children);

        self.nodes.push((id, ak));
        parent.children.push(id);
    }
}
fn apply_semantics(ak: &mut AkNode, access: &AccessNode, w: &dyn Widget, scale: f64) {
    apply_semantics_common(ak, access);

    // A widget that names where its node is (the one control it
    // paints inside a larger box) is taken at its word; otherwise its box.
    let r = access
        .bounds
        .map_or_else(|| w.get_rect(), crate::a11y::AccessBounds::to_rect);
    if r.width() > 0.0 || r.height() > 0.0 {
        ak.set_bounds(AkRect::new(
            r.x0 * scale,
            r.y0 * scale,
            r.x1 * scale,
            r.y1 * scale,
        ));
    }
}

fn apply_synthetic_bounds(
    ak: &mut AkNode,
    access: &AccessNode,
    owner: &dyn Widget,
    ordinal_path: &[usize],
    scale: f64,
) {
    if let Some(bounds) = crate::a11y::synthetic_bounds(access, owner, ordinal_path) {
        ak.set_bounds(AkRect::new(
            bounds.x0 * scale,
            bounds.y0 * scale,
            bounds.x1 * scale,
            bounds.y1 * scale,
        ));
    }
}

fn apply_semantics_common(ak: &mut AkNode, access: &AccessNode) {
    if access.state.protected && matches!(access.role, Role::TextInput | Role::MultilineTextInput) {
        ak.set_role(AkRole::PasswordInput);
    }
    if let Some(name) = &access.name {
        ak.set_label(name.clone());
    }
    if let Some(desc) = &access.description {
        ak.set_description(desc.clone());
    }
    if let Some(kind) = &access.role_description {
        ak.set_role_description(kind.clone());
    }
    if let Some(action) = &access.action {
        ak.set_author_id(action.clone());
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
    let value = match access.role {
        Role::Label | Role::StaticText => access.value.as_ref().or(access.name.as_ref()),
        _ => access.value.as_ref(),
    };
    // Never expose a protected (password) value, even if the widget mistakenly populated one.
    if !access.state.protected
        && let Some(value) = value
    {
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
    // but not press them. A table row is one too, as soon as it has a
    // target an activation reaches: that is what a person's Enter on the row,
    // a screen reader's activation and a click all route to.
    let actionable = access.action_target.is_some() && access.bounds.is_some();
    if !access.state.disabled
        && (crate::a11y::activation_role(access.role) || (access.role == Role::Row && actionable))
    {
        ak.add_action(AkAction::Click);
    }
    if let Some(expanded) = access.state.expanded.filter(|_| !access.state.disabled) {
        ak.add_action(if expanded {
            AkAction::Collapse
        } else {
            AkAction::Expand
        });
    }
    match access.role {
        Role::Slider | Role::Separator | Role::SpinButton => {
            if !access.state.disabled {
                ak.add_action(AkAction::Increment);
                ak.add_action(AkAction::Decrement);
                if matches!(access.role, Role::Slider | Role::Separator) && !access.state.read_only
                {
                    ak.add_action(AkAction::SetValue);
                }
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
    use crate::widget::TaffyTreeExt;
    use crate::widget::Widget;
    use std::collections::HashMap;
    use taffy::TaffyTree;

    struct TestWidget {
        access: Option<AccessNode>,
        rect: vello::kurbo::Rect,
        kids: Vec<Box<dyn Widget>>,
        name: String,
        represents: Vec<usize>,
    }

    impl TestWidget {
        fn new(access: Option<AccessNode>) -> Self {
            Self {
                access,
                rect: vello::kurbo::Rect::new(0.0, 0.0, 0.0, 0.0),
                kids: vec![],
                name: "test".into(),
                represents: vec![],
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
        fn sync(&mut self, _s: &crate::widget::AppModel, _p: &crate::widget::ModelPath) {}
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
            taffy.widget_node(taffy::Style::default(), &child_nodes, node_map, self)
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
        fn accessibility_represents_child(&self, child_index: usize) -> bool {
            self.represents.contains(&child_index)
        }
    }

    #[repr(C)]
    struct AliasWrapper {
        child: TestWidget,
    }

    impl Widget for AliasWrapper {
        fn sync(&mut self, _s: &crate::widget::AppModel, _p: &crate::widget::ModelPath) {}
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            theme: &Theme,
            nodes: &mut HashMap<TaffyNodeId, *mut dyn Widget>,
        ) -> TaffyNodeId {
            self.child.build_layout(tree, theme, nodes)
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
            assert_eq!(update.focus, widget_node_id(Some(child), 0));
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
            let parent = tree.new_leaf(taffy::Style::default()).unwrap();
            nodes.insert(parent, std::ptr::from_mut::<dyn Widget>(&mut wrapper));
            for focused in [parent, child] {
                let update = build_tree_update(&wrapper, &nodes, Some(focused), 1.0);
                assert_eq!(update.focus, widget_node_id(Some(focused), 0));
                assert!(
                    update
                        .nodes
                        .iter()
                        .any(|(id, _)| *id == widget_node_id(Some(parent), 0))
                );
                assert!(
                    update
                        .nodes
                        .iter()
                        .any(|(id, _)| *id == widget_node_id(Some(child), 0))
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
        // aimed at the child id found no node.
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
            let child_id = AkNodeId(u64::from(child_node).saturating_add(1));
            let root_id = AkNodeId(u64::from(root_node).saturating_add(1));

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

        {
            let mut root = TestWidget::new(Some(canvas(vec![clip("a", false), clip("b", false)])));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);

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
            let mut ids: Vec<u64> = update.nodes.iter().map(|(id, _)| id.0).collect();
            ids.sort_unstable();
            let count = ids.len();
            ids.dedup();
            assert_eq!(count, ids.len(), "duplicate node id in the tree update");
        }
    }

    fn only(update: &TreeUpdate, role: AkRole) -> AkNode {
        let mut found = update
            .nodes
            .iter()
            .map(|(_, node)| node)
            .filter(|node| node.role() == role);
        let node = found.next().unwrap_or_else(|| panic!("no {role:?}"));
        assert!(found.next().is_none(), "two {role:?} nodes");
        node.clone()
    }

    #[test]
    fn a_row_names_its_trailing_switch_and_describes_it() {
        let mut row = crate::widget::ListRow::new("remove-data", "Remove its data")
            .secondary("Your settings and saved files go to the Trash.")
            .trailing(Box::new(crate::widget::Toggle::from_state(
                &crate::LocalState::new(false),
            )));
        let (_taffy, node_map, _root) = lay_out(&mut row);
        let update = build_tree_update(&row, &node_map, None, 1.0);
        let switch = only(&update, AkRole::Switch);
        assert_eq!(switch.label(), Some("Remove its data"));
        assert_eq!(
            switch.description(),
            Some("Your settings and saved files go to the Trash.")
        );
    }

    #[test]
    fn a_control_that_names_itself_keeps_its_name() {
        let mut row = crate::widget::ListRow::new("name", "Device name").trailing(Box::new(
            crate::widget::TextInput::new(Vec::new()).with_accessible_name("Name shown to others"),
        ));
        let (_taffy, node_map, _root) = lay_out(&mut row);
        let update = build_tree_update(&row, &node_map, None, 1.0);
        let field = only(&update, AkRole::TextInput);
        assert_eq!(field.label(), Some("Name shown to others"));
    }

    #[test]
    fn a_rows_chooser_is_named_by_the_row_and_not_by_its_value() {
        let mut row = crate::widget::ListRow::new("currency", "Currency").trailing(Box::new(
            crate::widget::Dropdown::from_state(
                vec!["EUR".to_owned(), "USD".to_owned()],
                &crate::LocalState::new("EUR".to_owned()),
            ),
        ));
        let (_taffy, node_map, _root) = lay_out(&mut row);
        let update = build_tree_update(&row, &node_map, None, 1.0);
        let chooser = only(&update, AkRole::ComboBox);
        assert_eq!(chooser.label(), Some("Currency"));
        assert_eq!(chooser.value(), Some("EUR"));
    }

    #[test]
    fn a_wrapped_trailing_slider_keeps_the_rows_name_and_description() {
        let mut row = crate::widget::ListRow::new("sdr-brightness", "Brightness")
            .secondary("How bright standard content is")
            .description("How bright standard content is")
            .trailing(Box::new(
                crate::widget::Container::new()
                    .with_no_padding()
                    .with_child(Box::new(crate::widget::Slider::new("sdr", "sdr"))),
            ));
        let (_taffy, node_map, _root) = lay_out(&mut row);
        let update = build_tree_update(&row, &node_map, None, 1.0);
        let slider = only(&update, AkRole::Slider);
        assert_eq!(slider.label(), Some("Brightness"));
        assert_eq!(slider.description(), Some("How bright standard content is"));
    }

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

    #[test]
    fn two_windows_that_share_a_control_name_each_keep_their_own_node() {
        let toggle = |x0: f64| {
            AccessNode::new(Role::Button)
                .with_name("Show sidebar")
                .with_action_target("sidebar-toggle")
                .with_bounds(vello::kurbo::Rect::new(x0, 0.0, x0 + 32.0, 32.0))
                .with_state(crate::a11y::AccessState::new().focusable(true))
        };
        let mut windows = TestWidget::new(Some(
            AccessNode::new(Role::Group).with_children(vec![toggle(0.0), toggle(604.0)]),
        ));
        let (_taffy, node_map, _root) = lay_out(&mut windows);
        let update = build_tree_update(&windows, &node_map, None, 1.0);
        let shown: Vec<(AkNodeId, f64)> = update
            .nodes
            .iter()
            .filter(|(_, node)| node.label() == Some("Show sidebar"))
            .map(|(id, node)| (*id, node.bounds().map_or(f64::NAN, |bounds| bounds.x0)))
            .collect();
        assert_eq!(
            shown.len(),
            2,
            "both windows' toggles are in the tree: {shown:?}"
        );
        assert_ne!(
            shown[0].0, shown[1].0,
            "each window's toggle has its own id"
        );
        assert_eq!(
            [shown[0].1, shown[1].1],
            [0.0, 604.0],
            "each stands where its window drew it"
        );
        let ids: std::collections::HashSet<_> = update.nodes.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids.len(),
            update.nodes.len(),
            "a node id addresses one node of the tree"
        );
    }

    #[repr(C)]
    struct FirstFieldHost {
        child: TestWidget,
        pad: u64,
    }

    impl Widget for FirstFieldHost {
        fn sync(&mut self, _s: &crate::widget::AppModel, _p: &crate::widget::ModelPath) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let kid = self.child.build_layout(taffy, theme, node_map);
            taffy.widget_node(taffy::Style::default(), &[kid], node_map, self)
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
        {
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
            assert!(!node.supports_action(AkAction::Focus));
            assert!(!node.supports_action(AkAction::Click));
        }

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
    fn sidebar_shared_separator_exports_resize_verbs_to_accesskit() {
        use crate::widget::{SplitHandle, SplitOrientation, SplitRatio};
        let handle = SplitHandle::new(SplitOrientation::Horizontal, SplitRatio::default());
        let mut root = TestWidget::new(None).with_child(Box::new(handle));
        let (_taffy, map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &map, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == AkRole::Splitter)
            .expect("actual shared divider exported");
        for action in [AkAction::Increment, AkAction::Decrement, AkAction::SetValue] {
            assert!(node.supports_action(action));
        }
        assert_eq!(node.value(), Some("50%"));
    }

    #[test]
    fn supported_actions_are_advertised_and_round_trip() {
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

            let set = Action::SetValue("hello".into());
            let ak = map_action(&set);
            assert_eq!(ak, AkAction::SetValue);
            let data = accesskit::ActionData::Value("hello".into());
            assert_eq!(
                accesskit_action_to_ours(ak, Some(&data)),
                Some(Action::SetValue("hello".into()))
            );

            assert_eq!(map_action(&Action::ToggleCheck), AkAction::Click);
        }

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
                    tilo_actions::ResourceSpec::new("document:1", "test.document", "Document")
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
        {
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

            let roles: Vec<AkRole> = update.nodes.iter().map(|(_, n)| n.role()).collect();
            assert!(roles.contains(&AkRole::Window));
            assert!(roles.contains(&AkRole::Group));
            assert!(roles.contains(&AkRole::Button));
            assert!(roles.contains(&AkRole::Label));
            assert!(
                !roles.contains(&AkRole::GenericContainer),
                "presentational widgets must not emit a node"
            );

            let (_, window) = update.nodes.iter().find(|(id, _)| *id == ROOT_ID).unwrap();
            assert_eq!(
                window.children().len(),
                2,
                "window should own Group + reparented Label"
            );

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

        {
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

        {
            let mut root = TestWidget::new(Some(AccessNode::new(Role::Document)));
            let (_taffy, node_map, _root_node) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            assert_eq!(update.focus, ROOT_ID);
        }

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
            assert!(update.nodes.iter().any(|(id, _)| *id == update.focus));
        }

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
    fn text_values_expose_editable_runs_but_never_protected_content() {
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
            let run_id = *node
                .children()
                .first()
                .expect("unselected terminal has a readable text run");
            let run = update
                .nodes
                .iter()
                .find(|(id, _)| *id == run_id)
                .expect("terminal text run reaches the platform tree");
            assert_eq!(run.1.role(), AkRole::TextRun);
            assert_eq!(run.1.value(), Some("$ cargo test"));
            assert_eq!(run.1.character_lengths().len(), 12);
            let selection = node
                .text_selection()
                .expect("unselected transcript has a collapsed caret");
            assert_eq!(selection.anchor.node, run_id);
            assert_eq!(selection.anchor.character_index, 0);
            assert_eq!(selection.focus, selection.anchor);
        }

        {
            let pw = TestWidget::new(Some(
                AccessNode::new(Role::TextInput)
                    .with_name("Protected input")
                    .with_value("hunter2")
                    .with_state(AccessState::new().protected(true)),
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
            assert_eq!(
                node.value(),
                None,
                "protected value must not leak to the AT"
            );
        }

        {
            // AccessKit's AT-SPI adapter offers Text and EditableText only on a
            // text input with a text run; without them a screen reader cannot
            // focus a password as text or type into it. The run never has
            // the value, its length, or a caret that moved with typing.
            for (role, secret) in [
                (Role::TextInput, ""),
                (Role::TextInput, "hunter2"),
                (Role::MultilineTextInput, "a much longer password"),
            ] {
                let pw = TestWidget::new(Some(
                    AccessNode::new(role)
                        .with_name("Passphrase")
                        .with_value(secret)
                        .with_text_selection(secret.len(), secret.len())
                        .with_state(AccessState::new().focusable(true).protected(true)),
                ));
                let mut root = TestWidget::new(None).with_child(Box::new(pw));
                let (_taffy, node_map, _root_node) = lay_out(&mut root);
                let update = build_tree_update(&root, &node_map, None, 1.0);
                let (field_id, field) = update
                    .nodes
                    .iter()
                    .find(|(_, n)| n.label() == Some("Passphrase"))
                    .unwrap();
                assert_eq!(field.role(), AkRole::PasswordInput);
                assert!(field.supports_action(AkAction::SetValue));
                let [run_id] = field.children() else {
                    panic!("one text run: {:?}", field.children());
                };
                let (_, run) = update.nodes.iter().find(|(id, _)| id == run_id).unwrap();
                assert_eq!(run.role(), AkRole::TextRun);
                assert_eq!(run.value(), Some(""));
                assert!(run.character_lengths().is_empty());
                let selection = field.text_selection().unwrap();
                assert_eq!(
                    (selection.anchor, selection.focus),
                    (
                        AkTextPosition {
                            node: *run_id,
                            character_index: 0
                        },
                        AkTextPosition {
                            node: *run_id,
                            character_index: 0
                        }
                    )
                );
                assert_eq!(
                    resolve_node_id(&root, &node_map, *run_id),
                    resolve_node_id(&root, &node_map, *field_id),
                    "a verb aimed at the run reaches the field"
                );
            }
        }

        {
            let mut root =
                TestWidget::new(Some(AccessNode::new(Role::Dialog).with_children(vec![
                    AccessNode::new(Role::TextInput)
                        .with_name("Sheet password")
                        .with_action_target("sheet:password")
                        .with_state(AccessState::new().protected(true)),
                ])));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            let field_id = label_id(&update, "Sheet password");
            let field = &update
                .nodes
                .iter()
                .find(|(id, _)| *id == field_id)
                .unwrap()
                .1;
            assert_eq!(field.role(), AkRole::PasswordInput);
            let [run_id] = field.children() else {
                panic!("one text run: {:?}", field.children());
            };
            let (_, run) = update.nodes.iter().find(|(id, _)| id == run_id).unwrap();
            assert_eq!(run.value(), Some(""));
            assert!(matches!(
                resolve_node_id(&root, &node_map, *run_id),
                Some(A11yTarget::Synthetic { ref semantic_target, .. })
                    if semantic_target.as_deref() == Some("sheet:password")
            ));
        }

        {
            let mut root = TestWidget::new(Some(
                AccessNode::new(Role::Label)
                    .with_name("Recovery key")
                    .with_state(AccessState::new().protected(true)),
            ));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let update = build_tree_update(&root, &node_map, None, 1.0);
            assert!(
                update
                    .nodes
                    .iter()
                    .all(|(_, node)| node.role() != AkRole::TextRun)
            );
        }

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
                    .with_state(AccessState::new().protected(true)),
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
            assert!(password.children().iter().all(|child| {
                native
                    .nodes
                    .iter()
                    .any(|(id, node)| id == child && node.value() == Some(""))
            }));
        }
    }

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
        {
            // A canvas widget has keyboard focus and nominates its second clip as
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

            let canvas_ak = widget_ak_id(&node_map, canvas_taffy).unwrap();
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

        {
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
                widget_ak_id(&node_map, button_taffy).unwrap(),
                "focus stays on the widget that holds the keyboard"
            );
            let (_, focused) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == update.focus)
                .unwrap();
            assert_eq!(focused.label(), Some("Save"));
        }

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

        {
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
            // Second clip of the first track: [track 0, clip 1].
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
                "the exported focus resolves to the marked clip"
            );
        }
    }

    #[test]
    fn resolve_node_id_maps_emitted_ids_back_or_reports_them_gone() {
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
            assert_eq!(
                resolve_node_id(&root, &node_map, label_id(&update, "People")),
                Some(A11yTarget::Widget(table_taffy)),
            );
            assert_eq!(
                resolve_node_id(&root, &node_map, ROOT_ID),
                Some(A11yTarget::Root),
            );
        }

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

        {
            // A host that publishes a child's semantics itself (the shell's
            // palette inside its overlay tree) must not have the child
            // node exported beside it, and keyboard focus on the child goes
            // to the representation.
            let child = TestWidget::new(Some(
                AccessNode::new(Role::Dialog).with_name("palette widget"),
            ));
            let mut host = TestWidget::new(Some(AccessNode::new(Role::Group).with_children(vec![
                    AccessNode::new(Role::Dialog)
                        .with_name("palette")
                        .with_state(AccessState::new().focusable(true).focused(true)),
                ])))
            .with_child(Box::new(child));
            host.represents = vec![0];
            let (_taffy, node_map, _n) = lay_out(&mut host);
            let widget_id = node_map
                .iter()
                .find(|(_, w)| {
                    unsafe { (***w).accessibility() }
                        .is_some_and(|a| a.name.as_deref() == Some("palette widget"))
                })
                .map(|(id, _)| *id)
                .expect("the child is laid out");
            let update = build_tree_update(&host, &node_map, Some(widget_id), 1.0);
            assert!(
                !update
                    .nodes
                    .iter()
                    .any(|(_, n)| n.label() == Some("palette widget")),
                "the represented child is not exported again"
            );
            assert_eq!(update.focus, label_id(&update, "palette"));
            let read: Vec<_> = crate::screen_reader::flatten_reading_order(&host)
                .into_iter()
                .filter_map(|node| node.name)
                .collect();
            assert_eq!(read, ["palette"], "nor read twice by the built-in reader");
        }

        {
            // A query field drawn by its owner (a palette) has no widget of its
            // own. It still needs a text run, or AT-SPI exposes no Text and no
            // EditableText and a screen reader cannot type into it.
            let mut root =
                TestWidget::new(Some(AccessNode::new(Role::Dialog).with_children(vec![
                    AccessNode::new(Role::TextInput)
                        .with_name("query")
                        .with_action_target("palette:query")
                        .with_value("fi")
                        .with_text_selection(2, 2),
                ])));
            let (_taffy, node_map, _n) = lay_out(&mut root);
            let owner = taffy_id_of(&node_map, Role::Dialog);
            let update = build_tree_update(&root, &node_map, None, 1.0);

            let field_id = label_id(&update, "query");
            let field = &update
                .nodes
                .iter()
                .find(|(id, _)| *id == field_id)
                .expect("the field is exported")
                .1;
            assert!(field.supports_action(AkAction::SetValue));
            let run_id = field.children()[0];
            let (_, run) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == run_id)
                .expect("the run is exported");
            assert_eq!(run.role(), AkRole::TextRun);
            assert_eq!(run.value(), Some("fi"));
            assert_eq!(
                field
                    .text_selection()
                    .map(|s| (s.anchor.node, s.focus.character_index)),
                Some((run_id, 2)),
            );
            assert_ne!(run_id, field_id);
            assert_eq!(
                resolve_node_id(&root, &node_map, run_id),
                resolve_node_id(&root, &node_map, field_id),
            );
            assert!(matches!(
                resolve_node_id(&root, &node_map, run_id),
                Some(A11yTarget::Synthetic { owner: o, ref ordinal_path, ref semantic_target, .. })
                    if o == owner
                        && ordinal_path == &[0]
                        && semantic_target.as_deref() == Some("palette:query")
            ));
        }

        {
            // The run has the widget text, so a verb aimed at it belongs
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

    #[test]
    fn a_table_rows_press_and_menu_reach_an_assistive_technology() {
        let row = TestWidget::new(Some(
            AccessNode::new(Role::Row)
                .with_name("Quarterly report.pdf")
                .with_action_target("row:a:0")
                .with_bounds(vello::kurbo::Rect::new(0.0, 0.0, 200.0, 24.0))
                .with_state(AccessState::new().focusable(true).context_menu(true)),
        ))
        .with_rect(0.0, 0.0, 200.0, 24.0);
        let mut root = TestWidget::new(None).with_child(Box::new(row));
        let (_taffy, node_map, _n) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::Row)
            .expect("a row reaches AccessKit");
        assert_eq!(node.label(), Some("Quarterly report.pdf".into()));
        assert!(
            node.supports_action(AkAction::Click),
            "the row's activation is offered"
        );
        assert!(
            node.supports_action(AkAction::ShowContextMenu),
            "the row's menu is offered"
        );

        let inert = TestWidget::new(Some(
            AccessNode::new(Role::Row)
                .with_name("Budget")
                .with_bounds(vello::kurbo::Rect::new(0.0, 0.0, 200.0, 24.0)),
        ))
        .with_rect(0.0, 0.0, 200.0, 24.0);
        let mut root = TestWidget::new(None).with_child(Box::new(inert));
        let (_taffy, node_map, _n) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == AkRole::Row)
            .expect("a row reaches AccessKit");
        assert!(
            !node.supports_action(AkAction::Click),
            "a row nothing can reach is not offered a press"
        );
    }
}
