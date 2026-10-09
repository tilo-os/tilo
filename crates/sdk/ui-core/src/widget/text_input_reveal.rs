//! The show/hide control every single-line secret [`super::TextInput`] carries
//! at its trailing edge.
//!
//! It is a retained child of the field, so assistive technology sees a named
//! button that an AT or agent activation toggles. Like every inline accessory
//! of a field it is not a Tab stop: Tab moves from field to field, as on macOS
//! with full keyboard access off. A pointer press toggles without taking
//! focus, so typing continues in the field. Only the field's
//! paint changes: the value never reaches the accessibility tree, a log, the
//! clipboard or persisted state because of a reveal.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Position, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{ModelPath, TaffyTreeExt, Widget};

/// Fluent keys; the same string is the tooltip and the accessible name.
const SHOW_LABEL: &str = "text-input-show-password";
const HIDE_LABEL: &str = "text-input-hide-password";

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent reveal, pointer, focus and availability flags"
)]
pub(super) struct RevealToggle {
    revealed: bool,
    enabled: bool,
    rect: Rect,
    hovered: bool,
    pressed: bool,
    focused: bool,
    /// A finger that went down on the control; it toggles on release.
    touch: Option<u64>,
    test_id: String,
}

impl RevealToggle {
    pub(super) fn new() -> Self {
        Self {
            revealed: false,
            enabled: true,
            rect: Rect::ZERO,
            hovered: false,
            pressed: false,
            focused: false,
            touch: None,
            test_id: "text_input_reveal".to_string(),
        }
    }

    pub(super) fn revealed(&self) -> bool {
        self.revealed
    }

    pub(super) fn set_revealed(&mut self, on: bool) {
        self.revealed = on;
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.hovered = false;
            self.pressed = false;
            self.focused = false;
            self.touch = None;
        }
    }

    pub(super) fn set_field_id(&mut self, field_id: &str) {
        self.test_id = if field_id.is_empty() {
            "text_input_reveal".to_string()
        } else {
            format!("{field_id}_reveal")
        };
    }

    /// Width of the trailing square the control occupies inside a field of
    /// `field_height`.
    pub(super) fn width_for(field_height: f64) -> f64 {
        field_height.clamp(24.0, 48.0)
    }

    /// Place the control flush with the field's trailing edge.
    pub(super) fn place(&mut self, field: Rect) {
        let width = Self::width_for(field.height());
        self.rect = Rect::new(field.x1 - width, field.y0, field.x1, field.y1);
    }

    pub(super) fn contains(&self, point: Point) -> bool {
        self.enabled && self.rect.contains(point)
    }

    fn label(&self) -> &'static str {
        if self.revealed {
            HIDE_LABEL
        } else {
            SHOW_LABEL
        }
    }

    fn toggle(&mut self) -> bool {
        if !self.enabled {
            return false;
        }
        self.revealed = !self.revealed;
        true
    }

    /// The tooltip claim for `point`, for hosts that hit-test only the field.
    pub(super) fn tooltip_at(&self, point: Point) -> Option<(Rect, String)> {
        self.contains(point)
            .then(|| (self.rect, crate::i18n!(self.label())))
    }
}

impl Widget for RevealToggle {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Out of flow at the trailing edge; the field positions the rect
        // itself so embedded fields without a layout tree match exactly.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the control is a few dozen logical pixels, far inside f32"
        )]
        let width = Self::width_for(self.rect.height().max(40.0)) as f32;
        let style = Style {
            position: Position::Absolute,
            size: Size {
                width: Dimension::length(width),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    /// The owning field places this control in its own `apply_layout`.
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.is_zero_area() {
            return;
        }
        // A bar-sized control face inside the field's band, around the
        // standard glyph.
        let metrics = &theme.tokens.metrics;
        let side = f64::from(metrics.bar_control_height).min(self.rect.height());
        let face = RoundedRect::from_rect(
            Rect::from_center_size(self.rect.center(), (side, side)),
            f64::from(theme.tokens.radii.item),
        );
        let glyph = f64::from(metrics.icon_md).min(side);
        if self.enabled && (self.hovered || self.pressed) {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                // The field is the raised face; the control lifts off it.
                theme.tokens.colors.lift_on(
                    theme.tokens.colors.surfaces.raised,
                    if self.pressed { 1.0 } else { 0.5 },
                ),
                None,
                &face,
            );
        }
        if self.enabled && self.focused {
            crate::widget::paint_focus_ring(
                scene,
                transform,
                face.rect(),
                face.radii().top_left,
                theme,
            );
        }
        let color = if !self.enabled {
            theme
                .text_dim
                .with_alpha_factor(crate::widget::DISABLED_ALPHA)
        } else if self.hovered || self.focused {
            theme.text_primary
        } else {
            theme.text_dim
        };
        // The glyph names the action a press performs, as the label does.
        let icon = if self.revealed {
            Icon::EyeOff
        } else {
            Icon::Eye
        };
        icon.draw(
            scene,
            transform,
            Rect::from_center_size(self.rect.center(), (glyph, glyph)),
            color,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.enabled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                // A hover change is a repaint, never consumed input.
                self.hovered = self.rect.contains(Point::new(
                    position.x / ctx.scale_factor,
                    position.y / ctx.scale_factor,
                ));
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let pressed = *state == ElementState::Pressed;
                if self.hovered {
                    // Keyboard focus deliberately stays where it was: a
                    // pointer reveal must not pull the caret out of the field.
                    if pressed {
                        self.pressed = true;
                        *ctx.handled = true;
                    } else if self.pressed {
                        self.pressed = false;
                        self.toggle();
                        *ctx.handled = true;
                    }
                } else if !pressed && self.pressed {
                    self.pressed = false;
                    *ctx.handled = true;
                }
            }
            WindowEvent::Touch(touch) => {
                let point = Point::new(
                    touch.location.x / ctx.scale_factor,
                    touch.location.y / ctx.scale_factor,
                );
                match touch.phase {
                    TouchPhase::Started if self.touch.is_none() && self.rect.contains(point) => {
                        self.touch = Some(touch.id);
                        *ctx.handled = true;
                    }
                    TouchPhase::Moved if self.touch == Some(touch.id) => *ctx.handled = true,
                    TouchPhase::Ended if self.touch == Some(touch.id) => {
                        self.touch = None;
                        if self.rect.contains(point) {
                            self.toggle();
                        }
                        *ctx.handled = true;
                    }
                    TouchPhase::Cancelled if self.touch == Some(touch.id) => {
                        self.touch = None;
                        *ctx.handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "PasswordReveal"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.test_id.as_str())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn tooltip(&self) -> Option<&str> {
        self.enabled.then(|| self.label())
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.contains(point)
            .then_some(winit::window::CursorIcon::Pointer)
    }

    /// An inline accessory is not a Tab stop; the pointer, assistive
    /// technology and agents reach it.
    fn is_focusable(&self) -> bool {
        false
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused && self.enabled;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        match action {
            crate::a11y::Action::Activate => self.toggle(),
            _ => false,
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Button)
                .with_name(crate::i18n!(self.label()))
                .with_state(AccessState {
                    focused: self.focused,
                    disabled: !self.enabled,
                    ..Default::default()
                }),
        )
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }
}
