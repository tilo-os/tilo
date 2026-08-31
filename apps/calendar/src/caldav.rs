//! `CalDAV` sync — the real protocol edge of the Calendar app.
//!
//! ## The offline / online split (important for `--shots`)
//!
//! `--shots` runs one headless frame with no event loop and no live account, so
//! it cannot talk to a `CalDAV` server. To still render a fully populated client
//! we bundle a representative remote calendar as a real iCalendar payload and
//! **parse it through [`crate::ics::parse`]** here, exactly as a `calendar-data`
//! REPORT response would be parsed in the live app. So the screenshot exercises
//! the real import path, and the events that land in the model are
//! indistinguishable from synced ones.
//!
//! ## What the live app does (network path)
//!
//! Sync is **pull-only** today. The live binary (no `--shots`) drives, on a
//! background worker thread (mirroring the notes `Saver` pattern — the `ui`
//! event loop is synchronous and must not block), a `CalDAV` read against each
//! configured collection, built on the `reqwest` stack:
//!
//! * **Pull** — a `calendar-query` REPORT (falling back to a plain `GET` for
//!   servers that expose the collection as a single `.ics`) fetches the
//!   `calendar-data` for a time window around today; each returned `VCALENDAR`
//!   blob is fed into [`crate::ics::parse`] to produce [`Event`] / [`Task`]
//!   values, tagged with their source [`RemoteCalendar`]'s colour and id.
//!
//! Two-way sync — pushing local edits back with an `ETag`-guarded `PUT`, and
//! incremental `sync-token` pulls — is not implemented here; the local calendar
//! is the source of truth for edits, and `.ics` export is the outbound path. The
//! `Sync` affordance therefore only pulls, and says so.
//!
//! That HTTP + TLS stack is deliberately kept out of the headless build so it
//! stays small and green; the seam is this module, and the data types
//! ([`crate::account::CalDavConfig`], [`Event`], [`Task`]) are already the exact
//! shapes a `CalDAV` client consumes and produces.

use chrono::NaiveDate;

use crate::account::{CalDavConfig, RemoteCalendar};
use crate::ics;
use crate::model::{Event, Model, Palette, Task};

/// What a sync pass produced: the events and tasks parsed out of a calendar's
/// `calendar-data`, tagged with the colour and id of their source calendar.
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

/// The concrete `CalDAV` `PUT` a queued local event turns into: the resource URL
/// under the target collection and the `text/calendar` body to upload. This is
/// what "queue the event for the chosen `CalDAV` calendar" produces — a real
/// request the `live` push path (or a test) can inspect and send. Building it
/// runs the event through [`ics::export`], so the body is the exact iCalendar a
/// server would store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutRequest {
    /// Absolute URL of the event resource (`<collection>/<uid>.ics`).
    pub url: String,
    /// `text/calendar` payload — a one-event `VCALENDAR`.
    pub body: String,
}

/// Build the `CalDAV` `PUT` that uploads `event` to `calendar` under `config`.
/// The resource name is the event's UID with a `.ics` suffix, joined onto the
/// collection URL exactly as the pull path resolves it, and the body is a
/// single-event `VCALENDAR` produced by the in-tree exporter.
pub fn build_put_request(
    config: &CalDavConfig,
    calendar: &RemoteCalendar,
    event: &Event,
) -> PutRequest {
    let collection = collection_url_for(config, calendar);
    let url = format!("{}/{}.ics", collection.trim_end_matches('/'), event.id);
    let model = Model {
        projects: Vec::new(),
        events: vec![event.clone()],
        tasks: Vec::new(),
    };
    PutRequest {
        url,
        body: ics::export(&model),
    }
}

/// The concrete `CalDAV` `DELETE` a queued deletion turns into: the URL of the
/// event resource to remove from its collection. Mirrors [`PutRequest`] but
/// carries no body — a `DELETE` is addressed by URL alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteRequest {
    /// Absolute URL of the event resource (`<collection>/<uid>.ics`).
    pub url: String,
}

/// Build the `CalDAV` `DELETE` that removes `uid` from `calendar` under
/// `config`. The resource name matches the one [`build_put_request`] uploads
/// to, so a synced event round-trips to the same URL for create and delete.
pub fn build_delete_request(
    config: &CalDavConfig,
    calendar: &RemoteCalendar,
    uid: crate::model::Id,
) -> DeleteRequest {
    let collection = collection_url_for(config, calendar);
    let url = format!("{}/{}.ics", collection.trim_end_matches('/'), uid);
    DeleteRequest { url }
}

/// Remove every queued event from its target `CalDAV` collection, returning the
/// uids the server confirmed gone (so the caller can clear them from the delete
/// outbox). Each job is a `(config, calendar, uid)` triple. Compiled with
/// `live` this sends an HTTP `DELETE`; without the feature it removes nothing
/// and returns an empty list — the queue is retained until a live build drains
/// it, exactly like [`push_all`].
pub fn delete_all(
    jobs: &[(CalDavConfig, RemoteCalendar, crate::model::Id)],
) -> Vec<crate::model::Id> {
    #[cfg(feature = "live")]
    {
        live::delete_all_live(jobs)
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = jobs;
        Vec::new()
    }
}

/// Resolve a collection href (absolute or relative to the configured server)
/// to an absolute URL. Shared by the pull and push paths so both address the
/// same resource.
fn collection_url_for(config: &CalDavConfig, cal: &RemoteCalendar) -> String {
    if cal.id.starts_with("http://") || cal.id.starts_with("https://") {
        cal.id.clone()
    } else {
        format!(
            "{}/{}",
            config.server_url.trim_end_matches('/'),
            cal.id.trim_start_matches('/')
        )
    }
}

/// Sample pull for one remote calendar (demo / tests only).
///
/// Synthesises a representative `VCALENDAR` for the calendar and parses it
/// through the real [`ics::parse`], so the events that come back travel the
/// same path a live `calendar-data` REPORT response would. The live transport
/// lives in the `live` module and does not call this.
pub fn pull_calendar(
    _config: &CalDavConfig,
    calendar: &RemoteCalendar,
    today: NaiveDate,
) -> SyncResult {
    let payload = sample_calendar_ics(calendar, today);
    let (mut events, tasks) = ics::parse(&payload);
    // Tag each parsed event with its source calendar's colour AND id, exactly as
    // the live REPORT path does. The id is what lets the sidebar's visibility
    // toggle hide a synced calendar's events without a re-sync.
    let palette = calendar.palette();
    for e in &mut events {
        e.color = palette;
        e.calendar_id = Some(calendar.id.clone());
    }
    SyncResult { events, tasks }
}

/// Synchronise every visible calendar for the **live** app.
///
/// Compiled with the `live` feature this performs real `CalDAV` requests
/// (`REPORT` calendar-query, falling back to a plain `GET`) against each
/// account's server and parses the returned `calendar-data` through
/// [`ics::parse`]. Without that feature there is no transport, so it returns
/// an empty [`SyncOutcome::Offline`] — it never invents entries. Either way
/// this stays synchronous; the live app runs it off the UI thread.
pub fn sync_all(
    accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
    today: NaiveDate,
) -> (SyncResult, SyncOutcome) {
    #[cfg(feature = "live")]
    {
        live::sync_all_live(accounts, today)
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = (accounts, today);
        (SyncResult::default(), SyncOutcome::Offline)
    }
}

/// Upload every queued local event to its target `CalDAV` collection.
///
/// Each job is a `(config, calendar, event)` triple — the event and the remote
/// calendar it was created on. Compiled with `live` this sends an `ETag`-free
/// `PUT` of the event's `.ics` (via [`build_put_request`]) to each collection
/// and returns the ids that stored successfully, so the caller can clear them
/// from the outbox. Without the feature (headless / demo build) there is no
/// transport, so it uploads nothing and returns an empty list — the queue is
/// simply retained until a live build drains it.
pub fn push_all(jobs: &[(CalDavConfig, RemoteCalendar, Event)]) -> Vec<crate::model::Id> {
    #[cfg(feature = "live")]
    {
        live::push_all_live(jobs)
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = jobs;
        Vec::new()
    }
}

/// The offline sample pull used by the hermetic `--shots` / demo build and by
/// tests. It synthesises each calendar's events as real iCalendar and parses
/// them back through [`ics::parse`], so the sample data travels the exact
/// import path a live `calendar-data` REPORT response would — but it is never
/// reached by the live app.
pub fn sample_pull_all(
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
    (
        merged,
        SyncOutcome::Synced {
            calendars: count,
            events,
        },
    )
}

/// A small, self-contained `.ics` payload representing what a given remote
/// calendar would return, anchored near `today`. Distinct per calendar id so
/// each synced calendar shows different events. Built by round-tripping a tiny
/// model through [`ics::export`], so the parser is exercised end to end.
fn sample_calendar_ics(calendar: &RemoteCalendar, today: NaiveDate) -> String {
    use crate::model::Recurrence;
    use chrono::{Duration, NaiveDateTime, NaiveTime};

    // Every call below passes a literal, always-valid (h, m) pair; the
    // fallback to midnight only matters if that ever stops being true, and
    // even then a wrong sample time beats panicking the whole sync.
    let at = |date: NaiveDate, h: u32, m: u32| -> NaiveDateTime {
        date.and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap_or(NaiveTime::MIN))
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
            calendar_id: None,
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
            calendar_id: None,
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
            calendar_id: None,
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
            calendar_id: None,
            project: None,
        }],
    };

    let model = Model {
        projects: Vec::new(),
        events,
        tasks: Vec::new(),
    };
    ics::export(&model)
}

/// Real `CalDAV` transport, compiled only with the `live` feature. Kept behind a
/// module so its `reqwest` dependency never touches the default (headless)
/// build. This talks HTTP to a configured server; there is no synthesised data
/// here.
#[cfg(feature = "live")]
mod live {
    use std::time::Duration;

    use chrono::{Datelike, NaiveDate};

    use super::{RemoteCalendar, SyncOutcome, SyncResult};
    use crate::account::CalDavConfig;
    use crate::ics;

    /// Fetch every visible calendar across the configured accounts.
    pub fn sync_all_live(
        accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
        today: NaiveDate,
    ) -> (SyncResult, SyncOutcome) {
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("qOS-Calendar/0.1")
            .build()
        {
            Ok(c) => c,
            Err(e) => return (SyncResult::default(), SyncOutcome::Failed(e.to_string())),
        };

        let mut merged = SyncResult::default();
        let mut count = 0usize;
        for (config, calendars) in accounts {
            for cal in calendars.iter().filter(|c| c.visible) {
                match pull_calendar_live(&client, config, cal, today) {
                    Ok(mut res) => {
                        let palette = cal.palette();
                        for e in &mut res.events {
                            e.color = palette;
                            e.calendar_id = Some(cal.id.clone());
                        }
                        merged.events.append(&mut res.events);
                        merged.tasks.append(&mut res.tasks);
                        count += 1;
                    }
                    Err(e) => return (merged, SyncOutcome::Failed(e)),
                }
            }
        }
        if count == 0 {
            return (merged, SyncOutcome::Offline);
        }
        let events = merged.events.len();
        (
            merged,
            SyncOutcome::Synced {
                calendars: count,
                events,
            },
        )
    }

    /// Pull one collection: a `REPORT` calendar-query for a window around
    /// today, falling back to a plain `GET` for servers that expose the
    /// collection as a single `.ics`. The response body — a multistatus
    /// document embedding one `VCALENDAR` per resource — is handed to
    /// [`ics::parse`], which scans out every `VEVENT`/`VTODO` it contains.
    fn pull_calendar_live(
        client: &reqwest::blocking::Client,
        config: &CalDavConfig,
        cal: &RemoteCalendar,
        today: NaiveDate,
    ) -> Result<SyncResult, String> {
        let url = collection_url(config, cal);
        let report = reqwest::Method::from_bytes(b"REPORT").map_err(|e| e.to_string())?;
        let body = calendar_query_body(today);

        let resp = client
            .request(report, &url)
            .basic_auth(&config.username, Some(&config.password))
            .header("Depth", "1")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/xml; charset=utf-8",
            )
            .body(body)
            .send();

        let text = match resp {
            Ok(r) if r.status().is_success() => r.text().map_err(|e| e.to_string())?,
            Ok(_) => {
                // Server refused the REPORT — try a straight GET of the URL.
                let g = client
                    .get(&url)
                    .basic_auth(&config.username, Some(&config.password))
                    .send()
                    .map_err(|e| e.to_string())?;
                if !g.status().is_success() {
                    return Err(format!("server returned {}", g.status()));
                }
                g.text().map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        };

        let (events, tasks) = ics::parse(&xml_text_unescape(&text));
        Ok(SyncResult { events, tasks })
    }

    fn collection_url(config: &CalDavConfig, cal: &RemoteCalendar) -> String {
        super::collection_url_for(config, cal)
    }

    /// PUT every queued event to its target collection, returning the ids that
    /// stored successfully. Best-effort: a failed upload leaves that event in
    /// the outbox for the next attempt and does not abort the rest.
    pub fn push_all_live(
        jobs: &[(CalDavConfig, RemoteCalendar, super::Event)],
    ) -> Vec<crate::model::Id> {
        let Ok(client) = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("qOS-Calendar/0.1")
            .build()
        else {
            return Vec::new();
        };
        let mut pushed = Vec::new();
        for (config, cal, event) in jobs {
            let req = super::build_put_request(config, cal, event);
            let resp = client
                .put(&req.url)
                .basic_auth(&config.username, Some(&config.password))
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "text/calendar; charset=utf-8",
                )
                .body(req.body)
                .send();
            if matches!(resp, Ok(r) if r.status().is_success()) {
                pushed.push(event.id);
            }
        }
        pushed
    }

    /// DELETE every queued event resource, returning the uids the server
    /// confirmed removed. Best-effort and per-resource: a failed delete leaves
    /// that uid in the outbox for the next attempt and does not abort the rest.
    pub fn delete_all_live(
        jobs: &[(CalDavConfig, RemoteCalendar, crate::model::Id)],
    ) -> Vec<crate::model::Id> {
        let Ok(client) = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("qOS-Calendar/0.1")
            .build()
        else {
            return Vec::new();
        };
        let mut deleted = Vec::new();
        for (config, cal, uid) in jobs {
            let req = super::build_delete_request(config, cal, *uid);
            let resp = client
                .delete(&req.url)
                .basic_auth(&config.username, Some(&config.password))
                .send();
            // 2xx, or a 404 (already gone), both mean the resource is absent.
            let gone =
                matches!(resp, Ok(r) if r.status().is_success() || r.status().as_u16() == 404);
            if gone {
                deleted.push(*uid);
            }
        }
        deleted
    }

    fn calendar_query_body(today: NaiveDate) -> String {
        // A ±60-day window keeps the payload small while covering the views.
        let from = (today - chrono::Duration::days(60))
            .format("%Y%m%dT000000Z")
            .to_string();
        let to = (today + chrono::Duration::days(60))
            .format("%Y%m%dT000000Z")
            .to_string();
        let _ = today.year();
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<C:calendar-query xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\">\
<D:prop><D:getetag/><C:calendar-data/></D:prop>\
<C:filter><C:comp-filter name=\"VCALENDAR\">\
<C:time-range start=\"{from}\" end=\"{to}\"/>\
</C:comp-filter></C:filter></C:calendar-query>"
        )
    }

    /// Undo the handful of XML entities a server may apply to the embedded
    /// `calendar-data` text so the iCalendar lines parse cleanly.
    fn xml_text_unescape(s: &str) -> String {
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#13;", "")
            .replace("&amp;", "&")
    }
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
    fn pull_tags_events_with_source_calendar_id() {
        // The visibility toggle needs synced events to remember which calendar
        // they came from; the pull path stamps every event with `cal.id`.
        let store = AccountStore::sample();
        let (account, cal) = store
            .all_calendars()
            .find(|(_, c)| c.id == "work/default")
            .unwrap();
        let res = pull_calendar(&account.config, cal, today());
        assert!(!res.events.is_empty());
        assert!(
            res.events
                .iter()
                .all(|e| e.calendar_id.as_deref() == Some("work/default")),
            "every synced event is tagged with its source calendar id"
        );
    }

    #[test]
    fn sync_all_skips_hidden_calendars() {
        let store = AccountStore::sample();
        let accounts: Vec<_> = store
            .accounts
            .iter()
            .map(|a| (a.config.clone(), a.calendars.clone()))
            .collect();
        let (res, outcome) = sample_pull_all(&accounts, today());
        // Sample has 3 visible calendars; each yields at least one event.
        assert!(matches!(outcome, SyncOutcome::Synced { calendars: 3, .. }));
        assert!(!res.is_empty());
        // The hidden "Holidays" calendar's event must not appear.
        assert!(!res.events.iter().any(|e| e.title == "Public holiday"));
    }

    #[test]
    fn build_put_request_targets_collection_and_carries_event_ics() {
        use crate::model::{Event, Palette, Recurrence};
        use chrono::NaiveDate;
        let store = AccountStore::sample();
        let (account, cal) = store
            .all_calendars()
            .find(|(_, c)| c.id == "work/default")
            .unwrap();
        let start = NaiveDate::from_ymd_opt(2026, 6, 15)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap();
        let ev = Event {
            id: uuid::Uuid::new_v4(),
            title: "Kickoff".into(),
            start,
            end: start + chrono::Duration::hours(1),
            all_day: false,
            location: "Room 1".into(),
            color: Palette::Indigo,
            recurrence: Recurrence::None,
            reminder_min: None,
            calendar_id: Some(cal.id.clone()),
            project: None,
        };
        let req = build_put_request(&account.config, cal, &ev);
        // URL is the event resource under the collection.
        assert!(
            req.url.starts_with(&account.config.server_url),
            "url = {}",
            req.url
        );
        assert!(req.url.ends_with(&format!("{}.ics", ev.id)));
        // Body is a real one-event VCALENDAR carrying the title.
        assert!(req.body.contains("BEGIN:VEVENT"));
        assert!(req.body.contains("Kickoff"));
    }

    #[test]
    fn build_delete_request_targets_the_same_resource_as_the_put() {
        let store = AccountStore::sample();
        let (account, cal) = store
            .all_calendars()
            .find(|(_, c)| c.id == "work/default")
            .unwrap();
        let uid = uuid::Uuid::new_v4();
        let del = build_delete_request(&account.config, cal, uid);
        assert!(
            del.url.starts_with(&account.config.server_url),
            "url = {}",
            del.url
        );
        assert!(del.url.ends_with(&format!("{uid}.ics")));
    }

    #[test]
    fn delete_all_without_live_feature_removes_nothing() {
        // Symmetric with push: the headless build has no transport, so delete is
        // an honest no-op that keeps the queue rather than faking a success.
        let store = AccountStore::sample();
        let (account, cal) = store.all_calendars().next().unwrap();
        let jobs = vec![(account.config.clone(), cal.clone(), uuid::Uuid::new_v4())];
        #[cfg(not(feature = "live"))]
        assert!(delete_all(&jobs).is_empty());
        let _ = jobs;
    }

    #[test]
    fn push_all_without_live_feature_uploads_nothing() {
        // The default headless build has no transport: push is an honest no-op
        // that keeps the queue rather than fabricating a successful upload.
        let store = AccountStore::sample();
        let (account, cal) = store.all_calendars().next().unwrap();
        let start = chrono::NaiveDate::from_ymd_opt(2026, 6, 15)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap();
        let ev = crate::model::Event {
            id: uuid::Uuid::new_v4(),
            title: "x".into(),
            start,
            end: start,
            all_day: false,
            location: String::new(),
            color: crate::model::Palette::Indigo,
            recurrence: crate::model::Recurrence::None,
            reminder_min: None,
            calendar_id: Some(cal.id.clone()),
            project: None,
        };
        let jobs = vec![(account.config.clone(), cal.clone(), ev)];
        #[cfg(not(feature = "live"))]
        assert!(push_all(&jobs).is_empty());
        let _ = jobs;
    }

    #[test]
    fn empty_account_set_reports_offline() {
        let (res, outcome) = sample_pull_all(&[], today());
        assert!(res.is_empty());
        assert_eq!(outcome, SyncOutcome::Offline);
    }

    #[test]
    fn live_sync_with_no_accounts_is_offline_and_empty() {
        // The real path never fabricates: with nothing configured it returns
        // an empty Offline result (and, without the `live` feature, always).
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
        let dinner = res
            .events
            .iter()
            .find(|e| e.title == "Family dinner")
            .unwrap();
        assert_eq!(dinner.recurrence, crate::model::Recurrence::Weekly);
    }
}
