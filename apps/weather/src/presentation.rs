//! The saved-place row, shared by the live city list and the History
//! preview. Pure presentation: no live `Core`, no services.
//!
//! A row is the toolkit's list row: the current condition's glyph leading,
//! the place's name as the title, its current condition as the secondary
//! line (its region before the first forecast arrives), and the current
//! temperature as the row's trailing figure in tabular numerals. Selection, hover and the delete swipe are the
//! row's own states.
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

/// Paints a row's leading glyph into its slot.
type GlyphPainter = Box<dyn Fn(&mut PaintCtx<'_>, Rect)>;

/// The text one saved-place row shows, owned so the row face can borrow it.
pub(crate) struct PlaceRowText {
    /// Paints the current sky's glyph; before the first forecast it paints
    /// nothing, and the empty leading slot keeps the names lined up.
    glyph: GlyphPainter,
    secondary: String,
    temperature: Option<String>,
}

impl PlaceRowText {
    /// The row text for `loc` with its latest `snapshot` (none before the
    /// first fetch) in `units`.
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
                secondary: Sky::label(snap.current.weather_code).to_string(),
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

    /// The row face for `loc`, selected and interacting as given.
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

/// The painter for a row's leading condition glyph.
fn glyph(sky: Option<(Sky, bool)>) -> GlyphPainter {
    Box::new(move |ctx, slot| {
        if let Some((sky, is_day)) = sky {
            crate::icon::draw_icon(ctx.scene, ctx.transform, sky, is_day, slot, ctx.theme);
        }
    })
}

/// The height of one saved-place row in `theme`: every row has a title and
/// a secondary line, so every row is this tall.
pub(crate) fn row_height(text: &mut TextRenderer, theme: &Theme) -> f64 {
    ListRowFace::new("Ag").secondary("Ag").height(text, theme)
}
