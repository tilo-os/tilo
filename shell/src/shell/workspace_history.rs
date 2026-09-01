//! Workspace-wide history aggregation.
//!
//! A workspace is not one Automerge document: the shell tree describes its
//! windows and governance, while every open record owns a separate change
//! graph.  This module is the join.  It deliberately retains the originating
//! document beside each change; a bare [`HistoryEntryId`] is only unique
//! inside one Automerge graph and is therefore not safe as a workspace key.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use automerge::ChangeHash;
use collab_types::ArtifactKind;
use ui::history_source::{AuthorDirectory, HistorySource};
use ui::record::{RecordCheckout, RecordId, RecordKind, RecordStore, home};
use ui::widget::{HistoryEntry, HistoryEntryId};

use crate::shell::state::{RoomId, WorkspaceTree};

/// Hard ceilings keep a corrupt index or a room with thousands of references
/// from turning opening History into an unbounded allocation.
pub const MAX_FEED_ENTRIES: usize = 2_000;
pub const MAX_INDEXED_RECORDS: usize = 512;

/// Which collaborative document produced a history row.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HistoryOrigin {
    WorkspaceTree,
    Record { id: RecordId, kind: RecordKind },
}

impl HistoryOrigin {
    pub fn key(&self) -> String {
        match self {
            Self::WorkspaceTree => "workspace-tree".to_string(),
            Self::Record { id, .. } => format!("record:{id}"),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::WorkspaceTree => "Workspace".to_string(),
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

/// Persistable causal read state, partitioned by full document origin.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct WorkspaceSeenFrontier {
    pub documents: BTreeMap<String, DocumentSeenFrontier>,
}

impl WorkspaceSeenFrontier {
    /// Replace this frontier with exactly what the current index exposed.
    /// A room-scoped tree is only a projection of the global tree document, so
    /// its visible changes are exceptions rather than an unsafe global head.
    pub fn mark_seen(&mut self, index: &WorkspaceHistoryIndex) {
        self.documents.clear();
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
                if row.id.origin == HistoryOrigin::WorkspaceTree {
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
        if self.id.origin == HistoryOrigin::WorkspaceTree {
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
    pub referenced_records: usize,
    pub indexed_records: usize,
    pub unavailable_records: Vec<RecordCheckout>,
    pub capped_records: usize,
}

impl FeedCoverage {
    pub fn is_complete(&self) -> bool {
        self.unavailable_records.is_empty() && self.capped_records == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    query: WorkspaceHistoryQuery,
    documents: Vec<(String, Vec<ChangeHash>)>,
    references: Vec<RecordCheckout>,
    supplied_tree_rows: Option<Vec<(HistoryEntryId, std::time::SystemTime)>>,
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

    /// Return rows not causally covered by `frontier`. Persisted heads which
    /// are malformed or absent from the local graph fail open as unread.
    pub fn unseen<'a>(
        &'a self,
        frontier: &WorkspaceSeenFrontier,
        tree: &mut WorkspaceTree,
        records: &RecordStore,
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
                if !tree.doc.get_missing_deps(&heads).is_empty() {
                    continue;
                }
                tree.doc.get_changes(&heads)
            } else {
                let Some(record) = origin.strip_prefix("record:").and_then(RecordId::parse) else {
                    continue;
                };
                let Some(Some(changes)) = records.with(&record, |doc| {
                    doc.get_missing_deps(&heads)
                        .is_empty()
                        .then(|| doc.get_changes(&heads))
                }) else {
                    continue;
                };
                changes
            };
            after.insert(
                origin.clone(),
                changes
                    .iter()
                    .map(|change| {
                        ui::history_source::entry_id_from_hash(&change.hash().to_string())
                    })
                    .collect(),
            );
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
                    .map_or(true, |changes| changes.contains(&row.id.change))
            })
            .collect()
    }

    /// Build from the tree's complete history.  A surface that already has a
    /// correctly workspace-scoped tree reduction should use
    /// [`Self::refresh_with_workspace_entries`] to avoid leaking other rooms'
    /// shell metadata into a room-specific feed.
    pub fn refresh(
        &mut self,
        tree: &mut WorkspaceTree,
        records: &RecordStore,
        query: WorkspaceHistoryQuery,
    ) -> bool {
        self.refresh_inner(tree, records, query, None)
    }

    /// Join a caller's room-scoped tree rows with all records reachable from
    /// that room.  The supplied rows remain the exact objects used by the room
    /// pane, so naming and workspace labels cannot drift between surfaces.
    pub fn refresh_with_workspace_entries(
        &mut self,
        tree: &mut WorkspaceTree,
        records: &RecordStore,
        query: WorkspaceHistoryQuery,
        workspace_entries: &[HistoryEntry],
    ) -> bool {
        self.refresh_inner(tree, records, query, Some(workspace_entries))
    }

    fn refresh_inner(
        &mut self,
        tree: &mut WorkspaceTree,
        records: &RecordStore,
        query: WorkspaceHistoryQuery,
        supplied: Option<&[HistoryEntry]>,
    ) -> bool {
        let limit = query.bounded_limit();
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
        let open = records.open_records();

        let mut included = Vec::new();
        for checkout in &open {
            let belongs = if let Some(rooms) = query.rooms.as_ref() {
                referenced_ids.contains(&checkout.id)
                    || records.read(&checkout.id, |doc| {
                        home(doc)
                            .and_then(|value| value.parse::<uuid::Uuid>().ok())
                            .is_some_and(|home| rooms.iter().any(|room| room.as_uuid() == home))
                    }) == Some(true)
            } else {
                query.room.is_none()
                    || referenced_ids.contains(&checkout.id)
                    || records.read(&checkout.id, |doc| {
                        home(doc).and_then(|value| value.parse::<uuid::Uuid>().ok())
                            == query.room.map(|room| room.as_uuid())
                    }) == Some(true)
            };
            if belongs {
                included.push(checkout.clone());
            }
        }
        included.sort();

        let capped_records = included.len().saturating_sub(MAX_INDEXED_RECORDS);
        included.truncate(MAX_INDEXED_RECORDS);
        let included_ids: BTreeSet<RecordId> =
            included.iter().map(|record| record.id.clone()).collect();

        let mut documents = vec![("workspace-tree".to_string(), tree.doc.get_heads())];
        for record in &included {
            if let Some(heads) = records.with(&record.id, |doc| doc.get_heads()) {
                documents.push((format!("record:{}", record.id), heads));
            }
        }
        let supplied_tree_rows = supplied.map(|rows| {
            rows.iter()
                .map(|entry| (entry.id, entry.timestamp))
                .collect()
        });
        let fingerprint = Fingerprint {
            query: query.clone(),
            documents,
            references: referenced.clone(),
            supplied_tree_rows,
        };
        if self.fingerprint.as_ref() == Some(&fingerprint) {
            return false;
        }

        let mut by_id: BTreeMap<WorkspaceHistoryId, WorkspaceHistoryEntry> = BTreeMap::new();
        let tree_rows = supplied.map_or_else(
            || {
                HistorySource::new()
                    .with_authors(self.authors.clone())
                    .with_artifact_kind(ArtifactKind::Cluster)
                    .with_max_entries(limit)
                    .entries(&mut tree.doc)
            },
            <[HistoryEntry]>::to_vec,
        );
        insert_rows(&mut by_id, HistoryOrigin::WorkspaceTree, tree_rows);

        for record in &included {
            let kind = artifact_kind(&record.kind);
            let origin = HistoryOrigin::Record {
                id: record.id.clone(),
                kind: record.kind.clone(),
            };
            let authors = self.authors.clone();
            let _ = records.with(&record.id, |doc| {
                let rows = HistorySource::new()
                    .with_authors(authors)
                    .with_artifact_kind(kind)
                    .with_max_entries(limit)
                    .entries(doc);
                insert_rows(&mut by_id, origin, rows);
            });
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

        let unavailable_records = referenced
            .iter()
            .filter(|record| !included_ids.contains(&record.id))
            .cloned()
            .collect();
        self.coverage = FeedCoverage {
            referenced_records: referenced.len(),
            indexed_records: included.len(),
            unavailable_records,
            capped_records,
        };
        self.entries = entries;
        self.query = Some(query);
        self.document_heads = fingerprint.documents.iter().cloned().collect();
        self.fingerprint = Some(fingerprint);
        self.generation = self.generation.wrapping_add(1);
        true
    }
}

fn insert_rows(
    target: &mut BTreeMap<WorkspaceHistoryId, WorkspaceHistoryEntry>,
    origin: HistoryOrigin,
    rows: Vec<HistoryEntry>,
) {
    for entry in rows {
        let id = WorkspaceHistoryId {
            origin: origin.clone(),
            change: entry.id,
        };
        target
            .entry(id.clone())
            .or_insert(WorkspaceHistoryEntry { id, entry });
    }
}

fn referenced_records(tree: &WorkspaceTree, room: Option<RoomId>) -> Vec<RecordCheckout> {
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

#[cfg(test)]
mod tests {
    use automerge::ROOT;
    use automerge::transaction::{CommitOptions, Transactable};
    use ui::qos_actions::ActionContext;

    use super::*;
    use crate::shell::state::{Workspace, WorkspaceId};

    fn tree_and_room() -> (WorkspaceTree, RoomId) {
        let workspace = Workspace::new(WorkspaceId::new(), "Research");
        let room = workspace.room;
        let mut tree = WorkspaceTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..WorkspaceTree::default()
        };
        tree.doc.put(&ROOT, "name", "Research").unwrap();
        tree.doc.commit_with(CommitOptions::default().with_time(1));
        (tree, room)
    }

    fn action_record(store: &RecordStore, id: RecordId, room: RoomId, time: i64) -> RecordCheckout {
        let checkout = RecordCheckout::new(id, RecordKind::new("note"));
        assert!(store.open_record(&checkout));
        store.with(&checkout.id, |doc| {
            ui::record::init(doc, &checkout.id, &checkout.kind, &room.to_string());
            doc.put(&ROOT, "body", "A durable decision").unwrap();
            let context = ActionContext::for_resource(
                "Edit note",
                checkout.id.as_str(),
                "note",
                "Decision log",
            )
            .with_app("org.qos.Notes", "Notes")
            .with_purpose("Recorded the launch decision");
            doc.commit_with(
                CommitOptions::default()
                    .with_time(time)
                    .with_message(context.change_message().unwrap()),
            );
        });
        checkout
    }

    #[test]
    fn joins_tree_and_room_records_with_action_provenance() {
        let (mut tree, room) = tree_and_room();
        let records = RecordStore::in_memory();
        let kept = action_record(&records, RecordId::for_file("launch.md"), room, 20);
        let other = action_record(
            &records,
            RecordId::for_file("private.md"),
            RoomId::new(),
            30,
        );
        tree.room_records_held
            .entry(room)
            .or_default()
            .insert(kept.id.to_string(), kept.kind.to_string());

        let mut index = WorkspaceHistoryIndex::new();
        assert!(index.refresh(&mut tree, &records, WorkspaceHistoryQuery::for_room(room)));
        assert!(index.entries().iter().any(|row| {
            matches!(&row.id.origin, HistoryOrigin::Record { id, .. } if id == &kept.id)
                && row
                    .entry
                    .context
                    .as_ref()
                    .and_then(|c| c.purpose.as_deref())
                    == Some("Recorded the launch decision")
        }));
        assert!(!index.entries().iter().any(
            |row| matches!(&row.id.origin, HistoryOrigin::Record { id, .. } if id == &other.id)
        ));
        assert!(index.coverage().is_complete());
    }

    #[test]
    fn refresh_is_cached_and_projected_ids_include_document_identity() {
        let (mut tree, room) = tree_and_room();
        let records = RecordStore::in_memory();
        let first = action_record(&records, RecordId::for_tab(1), room, 20);
        let second = action_record(&records, RecordId::for_tab(2), room, 20);
        for record in [&first, &second] {
            tree.room_records_held
                .entry(room)
                .or_default()
                .insert(record.id.to_string(), record.kind.to_string());
        }
        let mut index = WorkspaceHistoryIndex::new();
        let query = WorkspaceHistoryQuery::for_room(room);
        assert!(index.refresh(&mut tree, &records, query.clone()));
        let generation = index.generation();
        assert!(!index.refresh(&mut tree, &records, query));
        assert_eq!(index.generation(), generation);

        let projected: std::collections::HashSet<HistoryEntryId> = index
            .entries()
            .iter()
            .filter(|row| matches!(row.id.origin, HistoryOrigin::Record { .. }))
            .map(|row| row.timeline_entry().id)
            .collect();
        assert!(projected.len() >= 2);
        assert!(index.entries().iter().any(|row| {
            row.id.origin == HistoryOrigin::WorkspaceTree
                && row.timeline_entry().id == row.id.change
        }));
    }

    #[test]
    fn reports_referenced_documents_that_are_not_local() {
        let (mut tree, room) = tree_and_room();
        let missing = RecordCheckout::new(RecordId::minted(), RecordKind::new("kanban"));
        tree.room_records_held
            .entry(room)
            .or_default()
            .insert(missing.id.to_string(), missing.kind.to_string());

        let mut index = WorkspaceHistoryIndex::new();
        index.refresh(
            &mut tree,
            &RecordStore::in_memory(),
            WorkspaceHistoryQuery::for_room(room),
        );
        assert!(!index.coverage().is_complete());
        assert_eq!(index.coverage().unavailable_records, vec![missing]);
    }

    #[test]
    fn causal_frontier_detects_new_record_changes_and_scoped_tree_exceptions() {
        let (mut tree, room) = tree_and_room();
        let records = RecordStore::in_memory();
        let record = action_record(&records, RecordId::for_tab(44), room, 20);
        tree.room_records_held
            .entry(room)
            .or_default()
            .insert(record.id.to_string(), record.kind.to_string());
        let query = WorkspaceHistoryQuery::for_room(room);
        let mut index = WorkspaceHistoryIndex::new();
        index.refresh(&mut tree, &records, query.clone());
        let mut frontier = WorkspaceSeenFrontier::default();
        frontier.mark_seen(&index);
        assert!(index.unseen(&frontier, &mut tree, &records).is_empty());

        tree.doc.put(&ROOT, "status", "review").unwrap();
        tree.doc.commit_with(CommitOptions::default().with_time(30));
        records.with(&record.id, |doc| {
            doc.put(&ROOT, "body", "A newer decision").unwrap();
            doc.commit_with(CommitOptions::default().with_time(40));
        });
        index.refresh(&mut tree, &records, query);
        let unseen = index.unseen(&frontier, &mut tree, &records);
        assert_eq!(unseen.len(), 2);
        assert!(
            unseen
                .iter()
                .any(|row| row.id.origin == HistoryOrigin::WorkspaceTree)
        );
        assert!(unseen.iter().any(
            |row| matches!(&row.id.origin, HistoryOrigin::Record { id, .. } if id == &record.id)
        ));
    }

    #[test]
    fn causal_frontier_persistence_round_trips_full_heads() {
        let (mut tree, room) = tree_and_room();
        let records = RecordStore::in_memory();
        let mut index = WorkspaceHistoryIndex::new();
        index.refresh(&mut tree, &records, WorkspaceHistoryQuery::for_room(room));
        let mut frontier = WorkspaceSeenFrontier::default();
        frontier.mark_seen(&index);
        let encoded = serde_json::to_string(&frontier).unwrap();
        let decoded: WorkspaceSeenFrontier = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, frontier);
        assert!(decoded.documents["workspace-tree"].heads.is_empty());
        assert!(!decoded.documents["workspace-tree"].exceptions.is_empty());
    }

    #[test]
    fn aggregate_is_oldest_first_while_retaining_the_newest_bound() {
        let (mut tree, room) = tree_and_room();
        let records = RecordStore::in_memory();
        for (name, time) in [("older.md", 10), ("newer.md", 20)] {
            let record = action_record(&records, RecordId::for_file(name), room, time);
            tree.room_records_held
                .entry(room)
                .or_default()
                .insert(record.id.to_string(), record.kind.to_string());
        }
        let mut index = WorkspaceHistoryIndex::new();
        index.refresh(
            &mut tree,
            &records,
            WorkspaceHistoryQuery {
                room: Some(room),
                limit: 2,
                ..WorkspaceHistoryQuery::default()
            },
        );
        let times: Vec<_> = index
            .entries()
            .iter()
            .map(|row| row.entry.timestamp)
            .collect();
        assert!(times.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(times.len(), 2);
    }

    #[test]
    fn multi_workspace_scope_excludes_open_records_outside_the_tree() {
        let (mut tree, room) = tree_and_room();
        let represented = RoomId::new();
        let records = RecordStore::in_memory();
        let first = action_record(&records, RecordId::for_file("first.md"), room, 20);
        let second = action_record(&records, RecordId::for_file("second.md"), represented, 30);
        let private = action_record(
            &records,
            RecordId::for_file("private.md"),
            RoomId::new(),
            40,
        );
        tree.room_records_held
            .entry(room)
            .or_default()
            .insert(first.id.to_string(), first.kind.to_string());
        tree.room_records_held
            .entry(represented)
            .or_default()
            .insert(second.id.to_string(), second.kind.to_string());

        let mut index = WorkspaceHistoryIndex::new();
        index.refresh(
            &mut tree,
            &records,
            WorkspaceHistoryQuery::for_rooms([room, represented]),
        );

        assert!(index.entries().iter().any(
            |row| matches!(&row.id.origin, HistoryOrigin::Record { id, .. } if id == &first.id)
        ));
        assert!(index.entries().iter().any(
            |row| matches!(&row.id.origin, HistoryOrigin::Record { id, .. } if id == &second.id)
        ));
        assert!(!index.entries().iter().any(
            |row| matches!(&row.id.origin, HistoryOrigin::Record { id, .. } if id == &private.id)
        ));
    }
}
