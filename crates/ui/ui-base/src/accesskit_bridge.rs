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
use crate::a11y_walk::HostedByIndex;
use crate::a11y_walk::{A11yVisitor, Visit, walk, walk_synthetic_children};
use crate::text_break::{grapheme_indices, graphemes};
use crate::widget::Widget;

mod floating_surface;
#[cfg(feature = "native")]
pub use floating_surface::surface_tree_update;
pub use floating_surface::{publish_host_part, surface_action_target};

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

/// [`map_action`] with the data its verb carries: what the session asks of a
/// window in another process, which runs the same action on `target` as its
/// own assistive client would.
#[must_use]
pub fn ours_action_to_accesskit(
    action: &Action,
    target: accesskit::NodeId,
) -> (AkAction, Option<accesskit::ActionData>) {
    use accesskit::ActionData;
    let data = match action {
        Action::SetValue(value) => Some(ActionData::Value(value.clone().into())),
        Action::SetSelection { anchor, focus } => {
            Some(ActionData::SetTextSelection(accesskit::TextSelection {
                anchor: accesskit::TextPosition {
                    node: target,
                    character_index: *anchor,
                },
                focus: accesskit::TextPosition {
                    node: target,
                    character_index: *focus,
                },
            }))
        }
        _ => None,
    };
    (map_action(action), data)
}

/// Prefers `taffy_id` (offset by 1 so it can never be [`ROOT_ID`], since
/// taffy ids start at 0); falls back to a high-range walk index for widgets
/// with no `node_map` entry.
pub fn widget_node_id(taffy_id: Option<TaffyNodeId>, walk_index: u64) -> AkNodeId {
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
pub fn resolve_node_id_bound<S: BuildHasher>(
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
    let Ok(_entry) = crate::widget::bound_nodes::enter_traversal::<BOUND>(w) else {
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
        let Ok(_entry) = crate::widget::bound_nodes::enter_traversal::<BOUND>(widget) else {
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
        .0
}

/// Adapt the redacted semantic tree to native-provider requirements.
/// AccessKit Windows 0.35 offers `IValueProvider` only when a node has a value.
/// A fixed empty placeholder enables `SetValue` on password fields without
/// disclosing their text, length, selection, or whether they contain anything.
/// The shared semantic tree continues to carry no password value at all.
#[cfg(feature = "native")]
pub fn native_tree_update(update: TreeUpdate) -> TreeUpdate {
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
pub fn build_tree_update_with_resources_bound<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    resources: &[tilo_actions::ResourceSpec],
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(&[root], node_map, focused, scale, resources).0
}

pub fn build_tree_update_bound<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(&[root], node_map, focused, scale, &[]).0
}

pub fn build_presentation_tree_bound<S: BuildHasher>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(roots, node_map, focused, scale, &[]).0
}

/// [`build_tree_update_bound`] with each node's [`AccessNode::title`] beside
/// its id: how a test finds a row by the line it is titled with.
pub fn build_tree_update_with_titles_bound<S: BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
) -> (TreeUpdate, Vec<(AkNodeId, String)>) {
    build_tree_update_with_resources_impl::<S, true>(&[root], node_map, focused, scale, &[])
}

/// [`build_presentation_tree_bound`] with each node's
/// [`AccessNode::title`] beside its id.
pub fn build_presentation_tree_with_titles_bound<S: BuildHasher>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
) -> (TreeUpdate, Vec<(AkNodeId, String)>) {
    build_tree_update_with_resources_impl::<S, true>(roots, node_map, focused, scale, &[])
}

/// Multiple presentation roots retain ordinary live resource nodes when their
/// owner is interactive; callers supply only currently presented resources.
pub fn build_presentation_with_resources_bound<S: BuildHasher>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    resources: &[tilo_actions::ResourceSpec],
) -> TreeUpdate {
    build_tree_update_with_resources_impl::<S, true>(roots, node_map, focused, scale, resources).0
}

fn build_tree_update_with_resources_impl<S: BuildHasher, const BOUND: bool>(
    roots: &[&dyn Widget],
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
    resources: &[tilo_actions::ResourceSpec],
) -> (TreeUpdate, Vec<(AkNodeId, String)>) {
    let ptr_to_taffy = reverse_node_map::<S, BOUND>(roots, node_map);

    let focused_ak = focused.map(|t| AkNodeId(u64::from(t).saturating_add(1)));

    let mut tree_walk = TreeWalk::<BOUND> {
        ptr_to_taffy: &ptr_to_taffy,
        scale,
        focused,
        walk_counter: 0,
        nodes: Vec::new(),
        titles: Vec::new(),
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
        titles,
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

    (
        TreeUpdate {
            nodes,
            tree: Some(TreeInfo::new(ROOT_ID)),
            tree_id: accesskit::TreeId::ROOT,
            focus,
        },
        titles,
    )
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
                tilo_action_types::KEY_SEP,
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
    /// Each exported node's [`AccessNode::title`], by its node id: the line a
    /// row is found by, beside the joined name assistive technology reads.
    titles: Vec<(AkNodeId, String)>,
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
        crate::a11y_walk::with_slot_words(&mut access, slot.as_ref());
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
        walk_synthetic_children::<BOUND, Self, _>(self, w, &access.children, hosted, &mut scope);
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
        if let Some(title) = &access.title {
            self.titles.push((id, title.clone()));
        }
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
        if let Some(title) = &access.title {
            self.titles.push((id, title.clone()));
        }
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
    // Click depends on the roles that can actually be operated. Offering it
    // on every node — which this used to do — makes an AT read "clickable" on
    // a plain label. A list item answers a press only where it carries a
    // target one reaches (the row a person opens); a plain settings line is
    // no control, and offering it the press would take the press a caller
    // made on the chooser it stands beside. `Row` is one as soon as it has a
    // target an activation reaches.
    let actionable = access.action_target.is_some() && access.bounds.is_some();
    let clickable = match access.role {
        Role::ListItem => actionable,
        role => crate::a11y::activation_role(role) || (role == Role::Row && actionable),
    };
    if !access.state.disabled && clickable {
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
