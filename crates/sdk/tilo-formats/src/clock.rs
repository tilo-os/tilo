//! The clock as a person reads it: the `locale.time_format` setting and how a time of day reads in
//! it.

use crate::Words;

/// How times of day render: the shared `locale.time_format` setting. Until the person picks a
/// clock, it follows the language ([`TimeStyle::Language`]: 12-hour in US English, 24-hour in
/// German).
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
pub enum TimeStyle {
    /// The language's own clock: `2:05 PM` in US English, `14:05` in German.
    #[default]
    #[model(rename = "Language")]
    Language,
    /// `14:05`
    #[model(rename = "H24")]
    H24,
    /// `2:05 PM`
    #[model(rename = "H12")]
    H12,
}

impl TimeStyle {
    pub const ALL: [TimeStyle; 3] = [TimeStyle::Language, TimeStyle::H24, TimeStyle::H12];

    /// The clock this style shows: itself, or for [`TimeStyle::Language`] the clock of the
    /// language `words` read in.
    #[must_use]
    pub fn resolved(self, words: &dyn Words) -> Self {
        match self {
            TimeStyle::Language => {
                if words.word("format-clock-hours", &[]).trim() == "12" {
                    TimeStyle::H12
                } else {
                    TimeStyle::H24
                }
            }
            style => style,
        }
    }

    /// Whether this style shows a 24-hour clock.
    #[must_use]
    pub fn is_24_hour(self, words: &dyn Words) -> bool {
        self.resolved(words) == TimeStyle::H24
    }

    /// `time` in this style.
    #[must_use]
    pub fn format(self, time: &impl chrono::Timelike, words: &dyn Words) -> String {
        self.format_hm(time.hour(), time.minute(), words)
    }

    /// `hour:minute` (hour `0..24`) in this style.
    #[must_use]
    pub fn format_hm(self, hour: u32, minute: u32, words: &dyn Words) -> String {
        match self.resolved(words) {
            TimeStyle::Language | TimeStyle::H24 => format!("{hour:02}:{minute:02}"),
            TimeStyle::H12 => {
                let half = words.word(
                    if hour % 24 < 12 {
                        "format-time-am"
                    } else {
                        "format-time-pm"
                    },
                    &[],
                );
                let hour = match hour % 12 {
                    0 => 12,
                    hour => hour,
                };
                format!("{hour}:{minute:02} {half}")
            }
        }
    }

    /// A worked example, 14:05 in this style.
    #[must_use]
    pub fn example(self, words: &dyn Words) -> String {
        self.format_hm(14, 5, words)
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TimeStyle::Language => "Follow language",
            TimeStyle::H24 => "24-hour (14:05)",
            TimeStyle::H12 => "12-hour (2:05 PM)",
        }
    }
}
