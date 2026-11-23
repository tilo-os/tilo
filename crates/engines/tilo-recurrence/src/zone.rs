//! Deterministic IANA time-zone resolution for a wall-time calendar.
//!
//! Stored occurrences are wall times plus a zone identifier, never an offset:
//! a series at 09:00 stays at 09:00 when its zone changes offset, so the
//! instant is only resolved when something needs it (interop, ordering,
//! reminders). This module owns that one resolution rule so every device
//! resolves a stored rule identically without consulting host state: choose
//! the earlier instant in a fold, and advance through a gap to the first
//! valid wall minute.

use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::{AmbiguousOffset, TimeZone};

/// How long the resolver walks forward out of a zone's skipped hour.
const MAX_GAP_MINUTES: i64 = 180;

/// What the resolver had to do to make an authored wall time real.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Exact,
    AmbiguousEarlier,
    ShiftedForward { minutes: i64 },
}

/// The zone named `name` (`Europe/Vienna`, any letter case) from the database built into Tilo,
/// so every device reads a named zone the same way whatever its host has installed.
///
/// A thin release build of an app that links the database's engine library
/// (`libtilo_timezone_engine.so`) reads it from there: `jiff-tzdb` is linked from that library,
/// so no app carries a copy.
#[must_use]
pub fn named(name: &str) -> Option<TimeZone> {
    let (name, data) = jiff_tzdb::get(name)?;
    TimeZone::tzif(name, data).ok()
}

/// The database's own spelling of the zone named `name`.
#[must_use]
pub fn canonical(name: &str) -> Option<&'static str> {
    jiff_tzdb::get(name).map(|(name, _)| name)
}

/// Every zone name in the database.
pub fn available() -> impl Iterator<Item = &'static str> {
    jiff_tzdb::available()
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
        canonical(value).map(str::to_owned)
    }
}

/// Resolve one named-zone wall time to UTC using the fold/gap policy above.
#[must_use]
pub fn wall_to_utc(zone: &str, wall: DateTime) -> Option<(Timestamp, Resolution)> {
    if zone == "UTC" {
        return Some((
            wall.to_zoned(TimeZone::UTC).ok()?.timestamp(),
            Resolution::Exact,
        ));
    }
    let zone = named(zone)?;
    let resolve = |wall: DateTime| {
        let local = zone.to_ambiguous_timestamp(wall);
        let gap = matches!(local.offset(), AmbiguousOffset::Gap { .. });
        let fold = matches!(local.offset(), AmbiguousOffset::Fold { .. });
        (!gap)
            .then(|| local.earlier().ok().map(|at| (at, fold)))
            .flatten()
    };
    if let Some((instant, fold)) = resolve(wall) {
        let resolution = if fold {
            Resolution::AmbiguousEarlier
        } else {
            Resolution::Exact
        };
        return Some((instant, resolution));
    }
    for minutes in 1..=MAX_GAP_MINUTES {
        let shifted = wall
            .checked_add(jiff::SignedDuration::from_mins(minutes))
            .ok()?;
        if let Some((instant, _)) = resolve(shifted) {
            return Some((instant, Resolution::ShiftedForward { minutes }));
        }
    }
    None
}

/// The instant `wall` names in `zone`, for a reader with no host clock to ask: `floating` (a
/// wall time with no zone of its own) and a zone this build does not know read as UTC.
#[must_use]
pub fn wall_instant(zone: &str, wall: DateTime) -> Timestamp {
    wall_to_utc(zone, wall).map_or_else(
        || {
            wall.to_zoned(TimeZone::UTC)
                .map_or(Timestamp::UNIX_EPOCH, |at| at.timestamp())
        },
        |(instant, _)| instant,
    )
}

/// Return the canonical wall time after applying the deterministic gap rule.
/// Fold inputs remain textually unchanged; their earlier-instant choice is made
/// by [`wall_to_utc`] whenever an instant is requested.
#[must_use]
pub fn normalize_wall(zone: &str, wall: DateTime) -> Option<(DateTime, Resolution)> {
    let (instant, resolution) = wall_to_utc(zone, wall)?;
    if zone == "UTC" {
        return Some((wall, resolution));
    }
    Some((instant.to_zoned(named(zone)?).datetime(), resolution))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(date: (i16, i8, i8), hour: i8, minute: i8) -> DateTime {
        DateTime::new(date.0, date.1, date.2, hour, minute, 0, 0).unwrap()
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
        assert_eq!(
            instant.to_zoned(TimeZone::UTC).datetime(),
            at((2026, 10, 25), 0, 30)
        );
        assert_eq!(resolution, Resolution::AmbiguousEarlier);

        // An invalid zone is refused instead of becoming floating.
        assert_eq!(canonical_zone("Mars/Olympus"), None);
        assert_eq!(wall_to_utc("Mars/Olympus", at((2026, 1, 1), 12, 0)), None);
    }

    /// A reminder of a record with no time of day counts from the start of its day in the
    /// person's zone; a zone this build does not know reads as UTC rather than dropping it.
    #[test]
    fn a_wall_time_resolves_to_the_instant_a_reader_can_count_from() {
        let midnight = at((2026, 10, 27), 0, 0);
        assert_eq!(
            wall_instant("America/Los_Angeles", midnight).to_string(),
            "2026-10-27T07:00:00Z"
        );
        assert_eq!(
            wall_instant("floating", midnight).to_string(),
            "2026-10-27T00:00:00Z"
        );
        assert_eq!(
            wall_instant("Mars/Olympus", midnight).to_string(),
            "2026-10-27T00:00:00Z"
        );
    }
}
