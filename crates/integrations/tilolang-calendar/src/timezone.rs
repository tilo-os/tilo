//! Deterministic IANA time-zone resolution for Calendar's wall-time model.
// Calendar arithmetic is bounded by the values a calendar holds: a time of
// day, a duration within one, a count of occurrences. Panic-producing lints
// stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "bounded date, time and duration arithmetic on calendar values"
)]

use jiff::civil::{Date, DateTime, Weekday};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::{SignedDuration, Span, Timestamp};

pub use tilo_recurrence::zone::{
    Resolution, available, canonical, canonical_zone, named, normalize_wall, wall_to_utc,
};

mod windows_zones;

pub fn endpoint_utc(zone: &str, wall: DateTime, later: bool) -> Option<Timestamp> {
    if later {
        let local = named(zone)?.to_ambiguous_timestamp(wall);
        if matches!(local.offset(), AmbiguousOffset::Fold { .. }) {
            return local.later().ok();
        }
    }
    Some(wall_to_utc(zone, wall)?.0)
}

pub fn valid_span(zone: &str, start: DateTime, end: DateTime, later: bool) -> bool {
    if zone == "floating" {
        return !later && end >= start;
    }
    wall_to_utc(zone, start)
        .zip(endpoint_utc(zone, end, later))
        .is_some_and(|((start, _), end)| end >= start)
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnsetDay {
    Nth(i8, Weekday),
    OnOrAfter(i8, Weekday),
    Fixed(i8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YearlyRule {
    pub month: i8,
    pub day: OnsetDay,
    pub until: Option<DateTime>,
}

impl YearlyRule {
    fn date_in(self, year: i16) -> Option<Date> {
        let first = Date::new(year, self.month, 1).ok()?;
        let next = first.last_of_month().tomorrow().ok()?;
        let on_or_after = |date: Date, weekday: Weekday| date.nth_weekday(1, weekday).ok();
        let on_or_after = |date: Date, weekday: Weekday| {
            if date.weekday() == weekday {
                Some(date)
            } else {
                on_or_after(date, weekday)
            }
        };
        let date = match self.day {
            OnsetDay::Fixed(day) => Date::new(year, self.month, day).ok()?,
            OnsetDay::OnOrAfter(day, weekday) => {
                on_or_after(Date::new(year, self.month, day).ok()?, weekday)?
            }
            OnsetDay::Nth(n, weekday) => first.nth_weekday_of_month(n, weekday).ok()?,
        };
        (date >= first && date < next).then_some(date)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observance {
    pub onset: DateTime,
    pub offset_from: i32,
    pub offset_to: i32,
    pub rule: Option<YearlyRule>,
    pub rdates: Vec<DateTime>,
}

impl Observance {
    fn onsets(&self, years: std::ops::RangeInclusive<i16>) -> impl Iterator<Item = DateTime> + '_ {
        let ruled = self.rule.into_iter().flat_map(move |rule| {
            years.clone().filter_map(move |year| {
                let wall = rule.date_in(year)?.to_datetime(self.onset.time());
                (wall >= self.onset && rule.until.is_none_or(|until| wall <= until)).then_some(wall)
            })
        });
        std::iter::once(self.onset)
            .chain(self.rdates.iter().copied())
            .chain(ruled)
            .filter_map(|wall| {
                wall.checked_sub(SignedDuration::from_secs(i64::from(self.offset_from)))
                    .ok()
            })
    }
}

fn offset_following(observances: &[Observance], at: DateTime) -> Option<i32> {
    let years = at.year().saturating_sub(1)..=at.year();
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

pub fn zone_following(observances: &[Observance], year: i16, tzid: &str) -> Option<String> {
    let noon = |month| {
        Date::new(year, month, 1)
            .ok()
            .map(|date| date.at(12, 0, 0, 0))
    };
    let mut samples: Vec<DateTime> = [noon(1), noon(7)].into_iter().flatten().collect();
    // Either side of each onset, to the minute: a zone that keeps the same
    // offsets but moves its clocks hours apart differs only here.
    let mut instants = Vec::new();
    for observance in observances {
        for onset in observance.onsets(year..=year) {
            if onset.year() == year {
                let day = Span::new().days(1);
                let minute = SignedDuration::from_mins(1);
                samples.extend(
                    [onset.checked_sub(day), onset.checked_add(day)]
                        .into_iter()
                        .flatten(),
                );
                instants.extend(
                    [onset.checked_sub(minute), onset.checked_add(minute)]
                        .into_iter()
                        .flatten(),
                );
            }
        }
    }
    let wanted_at = |at: &[DateTime]| {
        at.iter()
            .map(|at| offset_following(observances, *at).map(|offset| (*at, offset)))
            .collect::<Option<Vec<_>>>()
    };
    let wanted = wanted_at(&samples)?;
    let wanted_instants = wanted_at(&instants).unwrap_or_default();
    let keeps = |zone: &TimeZone, wanted: &[(DateTime, i32)]| {
        wanted.iter().all(|(at, offset)| {
            at.to_zoned(TimeZone::UTC)
                .is_ok_and(|at| zone.to_offset(at.timestamp()).seconds() == *offset)
        })
    };
    let candidates = windows_zones::WINDOWS_ZONES
        .iter()
        .map(|(_, iana)| *iana)
        .chain(available())
        .filter_map(|name| named(name).map(|zone| (name, zone)));
    let mut first = None;
    let mut on_time = None;
    for (name, zone) in candidates.filter(|(_, zone)| keeps(zone, &wanted)) {
        let city = name
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .replace('_', " ");
        if city.len() > 2 && tzid.contains(&city) {
            return canonical_zone(name);
        }
        if on_time.is_none() && keeps(&zone, &wanted_instants) {
            on_time = Some(name);
        }
        first.get_or_insert(name);
    }
    on_time.or(first).and_then(canonical_zone)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(date: (i16, i8, i8), hour: i8, minute: i8) -> DateTime {
        Date::new(date.0, date.1, date.2)
            .unwrap()
            .at(hour, minute, 0, 0)
    }

    tilo_test_env::scenario! {
        /// Time zones: the wall times a clock change skips or repeats, and the names Windows gives.
        fn zones_resolve_through_gaps_folds_and_windows_names;

        phase a_span_through_the_fold_ends_on_the_pass_it_names;
        {
            // 02:30 comes twice in Berlin that night: at 00:30 and at 01:30 UTC.
            let (start, end) = (at((2026, 10, 25), 2, 45), at((2026, 10, 25), 2, 30));
            let resolved =
                |later| endpoint_utc("Europe/Berlin", end, later).map(|instant| instant.to_zoned(TimeZone::UTC).datetime());
            assert_eq!(resolved(false), Some(at((2026, 10, 25), 0, 30)));
            assert_eq!(resolved(true), Some(at((2026, 10, 25), 1, 30)));
            // An end the clock shows before the start comes after it only on the second pass.
            assert!(valid_span("Europe/Berlin", start, end, true));
            assert!(!valid_span("Europe/Berlin", start, end, false));

            // Outside a fold there is one pass, whichever is asked for.
            let noon = at((2026, 6, 15), 12, 0);
            let summer =
                |later| endpoint_utc("Europe/Berlin", noon, later).map(|instant| instant.to_zoned(TimeZone::UTC).datetime());
            assert_eq!(summer(false), Some(at((2026, 6, 15), 10, 0)));
            assert_eq!(summer(true), summer(false));
            assert!(valid_span("Europe/Berlin", noon, noon, false));
            assert!(!valid_span(
                "Europe/Berlin",
                noon,
                noon - jiff::SignedDuration::from_mins(1),
                false
            ));

            // A floating time has no second pass, and no zone nobody knows has a span.
            assert!(valid_span("floating", noon, noon, false));
            assert!(!valid_span("floating", noon, noon, true));
            assert!(!valid_span("floating", start, end, false));
            assert!(!valid_span("Mars/Olympus", noon, noon, false));
            assert_eq!(endpoint_utc("Mars/Olympus", noon, true), None);
        }

        phase windows_names_are_sorted_and_resolve;
        {
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
}
