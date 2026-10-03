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

use super::{FrameHarness, Rect, WindowEvent};
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

    /// Where the control a projected `ui:` resource names is on the surface.
    pub fn projected_control_bounds(&self, resource: &str) -> Option<Rect> {
        let id = resource
            .strip_prefix(CONTROL_RESOURCE_PREFIX)
            .and_then(|encoded| {
                percent_encoding::percent_decode_str(encoded)
                    .decode_utf8()
                    .ok()
            })?;
        self.semantic_tree()
            .node(&SemanticId(id.into_owned()))?
            .bounds
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
                self.send(&WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(
                        centre.x * self.input_scale,
                        centre.y * self.input_scale,
                    ),
                });
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
                arguments
                    .get(tilo_actions::PROMPTED_PARAM)
                    .and_then(serde_json::Value::as_str)
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
                    widget.perform_action_at(verb, key_path, doc, root)
                } else {
                    widget.perform_semantic_action(&semantic_target, verb, doc, root)
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
