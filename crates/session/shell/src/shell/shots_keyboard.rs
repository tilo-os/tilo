//! The on-screen keyboard as an offscreen capture draws it.
//!
//! On a live desktop the keyboard is a client of its own: the compositor
//! floats its surface and `FrameBuilder` only mirrors the client's pixels
//! (`ForeignSurfaceWidget`). A capture has no clients, so it has no keyboard
//! surface and nothing to mirror — a touch flow could then only be reviewed
//! with the keyboard missing. This module draws the real keyboard widget
//! instead, in exactly the panel the compositor docks the surface in
//! ([`keyboard_panel`]) and in the layout the focused field's content
//! purpose asks for, so a shot shows what a touch screen shows.
//!
//! It is offscreen-capture scaffolding, not a second keyboard: the widget,
//! its layouts and its paint are `apps/osk`'s own.

use std::collections::HashMap;

use taffy::prelude::*;
use ui::widget::Widget;
use vello::kurbo::Rect;
use vello::peniko::kurbo::Point;

use osk::widget::OskWidget;

/// Where the on-screen keyboard is docked on display `output`: at its
/// bottom, centred, no wider than 960 and between 220 and 380 tall with a
/// margin of 20. From the output's own top and left: a display beside a
/// taller one, or below another, does not start at the desktop's top edge.
#[must_use]
pub fn keyboard_panel(output: Rect) -> Rect {
    let margin = 20.0_f64.min(output.width() / 20.0);
    let width = (output.width() - margin * 2.0).clamp(1.0, 960.0);
    let height = (output.height() * 0.40)
        .clamp(220.0, 380.0)
        .min((output.height() - margin * 2.0).max(1.0));
    let x0 = output.x0 + (output.width() - width) / 2.0;
    let y0 = output.y0 + (output.height() - height - margin).max(0.0);
    Rect::new(x0, y0, x0 + width, y0 + height)
}

/// Build the keyboard widget `content` asks for: its own layout for that
/// purpose (a terminal's editing strip, an email field's `@`) and the hints
/// the field allows (a secret field learns nothing). `None` for a field
/// that takes no typing at all, which raises no keyboard on a live desktop
/// either.
#[must_use]
pub fn keyboard_for(content: ui::ime::InputContent) -> OskWidget {
    let (hints, purpose) = content.wayland();
    let mut keyboard = OskWidget::new()
        .with_purpose(osk::purpose_from_wire(purpose))
        // It is given a whole panel, not a row in a column: its bands dock
        // at the panel's bottom instead of the panel growing to fit it.
        .with_surface_height();
    keyboard.set_content_hints(hints);
    keyboard.set_visible(true);
    keyboard
}

/// Lay `keyboard` out to fill `panel` and return where its keys landed.
pub fn lay_out(keyboard: &mut OskWidget, panel: Rect, theme: &ui::theme::Theme) -> Rect {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map = HashMap::<NodeId, *mut dyn Widget>::new();
    let child = keyboard.build_layout(&mut taffy, theme, &mut node_map);
    let size = Size {
        width: Dimension::length(panel.width() as f32),
        height: Dimension::length(panel.height() as f32),
    };
    if let Ok(root) = taffy.new_with_children(
        Style {
            size,
            ..Default::default()
        },
        &[child],
    ) && taffy
        .compute_layout(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(panel.width() as f32),
                height: taffy::AvailableSpace::Definite(panel.height() as f32),
            },
        )
        .is_ok()
    {
        keyboard.apply_layout(&taffy, child, Point::new(panel.x0, panel.y0));
    }
    keyboard.get_rect()
}
