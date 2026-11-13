//! Deterministic IANA time-zone resolution for Calendar's wall-time model.

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc,
    Weekday,
};
use chrono_tz::Tz;

pub use tilo_recurrence::zone::{Resolution, canonical_zone, normalize_wall, wall_to_utc};

mod windows_zones;

pub fn endpoint_utc(zone: &str, wall: NaiveDateTime, later: bool) -> Option<DateTime<Utc>> {
    if later
        && let LocalResult::Ambiguous(a, b) = zone.parse::<Tz>().ok()?.from_local_datetime(&wall)
    {
        return Some(a.max(b).with_timezone(&Utc));
    }
    Some(wall_to_utc(zone, wall)?.0)
}

pub fn valid_span(zone: &str, start: NaiveDateTime, end: NaiveDateTime, later: bool) -> bool {
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
    OnOrAfter(u32, Weekday),
    Fixed(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YearlyRule {
    pub month: u32,
    pub day: OnsetDay,
    pub until: Option<NaiveDateTime>,
}

impl YearlyRule {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observance {
    pub onset: NaiveDateTime,
    pub offset_from: i32,
    pub offset_to: i32,
    pub rule: Option<YearlyRule>,
    pub rdates: Vec<NaiveDateTime>,
}

impl Observance {
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
        let wall = at((2026, 3, 29), 2, 30);
        let (normalized, resolution) = normalize_wall("Europe/Berlin", wall).unwrap();
        assert_eq!(normalized, at((2026, 3, 29), 3, 0));
        assert_eq!(resolution, Resolution::ShiftedForward { minutes: 30 });

        let wall = at((2026, 10, 25), 2, 30);
        let (instant, resolution) = wall_to_utc("Europe/Berlin", wall).unwrap();
        assert_eq!(instant.naive_utc(), at((2026, 10, 25), 0, 30));
        assert_eq!(resolution, Resolution::AmbiguousEarlier);

        assert_eq!(canonical_zone("Mars/Olympus"), None);
        assert_eq!(wall_to_utc("Mars/Olympus", at((2026, 1, 1), 12, 0)), None);
    }

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
