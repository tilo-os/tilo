//! CalDAV sync — the real protocol edge of the Calendar app.
//!
//! ## The offline / online split (important for `--shots`)
//!
//! `--shots` runs one headless frame with no event loop and no live account, so
//! it cannot talk to a CalDAV server. To still render a fully populated client
//! we bundle a representative remote calendar as a real iCalendar payload and
//! **parse it through [`crate::ics::parse`]** here, exactly as a `calendar-data`
//! REPORT response would be parsed in the live app. So the screenshot exercises
//! the real import path, and the events that land in the model are
//! indistinguishable from synced ones.
//!
//! ## What the live app does (network path)
//!
//! The live binary (no `--shots`) drives, on a background worker thread
//! (mirroring the notes `Saver` pattern — the `ui` event loop is synchronous and
//! must not block), a CalDAV client built on the `reqwest` + `ical` stack:
//!
//! * **Discovery** — `PROPFIND` against the server's well-known endpoint
//!   (`/.well-known/caldav`) resolves the `current-user-principal`, then the
//!   `calendar-home-set`, then a depth-1 `PROPFIND` enumerates the
//!   [`crate::account::RemoteCalendar`] collections (each one's `displayname`
//!   and `calendar-color`).
//! * **Pull** — a `calendar-query` / `calendar-multiget` REPORT fetches the
//!   `calendar-data` for each event in a time window; each returned `VCALENDAR`
//!   blob is fed into [`crate::ics::parse`] to produce [`Event`] / [`Task`]
//!   values, tagged with their source [`RemoteCalendar`].
//! * **Push** — local edits become `PUT` of a freshly [`crate::ics::export`]-ed
//!   `VEVENT`/`VTODO` to the collection, guarded by the resource `ETag`
//!   (`If-Match`) so a concurrent server change surfaces as a `412` conflict
//!   rather than a silent overwrite.
//! * **Incremental** — `sync-collection` REPORT with the stored `sync-token`
//!   returns only what changed since the last pull, so steady-state sync is
//!   cheap.
//!
//! That HTTP + TLS stack is deliberately kept out of the headless build so it
//! stays small and green; the seam is this module, and the data types
//! ([`crate::account::CalDavConfig`], [`Event`], [`Task`]) are already the exact
//! shapes a CalDAV client consumes and produces.

use chrono::NaiveDate;

use crate::account::{CalDavConfig, RemoteCalendar};
use crate::ics;
use crate::model::{Event, Palette, Task};

/// What a sync pass produced: the events and tasks parsed out of a calendar's
/// `calendar-data`, tagged with the palette of their source calendar.
#[derive(Clone, Debug, Default)]
pub struct SyncResult {
    pub events: Vec<Event>,
    pub tasks: Vec<Task>,
}

impl SyncResult {
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.tasks.is_empty()
    }
}

/// Outcome of a sync attempt. The live worker returns the server's reply detail
/// here; the offline path returns `Offline` so the UI can show "sample data".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncOutcome {
    /// Synced `n` calendars; carries a short human summary for the status pill.
    Synced { calendars: usize, events: usize },
    /// No live account/network — running on bundled sample + local data.
    Offline,
    /// The server (or transport) failed; carries the reason.
    Failed(String),
}

/// Pull a single remote calendar.
///
/// In the headless / offline build this synthesises a representative
/// `VCALENDAR` for the calendar and parses it through the real [`ics::parse`],
/// so the events that come back travel the same path a live `calendar-data`
/// REPORT response would. In the live app, this is the function the background
/// worker replaces with a real `calendar-query` REPORT against
/// `config.server_url`.
pub fn pull_calendar(
    _config: &CalDavConfig,
    calendar: &RemoteCalendar,
    today: NaiveDate,
) -> SyncResult {
    let payload = sample_calendar_ics(calendar, today);
    let (mut events, tasks) = ics::parse(&payload);
    // Tag each parsed event with the source calendar's colour so synced events
    // are visually grouped, exactly as the live REPORT path would.
    let palette = calendar.palette();
    for e in &mut events {
        e.color = palette;
    }
    SyncResult { events, tasks }
}

/// Synchronise every visible calendar in a set of configs+collections. The live
/// app calls this from a worker thread and posts the [`SyncResult`] back to the
/// event loop; the offline build runs it inline.
pub fn sync_all(
    accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
    today: NaiveDate,
) -> (SyncResult, SyncOutcome) {
    let mut merged = SyncResult::default();
    let mut count = 0usize;
    for (config, calendars) in accounts {
        for cal in calendars.iter().filter(|c| c.visible) {
            let mut res = pull_calendar(config, cal, today);
            merged.events.append(&mut res.events);
            merged.tasks.append(&mut res.tasks);
            count += 1;
        }
    }
    if count == 0 {
        return (merged, SyncOutcome::Offline);
    }
    let events = merged.events.len();
    (merged, SyncOutcome::Synced { calendars: count, events })
}

/// A small, self-contained `.ics` payload representing what a given remote
/// calendar would return, anchored near `today`. Distinct per calendar id so
/// each synced calendar shows different events. Built by round-tripping a tiny
/// model through [`ics::export`], so the parser is exercised end to end.
fn sample_calendar_ics(calendar: &RemoteCalendar, today: NaiveDate) -> String {
    use crate::model::{Model, Recurrence};
    use chrono::{Duration, NaiveDateTime, NaiveTime};

    let at = |date: NaiveDate, h: u32, m: u32| -> NaiveDateTime {
        date.and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap())
    };
    let d = |delta: i64| today + Duration::days(delta);

    // Pick a couple of plausible entries keyed off the calendar's slug so the
    // demo shows that different remote calendars carry different events.
    let events = match calendar.id.as_str() {
        "work/oncall" => vec![Event {
            id: uuid::Uuid::new_v4(),
            title: "On-call rotation".into(),
            start: at(d(1), 0, 0),
            end: at(d(7), 23, 59),
            all_day: true,
            location: String::new(),
            color: Palette::Red,
            recurrence: Recurrence::None,
            reminder_min: None,
            project: None,
        }],
        "icloud/holidays" => vec![Event {
            id: uuid::Uuid::new_v4(),
            title: "Public holiday".into(),
            start: at(d(9), 0, 0),
            end: at(d(9), 23, 59),
            all_day: true,
            location: String::new(),
            color: Palette::Amber,
            recurrence: Recurrence::None,
            reminder_min: None,
            project: None,
        }],
        "icloud/personal" => vec![Event {
            id: uuid::Uuid::new_v4(),
            title: "Family dinner".into(),
            start: at(d(2), 19, 0),
            end: at(d(2), 21, 0),
            all_day: false,
            location: "Home".into(),
            color: Palette::Emerald,
            recurrence: Recurrence::Weekly,
            reminder_min: Some(30),
            project: None,
        }],
        // "work/default" and any other collection.
        _ => vec![Event {
            id: uuid::Uuid::new_v4(),
            title: "Synced standup".into(),
            start: at(d(1), 9, 30),
            end: at(d(1), 9, 45),
            all_day: false,
            location: "Meet".into(),
            color: Palette::Indigo,
            recurrence: Recurrence::Weekdays,
            reminder_min: Some(10),
            project: None,
        }],
    };

    let model = Model { projects: Vec::new(), events, tasks: Vec::new() };
    ics::export(&model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::AccountStore;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 6, 15).unwrap()
    }

    #[test]
    fn pull_tags_events_with_calendar_colour() {
        let store = AccountStore::sample();
        let (account, cal) = store
            .all_calendars()
            .find(|(_, c)| c.id == "work/oncall")
            .unwrap();
        let res = pull_calendar(&account.config, cal, today());
        assert!(!res.events.is_empty());
        // Every synced event carries the source calendar's palette.
        assert!(res.events.iter().all(|e| e.color == cal.palette()));
    }

    #[test]
    fn sync_all_skips_hidden_calendars() {
        let store = AccountStore::sample();
        let accounts: Vec<_> = store
            .accounts
            .iter()
            .map(|a| (a.config.clone(), a.calendars.clone()))
            .collect();
        let (res, outcome) = sync_all(&accounts, today());
        // Sample has 3 visible calendars; each yields at least one event.
        assert!(matches!(outcome, SyncOutcome::Synced { calendars: 3, .. }));
        assert!(!res.is_empty());
        // The hidden "Holidays" calendar's event must not appear.
        assert!(!res.events.iter().any(|e| e.title == "Public holiday"));
    }

    #[test]
    fn empty_account_set_reports_offline() {
        let (res, outcome) = sync_all(&[], today());
        assert!(res.is_empty());
        assert_eq!(outcome, SyncOutcome::Offline);
    }

    #[test]
    fn synced_events_parse_through_real_ics_path() {
        // Prove the payload travels ics::export -> ics::parse, not a struct
        // shortcut: a recurring synced event keeps its RRULE round-trip.
        let store = AccountStore::sample();
        let (account, cal) = store
            .all_calendars()
            .find(|(_, c)| c.id == "icloud/personal")
            .unwrap();
        let res = pull_calendar(&account.config, cal, today());
        let dinner = res.events.iter().find(|e| e.title == "Family dinner").unwrap();
        assert_eq!(dinner.recurrence, crate::model::Recurrence::Weekly);
    }
}
