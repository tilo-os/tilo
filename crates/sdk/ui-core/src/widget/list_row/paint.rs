//! Painting a short classifying code on its pill: the one badge a list row and a navigation row
//! share.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::RowMetrics;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

pub(super) fn paint_badge(ctx: &mut PaintCtx<'_>, _m: &RowMetrics, column: Rect, badge: &str) {
    paint_code_badge(ctx.scene, ctx.transform, ctx.text, ctx.theme, column, badge);
}

/// A short classifying code (`GET`, `.rs`) on its quiet pill at the leading
/// edge of `column`, centred on the column's line: the one badge a list
/// row and a navigation row share.
pub(crate) fn paint_code_badge(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    column: Rect,
    badge: &str,
) {
    let style = RowMetrics::new(theme).badge;
    let (width, height) = text.measure_style(badge, None, &style);
    let pad = f64::from(theme.tokens.spacing.s1);
    let pill = Rect::from_origin_size(
        (
            column.x0,
            column.center().y - f64::from(height) / 2.0 - pad / 2.0,
        ),
        (f64::from(width) + 2.0 * pad, f64::from(height) + pad),
    );
    let raised = theme.tokens.colors.surfaces.raised;
    crate::surface::card(
        scene,
        transform,
        pill,
        f64::from(theme.tokens.radii.detail),
        raised,
    );
    text.draw_style(
        scene,
        badge,
        theme.text_dim,
        transform.pre_translate(vello::kurbo::Vec2::new(pill.x0 + pad, pill.y0 + pad / 2.0)),
        None,
        &style,
    );
}
