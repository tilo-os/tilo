//! A moment written into rich text: a run marked [`MARK`], valued with the moment
//! and how a reader is shown it (`{"at":"<RFC 3339>","show":"relative"}`). The run
//! is the moment as written; every other reader shows it in their own zone and
//! language.

pub const MARK: &str = "time";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Show {
    #[default]
    Absolute,
    Relative,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineTime {
    /// The moment, in RFC 3339.
    pub at: String,
    pub show: Show,
}

impl InlineTime {
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
