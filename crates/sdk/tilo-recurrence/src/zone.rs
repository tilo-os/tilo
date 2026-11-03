//! Deterministic IANA time-zone resolution for a wall-time calendar.
//!
//! Stored occurrences are wall times plus a zone identifier, never an offset:
//! a series at 09:00 stays at 09:00 when its zone changes offset, so the
//! instant is only resolved when something needs it (interop, ordering,
//! reminders). This module owns that one resolution rule so every device
//! resolves a stored rule identically without consulting host state: choose
//! the earlier instant in a fold, and advance through a gap to the first
//! valid wall minute.

use chrono::{DateTime, Duration, LocalResult, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

/// How long the resolver walks forward out of a zone's skipped hour.
const MAX_GAP_MINUTES: i64 = 180;

/// What the resolver had to do to make an authored wall time real.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Exact,
    AmbiguousEarlier,
    ShiftedForward { minutes: i64 },
}

/// Validate a persisted zone spelling. `floating` deliberately has no instant;
/// `UTC` is accepted as the canonical zero-offset spelling.
#[must_use]
pub fn canonical_zone(value: &str) -> Option<String> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("floating") {
        Some("floating".into())
    } else if value.eq_ignore_ascii_case("UTC") || value == "Etc/UTC" {
        Some("UTC".into())
    } else {
        value.parse::<Tz>().ok().map(|zone| zone.name().to_string())
    }
}

/// Resolve one named-zone wall time to UTC using the fold/gap policy above.
#[must_use]
pub fn wall_to_utc(zone: &str, wall: NaiveDateTime) -> Option<(DateTime<Utc>, Resolution)> {
    if zone == "UTC" {
        return Some((Utc.from_utc_datetime(&wall), Resolution::Exact));
    }
    let zone = zone.parse::<Tz>().ok()?;
    match zone.from_local_datetime(&wall) {
        LocalResult::Single(value) => Some((value.with_timezone(&Utc), Resolution::Exact)),
        LocalResult::Ambiguous(earlier, later) => Some((
            earlier.min(later).with_timezone(&Utc),
            Resolution::AmbiguousEarlier,
        )),
        LocalResult::None => {
            for minutes in 1..=MAX_GAP_MINUTES {
                let shifted = wall.checked_add_signed(Duration::minutes(minutes))?;
                match zone.from_local_datetime(&shifted) {
                    LocalResult::Single(value) => {
                        return Some((
                            value.with_timezone(&Utc),
                            Resolution::ShiftedForward { minutes },
                        ));
                    }
                    LocalResult::Ambiguous(earlier, later) => {
                        return Some((
                            earlier.min(later).with_timezone(&Utc),
                            Resolution::ShiftedForward { minutes },
                        ));
                    }
                    LocalResult::None => {}
                }
            }
            None
        }
    }
}

/// Return the canonical wall time after applying the deterministic gap rule.
/// Fold inputs remain textually unchanged; their earlier-instant choice is made
/// by [`wall_to_utc`] whenever an instant is requested.
#[must_use]
pub fn normalize_wall(zone: &str, wall: NaiveDateTime) -> Option<(NaiveDateTime, Resolution)> {
    let (instant, resolution) = wall_to_utc(zone, wall)?;
    if zone == "UTC" {
        return Some((wall, resolution));
    }
    let zone = zone.parse::<Tz>().ok()?;
    Some((instant.with_timezone(&zone).naive_local(), resolution))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(date: (i32, u32, u32), hour: u32, minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(date.0, date.1, date.2)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
    }

    #[test]
    fn berlin_dst_gap_and_fold_resolve_and_invalid_zone_is_refused() {
        // DST gap advances to the first real minute.
        let wall = at((2026, 3, 29), 2, 30);
        let (normalized, resolution) = normalize_wall("Europe/Berlin", wall).unwrap();
        assert_eq!(normalized, at((2026, 3, 29), 3, 0));
        assert_eq!(resolution, Resolution::ShiftedForward { minutes: 30 });

        // DST fold chooses the earlier instant.
        let wall = at((2026, 10, 25), 2, 30);
        let (instant, resolution) = wall_to_utc("Europe/Berlin", wall).unwrap();
        assert_eq!(instant.naive_utc(), at((2026, 10, 25), 0, 30));
        assert_eq!(resolution, Resolution::AmbiguousEarlier);

        // An invalid zone is refused instead of becoming floating.
        assert_eq!(canonical_zone("Mars/Olympus"), None);
        assert_eq!(wall_to_utc("Mars/Olympus", at((2026, 1, 1), 12, 0)), None);
    }
}
