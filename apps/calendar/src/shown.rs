//! Events as the person looking at them sees them: each occurrence at its
//! instant, in the viewer's time zone.
//!
//! An event keeps the wall time and zone it was authored in (a New York
//! meeting at 09:00 `America/New_York`, a Google event at 07:00 `UTC`). The
//! views place it at that instant on the viewer's clock: 15:00 and 09:00 for
//! someone in Berlin in summer. Floating events and all-day dates have no
//! instant and keep their wall time and dates everywhere.

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::model::Event;

/// The zone the views are drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewer {
    /// A named zone, or `None` for whatever the system clock reports.
    zone: Option<Tz>,
}

impl Viewer {
    /// The device's zone: `$TZ` when it names one, else what
    /// `/etc/localtime` links to, else the system clock's offsets.
    pub fn device() -> Self {
        Self {
            zone: device_zone_name().and_then(|name| name.parse::<Tz>().ok()),
        }
    }

    /// A fixed zone, as a capture or a test pins it.
    pub fn named(zone: &str) -> Self {
        Self {
            zone: zone.parse::<Tz>().ok(),
        }
    }

    /// The zone's IANA name, which new events are authored in; `None` when
    /// the system names no zone.
    pub fn zone_name(&self) -> Option<&'static str> {
        self.zone.map(Tz::name)
    }

    pub fn wall(&self, instant: DateTime<Utc>) -> NaiveDateTime {
        match self.zone {
            Some(zone) => instant.with_timezone(&zone).naive_local(),
            None => instant.with_timezone(&chrono::Local).naive_local(),
        }
    }

    /// The instant the viewer's clock shows `wall` at: the earlier one in a
    /// repeated hour, the first real minute after a skipped one.
    pub fn instant(&self, wall: NaiveDateTime) -> Option<DateTime<Utc>> {
        match self.zone {
            Some(zone) => crate::timezone::wall_to_utc(zone.name(), wall).map(|(at, _)| at),
            None => match chrono::Local.from_local_datetime(&wall) {
                LocalResult::Single(at) => Some(at.with_timezone(&Utc)),
                LocalResult::Ambiguous(a, b) => Some(a.min(b).with_timezone(&Utc)),
                LocalResult::None => chrono::Local
                    .from_local_datetime(&(wall + Duration::hours(1)))
                    .earliest()
                    .map(|at| at.with_timezone(&Utc)),
            },
        }
    }

    /// `wall`, authored on the viewer's clock, as a wall time in `zone`.
    pub fn to_zone(&self, wall: NaiveDateTime, zone: &str) -> NaiveDateTime {
        if zone == "floating" || Some(zone) == self.zone_name() {
            return wall;
        }
        let Some(instant) = self.instant(wall) else {
            return wall;
        };
        if zone == "UTC" {
            return instant.naive_utc();
        }
        zone.parse::<Tz>()
            .map_or(wall, |zone| instant.with_timezone(&zone).naive_local())
    }
}

/// The IANA name of the device's zone, if the system names one this
/// calendar's zone database knows.
fn device_zone_name() -> Option<String> {
    let name = ui::time::local_zone_name()?;
    crate::timezone::canonical_zone(&name).filter(|zone| zone != "floating")
}

/// One occurrence of an event as the viewer sees it.
#[derive(Clone, Copy, Debug)]
pub struct Shown<'a> {
    pub event: &'a Event,
    /// The date the occurrence was generated for, in the event's own zone:
    /// what identifies it in the series.
    pub original: NaiveDate,
    /// Its start and end on the viewer's clock (dates at midnight for an
    /// all-day event, the end exclusive).
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    /// How long it lasts: the instants' difference, which a repeated or
    /// skipped wall hour makes differ from the walls'.
    pub elapsed: Duration,
}

impl Shown<'_> {
    /// Whether the occurrence is on `day`: it starts that day, or runs into
    /// it.
    pub fn on(&self, day: NaiveDate) -> bool {
        let from = day.and_time(NaiveTime::MIN);
        let to = from + Duration::days(1);
        self.start < to && (self.end > from || self.start >= from)
    }

    /// The minutes of `day` it covers, `[start, end)`, for a timed one.
    pub fn minutes_on(&self, day: NaiveDate) -> (i64, i64) {
        let from = day.and_time(NaiveTime::MIN);
        let start = (self.start - from).num_minutes().clamp(0, 24 * 60);
        let end = if self.start.date() == day {
            start + self.elapsed.num_minutes().max(0)
        } else {
            (self.end - from).num_minutes()
        };
        (start, end.clamp(start, 24 * 60))
    }
}

/// One generated occurrence's wall interval in the event's own zone. An
/// override that moved or resized the occurrence is respected; otherwise it
/// keeps the series' duration.
pub fn wall_interval(event: &Event, date: NaiveDate) -> (NaiveDateTime, NaiveDateTime) {
    if event.all_day {
        let start = date.and_time(NaiveTime::MIN);
        return (start, start + Duration::days(event.all_day_span_days()));
    }
    let edit = override_on(event, date);
    let start = edit
        .and_then(|edit| edit.start)
        .unwrap_or_else(|| date.and_time(event.start.time()));
    let end = edit
        .and_then(|edit| edit.end)
        .unwrap_or_else(|| event.occurrence_end(start));
    (start, end)
}

/// The override that applies to the occurrence on `date`: the one for that
/// original date, or one moved onto it.
fn override_on(event: &Event, date: NaiveDate) -> Option<&crate::model::OccurrenceOverride> {
    event.occurrence_overrides.iter().find(|edit| {
        edit.original_date == date || edit.start.is_some_and(|start| start.date() == date)
    })
}

/// The instants an occurrence spans; `None` for floating and all-day ones.
pub fn instants(event: &Event, date: NaiveDate) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    if event.all_day {
        return None;
    }
    let (start, end) = wall_interval(event, date);
    let (start, _) = crate::timezone::wall_to_utc(&event.timezone, start)?;
    let edit = override_on(event, date);
    let end = if let Some(edit) = edit.filter(|edit| edit.end.is_some()) {
        crate::timezone::endpoint_utc(&event.timezone, end, edit.end_later_fold)?
    } else if event.end_later_fold {
        start.checked_add_signed(event.elapsed_duration())?
    } else {
        crate::timezone::endpoint_utc(&event.timezone, end, false)?
    };
    Some((start, end))
}

/// Every occurrence of `event` the viewer sees between `from` and `to`
/// (inclusive dates on the viewer's clock), in order.
pub fn occurrences<'a>(
    event: &'a Event,
    from: NaiveDate,
    to: NaiveDate,
    viewer: &Viewer,
) -> Vec<Shown<'a>> {
    // Zones put one instant on dates up to a day apart, and an all-day
    // event that began earlier still covers the window.
    let reach = if event.all_day {
        event.all_day_span_days()
    } else {
        2
    };
    let window_from = from.and_time(NaiveTime::MIN);
    let window_to = (to + Duration::days(1)).and_time(NaiveTime::MIN);
    let mut out = Vec::new();
    for date in event.occurrences_in(from - Duration::days(reach), to + Duration::days(2)) {
        let (wall_start, wall_end) = wall_interval(event, date);
        let original = override_on(event, date).map_or(date, |edit| edit.original_date);
        let (start, end, elapsed) = match instants(event, date) {
            Some((start, end)) => (viewer.wall(start), viewer.wall(end), end - start),
            None => (wall_start, wall_end, wall_end - wall_start),
        };
        let shown = Shown {
            event,
            original,
            start,
            end,
            elapsed,
        };
        let empty = start == end;
        if start < window_to && (end > window_from || (empty && start >= window_from)) {
            out.push(shown);
        }
    }
    out.sort_by_key(|shown| shown.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Recurrence;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
    }

    fn event(zone: &str, start: NaiveDateTime, minutes: i64) -> Event {
        Event {
            id: uuid::Uuid::new_v4(),
            title: "Call".into(),
            start,
            end: start + Duration::minutes(minutes),
            timezone: zone.into(),
            recurrence_interval: 1,
            ..Event::default()
        }
    }

    #[test]
    fn events_are_placed_at_their_instant_on_the_viewers_clock() {
        let berlin = Viewer::named("Europe/Berlin");
        let day = NaiveDate::from_ymd_opt(2026, 6, 30).unwrap();

        let utc = event("UTC", at(2026, 6, 30, 7, 0), 60);
        let shown = occurrences(&utc, day, day, &berlin);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].start, at(2026, 6, 30, 9, 0));
        assert_eq!(shown[0].end, at(2026, 6, 30, 10, 0));

        let ny = event("America/New_York", at(2026, 6, 30, 9, 0), 30);
        let shown = occurrences(&ny, day, day, &berlin);
        assert_eq!(shown[0].start, at(2026, 6, 30, 15, 0));
        assert_eq!(shown[0].original, day);

        let floating = event("floating", at(2026, 6, 30, 9, 0), 30);
        assert_eq!(
            occurrences(&floating, day, day, &berlin)[0].start,
            at(2026, 6, 30, 9, 0)
        );
    }

    #[test]
    fn an_evening_elsewhere_lands_on_the_next_day_here() {
        let tokyo = Viewer::named("Asia/Tokyo");
        let ny = event("America/New_York", at(2026, 6, 30, 20, 0), 60);
        let jun30 = NaiveDate::from_ymd_opt(2026, 6, 30).unwrap();
        let jul1 = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        assert!(occurrences(&ny, jun30, jun30, &tokyo).is_empty());
        let shown = occurrences(&ny, jul1, jul1, &tokyo);
        assert_eq!(shown[0].start, at(2026, 7, 1, 9, 0));
        assert_eq!(
            shown[0].original, jun30,
            "the series still knows it by its own date"
        );
    }

    #[test]
    fn repeating_events_follow_their_own_zones_daylight_saving() {
        // New York changes its clocks a week before Berlin does in
        // October: a weekly 09:00 New York call is 15:00 in Berlin, then
        // 14:00 for the week between, then 15:00 again.
        let berlin = Viewer::named("Europe/Berlin");
        let mut weekly = event("America/New_York", at(2026, 10, 19, 9, 0), 60);
        weekly.recurrence = Recurrence::Weekly;
        let from = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        let hours: Vec<_> = occurrences(&weekly, from, to, &berlin)
            .iter()
            .map(|shown| chrono::Timelike::hour(&shown.start))
            .collect();
        assert_eq!(hours, vec![15, 14, 15]);
    }

    #[test]
    fn a_late_event_runs_into_the_next_day() {
        let berlin = Viewer::named("Europe/Berlin");
        let late = event("Europe/Berlin", at(2026, 6, 30, 23, 0), 120);
        let jul1 = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let shown = occurrences(&late, jul1, jul1, &berlin);
        assert_eq!(shown.len(), 1);
        assert!(shown[0].on(jul1));
        assert_eq!(shown[0].minutes_on(jul1), (0, 60));
        assert_eq!(
            shown[0].minutes_on(NaiveDate::from_ymd_opt(2026, 6, 30).unwrap()),
            (23 * 60, 24 * 60)
        );
    }

    #[test]
    fn a_time_authored_here_is_kept_as_the_same_instant_in_the_events_zone() {
        let berlin = Viewer::named("Europe/Berlin");
        assert_eq!(
            berlin.to_zone(at(2026, 6, 30, 15, 0), "America/New_York"),
            at(2026, 6, 30, 9, 0)
        );
        assert_eq!(
            berlin.to_zone(at(2026, 6, 30, 9, 0), "UTC"),
            at(2026, 6, 30, 7, 0)
        );
        assert_eq!(
            berlin.to_zone(at(2026, 6, 30, 9, 0), "floating"),
            at(2026, 6, 30, 9, 0)
        );
    }
}
