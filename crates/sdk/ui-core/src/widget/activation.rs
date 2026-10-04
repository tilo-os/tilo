//! Pointer, keyboard and accessibility activation shared by the content
//! composites that are themselves one pressable target (a list row, a media
//! card, an interactive container).
//!
//! The owner keeps an [`Activation`] and forwards pointer events to it; the
//! activation tracks hover and press against the owner's rect, fires the
//! bound action or callback on a completed click and on an accessibility
//! `Activate`, and answers the owner's [`ActionControls`] inventory.
//!
//! [`ActionControls`]: crate::widget::controls::ActionControls

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::widget::{ActionEmitter, ControlBindingKind, ControlBindingMetadata};

/// Hover, press and focus of one pressable surface. Faces take this so an
/// immediate-mode caller and the retained widget paint the same states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interaction {
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
    pub disabled: bool,
}

/// The keyboard focus ring around a pressable surface of `radius`: your
/// colour, 2 px, just outside the surface so it never covers its content.
/// Composites that own their focus (rows, cards) paint it themselves; a
/// painted surface with its own hit-testing calls this.
pub fn paint_focus_ring(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    theme: &crate::theme::Theme,
) {
    scene.stroke(
        &Stroke::new(2.0),
        transform,
        theme.border_focus,
        None,
        &RoundedRect::from_rect(rect.inflate(1.0, 1.0), radius + 1.0),
    );
}

/// The operation type every closure-bound control publishes (`on_effect`,
/// `on_view`): its payload is the control's id.
pub(crate) const NATIVE_INVOKE: &str = "tilo.ui.control.invoke";

/// A zero-argument effect or view action a control publishes for its own
/// closure, so a closure-bound control is still discoverable and
/// agent-driveable like [`Button::on_effect`](crate::widget::Button::on_effect).
pub(crate) struct NativeControl {
    resource: tilo_actions::Resource,
    handler: Box<dyn FnMut()>,
}

impl NativeControl {
    /// Publish `handler` as the `invoke` action of control `id`, returning the
    /// binding its pointer, keyboard and accessibility activation emit.
    #[expect(
        clippy::expect_used,
        reason = "String serialization and lookup of the just-created action are infallible"
    )]
    pub(crate) fn new(
        id: &str,
        label: &str,
        kind: tilo_actions::OperationKind,
        handler: Box<dyn FnMut()>,
    ) -> (Self, tilo_actions::ActionBinding<()>) {
        let mut spec =
            tilo_actions::ActionSpec::operation("invoke", label, kind, NATIVE_INVOKE, &id)
                .expect("a control id is always JSON serializable");
        spec.contract.discovery = tilo_actions::DiscoveryScope::Launchable;
        let resource =
            tilo_actions::Resource::new(format!("tilo.control:{id}"), "tilo.control", label)
                .with_action(spec);
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "invoke")
            .expect("the synthesized invoke action is nullary");
        (Self { resource, handler }, binding)
    }

    pub(crate) fn resource(&self) -> tilo_actions::Resource {
        self.resource.clone()
    }

    /// Run the handler for an `invoke` addressed to control `id`.
    pub(crate) fn start(
        &mut self,
        id: &str,
        op: &crate::widget::ResourceOperation<'_>,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        if request.kind == tilo_actions::OperationKind::Document || request.type_id != NATIVE_INVOKE
        {
            return Ok(None);
        }
        let control: String =
            serde_json::from_slice(request.payload).map_err(|error| error.to_string())?;
        if control != id {
            return Ok(None);
        }
        (self.handler)();
        Ok(Some(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

#[derive(Default)]
pub(crate) struct Activation {
    id: String,
    action: Option<ActionEmitter<()>>,
    on_activate: Option<Box<dyn FnMut()>>,
    native: Option<NativeControl>,
    pub(crate) state: Interaction,
    cursor: Point,
}

impl Activation {
    pub(crate) fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn set_callback(&mut self, f: Box<dyn FnMut()>) {
        self.on_activate = Some(f);
        self.action = None;
        self.native = None;
    }

    pub(crate) fn set_action(&mut self, binding: tilo_actions::ActionBinding<()>) {
        self.action = Some(ActionEmitter::new(binding));
        self.on_activate = None;
        self.native = None;
    }

    /// Publish `handler` as this surface's own effect or view action and
    /// route every activation through it.
    pub(crate) fn set_native(
        &mut self,
        kind: tilo_actions::OperationKind,
        label: &str,
        handler: Box<dyn FnMut()>,
    ) {
        let (native, binding) = NativeControl::new(&self.id, label, kind, handler);
        self.set_action(binding);
        self.native = Some(native);
    }

    /// The resource a closure-bound surface publishes, if any.
    pub(crate) fn native_resources(&self) -> Vec<tilo_actions::Resource> {
        self.native
            .as_ref()
            .map(NativeControl::resource)
            .into_iter()
            .collect()
    }

    /// The operation types this surface handles itself.
    pub(crate) fn native_handlers(&self) -> std::collections::HashSet<String> {
        self.native
            .as_ref()
            .map(|_| std::iter::once(NATIVE_INVOKE.to_owned()).collect())
            .unwrap_or_default()
    }

    pub(crate) fn start_native(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        match &mut self.native {
            Some(native) => native.start(&self.id, op),
            None => Ok(None),
        }
    }

    /// Whether anything happens when this surface is pressed.
    pub(crate) fn is_bound(&self) -> bool {
        self.action.is_some() || self.on_activate.is_some()
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.is_bound()
            && !self.state.disabled
            && self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }

    pub(crate) fn cursor(&self) -> Point {
        self.cursor
    }

    /// Fire the bound action or callback.
    pub(crate) fn activate(&mut self) -> bool {
        if !self.is_enabled() {
            return false;
        }
        if let Some(action) = &mut self.action {
            if let Err(error) = action.emit(()) {
                tracing::warn!(%error, control = %self.id, "action-bound surface refused its arguments");
            }
            return true;
        }
        if let Some(callback) = &mut self.on_activate {
            callback();
            return true;
        }
        false
    }

    /// Track hover and press against `rect`; a release over the surface that
    /// was pressed on it activates. `exclude` lists inner targets (a row's
    /// hover actions) that own their own clicks. Returns whether the visible
    /// state changed.
    pub(crate) fn handle_pointer(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
        rect: Rect,
        exclude: &[Rect],
    ) -> bool {
        let before = self.state;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.state.hovered = rect.contains(self.cursor);
            }
            WindowEvent::CursorLeft { .. } => {
                self.state.hovered = false;
                self.state.pressed = false;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } if self.is_enabled() => {
                let inside = rect.contains(self.cursor)
                    && !exclude.iter().any(|inner| inner.contains(self.cursor));
                match state {
                    ElementState::Pressed if inside => {
                        self.state.pressed = true;
                        *ctx.focus_target = Some(ctx.node_id);
                        *ctx.handled = true;
                    }
                    ElementState::Released if self.state.pressed => {
                        self.state.pressed = false;
                        *ctx.handled = true;
                        if inside {
                            self.activate();
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        before != self.state
    }

    pub(crate) fn install_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(action) = &mut self.action {
            let _ = action.install_custody(domain, attachment);
        }
    }

    pub(crate) fn take_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action.as_mut().and_then(ActionEmitter::take)
    }

    pub(crate) fn bindings(&self) -> Vec<ControlBindingMetadata> {
        if let Some(action) = &self.action {
            return vec![action.metadata(self.id.clone())];
        }
        if self.on_activate.is_some() {
            return vec![ControlBindingMetadata {
                control_id: self.id.clone(),
                kind: ControlBindingKind::LegacyCallback,
            }];
        }
        Vec::new()
    }

    /// Returns whether availability changed.
    pub(crate) fn refresh_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) -> bool {
        let Some(action) = &mut self.action else {
            return false;
        };
        let before = action.is_available();
        action.refresh_availability(allowed);
        before != action.is_available()
    }
}

/// Implement [`ActionControls`](crate::widget::controls::ActionControls) for
/// a widget whose own press is an [`Activation`] at `self.$field` and whose
/// children (if any) are reached through the default walks.
macro_rules! activation_controls {
    ($ty:ty, $($field:tt).+) => {
        impl crate::widget::controls::ActionControls for $ty {
            fn install_control_custody(
                &mut self,
                domain: &crate::ControlDomain,
                attachment: crate::control_custody::CustodyAttachment,
            ) {
                self.$($field).+.install_custody(domain, attachment);
                for child in crate::widget::Widget::children_mut(self) {
                    crate::widget::controls::install_control_custody(child, domain, attachment);
                }
            }
            fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
                self.$($field).+.take_invocation().or_else(|| {
                    crate::widget::Widget::children_mut(self)
                        .into_iter()
                        .find_map(crate::widget::controls::take_control_invocation)
                })
            }
            fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
                let mut bindings = self.$($field).+.bindings();
                for child in crate::widget::Widget::children(self) {
                    bindings.extend(crate::widget::controls::control_action_bindings(child));
                }
                bindings
            }
            fn refresh_control_action_availability(
                &mut self,
                allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
            ) {
                self.$($field).+.refresh_availability(allowed);
                for child in crate::widget::Widget::children_mut(self) {
                    crate::widget::controls::refresh_control_action_availability(child, allowed);
                }
            }
        }
    };
}
pub(crate) use activation_controls;
