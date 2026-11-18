//! When an event's occurrences fall, as instants and as the wall times its
//! own zone writes them: what a window draws and what a read of a span about
//! them answers.

use jiff::SignedDuration;
use jiff::civil::{Date, DateTime, Time};

use crate::model::Event;

#[derive(Clone, Copy, Debug)]
pub struct Shown<'a> {
    pub event: &'a Event,
    pub original: Date,
    pub start: DateTime,
    pub end: DateTime,
    pub elapsed: SignedDuration,
}

impl Shown<'_> {
    pub fn on(&self, day: Date) -> bool {
        let from = day.to_datetime(Time::MIN);
        let to = from + jiff::Span::new().days(1);
        self.start < to && (self.end > from || self.start >= from)
    }

    pub fn minutes_on(&self, day: Date) -> (i64, i64) {
        let from = day.to_datetime(Time::MIN);
        let start = self.start.duration_since(from).as_mins().clamp(0, 24 * 60);
        let end = if self.start.date() == day {
            start + self.elapsed.as_mins().max(0)
        } else {
            self.end.duration_since(from).as_mins()
        };
        (start, end.clamp(start, 24 * 60))
    }
}

/// One generated occurrence's wall interval in the event's own zone. An
/// override that moved or resized the occurrence is respected; otherwise it
/// keeps the series' duration.
pub fn wall_interval(event: &Event, date: Date) -> (DateTime, DateTime) {
    if event.all_day {
        let start = date.to_datetime(Time::MIN);
        return (
            start,
            start + jiff::Span::new().days(event.all_day_span_days()),
        );
    }
    let edit = override_on(event, date);
    let start = edit
        .and_then(|edit| edit.start)
        .unwrap_or_else(|| date.to_datetime(event.start.time()));
    let end = edit
        .and_then(|edit| edit.end)
        .unwrap_or_else(|| event.occurrence_end(start));
    (start, end)
}

pub fn override_on(event: &Event, date: Date) -> Option<&crate::model::OccurrenceOverride> {
    event.occurrence_overrides.iter().find(|edit| {
        edit.original_date == date || edit.start.is_some_and(|start| start.date() == date)
    })
}

pub fn instants(event: &Event, date: Date) -> Option<(jiff::Timestamp, jiff::Timestamp)> {
    if event.all_day {
        return None;
    }
    let (start, end) = wall_interval(event, date);
    let (start, _) = crate::timezone::wall_to_utc(&event.timezone, start)?;
    let edit = override_on(event, date);
    let end = if let Some(edit) = edit.filter(|edit| edit.end.is_some()) {
        crate::timezone::endpoint_utc(&event.timezone, end, edit.end_later_fold)?
    } else if event.end_later_fold {
        start.checked_add(event.elapsed_duration()).ok()?
    } else {
        crate::timezone::endpoint_utc(&event.timezone, end, false)?
    };
    Some((start, end))
}
