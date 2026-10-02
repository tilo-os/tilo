//! A themed toolbar: a row of labelled, groupable, toggle-able buttons. Apps
//! build a `Vec<ToolbarItem>`, call [`draw`] to render it (and get each button's
//! clickable rect), and map clicks back to their own actions — the toolkit owns
//! the look (active highlight, label cues, group separators), the app owns the
//! behaviour.
//!
//! The *width model* — leading pad, per-item gap, separator advance, label
//! size and minimum button height — is one value, [`ToolbarMetrics`], and
//! the geometry is one function, [`item_rects`]; [`draw`] paints exactly the
//! rects it returns. A host that packs items into rows itself (the word
//! processor wraps its toolbar on a narrow window) measures with the same
//! model instead of restating `14 / 6 / 34` beside its own packer.

use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;

/// Standard toolbar height, so callers can reserve space consistently.
pub const TOOLBAR_HEIGHT: f64 = 50.0;

/// The toolbar's width model. [`ToolbarMetrics::standard`] is what
/// [`draw`] lays out with; a host that packs rows itself reads the same
/// numbers from here so a row its packer admits is one `draw` lays out
/// without clipping.
///
/// These are the toolbar's long-standing values, kept identical while the
/// hosts move onto the shared model; where they sit against the spacing
/// scale: `leading_pad` 14 and `separator_advance` 14 are off-grid
/// (between `s3` 12 and `s4` 16), `item_gap` 6 is off-grid, `button_min_height`
/// 34 matches the modal action button, `label_size` 15 sits between
/// `scale.base` 14 and `scale.lg` 16.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToolbarMetrics {
    pub row_height: f64,
    pub left_padding: f64,
    pub group_gap: f64,
    /// Inset before the first item.
    pub leading_pad: f64,
    /// Horizontal advance after every item.
    pub item_gap: f64,
    /// Extra advance a group separator adds before a `group_start` item.
    pub separator_advance: f64,
    /// The separator line's inset from the start of its advance, its width
    /// and its half-height around the row's centre line.
    pub separator_inset: f64,
    pub separator_width: f64,
    pub separator_half_height: f64,
    /// A button is at least this tall; a taller label grows it by
    /// `label_pad_y` around the measured line.
    pub button_min_height: f64,
    pub label_pad_y: f64,
    /// Renderer-relative label size.
    pub label_size: f32,
}

impl ToolbarMetrics {
    /// The model [`draw`] uses.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            row_height: TOOLBAR_HEIGHT,
            left_padding: 14.0,
            group_gap: 14.0,
            leading_pad: 14.0,
            item_gap: 6.0,
            separator_advance: 14.0,
            separator_inset: 2.0,
            separator_width: 1.0,
            separator_half_height: 12.0,
            button_min_height: 34.0,
            label_pad_y: 12.0,
            label_size: 15.0,
        }
    }

    /// Horizontal room one item takes in the row: its separator when it
    /// starts a group, its width, and the gap after it. Summing this over a
    /// row plus `leading_pad` is the row's used width — the quantity a
    /// packer compares against the space it has.
    #[must_use]
    pub fn item_advance(&self, width: f64, group_start: bool) -> f64 {
        let separator = if group_start {
            self.separator_advance
        } else {
            0.0
        };
        separator + width + self.item_gap
    }

    /// Font weight a label is measured and drawn with for its cue.
    #[must_use]
    pub fn label_weight(cue: LabelCue) -> TextWeight {
        if cue == LabelCue::Bold {
            TextWeight::Bold
        } else {
            TextWeight::Regular
        }
    }

    /// Height of the button for a label that measures `label_height` tall.
    #[must_use]
    pub fn button_height(&self, label_height: f64) -> f64 {
        self.button_min_height.max(label_height + self.label_pad_y)
    }
}

/// Resolved row and spacing metrics for a system text scale.
#[must_use]
pub fn metrics(text_scale: f32) -> ToolbarMetrics {
    let scale = f64::from(text_scale.clamp(0.75, 2.0));
    let mut metrics = ToolbarMetrics::standard();
    metrics.row_height *= scale;
    metrics.leading_pad *= scale;
    metrics.left_padding = metrics.leading_pad;
    metrics.item_gap *= scale;
    metrics.separator_advance *= scale;
    metrics.group_gap = metrics.separator_advance;
    metrics.separator_inset *= scale;
    metrics.separator_width *= scale;
    metrics.separator_half_height *= scale;
    metrics.button_min_height *= scale;
    metrics.label_pad_y *= scale;
    metrics
}

impl Default for ToolbarMetrics {
    fn default() -> Self {
        Self::standard()
    }
}

/// Where a group separator is painted for an item whose advance starts at
/// `x`, centred on `cy`.
fn separator_rect(metrics: &ToolbarMetrics, x: f64, cy: f64) -> Rect {
    Rect::new(
        x + metrics.separator_inset,
        cy - metrics.separator_half_height,
        x + metrics.separator_inset + metrics.separator_width,
        cy + metrics.separator_half_height,
    )
}

/// One laid-out item: its button rect and its label's measured size, so
/// the paint pass never shapes a label the layout pass already shaped.
struct ItemLayout {
    rect: Rect,
    label_w: f64,
    label_h: f64,
}

/// Lay `items` out left to right in `rect` with `metrics`, measuring each
/// label once at `metrics.label_size` in its cue's weight.
fn lay_out(
    rect: Rect,
    items: &[ToolbarItem],
    tr: &mut TextRenderer,
    metrics: &ToolbarMetrics,
) -> Vec<ItemLayout> {
    let cy = rect.y0 + rect.height() / 2.0;
    let mut x = rect.x0 + metrics.leading_pad;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if item.group_start {
            x += metrics.separator_advance;
        }
        let weight = ToolbarMetrics::label_weight(item.cue);
        let (lw, lh) = tr.measure_weighted(&item.label, metrics.label_size, None, weight);
        let bh = metrics
            .button_height(f64::from(lh))
            .min(rect.height().max(f64::from(lh)));
        out.push(ItemLayout {
            rect: Rect::new(x, cy - bh / 2.0, x + item.width, cy + bh / 2.0),
            label_w: f64::from(lw),
            label_h: f64::from(lh),
        });
        x += item.width + metrics.item_gap;
    }
    out
}

/// Each item's clickable rect in `rect`, laid out left to right with
/// `metrics`: the geometry half of [`draw`], with nothing painted, so a
/// host can hit-test or pack rows against the exact rects a later `draw`
/// paints. Labels are measured at `metrics.label_size` in their cue's
/// weight, which is what sets a button's height.
pub fn item_rects(
    rect: Rect,
    items: &[ToolbarItem],
    tr: &mut TextRenderer,
    metrics: &ToolbarMetrics,
) -> Vec<Rect> {
    lay_out(rect, items, tr, metrics)
        .into_iter()
        .map(|item| item.rect)
        .collect()
}

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

/// Draw a themed toolbar in `rect` with the standard metrics; returns each
/// item's clickable rect in order — the same rects [`item_rects`] returns.
pub fn draw(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    items: &[ToolbarItem],
) -> Vec<Rect> {
    let metrics = metrics(tr.text_scale());
    let scale = metrics.row_height / TOOLBAR_HEIGHT;
    let items: Vec<_> = items
        .iter()
        .cloned()
        .map(|mut item| {
            item.width *= scale;
            item
        })
        .collect();
    draw_with_metrics(scene, transform, tr, theme, rect, &items, &metrics)
}

/// The bar role (HIG §9.1) behind a strip of controls along a window's top
/// edge: the raised surface and one derived bottom hairline.
pub fn paint_bar(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.raised,
        None,
        &rect,
    );
    let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
    if rule > 0.0 {
        scene.fill(
            Fill::NonZero,
            transform,
            crate::chrome::chrome_hairline(theme),
            None,
            &Rect::new(rect.x0, rect.y1 - rule, rect.x1, rect.y1),
        );
    }
}

/// The inset of a bar's controls from its edges: siblings' step across,
/// the tightest step above and below so the bar stays one control tall
/// (HIG §7b).
pub fn bar_padding(theme: &Theme) -> taffy::Rect<taffy::LengthPercentage> {
    let spacing = &theme.tokens.spacing;
    let across = taffy::LengthPercentage::length(spacing.s3);
    let along = taffy::LengthPercentage::length(spacing.s1);
    taffy::Rect {
        left: across,
        right: across,
        top: along,
        bottom: along,
    }
}

/// [`draw`] with an explicit width model — for a host that lays its rows
/// out with the same [`ToolbarMetrics`] it hands in here.
pub fn draw_with_metrics(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    items: &[ToolbarItem],
    metrics: &ToolbarMetrics,
) -> Vec<Rect> {
    paint_bar(scene, transform, theme, rect);

    // Ink on the accent fill comes from the theme (it walks out of white on
    // a bright accent), not from a literal white that the light and
    // high-contrast palettes cannot reach.
    let on_accent = theme
        .tokens
        .colors
        .on_accent(crate::tokens::colors::ON_FILL_MIN_CONTRAST);
    let cy = rect.y0 + rect.height() / 2.0;
    let laid = lay_out(rect, items, tr, metrics);
    for (item, laid) in items.iter().zip(&laid) {
        let r = laid.rect;
        if item.group_start {
            // The separator sits in the advance before the item.
            let sep = separator_rect(metrics, r.x0 - metrics.separator_advance, cy);
            scene.fill(
                Fill::NonZero,
                transform,
                crate::chrome::chrome_hairline(theme),
                None,
                &sep,
            );
        }
        let weight = ToolbarMetrics::label_weight(item.cue);
        if item.active {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.accent_primary,
                None,
                &RoundedRect::from_rect(r, f64::from(theme.tokens.radii.md)),
            );
        }
        let color = if item.active {
            on_accent
        } else {
            theme.text_primary
        };
        let lx = r.x0 + (item.width - laid.label_w) / 2.0;
        let ly = cy - laid.label_h / 2.0;
        tr.draw_text_weighted(
            scene,
            &item.label,
            metrics.label_size,
            color,
            translated(transform, lx, ly),
            None,
            weight,
        );
        let cue_thickness = 1.5 * f64::from(tr.text_scale());
        match item.cue {
            LabelCue::Underline => {
                // The underline hugs the measured line box; the strike crosses its middle.
                let uy = ly + laid.label_h - cue_thickness;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    color,
                    None,
                    &Rect::new(lx, uy, lx + laid.label_w, uy + cue_thickness),
                );
            }
            LabelCue::Strike => {
                let sy = ly + laid.label_h / 2.0 - cue_thickness / 2.0;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    color,
                    None,
                    &Rect::new(lx, sy, lx + laid.label_w, sy + cue_thickness),
                );
            }
            _ => {}
        }
    }
    laid.into_iter().map(|item| item.rect).collect()
}

/// Index of the button whose rect contains `point`, if any.
pub fn hit(rects: &[Rect], point: Point) -> Option<usize> {
    rects.iter().position(|r| r.contains(point))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_item_rects_fit_labels_match_paint_and_scale_hit_targets() {
        // Case `scaled_toolbar_labels_fit_inside_their_hit_rects_and_row`.
        {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            let mut scene = Scene::new();
            let items = [
                ToolbarItem::new("More", 100.0),
                ToolbarItem::new("Bold", 100.0).cue(LabelCue::Bold),
                ToolbarItem::new("U", 60.0).cue(LabelCue::Underline),
                ToolbarItem::new("S", 60.0).cue(LabelCue::Strike),
            ];
            let row = Rect::new(20.0, 40.0, 440.0, 104.0);
            let rects = draw(&mut scene, Affine::IDENTITY, &mut text, &theme, row, &items);
            for (item, button) in items.iter().zip(&rects) {
                let weight = if item.cue == LabelCue::Bold {
                    TextWeight::Bold
                } else {
                    TextWeight::Regular
                };
                let (_, height) = text.measure_weighted(&item.label, 15.0, None, weight);
                assert!(
                    button.height() >= f64::from(height),
                    "{} label ({height}px) overflows its {}px hit target",
                    item.label,
                    button.height()
                );
                assert!(
                    button.y0 >= row.y0 && button.y1 <= row.y1,
                    "scaled button must stay inside its allocated toolbar row"
                );
            }
        }

        // Case `item_rects_are_what_draw_paints_and_follow_the_advance_model`:
        // The geometry `draw` paints is the geometry `item_rects` returns, and
        // both follow the advance model a row packer sums: leading pad, then
        // per item its separator (when it starts a group), its width and the
        // gap. A host packing rows with `item_advance` therefore admits
        // exactly the rows `draw` lays out.
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let m = ToolbarMetrics::standard();
            let items = vec![
                ToolbarItem::new("B", 34.0).cue(LabelCue::Bold),
                ToolbarItem::new("I", 30.0),
                ToolbarItem::new("H1", 42.0).group_start(true),
                ToolbarItem::new("Quote", 60.0),
            ];
            let rect = Rect::new(10.0, 20.0, 610.0, 20.0 + TOOLBAR_HEIGHT);
            let laid = item_rects(rect, &items, &mut tr, &m);
            let drawn = draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, rect, &items);
            assert_eq!(laid, drawn, "draw paints the rects item_rects lays out");

            assert!((laid[0].x0 - (rect.x0 + m.leading_pad)).abs() < 1e-9);
            let mut x = rect.x0 + m.leading_pad;
            for (item, r) in items.iter().zip(&laid) {
                let sep = if item.group_start {
                    m.separator_advance
                } else {
                    0.0
                };
                assert!(
                    (r.x0 - (x + sep)).abs() < 1e-9,
                    "{} starts after its separator",
                    item.label
                );
                assert!((r.width() - item.width).abs() < 1e-9);
                assert!(
                    (r.height() - m.button_min_height).abs() < 1e-9,
                    "1× labels fit the minimum"
                );
                assert!(
                    (r.center().y - rect.center().y).abs() < 1e-9,
                    "buttons centre on the row"
                );
                x += m.item_advance(item.width, item.group_start);
            }
            // The used width the packer sums is where the next item would start.
            assert!((x - (laid[3].x1 + m.item_gap)).abs() < 1e-9);
            // The literal model the hosts mirrored: 14 / 6 / 34 / 15.
            assert_eq!(
                (
                    m.leading_pad,
                    m.item_gap,
                    m.separator_advance,
                    m.button_min_height
                ),
                (14.0, 6.0, 14.0, 34.0)
            );
            assert!((m.label_size - 15.0).abs() < f32::EPSILON);
        }

        // Case `draw_returns_a_rect_per_item_and_hit_tests`.
        {
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

        // Case `two_x_geometry_scales_hit_targets_without_changing_item_order`.
        {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let mut scene = Scene::new();
            let items = vec![
                ToolbarItem::new("New", 64.0),
                ToolbarItem::new("B", 34.0).group_start(true),
            ];
            let m = metrics(2.0);
            let rect = Rect::new(0.0, 0.0, 500.0, m.row_height);
            let rects = draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, rect, &items);

            assert_eq!(m.row_height, TOOLBAR_HEIGHT * 2.0);
            assert_eq!(rects[0].width(), 128.0);
            assert_eq!(rects[0].height(), 68.0);
            assert!(rects[1].x0 - rects[0].x1 >= m.group_gap);
            assert_eq!(hit(&rects, rects[0].center()), Some(0));
            assert_eq!(hit(&rects, rects[1].center()), Some(1));
        }
    }
}
