//! A workspace's activity as the session serves it: what changed since this
//! device last looked, the full history, and the replay of its layout.
//!
//! The session keeps the history index of every room on this device and
//! serves it under [`PROVIDER`] as resource [`RESOURCE`]. Its reads are live
//! views ([`crate::ExecutionMode::Live`]): an app follows [`FEED`],
//! [`SUMMARY`], [`REPLAY`] and [`DETAIL`] and gets a new result whenever the
//! room's history moves. Its writes are ordinary actions on the same
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
use crate::door::ActionAddress;

/// The provider the session serves workspace activity under.
pub const PROVIDER: &str = "tilo.activity";
/// The application whose window shows it, and whose grants reach it.
pub const APP_ID: &str = "so.tilo.activity";
/// The resource every action is an action of.
pub const RESOURCE: &str = "workspace.activity";

/// Live: the room's history, filtered by a [`FeedQuery`]; answers [`Feed`].
pub const FEED: &str = "feed";
/// Live: what changed since this device last looked; answers [`Summary`].
pub const SUMMARY: &str = "summary";
/// Live: the room's layout at each recent change; answers [`Replay`].
pub const REPLAY: &str = "replay";
/// Live: what a run of changes did and what can be done with it, given a
/// [`DetailQuery`]; answers [`Detail`].
pub const DETAIL: &str = "detail";
/// Live: what the person last asked to be shown from outside the app, until
/// an app shows it; answers an optional [`Focus`].
pub const FOCUS: &str = "focus";

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
/// Restore the room to before a [`Range`] of changes.
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
    /// The room's own changes, and ones belonging to no single workspace.
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
    pub name: String,
    /// The colour presence paints them in, `[r, g, b]`.
    pub rgb: [u8; 3],
    /// A short fragment of their key, telling apart two of the same name.
    pub identity: String,
    pub relation: Relation,
    /// For software: who it acted for, as a sentence names them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
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

/// The room's history, as a [`FeedQuery`] narrows it.
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

/// The room's layout at each recent change.
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
    /// Also compare the room before and after the run.
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
    /// The record inside that run.
    pub app_id: String,
    pub id: String,
}

/// What comparing the room before and after a run found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Comparison {
    /// What changed, one line each.
    Changes { lines: Vec<String> },
    /// The room's state either side could not be read.
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

#[cfg(test)]
mod tests {
    use super::*;

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
