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

use crate::pointer_replay::{HeldPress, PressAt, Release};
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
    /// What the surface shows. `state.pressed` mirrors `held`, which is
    /// the press itself: [`Self::press`], [`Self::release`] and
    /// [`Self::cancel_press`] keep the two in step.
    pub(crate) state: Interaction,
    held: HeldPress,
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
                self.cancel_press();
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
                        self.press(rect);
                        *ctx.focus_target = Some(ctx.node_id);
                        *ctx.handled = true;
                    }
                    ElementState::Released => {
                        let release = self.release(rect, inside);
                        *ctx.handled |= release.ended();
                        if release == Release::Click(()) {
                            self.activate();
                        }
                    }
                    ElementState::Pressed => {}
                }
            }
            _ => {}
        }
        before != self.state
    }

    /// A press landed on the surface at `rect`.
    pub(crate) fn press(&mut self, rect: Rect) {
        self.held.press(PressAt::one(&self.id, rect));
        self.state.pressed = true;
    }

    /// A left-button release, `over` the surface at `rect` or not: the
    /// press this surface took, or the one a surface it replaced took at
    /// this place, ends. Does not activate.
    pub(crate) fn release(&mut self, rect: Rect, over: bool) -> Release {
        let release = self
            .held
            .release(over.then_some(PressAt::one(&self.id, rect)));
        self.state.pressed = false;
        release
    }

    /// Forget the press without a click (the pointer left, a control
    /// inside took the gesture).
    pub(crate) fn cancel_press(&mut self) {
        self.held.cancel();
        self.state.pressed = false;
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

/// One pressable target a painted host registered for this frame.
#[derive(Clone, Debug)]
struct PaintedTarget {
    id: String,
    rect: Rect,
    label: String,
    disabled: bool,
}

/// The pressable targets of a widget that paints its own content (a
/// calculator's keys, a map's markers, a timeline's clips): the same hover,
/// press, focus and activation the retained controls have, for rects the
/// host lays out itself.
///
/// Each frame the host registers every target it paints, in reading order
/// ([`PaintedTargets::begin_frame`], then [`PaintedTargets::target`]),
/// forwards its events ([`PaintedTargets::handle_event`]), paints each
/// target with [`PaintedTargets::interaction`] (and the focus ring with
/// [`PaintedTargets::paint_focus`]), and acts on
/// [`PaintedTargets::take_activated`]. Its accessibility node lists the
/// targets as buttons ([`PaintedTargets::accessibility_children`]) and
/// routes their activation back ([`PaintedTargets::perform_action`],
/// [`PaintedTargets::perform_semantic_action`]).
/// Tab moves between the targets while the host holds keyboard focus
/// ([`PaintedTargets::wants_tab_input`]).
#[derive(Debug, Default)]
pub struct PaintedTargets {
    targets: Vec<PaintedTarget>,
    hovered: Option<String>,
    pressed: Option<String>,
    /// `pressed`, as a host rebuilt in this one's place sees it.
    carried: HeldPress,
    armed_rect: Option<Rect>,
    cancelled_release_pending: bool,
    focused: Option<String>,
    /// Whether the host holds keyboard focus, so the focused target shows
    /// its ring and takes keys.
    host_focused: bool,
    shift: bool,
    cursor: Point,
    activated: Vec<String>,
}

impl PaintedTargets {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget last frame's targets; register this frame's after it. Hover,
    /// press and focus stay with the ids that keep their target.
    pub fn begin_frame(&mut self) {
        self.targets.clear();
    }

    /// Register a target `id` at `rect`, announced as `label`. Later
    /// targets are on top: a point inside two goes to the later one.
    pub fn target(&mut self, id: impl Into<String>, rect: Rect, label: impl Into<String>) {
        let id = id.into();
        if self.pressed.as_deref() == Some(id.as_str()) && self.armed_rect != Some(rect) {
            self.cancel_pointer_press();
        }
        self.targets.push(PaintedTarget {
            id,
            rect,
            label: label.into(),
            disabled: false,
        });
    }

    /// [`Self::target`] that shows but does nothing (a key that does not
    /// apply now).
    pub fn disabled_target(&mut self, id: impl Into<String>, rect: Rect, label: impl Into<String>) {
        let id = id.into();
        if self.pressed.as_deref() == Some(id.as_str()) {
            self.cancel_pointer_press();
        }
        self.targets.push(PaintedTarget {
            id,
            rect,
            label: label.into(),
            disabled: true,
        });
    }

    /// How to paint target `id` now.
    #[must_use]
    pub fn interaction(&self, id: &str) -> Interaction {
        let disabled = self
            .targets
            .iter()
            .any(|target| target.id == id && target.disabled);
        Interaction {
            hovered: !disabled && self.hovered.as_deref() == Some(id),
            pressed: !disabled && self.pressed.as_deref() == Some(id),
            focused: self.host_focused && self.focused.as_deref() == Some(id),
            disabled,
        }
    }

    /// The target under `point`, topmost first.
    #[must_use]
    pub fn hit(&self, point: Point) -> Option<&str> {
        self.targets
            .iter()
            .rev()
            .find(|target| target.rect.contains(point))
            .map(|target| target.id.as_str())
    }

    /// The id of the target that holds keyboard focus.
    #[must_use]
    pub fn focused(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    /// Move keyboard focus to target `id`.
    pub fn focus(&mut self, id: &str) {
        if self.targets.iter().any(|target| target.id == id) {
            self.focused = Some(id.to_string());
        }
    }

    /// Follow the host's own keyboard focus (`Widget::set_focus`): the
    /// first target takes it when the host gains it with none focused.
    pub fn set_host_focus(&mut self, focused: bool) {
        if self.host_focused && !focused {
            self.cancel_pointer_press();
        }
        self.host_focused = focused;
        if focused && self.focused.is_none() {
            let first = self.enabled().next().map(|target| target.id.clone());
            self.focused = first;
        }
    }

    /// Cancel the gesture while retaining ownership of its eventual release.
    pub fn cancel_pointer_press(&mut self) {
        self.cancelled_release_pending |= self.pressed.take().is_some();
        self.carried.cancel();
        self.armed_rect = None;
    }

    /// Forward from the host's pointer-capture query, including cancelled gestures.
    #[must_use]
    pub fn wants_pointer_capture(&self) -> bool {
        self.pressed.is_some() || self.cancelled_release_pending
    }

    fn enabled(&self) -> impl Iterator<Item = &PaintedTarget> {
        self.targets.iter().filter(|target| !target.disabled)
    }

    /// Whether the next Tab moves between targets rather than out of the
    /// host: forward it from `Widget::wants_tab_input`.
    #[must_use]
    pub fn wants_tab_input(&self) -> bool {
        let ids: Vec<&str> = self.enabled().map(|target| target.id.as_str()).collect();
        let Some(index) = self
            .focused
            .as_deref()
            .and_then(|id| ids.iter().position(|candidate| *candidate == id))
        else {
            return false;
        };
        if self.shift {
            index > 0
        } else {
            index + 1 < ids.len()
        }
    }

    /// The ids activated since the last call, in order.
    pub fn take_activated(&mut self) -> Vec<String> {
        std::mem::take(&mut self.activated)
    }

    fn activate(&mut self, id: &str) -> bool {
        if self
            .targets
            .iter()
            .any(|target| target.id == id && !target.disabled)
        {
            self.activated.push(id.to_string());
            true
        } else {
            false
        }
    }

    fn handle_mouse(&mut self, state: ElementState, handled: &mut bool) {
        let under = self.hit(self.cursor).map(str::to_string);
        match state {
            ElementState::Pressed => {
                // A fresh press starts a new gesture even outside our targets:
                // an absent old release must not capture the new click.
                self.pressed = None;
                self.carried = HeldPress::default();
                self.armed_rect = None;
                self.cancelled_release_pending = false;
                if let Some(id) = under.filter(|id| !self.interaction(id).disabled) {
                    self.focused = Some(id.clone());
                    self.armed_rect = self
                        .targets
                        .iter()
                        .find(|target| target.id == id)
                        .map(|target| target.rect);
                    if let Some(rect) = self.armed_rect {
                        self.carried.press(PressAt::one(&id, rect));
                    }
                    self.cancelled_release_pending = false;
                    self.pressed = Some(id);
                    *handled = true;
                }
            }
            ElementState::Released => {
                self.armed_rect = None;
                if std::mem::take(&mut self.cancelled_release_pending) {
                    self.carried.cancel();
                    *handled = true;
                } else if let Some(pressed) = self.pressed.take() {
                    self.carried.cancel();
                    *handled = true;
                    if under.as_deref() == Some(pressed.as_str()) {
                        self.activate(&pressed);
                    }
                } else if let Some(target) = self
                    .targets
                    .iter()
                    .rev()
                    .find(|target| target.rect.contains(self.cursor))
                    .filter(|target| !target.disabled)
                    // A host rebuilt while the button was held: the target
                    // pressed in its predecessor, at the same place,
                    // completes here.
                    && self
                        .carried
                        .release(Some(PressAt::one(&target.id, target.rect)))
                        == Release::Click(())
                {
                    let id = target.id.clone();
                    *handled = true;
                    self.activate(&id);
                }
            }
        }
    }

    /// Pointer routing for canvas hosts without a widget focus context.
    /// Uses the same completed-click and cancellation rules as `handle_event`,
    /// without requesting keyboard focus. Returns whether feedback changed;
    /// handled includes a cancelled gesture's matching release.
    pub fn handle_pointer_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        handled: &mut bool,
    ) -> bool {
        let before = (
            self.hovered.clone(),
            self.pressed.clone(),
            self.focused.clone(),
            self.host_focused,
        );
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.hovered = self.hit(self.cursor).map(str::to_string);
            }
            WindowEvent::CursorLeft { .. } => {
                self.hovered = None;
                self.cancel_pointer_press();
            }
            WindowEvent::Focused(false) => {
                self.cancel_pointer_press();
                self.set_host_focus(false);
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key
                        == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                    && self.wants_pointer_capture() =>
            {
                self.cancel_pointer_press();
                *handled = true;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_mouse(*state, handled);
            }
            _ => {}
        }
        before
            != (
                self.hovered.clone(),
                self.pressed.clone(),
                self.focused.clone(),
                self.host_focused,
            )
    }

    /// Track the pointer and keys against the registered targets. A press
    /// and release on the same target activates it; Enter or Space
    /// activates the focused one; Tab and Shift+Tab move focus between
    /// targets. Marks the event handled when a target took it, and asks the
    /// host for keyboard focus when one is pressed. Returns whether
    /// anything visible changed.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let mut pointer_handled = false;
        let changed = self.handle_pointer_event(event, ctx.scale_factor, &mut pointer_handled);
        if pointer_handled {
            *ctx.handled = true;
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            ) {
                *ctx.focus_target = Some(ctx.node_id);
            }
        }
        let before = (self.focused.clone(), self.host_focused);
        match event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.shift = modifiers.state().shift_key();
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.host_focused && event.state == ElementState::Pressed =>
            {
                use winit::keyboard::{Key, NamedKey};
                match &event.logical_key {
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        if let Some(id) = self.focused.clone()
                            && (event.repeat || self.activate(&id))
                        {
                            *ctx.handled = true;
                        }
                    }
                    Key::Named(NamedKey::Tab) if self.wants_tab_input() => {
                        let ids: Vec<String> =
                            self.enabled().map(|target| target.id.clone()).collect();
                        if let Some(index) = self
                            .focused
                            .as_deref()
                            .and_then(|id| ids.iter().position(|candidate| candidate == id))
                        {
                            let next = if self.shift { index - 1 } else { index + 1 };
                            self.focused = ids.get(next).cloned();
                            *ctx.handled = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        changed || before != (self.focused.clone(), self.host_focused)
    }

    /// The focus ring around the focused target, while the host holds
    /// keyboard focus.
    pub fn paint_focus(
        &self,
        scene: &mut Scene,
        transform: Affine,
        radius: f64,
        theme: &crate::theme::Theme,
    ) {
        if !self.host_focused {
            return;
        }
        if let Some(target) = self
            .focused
            .as_deref()
            .and_then(|id| self.targets.iter().find(|target| target.id == id))
        {
            paint_focus_ring(scene, transform, target.rect, radius, theme);
        }
    }

    /// The targets as accessible buttons, for the host's
    /// `Widget::accessibility` node, each routed back by its id.
    #[must_use]
    pub fn accessibility_children(&self) -> Vec<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        self.targets
            .iter()
            .map(|target| {
                AccessNode::new(Role::Button)
                    .with_name(target.label.clone())
                    .with_action_target(target.id.clone())
                    .with_bounds(target.rect)
                    .with_state(
                        AccessState::new()
                            .focusable(!target.disabled)
                            .focused(
                                self.host_focused
                                    && self.focused.as_deref() == Some(target.id.as_str()),
                            )
                            .disabled(target.disabled),
                    )
            })
            .collect()
    }

    /// Route an assistive-technology action on one of
    /// [`Self::accessibility_children`]: forward the host's
    /// `Widget::perform_action` key path here.
    pub fn perform_action(&mut self, action: &crate::a11y::Action, key_path: &[u64]) -> bool {
        let Some(&key) = key_path.first() else {
            return false;
        };
        let Some(id) = self
            .targets
            .iter()
            .find(|target| crate::a11y::hash_key(&target.id) == key)
            .map(|target| target.id.clone())
        else {
            return false;
        };
        self.perform_semantic_action(action, &id)
    }

    /// [`Self::perform_action`] by the target's id (its semantic action
    /// target): forward the host's `Widget::perform_semantic_action` here.
    pub fn perform_semantic_action(&mut self, action: &crate::a11y::Action, target: &str) -> bool {
        match action {
            crate::a11y::Action::Activate => self.activate(target),
            crate::a11y::Action::Focus => {
                self.focus(target);
                self.focused.as_deref() == Some(target)
            }
            _ => false,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{Key, NamedKey};

    fn send(targets: &mut PaintedTargets, event: &WindowEvent) -> bool {
        let mut tree = taffy::TaffyTree::<()>::new();
        let node = crate::widget::TaffyTreeExt::leaf_node(&mut tree, taffy::Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        targets.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: crate::widget::Edits::lend(&mut doc),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
        handled
    }

    use crate::synthetic_input::cursor_moved;

    fn button(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        }
    }

    fn keys() -> PaintedTargets {
        let mut targets = PaintedTargets::new();
        targets.begin_frame();
        targets.target("seven", Rect::new(0.0, 0.0, 40.0, 40.0), "7");
        targets.target("eight", Rect::new(50.0, 0.0, 90.0, 40.0), "8");
        targets.disabled_target("nine", Rect::new(100.0, 0.0, 140.0, 40.0), "9");
        targets
    }

    #[test]
    fn painted_targets_ignore_held_submit_repeats() {
        let mut targets = keys();
        targets.set_host_focus(true);
        let mut enter =
            crate::synthetic_input::key_input(&Key::Named(NamedKey::Enter), ElementState::Pressed);
        assert!(send(&mut targets, &enter));
        assert_eq!(targets.take_activated(), vec!["seven"]);
        if let WindowEvent::KeyboardInput { event, .. } = &mut enter {
            event.repeat = true;
        }
        assert!(
            send(&mut targets, &enter),
            "owned held submit must be consumed"
        );
        assert!(
            targets.take_activated().is_empty(),
            "holding Enter must not repeat the action"
        );
    }

    #[test]
    fn painted_targets_cancel_a_press_when_its_registered_geometry_moves() {
        let mut targets = keys();
        send(&mut targets, &cursor_moved(10.0, 10.0));
        assert!(send(&mut targets, &button(ElementState::Pressed)));
        targets.begin_frame();
        targets.target("seven", Rect::new(100.0, 100.0, 140.0, 140.0), "7");
        send(&mut targets, &cursor_moved(110.0, 110.0));
        assert!(
            send(&mut targets, &button(ElementState::Released)),
            "the old press owns its cancelled release"
        );
        assert!(
            targets.take_activated().is_empty(),
            "moving a target must cancel the old-position press"
        );
    }

    #[test]
    fn painted_targets_cancel_pointer_activation_on_focus_loss() {
        let mut targets = keys();
        targets.set_host_focus(true);
        send(&mut targets, &cursor_moved(10.0, 10.0));
        send(&mut targets, &button(ElementState::Pressed));
        targets.set_host_focus(false);
        assert!(send(&mut targets, &button(ElementState::Released)));
        assert!(
            targets.take_activated().is_empty(),
            "an interrupted unfocused press must not activate"
        );
    }

    #[test]
    fn painted_targets_escape_cancels_and_consumes_the_pointer_release() {
        let mut targets = keys();
        send(&mut targets, &cursor_moved(10.0, 10.0));
        send(&mut targets, &button(ElementState::Pressed));
        let escape =
            crate::synthetic_input::key_input(&Key::Named(NamedKey::Escape), ElementState::Pressed);
        assert!(send(&mut targets, &escape));
        assert!(send(&mut targets, &button(ElementState::Released)));
        assert!(targets.take_activated().is_empty());
    }

    #[test]
    fn a_click_on_a_painted_target_hovers_presses_and_activates_it() {
        let mut targets = keys();
        send(&mut targets, &cursor_moved(10.0, 10.0));
        assert!(targets.interaction("seven").hovered);
        assert!(send(&mut targets, &button(ElementState::Pressed)));
        assert!(targets.interaction("seven").pressed);
        assert!(send(&mut targets, &button(ElementState::Released)));
        assert_eq!(targets.take_activated(), vec!["seven".to_string()]);
        assert!(targets.take_activated().is_empty(), "taken once");

        // Pressed on one target, released on another: nothing fires.
        send(&mut targets, &button(ElementState::Pressed));
        send(&mut targets, &cursor_moved(60.0, 10.0));
        send(&mut targets, &button(ElementState::Released));
        assert!(targets.take_activated().is_empty());

        // A disabled target shows but takes nothing.
        send(&mut targets, &cursor_moved(110.0, 10.0));
        assert!(!send(&mut targets, &button(ElementState::Pressed)));
        assert!(targets.interaction("nine").disabled);
    }

    #[test]
    fn keyboard_focus_moves_between_targets_and_activates() {
        let mut targets = keys();
        targets.set_host_focus(true);
        assert_eq!(targets.focused(), Some("seven"));
        assert!(targets.interaction("seven").focused);
        assert!(
            targets.wants_tab_input(),
            "Tab stays inside before the last target"
        );
        let tab =
            crate::synthetic_input::key_input(&Key::Named(NamedKey::Tab), ElementState::Pressed);
        assert!(send(&mut targets, &tab));
        assert_eq!(targets.focused(), Some("eight"));
        assert!(
            !targets.wants_tab_input(),
            "the disabled target is skipped; Tab leaves"
        );
        let enter =
            crate::synthetic_input::key_input(&Key::Named(NamedKey::Enter), ElementState::Pressed);
        assert!(send(&mut targets, &enter));
        assert_eq!(targets.take_activated(), vec!["eight".to_string()]);
    }

    #[test]
    fn painted_targets_are_accessible_buttons_routed_back_by_key() {
        let mut targets = keys();
        let nodes = targets.accessibility_children();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[1].role, crate::a11y::Role::Button);
        assert_eq!(nodes[1].name.as_deref(), Some("8"));
        assert!(nodes[2].state.disabled);
        let key = nodes[1].key.expect("keyed by id");
        assert!(targets.perform_action(&crate::a11y::Action::Activate, &[key]));
        assert_eq!(targets.take_activated(), vec!["eight".to_string()]);
        assert!(!targets.perform_action(&crate::a11y::Action::Activate, &[key ^ 1]));
        assert!(!targets.perform_semantic_action(&crate::a11y::Action::Activate, "nine"));
    }
}
