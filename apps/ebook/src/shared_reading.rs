//! Books and the reading preferences every reader shares.
//!
//! Feeds, the Browser's reader mode, Mail and Books set text in one set of
//! preferences ([`ReadingPreferences`]), so a person sets it once. Books
//! keeps its own richer settings in the library (line spacing, colour
//! schemes, its registry of faces, paged or scrolling); the ones the shared
//! set names follow it both ways: the text size, the face (the reading
//! serif, the interface sans), the warm paper and justification. A change in
//! Books' menu reaches the other readers, and a change made in another
//! reader reaches Books.

use ui::reading::{Paper, ReadingFont, ReadingPreferences};

use crate::model::{CompositionStyle, ReadTheme, Settings};

/// The registry id of the face the shared reading serif is.
pub const SERIF_FONT_ID: &str = "reading-literata";
/// The registry id of the interface sans.
pub const SANS_FONT_ID: &str = "default";

/// `settings` with what `prefs` says set: the shared size, face, paper and
/// justification. A face the shared set leaves to the publisher keeps
/// Books' own choice; the system's page shows the light or dark page of the
/// system (`system_dark`) where the warm paper was.
#[must_use]
pub fn adopted(settings: &Settings, prefs: &ReadingPreferences, system_dark: bool) -> Settings {
    let prefs = prefs.clamped();
    let mut out = settings.clone();
    out.font_scale = prefs
        .size
        .clamp(Settings::READING_SCALE_MIN, Settings::READING_SCALE_MAX);
    out.composition = if prefs.justify {
        CompositionStyle::Book
    } else {
        CompositionStyle::Ragged
    };
    match prefs.font {
        ReadingFont::Serif => SERIF_FONT_ID.clone_into(&mut out.font_id),
        ReadingFont::Sans => SANS_FONT_ID.clone_into(&mut out.font_id),
        ReadingFont::Publisher => {}
    }
    match prefs.paper {
        Paper::Warm => {
            out.theme = ReadTheme::Sepia;
            out.scheme_id.clear();
        }
        Paper::System if out.theme == ReadTheme::Sepia && out.scheme_id.is_empty() => {
            out.theme = if system_dark {
                ReadTheme::Dark
            } else {
                ReadTheme::Light
            };
        }
        Paper::System => {}
    }
    out
}

/// The shared preferences `prefs` with what Books' `settings` say of the
/// ones the shared set names.
#[must_use]
pub fn shared(settings: &Settings, prefs: ReadingPreferences) -> ReadingPreferences {
    let mut out = prefs;
    out.size = settings.font_scale;
    out.justify = settings.composition.justified();
    out.paper = if settings.theme == ReadTheme::Sepia && settings.scheme_id.is_empty() {
        Paper::Warm
    } else {
        Paper::System
    };
    // A face left to the publisher stays so: Books' own choice of face
    // does not take it back.
    if prefs.font != ReadingFont::Publisher {
        match settings.font_id.as_str() {
            SERIF_FONT_ID => out.font = ReadingFont::Serif,
            SANS_FONT_ID => out.font = ReadingFont::Sans,
            _ => {}
        }
    }
    out.clamped()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The size, face, paper and justification Books sets are the shared ones, both ways; what
    /// only Books has (line spacing, its colour schemes, paged or scrolling) stays its own.
    #[test]
    fn books_set_text_in_the_shared_reading_preferences_both_ways() {
        let prefs = ReadingPreferences {
            size: 1.3,
            font: ReadingFont::Sans,
            paper: Paper::Warm,
            justify: true,
            ..ReadingPreferences::default()
        };
        let settings = Settings {
            line_spacing: 1.4,
            ..Settings::default()
        };
        let books = adopted(&settings, &prefs, false);
        assert!((books.font_scale - 1.3).abs() < 1e-6);
        assert_eq!(books.font_id, SANS_FONT_ID);
        assert_eq!(books.theme, ReadTheme::Sepia);
        assert_eq!(books.composition, CompositionStyle::Book);
        assert!((books.line_spacing - 1.4).abs() < 1e-6, "Books' own stays");

        // What Books sets reaches every reader.
        let changed = Settings {
            font_scale: 1.1,
            font_id: SERIF_FONT_ID.into(),
            theme: ReadTheme::Dark,
            composition: CompositionStyle::Ragged,
            ..books.clone()
        };
        let back = shared(&changed, prefs);
        assert!((back.size - 1.1).abs() < 1e-6);
        assert_eq!(back.font, ReadingFont::Serif);
        assert_eq!(back.paper, Paper::System);
        assert!(!back.justify);

        // The system's page replaces the warm paper with the system's own.
        let system = ReadingPreferences {
            paper: Paper::System,
            ..prefs
        };
        assert_eq!(adopted(&books, &system, true).theme, ReadTheme::Dark);
        // A face left to the publisher keeps Books' choice.
        let publisher = ReadingPreferences {
            font: ReadingFont::Publisher,
            ..prefs
        };
        let garamond = Settings {
            font_id: "reading-eb-garamond".into(),
            ..books.clone()
        };
        assert_eq!(
            adopted(&garamond, &publisher, false).font_id,
            "reading-eb-garamond"
        );
    }
}
