//! Labels the charts share: a ring chart's centre copy and values shortened for a label.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

fn half_chord(radius: f64, y0: f64, y1: f64) -> f64 {
    let y = y0.abs().max(y1.abs());
    (radius * radius - y * y).max(0.0).sqrt()
}

/// Paint a chart's centre copy (a value over a caption) inside the hole of
/// `radius` round `center`, at the largest role that fits the hole: the
/// value as a large title, a title or a heading; the caption under it only
/// where it fits too. Where not even the value fits at a heading, the copy
/// is left out (the chart's title and tooltip carry it). Returns the rect
/// the copy covers, for the other labels to keep clear of.
pub(crate) fn paint_centre_label(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    center: Point,
    radius: f64,
    title: &str,
    subtitle: &str,
) -> Option<Rect> {
    let (title_rect, subtitle_rect) =
        centre_label_layout(tr, theme, center, radius, title, subtitle)?;
    let (role, rect) = title_rect;
    let style = theme.text(role).strong();
    tr.draw_style(
        scene,
        title,
        theme.tokens.colors.text_primary,
        transform * Affine::translate((rect.x0, rect.y0)),
        None,
        &style,
    );
    let mut covered = rect;
    if let Some(sub) = subtitle_rect {
        tr.draw_style(
            scene,
            subtitle,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((sub.x0, sub.y0)),
            None,
            &theme.text(TextRole::Caption),
        );
        covered = covered.union(sub);
    }
    Some(covered)
}

pub(crate) fn centre_label_layout(
    tr: &mut TextRenderer,
    theme: &Theme,
    center: Point,
    radius: f64,
    title: &str,
    subtitle: &str,
) -> Option<((TextRole, Rect), Option<Rect>)> {
    if title.is_empty() || radius <= 0.0 {
        return None;
    }
    let radius = radius - f64::from(theme.tokens.spacing.s1);
    let caption = theme.text(TextRole::Caption);
    let (sw, sh) = if subtitle.is_empty() {
        (0.0, 0.0)
    } else {
        let (w, h) = tr.measure_style(subtitle, None, &caption);
        (f64::from(w), f64::from(h))
    };
    for role in [TextRole::LargeTitle, TextRole::Title, TextRole::Heading] {
        let style = theme.text(role).strong();
        let (tw, th) = tr.measure_style(title, None, &style);
        let (tw, th) = (f64::from(tw), f64::from(th));
        if sh > 0.0 {
            let top = -(th + sh) / 2.0;
            if tw / 2.0 <= half_chord(radius, top, top + th)
                && sw / 2.0 <= half_chord(radius, top + th, top + th + sh)
            {
                let t = Rect::new(
                    center.x - tw / 2.0,
                    center.y + top,
                    center.x + tw / 2.0,
                    center.y + top + th,
                );
                let s = Rect::new(center.x - sw / 2.0, t.y1, center.x + sw / 2.0, t.y1 + sh);
                return Some(((role, t), Some(s)));
            }
        }
        if tw / 2.0 <= half_chord(radius, -th / 2.0, th / 2.0) {
            let t = Rect::new(
                center.x - tw / 2.0,
                center.y - th / 2.0,
                center.x + tw / 2.0,
                center.y + th / 2.0,
            );
            return Some(((role, t), None));
        }
    }
    None
}

/// A value shortened for a label: `1.2B`, `3.4M`, `5.6K`, `42`, `0.5`.
pub(crate) fn format_compact(value: f64) -> String {
    let absolute = value.abs();
    if absolute >= 1_000_000_000.0 {
        format!("{:.1}B", value / 1_000_000_000.0)
    } else if absolute >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if absolute >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else if value.fract().abs() < 0.001 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        /// Where a chart's labels are placed.
        /// Each phase was a test of its own and builds its own fixture.
        fn chart_labels_keep_clear_of_marks_and_each_other;

        phase the_centre_copy_fits_its_hole_or_is_left_out;
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let center = Point::new(0.0, 0.0);
            for radius in [12.0, 24.0, 36.0, 60.0, 120.0] {
                if let Some(((_, title), subtitle)) =
                    centre_label_layout(&mut tr, &theme, center, radius, "126", "projects")
                {
                    for rect in std::iter::once(title).chain(subtitle) {
                        for corner in [
                            Point::new(rect.x0, rect.y0),
                            Point::new(rect.x1, rect.y0),
                            Point::new(rect.x0, rect.y1),
                            Point::new(rect.x1, rect.y1),
                        ] {
                            assert!(
                                corner.distance(center) <= radius + 0.5,
                                "{radius}: {rect:?} spills out of the hole"
                            );
                        }
                    }
                } else {
                    assert!(radius < 36.0, "{radius}: a wide hole shows its copy");
                }
            }
        }

    }
}
