//! Operating on-screen controls the way a person does, without a window.
//!
//! [`FrameHarness::controls`] lists what the exported accessibility tree
//! carries — every button, field, tab, toggle and slider, whether or not
//! its app also declared an action for it, including the synthetic elements
//! a canvas or a framework view exposes — and
//! [`FrameHarness::operate_control`] presses, focuses, fills, types into or
//! scrolls one through [`FrameHarness::perform_a11y_action`], the route a
//! screen reader's request takes in the live `App`. Projected `ui:<node>`
//! actions ([`FrameHarness::invoke_action`]) resolve here too, so an agent
//! driving a headless mount reaches everything a person in a window can.

use super::{FrameHarness, Rect};
use crate::a11y::Action;
use crate::a11y_tree::{
    SEMANTIC_OPERATION_TYPE, SemanticActionKind, SemanticId, SemanticNode, SemanticTree,
};
use crate::host_core::HostCore as _;
use crate::widget::Widget;
use accesskit::{Action as AkAction, Role};

/// Resource id prefix of a projected control (`ui:<percent-encoded node>`).
pub const CONTROL_RESOURCE_PREFIX: &str = "ui:";

/// How far following someone has brought a frame's view: the newest action
/// of theirs it went to, whether it was shown, and whether its view had to be
/// opened first. A follower starts a new one each time they follow.
#[derive(Debug, Default)]
pub struct FollowedView {
    invocation: Option<tilo_actions::InvocationId>,
    shown: bool,
    opened: bool,
}

/// One control as the exported accessibility tree carries it now.
#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    /// The exported node id, in decimal. Derived from the element's key, so
    /// it survives relayouts and names the same element after siblings
    /// come and go.
    pub id: String,
    /// Exported role.
    pub role: Role,
    /// Accessible name, already localized.
    pub name: Option<String>,
    /// A field's contents, a slider's readout.
    pub value: Option<String>,
    /// Where it is on the surface, in logical pixels.
    pub bounds: Option<Rect>,
    /// Holds keyboard focus.
    pub focused: bool,
    /// Present but inoperable.
    pub disabled: bool,
    /// The accessibility verbs it answers (`activate`, `focus`,
    /// `set-value`, …).
    pub verbs: Vec<String>,
}

/// What a person can do to a control.
#[derive(Clone, Debug, PartialEq)]
pub enum ControlInput {
    /// Click, tap or press Enter on it: its `activate` verb, or a click at
    /// its centre when it has none.
    Press,
    /// Move keyboard focus to it.
    Focus,
    /// Replace its value (a field, a slider): its `set-value` verb.
    SetValue(String),
    /// Focus it, then type text at its caret through the input-method path.
    TypeText(String),
    /// Point at it and turn the wheel by line deltas.
    Scroll {
        /// Horizontal lines.
        dx: f64,
        /// Vertical lines.
        dy: f64,
    },
    /// Check or uncheck it.
    Toggle,
    /// Step it up.
    Increment,
    /// Step it down.
    Decrement,
    /// Open it.
    Expand,
    /// Close it.
    Collapse,
}

const VERBS: &[(AkAction, &str)] = &[
    (AkAction::Click, "activate"),
    (AkAction::Focus, "focus"),
    (AkAction::SetValue, "set-value"),
    (AkAction::Increment, "increment"),
    (AkAction::Decrement, "decrement"),
    (AkAction::Expand, "expand"),
    (AkAction::Collapse, "collapse"),
    (AkAction::ShowContextMenu, "show-context-menu"),
];

/// An agent's invocation of `action` on resource `resource`, as the broker
/// reports it while it applies: what [`FrameHarness::semantic_locus`] places.
#[must_use]
pub fn agent_acting(resource: &str, action: &str) -> tilo_actions::SemanticInteraction {
    tilo_actions::SemanticInteraction {
        version: tilo_actions::INTERACTION_VERSION,
        invocation: tilo_actions::InvocationId::new(),
        sequence: 1,
        actor: tilo_actions::InteractionActor {
            id: "agent".into(),
            kind: tilo_actions::ActorKind::Agent,
            for_person: None,
        },
        phase: tilo_actions::InteractionPhase::Applying,
        action: tilo_actions::InteractionAction {
            id: action.into(),
            label: action.into(),
            app: None,
            step: None,
            creates: None,
        },
        resource: tilo_actions::ActionResource {
            id: resource.into(),
            kind: resource.into(),
            label: resource.into(),
        },
        anchor: tilo_actions::InteractionAnchor::Resource,
        group: None,
    }
}

impl FrameHarness {
    fn semantic_tree(&self) -> SemanticTree {
        self.drain_component_wakes();
        let mut resources = Vec::new();
        crate::widget::collect_widget_resources(
            self.root.as_ref(),
            &self.state.doc,
            &mut resources,
        );
        self.semantic_tree_over(&resources)
    }

    /// The semantic tree the live `App` builds: over the exported node map
    /// and the bound components' resources, so widgets inside a hosted
    /// component are reached the way assistive technology reaches them.
    pub(super) fn semantic_tree_over(
        &self,
        resources: &[tilo_actions::ResourceSpec],
    ) -> SemanticTree {
        let components = crate::bound_dispatch::discover_bound_resources(
            self.root.as_ref(),
            &self.state.doc,
            None,
        )
        .unwrap_or_default();
        let mut tree = SemanticTree::new();
        tree.refresh_components(
            self.root.as_ref(),
            &self.node_map,
            &self.bindings.semantic_node_names(&self.node_map),
            self.focused,
            resources,
            &components,
        );
        tree
    }

    /// Resolve an exact action against the retained layout, without draining
    /// wakes, laying out, scrolling, focusing, or executing an action. Hosts
    /// must associate this metadata with the presentation that owns the layout;
    /// it is not a portable semantic node identity or a capture receipt.
    pub fn action_ui_routes(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Vec<crate::ActionUiRoute> {
        let Some((tree, bindings)) = self.retained_action_geometry() else {
            return Vec::new();
        };
        crate::action_locus::action_ui_routes(&tree, &bindings, address)
    }

    /// The exact retained control locus for a semantic action, if uniquely
    /// available. Missing/ambiguous/domain-specific targets return no geometry.
    /// An agent that has said where its caret or selection is in this frame's
    /// text is there, whatever control its action is bound to.
    pub fn semantic_locus(&self, interaction: &tilo_actions::SemanticInteraction) -> Option<Rect> {
        if interaction.actor.kind.acts_for_someone()
            && let Some((_, rect)) = self.text_locus(&interaction.actor.id)
        {
            return Some(rect);
        }
        let (tree, bindings) = self.retained_action_geometry()?;
        crate::action_locus::interaction_bounds(&tree, &bindings, interaction)
    }

    /// Where this frame shows `actor`'s caret or selection in text: the widget
    /// that draws it and its box, when a text widget here reports it.
    fn text_locus(&self, actor: &str) -> Option<(*const dyn Widget, Rect)> {
        let (key, range) = crate::presence::capability::actor_text_range(&self.presence, actor)?;
        crate::presence::capability::range_locus(self.root.as_ref(), &self.theme, key, range)
    }

    /// Bring the view to where `actor` acts, while this frame follows them:
    /// the text their caret or selection is in scrolls into view; else the
    /// control their newest action here is bound to; where no view here shows
    /// it (another sheet, tab or page), the nearest view its resource
    /// publishes is opened through its own action, and the place is scrolled
    /// to once that view is laid out. Each action is gone to once, so the
    /// follower can look around until the next one. Answers whether the view
    /// went there.
    /// Quiet: moves the view through `relayout`, and opens one through `invoke_action`, each of
    /// which invalidates; a frame where it finds nothing new changes nothing.
    pub fn follow_view(&mut self, actor: &str, followed: &mut FollowedView) -> bool {
        let Some(interaction) = self
            .presence
            .agent_marker_interactions(crate::time::now())
            .into_iter()
            .find(|interaction| interaction.actor.id == actor)
        else {
            return false;
        };
        // There is an action to follow: what it left to show is read off the layout as it is now.
        self.invalidate_local();
        if followed.invocation != Some(interaction.invocation) {
            *followed = FollowedView {
                invocation: Some(interaction.invocation),
                ..FollowedView::default()
            };
        }
        if followed.shown {
            return false;
        }
        if let Some((widget, rect)) = self.text_locus(actor) {
            followed.shown = true;
            return self.reveal(widget, rect);
        }
        let locus = self
            .retained_action_geometry()
            .and_then(|(tree, bindings)| {
                let node = crate::action_locus::interaction_node(&tree, &bindings, &interaction)?;
                Some((node.widget_taffy_id?, node.bounds?))
            });
        if let Some((owner, bounds)) = locus {
            followed.shown = true;
            let Some(&widget) = self.node_map.get(&taffy::NodeId::from(owner)) else {
                return false;
            };
            return self.reveal(widget.cast_const(), bounds);
        }
        if followed.opened {
            return false;
        }
        followed.opened = true;
        self.open_view_of(&interaction.resource.id)
    }

    /// Scroll `rect` of `target` into every viewport that shows it, and lay
    /// the frame out again when something moved. Answers whether it did.
    fn reveal(&mut self, target: *const dyn Widget, rect: Rect) -> bool {
        let moved = self.root.reveal_descendant(target, rect);
        if moved {
            self.relayout();
        }
        moved
    }

    /// Open the view nearest `resource` that the published resources offer
    /// (its own, else its closest parent's), as a person choosing it would.
    fn open_view_of(&mut self, resource: &str) -> bool {
        fn nearest(resources: &[tilo_actions::ResourceSpec], id: &str) -> Option<(String, String)> {
            resources.iter().find_map(|spec| {
                let own = || {
                    let view = spec
                        .views
                        .iter()
                        .find(|view| view.primary)
                        .or_else(|| spec.views.first())?;
                    Some((spec.id.as_str().to_owned(), view.open_action.clone()))
                };
                if spec.id.as_str() == id {
                    return own();
                }
                let child = spec
                    .children
                    .iter()
                    .any(|child| child.id.as_str() == id || nearest(&child.children, id).is_some());
                child
                    .then(|| nearest(&spec.children, id).or_else(own))
                    .flatten()
            })
        }
        let Some((owner, open)) = nearest(&self.resources(), resource) else {
            return false;
        };
        let address = tilo_actions::ActionAddress::new(owner, open);
        self.invoke_action(&address, &serde_json::Map::new(), false, None)
            .is_ok()
    }

    /// Resolve only a currently mounted publication owner, exact app instance
    /// and record. The alias is never decoded without its host/lifetime registry.
    /// Reads the retained frame without input, wake draining or document edits.
    pub fn published_action_locus(
        &self,
        instance: crate::application::InstanceId,
        app: &str,
        record: &str,
        address: &tilo_actions::ActionAddress,
    ) -> Option<Rect> {
        if !self.action_geometry_visible() {
            return None;
        }
        let target = self.bound_publication.target(address).ok()??;
        if target.binding.instance_id != instance
            || target.binding.app_id != app
            || target.binding.checkout.id.as_str() != record
        {
            return None;
        }
        let snapshots = crate::bound_dispatch::discover_bound_resources(
            self.root.as_ref(),
            &self.state.doc,
            None,
        )
        .ok()?;
        let snapshot = snapshots
            .iter()
            .find(|s| s.binding.same_component(&target.binding))?;
        let mut result = Vec::new();
        self.visit_publication_locus(
            self.root.as_ref(),
            &target,
            snapshot.resources(),
            &mut result,
        );
        if result.len() == 1 {
            result.pop().flatten()
        } else {
            None
        }
    }

    fn visit_publication_locus(
        &self,
        widget: &dyn crate::widget::Widget,
        target: &crate::bound_dispatch::BoundActionTarget,
        resources: &[tilo_actions::ResourceSpec],
        result: &mut Vec<Option<Rect>>,
    ) {
        if widget
            .callback_binding()
            .is_some_and(|b| b.same_component(&target.binding))
        {
            // Nested foreign owners cannot contribute a same-named control.
            // Their independent geometry must be observed through their own alias.
            fn foreign_owner(
                widget: &dyn crate::widget::Widget,
                owner: &crate::bound_dispatch::CallbackBinding,
            ) -> bool {
                if widget
                    .callback_binding()
                    .is_some_and(|b| !b.same_component(owner))
                {
                    return true;
                }
                let mut foreign = false;
                widget.for_each_child(&mut |child| {
                    foreign |= foreign_owner(child, owner);
                });
                foreign
            }
            if foreign_owner(widget, &target.binding) {
                result.push(None);
                return;
            }
            let mut tree = SemanticTree::new();
            tree.refresh_bound(
                widget,
                &self.node_map,
                &self.bindings.semantic_node_names(&self.node_map),
                self.focused,
                resources,
            );
            let bindings = crate::widget::controls::control_action_bindings(widget);
            result.push(
                crate::action_locus::resource_bounds(&tree, &bindings, &target.address.address)
                    .and_then(|(bounds, node)| self.clipped_publication_bounds(bounds, node)),
            );
            return;
        }
        widget.for_each_child(&mut |child| {
            self.visit_publication_locus(child, target, resources, result);
        });
    }

    fn clipped_publication_bounds(&self, bounds: Rect, node: u64) -> Option<Rect> {
        let pointer = *self.node_map.get(&taffy::NodeId::from(node))?;
        fn visit(
            widget: &dyn crate::widget::Widget,
            pointer: *const dyn crate::widget::Widget,
            bounds: Rect,
            matches: &mut Vec<Rect>,
        ) {
            if std::ptr::addr_eq(std::ptr::from_ref(widget), pointer) {
                matches.push(bounds);
                return;
            }
            // Painting clips ordinary children, including ancestors outside
            // the publication owner. Use the retained widget path, not just
            // the semantic path (which omits non-accessible wrappers).
            let bounds = if let Some(clip) = widget.child_clip_rect() {
                if ![clip.x0, clip.y0, clip.x1, clip.y1]
                    .into_iter()
                    .all(f64::is_finite)
                    || clip.width() <= 0.0
                    || clip.height() <= 0.0
                {
                    return;
                }
                bounds.intersect(clip)
            } else {
                bounds
            };
            if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
                return;
            }
            widget.for_each_child(&mut |child| visit(child, pointer, bounds, matches));
        }
        let mut matches = Vec::new();
        visit(self.root.as_ref(), pointer, bounds, &mut matches);
        (matches.len() == 1).then(|| matches[0])
    }

    fn action_geometry_visible(&self) -> bool {
        // A system surface hiding the app cannot expose its background locus.
        !(self.caller_palette.palette.is_some()
            || self
                .history
                .as_ref()
                .is_some_and(|history| !history.live_visible())
            || self
                .action_palette
                .as_deref()
                .is_some_and(|palette| palette.is_presented()))
    }

    pub(super) fn retained_action_geometry(
        &self,
    ) -> Option<(SemanticTree, Vec<crate::widget::ControlBindingMetadata>)> {
        if !self.action_geometry_visible() {
            return None;
        }
        // What the live window declares outside its components; theirs are
        // the components' own nodes, as the live window's tree holds them.
        let mut resources = Vec::new();
        crate::bound_dispatch::collect_unbound_resources(
            self.root.as_ref(),
            &self.state.doc,
            &mut resources,
        );
        Some((
            self.semantic_tree_over(&resources),
            crate::widget::controls::control_action_bindings(self.root.as_ref()),
        ))
    }

    /// Every operable control the exported accessibility tree carries now,
    /// in tree order.
    pub fn controls(&self) -> Vec<Control> {
        self.a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.role() != Role::Window)
            .filter_map(|(id, node)| {
                let verbs: Vec<String> = VERBS
                    .iter()
                    .filter(|(action, _)| node.supports_action(*action))
                    .map(|(_, name)| (*name).to_owned())
                    .collect();
                if verbs.is_empty() && !is_pressable(node.role()) {
                    return None;
                }
                Some(Control {
                    id: id.0.to_string(),
                    role: node.role(),
                    name: node.label().map(str::to_owned),
                    value: node.value().map(str::to_owned),
                    bounds: node.bounds().map(|b| Rect::new(b.x0, b.y0, b.x1, b.y1)),
                    focused: false,
                    disabled: node.is_disabled(),
                    verbs,
                })
            })
            .map({
                let focused = self.a11y_focus().map(|(id, _)| id.0.to_string());
                move |mut control| {
                    control.focused = focused.as_deref() == Some(control.id.as_str());
                    control
                }
            })
            .collect()
    }

    /// The control `id` names, if the current frame has it.
    pub fn control(&self, id: &str) -> Option<Control> {
        self.controls().into_iter().find(|control| control.id == id)
    }

    /// Operate the control `id` as a person would.
    ///
    /// A control bound to an app action runs it through the same door as a
    /// click, so an action that needs confirmation is refused here, not run.
    ///
    /// # Errors
    ///
    /// Returns why: no such control in this frame, disabled, it does not
    /// take this input, or the action it triggered was refused or failed.
    pub fn operate_control(&mut self, id: &str, input: &ControlInput) -> Result<(), String> {
        self.invalidate_local();
        let failures = self.control_failures.len();
        self.operate_control_now(id, input)?;
        match self.control_failures.get(failures..) {
            Some(new) if !new.is_empty() => Err(new.join("; ")),
            _ => Ok(()),
        }
    }

    fn operate_control_now(&mut self, id: &str, input: &ControlInput) -> Result<(), String> {
        self.pump_component_wakes();
        let control = self
            .control(id)
            .ok_or_else(|| format!("no control {id:?} in the current frame"))?;
        if control.disabled {
            return Err(format!("control {id:?} is disabled"));
        }
        let node = accesskit::NodeId(
            id.parse()
                .map_err(|_| format!("{id:?} is not a control id"))?,
        );
        let has = |verb: &str| control.verbs.iter().any(|own| own == verb);
        let centre = || {
            control
                .bounds
                .filter(|bounds| bounds.width() > 0.0 && bounds.height() > 0.0)
                .map(|bounds| bounds.center())
                .ok_or_else(|| format!("control {id:?} has no on-screen area"))
        };
        let verb = |name: &str, action: Action| {
            if has(name) {
                Ok(action)
            } else {
                Err(format!("control {id:?} does not answer {name}"))
            }
        };
        // Framework views and canvases can export elements the harness has
        // no dispatch handle for; a person would point at them instead.
        let pointer = centre();
        let routed =
            |harness: &mut Self, action: &Action| match harness.perform_a11y_action(node, action) {
                Err(error) if error.starts_with("no accessibility node") => {
                    if !matches!(
                        action,
                        Action::Activate | Action::ToggleCheck | Action::Focus
                    ) {
                        return Err(format!(
                            "control {id:?} is reachable only by pointer, which cannot {action:?}"
                        ));
                    }
                    let centre = pointer.clone()?;
                    harness.click_at(centre.x, centre.y);
                    harness.relayout();
                    Ok(())
                }
                other => other,
            };
        match input {
            ControlInput::Press if has("activate") => routed(self, &Action::Activate),
            ControlInput::Press => {
                let centre = pointer?;
                self.click_at(centre.x, centre.y);
                self.relayout();
                Ok(())
            }
            ControlInput::Focus => routed(self, &Action::Focus),
            ControlInput::SetValue(value) => {
                verb("set-value", Action::Focus)
                    .map_err(|error| format!("{error}; type into it instead"))?;
                match self.perform_a11y_action(node, &Action::SetValue(value.clone())) {
                    Err(error) if error.starts_with("no accessibility node") => {
                        routed(self, &Action::Focus)?;
                        self.select_all_text();
                        self.commit_text(value);
                        self.host_tick();
                        self.relayout();
                        Ok(())
                    }
                    other => other,
                }
            }
            ControlInput::TypeText(text) => {
                if has("focus") {
                    routed(self, &Action::Focus)?;
                } else {
                    let centre = pointer?;
                    self.click_at(centre.x, centre.y);
                }
                self.commit_text(text);
                self.host_tick();
                self.relayout();
                Ok(())
            }
            ControlInput::Scroll { dx, dy } => {
                let centre = pointer?;
                self.send(&crate::synthetic_input::cursor_moved(
                    centre.x * self.input_scale,
                    centre.y * self.input_scale,
                ));
                self.scroll(*dx, *dy);
                self.relayout();
                Ok(())
            }
            ControlInput::Toggle => routed(self, &verb("activate", Action::ToggleCheck)?),
            ControlInput::Increment => routed(self, &verb("increment", Action::Increment)?),
            ControlInput::Decrement => routed(self, &verb("decrement", Action::Decrement)?),
            ControlInput::Expand => routed(self, &verb("expand", Action::Expand)?),
            ControlInput::Collapse => routed(self, &verb("collapse", Action::Collapse)?),
        }
    }

    /// Select everything in the focused field with the platform's
    /// select-all shortcut.
    fn select_all_text(&mut self) {
        self.set_modifiers(&["command".to_owned()]);
        self.key("a");
        self.set_modifiers(&[]);
    }

    /// Run a projected `ui:` action: the same verb dispatch as
    /// [`Self::operate_control`], addressed the way the resource tree
    /// publishes it. Availability, confirmation and argument binding were
    /// already checked by the caller against the published spec.
    pub(super) fn invoke_control_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let unknown = || tilo_actions::DoorError::Unknown(address.clone());
        let id = address
            .resource
            .strip_prefix(CONTROL_RESOURCE_PREFIX)
            .and_then(|encoded| {
                percent_encoding::percent_decode_str(encoded)
                    .decode_utf8()
                    .ok()
            })
            .ok_or_else(unknown)?
            .into_owned();
        let tree = self.semantic_tree();
        let node = tree.node(&SemanticId(id)).ok_or_else(unknown)?.clone();
        let SemanticActionKind::Verb(verb) =
            &node.action(&address.action).ok_or_else(unknown)?.kind
        else {
            return Err(unknown());
        };
        let verb = match verb {
            Action::SetValue(_) => Action::SetValue(
                tilo_actions::sole_text_argument(arguments)
                    .ok_or_else(|| tilo_actions::DoorError::MissingArgument(address.clone()))?
                    .to_owned(),
            ),
            other => other.clone(),
        };
        self.perform_on_node(&node, &verb)
            .map_err(tilo_actions::DoorError::Failed)?;
        Ok(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    /// Dispatch `verb` to the widget that owns `node`, exactly as the live
    /// `App` routes an accessibility request: a synthetic child through its
    /// owner's semantic target, a widget through the AccessKit path with the
    /// focus and click fallbacks.
    fn perform_on_node(&mut self, node: &SemanticNode, verb: &Action) -> Result<(), String> {
        use crate::accesskit_bridge::A11yTarget;
        let taffy_raw = node
            .widget_taffy_id
            .ok_or_else(|| format!("control {:?} has no widget", node.id.0))?;
        let access = accesskit::NodeId(taffy_raw.saturating_add(1));
        let Some(semantic_target) = node.semantic_target.clone() else {
            return self.perform_a11y_action(access, verb);
        };
        let exported = self.bindings.exported_nodes(&self.node_map);
        let owner = self
            .presentation_roots()
            .iter()
            .find_map(|root| exported.resolve(*root, access))
            .and_then(|target| match target {
                A11yTarget::Widget(id) => Some(id),
                A11yTarget::Root | A11yTarget::Synthetic { .. } => None,
            })
            .ok_or_else(|| format!("control {:?} is not in the current frame", node.id.0))?;
        let handled = self.perform_semantic_target(owner, &semantic_target, verb)?;
        if handled {
            if matches!(verb, Action::Focus) {
                self.set_focused(Some(owner));
            }
            self.host_tick();
            self.relayout();
            return Ok(());
        }
        // A custom-painted widget exposes targets and geometry but keeps its
        // pointer hit-testing as the one input path; the App routes the
        // action there, and so does this.
        match (verb, node.bounds) {
            (Action::Activate | Action::ToggleCheck, Some(bounds)) => {
                let centre = bounds.center();
                self.click_at(centre.x, centre.y);
                self.relayout();
                Ok(())
            }
            (Action::Focus, _) => {
                self.set_focused(Some(owner));
                self.relayout();
                Ok(())
            }
            _ => Err(format!("control {:?} does not support {verb:?}", node.id.0)),
        }
    }
}

/// Whether a person could click this even without an accessibility verb.
fn is_pressable(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::CheckBox
            | Role::RadioButton
            | Role::Switch
            | Role::Tab
            | Role::MenuItem
            | Role::Link
            | Role::ListItem
            | Role::TreeItem
    )
}

/// Whether `spec` is a projected control's action rather than one an app
/// declared.
pub(super) fn is_control_operation(spec: &tilo_actions::ActionSpec) -> bool {
    spec.operation_type.as_deref() == Some(SEMANTIC_OPERATION_TYPE)
}

#[cfg(test)]
mod locus_tests {
    use super::*;
    use crate::state::State;
    use crate::widget::{BlockContent, Button, Column, Container, ListRow, resource_actions};
    use std::cell::Cell;
    use std::rc::Rc;

    fn fixture() -> (FrameHarness, Rc<Cell<u32>>) {
        let calls = Rc::new(Cell::new(0));
        let mut column = Column::new();
        for id in ["first", "second"] {
            let mut resource = resource_actions::resource(id, "Same label", "edit", "Edit");
            resource.id = tilo_actions::ResourceId::new(id);
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
            let counter = calls.clone();
            column = column.push(resource_actions::immediate(
                Box::new(Button::new(id, "Same label").on_action(binding)),
                resource,
                move |_| {
                    counter.set(counter.get() + 1);
                    Ok(tilo_actions::ActionOutput::default())
                },
            ));
        }
        (
            FrameHarness::new(State::new(), Box::new(column), (400, 240)),
            calls,
        )
    }

    fn interaction(resource: &str) -> tilo_actions::SemanticInteraction {
        tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: tilo_actions::InvocationId::new(),
            sequence: 1,
            actor: tilo_actions::InteractionActor {
                id: "synthetic-test-only".into(),
                kind: tilo_actions::ActorKind::Person,
                for_person: None,
            },
            phase: tilo_actions::InteractionPhase::Applying,
            action: tilo_actions::InteractionAction {
                id: "edit".into(),
                label: "Edit".into(),
                app: None,
                step: None,
                creates: None,
            },
            resource: tilo_actions::ActionResource {
                id: resource.into(),
                kind: "example".into(),
                label: "Same label".into(),
            },
            anchor: tilo_actions::InteractionAnchor::Resource,
            group: None,
        }
    }

    #[test]
    fn agent_marker_projects_real_action_without_reinvoking_and_retires_when_idle() {
        for (viewport, reduced_motion) in [
            ((1600, 900), false),
            ((800, 600), false),
            ((1600, 900), true),
            ((800, 600), true),
        ] {
            let (mut frame, calls) = fixture();
            let mut theme = frame.theme().clone();
            theme.reduce_motion = reduced_motion;
            frame.set_theme(theme);
            frame.resize(viewport).unwrap();
            frame
                .invoke_action(
                    &tilo_actions::ActionAddress::new("first", "edit"),
                    &serde_json::Map::new(),
                    false,
                    None,
                )
                .unwrap();
            assert_eq!(calls.get(), 1, "the actual semantic executor ran once");
            let mut event = interaction("first");
            event.actor.kind = tilo_actions::ActorKind::Agent;
            let now = crate::time::now();
            frame.update_presence(|presence| {
                presence.apply_interaction_at(event.clone(), now).unwrap()
            });
            assert!(frame.poll_frame().unwrap());
            assert_eq!(frame.semantic_locus(&event), Some(frame.targets()["first"]));
            assert_eq!(
                frame.frame_schedule().needs_animation_frame,
                !reduced_motion
            );
            assert_eq!(
                calls.get(),
                1,
                "painting is not a pointer click or an action"
            );

            // Observe already-settled host activity: no widget or input event
            // is needed to keep its expiry deadline alive.
            let mut presence = crate::presence::PresenceCtx::empty();
            presence
                .apply_interaction_at(
                    event.clone(),
                    now.checked_sub(crate::time::Duration::from_secs(2))
                        .unwrap(),
                )
                .unwrap();
            frame.set_presence_context(presence);
            let schedule = frame.frame_schedule();
            assert!(!schedule.needs_animation_frame, "settled dwell must sleep");
            assert!(schedule.next_wake_deadline.is_some_and(|at| at > now));

            // A discrete idle wake must clear an expired frame, including
            // reduced-motion hosts which never have an animation pump.
            let expired = now
                .checked_sub(crate::time::Duration::from_secs(16))
                .unwrap();
            let mut terminal = event.clone();
            terminal.sequence = 2;
            terminal.phase = tilo_actions::InteractionPhase::Committed;
            let mut expired_presence = crate::presence::PresenceCtx::empty();
            expired_presence
                .apply_interaction_at(terminal, expired)
                .unwrap();
            *frame.presence_mut() = expired_presence;
            assert!(
                !frame.frame_schedule().queued_work,
                "no input/redraw notification"
            );
            assert!(
                frame
                    .frame_schedule()
                    .next_wake_deadline
                    .is_some_and(|at| at <= crate::time::now())
            );
            assert!(frame.poll_frame().unwrap());
            assert!(
                frame
                    .presence_mut()
                    .visible_interactions(crate::time::now())
                    .is_empty()
            );
            assert!(frame.frame_schedule().next_wake_deadline.is_none());
            assert!(!frame.frame_schedule().needs_animation_frame);
            assert_eq!(calls.get(), 1);
        }
    }

    #[test]
    fn agent_marker_full_size_render_evidence() {
        let out = crate::headless::agent_marker_shots();
        for (name, theme) in [
            ("dark", crate::theme::Theme::dark()),
            ("light", crate::theme::Theme::light()),
            ("high-contrast", crate::theme::Theme::high_contrast()),
        ] {
            for viewport in [(1600, 900), (800, 600)] {
                let (mut frame, calls) = fixture();
                frame.relayout_themed(viewport, theme.clone()).unwrap();
                let path = out
                    .join(name)
                    .join(format!("{}x{}", viewport.0, viewport.1));
                std::fs::create_dir_all(&path).unwrap();
                frame.render_png(&path.join("before.png"));
                frame
                    .invoke_action(
                        &tilo_actions::ActionAddress::new("first", "edit"),
                        &serde_json::Map::new(),
                        false,
                        None,
                    )
                    .unwrap();
                let mut event = interaction("first");
                event.actor.id = "scribe".into();
                event.actor.kind = tilo_actions::ActorKind::Agent;
                let at = crate::time::now()
                    .checked_sub(crate::time::Duration::from_secs(2))
                    .unwrap();
                frame.update_presence(|presence| {
                    presence.apply_interaction_at(event.clone(), at).unwrap()
                });
                assert!(frame.poll_frame().unwrap());
                frame.render_png(&path.join("after.png"));
                assert_eq!(calls.get(), 1);
                for file in ["before.png", "after.png"] {
                    assert!(std::fs::metadata(path.join(file)).unwrap().len() > 0);
                }
                assert_ne!(
                    std::fs::read(path.join("before.png")).unwrap(),
                    std::fs::read(path.join("after.png")).unwrap(),
                    "the capture must actually observe action presence"
                );
            }
        }
    }

    #[test]
    fn semantic_locus_never_matches_labels_or_unbound_semantic_ids() {
        let (frame, calls) = fixture();
        let mut event = interaction("first");
        assert_eq!(frame.semantic_locus(&event), Some(frame.targets()["first"]));
        event.resource.id = "missing".into();
        assert_eq!(
            frame.semantic_locus(&event),
            None,
            "identical resource label is not identity"
        );
        event.resource.id = "first".into();
        let other = frame.action_ui_routes(&tilo_actions::ActionAddress::new("second", "edit"));
        event.anchor = tilo_actions::InteractionAnchor::SemanticControl {
            id: other[0].path.last().unwrap().as_str().into(),
        };
        assert_eq!(
            frame.semantic_locus(&event),
            None,
            "another resource's node is not this target"
        );
        event.anchor = tilo_actions::InteractionAnchor::Grid {
            sheet: "sheet".into(),
            ranges: vec![tilo_actions::InteractionGridRange {
                start_col: 1,
                start_row: 1,
                end_col: 1,
                end_row: 1,
            }],
        };
        assert_eq!(
            frame.semantic_locus(&event),
            None,
            "unsupported domain geometry remains missing"
        );
        assert_eq!(calls.get(), 0);
    }

    /// The capture a reviewer reads: an agent's item action marked on the row
    /// that shows the item, beside the row it did not act on.
    #[test]
    fn agent_item_marker_render_evidence() {
        let root = Column::new()
            .push(Box::new(
                ListRow::new("a", "First task")
                    .item("issue-a")
                    .checkbox(false, |_| {}),
            ))
            .push(Box::new(
                ListRow::new("b", "Second task")
                    .item("issue-b")
                    .checkbox(false, |_| {}),
            ));
        let mut frame = FrameHarness::new(State::new(), Box::new(root), (480, 240));
        frame.relayout();
        let out = crate::headless::agent_marker_shots();
        std::fs::create_dir_all(&out).unwrap();
        frame.render_png(&out.join("tasks-before.png"));
        let mut event = interaction("so.tilo.tasks/issues");
        event.actor.id = "scribe".into();
        event.actor.kind = tilo_actions::ActorKind::Agent;
        event.action.id = "check-off".into();
        event.action.label = "Check off".into();
        event.anchor = tilo_actions::InteractionAnchor::ListItems {
            ids: vec!["issue-b".into()],
        };
        // The line names the item the step changes, as the app shows it.
        event.resource.label = "Second task".into();
        // A second in: the pointer has travelled to the check box, as the
        // capture shows it at rest on the control rather than mid-glide.
        let at = crate::time::now()
            .checked_sub(crate::time::Duration::from_secs(1))
            .expect("the clock is later than a second ago");
        let mut label = String::new();
        frame.update_presence(|presence| {
            presence
                .names
                .insert(crate::permissions::UserId("scribe".into()), "Ada".into());
            let applied = presence
                .apply_interaction_at(event.clone(), at)
                .expect("a well-formed interaction applies");
            label = presence.interaction_status_label(&event);
            applied
        });
        assert_eq!(label, "Ada: Editing Second task");
        assert!(frame.poll_frame().unwrap());
        frame.render_png(&out.join("tasks-agent-checks-second.png"));
        assert!(
            std::fs::metadata(out.join("tasks-agent-checks-second.png"))
                .unwrap()
                .len()
                > 0
        );
        frame.set_theme(crate::theme::Theme::light());
        frame.render_png(&out.join("tasks-agent-checks-second-light.png"));
    }

    /// An agent acting on a task rests its pointer on the row's check box —
    /// the control a person presses — and its whole mark stays inside the row.
    #[test]
    fn an_agents_marker_rests_on_the_control_the_row_offers() {
        let root = Column::new()
            .push(Box::new(
                ListRow::new("a", "First task")
                    .item("issue-a")
                    .checkbox(false, |_| {}),
            ))
            .push(Box::new(
                ListRow::new("b", "Second task")
                    .item("issue-b")
                    .checkbox(false, |_| {}),
            ));
        let mut frame = FrameHarness::new(State::new(), Box::new(root), (480, 240));
        frame.relayout();
        let mut event = interaction("so.tilo.tasks/issues");
        event.actor.id = "scribe".into();
        event.actor.kind = tilo_actions::ActorKind::Agent;
        event.action.id = "check-off".into();
        event.action.label = "Check off".into();
        event.anchor = tilo_actions::InteractionAnchor::ListItems {
            ids: vec!["issue-b".into()],
        };
        let locus = frame.semantic_locus(&event).expect("the item's row");
        let (tree, bindings) = frame.retained_action_geometry().expect("a laid out frame");
        let check = tree
            .iter()
            .filter(|node| node.role == crate::a11y::Role::Checkbox)
            .filter_map(|node| node.bounds)
            .find(|bounds| bounds.y0 >= locus.y0 && bounds.y1 <= locus.y1)
            .expect("the row offers a check box");
        let at = crate::time::now()
            .checked_sub(crate::time::Duration::from_secs(1))
            .expect("the clock is later than a second ago");
        let mut presence = crate::presence::PresenceCtx::empty();
        presence
            .apply_interaction_at(event.clone(), at)
            .expect("a well-formed interaction applies");
        let theme = crate::theme::Theme::dark();
        let viewport = vello::kurbo::Rect::new(0.0, 0.0, 480.0, 240.0);
        let mut scene = vello::Scene::new();
        let mut text = crate::text_render::TextRenderer::new();
        let markers = crate::action_locus::draw_action_presence(
            &mut crate::paint::PaintCtx::new(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &theme,
            ),
            &mut presence,
            &tree,
            &bindings,
            &[],
            viewport,
            "native-primary",
            crate::time::now(),
        );
        let marker = markers.first().expect("the agent's mark is painted");
        let [x0, y0, x1, y1] = marker.marker;
        let mark = vello::kurbo::Rect::new(x0, y0, x1, y1);
        // The agent's mark is centred on its tip, its nametag beside it.
        let tip = check.center();
        assert!(
            mark.contains(tip) && locus.contains(tip),
            "the pointer rests on the row's check box: {mark:?} vs {check:?} in {locus:?}"
        );
        // One tag, saying who and what, standing where no word is: the row's title stays
        // readable beside the control the mark rests on.
        let tags = presence.caret_tags();
        let [tag] = tags.as_slice() else {
            panic!("the agent wears one tag: {tags:?}");
        };
        let words: Vec<vello::kurbo::Rect> = tree
            .iter()
            .filter(|node| {
                node.name
                    .as_deref()
                    .is_some_and(|name| !name.trim().is_empty())
                    || node
                        .value
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
            })
            .filter_map(|node| node.bounds)
            .collect();
        assert!(
            words
                .iter()
                .all(|word| tag.rect.intersect(*word).is_zero_area()),
            "the tag covers no word: {:?} among {words:?}",
            tag.rect
        );
        assert!(
            viewport.x0 <= x0 && x1 <= viewport.x1 && viewport.y0 <= y0 && y1 <= viewport.y1,
            "the whole mark stays inside the window: {marker:?} vs {viewport:?}"
        );
    }

    /// What an agent acts on is the item it names: the marker lands on the
    /// row showing that item, whichever row the action happens to be bound
    /// to, and an item no row shows has no place on screen.
    #[test]
    fn an_item_anchor_lands_on_the_row_showing_that_item() {
        let root = Column::new()
            .push(Box::new(ListRow::new("a", "First task").item("issue-a")))
            .push(Box::new(ListRow::new("b", "Second task").item("issue-b")));
        let frame = FrameHarness::new(State::new(), Box::new(root), (400, 240));
        let row = |name: &str| {
            frame
                .semantic_tree()
                .find_by_name(name)
                .and_then(|node| node.bounds)
                .expect("a laid out row has bounds")
        };
        let mut event = interaction("so.tilo.tasks/issues");
        event.anchor = tilo_actions::InteractionAnchor::ListItems {
            ids: vec!["issue-b".into()],
        };
        assert_eq!(frame.semantic_locus(&event), Some(row("Second task")));
        assert_ne!(
            frame.semantic_locus(&event),
            Some(row("First task")),
            "the marker never lands on a neighbouring row"
        );
        event.anchor = tilo_actions::InteractionAnchor::ListItems {
            ids: vec!["issue-missing".into()],
        };
        assert_eq!(
            frame.semantic_locus(&event),
            None,
            "an item no row shows is not guessed onto a row"
        );
        event.anchor = tilo_actions::InteractionAnchor::ListItems {
            ids: vec!["issue-a".into(), "issue-b".into()],
        };
        assert_eq!(
            frame.semantic_locus(&event),
            None,
            "two items have no single place"
        );
    }

    #[test]
    fn semantic_locus_refuses_multiple_affordances_for_one_action() {
        let resource = resource_actions::resource("shared", "Same label", "edit", "Edit");
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
        let root = resource_actions::immediate(
            Box::new(
                Column::new()
                    .push(Box::new(
                        Button::new("a", "Same label").on_action(binding.clone()),
                    ))
                    .push(Box::new(Button::new("b", "Same label").on_action(binding))),
            ),
            resource,
            |_| panic!("observation must not execute"),
        );
        let frame = FrameHarness::new(State::new(), root, (400, 240));
        assert_eq!(
            frame
                .action_ui_routes(&tilo_actions::ActionAddress::new("shared:root", "edit"))
                .len(),
            2
        );
        assert_eq!(frame.semantic_locus(&interaction("shared:root")), None);
    }

    #[test]
    fn semantic_locus_refuses_control_id_reuse_across_distinct_resources() {
        let mut root = Column::new();
        for kind in ["one", "two"] {
            let resource = resource_actions::resource(kind, "Same label", "edit", "Edit");
            let owner = resource.id.as_str().to_owned();
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
            // A widget id addresses a container for inspection; only a
            // semantic key such as a landmark makes it an owner, so each
            // resource's surface keeps its own `edit` control.
            root = root.push(resource_actions::immediate(
                Box::new(Container::new().landmark(kind, owner).with_child(Box::new(
                    Column::new().push(Box::new(
                        Button::new("edit", "Same label").on_action(binding),
                    )),
                ))),
                resource,
                |_| panic!("observation must not execute"),
            ));
        }
        let frame = FrameHarness::new(State::new(), Box::new(root), (400, 300));
        let paths = frame.semantic_tree().control_paths("edit");
        assert_eq!(
            paths.len(),
            2,
            "fixture contains distinct owners with the same control id"
        );
        let (tree, bindings) = frame.retained_action_geometry().unwrap();
        assert!(
            crate::action_locus::resource_bounds(
                &tree,
                &bindings,
                &tilo_actions::ActionAddress::new("one:root", "edit"),
            )
            .is_none()
        );
        for target in paths.iter().filter_map(|path| path.last()) {
            let mut event = interaction("one:root");
            event.anchor = tilo_actions::InteractionAnchor::SemanticControl {
                id: target.as_str().into(),
            };
            assert_eq!(frame.semantic_locus(&event), None);
        }
        assert!(
            frame
                .action_ui_routes(&tilo_actions::ActionAddress::new("one:root", "edit"))
                .is_empty()
        );
    }

    #[test]
    fn semantic_locus_is_hidden_when_caller_search_covers_the_app() {
        let (mut frame, _) = fixture();
        assert!(frame.semantic_locus(&interaction("first")).is_some());
        frame.open_caller_search("").unwrap();
        assert!(frame.semantic_locus(&interaction("first")).is_none());
    }

    #[test]
    fn semantic_locus_is_hidden_when_history_replaces_the_live_app() {
        use crate::history_mode::*;
        static DEFINITION: HistoryDefinition = HistoryDefinition {
            app_id: "test.locus",
            scopes: &[HistoryScopeDefinition {
                key: "document",
                title: "Document",
                presentation: HistoryPresentation::Domain {
                    create: crate::history_mode::test_preview,
                },
                content_schema: None,
            }],
        };
        let (mut frame, _) = fixture();
        let registration = HistoryRegistration::new(
            &DEFINITION,
            vec![HistorySourceRegistration::HostDocument {
                scope: "document",
                resource: HistoryResource::new(
                    "synthetic",
                    "room",
                    "test.locus",
                    crate::RecordId::for_tab(71),
                ),
            }],
        )
        .unwrap();
        frame.install_history(registration).unwrap();
        assert!(frame.semantic_locus(&interaction("first")).is_some());
        frame
            .perform_named("history-open", &crate::a11y::Action::Activate)
            .unwrap();
        assert!(!frame.history.as_ref().unwrap().live_visible());
        assert!(frame.semantic_locus(&interaction("first")).is_none());
    }

    /// An agent's action, shown as if it arrived a second ago.
    fn agent_acting(frame: &mut FrameHarness, resource: &str) -> tilo_actions::SemanticInteraction {
        let mut event = interaction(resource);
        event.actor.id = "scribe".into();
        event.actor.kind = tilo_actions::ActorKind::Agent;
        let at = crate::time::now()
            .checked_sub(crate::time::Duration::from_secs(1))
            .expect("the clock is later than a second ago");
        frame.update_presence(|presence| presence.apply_interaction_at(event.clone(), at).is_ok());
        event
    }

    /// Following an agent that edits far down a long document scrolls the
    /// view to its edit, once per action; following someone else moves
    /// nothing.
    #[test]
    fn following_an_agent_scrolls_to_its_edit_further_down() {
        let resource = resource_actions::resource("doc", "Long document", "edit", "Edit");
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
        let content = Column::new()
            .push(Box::new(crate::widget::Spacer::fixed(2000.0)))
            .push(Box::new(
                Button::new("far", "Far paragraph").on_action(binding),
            ));
        let root = crate::widget::Scrollable::new(crate::widget::ScrollAxis::Vertical).with_child(
            resource_actions::immediate(Box::new(content), resource, |_| {
                Ok(tilo_actions::ActionOutput::default())
            }),
        );
        let mut frame = FrameHarness::new(State::new(), Box::new(root), (400, 240));
        frame.relayout();
        let event = agent_acting(&mut frame, "doc:root");
        let view = Rect::new(0.0, 0.0, 400.0, 240.0);
        let edit = frame.semantic_locus(&event).expect("the edit's control");
        assert!(edit.y0 > view.y1, "the edit starts out of view: {edit:?}");
        assert!(!frame.follow_view("someone-else", &mut FollowedView::default()));
        assert_eq!(
            frame.semantic_locus(&event),
            Some(edit),
            "not following moves nothing"
        );
        let mut followed = FollowedView::default();
        assert!(frame.follow_view("scribe", &mut followed));
        let edit = frame.semantic_locus(&event).expect("the edit's control");
        assert!(
            view.contains(edit.origin()) && edit.y1 <= view.y1,
            "in view: {edit:?}"
        );
        assert!(
            !frame.follow_view("scribe", &mut followed),
            "each action is gone to once"
        );
    }

    /// An agent acting in a sheet no view here shows opens that sheet's own
    /// view, once.
    #[test]
    fn following_an_agent_into_another_sheet_opens_it() {
        let mut resource = resource_actions::resource("sheet", "Sheet 2", "open", "Open");
        resource.views.push(tilo_actions::ResourceViewSpec::new(
            "sheet", "Sheet 2", "open",
        ));
        let opened = Rc::new(Cell::new(0));
        let count = opened.clone();
        let root = resource_actions::immediate(Box::new(Column::new()), resource, move |_| {
            count.set(count.get() + 1);
            Ok(tilo_actions::ActionOutput::default())
        });
        let mut frame = FrameHarness::new(State::new(), root, (400, 240));
        frame.relayout();
        agent_acting(&mut frame, "sheet:root");
        let mut followed = FollowedView::default();
        assert!(!frame.follow_view("someone-else", &mut FollowedView::default()));
        assert_eq!(opened.get(), 0, "not following switches nothing");
        assert!(frame.follow_view("scribe", &mut followed));
        assert_eq!(opened.get(), 1, "the sheet's view opened");
        assert!(!frame.follow_view("scribe", &mut followed));
        assert_eq!(opened.get(), 1, "opened once");
    }

    /// A note long enough to read down: forty paragraphs, the last ones far
    /// below a short window.
    fn long_note() -> crate::widget::BlockEditor {
        let mut blocks = vec![BlockContent::paragraph("p-0", "A note about the move.")];
        for index in 1..40 {
            blocks.push(BlockContent::paragraph(
                format!("p-{index}"),
                format!("Paragraph {index} of the note."),
            ));
        }
        let mut editor = crate::widget::BlockEditor::new();
        editor.present_blocks(&blocks);
        editor
    }

    /// An agent editing text further down a long note is there for every
    /// viewer — its cursor rests on the words it wrote, not on whatever
    /// control the action is bound to — and a follower's view scrolls to
    /// them, once per action. A non-follower's view stays where they left it.
    #[test]
    fn following_an_agents_edit_in_text_scrolls_to_the_range_it_wrote() {
        let root = crate::widget::Scrollable::new(crate::widget::ScrollAxis::Vertical)
            .with_child(Box::new(long_note()));
        let mut frame = FrameHarness::new(State::new(), Box::new(root), (400, 240));
        frame.relayout();
        let event = agent_acting(&mut frame, "notes.document:root");
        // Where the agent says it is: the end of the paragraph it wrote.
        let written = 13;
        let block = crate::widget::block_editor::block_presence_key("p-33");
        frame.update_presence(|presence| {
            presence.set_remote_caret("scribe".into(), block.clone(), written);
            true
        });
        let viewport = Rect::new(0.0, 0.0, 400.0, 240.0);
        let edit = frame
            .semantic_locus(&event)
            .expect("the agent's place in the note");
        assert!(
            edit.y0 > viewport.y1,
            "the edited paragraph starts out of view: {edit:?}"
        );
        assert!(
            edit.height() < 40.0,
            "the cursor is on the line, not the whole note: {edit:?}"
        );
        assert!(!frame.follow_view("someone-else", &mut FollowedView::default()));
        assert_eq!(
            frame.semantic_locus(&event),
            Some(edit),
            "not following moves nothing"
        );
        let mut followed = FollowedView::default();
        assert!(frame.follow_view("scribe", &mut followed));
        let shown = frame
            .semantic_locus(&event)
            .expect("the agent's place in view");
        assert!(
            viewport.contains(shown.origin()) && shown.y1 <= viewport.y1,
            "the words it wrote are in view: {shown:?}"
        );
        assert!(
            !frame.follow_view("scribe", &mut followed),
            "each action is gone to once"
        );
    }

    /// A follower whose view shows an agent's caret in text settles: frame
    /// after frame of it goes to the words once, and says nothing of the
    /// follower's own presence.
    #[test]
    fn following_an_agents_caret_in_text_settles() {
        let root = crate::widget::Scrollable::new(crate::widget::ScrollAxis::Vertical)
            .with_child(Box::new(long_note()));
        let mut frame = FrameHarness::new(State::new(), Box::new(root), (400, 240));
        frame.relayout();
        let event = agent_acting(&mut frame, "notes.document:root");
        frame.update_presence(|presence| {
            let block = crate::widget::block_editor::block_presence_key("p-33");
            presence.set_remote_caret("scribe".into(), block, 13);
            true
        });
        let said = crate::presence::capability::subtree_presence(frame.root.as_ref());
        let mut followed = FollowedView::default();
        let mut moved = 0;
        for _ in 0..60 {
            moved += usize::from(frame.follow_view("scribe", &mut followed));
            frame.relayout();
        }
        assert_eq!(moved, 1, "the view goes to the words once");
        assert!(frame.semantic_locus(&event).is_some());
        assert_eq!(frame.focus_presence(), None, "the follower says nothing");
        assert_eq!(
            crate::presence::capability::subtree_presence(frame.root.as_ref()),
            said,
            "showing someone's caret says nothing of the follower's own"
        );
    }

    #[test]
    fn semantic_locus_refuses_a_control_whose_resource_is_no_longer_published() {
        let resource = resource_actions::resource("stale", "Same label", "edit", "Edit");
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
        let frame = FrameHarness::new(
            State::new(),
            Box::new(Button::new("stale-control", "Same label").on_action(binding)),
            (400, 240),
        );
        assert_eq!(frame.semantic_locus(&interaction("stale:root")), None);
    }

    #[test]
    fn semantic_locus_refuses_a_frame_local_projected_resource_identity() {
        let (mut frame, _) = fixture();
        let retained = std::mem::replace(&mut frame.root, Box::new(Column::new()));
        frame.root = Box::new(
            crate::widget::Scrollable::new(crate::widget::ScrollAxis::Vertical)
                .with_child(retained),
        );
        frame.invalidate_layout_tree();
        frame.relayout();
        let tree = frame.semantic_tree();
        let route = frame.action_ui_routes(&tilo_actions::ActionAddress::new("first", "edit"));
        let target = route[0].path.last().unwrap();
        assert!(
            !tree.node(target).unwrap().durable_identity,
            "fixture sits below an unkeyed parent"
        );
        let encoded = percent_encoding::utf8_percent_encode(
            target.as_str(),
            percent_encoding::NON_ALPHANUMERIC,
        );
        let mut event = interaction(&format!("ui:{encoded}"));
        event.action.id = "activate".into();
        assert_eq!(frame.semantic_locus(&event), None);
    }

    #[test]
    fn painted_marker_export_tracks_live_geometry_and_retires_with_roster() {
        let (frame, calls) = fixture();
        let (tree, bindings) = frame.retained_action_geometry().unwrap();
        let mut presence = crate::presence::PresenceCtx::empty();
        let mut event = interaction("first");
        event.actor.id = "opaque-software".into();
        event.actor.kind = tilo_actions::ActorKind::Agent;
        let at = crate::time::now();
        presence.apply_interaction_at(event.clone(), at).unwrap();
        let theme = crate::theme::Theme::dark();
        let viewport = vello::kurbo::Rect::new(0.0, 0.0, 400.0, 240.0);
        let mut scene = vello::Scene::new();
        let mut text = crate::text_render::TextRenderer::new();
        let mut paint = |presence: &mut crate::presence::PresenceCtx, now| {
            scene.reset();
            let markers = crate::action_locus::draw_action_presence(
                &mut crate::paint::PaintCtx::new(
                    &mut scene,
                    vello::kurbo::Affine::IDENTITY,
                    &mut text,
                    &theme,
                ),
                presence,
                &tree,
                &bindings,
                &[],
                viewport,
                "native-primary",
                now,
            );
            assert!(
                !scene.encoding().draw_tags.is_empty(),
                "the production painter emits pixels"
            );
            markers
        };
        let first = paint(&mut presence, at);
        let arrived = paint(&mut presence, at + crate::time::Duration::from_millis(500));
        assert_eq!(first.len(), 1);
        assert_eq!(arrived.len(), 1);
        assert_eq!(first[0].interaction, event);
        assert_eq!(
            first[0].locus,
            crate::floating_surface::protocol_rect_array(frame.targets()["first"])
        );
        assert_ne!(
            first[0].marker, arrived[0].marker,
            "published bounds follow the animated paint, not the target"
        );
        presence.unregister_peer(&event.actor.id);
        let retired = crate::action_locus::draw_action_presence(
            &mut crate::paint::PaintCtx::new(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &theme,
            ),
            &mut presence,
            &tree,
            &bindings,
            &[],
            viewport,
            "native-primary",
            at,
        );
        assert!(retired.is_empty());
        assert_eq!(
            calls.get(),
            0,
            "publication never executes the underlying action"
        );
    }

    #[test]
    fn action_locus_uses_exact_binding_without_input_or_state_change() {
        let (mut frame, calls) = fixture();
        let before = frame.state.doc.get_heads();
        let focus = frame.focused;
        let layouts = frame.relayout_frames();
        let first = tilo_actions::ActionAddress::new("first", "edit");
        let second = tilo_actions::ActionAddress::new("second", "edit");
        let a = frame.action_ui_routes(&first);
        let b = frame.action_ui_routes(&second);
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_ne!(a[0].bounds, b[0].bounds);
        assert_eq!(a[0].bounds, Some(frame.targets()["first"]));
        assert_eq!(b[0].bounds, Some(frame.targets()["second"]));
        assert!(
            frame
                .action_ui_routes(&tilo_actions::ActionAddress::new("missing", "edit"))
                .is_empty()
        );
        assert!(
            frame
                .action_ui_routes(&tilo_actions::ActionAddress::new("first", "missing"))
                .is_empty()
        );
        assert_eq!(frame.focused, focus);
        assert_eq!(frame.relayout_frames(), layouts);
        assert_eq!(frame.state.doc.get_heads(), before);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn exact_resource_bounds_matches_unique_ranked_route_without_input() {
        let (mut frame, calls) = fixture();
        let before = frame.state.doc.get_heads();
        let focus = frame.focused;
        let layouts = frame.relayout_frames();
        let (tree, bindings) = frame.retained_action_geometry().unwrap();
        for id in ["first", "second"] {
            let address = tilo_actions::ActionAddress::new(id, "edit");
            let ranked = frame.action_ui_routes(&address);
            assert_eq!(ranked.len(), 1);
            let exact = crate::action_locus::resource_bounds(&tree, &bindings, &address).unwrap();
            assert_eq!(Some(exact.0), ranked[0].bounds);
            assert_eq!(exact.0, frame.targets()[id]);
            let node = tree.node(ranked[0].path.last().unwrap()).unwrap();
            assert_eq!(Some(exact.1), node.widget_taffy_id);
        }
        for address in [
            tilo_actions::ActionAddress::new("missing", "edit"),
            tilo_actions::ActionAddress::new("first", "missing"),
        ] {
            assert!(crate::action_locus::resource_bounds(&tree, &bindings, &address).is_none());
        }
        assert_eq!(frame.focused, focus);
        assert_eq!(frame.relayout_frames(), layouts);
        assert_eq!(frame.state.doc.get_heads(), before);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn exact_resource_bounds_preserves_ambiguity_and_ranked_order() {
        let resource = resource_actions::resource("shared", "Same label", "edit", "Edit");
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
        let root = resource_actions::immediate(
            Box::new(
                Column::new()
                    .push(Box::new(
                        Button::new("b", "Same label").on_action(binding.clone()),
                    ))
                    .push(Box::new(Button::new("a", "Same label").on_action(binding))),
            ),
            resource,
            |_| panic!("observation must not execute"),
        );
        let frame = FrameHarness::new(State::new(), root, (400, 240));
        let address = tilo_actions::ActionAddress::new("shared:root", "edit");
        let ranked = frame.action_ui_routes(&address);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].control_id, "a");
        assert_eq!(ranked[1].control_id, "b");
        let (tree, mut bindings) = frame.retained_action_geometry().unwrap();
        for _ in 0..2 {
            assert_eq!(
                crate::action_locus::action_ui_routes(&tree, &bindings, &address),
                ranked
            );
            assert!(crate::action_locus::resource_bounds(&tree, &bindings, &address).is_none());
            bindings.reverse();
        }
    }

    #[test]
    fn action_locus_reads_current_layout_and_does_not_reuse_replaced_controls() {
        let (mut frame, _) = fixture();
        let address = tilo_actions::ActionAddress::new("first", "edit");
        let before = frame.action_ui_routes(&address)[0].bounds;
        frame.resize((700, 240)).unwrap();
        let after = frame.action_ui_routes(&address)[0].bounds;
        assert_eq!(after, Some(frame.targets()["first"]));
        let retained = std::mem::replace(&mut frame.root, Box::new(Column::new()));
        frame.root = Box::new(Column::new().push(retained));
        frame.invalidate_layout_tree();
        frame.relayout();
        let moved = frame.action_ui_routes(&address)[0].bounds;
        assert_eq!(moved, Some(frame.targets()["first"]));
        assert_ne!(before, moved);
        frame.root = Box::new(Button::new("replacement", "Same label"));
        frame.invalidate_layout_tree();
        frame.relayout();
        assert!(frame.action_ui_routes(&address).is_empty());
    }
}
