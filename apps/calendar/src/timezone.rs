//! Deterministic IANA time-zone resolution for Calendar's wall-time model.
//!
//! Events retain their authored wall time plus TZID.  Whenever an absolute
//! instant is needed (interop, reminders, ordering across zones), this module
//! resolves it with one cross-device rule: choose the earlier instant in a DST
//! fold, and advance through a DST gap to the first valid wall minute. An
//! elapsed duration may explicitly select the later fold for its end, through
//! [`endpoint_utc`], without changing the authored timezone or recurrence anchor.
//! Neither rule consults host state.
//!
//! Files from elsewhere name zones in other ways: Outlook and Exchange by
//! their Windows name, other publishers behind a path prefix, and some only
//! by the rules of a `VTIMEZONE` they carry. [`zone_from_tzid`] and
//! [`zone_following`] find the IANA zone each stands for.

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc,
    Weekday,
};
use chrono_tz::Tz;

mod windows_zones;

const MAX_GAP_MINUTES: i64 = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Exact,
    AmbiguousEarlier,
    ShiftedForward { minutes: i64 },
}

/// Validate a persisted zone spelling. `floating` deliberately has no instant;
/// `UTC` is accepted as the canonical zero-offset spelling.
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

/// Resolve one named-zone wall time to UTC using Calendar's fold/gap policy.
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

/// Resolve an endpoint whose persisted fold choice may select the later instant.
/// Ordinary authored times still use the earlier-fold/gap policy.
pub fn endpoint_utc(zone: &str, wall: NaiveDateTime, later: bool) -> Option<DateTime<Utc>> {
    if later
        && let LocalResult::Ambiguous(a, b) = zone.parse::<Tz>().ok()?.from_local_datetime(&wall)
    {
        return Some(a.max(b).with_timezone(&Utc));
    }
    Some(wall_to_utc(zone, wall)?.0)
}

/// Compare endpoints in their authored zone, not by ambiguous wall ordering.
pub fn valid_span(zone: &str, start: NaiveDateTime, end: NaiveDateTime, later: bool) -> bool {
    if zone == "floating" {
        return !later && end >= start;
    }
    wall_to_utc(zone, start)
        .zip(endpoint_utc(zone, end, later))
        .is_some_and(|((start, _), end)| end >= start)
}

/// Return the canonical wall time after applying the deterministic gap rule.
/// Fold inputs remain textually unchanged; their earlier-instant choice is made
/// by [`wall_to_utc`] whenever an instant is requested.
pub fn normalize_wall(zone: &str, wall: NaiveDateTime) -> Option<(NaiveDateTime, Resolution)> {
    let (instant, resolution) = wall_to_utc(zone, wall)?;
    if zone == "UTC" {
        return Some((wall, resolution));
    }
    let zone = zone.parse::<Tz>().ok()?;
    Some((instant.with_timezone(&zone).naive_local(), resolution))
}

/// The IANA zone an iCalendar TZID names: an IANA name, the Windows name
/// Outlook and Exchange write (`W. Europe Standard Time`), or an IANA name
/// behind a publisher's prefix (`/mozilla.org/20050126_1/Europe/Berlin`).
/// `None` when it names none of these.
pub fn zone_from_tzid(tzid: &str) -> Option<String> {
    let tzid = tzid.trim().trim_matches('"').trim();
    let named = |name: &str| canonical_zone(name).filter(|zone| zone != "floating");
    named(tzid)
        .or_else(|| windows_zone(tzid).and_then(named))
        .or_else(|| {
            let mut rest = tzid;
            while let Some((_, tail)) = rest.split_once('/') {
                if let Some(zone) = named(tail) {
                    return Some(zone);
                }
                rest = tail;
            }
            None
        })
}

/// The IANA zone of a Windows time-zone name, as CLDR maps it.
pub fn windows_zone(name: &str) -> Option<&'static str> {
    fn folded(text: &str) -> impl Iterator<Item = u8> + '_ {
        text.bytes().map(|byte| byte.to_ascii_lowercase())
    }
    let table = windows_zones::WINDOWS_ZONES;
    let index = table
        .binary_search_by(|(windows, _)| folded(windows).cmp(folded(name)))
        .ok()?;
    table.get(index).map(|(_, iana)| *iana)
}

/// Which day of its month a yearly zone transition falls on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnsetDay {
    /// The nth weekday of the month; negative counts from its end
    /// (`BYDAY=-1SU`, the last Sunday).
    Nth(i8, Weekday),
    /// The first such weekday on or after a day of the month
    /// (`BYMONTHDAY=8,9,10,11,12,13,14;BYDAY=SU`).
    OnOrAfter(u32, Weekday),
    /// A fixed day of the month.
    Fixed(u32),
}

/// A `FREQ=YEARLY` transition rule of a `VTIMEZONE` observance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YearlyRule {
    pub month: u32,
    pub day: OnsetDay,
    /// The rule's last onset, as written (UTC or wall time alike).
    pub until: Option<NaiveDateTime>,
}

impl YearlyRule {
    /// The date this rule's transition falls on in `year`, if it has one.
    fn date_in(self, year: i32) -> Option<NaiveDate> {
        let first = NaiveDate::from_ymd_opt(year, self.month, 1)?;
        let next = if self.month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(year, self.month + 1, 1)?
        };
        let on_or_after = |date: NaiveDate, weekday: Weekday| {
            let ahead =
                (7 + weekday.num_days_from_monday() - date.weekday().num_days_from_monday()) % 7;
            date + Duration::days(i64::from(ahead))
        };
        let date = match self.day {
            OnsetDay::Fixed(day) => NaiveDate::from_ymd_opt(year, self.month, day)?,
            OnsetDay::OnOrAfter(day, weekday) => {
                on_or_after(NaiveDate::from_ymd_opt(year, self.month, day)?, weekday)
            }
            OnsetDay::Nth(n, weekday) if n > 0 => {
                on_or_after(first, weekday) + Duration::weeks(i64::from(n) - 1)
            }
            OnsetDay::Nth(n, weekday) => {
                let last = next.pred_opt()?;
                let back = (7 + last.weekday().num_days_from_monday()
                    - weekday.num_days_from_monday())
                    % 7;
                last - Duration::days(i64::from(back)) - Duration::weeks(-i64::from(n) - 1)
            }
        };
        (date >= first && date < next).then_some(date)
    }
}

/// One STANDARD or DAYLIGHT observance of an iCalendar `VTIMEZONE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observance {
    /// Its first onset, as a wall time in `offset_from`.
    pub onset: NaiveDateTime,
    /// The UTC offsets before and after each onset, in seconds.
    pub offset_from: i32,
    pub offset_to: i32,
    pub rule: Option<YearlyRule>,
    /// Further onsets, as wall times in `offset_from`.
    pub rdates: Vec<NaiveDateTime>,
}

impl Observance {
    /// Each onset of this observance in `years`, as UTC instants.
    fn onsets(
        &self,
        years: std::ops::RangeInclusive<i32>,
    ) -> impl Iterator<Item = NaiveDateTime> + '_ {
        let ruled = self.rule.into_iter().flat_map(move |rule| {
            years.clone().filter_map(move |year| {
                let wall = rule.date_in(year)?.and_time(self.onset.time());
                (wall >= self.onset && rule.until.is_none_or(|until| wall <= until)).then_some(wall)
            })
        });
        std::iter::once(self.onset)
            .chain(self.rdates.iter().copied())
            .chain(ruled)
            .map(|wall| wall - Duration::seconds(i64::from(self.offset_from)))
    }
}

/// The UTC offset, in seconds, `observances` give at the UTC instant `at`:
/// the one the latest onset by then set, or before any onset the one the
/// earliest leaves.
fn offset_following(observances: &[Observance], at: NaiveDateTime) -> Option<i32> {
    let years = at.year() - 1..=at.year();
    let latest = observances
        .iter()
        .flat_map(|observance| {
            observance
                .onsets(years.clone())
                .filter(|onset| *onset <= at)
                .map(|onset| (onset, observance.offset_to))
        })
        .max_by_key(|(onset, _)| *onset);
    latest.map(|(_, offset)| offset).or_else(|| {
        observances
            .iter()
            .min_by_key(|observance| observance.onset)
            .map(|observance| observance.offset_from)
    })
}

/// The IANA zone whose offsets in `year` are the ones `observances` give,
/// for a `VTIMEZONE` whose TZID names no zone (`Customized Time Zone`,
/// `(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna`). The
/// zones Windows names come first; among zones that fit, one whose city the
/// TZID mentions, then one that changes its clocks at the very instants the
/// rules do (US Eastern time, not Havana, which moves its clocks on the same
/// days two hours earlier). `None` when no zone keeps those offsets.
pub fn zone_following(observances: &[Observance], year: i32, tzid: &str) -> Option<String> {
    let noon =
        |month| NaiveDate::from_ymd_opt(year, month, 1).and_then(|date| date.and_hms_opt(12, 0, 0));
    let mut samples: Vec<NaiveDateTime> = [noon(1), noon(7)].into_iter().flatten().collect();
    // Either side of each onset, to the minute: a zone that keeps the same
    // offsets but moves its clocks hours apart differs only here.
    let mut instants = Vec::new();
    for observance in observances {
        for onset in observance.onsets(year..=year) {
            if onset.year() == year {
                samples.push(onset - Duration::days(1));
                samples.push(onset + Duration::days(1));
                instants.push(onset - Duration::minutes(1));
                instants.push(onset + Duration::minutes(1));
            }
        }
    }
    let wanted_at = |at: &[NaiveDateTime]| {
        at.iter()
            .map(|at| offset_following(observances, *at).map(|offset| (*at, offset)))
            .collect::<Option<Vec<_>>>()
    };
    let wanted = wanted_at(&samples)?;
    let wanted_instants = wanted_at(&instants).unwrap_or_default();
    let keeps = |zone: &Tz, wanted: &[(NaiveDateTime, i32)]| {
        wanted.iter().all(|(at, offset)| {
            zone.offset_from_utc_datetime(at).fix().local_minus_utc() == *offset
        })
    };
    let candidates = windows_zones::WINDOWS_ZONES
        .iter()
        .filter_map(|(_, iana)| iana.parse::<Tz>().ok())
        .chain(chrono_tz::TZ_VARIANTS.iter().copied());
    let mut first = None;
    let mut on_time = None;
    for zone in candidates.filter(|zone| keeps(zone, &wanted)) {
        let city = zone
            .name()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .replace('_', " ");
        if city.len() > 2 && tzid.contains(&city) {
            return canonical_zone(zone.name());
        }
        if on_time.is_none() && keeps(&zone, &wanted_instants) {
            on_time = Some(zone);
        }
        first.get_or_insert(zone);
    }
    on_time
        .or(first)
        .and_then(|zone| canonical_zone(zone.name()))
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

        let wall = at((2026, 10, 25), 2, 30);
        let (instant, resolution) = wall_to_utc("Europe/Berlin", wall).unwrap();
        assert_eq!(instant.naive_utc(), at((2026, 10, 25), 0, 30));
        assert_eq!(resolution, Resolution::AmbiguousEarlier);

        // An invalid zone is refused instead of becoming floating.
        assert_eq!(canonical_zone("Mars/Olympus"), None);
        assert_eq!(wall_to_utc("Mars/Olympus", at((2026, 1, 1), 12, 0)), None);
    }

    /// Every Windows name maps to a zone the database knows, and the table
    /// stays sorted for its binary search.
    #[test]
    fn windows_names_are_sorted_and_resolve() {
        let table = windows_zones::WINDOWS_ZONES;
        let folded = |name: &str| name.to_ascii_lowercase();
        assert!(
            table
                .windows(2)
                .all(|pair| folded(pair[0].0) < folded(pair[1].0))
        );
        for (windows, iana) in table {
            assert!(canonical_zone(iana).is_some(), "{windows} maps to {iana}");
            assert_eq!(windows_zone(windows), Some(*iana));
        }
        assert_eq!(
            zone_from_tzid("W. Europe Standard Time").as_deref(),
            Some("Europe/Berlin")
        );
        assert_eq!(
            zone_from_tzid("w. europe standard time").as_deref(),
            Some("Europe/Berlin")
        );
        assert_eq!(
            zone_from_tzid("\"Tokyo Standard Time\"").as_deref(),
            Some("Asia/Tokyo")
        );
        assert_eq!(zone_from_tzid("UTC").as_deref(), Some("UTC"));
        assert_eq!(
            zone_from_tzid("/mozilla.org/20050126_1/America/New_York").as_deref(),
            Some("America/New_York")
        );
        assert_eq!(zone_from_tzid("Customized Time Zone"), None);
        assert_eq!(zone_from_tzid("floating"), None);
    }
}
