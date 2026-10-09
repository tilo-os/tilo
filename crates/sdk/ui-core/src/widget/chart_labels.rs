//! Collision-free label layout shared by the charts.
//!
//! A chart label is placed where it reads (outside a ring, beside a node,
//! inside a sector), shortened with an ellipsis to the room it has there,
//! and dropped when even a short form does not fit or it would collide with
//! a label already placed. A dropped label is never lost: every chart names
//! the item in its tooltip and its accessibility node. Labels are placed in
//! priority order (largest item first), so the labels that survive at a
//! small size are the ones that matter most.

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
    /// Air kept between two labels and between a label and an obstacle.
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

    /// Whether `rect` is inside the bounds and clear of everything placed.
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

    /// Place `rect` if it [`fits`](Self::fits); returns whether it did.
    pub(crate) fn place(&mut self, rect: Rect) -> bool {
        if self.fits(rect) {
            self.placed.push(rect);
            true
        } else {
            false
        }
    }
}

/// The smallest room a label is shortened into: three characters' worth.
/// Below it the label is dropped rather than shown as a stub.
pub(crate) fn min_room(tr: &mut TextRenderer, style: &TextStyle) -> f64 {
    3.0 * f64::from(tr.measure_style("M", None, style).0)
}

/// `label` shortened with an ellipsis to `room`, or `None` where not even
/// [`min_room`] is left or nothing but the ellipsis would show.
pub(crate) fn fit_label(
    tr: &mut TextRenderer,
    label: &str,
    room: f64,
    style: &TextStyle,
) -> Option<(String, f64, f64)> {
    if label.is_empty() || room < min_room(tr, style) {
        return None;
    }
    let shown = tr.elide_to_width_style(label, room as f32, style);
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
    // The box's corner nearest the centre sits on the anchor: start-aligned
    // on the right, end-aligned on the left, centred straight above and
    // below; likewise vertically.
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

/// How close to an axis a radial label's angle counts as on it.
const AXIS: f64 = 0.15;

/// The room a radial label has at `angle` before it leaves `bounds`.
pub(crate) fn radial_room(center: Point, radius: f64, angle: f64, bounds: Rect) -> f64 {
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

/// The half-width a line spanning `y0..y1` (relative to the centre) has
/// inside a circle of `radius`.
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

/// Where [`paint_centre_label`] puts the value (with the role it is set in)
/// and the caption; `None` when the value does not fit the hole.
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
    // Keep the copy off the ring: a step of air inside the hole.
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
        // With the caption: the pair is centred as one block.
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
        // The value alone.
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

/// The ink of a label drawn on the chart's ground.
pub(crate) fn ground_ink(theme: &Theme) -> Color {
    theme.tokens.colors.text_dim
}

/// A value shortened for a label: `1.2M`, `3.4K`, `42`, `0.5`.
pub(crate) fn format_compact(value: f64) -> String {
    if value.abs() >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value.abs() >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else if value.abs() >= 10.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radial_labels_stay_off_their_ring() {
        let center = Point::new(200.0, 200.0);
        let radius = 100.0;
        for step in 0..16 {
            let angle = f64::from(step) * std::f64::consts::TAU / 16.0;
            let rect = radial_label_rect(center, radius, angle, (60.0, 16.0));
            // The box's nearest point to the centre is at or outside the ring
            // (within rounding of the blended corner).
            let nearest = Point::new(
                center.x.clamp(rect.x0, rect.x1),
                center.y.clamp(rect.y0, rect.y1),
            );
            assert!(
                nearest.distance(center) >= radius * 0.98,
                "{angle}: {rect:?} reaches into the ring"
            );
        }
    }

    #[test]
    fn the_centre_copy_fits_its_hole_or_is_left_out() {
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
                assert!(radius < 36.0, "{radius}: a roomy hole shows its copy");
            }
        }
    }

    #[test]
    fn placed_labels_never_overlap() {
        let theme = Theme::dark();
        let mut placer = LabelPlacer::new(Rect::new(0.0, 0.0, 100.0, 100.0), &theme);
        assert!(placer.place(Rect::new(10.0, 10.0, 40.0, 20.0)));
        assert!(!placer.place(Rect::new(30.0, 15.0, 60.0, 25.0)), "overlap");
        assert!(!placer.place(Rect::new(90.0, 10.0, 120.0, 20.0)), "outside");
        assert!(placer.place(Rect::new(10.0, 30.0, 40.0, 40.0)));
    }
}
