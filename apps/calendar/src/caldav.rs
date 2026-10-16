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
//! configured collection, built on the shared `tilo-dav` client:
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
use crate::model::{Calendar, Event, Id, Palette, Task};
use ui::tilo_resource::Keyed;

#[cfg(any(feature = "live", test))]
const MAX_XML_BYTES: usize = 8 * 1024 * 1024;
#[cfg(any(feature = "live", test))]
const MAX_RESOURCES: usize = 10_000;
#[cfg(any(feature = "live", test))]
const MAX_HREF_BYTES: usize = 4096;
#[cfg(any(feature = "live", test))]
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
    PutRequest {
        url,
        body: event_body(event),
        if_match: None,
        if_none_match: true,
    }
}

/// The one-event `VCALENDAR` `event` is uploaded as.
pub fn event_body(event: &Event) -> String {
    ics::export(&Calendar {
        events: vec![event.clone()].into(),
        ..Calendar::default()
    })
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

pub async fn push_jobs(jobs: &[UploadJob]) -> MutationResult {
    #[cfg(feature = "live")]
    {
        live::push_jobs_live(jobs).await
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = jobs;
        MutationResult::default()
    }
}

pub async fn remove_jobs(jobs: &[RemoveJob]) -> MutationResult {
    #[cfg(feature = "live")]
    {
        live::remove_jobs_live(jobs).await
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = jobs;
        MutationResult::default()
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

/// Synchronise with durable per-collection checkpoints supplied by the caller.
/// Keeping the checkpoint in the Automerge document (rather than a transport
/// cache) makes restart and collaborative hand-off deterministic.
pub async fn sync_all_with_checkpoints(
    accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
    today: NaiveDate,
    checkpoints: &[(String, String)],
) -> (SyncResult, SyncOutcome) {
    #[cfg(feature = "live")]
    {
        live::sync_all_live_with_checkpoints(accounts, today, checkpoints).await
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = (accounts, today, checkpoints);
        (SyncResult::default(), SyncOutcome::Offline)
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
            color: Palette::Red,
            ..Event::default()
        }],
        "icloud/holidays" => vec![Event {
            id: uuid::Uuid::new_v4(),
            title: "Public holiday".into(),
            start: at(d(9), 0, 0),
            end: at(d(9), 23, 59),
            all_day: true,
            color: Palette::Amber,
            ..Event::default()
        }],
        "icloud/personal" => vec![Event {
            id: uuid::Uuid::new_v4(),
            title: "Family dinner".into(),
            start: at(d(2), 19, 0),
            end: at(d(2), 21, 0),
            location: "Home".into(),
            color: Palette::Emerald,
            recurrence: Recurrence::Weekly,
            reminder_min: Some(30),
            ..Event::default()
        }],
        // "work/default" and any other collection.
        _ => vec![Event {
            id: uuid::Uuid::new_v4(),
            title: "Synced standup".into(),
            start: at(d(1), 9, 30),
            end: at(d(1), 9, 45),
            location: "Meet".into(),
            recurrence: Recurrence::Weekdays,
            reminder_min: Some(10),
            ..Event::default()
        }],
    };

    let model = Calendar {
        projects: Keyed::default(),
        events: events.into(),
        tasks: Keyed::default(),
        ..Calendar::default()
    };
    ics::export(&model)
}

#[cfg(any(feature = "live", test))]
#[derive(Debug)]
struct ParsedMultistatus {
    token: Option<String>,
    changed: Vec<(String, Option<String>, String)>,
    deleted: Vec<String>,
}

#[cfg(any(feature = "live", test))]
/// Caps for one calendar REPORT/PROPFIND answer.
const LIMITS: tilo_dav::Limits = tilo_dav::Limits {
    max_bytes: MAX_XML_BYTES,
    max_responses: MAX_RESOURCES,
    max_depth: 32,
    max_nodes: 1_000_000,
    max_href_bytes: MAX_HREF_BYTES,
};

#[cfg(any(feature = "live", test))]
/// Properties every calendar REPORT asks for.
const REPORT_PROPS: [tilo_dav::PropName; 2] =
    [tilo_dav::props::GETETAG, tilo_dav::props::CALENDAR_DATA];

#[cfg(any(feature = "live", test))]
fn xml_error_message(error: &tilo_dav::XmlError) -> String {
    use tilo_dav::XmlError;
    match error {
        XmlError::TooLarge => "calendar response exceeds the 8 MiB safety limit".into(),
        XmlError::TooManyResponses => {
            "calendar response contains more than 10,000 resources".into()
        }
        XmlError::InvalidHref => "calendar response contains an invalid resource href".into(),
        XmlError::NotUtf8 => "calendar server returned non-UTF-8 XML".into(),
        _ => "calendar server returned malformed XML".into(),
    }
}

/// Parse a DAV multistatus without accepting an unbounded server document.
#[cfg(test)]
fn parse_multistatus(xml: &str) -> Result<ParsedMultistatus, String> {
    let parsed = tilo_dav::multistatus::parse(xml, &LIMITS).map_err(|e| xml_error_message(&e))?;
    interpret_multistatus(parsed)
}

#[cfg(any(feature = "live", test))]
/// Split a multistatus into changed resources (href, `ETag`, calendar data)
/// and removed hrefs. A whole-response 404 is a removal; a resource without
/// a successful `calendar-data` is skipped.
fn interpret_multistatus(parsed: tilo_dav::Multistatus) -> Result<ParsedMultistatus, String> {
    let mut changed = Vec::new();
    let mut deleted = Vec::new();
    for response in parsed.responses {
        if response.status == Some(404) {
            deleted.push(response.href);
            continue;
        }
        let etag = response
            .prop_text("getetag")
            .filter(|value| value.len() <= 1024)
            .map(ToOwned::to_owned);
        let Some(data) = response
            .prop("calendar-data")
            .and_then(|prop| prop.text.as_deref())
        else {
            continue;
        };
        if data.len() > MAX_ICAL_BYTES {
            return Err(format!(
                "calendar resource {} exceeds the 2 MiB safety limit",
                response.href
            ));
        }
        let data = data.to_owned();
        changed.push((response.href, etag, data));
    }
    Ok(ParsedMultistatus {
        token: parsed.sync_token,
        changed,
        deleted,
    })
}

#[cfg(any(feature = "live", test))]
fn sync_collection_body(token: &str) -> String {
    tilo_dav::body::sync_collection_body(token, &REPORT_PROPS)
}

/// Real `CalDAV` transport, compiled only with the `live` feature. Kept behind a
/// module so its HTTP stack never touches the default (headless) build. This
/// talks HTTP to a configured server; there is no synthesised data here.
#[cfg(feature = "live")]
mod live {
    use std::collections::HashMap;
    use std::time::Duration;

    use chrono::NaiveDate;
    use tilo_dav::net::{
        Credentials, DavClient, Depth, Error as DavError, Precondition, StatusCode,
    };

    use super::{RemoteCalendar, SyncOutcome, SyncResult};
    use crate::account::CalDavConfig;
    use crate::ics;

    const CALENDAR_CONTENT_TYPE: &str = "text/calendar; charset=utf-8";

    /// In a page, servers that do not allow the page's origin are reached
    /// through the host's fetch proxy when it offers one.
    fn client() -> Result<DavClient, DavError> {
        DavClient::with_fetch_proxy(
            &tilo_dav::net::Options {
                user_agent: Some("Tilo-Calendar/0.1"),
                timeout: Duration::from_secs(20),
                ..tilo_dav::net::Options::default()
            },
            tilo_net::proxy::ProxyUse::CrossOrigin,
        )
    }

    fn authenticated(client: &DavClient, config: &CalDavConfig) -> DavClient {
        client.with_credentials(Credentials::new(
            config.username.clone(),
            config.password.clone(),
        ))
    }

    fn dav_error_message(error: &DavError) -> String {
        match error {
            DavError::TooLarge => "calendar response exceeds the 8 MiB safety limit".into(),
            DavError::Xml(error) => super::xml_error_message(error),
            DavError::Transport(error) => error.to_string(),
        }
    }

    pub async fn sync_all_live_with_checkpoints(
        accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
        today: NaiveDate,
        checkpoints: &[(String, String)],
    ) -> (SyncResult, SyncOutcome) {
        let client = match client() {
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
                )
                .await
                {
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

    /// Pull one collection: an RFC 6578 `sync-collection` REPORT from the
    /// stored token, falling back to an authoritative `calendar-query` when
    /// the server refuses the token or the report. Each resource's
    /// `calendar-data` is handed to [`ics::parse`].
    async fn pull_calendar_live(
        client: &DavClient,
        config: &CalDavConfig,
        cal: &RemoteCalendar,
        _today: NaiveDate,
        checkpoint: Option<&str>,
    ) -> Result<SyncResult, String> {
        let client = authenticated(client, config);
        let url = collection_url(config, cal);
        let mut full = checkpoint.is_none();
        let body = super::sync_collection_body(checkpoint.unwrap_or_default());
        let mut response = client
            .report(url.as_str(), Depth::One, body)
            .await
            .map_err(|error| dav_error_message(&error))?;

        // RFC 6578 uses 403 with a valid-sync-token precondition (some servers
        // use 409). Reconcile from a bounded full listing before accepting a
        // replacement token; never silently discard local mapping state.
        let report_unsupported = matches!(response.status().as_u16(), 400 | 403 | 405 | 409 | 501);
        if report_unsupported {
            full = true;
            // Full fallback must be authoritative: a time window cannot
            // distinguish an old resource from a deletion. Response
            // byte/resource caps bound it.
            let query = tilo_dav::body::calendar_query_body(&super::REPORT_PROPS, None);
            response = client
                .report(url.as_str(), Depth::One, query)
                .await
                .map_err(|error| dav_error_message(&error))?;
        }
        if !response.status().is_success() {
            return Err(format!("calendar server returned {}", response.status()));
        }
        let listing = response
            .multistatus(&super::LIMITS)
            .await
            .map_err(|error| dav_error_message(&error))?;
        let parsed = super::interpret_multistatus(listing)?;
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
            discover_sync_token(&client, &url).await?
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

    async fn discover_sync_token(client: &DavClient, url: &str) -> Result<Option<String>, String> {
        let response = client
            .propfind(url, Depth::Zero, &[tilo_dav::props::SYNC_TOKEN])
            .await
            .map_err(|error| dav_error_message(&error))?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let listing = response
            .multistatus(&super::LIMITS)
            .await
            .map_err(|error| dav_error_message(&error))?;
        Ok(listing
            .responses
            .iter()
            .find_map(|response| response.prop_text("sync-token"))
            .filter(|token| token.len() <= 4096)
            .map(ToOwned::to_owned))
    }

    fn collection_url(config: &CalDavConfig, cal: &RemoteCalendar) -> String {
        super::collection_url_for(config, cal)
    }

    /// Normalize every REPORT href to the same absolute identity used by PUT
    /// and DELETE responses. Servers freely alternate between absolute and
    /// path hrefs, so retaining the wire spelling would duplicate resources.
    fn canonical_resource_url(collection_url: &str, href: &str) -> Result<String, String> {
        let base = tilo_dav::net::Url::parse(&format!("{}/", collection_url.trim_end_matches('/')))
            .map_err(|_| "calendar collection has an invalid URL".to_string())?;
        base.join(href)
            .map(|url| url.to_string())
            .map_err(|_| "calendar response contains an invalid resource href".to_string())
    }

    pub async fn push_jobs_live(jobs: &[super::UploadJob]) -> super::MutationResult {
        let Ok(client) = client() else {
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
            let precondition = match &request.if_match {
                Some(etag) => Precondition::IfMatch(etag),
                None if request.if_none_match => Precondition::IfNoneMatchAny,
                None => Precondition::None,
            };
            let sent = authenticated(&client, &job.config)
                .put(
                    request.url.as_str(),
                    std::mem::take(&mut request.body).into_bytes(),
                    Some(CALENDAR_CONTENT_TYPE),
                    precondition,
                )
                .await;
            match sent {
                Ok(response) if response.status().is_success() => {
                    result.confirmed.push(job.event.id);
                    result.revisions.push(super::RemoteRevision {
                        id: job.event.id,
                        calendar_id: job.calendar.id.clone(),
                        href: request.url,
                        etag: response.etag(),
                    });
                }
                Ok(response) if response.status() == StatusCode::PRECONDITION_FAILED => {
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

    pub async fn remove_jobs_live(jobs: &[super::RemoveJob]) -> super::MutationResult {
        let Ok(client) = client() else {
            return super::MutationResult::default();
        };
        let mut result = super::MutationResult::default();
        for job in jobs {
            let url = job.href.as_ref().map_or_else(
                || super::build_delete_request(&job.config, &job.calendar, job.id).url,
                |href| absolute_resource_url(&job.config, href),
            );
            // Even when an older server omitted the revision, keep deletion
            // conditional on the resource still existing. Known revisions use
            // their exact ETag and therefore detect concurrent modification.
            let sent = authenticated(&client, &job.config)
                .delete(
                    url.as_str(),
                    Precondition::IfMatch(job.etag.as_deref().unwrap_or("*")),
                )
                .await;
            match sent {
                Ok(response)
                    if response.status().is_success()
                        || response.status() == StatusCode::NOT_FOUND =>
                {
                    result.confirmed.push(job.id);
                }
                Ok(response) if response.status() == StatusCode::PRECONDITION_FAILED => {
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

    #[cfg(test)]
    mod tests {
        use test_http::{Reply, Responder};

        use super::*;

        fn run<T>(future: impl Future<Output = T>) -> T {
            ui::rt::block_on(future).expect("a test thread may block")
        }

        fn pull_calendar_live(
            client: &DavClient,
            config: &CalDavConfig,
            cal: &RemoteCalendar,
            today: NaiveDate,
            checkpoint: Option<&str>,
        ) -> Result<SyncResult, String> {
            run(super::pull_calendar_live(
                client, config, cal, today, checkpoint,
            ))
        }

        fn push_jobs_live(jobs: &[super::super::UploadJob]) -> super::super::MutationResult {
            run(super::push_jobs_live(jobs))
        }

        fn remove_jobs_live(jobs: &[super::super::RemoveJob]) -> super::super::MutationResult {
            run(super::remove_jobs_live(jobs))
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

        fn today() -> NaiveDate {
            NaiveDate::from_ymd_opt(2026, 6, 15).unwrap()
        }

        #[test]
        fn fake_server_listing_reports_are_incremental_canonical_and_bounded() {
            // Case `fake_server_receives_incremental_report_and_href_is_canonical`.
            {
                let server =
                    Responder::start(vec![Reply::status(207).body(multistatus("token-2"))]);
                let config = config(server.url());
                let result = pull_calendar_live(
                    &client().unwrap(),
                    &config,
                    &calendar(),
                    today(),
                    Some("token-1"),
                )
                .expect("incremental sync");
                let request = server.next_request();
                assert_eq!(request.method, "REPORT");
                assert_eq!(request.target, "/cal/");
                assert_eq!(request.header("depth"), Some("1"));
                assert!(request.header("authorization").is_some());
                assert!(
                    request
                        .body_text()
                        .contains("<D:sync-token>token-1</D:sync-token>")
                );
                assert_eq!(
                    result.resources[0].href,
                    format!("{}/cal/a.ics", server.url())
                );
                assert_eq!(result.checkpoints[0].token, "token-2");
                assert!(!result.checkpoints[0].full);
            }
            // Case `fake_server_invalid_token_falls_back_to_authoritative_report`.
            {
                let server = Responder::start(vec![
                    Reply::status(403),
                    Reply::status(207).body(multistatus("replacement")),
                ]);
                let result = pull_calendar_live(
                    &client().unwrap(),
                    &config(server.url()),
                    &calendar(),
                    today(),
                    Some("expired"),
                )
                .expect("full fallback");
                let incremental = server.next_request();
                let fallback = server.next_request();
                assert!(incremental.body_text().contains("<D:sync-collection"));
                assert!(fallback.body_text().contains("<C:calendar-query"));
                assert!(result.checkpoints[0].full);
                assert_eq!(result.checkpoints[0].token, "replacement");
            }
            // Case `full_listing_without_token_discovers_one_by_propfind`.
            {
                let listing = multistatus("").replace("<D:sync-token></D:sync-token>", "");
                let discovery = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/cal/</d:href><d:propstat><d:prop><d:sync-token>http://sabre.io/ns/sync/7</d:sync-token></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
                let server = Responder::start(vec![
                    Reply::status(207).body(listing),
                    Reply::status(207).body(discovery),
                ]);
                let result = pull_calendar_live(
                    &client().unwrap(),
                    &config(server.url()),
                    &calendar(),
                    today(),
                    None,
                )
                .expect("initial sync");
                let _report = server.next_request();
                let propfind = server.next_request();
                assert_eq!(propfind.method, "PROPFIND");
                assert_eq!(propfind.header("depth"), Some("0"));
                assert!(propfind.body_text().contains("<D:sync-token/>"));
                assert_eq!(result.checkpoints[0].token, "http://sabre.io/ns/sync/7");
                assert!(result.checkpoints[0].full);
            }
            // Case `non_multistatus_success_is_not_an_empty_authoritative_listing`.
            {
                let server = Responder::start(vec![Reply::status(200).body("<html>portal</html>")]);
                let error = pull_calendar_live(
                    &client().unwrap(),
                    &config(server.url()),
                    &calendar(),
                    today(),
                    None,
                )
                .expect_err("an HTML page must not read as an empty calendar");
                assert_eq!(error, "calendar server returned malformed XML");
            }
            // Case `oversized_report_is_refused_without_a_content_length`.
            {
                let server = Responder::start(vec![
                    Reply::status(207)
                        .body(vec![b' '; super::super::MAX_XML_BYTES + 1])
                        .without_content_length(),
                ]);
                let error = pull_calendar_live(
                    &client().unwrap(),
                    &config(server.url()),
                    &calendar(),
                    today(),
                    Some("token"),
                )
                .expect_err("oversized body");
                assert_eq!(error, "calendar response exceeds the 8 MiB safety limit");
            }
        }

        #[test]
        fn fake_server_writes_are_conditional_and_precondition_failures_conflict() {
            // Case `fake_server_observes_conditional_put_and_delete`.
            {
                let server = Responder::start(vec![
                    Reply::status(200).header("ETag", "\"v2\""),
                    Reply::status(200),
                    Reply::status(204),
                ]);
                let config = config(server.url());
                let calendar = calendar();
                let mut event = super::super::pull_calendar(&config, &calendar, today())
                    .events
                    .remove(0);
                event.calendar_id = Some(calendar.id.clone());
                let href = format!("{}/cal/a.ics", server.url());
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
                        href: Some(format!("{}/cal/legacy.ics", server.url())),
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
                let put = server.next_request();
                let legacy_put = server.next_request();
                let delete = server.next_request();
                assert_eq!(
                    (put.method.as_str(), put.target.as_str()),
                    ("PUT", "/cal/a.ics")
                );
                assert_eq!(put.header("if-match"), Some("\"v1\""));
                assert_eq!(put.header("content-type"), Some(CALENDAR_CONTENT_TYPE));
                assert_eq!(legacy_put.target, "/cal/legacy.ics");
                assert_eq!(legacy_put.header("if-match"), Some("*"));
                assert_eq!(
                    (delete.method.as_str(), delete.target.as_str()),
                    ("DELETE", "/cal/a.ics")
                );
                assert_eq!(delete.header("if-match"), Some("\"v2\""));
            }
            // Case `precondition_failures_surface_as_conflicts`.
            {
                let server = Responder::start(vec![Reply::status(412), Reply::status(412)]);
                let config = config(server.url());
                let calendar = calendar();
                let event = super::super::pull_calendar(&config, &calendar, today())
                    .events
                    .remove(0);
                let href = format!("{}/cal/a.ics", server.url());
                let upload = push_jobs_live(&[super::super::UploadJob {
                    config: config.clone(),
                    calendar: calendar.clone(),
                    event: event.clone(),
                    href: Some(href.clone()),
                    etag: Some("\"stale\"".into()),
                }]);
                assert!(upload.confirmed.is_empty());
                assert_eq!(upload.conflicts, vec![(event.id, href.clone())]);
                let removed = remove_jobs_live(&[super::super::RemoveJob {
                    config,
                    calendar,
                    id: event.id,
                    href: Some(href.clone()),
                    etag: Some("\"stale\"".into()),
                }]);
                assert_eq!(removed.conflicts, vec![(event.id, href)]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::AccountStore;

    fn run<T>(future: impl Future<Output = T>) -> T {
        ui::rt::block_on(future).expect("a test thread may block")
    }

    fn sync_all(
        accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
        today: NaiveDate,
    ) -> (SyncResult, SyncOutcome) {
        run(super::sync_all_with_checkpoints(accounts, today, &[]))
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 6, 15).unwrap()
    }

    #[test]
    fn pull_and_sync_tag_events_skip_hidden_and_report_offline() {
        // Case `pull_tags_events_with_calendar_colour`.
        {
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
        // Case `pull_tags_events_with_source_calendar_id`.
        {
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
        // Case `sync_all_skips_hidden_calendars`.
        {
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
        // Case `empty_account_set_reports_offline`.
        {
            let (res, outcome) = sample_pull_all(&[], today());
            assert!(res.is_empty());
            assert_eq!(outcome, SyncOutcome::Offline);
        }
        // Case `live_sync_with_no_accounts_is_offline_and_empty`.
        {
            // The real path never fabricates: with nothing configured it returns
            // an empty Offline result (and, without the `live` feature, always).
            let (res, outcome) = sync_all(&[], today());
            assert!(res.is_empty());
            assert_eq!(outcome, SyncOutcome::Offline);
        }
        // Case `synced_events_parse_through_real_ics_path`.
        {
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

    #[test]
    fn write_requests_target_one_resource_and_stay_inert_without_live() {
        // Case `build_put_request_targets_collection_and_carries_event_ics`.
        {
            use crate::model::Event;
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
                location: "Room 1".into(),
                calendar_id: Some(cal.id.clone()),
                ..Event::default()
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
        // Case `build_delete_request_targets_the_same_resource_as_the_put`.
        {
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
        // Case `remove_jobs_without_live_feature_removes_nothing`.
        {
            // Symmetric with push: the headless build has no transport, so delete is
            // an honest no-op that keeps the queue rather than faking a success.
            let store = AccountStore::sample();
            let (account, cal) = store.all_calendars().next().unwrap();
            let jobs = vec![RemoveJob {
                config: account.config.clone(),
                calendar: cal.clone(),
                id: uuid::Uuid::new_v4(),
                href: None,
                etag: None,
            }];
            #[cfg(not(feature = "live"))]
            assert!(run(remove_jobs(&jobs)).confirmed.is_empty());
            let _ = jobs;
        }
        // Case `push_jobs_without_live_feature_uploads_nothing`.
        {
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
                calendar_id: Some(cal.id.clone()),
                ..Event::default()
            };
            let jobs = vec![UploadJob {
                config: account.config.clone(),
                calendar: cal.clone(),
                event: ev,
                href: None,
                etag: None,
            }];
            #[cfg(not(feature = "live"))]
            assert!(run(push_jobs(&jobs)).confirmed.is_empty());
            let _ = jobs;
        }
        // Case `new_put_is_create_only_until_a_remote_etag_is_known`.
        {
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

    #[test]
    fn multistatus_parses_identity_and_rejects_malformed_input() {
        // Case `multistatus_parses_stable_identity_revision_token_and_deletion`.
        {
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
        // Case `malformed_and_oversized_multistatus_are_rejected_without_partial_data`.
        {
            assert!(parse_multistatus("<D:multistatus>").is_err());
            let oversized = "x".repeat(MAX_XML_BYTES + 1);
            assert!(parse_multistatus(&oversized).is_err());
        }
        // Case `sync_token_is_xml_escaped`.
        {
            let body = sync_collection_body("a<&\"'>");
            assert!(body.contains("a&lt;&amp;&quot;&apos;&gt;"));
            assert!(!body.contains("a<&"));
        }
        // Case `absolute_and_path_dav_hrefs_share_one_identity`.
        {
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
    }
}
