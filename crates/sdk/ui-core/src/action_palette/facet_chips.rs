//! The palette's facet chips: one row of each facet's name and its values, as many as fit, drawn
//! selected where the query filters by them.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::{ActionPalette, FACET_H};
use crate::search::FacetFilter;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

impl ActionPalette {
    /// One row of facet value chips, in facet order, as many as fit.
    pub(super) fn layout_facet_chips(
        &mut self,
        theme: &Theme,
        x0: f64,
        top: f64,
        width: f64,
        text: &mut TextRenderer,
    ) {
        self.facet_chips.clear();
        self.facet_labels.clear();
        if self.arg.is_some() || self.confirm.is_some() {
            return;
        }
        let gap = f64::from(theme.tokens.spacing.s2);
        let label_style = theme.text(crate::tokens::TextRole::Caption);
        let mut x = x0;
        for facet in &self.facets {
            let (label_w, label_h) = text.measure_style(&facet.label, None, &label_style);
            let label_w = f64::from(label_w);
            if x + label_w > x0 + width {
                return;
            }
            self.facet_labels.push((
                Point::new(
                    x,
                    crate::text_metrics::centered_text_top(top, FACET_H, f64::from(label_h)),
                ),
                facet.label.clone(),
            ));
            x += label_w + gap;
            for value in &facet.values {
                let label = value.label.clone();
                let w = crate::widget::ChipFace::intrinsic_width(
                    &label,
                    crate::widget::ChipLead::None,
                    false,
                    text,
                    theme,
                );
                if x + w > x0 + width {
                    return;
                }
                self.facet_chips.push((
                    Rect::new(x, top, x + w, top + FACET_H),
                    FacetFilter::new(facet.key.clone(), value.value.clone()),
                    label,
                ));
                x += w + gap;
            }
            x += gap;
        }
    }

    pub(super) fn draw_facet_chips(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let label_style = theme.text(crate::tokens::TextRole::Caption);
        for (at, label) in &self.facet_labels {
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix math and cannot overflow"
            )]
            let placed = transform * Affine::translate((at.x, at.y));
            text.draw_style(scene, label, theme.text_dim, placed, None, &label_style);
        }
        let applied = self.query.filters();
        for (rect, filter, label) in &self.facet_chips {
            let on = applied.contains(filter);
            // Selected is the lift: one step of ink, never a hue.
            crate::widget::ChipFace::new(*rect, label)
                .selected(on)
                .paint(scene, transform, text, theme);
        }
    }

    pub(super) fn facet_chip_at(&self, point: Point) -> Option<FacetFilter> {
        self.facet_chips
            .iter()
            .find(|(rect, ..)| rect.contains(point))
            .map(|(_, filter, _)| filter.clone())
    }
}
