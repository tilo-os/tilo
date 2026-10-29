//! A moment written into rich text: a run marked [`MARK`], valued with the moment and how a
//! reader is shown it (`{"at":"<RFC 3339>","show":"relative"}`). The run is the moment as it
//! was written, for a reader that shows text as it is; every other reader shows it in their own
//! zone and language (the toolkit's `rich_doc::time_label`).

/// The mark a time's run carries.
pub const MARK: &str = "time";

/// How an inline time reads for the person reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Show {
    /// The date and time of day in the reader's zone and formats.
    #[default]
    Absolute,
    /// How long ago, or how far ahead: `3 min ago`, `in 2 hr`.
    Relative,
    /// What is left until it, ticking; once past, how long ago.
    Countdown,
}

impl Show {
    fn name(self) -> &'static str {
        match self {
            Self::Absolute => "absolute",
            Self::Relative => "relative",
            Self::Countdown => "countdown",
        }
    }
}

/// A moment in rich text, as its mark names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineTime {
    /// The moment, in RFC 3339.
    pub at: String,
    pub show: Show,
}

impl InlineTime {
    /// The time a [`MARK`] mark's value names, when it names one.
    #[must_use]
    pub fn of_mark(value: &str) -> Option<Self> {
        let json: serde_json::Value = serde_json::from_str(value).ok()?;
        let at = json.get("at")?.as_str()?.to_owned();
        let show = match json.get("show").and_then(serde_json::Value::as_str) {
            Some("relative") => Show::Relative,
            Some("countdown") => Show::Countdown,
            _ => Show::Absolute,
        };
        Some(Self { at, show })
    }

    /// The [`MARK`] mark's value naming it.
    #[must_use]
    pub fn mark_value(&self) -> String {
        serde_json::json!({ "at": self.at, "show": self.show.name() }).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_round_trips_through_its_mark() {
        let time = InlineTime {
            at: "2026-10-29T12:00:00Z".into(),
            show: Show::Countdown,
        };
        assert_eq!(InlineTime::of_mark(&time.mark_value()), Some(time));
        assert_eq!(InlineTime::of_mark("not json"), None);
        assert_eq!(
            InlineTime::of_mark(r#"{"at":"2026-10-29T12:00:00Z"}"#).map(|t| t.show),
            Some(Show::Absolute)
        );
    }
}
