//! How old the forecast on screen is, as the page says it.
//!
//! A snapshot records when it was fetched ([`crate::model::WeatherSnapshot`]'s
//! `fetched_at`), and a restored one can be days old: the forecast is cached
//! on the device, and only an *observed* failed update raises the notice, which
//! a snapshot restored from storage never sees. So the page always says when
//! the forecast it shows was fetched: a relative age while that is still
//! recent, the clock time once it is older, and a date once it is from another
//! day — in the theme's warning treatment, and saying "last updated", once the
//! reading can no longer be the current hour at all.
//!
//! The sentence is a reading of the forecast, not decoration: the accessible
//! summary carries it too ([`crate::core::detail`]).

use chrono::NaiveDateTime;

use crate::i18n::{arg, t_args};

/// Past this age the forecast on screen is no longer the current hour:
/// Open-Meteo refreshes its current conditions hourly, so two forecast hours
/// of drift is the most the page can honestly call current.
pub(crate) const STALE_AFTER_SECS: i64 = 2 * 60 * 60;
/// Within this age the line counts minutes ("5 min ago"); past it, it names
/// the clock time the forecast was fetched at.
const RECENT_SECS: i64 = 60 * 60;

/// The page's freshness line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Updated {
    /// The sentence as the page shows it and reads it out.
    pub(crate) text: String,
    /// The forecast is older than it should be to call current.
    pub(crate) stale: bool,
}

/// The freshness line for a snapshot fetched at `fetched_at` (wall-clock
/// epoch seconds) read at `now`, or `None` when there is nothing honest to
/// say: a capture fixture's zero timestamp was never fetched, so its age
/// would be invented.
pub(crate) fn updated(fetched_at: u64, now: NaiveDateTime) -> Option<Updated> {
    if fetched_at == 0 {
        return None;
    }
    let at = chrono::DateTime::from_timestamp(i64::try_from(fetched_at).ok()?, 0)?
        .with_timezone(&chrono::Local)
        .naive_local();
    Some(updated_at(at, now))
}

/// [`updated`] for a fetch time already on the local wall clock: pure, so
/// every wording is testable without a clock or a time zone.
pub(crate) fn updated_at(at: NaiveDateTime, now: NaiveDateTime) -> Updated {
    // A fetch stamp ahead of the clock (a device whose time was corrected)
    // reads as "just now" rather than as a negative age.
    let age = (now - at).num_seconds().max(0);
    let stale = age >= STALE_AFTER_SECS;
    let (key, when) = if age < RECENT_SECS {
        (
            if stale {
                "weather-outdated-ago"
            } else {
                "weather-updated-ago"
            },
            ui::format::relative(age),
        )
    } else if at.date() == now.date() {
        (
            if stale {
                "weather-outdated-clock"
            } else {
                "weather-updated-clock"
            },
            ui::format::time(&at.time()),
        )
    } else {
        (
            if stale {
                "weather-outdated-date"
            } else {
                "weather-updated-date"
            },
            ui::format::timestamp(&at),
        )
    };
    Updated {
        text: t_args(key, &[arg("when", when)]),
        stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .and_then(|date| date.and_hms_opt(h, mi, 0))
            .expect("a real moment")
    }

    #[test]
    fn a_fresh_forecast_counts_the_minutes_since_it_was_fetched() {
        let now = at(2026, 6, 30, 14, 30);
        let fresh = updated_at(at(2026, 6, 30, 14, 25), now);
        assert_eq!(fresh.text, "Updated 5 min ago");
        assert!(!fresh.stale);
        // Just fetched, and a stamp the clock has not caught up with.
        assert_eq!(updated_at(now, now).text, "Updated just now");
        assert_eq!(
            updated_at(at(2026, 6, 30, 14, 31), now).text,
            "Updated just now"
        );
    }

    #[test]
    fn an_older_forecast_names_the_clock_time_it_was_fetched_at() {
        let now = at(2026, 6, 30, 14, 30);
        let fetched = at(2026, 6, 30, 13, 0);
        let older = updated_at(fetched, now);
        assert_eq!(
            older.text,
            format!("Updated at {}", ui::format::time(&fetched.time()))
        );
        assert!(!older.stale, "ninety minutes is still current");
    }

    #[test]
    fn a_stale_forecast_says_so_and_says_when_it_was_last_updated() {
        let now = at(2026, 6, 30, 14, 30);
        // Two hours old: the reading can no longer be the current hour.
        let stale = updated_at(at(2026, 6, 30, 12, 30), now);
        assert!(stale.stale, "{STALE_AFTER_SECS} s is the threshold");
        assert!(stale.text.starts_with("Last updated"), "{}", stale.text);
        // Another morning's snapshot names its clock time, and a snapshot
        // from another day names its date.
        let morning = updated_at(at(2026, 6, 30, 7, 0), now);
        assert_eq!(
            morning.text,
            format!(
                "Last updated at {}, which is out of date",
                ui::format::time(&at(2026, 6, 30, 7, 0).time())
            )
        );
        let days = updated_at(at(2026, 6, 27, 9, 15), now);
        assert!(days.stale);
        assert_eq!(
            days.text,
            format!(
                "Last updated {}, which is out of date",
                ui::format::timestamp(&at(2026, 6, 27, 9, 15))
            )
        );
    }

    #[test]
    fn a_capture_fixture_makes_no_claim_about_its_age() {
        assert_eq!(updated(0, at(2026, 6, 30, 14, 30)), None);
    }
}
