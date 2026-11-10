//! The person's region and formats: the `locale` section of their settings, which every app
//! follows and the Settings app's "Region & Formats" panel edits, and the formats every date and
//! clock reads in.

use crate::TimeStyle;

/// How dates render: the shared `locale.date_format` setting. The serde names are the exact
/// strings the settings file uses (`Language`/`ISO`/`US`/`EU`/`Long`). Until the person picks
/// one, dates follow the language ([`DateStyle::Language`]); ISO is a choice like the others.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::Model,
)]
pub enum DateStyle {
    /// The language's own convention: `Mar 15, 2023` in English, `15.03.2023` in German.
    #[default]
    #[serde(rename = "Language")]
    #[model(rename = "Language")]
    Language,
    /// `2023-03-15`
    #[serde(rename = "ISO")]
    #[model(rename = "ISO")]
    Iso,
    /// `03/15/2023`
    #[serde(rename = "US")]
    #[model(rename = "US")]
    Us,
    /// `15/03/2023`
    #[serde(rename = "EU")]
    #[model(rename = "EU")]
    Eu,
    /// `15 Mar 2023`, the month named in the language.
    #[serde(rename = "Long")]
    #[model(rename = "Long")]
    Long,
}

impl DateStyle {
    pub const ALL: [DateStyle; 5] = [
        DateStyle::Language,
        DateStyle::Iso,
        DateStyle::Us,
        DateStyle::Eu,
        DateStyle::Long,
    ];

    /// A worked example so the picker shows what each style produces: 15 March 2023 in this
    /// style, [`DateStyle::Language`] in the language `words` read in.
    #[must_use]
    pub fn example(self, words: &dyn crate::Words) -> String {
        let day = chrono::NaiveDate::from_ymd_opt(2023, 3, 15).unwrap_or_default();
        crate::date(
            RegionFormats {
                date: self,
                ..RegionFormats::default()
            },
            day,
            words,
        )
    }

    /// User-facing name for the dropdown.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            DateStyle::Language => "Follow language",
            DateStyle::Iso => "ISO (2023-03-15)",
            DateStyle::Us => "US (03/15/2023)",
            DateStyle::Eu => "European (15/03/2023)",
            DateStyle::Long => "Long (15 Mar 2023)",
        }
    }
}

/// Decimal and grouping convention. Kept apart from the currency so someone can use EUR with
/// either continental or English-language formatting.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::Model,
)]
pub enum NumberStyle {
    /// `1,234,567.89`
    #[default]
    DecimalPoint,
    /// `1.234.567,89`
    DecimalComma,
}

impl NumberStyle {
    pub const ALL: [Self; 2] = [Self::DecimalPoint, Self::DecimalComma];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::DecimalPoint => "1,234,567.89",
            Self::DecimalComma => "1.234.567,89",
        }
    }

    #[must_use]
    pub fn amount(self, symbol: &str) -> String {
        format!("{symbol}{}", self.label())
    }
}

/// The day a week starts on, for every calendar grid and week number.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::Model,
)]
pub enum FirstDayOfWeek {
    #[default]
    Monday,
    Sunday,
    Saturday,
}

impl FirstDayOfWeek {
    pub const ALL: [Self; 3] = [Self::Monday, Self::Sunday, Self::Saturday];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Monday => "Monday",
            Self::Sunday => "Sunday",
            Self::Saturday => "Saturday",
        }
    }

    /// The same day as a `chrono` weekday.
    #[must_use]
    pub fn weekday(self) -> chrono::Weekday {
        match self {
            Self::Monday => chrono::Weekday::Mon,
            Self::Sunday => chrono::Weekday::Sun,
            Self::Saturday => chrono::Weekday::Sat,
        }
    }
}

/// The formats a person chose in Region & Formats. `number` is `None` until they choose a number
/// style; the language's own convention applies then. The serde names are the settings file's,
/// and the app host's door carries the same shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RegionFormats {
    pub date: DateStyle,
    pub time: TimeStyle,
    pub number: Option<NumberStyle>,
    pub first_day: FirstDayOfWeek,
}

impl RegionFormats {
    /// Read the `locale` settings section: a missing or unreadable key keeps its default, key by
    /// key.
    #[must_use]
    pub fn from_section(mut section: serde_json::Map<String, serde_json::Value>) -> Self {
        fn take<T: serde::de::DeserializeOwned>(
            section: &mut serde_json::Map<String, serde_json::Value>,
            key: &str,
        ) -> Option<T> {
            section
                .remove(key)
                .and_then(|value| serde_json::from_value(value).ok())
        }
        Self {
            date: take(&mut section, "date_format").unwrap_or_default(),
            time: take(&mut section, "time_format").unwrap_or_default(),
            number: take(&mut section, "number_style"),
            first_day: take(&mut section, "first_day_of_week").unwrap_or_default(),
        }
    }
}
