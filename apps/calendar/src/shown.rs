//! Events as the person looking at them sees them: each occurrence at its
//! instant, in the viewer's time zone.

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use calendar_model::model::Event;
pub use calendar_model::shown::{Shown, instants, override_on, wall_interval};

/// Whose clock events are read on: a named zone, or this device's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Viewer {
    zone: Option<(&'static str, TimeZone)>,
}

impl Viewer {
    #[cfg(not(test))]
    pub fn device() -> Self {
        device_zone_name().map_or(Self { zone: None }, |name| Self::named(&name))
    }

    pub fn named(zone: &str) -> Self {
        Self {
            zone: crate::timezone::canonical(zone).zip(crate::timezone::named(zone)),
        }
    }

    pub fn zone_name(&self) -> Option<&'static str> {
        self.zone.as_ref().map(|(name, _)| *name)
    }

    fn time_zone(&self) -> TimeZone {
        self.zone
            .as_ref()
            .map_or_else(TimeZone::system, |(_, zone)| zone.clone())
    }

    pub fn wall(&self, instant: Timestamp) -> DateTime {
        instant.to_zoned(self.time_zone()).datetime()
    }

    pub fn instant(&self, wall: DateTime) -> Option<Timestamp> {
        match &self.zone {
            Some((name, _)) => crate::timezone::wall_to_utc(name, wall).map(|(at, _)| at),
            None => ui::clock::instant_of(wall).or_else(|| {
                ui::clock::instant_of(wall.checked_add(SignedDuration::from_hours(1)).ok()?)
            }),
        }
    }

    pub fn to_zone(&self, wall: DateTime, zone: &str) -> DateTime {
        if zone == "floating" || Some(zone) == self.zone_name() {
            return wall;
        }
        let Some(instant) = self.instant(wall) else {
            return wall;
        };
        if zone == "UTC" {
            return instant.to_zoned(TimeZone::UTC).datetime();
        }
        crate::timezone::named(zone).map_or(wall, |zone| instant.to_zoned(zone).datetime())
    }
}

#[cfg(not(test))]
fn device_zone_name() -> Option<String> {
    let name = ui::time::local_zone_name()?;
    crate::timezone::canonical_zone(&name).filter(|zone| zone != "floating")
}

pub fn occurrences<'a>(event: &'a Event, from: Date, to: Date, viewer: &Viewer) -> Vec<Shown<'a>> {
    let reach = if event.all_day {
        event.all_day_span_days()
    } else {
        2
    };
    let window_from = from.to_datetime(Time::MIN);
    let window_to = (to + jiff::Span::new().days(1)).to_datetime(Time::MIN);
    let mut out = Vec::new();
    for date in event.occurrences_in(
        from - jiff::Span::new().days(reach),
        to + jiff::Span::new().days(2),
    ) {
        let (wall_start, wall_end) = wall_interval(event, date);
        let original = override_on(event, date).map_or(date, |edit| edit.original_date);
        let (start, end, elapsed) = match instants(event, date) {
            Some((start, end)) => (
                viewer.wall(start),
                viewer.wall(end),
                end.duration_since(start),
            ),
            None => (wall_start, wall_end, wall_end.duration_since(wall_start)),
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

    fn at(y: i16, m: i8, d: i8, h: i8, min: i8) -> DateTime {
        Date::new(y, m, d).unwrap().at(h, min, 0, 0)
    }

    fn event(zone: &str, start: DateTime, minutes: i64) -> Event {
        Event {
            id: uuid::Uuid::new_v4(),
            title: "Call".into(),
            start,
            end: start + jiff::SignedDuration::from_mins(minutes),
            timezone: zone.into(),
            recurrence_interval: 1,
            ..Event::default()
        }
    }

    tilo_test_env::scenario! {
        /// An event as its viewer sees it: the same instant, on the viewer's clock and days.
        fn a_viewer_sees_events_at_their_instant_on_their_own_clock;

        phase events_are_placed_at_their_instant_on_the_viewers_clock;
        {
            let berlin = Viewer::named("Europe/Berlin");
            let day = Date::new(2026, 6, 30).unwrap();

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

        phase an_evening_elsewhere_lands_on_the_next_day_here;
        {
            let tokyo = Viewer::named("Asia/Tokyo");
            let ny = event("America/New_York", at(2026, 6, 30, 20, 0), 60);
            let jun30 = Date::new(2026, 6, 30).unwrap();
            let jul1 = Date::new(2026, 7, 1).unwrap();
            assert!(occurrences(&ny, jun30, jun30, &tokyo).is_empty());
            let shown = occurrences(&ny, jul1, jul1, &tokyo);
            assert_eq!(shown[0].start, at(2026, 7, 1, 9, 0));
            assert_eq!(
                shown[0].original, jun30,
                "the series still knows it by its own date"
            );
        }

        phase repeating_events_follow_their_own_zones_daylight_saving;
        {
            let berlin = Viewer::named("Europe/Berlin");
            let mut weekly = event("America/New_York", at(2026, 10, 19, 9, 0), 60);
            weekly.recurrence = Recurrence::Weekly;
            let from = Date::new(2026, 10, 19).unwrap();
            let to = Date::new(2026, 11, 8).unwrap();
            let hours: Vec<_> = occurrences(&weekly, from, to, &berlin)
                .iter()
                .map(|shown| shown.start.hour())
                .collect();
            assert_eq!(hours, vec![15, 14, 15]);
        }

        phase a_late_event_runs_into_the_next_day;
        {
            let berlin = Viewer::named("Europe/Berlin");
            let late = event("Europe/Berlin", at(2026, 6, 30, 23, 0), 120);
            let jul1 = Date::new(2026, 7, 1).unwrap();
            let shown = occurrences(&late, jul1, jul1, &berlin);
            assert_eq!(shown.len(), 1);
            assert!(shown[0].on(jul1));
            assert_eq!(shown[0].minutes_on(jul1), (0, 60));
            assert_eq!(
                shown[0].minutes_on(Date::new(2026, 6, 30).unwrap()),
                (23 * 60, 24 * 60)
            );
        }

        phase a_time_authored_here_is_kept_as_the_same_instant_in_the_events_zone;
        {
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
}
