//! Laying one widget out alone, for a test that reads what it draws or says
//! before any window hosts it.

use std::collections::HashMap;

use crate::taffy::{self, NodeId, TaffyTree};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Lay `widget` out in a space `width` by `height` points, as a window of that size
/// would, and give it that layout. When the widget's own layout cannot be solved.
pub fn lay_out(
    widget: &mut dyn Widget,
    width: f32,
    height: f32,
) -> Result<(), crate::widget::WidgetLayoutError> {
    lay_out_in(widget, width, height, &Theme::dark())
}

/// [`lay_out`] for a test that states the theme it draws with. When the widget's own
/// layout cannot be solved.
pub fn lay_out_in(
    widget: &mut dyn Widget,
    width: f32,
    height: f32,
    theme: &Theme,
) -> Result<(), crate::widget::WidgetLayoutError> {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let node = widget.build_layout(&mut taffy, theme, &mut node_map);
    let available = taffy::Size {
        width: taffy::AvailableSpace::Definite(width),
        height: taffy::AvailableSpace::Definite(height),
    };
    // SAFETY: the tree, its node map and the text renderer above stay exclusively borrowed.
    unsafe {
        crate::widget::solve_built_layout(widget, &mut taffy, node, &node_map, &mut text, available)
    }?;
    widget.apply_layout(&taffy, node, crate::kurbo::Point::ZERO);
    Ok(())
}
