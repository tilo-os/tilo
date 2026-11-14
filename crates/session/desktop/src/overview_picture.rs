//! The overview's picture of a workspace: every strip, top to bottom, with its windows left to
//! right in the order the strip holds them, each at its share of the size it has on the display
//! under its app's mark and its title, under the strip's name.
//! One picture for every desktop: the native shell flies its windows into these places on the
//! overview's zoom, and the browser desktop's overview panel draws the same places, wells, titles
//! and names, so both read the same way. The layout is plain geometry ([`layout_strips`]); painting
//! a window's picture is [`crate::window_switcher::draw_preview_well`], its title
//! [`draw_tile_title`], an empty strip [`draw_empty_strip`] and a strip's name [`draw_strip_name`].

use std::ops::Range;

use ui::TextRole;
use ui::num::f64_of_usize;
use ui::text_render::TextWeight;
use ui::theme::Theme;
use ui::widget::ControlSize;
use vello::kurbo::{Rect, Size};
use vello::peniko::{BlendMode, Color, Fill};

use crate::i18n::t;
use crate::window_switcher::SwitcherWindow;

/// A window is pictured at most at this share of its size on the display: a workspace of one window
/// still reads as a picture, not as the window.
pub const MAX_SCALE: f64 = 0.5;

/// Most faces drawn on one window before the picture says nothing more.
const TILE_FACES_MAX: usize = 3;

/// Spacing of the picture, from the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridMetrics {
    /// A strip's name line, with the air below it.
    pub label_h: f64,
    /// Air between a window's title and its picture below it.
    pub title_gap: f64,
    /// A window's title line: its app's mark and its title.
    pub title_lh: f64,
    /// Side of the app's mark leading a window's title.
    pub mark: f64,
    pub row_gap: f64,
    /// Air between two windows of a strip.
    pub window_gap: f64,
    /// Air between two tabs of one window, so they read as one window's.
    pub tab_gap: f64,
    /// Height of a strip's windows when the picture sits inside the panel.
    pub band: f64,
    /// Side of a strip's icon buttons.
    pub control: f64,
    /// Corner radius of a pictured window at rest.
    pub radius: f64,
    /// Corner radius of a window on its strip, where a picture starts.
    pub window_radius: f64,
}

impl GridMetrics {
    pub fn for_theme(theme: &Theme) -> Self {
        let tokens = &theme.tokens;
        let spacing = &tokens.spacing;
        let lh = |role| f64::from(tokens.typography.style(role).line_height_px());
        let control = ControlSize::Small.height(theme);
        Self {
            label_h: control.max(lh(TextRole::Label)) + f64::from(spacing.s2),
            title_gap: f64::from(spacing.s2),
            title_lh: lh(TextRole::Label).max(f64::from(tokens.metrics.icon_md)),
            mark: f64::from(tokens.metrics.icon_md),
            row_gap: f64::from(spacing.s6),
            window_gap: f64::from(spacing.s4),
            tab_gap: f64::from(spacing.s1),
            band: f64::from(spacing.s16) * 1.5,
            control,
            radius: f64::from(tokens.radii.control),
            window_radius: f64::from(ui::tokens::Radii::WINDOW),
        }
    }

    /// A window's title line over its picture, with the air below it.
    pub fn title_h(&self) -> f64 {
        self.title_gap + self.title_lh
    }
}

/// Where one strip sits in the picture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripPlace {
    /// The strip's name line, across the picture.
    pub label: Rect,
    /// Each window's picture, in strip order.
    pub wells: Vec<Rect>,
    /// Each window's title line, over its picture.
    pub titles: Vec<Rect>,
    /// The slot a strip with no windows shows instead.
    pub empty: Option<Rect>,
}

impl StripPlace {
    /// Everything the strip occupies: its name line and its windows.
    pub fn bounds(&self) -> Rect {
        self.wells
            .iter()
            .chain(&self.titles)
            .chain(&self.empty)
            .fold(self.label, |all, rect| all.union(*rect))
    }
}

/// Height of a picture of `strips` strips whose windows are `band` tall.
pub fn band_height(strips: usize, m: &GridMetrics, band: f64) -> f64 {
    if strips == 0 {
        return 0.0;
    }
    let n = f64_of_usize(strips);
    n * (m.label_h + band + m.title_h()) + (n - 1.0) * m.row_gap
}

/// One strip's windows as the layout reads them: their sizes on the display, and which of them are
/// tabs of the window before.
struct Windows<'a> {
    sizes: &'a [Size],
    joined: &'a [bool],
}

impl Windows<'_> {
    /// Air before window `wi` : a tab's against its window, a window's apart.
    fn gap(&self, wi: usize, m: &GridMetrics) -> f64 {
        if self.joined.get(wi).copied().unwrap_or(false) {
            m.tab_gap
        } else {
            m.window_gap
        }
    }

    /// Width of windows `line` side by side at `scale`, and the height of the tallest.
    fn extent(&self, line: &Range<usize>, scale: f64, m: &GridMetrics) -> (f64, f64) {
        let mut extent = (0.0, 0.0_f64);
        for (wi, size) in self
            .sizes
            .iter()
            .enumerate()
            .take(line.end)
            .skip(line.start)
        {
            if wi > line.start {
                extent.0 += self.gap(wi, m);
            }
            extent.0 += size.width * scale;
            extent.1 = extent.1.max(size.height * scale);
        }
        extent
    }

    /// The strip at `scale` across `width`: as many windows on each line as fit, a window's tabs
    /// never parted, each line as tall as its tallest window. A strip with no windows takes a line
    /// `empty` tall.
    fn wrapped(&self, scale: f64, width: f64, empty: f64, m: &GridMetrics) -> Placed {
        let mut lines: Vec<Range<usize>> = Vec::new();
        for wi in 0..self.sizes.len() {
            let next = wi.saturating_add(1);
            match lines.last_mut() {
                Some(line)
                    if self.gap(wi, m) < m.window_gap
                        || self.extent(&(line.start..next), scale, m).0 <= width =>
                {
                    line.end = next;
                }
                _ => lines.push(wi..next),
            }
        }
        let lines = if lines.is_empty() {
            vec![(0..0, empty)]
        } else {
            lines
                .into_iter()
                .map(|line| {
                    let tall = self.extent(&line, scale, m).1;
                    (line, tall)
                })
                .collect()
        };
        Placed { scale, lines }
    }

    /// The strip as one line `band` tall across `width`, narrower when it is too wide.
    fn banded(&self, band: f64, width: f64, empty: f64, m: &GridMetrics) -> Placed {
        let whole = 0..self.sizes.len();
        let (gaps, _) = self.extent(&whole, 0.0, m);
        let (widths, tallest) = self.extent(&whole, 1.0, m);
        let tallest = if tallest > 0.0 { tallest } else { empty };
        let narrower = if widths > gaps {
            (width - gaps).max(0.0) / (widths - gaps)
        } else {
            f64::INFINITY
        };
        Placed {
            scale: (band / tallest).min(narrower),
            lines: vec![(whole, band)],
        }
    }
}

/// How one strip stands in the picture: its windows' scale, and its lines with their heights.
struct Placed {
    scale: f64,
    lines: Vec<(Range<usize>, f64)>,
}

/// Height of the picture of `placed` : each strip's name, and its lines under their titles.
fn picture_height(placed: &[Placed], m: &GridMetrics) -> f64 {
    let count = f64_of_usize(placed.len());
    count * m.label_h
        + (count - 1.0) * m.row_gap
        + placed
            .iter()
            .map(|strip| {
                let gaps = f64_of_usize(strip.lines.len().saturating_sub(1)) * m.window_gap;
                gaps + strip
                    .lines
                    .iter()
                    .map(|(_, tall)| tall + m.title_h())
                    .sum::<f64>()
            })
            .sum::<f64>()
}

/// Width of the widest line of `placed` ; a strip with no windows shows a slot 16:10.
fn picture_width(strips: &[Windows<'_>], placed: &[Placed], m: &GridMetrics) -> f64 {
    strips
        .iter()
        .zip(placed)
        .flat_map(|(strip, placed)| {
            placed.lines.iter().map(move |(line, tall)| {
                if line.is_empty() {
                    tall * 16.0 / 10.0
                } else {
                    strip.extent(line, placed.scale, m).0
                }
            })
        })
        .fold(0.0, f64::max)
}

/// Each of `strips` as it stands in `area` : in a band each its own line, else every strip at the
/// largest one scale at which the picture fits.
fn place_strips(
    area: Rect,
    strips: &[Windows<'_>],
    m: &GridMetrics,
    band: Option<f64>,
) -> Vec<Placed> {
    let reference = strips
        .iter()
        .map(|strip| strip.extent(&(0..strip.sizes.len()), 1.0, m).1)
        .fold(0.0, f64::max)
        .max(1.0);
    if let Some(band) = band {
        return strips
            .iter()
            .map(|strip| strip.banded(band, area.width(), reference, m))
            .collect();
    }
    let at = |scale: f64| -> Vec<Placed> {
        strips
            .iter()
            .map(|strip| strip.wrapped(scale, area.width(), reference * scale, m))
            .collect()
    };
    let fits = |scale: f64| {
        let placed = at(scale);
        picture_height(&placed, m) <= area.height()
            && picture_width(strips, &placed, m) <= area.width()
    };
    if fits(MAX_SCALE) {
        return at(MAX_SCALE);
    }
    // More scale only ever adds lines, height and width, so the largest that fits is found by
    // halving.
    let (mut lo, mut hi) = (0.0, MAX_SCALE);
    for _ in 0..32 {
        let mid = f64::midpoint(lo, hi);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at(lo)
}

/// Lay out `strips`, each a list of its windows' sizes on the display, in
/// `area` (`joined[s][i]` puts strip `s`'s window `i` against the one before
/// it, as a tab of the same window). With `band`, every strip is one line of windows that tall
/// (narrower only when the line is too wide) and the picture grows down from the top of `area`.
/// Without, the whole picture takes one scale, the largest at which it fits `area`: a strip wider
/// than the picture wraps onto more lines rather than shrinking its windows to slivers whose titles
/// cannot be read, and the picture is centred.
pub fn layout_strips(
    area: Rect,
    strips: &[Vec<Size>],
    joined: &[Vec<bool>],
    m: &GridMetrics,
    band: Option<f64>,
) -> Vec<StripPlace> {
    if strips.is_empty() || area.width() <= 0.0 {
        return Vec::new();
    }
    let strips: Vec<Windows<'_>> = strips
        .iter()
        .enumerate()
        .map(|(si, sizes)| Windows {
            sizes,
            joined: joined.get(si).map_or(&[][..], Vec::as_slice),
        })
        .collect();
    let plan = place_strips(area, &strips, m, band);
    let widest = picture_width(&strips, &plan, m).min(area.width());
    let x0 = area.x0 + ((area.width() - widest) / 2.0).max(0.0);
    let mut y = if band.is_some() {
        area.y0
    } else {
        area.y0 + ((area.height() - picture_height(&plan, m)) / 2.0).max(0.0)
    };
    let mut places = Vec::with_capacity(plan.len());
    for (strip, placed) in strips.iter().zip(plan) {
        let label = Rect::new(x0, y, x0 + widest, y + m.label_h);
        y = label.y1;
        let mut place = StripPlace {
            label,
            ..StripPlace::default()
        };
        for (li, (line, line_h)) in placed.lines.into_iter().enumerate() {
            if li > 0 {
                y += m.window_gap;
            }
            if line.is_empty() {
                let wide = (line_h * 16.0 / 10.0).min(widest);
                place.empty = Some(Rect::new(x0, y, x0 + wide, y + line_h));
            }
            let top = y + m.title_h();
            let mut x = x0;
            for (wi, size) in strip
                .sizes
                .iter()
                .enumerate()
                .take(line.end)
                .skip(line.start)
            {
                if wi > line.start {
                    x += strip.gap(wi, m);
                }
                let (wide, tall) = (size.width * placed.scale, size.height * placed.scale);
                let well = Rect::new(x, top, x + wide, top + tall);
                place.wells.push(well);
                place
                    .titles
                    .push(Rect::new(well.x0, y, well.x1, y + m.title_lh));
                x = well.x1;
            }
            y += line_h + m.title_h();
        }
        y += m.row_gap;
        places.push(place);
    }
    places
}

/// A pictured window, as drawn.
pub struct Tile<'a> {
    pub window: &'a SwitcherWindow,
    /// The tab its window shows (the others stand beside it).
    pub front: bool,
    /// Where its picture rests, and its title line over it.
    pub well: Rect,
    pub title: Rect,
    /// The people in it: their initial and colour.
    pub faces: &'a [(char, Color)],
}

/// A pictured window's title over its picture at `rect` : its app's mark, its title, and the faces
/// of the people in it at the title's end, as the switcher's cards name their windows.
pub fn draw_tile_title(ctx: &mut ui::paint::PaintCtx<'_>, tile: &Tile<'_>, rect: Rect, alpha: f32) {
    let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
    let g = GridMetrics::for_theme(theme);
    let colors = &theme.tokens.colors;
    let style = theme.text(TextRole::Label);
    let line = Rect::new(
        rect.x0,
        rect.y0 - g.title_gap - g.title_lh,
        rect.x1,
        rect.y0 - g.title_gap,
    );
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        alpha,
        transform,
        &line.inflate(g.title_lh, g.title_lh),
    );
    let face = g.mark;
    let shown = tile.faces.len().min(TILE_FACES_MAX);
    let faces_w = if shown == 0 {
        0.0
    } else {
        face * (1.0 + 0.6 * f64_of_usize(shown.saturating_sub(1)))
    };
    let gap = f64::from(theme.tokens.spacing.s2);
    let app = &tile.window.app;
    let mark_top = line.center().y - g.mark / 2.0;
    ui::widget::draw_app_mark(
        scene,
        transform,
        Rect::new(line.x0, mark_top, line.x0 + g.mark, mark_top + g.mark),
        app.icon.as_ref(),
        app.kind,
        colors.text_primary,
    );
    let text_x = line.x0 + g.mark + gap;
    let text_w = (line.x1 - text_x - faces_w - gap).max(0.0);
    let shown_title =
        ctx.text
            .elide_to_width_style(&tile.window.title, ui::num::f32_of_f64(text_w), &style);
    let (_, text_h) = ctx.text.measure_style(&shown_title, None, &style);
    ctx.text.draw_style(
        scene,
        &shown_title,
        colors.text_primary,
        transform.pre_translate((text_x, line.center().y - f64::from(text_h) / 2.0).into()),
        None,
        &style,
    );
    let ground = ui::chrome::shell::desktop_ground(theme);
    let mut x1 = line.x1;
    for (initial, color) in tile.faces.iter().take(shown).rev() {
        let disc = Rect::new(
            x1 - face,
            line.center().y - face / 2.0,
            x1,
            line.center().y + face / 2.0,
        );
        ui::widget::AvatarFace::new(&initial.to_string(), "")
            .presence(*color)
            .ringed_on(ground)
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, &mut *ctx.text, theme),
                disc,
            );
        x1 -= face * 0.6;
    }
    scene.pop_layer();
}

/// The slot of a strip with no windows.
pub fn draw_empty_strip(ctx: &mut ui::paint::PaintCtx<'_>, slot: Rect, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
    scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, transform, &slot);
    let on = ui::widget::paint_surface(
        scene,
        transform,
        slot,
        ui::widget::SurfaceLevel::Region,
        theme,
    );
    let style = on.tokens.typography.style(TextRole::Caption);
    let label = t("overview-strip-empty");
    let (w, h) = ctx.text.measure_style(&label, None, &style);
    ctx.text.draw_style(
        scene,
        &label,
        on.tokens.colors.text_dim,
        transform.pre_translate(
            (
                slot.center().x - f64::from(w) / 2.0,
                slot.center().y - f64::from(h) / 2.0,
            )
                .into(),
        ),
        None,
        &style,
    );
    scene.pop_layer();
}

/// A strip's name on its name line `label` , elided before `end` : the strip with focus in the
/// primary ink and a heavier weight, the others dim.
pub fn draw_strip_name(
    ctx: &mut ui::paint::PaintCtx<'_>,
    label: Rect,
    end: f64,
    (name, focused): (&str, bool),
) {
    let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
    let mut style = theme.tokens.typography.style(TextRole::Label);
    if focused {
        style.weight = TextWeight::Semibold;
    }
    let name =
        ctx.text
            .elide_to_width_style(name, ui::num::f32_of_f64((end - label.x0).max(0.0)), &style);
    let (_, name_h) = ctx.text.measure_style(&name, None, &style);
    let colors = &theme.tokens.colors;
    let top =
        label.y0 + (label.height() - f64::from(theme.tokens.spacing.s2) - f64::from(name_h)) / 2.0;
    ctx.text.draw_style(
        scene,
        &name,
        if focused {
            colors.text_primary
        } else {
            colors.text_dim
        },
        transform.pre_translate((label.x0, top).into()),
        None,
        &style,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> GridMetrics {
        GridMetrics::for_theme(&Theme::dark())
    }

    tilo_test_env::scenario! {
        fn the_overview_picture_lays_out_strips_and_windows;

        phase strips_stack_top_to_bottom_with_their_windows_in_strip_order_and_relative_sizes;
        {
            let m = metrics();
            let area = Rect::new(0.0, 0.0, 1200.0, 800.0);
            let strips = vec![
                vec![Size::new(1000.0, 900.0), Size::new(500.0, 900.0)],
                vec![],
                vec![Size::new(800.0, 900.0)],
            ];
            let places = layout_strips(area, &strips, &[], &m, None);
            assert_eq!(places.len(), 3);
            let first = &places[0];
            assert_eq!(first.wells.len(), 2);
            assert!(first.wells[0].x1 < first.wells[1].x0, "left to right");
            let ratio = first.wells[0].width() / first.wells[1].width();
            assert!((ratio - 2.0).abs() < 1e-6, "sizes stay relative: {ratio}");
            assert!(places[1].empty.is_some(), "an empty strip keeps its row");
            assert!(places[0].label.y1 <= first.wells[0].y0);
            assert!(
                first.titles[0].y1 <= first.wells[0].y0,
                "a window's title stands over its picture"
            );
            assert!(places[0].label.y1 <= first.titles[0].y0);
            for pair in places.windows(2) {
                assert!(pair[0].bounds().y1 <= pair[1].label.y0, "top to bottom");
            }
            for place in &places {
                let b = place.bounds();
                assert!(b.x0 >= area.x0 && b.x1 <= area.x1 + 1e-6, "{b:?}");
                assert!(b.y0 >= area.y0 && b.y1 <= area.y1 + 1e-6, "{b:?}");
            }
            // Never larger than half the window it pictures.
            assert!(places[2].wells[0].height() <= 900.0 * MAX_SCALE + 1e-6);
        }

        phase the_tabs_of_one_window_sit_together_and_windows_apart;
        {
            let m = metrics();
            let area = Rect::new(0.0, 0.0, 1600.0, 800.0);
            let strips = vec![vec![Size::new(400.0, 900.0); 3]];
            // The first two are tabs of one window, the third its own window.
            let places = layout_strips(area, &strips, &[vec![false, true, false]], &m, None);
            let wells = &places[0].wells;
            assert!((wells[1].x0 - wells[0].x1 - m.tab_gap).abs() < 1e-6);
            assert!((wells[2].x0 - wells[1].x1 - m.window_gap).abs() < 1e-6);
        }

        phase a_strip_too_wide_for_the_picture_wraps_so_its_titles_can_be_read;
        {
            // The picture beside the panel on a 1280 x 800 display: six windows, two of them tabs
            // of one window, laid out on the display at half its width.
            let m = metrics();
            let area = Rect::new(24.0, 56.0, 712.0, 776.0);
            let strips = vec![
                vec![Size::new(604.0, 720.0); 6],
                vec![Size::new(400.0, 720.0)],
            ];
            let joined = vec![vec![false, false, false, true, false, false]];
            let places = layout_strips(area, &strips, &joined, &m, None);
            let wells = &places[0].wells;
            let lines: Vec<f64> = wells.iter().map(|w| w.y0).fold(Vec::new(), |mut ys, y| {
                if !ys.iter().any(|seen: &f64| (seen - y).abs() < 1e-6) {
                    ys.push(y);
                }
                ys
            });
            assert!(lines.len() > 1, "the strip wraps: {wells:?}");
            for title in &places[0].titles {
                assert!(
                    title.width() >= area.width() / 5.0,
                    "a title has room to be read: {title:?}"
                );
            }
            assert!(
                (wells[3].y0 - wells[2].y0).abs() < 1e-6,
                "a window's tabs stay on one line"
            );
            assert!(
                (places[1].wells[0].height() - wells[0].height()).abs() < 1e-6,
                "one scale for the whole picture"
            );
            for place in &places {
                let b = place.bounds();
                assert!(b.x0 >= area.x0 - 1e-6 && b.x1 <= area.x1 + 1e-6, "{b:?}");
                assert!(b.y0 >= area.y0 - 1e-6 && b.y1 <= area.y1 + 1e-6, "{b:?}");
            }
        }

        phase inside_the_panel_every_strip_is_one_band_tall_and_the_picture_grows_down;
        {
            let m = metrics();
            let area = Rect::new(10.0, 100.0, 610.0, 100.0);
            let strips = vec![vec![Size::new(800.0, 600.0)], vec![Size::new(400.0, 600.0)]];
            let places = layout_strips(area, &strips, &[], &m, Some(m.band));
            assert_eq!(places[0].label.y0, area.y0);
            assert!((places[0].wells[0].height() - m.band).abs() < 1e-6);
            let bottom = places.last().map(StripPlace::bounds).expect("strips").y1;
            assert!(
                (bottom - (area.y0 + band_height(2, &m, m.band))).abs() < 1e-6,
                "{bottom}"
            );
        }
    }
}
