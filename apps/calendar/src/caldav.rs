//! `CalDAV` sync — the real protocol edge of the Calendar app.

use jiff::civil::Date;

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

#[derive(Clone, Debug)]
pub struct RemoteResource {
    pub calendar_id: String,
    pub href: String,
    pub etag: Option<String>,
    pub events: Vec<Event>,
    pub tasks: Vec<Task>,
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncOutcome {
    Synced { calendars: usize, events: usize },
    Offline,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutRequest {
    pub url: String,
    pub body: String,
    pub if_match: Option<String>,
    pub if_none_match: bool,
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteRequest {
    pub url: String,
    pub if_match: Option<String>,
}

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

pub fn push_jobs(jobs: &[UploadJob]) -> impl std::future::Future<Output = MutationResult> {
    #[cfg(feature = "live")]
    {
        live::push_jobs_live(jobs)
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = jobs;
        std::future::ready(MutationResult::default())
    }
}

pub fn remove_jobs(jobs: &[RemoveJob]) -> impl std::future::Future<Output = MutationResult> {
    #[cfg(feature = "live")]
    {
        live::remove_jobs_live(jobs)
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = jobs;
        std::future::ready(MutationResult::default())
    }
}

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

/// Compare DAV href spellings after discarding an optional scheme and authority, so bindings stay
/// stable across servers that alternate between `/dav/x` and `https://host/dav/x`.
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
        // An origin-relative href and its absolute spelling are one target; two absolute hrefs from
        // different origins never are, or a cross-origin response could alias an existing event.
        (Some(left_path), None) => left_path == right,
        (None, Some(right_path)) => left == right_path,
        _ => false,
    }
}

/// Sample pull for one remote calendar (demo and tests only): real iCalendar through [`ics::parse`],
/// never the live transport.
pub fn pull_calendar(_config: &CalDavConfig, calendar: &RemoteCalendar, today: Date) -> SyncResult {
    let payload = sample_calendar_ics(calendar, today);
    let (mut events, tasks) = ics::parse(&payload);
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

pub fn sync_all_with_checkpoints(
    accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
    today: Date,
    checkpoints: &[(String, String)],
) -> impl std::future::Future<Output = (SyncResult, SyncOutcome)> {
    #[cfg(feature = "live")]
    {
        live::sync_all_live_with_checkpoints(accounts, today, checkpoints)
    }
    #[cfg(not(feature = "live"))]
    {
        let _ = (accounts, today, checkpoints);
        std::future::ready((SyncResult::default(), SyncOutcome::Offline))
    }
}

pub fn sample_pull_all(
    accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
    today: Date,
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

fn sample_calendar_ics(calendar: &RemoteCalendar, today: Date) -> String {
    use crate::model::Recurrence;
    use jiff::civil::{DateTime, Time};

    // Every call passes a literal valid (h, m): a wrong sample time beats panicking the sync.
    let at = |date: Date, h: i8, m: i8| -> DateTime {
        date.to_datetime(Time::new(h, m, 0, 0).unwrap_or(Time::MIN))
    };
    let d = |delta: i64| today + jiff::Span::new().days(delta);

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
const LIMITS: tilo_dav::Limits = tilo_dav::Limits {
    max_bytes: MAX_XML_BYTES,
    max_responses: MAX_RESOURCES,
    max_depth: 32,
    max_nodes: 1_000_000,
    max_href_bytes: MAX_HREF_BYTES,
};

#[cfg(any(feature = "live", test))]
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

#[cfg(test)]
fn parse_multistatus(xml: &str) -> Result<ParsedMultistatus, String> {
    let parsed = tilo_dav::multistatus::parse(xml, &LIMITS).map_err(|e| xml_error_message(&e))?;
    interpret_multistatus(parsed)
}

#[cfg(any(feature = "live", test))]
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

/// Real `CalDAV` transport, compiled only with the `live` feature, so its HTTP stack never touches
/// the default build.
#[cfg(feature = "live")]
mod live {
    use jiff::civil::Date;
    use std::collections::HashMap;
    use tilo_dav::net::{Depth, Error as DavError, Precondition, StatusCode};
    use tilo_dav::oauth::{AuthorisedClient, Service};

    use super::{RemoteCalendar, SyncOutcome, SyncResult};
    use crate::account::CalDavConfig;
    use crate::ics;

    const CALENDAR_CONTENT_TYPE: &str = "text/calendar; charset=utf-8";
    /// What a person is told where writing an event would have ended what its calendar
    /// server schedules of it (RFC 6638, 3.2.3.1).
    const WOULD_CALL_IT_OFF: &str = "the calendar server writes to the people this event names, \
         and the event here no longer says who organises it: writing it would have the server \
         call it off, so nothing was written to it";

    pub(crate) struct CalDavClient {
        service: Service,
    }

    impl CalDavClient {
        pub(crate) fn new(service: Service) -> Self {
            Self { service }
        }
    }

    fn client() -> CalDavClient {
        CalDavClient::new(Service::live())
    }

    async fn authenticated(
        client: &CalDavClient,
        config: &CalDavConfig,
        target: &str,
    ) -> Result<AuthorisedClient, String> {
        let service = client.service.clone();
        let config = config.clone();
        let target = target.to_owned();
        ui::rt::app()
            .spawn_blocking(move || {
                // The calendar of a Google account a member turned it on for signs in with
                // that account's grant.
                let account = config.oauth_account_id.as_deref().map(|id| {
                    accounts::Account::new(
                        accounts::AccountId::from(id),
                        accounts::cloud_types::GOOGLE_DRIVE,
                    )
                });
                service.authorised(
                    account.as_ref(),
                    &config.server_url,
                    &config.username,
                    &config.password,
                    &target,
                )
            })
            .await
            .map_err(|_| "calendar credential task failed")?
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
        today: Date,
        checkpoints: &[(String, String)],
    ) -> (SyncResult, SyncOutcome) {
        let client = client();

        client.sync_all(accounts, today, checkpoints).await
    }

    impl CalDavClient {
        pub(crate) async fn sync_all(
            &self,
            accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
            today: Date,
            checkpoints: &[(String, String)],
        ) -> (SyncResult, SyncOutcome) {
            let client = self;
            let checkpoint_map: HashMap<&str, &str> = checkpoints
                .iter()
                .map(|(calendar, token)| (calendar.as_str(), token.as_str()))
                .collect();
            let mut merged = SyncResult::default();
            let mut count = 0usize;
            for (config, calendars) in accounts {
                for cal in calendars.iter().filter(|c| c.visible) {
                    match pull_calendar_live(
                        client,
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
    }

    async fn pull_calendar_live(
        client: &CalDavClient,
        config: &CalDavConfig,
        cal: &RemoteCalendar,
        _today: Date,
        checkpoint: Option<&str>,
    ) -> Result<SyncResult, String> {
        let url = collection_url(config, cal);
        let mut full = checkpoint.is_none();
        let body = super::sync_collection_body(checkpoint.unwrap_or_default());
        let mut response = authenticated(client, config, &url)
            .await?
            .report(url.as_str(), Depth::One, body)
            .await?;

        // RFC 6578 says 403 with a valid-sync-token precondition, and some servers use 409: reconcile from a
        // bounded full listing before accepting a replacement token, never discarding the mapping.
        let report_unsupported = matches!(response.status().as_u16(), 400 | 403 | 405 | 409 | 501);
        if report_unsupported {
            full = true;
            // The full fallback is authoritative: a time window cannot tell an old resource from a deletion.
            let query = tilo_dav::body::calendar_query_body(&super::REPORT_PROPS, None);
            response = authenticated(client, config, &url)
                .await?
                .report(url.as_str(), Depth::One, query)
                .await?;
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
            discover_sync_token(&authenticated(client, config, &url).await?, &url).await?
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

    async fn discover_sync_token(
        client: &AuthorisedClient,
        url: &str,
    ) -> Result<Option<String>, String> {
        let response = client
            .propfind(url, Depth::Zero, &[tilo_dav::props::SYNC_TOKEN])
            .await?;
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

    fn canonical_resource_url(collection_url: &str, href: &str) -> Result<String, String> {
        let base = tilo_dav::net::Url::parse(&format!("{}/", collection_url.trim_end_matches('/')))
            .map_err(|_| "calendar collection has an invalid URL".to_string())?;
        base.join(href)
            .map(|url| url.to_string())
            .map_err(|_| "calendar response contains an invalid resource href".to_string())
    }

    pub async fn push_jobs_live(jobs: &[super::UploadJob]) -> super::MutationResult {
        client().push(jobs).await
    }

    impl CalDavClient {
        pub(crate) async fn push(&self, jobs: &[super::UploadJob]) -> super::MutationResult {
            let client = self;
            let mut result = super::MutationResult::default();
            for job in jobs {
                if job.calendar.read_only {
                    result
                        .failures
                        .push((job.event.id, "Calendar is read-only".into()));
                    continue;
                }
                let mut request = super::build_put_request(&job.config, &job.calendar, &job.event);
                if let Some(href) = &job.href {
                    request.url = absolute_resource_url(&job.config, href);
                    // An existing binding is never overwritten unconditionally: exact ETags detect edits, and `*` is the
                    // fallback where the binding predates ETag storage.
                    request.if_match = Some(job.etag.clone().unwrap_or_else(|| "*".into()));
                    request.if_none_match = false;
                }
                let authenticated = match authenticated(client, &job.config, &request.url).await {
                    Ok(client) => client,
                    Err(error) => {
                        result.failures.push((job.event.id, error));
                        continue;
                    }
                };
                // A server that schedules by itself is never started writing to anybody, and
                // never made to invite or call off by a line that says something else of who
                // writes than the server holds (RFC 6638, 3.2.1.1 and 3.2.1.2). A new entry
                // has every line marked. Over an existing one, what the server holds is read
                // first, also where the event names nobody here: its entry may name people
                // there, and goes by the UID it has there. It is written over as the version
                // that was read and as no other.
                if job.href.is_some() {
                    let held =
                        match held_now(&authenticated, &request.url, job.etag.as_deref()).await {
                            Ok((text, version)) => {
                                request.if_match = Some(version);
                                text
                            }
                            Err(Unread::Gone | Unread::Changed) => {
                                result.conflicts.push((job.event.id, request.url));
                                continue;
                            }
                            Err(Unread::Failed(reason)) => {
                                result.failures.push((job.event.id, reason));
                                continue;
                            }
                        };
                    // Whom the server holds and an event here cannot hold stays as held.
                    request.body = ics::exported_beside(&request.body, &held);
                    if ics::ends_what_its_server_schedules(&request.body, &held) {
                        result
                            .failures
                            .push((job.event.id, WOULD_CALL_IT_OFF.into()));
                        continue;
                    }
                } else if names_somebody(&request.body) {
                    request.body = ics::kept_text(&request.body);
                }
                let precondition = match &request.if_match {
                    Some(etag) => Precondition::IfMatch(etag),
                    None if request.if_none_match => Precondition::IfNoneMatchAny,
                    None => Precondition::None,
                };
                let sent = authenticated
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
                    Err(error) => result.failures.push((job.event.id, error)),
                }
            }
            result
        }
    }

    pub async fn remove_jobs_live(jobs: &[super::RemoveJob]) -> super::MutationResult {
        client().remove(jobs).await
    }

    impl CalDavClient {
        pub(crate) async fn remove(&self, jobs: &[super::RemoveJob]) -> super::MutationResult {
            let client = self;
            let mut result = super::MutationResult::default();
            for job in jobs {
                if job.calendar.read_only {
                    result
                        .failures
                        .push((job.id, "Calendar is read-only".into()));
                    continue;
                }
                let url = job.href.as_ref().map_or_else(
                    || super::build_delete_request(&job.config, &job.calendar, job.id).url,
                    |href| absolute_resource_url(&job.config, href),
                );
                let authenticated = match authenticated(client, &job.config, &url).await {
                    Ok(client) => client,
                    Err(error) => {
                        result.failures.push((job.id, error));
                        continue;
                    }
                };
                // A removal names the version it takes out by a strong tag (RFC 7232, 3.1).
                // Where the listing wrote this entry's version as none, the entry is read
                // first and taken out as the version read.
                let known = job.etag.as_deref();
                let read = if let Some(listed) = known
                    && !is_strong(listed)
                {
                    match held_now(&authenticated, &url, known).await {
                        Ok((_, version)) => Some(version),
                        Err(Unread::Gone) => {
                            result.confirmed.push(job.id);
                            continue;
                        }
                        Err(Unread::Changed) => {
                            result.conflicts.push((job.id, url));
                            continue;
                        }
                        Err(Unread::Failed(reason)) => {
                            result.failures.push((job.id, reason));
                            continue;
                        }
                    }
                } else {
                    None
                };
                // Where its person was invited, taking an entry out is not declining it: the
                // server answers the organiser nothing for them (RFC 6638, 3.2.2.4 and 8.1).
                let sent = authenticated
                    .delete_saying(
                        url.as_str(),
                        &[("Schedule-Reply", "F")],
                        Precondition::IfMatch(read.as_deref().or(known).unwrap_or("*")),
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
                    Err(error) => result.failures.push((job.id, error)),
                }
            }
            result
        }
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

    /// The opaque tag of the entity-tag `tag`: what stands between its quotes, without the
    /// mark of a weak one (RFC 7232, 2.3). A listing that leaves the quotes out is read as
    /// the tag it means.
    fn opaque_tag(tag: &str) -> &str {
        let tag = tag.trim();
        let tag = tag.strip_prefix("W/").unwrap_or(tag);
        tag.strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(tag)
    }

    /// Whether `tag` is a strong entity-tag, the only kind a write names the version it
    /// goes over by (RFC 7232, 3.1).
    fn is_strong(tag: &str) -> bool {
        let tag = tag.trim();
        tag.len() >= 2 && tag.starts_with('"') && tag.ends_with('"')
    }

    /// Whether two entity-tags name one version of an entry: their opaque tags match,
    /// whether or not either is marked weak (RFC 7232, 2.3.2, the weak comparison). A
    /// server may write a version one way in a listing and another in a read.
    fn same_version(known: &str, read: &str) -> bool {
        opaque_tag(known) == opaque_tag(read)
    }

    /// Why what a server holds of an entry was not taken as read.
    enum Unread {
        /// It is gone.
        Gone,
        /// It is no longer the version this calendar last saw.
        Changed,
        /// It could not be read: why, for the person.
        Failed(String),
    }

    /// Whether the entry `body` names somebody a calendar server could write to: who organises
    /// it, or somebody it invites.
    fn names_somebody(body: &str) -> bool {
        let named = ics::named(body);
        !(named.organizers.is_empty() && named.attendees.is_empty())
    }

    /// What the server holds at `url` now, and its version. It is read before an entry is
    /// written over it, because what each line says of who writes to the person it names,
    /// the `UID` the entry goes by and whom it names that an event here cannot hold have to
    /// stay as the server holds them (RFC 6638, 3.2.1.2; [`ics::exported_beside`]). `known`
    /// is the version this calendar last saw there: a server that holds another one changed
    /// the entry since, and nothing is written over it. A version is one version however a
    /// listing and a read write it ([`same_version`]); the one answered is the strong tag
    /// of the two, which a write goes over.
    async fn held_now(
        client: &AuthorisedClient,
        url: &str,
        known: Option<&str>,
    ) -> Result<(String, String), Unread> {
        let unread = |why: &str| {
            Unread::Failed(format!(
                "the calendar server did not show what it holds of this event ({why}), so \
                 nothing was written to it"
            ))
        };
        let response = client.get(url).await.map_err(|error| unread(&error))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(Unread::Gone);
        }
        if !response.status().is_success() {
            return Err(unread(&format!("server returned {}", response.status())));
        }
        let Some(read) = response.etag() else {
            return Err(unread("it gave no version of it"));
        };
        if known.is_some_and(|known| !same_version(known, &read)) {
            return Err(Unread::Changed);
        }
        // A write names the version it goes over by a strong tag (RFC 7232, 3.1): where
        // the read gave a weak one and the listing a strong one of the same version, by
        // the listing's.
        let version = if let Some(known) = known
            && !is_strong(&read)
            && is_strong(known)
        {
            known.to_owned()
        } else {
            read
        };
        let bytes = response
            .read_bounded(super::MAX_ICAL_BYTES)
            .await
            .map_err(|error| unread(&dav_error_message(&error)))?;
        let text = String::from_utf8(bytes).map_err(|_| unread("it is not text"))?;
        Ok((text, version))
    }

    #[cfg(test)]
    mod tests {
        use test_http::{Reply, Responder};

        use super::*;

        include!("caldav_adoption_tests.rs");

        fn run<T>(future: impl Future<Output = T>) -> T {
            ui::rt::block_on(future).expect("a test thread may block")
        }

        fn pull_calendar_live(
            client: &CalDavClient,
            config: &CalDavConfig,
            cal: &RemoteCalendar,
            today: Date,
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
                password_item: None,
                oauth_account_id: None,
            }
        }

        fn calendar() -> RemoteCalendar {
            RemoteCalendar {
                id: "cal/".into(),
                name: "Test".into(),
                color: 0,
                visible: true,
                read_only: false,
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

        fn today() -> Date {
            Date::new(2026, 6, 15).unwrap()
        }

        /// The entry of [`multistatus`] as a scripted server answers a read of it.
        const HELD_REVIEW: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Review\r\n\
             DTSTART:20260615T090000\r\nDTEND:20260615T100000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

        tilo_test_env::scenario! {
            /// The live CalDAV client against a scripted server.
            fn the_live_client_reports_incrementally_and_guards_its_writes;

            phase fake_server_listing_reports_are_incremental_canonical_and_bounded;
            {
                {
                    let server =
                        Responder::start(vec![Reply::status(207).body(multistatus("token-2"))]);
                    let config = config(server.url());
                    let result =
                        pull_calendar_live(&client(), &config, &calendar(), today(), Some("token-1"))
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
                {
                    let server = Responder::start(vec![
                        Reply::status(403),
                        Reply::status(207).body(multistatus("replacement")),
                    ]);
                    let result = pull_calendar_live(
                        &client(),
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
                {
                    let listing = multistatus("").replace("<D:sync-token></D:sync-token>", "");
                    let discovery = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/cal/</d:href><d:propstat><d:prop><d:sync-token>http://sabre.io/ns/sync/7</d:sync-token></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
                    let server = Responder::start(vec![
                        Reply::status(207).body(listing),
                        Reply::status(207).body(discovery),
                    ]);
                    let result = pull_calendar_live(
                        &client(),
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
                {
                    let server = Responder::start(vec![Reply::status(200).body("<html>portal</html>")]);
                    let error = pull_calendar_live(
                        &client(),
                        &config(server.url()),
                        &calendar(),
                        today(),
                        None,
                    )
                    .expect_err("an HTML page must not read as an empty calendar");
                    assert_eq!(error, "calendar server returned malformed XML");
                }
                {
                    let server = Responder::start(vec![
                        Reply::status(207)
                            .body(vec![b' '; super::super::MAX_XML_BYTES + 1])
                            .without_content_length(),
                    ]);
                    let error = pull_calendar_live(
                        &client(),
                        &config(server.url()),
                        &calendar(),
                        today(),
                        Some("token"),
                    )
                    .expect_err("oversized body");
                    assert_eq!(error, "calendar response exceeds the 8 MiB safety limit");
                }
            }

            phase read_only_collections_never_send_mutations;
            {
                let server = Responder::silent();
                let config = config(server.url());
                let mut calendar = calendar();
                calendar.read_only = true;
                let event = super::super::pull_calendar(&config, &calendar, today())
                    .events
                    .remove(0);
                let upload = push_jobs_live(&[super::super::UploadJob {
                    config: config.clone(),
                    calendar: calendar.clone(),
                    event: event.clone(),
                    href: None,
                    etag: None,
                }]);
                assert!(upload.confirmed.is_empty());
                assert_eq!(
                    upload.failures,
                    vec![(event.id, "Calendar is read-only".into())]
                );
                let removed = remove_jobs_live(&[super::super::RemoveJob {
                    config,
                    calendar,
                    id: event.id,
                    href: None,
                    etag: None,
                }]);
                assert!(removed.confirmed.is_empty());
                assert_eq!(
                    removed.failures,
                    vec![(event.id, "Calendar is read-only".into())]
                );
                server.assert_untouched();
            }

            phase fake_server_writes_are_conditional_and_precondition_failures_conflict;
            {
                {
                    // What the server holds is read before each write over it.
                    let server = Responder::start(vec![
                        Reply::status(200)
                            .header("ETag", "\"v1\"")
                            .body(HELD_REVIEW),
                        Reply::status(200).header("ETag", "\"v2\""),
                        Reply::status(200)
                            .header("ETag", "\"l1\"")
                            .body(HELD_REVIEW),
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
                    let read = server.next_request();
                    let put = server.next_request();
                    let legacy_read = server.next_request();
                    let legacy_put = server.next_request();
                    let delete = server.next_request();
                    assert_eq!(
                        (read.method.as_str(), read.target.as_str()),
                        ("GET", "/cal/a.ics")
                    );
                    assert_eq!(
                        (put.method.as_str(), put.target.as_str()),
                        ("PUT", "/cal/a.ics")
                    );
                    assert_eq!(put.header("if-match"), Some("\"v1\""));
                    assert_eq!(put.header("content-type"), Some(CALENDAR_CONTENT_TYPE));
                    assert_eq!(
                        (legacy_read.method.as_str(), legacy_read.target.as_str()),
                        ("GET", "/cal/legacy.ics")
                    );
                    assert_eq!(
                        (legacy_put.method.as_str(), legacy_put.target.as_str()),
                        ("PUT", "/cal/legacy.ics")
                    );
                    // Where this calendar kept no version of an entry, it is written over
                    // as the version just read, not as whatever stands there.
                    assert_eq!(legacy_put.header("if-match"), Some("\"l1\""));
                    assert_eq!(
                        (delete.method.as_str(), delete.target.as_str()),
                        ("DELETE", "/cal/a.ics")
                    );
                    assert_eq!(delete.header("if-match"), Some("\"v2\""));
                }
                {
                    // The entry changes on its server between the read and the write.
                    let server = Responder::start(vec![
                        Reply::status(200)
                            .header("ETag", "\"stale\"")
                            .body(HELD_REVIEW),
                        Reply::status(412),
                        Reply::status(412),
                    ]);
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

            phase what_a_calendar_server_is_sent_tells_it_to_write_to_nobody;
            {
                let server = Responder::start(vec![
                    Reply::status(201).header("ETag", "\"v1\""),
                    Reply::status(204),
                ]);
                let config = config(server.url());
                let calendar = calendar();
                let mut event = super::super::pull_calendar(&config, &calendar, today())
                    .events
                    .remove(0);
                event.calendar_id = Some(calendar.id.clone());
                // Its person organises it and invites Bo.
                let organizer = crate::model::Attendee {
                    id: crate::model::Id::new_v4(),
                    name: "Ada".into(),
                    email: "ada@example.test".into(),
                    status: crate::model::InviteStatus::Accepted,
                };
                event.organizer = Some(organizer.id);
                event.attendees.push(organizer);
                event.attendees.push(crate::model::Attendee {
                    id: crate::model::Id::new_v4(),
                    name: "Bo".into(),
                    email: "bo@example.test".into(),
                    status: crate::model::InviteStatus::NeedsAction,
                });
                let upload = push_jobs_live(&[super::super::UploadJob {
                    config: config.clone(),
                    calendar: calendar.clone(),
                    event: event.clone(),
                    href: None,
                    etag: None,
                }]);
                assert_eq!(upload.confirmed, vec![event.id]);
                let put = server.next_request();
                assert_eq!(put.method, "PUT");
                let text = put.body_text().replace("\r\n ", "");
                let naming: Vec<&str> = text
                    .lines()
                    .filter(|line| line.starts_with("ORGANIZER") || line.starts_with("ATTENDEE"))
                    .collect();
                assert_eq!(naming.len(), 3, "{text}");
                for line in naming {
                    assert!(
                        line.contains(";SCHEDULE-AGENT=CLIENT"),
                        "the calendar server is left to write to whoever this line names: {line}"
                    );
                }
                let removed = remove_jobs_live(&[super::super::RemoveJob {
                    config,
                    calendar,
                    id: event.id,
                    href: Some(format!("{}/cal/{}.ics", server.url(), event.id)),
                    etag: Some("\"v1\"".into()),
                }]);
                assert_eq!(removed.confirmed, vec![event.id]);
                let delete = server.next_request();
                assert_eq!(delete.method, "DELETE");
                assert_eq!(
                    delete.header("schedule-reply"),
                    Some("F"),
                    "a removal tells the calendar server to answer nobody"
                );
            }
        }

        /// The address the calendar server these tests start writes to its person at: the
        /// name her account signs in with.
        const ADA: &str = "ada@example.test";
        /// Whom that server wrote to by itself, where that is nobody.
        const NOBODY: [&str; 0] = [];
        /// What stands on a line that names somebody: nothing, which leaves writing to them
        /// to the server, or that the client writes.
        const LEFT_TO_SERVER: &str = "";
        const CLIENT: &str = ";SCHEDULE-AGENT=CLIENT";

        /// Ada's calendar server, which schedules by itself and answers by RFC 6638's
        /// tables, with her account as this device keeps it and her one calendar there.
        fn adas() -> (crate::fake_caldav::Server, CalDavConfig, RemoteCalendar) {
            let server = crate::fake_caldav::Server::start(ADA, "secret");
            let work = server.add_calendar("work", "Work");
            let account = CalDavConfig {
                username: ADA.into(),
                ..config(server.url())
            };
            let work = RemoteCalendar {
                id: work,
                ..calendar()
            };
            (server, account, work)
        }

        /// An entry as a server holds it: `by` organises it and invites `invited`, and
        /// `mark` stands on every line that names somebody.
        fn held_entry(uid: &str, by: &str, invited: &[&str], mark: &str) -> String {
            let people: String =
                std::iter::once(format!("ORGANIZER{mark};CN=Organiser:mailto:{by}\r\n"))
                    .chain(invited.iter().map(|address| {
                        format!(
                            "ATTENDEE{mark};PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{address}\r\n"
                        )
                    }))
                    .collect();
            format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Server//EN\r\n\
                 BEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:Dinner\r\nDTSTART:20261119T190000Z\r\n\
                 DTEND:20261119T200000Z\r\n{people}END:VEVENT\r\nEND:VCALENDAR\r\n"
            )
        }

        /// The lines of the entry `text` that name somebody, each whole.
        fn naming_lines(text: &str) -> Vec<String> {
            text.replace("\r\n ", "")
                .lines()
                .filter(|line| line.starts_with("ORGANIZER") || line.starts_with("ATTENDEE"))
                .map(str::to_owned)
                .collect()
        }

        /// The one entry Ada's calendar holds on her server.
        fn held_text(server: &crate::fake_caldav::Server) -> String {
            let mut held = server.held("work");
            assert_eq!(held.len(), 1, "her calendar holds one entry");
            held.remove(0).1
        }

        /// The upload of the one entry Ada's calendar holds, as this device read it just
        /// now: where the server keeps it and in which version.
        fn as_pulled(account: &CalDavConfig, work: &RemoteCalendar) -> super::super::UploadJob {
            let mut pulled = pull_calendar_live(&client(), account, work, today(), None)
                .expect("her calendar reads");
            let mut resource = pulled.resources.remove(0);
            let mut event = resource.events.remove(0);
            event.calendar_id = Some(work.id.clone());
            super::super::UploadJob {
                config: account.clone(),
                calendar: work.clone(),
                event,
                href: Some(resource.href),
                etag: resource.etag,
            }
        }

        fn attendee(name: &str, email: &str) -> crate::model::Attendee {
            crate::model::Attendee {
                id: crate::model::Id::new_v4(),
                name: name.into(),
                email: email.into(),
                status: crate::model::InviteStatus::NeedsAction,
            }
        }

        fn methods(requests: &[test_http::Request]) -> Vec<&str> {
            requests
                .iter()
                .map(|request| request.method.as_str())
                .collect()
        }

        tilo_test_env::scenario! {
            /// The live client against a calendar server that schedules by itself and answers
            /// by RFC 6638's tables: nothing this calendar stores or removes starts it writing
            /// to anybody.
            fn a_calendar_server_is_never_started_writing_to_anybody;

            phase a_new_entry_that_invites_people_and_its_removal_have_the_server_write_to_nobody;
            {
                let (server, account, work) = adas();
                let ada = attendee("Ada", ADA);
                let mut event = crate::model::Event {
                    id: crate::model::Id::new_v4(),
                    title: "Dinner".into(),
                    start: today().at(19, 0, 0, 0),
                    end: today().at(20, 0, 0, 0),
                    calendar_id: Some(work.id.clone()),
                    organizer: Some(ada.id),
                    ..crate::model::Event::default()
                };
                event.attendees.push(ada);
                event.attendees.push(attendee("Bo", "bo@example.test"));
                event.attendees.push(attendee("Eve", "eve@elsewhere.test"));
                let upload = push_jobs_live(&[super::super::UploadJob {
                    config: account.clone(),
                    calendar: work.clone(),
                    event: event.clone(),
                    href: None,
                    etag: None,
                }]);
                assert_eq!(upload.confirmed, vec![event.id], "{:?}", upload.failures);
                // RFC 6638, 3.2.1.1: a new entry's line left to the server has it invite
                // whoever the line names.
                assert_eq!(
                    server.mailed(),
                    NOBODY,
                    "her calendar server invited the people a new event names"
                );
                let naming = naming_lines(&held_text(&server));
                assert_eq!(naming.len(), 4, "{naming:?}");
                for line in &naming {
                    assert!(
                        line.contains(";SCHEDULE-AGENT=CLIENT"),
                        "the calendar server is left to write to whoever this line names: {line}"
                    );
                }
                let stored = &upload.revisions[0];
                let removed = remove_jobs_live(&[super::super::RemoveJob {
                    config: account,
                    calendar: work,
                    id: event.id,
                    href: Some(stored.href.clone()),
                    etag: stored.etag.clone(),
                }]);
                assert_eq!(removed.confirmed, vec![event.id], "{:?}", removed.failures);
                assert!(server.held("work").is_empty());
                // RFC 6638, 3.2.1.3: a removed entry's line left to the server has it tell
                // whoever the line names that the event is off.
                assert_eq!(
                    server.mailed(),
                    NOBODY,
                    "her calendar server told people a removed event is off"
                );
                let requests = server.requests();
                let removal = requests
                    .iter()
                    .find(|request| request.method == "DELETE")
                    .expect("the removal");
                assert_eq!(
                    removal.header("Schedule-Reply"),
                    Some("F"),
                    "a removal tells the calendar server to answer nobody"
                );
            }

            phase an_invitation_taken_out_has_the_server_answer_its_organiser_nothing;
            {
                // RFC 6638, 3.2.2.4 and 8.1: an invitation the server delivered leaves
                // answering its organiser to the server, which says she declines when the
                // entry goes, unless the removal says not to answer.
                let (server, account, work) = adas();
                server.put(
                    "work",
                    "offsite.ics",
                    &held_entry(
                        "offsite-9@example.org",
                        "bo@example.test",
                        &[ADA, "cy@example.test"],
                        LEFT_TO_SERVER,
                    ),
                );
                let job = as_pulled(&account, &work);
                let removed = remove_jobs_live(&[super::super::RemoveJob {
                    config: account,
                    calendar: work,
                    id: job.event.id,
                    href: job.href.clone(),
                    etag: job.etag.clone(),
                }]);
                assert_eq!(removed.confirmed, vec![job.event.id], "{:?}", removed.failures);
                assert!(server.held("work").is_empty());
                assert_eq!(
                    server.mailed(),
                    NOBODY,
                    "taking an invitation out of her calendar declined it for her"
                );
            }
        }

        tilo_test_env::scenario! {
            /// The live client against the same server: what an entry's lines say of who
            /// writes to the people they name goes up as its server holds it (RFC 6638,
            /// 3.2.1.2), and an entry that is no longer what was read is not written.
            fn what_an_entry_says_of_who_writes_to_people_goes_up_as_its_server_holds_it;

            phase an_entry_stored_with_client_marks_goes_up_again_with_them;
            {
                let (server, account, work) = adas();
                // As her own server keeps what it puts in: she organises, and every line
                // says that the client writes.
                server.put(
                    "work",
                    "dinner.ics",
                    &held_entry(
                        "dinner-3@example.org",
                        ADA,
                        &["bo@example.test", "eve@elsewhere.test"],
                        CLIENT,
                    ),
                );
                let mut job = as_pulled(&account, &work);
                job.event.title = "Dinner an hour later".into();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "{:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                // The table's row CLIENT, column SERVER: a line that loses its mark has the
                // server invite whoever it names.
                assert_eq!(
                    server.mailed(),
                    NOBODY,
                    "her calendar server invited the people of an event that was only changed"
                );
                let text = held_text(&server);
                assert!(text.contains("Dinner an hour later"), "{text}");
                let naming = naming_lines(&text);
                assert!(naming.len() >= 3, "{naming:?}");
                for line in &naming {
                    assert!(
                        line.contains(";SCHEDULE-AGENT=CLIENT"),
                        "the calendar server is left to write to whoever this line names: {line}"
                    );
                }
            }

            phase a_line_its_server_writes_for_goes_up_as_held_and_a_new_guest_is_written_to_by_nobody;
            {
                let (server, account, work) = adas();
                // Another client stored it and left its lines to the server, which invited
                // both.
                let stored = held_entry(
                    "lunch-4@example.org",
                    ADA,
                    &["bo@example.test", "eve@elsewhere.test"],
                    LEFT_TO_SERVER,
                );
                server.put("work", "lunch.ics", &stored);
                let mut job = as_pulled(&account, &work);
                job.event.title = "Lunch moved".into();
                job.event.attendees.push(attendee("Cy", "cy@example.test"));
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "{:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                // The table's row SERVER has no column that sends nothing. Its column SERVER
                // is the server going on as it did, to the two it already wrote to; its
                // column CLIENT would have it tell them the lunch is off. Cy is new: row
                // <Absent>, column CLIENT, which sends nothing.
                assert_eq!(
                    server.mailed(),
                    ["REQUEST to bo@example.test", "REQUEST to eve@elsewhere.test"],
                    "the server goes on writing to those it already wrote to, calls nothing \
                     off and starts writing to nobody"
                );
                let text = held_text(&server);
                assert!(text.contains("Lunch moved"), "{text}");
                let now = naming_lines(&text);
                for line in naming_lines(&stored) {
                    assert!(
                        now.contains(&line),
                        "a line the server writes for changed: {line} is not in {now:?}"
                    );
                }
                assert!(
                    now.iter().any(|line| {
                        line.contains("cy@example.test") && line.contains(";SCHEDULE-AGENT=CLIENT")
                    }),
                    "{now:?}"
                );
            }

            phase an_entry_that_changed_on_its_server_or_cannot_be_read_is_not_written;
            {
                let (server, account, work) = adas();
                let ours = held_entry("dinner-3@example.org", ADA, &["bo@example.test"], CLIENT);
                server.put("work", "dinner.ics", &ours);
                let mut job = as_pulled(&account, &work);
                job.event.title = "Dinner, changed here".into();
                // Meanwhile another client stores it with its lines left to the server.
                let theirs = held_entry(
                    "dinner-3@example.org",
                    ADA,
                    &["bo@example.test"],
                    LEFT_TO_SERVER,
                );
                server.put("work", "dinner.ics", &theirs);
                let _ = server.requests();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert!(upload.confirmed.is_empty());
                assert_eq!(upload.conflicts.len(), 1, "{:?}", upload.failures);
                assert_eq!(
                    methods(&server.requests()),
                    ["GET"],
                    "a read that is no longer true is followed by no write"
                );
                assert_eq!(held_text(&server), theirs);
                assert_eq!(server.mailed(), NOBODY);

                // The entry is gone from the server.
                server.remove("work", "dinner.ics");
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert!(upload.confirmed.is_empty());
                assert_eq!(upload.conflicts.len(), 1, "{:?}", upload.failures);
                assert_eq!(methods(&server.requests()), ["GET"]);
                assert!(server.held("work").is_empty());
                assert_eq!(server.mailed(), NOBODY);

                // A server that does not answer the read, and one that gives no version of
                // what it holds: nothing is written, and the person is told why.
                for (reply, why) in [
                    (Reply::status(500), "the read fails"),
                    (Reply::status(200).body(ours), "the read has no version"),
                ] {
                    let scripted = Responder::start(vec![reply]);
                    let mut blind = job.clone();
                    blind.config.server_url = scripted.url();
                    blind.href = Some(format!("{}/cal/dinner.ics", scripted.url()));
                    let upload = push_jobs_live(std::slice::from_ref(&blind));
                    assert!(upload.confirmed.is_empty(), "{why}");
                    assert!(upload.conflicts.is_empty(), "{why}");
                    assert_eq!(upload.failures.len(), 1, "{why}");
                    assert!(
                        upload.failures[0].1.contains("nothing was written"),
                        "{why}: {}",
                        upload.failures[0].1
                    );
                    assert_eq!(methods(&scripted.drain_requests()), ["GET"], "{why}");
                }
            }
        }

        /// The `UID` lines of the entry `text`, each whole.
        fn uid_lines(text: &str) -> Vec<String> {
            text.replace("\r\n ", "")
                .lines()
                .filter(|line| line.starts_with("UID"))
                .map(str::to_owned)
                .collect()
        }

        tilo_test_env::scenario! {
            /// The live client against the same server: an entry another calendar program
            /// made goes up under the `UID` its server holds it under. An event is known by
            /// its `UID`: an entry written over by one under another is refused (RFC 4791,
            /// 5.3.2.1), or, by a server that takes it, read as another event.
            fn an_entry_made_elsewhere_goes_up_under_the_uid_its_server_holds;

            phase a_server_that_keeps_the_standard_takes_her_change_to_it;
            {
                let (server, account, work) = adas();
                let stored = held_entry(
                    "lunch-4@example.org",
                    ADA,
                    &["bo@example.test"],
                    LEFT_TO_SERVER,
                );
                server.put("work", "lunch.ics", &stored);
                let mut job = as_pulled(&account, &work);
                job.event.title = "Lunch moved".into();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "her change to an entry made elsewhere did not go up: {:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                let text = held_text(&server);
                assert!(text.contains("Lunch moved"), "{text}");
                assert_eq!(uid_lines(&text), ["UID:lunch-4@example.org"], "{text}");
                // The table's row SERVER, column SERVER: the server goes on writing to the
                // one it already wrote to, about the event he knows.
                assert_eq!(server.mailed(), ["REQUEST to bo@example.test"]);
                // Read again, it is the event this calendar already has.
                assert_eq!(as_pulled(&account, &work).event.id, job.event.id);
            }

            phase a_server_that_takes_another_uid_is_given_the_one_it_holds;
            {
                let (server, account, work) = adas();
                server.takes_another_uid();
                let stored = held_entry(
                    "lunch-4@example.org",
                    ADA,
                    &["bo@example.test"],
                    LEFT_TO_SERVER,
                );
                server.put("work", "lunch.ics", &stored);
                let mut job = as_pulled(&account, &work);
                job.event.title = "Lunch moved".into();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "{:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                // Under another UID this server reads another event: the lunch it held
                // off for Bo, and a lunch he is invited to anew.
                assert_eq!(
                    server.mailed(),
                    ["REQUEST to bo@example.test"],
                    "her calendar server called the lunch off and invited to it again"
                );
                let text = held_text(&server);
                assert_eq!(uid_lines(&text), ["UID:lunch-4@example.org"], "{text}");
            }

            phase every_part_of_an_entry_that_repeats_keeps_the_uid_as_it_is_held;
            {
                let (server, account, work) = adas();
                // As a calendar program elsewhere stores it: an identifier in capitals with
                // nothing after it, on the event and on its one moved occurrence.
                let uid = "UID:6F1E2C3A-0B1D-4E5F-8A9B-0C1D2E3F4A5B";
                let stored = format!(
                    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Elsewhere//EN\r\n\
                     BEGIN:VEVENT\r\n{uid}\r\nSUMMARY:Standup\r\nDTSTART:20261116T090000Z\r\n\
                     DTEND:20261116T091500Z\r\nRRULE:FREQ=WEEKLY\r\n\
                     ORGANIZER;SCHEDULE-AGENT=CLIENT:mailto:{ADA}\r\n\
                     ATTENDEE;SCHEDULE-AGENT=CLIENT:mailto:bo@example.test\r\nEND:VEVENT\r\n\
                     BEGIN:VEVENT\r\n{uid}\r\nRECURRENCE-ID:20261123T090000Z\r\n\
                     SUMMARY:Standup\r\nDTSTART:20261123T100000Z\r\nDTEND:20261123T101500Z\r\n\
                     END:VEVENT\r\nEND:VCALENDAR\r\n"
                );
                server.put("work", "standup.ics", &stored);
                let mut job = as_pulled(&account, &work);
                assert_eq!(
                    job.event.occurrence_overrides.iter().count(),
                    1,
                    "its moved occurrence is read"
                );
                job.event.title = "Standup shortened".into();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "{:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                let text = held_text(&server);
                assert!(text.contains("Standup shortened"), "{text}");
                assert_eq!(uid_lines(&text), [uid, uid], "{text}");
                assert_eq!(server.mailed(), NOBODY);
            }
        }

        tilo_test_env::scenario! {
            /// The live client against the same server: whom an entry's server holds and an
            /// event here cannot hold stays as held when the event goes up. A line that is
            /// gone is somebody taken out, and the server tells them the event is off (RFC
            /// 6638, 3.2.1.2, column `<Removed>`).
            fn whom_an_event_here_cannot_hold_stays_as_its_server_holds_them;

            phase somebody_named_by_another_address_than_a_mail_address_stays;
            {
                let (server, account, work) = adas();
                // Another program stored it and left its lines to the server: Bo by his
                // mail address, Fay by an address that is none.
                let fay = "ATTENDEE;CN=Fay Ops:urn:uuid:fay-4";
                let stored = held_entry(
                    "review-5@example.org",
                    ADA,
                    &["bo@example.test"],
                    LEFT_TO_SERVER,
                )
                .replace("END:VEVENT\r\n", &format!("{fay}\r\nEND:VEVENT\r\n"));
                server.put("work", "review.ics", &stored);
                let mut job = as_pulled(&account, &work);
                assert_eq!(
                    job.event.attendees.iter().count(),
                    2,
                    "Ada and Bo are read, Fay is not"
                );
                job.event.title = "Review moved".into();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "{:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                // The table's row SERVER, column SERVER, for both. Its column <Removed>
                // has the server tell Fay that the review is off.
                assert_eq!(
                    server.mailed(),
                    ["REQUEST to bo@example.test", "REQUEST to urn:uuid:fay-4"],
                    "her calendar server called the event off for somebody this calendar \
                     cannot hold"
                );
                let text = held_text(&server);
                assert!(text.contains("Review moved"), "{text}");
                assert!(
                    naming_lines(&text).iter().any(|line| line == fay),
                    "{text}"
                );
            }

            phase somebody_named_on_one_occurrence_only_stays;
            {
                let (server, account, work) = adas();
                // Every week with Bo, and with Dan too on the one occurrence that was moved.
                let people =
                    format!("ORGANIZER:mailto:{ADA}\r\nATTENDEE:mailto:bo@example.test\r\n");
                let dan = "ATTENDEE;RSVP=TRUE:mailto:dan@example.test";
                let stored = format!(
                    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Elsewhere//EN\r\n\
                     BEGIN:VEVENT\r\nUID:standup-7@example.org\r\nSUMMARY:Standup\r\n\
                     DTSTART:20261116T090000Z\r\nDTEND:20261116T091500Z\r\nRRULE:FREQ=WEEKLY\r\n\
                     {people}END:VEVENT\r\n\
                     BEGIN:VEVENT\r\nUID:standup-7@example.org\r\n\
                     RECURRENCE-ID:20261123T090000Z\r\nSUMMARY:Standup\r\n\
                     DTSTART:20261123T100000Z\r\nDTEND:20261123T101500Z\r\n\
                     {people}{dan}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
                );
                server.put("work", "standup.ics", &stored);
                let mut job = as_pulled(&account, &work);
                job.event.title = "Standup shortened".into();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "{:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                assert_eq!(
                    server.mailed(),
                    ["REQUEST to bo@example.test", "REQUEST to dan@example.test"],
                    "her calendar server called the event off for somebody invited to one \
                     occurrence of it"
                );
                let text = held_text(&server);
                assert!(text.contains("Standup shortened"), "{text}");
                let moved = text.split("RECURRENCE-ID").nth(1).unwrap_or_default();
                assert!(
                    naming_lines(moved).iter().any(|line| line == dan),
                    "{text}"
                );
            }

            phase a_line_its_server_writes_for_nobody_stays_as_held_too;
            {
                let (server, account, work) = adas();
                // Nobody is written to for Fay, by the server or by anybody: taken out,
                // she is sent nothing, and she is still not this calendar's to take out.
                let fay = "ATTENDEE;SCHEDULE-AGENT=NONE;CN=Fay Ops:urn:uuid:fay-4";
                let stored = held_entry(
                    "review-6@example.org",
                    ADA,
                    &["bo@example.test"],
                    CLIENT,
                )
                .replace("END:VEVENT\r\n", &format!("{fay}\r\nEND:VEVENT\r\n"));
                server.put("work", "review.ics", &stored);
                let mut job = as_pulled(&account, &work);
                job.event.title = "Review moved".into();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "{:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                assert_eq!(server.mailed(), NOBODY);
                let text = held_text(&server);
                assert!(text.contains("Review moved"), "{text}");
                assert!(
                    naming_lines(&text).iter().any(|line| line == fay),
                    "{text}"
                );
            }

            phase an_occurrence_moved_here_names_whom_its_event_names;
            {
                let (server, account, work) = adas();
                // Every week with Bo, as another program stored it. Here one occurrence
                // of it is moved an hour.
                let stored = held_entry(
                    "weekly-8@example.org",
                    ADA,
                    &["bo@example.test"],
                    LEFT_TO_SERVER,
                )
                .replace("END:VEVENT\r\n", "RRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n");
                server.put("work", "weekly.ics", &stored);
                let mut job = as_pulled(&account, &work);
                let day = jiff::civil::date(2026, 11, 26);
                job.event
                    .occurrence_overrides
                    .push(crate::model::OccurrenceOverride {
                        original_date: day,
                        original_time: Some(jiff::civil::time(19, 0, 0, 0)),
                        cancelled: false,
                        start: Some(day.at(20, 0, 0, 0)),
                        end: Some(day.at(21, 0, 0, 0)),
                        end_later_fold: false,
                        title: None,
                        location: None,
                    });
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "{:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                // RFC 6638, 3.1: an entry is one its server schedules where each of its
                // parts says who organises it. An occurrence that names nobody ends that:
                // the server takes the entry for removed and tells Bo all of it is off.
                assert_eq!(
                    server.mailed(),
                    ["REQUEST to bo@example.test"],
                    "her calendar server called the whole event off when one occurrence of \
                     it was moved"
                );
                let text = held_text(&server);
                let moved = naming_lines(text.split("RECURRENCE-ID").nth(1).unwrap_or_default());
                for line in naming_lines(&stored) {
                    assert!(
                        moved.contains(&line),
                        "the moved occurrence does not say {line}: {text}"
                    );
                }
            }
        }

        /// An entry as a server holds it that names nobody.
        fn held_naming_nobody(uid: &str) -> String {
            format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Elsewhere//EN\r\n\
                 BEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:Notes\r\nDTSTART:20261119T190000Z\r\n\
                 DTEND:20261119T200000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            )
        }

        /// The upload `job` with nobody named in its event any more.
        fn naming_nobody(mut job: super::super::UploadJob) -> super::super::UploadJob {
            job.event.organizer = None;
            job.event.attendees = Vec::<crate::model::Attendee>::new().into();
            job
        }

        /// That `upload` wrote nothing and says so in words for its person.
        fn nothing_was_written(upload: &super::super::MutationResult) {
            assert!(upload.confirmed.is_empty(), "{:?}", upload.confirmed);
            assert!(upload.conflicts.is_empty(), "{:?}", upload.conflicts);
            assert_eq!(upload.failures.len(), 1, "{:?}", upload.failures);
            assert!(
                upload.failures[0].1.contains("nothing was written"),
                "{}",
                upload.failures[0].1
            );
        }

        tilo_test_env::scenario! {
            /// The live client against the same server: an entry is read before it is
            /// written over, also where the event names nobody here. What its server holds
            /// may name people, and written over without them the entry is one the server
            /// no longer schedules, which it takes for the entry's removal (RFC 6638,
            /// 3.2.3.1).
            fn an_entry_that_names_nobody_here_is_read_before_it_is_written;

            phase an_event_she_organises_is_not_called_off_by_an_upload_that_names_nobody;
            {
                let (server, account, work) = adas();
                // Under the UID this calendar writes too, so that nothing but the people
                // differs: another client left its lines to the server, which invited both.
                let stored = held_entry(
                    "5b0d6f6e-2f0c-4d57-9c1e-0d3a51c6a001@tilo",
                    ADA,
                    &["bo@example.test", "eve@elsewhere.test"],
                    LEFT_TO_SERVER,
                );
                server.put("work", "lunch.ics", &stored);
                let mut job = naming_nobody(as_pulled(&account, &work));
                job.event.title = "Lunch moved".into();
                let _ = server.requests();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                // RFC 6638, 3.2.3.1 and 3.2.1.3: an entry that scheduled and no longer does
                // was removed, and everybody the server wrote to is told the event is off.
                assert_eq!(
                    server.mailed(),
                    NOBODY,
                    "her calendar server called the event off for everybody it invites"
                );
                nothing_was_written(&upload);
                assert_eq!(
                    methods(&server.requests()),
                    ["GET"],
                    "a read that forbids the write is followed by none"
                );
                assert_eq!(held_text(&server), stored);
            }

            phase an_invitation_is_not_declined_by_an_upload_that_names_nobody;
            {
                let (server, account, work) = adas();
                // Eve invited her, and the server answers Eve for her.
                let stored = held_entry(
                    "5b0d6f6e-2f0c-4d57-9c1e-0d3a51c6a002@tilo",
                    "eve@elsewhere.test",
                    &[ADA, "cy@example.test"],
                    LEFT_TO_SERVER,
                );
                server.put("work", "offsite.ics", &stored);
                let mut job = naming_nobody(as_pulled(&account, &work));
                job.event.title = "Offsite moved".into();
                let _ = server.requests();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                // RFC 6638, 3.2.3.1 and 3.2.2.4: an invitation that is none any more was
                // removed, and its organiser is told that she declines.
                assert_eq!(
                    server.mailed(),
                    NOBODY,
                    "her calendar server declined an invitation for her"
                );
                nothing_was_written(&upload);
                assert_eq!(methods(&server.requests()), ["GET"]);
                assert_eq!(held_text(&server), stored);
            }

            phase an_entry_that_names_nobody_anywhere_is_written_over_as_the_version_read;
            {
                let (server, account, work) = adas();
                server.put("work", "notes.ics", &held_naming_nobody("notes-2@example.org"));
                let mut job = as_pulled(&account, &work);
                job.event.title = "Notes moved".into();
                let _ = server.requests();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "her change to an entry made elsewhere did not go up: {:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                let requests = server.requests();
                assert_eq!(methods(&requests), ["GET", "PUT"]);
                assert_eq!(requests[1].header("If-Match"), job.etag.as_deref());
                let text = held_text(&server);
                assert!(text.contains("Notes moved"), "{text}");
                assert_eq!(uid_lines(&text), ["UID:notes-2@example.org"], "{text}");
                assert_eq!(server.mailed(), NOBODY);

                // Where this calendar kept no version of the entry, it is written over as
                // the version just read, not as whatever stands there.
                let mut unversioned = as_pulled(&account, &work);
                let version = unversioned.etag.take();
                assert!(version.is_some());
                unversioned.event.title = "Notes moved again".into();
                let _ = server.requests();
                let upload = push_jobs_live(std::slice::from_ref(&unversioned));
                assert_eq!(
                    upload.confirmed,
                    vec![unversioned.event.id],
                    "{:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                let requests = server.requests();
                assert_eq!(methods(&requests), ["GET", "PUT"]);
                assert_eq!(requests[1].header("If-Match"), version.as_deref());
                assert!(held_text(&server).contains("Notes moved again"));
            }

            phase an_entry_that_changed_on_its_server_since_is_not_written;
            {
                let (server, account, work) = adas();
                server.put("work", "notes.ics", &held_naming_nobody("notes-3@example.org"));
                let mut job = as_pulled(&account, &work);
                job.event.title = "Notes changed here".into();
                // Meanwhile another client invites Bo to it and leaves that to the server.
                let theirs = held_entry(
                    "notes-3@example.org",
                    ADA,
                    &["bo@example.test"],
                    LEFT_TO_SERVER,
                );
                server.put("work", "notes.ics", &theirs);
                let _ = server.requests();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert!(upload.confirmed.is_empty());
                assert_eq!(upload.conflicts.len(), 1, "{:?}", upload.failures);
                assert_eq!(
                    methods(&server.requests()),
                    ["GET"],
                    "a read that is no longer true is followed by no write"
                );
                assert_eq!(held_text(&server), theirs);
                assert_eq!(server.mailed(), NOBODY);
            }

            phase people_only_its_server_names_stay_where_the_event_names_nobody_here;
            {
                let (server, account, work) = adas();
                // Both lines are written as this calendar does not read them: who organises
                // it with another spelling of the scheme, Fay by an address that is no mail
                // address. So the event names nobody here.
                let people = "ORGANIZER:Mailto:ada@example.test\r\nATTENDEE:urn:uuid:fay-4\r\n";
                let stored = held_naming_nobody("5b0d6f6e-2f0c-4d57-9c1e-0d3a51c6a003@tilo")
                    .replace("END:VEVENT\r\n", &format!("{people}END:VEVENT\r\n"));
                server.put("work", "review.ics", &stored);
                let mut job = as_pulled(&account, &work);
                assert_eq!(job.event.attendees.iter().count(), 0, "nobody is read");
                job.event.title = "Review moved".into();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "{:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                // The table's row SERVER, column SERVER. Written over without its lines,
                // the entry was removed: Fay is told the review is off.
                assert_eq!(
                    server.mailed(),
                    ["REQUEST to urn:uuid:fay-4"],
                    "her calendar server called the event off for somebody this calendar \
                     cannot hold"
                );
                let text = held_text(&server);
                assert!(text.contains("Review moved"), "{text}");
                assert_eq!(naming_lines(&text), naming_lines(&stored), "{text}");
            }

            phase an_entry_its_server_writes_to_nobody_for_may_lose_its_people;
            {
                let (server, account, work) = adas();
                // As her own server keeps what this calendar put in: every line says that
                // the client writes. Nobody is written to when its people go.
                let stored = held_entry(
                    "5b0d6f6e-2f0c-4d57-9c1e-0d3a51c6a004@tilo",
                    ADA,
                    &["bo@example.test"],
                    CLIENT,
                );
                server.put("work", "dinner.ics", &stored);
                let mut job = naming_nobody(as_pulled(&account, &work));
                job.event.title = "Dinner alone".into();
                let _ = server.requests();
                let upload = push_jobs_live(std::slice::from_ref(&job));
                assert_eq!(
                    upload.confirmed,
                    vec![job.event.id],
                    "{:?} {:?}",
                    upload.conflicts,
                    upload.failures
                );
                assert_eq!(methods(&server.requests()), ["GET", "PUT"]);
                let text = held_text(&server);
                assert!(text.contains("Dinner alone"), "{text}");
                assert_eq!(naming_lines(&text), Vec::<String>::new(), "{text}");
                assert_eq!(server.mailed(), NOBODY);
            }
        }

        /// Her change to an entry goes up to a server that `writes` its versions another
        /// way in a listing than in a read, over a strong tag, and what changed on that
        /// server since is still not written over. `listed` knows a listing's version.
        fn her_change_goes_up_and_a_change_there_still_stops_one(
            writes: fn(&crate::fake_caldav::Server),
            listed: fn(&str) -> bool,
        ) {
            let (server, account, work) = adas();
            writes(&server);
            server.put(
                "work",
                "notes.ics",
                &held_naming_nobody("notes-5@example.org"),
            );
            let mut job = as_pulled(&account, &work);
            assert!(
                job.etag.as_deref().is_some_and(listed),
                "the listing wrote its version as {:?}",
                job.etag
            );
            job.event.title = "Notes moved".into();
            let _ = server.requests();
            let upload = push_jobs_live(std::slice::from_ref(&job));
            assert_eq!(
                upload.confirmed,
                vec![job.event.id],
                "her change did not go up: {:?} {:?}",
                upload.conflicts,
                upload.failures
            );
            let requests = server.requests();
            assert_eq!(methods(&requests), ["GET", "PUT"]);
            let over = requests[1].header("If-Match");
            assert!(
                over.is_some_and(|over| over.starts_with('"') && over.ends_with('"')),
                "it was written over {over:?}"
            );
            assert!(held_text(&server).contains("Notes moved"));

            // Meanwhile another client invites Bo to it and leaves that to the server: a
            // version that is another one is still another one.
            let mut job = as_pulled(&account, &work);
            job.event.title = "Notes changed here".into();
            let theirs = held_entry(
                "notes-5@example.org",
                ADA,
                &["bo@example.test"],
                LEFT_TO_SERVER,
            );
            server.put("work", "notes.ics", &theirs);
            let _ = server.requests();
            let upload = push_jobs_live(std::slice::from_ref(&job));
            assert!(upload.confirmed.is_empty());
            assert_eq!(upload.conflicts.len(), 1, "{:?}", upload.failures);
            assert_eq!(
                methods(&server.requests()),
                ["GET"],
                "a read that is no longer true is followed by no write"
            );
            assert_eq!(held_text(&server), theirs);
            assert_eq!(server.mailed(), NOBODY);
        }

        /// Taking an entry out of a server that `writes` its versions another way goes
        /// over a strong tag too: `asked` is what the server is asked for it.
        fn taking_it_out_goes_over_a_strong_tag(
            writes: fn(&crate::fake_caldav::Server),
            asked: &[&str],
        ) {
            let (server, account, work) = adas();
            writes(&server);
            server.put(
                "work",
                "notes.ics",
                &held_naming_nobody("notes-6@example.org"),
            );
            let pulled = as_pulled(&account, &work);
            let _ = server.requests();
            let removed = remove_jobs_live(&[super::super::RemoveJob {
                config: pulled.config.clone(),
                calendar: pulled.calendar.clone(),
                id: pulled.event.id,
                href: pulled.href.clone(),
                etag: pulled.etag.clone(),
            }]);
            assert_eq!(
                removed.confirmed,
                vec![pulled.event.id],
                "it was not taken out: {:?} {:?}",
                removed.conflicts,
                removed.failures
            );
            let requests = server.requests();
            assert_eq!(methods(&requests), asked);
            let over = requests.last().and_then(|last| last.header("If-Match"));
            assert!(
                over.is_some_and(|over| over.starts_with('"') && over.ends_with('"')),
                "it was taken out over {over:?}"
            );
            assert!(server.held("work").is_empty());
            assert_eq!(server.mailed(), NOBODY);
        }

        tilo_test_env::scenario! {
            /// The live client against a server that writes an entry's version one way in
            /// a listing and another in a read. Two entity-tags name one version where
            /// their opaque tags match, whether or not either is marked weak (RFC 7232,
            /// 2.3.2), and a write names the version it goes over by a strong one (3.1).
            /// So such an entry is written and taken out, and one that changed on its
            /// server since is not.
            fn a_version_written_another_way_in_a_listing_than_in_a_read_is_one_version;

            phase a_listing_that_marks_its_versions_weak;
            {
                her_change_goes_up_and_a_change_there_still_stops_one(
                    crate::fake_caldav::Server::lists_versions_weak,
                    |listed| listed.starts_with("W/\""),
                );
                taking_it_out_goes_over_a_strong_tag(
                    crate::fake_caldav::Server::lists_versions_weak,
                    &["GET", "DELETE"],
                );
            }

            phase a_listing_that_leaves_the_quotes_out;
            {
                her_change_goes_up_and_a_change_there_still_stops_one(
                    crate::fake_caldav::Server::lists_versions_bare,
                    |listed| !listed.contains('"'),
                );
                taking_it_out_goes_over_a_strong_tag(
                    crate::fake_caldav::Server::lists_versions_bare,
                    &["GET", "DELETE"],
                );
            }

            phase a_read_that_marks_its_versions_weak;
            {
                her_change_goes_up_and_a_change_there_still_stops_one(
                    crate::fake_caldav::Server::reads_versions_weak,
                    |listed| listed.starts_with('"'),
                );
                taking_it_out_goes_over_a_strong_tag(
                    crate::fake_caldav::Server::reads_versions_weak,
                    &["DELETE"],
                );
            }
        }

        /// RFC 7232, 2.3.2 and 3.1: how two versions are compared, and which a write
        /// names.
        #[test]
        fn two_entity_tags_name_one_version_where_their_opaque_tags_match() {
            use super::{is_strong, same_version};
            assert!(same_version("\"1\"", "\"1\""));
            assert!(same_version("W/\"1\"", "\"1\""));
            assert!(same_version("\"1\"", "W/\"1\""));
            assert!(same_version("W/\"1\"", "W/\"1\""));
            assert!(!same_version("\"1\"", "\"2\""));
            assert!(!same_version("W/\"1\"", "W/\"2\""));
            // A listing that leaves the quotes out means the tag between them.
            assert!(same_version("1", "\"1\""));
            assert!(!same_version("1", "\"2\""));
            // The mark of a weak tag is written in capitals and in no other way.
            assert!(!same_version("w/\"1\"", "\"1\""));
            assert!(is_strong("\"1\""));
            assert!(!is_strong("W/\"1\""));
            assert!(!is_strong("1"));
            assert!(!is_strong("\""));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::CalendarAccounts;

    fn run<T>(future: impl Future<Output = T>) -> T {
        ui::rt::block_on(future).expect("a test thread may block")
    }

    fn sync_all(
        accounts: &[(CalDavConfig, Vec<RemoteCalendar>)],
        today: Date,
    ) -> (SyncResult, SyncOutcome) {
        run(super::sync_all_with_checkpoints(accounts, today, &[]))
    }

    fn today() -> Date {
        Date::new(2026, 6, 15).unwrap()
    }

    tilo_test_env::scenario! {
        /// The CalDAV layer without a server: what it pulls, what it would write and what it
        /// parses.
        fn caldav_pulls_writes_and_parses_by_resource;

        phase pull_and_sync_tag_events_skip_hidden_and_report_offline;
        {
            {
                let store = CalendarAccounts::sample();
                let (account, cal) = store
                    .all_calendars()
                    .find(|(_, c)| c.id == "work/oncall")
                    .unwrap();
                let res = pull_calendar(&account.config, cal, today());
                assert!(!res.events.is_empty());
                assert!(res.events.iter().all(|e| e.color == cal.palette()));
            }
            {
                let store = CalendarAccounts::sample();
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
            {
                let store = CalendarAccounts::sample();
                let accounts: Vec<_> = store
                    .accounts
                    .iter()
                    .map(|a| (a.config.clone(), a.calendars.clone()))
                    .collect();
                let (res, outcome) = sample_pull_all(&accounts, today());
                assert!(matches!(outcome, SyncOutcome::Synced { calendars: 3, .. }));
                assert!(!res.is_empty());
                assert!(!res.events.iter().any(|e| e.title == "Public holiday"));
            }
            {
                let (res, outcome) = sample_pull_all(&[], today());
                assert!(res.is_empty());
                assert_eq!(outcome, SyncOutcome::Offline);
            }
            {
                let (res, outcome) = sync_all(&[], today());
                assert!(res.is_empty());
                assert_eq!(outcome, SyncOutcome::Offline);
            }
            {
                let store = CalendarAccounts::sample();
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

        phase write_requests_target_one_resource_and_stay_inert_without_live;
        {
            {
                use crate::model::Event;
                use jiff::civil::Date;
                let store = CalendarAccounts::sample();
                let (account, cal) = store
                    .all_calendars()
                    .find(|(_, c)| c.id == "work/default")
                    .unwrap();
                let start = Date::new(2026, 6, 15).unwrap()
                    .at(9, 0, 0, 0);
                let ev = Event {
                    id: uuid::Uuid::new_v4(),
                    title: "Kickoff".into(),
                    start,
                    end: start + jiff::SignedDuration::from_hours(1),
                    location: "Room 1".into(),
                    calendar_id: Some(cal.id.clone()),
                    ..Event::default()
                };
                let req = build_put_request(&account.config, cal, &ev);
                assert!(
                    req.url.starts_with(&account.config.server_url),
                    "url = {}",
                    req.url
                );
                assert!(req.url.ends_with(&format!("{}.ics", ev.id)));
                assert!(req.body.contains("BEGIN:VEVENT"));
                assert!(req.body.contains("Kickoff"));
            }
            {
                let store = CalendarAccounts::sample();
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
            {
                let store = CalendarAccounts::sample();
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
            {
                let store = CalendarAccounts::sample();
                let (account, cal) = store.all_calendars().next().unwrap();
                let start = jiff::civil::Date::new(2026, 6, 15).unwrap()
                    .at(9, 0, 0, 0);
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
            {
                let store = CalendarAccounts::sample();
                let (account, cal) = store.all_calendars().next().unwrap();
                let event = pull_calendar(&account.config, cal, today())
                    .events
                    .remove(0);
                let request = build_put_request(&account.config, cal, &event);
                assert!(request.if_none_match);
                assert!(request.if_match.is_none());
            }
        }

        phase multistatus_parses_identity_and_rejects_malformed_input;
        {
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
            {
                assert!(parse_multistatus("<D:multistatus>").is_err());
                let oversized = "x".repeat(MAX_XML_BYTES + 1);
                assert!(parse_multistatus(&oversized).is_err());
            }
            {
                let body = sync_collection_body("a<&\"'>");
                assert!(body.contains("a&lt;&amp;&quot;&apos;&gt;"));
                assert!(!body.contains("a<&"));
            }
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
}
