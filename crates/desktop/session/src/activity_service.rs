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

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::remote::workspace_activity as wire;
use tilo_actions::{ActionInvoker, ApiError, ResourceSpec};
use tilo_service::{Service, Started, offer};
use tilo_session_core::desktop::session::ActivityRequests;

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

/// The kind a reader with no words of its own knows the resource by.
#[derive(tilo_resource::Row)]
#[row(kind = "activity", label = "Activity")]
pub(crate) struct ActivityRoot;

/// Who made a change.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Author {
    /// The name it goes by.
    pub name: String,
    /// The colour presence paints them in.
    pub rgb: Vec<u8>,
    /// A short fragment of their key, telling two of the same name apart.
    pub identity: String,
    /// `reader`, another `person`, or `software` acting for someone.
    pub relation: String,
    /// For software: who it acted for.
    pub on_behalf_of: Option<String>,
    /// The person it acted for is the reader.
    pub for_reader: bool,
    /// The task that named it.
    pub task: Option<String>,
}

/// A device that refused one of a change's writes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Refusal {
    pub device: String,
    pub label: String,
    pub reason: String,
    /// Whether waiting will change nothing.
    pub permanent: bool,
}

/// One change.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Entry {
    pub id: String,
    pub author: Author,
    /// `text`, `move`, `details`, `comment`, `delete`, `restore`, `access`, `compaction` or
    /// `redaction`.
    pub kind: String,
    /// When, in milliseconds since the Unix epoch.
    pub at_ms: i64,
    pub summary: String,
    /// The app it was made in.
    pub made_in: Option<String>,
    /// The resource it changed.
    pub resource: Option<String>,
    /// The named work group it sits in.
    pub group: Option<String>,
    /// Whether its content was redacted.
    pub redacted: bool,
    /// The devices that refused it.
    pub refused: Vec<Refusal>,
}

/// A named range of changes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Group {
    pub id: String,
    pub label: String,
    pub start: String,
    pub end: Option<String>,
    pub named_by: Option<String>,
}

/// A way to narrow the feed.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Lens {
    pub key: String,
    pub label: String,
}

/// A workspace's history, newest first.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Feed {
    /// The workspace's name.
    pub workspace: String,
    /// `this-workspace` or `all-workspaces`.
    pub scope: String,
    /// Changes the model matched, of which `entries` holds the newest.
    pub total: i64,
    pub entries: Vec<Entry>,
    pub groups: Vec<Group>,
    /// Everyone who changed something, in the order they first did.
    pub people: Vec<String>,
    pub lenses: Vec<Lens>,
    /// The history is still being read.
    pub pending: bool,
    /// Whether this device can change the history here.
    pub mutations: bool,
}

/// One action of a work session's evidence.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct BriefAction {
    pub author: Author,
    pub summary: String,
    pub at_ms: i64,
}

/// A work session: related changes told as an outcome.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Brief {
    pub title: String,
    pub detail: String,
    pub authors: Vec<Author>,
    pub apps: Vec<String>,
    pub resources: Vec<String>,
    pub changes: i64,
    /// Oldest first.
    pub actions: Vec<BriefAction>,
    pub first_ms: i64,
    pub last_ms: i64,
    pub group: Option<String>,
    /// The record of a person-corrected session.
    pub work_session: Option<String>,
    /// The changes it tells of.
    pub entries: Vec<String>,
}

/// A resource a change touched, and where it opens.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub(crate) struct Target {
    pub app_id: Option<String>,
    pub app_label: Option<String>,
    pub id: String,
    pub kind: String,
    pub label: String,
}

/// Where the reader left off.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Resume {
    pub target: Target,
    pub author: Author,
    pub app: Option<String>,
    pub at_ms: i64,
}

/// A resource this device chose to hear about each morning.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Source {
    pub key: String,
    pub label: String,
    /// What its app says about it now.
    pub text: String,
    pub refreshable: bool,
    /// How the last command on it went.
    pub status: Option<String>,
}

/// What changed since this device last looked.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Summary {
    pub workspace: String,
    /// Newest first.
    pub briefs: Vec<Brief>,
    /// The briefs told in a sentence by a model on this device.
    pub narrative: Option<String>,
    pub resume: Option<Resume>,
    /// Whether these are the changes made while the reader was away.
    pub away: bool,
    /// Whether morning sources can be chosen here.
    pub sources_available: bool,
    pub sources: Vec<Source>,
    pub pending: bool,
}

/// One group of tabs in a replayed workspace.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct ReplayGroup {
    pub tabs: Vec<String>,
    /// The tab it showed, by index into `tabs`.
    pub active: i64,
    /// Its share of the strip, in thousandths.
    pub width_permille: i64,
}

/// A workspace as it was.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct ReplayWorkspace {
    pub name: String,
    pub groups: Vec<ReplayGroup>,
}

/// Someone's pointer as it was.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct ReplayPointer {
    pub name: String,
    pub rgb: Vec<u8>,
}

/// The layout after one change.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Frame {
    pub entry: String,
    pub author: Author,
    pub summary: String,
    pub at_ms: i64,
    pub workspaces: Vec<ReplayWorkspace>,
    pub pointers: Vec<ReplayPointer>,
}

/// The workspace's layout at each recent change.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Replay {
    /// Oldest first.
    pub frames: Vec<Frame>,
    /// Changes whose layout could not be read back.
    pub unavailable: i64,
    /// What the replay cannot show, said once.
    pub note: Option<String>,
    pub pending: bool,
}

/// One action a view may offer for a range of changes.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub(crate) struct Offer {
    /// What it does, as the model names it.
    pub verb: String,
    /// How it reads.
    pub label: String,
}

/// One field a range of changes changed, with what it held either side of it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub(crate) struct FieldChange {
    /// The field's name as the person reads it.
    pub field: String,
    /// What it held before; empty when it was not there.
    pub before: String,
    /// What it holds now; empty when it was removed.
    pub after: String,
}

/// What a range of changes did, as a view of that one range reads it: the reading a view names
/// as `resource("workspace.activity/range~<from>~<to>", of: activity.RangeDetail)`.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub(crate) struct Detail {
    pub title: String,
    /// Who, where and why, in the history's own words.
    pub provenance: String,
    pub authors: Vec<String>,
    pub resources: Vec<Target>,
    pub group: Option<String>,
    pub group_label: Option<String>,
    /// What can be done with it, worded for this range.
    pub offers: Vec<Offer>,
    /// The fields it changed, with what they held either side.
    pub changes: Vec<FieldChange>,
    /// What a comparison of the workspace before and after it says, one line each.
    pub comparison: Option<String>,
    /// The session is no longer in the history (an undo or a filter moved it).
    pub gone: bool,
}

/// What to show, asked for outside the app.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Focus {
    /// Each ask has its own serial; the app that shows it says so.
    pub serial: i64,
    /// `whats-new`, `history` or `replay`.
    pub view: String,
    pub author: Option<String>,
    pub lens: Option<String>,
    /// This change, selected.
    pub entry: Option<String>,
}

/// A range of changes, by its first and last in feed order.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub(crate) struct Span {
    pub from: String,
    pub to: String,
}

/// Where a command's execution is recorded: the workspace its shell's record is in (none for this
/// one), the app and record that keep the session, and the execution in it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub(crate) struct Execution {
    pub workspace: Option<String>,
    pub app_id: String,
    pub record: String,
    pub execution: String,
}

/// One line of a file's change.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub(crate) struct DiffLine {
    /// `both`, `removed` or `added`.
    pub side: String,
    pub text: String,
}

/// One action an agent session took, and what it did.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct SessionAction {
    /// Its changes, which Undo takes back on their own.
    pub range: Span,
    /// How many changes `range` is.
    pub changes: i64,
    pub at_ms: i64,
    /// What it did, as a sentence.
    pub summary: String,
    /// `file`, `command`, `page` or `record`.
    pub kind: String,
    /// A file's path, a command line, a page's address, or a record's id.
    pub place: String,
    /// The resource the action touched, where it opens.
    pub target: Target,
    /// The page's title, for a page.
    pub title: String,
    /// The directory a command ran in.
    pub cwd: String,
    /// The last lines a command printed.
    pub output: Vec<String>,
    /// It printed more than `output` holds.
    pub more: bool,
    /// How a command ended; absent while it runs or when unknown.
    pub exit: Option<i64>,
    pub ended_ms: Option<i64>,
    /// The lines a file's change added or removed, each with its side.
    pub diff: Vec<DiffLine>,
    /// The app and record it changed.
    pub app_id: String,
    pub record: String,
    /// The execution a command ran as, when its shell recorded one: what reads everything it
    /// printed.
    pub execution: Option<Execution>,
    /// The action that undoes this record's share of the session alone, when it can be undone on
    /// its own here.
    pub undo_label: Option<String>,
    /// What that action is, as the model names it: `undo-entry` for the record's share,
    /// `undo` for the range of changes it took.
    pub undo_verb: Option<String>,
}

/// One of a session's changes still in effect: what it changed, where, and how to open it and
/// take it back on its own.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub(crate) struct SessionChange {
    /// The change, as a range of one in the history.
    pub range: Span,
    /// The change in its record's own history, which its undo names as the step.
    pub step: String,
    pub at_ms: i64,
    /// What it changed, as a sentence.
    pub summary: String,
    /// The app and record it changed.
    pub app_id: String,
    pub record: String,
    /// The resource it touched, where it opens.
    pub target: Target,
    /// What the press to undo it says, when it can be undone on its own here.
    pub undo_label: Option<String>,
    /// What that press is, as the model names it: `undo-entry` for the record's share.
    pub undo_verb: Option<String>,
}

/// One agent session: an instruction it worked, and the actions it took for it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct AgentSession {
    pub id: String,
    pub agent: Author,
    /// The workspace it worked in, under every workspace's sessions.
    pub workspace: Option<String>,
    /// What it was asked, as the person phrased it.
    pub instruction: String,
    /// Why it did what it did.
    pub decision: Option<String>,
    pub started_ms: i64,
    pub last_ms: i64,
    /// Every change it made, which Undo takes back.
    pub range: Span,
    /// Oldest first.
    pub actions: Vec<SessionAction>,
    /// Its changes still in effect, newest first: what the session still leaves in place, each
    /// undone on its own. A page of `changes_total`.
    pub in_effect: Vec<SessionChange>,
    /// How many of its changes are still in effect.
    pub changes_total: i64,
    /// Why it cannot be undone from here.
    pub undo_blocked: Option<String>,
    /// What it is doing now, as a sentence; empty when it is not working.
    pub now: String,
    /// What its `now` is: `running`, `editing`, `browsing`, `changing` or `ran`; empty when it
    /// is not working.
    pub now_kind: String,
}

/// Every agent session the history holds, newest first.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Sessions {
    pub sessions: Vec<AgentSession>,
    pub total: i64,
    pub pending: bool,
    pub mutations: bool,
}

/// The activity as an app reads it: the model's last reads, taken together.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct Activity {
    /// This workspace's changes.
    pub feed: Option<Feed>,
    /// Every workspace's changes on this device.
    pub everywhere: Option<Feed>,
    /// What changed since this device last looked.
    pub summary: Option<Summary>,
    /// The layout at each recent change.
    pub replay: Option<Replay>,
    /// Every agent session.
    pub sessions: Option<Sessions>,
    /// What the person was asked to open from outside, until the app shows it.
    pub focus: Option<Focus>,
}

/// A page of the feed as its own reading: one optional section, as a range's detail is.
#[derive(Clone, Debug, Default, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub(crate) struct FeedReading {
    pub feed: Option<Feed>,
}

/// A range's detail as its own reading: one optional section, so a range the model has not read
/// yet reads as having nothing, rather than as a record missing its fields.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub(crate) struct RangeDetail {
    pub detail: Option<Detail>,
}

/// A command's whole output as its own reading: at most the lines its shell kept, or why this
/// device holds none.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub(crate) struct OutputReading {
    pub output: Option<SessionOutput>,
}

/// Everything a command printed, as this device keeps it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub(crate) struct SessionOutput {
    /// Its lines, without terminal escapes.
    pub lines: Vec<String>,
    /// It printed more than the lines hold, or more than its shell kept.
    pub truncated: bool,
    /// The execution is not known here, or its output has not reached this device.
    pub unavailable: bool,
    /// The execution has not ended.
    pub running: bool,
}

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
        match self {
            Self::Range | Self::Compare => detail_json(value),
            Self::Feed => feed_json(value),
            Self::Sessions => sessions_json(value),
            Self::Output => output_json(value),
        }
    }

    /// What it reads as before the model has answered.
    fn empty(self) -> Value {
        match self {
            Self::Range | Self::Compare => empty_detail(),
            Self::Feed => empty_feed(),
            Self::Sessions => serde_json::json!({
                "sessions": [],
                "total": 0,
                "pending": true,
                "mutations": false,
            }),
            Self::Output => empty_output(),
        }
    }
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
        let sessions: Sessions = self
            .last::<SessionsValue>(Read::Sessions)
            .map(SessionsValue::into_sessions)
            .unwrap_or_default();
        let focus: Option<Focus> = self.last(Read::Focus);
        self.reading = Activity {
            feed: Some(feed),
            everywhere: Some(everywhere),
            summary: Some(summary),
            replay: Some(replay),
            sessions: Some(sessions),
            focus,
        };
        self.json = serde_json::to_value(&self.reading).unwrap_or(Value::Null);
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<ActivityAction>(tilo_resource::typed_resource::<
            ActivityRoot,
        >(RESOURCE, ""))]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        if resource == RESOURCE {
            return Some(self.json.clone());
        }
        let Some(key) = Key::of(resource) else {
            return None;
        };
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
        let ran = self.requests.act(wire_action(action), arguments.clone());
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

/// The model's action a verb of this package names. Two of them carry a word the session's own
/// package already declares (`open`, `undo`): a caller of this package's surface says which of
/// them it means, and the model hears its own name.
fn wire_action(action: &str) -> &str {
    match action {
        "open-change" => wire::OPEN,
        "undo-change" => wire::UNDO,
        "merge-groups" => wire::MERGE,
        other => other,
    }
}

/// Whether `action` changes only what the person at this device saw and hears about.
fn is_personal(action: &str) -> bool {
    tilo_session_core::desktop::activity::is_personal(action)
}

/// The resource as a view of another language reads and changes it: the reading's fields, and the
/// model's writes as its verbs. Published as `activity-surface.json` beside the crate, which is
/// what an app compiles against; this is the same surface as the session builds it.
#[cfg(test)]
#[must_use]
pub(crate) fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<Activity, ActivityAction>("activity", "Activity")
}

/// The reading as a view of another language names it: a keyed resource's fields are its root
/// record's, and `reading_surface` folds that record into the resource. A view names the type in
/// `resource(…, of: history.RangeDetail)`, so the record is declared beside the fields too.
#[cfg(test)]
fn named_root(
    mut surface: tilo_actions::surface::Surface,
    name: &str,
    doc: &str,
) -> tilo_actions::surface::Surface {
    let fields = surface
        .fields
        .iter()
        .map(|field| tilo_actions::surface::TypeField {
            name: field.path.clone(),
            ty: field.ty.clone(),
            title: field.title.clone(),
        })
        .collect();
    surface.types.push(tilo_actions::surface::NamedType {
        name: name.to_owned(),
        doc: doc.to_owned(),
        fields,
        cases: Vec::new(),
    });
    surface
}

/// A page of the feed as a view of another language reads it.
#[cfg(test)]
#[must_use]
pub(crate) fn feed_surface() -> tilo_actions::surface::Surface {
    named_root(
        tilo_resource::surface::reading_surface::<FeedReading, tilo_resource::NoEffect>(
            "activity-feed",
            "Activity feed",
        ),
        "FeedReading",
        "A page of the feed, narrowed by what the person chose.",
    )
}

/// A range's detail as a view of another language reads it: the reading a view names by the
/// range it shows. It offers no verbs of its own: what can be done with a range is one of the
/// activity's, worded for this range.
#[cfg(test)]
#[must_use]
pub(crate) fn range_surface() -> tilo_actions::surface::Surface {
    named_root(
        tilo_resource::surface::reading_surface::<RangeDetail, tilo_resource::NoEffect>(
            "activity-range",
            "Activity change",
        ),
        "RangeDetail",
        "What a range of changes did.",
    )
}

/// A command's whole output as a view of another language reads it: the reading a view names by
/// the command it shows. It offers no verbs of its own.
#[cfg(test)]
#[must_use]
pub(crate) fn output_surface() -> tilo_actions::surface::Surface {
    named_root(
        tilo_resource::surface::reading_surface::<OutputReading, tilo_resource::NoEffect>(
            "activity-output",
            "Command output",
        ),
        "OutputReading",
        "Everything a command printed, as this device keeps it.",
    )
}

/// What a view or an agent asks of the activity: an action of the model's, with its arguments.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.activity")]
pub enum ActivityAction {
    /// Mark the changes the reader was shown as seen on this device.
    #[effect(kind = "undoable")]
    MarkViewed {
        /// The changes, by their history id.
        #[param("The changes, by their history id")]
        entries: Vec<String>,
        /// Count them seen once the reader leaves what is new.
        #[param("Count them seen once the reader leaves what is new")]
        later: bool,
    },
    /// Say the ask the serial names was shown.
    #[effect(kind = "undoable")]
    Focused {
        /// The serial of the ask.
        #[param("The serial of the ask")]
        serial: i64,
    },
    /// Name, rename or un-name a range of changes; an empty name removes it.
    #[effect(kind = "undoable")]
    Name {
        /// The changes to name.
        range: Span,
        /// The naming record a rename replaces.
        #[param("The naming record a rename replaces")]
        record: Option<String>,
        /// The name; empty removes it.
        #[param("The name; empty removes it")]
        label: String,
    },
    /// Split the named work group a range sits in at its start.
    #[effect(kind = "undoable")]
    Split {
        /// The first change.
        #[param("The first change")]
        from: String,
        /// The last change.
        #[param("The last change")]
        to: String,
    },
    /// Merge the named work groups a range spans.
    #[effect(kind = "undoable")]
    MergeGroups {
        /// The first change.
        #[param("The first change")]
        from: String,
        /// The last change.
        #[param("The last change")]
        to: String,
    },
    /// Move the boundary between two work sessions.
    #[effect(kind = "undoable")]
    MoveBoundary {
        /// A change of the older session.
        #[param("A change of the older session")]
        older: String,
        /// A change of the newer session.
        #[param("A change of the newer session")]
        newer: String,
        /// The way the boundary moves: `newer` or `older`.
        #[param("The way the boundary moves: `newer` or `older`")]
        toward: String,
    },
    /// Undo a range of changes.
    #[effect(kind = "undoable", destructive)]
    UndoChange {
        /// The first change.
        #[param("The first change")]
        from: String,
        /// The last change.
        #[param("The last change")]
        to: String,
    },
    /// Restore the workspace to before a range of changes.
    #[effect(kind = "undoable", destructive)]
    Restore {
        /// The first change.
        #[param("The first change")]
        from: String,
        /// The last change.
        #[param("The last change")]
        to: String,
    },
    /// Undo the agent session a range belongs to.
    #[effect(kind = "undoable", destructive)]
    UndoSession {
        /// The first change.
        #[param("The first change")]
        from: String,
        /// The last change.
        #[param("The last change")]
        to: String,
    },
    /// Undo one record's share of an agent session.
    #[effect(kind = "undoable", destructive)]
    UndoRecord {
        /// The session, by its range in the history.
        range: Span,
        /// The session, by the task it worked.
        #[param("The session, by the task it worked")]
        task: Option<String>,
        /// The app's id.
        #[param("The app's id")]
        app_id: String,
        /// The record's own id.
        #[param("The record's own id")]
        id: String,
        /// The one change inside that record to take back, as the record's own history names
        /// it; absent takes back the session's whole share of the record.
        #[param(
            "The one change inside that record to take back, as the record's own history names it"
        )]
        step: Option<String>,
    },
    /// Open what a change touched in its app.
    #[effect(kind = "undoable")]
    OpenChange {
        /// The app it opens in.
        #[param("The app it opens in")]
        app_id: Option<String>,
        /// The app's name.
        #[param("The app's name")]
        app_label: Option<String>,
        /// The resource's id.
        #[param("The resource's id")]
        id: String,
        /// The resource's kind.
        #[param("The resource's kind")]
        kind: String,
        /// The resource's title.
        #[param("The resource's title")]
        label: String,
    },
    /// Choose, or stop choosing, the resource a range changed as a morning source.
    #[effect(kind = "undoable")]
    ChooseSource {
        /// The first change.
        #[param("The first change")]
        from: String,
        /// The last change.
        #[param("The last change")]
        to: String,
    },
    /// Run a command on a chosen morning source.
    #[effect(kind = "undoable")]
    Source {
        /// The source's key.
        #[param("The source's key")]
        key: String,
        /// `open`, `remove`, `refresh`, `draft-refresh` or `next-source`.
        #[param("`open`, `remove`, `refresh`, `draft-refresh` or `next-source`")]
        command: String,
    },
}

/// The sessions read, before it is narrowed to what an app reads of it.
#[derive(Default, Deserialize)]
#[serde(default)]
struct SessionsValue {
    sessions: Vec<wire::AgentSession>,
    total: usize,
    pending: bool,
    mutations: bool,
}

impl SessionsValue {
    fn into_sessions(self) -> Sessions {
        Sessions {
            sessions: self.sessions.iter().map(session).collect(),
            total: i64::try_from(self.total).unwrap_or(i64::MAX),
            pending: self.pending,
            mutations: self.mutations,
        }
    }
}

/// An agent session as an app reads it.
fn session(session: &wire::AgentSession) -> AgentSession {
    let (now, now_kind) = session.now.as_ref().map_or_else(
        || (String::new(), String::new()),
        |now| match now {
            wire::Now::Running { command, .. } => (command.clone(), "running".to_owned()),
            wire::Now::Editing { path, .. } => (path.clone(), "editing".to_owned()),
            wire::Now::Browsing { url, title, .. } => (
                if title.is_empty() {
                    url.clone()
                } else {
                    title.clone()
                },
                "browsing".to_owned(),
            ),
            wire::Now::Changing { summary, .. } => (summary.clone(), "changing".to_owned()),
            wire::Now::Ran { command, .. } => (command.clone(), "ran".to_owned()),
        },
    );
    AgentSession {
        id: session.id.clone(),
        agent: author(&session.agent),
        workspace: session.workspace.clone(),
        instruction: session.instruction.clone(),
        decision: session.decision.clone(),
        started_ms: session.started_ms,
        last_ms: session.last_ms,
        range: span(&session.range),
        actions: session.actions.iter().map(action).collect(),
        in_effect: session.in_effect.iter().map(change).collect(),
        changes_total: i64::try_from(session.changes_total).unwrap_or(i64::MAX),
        undo_blocked: session.undo_blocked.clone(),
        now,
        now_kind,
    }
}

/// What an undo a session's change offers is, as an app names the press: `undo-entry` takes back
/// the record's share of it, `undo-change` the range, and the rest are the history's own verbs.
fn action_verb(action: wire::DetailAction) -> String {
    match action {
        wire::DetailAction::UndoEntry => "undo-entry",
        wire::DetailAction::Undo => "undo-change",
        other => other.action(),
    }
    .to_owned()
}

/// One of an agent session's changes still in effect, as an app reads it.
fn change(change: &wire::SessionChange) -> SessionChange {
    let (undo_label, undo_verb) = change.undo.as_ref().map_or((None, None), |offer| {
        (Some(offer.label.clone()), Some(action_verb(offer.action)))
    });
    SessionChange {
        range: span(&change.range),
        step: change.step.clone(),
        at_ms: change.at_ms,
        summary: change.summary.clone(),
        app_id: change.app_id.clone(),
        record: change.record.clone(),
        target: Target {
            app_id: change.target.app_id.clone(),
            app_label: change.target.app_label.clone(),
            id: change.target.id.clone(),
            kind: change.target.kind.clone(),
            label: change.target.label.clone(),
        },
        undo_label,
        undo_verb,
    }
}

/// One action of an agent session as an app reads it.
fn action(action: &wire::SessionAction) -> SessionAction {
    let (kind, place, title, cwd, output, more, exit, ended_ms, diff) = match &action.view {
        wire::ActionView::File { path, lines, .. } => (
            "file",
            path.clone(),
            String::new(),
            String::new(),
            Vec::new(),
            false,
            None,
            None,
            lines
                .iter()
                .map(|line| DiffLine {
                    side: match line.side {
                        wire::DiffSide::Both => "both",
                        wire::DiffSide::Removed => "removed",
                        wire::DiffSide::Added => "added",
                    }
                    .to_owned(),
                    text: line.text.clone(),
                })
                .collect(),
        ),
        wire::ActionView::Command {
            command,
            cwd,
            output,
            more,
            exit,
            ended_ms,
            ..
        } => (
            "command",
            command.clone(),
            String::new(),
            cwd.clone(),
            output.clone(),
            *more,
            *exit,
            *ended_ms,
            Vec::new(),
        ),
        wire::ActionView::Page { url, title, .. } => (
            "page",
            url.clone(),
            title.clone(),
            String::new(),
            Vec::new(),
            false,
            None,
            None,
            Vec::new(),
        ),
        wire::ActionView::Record { target } => (
            "record",
            target.id.clone(),
            String::new(),
            String::new(),
            Vec::new(),
            false,
            None,
            None,
            Vec::new(),
        ),
    };
    let execution = match &action.view {
        wire::ActionView::Command {
            execution: Some(execution),
            ..
        } => Some(Execution {
            workspace: execution.workspace.clone(),
            app_id: execution.app_id.clone(),
            record: execution.record.clone(),
            execution: execution.execution.clone(),
        }),
        _ => None,
    };
    let (undo_label, undo_verb) = action.undo.as_ref().map_or((None, None), |offer| {
        (Some(offer.label.clone()), Some(action_verb(offer.action)))
    });
    SessionAction {
        range: span(&action.range),
        changes: i64::try_from(action.changes).unwrap_or(i64::MAX),
        at_ms: action.at_ms,
        summary: action.summary.clone(),
        kind: kind.to_owned(),
        place,
        target: Target {
            app_id: action.view.target().app_id.clone(),
            app_label: action.view.target().app_label.clone(),
            id: action.view.target().id.clone(),
            kind: action.view.target().kind.clone(),
            label: action.view.target().label.clone(),
        },
        title,
        cwd,
        output,
        more,
        exit: exit.map(i64::from),
        ended_ms,
        diff,
        app_id: action.app_id.clone(),
        record: action.record.clone(),
        execution,
        undo_label,
        undo_verb,
    }
}

fn span(range: &wire::Range) -> Span {
    Span {
        from: range.from.clone(),
        to: range.to.clone(),
    }
}

fn author(author: &wire::Author) -> Author {
    Author {
        name: author.name.clone(),
        rgb: author.rgb.to_vec(),
        identity: author.identity.clone(),
        relation: serde_json::to_value(author.relation)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default(),
        on_behalf_of: author.on_behalf_of.clone(),
        for_reader: author.for_reader,
        task: author.task.clone(),
    }
}

/// A reading of the activity with nothing in it, as an app starts: the resource reads this until
/// the model serves its first pass.
#[must_use]
pub(crate) fn empty() -> Value {
    serde_json::to_value(Activity::default()).unwrap_or(Value::Null)
}

/// What a page of the feed with nothing read yet shows.
fn empty_feed() -> Value {
    serde_json::to_value(FeedReading::default()).unwrap_or(Value::Null)
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

/// A command's output as an app reads it, from what the model read.
fn output_json(value: &Value) -> Value {
    match serde_json::from_value::<SessionOutput>(value.clone()) {
        Ok(output) => serde_json::to_value(OutputReading {
            output: Some(output),
        })
        .unwrap_or_else(|_| empty_output()),
        Err(error) => {
            tracing::warn!(%error, "a command's output is not the shape an app reads");
            empty_output()
        }
    }
}

/// What a command's output with nothing read yet shows.
fn empty_output() -> Value {
    serde_json::to_value(OutputReading::default()).unwrap_or(Value::Null)
}

/// A page of the feed as an app reads it, from what the model read.
fn feed_json(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(entries) = value.get_mut("entries").and_then(Value::as_array_mut) {
        for entry in entries {
            if let Some(map) = entry.as_object_mut()
                && let Some(app) = map.remove("app")
            {
                map.insert("made_in".to_owned(), app);
            }
        }
    }
    match serde_json::from_value::<Feed>(value) {
        Ok(feed) => {
            serde_json::to_value(FeedReading { feed: Some(feed) }).unwrap_or_else(|_| empty_feed())
        }
        Err(error) => {
            tracing::warn!(%error, "a page of the feed is not the shape an app reads");
            empty_feed()
        }
    }
}

/// What a range with nothing read yet shows.
fn empty_detail() -> Value {
    serde_json::to_value(RangeDetail::default()).unwrap_or(Value::Null)
}

/// The range a keyed reading names: the first and last change, after [`RANGE_PREFIX`].
/// A sessions read as an app gets it: the sessions as the types an app reads, not the wire.
fn sessions_json(value: &Value) -> Value {
    let Ok(read) = serde_json::from_value::<SessionsValue>(value.clone()) else {
        return Key::Sessions.empty();
    };
    serde_json::to_value(read.into_sessions()).unwrap_or_else(|_| Key::Sessions.empty())
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

/// A range's detail as an app reads it, from what the model read.
fn detail_json(value: &Value) -> Value {
    match serde_json::from_value::<wire::Detail>(value.clone()) {
        Ok(detail) => {
            let comparison = match detail.comparison {
                Some(wire::Comparison::Changes { lines }) => Some(lines.join("\n")),
                Some(wire::Comparison::Unavailable) | None => None,
            };
            let read = Detail {
                title: detail.title,
                provenance: detail.provenance,
                authors: detail.authors,
                resources: detail
                    .resources
                    .iter()
                    .map(|target| Target {
                        app_id: target.app_id.clone(),
                        app_label: target.app_label.clone(),
                        id: target.id.clone(),
                        kind: target.kind.clone(),
                        label: target.label.clone(),
                    })
                    .collect(),
                group: detail.group,
                group_label: detail.group_label,
                offers: detail
                    .offers
                    .iter()
                    .map(|offer| Offer {
                        verb: offer.action.action().to_owned(),
                        label: offer.label.clone(),
                    })
                    .collect(),
                changes: detail
                    .changes
                    .iter()
                    .map(|change| FieldChange {
                        field: change.field.clone(),
                        before: change.before.clone(),
                        after: change.after.clone(),
                    })
                    .collect(),
                comparison,
                gone: detail.gone,
            };
            serde_json::to_value(RangeDetail { detail: Some(read) })
                .unwrap_or_else(|_| empty_detail())
        }
        Err(error) => {
            tracing::warn!(%error, "a range's detail is not the shape an app reads");
            empty_detail()
        }
    }
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
        assert_eq!(sessions.sessions[0].actions[0].place, "note-1");
        assert_eq!(sessions.sessions[0].actions[0].kind, "record");

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
        assert_eq!(changes[0]["undo_label"], "Undo this step");
        assert_eq!(changes[0]["undo_verb"], "undo-entry");
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

    /// The surface an app compiles against is the one the session publishes, written beside the
    /// session as `activity-surface.json`.
    #[test]
    fn the_activity_surface_file_is_what_the_session_publishes() {
        let published = vec![surface(), range_surface(), feed_surface(), output_surface()];
        let text = tilo_surface_compile::canonical(&serde_json::json!({ "resources": published }));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("activity-surface.json");
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        if on_disk == text {
            return;
        }
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|v| v == "1") {
            std::fs::write(&path, &text).expect("activity-surface.json is written");
            return;
        }
        panic!(
            "crates/session/session/activity-surface.json is not what the session publishes; \
             write it with UPDATE_SNAPSHOTS=1:\n{text}"
        );
    }
}
