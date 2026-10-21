//! The person's reading preferences: one set for every reader on Tilo.
//!
//! Feeds, Books, the Browser's reader mode and Mail read the same section of
//! the person's settings (`reading`), so text reads the same everywhere and
//! is set once. The section follows the person to their other devices.

use serde::{Deserialize, Serialize};

use super::palette::Paper;
use crate::text_render::{FontChoice, UI_SANS_FAMILY};

/// The face long-form text is set in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingFont {
    /// The reading serif: Literata, drawn for long reading on screens, with
    /// optical sizes, old-style figures and small capitals.
    #[default]
    Serif,
    /// The interface sans, Tilo Sans.
    Sans,
    /// The publisher's own face where the document names one the system
    /// has (a book's embedded font), else the reading serif.
    Publisher,
}

/// The reading serif's family.
pub const READING_SERIF_FAMILY: &str = "Literata";

impl ReadingFont {
    pub const ALL: [Self; 3] = [Self::Serif, Self::Sans, Self::Publisher];

    /// What the reading menu calls it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Serif => "Serif",
            Self::Sans => "Sans",
            Self::Publisher => "Original",
        }
    }

    /// The family text is set in: `publisher` is the document's own face,
    /// when it names one.
    #[must_use]
    pub fn family(self, publisher: Option<&FontChoice>) -> FontChoice {
        match (self, publisher) {
            (Self::Sans, _) => FontChoice::Named(UI_SANS_FAMILY.to_owned()),
            (Self::Publisher, Some(face)) => face.clone(),
            (Self::Serif | Self::Publisher, _) => {
                FontChoice::Named(READING_SERIF_FAMILY.to_owned())
            }
        }
    }
}

/// How long a line runs, in characters of the reading face.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Measure {
    Narrow,
    #[default]
    Medium,
    Wide,
}

/// The shortest line the reader sets where the window allows it.
pub const MIN_MEASURE_CHARS: f32 = 60.0;
/// The longest line the reader ever sets.
pub const MAX_MEASURE_CHARS: f32 = 72.0;

impl Measure {
    pub const ALL: [Self; 3] = [Self::Narrow, Self::Medium, Self::Wide];

    /// Characters per line, within [`MIN_MEASURE_CHARS`]..=[`MAX_MEASURE_CHARS`].
    #[must_use]
    pub fn chars(self) -> f32 {
        match self {
            Self::Narrow => MIN_MEASURE_CHARS,
            Self::Medium => 66.0,
            Self::Wide => MAX_MEASURE_CHARS,
        }
    }

    /// What the reading menu calls it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Narrow => "Narrow",
            Self::Medium => "Medium",
            Self::Wide => "Wide",
        }
    }
}

/// The person's reading preferences (see the [module docs](self)).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReadingPreferences {
    pub font: ReadingFont,
    /// The reading size: a multiple of [`READING_BODY_PX`], within
    /// [`Self::SIZE_RANGE`].
    pub size: f32,
    pub measure: Measure,
    pub paper: Paper,
    /// Set paragraphs justified (hyphenated by the document's language)
    /// instead of ragged.
    pub justify: bool,
    /// How fast this person reads, measured as they read; what "12 min
    /// left" is estimated from.
    pub words_per_minute: f32,
}

/// The body size at a reading size of 1, in logical pixels before the
/// system's text scale.
pub const READING_BODY_PX: f32 = 18.0;

impl Default for ReadingPreferences {
    fn default() -> Self {
        Self {
            font: ReadingFont::Serif,
            size: 1.0,
            measure: Measure::Medium,
            paper: Paper::System,
            justify: false,
            words_per_minute: super::document::DEFAULT_WORDS_PER_MINUTE,
        }
    }
}

impl crate::storage::SettingsSchema for ReadingPreferences {
    const SCOPE: crate::storage::Scope = crate::storage::Scope::Shared(READING_SECTION);
}

/// The settings section every reader shares.
pub const READING_SECTION: &str = "reading";

impl ReadingPreferences {
    /// The reading sizes a person can choose.
    pub const SIZE_RANGE: std::ops::RangeInclusive<f32> = 0.8..=2.0;
    /// One step of the reading size, as Ctrl+= and Ctrl+- take it.
    pub const SIZE_STEP: f32 = 0.1;
    /// The reading speeds the estimate keeps to.
    pub const WPM_RANGE: std::ops::RangeInclusive<f32> = 80.0..=900.0;

    /// The body size in logical pixels, before the system's text scale.
    #[must_use]
    pub fn body_px(&self) -> f32 {
        READING_BODY_PX * self.clamped().size
    }

    /// One step larger (`direction` 1) or smaller (-1), kept in range and
    /// on whole steps.
    pub fn step_size(&mut self, direction: i8) {
        let steps = (self.size / Self::SIZE_STEP).round() + f32::from(direction);
        self.size = steps * Self::SIZE_STEP;
        *self = self.clamped();
    }

    /// The reading size as a whole percentage (`110` for 1.1).
    #[must_use]
    pub fn percent(&self) -> u16 {
        crate::num::u32_of_f64(f64::from(self.clamped().size) * 100.0)
            .try_into()
            .unwrap_or(100)
    }

    /// These preferences with every value in range: a hand edit or another
    /// version's value never sets unreadable text.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        if !self.size.is_finite() {
            self.size = 1.0;
        }
        self.size = self
            .size
            .clamp(*Self::SIZE_RANGE.start(), *Self::SIZE_RANGE.end());
        if !self.words_per_minute.is_finite() {
            self.words_per_minute = super::document::DEFAULT_WORDS_PER_MINUTE;
        }
        self.words_per_minute = self
            .words_per_minute
            .clamp(*Self::WPM_RANGE.start(), *Self::WPM_RANGE.end());
        self
    }
}
