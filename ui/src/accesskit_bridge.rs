//! AccessKit bridge: turns the toolkit's per-widget semantic model
//! ([`crate::a11y::AccessNode`], from
//! [`Widget::accessibility`](crate::widget::Widget::accessibility)) into an
//! [`accesskit::TreeUpdate`], published over AT-SPI2.
//!
//! [`build_tree_update`] is a pure function of `(root_widget, node_map,
//! focused)`: a depth-first walk that emits one node per widget returning
//! `Some(AccessNode)`, and collapses presentational widgets (`None`) by
//! reparenting their accessible children onto the nearest ancestor —
//! mirroring ARIA `role="presentation"`. Being pure and keyed off the same
//! `node_map` the `App` maintains makes it unit-testable without a live
//! AT-SPI bus.
//!
//! Each node's [`accesskit::NodeId`] is derived deterministically from its
//! `taffy::NodeId` (see [`widget_node_id`]) so ids stay stable frame to
//! frame; widgets with no taffy node fall back to a structural walk index.

use std::collections::HashMap;

use accesskit::{
    Action as AkAction, Node as AkNode, NodeId as AkNodeId, Rect as AkRect, Role as AkRole,
    TextPosition as AkTextPosition, TextSelection as AkTextSelection, Toggled, TreeInfo,
    TreeUpdate,
};
use taffy::NodeId as TaffyNodeId;
use unicode_segmentation::UnicodeSegmentation;

use crate::a11y::{AccessNode, Action, Role};
use crate::widget::Widget;

/// Reserved so no widget can collide with it: widget ids are offset by +1
/// from their taffy node id, and structural fallback ids are large offsets
/// — see [`widget_node_id`].
pub const ROOT_ID: AkNodeId = AkNodeId(0);
const TEXT_RUN_ID_BASE: u64 = 1u64 << 62;
pub const SYNTHETIC_ID_BASE: u64 = 1u64 << 48;

/// E.g. [`Role::Spinner`] becomes a busy [`AkRole::ProgressIndicator`] and
/// [`Role::ToggleButton`] becomes [`AkRole::Button`] carrying `toggled` state.
#[must_use]
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
        Role::Canvas => AkRole::Canvas,
        Role::Unknown => AkRole::Unknown,
    }
}

/// `ToggleCheck` has no dedicated verb — toggling is a `Click` at the
/// platform level. `SetValue`/`SetSelection` payloads travel in `ActionData`
/// on the round trip ([`accesskit_action_to_ours`]).
#[must_use]
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
#[must_use]
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
#[must_use]
fn widget_node_id(taffy_id: Option<TaffyNodeId>, walk_index: u64) -> AkNodeId {
    match taffy_id {
        // Bounded by the number of taffy nodes in one tree — nowhere near
        // `u64::MAX`; saturating just makes that bound explicit.
        Some(id) => AkNodeId(u64::from(id).saturating_add(1)),
        // 1<<48 keeps fallback ids well clear of any realistic taffy id
        // range, and `walk_index` is bounded by the number of widgets
        // walked in one tree.
        None => AkNodeId(SYNTHETIC_ID_BASE.saturating_add(walk_index)),
    }
}

fn text_run_node_id(parent: AkNodeId) -> AkNodeId {
    AkNodeId(TEXT_RUN_ID_BASE | parent.0)
}

fn byte_to_grapheme_index(text: &str, byte: usize) -> usize {
    let byte = crate::text_selection::grapheme_boundary_at_or_before(text, byte.min(text.len()));
    text.grapheme_indices(true)
        .take_while(|(start, _)| *start < byte)
        .map(|(_, grapheme)| grapheme.len().div_ceil(u8::MAX as usize))
        .sum()
}

fn accessible_character_lengths(text: &str) -> Vec<u8> {
    let mut lengths = Vec::new();
    for grapheme in text.graphemes(true) {
        let mut remaining = grapheme.len();
        while remaining > 0 {
            let chunk = remaining.min(u8::MAX as usize);
            // `chunk` is clamped to `u8::MAX` above, so the cast never truncates.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "chunk is clamped to u8::MAX just above"
            )]
            lengths.push(chunk as u8);
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
pub fn build_tree_update<S: std::hash::BuildHasher>(
    root: &dyn Widget,
    node_map: &HashMap<TaffyNodeId, *mut dyn Widget, S>,
    focused: Option<TaffyNodeId>,
    scale: f64,
) -> TreeUpdate {
    // Reverse map so we can look up a stable id for any &dyn Widget during the walk.
    let mut ptr_to_taffy: HashMap<*const dyn Widget, TaffyNodeId> = HashMap::new();
    for (&taffy_id, &w_ptr) in node_map {
        ptr_to_taffy.insert(w_ptr as *const dyn Widget, taffy_id);
    }

    // Same taffy-id-count bound as `widget_node_id` above.
    let focused_ak = focused.map(|t| AkNodeId(u64::from(t).saturating_add(1)));

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

    // AccessKit panics the AT if `focus` references a node absent from this
    // update. The taffy id can be stale (relayout reassigns ids) or point
    // at a collapsed node, so only honour it when actually emitted.
    let focus = match focused_ak {
        Some(id) if nodes.iter().any(|(nid, _)| *nid == id) => id,
        _ => ROOT_ID,
    };

    TreeUpdate {
        nodes,
        tree: Some(TreeInfo::new(ROOT_ID)),
        tree_id: accesskit::TreeId::ROOT,
        focus,
    }
}

/// DFS helper: if `w.accessibility()` is `None` (presentational), reparent
/// the children's ids straight onto `parent_children` instead of emitting a node.
fn visit(
    w: &dyn Widget,
    ptr_to_taffy: &HashMap<*const dyn Widget, TaffyNodeId>,
    scale: f64,
    walk_counter: &mut u64,
    nodes: &mut Vec<(AkNodeId, AkNode)>,
    parent_children: &mut Vec<AkNodeId>,
) {
    let access = w.accessibility();
    let w_ptr = std::ptr::from_ref::<dyn Widget>(w);
    let taffy_id = ptr_to_taffy.get(&w_ptr).copied();

    if let Some(access_node) = access {
        let id = widget_node_id(taffy_id, *walk_counter);
        *walk_counter = walk_counter.saturating_add(1);

        let mut ak = AkNode::new(map_role(access_node.role));
        apply_semantics(&mut ak, &access_node, w, scale);

        let mut my_children: Vec<AkNodeId> = Vec::new();
        for child in w.children() {
            visit(
                child,
                ptr_to_taffy,
                scale,
                walk_counter,
                nodes,
                &mut my_children,
            );
        }
        // Synthetic children with no backing widget — a table's rows/cells.
        for synth in &access_node.children {
            visit_synthetic(synth, walk_counter, nodes, &mut my_children);
        }
        if !access_node.state.protected
            && let (Some(value), Some(selection)) = (&access_node.value, access_node.text_selection)
        {
            let run_id = text_run_node_id(id);
            let mut run = AkNode::new(AkRole::TextRun);
            run.set_value(value.clone());
            run.set_character_lengths(accessible_character_lengths(value));
            let rect = w.get_rect();
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
            nodes.push((run_id, run));
        }
        ak.set_children(my_children);

        nodes.push((id, ak));
        parent_children.push(id);
    } else {
        // Presentational: collapse out, reparent children onto the parent.
        *walk_counter = walk_counter.saturating_add(1);
        for child in w.children() {
            visit(
                child,
                ptr_to_taffy,
                scale,
                walk_counter,
                nodes,
                parent_children,
            );
        }
    }
}

/// A node with no backing [`Widget`] — a table row/cell, a virtualized list
/// item. Carries no geometry; everything else is applied as for a real node.
fn visit_synthetic(
    access: &AccessNode,
    walk_counter: &mut u64,
    nodes: &mut Vec<(AkNodeId, AkNode)>,
    parent_children: &mut Vec<AkNodeId>,
) {
    let id = widget_node_id(None, *walk_counter);
    *walk_counter = walk_counter.saturating_add(1);

    let mut ak = AkNode::new(map_role(access.role));
    apply_semantics_common(&mut ak, access);

    let mut my_children: Vec<AkNodeId> = Vec::new();
    for child in &access.children {
        visit_synthetic(child, walk_counter, nodes, &mut my_children);
    }
    ak.set_children(my_children);

    nodes.push((id, ak));
    parent_children.push(id);
}

/// Resolve a high-range AccessKit id back to the widget that published the
/// synthetic `AccessNode` and its child-index path. This repeats the same DFS
/// counter used by `build_tree_update`, so immediate-mode controls can be
/// focused and activated without pretending they are taffy widgets.
pub fn resolve_synthetic_target<'a>(
    root: &'a dyn Widget,
    target: AkNodeId,
) -> Option<(*const (dyn Widget + 'a), Vec<usize>)> {
    if target.0 < SYNTHETIC_ID_BASE || target.0 >= TEXT_RUN_ID_BASE {
        return None;
    }
    fn visit_widget<'a>(
        widget: &'a dyn Widget,
        target: u64,
        counter: &mut u64,
    ) -> Option<(*const (dyn Widget + 'a), Vec<usize>)> {
        let access = widget.accessibility();
        *counter = counter.saturating_add(1);
        if let Some(access) = access {
            for child in widget.children() {
                if let Some(found) = visit_widget(child, target, counter) {
                    return Some(found);
                }
            }
            for (index, child) in access.children.iter().enumerate() {
                if let Some(found) = visit_child(widget, child, vec![index], target, counter) {
                    return Some(found);
                }
            }
        } else {
            for child in widget.children() {
                if let Some(found) = visit_widget(child, target, counter) {
                    return Some(found);
                }
            }
        }
        None
    }
    fn visit_child<'a>(
        owner: &'a dyn Widget,
        child: &AccessNode,
        path: Vec<usize>,
        target: u64,
        counter: &mut u64,
    ) -> Option<(*const (dyn Widget + 'a), Vec<usize>)> {
        let id = SYNTHETIC_ID_BASE.saturating_add(*counter);
        *counter = counter.saturating_add(1);
        if id == target {
            return Some((std::ptr::from_ref(owner), path));
        }
        for (index, nested) in child.children.iter().enumerate() {
            let mut nested_path = path.clone();
            nested_path.push(index);
            if let Some(found) = visit_child(owner, nested, nested_path, target, counter) {
                return Some(found);
            }
        }
        None
    }

    visit_widget(root, target.0, &mut 0)
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

/// The widget-independent half of [`apply_semantics`], also shared with
/// [`visit_synthetic`] (table rows/cells), which has no geometry.
fn apply_semantics_common(ak: &mut AkNode, access: &AccessNode) {
    if let Some(name) = &access.name {
        ak.set_label(name.clone());
    }
    if let Some(desc) = &access.description {
        ak.set_description(desc.clone());
    }
    // Never expose a protected (password) value, even if the widget mistakenly populated one.
    if !access.state.protected
        && let Some(value) = &access.value
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

    if access.state.focusable && !access.state.disabled {
        ak.add_action(AkAction::Focus);
    }
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
        _ => {}
    }
}

fn apply_state(ak: &mut AkNode, access: &AccessNode) {
    let state = &access.state;

    if state.focused {
        // Focus is carried by the tree's `focus` field, nothing per-node here.
    }
    if state.disabled {
        ak.set_disabled();
    }
    if state.hidden {
        ak.set_hidden();
    }
    if state.read_only {
        ak.set_read_only();
    }
    if state.required {
        ak.set_required();
    }
    if state.busy {
        ak.set_busy();
    }
    if state.modal {
        ak.set_modal();
    }
    // AccessKit 0.24 has no `multiline` flag; expressed via `MultilineTextInput` (see `map_role`).
    if let Some(checked) = state.checked {
        ak.set_toggled(if state.mixed {
            Toggled::Mixed
        } else if checked {
            Toggled::True
        } else {
            Toggled::False
        });
    } else if state.mixed {
        ak.set_toggled(Toggled::Mixed);
    }
    if let Some(expanded) = state.expanded {
        ak.set_expanded(expanded);
    }
    // Clear `selected` explicitly (not just omit) for selectable roles so an
    // AT doesn't read "not selected" on unrelated controls.
    if state.selected {
        ak.set_selected(true);
    } else if matches!(
        access.role,
        Role::ListItem | Role::TreeItem | Role::Tab | Role::Row | Role::Cell | Role::MenuItem
    ) {
        ak.set_selected(false);
    }
    if state.invalid {
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
            self.kids.iter().map(Box::as_ref).collect()
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

    #[test]
    fn synthetic_children_flow_into_the_tree_with_position_and_labels() {
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

    #[test]
    fn focus_targets_the_focused_widget() {
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

    #[test]
    fn focus_falls_back_to_root_when_nothing_focused() {
        let mut root = TestWidget::new(Some(AccessNode::new(Role::Document)));
        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        assert_eq!(update.focus, ROOT_ID);
    }

    #[test]
    fn focus_falls_back_to_root_when_focused_id_is_stale() {
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
        assert_eq!(
            node.value(),
            None,
            "protected value must not leak to the AT"
        );
    }

    #[test]
    fn editable_text_exposes_grapheme_run_and_directional_selection() {
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
            [1, u8::try_from(family.len()).unwrap(), 1].as_slice(),
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

    #[test]
    fn synthetic_accesskit_id_resolves_to_owner_and_child_path() {
        let mut access = AccessNode::new(Role::Dialog).with_name("History");
        access.children.push(
            AccessNode::new(Role::Button)
                .with_name("Compare")
                .with_state(AccessState::new().focusable(true)),
        );
        let mut root = TestWidget::new(Some(access));
        let (_taffy, node_map, _root_node) = lay_out(&mut root);
        let update = build_tree_update(&root, &node_map, None, 1.0);
        let button_id = update
            .nodes
            .iter()
            .find_map(|(id, node)| (node.role() == AkRole::Button).then_some(*id))
            .unwrap();
        let (owner, path) = resolve_synthetic_target(&root, button_id).unwrap();
        assert_eq!(path, vec![0]);
        assert!(std::ptr::addr_eq(
            owner,
            std::ptr::from_ref::<dyn Widget>(&root)
        ));
    }
}
