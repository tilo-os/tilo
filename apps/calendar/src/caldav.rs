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
//! The live binary (no `--shots`) drives, on a
//! background worker thread (mirroring the notes `Saver` pattern — the `ui`
//! event loop is synchronous and must not block), a `CalDAV` read against each
//! configured collection, built on the `reqwest` stack:
//!
//! * **Pull** — bounded RFC 6578 `sync-collection` REPORTs use the durable token
//!   for each collection. Unsupported/invalid tokens fall back to one bounded,
//!   authoritative `calendar-query`, followed by `sync-token` discovery.
//!   Href + `ETag` are retained as server identity/revision; XML is parsed rather
//!   than scraped, and tokens commit only after reconciliation.
//!
//! * **Push/delete** — crash-safe outboxes drain with conditional `If-Match` or
//!   create-only `If-None-Match: *`. A 412 preserves the local version, leaves
//!   it queued, and surfaces an understandable conflict instead of overwriting.
//!
//! That HTTP + TLS stack is deliberately kept out of the headless build so it
//! stays small and green; the seam is this module, and the data types
//! ([`crate::account::CalDavConfig`], [`Event`], [`Task`]) are already the exact
//! shapes a `CalDAV` client consumes and produces.

use chrono::NaiveDate;

use crate::account::{CalDavConfig, RemoteCalendar};
use crate::ics;
use crate::model::{Event, Id, Model, Palette, Task};

const MAX_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESOURCES: usize = 10_000;
const MAX_HREF_BYTES: usize = 4096;
const MAX_ICAL_BYTES: usize = 2 * 1024 * 1024;

/// Stable server-side identity and revision for a pulled calendar object.
#[derive(Clone, Debug)]
pub struct RemoteResource {
    pub calendar_id: String,
    pub href: String,
    pub etag: Option<String>,
    pub events: Vec<Event>,
    pub tasks: Vec<Task>,
}

/// One server-confirmed removal, addressed by collection + href rather than
/// an iCalendar title or a newly generated local UUID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteDeletion {
    pub calendar_id: String,
    pub href: String,
}

/// A token is a commit marker: callers persist it only after every resource in
/// the same response has been reconciled successfully.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionCheckpoint {
    pub calendar_id: String,
    pub token: String,
    /// Full queries list the authoritative collection contents. Incremental
    /// reports list only changed/deleted members.
    pub full: bool,
    pub seen_hrefs: Vec<String>,
}

/// What a sync pass produced: the events and tasks parsed out of a calendar's
/// `calendar-data`, tagged with the colour and id of their source calendar.
#[derive(Clone, Debug, Default)]
pub struct SyncResult {
    pub events: Vec<Event>,
    pub tasks: Vec<Task>,
    pub resources: Vec<RemoteResource>,
    pub deletions: Vec<RemoteDeletion>,
    pub checkpoints: Vec<CollectionCheckpoint>,
}

impl SyncResult {
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.tasks.is_empty() && self.resources.is_empty()
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
    pub if_match: Option<String>,
    pub if_none_match: bool,
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
        if_match: None,
        if_none_match: true,
    }
}

#[derive(Clone, Debug)]
pub struct UploadJob {
    pub config: CalDavConfig,
    pub calendar: RemoteCalendar,
    pub event: Event,
    pub href: Option<String>,
    pub etag: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RemoveJob {
    pub config: CalDavConfig,
    pub calendar: RemoteCalendar,
    pub id: Id,
    pub href: Option<String>,
    pub etag: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct MutationResult {
    pub confirmed: Vec<Id>,
    pub revisions: Vec<RemoteRevision>,
    pub conflicts: Vec<(Id, String)>,
    pub failures: Vec<(Id, String)>,
}

#[derive(Clone, Debug)]
pub struct RemoteRevision {
    pub id: Id,
    pub calendar_id: String,
    pub href: String,
    pub etag: Option<String>,
}

/// The concrete `CalDAV` `DELETE` a queued deletion turns into: the URL of the
/// event resource to remove from its collection. Mirrors [`PutRequest`] but
/// carries no body — a `DELETE` is addressed by URL alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteRequest {
    /// Absolute URL of the event resource (`<collection>/<uid>.ics`).
    pub url: String,
    pub if_match: Option<String>,
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
    DeleteRequest {
        url,
        if_match: None,
    }
}

pub fn push_jobs(jobs: &[UploadJob]) -> MutationResult {
    #[cfg(feature = "live")]
    {
        live::push_jobs_live(jobs)
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = jobs;
        MutationResult::default()
    }
}

pub fn remove_jobs(jobs: &[RemoveJob]) -> MutationResult {
    #[cfg(feature = "live")]
    {
        live::remove_jobs_live(jobs)
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = jobs;
        MutationResult::default()
    }
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

/// Compare DAV href spellings after discarding an optional scheme/authority.
/// This keeps bindings stable across servers that alternate between `/dav/x`
/// and `https://host/dav/x` in otherwise equivalent responses.
pub fn hrefs_equal(left: &str, right: &str) -> bool {
    fn absolute_path(value: &str) -> Option<&str> {
        value
            .split_once("://")
            .and_then(|(_, rest)| rest.find('/').and_then(|index| rest.get(index..)))
    }
    if left == right {
        return true;
    }
    match (absolute_path(left), absolute_path(right)) {
        // Servers commonly alternate between an origin-relative href and its
        // absolute spelling. Two *absolute* hrefs from different origins are
        // never equated merely because their paths match: that would let an
        // unexpected redirect/cross-origin response alias an existing event.
        (Some(left_path), None) => left_path == right,
        (None, Some(right_path)) => left == right_path,
        _ => false,
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
    SyncResult {
        events,
        tasks,
        ..SyncResult::default()
    }
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

/// Synchronise with durable per-collection checkpoints supplied by the caller.
/// Keeping the checkpoint in the Automerge document (rather than a transport
/// cache) makes restart and collaborative hand-off deterministic.
pub fn sync_all_with_checkpoints(
    accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
    today: NaiveDate,
    checkpoints: &[(String, String)],
) -> (SyncResult, SyncOutcome) {
    #[cfg(feature = "live")]
    {
        live::sync_all_live_with_checkpoints(accounts, today, checkpoints)
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = (accounts, today, checkpoints);
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

#[derive(Debug)]
struct ParsedMultistatus {
    token: Option<String>,
    changed: Vec<(String, Option<String>, String)>,
    deleted: Vec<String>,
}

/// Parse a DAV multistatus without accepting an unbounded server document.
/// Namespace prefixes are deliberately ignored; DAV/CalDAV servers choose
/// their own prefixes, while the namespace-expanded local names are stable.
fn parse_multistatus(xml: &str) -> Result<ParsedMultistatus, String> {
    if xml.len() > MAX_XML_BYTES {
        return Err("calendar response exceeds the 8 MiB safety limit".into());
    }
    let doc =
        roxmltree::Document::parse(xml).map_err(|_| "calendar server returned malformed XML")?;
    let token = doc
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "sync-token")
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let mut changed = Vec::new();
    let mut deleted = Vec::new();
    for response in doc
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "response")
    {
        if changed.len() + deleted.len() >= MAX_RESOURCES {
            return Err("calendar response contains more than 10,000 resources".into());
        }
        let href = response
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "href")
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= MAX_HREF_BYTES)
            .ok_or_else(|| "calendar response contains an invalid resource href".to_string())?;
        let response_status = response
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "status")
            .and_then(|node| node.text())
            .unwrap_or_default();
        if response_status.contains(" 404 ") || response_status.ends_with(" 404") {
            deleted.push(href.to_owned());
            continue;
        }
        let successful_prop = response.children().find(|node| {
            node.is_element()
                && node.tag_name().name() == "propstat"
                && node.descendants().any(|child| {
                    child.is_element()
                        && child.tag_name().name() == "status"
                        && child.text().is_some_and(|status| status.contains(" 200 "))
                })
        });
        let Some(propstat) = successful_prop else {
            continue;
        };
        let etag = propstat
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "getetag")
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= 1024)
            .map(ToOwned::to_owned);
        let Some(data) = propstat
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "calendar-data")
            .and_then(|node| node.text())
        else {
            continue;
        };
        if data.len() > MAX_ICAL_BYTES {
            return Err(format!(
                "calendar resource {href} exceeds the 2 MiB safety limit"
            ));
        }
        changed.push((href.to_owned(), etag, data.to_owned()));
    }
    Ok(ParsedMultistatus {
        token,
        changed,
        deleted,
    })
}

fn sync_collection_body(token: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<D:sync-collection xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\">\
<D:sync-token>{}</D:sync-token><D:sync-level>1</D:sync-level>\
<D:prop><D:getetag/><C:calendar-data/></D:prop></D:sync-collection>",
        xml_escape(token)
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Real `CalDAV` transport, compiled only with the `live` feature. Kept behind a
/// module so its `reqwest` dependency never touches the default (headless)
/// build. This talks HTTP to a configured server; there is no synthesised data
/// here.
#[cfg(feature = "live")]
mod live {
    use std::collections::HashMap;
    use std::io::Read;
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
        sync_all_live_with_checkpoints(accounts, today, &[])
    }

    pub fn sync_all_live_with_checkpoints(
        accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
        today: NaiveDate,
        checkpoints: &[(String, String)],
    ) -> (SyncResult, SyncOutcome) {
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("qOS-Calendar/0.1")
            .build()
        {
            Ok(c) => c,
            Err(e) => return (SyncResult::default(), SyncOutcome::Failed(e.to_string())),
        };

        let checkpoint_map: HashMap<&str, &str> = checkpoints
            .iter()
            .map(|(calendar, token)| (calendar.as_str(), token.as_str()))
            .collect();
        let mut merged = SyncResult::default();
        let mut count = 0usize;
        for (config, calendars) in accounts {
            for cal in calendars.iter().filter(|c| c.visible) {
                match pull_calendar_live(
                    &client,
                    config,
                    cal,
                    today,
                    checkpoint_map.get(cal.id.as_str()).copied(),
                ) {
                    Ok(mut res) => {
                        let palette = cal.palette();
                        for e in &mut res.events {
                            e.color = palette;
                            e.calendar_id = Some(cal.id.clone());
                        }
                        merged.events.append(&mut res.events);
                        merged.tasks.append(&mut res.tasks);
                        merged.resources.append(&mut res.resources);
                        merged.deletions.append(&mut res.deletions);
                        merged.checkpoints.append(&mut res.checkpoints);
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
        checkpoint: Option<&str>,
    ) -> Result<SyncResult, String> {
        let url = collection_url(config, cal);
        let report = reqwest::Method::from_bytes(b"REPORT").map_err(|e| e.to_string())?;
        let mut full = checkpoint.is_none();
        let body = super::sync_collection_body(checkpoint.unwrap_or_default());
        let mut response = client
            .request(report.clone(), &url)
            .basic_auth(&config.username, Some(&config.password))
            .header("Depth", "1")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/xml; charset=utf-8",
            )
            .body(body)
            .send()
            .map_err(|error| error.to_string())?;

        // RFC 6578 uses 403 with a valid-sync-token precondition (some servers
        // use 409). Reconcile from a bounded full listing before accepting a
        // replacement token; never silently discard local mapping state.
        let report_unsupported = matches!(response.status().as_u16(), 400 | 403 | 405 | 409 | 501);
        if report_unsupported {
            full = true;
            response = client
                .request(report, &url)
                .basic_auth(&config.username, Some(&config.password))
                .header("Depth", "1")
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/xml; charset=utf-8",
                )
                .body(calendar_query_body(today))
                .send()
                .map_err(|error| error.to_string())?;
        }
        if !response.status().is_success() {
            return Err(format!("calendar server returned {}", response.status()));
        }
        let text = read_bounded_xml(response)?;
        let parsed = super::parse_multistatus(&text)?;
        let mut result = SyncResult::default();
        let mut seen_hrefs = Vec::with_capacity(parsed.changed.len());
        for (href, etag, payload) in parsed.changed {
            let href = canonical_resource_url(&url, &href)?;
            let (mut events, tasks) = ics::parse(&payload);
            if events.is_empty() && tasks.is_empty() {
                return Err(format!(
                    "calendar resource {href} contains no supported item"
                ));
            }
            for event in &mut events {
                event.color = cal.palette();
                event.calendar_id = Some(cal.id.clone());
            }
            seen_hrefs.push(href.clone());
            result.resources.push(super::RemoteResource {
                calendar_id: cal.id.clone(),
                href,
                etag,
                events,
                tasks,
            });
        }
        result.deletions = parsed
            .deleted
            .into_iter()
            .map(|href| {
                Ok(super::RemoteDeletion {
                    calendar_id: cal.id.clone(),
                    href: canonical_resource_url(&url, &href)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let next_token = if parsed.token.is_some() {
            parsed.token
        } else if full {
            discover_sync_token(client, config, &url)?
        } else {
            None
        };
        if let Some(token) = next_token {
            result.checkpoints.push(super::CollectionCheckpoint {
                calendar_id: cal.id.clone(),
                token,
                full,
                seen_hrefs,
            });
        } else if checkpoint.is_some() {
            return Err("calendar server omitted the incremental sync token".into());
        }
        Ok(result)
    }

    fn discover_sync_token(
        client: &reqwest::blocking::Client,
        config: &CalDavConfig,
        url: &str,
    ) -> Result<Option<String>, String> {
        let propfind =
            reqwest::Method::from_bytes(b"PROPFIND").map_err(|error| error.to_string())?;
        let response = client
            .request(propfind, url)
            .basic_auth(&config.username, Some(&config.password))
            .header("Depth", "0")
            .header(reqwest::header::CONTENT_TYPE, "application/xml; charset=utf-8")
            .body("<?xml version=\"1.0\"?><D:propfind xmlns:D=\"DAV:\"><D:prop><D:sync-token/></D:prop></D:propfind>")
            .send()
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let text = read_bounded_xml(response)?;
        let doc = roxmltree::Document::parse(&text)
            .map_err(|_| "calendar server returned malformed XML")?;
        Ok(doc
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "sync-token")
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|token| !token.is_empty() && token.len() <= 4096)
            .map(ToOwned::to_owned))
    }

    fn collection_url(config: &CalDavConfig, cal: &RemoteCalendar) -> String {
        super::collection_url_for(config, cal)
    }

    /// Read a DAV XML response without ever allocating past the protocol cap.
    /// `Response::text` is intentionally avoided because a hostile server can
    /// omit `Content-Length` and otherwise make the post-read size check moot.
    fn read_bounded_xml(response: reqwest::blocking::Response) -> Result<String, String> {
        if response
            .content_length()
            .is_some_and(|length| length > super::MAX_XML_BYTES as u64)
        {
            return Err("calendar response exceeds the 8 MiB safety limit".into());
        }
        let mut bytes = Vec::new();
        response
            .take(super::MAX_XML_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > super::MAX_XML_BYTES {
            return Err("calendar response exceeds the 8 MiB safety limit".into());
        }
        String::from_utf8(bytes).map_err(|_| "calendar server returned non-UTF-8 XML".into())
    }

    /// Normalize every REPORT href to the same absolute identity used by PUT
    /// and DELETE responses. Servers freely alternate between absolute and
    /// path hrefs, so retaining the wire spelling would duplicate resources.
    fn canonical_resource_url(collection_url: &str, href: &str) -> Result<String, String> {
        let base = reqwest::Url::parse(&format!("{}/", collection_url.trim_end_matches('/')))
            .map_err(|_| "calendar collection has an invalid URL".to_string())?;
        base.join(href)
            .map(|url| url.to_string())
            .map_err(|_| "calendar response contains an invalid resource href".to_string())
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

    pub fn push_jobs_live(jobs: &[super::UploadJob]) -> super::MutationResult {
        let Ok(client) = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("qOS-Calendar/0.1")
            .build()
        else {
            return super::MutationResult::default();
        };
        let mut result = super::MutationResult::default();
        for job in jobs {
            let mut request = super::build_put_request(&job.config, &job.calendar, &job.event);
            if let Some(href) = &job.href {
                request.url = absolute_resource_url(&job.config, href);
                // An existing binding is never overwritten unconditionally.
                // Exact ETags detect edits; `*` is the safe fallback for old
                // servers/documents where the binding predates ETag storage.
                request.if_match = Some(job.etag.clone().unwrap_or_else(|| "*".into()));
                request.if_none_match = false;
            }
            let mut builder = client
                .put(&request.url)
                .basic_auth(&job.config.username, Some(&job.config.password))
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "text/calendar; charset=utf-8",
                )
                .body(request.body);
            if let Some(etag) = &request.if_match {
                builder = builder.header(reqwest::header::IF_MATCH, etag);
            } else if request.if_none_match {
                builder = builder.header(reqwest::header::IF_NONE_MATCH, "*");
            }
            match builder.send() {
                Ok(response) if response.status().is_success() => {
                    let etag = response
                        .headers()
                        .get(reqwest::header::ETAG)
                        .and_then(|value| value.to_str().ok())
                        .map(ToOwned::to_owned);
                    result.confirmed.push(job.event.id);
                    result.revisions.push(super::RemoteRevision {
                        id: job.event.id,
                        calendar_id: job.calendar.id.clone(),
                        href: request.url,
                        etag,
                    });
                }
                Ok(response) if response.status().as_u16() == 412 => {
                    result.conflicts.push((job.event.id, request.url));
                }
                Ok(response) => result.failures.push((
                    job.event.id,
                    format!("server returned {}", response.status()),
                )),
                Err(error) => result.failures.push((job.event.id, error.to_string())),
            }
        }
        result
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

    pub fn remove_jobs_live(jobs: &[super::RemoveJob]) -> super::MutationResult {
        let Ok(client) = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("qOS-Calendar/0.1")
            .build()
        else {
            return super::MutationResult::default();
        };
        let mut result = super::MutationResult::default();
        for job in jobs {
            let url = job.href.as_ref().map_or_else(
                || super::build_delete_request(&job.config, &job.calendar, job.id).url,
                |href| absolute_resource_url(&job.config, href),
            );
            let mut builder = client
                .delete(&url)
                .basic_auth(&job.config.username, Some(&job.config.password));
            // Even when an older server omitted the revision, keep deletion
            // conditional on the resource still existing. Known revisions use
            // their exact ETag and therefore detect concurrent modification.
            builder = builder.header(
                reqwest::header::IF_MATCH,
                job.etag.as_deref().unwrap_or("*"),
            );
            match builder.send() {
                Ok(response)
                    if response.status().is_success() || response.status().as_u16() == 404 =>
                {
                    result.confirmed.push(job.id);
                }
                Ok(response) if response.status().as_u16() == 412 => {
                    result.conflicts.push((job.id, url));
                }
                Ok(response) => result
                    .failures
                    .push((job.id, format!("server returned {}", response.status()))),
                Err(error) => result.failures.push((job.id, error.to_string())),
            }
        }
        result
    }

    fn absolute_resource_url(config: &CalDavConfig, href: &str) -> String {
        if href.starts_with("http://") || href.starts_with("https://") {
            href.to_owned()
        } else {
            format!(
                "{}/{}",
                config.server_url.trim_end_matches('/'),
                href.trim_start_matches('/')
            )
        }
    }

    fn calendar_query_body(today: NaiveDate) -> String {
        // Full fallback must be authoritative: a time window cannot distinguish
        // an old resource from a deletion. Response byte/resource caps bound it.
        let _ = today.year();
        String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<C:calendar-query xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\">\
<D:prop><D:getetag/><C:calendar-data/></D:prop>\
<C:filter><C:comp-filter name=\"VCALENDAR\"/></C:filter></C:calendar-query>",
        )
    }

    #[cfg(test)]
    mod tests {
        type Response = (u16, Vec<(&'static str, &'static str)>, String);

        use std::fmt::Write as _;
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::sync::mpsc;

        use super::*;

        fn serve(responses: Vec<Response>) -> (String, mpsc::Receiver<String>) {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake CalDAV server");
            let address = listener.local_addr().expect("fake server address");
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for (status, headers, body) in responses {
                    let (mut stream, _) = listener.accept().expect("accept CalDAV request");
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    let header_end = loop {
                        let read = stream.read(&mut buffer).expect("read request");
                        assert!(read != 0, "request ended before headers");
                        request.extend_from_slice(&buffer[..read]);
                        if let Some(position) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            break position + 4;
                        }
                    };
                    let headers_text = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers_text
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    while request.len() < header_end + content_length {
                        let read = stream.read(&mut buffer).expect("read request body");
                        assert!(read != 0, "request body ended early");
                        request.extend_from_slice(&buffer[..read]);
                    }
                    tx.send(String::from_utf8_lossy(&request).into_owned())
                        .expect("capture request");
                    let reason = if status == 207 {
                        "Multi-Status"
                    } else {
                        "Response"
                    };
                    let mut response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                        body.len()
                    );
                    for (name, value) in headers {
                        let _ = write!(response, "{name}: {value}\r\n");
                    }
                    response.push_str("\r\n");
                    response.push_str(&body);
                    stream
                        .write_all(response.as_bytes())
                        .expect("write response");
                }
            });
            (format!("http://{address}"), rx)
        }

        fn config(server_url: String) -> CalDavConfig {
            CalDavConfig {
                display_name: "Test".into(),
                server_url,
                username: "user".into(),
                password: "secret".into(),
            }
        }

        fn calendar() -> RemoteCalendar {
            RemoteCalendar {
                id: "cal/".into(),
                name: "Test".into(),
                color: 0,
                visible: true,
            }
        }

        fn multistatus(token: &str) -> String {
            format!(
                r#"<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
<D:sync-token>{token}</D:sync-token><D:response><D:href>/cal/a.ics</D:href>
<D:propstat><D:prop><D:getetag>&quot;v2&quot;</D:getetag><C:calendar-data>BEGIN:VCALENDAR
BEGIN:VEVENT
UID:a
SUMMARY:Review
DTSTART:20260615T090000
DTEND:20260615T100000
END:VEVENT
END:VCALENDAR</C:calendar-data></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
</D:response></D:multistatus>"#
            )
        }

        #[test]
        fn fake_server_receives_incremental_report_and_href_is_canonical() {
            let (server, requests) = serve(vec![(207, vec![], multistatus("token-2"))]);
            let config = config(server.clone());
            let result = pull_calendar_live(
                &reqwest::blocking::Client::new(),
                &config,
                &calendar(),
                NaiveDate::from_ymd_opt(2026, 6, 15).unwrap(),
                Some("token-1"),
            )
            .expect("incremental sync");
            let request = requests.recv().expect("captured REPORT");
            assert!(request.starts_with("REPORT /cal/ HTTP/1.1"), "{request}");
            assert!(request.contains("<D:sync-token>token-1</D:sync-token>"));
            assert_eq!(result.resources[0].href, format!("{server}/cal/a.ics"));
            assert_eq!(result.checkpoints[0].token, "token-2");
            assert!(!result.checkpoints[0].full);
        }

        #[test]
        fn fake_server_invalid_token_falls_back_to_authoritative_report() {
            let (server, requests) = serve(vec![
                (403, vec![], String::new()),
                (207, vec![], multistatus("replacement")),
            ]);
            let result = pull_calendar_live(
                &reqwest::blocking::Client::new(),
                &config(server),
                &calendar(),
                NaiveDate::from_ymd_opt(2026, 6, 15).unwrap(),
                Some("expired"),
            )
            .expect("full fallback");
            let incremental = requests.recv().expect("incremental request");
            let fallback = requests.recv().expect("fallback request");
            assert!(incremental.contains("<D:sync-collection"));
            assert!(fallback.contains("<C:calendar-query"));
            assert!(result.checkpoints[0].full);
            assert_eq!(result.checkpoints[0].token, "replacement");
        }

        #[test]
        fn fake_server_observes_conditional_put_and_delete() {
            let (server, requests) = serve(vec![
                (200, vec![("ETag", "\"v2\"")], String::new()),
                (200, vec![], String::new()),
                (204, vec![], String::new()),
            ]);
            let config = config(server.clone());
            let calendar = calendar();
            let mut event = super::super::pull_calendar(
                &config,
                &calendar,
                NaiveDate::from_ymd_opt(2026, 6, 15).unwrap(),
            )
            .events
            .remove(0);
            event.calendar_id = Some(calendar.id.clone());
            let href = format!("{server}/cal/a.ics");
            let mut legacy_event = event.clone();
            legacy_event.id = crate::model::Id::new_v4();
            let upload = push_jobs_live(&[
                super::super::UploadJob {
                    config: config.clone(),
                    calendar: calendar.clone(),
                    event: event.clone(),
                    href: Some(href.clone()),
                    etag: Some("\"v1\"".into()),
                },
                super::super::UploadJob {
                    config: config.clone(),
                    calendar: calendar.clone(),
                    event: legacy_event.clone(),
                    href: Some(format!("{server}/cal/legacy.ics")),
                    etag: None,
                },
            ]);
            assert_eq!(upload.confirmed, vec![event.id, legacy_event.id]);
            assert_eq!(upload.revisions[0].etag.as_deref(), Some("\"v2\""));
            let removed = remove_jobs_live(&[super::super::RemoveJob {
                config,
                calendar,
                id: event.id,
                href: Some(href),
                etag: Some("\"v2\"".into()),
            }]);
            assert_eq!(removed.confirmed, vec![event.id]);
            let put = requests.recv().expect("PUT request").to_ascii_lowercase();
            let legacy_put = requests
                .recv()
                .expect("legacy PUT request")
                .to_ascii_lowercase();
            let delete = requests
                .recv()
                .expect("DELETE request")
                .to_ascii_lowercase();
            assert!(put.starts_with("put /cal/a.ics http/1.1"));
            assert!(put.contains("if-match: \"v1\""));
            assert!(legacy_put.starts_with("put /cal/legacy.ics http/1.1"));
            assert!(legacy_put.contains("if-match: *"));
            assert!(delete.starts_with("delete /cal/a.ics http/1.1"));
            assert!(delete.contains("if-match: \"v2\""));
        }
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

    #[test]
    fn multistatus_parses_stable_identity_revision_token_and_deletion() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
 <D:sync-token>token-2</D:sync-token>
 <D:response><D:href>/cal/a.ics</D:href><D:propstat><D:prop>
  <D:getetag>&quot;v2&quot;</D:getetag><C:calendar-data>BEGIN:VCALENDAR
BEGIN:VEVENT
SUMMARY:Review
DTSTART:20260615T090000
DTEND:20260615T100000
END:VEVENT
END:VCALENDAR</C:calendar-data></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
 <D:response><D:href>/cal/gone.ics</D:href><D:status>HTTP/1.1 404 Not Found</D:status></D:response>
</D:multistatus>"#;
        let parsed = parse_multistatus(xml).unwrap();
        assert_eq!(parsed.token.as_deref(), Some("token-2"));
        assert_eq!(parsed.changed[0].0, "/cal/a.ics");
        assert_eq!(parsed.changed[0].1.as_deref(), Some("\"v2\""));
        assert_eq!(parsed.deleted, vec!["/cal/gone.ics"]);
        assert!(parsed.changed[0].2.contains("SUMMARY:Review"));
    }

    #[test]
    fn malformed_and_oversized_multistatus_are_rejected_without_partial_data() {
        assert!(parse_multistatus("<D:multistatus>").is_err());
        let oversized = "x".repeat(MAX_XML_BYTES + 1);
        assert!(parse_multistatus(&oversized).is_err());
    }

    #[test]
    fn sync_token_is_xml_escaped() {
        let body = sync_collection_body("a<&\"'>");
        assert!(body.contains("a&lt;&amp;&quot;&apos;&gt;"));
        assert!(!body.contains("a<&"));
    }

    #[test]
    fn absolute_and_path_dav_hrefs_share_one_identity() {
        assert!(hrefs_equal(
            "https://calendar.example/dav/work/a.ics",
            "/dav/work/a.ics"
        ));
        assert!(!hrefs_equal(
            "https://calendar.example/dav/work/a.ics",
            "/dav/work/b.ics"
        ));
        assert!(!hrefs_equal(
            "https://calendar.example/dav/work/a.ics",
            "https://attacker.example/dav/work/a.ics"
        ));
    }

    #[test]
    fn new_put_is_create_only_until_a_remote_etag_is_known() {
        let store = AccountStore::sample();
        let (account, cal) = store.all_calendars().next().unwrap();
        let event = pull_calendar(&account.config, cal, today())
            .events
            .remove(0);
        let request = build_put_request(&account.config, cal, &event);
        assert!(request.if_none_match);
        assert!(request.if_match.is_none());
    }
}
