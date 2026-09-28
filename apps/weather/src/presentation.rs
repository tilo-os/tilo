//! Pure saved-place painting shared with the live Weather overview. No live Core or services.
use crate::{
    core::{
        CHIP_NAME_TEXT, ChipMetrics, hover_wash, lerp_color, text_left, text_right, text_sm,
        text_x2l,
    },
    icon,
    model::{SavedLocation, WeatherSnapshot},
    units::Units,
    wxcode::Sky,
};
use ui::{TextRenderer, Theme, paint::PaintCtx, text_render::TextWeight};
use vello::{
    Scene,
    kurbo::{Rect, RoundedRect, Stroke},
    peniko::{Color, Fill},
};

/// One saved-place row: the place and its latest snapshot (none before the
/// first fetch), the units its temperature reads in, and its state.
pub(crate) struct SavedPlaceFace<'a> {
    pub(crate) chip: Rect,
    pub(crate) loc: &'a SavedLocation,
    pub(crate) snapshot: Option<&'a WeatherSnapshot>,
    pub(crate) units: Units,
    pub(crate) selected: bool,
    pub(crate) hovered: bool,
    pub(crate) accent: Color,
}

#[allow(
    clippy::too_many_lines,
    reason = "preserve the established live row painter as one shared presentation seam"
)]
pub(crate) fn draw_saved_place(ctx: &mut PaintCtx<'_>, face: &SavedPlaceFace<'_>) {
    let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let SavedPlaceFace {
        chip,
        loc,
        snapshot,
        units,
        selected,
        hovered,
        accent,
    } = *face;
    let c = &theme.tokens.colors;
    let radius = f64::from(theme.tokens.radii.md);
    let bg = if selected {
        lerp_color(c.surfaces.base, accent, 0.18)
    } else {
        c.surfaces.raised
    };
    // Fill plus the edge derived from that fill (HIG §9.5); hover is a
    // derived wash over the rest face, and the selected city keeps its
    // accent ring. The declared `border_standard` stroke this replaces was
    // three shades off `raised` on dark and vanished.
    ui::surface::card(scene, t, chip, radius, bg);
    let shape = RoundedRect::from_rect(chip, radius);
    if hovered && !selected {
        scene.fill(Fill::NonZero, t, hover_wash(bg), None, &shape);
    }
    if selected {
        scene.stroke(&Stroke::new(1.5), t, accent, None, &shape);
    }
    // Glyph, identity block and temperature all centre on the chip from
    // their measured heights, so a scaled row keeps them on one axis
    // instead of colliding at fixed 1× offsets.
    let m = ChipMetrics::new(&theme.tokens.spacing, tr.text_scale());
    let sp = &theme.tokens.spacing;
    let icon_x = chip.x0 + 10.0;
    if let Some(snap) = snapshot {
        let sky = Sky::from_code(snap.current.weather_code);
        let icon_y = chip.y0 + (chip.height() - m.icon) / 2.0;
        icon::draw_icon(
            scene,
            t,
            sky,
            snap.current.is_day,
            Rect::new(icon_x, icon_y, icon_x + m.icon, icon_y + m.icon),
            theme,
        );
    }
    let text_x = icon_x + m.icon + f64::from(sp.s2);
    let block_h = m.name_h + m.sub_h;
    let name_y = chip.y0 + (chip.height() - block_h) / 2.0;
    // Second line: country + condition word (the "quick per-city info").
    let sub = if let Some(snap) = snapshot {
        format!(
            "{} · {}",
            loc.country,
            Sky::label(snap.current.weather_code)
        )
    } else {
        loc.country.clone()
    };
    // The temperature owns the trailing column; identity text elides
    // before it rather than running underneath it.
    let temp_label = snapshot.map(|snap| {
        format!(
            "{}°",
            snap.temp_in(snap.current.temperature, units).round() as i64
        )
    });
    let (temp_text, temp_size, temp_weight) = match &temp_label {
        Some(label) => (label.as_str(), text_x2l(), TextWeight::Bold),
        None => ("—", 18.0, TextWeight::Regular),
    };
    let (temp_w, temp_h) = tr.measure_weighted(temp_text, temp_size, None, temp_weight);
    let temp_right = chip.x1 - 14.0;
    let text_max = ((temp_right - f64::from(temp_w) - f64::from(sp.s3)) - text_x).max(24.0);
    let name = tr.elide_to_width_weighted(
        &loc.name,
        CHIP_NAME_TEXT,
        text_max as f32,
        TextWeight::Semibold,
    );
    text_left(
        scene,
        tr,
        &name,
        text_x,
        name_y,
        CHIP_NAME_TEXT,
        TextWeight::Semibold,
        c.text_primary,
    );
    let sub = tr.elide_to_width(&sub, text_sm(), text_max as f32);
    text_left(
        scene,
        tr,
        &sub,
        text_x,
        name_y + m.name_h,
        text_sm(),
        TextWeight::Regular,
        c.text_dim,
    );
    text_right(
        scene,
        tr,
        temp_text,
        temp_right,
        chip.y0 + (chip.height() - f64::from(temp_h)) / 2.0,
        temp_size,
        temp_weight,
        if temp_label.is_some() {
            c.text_primary
        } else {
            c.text_dim
        },
    );
}
/// The place identity used by both the live detail header and frozen detail.
pub(crate) fn draw_identity(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    theme: &Theme,
    loc: &SavedLocation,
    x: f64,
    y: f64,
    width: f64,
) -> f64 {
    let h = f64::from(tr.measure_weighted("Ag", 26., None, TextWeight::Bold).1);
    let name = tr.elide_to_width_weighted(&loc.name, 26., width.max(20.) as f32, TextWeight::Bold);
    text_left(
        scene,
        tr,
        &name,
        x,
        y,
        26.,
        TextWeight::Bold,
        theme.tokens.colors.text_primary,
    );
    let sub_y = y + h + f64::from(theme.tokens.spacing.s1);
    let sub = tr.elide_to_width(&loc.subtitle(), 13., width.max(20.) as f32);
    text_left(
        scene,
        tr,
        &sub,
        x,
        sub_y,
        13.,
        TextWeight::Regular,
        theme.tokens.colors.text_dim,
    );
    sub_y
}
