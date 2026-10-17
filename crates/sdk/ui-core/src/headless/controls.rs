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
use accesskit::{Action as AkAction, Role};

/// Resource id prefix of a projected control (`ui:<percent-encoded node>`).
pub const CONTROL_RESOURCE_PREFIX: &str = "ui:";

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
    pub(super) fn semantic_tree_over(&self, resources: &[tilo_actions::Resource]) -> SemanticTree {
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
    pub fn semantic_locus(&self, interaction: &tilo_actions::SemanticInteraction) -> Option<Rect> {
        let (tree, bindings) = self.retained_action_geometry()?;
        crate::action_locus::interaction_bounds(&tree, &bindings, interaction)
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
        resources: &[tilo_actions::Resource],
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
        !(self.system_search_presentation().is_some()
            || self.caller_palette.palette.is_some()
            || self
                .history
                .as_ref()
                .is_some_and(|history| !history.live_visible())
            || self
                .history_layout
                .is_some_and(|layout| !layout.app_visible)
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
        let mut resources = Vec::new();
        crate::widget::collect_widget_resources(
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
        let command = match self.platform_conventions().platform() {
            crate::platform::Platform::MacOs => "logo",
            _ => "ctrl",
        };
        self.set_modifiers(&[command.to_owned()]);
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
        let key_path = semantic_target
            .strip_prefix("tilo.key-action:")
            .and_then(|target| target.rsplit_once(':'))
            .map(|(_, path)| {
                path.split('/')
                    .map(str::parse::<u64>)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(|error| format!("invalid keyed semantic action: {error}"))?;
        let handled = self
            .dispatch_semantic_node(owner, verb, |widget, doc, root| {
                if let Some(key_path) = &key_path {
                    widget.perform_action(
                        verb,
                        key_path,
                        &mut crate::widget::Edits::lend(doc),
                        root,
                    )
                } else {
                    widget.perform_semantic_action(
                        &semantic_target,
                        verb,
                        &mut crate::widget::Edits::lend(doc),
                        root,
                    )
                }
            })
            .map_err(|error| format!("component dispatch refused: {error:?}"))?;
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
    use crate::widget::{Button, Column, resource_actions};
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
        let out = crate::env::var_os("TILO_UI_AGENT_MARKER_SHOTS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../target/agent-marker")
            });
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
        event.anchor = tilo_actions::InteractionAnchor::ListItems {
            ids: vec!["first".into()],
        };
        assert_eq!(
            frame.semantic_locus(&event),
            None,
            "unsupported domain geometry remains missing"
        );
        assert_eq!(calls.get(), 0);
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
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
            root = root.push(resource_actions::immediate(
                Box::new(Column::new().with_id(kind).push(Box::new(
                    Button::new("edit", "Same label").on_action(binding),
                ))),
                resource,
                |_| panic!("observation must not execute"),
            ));
        }
        let frame = FrameHarness::new(State::new(), Box::new(root), (400, 300));
        let paths = frame.semantic_tree().control_paths("edit");
        assert!(
            paths.len() >= 2,
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
        // Complete empty certified rosters remove their participants through
        // this same PresenceCtx retirement path.
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
