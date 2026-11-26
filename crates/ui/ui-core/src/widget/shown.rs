//! A control shown in one state of interaction whatever the pointer and the keyboard do: hovered,
//! pressed or focused, still and taking no input. What a style guide, a design review or a page
//! of documentation shows beside the live control, drawn by the control itself rather than a
//! picture of it.
//!
//! [`InState`] draws its content with the state it names in effect: each control asks
//! [`interaction`] for the state it paints, which is its own unless an [`InState`] around it is
//! being drawn.

use std::cell::Cell;

use winit::event::WindowEvent;

use crate::widget::Interaction;
use tilo_ui_base::{component::Component, component::Described, component::PartViews};
use tilo_ui_base::{widget::Element, widget::EventCtx, widget::Widget};

thread_local! {
    static SHOWN: Cell<Option<Interaction>> = const { Cell::new(None) };
}

/// The state a control paints: `own`, the state its input gave it, unless an [`InState`] around it
/// is being drawn, whose state it shows instead. Disabled stays the control's own: a control that
/// cannot be used says so in every state.
#[must_use]
pub fn interaction(own: Interaction) -> Interaction {
    match SHOWN.with(Cell::get) {
        Some(shown) => Interaction {
            disabled: own.disabled,
            ..shown
        },
        None => own,
    }
}

/// Whether an [`InState`] is drawing what is being drawn now: a control whose look is cached by
/// its own state draws again under one.
#[must_use]
pub fn is_shown() -> bool {
    SHOWN.with(Cell::get).is_some()
}

/// Run `f` with `state` in effect for every control it draws.
fn with<R>(state: Interaction, f: impl FnOnce() -> R) -> R {
    let before = SHOWN.with(|shown| shown.replace(Some(state)));
    let result = f();
    SHOWN.with(|shown| shown.set(before));
    result
}

/// A state of interaction a control can be shown in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    /// As it rests: no pointer over it, no focus in it.
    #[default]
    Rest,
    /// With the pointer over it.
    Hovered,
    /// Held down by a press.
    Pressed,
    /// With the keyboard's focus in it, its ring showing.
    Focused,
}

tilo_ui_base::choice!(State, "InteractionState", {
    Rest => "rest", Hovered => "hovered", Pressed => "pressed", Focused => "focused",
});

impl State {
    fn interaction(self) -> Interaction {
        Interaction {
            hovered: matches!(self, Self::Hovered | Self::Pressed),
            pressed: self == Self::Pressed,
            focused: self == Self::Focused,
            disabled: false,
        }
    }
}

/// A control as it looks in one state of interaction (hovered, pressed, focused), held still and
/// taking no input: the states of a button or a field side by side, as a style guide or a page of
/// documentation shows them. Disabled, checked, chosen and invalid are the control's own props;
/// this shows what only a pointer or the keyboard otherwise gives it. Assistive technology skips
/// it: the control it shows is a picture of one, not one to use.
#[derive(Component, Default)]
#[component(name = "InState", role = "Group")]
pub struct InStateProps {
    /// The state its content is shown in.
    #[component(positional)]
    pub state: State,
    /// The control shown.
    #[component(part(default))]
    pub content: Option<Element>,
}

/// See [`InStateProps`].
pub struct InState {
    state: State,
    content: Element,
}

impl InState {
    pub fn new(state: State, content: Element) -> Self {
        Self { state, content }
    }

    fn nothing() -> Element {
        Box::new(tilo_ui_base::widget::Spacer::token(
            tilo_ui_base::tokens::SpacingStep::S0,
        ))
    }
}

impl tilo_ui_base::widget::Decorator for InState {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "a decorator is the widget it wraps: that widget draws under the decorator's key"
    )]
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        let state = self.state.interaction();
        with(state, || self.content.draw(ctx));
    }

    /// What it is: a picture of the control in a state, which a page sends and a test finds as
    /// such, not the control it shows.
    fn name(&self) -> &'static str {
        "InState"
    }

    /// Still: no pointer, key or touch reaches what it shows.
    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut EventCtx<'_>) {}

    fn is_focusable(&self) -> bool {
        false
    }

    fn cursor_shape(&self, _point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
        None
    }

    fn tooltip_target_at(
        &self,
        _point: vello::kurbo::Point,
    ) -> Option<(vello::kurbo::Rect, String)> {
        None
    }

    /// Hidden, with what it holds: a reader and the keyboard's order meet the live control, never
    /// its picture.
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        let mut node = self.content.accessibility().unwrap_or_else(|| {
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Group)
        });
        node.state.hidden = true;
        node.state.focusable = false;
        Some(node)
    }
}

impl Described for InState {
    type Props = InStateProps;

    fn from_props(_key: &str, props: InStateProps) -> Self {
        Self::new(props.state, props.content.unwrap_or_else(Self::nothing))
    }

    fn set_props(&mut self, props: InStateProps) -> bool {
        if props.state != self.state {
            return false;
        }
        if let Some(content) = props.content {
            self.content = content;
        }
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        (part == "content").then_some(self as &mut dyn PartViews)
    }
}

impl PartViews for InState {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        (index == 0).then_some(&mut self.content)
    }

    fn insert(&mut self, index: usize, view: Element) {
        if index == 0 {
            self.content = view;
        }
    }

    fn remove(&mut self, index: usize) {
        if index == 0 {
            self.content = Self::nothing();
        }
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, index: usize, view: Element) {
        if index == 0 {
            self.content = view;
        }
    }
}

#[cfg(test)]
#[path = "shown_tests.rs"]
mod tests;
