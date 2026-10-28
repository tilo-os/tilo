//! The overview's picture of a workspace: every strip, top to bottom, with
//! its windows left to right in the order the strip holds them, each at its
//! share of the size it has on the display, under the strip's name.
//!
//! One picture for every desktop: the native shell flies its windows into
//! these places on the overview's zoom, and the browser desktop's overview
//! panel draws the same places, wells, titles and names, so both read the
//! same way. The layout is plain geometry ([`layout_strips`]); painting a
//! window's picture is [`crate::window_switcher::draw_preview_well`], its
//! title [`draw_tile_title`], an empty strip [`draw_empty_strip`] and a
//! strip's name [`draw_strip_name`].

use ui::TextRole;
use ui::num::f64_of_usize;
use ui::text_render::TextWeight;
use ui::theme::Theme;
use ui::widget::ControlSize;
use vello::kurbo::{Rect, Size};
use vello::peniko::{BlendMode, Color, Fill};

use crate::i18n::t;
use crate::window_switcher::SwitcherWindow;

/// A window is pictured at most at this share of its size on the display:
/// a workspace of one window still reads as a picture, not as the window.
pub const MAX_SCALE: f64 = 0.5;

/// Most faces drawn on one window before the picture says nothing more.
const TILE_FACES_MAX: usize = 3;

/// Spacing of the picture, from the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridMetrics {
    /// A strip's name line, with the air below it.
    pub label_h: f64,
    /// Air between a pictured window and its title.
    pub title_gap: f64,
    /// Line height of a window's title.
    pub title_lh: f64,
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
            title_gap: f64::from(spacing.s1),
            title_lh: lh(TextRole::Caption),
            row_gap: f64::from(spacing.s6),
            window_gap: f64::from(spacing.s4),
            tab_gap: f64::from(spacing.s1),
            band: f64::from(spacing.s16) * 1.5,
            control,
            radius: f64::from(tokens.radii.control),
            window_radius: f64::from(ui::tokens::Radii::WINDOW),
        }
    }

    /// A window's title line under its picture, with the air above it.
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
    /// Each window's title line, under its picture.
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

/// Lay out `strips`, each a list of its windows' sizes on the display, in
/// `area` (`joined[s][i]` puts strip `s`'s window `i` against the one before
/// it, as a tab of the same window): one scale for the whole picture so windows keep their sizes
/// relative to each other, a smaller one only for a strip too wide for the
/// picture. With `band`, every strip's windows are that tall and the picture
/// grows down from the top of `area`; without, it fills `area`'s height,
/// centred.
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
    let gap = |strip: usize, window: usize| {
        if joined
            .get(strip)
            .and_then(|joined| joined.get(window))
            .copied()
            .unwrap_or(false)
        {
            m.tab_gap
        } else {
            m.window_gap
        }
    };
    let gaps = |strip: usize, count: usize| (1..count).map(|i| gap(strip, i)).sum::<f64>();
    let tallest = |sizes: &Vec<Size>| sizes.iter().map(|s| s.height).fold(0.0, f64::max);
    let reference = strips.iter().map(tallest).fold(0.0, f64::max).max(1.0);
    // A strip with no windows takes a row as tall as the others' windows.
    let heights: Vec<f64> = strips
        .iter()
        .map(|sizes| {
            Some(tallest(sizes))
                .filter(|h| *h > 0.0)
                .unwrap_or(reference)
        })
        .collect();
    let fixed = band_height(strips.len(), m, 0.0);
    let fill_scale = ((area.height() - fixed) / heights.iter().sum::<f64>()).clamp(0.0, MAX_SCALE);
    let rows: Vec<(f64, f64)> = strips
        .iter()
        .zip(&heights)
        .enumerate()
        .map(|(si, (sizes, height))| {
            let height_scale = band.map_or(fill_scale, |band| band / height);
            let widths: f64 = sizes.iter().map(|s| s.width).sum();
            let gaps = gaps(si, sizes.len());
            let width_scale = if widths > 0.0 {
                ((area.width() - gaps) / widths).max(0.0)
            } else {
                f64::INFINITY
            };
            (height_scale.min(width_scale), height * height_scale)
        })
        .collect();
    let row_width = |si: usize, sizes: &Vec<Size>, (scale, band_h): (f64, f64)| {
        if sizes.is_empty() {
            return band_h * 16.0 / 10.0;
        }
        sizes.iter().map(|s| s.width * scale).sum::<f64>() + gaps(si, sizes.len())
    };
    let widest = strips
        .iter()
        .zip(&rows)
        .enumerate()
        .map(|(si, (sizes, row))| row_width(si, sizes, *row))
        .fold(0.0, f64::max)
        .min(area.width());
    let x0 = area.x0 + ((area.width() - widest) / 2.0).max(0.0);
    let total = fixed + rows.iter().map(|(_, band_h)| band_h).sum::<f64>();
    let mut y = if band.is_some() {
        area.y0
    } else {
        area.y0 + ((area.height() - total) / 2.0).max(0.0)
    };
    let mut places = Vec::with_capacity(strips.len());
    for (si, (sizes, (scale, band_h))) in strips.iter().zip(rows).enumerate() {
        let label = Rect::new(x0, y, x0 + widest, y + m.label_h);
        y = label.y1;
        let mut place = StripPlace {
            label,
            ..StripPlace::default()
        };
        if sizes.is_empty() {
            let w = (band_h * 16.0 / 10.0).min(widest);
            place.empty = Some(Rect::new(x0, y, x0 + w, y + band_h));
        }
        let mut x = x0;
        for (wi, size) in sizes.iter().enumerate() {
            if wi > 0 {
                x += gap(si, wi);
            }
            let well = Rect::new(x, y, x + size.width * scale, y + size.height * scale);
            let title_y = well.y1 + m.title_gap;
            place.wells.push(well);
            place
                .titles
                .push(Rect::new(well.x0, title_y, well.x1, title_y + m.title_lh));
            x = well.x1;
        }
        y += band_h + m.title_h() + m.row_gap;
        places.push(place);
    }
    places
}

/// A pictured window, as drawn.
pub struct Tile<'a> {
    pub window: &'a SwitcherWindow,
    /// The tab its window shows (the others stand beside it).
    pub front: bool,
    /// Where its picture rests, and its title line under it.
    pub well: Rect,
    pub title: Rect,
    /// The people in it: their initial and colour.
    pub faces: &'a [(char, Color)],
}

/// A pictured window's title under its picture at `rect`, and the faces
/// of the people in it at the title's end.
pub fn draw_tile_title(
    ctx: &mut ui::paint::PaintCtx<'_>,
    tile: &Tile<'_>,
    rect: Rect,
    (alpha, active): (f32, bool),
) {
    let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
    let g = GridMetrics::for_theme(theme);
    let colors = &theme.tokens.colors;
    let style = theme.tokens.typography.style(TextRole::Caption);
    let line = Rect::new(
        rect.x0,
        rect.y1 + g.title_gap,
        rect.x1,
        rect.y1 + g.title_gap + g.title_lh,
    );
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        alpha,
        transform,
        &line.inflate(g.title_lh, g.title_lh),
    );
    let face = g.title_lh;
    let shown = tile.faces.len().min(TILE_FACES_MAX);
    let faces_w = if shown == 0 {
        0.0
    } else {
        face * (1.0 + 0.6 * f64_of_usize(shown.saturating_sub(1)))
    };
    let text_w = (line.width() - faces_w - f64::from(theme.tokens.spacing.s1)).max(0.0);
    let shown_title =
        ctx.text
            .elide_to_width_style(&tile.window.title, ui::num::f32_of_f64(text_w), &style);
    ctx.text.draw_style(
        scene,
        &shown_title,
        if active {
            colors.text_primary
        } else {
            colors.text_dim
        },
        transform.pre_translate((line.x0, line.y0).into()),
        None,
        &style,
    );
    let ground = ui::chrome::shell::desktop_ground(theme);
    let mut x1 = line.x1;
    for (initial, color) in tile.faces.iter().take(shown).rev() {
        let disc = Rect::new(x1 - face, line.y0, x1, line.y0 + face);
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

/// A strip's name on its name line `label`, elided before `end`: the
/// strip with focus in the primary ink and a heavier weight, the others
/// dim.
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

    #[test]
    fn strips_stack_top_to_bottom_with_their_windows_in_strip_order_and_relative_sizes() {
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
        assert!(first.titles[0].y0 > first.wells[0].y1);
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

    #[test]
    fn the_tabs_of_one_window_sit_together_and_windows_apart() {
        let m = metrics();
        let area = Rect::new(0.0, 0.0, 1600.0, 800.0);
        let strips = vec![vec![Size::new(400.0, 900.0); 3]];
        // The first two are tabs of one window, the third its own window.
        let places = layout_strips(area, &strips, &[vec![false, true, false]], &m, None);
        let wells = &places[0].wells;
        assert!((wells[1].x0 - wells[0].x1 - m.tab_gap).abs() < 1e-6);
        assert!((wells[2].x0 - wells[1].x1 - m.window_gap).abs() < 1e-6);
    }

    #[test]
    fn a_strip_too_wide_for_the_picture_shrinks_alone() {
        let m = metrics();
        let area = Rect::new(0.0, 0.0, 600.0, 800.0);
        let strips = vec![
            vec![Size::new(1000.0, 900.0); 6],
            vec![Size::new(400.0, 900.0)],
        ];
        let places = layout_strips(area, &strips, &[], &m, None);
        let wide = places[0].wells.last().expect("windows");
        assert!(wide.x1 <= area.x1 + 1e-6, "{wide:?}");
        assert!(
            places[1].wells[0].height() > places[0].wells[0].height(),
            "the other strip keeps the picture's scale"
        );
    }

    #[test]
    fn inside_the_panel_every_strip_is_one_band_tall_and_the_picture_grows_down() {
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
