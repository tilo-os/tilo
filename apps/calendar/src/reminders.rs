//! Event reminders as system notifications.

use std::collections::HashSet;
use std::hash::Hasher as _;

use ui::notify::{Notification, Notifier, NotifyError, Scheduled};

use crate::model::{Calendar, Event};
use crate::shown::Viewer;

pub(crate) const HORIZON_DAYS: i64 = 7;

pub(crate) const MAX_SCHEDULED: usize = 48;

const ALL_DAY_MORNING: jiff::civil::Time = jiff::civil::time(9, 0, 0, 0);

pub(crate) const SCHEME: &str = "tilo-calendar";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reminder {
    pub tag: String,
    pub at: jiff::Timestamp,
    pub title: String,
    pub body: String,
    pub activation: String,
}

fn tag(event: &Event, original: jiff::civil::Date) -> String {
    let mut hasher = fnv::FnvHasher::default();
    hasher.write(event.id.as_bytes());
    hasher.write(original.to_string().as_bytes());
    format!("c{:014x}", hasher.finish() & 0x00ff_ffff_ffff_ffff)
}

pub(crate) fn due(
    model: &Calendar,
    viewer: &Viewer,
    now: jiff::Timestamp,
    hidden: &HashSet<String>,
) -> Vec<Reminder> {
    let today = viewer.wall(now).date();
    let until = today + jiff::Span::new().days(HORIZON_DAYS);
    let mut due: Vec<Reminder> = model
        .events
        .iter()
        .filter(|event| event.reminder_min.is_some() && event.visible_under(hidden))
        .flat_map(|event| crate::shown::occurrences(event, today, until, viewer))
        .filter_map(|shown| {
            let minutes = shown.event.reminder_min?;
            let start = if shown.event.all_day {
                shown.start.date().to_datetime(ALL_DAY_MORNING)
            } else {
                shown.start
            };
            let start = viewer.instant(start)?;
            let at = start - jiff::SignedDuration::from_mins(minutes);
            (at > now).then(|| Reminder {
                tag: tag(shown.event, shown.original),
                at,
                title: shown.event.display_title(),
                body: body(&shown),
                activation: format!(
                    "{SCHEME}:open/{}",
                    crate::actions::occurrence_resource_id(shown.event.id, shown.original)
                ),
            })
        })
        .collect();
    due.sort_by_key(|reminder| reminder.at);
    due.truncate(MAX_SCHEDULED);
    due
}

fn body(shown: &crate::shown::Shown<'_>) -> String {
    let when = if shown.event.all_day {
        "All day".to_owned()
    } else {
        format!(
            "{}–{}",
            ui::clock::time_of_day(shown.start),
            ui::clock::time_of_day(shown.end)
        )
    };
    if shown.event.location.trim().is_empty() {
        when
    } else {
        format!("{when} — {}", shown.event.location.trim())
    }
}

fn system_time(at: jiff::Timestamp) -> ui::time::SystemTime {
    let millis = u64::try_from(at.as_millisecond()).unwrap_or(0);
    ui::time::SystemTime::UNIX_EPOCH
        .checked_add(std::time::Duration::from_millis(millis))
        .unwrap_or(ui::time::SystemTime::UNIX_EPOCH)
}

pub(crate) fn reconcile(notifier: &dyn Notifier, wanted: &[Reminder]) -> Result<(), NotifyError> {
    let held: Vec<Scheduled> = notifier.scheduled()?;
    for scheduled in &held {
        if !wanted.iter().any(|reminder| reminder.tag == scheduled.tag) {
            notifier.cancel_scheduled(&scheduled.tag)?;
        }
    }
    for reminder in wanted {
        let at = system_time(reminder.at);
        if held
            .iter()
            .any(|scheduled| scheduled.tag == reminder.tag && scheduled.at == at)
        {
            continue;
        }
        let notification = Notification::new(&reminder.tag, &reminder.title, &reminder.body)
            .with_activation(&reminder.activation);
        notifier.schedule(&notification, at)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::Date;
    use ui::notify::conformance::Recording;

    fn at(y: i16, m: i8, d: i8, h: i8, min: i8) -> jiff::civil::DateTime {
        Date::new(y, m, d).unwrap().at(h, min, 0, 0)
    }

    fn event(
        title: &str,
        zone: &str,
        start: jiff::civil::DateTime,
        reminder: Option<i64>,
    ) -> Event {
        Event {
            id: uuid::Uuid::new_v4(),
            title: title.into(),
            start,
            end: start + jiff::SignedDuration::from_mins(45),
            timezone: zone.into(),
            reminder_min: reminder,
            recurrence_interval: 1,
            location: "Meet".into(),
            ..Event::default()
        }
    }

    /// The instant `y`-`mo`-`d` `h`:`mi`:`s` UTC.
    fn utc(y: i16, mo: i8, d: i8, h: i8, mi: i8, s: i8) -> jiff::Timestamp {
        jiff::civil::date(y, mo, d)
            .at(h, mi, s, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp()
    }

    tilo_test_env::scenario! {
        /// Reminders: when they ring, for which calendars, and what a change reschedules.
        fn reminders_ring_for_what_is_shown_and_follow_what_changed;

        phase reminders_ring_before_each_occurrence_at_its_instant;
        {
            let berlin = Viewer::named("Europe/Berlin");
            let now = utc(2026, 6, 30, 6, 0, 0);
            let mut standup = event("Standup", "Europe/Berlin", at(2026, 6, 29, 9, 30), Some(10));
            standup.recurrence = crate::model::Recurrence::Weekdays;
            let ny = event(
                "New York call",
                "America/New_York",
                at(2026, 6, 30, 9, 0),
                Some(30),
            );
            let silent = event("No reminder", "Europe/Berlin", at(2026, 6, 30, 12, 0), None);
            let past = event("Earlier", "Europe/Berlin", at(2026, 6, 30, 7, 0), Some(10));
            let model = Calendar {
                events: vec![standup, ny, silent, past].into(),
                ..Calendar::default()
            };
            let due = due(&model, &berlin, now, &HashSet::new());
            let first: Vec<(&str, jiff::Timestamp)> = due
                .iter()
                .take(2)
                .map(|reminder| (reminder.title.as_str(), reminder.at))
                .collect();
            assert_eq!(
                first,
                vec![
                    (
                        "Standup",
                        utc(2026, 6, 30, 7, 20, 0)
                    ),
                    (
                        "New York call",
                        utc(2026, 6, 30, 12, 30, 0)
                    ),
                ]
            );
            assert_eq!(due[1].body, "3:00 PM–3:45 PM — Meet");
            assert!(due.iter().all(|reminder| reminder.title != "No reminder"));
            assert!(
                due.iter().all(|reminder| reminder.title != "Earlier"),
                "past ones never ring"
            );
            let standups = due.iter().filter(|reminder| reminder.title == "Standup");
            assert_eq!(standups.clone().count(), 6);
            let tags: HashSet<_> = standups.map(|reminder| reminder.tag.clone()).collect();
            assert_eq!(tags.len(), 6);
            assert!(due.iter().all(|reminder| reminder.tag.len() == 15));
            assert!(
                due[0]
                    .activation
                    .starts_with("tilo-calendar:open/calendar.calendar/events/")
            );
        }

        phase a_hidden_calendar_rings_nothing;
        {
            let berlin = Viewer::named("Europe/Berlin");
            let now = utc(2026, 6, 30, 6, 0, 0);
            let mut synced = event("Synced", "Europe/Berlin", at(2026, 6, 30, 12, 0), Some(10));
            synced.calendar_id = Some("work/team".into());
            let model = Calendar {
                events: vec![synced].into(),
                ..Calendar::default()
            };
            assert_eq!(due(&model, &berlin, now, &HashSet::new()).len(), 1);
            let hidden = HashSet::from(["work/team".to_owned()]);
            assert!(due(&model, &berlin, now, &hidden).is_empty());
        }

        phase reconciling_cancels_the_gone_and_schedules_the_new_and_moved;
        {
            let notifier = Recording::default();
            let reminder = |tag: &str, minute: i8| Reminder {
                tag: tag.into(),
                at: utc(2026, 6, 30, 7, minute, 0),
                title: "Standup".into(),
                body: "09:30–09:45".into(),
                activation: "tilo-calendar:open/x".into(),
            };
            reconcile(&notifier, &[reminder("ca", 20), reminder("cb", 30)]).unwrap();
            assert_eq!(notifier.scheduled().unwrap().len(), 2);
            reconcile(&notifier, &[reminder("ca", 25), reminder("cc", 40)]).unwrap();
            let mut held: Vec<(String, ui::time::SystemTime)> = notifier
                .scheduled()
                .unwrap()
                .into_iter()
                .map(|scheduled| (scheduled.tag, scheduled.at))
                .collect();
            held.sort();
            assert_eq!(
                held,
                vec![
                    ("ca".to_owned(), system_time(reminder("ca", 25).at)),
                    ("cc".to_owned(), system_time(reminder("cc", 40).at)),
                ]
            );
        }
    }
}
