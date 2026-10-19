//! The person's region and formats, as every formatter reads them: the
//! `locale` section of their settings (date style, 12/24-hour clock, number
//! style, first day of the week) next to the desktop's language.
//!
//! The section is read once and cached for the whole process. A host calls
//! [`refresh`] when its settings change (the toolkit's app host and the shell
//! do, on the same settings stamp that re-themes them); a change bumps
//! [`revision`], which [`crate::i18n::I18nManager::revision`] folds in, so
//! every view and retained widget that follows the language re-renders its
//! dates and numbers in the new formats without a restart.

use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use tilo_lock::RwLockRecover as _;

use crate::clock::TimeStyle;

/// How dates render: the shared `locale.date_format` setting. The serde
/// names are the exact strings the settings file uses
/// (`Language`/`ISO`/`US`/`EU`/`Long`). Until the person picks one, dates
/// follow the desktop's language ([`DateStyle::Language`]); ISO is a choice
/// like the others.
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
    /// The desktop language's own convention: `Mar 15, 2023` in English,
    /// `15.03.2023` in German.
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
    /// `15 Mar 2023`, the month named in the desktop's language.
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

    /// A worked example so the picker shows what each style produces:
    /// 15 March 2023 in this style, [`DateStyle::Language`] in the language
    /// active on the calling thread.
    #[must_use]
    pub fn example(self) -> String {
        let day = chrono::NaiveDate::from_ymd_opt(2023, 3, 15).unwrap_or_default();
        super::Formats::with(RegionFormats {
            date: self,
            ..RegionFormats::default()
        })
        .date(day)
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

/// Decimal and grouping convention. Kept apart from the currency so someone
/// can use EUR with either continental or English-language formatting.
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

/// The formats a person chose in Region & Formats. `number` is `None` until
/// they choose a number style; the language's own convention applies then.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegionFormats {
    pub date: DateStyle,
    pub time: TimeStyle,
    pub number: Option<NumberStyle>,
    pub first_day: FirstDayOfWeek,
}

impl RegionFormats {
    /// Read the `locale` settings section. Missing or unreadable keys keep
    /// their defaults, key by key.
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

    /// The person's formats from their settings, uncached.
    #[must_use]
    pub fn configured() -> Self {
        Self::from_section(settings_store::load_section("locale"))
    }
}

static CURRENT: RwLock<Option<RegionFormats>> = RwLock::new(None);
static REVISION: AtomicU64 = AtomicU64::new(0);

/// The person's formats, read from their settings on first use and kept
/// until the next [`refresh`].
#[must_use]
pub fn current() -> RegionFormats {
    if let Some(formats) = *CURRENT.read_recover() {
        return formats;
    }
    refresh();
    CURRENT.read_recover().unwrap_or_default()
}

/// Read the person's formats again. Returns whether they changed; a change
/// bumps [`revision`] so views re-render with them. Hosts call this when
/// their settings change.
pub fn refresh() -> bool {
    install(RegionFormats::configured())
}

/// Use `formats` from now on, as [`refresh`] would after reading them. For
/// hosts that hold the settings document themselves, and for tests.
pub fn install(formats: RegionFormats) -> bool {
    let mut current = CURRENT.write_recover();
    if *current == Some(formats) {
        return false;
    }
    let first = current.is_none();
    *current = Some(formats);
    if !first {
        REVISION.fetch_add(1, Ordering::AcqRel);
    }
    !first
}

/// Changes whenever the person's formats change.
#[must_use]
pub fn revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}
