//! The workspace's activity as a resource an app reads and changes: the shell's model, served to
//! every app of the session as `so.tilo.history`.
//!
//! The model serves its reads to whoever follows them (`ActivityRequests`). This service follows
//! the ones an app shows — the feed of this workspace's changes and of every workspace's, what is
//! new since the person last looked, the replay of the layout, every agent session, and what the
//! person was asked to open from outside — and publishes what they last read as one reading. A
//! change of it wakes the views that read it, as any published resource's reading does.
//!
//! Its writes are the model's own actions: naming, splitting and merging work groups, moving a
//! boundary, undoing and restoring, undoing an agent's session or one record of it, opening what a
//! change touched, and what the person saw and hears about each morning. Each runs on the model's
//! thread through [`ActivityRequests::act`], so a write and a read of the same change never cross.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};
use tilo_actions::{ActionInvoker, ApiError, ResourceSpec};
use tilo_service::{Service, Started, offer};
use tilo_session_core::desktop::activity::vocabulary as wire;
use tilo_session_core::desktop::session::ActivityRequests;
use wire::{
    Activity, Detail, Feed, FeedReading, Focus, OutputReading, RangeDetail, Replay, SessionOutput,
    Sessions, Summary,
};

/// The resource every app reads the activity as, and every action is an action of: the id the
/// model publishes it under.
pub(crate) const RESOURCE: &str = wire::RESOURCE;

// The keys of the readings a view names are one path step after the root, their parts separated
// by `~`: a view's reference is the document and the last step of the id it names, so a
// `workspace.activity/range/<from>-<to>` would reach a service as `workspace.activity/<to>` and
// name no range at all. One step keeps every part of the key the session reads.

/// What a reading keyed by a range is published under: the range's first and last change, after
/// this: `workspace.activity/range~<from>~<to>`. A view that shows one change beside another
/// reads its own, and no two windows share a detail.
pub(crate) const RANGE_PREFIX: &str = "workspace.activity/range~";

/// What the workspace before and after a range of changes is published under, after this: the
/// range's first and last change, as [`RANGE_PREFIX`]. A window that previews one change reads
/// its own comparison.
pub(crate) const COMPARE_PREFIX: &str = "workspace.activity/compare~";

/// What a feed narrowed by the person's filters is published under: this workspace's changes or
/// every workspace's, how far back, the lens (a dash for none) and how many of the newest
/// changes it holds, after this: `workspace.activity/feed~<scope>~<period>~<lens>~<limit>`. A
/// window reads the page it shows, so two windows showing two periods each read their own.
pub(crate) const FEED_PREFIX: &str = "workspace.activity/feed~";

/// What every agent session across the person's workspaces is published under, after this:
/// `workspace.activity/sessions~<scope>~<changes>`: this workspace's sessions or every
/// workspace's, and how many of each session's changes still in effect the page holds. A window
/// that lists a session's changes reads the page it shows, so two windows each read their own.
pub(crate) const SESSIONS_PREFIX: &str = "workspace.activity/sessions~";

/// What a command's whole output is published under, after this: the workspace its shell's
/// record is in (a dash for this one), the app and record that keep the shell session, and the
/// execution: `workspace.activity/output/-/<app>/<record>/<execution>`. A window that shows one
/// command's output reads its own, while no other is read.
pub(crate) const OUTPUT_PREFIX: &str = "workspace.activity/output/";

/// The reads the model serves, in the order the service follows them: the action, and the
/// arguments it is read with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Read {
    Feed,
    Everywhere,
    Summary,
    Replay,
    Sessions,
    Focus,
}

impl Read {
    const ALL: [Self; 6] = [
        Self::Feed,
        Self::Everywhere,
        Self::Summary,
        Self::Replay,
        Self::Sessions,
        Self::Focus,
    ];

    /// The read's action of [`RESOURCE`].
    const fn action(self) -> &'static str {
        match self {
            Self::Feed | Self::Everywhere => wire::FEED,
            Self::Summary => wire::SUMMARY,
            Self::Replay => wire::REPLAY,
            Self::Sessions => wire::SESSIONS,
            Self::Focus => wire::FOCUS,
        }
    }

    /// What the read is asked of the model: the first page of each, and the feed of every
    /// workspace on this device beside the one the person is in.
    fn arguments(self) -> Map<String, Value> {
        let query = match self {
            Self::Everywhere => wire::FeedQuery {
                scope: wire::Scope::AllWorkspaces,
                ..wire::FeedQuery::default()
            },
            Self::Sessions => {
                return read_arguments(&wire::SessionsQuery {
                    scope: wire::Scope::ThisWorkspace,
                    limit: 0,
                    changes: 0,
                });
            }
            _ => return Map::new(),
        };
        read_arguments(&query)
    }
}

/// `value` as a read is asked with it: the object its own type writes.
fn read_arguments(value: &impl Serialize) -> Map<String, Value> {
    match serde_json::to_value(value) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// The model's last read of one action.
struct Followed {
    invocation: tilo_actions::InvocationId,
    /// The reading the model last served, as a value of the type it is read into.
    last: Option<Value>,
}

/// The activity, published as a resource of the session.
pub(crate) struct Served {
    requests: ActivityRequests,
    /// The reads followed, once the service first polled.
    followed: Option<BTreeMap<Read, Followed>>,
    /// What the reads last built, and the reading an app gets of it.
    reading: Activity,
    json: Value,
    /// The readings views asked for by a key, each followed while one shows it.
    keyed: std::cell::RefCell<Keyed>,
    /// The resources whose readings moved since the last [`Served::moved`].
    moved: Vec<String>,
}

/// The readings a view asked for by a key — a range of changes, a filtered feed — each
/// followed while a view of that window names it, and dropped with the window.
#[derive(Default)]
struct Keyed {
    /// The reads followed, by resource id.
    followed: BTreeMap<String, Followed>,
    /// The resources views asked for that are not followed yet.
    asked: Vec<String>,
    /// What each reads as, by resource id.
    json: BTreeMap<String, Value>,
}

/// What a keyed reading is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Range,
    Compare,
    Feed,
    Sessions,
    Output,
}

impl Key {
    /// The keyed reading `resource` names.
    fn of(resource: &str) -> Option<Self> {
        if resource.starts_with(RANGE_PREFIX) {
            Some(Self::Range)
        } else if resource.starts_with(COMPARE_PREFIX) {
            Some(Self::Compare)
        } else if resource.starts_with(FEED_PREFIX) {
            Some(Self::Feed)
        } else if resource.starts_with(SESSIONS_PREFIX) {
            Some(Self::Sessions)
        } else if resource.starts_with(OUTPUT_PREFIX) {
            Some(Self::Output)
        } else {
            None
        }
    }

    /// The model's read that answers it, and with what.
    fn read(self, resource: &str) -> Option<(&'static str, Map<String, Value>)> {
        match self {
            Self::Range => {
                let range = range_of(resource)?;
                Some((
                    wire::DETAIL,
                    read_arguments(&wire::DetailQuery {
                        range,
                        compare: false,
                    }),
                ))
            }
            Self::Compare => {
                let range = compare_range_of(resource)?;
                Some((
                    wire::DETAIL,
                    read_arguments(&wire::DetailQuery {
                        range,
                        compare: true,
                    }),
                ))
            }
            Self::Feed => {
                let query = feed_query_of(resource)?;
                Some((wire::FEED, read_arguments(&query)))
            }
            Self::Sessions => {
                let rest = resource.strip_prefix(SESSIONS_PREFIX)?;
                let mut parts = rest.split('~');
                let scope = match parts.next()? {
                    "all" => wire::Scope::AllWorkspaces,
                    "this" => wire::Scope::ThisWorkspace,
                    _ => return None,
                };
                let changes = match parts.next() {
                    Some(page) => page.parse().ok()?,
                    None => 0,
                };
                if parts.next().is_some() {
                    return None;
                }
                Some((
                    wire::SESSIONS,
                    read_arguments(&wire::SessionsQuery {
                        scope,
                        limit: 0,
                        changes,
                    }),
                ))
            }
            Self::Output => Some((wire::SESSION_OUTPUT, output_of(resource)?)),
        }
    }

    /// The reading an app gets of the model's last read of it.
    fn reading(self, value: &Value) -> Value {
        let empty = || self.empty();
        match self {
            Self::Range | Self::Compare => reading_of::<Detail, _>(
                value,
                |detail| RangeDetail {
                    detail: Some(detail),
                },
                empty,
            ),
            Self::Feed => {
                reading_of::<Feed, _>(value, |feed| FeedReading { feed: Some(feed) }, empty)
            }
            Self::Sessions => reading_of::<Sessions, _>(value, |sessions| sessions, empty),
            Self::Output => reading_of::<SessionOutput, _>(
                value,
                |output| OutputReading {
                    output: Some(output),
                },
                empty,
            ),
        }
    }

    /// What it reads as before the model has answered.
    fn empty(self) -> Value {
        match self {
            Self::Range | Self::Compare => json_or_null(&RangeDetail::default()),
            Self::Feed => json_or_null(&FeedReading::default()),
            Self::Sessions => json_or_null(&Sessions {
                pending: true,
                ..Sessions::default()
            }),
            Self::Output => json_or_null(&OutputReading::default()),
        }
    }
}

/// The reading an app gets of the model's answer `value`: `T` read out of it and wrapped as the
/// record a view names it by, or what the reading holds before the model answers when the answer
/// is not the shape an app reads.
fn reading_of<T: serde::de::DeserializeOwned, R: Serialize>(
    value: &Value,
    wrap: impl FnOnce(T) -> R,
    empty: impl Fn() -> Value,
) -> Value {
    match serde_json::from_value::<T>(value.clone()) {
        Ok(read) => json_or_null(&wrap(read)),
        Err(error) => {
            tracing::warn!(%error, "a read of the activity is not the shape an app reads");
            empty()
        }
    }
}

/// `value` as JSON: nothing at all when it does not write, which no record here fails to.
fn json_or_null(value: &impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

impl Served {
    /// The service over the model `requests` serves.
    pub(crate) fn new(requests: ActivityRequests) -> Self {
        Self {
            requests,
            followed: None,
            reading: Activity::default(),
            json: empty(),
            keyed: std::cell::RefCell::new(Keyed::default()),
            moved: Vec::new(),
        }
    }

    /// Follow the reads an app shows. The session reads as the activity app itself: what a read
    /// says is what the person using it sees, and reading it marks nothing as seen.
    fn start(&mut self) {
        if self.followed.is_some() {
            return;
        }
        let caller = tilo_actions::remote::Caller::local_app(wire::APP_ID);
        let mut followed = BTreeMap::new();
        for read in Read::ALL {
            let invocation = tilo_actions::InvocationId::new();
            if self
                .requests
                .follow(&caller, invocation, read.action(), read.arguments())
                .is_err()
            {
                continue;
            }
            followed.insert(
                read,
                Followed {
                    invocation,
                    last: None,
                },
            );
        }
        self.followed = Some(followed);
    }

    /// Take in what the model read since the last call: each read's newest value, and the reading
    /// an app gets of them all. Whether the reading changed.
    fn take(&mut self) -> bool {
        self.start();
        let mut changed = false;
        if let Some(followed) = self.followed.as_mut() {
            for entry in followed.values_mut() {
                let Some(Ok(value)) = self.requests.served(&entry.invocation) else {
                    continue;
                };
                if entry.last.as_ref() != Some(&value) {
                    entry.last = Some(value);
                    changed = true;
                }
            }
        }
        if !changed {
            // Nothing of the whole moved; a keyed reading a view asked for since still may.
            return self.take_keyed();
        }
        self.build();
        self.moved.push(RESOURCE.to_owned());
        let _ = self.take_keyed();
        true
    }

    /// Follow the keyed readings views asked for since the last call, and take in what the model
    /// read of them. Whether one changed.
    fn take_keyed(&mut self) -> bool {
        let mut keyed = self.keyed.borrow_mut();
        let caller = tilo_actions::remote::Caller::local_app(wire::APP_ID);
        for id in std::mem::take(&mut keyed.asked) {
            let Some(key) = Key::of(&id) else {
                continue;
            };
            let Some((action, arguments)) = key.read(&id) else {
                continue;
            };
            let invocation = tilo_actions::InvocationId::new();
            if self
                .requests
                .follow(&caller, invocation, action, arguments)
                .is_err()
            {
                continue;
            }
            keyed.followed.insert(
                id,
                Followed {
                    invocation,
                    last: None,
                },
            );
        }
        let mut changed: Vec<(String, Value)> = Vec::new();
        for (id, entry) in &mut keyed.followed {
            let Some(Ok(value)) = self.requests.served(&entry.invocation) else {
                continue;
            };
            if entry.last.as_ref() == Some(&value) {
                continue;
            }
            entry.last = Some(value.clone());
            let reading = Key::of(id).map_or_else(|| Value::Null, |key| key.reading(&value));
            changed.push((id.clone(), reading));
        }
        for (id, json) in &changed {
            keyed.json.insert(id.clone(), json.clone());
        }
        let changed: Vec<String> = changed.into_iter().map(|(id, _)| id).collect();
        drop(keyed);
        self.moved.extend(changed.iter().cloned());
        !changed.is_empty()
    }

    /// The model's last read of `read`, as the type an app reads it into.
    fn last<T: serde::de::DeserializeOwned>(&self, read: Read) -> Option<T> {
        let value = self.followed.as_ref()?.get(&read)?.last.clone()?;
        match serde_json::from_value(value) {
            Ok(typed) => Some(typed),
            Err(error) => {
                tracing::warn!(?read, %error, "a read of the activity is not the shape an app reads");
                None
            }
        }
    }

    /// Build the reading an app gets from the reads' last values.
    fn build(&mut self) {
        let feed: Feed = self.last(Read::Feed).unwrap_or_default();
        let everywhere: Feed = self.last(Read::Everywhere).unwrap_or_default();
        let summary: Summary = self.last(Read::Summary).unwrap_or_default();
        let replay: Replay = self.last(Read::Replay).unwrap_or_default();
        let sessions: Sessions = self.last(Read::Sessions).unwrap_or_default();
        let focus: Option<Focus> = self.last(Read::Focus);
        self.reading = Activity {
            feed: Some(feed),
            everywhere: Some(everywhere),
            summary: Some(summary),
            replay: Some(replay),
            sessions: Some(sessions),
            focus,
        };
        self.json = json_or_null(&self.reading);
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<wire::ActivityAction>(
            tilo_resource::typed_resource::<wire::ActivityRoot>(RESOURCE, ""),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        if resource == RESOURCE {
            return Some(self.json.clone());
        }
        let key = Key::of(resource)?;
        let mut keyed = self.keyed.borrow_mut();
        if let Some(json) = keyed.json.get(resource) {
            return Some(json.clone());
        }
        // The model has not read this one yet: ask for it now, and start from what it reads as
        // with nothing in it, so the view that named it draws before the model answers.
        if !keyed.followed.contains_key(resource) && !keyed.asked.iter().any(|id| id == resource) {
            keyed.asked.push(resource.to_owned());
        }
        Some(key.empty())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            )));
        }
        let ran = self.requests.act(action, arguments.clone());
        Ok(Started::Later(ran.map(|ran| {
            ran.map(|()| tilo_actions::ActionOutput::default())
                .map_err(ApiError::Failed)
        })))
    }

    fn invoke_as(
        &mut self,
        invoker: &ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        // What this device has seen and the sources it hears about are the person's own: software
        // asking to read or change them is refused, as the model refuses it.
        if !invoker.acting.is_person() && is_personal(action) {
            return Err(ApiError::Refused {
                code: "activity_served_personal_only".to_owned(),
                reason: "Only the person at this device reads or changes what they saw.".to_owned(),
            });
        }
        self.invoke(resource, action, arguments)
    }

    fn poll(&mut self) -> bool {
        self.take()
    }

    fn moved(&mut self) -> Option<Vec<String>> {
        (!self.moved.is_empty()).then(|| std::mem::take(&mut self.moved))
    }
}

/// Whether `action` changes only what the person at this device saw and hears about.
fn is_personal(action: &str) -> bool {
    tilo_session_core::desktop::activity::is_personal(action)
}

/// A reading of the activity with nothing in it, as an app starts: the resource reads this until
/// the model serves its first pass.
#[must_use]
pub(crate) fn empty() -> Value {
    json_or_null(&Activity::default())
}

/// The query a feed reading names: this workspace's changes or every workspace's, how far back,
/// the lens (a dash for none) and how many of the newest changes it holds (`0` for the first
/// page).
pub(crate) fn feed_query_of(resource: &str) -> Option<wire::FeedQuery> {
    let rest = resource.strip_prefix(FEED_PREFIX)?;
    let mut parts = rest.splitn(4, '~');
    let scope = match parts.next()? {
        "all" => wire::Scope::AllWorkspaces,
        "this" => wire::Scope::ThisWorkspace,
        _ => return None,
    };
    let period = match parts.next()? {
        "today" => wire::Period::Today,
        "week" => wire::Period::Week,
        "month" => wire::Period::Month,
        "all-time" => wire::Period::AllTime,
        _ => return None,
    };
    let lens = parts.next()?;
    let limit = match parts.next() {
        Some(limit) => limit.parse().ok()?,
        None => 0,
    };
    Some(wire::FeedQuery {
        scope,
        period,
        lens: (lens != "-").then(|| lens.to_owned()),
        limit,
        ..wire::FeedQuery::default()
    })
}

/// The resource a page of the feed is published as.
#[cfg(test)]
pub(crate) fn feed_resource(scope: &str, period: &str, lens: &str, limit: usize) -> String {
    let lens = if lens.is_empty() { "-" } else { lens };
    format!("{FEED_PREFIX}{scope}~{period}~{lens}~{limit}")
}

/// What a command's whole output is read with: the workspace its shell's record is in (a dash
/// for this one), the app and record that keep the shell session, and the execution.
fn output_of(resource: &str) -> Option<Map<String, Value>> {
    let rest = resource.strip_prefix(OUTPUT_PREFIX)?;
    let mut parts = rest.splitn(4, '/');
    let workspace = match parts.next()? {
        "-" => None,
        workspace => Some(workspace.to_owned()),
    };
    let app_id = parts.next()?.to_owned();
    let record = parts.next()?.to_owned();
    let execution = parts.next()?.to_owned();
    if app_id.is_empty() || record.is_empty() || execution.is_empty() {
        return None;
    }
    Some(read_arguments(&wire::CommandExecution {
        workspace,
        app_id,
        record,
        execution,
    }))
}

/// The resource a command's whole output is published as.
#[cfg(test)]
pub(crate) fn output_resource(
    workspace: Option<&str>,
    app_id: &str,
    record: &str,
    execution: &str,
) -> String {
    format!(
        "{OUTPUT_PREFIX}{}/{app_id}/{record}/{execution}",
        workspace.unwrap_or("-")
    )
}

pub(crate) fn range_of(resource: &str) -> Option<wire::Range> {
    let rest = resource.strip_prefix(RANGE_PREFIX)?;
    let (from, to) = rest.split_once('~')?;
    Some(wire::Range {
        from: from.to_owned(),
        to: to.to_owned(),
    })
}

/// The range a comparison reading names, after [`COMPARE_PREFIX`].
pub(crate) fn compare_range_of(resource: &str) -> Option<wire::Range> {
    let rest = resource.strip_prefix(COMPARE_PREFIX)?;
    let (from, to) = rest.split_once('~')?;
    Some(wire::Range {
        from: from.to_owned(),
        to: to.to_owned(),
    })
}

/// The resource a range's comparison is published as.
#[cfg(test)]
pub(crate) fn compare_resource(range: &wire::Range) -> String {
    format!("{COMPARE_PREFIX}{}~{}", range.from, range.to)
}

/// The resource a range's detail is published as.
#[cfg(test)]
pub(crate) fn range_resource(range: &wire::Range) -> String {
    format!("{RANGE_PREFIX}{}~{}", range.from, range.to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tilo_session_core::desktop::session::{ActivityAsk, ActivityReads, activity_requests};

    /// The model as a test plays it: one change of one workspace, one of another, and the same
    /// reads each pass.
    struct Model {
        revision: u64,
        /// The summary's first change, as the person reads it.
        summary: String,
        /// How many changes the last feed read asked for.
        limit: usize,
        /// How many of each session's changes still in effect the last sessions read asked for.
        changes: usize,
    }

    impl Model {
        fn new() -> Self {
            Self {
                revision: 1,
                summary: "Renamed a note".to_owned(),
                limit: 0,
                changes: 0,
            }
        }
    }

    impl ActivityReads for Model {
        fn revision(&self) -> u64 {
            self.revision
        }

        #[expect(
            clippy::too_many_lines,
            reason = "one test's whole flow, from its setup to what it proves"
        )]
        fn read(&mut self, action: &str, arguments: &Map<String, Value>) -> Result<Value, String> {
            let feed = |workspace: &str, summary: &str| {
                serde_json::json!({
                    "workspace": workspace,
                    "scope": if workspace == "Home" { "this-workspace" } else { "all-workspaces" },
                    "total": 1,
                    "entries": [{
                        "id": "00112233445566778899aabbccddeeff",
                        "author": {"name": "Ada", "rgb": [200, 120, 90], "identity": "ada",
                                   "relation": "reader", "for_reader": true},
                        "kind": "text",
                        "at_ms": 1_700_000_000_000_i64,
                        "summary": summary,
                        "app": "Notes",
                        "redacted": false,
                    }],
                    "groups": [],
                    "people": ["Ada"],
                    "lenses": [],
                    "pending": false,
                    "mutations": true,
                })
            };
            match action {
                wire::FEED => {
                    let query: wire::FeedQuery =
                        serde_json::from_value(Value::Object(arguments.clone()))
                            .map_err(|error| error.to_string())?;
                    self.limit = query.page();
                    Ok(match query.scope {
                        wire::Scope::ThisWorkspace => feed("Home", &self.summary),
                        wire::Scope::AllWorkspaces => feed("Elsewhere", &self.summary),
                    })
                }
                wire::SUMMARY => Ok(serde_json::json!({
                    "workspace": "Home",
                    "briefs": [{
                        "title": "A morning of notes",
                        "detail": "Ada changed two notes",
                        "changes": 2,
                        "actions": [],
                        "first_ms": 1_700_000_000_000_i64,
                        "last_ms": 1_700_000_060_000_i64,
                        "entries": ["00112233445566778899aabbccddeeff"],
                    }],
                    "away": true,
                    "sources_available": true,
                    "sources": [],
                    "pending": false,
                })),
                wire::REPLAY => Ok(serde_json::json!({
                    "frames": [{
                        "entry": "00112233445566778899aabbccddeeff",
                        "author": {"name": "Ada", "rgb": [200, 120, 90], "identity": "ada",
                                   "relation": "reader", "for_reader": true},
                        "summary": "Renamed a note",
                        "at_ms": 1_700_000_000_000_i64,
                        "workspaces": [{"name": "Home", "groups": []}],
                        "pointers": [],
                    }],
                    "unavailable": 0,
                    "pending": false,
                })),
                wire::SESSIONS => {
                    let query: wire::SessionsQuery =
                        serde_json::from_value(Value::Object(arguments.clone()))
                            .map_err(|error| error.to_string())?;
                    self.changes = query.changes_page();
                    Ok(serde_json::json!({
                        "sessions": [{
                            "id": "run-1",
                            "agent": {"name": "Ada's agent", "rgb": [90, 120, 200], "identity": "agent",
                                      "relation": "software", "for_reader": true},
                            "instruction": "Tidy the notes",
                            "started_ms": 1_700_000_000_000_i64,
                            "last_ms": 1_700_000_030_000_i64,
                            "range": {"from": "00112233445566778899aabbccddeeff",
                                      "to": "00112233445566778899aabbccddeeff"},
                            "actions": [{
                                "range": {"from": "00112233445566778899aabbccddeeff",
                                          "to": "00112233445566778899aabbccddeeff"},
                                "changes": 1,
                                "at_ms": 1_700_000_030_000_i64,
                                "summary": "Renamed a note",
                                "view": {"kind": "record", "target": {"id": "note-1", "kind": "note", "label": "Plan"}},
                                "app_id": "so.tilo.lang.notes",
                                "record": "note-1",
                            }],
                            "in_effect": [{
                                "range": {"from": "00112233445566778899aabbccddeeff",
                                          "to": "00112233445566778899aabbccddeeff"},
                                "step": "aa11bb22cc33dd44ee55ff660011223344556677",
                                "at_ms": 1_700_000_030_000_i64,
                                "summary": "Renamed a note",
                                "app_id": "so.tilo.lang.notes",
                                "record": "note-1",
                                "target": {"id": "note-1", "kind": "note", "label": "Plan"},
                                "undo": {"action": "undo-entry", "label": "Undo this step"},
                            }],
                            "changes_total": 1,
                        }],
                        "total": 1,
                        "pending": false,
                        "mutations": true,
                    }))
                }
                wire::DETAIL => Ok(serde_json::json!({
                    "title": "Renamed a note",
                    "provenance": "Ada renamed it",
                    "authors": ["Ada"],
                    "resources": [{"id": "note-1", "kind": "note", "label": "Plan"}],
                    "offers": [{"action": "undo", "label": "Undo"}],
                    "changes": [{"field": "Title", "before": "Plan", "after": "Trip"}],
                    "gone": false,
                })),
                wire::FOCUS => Ok(Value::Null),
                wire::SESSION_OUTPUT => Ok(serde_json::json!({
                    "lines": ["$ cargo test", "ok"],
                    "truncated": false,
                    "unavailable": false,
                    "running": false,
                })),
                other => Err(format!("no read `{other}`")),
            }
        }
    }

    /// Run the model's reads once, as the shell's activity loop does each pass.
    fn serve(requests: &ActivityRequests, model: &mut Model) {
        requests.serve(model);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one test's whole flow, from its setup to what it proves"
    )]
    fn the_reads_an_app_shows_are_one_reading_and_a_write_runs_on_the_model() {
        let requests = activity_requests(Arc::new(AtomicBool::new(false)));
        let mut model = Model::new();
        let mut served = Served::new(requests.clone());
        // The service follows its reads on its first pass; the model answers them on its next.
        let _ = served.poll();
        serve(&requests, &mut model);
        assert!(
            served.poll(),
            "the first pass after the model read takes it in"
        );

        let json = served.read(RESOURCE).expect("the activity is published");
        let reading: Activity = serde_json::from_value(json).expect("the reading reads");
        let feed = reading.feed.expect("this workspace's feed");
        assert_eq!(feed.workspace, "Home");
        assert_eq!(feed.entries[0].summary, "Renamed a note");
        assert_eq!(feed.entries[0].author.name, "Ada");
        assert_eq!(
            reading
                .everywhere
                .expect("every workspace's feed")
                .workspace,
            "Elsewhere",
            "every workspace of this device is read beside the one the person is in"
        );
        let summary = reading.summary.expect("what is new");
        assert_eq!(summary.briefs[0].title, "A morning of notes");
        assert!(summary.away);
        assert_eq!(
            reading.replay.expect("the replay").frames[0].workspaces[0].name,
            "Home"
        );
        let sessions = reading.sessions.expect("agent sessions");
        assert_eq!(sessions.sessions[0].instruction, "Tidy the notes");
        assert!(
            matches!(
                &sessions.sessions[0].actions[0].view,
                wire::ActionView::Record { target } if target.id == "note-1"
            ),
            "the action's view says what it changed"
        );

        // The model reads differently: the reading moves, and only then.
        assert!(
            !served.poll(),
            "a pass the model did not move changes nothing"
        );
        model.revision = 2;
        model.summary = "Added a page".to_owned();
        serve(&requests, &mut model);
        assert!(
            served.poll(),
            "a read that moved the model changes the reading"
        );
        let json = served.read(RESOURCE).unwrap();
        let reading: Activity = serde_json::from_value(json).unwrap();
        assert_eq!(
            reading.feed.expect("this workspace's feed").entries[0].summary,
            "Added a page"
        );

        // A write is the model's action, with the arguments the caller gave.
        let answered = served
            .invoke(
                RESOURCE,
                wire::MARK_VIEWED,
                &serde_json::json!({"entries": ["00112233445566778899aabbccddeeff"], "later": true})
                    .as_object()
                    .cloned()
                    .unwrap(),
            )
            .expect("the write is taken");
        assert!(matches!(answered, Started::Later(_)));
        let asks = requests.take();
        let ActivityAsk::Act {
            action,
            arguments,
            answer,
        } = asks.into_iter().next().expect("the model was asked")
        else {
            panic!("a write is an act");
        };
        assert_eq!(action, wire::MARK_VIEWED);
        assert_eq!(
            arguments
                .get("entries")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        answer.complete(Ok(()));

        // A range of changes is a reading of its own, keyed by that range: a view that shows
        // one asks for it by its id, and what the model reads of it is handed to that view.
        let range = wire::Range {
            from: "00112233445566778899aabbccddeeff".to_owned(),
            to: "00112233445566778899aabbccddeeff".to_owned(),
        };
        let resource = range_resource(&range);
        let first = served.read(&resource).expect("a range reads");
        assert!(
            first["detail"].is_null(),
            "a range the model has not read yet reads as one with nothing in it"
        );
        // The pass that follows the range the view named; the model answers it on its next.
        let _ = served.poll();
        serve(&requests, &mut model);
        assert!(served.poll(), "the range's read arrives");
        let detail = served.read(&resource).expect("the range reads");
        assert_eq!(detail["detail"]["title"], "Renamed a note");
        assert_eq!(detail["detail"]["changes"][0]["field"], "Title");
        assert!(
            served
                .moved()
                .is_some_and(|moved| moved.contains(&resource)),
            "the range's reading is named as the one that moved"
        );
        assert_eq!(served.moved(), None, "and it moved once");

        // What the workspace held either side of that range is its own reading, so a window that
        // previews a change asks for exactly that range's comparison.
        let compare = compare_resource(&range);
        let (action, arguments) = Key::Compare
            .read(&compare)
            .expect("the comparison read is asked");
        assert_eq!(action, wire::DETAIL);
        assert_eq!(
            arguments.get("compare").and_then(Value::as_bool),
            Some(true),
            "the comparison is asked as one"
        );
        let first = served.read(&compare).expect("the comparison reads");
        assert!(
            first["detail"].is_null(),
            "a comparison the model has not read yet reads as one with nothing in it"
        );
        let _ = served.poll();
        serve(&requests, &mut model);
        assert!(served.poll(), "the comparison's read arrives");
        let detail = served.read(&compare).expect("the comparison reads");
        assert_eq!(detail["detail"]["changes"][0]["after"], "Trip");

        // A page of the feed is a reading too, keyed by the person's filters: a window asking
        // for a week of one workspace reads that page and no other.
        let page = feed_resource("this", "week", "app:so.tilo.notes", 400);
        let first = served.read(&page).expect("a page reads");
        assert!(
            first["feed"].is_null(),
            "a page the model has not read yet reads as one with nothing in it"
        );
        let _ = served.poll();
        serve(&requests, &mut model);
        assert!(served.poll(), "the page's read arrives");
        let page_read = served.read(&page).expect("the page reads");
        assert_eq!(page_read["feed"]["workspace"], "Home");
        assert_eq!(
            model.limit, 400,
            "a page asks the model for as many of the newest changes as it shows"
        );
        assert!(
            served.moved().is_some_and(|moved| moved.contains(&page)),
            "the page's reading is named as the one that moved"
        );

        // Every agent session across the person's workspaces is one more reading, keyed
        // `sessions/<scope>/<changes>`: what a window listing them all shows, and how many of
        // each session's changes still in effect its page holds.
        let all = "workspace.activity/sessions~all~40";
        let (action, arguments) = Key::Sessions
            .read(all)
            .expect("the keyed sessions read is asked");
        assert_eq!(action, wire::SESSIONS);
        assert_eq!(
            arguments.get("scope").and_then(Value::as_str),
            Some("all-workspaces"),
            "every workspace of the person is read, not this one's"
        );
        assert_eq!(
            arguments.get("changes").and_then(Value::as_u64),
            Some(40),
            "the page of changes a window shows is the one asked for"
        );
        let this = "workspace.activity/sessions~this~0";
        let (_, arguments) = Key::Sessions
            .read(this)
            .expect("a window's own workspace sessions read");
        assert_eq!(
            arguments.get("scope").and_then(Value::as_str),
            Some("this-workspace")
        );
        let first = served.read(all).expect("the sessions read");
        assert!(
            first["sessions"].as_array().is_some_and(Vec::is_empty),
            "a read the model has not answered yet holds no session"
        );
        let _ = served.poll();
        serve(&requests, &mut model);
        assert!(served.poll(), "the sessions' read arrives");
        let sessions = served.read(all).expect("the sessions read");
        assert_eq!(sessions["sessions"][0]["instruction"], "Tidy the notes");
        assert_eq!(sessions["mutations"], true);
        assert_eq!(
            model.changes, 40,
            "a window asks the model for exactly the page of a session's changes it shows"
        );
        let changes = &sessions["sessions"][0]["in_effect"];
        assert_eq!(changes[0]["summary"], "Renamed a note");
        assert_eq!(
            changes[0]["step"], "aa11bb22cc33dd44ee55ff660011223344556677",
            "the change is named as its record's own history names it, for its undo"
        );
        assert_eq!(changes[0]["target"]["label"], "Plan");
        assert_eq!(changes[0]["undo"]["label"], "Undo this step");
        assert_eq!(changes[0]["undo"]["action"], "undo-entry");
        assert_eq!(sessions["sessions"][0]["changes_total"], 1);
        assert!(
            served
                .moved()
                .is_some_and(|moved| moved.contains(&all.to_owned())),
            "the sessions' reading is named as the one that moved"
        );

        // A command's whole output is one more reading, keyed by the execution the session's
        // action names: a window that shows one command's output reads that one.
        let output = output_resource(Some("Home"), "so.tilo.terminal", "shell-1", "exec-1");
        let (action, arguments) = Key::Output
            .read(&output)
            .expect("the keyed output read is asked");
        assert_eq!(action, wire::SESSION_OUTPUT);
        assert_eq!(
            arguments.get("execution").and_then(Value::as_str),
            Some("exec-1")
        );
        assert_eq!(
            arguments.get("workspace").and_then(Value::as_str),
            Some("Home"),
            "the workspace its shell's record is in is named"
        );
        let first = served.read(&output).expect("the output reads");
        assert!(
            first["output"].is_null(),
            "a command's output the model has not read yet holds nothing"
        );
        let _ = served.poll();
        serve(&requests, &mut model);
        assert!(served.poll(), "the output's read arrives");
        let output = served.read(&output).expect("the output reads");
        assert_eq!(output["output"]["lines"][1], "ok");
        assert_eq!(output["output"]["running"], false);
    }
}
