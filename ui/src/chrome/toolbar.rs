//! A themed toolbar: a row of labelled, groupable, toggle-able buttons. Apps
//! build a `Vec<ToolbarItem>`, call [`draw`] to render it (and get each button's
//! clickable rect), and map clicks back to their own actions — the toolkit owns
//! the look (active highlight, label cues, group separators), the app owns the
//! behaviour.

use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

/// Standard toolbar height, so callers can reserve space consistently.
pub const TOOLBAR_HEIGHT: f64 = 50.0;

/// `kurbo::Affine` composition is plain f64 matrix math with no panic
/// path; `arithmetic_side_effects` flags the `*` operator overload anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine composition is f64 matrix math; it never panics \
              or overflows, unlike integer +/-/*"
)]
fn translated(transform: Affine, dx: f64, dy: f64) -> Affine {
    transform * Affine::translate((dx, dy))
}

/// A decoration drawn on (and font weight applied to) a button's label, so a
/// formatting button reads as what it does (Bold = bold "B", etc.).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum LabelCue {
    #[default]
    None,
    Bold,
    Underline,
    Strike,
}

#[derive(Clone)]
pub struct ToolbarItem {
    pub label: String,
    pub width: f64,
    pub active: bool,
    pub cue: LabelCue,
    /// Draw a group separator immediately before this item.
    pub group_start: bool,
}

impl ToolbarItem {
    pub fn new(label: impl Into<String>, width: f64) -> Self {
        Self {
            label: label.into(),
            width,
            active: false,
            cue: LabelCue::None,
            group_start: false,
        }
    }
    #[must_use]
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
    #[must_use]
    pub fn cue(mut self, cue: LabelCue) -> Self {
        self.cue = cue;
        self
    }
    #[must_use]
    pub fn group_start(mut self, group_start: bool) -> Self {
        self.group_start = group_start;
        self
    }
}

/// Draw a themed toolbar in `rect`; returns each item's clickable rect in order.
pub fn draw(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    items: &[ToolbarItem],
) -> Vec<Rect> {
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.raised,
        None,
        &rect,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.border_standard,
        None,
        &Rect::new(rect.x0, rect.y1 - 1.0, rect.x1, rect.y1),
    );

    let white = Color::from_rgb8(0xff, 0xff, 0xff);
    let cy = rect.y0 + rect.height() / 2.0;
    let bh = 34.0;
    let mut x = rect.x0 + 14.0;
    let mut rects = Vec::with_capacity(items.len());
    for item in items {
        if item.group_start {
            let sep = Rect::new(x + 2.0, cy - 12.0, x + 3.0, cy + 12.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.border_standard,
                None,
                &sep,
            );
            x += 14.0;
        }
        let r = Rect::new(x, cy - bh / 2.0, x + item.width, cy + bh / 2.0);
        if item.active {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.accent_primary,
                None,
                &RoundedRect::from_rect(r, 6.0),
            );
        }
        let color = if item.active {
            white
        } else {
            theme.text_primary
        };
        let (lw, _) = tr.measure(&item.label, 15.0, None);
        let lx = x + (item.width - f64::from(lw)) / 2.0;
        let ly = cy - 9.0;
        if item.cue == LabelCue::Bold {
            tr.draw_text_weighted(
                scene,
                &item.label,
                15.0,
                color,
                translated(transform, lx, ly),
                None,
                TextWeight::Bold,
            );
        } else {
            tr.draw_text(
                scene,
                &item.label,
                15.0,
                color,
                translated(transform, lx, ly),
                None,
            );
        }
        match item.cue {
            LabelCue::Underline => {
                let uy = ly + 18.0;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    color,
                    None,
                    &Rect::new(lx, uy, lx + f64::from(lw), uy + 1.5),
                );
            }
            LabelCue::Strike => {
                let sy = ly + 9.0;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    color,
                    None,
                    &Rect::new(lx, sy, lx + f64::from(lw), sy + 1.5),
                );
            }
            _ => {}
        }
        rects.push(r);
        x += item.width + 6.0;
    }
    rects
}

/// Index of the button whose rect contains `point`, if any.
pub fn hit(rects: &[Rect], point: Point) -> Option<usize> {
    rects.iter().position(|r| r.contains(point))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_returns_a_rect_per_item_and_hit_tests() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let items = vec![
            ToolbarItem::new("B", 34.0).active(true).cue(LabelCue::Bold),
            ToolbarItem::new("I", 30.0),
            ToolbarItem::new("H1", 42.0).group_start(true),
        ];
        let rect = Rect::new(0.0, 0.0, 600.0, TOOLBAR_HEIGHT);
        let rects = draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, rect, &items);
        assert_eq!(rects.len(), 3);
        // ascending, non-overlapping, with a separator gap before item 2
        assert!(rects[0].x1 <= rects[1].x0);
        assert!(
            rects[1].x1 + 10.0 <= rects[2].x0,
            "group separator widened the gap"
        );
        let mid = rects[1].center();
        assert_eq!(hit(&rects, mid), Some(1));
        assert_eq!(hit(&rects, Point::new(5000.0, 5.0)), None);
    }
}
