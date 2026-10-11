//! The toolkit's [`Dropdown`] hosted in the painted reader.
//!
//! The reader lays out and paints its own screens, so a dropdown here is
//! placed at the rect the app computes, painted with the app, and its list
//! painted in the top layer when the app finishes. Events reach it while its
//! list is open or it has focus, and on a press over it; the value the person
//! picks is reported to the app once, which persists it through its own
//! actions.

use automerge::ObjId;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::LocalState;
use ui::a11y::AccessNode;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ControlSize, Dropdown, DropdownFace, Widget};

use crate::input_controls::InputEventCtx;

pub(crate) struct HostedDropdown {
    id: String,
    widget: Dropdown,
    value: LocalState<String>,
    /// The value the app last set or was told about.
    seen: String,
    /// Painted this frame: only a painted dropdown takes events.
    drawn: bool,
    cursor: Point,
}

impl HostedDropdown {
    pub(crate) fn new(id: &str, accessible_name: &str) -> Self {
        let value = LocalState::new(String::new());
        let widget = Dropdown::from_state(Vec::new(), &value)
            .with_id(id)
            .with_accessible_name(accessible_name);
        Self {
            id: id.to_string(),
            widget,
            value,
            seen: String::new(),
            drawn: false,
            cursor: Point::ZERO,
        }
    }

    pub(crate) fn begin_frame(&mut self) {
        self.drawn = false;
    }

    pub(crate) fn is_open(&self) -> bool {
        self.widget.is_open()
    }

    pub(crate) fn close(&mut self) {
        self.widget.close();
    }

    /// Offer `options` as `(value, label)` pairs with `selected` chosen.
    pub(crate) fn set(&mut self, options: Vec<(String, String)>, selected: &str) {
        self.widget.set_value_options(options);
        if self.seen != selected {
            self.widget.set_selected(selected);
            selected.clone_into(&mut self.seen);
        }
    }

    /// The width the control needs to show its widest `labels` beside the
    /// chevron, at `size`.
    pub(crate) fn width<'a>(
        labels: impl IntoIterator<Item = &'a str>,
        size: ControlSize,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        DropdownFace::new(Rect::ZERO, "")
            .control_size(size)
            .intrinsic_width(labels, text, theme)
    }

    /// Paint the closed control at `rect`.
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        self.widget.apply_layout_external(rect);
        self.widget
            .draw(scene, transform, text, theme, &PresenceCtx::empty());
        self.drawn = true;
    }

    /// Paint the open list over everything, inside `window`.
    pub(crate) fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        if !self.drawn {
            self.widget.close();
            return;
        }
        self.widget.set_viewport(window);
        self.widget
            .draw_overlay(scene, transform, text, theme, &PresenceCtx::empty());
    }

    /// Route one event to the dropdown. Returns whether it took the event,
    /// in which case nothing under it may act on it. Pointer moves are
    /// always shared: the app tracks the pointer too.
    pub(crate) fn handle_event(&mut self, ctx: &mut InputEventCtx<'_>) -> bool {
        if !self.drawn {
            return false;
        }
        let press = matches!(
            ctx.event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        if let WindowEvent::CursorMoved { position, .. } = ctx.event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        let over = self.widget.get_rect().contains(self.cursor);
        let engaged = self.widget.is_open() || self.widget.is_focused();
        if press && !over && !self.widget.is_open() {
            self.widget.set_focus(false);
            return false;
        }
        if !(engaged || over) {
            return false;
        }
        let mut handled = false;
        let mut focus = None;
        self.widget
            .handle_event(ctx.event, &mut ctx.widget_ctx(&mut handled, &mut focus));
        if focus.is_some() {
            self.widget.set_focus(true);
        }
        let shared = matches!(
            ctx.event,
            WindowEvent::CursorMoved { .. } | WindowEvent::CursorLeft { .. }
        );
        handled && !shared
    }

    /// The value the person just picked, once.
    pub(crate) fn take_change(&mut self) -> Option<String> {
        let value = self.value.get();
        (value != self.seen).then(|| {
            value.clone_into(&mut self.seen);
            value
        })
    }

    /// The combobox node, at the control's bounds.
    pub(crate) fn accessibility(&self) -> Option<AccessNode> {
        if !self.drawn {
            return None;
        }
        self.widget
            .accessibility()
            .map(|node| node.with_bounds(self.widget.get_rect()))
    }

    /// An assistive technology's action on the combobox. Returns whether it
    /// was this dropdown's.
    pub(crate) fn perform_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) -> bool {
        target == self.id && self.widget.perform_action(action, &[], state, root)
    }
}
