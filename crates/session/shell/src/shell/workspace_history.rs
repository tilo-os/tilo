//! The workspace history index, shared with the desktop.
//!
//! [`tilo_desktop::activity::index`] builds it; the tests here exercise it
//! against the native workspace tree and record store.
pub use tilo_desktop::activity::index::*;

#[cfg(test)]
mod tests {
    use crate::shell::state::FreshId as _;
    use std::cell::Cell;

    use automerge::ROOT;
    use automerge::transaction::{CommitOptions, Transactable};
    use ui::tilo_actions::ActionContext;

    use super::*;
    use crate::shell::state::{RoomId, Workspace, WorkspaceId, WorkspaceTree};
    use automerge::AutoCommit;
    use std::collections::{BTreeMap, BTreeSet};
    use ui::record::{RecordCheckout, RecordId, RecordKind, RecordStore};
    use ui::widget::HistoryEntryId;

    struct ColdRecords {
        documents: BTreeMap<RecordId, Vec<u8>>,
        loads: Cell<usize>,
    }

    impl PersistedRecordHistory for ColdRecords {
        fn load_record(&self, id: &RecordId) -> Option<AutoCommit> {
            self.loads.set(self.loads.get().saturating_add(1));
            AutoCommit::load(self.documents.get(id)?).ok()
        }
    }

    fn tree_and_room() -> (WorkspaceTree, RoomId) {
        let workspace = Workspace::new(WorkspaceId::new(), "Research");
        let room = workspace.room;
        let mut tree = WorkspaceTree {
            desktop: crate::shell::state::DesktopTree {
                active: workspace.id,
                workspaces: vec![workspace],
                ..Default::default()
            },
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
            let context = ActionContext::for_resource(checkout.id.as_str(), "note", "Decision log")
                .with_app("org.tilo.Notes", "Notes")
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn history_projection_joins_tree_and_room_records_with_provenance() {
        // Phase `joins_tree_and_room_records_with_action_provenance`.
        {
            let (mut tree, room) = tree_and_room();
            let records = RecordStore::in_memory();
            let kept = action_record(&records, RecordId::for_file("launch.md"), room, 20);
            let other = action_record(
                &records,
                RecordId::for_file("private.md"),
                RoomId::new(),
                30,
            );
            tree.desktop
                .room_records_held
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

        // Phase `refresh_is_cached_and_projected_ids_include_document_identity`.
        {
            let (mut tree, room) = tree_and_room();
            let records = RecordStore::in_memory();
            let first = action_record(&records, RecordId::for_tab(1), room, 20);
            let second = action_record(&records, RecordId::for_tab(2), room, 20);
            for record in [&first, &second] {
                tree.desktop
                    .room_records_held
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
                row.id.origin == HistoryOrigin::DesktopTree
                    && row.timeline_entry().id == row.id.change
            }));
        }

        // Phase `history_observation_preserves_pending_record_action_and_attribution`.
        {
            let (mut tree, room) = tree_and_room();
            let records = RecordStore::in_memory();
            let record = action_record(&records, RecordId::for_tab(72), room, 20);
            tree.desktop
                .room_records_held
                .entry(room)
                .or_default()
                .insert(record.id.to_string(), record.kind.to_string());
            let query = WorkspaceHistoryQuery::for_room(room);
            let mut index = WorkspaceHistoryIndex::new();
            assert!(index.refresh(&mut tree, &records, query.clone()));
            let committed_entries = index.entries().to_vec();
            let mut frontier = WorkspaceSeenFrontier::default();
            frontier.mark_seen(&index);
            let before_heads = records.read(&record.id, AutoCommit::base_heads).unwrap();

            // This is an explicit legacy action rather than an internal component
            // ActionFrame; its owner commits the whole edit with provenance below.
            records.with(&record.id, |doc| {
                doc.put(ROOT, "body", "An unfinished edit").unwrap();
            });
            let pending = records.read(&record.id, Transactable::pending_ops).unwrap();
            assert!(pending > 0);
            tree.doc.put(ROOT, "status", "unfinished").unwrap();
            let pending_tree = tree.doc.pending_ops();
            assert!(!index.refresh(&mut tree, &records, query.clone()));
            // Also force projection, rather than only exercising the cache hit.
            index.authors_mut();
            // A reader may hold the live document while history projects it. Any
            // mutable RecordStore loan here would finish a component action and
            // also conflict with this deliberately retained immutable loan.
            records
                .read(&record.id, |_| {
                    assert!(index.refresh(&mut tree, &records, query.clone()));
                    assert!(index.unseen(&frontier, &mut tree, &records).is_empty());
                })
                .unwrap();
            assert_eq!(index.entries(), committed_entries);
            assert!(index.unseen(&frontier, &mut tree, &records).is_empty());
            assert_eq!(tree.doc.pending_ops(), pending_tree);
            assert_eq!(
                records.read(&record.id, |doc| (doc.base_heads(), doc.pending_ops())),
                Some((before_heads.clone(), pending)),
            );

            let context = ActionContext::for_resource(record.id.as_str(), "note", "Decision log")
                .with_purpose("Finished the whole edit");
            let message = context.change_message().unwrap();
            let committed = records
                .with(&record.id, |doc| {
                    doc.put(ROOT, "body", "The completed edit").unwrap();
                    doc.commit_with(CommitOptions::default().with_time(40).with_message(message))
                        .unwrap()
                })
                .unwrap();
            assert!(index.refresh(&mut tree, &records, query));
            let unseen = index.unseen(&frontier, &mut tree, &records);
            assert_eq!(unseen.len(), 1);
            assert_eq!(
                unseen[0].id.change,
                ui::history_source::entry_id_from_hash(&committed.to_string())
            );
            assert_eq!(
                unseen[0].entry.context.as_ref().unwrap().purpose.as_deref(),
                Some("Finished the whole edit")
            );
            records
                .read(&record.id, |doc| {
                    let mut settled = doc.clone();
                    settled.rollback();
                    let changes = settled.get_changes(&before_heads);
                    assert_eq!(changes.len(), 1, "observation must not split the action");
                    assert_eq!(changes[0].hash(), committed);
                })
                .unwrap();
        }

        // Phase `reports_referenced_documents_that_are_not_local`.
        {
            let (mut tree, room) = tree_and_room();
            let missing = RecordCheckout::new(RecordId::minted(), RecordKind::new("kanban"));
            tree.desktop
                .room_records_held
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

        // Phase `multi_workspace_scope_excludes_open_records_outside_the_tree`.
        {
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
            tree.desktop
                .room_records_held
                .entry(room)
                .or_default()
                .insert(first.id.to_string(), first.kind.to_string());
            tree.desktop
                .room_records_held
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

    #[test]
    fn cold_history_indexes_without_the_record_store_and_marks_corruption() {
        // Phase `indexes_cold_local_history_without_opening_the_record_store`.
        {
            let (mut tree, room) = tree_and_room();
            let records = RecordStore::in_memory();
            let checkout =
                RecordCheckout::new(RecordId::for_file("cold.md"), RecordKind::new("note"));
            let mut doc = AutoCommit::new();
            ui::record::init(&mut doc, &checkout.id, &checkout.kind, &room.to_string());
            doc.put(&ROOT, "body", "Persisted but not mounted").unwrap();
            doc.commit_with(CommitOptions::default().with_time(25));
            let cold = ColdRecords {
                documents: [(checkout.id.clone(), doc.save())].into_iter().collect(),
                loads: Cell::new(0),
            };
            tree.desktop
                .room_records_held
                .entry(room)
                .or_default()
                .insert(checkout.id.to_string(), checkout.kind.to_string());

            assert!(records.open_records().is_empty());
            let mut index = WorkspaceHistoryIndex::new();
            index.refresh_with_local_records(
                &mut tree,
                &records,
                &cold,
                WorkspaceHistoryQuery::for_room(room),
            );

            assert!(records.open_records().is_empty());
            assert!(cold.loads.get() > 0);
            assert!(index.coverage().is_complete());
            assert!(index.entries().iter().any(|row| {
                matches!(&row.id.origin, HistoryOrigin::Record { id, .. } if id == &checkout.id)
            }));
        }

        // Phase `corrupt_cold_record_remains_explicitly_unavailable`.
        {
            let (mut tree, room) = tree_and_room();
            let records = RecordStore::in_memory();
            let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
            let cold = ColdRecords {
                documents: [(checkout.id.clone(), b"not automerge".to_vec())]
                    .into_iter()
                    .collect(),
                loads: Cell::new(0),
            };
            tree.desktop
                .room_records_held
                .entry(room)
                .or_default()
                .insert(checkout.id.to_string(), checkout.kind.to_string());

            let mut index = WorkspaceHistoryIndex::new();
            index.refresh_with_local_records(
                &mut tree,
                &records,
                &cold,
                WorkspaceHistoryQuery::for_room(room),
            );
            assert_eq!(index.coverage().unavailable_records, vec![checkout]);
            assert!(records.open_records().is_empty());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn causal_frontier_detects_persists_and_unions_changes() {
        // Phase `causal_frontier_detects_new_record_changes_and_scoped_tree_exceptions`.
        {
            let (mut tree, room) = tree_and_room();
            let records = RecordStore::in_memory();
            let record = action_record(&records, RecordId::for_tab(44), room, 20);
            tree.desktop
                .room_records_held
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
                    .any(|row| row.id.origin == HistoryOrigin::DesktopTree)
            );
            assert!(unseen.iter().any(
                |row| matches!(&row.id.origin, HistoryOrigin::Record { id, .. } if id == &record.id)
            ));
        }

        // Phase `causal_frontier_persistence_round_trips_full_heads`.
        {
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

        // Phase `mark_seen_retains_an_origin_omitted_from_the_current_index`.
        {
            let (mut tree, room) = tree_and_room();
            let records = RecordStore::in_memory();
            let mut index = WorkspaceHistoryIndex::new();
            index.refresh(&mut tree, &records, WorkspaceHistoryQuery::for_room(room));
            let mut frontier = WorkspaceSeenFrontier::default();
            frontier.documents.insert(
                "record:uuid:temporarily-unavailable".to_string(),
                DocumentSeenFrontier {
                    heads: vec!["11".repeat(32)],
                    exceptions: BTreeSet::new(),
                },
            );
            frontier.mark_seen(&index);
            assert_eq!(
                frontier.documents["record:uuid:temporarily-unavailable"].heads,
                ["11".repeat(32)]
            );
        }

        // Phase `bounded_room_projection_unions_prior_tree_exceptions`.
        {
            let (mut tree, room) = tree_and_room();
            let records = RecordStore::in_memory();
            let query = WorkspaceHistoryQuery {
                room: Some(room),
                limit: 1,
                ..WorkspaceHistoryQuery::default()
            };
            let mut index = WorkspaceHistoryIndex::new();
            index.refresh(&mut tree, &records, query.clone());
            let mut frontier = WorkspaceSeenFrontier::default();
            frontier.mark_seen(&index);
            let before = frontier.documents["workspace-tree"].exceptions.clone();
            assert_eq!(before.len(), 1);

            tree.doc.put(&ROOT, "status", "new").unwrap();
            tree.doc.commit_with(CommitOptions::default().with_time(50));
            index.refresh(&mut tree, &records, query);
            frontier.mark_seen(&index);
            let after = &frontier.documents["workspace-tree"].exceptions;
            assert_eq!(after.len(), 2);
            assert!(before.is_subset(after));
        }
    }

    #[test]
    fn history_ranges_stay_contiguous_and_aggregate_oldest_first() {
        // Phase `named_range_cannot_absorb_an_interleaved_foreign_record`.
        {
            let (mut tree, room) = tree_and_room();
            let records = RecordStore::in_memory();
            let record = action_record(&records, RecordId::for_tab(55), room, 20);
            tree.desktop
                .room_records_held
                .entry(room)
                .or_default()
                .insert(record.id.to_string(), record.kind.to_string());
            tree.doc.put(&ROOT, "status", "done").unwrap();
            tree.doc.commit_with(CommitOptions::default().with_time(30));
            let mut index = WorkspaceHistoryIndex::new();
            index.refresh(&mut tree, &records, WorkspaceHistoryQuery::for_room(room));
            let tree_ids: Vec<_> = index
                .entries()
                .iter()
                .filter(|row| row.id.origin == HistoryOrigin::DesktopTree)
                .map(|row| row.timeline_entry().id)
                .collect();
            assert!(tree_ids.len() >= 2);
            let range = ui::NamedRange {
                id: "tree-band".into(),
                label: "Workspace work".into(),
                start: tree_ids[0],
                end: tree_ids.last().copied(),
                named_by: None,
                created: std::time::SystemTime::UNIX_EPOCH,
            };
            assert!(index.origin_safe_named_ranges(&[range]).is_empty());
        }

        // Phase `aggregate_is_oldest_first_while_retaining_the_newest_bound`.
        {
            let (mut tree, room) = tree_and_room();
            let records = RecordStore::in_memory();
            for (name, time) in [("older.md", 10), ("newer.md", 20)] {
                let record = action_record(&records, RecordId::for_file(name), room, time);
                tree.desktop
                    .room_records_held
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
    }
}
