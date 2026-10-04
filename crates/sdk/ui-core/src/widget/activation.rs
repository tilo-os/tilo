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

#[derive(Default)]
pub(crate) struct Activation {
    id: String,
    action: Option<ActionEmitter<()>>,
    on_activate: Option<Box<dyn FnMut()>>,
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
    }

    pub(crate) fn set_action(&mut self, binding: tilo_actions::ActionBinding<()>) {
        self.action = Some(ActionEmitter::new(binding));
        self.on_activate = None;
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
/// routes their activation back ([`PaintedTargets::perform_action_at`],
/// [`PaintedTargets::perform_semantic_action`]).
/// Tab moves between the targets while the host holds keyboard focus
/// ([`PaintedTargets::wants_tab_input`]).
#[derive(Debug, Default)]
pub struct PaintedTargets {
    targets: Vec<PaintedTarget>,
    hovered: Option<String>,
    pressed: Option<String>,
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
        self.targets.push(PaintedTarget {
            id: id.into(),
            rect,
            label: label.into(),
            disabled: false,
        });
    }

    /// [`Self::target`] that shows but does nothing (a key that does not
    /// apply now).
    pub fn disabled_target(&mut self, id: impl Into<String>, rect: Rect, label: impl Into<String>) {
        self.targets.push(PaintedTarget {
            id: id.into(),
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
        self.host_focused = focused;
        if focused && self.focused.is_none() {
            let first = self.enabled().next().map(|target| target.id.clone());
            self.focused = first;
        }
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
        let before = (
            self.hovered.clone(),
            self.pressed.clone(),
            self.focused.clone(),
        );
        match event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.shift = modifiers.state().shift_key();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.hovered = self.hit(self.cursor).map(str::to_string);
            }
            WindowEvent::CursorLeft { .. } => {
                self.hovered = None;
                self.pressed = None;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let under = self.hit(self.cursor).map(str::to_string);
                match state {
                    ElementState::Pressed => {
                        if let Some(id) = under.filter(|id| !self.interaction(id).disabled) {
                            self.focused = Some(id.clone());
                            self.pressed = Some(id);
                            *ctx.focus_target = Some(ctx.node_id);
                            *ctx.handled = true;
                        }
                    }
                    ElementState::Released => {
                        if let Some(pressed) = self.pressed.take() {
                            *ctx.handled = true;
                            if under.as_deref() == Some(pressed.as_str()) {
                                self.activate(&pressed);
                            }
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.host_focused && event.state == ElementState::Pressed =>
            {
                use winit::keyboard::{Key, NamedKey};
                match &event.logical_key {
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        if let Some(id) = self.focused.clone()
                            && self.activate(&id)
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
        before
            != (
                self.hovered.clone(),
                self.pressed.clone(),
                self.focused.clone(),
            )
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
    /// `Widget::perform_action_at` key path here.
    pub fn perform_action_at(&mut self, action: &crate::a11y::Action, key_path: &[u64]) -> bool {
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

    /// [`Self::perform_action_at`] by the target's id (its semantic action
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
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
        handled
    }

    fn moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn button(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
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
    fn a_click_on_a_painted_target_hovers_presses_and_activates_it() {
        let mut targets = keys();
        send(&mut targets, &moved(10.0, 10.0));
        assert!(targets.interaction("seven").hovered);
        assert!(send(&mut targets, &button(ElementState::Pressed)));
        assert!(targets.interaction("seven").pressed);
        assert!(send(&mut targets, &button(ElementState::Released)));
        assert_eq!(targets.take_activated(), vec!["seven".to_string()]);
        assert!(targets.take_activated().is_empty(), "taken once");

        // Pressed on one target, released on another: nothing fires.
        send(&mut targets, &button(ElementState::Pressed));
        send(&mut targets, &moved(60.0, 10.0));
        send(&mut targets, &button(ElementState::Released));
        assert!(targets.take_activated().is_empty());

        // A disabled target shows but takes nothing.
        send(&mut targets, &moved(110.0, 10.0));
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
            crate::synthetic_input::key_event(&Key::Named(NamedKey::Tab), ElementState::Pressed);
        assert!(send(&mut targets, &tab));
        assert_eq!(targets.focused(), Some("eight"));
        assert!(
            !targets.wants_tab_input(),
            "the disabled target is skipped; Tab leaves"
        );
        let enter =
            crate::synthetic_input::key_event(&Key::Named(NamedKey::Enter), ElementState::Pressed);
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
        assert!(targets.perform_action_at(&crate::a11y::Action::Activate, &[key]));
        assert_eq!(targets.take_activated(), vec!["eight".to_string()]);
        assert!(!targets.perform_action_at(&crate::a11y::Action::Activate, &[key ^ 1]));
        assert!(!targets.perform_semantic_action(&crate::a11y::Action::Activate, "nine"));
    }
}
