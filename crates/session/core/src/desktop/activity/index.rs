//! Workspace-wide history aggregation.
//!
//! A workspace is not one Automerge document: the layout tree describes its
//! windows and governance, while every open record owns a separate change
//! graph.  This module is the join.  It deliberately retains the originating
//! document beside each change; a bare [`HistoryEntryId`] is only unique
//! inside one Automerge graph and is therefore not safe as a workspace key.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tilo_sort::CompactSort as _;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ReadDoc as _};
use collab_types::ArtifactKind;
use ui::history_source::{AuthorDirectory, HistoryLog, HistorySource};
use ui::resource::{DocumentCheckout, DocumentId, ResourceType};
use ui::time::SystemTime;
use ui::widget::{HistoryEntry, HistoryEntryId};

use super::ActivityTree;
use tilo_desktop_model::WorkspaceId;

/// Hard ceilings keep a corrupt index or a workspace with thousands of references from turning
/// opening History into an unbounded allocation.
pub const MAX_FEED_ENTRIES: usize = 2_000;
pub const MAX_INDEXED_RECORDS: usize = 512;

/// Which collaborative document produced a history row.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HistoryOrigin {
    Layout,
    AppRecord {
        app: String,
        id: DocumentId,
        kind: ResourceType,
    },
}

impl HistoryOrigin {
    pub fn key(&self) -> String {
        match self {
            Self::Layout => "workspace-tree".to_string(),
            // A tuple of strings always encodes; the empty fallback is unreachable.
            Self::AppRecord { app, id, kind } => format!(
                "app-record:{}",
                serde_json::to_string(&(app, id.as_str(), kind.as_str())).unwrap_or_default()
            ),
        }
    }

    /// The record this row came from, if it is not the workspace document.
    pub fn record(&self) -> Option<&DocumentId> {
        match self {
            Self::Layout => None,
            Self::AppRecord { id, .. } => Some(id),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Layout => crate::desktop::i18n::t("activity-source-workspace"),
            Self::AppRecord { app, id, .. } => format!("{app}: {id}"),
        }
    }
}

/// One current-admission record, captured through a read-only owner borrow. Missing and unsettled
/// owners carry no document; clones must be settled.
#[derive(Clone)]
pub struct DetachedRecordHistory {
    pub app: String,
    pub checkout: DocumentCheckout,
    pub document: Option<AutoCommit>,
    /// A workspace tree's chunk records, each read on its own beside the tree's own record in
    /// `document`; empty for every other record.
    pub tree_chunks: Vec<(fs_protocol::files::TreeChunk, AutoCommit)>,
}
impl DetachedRecordHistory {
    /// A record read as one document: every record but a workspace's tree.
    pub fn new(
        app: impl Into<String>,
        checkout: DocumentCheckout,
        document: Option<AutoCommit>,
    ) -> Self {
        Self {
            app: app.into(),
            checkout,
            document,
            tree_chunks: Vec::new(),
        }
    }

    pub fn origin(&self) -> HistoryOrigin {
        HistoryOrigin::AppRecord {
            app: self.app.clone(),
            id: self.checkout.id.clone(),
            kind: self.checkout.kind.clone(),
        }
    }

    /// Whether `entry` is one of this record's steps.
    ///
    /// One document may hold several apps' records: the workspace record the session keeps holds
    /// a root per package it runs, and each app's record is admitted as one of its own, all over
    /// that same document. Such a record's kind (`tl.<app>.<Type>`) says whose steps are its own;
    /// every other record is a document of one app's, and every step in it is that record's.
    fn holds(&self, entry: &HistoryEntry) -> bool {
        let Some(app) = self
            .checkout
            .kind
            .as_str()
            .strip_prefix("tl.")
            .and_then(|kind| kind.rsplit_once('.'))
            .map(|(app, _)| app)
        else {
            return true;
        };
        let of_app = format!("tl.{app}.");
        entry
            .context
            .as_ref()
            .and_then(|context| context.resource.as_ref())
            .is_some_and(|resource| resource.kind.starts_with(&of_app))
    }
}

/// A globally unambiguous change address.  Both fields are required: two
/// documents may legitimately contain the same native change hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceHistoryId {
    pub origin: HistoryOrigin,
    pub change: HistoryEntryId,
}

impl Ord for WorkspaceHistoryId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.origin
            .cmp(&other.origin)
            .then_with(|| self.change.as_bytes().cmp(other.change.as_bytes()))
    }
}

impl PartialOrd for WorkspaceHistoryId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A history row plus enough provenance to open, diff, or undo it against the correct document
/// later.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceHistoryEntry {
    pub id: WorkspaceHistoryId,
    pub entry: HistoryEntry,
}

/// Private read state for one collaborative document. Full Automerge heads define the causal past;
/// exceptions acknowledge individual changes without claiming that their concurrent siblings were
/// read.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct DocumentSeenFrontier {
    pub heads: Vec<String>,
    pub exceptions: BTreeSet<String>,
}

/// When this device left a workspace and the causal frontier it had seen then. Private to the
/// device; the host keeps it with the seen frontiers.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct WorkspaceDeparture {
    pub departed_at_secs: u64,
    pub frontier: WorkspaceSeenFrontier,
}

/// Persistable causal read state, partitioned by full document origin.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct WorkspaceSeenFrontier {
    pub documents: BTreeMap<String, DocumentSeenFrontier>,
}

impl WorkspaceSeenFrontier {
    /// Advance origins the current index exposed, retaining absent origins and previously
    /// acknowledged exceptions outside a bounded projection. A workspace-scoped tree is only a
    /// projection of the global tree document, so its visible changes are exceptions rather than an
    /// unsafe global head.
    pub fn mark_seen(&mut self, index: &WorkspaceHistoryIndex) {
        let workspace_scoped = index.query.as_ref().is_some_and(|query| {
            query.workspace.is_some()
                || query
                    .workspaces
                    .as_ref()
                    .is_some_and(|workspaces| workspaces.len() == 1)
        });
        for (origin, heads) in &index.document_heads {
            let state = self.documents.entry(origin.clone()).or_default();
            if !(workspace_scoped && origin == "workspace-tree") {
                state.heads = heads.iter().map(ToString::to_string).collect();
            }
        }
        if workspace_scoped {
            for row in &index.entries {
                if row.id.origin == HistoryOrigin::Layout {
                    self.acknowledge(&row.id);
                }
            }
        }
    }

    pub fn acknowledge(&mut self, id: &WorkspaceHistoryId) {
        self.documents
            .entry(id.origin.key())
            .or_default()
            .exceptions
            .insert(ui::widget::history_group::hex16(&id.change));
    }

    pub fn acknowledged(&self, id: &WorkspaceHistoryId) -> bool {
        self.documents.get(&id.origin.key()).is_some_and(|state| {
            state
                .exceptions
                .contains(&ui::widget::history_group::hex16(&id.change))
        })
    }
}

impl WorkspaceHistoryEntry {
    /// A collision-resistant projection for widgets whose callback API still
    /// accepts only a 16-byte id.  Authoritative operations must retain `id`.
    pub fn timeline_entry(&self) -> HistoryEntry {
        let mut projected = self.entry.clone();
        // Collaborative named ranges live in the workspace tree and address its native ids.
        // Preserve those ids so adding record rows cannot detach an existing name from its band.
        // Only foreign document ids need projection into the shared widget namespace.
        if self.id.origin == HistoryOrigin::Layout {
            return projected;
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.id.origin.key().as_bytes());
        hasher.update(self.id.change.as_bytes());
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        projected.id = HistoryEntryId::from_bytes(bytes);
        projected
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceHistoryQuery {
    /// `Some` includes only records held by, or originally minted in, this workspace. `None` defers
    /// scope to `workspaces` .
    pub workspace: Option<WorkspaceId>,
    /// When set, restrict a multi-workspace query to this exact union instead of treating
    /// `workspace: None` as unrestricted device history.
    pub workspaces: Option<Vec<WorkspaceId>>,
    pub limit: usize,
}

impl Default for WorkspaceHistoryQuery {
    fn default() -> Self {
        Self {
            workspace: None,
            workspaces: None,
            limit: 500,
        }
    }
}

impl WorkspaceHistoryQuery {
    pub fn for_room(workspace: WorkspaceId) -> Self {
        Self {
            workspace: Some(workspace),
            ..Self::default()
        }
    }

    fn bounded_limit(&self) -> usize {
        self.limit.clamp(1, MAX_FEED_ENTRIES)
    }
}

/// Explains whether the feed is complete on this device.  A referenced record
/// can be known to the workspace before its Automerge document has been opened or
/// downloaded; silently omitting it would make the history falsely reassuring.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedCoverage {
    pub referenced_records: usize,
    pub indexed_resources: usize,
    pub unavailable_resources: Vec<DocumentCheckout>,
    pub capped_records: usize,
    pub excluded_apps: Vec<String>,
}

impl FeedCoverage {
    pub fn is_complete(&self) -> bool {
        self.unavailable_resources.is_empty()
            && self.capped_records == 0
            && self.excluded_apps.is_empty()
    }
}

/// Cached, deterministic reduction of the workspace's document graphs.
pub struct WorkspaceHistoryIndex {
    authors: AuthorDirectory,
    entries: Vec<WorkspaceHistoryEntry>,
    coverage: FeedCoverage,
    query: Option<WorkspaceHistoryQuery>,
    document_heads: BTreeMap<String, Vec<ChangeHash>>,
    /// What each record's history read of its document, by origin: a refresh reads only the
    /// changes made since, however large the record grew.
    logs: HashMap<String, HistoryLog>,
}

impl Default for WorkspaceHistoryIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// The changes a folder record's document holds: one row per file and session,
/// coalesced as Files' own history coalesces them, attributed to the person
/// whose device signed the version and the agent that wrote for them when one
/// did.
fn folder_rows(
    doc: &AutoCommit,
    chunks: &[(fs_protocol::files::TreeChunk, AutoCommit)],
    authors: &AuthorDirectory,
    limit: usize,
) -> Vec<HistoryEntry> {
    let rows = if chunks.is_empty() {
        ui::history_source::folder::entries_in(doc, authors)
    } else {
        ui::history_source::folder::entries_in_chunks(doc, chunks, authors)
    };
    rows.into_iter()
        .rev()
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// The rows of `doc` , each saying what a model described it as when one did: a description stands
/// in place of the change's own summary, never beside it, and never replaces its author, time or
/// kind.
fn described(mut rows: Vec<HistoryEntry>, doc: &AutoCommit) -> Vec<HistoryEntry> {
    crate::desktop::activity::describer::shown(&mut rows, doc);
    rows
}

impl WorkspaceHistoryIndex {
    pub fn new() -> Self {
        Self {
            authors: AuthorDirectory::new(),
            entries: Vec::new(),
            coverage: FeedCoverage::default(),
            query: None,
            document_heads: BTreeMap::new(),
            logs: HashMap::new(),
        }
    }

    pub fn authors_mut(&mut self) -> &mut AuthorDirectory {
        &mut self.authors
    }

    pub fn set_authors(&mut self, authors: AuthorDirectory) {
        self.authors = authors;
    }

    pub fn entries(&self) -> &[WorkspaceHistoryEntry] {
        &self.entries
    }

    pub fn coverage(&self) -> &FeedCoverage {
        &self.coverage
    }

    /// How many changes of the records' documents this index has read, across every refresh: a
    /// refresh reads the changes made since the last one, however long a record's history is.
    pub fn changes_read(&self) -> usize {
        self.logs.values().map(HistoryLog::changes_read).sum()
    }

    /// Retain only names whose whole positional span belongs to one document. Until named-range
    /// storage carries an explicit multi-origin membership list, dropping an ambiguous band is
    /// safer than visually absorbing and potentially undoing changes from another record.
    pub fn origin_safe_named_ranges(&self, ranges: &[ui::NamedRange]) -> Vec<ui::NamedRange> {
        let projected: Vec<(HistoryEntryId, &HistoryOrigin)> = self
            .entries
            .iter()
            .map(|row| (row.timeline_entry().id, &row.id.origin))
            .collect();
        ranges
            .iter()
            .filter(|range| {
                let Some(start) = projected.iter().position(|(id, _)| *id == range.start) else {
                    return false;
                };
                let end = match range.end {
                    Some(id) => projected
                        .iter()
                        .position(|(candidate, _)| *candidate == id)
                        .map(|index| index.saturating_add(1)),
                    None => Some(projected.len()),
                };
                let Some(end) = end else {
                    return false;
                };
                if end <= start {
                    return false;
                }
                let Some(span) = projected.get(start..end) else {
                    return false;
                };
                let Some((_, origin)) = span.first() else {
                    return false;
                };
                span.iter().all(|(_, candidate)| candidate == origin)
            })
            .cloned()
            .collect()
    }

    /// Replaces the entire admitted multi-owner input atomically. Never borrows a live store
    /// mutably or closes a producer transaction: a record whose document has an edit pending is
    /// left as it was.
    pub fn refresh_detached(
        &mut self,
        tree: &mut impl ActivityTree,
        query: WorkspaceHistoryQuery,
        tree_rows: &[HistoryEntry],
        records: &mut [DetachedRecordHistory],
    ) -> bool {
        let limit = query.bounded_limit();
        let mut by_id = BTreeMap::new();
        let mut coverage = FeedCoverage {
            referenced_records: records.len(),
            ..Default::default()
        };
        let mut documents = BTreeMap::new();
        insert_rows(&mut by_id, &HistoryOrigin::Layout, tree_rows.to_vec());
        documents.insert("workspace-tree".into(), tree.document_mut().get_heads());
        let mut ordered = records.iter_mut().collect::<Vec<_>>();
        ordered.compact_sort_by_key(|record| record.origin());
        let mut logs = HashMap::with_capacity(ordered.len().min(MAX_INDEXED_RECORDS));
        for (i, record) in ordered.into_iter().enumerate() {
            if i >= MAX_INDEXED_RECORDS {
                coverage.capped_records = coverage.capped_records.saturating_add(1);
                continue;
            }
            let origin = record.origin();
            let doc = match record.document.as_mut() {
                Some(doc) if doc.pending_ops() == 0 => doc,
                _ => {
                    coverage.unavailable_resources.push(record.checkout.clone());
                    // Keep the last settled rendering while this admitted owner is busy, but never
                    // carry rows from an owner absent from the new admission. Unavailable
                    // documents cannot establish seen heads.
                    for row in self.entries.iter().filter(|row| row.id.origin == origin) {
                        let place = Placed {
                            time: row.entry.timestamp,
                            seq: by_id.len(),
                        };
                        by_id.insert(row.id.clone(), (place, row.clone()));
                    }
                    if let Some(log) = self.logs.remove(&origin.key()) {
                        logs.insert(origin.key(), log);
                    }
                    continue;
                }
            };
            let key = origin.key();
            documents.insert(key.clone(), doc.get_heads());
            let mut log = self.logs.remove(&key).unwrap_or_default();
            // A folder's files are not actions: a record's own steps say
            // nothing of them. Their changes are the versions it holds, one
            // row per file and session, as Files' own history lists them.
            let folder = if record.checkout.kind.as_str() == fs_protocol::tree::TREE_KIND {
                folder_rows(doc, &record.tree_chunks, &self.authors, limit)
            } else {
                Vec::new()
            };
            // What people did: a record's genesis names no one.
            let rows = described(
                HistorySource::new()
                    .with_authors(self.authors.clone())
                    .with_artifact_kind(artifact_kind(&record.checkout.kind))
                    .with_max_entries(limit)
                    .without_resource_genesis()
                    .entries_since(doc, &mut log),
                doc,
            );
            logs.insert(key, log);
            let rows = rows
                .into_iter()
                .filter(|entry| record.holds(entry))
                .collect::<Vec<_>>();
            insert_rows(&mut by_id, &origin, rows);
            insert_rows(&mut by_id, &origin, folder);
            coverage.indexed_resources = coverage.indexed_resources.saturating_add(1);
        }
        self.logs = logs;
        // Each document's changes in its own causal order, merged by when they were made: a
        // change's clock is whole seconds and may run behind what it builds on, so the clock only
        // orders documents against each other ([`insert_rows`]).
        let mut placed = by_id.into_values().collect::<Vec<_>>();
        placed.compact_sort_by_key(|(place, _)| *place);
        let mut entries = placed
            .into_iter()
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>();
        if entries.len() > limit {
            entries.drain(..entries.len().saturating_sub(limit));
        }
        let changed = self.entries != entries
            || self.coverage != coverage
            || self.document_heads != documents
            || self.query.as_ref() != Some(&query);
        self.entries = entries;
        self.coverage = coverage;
        self.document_heads = documents;
        self.query = Some(query);
        changed
    }

    pub fn unseen_detached<'a>(
        &'a self,
        frontier: &WorkspaceSeenFrontier,
        tree: &mut impl ActivityTree,
        records: &[DetachedRecordHistory],
    ) -> Vec<&'a WorkspaceHistoryEntry> {
        let mut after = BTreeMap::<String, HashSet<HistoryEntryId>>::new();
        for (key, state) in &frontier.documents {
            let heads = state
                .heads
                .iter()
                .map(|h| h.parse::<ChangeHash>())
                .collect::<Result<Vec<_>, _>>();
            let Ok(heads) = heads else {
                continue;
            };
            if heads.is_empty() {
                continue;
            }
            // Read in place: only which changes came after, never the document copied or its
            // changes rebuilt.
            let doc = if key == "workspace-tree" {
                Some(tree.document())
            } else {
                records
                    .iter()
                    .find(|r| r.origin().key() == *key)
                    .and_then(|r| r.document.as_ref())
            };
            let Some(doc) = doc
                .filter(|d| d.pending_ops() == 0)
                .map(AutoCommit::committed_document)
            else {
                continue;
            };
            if !doc.get_missing_deps(&heads).is_empty() {
                continue;
            }
            after.insert(
                key.clone(),
                doc.get_changes_meta(&heads)
                    .iter()
                    .map(|meta| ui::history_source::entry_id_from_hash(&meta.hash.to_string()))
                    .collect(),
            );
        }
        self.entries
            .iter()
            .filter(|row| {
                let key = row.id.origin.key();
                !frontier.documents.get(&key).is_some_and(|s| {
                    s.exceptions
                        .contains(&ui::widget::history_group::hex16(&row.id.change))
                }) && after
                    .get(&key)
                    .is_none_or(|changes| changes.contains(&row.id.change))
            })
            .collect()
    }
}

/// The feed lists what people did to the workspace: housekeeping about a document (a read marker, a
/// reading position) and content that came from outside (a refresh's articles, a process's output)
/// stay in that document's own history.
/// Where a row goes in the merged history: when it was made, no earlier than anything its document
/// listed before it, then the order it was read in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Placed {
    time: SystemTime,
    seq: usize,
}

/// Add `rows`, one producer's changes in its causal order (a change after what it builds on), to
/// `target`. A change made in the same second as the one it builds on, or stamped by a clock behind
/// it, is placed after it all the same: its time is never earlier than its document's changes
/// before it.
fn insert_rows(
    target: &mut BTreeMap<WorkspaceHistoryId, (Placed, WorkspaceHistoryEntry)>,
    origin: &HistoryOrigin,
    rows: Vec<HistoryEntry>,
) {
    let mut latest = SystemTime::UNIX_EPOCH;
    for entry in rows {
        latest = latest.max(made_at(&entry));
        if !entry
            .context
            .as_ref()
            .is_none_or(ui::tilo_actions::ActionContext::is_activity)
        {
            continue;
        }
        let id = WorkspaceHistoryId {
            origin: origin.clone(),
            change: entry.id,
        };
        let place = Placed {
            time: latest,
            seq: target.len(),
        };
        target
            .entry(id.clone())
            .or_insert((place, WorkspaceHistoryEntry { id, entry }));
    }
}

/// When `entry`'s change was made, to the millisecond where the action it records says so: a
/// change keeps whole seconds, so two records' changes of one second are placed by the clock of the
/// action that made each (its `at`), when that falls in the change's own second.
fn made_at(entry: &HistoryEntry) -> SystemTime {
    let at_ms = entry
        .context
        .as_ref()
        .and_then(|context| context.action.as_ref())
        .and_then(|action| {
            #[derive(serde::Deserialize)]
            struct Stamped {
                at: u64,
            }
            serde_json::from_slice::<Stamped>(&action.payload).ok()
        })
        .map(|stamped| stamped.at);
    let second = |time: SystemTime| {
        time.duration_since(SystemTime::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .ok()
    };
    match at_ms
        .and_then(|ms| SystemTime::UNIX_EPOCH.checked_add(ui::time::Duration::from_millis(ms)))
    {
        Some(at) if second(at) == second(entry.timestamp) => at.max(entry.timestamp),
        _ => entry.timestamp,
    }
}

fn artifact_kind(kind: &ResourceType) -> ArtifactKind {
    let lower = kind.as_str().to_ascii_lowercase();
    if lower.contains("kanban") || lower.contains("board") {
        ArtifactKind::Kanban
    } else if lower.contains("chat") || lower.contains("message") {
        ArtifactKind::ChatLog
    } else if lower.contains("note") || lower == "tab" || lower.contains("document") {
        ArtifactKind::Note
    } else {
        ArtifactKind::File
    }
}

#[cfg(test)]
mod folder_rows_tests {

    use super::*;
    use fs_protocol::FileAction;
    use fs_protocol::tree::Placing;

    fn person() -> [u8; 32] {
        [7; 32]
    }

    fn device() -> [u8; 32] {
        [9; 32]
    }

    /// Write `node` as the executor writes a file action: one change, naming `invoker`, else
    /// the person at this device.
    fn write(doc: &mut AutoCommit, node: &str, invoker: Option<tilo_actions::ActionInvoker>) {
        let action = FileAction::Write {
            node: node.to_owned(),
            hash: Some("c".repeat(64)),
            size: 4,
            mtime: 2,
            author: hex::encode(device()),
        };
        let context = fs_protocol::file_action::change_context("tree", action.described(), invoker);
        let encoded = tilo_actions::EncodedAction {
            type_id: fs_protocol::file_action::FILE_ACTION_TYPE.to_owned(),
            payload: serde_json::to_vec(&action).unwrap(),
        };
        tilo_actions::apply_local_with_context(doc, &action, encoded, None, context).unwrap();
    }

    /// An agent session working for the person.
    fn agent() -> tilo_actions::ActionInvoker {
        tilo_actions::ActionInvoker::admitted(
            &tilo_actions::Acting::agent("claude").in_session("abc"),
        )
        .named(Some("Claude Code".into()))
        .acting_for(hex::encode(person()))
    }

    fn authors() -> AuthorDirectory {
        let person = hex::encode(person());
        let mut authors = AuthorDirectory::new();
        authors.register_member(
            &ui::tilo_actions::MemberId::person(person.clone()),
            ui::history_source::author_for_identity("Ada".to_owned(), &person),
        );
        authors.register_device(&hex::encode(device()), "Ada's laptop");
        authors
    }

    tilo_test_env::scenario! {
        fn folder_rows_name_each_file_and_who_changed_it;

        phase folder_rows_name_the_agent_that_wrote_a_file;
        // A folder's changes name the file, and the agent session that made them for its person.
        {
            let mut doc = AutoCommit::new();
            let mut tree = fs_protocol::tree::Tree::default();
            let placed = fs_protocol::tree::place(
                (&mut doc, 1),
                &mut tree,
                fs_protocol::tree::ROOT_ID,
                "terms.txt",
                Placing::File {
                    hash: &"a".repeat(64),
                    size: 12,
                },
                (1_000, &hex::encode(device())),
            )
            .unwrap();
            doc.commit();
            write(&mut doc, &placed.id, Some(agent()));
            write(&mut doc, &placed.id, Some(agent()));

            let rows = folder_rows(&doc, &[], &authors(), 100);
            let written: Vec<_> = rows
                .iter()
                .filter(|row| row.summary == "Edited terms.txt")
                .collect();
            assert!(!written.is_empty(), "{rows:#?}");
            for row in written {
                let parts = ui::author_parts(&row.author);
                assert_eq!(parts.person, "Ada", "{row:#?}");
                // The agent is named as the workspace knows it; the session's title ("Claude
                // Code") is detail on the hover, never who did it.
                assert_eq!(parts.writer.as_deref(), Some("Claude"), "{row:#?}");
            }
        }

        phase folder_rows_name_each_file_and_the_person_at_their_device;
        // The person at their own device is one writer; a second file is its own row.
        {
            let mut doc = AutoCommit::new();
            let mut tree = fs_protocol::tree::Tree::default();
            for name in ["terms.txt", "logo.png"] {
                let placed = fs_protocol::tree::place(
                    (&mut doc, 1),
                    &mut tree,
                    fs_protocol::tree::ROOT_ID,
                    name,
                    Placing::File {
                        hash: &"b".repeat(64),
                        size: 3,
                    },
                    (1_000, &hex::encode(device())),
                )
                .unwrap();
                doc.commit();
                write(
                    &mut doc,
                    &placed.id,
                    Some(tilo_actions::ActionInvoker::person(hex::encode(person()))),
                );
            }
            let rows = folder_rows(&doc, &[], &authors(), 100);
            let written: Vec<_> = rows
                .iter()
                .filter(|row| row.summary.starts_with("Edited "))
                .collect();
            assert!(
                written.iter().all(|row| ui::author_line(&row.author) == "Ada"),
                "{rows:#?}"
            );
            assert!(written.iter().any(|row| row.summary == "Edited terms.txt"));
            assert!(written.iter().any(|row| row.summary == "Edited logo.png"));
        }
    }
}
