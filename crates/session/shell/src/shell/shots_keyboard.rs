//! Where the on-screen keyboard docks, and what a focused field asks of it.
//!
//! On a live desktop the keyboard is a client of its own: the compositor
//! floats its surface in [`keyboard_panel`] and `FrameBuilder` only mirrors
//! the client's pixels (`ForeignSurfaceWidget`).

use ui::widget::Widget;
use vello::kurbo::Rect;

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

/// What `widget`'s field asks an input method for: a focused field's content
/// if it has one, else the first field it holds — the one a finger would
/// reach first. Read off the widget tree the way the keyboard app reads the
/// purpose off the wire.
#[must_use]
pub fn field_content(widget: &dyn Widget) -> Option<ui::ime::InputContent> {
    let (mut first, mut focused) = (None, None);
    walk_for_fields(widget, &mut first, &mut focused);
    focused.or(first)
}

fn walk_for_fields(
    widget: &dyn Widget,
    first: &mut Option<ui::ime::InputContent>,
    focused: &mut Option<ui::ime::InputContent>,
) {
    if let Some(content) = widget.ime_content() {
        if widget.is_focused() && focused.is_none() {
            *focused = Some(content);
        }
        if first.is_none() {
            *first = Some(content);
        }
    }
    for child in widget.children() {
        walk_for_fields(child, first, focused);
    }
}
