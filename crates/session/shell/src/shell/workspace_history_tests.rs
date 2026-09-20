#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use automerge::ROOT;
    use automerge::transaction::{CommitOptions, Transactable};
    use ui::tilo_actions::ActionContext;

    use super::*;
    use crate::shell::state::{Workspace, WorkspaceId};

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
            desktop: qos_desktop::WorkspaceTree {
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
    fn indexes_cold_local_history_without_opening_the_record_store() {
        let (mut tree, room) = tree_and_room();
        let records = RecordStore::in_memory();
        let checkout = RecordCheckout::new(RecordId::for_file("cold.md"), RecordKind::new("note"));
        let mut doc = AutoCommit::new();
        ui::record::init(&mut doc, &checkout.id, &checkout.kind, &room.to_string());
        doc.put(&ROOT, "body", "Persisted but not mounted").unwrap();
        doc.commit_with(CommitOptions::default().with_time(25));
        let cold = ColdRecords {
            documents: [(checkout.id.clone(), doc.save())].into_iter().collect(),
            loads: Cell::new(0),
        };
        tree.room_records_held
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

    #[test]
    fn corrupt_cold_record_remains_explicitly_unavailable() {
        let (mut tree, room) = tree_and_room();
        let records = RecordStore::in_memory();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let cold = ColdRecords {
            documents: [(checkout.id.clone(), b"not automerge".to_vec())]
                .into_iter()
                .collect(),
            loads: Cell::new(0),
        };
        tree.room_records_held
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
    fn mark_seen_retains_an_origin_omitted_from_the_current_index() {
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

    #[test]
    fn bounded_room_projection_unions_prior_tree_exceptions() {
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

    #[test]
    fn named_range_cannot_absorb_an_interleaved_foreign_record() {
        let (mut tree, room) = tree_and_room();
        let records = RecordStore::in_memory();
        let record = action_record(&records, RecordId::for_tab(55), room, 20);
        tree.room_records_held
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
            .filter(|row| row.id.origin == HistoryOrigin::WorkspaceTree)
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
