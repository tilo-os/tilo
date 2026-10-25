//! A value that carries its measure, its provenance and its as-of.
//!
//! A number on screen is a claim. [`Fact`] keeps the three things that make
//! the claim honest together, so an app cannot render the number without also
//! having the answer to "measured how?", "from where?" and "when?":
//!
//! - a [`Measure`] names what the number counts (`Memory in use`, `Memory
//!   (RSS)`, `Packets dropped`), and its unit when it has one;
//! - a source names where the reading came from (`this device`, the system
//!   journal, a repository manifest);
//! - an as-of instant makes the reading age on screen, and a reading older
//!   than its own expected interval is [stale](Measured::is_stale).
//!
//! `Unknown` is a designed state, not zero: [`Measured::unknown`] carries the
//! reason (and the app can offer the action that would resolve it) and renders
//! an em dash. A number is never a stand-in for "we did not measure".
//!
//! The type is a value, not a widget: it is what a view renders. Use
//! [`Measured::value_or_dash`] in the value position, [`Measured::caption`] under it
//! (provenance is always the caption line, never beside the value), and
//! [`Measured::is_stale`] to dim the provenance rather than the value.
//!
//! ```
//! use std::time::Duration;
//! use tilo_ui::fact::{Fact, Measure};
//! use tilo_ui::time::{SystemTime, UNIX_EPOCH};
//!
//! let at = UNIX_EPOCH + Duration::from_secs(120);
//! let memory = Measured::measured(
//!     Measure::new("Memory in use").unit("GiB"),
//!     "8.6",
//!     "this device",
//!     at,
//!     Duration::from_secs(5),
//! );
//! assert_eq!(memory.value_or_dash(), "8.6");
//! assert!(!memory.is_stale(at + Duration::from_secs(2)));
//! ```

use crate::time::{Duration, SystemTime};

/// What a value counts, and the unit it counts in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Measure {
    label: String,
    unit: Option<String>,
}

impl Measure {
    /// A measure named in the person's words: `Memory in use`, `Packets
    /// dropped`, `Memory (RSS)`.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            unit: None,
        }
    }

    /// The same measure with the unit it is read in (`GiB`, `%`, `/s`).
    #[must_use]
    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    /// The measure's name. Two values with different names are different
    /// claims even when both read "Memory".
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The unit, when the value has one.
    #[must_use]
    pub fn unit_suffix(&self) -> Option<&str> {
        self.unit.as_deref()
    }
}

/// One value as the app knows it: a measurement with provenance, or the
/// reason it has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Measured {
    measure: Measure,
    state: State,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Measured {
        value: String,
        source: String,
        as_of: SystemTime,
        expected: Option<Duration>,
    },
    Unknown {
        reason: String,
    },
}

impl Measured {
    /// A reading of `value` in `measure`, taken at `as_of` from `source`.
    /// `expected` is how long the reading is good for before it is stale;
    /// `None` means the app has no interval to judge it by.
    #[must_use]
    pub fn measured(
        measure: Measure,
        value: impl Into<String>,
        source: impl Into<String>,
        as_of: SystemTime,
        expected: Option<Duration>,
    ) -> Self {
        Self {
            measure,
            state: State::Measured {
                value: value.into(),
                source: source.into(),
                as_of,
                expected,
            },
        }
    }

    /// The designed "we did not measure this" state: an em dash and why.
    #[must_use]
    pub fn unknown(measure: Measure, reason: impl Into<String>) -> Self {
        Self {
            measure,
            state: State::Unknown {
                reason: reason.into(),
            },
        }
    }

    /// What is being claimed.
    #[must_use]
    pub fn measure(&self) -> &Measure {
        &self.measure
    }

    /// The measured value, or `None` when nothing was measured. Never `Some`
    /// for an unknown fact: zero is a measurement like any other.
    #[must_use]
    pub fn value(&self) -> Option<&str> {
        match &self.state {
            State::Measured { value, .. } => Some(value),
            State::Unknown { .. } => None,
        }
    }

    /// The value in a value position: the reading, or the em dash that means
    /// "not known" (never a zero).
    #[must_use]
    pub fn value_or_dash(&self) -> &str {
        self.value().unwrap_or("\u{2014}")
    }

    /// Where the reading came from, when it was measured.
    #[must_use]
    pub fn source(&self) -> Option<&str> {
        match &self.state {
            State::Measured { source, .. } => Some(source),
            State::Unknown { .. } => None,
        }
    }

    /// When the reading was taken.
    #[must_use]
    pub fn as_of(&self) -> Option<SystemTime> {
        match &self.state {
            State::Measured { as_of, .. } => Some(*as_of),
            State::Unknown { .. } => None,
        }
    }

    /// Why the fact is unknown, when it is.
    #[must_use]
    pub fn unknown_reason(&self) -> Option<&str> {
        match &self.state {
            State::Unknown { reason } => Some(reason),
            State::Measured { .. } => None,
        }
    }

    /// How old the reading is at `now`, never negative.
    #[must_use]
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        self.as_of()
            .map(|as_of| now.duration_since(as_of).unwrap_or(Duration::ZERO))
    }

    /// Whether the reading is older than the interval it was measured for.
    /// An unknown fact is stale by definition; a fact with no expected
    /// interval is never judged stale.
    #[must_use]
    pub fn is_stale(&self, now: SystemTime) -> bool {
        match &self.state {
            State::Unknown { .. } => true,
            State::Measured {
                as_of, expected, ..
            } => expected
                .is_some_and(|expected| now.duration_since(*as_of).is_ok_and(|age| age > expected)),
        }
    }

    /// The caption line under the value: provenance, age and — when it has
    /// outlived its interval — that it is stale. Unknown facts say why and
    /// nothing else.
    ///
    /// Captions are English fallbacks for a view with no catalog entry. An
    /// app with localized copy can build the same line from
    /// [`source`](Self::source), [`age`](Self::age) and
    /// [`is_stale`](Self::is_stale).
    #[must_use]
    pub fn caption(&self, now: SystemTime) -> String {
        match &self.state {
            State::Unknown { reason } => format!("not measured \u{b7} {reason}"),
            State::Measured {
                source,
                as_of,
                expected,
                ..
            } => {
                let age = age_label(now.duration_since(*as_of).unwrap_or(Duration::ZERO));
                let mut caption = if source.is_empty() {
                    format!("measured {age} ago")
                } else {
                    format!("measured {age} ago \u{b7} {source}")
                };
                if expected.is_some_and(|expected| {
                    now.duration_since(*as_of).is_ok_and(|age| age > expected)
                }) {
                    caption.push_str(" \u{b7} stale");
                }
                caption
            }
        }
    }
}

/// An age as a person reads it: `2 s`, `4 min`, `3 h`, `2 d`.
fn age_label(age: Duration) -> String {
    let seconds = age.as_secs();
    if seconds < 60 {
        format!("{seconds} s")
    } else if seconds < 60 * 60 {
        format!("{} min", seconds / 60)
    } else if seconds < 24 * 60 * 60 {
        format!("{} h", seconds / (60 * 60))
    } else {
        format!("{} d", seconds / (24 * 60 * 60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::UNIX_EPOCH;

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn a_measurement_keeps_its_measure_source_and_age() {
        let fact = Measured::measured(
            Measure::new("Memory in use").unit("GiB"),
            "8.6",
            "this device",
            at(100),
            Some(Duration::from_secs(5)),
        );
        assert_eq!(fact.measure().label(), "Memory in use");
        assert_eq!(fact.measure().unit_suffix(), Some("GiB"));
        assert_eq!(fact.value(), Some("8.6"));
        assert_eq!(fact.value_or_dash(), "8.6");
        assert_eq!(fact.caption(at(102)), "measured 2 s ago \u{b7} this device");
        assert!(!fact.is_stale(at(102)));
        assert!(fact.is_stale(at(106)));
        assert_eq!(
            fact.caption(at(106)),
            "measured 6 s ago \u{b7} this device \u{b7} stale"
        );
    }

    #[test]
    fn an_unknown_fact_is_an_em_dash_with_a_reason_never_a_zero() {
        let fact = Measured::unknown(
            Measure::new("Memory (RSS)"),
            "the process list is unavailable",
        );
        assert_eq!(fact.value(), None);
        assert_eq!(fact.value_or_dash(), "\u{2014}");
        assert!(fact.is_stale(at(1)));
        assert_eq!(
            fact.caption(at(1)),
            "not measured \u{b7} the process list is unavailable"
        );
    }

    #[test]
    fn two_measures_with_the_same_word_are_still_different_claims() {
        let card = Measured::measured(
            Measure::new("Memory in use").unit("GiB"),
            "8.6",
            "system information service",
            at(100),
            None,
        );
        let row = Measured::measured(
            Measure::new("Memory (with children)").unit("GiB"),
            "10.4",
            "process table",
            at(100),
            None,
        );
        assert_ne!(card.measure(), row.measure());
    }

    #[test]
    fn an_age_reads_in_the_largest_whole_unit() {
        assert_eq!(age_label(Duration::from_secs(2)), "2 s");
        assert_eq!(age_label(Duration::from_secs(125)), "2 min");
        assert_eq!(age_label(Duration::from_secs(3 * 60 * 60)), "3 h");
        assert_eq!(age_label(Duration::from_secs(2 * 24 * 60 * 60)), "2 d");
    }
}
