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
//! updates and focus events line up frame-to-frame. We derive each node's id
//! **deterministically from the `taffy::NodeId`** found in the `node_map` (a
//! `u64`), wrapped in our own newtype so the *root* id is reserved. When a
//! widget has no taffy node (rare — only widgets that were never laid out), we
//! fall back to a structural walk index, which is also stable for a fixed tree
//! shape.
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

use crate::a11y::{AccessNode, Action, Role};
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
    // Reverse map: widget pointer -> taffy id, so we can look up a stable id
    // for any &dyn Widget we encounter during the walk.
    let mut ptr_to_taffy: HashMap<*const dyn Widget, TaffyNodeId> = HashMap::new();
    for (&taffy_id, &w_ptr) in node_map {
        ptr_to_taffy.insert(w_ptr as *const dyn Widget, taffy_id);
    }

    let focused_ak = focused.map(|t| AkNodeId(u64::from(t) + 1));

    let mut nodes: Vec<(AkNodeId, AkNode)> = Vec::new();
    let mut walk_counter: u64 = 0;

    // The synthetic window root gathers the top-level accessible nodes.
    let mut root_node = AkNode::new(AkRole::Window);
    root_node.set_label("qOS application window");

    let mut root_children: Vec<AkNodeId> = Vec::new();
    visit(
        root,
        &ptr_to_taffy,
        scale,
        &mut walk_counter,
        &mut nodes,
        &mut root_children,
    );
    root_node.set_children(root_children);
    nodes.push((ROOT_ID, root_node));

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(ROOT_ID)),
        tree_id: accesskit::TreeId::ROOT,
        // AccessKit requires `focus` to be a node that exists in the tree; the
        // synthetic window root is the safe fallback when nothing is focused.
        focus: focused_ak.unwrap_or(ROOT_ID),
    }
}

/// DFS helper. For the widget `w`:
///   - if `w.accessibility()` is `Some`, emit a node, recurse to gather *its*
///     accessible children, and append the node's id to `parent_children`;
///   - if `None` (presentational), recurse and append the children's ids
///     directly to `parent_children` (reparenting — the widget collapses out).
fn visit(
    w: &dyn Widget,
    ptr_to_taffy: &HashMap<*const dyn Widget, TaffyNodeId>,
    scale: f64,
    walk_counter: &mut u64,
    nodes: &mut Vec<(AkNodeId, AkNode)>,
    parent_children: &mut Vec<AkNodeId>,
) {
    let access = w.accessibility();
    let w_ptr = w as *const dyn Widget;
    let taffy_id = ptr_to_taffy.get(&w_ptr).copied();

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
                visit(child, ptr_to_taffy, scale, walk_counter, nodes, &mut my_children);
            }
            ak.set_children(my_children);

            nodes.push((id, ak));
            parent_children.push(id);
        }
        None => {
            // Presentational: collapse out, reparent children onto the parent.
            *walk_counter += 1;
            for child in w.children() {
                visit(child, ptr_to_taffy, scale, walk_counter, nodes, parent_children);
            }
        }
    }
}

/// Copy an [`AccessNode`]'s name/description/value/state plus the widget's
/// bounds and supported actions onto an [`accesskit::Node`].
fn apply_semantics(ak: &mut AkNode, access: &AccessNode, w: &dyn Widget, scale: f64) {
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

    apply_state(ak, access);

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
