//! Pointer, keyboard and accessibility activation shared by the content composites that are themselves
//! one pressable target (a list row, a media card, an interactive container).

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::pointer_replay::{HeldPress, PressAt, Release};
use tilo_ui_base::{widget::ActionEmitter, widget::ControlBindingMetadata};

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
    theme: &tilo_ui_base::theme::Theme,
) {
    // Keyboard focus only: a click that focused the surface shows nothing.
    if !tilo_ui_base::focus_visible::shown() {
        return;
    }
    scene.stroke(
        &Stroke::new(2.0),
        transform,
        theme.border_focus,
        None,
        &RoundedRect::from_rect(rect.inflate(1.0, 1.0), radius + 1.0),
    );
}

/// Lay everything painted next under the disabled veil, over the shape the
/// control draws as its bounds: a disabled control is its rest face dimmed as
/// one group, at full-strength edges so it still reads where it stands. The
/// caller pops the layer once its face is painted.
pub fn paint_disabled(scene: &mut Scene, transform: Affine, bounds: &impl vello::kurbo::Shape) {
    scene.push_layer(
        vello::peniko::Fill::NonZero,
        vello::peniko::BlendMode::default(),
        tilo_ui_base::widget::DISABLED_ALPHA,
        transform,
        bounds,
    );
}

thread_local! {
    static PRESSES: std::cell::RefCell<Vec<Point>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The presses asked for since the last call, in order.
#[cfg(feature = "model")]
pub fn take_presses() -> Vec<Point> {
    PRESSES.with(|presses| std::mem::take(&mut *presses.borrow_mut()))
}

#[derive(Default)]
pub(crate) struct Activation {
    id: String,
    action: Option<ActionEmitter<()>>,
    on_activate: Option<Box<dyn FnMut()>>,
    pub(crate) state: Interaction,
    held: HeldPress,
    cursor: Point,
    /// It opens or shows something (a view operation), which a double-click asks for once.
    opens: bool,
}

std::thread_local! {
    /// The window whose document the pointer last clicked in, the control it clicked, when and
    /// where: a click on the same control again in the same window within the multi-click window
    /// at the same place is the rest of a double-click, never a second open. Kept apart from the
    /// control, which a view may build afresh between the two clicks; the window is its
    /// document's actor, so one card keyed `blue` in one window never counts a click in another
    /// window's card of that key as its own second click.
    static LAST_CLICK: std::cell::RefCell<
        Option<(automerge::ActorId, String, crate::time::Instant, Point)>,
    > = const { std::cell::RefCell::new(None) };
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
        self.opens = false;
    }

    pub(crate) fn set_action(&mut self, binding: impl crate::Bind<()>) {
        self.action = Some(ActionEmitter::new(crate::Bind::bound(binding)));
        self.on_activate = None;
        self.opens = false;
    }

    /// A press that opens or shows something: a double-click asks for it once.
    pub(crate) fn set_open(&mut self, handler: Box<dyn FnMut()>) {
        self.set_callback(handler);
        self.opens = true;
    }

    pub(crate) fn set_event(&mut self, event: tilo_ui_base::component::Event) {
        match event {
            tilo_ui_base::component::Event::None => self.clear_event(),
            tilo_ui_base::component::Event::Action(binding) => self.set_action(binding),
            tilo_ui_base::component::Event::View(intent) => {
                self.set_open(Box::new(intent.sender()))
            }
        }
    }

    pub(crate) fn clear_event(&mut self) {
        self.on_activate = None;
        self.action = None;
    }

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

    /// Whether a pointer press on it is taken: an action the person may not run still takes it,
    /// and the dispatch says why it does not run ([`Self::click`]).
    pub(crate) fn takes_press(&self) -> bool {
        self.is_bound() && !self.state.disabled
    }

    /// The person's own click: it activates, or sends the press of an action they may not run
    /// here, to be refused with why ([`ActionEmitter::press`]).
    pub(crate) fn click(&mut self) -> bool {
        if self.is_enabled() {
            return self.activate();
        }
        if !self.takes_press() {
            return false;
        }
        if let Some(action) = &mut self.action
            && let Err(error) = action.press(())
        {
            tracing::warn!(%error, control = %self.id, "action-bound surface refused its arguments");
        }
        true
    }

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

    pub(crate) fn handle_pointer(
        &mut self,
        event: &WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
        rect: Rect,
        exclude: &[Rect],
    ) -> bool {
        let before = self.state;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = ctx.logical(position);
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
            } if self.takes_press() => {
                let inside = rect.contains(self.cursor)
                    && !exclude.iter().any(|inner| inner.contains(self.cursor));
                match state {
                    ElementState::Pressed if inside => {
                        self.press(rect);
                        if self.is_enabled() {
                            *ctx.focus_target = Some(ctx.node_id);
                        }
                        *ctx.handled = true;
                    }
                    ElementState::Released => {
                        let release = self.release(rect, inside);
                        *ctx.handled |= release.ended();
                        if release == Release::Click(())
                            && !self.repeats_last_click(ctx.state.doc().get_actor())
                        {
                            self.click();
                        }
                    }
                    ElementState::Pressed => {}
                }
            }
            _ => {}
        }
        before != self.state
    }

    /// Note a pointer click in the window whose document is `window`, and whether it only repeats
    /// the one before: a double-click on something that opens (a file in a list, a contact) opens
    /// it once, as a person who double-clicks by habit means. A control that changes something
    /// counts every click.
    fn repeats_last_click(&self, window: &automerge::ActorId) -> bool {
        let now = crate::time::now();
        LAST_CLICK.with_borrow_mut(|last| {
            let repeats = self.opens
                && last.as_ref().is_some_and(|(in_window, id, at, place)| {
                    in_window == window
                        && *id == self.id
                        && now.duration_since(*at) < tilo_ui_base::gesture::multi_click_interval()
                        && place.distance(self.cursor) < tilo_ui_base::gesture::MULTI_CLICK_DIST
                });
            *last = Some((window.clone(), self.id.clone(), now, self.cursor));
            repeats
        })
    }

    pub(crate) fn press(&mut self, rect: Rect) {
        self.held.press(PressAt::one(&self.id, rect));
        self.state.pressed = true;
    }

    pub(crate) fn release(&mut self, rect: Rect, over: bool) -> Release {
        let release = self
            .held
            .release(over.then_some(PressAt::one(&self.id, rect)));
        self.state.pressed = false;
        release
    }

    pub(crate) fn cancel_press(&mut self) {
        self.held.cancel();
        self.state.pressed = false;
    }

    pub(crate) fn attach(
        &mut self,
        domain: &tilo_ui_base::ControlDomain,
        attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
        if let Some(action) = &mut self.action {
            let _ = action.attach(domain, attachment);
        }
    }

    pub(crate) fn take_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action.as_mut().and_then(ActionEmitter::take)
    }

    pub(crate) fn bindings(&self) -> Vec<ControlBindingMetadata> {
        if let Some(action) = &self.action {
            return vec![action.metadata(self.id.clone())];
        }
        Vec::new()
    }

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

/// Implement [`ActionControls`](tilo_ui_base::widget::controls::ActionControls) for
/// a widget whose own press is an [`Activation`] at `self.$field` and whose
/// children (if any) are reached through the default walks.
macro_rules! activation_controls {
    ($ty:ty, $($field:tt).+) => {
        impl tilo_ui_base::widget::controls::ActionControls for $ty {
            fn attach_controls(
                &mut self,
                domain: &tilo_ui_base::ControlDomain,
                attachment: tilo_ui_base::control_domain::DomainAttachment,
            ) {
                self.$($field).+.attach(domain, attachment);
                for child in tilo_ui_base::widget::Widget::children_mut(self) {
                    tilo_ui_base::widget::controls::attach_controls(child, domain, attachment);
                }
            }
            fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
                self.$($field).+.take_invocation().or_else(|| {
                    tilo_ui_base::widget::Widget::children_mut(self)
                        .into_iter()
                        .find_map(tilo_ui_base::widget::controls::take_control_invocation)
                })
            }
            fn control_action_bindings(&self) -> Vec<tilo_ui_base::widget::ControlBindingMetadata> {
                let mut bindings = self.$($field).+.bindings();
                for child in tilo_ui_base::widget::Widget::children(self) {
                    bindings.extend(tilo_ui_base::widget::controls::control_action_bindings(child));
                }
                bindings
            }
            fn refresh_control_action_availability(
                &mut self,
                allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
            ) {
                self.$($field).+.refresh_availability(allowed);
                for child in tilo_ui_base::widget::Widget::children_mut(self) {
                    tilo_ui_base::widget::controls::refresh_control_action_availability(child, allowed);
                }
            }
        }
    };
}
pub(crate) use activation_controls;
