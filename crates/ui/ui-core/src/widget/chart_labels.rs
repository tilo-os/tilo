//! Collision-free label layout shared by the charts.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;

/// The labels already placed in one chart, and the bounds every label must
/// stay inside.
#[derive(Clone, Debug)]
pub(crate) struct LabelPlacer {
    bounds: Rect,
    placed: Vec<Rect>,
    air: f64,
}

impl LabelPlacer {
    pub(crate) fn new(bounds: Rect, theme: &Theme) -> Self {
        Self {
            bounds,
            placed: Vec::new(),
            air: f64::from(theme.tokens.spacing.s1),
        }
    }

    /// Something a label may not cover (a node, the chart's centre copy).
    pub(crate) fn block(&mut self, rect: Rect) {
        self.placed.push(rect);
    }

    pub(crate) fn fits(&self, rect: Rect) -> bool {
        let inside = rect.x0 >= self.bounds.x0 - 0.5
            && rect.x1 <= self.bounds.x1 + 0.5
            && rect.y0 >= self.bounds.y0 - 0.5
            && rect.y1 <= self.bounds.y1 + 0.5;
        inside
            && self.placed.iter().all(|other| {
                rect.inflate(self.air / 2.0, self.air / 2.0)
                    .intersect(other.inflate(self.air / 2.0, self.air / 2.0))
                    .is_zero_area()
            })
    }

    pub(crate) fn place(&mut self, rect: Rect) -> bool {
        if self.fits(rect) {
            self.placed.push(rect);
            true
        } else {
            false
        }
    }
}

/// The smallest width a label is shortened into: three characters' worth.
/// Below it the label is dropped rather than shown as a stub.
pub(crate) fn min_label_width(tr: &mut TextRenderer, style: &TextStyle) -> f64 {
    3.0 * f64::from(tr.measure_style("M", None, style).0)
}

/// `label` shortened with an ellipsis to `spare`, or `None` where not even
/// [`min_label_width`] is left or nothing but the ellipsis would show.
pub(crate) fn fit_label(
    tr: &mut TextRenderer,
    label: &str,
    spare: f64,
    style: &TextStyle,
) -> Option<(String, f64, f64)> {
    if label.is_empty() || spare < min_label_width(tr, style) {
        return None;
    }
    let shown = tr.elide_to_width_style(label, spare as f32, style);
    if shown.is_empty() || shown == "…" {
        return None;
    }
    let (w, h) = tr.measure_style(&shown, None, style);
    Some((shown, f64::from(w), f64::from(h)))
}

/// A label of `size` outside a ring of `radius` round `center`, at `angle`:
/// on the right half it starts at the ring, on the left half it ends there,
/// and it is centred over and under the ring at the top and bottom, so it
/// never covers the ring it names.
pub(crate) fn radial_label_rect(center: Point, radius: f64, angle: f64, size: (f64, f64)) -> Rect {
    let (w, h) = size;
    let (sin, cos) = angle.sin_cos();
    let anchor = Point::new(center.x + radius * cos, center.y + radius * sin);
    let x0 = if cos > AXIS {
        anchor.x
    } else if cos < -AXIS {
        anchor.x - w
    } else {
        anchor.x - w / 2.0
    };
    let y0 = if sin > AXIS {
        anchor.y
    } else if sin < -AXIS {
        anchor.y - h
    } else {
        anchor.y - h / 2.0
    };
    Rect::new(x0, y0, x0 + w, y0 + h)
}

const AXIS: f64 = 0.15;

pub(crate) fn radial_extent(center: Point, radius: f64, angle: f64, bounds: Rect) -> f64 {
    let cos = angle.cos();
    let x = center.x + radius * cos;
    if cos > AXIS {
        bounds.x1 - x
    } else if cos < -AXIS {
        x - bounds.x0
    } else {
        2.0 * (x - bounds.x0).min(bounds.x1 - x)
    }
}

fn half_chord(radius: f64, y0: f64, y1: f64) -> f64 {
    let y = y0.abs().max(y1.abs());
    (radius * radius - y * y).max(0.0).sqrt()
}

/// A chart's centre copy, placed in its hole: the words, the role they take and the rect they
/// cover, with the caption's rect under them where it fits too.
pub(crate) struct CentreLabel {
    pub(crate) role: TextRole,
    pub(crate) text: String,
    pub(crate) rect: Rect,
    pub(crate) subtitle: Option<Rect>,
}

/// Paint a chart's centre copy (a value over a caption) inside the hole of
/// `radius` round `center`, at the largest role that fits the hole: the
/// value as a large title, a title, a heading or a smaller line; the caption
/// under it only where it fits too. Where not even a short line fits, it is
/// shortened into the hole rather than dropped, so a narrow chart still says
/// its total. Returns the rect the copy covers, for the other labels to keep
/// clear of.
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
    let label = centre_label_layout(tr, theme, center, radius, title, subtitle)?;
    let style = theme.text(label.role).strong();
    tr.draw_style(
        scene,
        &label.text,
        theme.tokens.colors.text_primary,
        transform * Affine::translate((label.rect.x0, label.rect.y0)),
        None,
        &style,
    );
    let mut covered = label.rect;
    if let Some(sub) = label.subtitle {
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
) -> Option<CentreLabel> {
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
    for role in [
        TextRole::LargeTitle,
        TextRole::Title,
        TextRole::Heading,
        TextRole::Body,
        TextRole::Label,
        TextRole::Caption,
    ] {
        let style = theme.text(role).strong();
        let (tw, th) = tr.measure_style(title, None, &style);
        let (tw, th) = (f64::from(tw), f64::from(th));
        let centred = Rect::new(
            center.x - tw / 2.0,
            center.y - th / 2.0,
            center.x + tw / 2.0,
            center.y + th / 2.0,
        );
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
                return Some(CentreLabel {
                    role,
                    text: title.to_owned(),
                    rect: t,
                    subtitle: Some(s),
                });
            }
        }
        if tw / 2.0 <= half_chord(radius, -th / 2.0, th / 2.0) {
            return Some(CentreLabel {
                role,
                text: title.to_owned(),
                rect: centred,
                subtitle: None,
            });
        }
    }
    // No role holds the whole line: shorten it into the hole at the largest role that still
    // shows more than an ellipsis, so a small chart says as much of its total as it can.
    for role in [
        TextRole::Title,
        TextRole::Heading,
        TextRole::Body,
        TextRole::Label,
        TextRole::Caption,
    ] {
        let style = theme.text(role).strong();
        let line = f64::from(style.line_height_px());
        let room = 2.0 * half_chord(radius, -line / 2.0, line / 2.0);
        let Some((text, tw, th)) = fit_label(tr, title, room, &style) else {
            continue;
        };
        let rect = Rect::new(
            center.x - tw / 2.0,
            center.y - th / 2.0,
            center.x + tw / 2.0,
            center.y + th / 2.0,
        );
        return Some(CentreLabel {
            role,
            text,
            rect,
            subtitle: None,
        });
    }
    None
}

pub(crate) fn ground_ink(theme: &Theme) -> Color {
    theme.tokens.colors.text_dim
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
