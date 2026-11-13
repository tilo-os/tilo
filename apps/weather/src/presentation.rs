//! The saved-place row, shared by the live list and the History preview: the
//! condition's glyph, the name, the condition or region under it, and the
//! temperature trailing.
use crate::{
    model::{SavedLocation, WeatherSnapshot},
    units::Units,
    wxcode::Sky,
};
use ui::{
    TextRenderer, Theme,
    paint::PaintCtx,
    widget::{Interaction, ListRowFace, RowLeading},
};
use vello::kurbo::Rect;

type GlyphPainter = Box<dyn Fn(&mut PaintCtx<'_>, Rect)>;

pub(crate) struct PlaceRowText {
    glyph: GlyphPainter,
    secondary: String,
    temperature: Option<String>,
}

impl PlaceRowText {
    pub(crate) fn of(
        loc: &SavedLocation,
        snapshot: Option<&WeatherSnapshot>,
        units: Units,
    ) -> Self {
        match snapshot {
            Some(snap) => Self {
                glyph: glyph(Some((
                    Sky::from_code(snap.current.weather_code),
                    snap.current.is_day,
                ))),
                secondary: Sky::label(snap.current.weather_code),
                temperature: Some(format!(
                    "{}°",
                    snap.temp_in(snap.current.temperature, units).round() as i64
                )),
            },
            None => Self {
                glyph: glyph(None),
                secondary: loc.subtitle(),
                temperature: None,
            },
        }
    }

    pub(crate) fn face<'a>(
        &'a self,
        loc: &'a SavedLocation,
        selected: bool,
        interaction: Interaction,
    ) -> ListRowFace<'a> {
        let face = ListRowFace::new(&loc.name)
            .leading(RowLeading::Paint(&*self.glyph))
            .secondary(&self.secondary)
            .selected(selected)
            .interaction(interaction);
        match &self.temperature {
            Some(temperature) => face.title_value(temperature),
            None => face,
        }
    }
}

fn glyph(sky: Option<(Sky, bool)>) -> GlyphPainter {
    Box::new(move |ctx, slot| {
        if let Some((sky, is_day)) = sky {
            crate::icon::draw_icon(ctx.scene, ctx.transform, sky, is_day, slot, ctx.theme);
        } else {
            ui::icons::draw_icon(
                ctx.scene,
                ctx.transform,
                slot,
                ui::icons::Icon::MapPin,
                ctx.theme.text_dim,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    })
}

pub(crate) fn row_height(text: &mut TextRenderer, theme: &Theme) -> f64 {
    ListRowFace::new("Ag").secondary("Ag").height(text, theme)
}

/// The observation as bounded data for the palette; a zero timestamp is a capture
/// fixture, never a live fetch.
pub(crate) fn morning_preview(
    snapshot: &WeatherSnapshot,
    units: Units,
    notice: Option<crate::core::ForecastNotice>,
) -> ui::tilo_actions::ResourcePresentation {
    use ui::i18n::ArgValue;
    ui::embedded_catalog!(static CATALOG, ["en-US", "de-DE"]);
    let temperature = format!(
        "{:.0}{}",
        snapshot.temp_in(snapshot.current.temperature, units),
        units.temp_suffix()
    );
    let preview = CATALOG.t_args(
        "weather-morning-summary",
        &[
            ("temperature", ArgValue::from(temperature)),
            (
                "condition",
                ArgValue::from(Sky::label(snapshot.current.weather_code)),
            ),
        ],
    );
    let detail = CATALOG.t_args(
        "weather-morning-source",
        &[(
            "fetched",
            ArgValue::from(
                i64::try_from(snapshot.fetched_at)
                    .ok()
                    .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                    .map_or_else(
                        || snapshot.fetched_at.to_string(),
                        |time| {
                            ui::format::timestamp(&time.with_timezone(&chrono::Local).naive_local())
                        },
                    ),
            ),
        )],
    );
    let badge = CATALOG.t(if snapshot.fetched_at == 0 {
        "weather-morning-fixture"
    } else if notice.is_some() {
        "weather-morning-stale"
    } else {
        "weather-morning-freshness"
    });
    ui::tilo_actions::ResourcePresentation {
        detail: Some(detail),
        badge: Some(badge),
        preview: Some(preview),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_place_without_a_forecast_still_shows_a_leading_mark() {
        let mut scene = vello::Scene::new();
        let mut text = ui::TextRenderer::new();
        let theme = ui::Theme::dark();
        {
            let mut ctx = ui::paint::PaintCtx::new(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &theme,
            );
            (super::glyph(None))(&mut ctx, vello::kurbo::Rect::new(0.0, 0.0, 20.0, 20.0));
        }
        assert!(
            !scene.encoding().is_empty(),
            "the row's leading slot is not a blank gap"
        );
    }
}
