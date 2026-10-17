//! Workspace-wide history aggregation.
//!
//! A workspace is not one Automerge document: the shell tree describes its
//! windows and governance, while every open record owns a separate change
//! graph.  This module is the join.  It deliberately retains the originating
//! document beside each change; a bare [`HistoryEntryId`] is only unique
//! inside one Automerge graph and is therefore not safe as a workspace key.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash};
use collab_types::ArtifactKind;
use ui::history_source::{AuthorDirectory, HistorySource, committed_heads};
use ui::record::{RecordCheckout, RecordId, RecordKind, RecordStore, home};
use ui::widget::{HistoryEntry, HistoryEntryId};

use super::ActivityTree;
use tilo_desktop_model::RoomId;

/// Hard ceilings keep a corrupt index or a room with thousands of references
/// from turning opening History into an unbounded allocation.
pub const MAX_FEED_ENTRIES: usize = 2_000;
pub const MAX_INDEXED_RECORDS: usize = 512;

/// A read-only source for referenced record documents which are local on disk
/// but are not mounted in the live [`RecordStore`]. Implementations must not
/// acquire a writer lock or add the document to the live/open record set.
pub trait PersistedRecordHistory {
    /// Load one complete document. `None` means missing, corrupt, too large, or
    /// no longer at the validated location; all are reported as unavailable by
    /// [`FeedCoverage`] rather than silently disappearing.
    fn load_record(&self, id: &RecordId) -> Option<AutoCommit>;

    /// A cheap witness of the persisted bytes [`Self::load_record`] would
    /// read now, such as the file's length and modification time. Equal
    /// witnesses promise an unchanged document, so what was derived from the
    /// last load is reused instead of reading and decoding it again. `None`
    /// promises nothing: the document is loaded on every read.
    fn revision(&self, _id: &RecordId) -> Option<u64> {
        None
    }

    /// The documents apps keep for `room` itself rather than as records
    /// (Notes keeps its notebook there), as this device holds them: they
    /// are part of the room's history as much as its records are. None by
    /// default.
    fn room_documents(&self, _room: RoomId) -> Vec<RecordCheckout> {
        Vec::new()
    }
}

/// The most frontiers whose unread changes are remembered per persisted
/// record: the passive, catch-up and away readers, and one spare.
const COLD_FRONTIERS_PER_RECORD: usize = 4;

/// What the index derived from one persisted record at one revision, so an
/// unchanged document on disk is not read and decoded again.
struct ColdRecordState {
    revision: u64,
    heads: Vec<ChangeHash>,
    /// The change ids after each frontier's heads; `None` when the document
    /// lacks a dependency of those heads.
    after: Vec<(Vec<ChangeHash>, Option<HashSet<HistoryEntryId>>)>,
}

fn change_ids(changes: &[automerge::Change]) -> HashSet<HistoryEntryId> {
    changes
        .iter()
        .map(|change| ui::history_source::entry_id_from_hash(&change.hash().to_string()))
        .collect()
}

/// Which collaborative document produced a history row.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HistoryOrigin {
    DesktopTree,
    Record {
        id: RecordId,
        kind: RecordKind,
    },
    AppRecord {
        app: String,
        id: RecordId,
        kind: RecordKind,
    },
}

impl HistoryOrigin {
    pub fn key(&self) -> String {
        match self {
            Self::DesktopTree => "workspace-tree".to_string(),
            Self::Record { id, .. } => format!("record:{id}"),
            // A tuple of strings always encodes; the empty fallback is unreachable.
            Self::AppRecord { app, id, kind } => format!(
                "app-record:{}",
                serde_json::to_string(&(app, id.as_str(), kind.as_str())).unwrap_or_default()
            ),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::DesktopTree => crate::i18n::t("activity-source-workspace"),
            Self::AppRecord { app, id, .. } => format!("{app}: {id}"),
            Self::Record { id, .. } => id.file_path().map_or_else(
                || id.as_str().to_string(),
                |path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_else(|| id.as_str())
                        .to_string()
                },
            ),
        }
    }
}

/// One current-admission record, captured through a read-only owner borrow.
/// Missing and unsettled owners carry no document; clones must be settled.
#[derive(Clone)]
pub struct DetachedRecordHistory {
    pub app: String,
    pub checkout: RecordCheckout,
    pub document: Option<AutoCommit>,
}
impl DetachedRecordHistory {
    pub fn origin(&self) -> HistoryOrigin {
        HistoryOrigin::AppRecord {
            app: self.app.clone(),
            id: self.checkout.id.clone(),
            kind: self.checkout.kind.clone(),
        }
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

/// A history row plus enough provenance to open, diff, or undo it against the
/// correct document later.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceHistoryEntry {
    pub id: WorkspaceHistoryId,
    pub entry: HistoryEntry,
}

/// Private read state for one collaborative document. Full Automerge heads
/// define the causal past; exceptions acknowledge individual changes without
/// claiming that their concurrent siblings were read.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct DocumentSeenFrontier {
    pub heads: Vec<String>,
    pub exceptions: BTreeSet<String>,
}

/// When this device left a workspace and the causal frontier it had seen
/// then. Private to the device; the host keeps it with the seen frontiers.
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
    /// Advance origins the current index exposed, retaining absent origins and
    /// previously acknowledged exceptions outside a bounded projection.
    /// A room-scoped tree is only a projection of the global tree document, so
    /// its visible changes are exceptions rather than an unsafe global head.
    pub fn mark_seen(&mut self, index: &WorkspaceHistoryIndex) {
        let room_scoped = index.query.as_ref().is_some_and(|query| {
            query.room.is_some() || query.rooms.as_ref().is_some_and(|rooms| rooms.len() == 1)
        });
        for (origin, heads) in &index.document_heads {
            let state = self.documents.entry(origin.clone()).or_default();
            if !(room_scoped && origin == "workspace-tree") {
                state.heads = heads.iter().map(ToString::to_string).collect();
            }
        }
        if room_scoped {
            for row in &index.entries {
                if row.id.origin == HistoryOrigin::DesktopTree {
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
        // Collaborative named ranges live in the workspace tree and address
        // its native ids. Preserve those ids so adding record rows cannot
        // detach an existing name from its band. Only foreign document ids
        // need projection into the shared widget namespace.
        if self.id.origin == HistoryOrigin::DesktopTree {
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
    /// `Some` includes only records held by, or originally minted in, this
    /// room. `None` defers scope to `rooms`.
    pub room: Option<RoomId>,
    /// When set, restrict a multi-workspace query to this exact union instead
    /// of treating `room: None` as unrestricted device history.
    pub rooms: Option<Vec<RoomId>>,
    pub limit: usize,
}

impl Default for WorkspaceHistoryQuery {
    fn default() -> Self {
        Self {
            room: None,
            rooms: None,
            limit: 500,
        }
    }
}

impl WorkspaceHistoryQuery {
    pub fn for_room(room: RoomId) -> Self {
        Self {
            room: Some(room),
            ..Self::default()
        }
    }

    pub fn for_rooms(rooms: impl IntoIterator<Item = RoomId>) -> Self {
        let mut rooms: Vec<_> = rooms.into_iter().collect();
        rooms.sort();
        rooms.dedup();
        Self {
            rooms: Some(rooms),
            ..Self::default()
        }
    }

    fn bounded_limit(&self) -> usize {
        self.limit.clamp(1, MAX_FEED_ENTRIES)
    }
}

/// Explains whether the feed is complete on this device.  A referenced record
/// can be known to the room before its Automerge document has been opened or
/// downloaded; silently omitting it would make the history falsely reassuring.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedCoverage {
    /// The live tree has an unfinished producer transaction. No history read
    /// may implicitly commit it; observation resumes after producer settlement.
    pub pending_tree: bool,
    pub referenced_records: usize,
    pub indexed_records: usize,
    pub unavailable_records: Vec<RecordCheckout>,
    pub capped_records: usize,
    pub excluded_apps: Vec<String>,
}

impl FeedCoverage {
    pub fn is_complete(&self) -> bool {
        !self.pending_tree
            && self.unavailable_records.is_empty()
            && self.capped_records == 0
            && self.excluded_apps.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    query: WorkspaceHistoryQuery,
    documents: Vec<(String, Vec<ChangeHash>)>,
    references: Vec<RecordCheckout>,
    supplied_tree_rows: Option<Vec<(HistoryEntryId, ui::time::SystemTime)>>,
}

struct IndexedRecords {
    referenced: Vec<RecordCheckout>,
    included: Vec<RecordCheckout>,
    cold: BTreeMap<RecordId, AutoCommit>,
    /// Committed heads of persisted records unchanged since they were last
    /// loaded. Their documents are read again only to rebuild the feed.
    cold_heads: BTreeMap<RecordId, Vec<ChangeHash>>,
    capped_records: usize,
}

/// Cached, deterministic reduction of the workspace's document graphs.
pub struct WorkspaceHistoryIndex {
    authors: AuthorDirectory,
    entries: Vec<WorkspaceHistoryEntry>,
    coverage: FeedCoverage,
    fingerprint: Option<Fingerprint>,
    query: Option<WorkspaceHistoryQuery>,
    document_heads: BTreeMap<String, Vec<ChangeHash>>,
    generation: u64,
    /// Persisted records' derived state by revision; see [`ColdRecordState`].
    cold: std::cell::RefCell<BTreeMap<RecordId, ColdRecordState>>,
}

impl Default for WorkspaceHistoryIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceHistoryIndex {
    pub fn new() -> Self {
        Self {
            authors: AuthorDirectory::new(),
            entries: Vec::new(),
            coverage: FeedCoverage::default(),
            fingerprint: None,
            query: None,
            document_heads: BTreeMap::new(),
            generation: 0,
            cold: std::cell::RefCell::new(BTreeMap::new()),
        }
    }

    pub fn authors_mut(&mut self) -> &mut AuthorDirectory {
        // A registration can change display names and avatar hues without
        // changing any document head, so the next refresh must re-project.
        self.fingerprint = None;
        &mut self.authors
    }

    pub fn set_authors(&mut self, authors: AuthorDirectory) {
        if self.authors != authors {
            self.authors = authors;
            self.fingerprint = None;
        }
    }

    pub fn entries(&self) -> &[WorkspaceHistoryEntry] {
        &self.entries
    }

    pub fn coverage(&self) -> &FeedCoverage {
        &self.coverage
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Retain only names whose whole positional span belongs to one document.
    /// Until named-range storage carries an explicit multi-origin membership
    /// list, dropping an ambiguous band is safer than visually absorbing and
    /// potentially undoing changes from another record.
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

    /// Return rows not causally covered by `frontier`. Persisted heads which
    /// are malformed or absent from the local graph fail open as unread.
    pub fn unseen<'a>(
        &'a self,
        frontier: &WorkspaceSeenFrontier,
        tree: &mut impl ActivityTree,
        records: &RecordStore,
    ) -> Vec<&'a WorkspaceHistoryEntry> {
        self.unseen_inner(frontier, tree, records, None)
    }

    /// The causal unread reduction including referenced local records which
    /// are not mounted in the live store.
    pub fn unseen_with_local_records<'a>(
        &'a self,
        frontier: &WorkspaceSeenFrontier,
        tree: &mut impl ActivityTree,
        records: &RecordStore,
        local: &dyn PersistedRecordHistory,
    ) -> Vec<&'a WorkspaceHistoryEntry> {
        self.unseen_inner(frontier, tree, records, Some(local))
    }

    fn unseen_inner<'a>(
        &'a self,
        frontier: &WorkspaceSeenFrontier,
        tree: &impl ActivityTree,
        records: &RecordStore,
        local: Option<&dyn PersistedRecordHistory>,
    ) -> Vec<&'a WorkspaceHistoryEntry> {
        let mut after: BTreeMap<String, HashSet<HistoryEntryId>> = BTreeMap::new();
        for (origin, state) in &frontier.documents {
            let heads: Vec<ChangeHash> = state
                .heads
                .iter()
                .filter_map(|encoded| encoded.parse().ok())
                .collect();
            if heads.is_empty() || heads.len() != state.heads.len() {
                continue;
            }
            let changes = if origin == "workspace-tree" {
                let mut doc = committed_snapshot(tree.document());
                if !doc.get_missing_deps(&heads).is_empty() {
                    continue;
                }
                change_ids(&doc.get_changes(&heads))
            } else {
                let Some(record) = origin.strip_prefix("record:").and_then(RecordId::parse) else {
                    continue;
                };
                let changes = records.read(&record, |doc| {
                    let mut doc = committed_snapshot(doc);
                    doc.get_missing_deps(&heads)
                        .is_empty()
                        .then(|| change_ids(&doc.get_changes(&heads)))
                });
                let changes = match changes {
                    Some(changes) => changes,
                    None => {
                        local.and_then(|source| self.cold_changes_after(source, &record, &heads))
                    }
                };
                let Some(changes) = changes else {
                    continue;
                };
                changes
            };
            after.insert(origin.clone(), changes);
        }
        self.entries
            .iter()
            .filter(|row| {
                let key = row.id.origin.key();
                let explicitly_seen = frontier.documents.get(&key).is_some_and(|state| {
                    state
                        .exceptions
                        .contains(&ui::widget::history_group::hex16(&row.id.change))
                });
                if explicitly_seen {
                    return false;
                }
                after
                    .get(&key)
                    .is_none_or(|changes| changes.contains(&row.id.change))
            })
            .collect()
    }

    /// Build from the tree's complete history.  A surface that already has a
    /// correctly workspace-scoped tree reduction should use
    /// [`Self::refresh_with_workspace_entries`] to avoid leaking other rooms'
    /// shell metadata into a room-specific feed.
    pub fn refresh(
        &mut self,
        tree: &mut impl ActivityTree,
        records: &RecordStore,
        query: WorkspaceHistoryQuery,
    ) -> bool {
        self.refresh_inner(tree, records, query, None, None)
    }

    /// Build a feed which also reads referenced, persisted records without
    /// opening them in the app's live [`RecordStore`].
    pub fn refresh_with_local_records(
        &mut self,
        tree: &mut impl ActivityTree,
        records: &RecordStore,
        local: &dyn PersistedRecordHistory,
        query: WorkspaceHistoryQuery,
    ) -> bool {
        self.refresh_inner(tree, records, query, None, Some(local))
    }

    /// Join a caller's room-scoped tree rows with all records reachable from
    /// that room.  The supplied rows remain the exact objects used by the room
    /// pane, so naming and workspace labels cannot drift between surfaces.
    pub fn refresh_with_workspace_entries(
        &mut self,
        tree: &mut impl ActivityTree,
        records: &RecordStore,
        query: WorkspaceHistoryQuery,
        workspace_entries: &[HistoryEntry],
    ) -> bool {
        self.refresh_inner(tree, records, query, Some(workspace_entries), None)
    }

    /// Join pre-scoped workspace rows with both live and cold local records.
    pub fn refresh_with_workspace_entries_and_local_records(
        &mut self,
        tree: &mut impl ActivityTree,
        records: &RecordStore,
        local: &dyn PersistedRecordHistory,
        query: WorkspaceHistoryQuery,
        workspace_entries: &[HistoryEntry],
    ) -> bool {
        self.refresh_inner(tree, records, query, Some(workspace_entries), Some(local))
    }

    /// Replaces the entire admitted multi-owner input atomically. Never borrows
    /// a live `RecordStore` mutably or closes a producer transaction.
    pub fn refresh_detached(
        &mut self,
        tree: &mut impl ActivityTree,
        query: WorkspaceHistoryQuery,
        tree_rows: &[HistoryEntry],
        records: &[DetachedRecordHistory],
    ) -> bool {
        let limit = query.bounded_limit();
        let mut by_id = BTreeMap::new();
        let mut coverage = FeedCoverage {
            pending_tree: tree.document().pending_ops() > 0,
            referenced_records: records.len(),
            ..Default::default()
        };
        let mut documents = BTreeMap::new();
        if !coverage.pending_tree {
            insert_rows(&mut by_id, &HistoryOrigin::DesktopTree, tree_rows.to_vec());
            documents.insert("workspace-tree".into(), tree.document_mut().get_heads());
        }
        let mut ordered = records.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|record| record.origin());
        for (i, record) in ordered.into_iter().enumerate() {
            if i >= MAX_INDEXED_RECORDS {
                coverage.capped_records = coverage.capped_records.saturating_add(1);
                continue;
            }
            let Some(mut doc) = record
                .document
                .as_ref()
                .filter(|doc| doc.pending_ops() == 0)
                .cloned()
            else {
                coverage.unavailable_records.push(record.checkout.clone());
                // Keep the last settled rendering while this admitted owner is
                // busy, but never carry rows from an owner absent from the new
                // admission. Unavailable documents cannot establish seen heads.
                for row in self
                    .entries
                    .iter()
                    .filter(|row| row.id.origin == record.origin())
                {
                    by_id.insert(row.id.clone(), row.clone());
                }
                continue;
            };
            let origin = record.origin();
            documents.insert(origin.key(), doc.get_heads());
            // What people did: a record's genesis names no one.
            let rows = HistorySource::new()
                .without_field_paths()
                .with_authors(self.authors.clone())
                .with_artifact_kind(artifact_kind(&record.checkout.kind))
                .with_max_entries(limit)
                .without_record_genesis()
                .entries(&mut doc);
            insert_rows(&mut by_id, &origin, rows);
            coverage.indexed_records = coverage.indexed_records.saturating_add(1);
        }
        let mut entries = by_id.into_values().collect::<Vec<_>>();
        entries.sort_by(|a, b| {
            a.entry
                .timestamp
                .cmp(&b.entry.timestamp)
                .then_with(|| a.id.cmp(&b.id))
        });
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
        self.fingerprint = None;
        if changed {
            self.generation = self.generation.wrapping_add(1);
        }
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
            let doc = if key == "workspace-tree" {
                Some(tree.document().clone())
            } else {
                records
                    .iter()
                    .find(|r| r.origin().key() == *key)
                    .and_then(|r| r.document.clone())
            };
            let Some(mut doc) = doc.filter(|d| d.pending_ops() == 0) else {
                continue;
            };
            if !doc.get_missing_deps(&heads).is_empty() {
                continue;
            }
            after.insert(
                key.clone(),
                doc.get_changes(&heads)
                    .iter()
                    .map(|c| ui::history_source::entry_id_from_hash(&c.hash().to_string()))
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

    fn refresh_inner(
        &mut self,
        tree: &impl ActivityTree,
        records: &RecordStore,
        query: WorkspaceHistoryQuery,
        supplied: Option<&[HistoryEntry]>,
        local: Option<&dyn PersistedRecordHistory>,
    ) -> bool {
        let limit = query.bounded_limit();
        let mut indexed = self.index_records(tree, records, &query, local);
        let fingerprint = Self::build_fingerprint(tree, records, &query, supplied, &indexed);
        if self.fingerprint.as_ref() == Some(&fingerprint) {
            return false;
        }

        let entries = self.collect_entries(tree, records, supplied, &mut indexed, local, limit);
        self.finish_refresh(query, &indexed, fingerprint, entries);
        true
    }

    /// The committed heads of persisted record `id` if `source` still holds
    /// the revision they were derived from.
    fn cold_heads(
        &self,
        source: &dyn PersistedRecordHistory,
        id: &RecordId,
    ) -> Option<Vec<ChangeHash>> {
        let revision = source.revision(id)?;
        self.cold
            .borrow()
            .get(id)
            .filter(|state| state.revision == revision)
            .map(|state| state.heads.clone())
    }

    /// Load persisted record `id` from `source`, committed changes only, and
    /// remember its heads under the revision it was read at.
    fn load_cold(&self, source: &dyn PersistedRecordHistory, id: &RecordId) -> Option<AutoCommit> {
        let revision = source.revision(id);
        let mut doc = source.load_record(id)?;
        // A persisted source contributes committed changes only.
        doc.rollback();
        let mut cold = self.cold.borrow_mut();
        match revision {
            Some(revision) => {
                let heads = committed_heads(&doc);
                let unchanged = cold
                    .get(id)
                    .is_some_and(|state| state.revision == revision && state.heads == heads);
                if !unchanged {
                    cold.insert(
                        id.clone(),
                        ColdRecordState {
                            revision,
                            heads,
                            after: Vec::new(),
                        },
                    );
                }
            }
            None => {
                cold.remove(id);
            }
        }
        Some(doc)
    }

    /// The change ids persisted record `id` holds after `heads`, read from
    /// `source` only when its revision moved since the last answer.
    fn cold_changes_after(
        &self,
        source: &dyn PersistedRecordHistory,
        id: &RecordId,
        heads: &[ChangeHash],
    ) -> Option<HashSet<HistoryEntryId>> {
        let remembered = self.cold_heads(source, id).and_then(|_| {
            let cold = self.cold.borrow();
            cold.get(id)?
                .after
                .iter()
                .find(|(frontier, _)| frontier.as_slice() == heads)
                .map(|(_, after)| after.clone())
        });
        if let Some(after) = remembered {
            return after;
        }
        let mut doc = self.load_cold(source, id)?;
        let after = doc
            .get_missing_deps(heads)
            .is_empty()
            .then(|| change_ids(&doc.get_changes(heads)));
        if let Some(state) = self.cold.borrow_mut().get_mut(id) {
            if state.after.len() >= COLD_FRONTIERS_PER_RECORD {
                state.after.remove(0);
            }
            state.after.push((heads.to_vec(), after.clone()));
        }
        after
    }

    fn index_records(
        &self,
        tree: &impl ActivityTree,
        records: &RecordStore,
        query: &WorkspaceHistoryQuery,
        local: Option<&dyn PersistedRecordHistory>,
    ) -> IndexedRecords {
        let referenced = query.rooms.as_ref().map_or_else(
            || referenced_records(tree, query.room),
            |rooms| {
                rooms
                    .iter()
                    .flat_map(|room| referenced_records(tree, Some(*room)))
                    .collect()
            },
        );
        let referenced_ids: BTreeSet<RecordId> =
            referenced.iter().map(|record| record.id.clone()).collect();
        let mut candidates = BTreeMap::new();
        for checkout in records.open_records() {
            if Self::record_belongs(records, &checkout.id, query, &referenced_ids) {
                candidates.insert(checkout.id.clone(), checkout);
            }
        }
        for checkout in &referenced {
            candidates
                .entry(checkout.id.clone())
                .or_insert_with(|| checkout.clone());
        }
        if let Some(local) = local {
            let rooms = query
                .rooms
                .clone()
                .unwrap_or_else(|| query.room.into_iter().collect());
            for room in rooms {
                for checkout in local.room_documents(room) {
                    candidates.entry(checkout.id.clone()).or_insert(checkout);
                }
            }
        }
        let mut candidates: Vec<_> = candidates.into_values().collect();
        candidates.sort();

        let capped_records = candidates.len().saturating_sub(MAX_INDEXED_RECORDS);
        candidates.truncate(MAX_INDEXED_RECORDS);
        let mut cold = BTreeMap::new();
        let mut cold_heads = BTreeMap::new();
        let mut included = Vec::new();
        for checkout in candidates {
            if records.read(&checkout.id, |_| ()).is_some() {
                included.push(checkout);
            } else if let Some(source) = local {
                if let Some(heads) = self.cold_heads(source, &checkout.id) {
                    cold_heads.insert(checkout.id.clone(), heads);
                    included.push(checkout);
                } else if let Some(doc) = self.load_cold(source, &checkout.id) {
                    cold.insert(checkout.id.clone(), doc);
                    included.push(checkout);
                }
            }
        }
        IndexedRecords {
            referenced,
            included,
            cold,
            cold_heads,
            capped_records,
        }
    }

    fn record_belongs(
        records: &RecordStore,
        id: &RecordId,
        query: &WorkspaceHistoryQuery,
        referenced_ids: &BTreeSet<RecordId>,
    ) -> bool {
        if referenced_ids.contains(id) {
            return true;
        }
        let Some(rooms) = query.rooms.as_ref() else {
            return query.room.is_none()
                || records.read(id, |doc| {
                    home(doc).and_then(|value| value.parse::<uuid::Uuid>().ok())
                        == query.room.map(|room| room.as_uuid())
                }) == Some(true);
        };
        records.read(id, |doc| {
            home(doc)
                .and_then(|value| value.parse::<uuid::Uuid>().ok())
                .is_some_and(|home| rooms.iter().any(|room| room.as_uuid() == home))
        }) == Some(true)
    }

    fn build_fingerprint(
        tree: &impl ActivityTree,
        records: &RecordStore,
        query: &WorkspaceHistoryQuery,
        supplied: Option<&[HistoryEntry]>,
        indexed: &IndexedRecords,
    ) -> Fingerprint {
        let mut documents = vec![(
            "workspace-tree".to_string(),
            committed_heads(tree.document()),
        )];
        for record in &indexed.included {
            if let Some(heads) = records
                .read(&record.id, committed_heads)
                .or_else(|| indexed.cold.get(&record.id).map(committed_heads))
                .or_else(|| indexed.cold_heads.get(&record.id).cloned())
            {
                documents.push((format!("record:{}", record.id), heads));
            }
        }
        let supplied_tree_rows = supplied.map(|rows| {
            rows.iter()
                .map(|entry| (entry.id, entry.timestamp))
                .collect()
        });
        Fingerprint {
            query: query.clone(),
            documents,
            references: indexed.referenced.clone(),
            supplied_tree_rows,
        }
    }

    fn collect_entries(
        &self,
        tree: &impl ActivityTree,
        records: &RecordStore,
        supplied: Option<&[HistoryEntry]>,
        indexed: &mut IndexedRecords,
        local: Option<&dyn PersistedRecordHistory>,
        limit: usize,
    ) -> Vec<WorkspaceHistoryEntry> {
        let included = &indexed.included;
        let cold = &mut indexed.cold;
        let mut by_id: BTreeMap<WorkspaceHistoryId, WorkspaceHistoryEntry> = BTreeMap::new();
        let tree_rows = supplied.map_or_else(
            || {
                let mut doc = committed_snapshot(tree.document());
                HistorySource::new()
                    .without_field_paths()
                    .with_authors(self.authors.clone())
                    .with_artifact_kind(ArtifactKind::Cluster)
                    .with_max_entries(limit)
                    .entries(&mut doc)
            },
            <[HistoryEntry]>::to_vec,
        );
        insert_rows(&mut by_id, &HistoryOrigin::DesktopTree, tree_rows);

        for record in included {
            let kind = artifact_kind(&record.kind);
            let origin = HistoryOrigin::Record {
                id: record.id.clone(),
                kind: record.kind.clone(),
            };
            let authors = self.authors.clone();
            let inserted_live = records.read(&record.id, |doc| {
                let mut doc = committed_snapshot(doc);
                let rows = HistorySource::new()
                    .without_field_paths()
                    .with_authors(authors)
                    .with_artifact_kind(kind)
                    .with_max_entries(limit)
                    .entries(&mut doc);
                insert_rows(&mut by_id, &origin, rows);
            });
            if inserted_live.is_some() {
                continue;
            }
            if !cold.contains_key(&record.id)
                && indexed.cold_heads.contains_key(&record.id)
                && let Some(doc) = local.and_then(|source| self.load_cold(source, &record.id))
            {
                cold.insert(record.id.clone(), doc);
            }
            if let Some(doc) = cold.get_mut(&record.id) {
                let rows = HistorySource::new()
                    .without_field_paths()
                    .with_authors(self.authors.clone())
                    .with_artifact_kind(kind)
                    .with_max_entries(limit)
                    .entries(doc);
                insert_rows(&mut by_id, &origin, rows);
            }
        }

        let mut entries: Vec<_> = by_id.into_values().collect();
        entries.sort_by(|a, b| {
            a.entry
                .timestamp
                .cmp(&b.entry.timestamp)
                .then_with(|| a.id.origin.cmp(&b.id.origin))
                .then_with(|| a.id.change.as_bytes().cmp(b.id.change.as_bytes()))
        });
        // ActivityTimeline/group_entries require oldest-first input. Keep the
        // newest suffix when applying the bound, then preserve that order.
        if entries.len() > limit {
            entries.drain(..entries.len().saturating_sub(limit));
        }
        entries
    }

    fn finish_refresh(
        &mut self,
        query: WorkspaceHistoryQuery,
        indexed: &IndexedRecords,
        fingerprint: Fingerprint,
        entries: Vec<WorkspaceHistoryEntry>,
    ) {
        let included_ids: BTreeSet<RecordId> = indexed
            .included
            .iter()
            .map(|record| record.id.clone())
            .collect();
        let unavailable_records = indexed
            .referenced
            .iter()
            .filter(|record| !included_ids.contains(&record.id))
            .cloned()
            .collect();
        self.coverage = FeedCoverage {
            pending_tree: false,
            referenced_records: indexed.referenced.len(),
            indexed_records: indexed.included.len(),
            unavailable_records,
            capped_records: indexed.capped_records,
            excluded_apps: Vec::new(),
        };
        self.entries = entries;
        self.query = Some(query);
        self.document_heads = fingerprint.documents.iter().cloned().collect();
        self.fingerprint = Some(fingerprint);
        self.generation = self.generation.wrapping_add(1);
    }
}

/// History observes only settled commits. Cloning and rolling back pending
/// operations keeps an in-flight component action owned by its producer.
fn committed_snapshot(doc: &AutoCommit) -> AutoCommit {
    let mut snapshot = doc.clone();
    snapshot.rollback();
    snapshot
}

/// The feed lists what people did to the workspace: housekeeping about a
/// document (a read marker, a reading position) and content that came from
/// outside (a refresh's articles, a process's output) stay in that document's
/// own history.
fn insert_rows(
    target: &mut BTreeMap<WorkspaceHistoryId, WorkspaceHistoryEntry>,
    origin: &HistoryOrigin,
    rows: Vec<HistoryEntry>,
) {
    for entry in rows {
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
        target
            .entry(id.clone())
            .or_insert(WorkspaceHistoryEntry { id, entry });
    }
}

fn referenced_records(tree: &impl ActivityTree, room: Option<RoomId>) -> Vec<RecordCheckout> {
    let Some(room) = room else {
        return Vec::new();
    };
    tree.room_records_held
        .get(&room)
        .into_iter()
        .flat_map(|records| records.iter())
        .filter_map(|(id, kind)| {
            Some(RecordCheckout::new(
                RecordId::parse(id)?,
                RecordKind::new(kind),
            ))
        })
        .collect()
}

fn artifact_kind(kind: &RecordKind) -> ArtifactKind {
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
