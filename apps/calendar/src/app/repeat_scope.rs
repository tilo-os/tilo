//! Which events a drag on a repeating event changes.
//!
//! Dragging an occurrence of a repeating event (moving it, or its end) asks
//! "Only this event" or "All future events", as Apple Calendar, Fantastical
//! and Notion Calendar do. Only this event becomes an override of that one
//! date. All future events splits the series at the occurrence: the old
//! series ends the day before (its RRULE gains an UNTIL), and a new series
//! starts at the occurrence's new time, carrying on with what was left of
//! the old one (its end, its remaining count, its later exceptions).

use chrono::{Duration, NaiveDate, NaiveDateTime};
use uuid::Uuid;
use vello::kurbo::Rect;

use super::{CalendarApp, min_to_time};
use crate::model::{Calendar, Event, Id, OccurrenceOverride, Recurrence};

/// The rows of the question, in order: only this event, all future events.
pub(super) fn choices() -> [String; 2] {
    [
        crate::resource::t("calendar-repeat-only-this"),
        crate::resource::t("calendar-repeat-all-future"),
    ]
}

/// What the question is called for assistive technology.
pub(super) fn label() -> String {
    crate::resource::t("calendar-repeat-change")
}

/// Why the events did not move.
fn failed(error: &str) -> String {
    crate::resource::t_args(
        "calendar-repeat-failed",
        &[("error", ui::i18n::ArgValue::Text(error.to_owned()))],
    )
}

/// What a drag asked of an occurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DragChange {
    /// Start on `day` at `start_min` on the viewer's clock, keeping the
    /// length.
    Move { day: NaiveDate, start_min: i64 },
    /// End at `end` on the viewer's clock.
    Resize { end: NaiveDateTime },
}

/// A drag on an occurrence of a repeating event, waiting for the person to
/// say which events it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PendingRepeatChange {
    pub id: Id,
    pub original: NaiveDate,
    pub change: DragChange,
}

impl CalendarApp {
    /// Apply a drag on the occurrence of `id` on `original`: right away for
    /// an event that does not repeat, after asking for one that does.
    pub(super) fn change_occurrence(
        &mut self,
        id: Id,
        original: NaiveDate,
        change: DragChange,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let repeats = self
            .model
            .event(id)
            .is_some_and(|event| !event.all_day && event.recurrence != Recurrence::None);
        let pending = PendingRepeatChange {
            id,
            original,
            change,
        };
        if !repeats {
            self.apply_repeat_change(pending, false, doc);
            return;
        }
        self.pending_repeat = Some(pending);
        // The question opens where the occurrence was let go.
        let anchor = Rect::from_center_size(self.last_mouse, (1.0, 1.0));
        self.repeat_scope_menu.set_viewport(self.rect);
        self.repeat_scope_menu.open_at(anchor, None);
        self.dirty = true;
    }

    /// The person's answer: `following` for all future events, else only
    /// this one.
    pub(super) fn apply_repeat_change(
        &mut self,
        pending: PendingRepeatChange,
        following: bool,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let PendingRepeatChange {
            id,
            original,
            change,
        } = pending;
        if following {
            self.change_following(id, original, change, doc);
            return;
        }
        match change {
            DragChange::Move { day, start_min } => {
                self.move_event(id, original, (day, start_min), doc);
            }
            DragChange::Resize { end } => self.resize_event(id, original, end, doc),
        }
    }

    /// Input while the question is open: it owns it. Returns whether it was
    /// open.
    pub(super) fn handle_repeat_scope_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.repeat_scope_menu.is_open() {
            return false;
        }
        ui::Widget::handle_event(&mut self.repeat_scope_menu, event, ctx);
        let choice = self.repeat_scope_menu.take_selection();
        if !self.repeat_scope_menu.is_open() {
            // Answered, or dismissed: a dismissed drag changes nothing.
            if let (Some(pending), Some(choice)) = (self.pending_repeat.take(), choice) {
                self.apply_repeat_change(pending, choice == 1, &mut ctx.state);
            }
            self.pending_repeat = None;
        }
        self.dirty = true;
        true
    }

    /// The new start and end, on the event's own clock, that `change` gives
    /// the occurrence of `before` on `original`.
    fn changed_interval(
        &self,
        before: &Event,
        original: NaiveDate,
        change: DragChange,
    ) -> Option<(NaiveDateTime, NaiveDateTime, bool)> {
        match change {
            DragChange::Move { day, start_min } => {
                let start = self
                    .viewer
                    .to_zone(day.and_time(min_to_time(start_min)), &before.timezone);
                let (end, later_fold) =
                    Self::end_after(&before.timezone, start, before.elapsed_duration());
                Some((start, end, later_fold))
            }
            DragChange::Resize { end } => {
                let (start, _) = crate::shown::wall_interval(before, original);
                let wall_end = self.viewer.to_zone(end, &before.timezone);
                let later_fold = self.viewer.instant(end).is_some_and(|instant| {
                    crate::timezone::endpoint_utc(&before.timezone, wall_end, false)
                        .is_some_and(|earlier| earlier != instant)
                });
                (wall_end > start || later_fold).then_some((start, wall_end, later_fold))
            }
        }
    }

    /// Apply `change` to the occurrence on `original` and every later one.
    fn change_following(
        &mut self,
        id: Id,
        original: NaiveDate,
        change: DragChange,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let Some(before) = self.model.event(id).filter(|e| !e.all_day).cloned() else {
            return;
        };
        let Some((start, end, later_fold)) = self.changed_interval(&before, original, change)
        else {
            return;
        };
        let (old, new) = split_series(&before, original, (start, end, later_fold), Uuid::new_v4());
        let Some(new) = new else {
            // The first occurrence: all of them are its future.
            if self.submit_event_change(doc, &before, &old) {
                if let Some(event) = self.model.event_mut(id) {
                    *event = old;
                }
                self.dirty = true;
            }
            return;
        };
        // The old series' end and the new series, as one change: one undo
        // step, and no moment with both or neither.
        let insert = match ui::framework::Submit::<Calendar>::into_invoke(Calendar::NEW_EVENT.with(
            crate::resource::calendar::NewEvent {
                event: crate::actions::EventData::from_event(&new),
            },
        )) {
            Ok(insert) => insert,
            Err(error) => {
                self.status.show(failed(&error));
                return;
            }
        };
        let mut members: Vec<ui::tilo_resource::Invoke<Calendar>> = Calendar::paths()
            .events
            .upsert(Some(&before), &old)
            .into_iter()
            .map(ui::tilo_resource::Invoke::from)
            .collect();
        members.push(insert);
        if let Err(error) = self.try_submit(doc, ui::tilo_resource::Invoke::group(members)) {
            self.status.show(failed(&error.to_string()));
            return;
        }
        if let Some(event) = self.model.event_mut(id) {
            *event = old;
        }
        self.model.events.push(new);
        self.model.events.settle();
        self.dirty = true;
    }
}

/// Split `before` at its occurrence on `original`, which now runs
/// `start`..`end` (the event's own clock, `end` maybe the later reading of
/// a repeated hour). Gives the old series as it is to be kept, and the new
/// series `new_id` from that occurrence on; no new series when `original`
/// is the first occurrence, as the whole series then changes.
pub(super) fn split_series(
    before: &Event,
    original: NaiveDate,
    (start, end, later_fold): (NaiveDateTime, NaiveDateTime, bool),
    new_id: Id,
) -> (Event, Option<Event>) {
    let seed = before.start.date();
    // Later occurrences keep their weekday or date relative to this one.
    let shift = start.date() - original;
    let carried = |edit: &OccurrenceOverride| OccurrenceOverride {
        original_date: edit.original_date + shift,
        original_time: edit.original_time.map(|_| start.time()),
        ..edit.clone()
    };
    if original <= seed {
        let mut whole = before.clone();
        whole.start = start;
        whole.end = end;
        whole.end_later_fold = later_fold;
        // Moved to another day, the exceptions move with the dates they
        // were for. Only the time changed, they stay as they are: their
        // dates still name occurrences of the series.
        if shift != Duration::zero() {
            whole.occurrence_overrides = before
                .occurrence_overrides
                .iter()
                .filter(|edit| edit.original_date != original)
                .map(carried)
                .collect();
            whole.recurrence_exdates = before
                .recurrence_exdates
                .iter()
                .map(|date| *date + shift)
                .collect();
            whole.recurrence_until = before.recurrence_until.map(|until| until + shift);
        } else if let Some(position) = whole
            .occurrence_overrides
            .iter()
            .position(|edit| edit.original_date == original)
        {
            whole.occurrence_overrides.remove(position);
        }
        whole.occurrence_overrides.settle();
        return (whole, None);
    }
    // How many occurrences the old series keeps, for a series bounded by a
    // count: the new one takes the rest.
    let kept = before
        .recurrence_count
        .map(|_| u32::try_from(before.occurrences_before(original)).unwrap_or(u32::MAX));
    let mut old = before.clone();
    // RFC 5545 allows UNTIL or COUNT, never both.
    old.recurrence_count = None;
    old.recurrence_until = Some(
        before
            .recurrence_until
            .map_or(original - Duration::days(1), |until| {
                until.min(original - Duration::days(1))
            }),
    );
    old.occurrence_overrides
        .retain(|edit| edit.original_date < original);
    old.occurrence_overrides.settle();
    old.recurrence_exdates = before
        .recurrence_exdates
        .iter()
        .filter(|date| **date < original)
        .copied()
        .collect();

    let mut new = before.clone();
    new.id = new_id;
    new.start = start;
    new.end = end;
    new.end_later_fold = later_fold;
    new.recurrence_count = before
        .recurrence_count
        .zip(kept)
        .map(|(count, kept)| count.saturating_sub(kept).max(1));
    new.recurrence_until = before.recurrence_until.map(|until| until + shift);
    new.occurrence_overrides = before
        .occurrence_overrides
        .iter()
        .filter(|edit| edit.original_date > original)
        .map(carried)
        .collect();
    new.occurrence_overrides.settle();
    new.recurrence_exdates = before
        .recurrence_exdates
        .iter()
        .filter(|date| **date > original)
        .map(|date| *date + shift)
        .collect();
    (old, Some(new))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(m: u32, d: u32, h: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, m, d)
            .unwrap()
            .and_hms_opt(h, 0, 0)
            .unwrap()
    }

    fn date(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    /// Weekly on Mondays from 1 June 09:00, ten times; 29 June skipped, 8
    /// June and 6 July moved by an hour.
    fn weekly() -> Event {
        let moved = |day: NaiveDate| OccurrenceOverride {
            original_date: day,
            original_time: Some(at(6, 1, 9).time()),
            start: Some(day.and_hms_opt(10, 0, 0).unwrap()),
            end: Some(day.and_hms_opt(11, 0, 0).unwrap()),
            ..OccurrenceOverride::default()
        };
        Event {
            id: Uuid::new_v4(),
            title: "Planning".into(),
            start: at(6, 1, 9),
            end: at(6, 1, 10),
            timezone: "Europe/Berlin".into(),
            recurrence: Recurrence::Weekly,
            recurrence_count: Some(10),
            recurrence_exdates: [date(6, 29)].into_iter().collect(),
            occurrence_overrides: vec![moved(date(6, 8)), moved(date(7, 6))].into(),
            ..Event::default()
        }
    }

    #[test]
    fn all_future_events_split_the_series_at_the_occurrence() {
        let before = weekly();
        let new_id = Uuid::new_v4();
        // 22 June, dragged to Tuesday 23 June at 11:00.
        let (old, new) = split_series(
            &before,
            date(6, 22),
            (at(6, 23, 11), at(6, 23, 12), false),
            new_id,
        );
        let new = new.expect("a later occurrence starts a new series");

        // The old series ends the day before, by UNTIL alone.
        assert_eq!(old.id, before.id);
        assert_eq!(old.recurrence_until, Some(date(6, 21)));
        assert_eq!(old.recurrence_count, None);
        assert_eq!(old.start, before.start);
        assert_eq!(
            old.occurrences_in(date(6, 1), date(12, 31)),
            vec![date(6, 1), date(6, 8), date(6, 15)]
        );
        assert!(old.recurrence_exdates.is_empty());

        // The new series carries on from there, on Tuesdays at 11:00, with
        // what was left: seven of ten, the skipped and the moved one.
        assert_eq!(new.id, new_id);
        assert_eq!((new.start, new.end), (at(6, 23, 11), at(6, 23, 12)));
        assert_eq!(new.recurrence, Recurrence::Weekly);
        assert_eq!(new.recurrence_count, Some(7));
        assert!(new.recurrence_exdates.contains(&date(6, 30)));
        let carried: Vec<_> = new.occurrence_overrides.iter().collect();
        assert_eq!(carried.len(), 1);
        assert_eq!(carried[0].original_date, date(7, 7));
        assert_eq!(carried[0].original_time, Some(at(6, 23, 11).time()));
        assert_eq!(
            carried[0].start,
            Some(date(7, 6).and_hms_opt(10, 0, 0).unwrap())
        );
        let shown = new.occurrences_in(date(6, 1), date(12, 31));
        assert_eq!(shown.len(), 6, "seven left, one skipped: {shown:?}");
        assert_eq!(shown[0], date(6, 23));
        assert!(!shown.contains(&date(6, 30)));
    }

    #[test]
    fn all_future_events_from_the_first_move_the_whole_series() {
        let before = weekly();
        let (whole, new) = split_series(
            &before,
            date(6, 1),
            (at(6, 1, 13), at(6, 1, 14), false),
            Uuid::new_v4(),
        );
        assert!(new.is_none());
        assert_eq!(whole.id, before.id);
        assert_eq!((whole.start, whole.end), (at(6, 1, 13), at(6, 1, 14)));
        assert_eq!(whole.recurrence_count, Some(10));
        assert!(whole.recurrence_exdates.contains(&date(6, 29)));
        assert_eq!(whole.occurrence_overrides.len(), 2);
    }

    #[test]
    fn an_until_bounded_series_keeps_its_end() {
        let mut before = weekly();
        before.recurrence_count = None;
        before.recurrence_until = Some(date(8, 31));
        let (old, new) = split_series(
            &before,
            date(7, 13),
            (at(7, 13, 9), at(7, 13, 10), false),
            Uuid::new_v4(),
        );
        let new = new.unwrap();
        assert_eq!(old.recurrence_until, Some(date(7, 12)));
        assert_eq!(new.recurrence_until, Some(date(8, 31)));
        assert_eq!(new.recurrence_count, None);
    }
}
