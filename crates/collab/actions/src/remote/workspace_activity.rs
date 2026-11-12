//! A workspace's activity as the session serves it: what changed since this
//! device last looked, the full history, and the replay of its layout.
//!
//! The session keeps the history index of every workspace on this device and
//! serves it under [`PROVIDER`] as resource [`RESOURCE`]. Its reads are live
//! views ([`crate::ExecutionMode::Live`]): an app follows [`FEED`],
//! [`SUMMARY`], [`REPLAY`] and [`DETAIL`] and gets a new result whenever the
//! workspace's history moves. Its writes are ordinary actions on the same
//! resource: naming, splitting and merging work groups, undoing and
//! restoring, opening what a change touched, and marking what was seen.
//!
//! What this device has seen stays with the session: [`MARK_VIEWED`] moves
//! its own frontier, never a shared record.
//!
//! Changes are addressed by their history id ([`Entry::id`]), 32 lowercase
//! hex digits, and a run of changes by its first and last ([`Range`]) in
//! the order [`Feed::entries`] lists them.

use serde::{Deserialize, Serialize};

use super::{InvokeArgument, RemoteActionTarget};
use crate::ActorKind;
use crate::door::ActionAddress;

/// The provider the session serves workspace activity under.
pub const PROVIDER: &str = "tilo.activity";
/// The application whose window shows it, and whose grants reach it.
pub const APP_ID: &str = "so.tilo.activity";
/// The resource every action is an action of.
pub const RESOURCE: &str = "workspace.activity";

/// Live: the workspace's history, filtered by a [`FeedQuery`]; answers [`Feed`].
pub const FEED: &str = "feed";
/// Live: what changed since this device last looked; answers [`Summary`].
pub const SUMMARY: &str = "summary";
/// Live: the workspace's layout at each recent change; answers [`Replay`].
pub const REPLAY: &str = "replay";
/// Live: what a run of changes did and what can be done with it, given a
/// [`DetailQuery`]; answers [`Detail`].
pub const DETAIL: &str = "detail";
/// Live: what the person last asked to be shown from outside the app, until
/// an app shows it; answers an optional [`Focus`].
pub const FOCUS: &str = "focus";
/// Live: every agent run the history holds, each as the timeline of the
/// actions it took, given a [`RunsQuery`]; answers [`Runs`].
pub const RUNS: &str = "runs";
/// Every line a command an agent ran printed, given the [`CommandRun`] its
/// [`ActionView::Command`] names; answers [`RunOutput`].
pub const RUN_OUTPUT: &str = "run-output";

/// Mark the changes [`Viewed`] names as seen on this device.
pub const MARK_VIEWED: &str = "mark-viewed";
/// Say the [`Focus`] with the serial [`Focused`] names was shown.
pub const FOCUSED: &str = "focused";
/// Name, rename or un-name a run of changes ([`Name`]).
pub const NAME: &str = "name";
/// Split the named work group a [`Range`] sits in at its start.
pub const SPLIT: &str = "split";
/// Merge the named work groups a [`Range`] spans.
pub const MERGE: &str = "merge";
/// Move the boundary between two work sessions ([`Boundary`]).
pub const MOVE_BOUNDARY: &str = "move-boundary";
/// Undo a [`Range`] of changes.
pub const UNDO: &str = "undo";
/// Restore the workspace to before a [`Range`] of changes.
pub const RESTORE: &str = "restore";
/// Undo the agent's task run a [`Range`] belongs to.
pub const UNDO_RUN: &str = "undo-run";
/// Undo one record's share of that run ([`RecordQuery`]).
pub const UNDO_RECORD: &str = "undo-record";
/// Open a [`Target`] in its app.
pub const OPEN: &str = "open";
/// Choose, or stop choosing, the resource a [`Range`] changed as a morning
/// source of this device.
pub const CHOOSE_SOURCE: &str = "choose-source";
/// Run a [`SourceCommand`] on a chosen morning source.
pub const SOURCE: &str = "source";

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// The argument an invocation carrying `value` sends.
#[must_use]
pub fn argument(value: &impl Serialize) -> InvokeArgument {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::Object(map)) => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    }
}

/// Which workspaces' changes are shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// The workspace's own changes, and ones belonging to no single workspace.
    #[default]
    ThisWorkspace,
    /// Every workspace on this device, each change labelled with its own.
    AllWorkspaces,
}

/// How far back the feed reaches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Period {
    #[default]
    AllTime,
    Today,
    Week,
    Month,
}

/// What a [`FEED`] reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeedQuery {
    pub scope: Scope,
    pub period: Period,
    /// Only this person's changes, by the name [`Feed::people`] lists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Only the changes of this lens, by [`Lens::key`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    /// The newest this many changes; `0` for the first page.
    pub limit: usize,
}

/// How many changes a feed answers when its query asks for no number.
pub const FEED_PAGE: usize = 200;

impl FeedQuery {
    /// How many changes the feed answers.
    #[must_use]
    pub const fn page(&self) -> usize {
        if self.limit == 0 {
            FEED_PAGE
        } else {
            self.limit
        }
    }
}

/// Who an author is to the person reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Relation {
    /// The reader themselves.
    Reader,
    /// Another person.
    #[default]
    Person,
    /// Software acting for someone: an agent, a Shortcut, an app.
    Software,
}

/// Who made a change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Author {
    /// The name it goes by: a person's, an agent's own — the reader's account
    /// name too, since the line that names the author words the reader as
    /// "You" from [`Relation`]. A name, never a sentence: a row that says whom
    /// software acted for words that line once, from the facts beside it.
    pub name: String,
    /// The colour presence paints them in, `[r, g, b]`.
    pub rgb: [u8; 3],
    /// A short fragment of their key, telling apart two of the same name.
    pub identity: String,
    pub relation: Relation,
    /// What acted: a person, an agent, a Shortcut, an app or a bot. The row's
    /// face shows it; the line that names the author never words it.
    pub kind: ActorKind,
    /// For software: who it acted for, as a sentence names them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    /// The person it acted for is the reader.
    pub for_reader: bool,
    /// The task that named it, as it was called then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

/// What kind of change an entry is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    #[default]
    Text,
    Move,
    Details,
    Comment,
    Delete,
    Restore,
    Access,
    Compaction,
    Redaction,
}

/// A resource a change touched, and where it opens.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Target {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_label: Option<String>,
    pub id: String,
    pub kind: String,
    pub label: String,
}

/// A device that refused one of a change's writes, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Refusal {
    /// The change it refused, by its history id.
    pub change: String,
    /// The refusing device, by its key.
    pub device: String,
    /// What the person calls that device.
    pub label: String,
    /// Why the write is not there, as the refusing device put it.
    pub reason: String,
    /// Whether waiting will change nothing: the device will not take the
    /// write, whatever the person does short of what the reason names.
    pub permanent: bool,
}

/// One change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub id: String,
    pub author: Author,
    pub kind: Kind,
    /// When, in milliseconds since the Unix epoch.
    pub at_ms: i64,
    pub summary: String,
    /// The workspace it belongs to, under [`Scope::AllWorkspaces`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The app it was made in, as a person names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// The resource it changed, as its own title names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// The named work group it sits in, by [`Group::id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Whether its content was redacted.
    pub redacted: bool,
    /// The devices that refused it, so its author reads where it is not.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<Refusal>,
}

/// A named run of changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Group {
    /// The naming record; a rename addresses it.
    pub id: String,
    pub label: String,
    /// Its oldest change.
    pub start: String,
    /// Its newest change; `None` while it is still open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// Who named it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub named_by: Option<String>,
}

/// A way to narrow the feed: an app, a resource, a work group.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lens {
    /// What [`FeedQuery::lens`] names it by.
    pub key: String,
    pub label: String,
}

/// The workspace's history, as a [`FeedQuery`] narrows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Feed {
    /// The workspace's name.
    pub workspace: String,
    pub scope: Scope,
    /// Changes the query matches, of which [`Self::entries`] holds the
    /// newest.
    pub total: usize,
    /// Newest first.
    pub entries: Vec<Entry>,
    pub groups: Vec<Group>,
    /// Everyone who changed something, in the order they first did.
    pub people: Vec<String>,
    pub lenses: Vec<Lens>,
    /// The history is still being read.
    pub pending: bool,
    /// Whether this device can change the history here: name, undo,
    /// restore.
    pub mutations: bool,
}

/// Where in the app a [`Focus`] opens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum View {
    /// What changed since this device last looked.
    #[default]
    WhatsNew,
    /// Every change, filtered and selected as the focus says.
    History,
    /// The workspace played back change by change.
    Replay,
}

/// What to show, asked for outside the app: the glance in the top bar, a
/// file's own history, a command naming a view. Each ask has its own `serial`; the app that shows it
/// says so ([`FOCUSED`]), and it is not offered again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Focus {
    pub serial: u64,
    pub view: View,
    /// Only this person's changes, as [`FeedQuery::author`] names them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Only this lens's changes, as [`FeedQuery::lens`] names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    /// This change, selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

/// One action of a work session's evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BriefAction {
    pub author: Author,
    pub summary: String,
    pub at_ms: i64,
}

/// A work session: a run of related changes, told as an outcome.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Brief {
    pub title: String,
    /// Who and how much, in the history's own words.
    pub detail: String,
    pub authors: Vec<Author>,
    pub apps: Vec<String>,
    pub resources: Vec<String>,
    pub changes: usize,
    /// Oldest first.
    pub actions: Vec<BriefAction>,
    pub first_ms: i64,
    pub last_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The record of a person-corrected session; only such a session's
    /// boundary moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_session: Option<String>,
    /// The changes it tells of.
    pub entries: Vec<String>,
}

/// Where the reader left off.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Resume {
    pub target: Target,
    pub author: Author,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    pub at_ms: i64,
}

/// A resource this device chose to hear about each morning.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    /// What [`SourceRequest::key`] names it by.
    pub key: String,
    pub label: String,
    /// What its app says about it now; empty when it says nothing.
    pub text: String,
    /// Whether its app refreshes it on request.
    pub refreshable: bool,
    /// How the last command on it went.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// What changed since this device last looked.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub workspace: String,
    /// Newest first.
    pub briefs: Vec<Brief>,
    /// The briefs told in a sentence by a model on this device.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narrative: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume: Option<Resume>,
    /// Whether these are the changes made while the reader was away.
    pub away: bool,
    /// Whether morning sources can be chosen here.
    pub sources_available: bool,
    pub sources: Vec<Source>,
    /// The history is still being read.
    pub pending: bool,
}

/// One group of tabs in a replayed workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayGroup {
    pub tabs: Vec<String>,
    /// The tab it showed, by index into [`Self::tabs`].
    pub active: usize,
    /// Its share of the strip, in thousandths.
    pub width_permille: u32,
}

/// A workspace as it was.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayWorkspace {
    pub name: String,
    pub groups: Vec<ReplayGroup>,
}

/// Someone's pointer as it was.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayPointer {
    pub name: String,
    pub rgb: [u8; 3],
}

/// The layout after one change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Frame {
    pub entry: String,
    pub author: Author,
    pub summary: String,
    pub at_ms: i64,
    pub workspaces: Vec<ReplayWorkspace>,
    pub pointers: Vec<ReplayPointer>,
}

/// The workspace's layout at each recent change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Replay {
    /// Oldest first.
    pub frames: Vec<Frame>,
    /// Changes whose layout could not be read back.
    pub unavailable: usize,
    /// What the replay cannot show, said once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The history is still being read.
    pub pending: bool,
}

/// A run of changes, by its first and last in feed order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub from: String,
    pub to: String,
}

impl Range {
    /// The run of the one change `id`.
    #[must_use]
    pub fn one(id: &str) -> Self {
        Self {
            from: id.to_owned(),
            to: id.to_owned(),
        }
    }
}

/// What a [`DETAIL`] reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DetailQuery {
    pub range: Range,
    /// Also compare the workspace before and after the run.
    pub compare: bool,
}

/// What can be done with a run of changes, and which action does it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DetailAction {
    Open,
    Compare,
    Restore,
    Undo,
    UndoRun,
    Split,
    Merge,
    ChooseSource,
    UndoRecord,
}

impl DetailAction {
    /// The action of [`RESOURCE`] that does it; [`DetailAction::Compare`]
    /// is a [`DetailQuery`] of its own.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Open => OPEN,
            Self::Compare => DETAIL,
            Self::Restore => RESTORE,
            Self::Undo => UNDO,
            Self::UndoRun => UNDO_RUN,
            Self::UndoRecord => UNDO_RECORD,
            Self::Split => SPLIT,
            Self::Merge => MERGE,
            Self::ChooseSource => CHOOSE_SOURCE,
        }
    }
}

/// An offered action, worded for the run it acts on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub action: DetailAction,
    pub label: String,
}

/// An agent's task run a change belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    /// The instruction, as the person phrased it.
    pub instruction: String,
    /// Who ran it and who asked, as a sentence.
    pub byline: String,
    /// Why the agent did what it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// The records it changed, each with what it did there.
    pub records: Vec<RunRecord>,
    /// Why it cannot be undone from here, when it cannot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo_blocked: Option<String>,
    /// How its undo went, while one runs or once it ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo_status: Option<String>,
    /// The records its undo did not reach, one sentence each: reported,
    /// never left out of what the person is told happened.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unreverted: Vec<String>,
}

/// A record an agent's run changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunRecord {
    /// The record, as its title names it.
    pub record: String,
    /// The app it belongs to.
    pub app: String,
    /// The record's own id, which [`RecordQuery`] names to undo just it.
    pub id: String,
    /// The app's id, which [`RecordQuery`] names to undo just it.
    pub app_id: String,
    /// What the run did to it, as a sentence.
    pub change: String,
    /// The action that undoes this record's share of the run, when the
    /// document is open here; absent when it cannot be undone on its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo: Option<Offer>,
}

/// One record of an agent's run, as [`UNDO_RECORD`] names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordQuery {
    /// The run, by its range in the history.
    pub range: Range,
    /// The run, by the task it worked (`task:<id>`, or a Tasks issue's address), in place
    /// of [`Self::range`]: what a view that follows the task rather than the history (the agent
    /// composer) names it by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The record inside that run.
    pub app_id: String,
    pub id: String,
}

/// What comparing the workspace before and after a run found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Comparison {
    /// What changed, one line each.
    Changes { lines: Vec<String> },
    /// The workspace's state either side could not be read.
    Unavailable,
}

/// One field a run of changes changed, with what it held either side of it.
///
/// [`Comparison::Changes`] says a field changed; this says what it changed
/// from and to, which is the question "compare before and after" is asked
/// for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FieldChange {
    /// The field's name as the person reads it ("Title").
    pub field: String,
    /// What it held before; empty when it was not there.
    pub before: String,
    /// What it holds after; empty when it was removed.
    pub after: String,
}

/// What a run of changes did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detail {
    pub title: String,
    /// Who, where and why, in the history's own words.
    pub provenance: String,
    pub authors: Vec<String>,
    pub resources: Vec<Target>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_label: Option<String>,
    pub offers: Vec<Offer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<Run>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison: Option<Comparison>,
    /// The fields the run changed, with what they held either side: empty
    /// when the host could not read both states.
    pub changes: Vec<FieldChange>,
    /// The run is no longer in the history (an undo or a filter moved it).
    pub gone: bool,
}

/// What a [`RUNS`] reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunsQuery {
    pub scope: Scope,
    /// The newest this many runs; `0` for the first page.
    pub limit: usize,
}

/// How many runs a [`RUNS`] read answers when its query asks for no number.
pub const RUNS_PAGE: usize = 50;

impl RunsQuery {
    /// How many runs the read answers.
    #[must_use]
    pub const fn page(&self) -> usize {
        if self.limit == 0 {
            RUNS_PAGE
        } else {
            self.limit
        }
    }
}

/// What one action of a run shows when it is opened: every action an agent
/// takes has a view of its own, so none of them is opaque.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ActionView {
    /// A file it changed: its path, and the lines either side when the host
    /// read them. Without lines the view reads them with a [`DETAIL`]
    /// comparison of the action's range.
    File {
        target: Target,
        path: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lines: Vec<DiffLine>,
    },
    /// A command line it ran in a shell, with what came of it: the shell
    /// session's own record of the run ([`CommandRun`]), never a copy.
    Command {
        target: Target,
        /// The command line, as the shell ran it.
        command: String,
        /// The directory it ran in.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        cwd: String,
        /// The last lines it printed, without terminal escapes.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        output: Vec<String>,
        /// It printed more than `output` holds; the rest is
        /// read with [`RUN_OUTPUT`].
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        more: bool,
        /// How the program ended; `None` while it runs or when unknown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_ms: Option<i64>,
        /// When it ended; `None` while it runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended_ms: Option<i64>,
        /// The run record, once the shell asked to run it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<CommandRun>,
    },
    /// A web page it opened or acted on.
    Page {
        target: Target,
        url: String,
        /// The page's title, when it had one.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        title: String,
    },
    /// A record of an app it changed.
    Record { target: Target },
}

impl ActionView {
    /// Whether it is a command its shell has not finished running.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        matches!(
            self,
            Self::Command {
                ended_ms: None,
                run: Some(_),
                ..
            }
        )
    }

    /// The resource the action touched, where it opens.
    #[must_use]
    pub const fn target(&self) -> &Target {
        match self {
            Self::File { target, .. }
            | Self::Command { target, .. }
            | Self::Page { target, .. }
            | Self::Record { target } => target,
        }
    }
}

/// One line of a file's change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub side: DiffSide,
    pub text: String,
}

/// Which side of a change a [`DiffLine`] is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffSide {
    /// Unchanged, shown for its context.
    Both,
    Removed,
    Added,
}

/// One action of an agent's run: what it did in one record, and the view
/// that shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunAction {
    /// Its changes, which [`UNDO`] takes back on their own.
    pub range: Range,
    /// How many changes `range` is: an action that wrote four paragraphs in one note is one
    /// action of four changes.
    pub changes: usize,
    pub at_ms: i64,
    /// What it did, as a sentence.
    pub summary: String,
    pub view: ActionView,
    /// The app and record it changed, as [`RecordQuery`] names them to undo
    /// just this action.
    pub app_id: String,
    pub record: String,
    /// The action that undoes this record's share of the run alone, when
    /// it can be undone on its own here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<Offer>,
}

/// One agent's run: an instruction it worked, and the actions it took for
/// it, oldest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRun {
    /// The run's record, as the history keeps it.
    pub id: String,
    /// The agent, and who it acted for.
    pub agent: Author,
    /// The workspace it worked in, under [`Scope::AllWorkspaces`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// What it was asked, as the person phrased it.
    pub instruction: String,
    /// Why it did what it did, when it said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    pub started_ms: i64,
    /// When it last acted.
    pub last_ms: i64,
    /// Every change it made, which [`UNDO_RUN`] takes back.
    pub range: Range,
    pub actions: Vec<RunAction>,
    /// Why it cannot be undone from here, when it cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_blocked: Option<String>,
    /// What it is doing now, or did last: a command still running, else its
    /// newest action. A reader shows it only for a run that is working.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<Now>,
}

impl AgentRun {
    /// How many changes the run made to records and files, as a person counts what it changed:
    /// each of its edits, not the commands it ran or the pages it read. What its card and the
    /// notice that it finished both say.
    #[must_use]
    pub fn changes(&self) -> usize {
        self.actions
            .iter()
            .filter(|action| {
                matches!(
                    action.view,
                    ActionView::File { .. } | ActionView::Record { .. }
                )
            })
            .map(|action| action.changes.max(1))
            .sum()
    }
}

/// What a run is doing at the moment it is read.
///
/// Written as derived; read through one `serde_json::Value` ([`Now::read`]),
/// which takes the same JSON. A derived reader of an internally tagged enum
/// buffers the value and is compiled again for every reader it meets: a
/// large share of the code a page loads first, for a handful of fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Now {
    /// A command line it asked a shell to run, which has not ended.
    Running { command: String, since_ms: i64 },
    /// A file it changed last.
    Editing { path: String, at_ms: i64 },
    /// A web page it opened or acted on last.
    Browsing {
        url: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        title: String,
        at_ms: i64,
    },
    /// A record of an app it changed last, as its action says it.
    Changing { summary: String, at_ms: i64 },
    /// A command it ran last, which ended.
    Ran { command: String, at_ms: i64 },
}

impl<'de> Deserialize<'de> for Now {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <serde_json::Value as Deserialize>::deserialize(deserializer)?;
        Self::read(&value).map_err(serde::de::Error::custom)
    }
}

impl Now {
    /// The moment `value` writes: `{"kind": .., ..fields}`.
    #[inline(never)]
    fn read(value: &serde_json::Value) -> Result<Self, String> {
        let field = |name: &str| value.get(name);
        let text = |name: &str| {
            field(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("missing field `{name}`"))
        };
        let millis = |name: &str| {
            field(name)
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| format!("missing field `{name}`"))
        };
        let kind = field("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or("missing field `kind`")?;
        Ok(match kind {
            "running" => Self::Running {
                command: text("command")?,
                since_ms: millis("since_ms")?,
            },
            "editing" => Self::Editing {
                path: text("path")?,
                at_ms: millis("at_ms")?,
            },
            "browsing" => Self::Browsing {
                url: text("url")?,
                title: if field("title").is_some() {
                    text("title")?
                } else {
                    String::new()
                },
                at_ms: millis("at_ms")?,
            },
            "changing" => Self::Changing {
                summary: text("summary")?,
                at_ms: millis("at_ms")?,
            },
            "ran" => Self::Ran {
                command: text("command")?,
                at_ms: millis("at_ms")?,
            },
            other => return Err(format!("unknown variant `{other}`")),
        })
    }
}

/// Where a command's run is recorded: the shell session's record and the run
/// in it. Terminal keeps every run a session ran there, with its command,
/// directory, exit status, times and output.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandRun {
    /// The workspace the session's record is in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The app that keeps the session's record.
    pub app_id: String,
    /// The session's record.
    pub record: String,
    /// The run, an item of the session's runs.
    pub run: String,
}

/// Everything a run printed, as [`RUN_OUTPUT`] reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunOutput {
    /// Its lines, without terminal escapes, at most [`RUN_OUTPUT_LINES`].
    pub lines: Vec<String>,
    /// It printed more than the lines hold, or more than its shell kept.
    pub truncated: bool,
    /// The run is not known here, or its output has not reached this device.
    pub unavailable: bool,
    /// The run has not ended.
    pub running: bool,
}

/// The most lines a [`RUN_OUTPUT`] read answers: its newest.
pub const RUN_OUTPUT_LINES: usize = 10_000;
/// The most lines an [`ActionView::Command`] carries.
pub const COMMAND_TAIL_LINES: usize = 12;

/// Every agent run a [`RunsQuery`] reads, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Runs {
    pub runs: Vec<AgentRun>,
    /// Runs the query matches, of which [`Self::runs`] holds the newest.
    pub total: usize,
    /// The history is still being read.
    pub pending: bool,
    /// Whether this device can undo here.
    pub mutations: bool,
}

/// Which [`Focus`] [`FOCUSED`] says was shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Focused {
    pub serial: u64,
}

/// What [`MARK_VIEWED`] marks: the changes the reader was shown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Viewed {
    pub entries: Vec<String>,
    /// Count them seen once the reader stops following [`SUMMARY`], rather
    /// than now: the summary they are reading stays as it is until then.
    pub later: bool,
}

/// What [`NAME`] writes. An empty label removes the name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Name {
    pub range: Range,
    /// The naming record a rename replaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    pub label: String,
}

/// Which way a [`Boundary`] moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Toward {
    /// The older session's newest change joins the newer session.
    #[default]
    Newer,
    /// The newer session's oldest change joins the older session.
    Older,
}

/// What [`MOVE_BOUNDARY`] moves: the boundary between the two adjacent
/// work sessions holding `older` and `newer`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Boundary {
    /// A change of the older session.
    pub older: String,
    /// A change of the newer session.
    pub newer: String,
    pub toward: Toward,
}

/// A command on a chosen morning source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceCommand {
    #[default]
    Open,
    Remove,
    Refresh,
    DraftRefresh,
    NextSource,
}

/// What [`SOURCE`] runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourceRequest {
    pub key: String,
    pub command: SourceCommand,
}

/// A change's history id from its [`Entry::id`].
#[must_use]
pub fn entry_id(hex: &str) -> Option<[u8; 16]> {
    let mut bytes = [0_u8; 16];
    hex::decode_to_slice(hex, &mut bytes).ok()?;
    Some(bytes)
}

/// The [`Entry::id`] of history id `bytes`.
#[must_use]
pub fn entry_hex(bytes: &[u8; 16]) -> String {
    hex::encode(bytes)
}

/// The history id of a change, as [`Entry::id`] spells it.
#[must_use]
pub fn change_hex(change: &[u8; 32]) -> String {
    hex::encode(&change[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_moment_reads_back_as_written_and_a_bare_page_has_no_title() {
        let moments = [
            Now::Running {
                command: "cargo test".into(),
                since_ms: -3,
            },
            Now::Editing {
                path: "src/lib.rs".into(),
                at_ms: 4,
            },
            Now::Browsing {
                url: "https://example.org".into(),
                title: "Example".into(),
                at_ms: 5,
            },
            Now::Browsing {
                url: "https://example.org".into(),
                title: String::new(),
                at_ms: 6,
            },
            Now::Changing {
                summary: "Renamed a note".into(),
                at_ms: 7,
            },
            Now::Ran {
                command: "ls".into(),
                at_ms: 8,
            },
        ];
        for now in moments {
            let written = serde_json::to_string(&now).unwrap();
            assert_eq!(
                serde_json::from_str::<Now>(&written).unwrap(),
                now,
                "{written}"
            );
            let value = serde_json::to_value(&now).unwrap();
            assert_eq!(serde_json::from_value::<Now>(value).unwrap(), now);
        }
        assert!(serde_json::from_str::<Now>(r#"{"kind":"sleeping","at_ms":1}"#).is_err());
        assert!(serde_json::from_str::<Now>(r#"{"kind":"ran","at_ms":1}"#).is_err());
        assert_eq!(
            serde_json::from_str::<Now>(r#"{"kind":"ran","command":"ls","at_ms":1,"more":0}"#)
                .unwrap(),
            Now::Ran {
                command: "ls".into(),
                at_ms: 1
            },
            "a field a newer writer adds is passed over"
        );
    }

    #[test]
    fn an_entry_id_reads_back_from_its_hex_and_nothing_else_does() {
        let id = [7_u8; 16];
        assert_eq!(entry_id(&entry_hex(&id)), Some(id));
        assert_eq!(entry_id("07"), None);
        assert_eq!(entry_id(&"zz".repeat(16)), None);
    }

    #[test]
    fn a_query_reads_back_and_an_empty_one_asks_for_the_first_page() {
        let query = FeedQuery {
            scope: Scope::AllWorkspaces,
            period: Period::Week,
            author: Some("Ada".into()),
            lens: Some("app:so.tilo.notes".into()),
            limit: 400,
        };
        let InvokeArgument::Json(map) = argument(&query) else {
            panic!("a query is an object");
        };
        let read: FeedQuery = serde_json::from_value(serde_json::Value::Object(map)).unwrap();
        assert_eq!(read, query);
        assert_eq!(FeedQuery::default().page(), FEED_PAGE);
    }
}
