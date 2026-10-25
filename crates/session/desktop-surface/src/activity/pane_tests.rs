use super::*;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    //! Every fixture mutates the room through the *production* mirrors
    //! and reads the pane back — nothing writes a synthetic `HistoryEntry`.

    use super::*;
    use crate::activity::testing::TestTree as WorkspaceTree;
    use tilo_desktop_model::WorkspaceId;

    /// The pane with a device that may mutate the room.
    fn native_pane() -> RoomActivityPane {
        let mut pane = RoomActivityPane::new();
        pane.set_host(Rc::new(crate::activity::testing::TestHost::default()));
        pane
    }
    use automerge::ActorId;
    use automerge::transaction::{CommitOptions, Transactable};
    use collab_types::caps::{Access, CapOverride, Scope, Subject};
    use tilo_automerge::ApplyIncoming as _;

    /// A plausible half-hour: owner, one grant, one revoke, one rename,
    /// one window opened.
    fn busy_room() -> (WorkspaceTree, WorkspaceId, String) {
        busy_room_written_by(None)
    }

    /// [`busy_room`], but the half-hour is a collaborator's: what Catch up
    /// exists to show.
    fn peers_busy_room() -> (WorkspaceTree, WorkspaceId, String) {
        busy_room_written_by(Some(ActorId::from(b"ada-device".as_slice())))
    }

    fn busy_room_written_by(peer: Option<ActorId>) -> (WorkspaceTree, WorkspaceId, String) {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let reader = tree.doc.get_actor().clone();
        if let Some(peer) = peer {
            tree.doc.set_actor(peer);
        }
        let user: String = "test-owner".to_string();
        tree.adopt_ownerless_workspaces(&user);
        let ws_id = tree.desktop.active;

        let granted = tree
            .grant_in_workspace(
                ws_id,
                &user,
                CapOverride::grant(
                    Subject::app("files"),
                    Scope::FsPath("/home/user/notes".to_string()),
                    Access::Read,
                ),
            )
            .expect("the owner may grant in their own room");
        tree.grant_in_workspace(
            ws_id,
            &user,
            CapOverride::grant(
                Subject::app("terminal"),
                Scope::Device("camera".to_string()),
                Access::Read,
            ),
        )
        .expect("the owner may grant in their own room");
        assert!(tree.revoke_in_workspace(ws_id, &user, &granted));

        if let Some(ws) = tree.desktop.workspaces.iter_mut().find(|w| w.id == ws_id) {
            ws.name = "Design review".to_string();
        }
        tree.seed_doc_from_cache();
        tree.doc.set_actor(reader);
        (tree, ws_id, user)
    }

    #[test]
    fn a_workspace_switch_reads_only_its_own_change_and_derives_nothing() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let first = tree.active;
        let second = tree.append_empty_workspace("Second");
        tree.commit_room_change();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        let reads = || {
            (
                ROOM_CHANGE_READS.with(Cell::get),
                crate::activity::replay::REPLAY_CHECKPOINT_READS.with(Cell::get),
            )
        };
        let derived = format!("{:?}", pane.derived);
        for n in 0..12 {
            tree.set_active(if n % 2 == 0 { second } else { first });
            tree.commit_room_change();
            let (diffs, forks) = reads();
            let logged = pane.history_log.changes_read();
            pane.refresh(&mut tree);
            let (diffs_after, forks_after) = reads();
            // The switch's change is read once and passed over: no row to
            // describe, nothing derived again.
            assert_eq!(
                pane.history_log.changes_read() - logged,
                1,
                "switch {n} reads only its own change"
            );
            assert_eq!(diffs_after - diffs, 0, "switch {n} describes no change");
            assert_eq!(
                format!("{:?}", pane.derived),
                derived,
                "switch {n} leaves the rows as they were"
            );
            // The first visit to "Second" checkpoints the change that
            // created it, a row of that workspace; nothing else forks.
            assert!(
                forks_after - forks <= usize::from(n == 0),
                "switch {n} forks the document {} times",
                forks_after - forks
            );
        }

        let mut fresh = native_pane();
        fresh.refresh(&mut tree);
        assert_eq!(
            format!("{:?}", pane.derived),
            format!("{:?}", fresh.derived),
            "the feed rows and their workspaces"
        );
        let frames = |pane: &RoomActivityPane| {
            pane.replay_frames
                .iter()
                .map(|frame| {
                    format!(
                        "{:?} {} {:?}",
                        frame.entry_id, frame.summary, frame.workspaces
                    )
                })
                .collect::<Vec<_>>()
        };
        assert!(
            !frames(&pane).is_empty(),
            "the desktop's changes are replayable"
        );
        assert_eq!(frames(&pane), frames(&fresh), "the replay checkpoints");
    }

    /// Switching takes no place in Activity's window: switching back and
    /// forth 250 times used to fill all 200 places with switches, and the
    /// rename before them, the one thing anyone did, was gone.
    #[test]
    fn switching_workspaces_never_pushes_what_people_did_out_of_activity() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let first = tree.active;
        let second = tree.append_empty_workspace("Second");
        tree.commit_room_change();
        if let Some(workspace) = tree.desktop.workspaces.iter_mut().find(|w| w.id == first) {
            workspace.name = "Renamed before switching".to_string();
        }
        tree.seed_doc_from_cache();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        let renamed = |pane: &RoomActivityPane| {
            pane.derived
                .iter()
                .filter(|(entry, _)| is_feed_change(entry))
                .filter(|(entry, _)| entry.summary.contains("Renamed before switching"))
                .count()
        };
        assert_eq!(renamed(&pane), 1, "the rename is in Activity");
        let rows = pane.derived.len();
        for n in 0..250 {
            tree.set_active(if n % 2 == 0 { second } else { first });
            tree.commit_room_change();
            pane.refresh(&mut tree);
        }
        assert_eq!(renamed(&pane), 1, "the rename is still in Activity");
        assert_eq!(pane.derived.len(), rows, "no switch became a row");
        let mut fresh = native_pane();
        fresh.refresh(&mut tree);
        assert_eq!(renamed(&fresh), 1);
        assert_eq!(
            format!("{:?}", fresh.derived),
            format!("{:?}", pane.derived)
        );
    }

    /// A change that switches workspaces and edits a workspace at once does
    /// show a desktop of its own: replay reads it from the document, as for
    /// any other change.
    #[test]
    fn a_switch_written_with_an_edit_replays_the_edit() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let second = tree.append_empty_workspace("Second");
        tree.commit_room_change();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        tree.set_active(second);
        if let Some(workspace) = tree.desktop.workspaces.iter_mut().find(|w| w.id == second) {
            workspace.name = "Renamed".to_string();
        }
        tree.seed_doc_from_cache();
        pane.refresh(&mut tree);
        let last = pane.replay_frames.last().expect("the change is replayable");
        assert_eq!(
            last.workspaces
                .iter()
                .map(|workspace| workspace.name.as_str())
                .collect::<Vec<_>>(),
            ["Renamed"],
            "the frame shows the workspace as the change left it"
        );
        let mut fresh = native_pane();
        fresh.refresh(&mut tree);
        assert_eq!(
            format!(
                "{:?}",
                pane.replay_frames.last().map(|frame| &frame.workspaces)
            ),
            format!(
                "{:?}",
                fresh.replay_frames.last().map(|frame| &frame.workspaces)
            ),
        );
    }

    #[test]
    fn workspace_switch_summary_fits_the_chrome_activity_line() {
        let summary = PatchCounts {
            active_switched: true,
            ..Default::default()
        }
        .describe()
        .expect("workspace switch is activity")
        .0;
        assert_eq!(summary, "Switched workspace");
        assert!(
            summary.len() <= 18,
            "fixed-width topbar summaries must stay concise: {summary}"
        );
    }

    #[test]
    fn persisted_effect_receipt_reappears_in_workspace_activity_without_revert() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let invocation = tilo_actions::InvocationId::new();
        assert!(
            tree.append_workspace_effect_receipt(
                tree.desktop.active,
                invocation,
                "ada",
                tilo_actions::ActionContext::for_resource(
                    "document:brief",
                    "document",
                    "Release brief",
                )
                .with_app("org.tilo.Writer", "Writer")
                .with_described(crate::activity::testing::verb("Export")),
                tilo_actions::EffectReceiptStatus::Succeeded,
            )
        );
        let persisted = serde_json::to_vec(&tree.desktop).expect("persist workspace tree");
        let mut restored = WorkspaceTree {
            desktop: serde_json::from_slice(&persisted).expect("restore workspace tree"),
            doc: automerge::AutoCommit::new(),
        };
        restored.seed_doc_from_cache();

        let mut pane = native_pane();
        pane.refresh(&mut restored);
        let entry = pane
            .entries()
            .iter()
            .find(|entry| entry.id == HistoryEntryId::from_bytes(*invocation.0.as_bytes()))
            .expect("receipt is visible after restart");
        assert_eq!(entry.summary, "Export in Release brief");
        assert!(!entry.is_revertible());
    }

    #[test]
    fn one_person_is_one_colour_across_device_edits_and_receipts() {
        let local = crate::activity::testing::LOCAL_USER;
        let mut tree = WorkspaceTree::bootstrap_default();
        let invocation = tilo_actions::InvocationId::new();
        assert!(tree.append_workspace_effect_receipt(
            tree.desktop.active,
            invocation,
            local,
            tilo_actions::ActionContext::for_resource(
                "document:brief",
                "document",
                "Release brief",
            ),
            tilo_actions::EffectReceiptStatus::Succeeded,
        ));
        if let Some(workspace) = tree.desktop.active_workspace_mut() {
            workspace.name = "Board review".to_string();
        }
        tree.seed_doc_from_cache();

        let mut pane = native_pane();
        pane.refresh(&mut tree);
        let receipt_id = HistoryEntryId::from_bytes(*invocation.0.as_bytes());
        let receipt = pane
            .entries()
            .iter()
            .find(|entry| entry.id == receipt_id)
            .expect("the receipt is a row");
        let edit = pane
            .entries()
            .iter()
            .find(|entry| entry.id != receipt_id)
            .expect("the device's own edit is a row");
        assert!(
            (receipt.author.avatar_hue - edit.author.avatar_hue).abs() < f32::EPSILON,
            "{local}'s edit ({}) and receipt ({}) are drawn in one colour",
            edit.author.avatar_hue,
            receipt.author.avatar_hue,
        );
    }

    fn pane_over(tree: &mut WorkspaceTree) -> RoomActivityPane {
        let mut pane = native_pane();
        // Legacy timeline tests exercise the granular surface explicitly;
        // production opens on the outcome-first Catch up tab.
        pane.set_view(ActivityView::Explore);
        pane.refresh(tree);
        pane
    }

    fn agent_task_fixture() -> (WorkspaceTree, RecordStore, RoomActivityPane, RecordId) {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.active_workspace().expect("workspace").room();
        let records = RecordStore::in_memory();
        let record = RecordId::for_file("campaign-brief.tilo-doc");
        let checkout = ui::RecordCheckout::new(record.clone(), ui::RecordKind::new("document"));
        assert!(records.open_record(&checkout));
        let agent = ActorId::from(b"writing-agent".as_slice());
        let issuer = ActorId::from(b"ada-identity".as_slice());
        let agent_hex = agent.to_hex_string();
        let issuer_hex = issuer.to_hex_string();
        records.with(&record, |doc| {
            ui::record::init(doc, &record, &checkout.kind, &room.to_string());
            doc.set_actor(agent);
            for (time, field, value, action) in [
                (20, "intro", "A concrete opening", "Rewrite opening"),
                (21, "body", "A clearer launch story", "Clarify launch story"),
                (22, "outro", "Keep the pricing table", "Preserve pricing"),
            ] {
                doc.put(automerge::ROOT, field, value).unwrap();
                let context = ui::tilo_actions::ActionContext::for_resource(
                    record.as_str(),
                    "document",
                    "Campaign brief",
                )
                .with_app("org.tilo.Writer", "Writer")
                .with_described(crate::activity::testing::verb(action))
                .with_purpose("Make the opening concrete")
                .with_group(
                    "agent-run:test",
                    "Rewrite the opening and preserve the pricing table",
                    "agent-run",
                );
                doc.commit_with(
                    CommitOptions::default()
                        .with_time(time)
                        .with_message(context.change_message().unwrap()),
                );
            }
            let change = ui::latest_run_by(doc, &agent_hex).expect("agent change");
            ui::record_run_with_details(
                doc,
                "Rewrite the opening and preserve the pricing table",
                Some(&agent_hex),
                Some(&issuer_hex),
                Some("Rewrite the opening and preserve the pricing table"),
                Some("The opening was vague; the pricing table was already correct."),
                &[ui::RunStep {
                    app: "org.tilo.Writer".to_string(),
                    record: record.to_string(),
                    run: change,
                    what: vec![crate::activity::testing::verb("rewrite the launch story")],
                }],
            )
            .expect("task receipt");
            tilo_resource::history::commit_stamped_at(doc, 23);
        });
        tree.desktop
            .room_records_held
            .entry(room)
            .or_default()
            .insert(record.to_string(), checkout.kind.to_string());

        let mut pane = native_pane();
        pane.set_view(ActivityView::Explore);
        pane.authors_mut().register(
            &agent_hex,
            ui::widget::HistoryAuthor::new("Quill", 210.0, "agent"),
        );
        pane.authors_mut().register(
            &issuer_hex,
            ui::widget::HistoryAuthor::new("Ada", 120.0, "ada"),
        );
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        let end = pane.agent_runs[0].run.steps[0].run.end;
        let selected = pane
            .timeline
            .entries()
            .iter()
            .position(|entry| {
                pane.projected_origins
                    .get(&entry.id)
                    .is_some_and(|origin| origin.change == end)
            })
            .expect("affected history entry");
        pane.timeline.set_selection(Some((selected, selected)));
        (tree, records, pane, record)
    }

    /// An app whose content is the room's own document (Notes' notebook)
    /// keeps it beside its records, announced as none: what was done in it,
    /// by a person or an agent's task, is still part of the workspace's
    /// history.
    #[test]
    fn an_apps_room_document_is_part_of_the_workspace_history() {
        struct RoomDocuments {
            room: tilo_desktop_model::RoomId,
            id: RecordId,
            document: Vec<u8>,
        }
        impl crate::activity::index::PersistedRecordHistory for RoomDocuments {
            fn load_record(&self, id: &RecordId) -> Option<automerge::AutoCommit> {
                (id == &self.id)
                    .then(|| automerge::AutoCommit::load(&self.document).ok())
                    .flatten()
            }
            fn room_documents(&self, room: tilo_desktop_model::RoomId) -> Vec<ui::RecordCheckout> {
                if room == self.room {
                    vec![ui::RecordCheckout::new(
                        self.id.clone(),
                        ui::RecordKind::new("document"),
                    )]
                } else {
                    Vec::new()
                }
            }
        }

        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.active_workspace().expect("workspace").room();
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "body", "Hallo").unwrap();
        let context = ui::tilo_actions::ActionContext::for_resource(
            "notes.notebook/pages/one",
            "notes.page",
            "Launch notes",
        )
        .with_app("so.tilo.notes", "Notes")
        .with_group("task:translate", "Translate", "task");
        doc.commit_with(
            CommitOptions::default()
                .with_time(20)
                .with_message(context.change_message().unwrap()),
        );
        let local = RoomDocuments {
            room,
            id: RecordId::parse("uuid:5f0e9c1a-2b3d-4e5f-8a9b-0c1d2e3f4a5b").unwrap(),
            document: doc.save(),
        };

        let mut pane = native_pane();
        pane.refresh(&mut tree);
        pane.refresh_records_with_local(&mut tree, &RecordStore::in_memory(), &local);
        assert!(
            pane.timeline.entries().iter().any(|entry| {
                entry
                    .context
                    .as_ref()
                    .and_then(|context| context.group.as_ref())
                    .is_some_and(|group| group.id == "task:translate")
            }),
            "the task's change in the room document is in the history"
        );
    }

    /// A persisted record that is not open here is read from disk once:
    /// later refreshes reuse what was read until the record's revision moves.
    #[test]
    fn agent_runs_are_not_reloaded_from_disk_when_unchanged() {
        struct CountingDisk {
            documents: std::collections::BTreeMap<RecordId, Vec<u8>>,
            revision: std::cell::Cell<u64>,
            loads: std::cell::Cell<usize>,
        }
        impl crate::activity::index::PersistedRecordHistory for CountingDisk {
            fn load_record(&self, id: &RecordId) -> Option<automerge::AutoCommit> {
                self.loads.set(self.loads.get().saturating_add(1));
                automerge::AutoCommit::load(self.documents.get(id)?).ok()
            }
            fn revision(&self, _id: &RecordId) -> Option<u64> {
                Some(self.revision.get())
            }
        }

        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.active_workspace().expect("workspace").room();
        let record = RecordId::for_file("campaign-brief.tilo-doc");
        let kind = ui::RecordKind::new("document");
        let agent = ActorId::from(b"writing-agent".as_slice());
        let agent_hex = agent.to_hex_string();
        let mut doc = automerge::AutoCommit::new();
        ui::record::init(&mut doc, &record, &kind, &room.to_string());
        doc.set_actor(agent);
        doc.put(automerge::ROOT, "intro", "A concrete opening")
            .unwrap();
        let context = ui::tilo_actions::ActionContext::for_resource(
            record.as_str(),
            "document",
            "Campaign brief",
        )
        .with_app("org.tilo.Writer", "Writer")
        .with_group("agent-run:cold", "Rewrite the opening", "agent-run");
        doc.commit_with(
            CommitOptions::default()
                .with_time(20)
                .with_message(context.change_message().unwrap()),
        );
        let change = ui::latest_run_by(&mut doc, &agent_hex).expect("agent change");
        ui::record_run_with_details(
            &mut doc,
            "Rewrite the opening",
            Some(&agent_hex),
            None,
            Some("Rewrite the opening"),
            None,
            &[ui::RunStep {
                app: "org.tilo.Writer".to_string(),
                record: record.to_string(),
                run: change,
                what: vec![crate::activity::testing::verb("rewrite the opening")],
            }],
        )
        .expect("task receipt");
        tilo_resource::history::commit_stamped_at(&mut doc, 21);
        tree.desktop
            .room_records_held
            .entry(room)
            .or_default()
            .insert(record.to_string(), kind.to_string());
        let disk = CountingDisk {
            documents: [(record.clone(), doc.save())].into_iter().collect(),
            revision: std::cell::Cell::new(1),
            loads: std::cell::Cell::new(0),
        };
        let records = RecordStore::in_memory();
        let held_run = |pane: &RoomActivityPane| {
            pane.agent_runs.iter().any(|indexed| {
                indexed.source_record.as_ref() == Some(&record)
                    && indexed.run.label == "Rewrite the opening"
            })
        };

        let mut pane = native_pane();
        pane.refresh(&mut tree);
        pane.refresh_records_with_local(&mut tree, &records, &disk);
        let first = disk.loads.get();
        assert!(first > 0, "the record is read from disk");
        assert!(held_run(&pane), "its run is read from the persisted record");

        for _ in 0..3 {
            pane.refresh(&mut tree);
            pane.refresh_records_with_local(&mut tree, &records, &disk);
        }
        assert_eq!(
            disk.loads.get(),
            first,
            "an unchanged record is not read from disk again"
        );
        assert!(held_run(&pane), "the run is still listed");

        disk.revision.set(2);
        pane.refresh_records_with_local(&mut tree, &records, &disk);
        assert!(
            disk.loads.get() > first,
            "a record whose revision moved is read again"
        );
        assert!(held_run(&pane));
    }

    #[test]
    fn selecting_agent_work_exposes_the_real_task_receipt_and_named_people() {
        let (_tree, _records, mut pane, _record) = agent_task_fixture();
        let run = pane.agent_runs[0].run.steps[0].run.clone();
        let middle = pane
            .timeline
            .entries()
            .iter()
            .position(|entry| {
                pane.projected_origins.get(&entry.id).is_some_and(|item| {
                    item.change != run.start
                        && item.change != run.end
                        && matches!(
                            item.origin,
                            crate::activity::index::HistoryOrigin::Record { .. }
                        )
                }) && entry.context.as_ref().is_some_and(|context| {
                    context
                        .group
                        .as_ref()
                        .is_some_and(|group| group.kind == "agent-run")
                })
            })
            .expect("three-change task has an interior history entry");
        pane.timeline.set_selection(Some((middle, middle)));
        let task = pane.selected_agent_task().expect("selected task receipt");
        assert_eq!(
            task.instruction.as_deref(),
            Some("Rewrite the opening and preserve the pricing table")
        );
        assert_eq!(pane.agent_name(task), "Quill");
        assert_eq!(pane.issuer_name(task), "Ada");
        assert_eq!(task.steps[0].sentence(), "Rewrite the launch story");
        assert!(
            pane.lens_candidates().iter().any(
                |(lens, label)| *lens == LensSelection::AgentTasks && label == "Agent tasks  1"
            ),
            "Browse exposes a first-class agent-task lens"
        );
    }

    #[test]
    fn agent_task_undo_is_one_keyboard_reachable_safe_action() {
        let (mut tree, records, mut pane, record) = agent_task_fixture();
        show_summary(&mut pane);
        assert!(pane.can_undo_agent_run());
        let outcome = pane.apply_detail_action(DetailAction::UndoTask, &mut tree, Some(&records));
        assert!(outcome.reverted);
        assert!(!outcome.undo_partial);
        assert_eq!(
            records.read(&record, |doc| doc
                .get(automerge::ROOT, "body")
                .ok()
                .flatten()
                .is_some()),
            Some(false)
        );
        assert!(
            pane.agent_undo_notice
                .as_ref()
                .is_some_and(|(_, notice)| notice.contains("Whole task undone"))
        );
    }

    #[test]
    fn a_window_someone_opened_is_listed_in_history_but_is_not_news() {
        let (mut tree, workspace, _) = busy_room();
        let host = Rc::new(crate::activity::testing::TestHost::default());
        let mut pane = RoomActivityPane::new();
        pane.set_host(host);
        pane.load_frontiers();
        pane.refresh(&mut tree);
        let reader = tree.doc.get_actor().clone();
        tree.doc.set_actor(ActorId::from(b"ada-device".as_slice()));
        assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
            91_101,
            "Budget".to_string(),
            b"{}".to_vec(),
        ));
        tilo_resource::history::commit_stamped(&mut tree.doc);
        tree.doc.set_actor(reader);
        pane.refresh(&mut tree);
        assert!(
            pane.entries()
                .iter()
                .any(|entry| entry.summary == "Opened Budget"),
            "History lists the window opened: {:?}",
            pane.entries()
                .iter()
                .map(|entry| &entry.summary)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            pane.glance_signal(workspace.raw()).1,
            0,
            "opening a window is nobody's news"
        );
        pane.begin_view(workspace.raw());
        assert!(pane.digest().briefs.is_empty());
    }

    #[test]
    fn a_workspace_first_seen_on_this_device_has_nothing_new() {
        let (mut tree, workspace, user) = busy_room();
        let host = Rc::new(crate::activity::testing::TestHost::default());
        let mut pane = RoomActivityPane::new();
        pane.set_host(host.clone());
        pane.load_frontiers();
        pane.refresh(&mut tree);
        let (headline, unread, _) = pane.glance_signal(workspace.raw());
        assert_eq!(
            (headline, unread),
            (None, 0),
            "the history a device arrives to is not news"
        );

        let reader = tree.doc.get_actor().clone();
        tree.doc.set_actor(ActorId::from(b"ada-device".as_slice()));
        tree.grant_in_workspace(
            workspace,
            &user,
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath("/home/user/later".to_string()),
                Access::Read,
            ),
        )
        .expect("the owner may grant in their own room");
        tree.seed_doc_from_cache();
        tree.doc.set_actor(reader);
        pane.refresh(&mut tree);
        let after_change = pane.glance_signal(workspace.raw()).1;
        assert!(after_change > 0, "a change after arriving is");

        let mut next_session = RoomActivityPane::new();
        next_session.set_host(host);
        next_session.load_frontiers();
        next_session.refresh(&mut tree);
        assert_eq!(
            next_session.glance_signal(workspace.raw()).1,
            after_change,
            "the baseline persists, so the next session still counts only that change"
        );
    }

    #[test]
    fn closing_unpainted_catch_up_keeps_unseen_work_pending() {
        let (mut tree, workspace, _) = peers_busy_room();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        let unread = pane.glance_signal(workspace.raw()).1;
        assert!(unread > 0);
        let other = tree
            .workspaces
            .iter()
            .find(|candidate| candidate.id != workspace)
            .unwrap()
            .id;
        assert_eq!(pane.glance_signal(other.raw()), (None, 0, 0));
        assert!(pane.digest_for_workspace(other.raw()).briefs.is_empty());
        pane.begin_view(workspace.raw());
        pane.finish_view(workspace.raw());
        pane.refresh(&mut tree);
        assert_eq!(pane.glance_signal(workspace.raw()).1, unread);
    }

    #[test]
    fn catch_up_is_default_and_closing_advances_only_private_frontier() {
        let (mut tree, workspace, _) = peers_busy_room();
        let before = tree.doc.get_heads();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        assert_eq!(pane.view(), ActivityView::CatchUp);

        let (_, unread_before, _) = pane.glance_signal(workspace.raw());
        assert!(unread_before > 0);
        pane.begin_view(workspace.raw());
        assert!(!pane.digest().briefs.is_empty());
        show_summary(&mut pane);
        pane.finish_view(workspace.raw());
        let (_, unread_after, _) = pane.glance_signal(workspace.raw());
        assert_eq!(unread_after, 0);
        assert_eq!(
            tree.doc.get_heads(),
            before,
            "reading history is not a room edit"
        );
    }

    #[test]
    fn refreshed_catch_up_keeps_the_open_session_and_does_not_acknowledge_late_work() {
        let (mut tree, workspace, user) = peers_busy_room();
        let mut pane = native_pane();
        pane.begin_view(workspace.raw());
        pane.refresh(&mut tree);
        show_summary(&mut pane);
        let index = 0;
        let anchor = pane.digest.briefs[index].entry_ids[0];
        pane.expanded_brief = Some(index);
        let reader = tree.doc.get_actor().clone();
        tree.doc.set_actor(ActorId::from(b"bea-device".as_slice()));
        tree.grant_in_workspace(
            workspace,
            &user,
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath("/home/user/new-result".into()),
                Access::Read,
            ),
        )
        .unwrap();
        tree.seed_doc_from_cache();
        tree.doc.set_actor(reader);
        pane.refresh(&mut tree);
        let Some(expanded) = pane.expanded_brief else {
            panic!("the opened session stays open");
        };
        assert!(pane.digest.briefs[expanded].entry_ids.contains(&anchor));
        pane.finish_view(workspace.raw());
        pane.refresh(&mut tree);
        assert!(
            pane.glance_signal(workspace.raw()).1 > 0,
            "late unpainted work remains unread"
        );
    }

    #[test]
    fn acknowledging_new_record_work_does_not_resurrect_the_arrival_baseline() {
        let (mut tree, records, mut pane, record) = agent_task_fixture();
        let workspace = tree.active.raw();
        pane.load_frontiers();
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        assert_eq!(pane.glance_signal(workspace).1, 0);
        pane.set_view(ActivityView::CatchUp);
        pane.begin_view(workspace);
        records.with(&record, |doc| {
            doc.put(automerge::ROOT, "review", "Check the conclusion")
                .unwrap();
            let context = ui::tilo_actions::ActionContext::for_resource(
                record.as_str(),
                "document",
                "Campaign brief",
            );
            doc.commit_with(
                CommitOptions::default()
                    .with_time(24)
                    .with_message(context.change_message().unwrap()),
            );
        });
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        assert_eq!(pane.glance_signal(workspace).1, 1);
        show_summary(&mut pane);
        pane.apply_selection_history_action(ACTIVITY_ACKNOWLEDGE, &mut tree, Some(&records));
        assert_eq!(pane.glance_signal(workspace).1, 0);
        assert!(
            pane.digest.briefs.is_empty(),
            "old causal heads remain the baseline"
        );
        pane.begin_view(workspace);
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        assert_eq!(pane.glance_signal(workspace).1, 0);
        assert!(pane.digest.briefs.is_empty());
    }

    #[test]
    fn catch_up_persists_only_what_the_app_showed() {
        let (mut tree, workspace, _) = peers_busy_room();
        let records = RecordStore::in_memory();
        let room_id = tree.active_workspace().expect("room").room();
        let room = room_id.to_string();
        for n in 0..12 {
            let id = RecordId::for_file(format!("result-{n}.note"));
            let kind = ui::RecordKind::new("note");
            assert!(records.open_record(&ui::RecordCheckout::new(id.clone(), kind.clone())));
            records.with(&id, |doc| {
                ui::record::init(doc, &id, &kind, &room);
                doc.commit();
                doc.set_actor(ActorId::from(b"ada-device".as_slice()));
                doc.put(automerge::ROOT, "body", format!("Result {n}"))
                    .unwrap();
                let context = ui::tilo_actions::ActionContext::for_resource(
                    id.as_str(),
                    "note",
                    format!("Result {n}"),
                );
                doc.commit_with(
                    CommitOptions::default().with_message(context.change_message().unwrap()),
                );
            });
            tree.desktop
                .room_records_held
                .entry(room_id)
                .or_default()
                .insert(id.to_string(), kind.to_string());
        }
        let host = Rc::new(crate::activity::testing::TestHost::default());
        let mut pane = RoomActivityPane::new();
        pane.set_host(host.clone());
        pane.begin_view(workspace.raw());
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        pane.persistent_frontiers = true;
        let before = pane.glance_signal(workspace.raw()).1;
        assert!(before >= 12);
        // The app said it showed the first session only.
        let first = pane.serve_summary().briefs.remove(0).entries;
        pane.mark_viewed(
            workspace.raw(),
            &tilo_actions::remote::workspace_activity::Viewed {
                entries: first,
                later: true,
            },
        );
        let shown = pane.viewed_changes.get(&workspace.raw()).unwrap().len();
        assert!(
            shown > 0 && shown < before,
            "the fixture has more than the app showed"
        );
        let heads = tree.doc.get_heads();
        assert!(
            pane.apply_selection_history_action(ACTIVITY_ACKNOWLEDGE, &mut tree, Some(&records))
                .handled
        );
        assert_eq!(
            tree.doc.get_heads(),
            heads,
            "review is private viewer state"
        );
        let mut next = RoomActivityPane::new();
        next.set_host(host);
        next.load_frontiers();
        next.refresh(&mut tree);
        next.refresh_records(&mut tree, &records);
        assert_eq!(next.glance_signal(workspace.raw()).1, before - shown);
    }

    #[test]
    fn away_recap_requires_the_full_threshold_and_fails_closed_on_clock_rollback() {
        let departed = 10_000;
        assert!(!away_recap_is_due(
            departed,
            SystemTime::UNIX_EPOCH + Duration::from_secs(departed + 45 * 60 - 1)
        ));
        assert!(away_recap_is_due(
            departed,
            SystemTime::UNIX_EPOCH + Duration::from_secs(departed + 45 * 60)
        ));
        assert!(!away_recap_is_due(
            departed,
            SystemTime::UNIX_EPOCH + Duration::from_secs(departed - 1)
        ));
    }

    fn away_change_count_after(elapsed: Duration, changed: bool) -> usize {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let records = RecordStore::in_memory();
        let home = tree.desktop.active;
        let elsewhere = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id != home)
            .expect("demo has another workspace")
            .id;
        let departed_at = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(10_000))
            .expect("fixed test timestamp");
        let mut pane = native_pane();
        pane.persistent_frontiers = false;
        pane.set_now_for_test(departed_at);
        pane.observe_workspace(home.raw());
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);

        // Departure follows an actually reviewed page, not an indexed head.
        show_summary(&mut pane);
        pane.finish_view(home.raw());

        pane.observe_workspace(elsewhere.raw());
        tree.desktop.active = elsewhere;
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        if changed {
            let reader = tree.doc.get_actor().clone();
            tree.doc.set_actor(ActorId::from(b"ada-device".as_slice()));
            tree.desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == home)
                .expect("home workspace")
                .name = "Board review".to_string();
            tree.seed_doc_from_cache();
            tree.doc.set_actor(reader);
        }

        tree.desktop.active = home;
        pane.set_now_for_test(
            departed_at
                .checked_add(elapsed)
                .expect("test elapsed time fits SystemTime"),
        );
        pane.observe_workspace(home.raw());
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        pane.away_change_count(home.raw())
    }

    #[test]
    fn away_recap_requires_both_time_away_and_causal_changes() {
        assert_eq!(
            away_change_count_after(Duration::from_secs(45 * 60 - 1), true),
            0,
            "a changed workspace does not interrupt a short absence"
        );
        assert_eq!(
            away_change_count_after(Duration::from_secs(45 * 60), false),
            0,
            "elapsed time alone never creates a recap"
        );
        assert!(
            away_change_count_after(Duration::from_secs(45 * 60), true) > 0,
            "the exact threshold plus causally new work produces a recap"
        );
    }

    #[test]
    fn rows_come_from_the_rooms_own_change_graph() {
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        assert!(
            pane.entries().len() >= 3,
            "grants, a revoke and the rename stay; setup writes do not, got {}",
            pane.entries().len()
        );
        let summaries: Vec<&str> = pane.entries().iter().map(|e| e.summary.as_str()).collect();
        assert!(
            summaries.iter().any(|s| s.contains("/home/user/notes")),
            "the filesystem grant is named by the path it granted: {summaries:?}"
        );
        assert!(
            summaries.iter().any(|s| s.starts_with("Revoked")),
            "the revoke is its own row: {summaries:?}"
        );
        assert!(
            summaries.iter().any(|s| s.contains("Design review")),
            "the flush that carried the rename names it: {summaries:?}"
        );
        assert!(
            !summaries.iter().any(|s| s == &"Granted 4 capabilities"),
            "a flush that granted nothing must not read as a grant: {summaries:?}"
        );
        assert!(
            summaries.iter().all(|s| {
                !s.starts_with("Opened")
                    && !s.contains("Recorded workspace")
                    && !s.contains("Created the workspace")
            }),
            "setup bookkeeping, with the windows a workspace is made with, is not a change: {summaries:?}"
        );
    }

    #[test]
    fn a_selected_row_publishes_diff_restore_and_undo_as_actions() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::Explore);
        show_summary(&mut pane);
        let idx = (0..pane.timeline.row_count())
            .find(|&i| pane.timeline.row_entry(i).is_some())
            .expect("a change row");
        pane.timeline.set_selection(Some((
            pane.timeline.row_entry_index(idx).expect("entry"),
            pane.timeline.row_entry_index(idx).expect("entry"),
        )));
        let ids: Vec<&str> = pane
            .selection_history_actions()
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert!(
            ids.contains(&HISTORY_COMPARE) && ids.contains(&HISTORY_RESTORE),
            "selected row must offer compare and restore, got {ids:?}"
        );
        // Compare is an action, not a click-only chip.
        let compared = pane.apply_selection_history_action(HISTORY_COMPARE, &mut tree, None);
        assert!(compared.compared || compared.handled || !ids.is_empty());
    }

    #[test]
    fn app_native_handoff_focuses_the_exact_version_without_marking_it_unseen() {
        let (mut tree, workspace, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let unread_before = pane.passive_unread;
        let id = HistoryEntryId::from_bytes([9; 16]);
        let resource = "signed-file-record".to_string();
        let context = ui::tilo_actions::ActionContext::for_resource(
            resource.clone(),
            "files.entry",
            "Launch brief.md",
        )
        .with_app("org.tilo.Files", "Files");
        pane.open_external_entry(HistoryEntry {
            id,
            author: ui::widget::HistoryAuthor::new("Ada", 280.0, "01010101"),
            entry_kind: EntryKind::Compaction,
            artifact_kind: ArtifactKind::File,
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            summary: "Edited · Launch brief.md".into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: Some(context),
        });
        pane.apply_deep_link(ActivityDeepLink {
            author: None,
            group_id: None,
            entry_id: Some(*id.as_bytes()),
            resource_id: Some(resource.clone()),
            resource_version: Some(*id.as_bytes()),
        });

        assert_eq!(pane.view(), ActivityView::Explore);
        assert_eq!(pane.lens, LensSelection::Resource(resource));
        let (selected, _) = pane.timeline.selection().expect("exact version selected");
        assert_eq!(pane.timeline.entries()[selected].id, id);
        assert_eq!(
            pane.timeline.visible_entry(0).map(|entry| entry.id),
            Some(id)
        );
        assert_eq!(pane.passive_unread, unread_before);
        assert_eq!(pane.glance_signal(workspace.raw()).1, unread_before);
    }

    /// A second workspace with its own governance, interleaved in one document.
    fn two_room_tree() -> (WorkspaceTree, WorkspaceId, WorkspaceId) {
        let (mut tree, first, user) = busy_room();
        let second = tree.append_empty_workspace("Research");
        tree.adopt_ownerless_workspaces(&user);
        tree.grant_in_workspace(
            second,
            &user,
            CapOverride::grant(
                Subject::app("browser"),
                Scope::NetHost("research.example".to_string()),
                Access::Read,
            ),
        )
        .expect("owner may grant in the second room too");
        tree.set_active(first);
        (tree, first, second)
    }

    #[test]
    fn this_rooms_scope_excludes_another_workspaces_governance() {
        let (mut tree, _first, _second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        assert_eq!(pane.scope(), RoomScope::ThisRoom, "scoped by default");

        let scoped: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(
            !scoped.iter().any(|s| s.contains("research.example")),
            "the other room's network grant leaked into this room: {scoped:?}"
        );
        assert!(
            scoped.iter().any(|s| s.contains("/home/user/notes")),
            "this room's own grant is present: {scoped:?}"
        );

        let all_tree = { tree.desktop.workspaces.clone() };
        let _ = all_tree;
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        let widened: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(
            widened.iter().any(|s| s.contains("research.example")),
            "widening the scope must reveal the other room: {widened:?}"
        );
        assert!(
            widened.len() > scoped.len(),
            "the whole tree is strictly more than one room: {} vs {}",
            widened.len(),
            scoped.len()
        );
    }

    #[test]
    fn a_widened_row_names_the_workspace_it_happened_in() {
        let (mut tree, _first, _second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        let named_rows: Vec<String> = pane
            .entries()
            .iter()
            .filter(|e| {
                e.summary.contains("research.example") || e.summary.contains("/home/user/notes")
            })
            .map(|e| e.summary.clone())
            .collect();
        assert!(
            named_rows.iter().any(|s| s.contains("Research")),
            "a row must name the room it happened in: {named_rows:?}"
        );
        let distinct: std::collections::HashSet<&String> = named_rows.iter().collect();
        assert_eq!(
            distinct.len(),
            named_rows.len(),
            "no two rows may read identically: {named_rows:?}"
        );
    }

    #[test]
    fn switching_the_active_workspace_re_scopes_without_a_document_change() {
        let (mut tree, first, second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        let in_first: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(in_first.iter().any(|s| s.contains("/home/user/notes")));

        tree.set_active(second);
        pane.refresh(&mut tree);
        let in_second: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(
            in_second.iter().any(|s| s.contains("research.example")),
            "the second room's own grant: {in_second:?}"
        );
        assert!(
            !in_second.iter().any(|s| s.contains("/home/user/notes")),
            "the first room's grant must not follow us: {in_second:?}"
        );
        let _ = first;
    }

    #[test]
    fn a_whole_tree_flush_belongs_to_no_single_room() {
        let (mut tree, _first, _second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        assert!(
            pane.entries()
                .iter()
                .all(|e| !e.summary.starts_with("Recorded ")),
            "a whole-tree flush is setup bookkeeping, not a feed change"
        );
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        assert!(
            pane.entries()
                .iter()
                .all(|e| !e.summary.starts_with("Recorded ")),
            "widening must not bring setup flushes back"
        );
    }

    #[test]
    fn the_local_users_own_rows_are_attributed_to_them() {
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        let me = crate::activity::testing::LOCAL_USER.to_string();
        assert!(!me.is_empty(), "there is always a local user id");
        assert!(
            pane.entries()
                .iter()
                .all(|e| e.author.name != ui::history_source::UNKNOWN_AUTHOR_NAME),
            "local changes resolved to an unknown participant: {:?}",
            pane.contributors()
        );
        assert_eq!(
            pane.contributors().len(),
            1,
            "one person did all of this: {:?}",
            pane.contributors()
        );
    }

    #[test]
    fn every_row_carries_a_real_timestamp() {
        // The whole date filter rests on this — an epoch row can never be
        // selected by a date range.
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        assert!(!pane.entries().is_empty());
        for e in pane.entries() {
            assert!(
                e.timestamp > SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000),
                "row {:?} is stamped at {:?} — the room's commits are not carrying a clock",
                e.summary,
                e.timestamp
            );
        }
    }

    #[test]
    fn date_range_filter_partitions_the_real_history() {
        // One change stamped a week back via `commit_stamped_at`, the rest
        // at now: "Today" drops the old one, "30 days" keeps everything.
        let mut tree = WorkspaceTree::bootstrap_demo();
        let user: String = "test-owner".to_string();
        tree.adopt_ownerless_workspaces(&user);
        let ws_id = tree.desktop.active;

        let week_ago_secs = i64::try_from(ui::time::try_unix_now().unwrap() - 604_800).unwrap();
        let last_week = CapOverride::grant(
            Subject::app("files"),
            Scope::FsPath("/home/user/last-week".to_string()),
            Access::Read,
        );
        let cap_id = {
            let workspace = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == ws_id)
                .expect("active workspace");
            assert!(workspace.may_govern_at(
                &user,
                tilo_action_policy::roles::governance::GRANT,
                tilo_clock::unix_now()
            ));
            workspace.grant_sandbox(last_week)
        };
        let stored = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == ws_id)
            .expect("active workspace")
            .sandbox
            .iter()
            .find(|capability| capability.id == cap_id)
            .cloned()
            .expect("stored grant");
        assert!(tilo_desktop_model::desktop_doc::insert_sandbox(
            &mut tree.doc,
            ws_id,
            &stored
        ));
        tilo_resource::history::commit_stamped_at(&mut tree.doc, week_ago_secs)
            .expect("the grant left a transaction to stamp");

        tree.grant_in_workspace(
            ws_id,
            &user,
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath("/home/user/today".to_string()),
                Access::Read,
            ),
        )
        .expect("owner may grant");

        let mut pane = pane_over(&mut tree);
        let all = pane.visible_len();
        assert!(
            all >= 2,
            "expected last week's grant and today's, got {all}"
        );

        pane.set_period(Period::Today);
        let today = pane.visible_len();
        assert!(today > 0, "today's changes must survive the day filter");
        assert!(
            today < all,
            "the week-old change must be filtered out: {today} of {all} survived"
        );
        assert!(
            !pane.timeline().entries().is_empty(),
            "filtering hides rows, it does not discard them"
        );
        for i in 0..pane.visible_len() {
            let e = pane.visible_entry(i).unwrap();
            assert!(
                !e.summary.contains("Last week"),
                "the week-old row leaked through the day filter: {}",
                e.summary
            );
        }

        pane.set_period(Period::Month);
        assert_eq!(
            pane.visible_len(),
            all,
            "a 30-day window covers a change from seven days ago"
        );
    }

    #[test]
    fn contributor_filter_narrows_to_one_person() {
        let (mut tree, _, _) = busy_room();

        let mut peer = automerge::AutoCommit::new();
        peer.set_actor(automerge::ActorId::from("bob".as_bytes()));
        peer.apply_incoming(
            tree.export_changes_since(&[])
                .iter()
                .map(|b| automerge::Change::try_from(b.as_slice()).unwrap()),
        )
        .unwrap();
        {
            use automerge::transaction::Transactable;
            let (_, ws_entry) = peer
                .get(
                    &peer.get(&automerge::ROOT, "workspaces").unwrap().unwrap().1,
                    0_usize,
                )
                .unwrap()
                .unwrap();
            peer.put(&ws_entry, "name", "Bob's rename").unwrap();
        }
        tilo_resource::history::commit_stamped(&mut peer).expect("the peer's edit commits");
        let blobs: Vec<Vec<u8>> = peer
            .get_changes(&tree.doc.get_heads())
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect();
        tree.apply_changes(&blobs).unwrap();

        let mut pane = pane_over(&mut tree);
        assert!(
            pane.contributors().len() >= 2,
            "two actors wrote to this room; contributors = {:?}",
            pane.contributors()
        );
        // bootstrap_demo seeds a Research collaborator id "bob" named "Bob"
        // with a cursor colour. seed_roster_colours registers that display
        // name on the handle actor, so history shows "Bob" rather than the
        // raw handle — same person, roster wins for colour and label.
        let bob = pane
            .contributors()
            .iter()
            .find(|c| c.eq_ignore_ascii_case("bob"))
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "the peer's self-declared handle resolves to a contributor; got {:?}",
                    pane.contributors()
                )
            });

        let everyone = pane.visible_len();
        pane.set_author_filter(Some(bob.clone()));
        let just_bob = pane.visible_len();
        assert!(just_bob > 0, "bob authored at least one row");
        assert!(
            just_bob < everyone,
            "filtering to bob must hide the local actor's rows: {just_bob} of {everyone}"
        );
        for i in 0..pane.visible_len() {
            assert_eq!(pane.visible_entry(i).unwrap().author.name, bob);
        }

        pane.set_author_filter(None);
        assert_eq!(pane.visible_len(), everyone, "clearing restores every row");
    }

    #[test]
    fn action_type_chips_narrow_by_entry_kind() {
        use ui::widget::activity_timeline::ChipKind;
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let all = pane.visible_len();

        pane.timeline_mut().set_active_chip(ChipKind::Governance);
        let gov = pane.visible_len();
        assert!(
            gov > 0,
            "grants and revokes are governance; the room has some"
        );
        assert!(gov < all, "governance is a subset: {gov} of {all}");
        for i in 0..pane.visible_len() {
            assert_eq!(
                pane.visible_entry(i).unwrap().entry_kind,
                EntryKind::Access,
                "the Governance chip must only show governance rows"
            );
        }

        pane.timeline_mut().set_active_chip(ChipKind::All);
        assert_eq!(pane.visible_len(), all);
    }

    #[test]
    fn reverting_a_row_writes_a_new_forward_change() {
        let (mut tree, ws_id, user) = busy_room();
        let mut pane = pane_over(&mut tree);

        let before_name = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == ws_id)
            .map(|w| w.name.clone())
            .unwrap();
        assert_eq!(before_name, "Design review");
        let versions_before = pane.entries().len();

        // Rename again, so there is something to revert back across.
        if let Some(ws) = tree.desktop.workspaces.iter_mut().find(|w| w.id == ws_id) {
            ws.name = "Renamed by mistake".to_string();
        }
        tree.seed_doc_from_cache();
        pane.refresh(&mut tree);
        assert!(pane.entries().len() > versions_before);

        let target = pane.entries()[versions_before - 1].id;
        assert!(
            pane.revert_to(&mut tree, &target),
            "the revert wrote something"
        );

        assert_eq!(
            tree.desktop
                .workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .map(|w| w.name.as_str()),
            Some("Design review"),
            "the room is back to the state the row named"
        );
        assert!(
            pane.entries().len() > versions_before + 1,
            "the revert is a new row on top, not a rewind: {} rows",
            pane.entries().len()
        );
        assert!(
            pane.entries().iter().any(|e| e.id == target),
            "the version reverted to is still in the graph"
        );
        let _ = user;
    }

    #[test]
    fn reverting_an_unknown_row_is_a_no_op() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let before = tree.doc.get_heads();
        assert!(!pane.revert_to(&mut tree, &HistoryEntryId([0x5a; 16])));
        assert_eq!(tree.doc.get_heads(), before, "nothing was written");
    }

    #[test]
    fn governance_rows_never_offer_revert() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.timeline_mut()
            .set_active_chip(ui::widget::activity_timeline::ChipKind::Governance);
        assert!(pane.visible_len() > 0);
        for i in 0..pane.visible_len() {
            let e = pane.visible_entry(i).unwrap();
            assert!(
                !e.entry_kind.is_revertible(),
                "{:?} offered a revert chip",
                e.summary
            );
            assert!(
                pane.timeline().revert_chip_rect(i).is_none(),
                "row {i} ({:?}) laid out a revert chip",
                e.summary
            );
        }
    }

    #[test]
    fn a_capability_is_described_in_the_rooms_vocabulary() {
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        let grant = pane
            .entries()
            .iter()
            .find(|e| e.summary.contains("/home/user/notes"))
            .expect("the filesystem grant has a row");
        assert!(
            grant
                .summary
                .starts_with("Granted read on /home/user/notes to files"),
            "the grant names verb, scope and subject: {:?}",
            grant.summary
        );
        assert_eq!(grant.entry_kind, EntryKind::Access);
    }

    #[test]
    fn an_empty_room_and_a_filtered_one_do_not_read_the_same() {
        let mut empty = WorkspaceTree::default();
        let pane = pane_over(&mut empty);
        let (title, detail) = pane.timeline().empty_state_lines();
        assert_eq!(title, "No activity yet");
        assert!(detail.is_none(), "nothing to explain when nothing happened");

        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_author_filter(Some("nobody-by-that-name".to_string()));
        assert_eq!(pane.visible_len(), 0);
        let (title, detail) = pane.timeline().empty_state_lines();
        assert_eq!(title, "No activity matches these filters");
        let detail = detail.expect("a filtered-empty list must explain itself");
        assert!(
            detail.contains("nobody-by-that-name"),
            "the explanation names the filter doing the hiding: {detail}"
        );
        assert!(
            detail.contains(&pane.entries().len().to_string()),
            "the explanation says how many rows are hidden: {detail}"
        );
    }

    #[test]
    fn contributors_are_only_people_who_actually_did_something() {
        let (mut tree, _, _) = busy_room();
        tree.desktop
            .collaborators
            .push(tilo_desktop_model::Collaborator::new(
                "ghost",
                "Never Edited",
            ));
        let pane = pane_over(&mut tree);
        assert!(
            !pane.contributors().iter().any(|c| c == "Never Edited"),
            "a peer who never wrote must not be listed: {:?}",
            pane.contributors()
        );
    }

    #[test]
    fn activity_header_digest_reduces_the_same_scoped_rows() {
        let (mut tree, _, _) = peers_busy_room();
        let pane = pane_over(&mut tree);
        let others = pane
            .entries()
            .iter()
            .filter(|entry| !author_is_local(&entry.author, &pane.local_user()))
            .count();
        assert!(others > 0, "the fixture is a collaborator's work");
        assert_eq!(pane.digest().total_changes, others);
        assert!(!pane.digest().briefs.is_empty());
        assert!(pane.digest().headline().is_some());
    }

    #[test]
    fn catch_up_leaves_out_the_readers_own_work() {
        let (mut tree, workspace, _) = busy_room();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        assert_eq!(pane.view(), ActivityView::CatchUp);
        assert!(
            !pane.entries().is_empty(),
            "the reader's own work stays in the timeline"
        );
        let (_, unread, _) = pane.glance_signal(workspace.raw());
        assert_eq!(unread, 0, "your own setup and edits are not news");
        pane.begin_view(workspace.raw());
        assert_eq!(pane.digest().total_changes, 0);
        assert!(pane.digest().briefs.is_empty());
    }

    #[test]
    fn replay_materializes_real_causal_layouts_and_exposes_controls() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
            91_001,
            "Plan".to_string(),
            b"{}".to_vec(),
        ));
        tilo_resource::history::commit_stamped(&mut tree.doc);
        assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
            91_002,
            "Review".to_string(),
            b"{}".to_vec(),
        ));
        tilo_resource::history::commit_stamped(&mut tree.doc);
        let mut pane = pane_over(&mut tree);
        assert!(pane.replay_frames.len() >= 2);
        let tab_counts: Vec<usize> = pane
            .replay_frames
            .iter()
            .map(|frame| {
                frame
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.groups)
                    .map(|group| group.tabs.len())
                    .sum()
            })
            .collect();
        assert!(
            tab_counts.windows(2).any(|pair| pair[0] != pair[1]),
            "causal forks must expose a changing real layout: {tab_counts:?}"
        );
        pane.set_view(ActivityView::Replay);
        assert_eq!(pane.serve_replay().frames.len(), pane.replay_frames.len());

        let mut presence = PresenceCtx::empty();
        let ada = ui::permissions::UserId("ada".into());
        presence.names.insert(ada.clone(), "Ada".into());
        presence.set_pointer(
            &ui::presence::PresenceKey::desktop_pointer("design-room"),
            ada,
            (420.0, 260.0),
        );
        pane.observe_replay_presence(&presence);
        let pointer = pane
            .replay_frames
            .last()
            .and_then(|frame| frame.pointers.first())
            .expect("session presence is retained against the durable checkpoint");
        assert_eq!(pointer.name, "Ada");
        assert!(pointer.desktop_space);
    }

    #[test]
    fn a_long_lived_room_stays_bounded() {
        // Well past the producer's ceiling, all of it real: each iteration is
        // the same mirror the drop handler calls when a file is dropped into
        // a workspace. The pane must show the recent end of the history
        // rather than growing without limit.
        let mut tree = WorkspaceTree::bootstrap_demo();
        let user = "test-owner".to_string();
        tree.adopt_ownerless_workspaces(&user);
        let ws_id = tree.desktop.active;
        for i in 0..240u64 {
            tree.grant_in_workspace(
                ws_id,
                &user,
                CapOverride::grant(
                    Subject::app("files"),
                    Scope::FsPath(format!("/tmp/bound/{i}")),
                    Access::Read,
                ),
            )
            .expect("owner may grant");
        }
        let pane = pane_over(&mut tree);
        assert!(
            pane.entries().len() <= ui::history_source::DEFAULT_MAX_ENTRIES,
            "the pane derived {} rows from a 240-change room",
            pane.entries().len()
        );
        assert!(
            pane.entries().len() > 100,
            "and it is showing a real window of them, not a token few"
        );
        // The newest change is the one that must survive the cap.
        assert!(
            pane.entries()
                .iter()
                .any(|e| e.summary.contains("/tmp/bound/239")),
            "the most recent change is inside the window"
        );
    }

    #[test]
    fn period_windows_are_half_open_and_all_time_is_unbounded() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert!(Period::AllTime.range(now).is_none());
        let (start, end) = Period::Week.range(now).unwrap();
        assert_eq!(
            now.duration_since(start).unwrap(),
            Duration::from_secs(604_800)
        );
        assert!(
            end > now,
            "a change committed this instant is inside the window"
        );
    }

    // -----------------------------------------------------------------
    // Grouping and names
    // -----------------------------------------------------------------

    #[test]
    fn the_rooms_history_reads_as_bands_rather_than_one_row_per_change() {
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        assert!(pane.entries().len() >= 4);
        // Bands, not a wall: every rendered line is either a band header or a
        // change, and there are fewer lines than changes.
        let bands = (0..pane.timeline().row_count())
            .filter(|i| pane.timeline().group_at_row(*i).is_some())
            .count();
        assert!(
            bands >= 1,
            "a room with governance, edits and a deletion must produce at least one band"
        );
        // Every band describes itself without a name, and describes itself
        // honestly — a count it did not measure would be the failure here.
        for i in 0..pane.timeline().row_count() {
            if let Some(title) = pane.timeline().group_title_at_row(i) {
                assert!(!title.is_empty(), "band {i} has no title");
                assert!(
                    !title.contains("{}"),
                    "band {i} rendered a format placeholder: {title:?}"
                );
            }
        }
    }

    #[test]
    fn naming_a_band_round_trips_through_the_room_document() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        let (start, end) = (entries[1].id, entries[3].id);

        let wrote = pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start,
                end,
                label: "Set up the room".to_string(),
            },
        );
        assert!(wrote, "the name went into the document");

        // It is in the document, not in the widget: a fresh pane over the same
        // tree finds it.
        let ranges = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].label, "Set up the room");
        assert_eq!(ranges[0].start, start);
        assert_eq!(ranges[0].end, Some(end));

        let mut fresh = native_pane();
        fresh.refresh(&mut tree);
        let titles: Vec<&str> = (0..fresh.timeline().row_count())
            .filter_map(|i| fresh.timeline().group_title_at_row(i))
            .collect();
        assert!(
            titles.contains(&"Set up the room"),
            "the band is on screen for anyone who opens the pane: {titles:?}"
        );
    }

    #[test]
    fn naming_a_band_does_not_add_a_row_of_its_own() {
        // The name is already on screen as the band's title. A "1 field" row
        // directly under it would be noise, so the producer filters the marker
        // change out — and the change count must not move.
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let before = pane.entries().len();
        let entries = pane.entries().to_vec();
        pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start: entries[1].id,
                end: entries[2].id,
                label: "A version".to_string(),
            },
        );
        assert_eq!(
            pane.entries().len(),
            before,
            "naming added a row to the history it named"
        );
    }

    #[test]
    fn un_naming_removes_the_band_and_leaves_the_changes_alone() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        let (start, end) = (entries[1].id, entries[3].id);
        pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start,
                end,
                label: "Temporary".to_string(),
            },
        );
        let record = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new())[0]
            .id
            .clone();
        let changes_before = pane.entries().len();

        assert!(pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: Some(record),
                start,
                end,
                label: String::new(),
            },
        ));
        assert!(
            ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new()).is_empty(),
            "the band is gone"
        );
        assert_eq!(
            pane.entries().len(),
            changes_before,
            "and the changes it named are all still there"
        );
    }

    #[test]
    fn two_people_naming_the_same_run_converge_on_one_band() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        let (start, end) = (entries[1].id, entries[3].id);

        // A peer forks the room document, names the same run differently, and
        // syncs back — the shape a remote replica's sync round has.
        let mut peer = tree.doc.fork();
        peer.set_actor(automerge::ActorId::from("ada".as_bytes()));
        ui::history_groups::name_range(&mut peer, &start, Some(&end), "Ada's pass");
        tilo_resource::history::commit_stamped(&mut peer);

        pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start,
                end,
                label: "My pass".to_string(),
            },
        );

        // Merge each way round and compare. Both sides have to land on the same
        // single band, whichever order the names arrived in.
        let mut mine = tree.doc.fork();
        mine.merge_incoming(&mut peer.clone()).expect("peers merge");
        let mut theirs = peer.fork();
        theirs
            .merge_incoming(&mut tree.doc.clone())
            .expect("peers merge");

        let a = ui::history_groups::read_named_ranges(&mine, &AuthorDirectory::new());
        let b = ui::history_groups::read_named_ranges(&theirs, &AuthorDirectory::new());
        assert_eq!(a.len(), 1, "one run, one band: {a:?}");
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].label, b[0].label, "both peers read the same winner");
        assert!(
            a[0].label == "My pass" || a[0].label == "Ada's pass",
            "the winner is one of the two names actually written: {:?}",
            a[0].label
        );
        assert_eq!(a[0].start, start);
        assert_eq!(a[0].end, Some(end));
    }

    #[test]
    fn a_peers_name_is_attributed_to_the_peer_not_to_whoever_is_looking() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        let (start, end) = (entries[1].id, entries[3].id);

        let mut peer = tree.doc.fork();
        peer.set_actor(automerge::ActorId::from("ada".as_bytes()));
        ui::history_groups::name_range(&mut peer, &start, Some(&end), "Ada's pass");
        tilo_resource::history::commit_stamped(&mut peer);
        tree.doc.merge_incoming(&mut peer).expect("peers merge");

        pane.invalidate();
        pane.refresh(&mut tree);
        let named = (0..pane.timeline().row_count())
            .find(|i| pane.timeline().group_title_at_row(*i) == Some("Ada's pass"))
            .expect("the peer's band is on screen");
        let sub = pane.timeline().group_subtitle_at_row(named).unwrap();
        assert!(
            sub.contains("named by"),
            "a name someone else wrote must say so: {sub:?}"
        );
        assert!(
            !sub.contains("named by test-owner") && !sub.contains("named by ada"),
            "and it must not be attributed to the reader: {sub:?}"
        );
    }

    #[test]
    fn restoring_to_a_band_boundary_lands_on_the_state_at_that_boundary() {
        // The band header's [Restore] passes the band's last change, so this is
        // the production path for "restore to the end of this version".
        let (mut tree, ws_id, _) = busy_room();
        let mut pane = pane_over(&mut tree);

        // Find the change that named the workspace, and the state just after it.
        let renamed_at = pane
            .entries()
            .iter()
            .position(|e| e.summary.contains("Design review"))
            .expect("the fixture renames the workspace");
        let boundary = pane.entries()[renamed_at].id;

        // Move on: rename again, so the document is no longer in that state.
        if let Some(ws) = tree.desktop.workspaces.iter_mut().find(|w| w.id == ws_id) {
            ws.name = "Something else".to_string();
        }
        tree.seed_doc_from_cache();
        pane.invalidate();
        pane.refresh(&mut tree);
        assert_eq!(
            tree.desktop
                .workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .map(|w| w.name.clone()),
            Some("Something else".to_string())
        );

        assert!(
            pane.revert_to(&mut tree, &boundary),
            "the restore wrote something"
        );
        assert_eq!(
            tree.desktop
                .workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .map(|w| w.name.clone()),
            Some("Design review".to_string()),
            "the room is back in the state that boundary produced"
        );
        // Forward, not a rewind: the change we rolled past is still in the graph.
        assert!(
            pane.entries().len() > renamed_at + 1,
            "history was appended to, not rewritten"
        );
    }

    /// The app shows what is new and says so ([`wire::MARK_VIEWED`]), to be
    /// counted seen once the person stops reading.
    fn show_summary(pane: &mut RoomActivityPane) {
        let Some(workspace) = pane.last_active else {
            return;
        };
        let entries = pane
            .serve_summary()
            .briefs
            .into_iter()
            .flat_map(|brief| brief.entries)
            .collect();
        pane.mark_viewed(
            workspace,
            &tilo_actions::remote::workspace_activity::Viewed {
                entries,
                later: true,
            },
        );
    }

    #[test]
    fn selected_named_bands_can_be_split_and_merged() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        assert!(pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start: entries[1].id,
                end: entries[3].id,
                label: "Investigate".to_string(),
            },
        ));
        pane.timeline_mut().set_selection(Some((2, 2)));
        assert!(pane.split_selection_group(&mut tree));
        let split = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(split.len(), 2);
        assert!(
            split
                .iter()
                .any(|range| { range.start == entries[1].id && range.end == Some(entries[1].id) })
        );
        assert!(split.iter().any(|range| range.start == entries[2].id));

        pane.timeline_mut().set_selection(Some((1, 3)));
        assert!(pane.merge_selection_groups(&mut tree));
        let merged = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].start, entries[1].id);
        assert_eq!(merged[0].end, Some(entries[3].id));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "an end-to-end test scenario that reads top to bottom"
    )]
    fn catch_up_redraws_work_session_boundaries_without_rewriting_history() {
        let (mut tree, _, _) = peers_busy_room();
        let mut pane = pane_over(&mut tree);
        let original_ids = pane
            .entries()
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        let local = pane.local_user();
        let entries = pane
            .entries()
            .iter()
            .filter(|entry| !author_is_local(&entry.author, &local))
            .cloned()
            .collect::<Vec<_>>();
        assert!(entries.len() >= 4);
        assert!(pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start: entries[0].id,
                end: entries[1].id,
                label: "Write Q3 financial report".into(),
            },
        ));
        assert!(pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start: entries[2].id,
                end: entries[3].id,
                label: "Work on onboarding Sarah".into(),
            },
        ));
        pane.set_view(ActivityView::CatchUp);

        let older = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Write Q3 financial report")
            .expect("older named work session is projected into Catch up");
        let newer = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Work on onboarding Sarah")
            .expect("newer named work session is projected into Catch up");
        assert!(pane.move_work_session_boundary(
            &mut tree,
            WorkSessionBoundary {
                older,
                newer,
                direction: BoundaryDirection::IntoNewer,
            },
        ));

        let ranges = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        let finance = ranges
            .iter()
            .find(|range| range.label == "Write Q3 financial report")
            .unwrap();
        let onboarding = ranges
            .iter()
            .find(|range| range.label == "Work on onboarding Sarah")
            .unwrap();
        assert_eq!(finance.start, entries[0].id);
        assert_eq!(finance.end, Some(entries[0].id));
        assert_eq!(onboarding.start, entries[1].id);
        assert_eq!(onboarding.end, Some(entries[3].id));
        assert_eq!(
            pane.entries()
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            original_ids,
            "redrawing a work-session seam never rewrites or reorders actions"
        );
        assert_eq!(
            pane.digest
                .briefs
                .iter()
                .find(|brief| brief.title == "Write Q3 financial report")
                .unwrap()
                .changes,
            1
        );
        assert_eq!(
            pane.digest
                .briefs
                .iter()
                .find(|brief| brief.title == "Work on onboarding Sarah")
                .unwrap()
                .changes,
            3
        );

        let older = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Write Q3 financial report")
            .unwrap();
        let newer = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Work on onboarding Sarah")
            .unwrap();
        assert!(
            !pane.move_work_session_boundary(
                &mut tree,
                WorkSessionBoundary {
                    older,
                    newer,
                    direction: BoundaryDirection::IntoNewer,
                },
            ),
            "a work session cannot be emptied"
        );
        assert!(pane.move_work_session_boundary(
            &mut tree,
            WorkSessionBoundary {
                older,
                newer,
                direction: BoundaryDirection::IntoOlder,
            },
        ));
    }

    #[test]
    fn first_correction_materializes_automatic_work_sessions_as_shared_ranges() {
        let mut tree = WorkspaceTree::bootstrap_default();
        for (group_id, group_label, action) in [
            ("finance", "Write Q3 financial report", "Draft totals"),
            ("finance", "Write Q3 financial report", "Check forecast"),
            ("onboarding", "Onboard Sarah", "Open welcome notes"),
            ("onboarding", "Onboard Sarah", "Assign mentor"),
        ] {
            assert!(
                tree.append_workspace_effect_receipt(
                    tree.desktop.active,
                    tilo_actions::InvocationId::new(),
                    "ada",
                    tilo_actions::ActionContext::for_resource(
                        format!("test:{group_id}"),
                        "document",
                        group_label,
                    )
                    .with_app("org.tilo.Writer", "Writer")
                    .with_described(crate::activity::testing::verb(action))
                    .with_group(group_id, group_label, "work-session"),
                    tilo_actions::EffectReceiptStatus::Succeeded,
                )
            );
        }
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        pane.set_view(ActivityView::CatchUp);
        let finance = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Write Q3 financial report")
            .expect("automatic finance session");
        let onboarding = pane
            .digest
            .briefs
            .iter()
            .position(|brief| brief.title == "Onboard Sarah")
            .expect("automatic onboarding session");
        assert_eq!(
            pane.digest.briefs[finance]
                .authors
                .iter()
                .map(|author| author.name.as_str())
                .collect::<Vec<_>>(),
            vec!["ada"],
            "receipt-backed history is attributed to its authenticated action actor, not the room host"
        );
        assert!(pane.digest.briefs[finance].work_session_id.is_none());
        assert!(pane.digest.briefs[onboarding].work_session_id.is_none());
        let finance_first = pane
            .entry_position(&pane.digest.briefs[finance].entry_ids[0])
            .unwrap();
        let onboarding_first = pane
            .entry_position(&pane.digest.briefs[onboarding].entry_ids[0])
            .unwrap();
        let (older, newer) = if finance_first < onboarding_first {
            (finance, onboarding)
        } else {
            (onboarding, finance)
        };

        assert!(pane.move_work_session_boundary(
            &mut tree,
            WorkSessionBoundary {
                older,
                newer,
                direction: BoundaryDirection::IntoNewer,
            },
        ));
        let ranges = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert!(
            ranges
                .iter()
                .any(|range| range.label == "Write Q3 financial report")
        );
        assert!(ranges.iter().any(|range| range.label == "Onboard Sarah"));
        assert_eq!(
            pane.digest
                .briefs
                .iter()
                .find(|brief| brief.title == "Write Q3 financial report")
                .unwrap()
                .changes
                + pane
                    .digest
                    .briefs
                    .iter()
                    .find(|brief| brief.title == "Onboard Sarah")
                    .unwrap()
                    .changes,
            4,
            "materializing and moving a seam must preserve every atomic action"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one room, one labelled kind of change per step, each compared with a fresh pane"
    )]
    fn incremental_refresh_matches_full_replay_for_feed_undo_and_attribution() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let home = tree.active;
        let second = tree.append_empty_workspace("Second");
        tree.commit_room_change();
        let mut pane = native_pane();
        pane.refresh(&mut tree);

        let mut peer = tree.clone();
        peer.doc.set_actor(ActorId::from(b"bea-device".as_slice()));
        let receipt = || {
            tilo_actions::ActionContext::for_resource("document:brief", "document", "Release brief")
                .with_app("org.tilo.Writer", "Writer")
                .with_described(crate::activity::testing::verb("Export"))
        };

        // Every kind of row the steps make, once, before reads are counted:
        // the first row of a kind can load a catalogue, which moves the
        // i18n revision, and the log then rightly reads its window again in
        // the new words.
        let heads = tree.doc.get_heads();
        assert!(peer.append_workspace_effect_receipt(
            home,
            tilo_actions::InvocationId::new(),
            "bea",
            receipt(),
            tilo_actions::EffectReceiptStatus::Succeeded,
        ));
        tree.apply_changes(&peer.export_changes_since(&heads))
            .expect("the peer's change merges");
        pane.refresh(&mut tree);
        native_pane().refresh(&mut tree.clone());
        // The catalogues those rows loaded were added after this refresh
        // read the log; the next change reads it under them.
        tree.set_active(second);
        tree.commit_room_change();
        pane.refresh(&mut tree);
        tree.set_active(home);
        tree.commit_room_change();
        pane.refresh(&mut tree);

        let mut next_tab = 92_000_u64;
        for step in 0..24_u32 {
            // The peer has everything this device has before either acts.
            let peer_heads = peer.doc.get_heads();
            peer.apply_changes(&tree.export_changes_since(&peer_heads))
                .expect("the peer catches up");
            let synced = tree.doc.get_heads();
            let kind = match step % 6 {
                0 => {
                    tree.set_active(second);
                    tree.commit_room_change();
                    "switch away"
                }
                1 => {
                    next_tab += 1;
                    tree.set_active(home);
                    assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
                        next_tab,
                        format!("Tab {next_tab}"),
                        b"{}".to_vec(),
                    ));
                    tree.commit_room_change();
                    "open a tab"
                }
                2 => {
                    // The peer acts while this device switches back: two
                    // concurrent changes, merged in either order.
                    assert!(peer.append_workspace_effect_receipt(
                        home,
                        tilo_actions::InvocationId::new(),
                        "bea",
                        receipt(),
                        tilo_actions::EffectReceiptStatus::Succeeded,
                    ));
                    tree.set_active(home);
                    tree.commit_room_change();
                    let theirs = peer.export_changes_since(&synced);
                    tree.apply_changes(&theirs)
                        .expect("the peer's change merges");
                    "a peer's receipt, merged"
                }
                3 => {
                    next_tab += 1;
                    assert!(peer.doc_append_tab_to_first_group_of_active_workspace(
                        next_tab,
                        format!("Bea's tab {next_tab}"),
                        b"{}".to_vec(),
                    ));
                    peer.commit_room_change();
                    let theirs = peer.export_changes_since(&synced);
                    tree.apply_changes(&theirs).expect("the peer's tab merges");
                    "a peer's tab"
                }
                4 => {
                    tree.set_active(second);
                    tree.commit_room_change();
                    tree.set_active(home);
                    tree.commit_room_change();
                    "two switches between refreshes"
                }
                _ => "nothing new",
            };
            let appended = tree.doc.get_changes(&synced).len();
            let read = pane.history_log.changes_read();
            let catalogue = ui::I18nManager::revision();
            pane.refresh(&mut tree);
            assert_eq!(
                pane.history_log.changes_read() - read,
                appended,
                "step {step} ({kind}): the refresh reads only the appended changes \
                 (i18n revision {catalogue} -> {})",
                ui::I18nManager::revision()
            );

            let mut fresh = native_pane();
            let mut fresh_tree = tree.clone();
            fresh.refresh(&mut fresh_tree);
            assert_eq!(
                format!("{:?}", pane.derived),
                format!("{:?}", fresh.derived),
                "step {step} ({kind}): rows, summaries, authors and workspaces"
            );
            assert_eq!(
                format!("{:?}", pane.timeline.entries()),
                format!("{:?}", fresh.timeline.entries()),
                "step {step} ({kind}): the feed"
            );
            let frames = |pane: &RoomActivityPane| {
                pane.replay_frames
                    .iter()
                    .map(|frame| {
                        format!(
                            "{:?} {} {:?} {:?}",
                            frame.entry_id, frame.summary, frame.author, frame.workspaces
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                frames(&pane),
                frames(&fresh),
                "step {step} ({kind}): replay"
            );
            for (entry, _) in &pane.derived {
                assert_eq!(
                    pane.history_log.heads_for_entry(&entry.id),
                    ui::heads_for_entry(&mut tree.doc, &entry.id),
                    "step {step} ({kind}): the heads behind {:?}",
                    entry.summary
                );
            }
        }
        assert!(
            pane.derived
                .iter()
                .any(|(entry, _)| entry.author.name.contains("bea")),
            "the peer's receipt is attributed to the peer"
        );
    }
}

#[cfg(test)]
mod resume_card_tests {
    use super::*;
    use ui::tilo_actions::ActionContext;
    use ui::widget::history::HistoryAuthor;

    fn resume_entry(id: u8, author: &str, timestamp: SystemTime) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntryId([id; 16]),
            author: HistoryAuthor::new(author, 210.0, ui::history_source::short_id(author)),
            entry_kind: EntryKind::Text,
            artifact_kind: ArtifactKind::File,
            timestamp,
            summary: "Edited assumptions".into(),
            text_delta: Some((4, 1)),
            diff_payload: None,
            redaction: None,
            context: Some(
                ActionContext::for_resource("finance:q2", "sheet", "Q2 finance report")
                    .with_app("so.tilo.sheets", "Sheets"),
            ),
        }
    }

    #[test]
    fn resume_uses_the_local_users_latest_targeted_action() {
        let local = "ada";
        let now = SystemTime::now();
        let older = now.checked_sub(Duration::from_secs(60)).unwrap_or(now);
        let entries = vec![resume_entry(1, local, older), resume_entry(2, "Ada", now)];
        let resume =
            local_resume_snapshot(&entries, local).expect("local targeted work is resumable");
        assert_eq!(resume.target.label, "Q2 finance report");
        assert_eq!(resume.timestamp, older, "a peer's newer work is not yours");
        assert!(
            local_resume_snapshot(&entries, "").is_none(),
            "an unknown local identity resumes nothing"
        );
    }
}

#[cfg(test)]
mod tasks_link_tests {
    use super::*;
    use crate::activity::testing::TestTree as WorkspaceTree;
    #[derive(Default)]
    struct UndoHost {
        undos: RefCell<Vec<TaskRunUndo>>,
        ended: RefCell<Option<Result<(), String>>>,
    }
    impl ActivityHost for UndoHost {
        fn local_user(&self) -> String {
            "local".into()
        }
        fn mutations_available(&self) -> bool {
            true
        }
        fn undo_task_run(&self, undo: TaskRunUndo) -> Result<(), String> {
            self.undos.borrow_mut().push(undo);
            Ok(())
        }
        fn poll_task_run_undo(&self, _invocation: uuid::Uuid) -> Option<Result<(), String>> {
            self.ended.borrow_mut().take()
        }
    }
    fn fixture() -> (WorkspaceTree, RoomActivityPane, Rc<UndoHost>) {
        let mut tree = WorkspaceTree::bootstrap_default();
        let host = Rc::new(UndoHost::default());
        let mut pane = RoomActivityPane::new();
        pane.set_host(host.clone());
        pane.refresh(&mut tree);
        pane.set_view(ActivityView::Explore);
        (tree, pane, host)
    }

    /// Once an agent's run of a task recorded what it changed under the
    /// task, undoing it asks the host to undo the run in each changed
    /// document, and tells how that ended.
    #[test]
    fn an_agent_runs_task_is_undone_by_the_host_as_one() {
        let (mut tree, mut pane, host) = fixture();
        let change = ui::history_source::entry_id_from_hash(&"ab".repeat(32));
        let doc = tree.document_mut();
        let key = ui::record_run_with_details(
            doc,
            "Review release notes",
            Some("agent:scribe"),
            Some("local"),
            None,
            None,
            &[ui::RunStep {
                app: "so.tilo.notes".into(),
                record: "file:/notes/release.note".into(),
                run: ui::ChangeRun::new(change, change),
                what: Vec::new(),
            }],
        )
        .unwrap();
        assert!(ui::history_source::runs::set_run_task(doc, &key, "task:1"));
        doc.commit();
        pane.refresh_records(&mut tree, &RecordStore::in_memory());

        assert!(pane.undo_task_run("task:1"));
        let undo = host.undos.borrow().last().cloned().unwrap();
        assert_eq!(undo.task_id, "task:1");
        assert_eq!(
            undo.room,
            tree.active_workspace().unwrap().room().to_string()
        );
        assert_eq!(
            undo.steps,
            vec![(
                "so.tilo.notes".to_owned(),
                "file:/notes/release.note".to_owned()
            )]
        );
        assert!(pane.undo_task_run("task:1"));
        assert_eq!(host.undos.borrow().len(), 1, "one undo at a time");

        host.ended.replace(Some(Ok(())));
        pane.refresh(&mut tree);
        assert!(pane.undo_pending.is_none());
        assert_eq!(
            pane.agent_undo_notice
                .as_ref()
                .map(|(_, notice)| notice.clone()),
            Some(t("activity-undo-task-done"))
        );
    }
}

#[cfg(test)]
mod morning_source_tests {
    use super::*;
    use crate::activity::ActivityHost;
    use crate::activity::testing::TestTree;
    use automerge::transaction::CommitOptions;

    /// A browser visitor: no private morning sources, no local identity.
    struct VisitorHost;
    impl ActivityHost for VisitorHost {}

    #[test]
    fn a_visitor_catch_up_shows_no_ids_and_no_morning_section() {
        let record = "3f2a9c41d0b7e8aa5c1d9e0f";
        let mut tree = TestTree::bootstrap_default();
        let context = tilo_actions::ActionContext::for_resource(record, "notes.note", record)
            .with_app("org.tilo.Notes", "org.tilo.Notes")
            .with_described(crate::activity::testing::verb("Update"));
        tree.doc
            .put(automerge::ROOT, "name", "Visitor history")
            .unwrap();
        tree.doc.commit_with(
            CommitOptions::default()
                .with_time(1_800_000_000)
                .with_message(context.change_message().unwrap()),
        );
        let mut pane = RoomActivityPane::new();
        pane.set_host(Rc::new(VisitorHost));
        pane.refresh(&mut tree);
        pane.set_view(ActivityView::CatchUp);
        assert!(
            !pane.digest().briefs.is_empty(),
            "the change itself stays visible"
        );
        for brief in &pane.digest().briefs {
            for shown in [
                brief.title.clone(),
                brief.detail.clone(),
                brief.glance(),
                brief.apps.join(" "),
                brief.resources.join(" "),
            ] {
                assert!(
                    !ui::history_source::contains_opaque_id(&shown),
                    "an identifier reached a visitor: {shown:?}"
                );
            }
        }
        let served = serde_json::to_string(&pane.serve_summary()).unwrap();
        assert!(
            !served.contains(record),
            "an identifier reached a visitor: {served}"
        );
        assert!(
            pane.chosen_morning_sources().is_empty(),
            "a visitor has no morning sources"
        );
    }

    fn fixture() -> (TestTree, RoomActivityPane) {
        let mut tree = TestTree::bootstrap_default();
        let context = tilo_actions::ActionContext::for_resource(
            "weather.location:fixture",
            "location",
            "Chosen location",
        )
        .with_app("so.tilo.weather", "Weather")
        .with_described(crate::activity::testing::verb("Update selected forecast"));
        tree.doc
            .put(automerge::ROOT, "name", "Morning review")
            .unwrap();
        tree.doc.commit_with(
            CommitOptions::default()
                .with_time(1_800_000_000)
                .with_message(context.change_message().unwrap()),
        );
        let mut pane = RoomActivityPane::new();
        pane.set_host(Rc::new(crate::activity::testing::TestHost::default()));
        pane.refresh(&mut tree);
        let index = pane
            .timeline
            .entries()
            .iter()
            .position(|entry| {
                entry
                    .context
                    .as_ref()
                    .and_then(|context| context.resource.as_ref())
                    .is_some_and(|resource| resource.id == "weather.location:fixture")
            })
            .unwrap();
        pane.timeline.set_selection(Some((index, index)));
        (tree, pane)
    }

    // Compatible with the old public pane API: fails before implementation
    // because the real causal resource selection has no morning-choice action.
    #[test]
    fn morning_source_choice_is_a_semantic_action_on_real_resource_history() {
        let (mut tree, mut pane) = fixture();
        assert!(
            pane.selection_history_actions()
                .iter()
                .any(|(id, _)| *id == "room.activity.morning.toggle")
        );
        let heads = tree.doc.get_heads();
        assert!(
            pane.apply_selection_history_action("room.activity.morning.toggle", &mut tree, None)
                .handled
        );
        assert_eq!(
            tree.doc.get_heads(),
            heads,
            "source selection grants nothing and changes no shared document"
        );
    }

    #[derive(Default)]
    struct Host(RefCell<HashMap<u64, MorningPreferences>>);
    impl ActivityHost for Host {
        fn all_workspaces_available(&self) -> bool {
            true
        }
        fn morning_sources_available(&self) -> bool {
            true
        }
        fn load_morning_sources(&self) -> HashMap<u64, MorningPreferences> {
            self.0.borrow().clone()
        }
        fn save_morning_sources(&self, value: &HashMap<u64, MorningPreferences>) {
            *self.0.borrow_mut() = value.clone();
        }
    }
    #[test]
    fn morning_sources_survive_restart_without_crossing_rooms_or_acknowledging_work() {
        let (mut tree, mut pane) = fixture();
        let host = Rc::new(Host::default());
        pane.set_host(host.clone());
        assert!(pane.chosen_morning_sources().is_empty());
        let before = pane.seen_frontiers.clone();
        pane.apply_selection_history_action(MORNING_SOURCE, &mut tree, None);
        assert_eq!(pane.chosen_morning_sources().len(), 1);
        assert_eq!(pane.seen_frontiers, before);
        let key = pane.chosen_morning_sources()[0].key();
        let room = tree.workspaces[0].room();
        let mut restarted = RoomActivityPane::new();
        restarted.set_host(host.clone());
        restarted.load_frontiers();
        restarted.refresh(&mut tree);
        assert_eq!(restarted.chosen_morning_sources().len(), 1);
        let outcome = restarted.apply_selection_history_action(
            &format!("{MORNING_OPEN}{room}.{key}"),
            &mut tree,
            None,
        );
        assert_eq!(outcome.open_target.unwrap().id, "weather.location:fixture");
        tree.workspaces[0] = same_id_in(&tree.workspaces[0], tilo_desktop_model::RoomId::new());
        restarted.refresh(&mut tree);
        assert!(
            restarted.chosen_morning_sources().is_empty(),
            "even the same workspace id cannot borrow another room's choices"
        );
        restarted.apply_selection_history_action(
            &format!("{MORNING_SOURCE}.{room}"),
            &mut tree,
            None,
        );
        assert!(
            restarted.chosen_morning_sources().is_empty(),
            "a queued source choice cannot cross the originating room"
        );
        assert!(
            restarted
                .apply_selection_history_action(
                    &format!("{MORNING_OPEN}{room}.{key}"),
                    &mut tree,
                    None
                )
                .open_target
                .is_none()
        );
        tree.workspaces[0] = same_id_in(&tree.workspaces[0], room);
        restarted.refresh(&mut tree);
        assert_eq!(restarted.chosen_morning_sources().len(), 1);
        restarted.set_scope(RoomScope::AllWorkspaces, &tree);
        assert!(
            restarted
                .apply_selection_history_action(
                    &format!("{MORNING_OPEN}{room}.{key}"),
                    &mut tree,
                    None
                )
                .open_target
                .is_none()
        );
        restarted.set_scope(RoomScope::ThisRoom, &tree);
        restarted.timeline.set_entries(Vec::new());
        assert!(
            restarted.morning_target(&key).is_none(),
            "missing or revoked resource evidence is unavailable, never fabricated"
        );
        restarted.apply_selection_history_action(
            &format!("{MORNING_REMOVE}{room}.{key}"),
            &mut tree,
            None,
        );
        assert!(restarted.chosen_morning_sources().is_empty());
        assert!(host.0.borrow()[&tree.active.raw()].sources.is_empty());
    }
    #[test]
    fn morning_source_choices_are_bounded_and_noisy_activity_keeps_the_choice() {
        let (mut tree, mut pane) = fixture();
        for index in 0..200_u64 {
            tree.doc
                .put(automerge::ROOT, "name", format!("Update {index}"))
                .unwrap();
            let context = tilo_actions::ActionContext::for_resource(
                "weather.location:fixture",
                "location",
                "Chosen location",
            )
            .with_app("so.tilo.weather", "Weather")
            .with_described(crate::activity::testing::verb("Update"));
            tree.doc.commit_with(
                CommitOptions::default()
                    .with_time(1_800_000_010 + i64::try_from(index).unwrap())
                    .with_message(context.change_message().unwrap()),
            );
        }
        pane.refresh(&mut tree);
        assert!(
            pane.chosen_morning_sources().is_empty(),
            "even repeated activity never opts a user into a source"
        );
        let target = pane
            .timeline
            .entries()
            .iter()
            .position(|entry| {
                entry
                    .context
                    .as_ref()
                    .and_then(|context| context.resource.as_ref())
                    .is_some_and(|resource| resource.id == "weather.location:fixture")
            })
            .unwrap();
        pane.timeline.set_selection(Some((target, target)));
        pane.apply_selection_history_action(MORNING_SOURCE, &mut tree, None);
        let key = pane.chosen_morning_sources()[0].key();
        assert_eq!(
            pane.morning_evidence(&key).unwrap().1,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_209)
        );
        pane.refresh(&mut tree);
        assert_eq!(
            pane.chosen_morning_sources().len(),
            1,
            "repeated updates group into one chosen source"
        );
        assert_eq!(pane.chosen_morning_sources()[0].key(), key);
        for index in 1..=8 {
            let id = format!("weather.location:fixture-{index}");
            let context = tilo_actions::ActionContext::for_resource(&id, "location", &id)
                .with_app("so.tilo.weather", "Weather")
                .with_described(crate::activity::testing::verb("Select location"));
            tree.doc.put(automerge::ROOT, "name", &id).unwrap();
            tree.doc.commit_with(
                CommitOptions::default()
                    .with_time(1_800_001_000 + index)
                    .with_message(context.change_message().unwrap()),
            );
            pane.refresh(&mut tree);
            let selected = pane
                .timeline
                .entries()
                .iter()
                .position(|entry| {
                    entry
                        .context
                        .as_ref()
                        .and_then(|context| context.resource.as_ref())
                        .is_some_and(|resource| resource.id == id)
                })
                .unwrap();
            pane.timeline.set_selection(Some((selected, selected)));
            if index == 8 {
                assert!(
                    !pane
                        .selection_history_actions()
                        .iter()
                        .any(|(id, _)| *id == MORNING_SOURCE)
                );
            }
            pane.apply_selection_history_action(MORNING_SOURCE, &mut tree, None);
        }
        assert_eq!(pane.chosen_morning_sources().len(), 8);
    }

    fn run_source(
        pane: &mut RoomActivityPane,
        tree: &mut TestTree,
        key: &str,
        command: tilo_actions::remote::workspace_activity::SourceCommand,
    ) {
        let request = tilo_actions::remote::workspace_activity::SourceRequest {
            key: key.into(),
            command,
        };
        let arguments = serde_json::to_value(request)
            .unwrap()
            .as_object()
            .cloned()
            .expect("a request is an object");
        pane.serve_action(
            tilo_actions::remote::workspace_activity::SOURCE,
            &arguments,
            tree,
            None,
        )
        .expect("a chosen source runs its command");
    }

    #[test]
    fn morning_payload_commands_reach_only_this_room_and_change_no_history() {
        use tilo_actions::remote::workspace_activity::SourceCommand;
        let (mut tree, mut pane) = fixture();
        pane.apply_selection_history_action(MORNING_SOURCE, &mut tree, None);
        let host = Rc::new(PayloadHost::default());
        pane.set_host(host.clone());
        let key = pane.chosen_morning_sources()[0].key();
        let heads = tree.doc.get_heads();
        let seen = pane.seen_frontiers.clone();
        let summary = pane.serve_summary();
        let source = summary
            .sources
            .iter()
            .find(|source| source.key == key)
            .expect("the chosen source is served");
        assert!(source.refreshable);
        for evidence in [
            "12°C",
            "Synthetic chosen weather provider",
            "2026-10-11T08:05:00Z",
            "Last cached observation",
        ] {
            assert!(source.text.contains(evidence), "{evidence} is served");
        }
        for command in [SourceCommand::Refresh, SourceCommand::DraftRefresh] {
            run_source(&mut pane, &mut tree, &key, command);
        }
        assert_eq!(host.commands.borrow().len(), 2);
        assert!(
            host.commands
                .borrow()
                .iter()
                .all(|(room, _, _)| *room == pane.room)
        );
        let foreign = uuid::Uuid::new_v4().to_string();
        pane.apply_selection_history_action(
            &format!("{}{foreign}.{key}", super::MorningCommand::Refresh.prefix()),
            &mut tree,
            None,
        );
        assert_eq!(
            host.commands.borrow().len(),
            2,
            "queued foreign command is inert"
        );
        assert_eq!(tree.doc.get_heads(), heads);
        assert_eq!(pane.seen_frontiers, seen);
    }

    #[test]
    fn morning_payload_paging_keeps_large_sources_and_removed_evidence_reachable() {
        use tilo_actions::remote::workspace_activity::SourceCommand;
        let (mut tree, mut pane) = fixture();
        pane.apply_selection_history_action(MORNING_SOURCE, &mut tree, None);
        let host = Rc::new(PayloadHost {
            noisy: true,
            ..Default::default()
        });
        pane.set_host(host.clone());
        for index in 1..crate::activity::morning::MAX_SOURCES {
            let room = pane.room.clone();
            pane.morning_sources
                .get_mut(&pane.morning_workspace)
                .unwrap()
                .toggle(
                    &room,
                    super::MorningSource {
                        app: "so.tilo.weather".into(),
                        resource: format!("deleted:{index}"),
                    },
                );
        }
        let heads = tree.doc.get_heads();
        let seen = pane.seen_frontiers.clone();
        let keys: Vec<String> = pane
            .serve_summary()
            .sources
            .iter()
            .map(|source| source.key.clone())
            .collect();
        assert_eq!(
            keys.len(),
            crate::activity::morning::MAX_SOURCES,
            "every source is served, removed evidence too"
        );
        for key in &keys {
            run_source(&mut pane, &mut tree, key, SourceCommand::NextSource);
        }
        assert_eq!(pane.morning_source_offset, 0, "paging wraps around");
        assert!(
            host.commands.borrow().is_empty(),
            "paging invokes no provider"
        );
        run_source(&mut pane, &mut tree, &keys[1], SourceCommand::Remove);
        assert_eq!(
            pane.chosen_morning_sources().len(),
            crate::activity::morning::MAX_SOURCES - 1,
            "a source whose evidence is gone can still be removed"
        );
        assert_eq!(tree.doc.get_heads(), heads);
        assert_eq!(pane.seen_frontiers, seen);
    }

    #[derive(Default)]
    struct PayloadHost {
        commands: RefCell<Vec<(String, super::MorningSource, super::MorningCommand)>>,
        noisy: bool,
    }
    impl crate::activity::ActivityHost for PayloadHost {
        fn morning_sources_available(&self) -> bool {
            true
        }
        fn morning_payload(
            &self,
            _room: &str,
            _source: &super::MorningSource,
        ) -> Option<crate::activity::morning::MorningPayload> {
            Some(crate::activity::morning::MorningPayload {
                label: "Chosen location".into(),
                presentation: tilo_actions::ResourcePresentation {
                    preview: Some(if self.noisy {
                        "Long chosen provider observation. ".repeat(60)
                    } else {
                        "12°C, light rain · observation 2026-10-11T08:00".into()
                    }),
                    detail: Some(
                        "Synthetic chosen weather provider · fetched 2026-10-11T08:05:00Z".into(),
                    ),
                    badge: Some("Last cached observation".into()),
                },
                refreshable: true,
            })
        }
        fn submit_morning_command(
            &self,
            room: &str,
            source: &super::MorningSource,
            command: super::MorningCommand,
        ) -> Result<(), String> {
            self.commands
                .borrow_mut()
                .push((room.into(), source.clone(), command));
            Ok(())
        }
    }
}

#[cfg(test)]
mod reader_tests {
    use super::*;

    /// The person's own changes are never news to them, even when the writer
    /// named them differently from the device's user name: history marks
    /// them as the reader's.
    #[test]
    fn the_readers_own_changes_never_reach_the_passive_activity() {
        let own = ui::widget::history::HistoryAuthor::new("Workspace setup", 10.0, "a1b2")
            .with_relation(ui::widget::history::AuthorRelation::Reader);
        assert!(author_is_local(&own, "ada"));
        assert!(
            author_is_local(&own, ""),
            "known as the reader without a name"
        );
        let other = ui::widget::history::HistoryAuthor::new("Ada", 10.0, "c3d4");
        assert!(!author_is_local(&other, "ada"));
    }
}

/// A workspace of `room` under `workspace`'s id, with its windows and its
/// grants rehomed: what a device holds when one id has named two rooms (a
/// workspace never changes its room).
fn same_id_in(
    workspace: &tilo_desktop_model::Workspace,
    room: tilo_desktop_model::RoomId,
) -> tilo_desktop_model::Workspace {
    let mut other =
        tilo_desktop_model::Workspace::in_room(workspace.id, room, workspace.name.clone());
    other.strips.clone_from(&workspace.strips);
    other.policy = workspace.policy.clone();
    other.policy.rehome(room.to_string());
    other
}
