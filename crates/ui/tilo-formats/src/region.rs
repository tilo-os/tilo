//! The person's region and formats: the `locale` section of their settings, which every app
//! follows and the Settings app's "Region & Formats" panel edits, and the formats every date and
//! clock reads in.

use crate::TimeStyle;

/// How dates render: the shared `locale.date_format` setting. The serde names are the exact
/// strings the settings file uses (`Language`/`ISO`/`US`/`EU`/`Long`). Until the person picks
/// one, dates follow the language ([`DateStyle::Language`]); ISO is a choice like the others.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DateStyle {
    /// The language's own convention: `Mar 15, 2023` in English, `15.03.2023` in German.
    #[default]
    #[serde(rename = "Language")]
    #[cfg_attr(feature = "model", model(rename = "Language"))]
    Language,
    /// `2023-03-15`
    #[serde(rename = "ISO")]
    #[cfg_attr(feature = "model", model(rename = "ISO"))]
    Iso,
    /// `03/15/2023`
    #[serde(rename = "US")]
    #[cfg_attr(feature = "model", model(rename = "US"))]
    Us,
    /// `15/03/2023`
    #[serde(rename = "EU")]
    #[cfg_attr(feature = "model", model(rename = "EU"))]
    Eu,
    /// `15 Mar 2023`, the month named in the language.
    #[serde(rename = "Long")]
    #[cfg_attr(feature = "model", model(rename = "Long"))]
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
        let day = jiff::civil::date(2023, 3, 15);
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
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

    /// The same day as a weekday.
    #[must_use]
    pub fn weekday(self) -> jiff::civil::Weekday {
        match self {
            Self::Monday => jiff::civil::Weekday::Monday,
            Self::Sunday => jiff::civil::Weekday::Sunday,
            Self::Saturday => jiff::civil::Weekday::Saturday,
        }
    }
}

/// The formats a person chose in Region & Formats. `number` is `None` until they choose a number
/// style; the language's own convention applies then. The serde names are the settings file's,
/// and the app host's API carries the same shape.
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

    /// The formats as the `locale` settings section keeps them, which [`Self::from_section`]
    /// reads back: a number style not chosen yet is `null`, so writing the keys over a section
    /// that held one leaves the language's convention.
    #[must_use]
    pub fn to_section(&self) -> serde_json::Map<String, serde_json::Value> {
        let value = |value: serde_json::Result<serde_json::Value>| value.unwrap_or_default();
        serde_json::Map::from_iter([
            (
                "date_format".to_owned(),
                value(serde_json::to_value(self.date)),
            ),
            (
                "time_format".to_owned(),
                value(serde_json::to_value(self.time)),
            ),
            (
                "number_style".to_owned(),
                value(serde_json::to_value(self.number)),
            ),
            (
                "first_day_of_week".to_owned(),
                value(serde_json::to_value(self.first_day)),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_formats_read_back_from_the_section_they_write() {
        let formats = RegionFormats {
            date: DateStyle::Eu,
            time: TimeStyle::H24,
            number: Some(NumberStyle::DecimalComma),
            first_day: FirstDayOfWeek::Sunday,
        };
        let section = formats.to_section();
        assert_eq!(section["date_format"], "EU");
        assert_eq!(section["time_format"], "H24");
        assert_eq!(RegionFormats::from_section(section), formats);
        let unchosen = RegionFormats::default();
        assert_eq!(RegionFormats::from_section(unchosen.to_section()), unchosen);
    }
}
