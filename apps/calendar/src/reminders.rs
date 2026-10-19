//! Event reminders as system notifications.
//!
//! The reminders of the coming days are handed to the platform's
//! notification scheduler, which shows them at their time whether or not
//! Calendar is running. Whenever what is due changes (an event moved, a
//! reminder changed, a calendar hidden, the days rolled on) the scheduled set
//! is reconciled with it: what is no longer due is cancelled, what is new or
//! moved is scheduled.

use std::collections::HashSet;
use std::hash::Hasher as _;

use chrono::{DateTime, Duration, Utc};
use ui::notify::{Notification, Notifier, NotifyError, Scheduled};

use crate::model::{Calendar, Event};
use crate::shown::Viewer;

/// How far ahead reminders are handed to the scheduler. A window left
/// open, or the next start, carries the horizon on.
pub(crate) const HORIZON_DAYS: i64 = 7;

/// The most reminders scheduled at once: the earliest ones. The platform
/// keeps 64 per app.
pub(crate) const MAX_SCHEDULED: usize = 48;

/// The time an all-day event's reminder counts back from.
const ALL_DAY_MORNING: chrono::NaiveTime = match chrono::NaiveTime::from_hms_opt(9, 0, 0) {
    Some(time) => time,
    None => chrono::NaiveTime::MIN,
};

/// The scheme of a reminder's activation: a click opens its event.
pub(crate) const SCHEME: &str = "tilo-calendar";

/// One reminder to ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reminder {
    /// Stable per occurrence, so a moved event replaces its reminder.
    pub tag: String,
    pub at: DateTime<Utc>,
    pub title: String,
    pub body: String,
    /// What a click opens.
    pub activation: String,
}

/// The tag of the reminder of `event`'s occurrence on `original`: 15 ASCII
/// characters, the shortest limit among the platforms.
fn tag(event: &Event, original: chrono::NaiveDate) -> String {
    let mut hasher = fnv::FnvHasher::default();
    hasher.write(event.id.as_bytes());
    hasher.write(original.to_string().as_bytes());
    format!("c{:014x}", hasher.finish() & 0x00ff_ffff_ffff_ffff)
}

/// The reminders that ring from `now` through the horizon, earliest first:
/// those of events in shown calendars, at their instant.
pub(crate) fn due(
    model: &Calendar,
    viewer: Viewer,
    now: DateTime<Utc>,
    hidden: &HashSet<String>,
) -> Vec<Reminder> {
    let today = viewer.wall(now).date();
    let until = today + Duration::days(HORIZON_DAYS);
    let mut due: Vec<Reminder> = model
        .events
        .iter()
        .filter(|event| event.reminder_min.is_some() && event.visible_under(hidden))
        .flat_map(|event| crate::shown::occurrences(event, today, until, &viewer))
        .filter_map(|shown| {
            let minutes = shown.event.reminder_min?;
            // An all-day event's reminder counts back from the morning.
            let start = if shown.event.all_day {
                shown.start.date().and_time(ALL_DAY_MORNING)
            } else {
                shown.start
            };
            let start = viewer.instant(start)?;
            let at = start - Duration::minutes(minutes);
            (at > now).then(|| Reminder {
                tag: tag(shown.event, shown.original),
                at,
                title: shown.event.display_title().to_owned(),
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

/// What the notification says under the title: when, and where.
fn body(shown: &crate::shown::Shown<'_>) -> String {
    let when = if shown.event.all_day {
        "All day".to_owned()
    } else {
        format!(
            "{}–{}",
            ui::clock::time_of_day(&shown.start),
            ui::clock::time_of_day(&shown.end)
        )
    };
    if shown.event.location.trim().is_empty() {
        when
    } else {
        format!("{when} — {}", shown.event.location.trim())
    }
}

/// `at` on the platform clock.
fn system_time(at: DateTime<Utc>) -> ui::time::SystemTime {
    let millis = u64::try_from(at.timestamp_millis()).unwrap_or(0);
    ui::time::SystemTime::UNIX_EPOCH
        .checked_add(std::time::Duration::from_millis(millis))
        .unwrap_or(ui::time::SystemTime::UNIX_EPOCH)
}

/// Make the scheduler hold exactly `wanted`: cancel what it holds that is
/// not wanted, schedule what is new or moved, leave the rest.
///
/// # Errors
/// The scheduler is unavailable or refused a request; what could be done
/// before it was.
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
    use chrono::{NaiveDate, TimeZone};
    use ui::notify::conformance::Recording;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
    }

    fn event(
        title: &str,
        zone: &str,
        start: chrono::NaiveDateTime,
        reminder: Option<i64>,
    ) -> Event {
        Event {
            id: uuid::Uuid::new_v4(),
            title: title.into(),
            start,
            end: start + Duration::minutes(45),
            timezone: zone.into(),
            reminder_min: reminder,
            recurrence_interval: 1,
            location: "Meet".into(),
            ..Event::default()
        }
    }

    #[test]
    fn reminders_ring_before_each_occurrence_at_its_instant() {
        let berlin = Viewer::named("Europe/Berlin");
        // Tuesday 30 June, 08:00 in Berlin.
        let now = Utc.with_ymd_and_hms(2026, 6, 30, 6, 0, 0).unwrap();
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
        let due = due(&model, berlin, now, &HashSet::new());
        let first: Vec<(&str, DateTime<Utc>)> = due
            .iter()
            .take(2)
            .map(|reminder| (reminder.title.as_str(), reminder.at))
            .collect();
        assert_eq!(
            first,
            vec![
                // 09:20 in Berlin.
                (
                    "Standup",
                    Utc.with_ymd_and_hms(2026, 6, 30, 7, 20, 0).unwrap()
                ),
                // 09:00 in New York is 15:00 in Berlin; half an hour before.
                (
                    "New York call",
                    Utc.with_ymd_and_hms(2026, 6, 30, 12, 30, 0).unwrap()
                ),
            ]
        );
        assert_eq!(due[1].body, "15:00–15:45 — Meet");
        assert!(due.iter().all(|reminder| reminder.title != "No reminder"));
        assert!(
            due.iter().all(|reminder| reminder.title != "Earlier"),
            "past ones never ring"
        );
        // The standup rings each weekday of the coming week, each its own.
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

    #[test]
    fn a_hidden_calendar_rings_nothing() {
        let berlin = Viewer::named("Europe/Berlin");
        let now = Utc.with_ymd_and_hms(2026, 6, 30, 6, 0, 0).unwrap();
        let mut synced = event("Synced", "Europe/Berlin", at(2026, 6, 30, 12, 0), Some(10));
        synced.calendar_id = Some("work/team".into());
        let model = Calendar {
            events: vec![synced].into(),
            ..Calendar::default()
        };
        assert_eq!(due(&model, berlin, now, &HashSet::new()).len(), 1);
        let hidden = HashSet::from(["work/team".to_owned()]);
        assert!(due(&model, berlin, now, &hidden).is_empty());
    }

    #[test]
    fn reconciling_cancels_the_gone_and_schedules_the_new_and_moved() {
        let notifier = Recording::default();
        let reminder = |tag: &str, minute: u32| Reminder {
            tag: tag.into(),
            at: Utc.with_ymd_and_hms(2026, 6, 30, 7, minute, 0).unwrap(),
            title: "Standup".into(),
            body: "09:30–09:45".into(),
            activation: "tilo-calendar:open/x".into(),
        };
        reconcile(&notifier, &[reminder("ca", 20), reminder("cb", 30)]).unwrap();
        assert_eq!(notifier.scheduled().unwrap().len(), 2);
        // "cb" is gone, "ca" moved, "cc" is new.
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
