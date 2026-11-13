//! The persistent control a retained row holds at its trailing edge, and how
//! it fills the row once it wraps under the text.

use super::{ListRow, MIN_TEXT_LINES};
use crate::a11y::Role;
use crate::widget::{Element, Widget};
use taffy::prelude::length;
use taffy::{NodeId, TaffyTree};
use vello::kurbo::Rect;

use crate::text_render::TextRenderer;

/// Whether `control` draws a face of one size whatever room it gets (a
/// switch, a checkbox), so stretching it under the text would only move it
/// off the text's edge. Looks through wrappers that announce nothing
/// themselves to the control they hold.
fn keeps_its_face_size(control: &dyn Widget) -> bool {
    match control.accessibility() {
        Some(node) => matches!(node.role, Role::Switch | Role::Checkbox | Role::RadioButton),
        None => match control.children().as_slice() {
            [only] => keeps_its_face_size(*only),
            _ => false,
        },
    }
}

/// The persistent control a retained row holds at its trailing edge.
pub(super) struct Trailing {
    /// Stands for the text band among the row's children: the row's node
    /// holds the band's node, then the control's, and every walk that pairs
    /// a widget's children with its layout nodes (focus order, the
    /// accessibility tree) must meet the control at the control's node.
    pub(super) band_slot: crate::widget::Absent,
    pub(super) element: Element,
    pub(super) band: Option<NodeId>,
    pub(super) node: Option<NodeId>,
    /// The text band's height, settled from measured line boxes.
    pub(super) band_height: f32,
    /// The control wrapped under the text.
    pub(super) wrapped: bool,
    /// The control's own width, while it is not filling the row.
    pub(super) natural: f32,
    /// The margins the control stands in beside the text (the gap before
    /// it and the slot after it): the row's wrap counts them, so the
    /// control stops filling only where it would fit with them.
    pub(super) margins: f32,
    /// The width the control fills under the text, from the text's edge to
    /// the row's, where a control beside the text ends.
    pub(super) fill: Option<f32>,
    /// The control's own minimum width, restored when it stops filling.
    pub(super) own_min: taffy::Dimension,
    /// Centred on the column a disclosure chevron stands in
    /// ([`ListRow::trailing_disclosure`]).
    pub(super) on_disclosure_column: bool,
}

impl ListRow {
    /// Wrapped under the text, the control fills the row from the text's
    /// edge to the row's own, where every control beside its text ends,
    /// instead of keeping its own width. It keeps filling while its own
    /// width and its margins still would not fit beside the text's
    /// narrowest. True when the control's style changed.
    pub(super) fn fill_under_text(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let (Some(theme), Some(metrics)) = (self.theme.clone(), self.metrics.as_ref()) else {
            return false;
        };
        let min_text = f64::from(MIN_TEXT_LINES) * f64::from(metrics.title.line_height_px());
        let bleed = metrics.bleed;
        let Some(trailing) = &self.trailing else {
            return false;
        };
        let (Some(band), Some(child)) = (trailing.band, trailing.node) else {
            return false;
        };
        let (Ok(row), Ok(band_layout), Ok(child_layout)) =
            (taffy.layout(node), taffy.layout(band), taffy.layout(child))
        else {
            return false;
        };
        let row_width = f64::from(row.size.width);
        let band_height = f64::from(trailing.band_height);
        let (natural, wrapped) = if trailing.fill.is_some() {
            // Filling, the control is as wide as the row and always under
            // the text: whether it would wrap at its own width is the row's
            // wrap reckoned again, exactly as the flex layout breaks the
            // line (each item's outer size added up, in its own precision,
            // past the row's width): the text band at its minimum, then the
            // control at its own width with both its margins. Any share of
            // those left out, or any slack either way, gives a width the
            // control wraps at by the one reckoning and fits at by the
            // other, and the row fills and stops filling on every pass of
            // the solve; or keeps filling where a fresh layout would not.
            let beside = crate::num::f32_of_f64(min_text) + (trailing.natural + trailing.margins);
            (
                f64::from(trailing.natural),
                beside > taffy.unrounded_layout(node).size.width,
            )
        } else {
            (
                f64::from(taffy.unrounded_layout(child).size.width),
                child_layout.location.y >= band_layout.location.y + trailing.band_height - 0.5,
            )
        };
        let fill = (wrapped && !keeps_its_face_size(trailing.element.as_ref())).then(|| {
            let band_rect = Rect::new(-bleed, 0.0, row_width + bleed, band_height);
            let m = self.metrics(&theme);
            let text_edge = self.with_face(|face| face.text_x(band_rect, &m, text, &theme));
            crate::num::f32_of_f64((row_width - text_edge.max(0.0)).max(0.0))
        });
        let Some(trailing) = &mut self.trailing else {
            return false;
        };
        if trailing.fill.is_none() {
            trailing.natural = crate::num::f32_of_f64(natural);
        }
        let same = match (trailing.fill, fill) {
            (Some(a), Some(b)) => (a - b).abs() < 0.5,
            (None, None) => true,
            _ => false,
        };
        if same {
            return false;
        }
        trailing.fill = fill;
        let own_min = trailing.own_min;
        let Ok(style) = taffy.style(child) else {
            return false;
        };
        let mut style = style.clone();
        style.min_size.width = fill.map_or(own_min, length);
        taffy.set_style(child, style).is_ok()
    }
}
