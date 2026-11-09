//! Laying one widget out alone, for a test that reads what it draws or says
//! before any window hosts it.

use std::collections::HashMap;

use crate::taffy::{self, NodeId, TaffyTree};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Lay `widget` out in a space `width` by `height` points, as a window of
/// that size would, and give it that layout. The rows of a list, the parts
/// of a card and the text a widget measures exist only once it is laid out:
/// a test that asks such a widget what assistive technology hears, or where
/// its parts stand, lays it out first.
///
/// # Errors
/// When the widget's own layout cannot be solved.
pub fn lay_out(widget: &mut dyn Widget, width: f32, height: f32) -> Result<(), taffy::TaffyError> {
    lay_out_in(widget, width, height, &Theme::dark())
}

/// [`lay_out`] for a test that states the theme it draws with.
///
/// # Errors
/// When the widget's own layout cannot be solved.
pub fn lay_out_in(
    widget: &mut dyn Widget,
    width: f32,
    height: f32,
    theme: &Theme,
) -> Result<(), taffy::TaffyError> {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let node = widget.build_layout(&mut taffy, theme, &mut node_map);
    taffy.compute_layout_with_measure(
        node,
        taffy::Size {
            width: taffy::AvailableSpace::Definite(width),
            height: taffy::AvailableSpace::Definite(height),
        },
        |known, available, id, _, _| {
            node_map.get(&id).map_or(taffy::Size::ZERO, |&part| unsafe {
                (*part).measure(known, available, &mut text)
            })
        },
    )?;
    widget.apply_layout(&taffy, node, crate::kurbo::Point::ZERO);
    Ok(())
}
