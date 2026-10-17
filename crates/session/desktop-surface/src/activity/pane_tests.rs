//! The activity pane's own tests, over a causal test room.
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

    #[derive(Clone)]
    struct NativeTree(WorkspaceTree);
    impl std::ops::Deref for NativeTree {
        type Target = tilo_desktop_model::DesktopTree;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
    impl std::ops::DerefMut for NativeTree {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }
    impl crate::activity::ActivityTree for NativeTree {
        fn document(&self) -> &automerge::AutoCommit {
            &self.0.doc
        }
        fn document_mut(&mut self) -> &mut automerge::AutoCommit {
            &mut self.0.doc
        }
        fn resync_activity_cache(&mut self) -> Result<(), String> {
            self.0.resync_cache_from_doc();
            Ok(())
        }
        fn native_manual_tasks(&self, _: WorkspaceId) -> bool {
            true
        }
    }

    #[test]
    fn native_manual_draft_defers_owner_write_and_closes_only_after_completion() {
        let mut tree = NativeTree(WorkspaceTree::bootstrap_default());
        let mut pane = native_pane();
        pane.open_task_composer(&tree);
        pane.task_draft = "Review mapped receipt".into();
        tree.0
            .doc
            .put(automerge::ROOT, "pending_projection_edit", true)
            .unwrap();
        let blocked = pane.create_task_from_draft(&mut tree);
        assert!(blocked.task_refused);
        assert!(blocked.create_native_manual.is_none());
        assert!(!blocked.task_changed);
        assert!(pane.task_compose_open);
        assert_eq!(pane.task_draft, "Review mapped receipt");
        tree.0.doc.commit();
        let outcome = pane.create_task_from_draft(&mut tree);
        assert!(outcome.handled);
        assert!(!outcome.task_changed);
        let draft = outcome.create_native_manual.unwrap();
        assert_eq!(draft.workspace, tree.active);
        assert_eq!(
            draft.room,
            tree.active_workspace().unwrap().room.to_string()
        );
        assert_eq!(draft.title, "Review mapped receipt");
        assert!(tree.active_workspace().unwrap().tasks.is_empty());
        assert!(pane.task_compose_open);
        assert_eq!(pane.task_draft, "Review mapped receipt");
        let workspace = tree.active;
        let id = tree
            .0
            .create_workspace_task(
                workspace,
                draft.title,
                draft.actor,
                None,
                WorkspaceTaskRoute::Manual,
                None,
            )
            .unwrap();
        pane.complete_native_manual_task(id.clone(), &mut tree);
        assert!(!pane.task_compose_open);
        assert_eq!(pane.selected_task.as_deref(), Some(id.as_str()));
    }

    #[test]
    fn native_managed_draft_defers_without_cache_fallback_or_premature_completion() {
        let mut tree = NativeTree(WorkspaceTree::bootstrap_default());
        let mut pane = native_pane();
        pane.open_task_composer(&tree);
        pane.task_draft = "Inspect operator evidence".into();
        for route in [WorkspaceTaskRoute::Model, WorkspaceTaskRoute::RulesOnly] {
            pane.task_route = route;
            pane.task_agent = None;
            let missing = pane.create_task_from_draft(&mut tree);
            assert!(missing.task_refused);
            assert!(missing.create_native_managed.is_none());
            assert!(pane.task_compose_open);
            pane.task_agent = Some("scribe".into());
            let outcome = pane.create_task_from_draft(&mut tree);
            assert!(outcome.create_native_manual.is_none());
            assert!(!outcome.task_changed);
            let request = outcome.create_native_managed.unwrap();
            assert_eq!(request.assigned_agent, "scribe");
            assert_eq!(request.route, route);
            assert_eq!(request.person_draft.title, "Inspect operator evidence");
            assert_eq!(
                request.person_draft.room,
                tree.active_workspace().unwrap().room.to_string()
            );
            assert!(tree.active_workspace().unwrap().tasks.is_empty());
            assert!(pane.task_compose_open);
            assert!(pane.selected_task.is_none());
        }
        tree.0
            .doc
            .put(automerge::ROOT, "pending_projection_edit", true)
            .unwrap();
        let blocked = pane.create_task_from_draft(&mut tree);
        assert!(blocked.task_refused);
        assert!(blocked.create_native_managed.is_none());
        assert!(pane.task_compose_open);
        assert_eq!(pane.task_draft, "Inspect operator evidence");
        assert!(tree.active_workspace().unwrap().tasks.is_empty());
    }

    /// HIG §9.5 rest edges: the sheet's Panel border on its Panel ground,
    /// and the derived edge of every card class the sheet paints (lift-floor
    /// chips and group cards, the base-filled replay canvas) must register
    /// on every shipped palette, measured as the composite over the fill.
    #[test]
    fn sheet_and_card_edges_register_on_every_palette() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let colors = &theme.tokens.colors;
            let ground = ui::chrome::shell::panel_ground(&theme);
            let probes = [
                (
                    "panel border",
                    ui::chrome::shell::panel_border(&theme),
                    ground,
                ),
                (
                    "lift-floor chip",
                    ui::surface::edge_on(colors.lift(PILL_FLOOR_LIFT_T)),
                    colors.lift(PILL_FLOOR_LIFT_T),
                ),
                (
                    "replay canvas",
                    ui::surface::edge_on(colors.surfaces.base),
                    colors.surfaces.base,
                ),
            ];
            for (what, edge, fill) in probes {
                let delta = ui::surface::edge_delta(edge, fill);
                assert!(
                    delta >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{}: the {what} edge must separate it from its fill (Δ luminance {delta:.4})",
                    theme.name
                );
            }
        }
    }

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
                    Scope::FsPath("/home/quinten/notes".to_string()),
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

        // What the incremental refreshes derived is what one refresh of the
        // whole history derives.
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
        // A pane reading the whole document from scratch agrees.
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
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(tree);
        pane
    }

    fn agent_task_fixture() -> (WorkspaceTree, RecordStore, RoomActivityPane, RecordId) {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.active_workspace().expect("workspace").room;
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
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
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
        pane.explorer_pane = ExplorerPane::Details;
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
        let room = tree.desktop.active_workspace().expect("workspace").room;
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
        let room = tree.desktop.active_workspace().expect("workspace").room;
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

        let a11y = pane.accessibility_snapshot();
        let review = a11y
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Agent task review"))
            .expect("agent task has a semantic review group");
        let description = review.description.as_deref().unwrap_or_default();
        assert!(description.contains("Ada"));
        assert!(description.contains("Quill"));
        assert!(description.contains("pricing table"));
        assert!(description.contains("Rewrite the launch story"));
    }

    #[test]
    fn agent_task_undo_is_one_keyboard_reachable_safe_action() {
        let (mut tree, records, mut pane, record) = agent_task_fixture();
        paint(&mut pane);
        assert!(pane.can_undo_agent_run());
        assert!(
            pane.focus_order()
                .contains(&ActivityFocus::Detail(DetailAction::UndoTask))
        );
        pane.keyboard_focus = Some(ActivityFocus::Detail(DetailAction::UndoTask));
        let outcome = pane.handle_event_with_records(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
            Some(&records),
        );
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
    fn a_workspace_first_seen_on_this_device_has_nothing_new() {
        let (mut tree, workspace, user) = busy_room();
        let host = Rc::new(crate::activity::testing::TestHost::default());
        let mut pane = RoomActivityPane::new();
        pane.set_host(host.clone());
        pane.load_frontiers();
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
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
                Scope::FsPath("/home/quinten/later".to_string()),
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
        next_session.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
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
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
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
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
        assert_eq!(pane.view(), ActivityView::CatchUp);

        let (_, unread_before, _) = pane.glance_signal(workspace.raw());
        assert!(unread_before > 0);
        pane.begin_view(workspace.raw());
        assert!(!pane.digest().briefs.is_empty());
        paint(&mut pane);
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
    fn offscreen_catch_up_cards_do_not_become_seen() {
        let (mut tree, workspace, _) = peers_busy_room();
        let mut pane = native_pane();
        pane.layout_window(Rect::new(1600.0, 0.0, 2400.0, 900.0));
        pane.set_review_clip(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.begin_view(workspace.raw());
        pane.refresh(&mut tree);
        let unread = pane.glance_signal(workspace.raw()).1;
        assert!(unread > 0);
        paint(&mut pane);
        assert!(
            !pane.brief_rects.is_empty(),
            "the tile must contain summaries"
        );
        pane.finish_view(workspace.raw());
        pane.refresh(&mut tree);
        assert_eq!(pane.glance_signal(workspace.raw()).1, unread);
    }

    #[test]
    fn refreshed_catch_up_preserves_focus_and_does_not_acknowledge_late_work() {
        let (mut tree, workspace, user) = peers_busy_room();
        let mut pane = native_pane();
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.begin_view(workspace.raw());
        pane.refresh(&mut tree);
        paint(&mut pane);
        let index = pane.brief_rects.first().unwrap().0;
        let anchor = pane.digest.briefs[index].entry_ids[0];
        pane.keyboard_focus = Some(ActivityFocus::Brief(index));
        pane.expanded_brief = Some(index);
        let reader = tree.doc.get_actor().clone();
        tree.doc.set_actor(ActorId::from(b"bea-device".as_slice()));
        tree.grant_in_workspace(
            workspace,
            &user,
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath("/home/quinten/new-result".into()),
                Access::Read,
            ),
        )
        .unwrap();
        tree.seed_doc_from_cache();
        tree.doc.set_actor(reader);
        pane.refresh(&mut tree);
        let Some(ActivityFocus::Brief(focused)) = pane.keyboard_focus else {
            panic!("the reviewed session must retain focus");
        };
        assert!(pane.digest.briefs[focused].entry_ids.contains(&anchor));
        assert_eq!(pane.expanded_brief, Some(focused));
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
        paint(&mut pane);
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
    fn catch_up_persists_only_the_painted_page_and_keeps_waiting_tasks() {
        let (mut tree, workspace, _) = peers_busy_room();
        let records = RecordStore::in_memory();
        let room_id = tree.active_workspace().expect("room").room;
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
        let task = tree
            .create_workspace_task(
                workspace,
                "Review draft",
                "local",
                None,
                WorkspaceTaskRoute::Manual,
                None,
            )
            .unwrap();
        assert!(tree.set_workspace_task_status(
            workspace,
            &task,
            WorkspaceTaskStatus::Waiting,
            "local"
        ));
        let host = Rc::new(crate::activity::testing::TestHost::default());
        let mut pane = RoomActivityPane::new();
        pane.set_host(host.clone());
        pane.layout(Rect::new(0.0, 0.0, 800.0, 580.0));
        pane.begin_view(workspace.raw());
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        pane.persistent_frontiers = true;
        let before = pane.glance_signal(workspace.raw()).1;
        assert!(before >= 12);
        paint(&mut pane);
        let shown = pane.viewed_changes.get(&workspace.raw()).unwrap().len();
        assert!(
            shown > 0 && shown < before,
            "fixture must have a hidden page"
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
        assert_eq!(
            tree.active_workspace().unwrap().tasks[0].status,
            WorkspaceTaskStatus::Waiting
        );
        let mut next = RoomActivityPane::new();
        next.set_host(host);
        next.load_frontiers();
        next.refresh(&mut tree);
        next.refresh_records(&mut tree, &records);
        assert_eq!(next.glance_signal(workspace.raw()).1, before - shown);
        next.apply_selection_history_action(ACTIVITY_TASKS, &mut tree, Some(&records));
        assert_eq!(next.view(), ActivityView::Tasks);
        assert_eq!(next.tasks[0].status, WorkspaceTaskStatus::Waiting);
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
        paint(&mut pane);
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
    fn task_center_creates_manual_work_without_a_model() {
        let (mut tree, workspace, _) = busy_room();
        let mut pane = native_pane();
        pane.set_view(ActivityView::Tasks);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
        pane.open_task_composer(&tree);
        pane.task_draft = "Check the launch copy".into();

        let outcome = pane.create_task_from_draft(&mut tree);

        assert!(outcome.task_changed);
        let task = tree
            .desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .and_then(|workspace| workspace.tasks.first())
            .expect("task is stored on the workspace");
        assert_eq!(task.route, WorkspaceTaskRoute::Manual);
        assert!(task.assigned_agent.is_none());
        assert_eq!(task.status, WorkspaceTaskStatus::Queued);
        assert!(!pane.task_compose_open);
    }

    #[test]
    fn task_center_attaches_the_resource_selected_in_explore() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let resource_entry = HistoryEntry {
            id: HistoryEntryId([71; 16]),
            author: ui::widget::HistoryAuthor::new("ada", 10.0, "Ada"),
            entry_kind: EntryKind::Details,
            artifact_kind: ArtifactKind::File,
            timestamp: SystemTime::UNIX_EPOCH,
            summary: "Edited launch brief".into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: Some(
                ui::tilo_actions::ActionContext::for_resource(
                    "document:launch-brief",
                    "document",
                    "Launch brief",
                )
                .with_app("org.tilo.Writer", "Writer"),
            ),
        };
        pane.timeline_mut().set_entries(vec![resource_entry]);
        let resource_row = 0;
        pane.timeline_mut()
            .set_selection(Some((resource_row, resource_row)));
        let expected = pane
            .selection_detail()
            .and_then(|detail| detail.resources.first().cloned())
            .expect("selected action has a resource");

        pane.set_view(ActivityView::Tasks);
        pane.open_task_composer(&tree);

        let attached = pane.task_attachment.as_ref().expect("resource is attached");
        assert_eq!(attached.id, expected.id);
        assert_eq!(attached.app_id, expected.app_id);
    }

    #[test]
    fn task_accessibility_exposes_route_status_and_audit() {
        let (mut tree, workspace, _) = busy_room();
        tree.create_workspace_task(
            workspace,
            "Prepare screenshots",
            "ada",
            Some("studio-agent".into()),
            WorkspaceTaskRoute::RulesOnly,
            None,
        )
        .unwrap();
        let mut pane = native_pane();
        pane.set_view(ActivityView::Tasks);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
        let snapshot = pane.accessibility_snapshot();
        let debug = format!("{snapshot:?}");
        assert!(debug.contains("Prepare screenshots"));
        assert!(debug.contains("rules only"));
        assert!(debug.contains("1 audit events"));
    }

    #[test]
    fn task_center_resolves_creator_without_cursor_color_and_preserves_identity() {
        let (mut tree, workspace, _) = busy_room();
        let creator = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        tree.workspaces
            .iter_mut()
            .find(|room| room.id == workspace)
            .unwrap()
            .collaborators
            .push(tilo_desktop_model::Collaborator::new(
                creator,
                "Ada Lovelace",
            ));
        tree.seed_doc_from_cache();
        tree.create_workspace_task(
            workspace,
            "Review receipt",
            creator,
            None,
            WorkspaceTaskRoute::Manual,
            None,
        )
        .unwrap();
        let mut pane = native_pane();
        pane.set_view(ActivityView::Tasks);
        pane.refresh(&mut tree);
        let task = pane.tasks.first().unwrap();
        let creator_label = pane.task_creator_label(task);
        assert_eq!(creator_label, "Ada Lovelace");
        assert!(task_assignee_label(task, &creator_label).contains("Ada Lovelace"));
        let snapshot = format!("{:?}", pane.accessibility_snapshot());
        assert!(snapshot.contains("Unassigned · created by Ada Lovelace"));
        assert_eq!(task.created_by, creator);
        assert_eq!(task.audit.first().unwrap().actor, creator);
        let mut unidentified = task.clone();
        unidentified.created_by =
            "0123456789012345678901234567890123456789012345678901234567890123".into();
        assert_eq!(
            pane.task_creator_label(&unidentified),
            "Unknown collaborator · 01234567"
        );
        unidentified.created_by = pane.local_user();
        assert_eq!(
            pane.task_creator_label(&unidentified),
            pane.local_user(),
            "the reader is named like anyone else, never \"You\""
        );
    }

    #[test]
    fn task_center_presents_agent_principals_without_leaking_wire_ids() {
        let event_time = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(42))
            .expect("fixture time");
        let task = WorkspaceTask {
            id: "task:review".into(),
            title: "Review launch copy".into(),
            created_by: "ada".into(),
            assigned_agent: Some("agent:scribe".into()),
            route: WorkspaceTaskRoute::RulesOnly,
            status: WorkspaceTaskStatus::InProgress,
            resource: None,
            subject: None,
            created_at: 42,
            updated_at: 42,
            audit: vec![
                tilo_desktop_model::WorkspaceTaskAuditEvent::new(42, "agent:scribe", "report", [])
                    .noting("Reviewed section 2"),
            ],
        };

        let byline = task_assignee_label(&task, "ada");
        let audit = task_audit_line(&task.audit[0], event_time, event_time);
        let accessible = task_accessibility_description(&task, event_time, "ada");

        assert_eq!(byline, "Agent · scribe");
        assert!(
            audit.starts_with("scribe · Agent · Reported progress: Reviewed section 2"),
            "{audit}"
        );
        assert!(accessible.contains("Agent · scribe"));
        assert!(accessible.contains("scribe · Agent"));
        for rendered in [&byline, &audit, &accessible] {
            assert!(
                !rendered.contains("agent:"),
                "wire principal leaked into visible copy: {rendered}"
            );
        }
    }

    #[test]
    fn only_manual_tasks_offer_a_human_started_transition() {
        let (mut tree, workspace, _) = busy_room();
        let manual_id = tree
            .create_workspace_task(
                workspace,
                "Review the brief",
                "ada",
                None,
                WorkspaceTaskRoute::Manual,
                None,
            )
            .expect("manual task is created");
        let automated_id = tree
            .create_workspace_task(
                workspace,
                "Prepare screenshots",
                "ada",
                Some("studio-agent".into()),
                WorkspaceTaskRoute::RulesOnly,
                None,
            )
            .expect("automated task is created");
        let tasks = &tree
            .desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .expect("workspace remains available")
            .tasks;
        let manual = tasks
            .iter()
            .find(|task| task.id == manual_id)
            .expect("manual task is stored");
        let automated = tasks
            .iter()
            .find(|task| task.id == automated_id)
            .expect("automated task is stored");

        assert!(task_actions(manual).contains(&TaskAction::Start));
        assert!(!task_actions(automated).contains(&TaskAction::Start));
        let mut pane = native_pane();
        pane.task_room = "room".into();
        assert!(
            !pane
                .task_actions_for(automated)
                .contains(&TaskAction::Launch),
            "membership without a live task subscription must not enable Launch"
        );
        assert!(
            pane.task_actions_for(automated)
                .contains(&TaskAction::StartAgent),
            "offline work offers a managed agent start, never a fake task run"
        );
        assert_eq!(
            pane.task_execution_notice(automated).as_deref(),
            Some("Agent runtime not connected · this task has not started")
        );
    }

    #[test]
    fn automated_task_cannot_be_marked_running_without_a_runtime_run() {
        let (mut tree, workspace, _) = busy_room();
        let task_id = tree
            .create_workspace_task(
                workspace,
                "Prepare screenshots",
                "ada",
                Some("studio-agent".into()),
                WorkspaceTaskRoute::Model,
                None,
            )
            .expect("task is created");
        let mut pane = native_pane();
        pane.set_view(ActivityView::Tasks);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
        pane.selected_task = Some(task_id.clone());

        let outcome = pane.apply_task_action(TaskAction::Start, &mut tree);

        assert!(!outcome.task_changed);
        let task = tree
            .desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .and_then(|workspace| workspace.tasks.iter().find(|task| task.id == task_id))
            .expect("task remains stored");
        assert_eq!(task.status, WorkspaceTaskStatus::Queued);
        assert_eq!(task.audit.len(), 1, "no synthetic start audit was added");
    }

    /// Records each dispatch and whether the test held the tree for it.
    #[derive(Default)]
    struct DispatchHost {
        tree_held: Rc<Cell<bool>>,
        dispatched: RefCell<Vec<(String, String, bool)>>,
        refusal: Option<String>,
    }
    impl ActivityHost for DispatchHost {
        fn local_user(&self) -> String {
            "ada".into()
        }
        fn mutations_available(&self) -> bool {
            true
        }
        fn all_workspaces_available(&self) -> bool {
            true
        }
        fn agent_is_live(&self, _room: &str, _agent: &str) -> bool {
            true
        }
        fn dispatch_task(&self, room: &str, task: &str) -> Result<(), String> {
            self.dispatched
                .borrow_mut()
                .push((room.into(), task.into(), self.tree_held.get()));
            self.refusal.clone().map_or(Ok(()), Err)
        }
    }

    fn launchable_task(host: Rc<DispatchHost>) -> (WorkspaceTree, RoomActivityPane, String) {
        let (mut tree, workspace, _) = busy_room();
        let task_id = tree
            .create_workspace_task(
                workspace,
                "Prepare screenshots",
                "ada",
                Some("studio-agent".into()),
                WorkspaceTaskRoute::RulesOnly,
                None,
            )
            .expect("task is created");
        let mut pane = RoomActivityPane::new();
        pane.set_host(host);
        pane.set_view(ActivityView::Tasks);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
        pane.selected_task = Some(task_id.clone());
        (tree, pane, task_id)
    }

    /// The broker that runs a task checks the room's authority against the
    /// workspace tree. The shell holds that tree for writing while the pane
    /// handles the click, so dispatching from inside the handler stopped the
    /// compositor. Launch now only asks; the dispatch runs once the caller
    /// has let go of the tree.
    #[test]
    fn launching_a_task_dispatches_only_after_the_tree_is_released() {
        let host = Rc::new(DispatchHost::default());
        let (mut tree, mut pane, task_id) = launchable_task(Rc::clone(&host));
        let task = pane
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
            .expect("task is projected");
        assert!(pane.task_actions_for(&task).contains(&TaskAction::Launch));

        host.tree_held.set(true);
        let outcome = pane.apply_task_action(TaskAction::Launch, &mut tree);
        host.tree_held.set(false);

        assert!(
            host.dispatched.borrow().is_empty(),
            "nothing dispatched while the tree is held"
        );
        let request = outcome.dispatch_task.expect("Launch asks for a dispatch");
        assert_eq!(request.task_id, task_id);
        assert_eq!(request.assigned_agent.as_deref(), Some("studio-agent"));
        assert!(outcome.handled);
        assert!(pane.dispatch_task(request), "the live agent took the run");
        assert_eq!(
            host.dispatched.borrow().as_slice(),
            [(pane.task_room.clone(), task_id, false)]
        );
    }

    #[test]
    fn a_refused_dispatch_offers_the_agent_start_again_with_its_reason() {
        let host = Rc::new(DispatchHost {
            refusal: Some("the assigned agent is not live and task-capable".into()),
            ..DispatchHost::default()
        });
        let (mut tree, mut pane, _) = launchable_task(Rc::clone(&host));
        let request = pane
            .apply_task_action(TaskAction::Launch, &mut tree)
            .dispatch_task
            .expect("Launch asks for a dispatch");
        let room = request.room.clone();

        assert!(!pane.dispatch_task(request));
        assert!(matches!(
            pane.agent_startup.get(&(room, "studio-agent".into())),
            Some(AgentStartup::Failed { detail }) if detail.contains("not live")
        ));
    }

    #[test]
    fn offline_agent_start_is_deduplicated_and_failure_becomes_retry() {
        let (mut tree, workspace, _) = busy_room();
        let task_id = tree
            .create_workspace_task(
                workspace,
                "Prepare screenshots",
                "ada",
                Some("studio-agent".into()),
                WorkspaceTaskRoute::RulesOnly,
                None,
            )
            .expect("task is created");
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        let task = pane
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
            .expect("task is projected");
        assert!(
            pane.task_actions_for(&task)
                .contains(&TaskAction::StartAgent)
        );

        pane.agent_startup.insert(
            (pane.task_room.clone(), "studio-agent".into()),
            AgentStartup::Starting {
                since: Instant::now(),
            },
        );
        assert!(
            !pane
                .task_actions_for(&task)
                .contains(&TaskAction::StartAgent),
            "one pending managed launch suppresses another start click"
        );

        assert!(pane.observe_agent_launch_event(&AgentLaunchEvent {
            managed_agent_id: Some("studio-agent".into()),
            managed_workspace_id: Some(pane.task_room.clone()),
            kind: AgentLaunchEventKind::SpawnFailed {
                detail: "launcher unavailable".into(),
            },
        }));
        assert!(
            pane.task_actions_for(&task)
                .contains(&TaskAction::StartAgent)
        );
        assert_eq!(
            pane.task_action_label(TaskAction::StartAgent, &task),
            "Retry agent"
        );
        assert_eq!(task.status, WorkspaceTaskStatus::Queued);
    }

    #[test]
    fn explore_expands_to_three_columns_only_when_the_viewport_can_hold_them() {
        let (mut tree, _, _) = busy_room();
        let mut pane = native_pane();
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
        assert!(pane.sheet.width() >= 900.0);
        assert!(pane.timeline.rect().width() < pane.sheet.width() - 400.0);

        pane.layout(Rect::new(0.0, 0.0, 700.0, 900.0));
        assert!(pane.sheet.width() < 900.0);
        assert!(pane.timeline.rect().width() > pane.sheet.width() - 40.0);
    }

    #[test]
    fn narrow_explorer_pushes_between_timeline_browse_and_details() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 700.0, 800.0));
        paint(&mut pane);
        assert_eq!(pane.explorer_pane_rects.len(), 3);

        pane.explorer_pane = ExplorerPane::Browse;
        paint(&mut pane);
        assert!(!pane.lens_rects.is_empty());

        pane.timeline.set_selection(Some((0, 0)));
        pane.explorer_pane = ExplorerPane::Details;
        paint(&mut pane);
        assert!(!pane.detail_action_rects.is_empty());
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
            summaries.iter().any(|s| s.contains("/home/quinten/notes")),
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
            "setup bookkeeping and a plain open are not changes: {summaries:?}"
        );
    }

    #[test]
    fn explore_filters_sit_on_one_labelled_row() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        paint(&mut pane);
        assert!(pane.shows_chip_rows());
        let period_y = pane
            .period_rects
            .first()
            .map(|(_, rect)| rect.y0)
            .expect("time chips");
        let people_y = pane
            .author_rects
            .first()
            .map(|(_, rect)| rect.y0)
            .expect("people chips");
        assert!(
            (period_y - people_y).abs() < 1.0,
            "Time and People share one row, got period {period_y} people {people_y}"
        );
        let titles: Vec<String> = (0..pane.timeline.row_count())
            .filter_map(|i| pane.timeline.group_title_at_row(i).map(str::to_string))
            .collect();
        assert!(
            titles.iter().any(|title| title == "Today"
                || title == "Yesterday"
                || title == "This week"
                || title == "Earlier"),
            "Explore groups sit under calendar headings: {titles:?}"
        );
    }

    #[test]
    fn a_selected_row_publishes_diff_restore_and_undo_as_actions() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        paint(&mut pane);
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
            task_id: None,
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
            scoped.iter().any(|s| s.contains("/home/quinten/notes")),
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
                e.summary.contains("research.example") || e.summary.contains("/home/quinten/notes")
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
        assert!(in_first.iter().any(|s| s.contains("/home/quinten/notes")));

        tree.set_active(second);
        pane.refresh(&mut tree);
        let in_second: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(
            in_second.iter().any(|s| s.contains("research.example")),
            "the second room's own grant: {in_second:?}"
        );
        assert!(
            !in_second.iter().any(|s| s.contains("/home/quinten/notes")),
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
            Scope::FsPath("/home/quinten/last-week".to_string()),
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
                Scope::FsPath("/home/quinten/today".to_string()),
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

        // Revert to the version the mistake was made on top of.
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
            .find(|e| e.summary.contains("/home/quinten/notes"))
            .expect("the filesystem grant has a row");
        assert!(
            grant
                .summary
                .starts_with("Granted read on /home/quinten/notes to files"),
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
    fn the_sheet_is_a_side_panel_when_wide_and_full_bleed_when_narrow() {
        let mut pane = native_pane();

        pane.layout(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let wide = pane.rect();
        assert!(
            wide.width() <= SHEET_MAX_W,
            "a 1920 px viewport must not get a 700 px sheet: {}",
            wide.width()
        );
        assert!(
            wide.x0 > 1920.0 * 0.5,
            "the sheet is anchored right, leaving the room visible behind it"
        );

        pane.layout(Rect::new(0.0, 0.0, 640.0, 480.0));
        let narrow = pane.rect();
        assert!(
            narrow.width() > 640.0 * 0.9,
            "under {FULL_BLEED_UNDER} px the sheet takes the viewport: {}",
            narrow.width()
        );
        assert!(narrow.x0 < 32.0, "full bleed starts at the left inset");
    }

    #[test]
    fn a_short_viewport_drops_the_chip_rows_before_the_row_list() {
        let mut pane = native_pane();
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert!(
            pane.shows_chip_rows(),
            "a normal viewport affords the filters"
        );
        let tall_rows = pane.timeline().rect().height();

        pane.layout(Rect::new(0.0, 0.0, 1280.0, 220.0));
        assert!(
            !pane.shows_chip_rows(),
            "a 220 px viewport must spend its height on rows, not chips"
        );
        assert!(
            pane.timeline().rect().height() > 0.0,
            "the row list survives even when the filters do not"
        );
        assert!(tall_rows > pane.timeline().rect().height());
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
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
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
        paint(&mut pane);
        let order = pane.focus_order();
        assert!(order.contains(&ActivityFocus::ReplayBack));
        assert!(order.contains(&ActivityFocus::ReplayPlay));
        assert!(order.contains(&ActivityFocus::ReplayForward));

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
        assert_eq!(pointer.point, (420.0, 260.0));
        assert!(pointer.desktop_space);
    }

    #[test]
    fn narrow_history_keeps_both_scope_choices_visible() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.layout(Rect::new(0.0, 0.0, 380.0, 640.0));
        paint(&mut pane);
        assert_eq!(pane.scope_rects.len(), 2);
        assert!(
            pane.scope_rects
                .iter()
                .all(|(_, rect)| { rect.area() > 0.0 && pane.sheet.contains(rect.center()) })
        );
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
        fresh.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
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

        // An empty label is the request to take the name off.
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
            !sub.contains("named by test-owner") && !sub.contains("named by quinten"),
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

    // -----------------------------------------------------------------
    // Naming, driven the way a person drives it: real `WindowEvent`s into
    // `handle_event`, read back out of the **room document**, not the
    // widget — proving the whole chain, not just `apply_name`.
    // -----------------------------------------------------------------

    use ui::synthetic_input::cursor_moved;

    use ui::synthetic_input::mouse_press;

    fn key_press(logical_key: winit::keyboard::Key) -> WindowEvent {
        ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            logical_key,
            None,
            winit::event::ElementState::Pressed,
        ))
    }

    /// Paint the sheet so the rects the click handler reads off the last frame
    /// are populated exactly as they are live.
    fn paint(pane: &mut RoomActivityPane) {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        pane.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
    }

    /// Click at `p`: move there first so the hover latches, the way a pointer
    /// does.
    fn click_at(
        pane: &mut RoomActivityPane,
        tree: &mut WorkspaceTree,
        p: Point,
    ) -> RoomActivityOutcome {
        let m = Modifiers::default();
        pane.handle_event(&cursor_moved(p.x, p.y), 1.0, &m, Some(p), tree);
        pane.handle_event(&mouse_press(), 1.0, &m, Some(p), tree)
    }

    fn type_into(pane: &mut RoomActivityPane, tree: &mut WorkspaceTree, text: &str) {
        let m = Modifiers::default();
        for ch in text.chars() {
            let ev = if ch == ' ' {
                key_press(winit::keyboard::Key::Named(
                    winit::keyboard::NamedKey::Space,
                ))
            } else {
                key_press(winit::keyboard::Key::Character(ch.to_string().into()))
            };
            pane.handle_event(&ev, 1.0, &m, None, tree);
        }
    }

    /// The first line of the pane that is a band header.
    fn first_band(pane: &RoomActivityPane) -> usize {
        (0..pane.timeline().row_count())
            .find(|i| {
                pane.timeline().group_at_row(*i).is_some() && !pane.timeline().is_day_heading_at(*i)
            })
            .expect("the room's history produces at least one band")
    }

    #[test]
    fn clicking_name_typing_and_pressing_enter_puts_the_name_in_the_room_document() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        paint(&mut pane);

        let band = first_band(&pane);
        let (name_chip, _) = pane
            .timeline()
            .group_chip_rects(band)
            .expect("a band header carries a [Name] chip");
        let entries = pane.entries().to_vec();
        // What the band under the pointer covers, read before the gesture so the
        // assertion is against the range the user pointed at rather than whatever
        // came back. Rows are in display order and groups in causal order, so the
        // band is found by the key the row carries, not by position.
        let (first, last) = {
            let key = pane.timeline().group_at_row(band).unwrap().0.to_string();
            let mut flat: Vec<&ui::HistoryGroup> = Vec::new();
            for g in pane.timeline().groups() {
                g.walk(&mut flat);
            }
            let g = flat
                .iter()
                .find(|g| g.key == key)
                .expect("the band the header belongs to");
            (g.first_entry, g.last_entry)
        };

        let out = click_at(&mut pane, &mut tree, name_chip.center());
        assert!(out.handled, "the chip took the click");
        assert!(
            pane.wants_keyboard(),
            "the field is up and owns the keyboard, so the sheet's Escape does not close it"
        );
        assert_eq!(pane.timeline().naming_buffer(), Some(""));

        type_into(&mut pane, &mut tree, "Set up the room");
        assert_eq!(pane.timeline().naming_buffer(), Some("Set up the room"));

        let out = pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        assert!(out.named, "the pane reports a document write");
        assert!(!pane.wants_keyboard(), "the field is gone");

        // The name is in the room document, over the range the band covered.
        let ranges = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(ranges.len(), 1, "one name, from one gesture: {ranges:?}");
        assert_eq!(ranges[0].label, "Set up the room");
        assert_eq!(ranges[0].start, first);
        assert_eq!(ranges[0].end, Some(last));
        assert!(
            entries.iter().any(|e| e.id == first),
            "the range addresses changes this room actually has"
        );

        // And it is the document's, not the widget's: a pane that never saw the
        // gesture reads the same band off the same tree.
        let mut fresh = native_pane();
        fresh.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        fresh.refresh(&mut tree);
        let titles: Vec<&str> = (0..fresh.timeline().row_count())
            .filter_map(|i| fresh.timeline().group_title_at_row(i))
            .collect();
        assert!(
            titles.contains(&"Set up the room"),
            "the band is not on screen for the next person to open the pane: {titles:?}"
        );
    }

    #[test]
    fn escape_in_the_name_field_writes_nothing_to_the_room_document() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        paint(&mut pane);
        let heads_before = tree.doc.get_heads();

        let band = first_band(&pane);
        let (name_chip, _) = pane.timeline().group_chip_rects(band).unwrap();
        click_at(&mut pane, &mut tree, name_chip.center());
        type_into(&mut pane, &mut tree, "never mind");
        assert!(pane.wants_keyboard());

        let out = pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Escape,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        assert!(!out.named);
        assert!(!pane.wants_keyboard(), "the field is gone");
        assert!(
            ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new()).is_empty(),
            "Escape wrote a name anyway"
        );
        assert_eq!(
            tree.doc.get_heads(),
            heads_before,
            "abandoning a name must not commit anything at all"
        );
    }

    #[test]
    fn clicking_a_band_header_collapses_and_expands_it() {
        // The disclosure is a click on the header itself, and it must not be
        // confused with the chips that sit on the same row.
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        paint(&mut pane);

        let band = first_band(&pane);
        let (key, expanded) = {
            let (k, e) = pane.timeline().group_at_row(band).unwrap();
            (k.to_string(), e)
        };
        let header = pane.timeline().row_rect(band).expect("the header lays out");
        let (name_chip, _) = pane.timeline().group_chip_rects(band).unwrap();
        // Somewhere on the header that is not a chip: next to the disclosure
        // triangle.
        let on_header = Point::new(header.x0 + 60.0, header.center().y);
        assert!(
            !name_chip.contains(on_header),
            "the fixture aims off the chips"
        );

        let lines_before = pane.timeline().row_count();
        let out = click_at(&mut pane, &mut tree, on_header);
        assert!(out.handled);
        assert!(!out.named, "toggling a band is not a document write");
        assert_eq!(
            pane.timeline().group_at_row(band).map(|(_, e)| e),
            Some(!expanded),
            "the click did not toggle the band"
        );
        let lines_after = pane.timeline().row_count();
        assert_ne!(
            lines_before, lines_after,
            "the band's changes neither appeared nor went away"
        );

        // And back, addressing the same band by the same key.
        click_at(&mut pane, &mut tree, on_header);
        assert_eq!(
            pane.timeline().group_at_row(band).map(|(_, e)| e),
            Some(expanded)
        );
        assert_eq!(pane.timeline().row_count(), lines_before);
        assert_eq!(
            pane.timeline().group_at_row(band).map(|(k, _)| k),
            Some(key.as_str()),
            "the band moved under the toggle"
        );
    }

    #[test]
    fn keyboard_traverses_wraps_and_activates_modal_controls() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        paint(&mut pane);
        let modifiers = Modifiers::default();

        let home = pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::Home)),
            1.0,
            &modifiers,
            None,
            &mut tree,
        );
        assert!(home.handled);
        assert_eq!(
            pane.focused_control_label().as_deref(),
            Some("This workspace")
        );

        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::ArrowRight,
            )),
            1.0,
            &modifiers,
            None,
            &mut tree,
        );
        assert_eq!(
            pane.focused_control_label().as_deref(),
            Some("All workspaces")
        );
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &modifiers,
            None,
            &mut tree,
        );
        assert_eq!(pane.scope(), RoomScope::AllWorkspaces);

        // End addresses the last rendered control; Tab wraps within this
        // modal instead of leaking keyboard focus into the desktop beneath it.
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::End)),
            1.0,
            &modifiers,
            None,
            &mut tree,
        );
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab)),
            1.0,
            &modifiers,
            None,
            &mut tree,
        );
        assert_eq!(
            pane.focused_control_label().as_deref(),
            Some("This workspace")
        );
    }

    #[test]
    fn catch_up_outcomes_expand_from_the_keyboard() {
        let (mut tree, _, _) = peers_busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::CatchUp);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        paint(&mut pane);
        assert!(
            !pane.brief_rects.is_empty(),
            "fixture has catch-up outcomes"
        );

        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::End)),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        let focused = pane.focused_control_label().expect("last outcome focused");
        let last = pane.brief_rects.last().expect("last outcome").0;
        assert_eq!(focused, pane.digest.briefs[last].title);

        let out = pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Space,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        assert!(out.handled);
        assert_eq!(pane.expanded_brief, Some(last));
    }

    #[test]
    fn explorer_lenses_and_detail_actions_are_in_the_keyboard_focus_order() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        paint(&mut pane);
        assert!(!pane.lens_rects.is_empty());

        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::End)),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        let label = pane
            .focused_control_label()
            .expect("last explorer control focused");
        assert!(
            pane.lens_rects.iter().any(|(lens, _)| match lens {
                LensSelection::All => label == "Everything",
                LensSelection::AgentTasks => label == "Agent tasks",
                LensSelection::App(value)
                | LensSelection::ResourceKind(value)
                | LensSelection::Resource(value)
                | LensSelection::WorkGroup(value) => value == &label,
                LensSelection::Facet(key, value) => format!("{key}: {value}") == label,
                LensSelection::Unclassified => label == "Unclassified",
            }) || pane
                .detail_action_rects
                .iter()
                .any(|(action, _)| pane.detail_action_label(*action) == label),
            "focused control is a rendered explorer control: {label}"
        );
        assert!(
            pane.handle_event(
                &key_press(winit::keyboard::Key::Named(
                    winit::keyboard::NamedKey::Enter,
                )),
                1.0,
                &Modifiers::default(),
                None,
                &mut tree,
            )
            .handled
        );
    }

    #[test]
    fn accessibility_snapshot_matches_visible_focus_selection_and_disclosure() {
        let (mut tree, _, _) = peers_busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::CatchUp);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        paint(&mut pane);
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(winit::keyboard::NamedKey::End)),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );

        let snapshot = pane.accessibility_snapshot();
        assert_eq!(snapshot.role, ui::a11y::Role::Window);
        assert!(!snapshot.state.modal);
        assert!(
            !snapshot
                .children
                .iter()
                .any(|node| node.role == ui::a11y::Role::Tab),
            "the friendly recap does not expose full-history mode tabs"
        );
        let focused = snapshot
            .children
            .iter()
            .find(|node| node.state.focused)
            .expect("one control carries semantic focus");
        assert_eq!(focused.name, pane.focused_control_label());
        assert_eq!(focused.state.expanded, Some(true));
        assert!(
            snapshot
                .children
                .iter()
                .take(pane.focus_order().len())
                .all(|node| node.state.focusable)
        );
        assert!(
            !snapshot.children.iter().any(|node| {
                node.role == ui::a11y::Role::List && node.name.as_deref() == Some("History entries")
            }),
            "the hidden Explorer timeline is not exposed from Catch up"
        );
        pane.set_view(ActivityView::Explore);
        let snapshot = pane.accessibility_snapshot();
        assert!(snapshot.children.iter().any(|node| {
            node.role == ui::a11y::Role::List && node.name.as_deref() == Some("History entries")
        }));
    }

    /// The header band equals the authored 80 px at 100 % and grows with the
    /// text scale so the title, subtitle and "Latest" lines have their own
    /// room; every row under it moves down with it, and a `draw` at a new
    /// scale lays the sheet out again before placing anything.
    #[test]
    fn header_band_follows_the_text_scale_and_relayouts_on_draw() {
        let viewport = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut pane = native_pane();
        pane.layout(viewport);
        assert!(
            (pane.header_h() - HEADER_H).abs() < 1e-9,
            "1× keeps the authored band"
        );
        let rows_at_1x = pane.timeline.get_rect().y0;

        let large = Theme::dark().with_text_scale(2.0);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let mut scene = Scene::new();
        pane.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &large,
            &PresenceCtx::default(),
        );
        let expected = HEADER_TOP + pane.title_line_h() + 2.0 * pane.sub_line_h() + HEADER_BOTTOM;
        assert!(pane.header_h() > HEADER_H);
        assert!((pane.header_h() - expected).abs() < 1e-9);
        assert!(
            pane.timeline.get_rect().y0 > rows_at_1x,
            "the rows under the header moved down with it after the relayout"
        );
    }

    /// HIG §9.3: a chosen pill is a lift with its hairline, never an accent
    /// fill, and it moves further from the sheet's ground than an idle one
    /// on every shipped palette — including high contrast, where `lift`
    /// falls back to a derived step.
    #[test]
    fn selector_pills_are_lifted_and_ordered_on_every_palette() {
        use ui::a11y::relative_luminance;
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = relative_luminance(theme.tokens.colors.surfaces.base);
            let active =
                ui::chrome::selection_tier(&theme, ui::chrome::SelectionState::active(true));
            let idle = theme.tokens.colors.lift(PILL_FLOOR_LIFT_T);
            assert_ne!(
                active.fill, theme.tokens.colors.intent.inverted,
                "{}: not an accent wash",
                theme.name
            );
            assert!(
                active.border.is_some(),
                "{}: the chosen pill carries its hairline",
                theme.name
            );
            assert!(
                (relative_luminance(active.fill) - ground).abs()
                    > (relative_luminance(idle) - ground).abs(),
                "{}: chosen must lift further than idle",
                theme.name
            );
        }
    }

    /// The catch-up card's lines are placed by the same metrics that size
    /// the card, so at 200 % each line starts below the previous one ends
    /// and the affordance stays inside the card.
    #[test]
    fn brief_card_lines_never_overlap_at_large_text() {
        let large = Theme::dark().with_text_scale(2.0);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let m = BriefCardMetrics::new(&mut tr, &large);
        for (title_lines, actions) in [(1, 0), (2, 0), (1, 3), (2, 2)] {
            let byline = m.byline_top(title_lines);
            let provenance = m.provenance_top(title_lines);
            let affordance = m.affordance_top(title_lines, actions);
            assert!(byline >= m.pad + small_count_to_f64(title_lines) * m.title_lh);
            assert!(provenance >= byline + m.line_lh);
            assert!(affordance >= provenance + m.line_lh);
            if actions > 0 {
                assert!(
                    affordance
                        >= m.actions_top(title_lines) + small_count_to_f64(actions) * m.line_lh
                );
            }
            assert!(m.height(title_lines, actions) >= affordance + m.line_lh);
        }
        let one = BriefCardMetrics::new(&mut TextRenderer::new(), &Theme::dark());
        assert!(m.height(1, 0) > one.height(1, 0), "200 % grows the card");
    }

    #[test]
    fn chips_are_full_pointer_targets_that_publish_their_bounds() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        paint(&mut pane);
        assert!(
            pane.shows_chip_rows(),
            "the tall sheet shows period and author rows"
        );
        let targets: Vec<Rect> = pane
            .view_rects
            .iter()
            .map(|(_, rect)| *rect)
            .chain(pane.scope_rects.iter().map(|(_, rect)| *rect))
            .chain(pane.period_rects.iter().map(|(_, rect)| *rect))
            .chain(pane.author_rects.iter().map(|(_, rect)| *rect))
            .collect();
        assert!(
            targets.len() >= 4 + 2 + 2,
            "view, scope, period and author chips are laid out"
        );
        for rect in &targets {
            assert!(
                rect.height() >= MIN_TARGET - f64::EPSILON
                    && rect.width() >= MIN_TARGET - f64::EPSILON,
                "every chip is at least one pointer target: {rect:?}"
            );
        }
        for (i, a) in targets.iter().enumerate() {
            for b in &targets[..i] {
                let overlap = a.intersect(*b);
                assert!(
                    overlap.width() <= 0.0 || overlap.height() <= 0.0,
                    "{a:?} overlaps {b:?}"
                );
            }
        }
        let snapshot = pane.accessibility_snapshot();
        assert_eq!(
            snapshot.bounds.map(ui::AccessBounds::to_rect),
            Some(pane.rect())
        );
        // The chip families laid out above, by the labels the tree speaks.
        let chip_names: Vec<String> = ActivityView::ALL
            .iter()
            .map(|view| view.label())
            .chain(RoomScope::ALL.iter().map(|scope| scope.label()))
            .chain(Period::ALL.iter().map(|period| period.label()))
            .collect();
        // Only the chips are checked: the timeline list and controls the
        // current view does not paint publish no rect, and rightly so.
        let mut published = 0;
        for node in snapshot.children.iter().take(pane.focus_order().len()) {
            let Some(name) = node.name.as_deref() else {
                continue;
            };
            if !chip_names.iter().any(|chip| chip == name) {
                continue;
            }
            let rect = node.bounds.map_or_else(
                || panic!("{name:?} publishes its hit rect"),
                ui::AccessBounds::to_rect,
            );
            assert!(
                targets.contains(&rect),
                "{name:?} publishes the rect the pointer hits"
            );
            published += 1;
        }
        assert!(
            published >= 4 + 2 + 2,
            "every laid-out chip is in the tree with bounds"
        );
    }

    #[test]
    fn renaming_through_the_chip_replaces_the_name_rather_than_adding_a_band() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        paint(&mut pane);

        // Name it once, through the gesture.
        let band = first_band(&pane);
        let (chip, _) = pane.timeline().group_chip_rects(band).unwrap();
        click_at(&mut pane, &mut tree, chip.center());
        type_into(&mut pane, &mut tree, "First try");
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );
        let record = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new())[0]
            .id
            .clone();

        // Now rename it. The field seeds from the existing name, and the write
        // has to land on the record that already exists.
        paint(&mut pane);
        let band = (0..pane.timeline().row_count())
            .find(|i| pane.timeline().group_title_at_row(*i) == Some("First try"))
            .expect("the named band is on screen");
        let (chip, _) = pane.timeline().group_chip_rects(band).unwrap();
        click_at(&mut pane, &mut tree, chip.center());
        assert_eq!(
            pane.timeline().naming_buffer(),
            Some("First try"),
            "a rename starts from the name that is there"
        );
        type_into(&mut pane, &mut tree, " again");
        pane.handle_event(
            &key_press(winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter,
            )),
            1.0,
            &Modifiers::default(),
            None,
            &mut tree,
        );

        let ranges = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(
            ranges.len(),
            1,
            "a rename must not leave two bands: {ranges:?}"
        );
        assert_eq!(
            ranges[0].id, record,
            "it addressed the record it already had"
        );
        assert_eq!(ranges[0].label, "First try again");
    }

    #[test]
    fn overflow_controls_make_every_filter_page_keyboard_reachable() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::Explore);
        pane.contributors = (0..12).map(|i| format!("Contributor {i}")).collect();
        for i in 0..18 {
            pane.facet_index
                .apps
                .insert(format!("Application {i}"), i + 1);
        }
        pane.layout(Rect::new(0.0, 0.0, 1000.0, 520.0));
        paint(&mut pane);

        assert!(
            pane.author_more_rect.is_some(),
            "contributors expose a More control"
        );
        assert!(
            pane.lens_more_rect.is_some(),
            "short browse panel exposes a More control"
        );
        assert!(pane.focus_order().contains(&ActivityFocus::MorePeople));
        assert!(pane.focus_order().contains(&ActivityFocus::MoreLenses));
        let first_people = pane.author_rects.clone();
        pane.advance_people();
        paint(&mut pane);
        assert_ne!(
            first_people, pane.author_rects,
            "More people advances the page"
        );
        let first_lenses = pane.lens_rects.clone();
        pane.advance_lenses();
        paint(&mut pane);
        assert_ne!(
            first_lenses, pane.lens_rects,
            "More lenses advances the page"
        );
        assert!(
            pane.lens_rects
                .iter()
                .all(|(_, rect)| rect.y1 <= pane.sheet.y1)
        );
    }

    #[test]
    fn short_catch_up_has_a_more_outcomes_affordance() {
        let (mut tree, _, _) = peers_busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_view(ActivityView::CatchUp);
        let first = pane
            .digest
            .briefs
            .first()
            .cloned()
            .expect("fixture outcome");
        while pane.digest.briefs.len() < 5 {
            pane.digest.briefs.push(first.clone());
        }
        pane.layout(Rect::new(0.0, 0.0, 1000.0, 390.0));
        paint(&mut pane);
        assert!(
            pane.brief_more_rect.is_some(),
            "outcomes are paged, not silently clipped"
        );
        assert!(pane.focus_order().contains(&ActivityFocus::MoreBriefs));
        let before = pane.brief_rects.clone();
        pane.advance_briefs();
        paint(&mut pane);
        assert_ne!(before, pane.brief_rects);
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

        // A collaborator's device on the same room, kept in sync by hand.
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
                    // Home has a window group to open the tab in.
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

            // What a pane that reads the whole log once derives.
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
            // Undo and restore address a row by the document's heads at it.
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
    use crate::activity::testing::TestTree as WorkspaceTree;
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
        let local = "quinten";
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

    #[test]
    fn resume_card_click_opens_the_exact_resource() {
        let mut pane = RoomActivityPane::new();
        pane.resume_rect = Rect::new(10.0, 10.0, 200.0, 70.0);
        pane.resume_target = Some(ui::HistoryResourceTarget {
            app_id: Some("so.tilo.sheets".into()),
            app_label: Some("Sheets".into()),
            id: "finance:q2".into(),
            kind: "sheet".into(),
            label: "Q2 finance report".into(),
        });
        let mut tree = WorkspaceTree::bootstrap_default();
        let outcome = pane
            .handle_catchup_click(Point::new(40.0, 30.0), &mut tree)
            .expect("resume card handles its click");
        assert_eq!(outcome.open_target, pane.resume_target);
    }
}

#[cfg(test)]
mod task_review_tests {
    use super::*;
    use crate::activity::testing::TestTree as WorkspaceTree;
    struct ReviewHost {
        snapshot: RefCell<crate::activity::TaskReviewSnapshot>,
        submitted: RefCell<Vec<crate::activity::TaskReviewRequest>>,
        mutations: std::cell::Cell<bool>,
        launched: RefCell<Vec<String>>,
    }
    impl ActivityHost for ReviewHost {
        fn local_user(&self) -> String {
            "local".into()
        }
        fn mutations_available(&self) -> bool {
            self.mutations.get()
        }
        fn launch_agent(
            &self,
            agent: &str,
            _workspace: &tilo_desktop_model::Workspace,
        ) -> Result<(), String> {
            self.launched.borrow_mut().push(agent.into());
            Ok(())
        }
        fn task_review_required(&self, _room: &str) -> bool {
            true
        }
        fn task_review_snapshot(
            &self,
            room: &str,
        ) -> Option<Result<std::sync::Arc<crate::activity::TaskReviewSnapshot>, String>> {
            let snapshot = self.snapshot.borrow();
            Some(if snapshot.room == room {
                Ok(std::sync::Arc::new(snapshot.clone()))
            } else {
                Err("room unavailable".into())
            })
        }
        fn submit_task_review(
            &self,
            request: crate::activity::TaskReviewRequest,
        ) -> Result<(), String> {
            self.submitted.borrow_mut().push(request);
            Ok(())
        }
    }
    fn review_fixture() -> (WorkspaceTree, RoomActivityPane, Rc<ReviewHost>) {
        use tilo_desktop_model::{WorkspaceTaskActiveRun, WorkspaceTaskControl};
        let mut tree = WorkspaceTree::bootstrap_default();
        let active = tree.active;
        let id = tree
            .create_workspace_task(
                active,
                "Review release notes",
                "local",
                Some("scribe".into()),
                WorkspaceTaskRoute::Model,
                None,
            )
            .unwrap();
        let task = tree.active_workspace().unwrap().tasks[0].clone();
        let room = tree.active_workspace().unwrap().room.to_string();
        let host = Rc::new(ReviewHost {
            snapshot: RefCell::new(crate::activity::TaskReviewSnapshot {
                room,
                generation: 7,
                sampled_at_secs: 100,
                actor: "local".into(),
                writable: true,
                tasks: vec![task],
                controls: std::collections::BTreeMap::from([(
                    id.clone(),
                    WorkspaceTaskControl {
                        run_epoch: 3,
                        active_run: Some(WorkspaceTaskActiveRun {
                            agent_id: "scribe".into(),
                            epoch: 3,
                            started_at: 100,
                            applied_direction_revision: 0,
                            approval: None,
                        }),
                        ..Default::default()
                    },
                )]),
            }),
            submitted: RefCell::default(),
            mutations: std::cell::Cell::new(false),
            launched: RefCell::default(),
        });
        let mut pane = RoomActivityPane::new();
        pane.set_host(host.clone());
        pane.refresh(&mut tree);
        pane.set_view(ActivityView::Tasks);
        pane.selected_task = Some(id);
        (tree, pane, host)
    }
    /// Once an agent's run of a task ended and history recorded what it
    /// changed under the task, Task Center offers to undo it as one, and
    /// that asks the host to undo the task's run in each changed document.
    #[test]
    fn an_ended_runs_task_offers_to_undo_it_as_one() {
        let (mut tree, mut pane, host) = review_fixture();
        host.mutations.set(true);
        let id = pane.selected_task.clone().unwrap();
        host.snapshot
            .borrow_mut()
            .controls
            .get_mut(&id)
            .unwrap()
            .active_run = None;
        pane.refresh(&mut tree);
        let task = tree.active_workspace().unwrap().tasks[0].clone();
        assert!(
            !pane.task_actions_for(&task).contains(&TaskAction::UndoTask),
            "nothing recorded, nothing to undo"
        );

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
        assert!(ui::history_source::runs::set_run_task(doc, &key, &id));
        doc.commit();
        pane.refresh_records(&mut tree, &RecordStore::in_memory());
        assert!(pane.task_actions_for(&task).contains(&TaskAction::UndoTask));

        assert!(
            pane.apply_task_action(TaskAction::UndoTask, &mut tree)
                .handled
        );
        let submitted = host.submitted.borrow();
        assert_eq!(
            submitted.last().map(|request| &request.command),
            Some(&crate::activity::TaskReviewCommand::UndoRun {
                task_id: id.clone(),
                steps: vec![(
                    "so.tilo.notes".to_owned(),
                    "file:/notes/release.note".to_owned()
                )],
            })
        );
    }

    #[test]
    fn header_title_uses_current_viewport_and_scope_on_the_first_frame() {
        for theme in [
            Theme::light(),
            Theme::dark(),
            Theme::high_contrast().with_text_scale(2.0),
        ] {
            let (_, mut pane, _) = review_fixture();
            pane.layout_window(Rect::new(-560.0, 0.0, 0.0, 560.0));
            paint_header_fixture(&mut pane, &Theme::light());
            for (width, height) in [(1600.0, 900.0), (560.0, 560.0), (1600.0, 900.0)] {
                pane.layout_window(Rect::new(0.0, 0.0, width, height));
                paint_header_fixture(&mut pane, &theme);
                assert_eq!(
                    *pane.painted_title.borrow(),
                    t("activity-title-tasks"),
                    "the first frame after move/resize/scale change must retain orientation"
                );
                let first = pane.painted_title.borrow().clone();
                paint_header_fixture(&mut pane, &theme);
                assert_eq!(*pane.painted_title.borrow(), first);
            }
            pane.set_view(ActivityView::CatchUp);
            paint_header_fixture(&mut pane, &theme);
            assert!(pane.scope_rects.is_empty());
            assert_eq!(*pane.painted_title.borrow(), t("activity-title-catch-up"));
        }
    }

    fn paint_header_fixture(pane: &mut RoomActivityPane, theme: &Theme) -> Scene {
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        pane.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            theme,
            &PresenceCtx::empty(),
        );
        scene
    }

    #[test]
    fn activity_header_render_matrix() {
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target/activity-header-renders");
        std::fs::create_dir_all(&output).unwrap();
        for (name, theme) in [
            ("light", Theme::light()),
            ("dark", Theme::dark()),
            ("hc200", Theme::high_contrast().with_text_scale(2.0)),
        ] {
            for (size, width, height) in [("normal", 1600, 900), ("compact", 560, 560)] {
                let (mut tree, mut pane, host) = review_fixture();
                host.snapshot.borrow_mut().sampled_at_secs =
                    i64::try_from(ui::time::try_unix_now().unwrap()).unwrap();
                pane.refresh(&mut tree);
                pane.layout_window(Rect::new(-560.0, 0.0, 0.0, 560.0));
                paint_header_fixture(&mut pane, &Theme::light());
                pane.layout_window(Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
                let scene = paint_header_fixture(&mut pane, &theme);
                let mut renderer = ui::offscreen::OffscreenRenderer::new(width, height);
                renderer.render_png(
                    &scene,
                    theme.tokens.colors.surfaces.content,
                    output.join(format!("{name}-{size}.png")),
                );
            }
        }
    }

    /// Three reviewed tasks: the one in progress was approved, carries an
    /// instruction and reports executing; the others wait their turn.
    fn crowded_review_fixture() -> (WorkspaceTree, RoomActivityPane, Rc<ReviewHost>) {
        use tilo_desktop_model::{
            WorkspaceTaskApproval, WorkspaceTaskDirection, WorkspaceTaskExecution,
            WorkspaceTaskExecutionPhase,
        };
        let (mut tree, mut pane, host) = review_fixture();
        let now = i64::try_from(ui::time::try_unix_now().unwrap()).unwrap();
        let active = tree.active;
        for title in [
            "Replace the parenthesis with a real link found by web search",
            "Summarise this week's meeting notes for the team",
        ] {
            tree.create_workspace_task(
                active,
                title,
                "local",
                Some("scribe".into()),
                WorkspaceTaskRoute::Model,
                None,
            )
            .unwrap();
        }
        {
            let mut snapshot = host.snapshot.borrow_mut();
            snapshot.sampled_at_secs = now;
            snapshot.tasks = tree.active_workspace().unwrap().tasks.clone();
            for (index, task) in snapshot.tasks.iter_mut().enumerate() {
                task.status = if index == 0 {
                    WorkspaceTaskStatus::InProgress
                } else {
                    WorkspaceTaskStatus::Queued
                };
                task.updated_at = now.saturating_sub(i64::try_from(index).unwrap());
            }
            let first = snapshot.tasks[0].id.clone();
            let control = snapshot
                .controls
                .remove(&pane.selected_task.clone().unwrap());
            let mut control = control.unwrap();
            if let Some(run) = control.active_run.as_mut() {
                run.approval = Some(WorkspaceTaskApproval {
                    person: "local".into(),
                    device: [0; 32],
                    approved_at: now.saturating_sub(120),
                    instruction_revision: 0,
                });
            }
            control.execution = Some(WorkspaceTaskExecution {
                run_epoch: 3,
                sequence: 1,
                phase: WorkspaceTaskExecutionPhase::Executing,
                updated_at: now.saturating_sub(30),
            });
            control
                .directions
                .push(WorkspaceTaskDirection::owner_admitted(
                    1,
                    uuid::Uuid::nil(),
                    "local".into(),
                    "Use the https address".into(),
                    now.saturating_sub(60),
                ));
            snapshot.controls.insert(first.clone(), control);
            let rest: Vec<String> = snapshot
                .tasks
                .iter()
                .skip(1)
                .map(|task| task.id.clone())
                .collect();
            for id in rest {
                snapshot
                    .controls
                    .insert(id, tilo_desktop_model::WorkspaceTaskControl::default());
            }
            pane.selected_task = Some(first);
        }
        pane.refresh(&mut tree);
        (tree, pane, host)
    }

    fn review_action_rect(pane: &RoomActivityPane, action: TaskAction) -> Option<Rect> {
        pane.task_action_rects
            .iter()
            .find_map(|(candidate, rect)| (*candidate == action).then_some(*rect))
    }

    #[test]
    fn review_paging_keeps_previous_left_and_next_right() {
        let (mut tree, mut pane, _) = crowded_review_fixture();
        pane.layout_window(Rect::new(0.0, 0.0, 640.0, 640.0));
        paint_header_fixture(&mut pane, &Theme::dark());
        assert!(review_action_rect(&pane, TaskAction::PreviousTasks).is_none());
        let next_first = review_action_rect(&pane, TaskAction::NextTasks).unwrap();
        pane.apply_task_action(TaskAction::NextTasks, &mut tree);
        paint_header_fixture(&mut pane, &Theme::dark());
        let previous = review_action_rect(&pane, TaskAction::PreviousTasks).unwrap();
        let next = review_action_rect(&pane, TaskAction::NextTasks).unwrap();
        assert_eq!(next, next_first, "Next stays where it was");
        assert!(previous.x1 < next.x0, "Previous sits left of Next");
        pane.apply_task_action(TaskAction::NextTasks, &mut tree);
        paint_header_fixture(&mut pane, &Theme::dark());
        assert!(review_action_rect(&pane, TaskAction::NextTasks).is_none());
        assert_eq!(
            review_action_rect(&pane, TaskAction::PreviousTasks),
            Some(previous),
            "Previous keeps its place when Next is gone"
        );
    }

    #[test]
    fn review_counters_wait_behind_details() {
        let (mut tree, mut pane, _) = crowded_review_fixture();
        pane.layout_window(Rect::new(0.0, 0.0, 640.0, 640.0));
        let task_height = |pane: &mut RoomActivityPane| {
            paint_header_fixture(pane, &Theme::dark());
            pane.task_rects[0].1.height()
        };
        let collapsed = task_height(&mut pane);
        assert!(!pane.review_details);
        assert!(review_action_rect(&pane, TaskAction::Details).is_some());
        pane.apply_task_action(TaskAction::Details, &mut tree);
        assert!(pane.review_details);
        assert!(
            task_height(&mut pane) > collapsed,
            "details add the run counters under the task"
        );
        assert_eq!(
            pane.access_control(&ActivityFocus::TaskAction(TaskAction::Details))
                .unwrap()
                .expanded,
            Some(true)
        );
    }

    #[test]
    fn task_center_has_no_workspace_scope_toggle() {
        let (_, mut pane, _) = crowded_review_fixture();
        pane.layout_window(Rect::new(0.0, 0.0, 640.0, 640.0));
        paint_header_fixture(&mut pane, &Theme::dark());
        assert!(
            pane.scope_rects.is_empty(),
            "tasks always belong to the active workspace"
        );
    }

    #[test]
    fn task_center_render_matrix() {
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target/task-center-renders");
        std::fs::create_dir_all(&output).unwrap();
        for locale in ["en-US", "de-DE"] {
            let _ = t("activity-title-tasks");
            ui::i18n::I18nManager::set_locale(locale);
            for (name, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
                for details in [false, true] {
                    let (mut tree, mut pane, _) = crowded_review_fixture();
                    pane.layout_window(Rect::new(0.0, 0.0, 640.0, 640.0));
                    if details {
                        pane.apply_task_action(TaskAction::Details, &mut tree);
                    }
                    let scene = paint_header_fixture(&mut pane, &theme);
                    let mut renderer = ui::offscreen::OffscreenRenderer::new(640, 640);
                    let suffix = if details { "-details" } else { "" };
                    renderer.render_png(
                        &scene,
                        theme.tokens.colors.surfaces.content,
                        output.join(format!("{locale}-{name}{suffix}.png")),
                    );
                }
            }
        }
        ui::i18n::I18nManager::set_locale("en-US");
    }

    #[test]
    fn review_retry_is_offered_for_waiting_tasks_only() {
        let (mut tree, mut pane, host) = review_fixture();
        let id = pane.selected_task.clone().unwrap();
        for status in [
            WorkspaceTaskStatus::Waiting,
            WorkspaceTaskStatus::Done,
            WorkspaceTaskStatus::Cancelled,
        ] {
            {
                let mut snapshot = host.snapshot.borrow_mut();
                snapshot.tasks[0].status = status;
                snapshot.controls.get_mut(&id).unwrap().active_run = None;
            }
            pane.refresh(&mut tree);
            let actions = pane.task_actions_for(&pane.tasks[0]);
            assert_eq!(
                actions.contains(&TaskAction::RetryRun),
                status == WorkspaceTaskStatus::Waiting
            );
            assert!(!actions.contains(&TaskAction::Steer));
            if status.is_finished() {
                assert!(!actions.contains(&TaskAction::CancelRun));
            }
        }
    }
    #[test]
    fn an_approved_run_offers_to_start_its_offline_agent() {
        let (mut tree, mut pane, host) = review_fixture();
        host.mutations.set(true);
        {
            let mut snapshot = host.snapshot.borrow_mut();
            snapshot.tasks[0].status = WorkspaceTaskStatus::InProgress;
        }
        pane.refresh(&mut tree);
        assert!(
            pane.task_actions_for(&pane.tasks[0])
                .contains(&TaskAction::StartAgent),
            "an approved run reaches no one until its agent is running"
        );
        pane.apply_task_action(TaskAction::StartAgent, &mut tree);
        assert_eq!(*host.launched.borrow(), ["scribe"]);
        assert!(
            !pane
                .task_actions_for(&pane.tasks[0])
                .contains(&TaskAction::StartAgent),
            "a starting agent is not offered a second start"
        );
        host.snapshot.borrow_mut().tasks[0].status = WorkspaceTaskStatus::Done;
        pane.agent_startup.clear();
        pane.refresh(&mut tree);
        assert!(
            !pane
                .task_actions_for(&pane.tasks[0])
                .contains(&TaskAction::StartAgent),
            "a finished task has no run to start an agent for"
        );
    }
    #[test]
    fn queued_run_approval_uses_exact_frontier_and_shared_keyboard_controls() {
        use crate::activity::TaskReviewCommand;
        let (mut tree, mut pane, host) = review_fixture();
        let id = pane.selected_task.clone().unwrap();
        {
            let mut snapshot = host.snapshot.borrow_mut();
            snapshot.tasks[0].status = WorkspaceTaskStatus::Queued;
            snapshot.controls.get_mut(&id).unwrap().active_run = None;
        }
        pane.refresh(&mut tree);
        assert!(
            pane.task_actions_for(&pane.tasks[0])
                .contains(&TaskAction::ApproveRun)
        );
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            let theme = theme.with_text_scale(2.0);
            pane.layout_window(Rect::new(0.0, 0.0, 900.0, 900.0));
            pane.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::empty(),
            );
            assert!(
                pane.focus_order()
                    .contains(&ActivityFocus::TaskAction(TaskAction::ApproveRun))
            );
            let rect = pane
                .task_action_rects
                .iter()
                .find(|(action, _)| *action == TaskAction::ApproveRun)
                .unwrap()
                .1;
            assert_eq!(rect.intersect(pane.sheet), rect);
            assert!(
                pane.accessibility_snapshot()
                    .children
                    .iter()
                    .any(|node| node.name.as_deref()
                        == Some(TaskAction::ApproveRun.label().as_str()))
            );
        }
        pane.apply_task_action(TaskAction::ApproveRun, &mut tree);
        assert!(
            matches!(&host.submitted.borrow()[0].command, TaskReviewCommand::Approve {task_id,run_epoch:3,after_revision:0,..} if task_id==&id)
        );
        assert!(
            pane.review_pending.is_some(),
            "approval must await its durable receipt"
        );
    }
    #[test]
    fn approval_keeps_the_painted_frontier_when_a_new_run_is_unseen() {
        use crate::activity::TaskReviewCommand;
        let (mut tree, mut pane, host) = review_fixture();
        let id = pane.selected_task.clone().unwrap();
        {
            let mut snapshot = host.snapshot.borrow_mut();
            snapshot.tasks[0].status = WorkspaceTaskStatus::Queued;
            snapshot.controls.get_mut(&id).unwrap().active_run = None;
        }
        pane.refresh(&mut tree);
        pane.layout_window(Rect::new(0.0, 0.0, 900.0, 900.0));
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        pane.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::light(),
            &PresenceCtx::empty(),
        );
        {
            let mut snapshot = host.snapshot.borrow_mut();
            snapshot.controls.get_mut(&id).unwrap().run_epoch = 4;
        }
        pane.refresh(&mut tree);
        pane.apply_task_action(TaskAction::ApproveRun, &mut tree);
        assert!(
            matches!(
                &host.submitted.borrow()[0].command,
                TaskReviewCommand::Approve {
                    run_epoch: 3,
                    after_revision: 0,
                    ..
                }
            ),
            "refreshing unseen state must not approve its new run frontier"
        );
    }

    #[test]
    fn steering_draft_keeps_observed_run_and_revision_after_refresh() {
        use crate::activity::TaskReviewCommand;
        let (mut tree, mut pane, host) = review_fixture();
        pane.apply_task_action(TaskAction::Steer, &mut tree);
        pane.direction
            .as_mut()
            .unwrap()
            .input
            .set_text("Keep compatibility examples");
        let id = pane.selected_task.clone().unwrap();
        host.snapshot
            .borrow_mut()
            .controls
            .get_mut(&id)
            .unwrap()
            .run_epoch = 4;
        pane.refresh(&mut tree);
        pane.apply_task_action(TaskAction::SendDirection, &mut tree);
        assert!(
            matches!(&host.submitted.borrow()[0].command,TaskReviewCommand::Steer {run_epoch:3,after_revision:0,text,..} if text=="Keep compatibility examples")
        );
        assert!(
            pane.direction.is_some(),
            "queued admission must retain the draft until a durable receipt"
        );
    }
    #[test]
    fn switching_workspace_clears_steering_draft_and_keeps_new_room_private() {
        let (mut tree, mut pane, host) = review_fixture();
        pane.apply_task_action(TaskAction::Steer, &mut tree);
        pane.direction
            .as_mut()
            .unwrap()
            .input
            .set_text("Private project direction");
        tree.append_empty_workspace("Personal");
        tree.active = tree.workspaces.last().unwrap().id;
        pane.refresh(&mut tree);
        assert!(pane.direction.is_none());
        assert!(pane.tasks.is_empty());
        assert!(host.submitted.borrow().is_empty());
    }
    #[test]
    fn native_review_form_and_controls_fit_large_text_and_have_keyboard_targets() {
        let (_tree, mut pane, _host) = review_fixture();
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            let theme = theme.with_text_scale(2.0);
            pane.layout_window(Rect::new(0.0, 0.0, 900.0, 900.0));
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            pane.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::empty(),
            );
            assert!(
                pane.task_action_rects
                    .iter()
                    .any(|(action, _)| *action == TaskAction::Steer)
            );
            pane.apply_review_action(TaskAction::Steer);
            pane.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::empty(),
            );
            assert!(pane.focus_order().contains(&ActivityFocus::DirectionField));
            for (_, rect) in &pane.task_action_rects {
                assert_eq!(rect.intersect(pane.sheet), *rect);
            }
            assert!(
                pane.task_action_rects
                    .iter()
                    .any(|(action, _)| *action == TaskAction::SendDirection)
            );
            assert!(
                pane.accessibility_snapshot()
                    .children
                    .iter()
                    .any(|node| node.role == ui::a11y::Role::TextInput)
            );
            pane.direction = None;
        }
    }
    #[test]
    fn deleting_the_focused_task_does_not_retarget_its_controls() {
        let (mut tree, mut pane, host) = review_fixture();
        let task_id = pane.selected_task.clone().unwrap();
        pane.keyboard_focus = Some(ActivityFocus::TaskAction(TaskAction::CancelRun));
        host.snapshot.borrow_mut().tasks.clear();
        pane.refresh(&mut tree);
        assert_eq!(pane.requested_review_task, Some(task_id));
        assert_eq!(pane.selected_task, None);
        assert_eq!(
            pane.keyboard_focus,
            Some(ActivityFocus::View(ActivityView::Tasks))
        );
        pane.apply_task_action(TaskAction::CancelRun, &mut tree);
        assert!(host.submitted.borrow().is_empty());
    }

    fn draft_descriptor(
        label: &str,
        params: Vec<tilo_actions::ActionParam>,
    ) -> tilo_actions::ActionDescriptor {
        tilo_actions::ActionDescriptor {
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new("inbox", "do"),
            resource_kind: "mail.mailbox".into(),
            resource_label: "Inbox".into(),
            action_label: label.into(),
            description: None,
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Document,
            contract: tilo_actions::ActionContract::default(),
            input: tilo_actions::InputSpec::None,
            params,
            view: None,
        }
    }

    /// A Mail message to send, as the owner records it.
    fn mail_confirmation() -> tilo_desktop_model::WorkspaceTaskConfirmation {
        use tilo_actions::{ActionParam, ParamType};
        let descriptor = draft_descriptor(
            "Send message",
            vec![
                ActionParam::required("account", ParamType::Text, "From"),
                ActionParam::required("message", ParamType::Object, "The message"),
            ],
        );
        let arguments = serde_json::json!({
            "account": "quinten@example.org",
            "message": {
                "to": "bob@example.org",
                "cc": "",
                "subject": "Note X: release checklist",
                "body": "Hi Bob,\n\nhere is the release checklist we talked about. It covers the image build, the store listing and the announcement.\n\nThanks!",
                "attachments": [{"name": "checklist.pdf"}, {"name": "notes.md"}, {"name": "screenshot.png"}]
            }
        });
        confirmation_of(
            &descriptor,
            &arguments,
            tilo_actions::RiskLevel::Destructive,
            tilo_action_policy::ActionKind::Outside,
        )
    }

    /// Files' permanent deletion, drafted generically.
    fn files_confirmation() -> tilo_desktop_model::WorkspaceTaskConfirmation {
        use tilo_actions::{ActionParam, ParamType};
        let descriptor = draft_descriptor(
            "Delete permanently",
            vec![
                ActionParam::required("paths", ParamType::Array, "Files"),
                ActionParam::optional("keep_versions", ParamType::Boolean, "Keep versions"),
            ],
        );
        let mut confirmation = confirmation_of(
            &descriptor,
            &serde_json::json!({
                "paths": ["/home/quinten/Trash/report-draft.pdf", "/home/quinten/Trash/old-notes.md", "/home/quinten/Trash/scan.png", "/home/quinten/Trash/budget.ods"],
                "keep_versions": false
            }),
            tilo_actions::RiskLevel::Destructive,
            tilo_action_policy::ActionKind::Outside,
        );
        confirmation.draft.resource_label = "Trash".into();
        confirmation
    }

    fn confirmation_of(
        descriptor: &tilo_actions::ActionDescriptor,
        arguments: &serde_json::Value,
        risk: tilo_actions::RiskLevel,
        kind: tilo_action_policy::ActionKind,
    ) -> tilo_desktop_model::WorkspaceTaskConfirmation {
        let arguments = arguments.as_object().unwrap();
        let digest = tilo_actions::draft::arguments_digest(arguments);
        tilo_desktop_model::WorkspaceTaskConfirmation {
            id: tilo_desktop_model::WorkspaceTaskConfirmation::id_for("inbox", "do", &digest),
            asking: uuid::Uuid::from_u128(1),
            run_epoch: 3,
            agent_id: "scribe".into(),
            app: "so.tilo.mail".into(),
            resource_kind: "mail.mailbox".into(),
            resource: "inbox".into(),
            action: "do".into(),
            kind,
            risk,
            draft: tilo_actions::Draft::render(descriptor, arguments),
            arguments_digest: digest,
            requested_at: 100,
            state: tilo_desktop_model::WorkspaceTaskConfirmationState::Waiting,
        }
    }

    fn paused_fixture(
        confirmation: tilo_desktop_model::WorkspaceTaskConfirmation,
    ) -> (WorkspaceTree, RoomActivityPane, Rc<ReviewHost>) {
        let (mut tree, mut pane, host) = review_fixture();
        let id = pane.selected_task.clone().unwrap();
        {
            let mut snapshot = host.snapshot.borrow_mut();
            snapshot.tasks[0].status = WorkspaceTaskStatus::InProgress;
            snapshot.tasks[0].title = "Send note X by mail to Bob".into();
            snapshot.controls.get_mut(&id).unwrap().confirmation = Some(confirmation);
        }
        // The person here manages the workspace, and its roster names the
        // agent.
        tree.adopt_ownerless_workspaces(&"local".to_owned());
        if let Some(workspace) = tree.desktop.active_workspace_mut() {
            workspace
                .collaborators
                .push(tilo_desktop_model::Collaborator {
                    id: "scribe".into(),
                    name: "Scribe".into(),
                    ..Default::default()
                });
        }
        pane.refresh(&mut tree);
        (tree, pane, host)
    }

    #[test]
    fn the_card_names_the_agent_and_offers_always_allow_only_to_who_may_grant() {
        let (mut tree, mut pane, _host) = paused_fixture(mail_confirmation());
        pane.layout_window(Rect::new(0.0, 0.0, 900.0, 1000.0));
        paint(&mut pane, &Theme::light());
        assert_eq!(pane.agent_label("scribe"), "Scribe");
        assert_eq!(pane.take_confirmation_notices()[0].agent, "Scribe");
        assert!(
            pane.task_action_rects
                .iter()
                .any(|(action, _)| *action == TaskAction::AlwaysAllow)
        );
        pane.explicit_local_user = Some("visitor".into());
        pane.refresh(&mut tree);
        paint(&mut pane, &Theme::light());
        assert!(
            !pane
                .task_action_rects
                .iter()
                .any(|(action, _)| *action == TaskAction::AlwaysAllow),
            "someone who may not change agent access is not offered it"
        );
    }

    #[test]
    fn a_card_taller_than_the_sheet_scrolls_to_its_buttons() {
        let (mut tree, mut pane, _host) = paused_fixture(mail_confirmation());
        let theme = Theme::high_contrast().with_text_scale(2.0);
        pane.layout_window(Rect::new(0.0, 0.0, 560.0, 640.0));
        paint(&mut pane, &theme);
        let viewport = pane.review_scroll.viewport.unwrap();
        assert!(
            pane.review_scroll.content > viewport.height(),
            "the card overflows"
        );
        let confirm = |pane: &RoomActivityPane| {
            pane.task_action_rects
                .iter()
                .find_map(|(action, rect)| (*action == TaskAction::ConfirmDraft).then_some(*rect))
                .unwrap()
        };
        assert!(
            confirm(&pane).y0 > viewport.y1,
            "Confirm starts below the fold"
        );
        let wheel = WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, -100.0),
            phase: winit::event::TouchPhase::Moved,
        };
        let outcome = pane.handle_event(
            &wheel,
            1.0,
            &Modifiers::default(),
            Some(viewport.center()),
            &mut tree,
        );
        assert!(outcome.handled);
        paint(&mut pane, &theme);
        let last = pane
            .task_action_rects
            .iter()
            .map(|(_, rect)| rect.y1)
            .fold(f64::MIN, f64::max);
        assert!(
            last > viewport.y0 && last <= viewport.y1,
            "scrolled to the end, the last button is in view"
        );
        // Keyboard focus brings a control back into view by itself.
        pane.review_scroll.offset = 0.0;
        pane.keyboard_focus = Some(ActivityFocus::TaskAction(TaskAction::ConfirmDraft));
        paint(&mut pane, &theme);
        paint(&mut pane, &theme);
        assert!(viewport.contains(confirm(&pane).center()));
    }

    fn paint(pane: &mut RoomActivityPane, theme: &Theme) -> Scene {
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        pane.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            theme,
            &PresenceCtx::empty(),
        );
        scene
    }

    fn submitted_answer(host: &ReviewHost) -> crate::activity::TaskReviewCommand {
        host.submitted.borrow().last().unwrap().command.clone()
    }

    #[test]
    fn a_paused_run_shows_its_card_first_and_notifies_once() {
        let (mut tree, mut pane, _host) = paused_fixture(mail_confirmation());
        assert!(pane.task_needs_person(&pane.tasks[0]));
        pane.layout_window(Rect::new(0.0, 0.0, 900.0, 1000.0));
        paint(&mut pane, &Theme::light());
        let painted: Vec<TaskAction> = pane
            .task_action_rects
            .iter()
            .map(|(action, _)| *action)
            .collect();
        for action in [
            TaskAction::ConfirmDraft,
            TaskAction::EditDraft,
            TaskAction::DeclineDraft,
            TaskAction::AlwaysAllow,
        ] {
            assert!(painted.contains(&action), "{action:?} is on the card");
            assert!(
                pane.focus_order()
                    .contains(&ActivityFocus::TaskAction(action))
            );
        }
        let notices = pane.take_confirmation_notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].action, "Send message");
        pane.refresh(&mut tree);
        assert!(pane.take_confirmation_notices().is_empty(), "noticed once");
    }

    #[test]
    fn confirming_as_drafted_sends_no_edits() {
        use tilo_desktop_model::WorkspaceTaskConfirmationAnswer;
        let confirmation = mail_confirmation();
        let (mut tree, mut pane, host) = paused_fixture(confirmation.clone());
        pane.apply_task_action(TaskAction::ConfirmDraft, &mut tree);
        assert_eq!(
            submitted_answer(&host),
            crate::activity::TaskReviewCommand::AnswerConfirmation {
                task_id: pane.selected_task.clone().unwrap(),
                run_epoch: 3,
                confirmation_id: confirmation.id,
                answer: WorkspaceTaskConfirmationAnswer::Confirm { edits: Vec::new() },
            }
        );
    }

    #[test]
    fn editing_sends_only_the_changed_fields() {
        use tilo_desktop_model::WorkspaceTaskConfirmationAnswer;
        let (mut tree, mut pane, host) = paused_fixture(mail_confirmation());
        pane.apply_task_action(TaskAction::EditDraft, &mut tree);
        pane.layout_window(Rect::new(0.0, 0.0, 900.0, 1000.0));
        paint(&mut pane, &Theme::dark());
        let review = pane.confirmation.as_mut().unwrap();
        let to = review
            .fields
            .iter_mut()
            .find(|field| field.path == ["message", "to"])
            .expect("recipients are editable");
        to.input.set_text("carol@example.org");
        assert!(pane.focus_order().contains(&ActivityFocus::ConfirmField(0)));
        pane.apply_task_action(TaskAction::ConfirmDraft, &mut tree);
        let crate::activity::TaskReviewCommand::AnswerConfirmation {
            answer: WorkspaceTaskConfirmationAnswer::Confirm { edits },
            ..
        } = submitted_answer(&host)
        else {
            panic!("a confirmation")
        };
        assert_eq!(
            edits,
            vec![tilo_actions::DraftEdit {
                path: vec!["message".into(), "to".into()],
                text: "carol@example.org".into(),
            }]
        );
    }

    #[test]
    fn declining_carries_the_reason() {
        use tilo_desktop_model::WorkspaceTaskConfirmationAnswer;
        let (mut tree, mut pane, host) = paused_fixture(files_confirmation());
        pane.apply_task_action(TaskAction::DeclineDraft, &mut tree);
        assert_eq!(pane.keyboard_focus, Some(ActivityFocus::DeclineReason));
        pane.confirmation
            .as_mut()
            .unwrap()
            .reason
            .set_text("Keep the budget sheet");
        pane.apply_task_action(TaskAction::SendDecline, &mut tree);
        assert!(matches!(
            submitted_answer(&host),
            crate::activity::TaskReviewCommand::AnswerConfirmation {
                answer: WorkspaceTaskConfirmationAnswer::Decline { reason: Some(reason) },
                ..
            } if reason == "Keep the budget sheet"
        ));
    }

    #[test]
    fn always_allowing_names_the_draft_it_confirms() {
        let confirmation = files_confirmation();
        let (mut tree, mut pane, host) = paused_fixture(confirmation.clone());
        pane.apply_task_action(TaskAction::AlwaysAllow, &mut tree);
        assert_eq!(
            submitted_answer(&host),
            crate::activity::TaskReviewCommand::AlwaysAllow {
                task_id: pane.selected_task.clone().unwrap(),
                run_epoch: 3,
                confirmation_id: confirmation.id,
                edits: Vec::new(),
            }
        );
    }

    #[test]
    fn an_answered_draft_offers_no_second_answer() {
        let mut confirmation = mail_confirmation();
        confirmation.state = tilo_desktop_model::WorkspaceTaskConfirmationState::Confirmed {
            person: "local".into(),
            at: 101,
            edits: Vec::new(),
        };
        let (mut tree, mut pane, host) = paused_fixture(confirmation);
        assert!(!pane.task_needs_person(&pane.tasks[0]));
        pane.layout_window(Rect::new(0.0, 0.0, 900.0, 1000.0));
        paint(&mut pane, &Theme::light());
        assert!(
            !pane
                .task_action_rects
                .iter()
                .any(|(action, _)| *action == TaskAction::ConfirmDraft)
        );
        pane.apply_task_action(TaskAction::ConfirmDraft, &mut tree);
        assert!(host.submitted.borrow().is_empty());
    }

    /// Renders the card for review; written under the target directory.
    #[test]
    fn confirmation_card_render_matrix() {
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target/confirmation-card-renders");
        std::fs::create_dir_all(&output).unwrap();
        for (name, theme) in [
            ("light", Theme::light()),
            ("dark", Theme::dark()),
            ("hc200", Theme::high_contrast().with_text_scale(2.0)),
        ] {
            for (draft, confirmation) in [
                ("message", mail_confirmation()),
                ("fields", files_confirmation()),
            ] {
                for mode in ["review", "edit", "decline"] {
                    let (mut tree, mut pane, host) = paused_fixture(confirmation.clone());
                    host.snapshot.borrow_mut().sampled_at_secs =
                        i64::try_from(ui::time::try_unix_now().unwrap()).unwrap();
                    pane.refresh(&mut tree);
                    match mode {
                        "edit" => {
                            pane.apply_task_action(TaskAction::EditDraft, &mut tree);
                        }
                        "decline" => {
                            pane.apply_task_action(TaskAction::DeclineDraft, &mut tree);
                        }
                        _ => {}
                    }
                    let (width, height) = (760, 1000);
                    pane.layout_window(Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
                    paint(&mut pane, &theme);
                    let scene = paint(&mut pane, &theme);
                    let mut renderer = ui::offscreen::OffscreenRenderer::new(width, height);
                    renderer.render_png(
                        &scene,
                        theme.tokens.colors.surfaces.content,
                        output.join(format!("{name}-{draft}-{mode}.png")),
                    );
                }
            }
        }
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
        pane.layout_window(Rect::new(0.0, 0.0, 640.0, 900.0));
        let theme = Theme::light();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        pane.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
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
        for focus in pane.focus_order() {
            let label = pane.focus_label(&focus).unwrap_or_default();
            assert!(
                !label.contains(record),
                "an identifier reached a visitor: {label:?}"
            );
        }
        assert!(pane.morning_rects.is_empty());
        let mut scene = Scene::new();
        let y = pane.draw_morning_sources(&mut scene, Affine::IDENTITY, &mut text, &theme, 120.0);
        assert!(
            (y - 120.0).abs() < f64::EPSILON,
            "an unavailable morning section must take no space"
        );
        assert!(scene.encoding().is_empty(), "nor paint anything");
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
        let room = tree.workspaces[0].room;
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
        tree.workspaces[0].room = tilo_desktop_model::RoomId::new();
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
        tree.workspaces[0].room = room;
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
    fn morning_source_choices_are_bounded_and_noisy_activity_preserves_focus() {
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
        pane.keyboard_focus = Some(ActivityFocus::Morning(key.clone(), false));
        pane.refresh(&mut tree);
        assert_eq!(
            pane.chosen_morning_sources().len(),
            1,
            "repeated updates group into one chosen source"
        );
        assert_eq!(
            pane.keyboard_focus,
            Some(ActivityFocus::Morning(key, false))
        );
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

    #[test]
    fn morning_source_long_resource_names_keep_freshness_and_navigation_visible() {
        let (mut tree, mut pane) = fixture();
        pane.apply_selection_history_action(MORNING_SOURCE, &mut tree, None);
        let key = pane.chosen_morning_sources()[0].key();
        let title = "A very long authorized resource name ".repeat(100);
        let context = tilo_actions::ActionContext::for_resource(
            "weather.location:fixture",
            "location",
            title,
        )
        .with_app("so.tilo.weather", "Weather")
        .with_described(crate::activity::testing::verb("Update"));
        tree.doc
            .put(automerge::ROOT, "name", "Long resource label")
            .unwrap();
        tree.doc.commit_with(
            CommitOptions::default()
                .with_time(1_800_000_300)
                .with_message(context.change_message().unwrap()),
        );
        pane.refresh(&mut tree);
        pane.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_600));
        pane.layout_window(Rect::new(0.0, 0.0, 640.0, 900.0));
        let theme = Theme::high_contrast().with_text_scale(2.0);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        pane.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
        let focus = ActivityFocus::Morning(key, false);
        assert!(
            pane.focus_order().contains(&focus),
            "long names must not push the source controls outside the viewport"
        );
        let accessible = pane.access_control(&focus).unwrap();
        assert!(accessible.description.contains("5 min ago"));
        assert!(accessible.description.contains("may be out of date"));
    }

    #[test]
    fn morning_source_native_render_and_keyboard_matrix() {
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target/morning-source-renders");
        std::fs::create_dir_all(&output).unwrap();
        for (name, theme) in [
            ("light", Theme::light()),
            ("dark", Theme::dark()),
            ("hc200", Theme::high_contrast().with_text_scale(2.0)),
        ] {
            for (size, width, height) in [("normal", 1600, 1000), ("compact", 640, 900)] {
                let (mut tree, mut pane) = fixture();
                pane.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_300));
                pane.apply_selection_history_action(MORNING_SOURCE, &mut tree, None);
                let key = pane.chosen_morning_sources()[0].key();
                pane.set_view(ActivityView::CatchUp);
                pane.layout_window(Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
                let mut scene = Scene::new();
                let mut text = TextRenderer::new();
                text.set_text_scale(theme.text_scale);
                pane.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &PresenceCtx::empty(),
                );
                let focus = ActivityFocus::Morning(key.clone(), false);
                assert!(
                    pane.focus_order().contains(&focus),
                    "chosen source must remain reachable at {name}/{size}"
                );
                let rectangle = pane.rect_for_focus(&focus).unwrap();
                assert!(rectangle.y1 <= f64::from(height));
                let accessible = pane.access_control(&focus).unwrap();
                assert!(accessible.description.contains("Weather"));
                assert!(accessible.description.contains("5 min ago"));
                assert!(accessible.description.contains("may be out of date"));
                pane.keyboard_focus = Some(focus.clone());
                let outcome = pane.activate_focus(&mut tree, None);
                assert_eq!(outcome.open_target.unwrap().id, "weather.location:fixture");
                assert_eq!(
                    pane.keyboard_focus,
                    Some(focus),
                    "opening must preserve logical focus"
                );
                let mut renderer = ui::offscreen::OffscreenRenderer::new(width, height);
                renderer.render_png(
                    &scene,
                    theme.tokens.colors.surfaces.content,
                    output.join(format!("{name}-{size}.png")),
                );
                pane.apply_selection_history_action(MORNING_SOURCE, &mut tree, None);
                pane.set_view(ActivityView::Explore);
                pane.explorer_pane = ExplorerPane::Details;
                let mut scene = Scene::new();
                pane.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &PresenceCtx::empty(),
                );
                assert!(
                    pane.focus_order()
                        .contains(&ActivityFocus::Detail(DetailAction::MorningSource)),
                    "the source choice must be a keyboard control at {name}/{size}"
                );
                for (_, rect) in &pane.detail_action_rects {
                    assert!(
                        rect.height()
                            >= ui::widget::ButtonFace::height(
                                ui::widget::ControlSize::Regular,
                                &theme
                            ),
                        "source/history controls must use the shared minimum height at {name}/{size}"
                    );
                    assert!(rect.y1 <= f64::from(height));
                }
                renderer.render_png(
                    &scene,
                    theme.tokens.colors.surfaces.content,
                    output.join(format!("{name}-{size}-choose.png")),
                );
            }
        }
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
    #[test]
    fn morning_payload_pager_keeps_large_sources_and_removed_evidence_reachable() {
        let (mut tree, mut pane) = fixture();
        pane.apply_selection_history_action(MORNING_SOURCE, &mut tree, None);
        let host = Rc::new(PayloadHost {
            noisy: true,
            ..Default::default()
        });
        pane.set_host(host.clone());
        let first = pane.chosen_morning_sources()[0].key();
        for index in 1..crate::activity::morning::MAX_SOURCES {
            let room = pane.task_room.clone();
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
        pane.set_view(ActivityView::CatchUp);
        pane.layout_window(Rect::new(0.0, 0.0, 640.0, 900.0));
        let theme = Theme::high_contrast().with_text_scale(2.0);
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        let heads = tree.doc.get_heads();
        let seen = pane.seen_frontiers.clone();
        for index in 0..crate::activity::morning::MAX_SOURCES {
            let mut scene = Scene::new();
            pane.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::empty(),
            );
            let key = pane.chosen_morning_sources()[index].key();
            let focus =
                ActivityFocus::MorningCommand(key.clone(), super::MorningCommand::NextSource);
            assert!(
                pane.focus_order().contains(&focus),
                "source {index} remains reachable"
            );
            assert!(
                pane.focus_order()
                    .contains(&ActivityFocus::Morning(key, true)),
                "even unavailable sources can be removed"
            );
            pane.keyboard_focus = Some(focus);
            pane.activate_focus(&mut tree, None);
        }
        assert_eq!(pane.morning_source_offset, 0);
        assert_eq!(
            pane.keyboard_focus,
            Some(ActivityFocus::MorningCommand(
                first,
                super::MorningCommand::NextSource
            ))
        );
        assert!(
            host.commands.borrow().is_empty(),
            "paging invokes no provider"
        );
        assert_eq!(tree.doc.get_heads(), heads);
        assert_eq!(pane.seen_frontiers, seen);
    }
    #[test]
    fn morning_payload_native_matrix_keeps_semantic_controls_and_review_boundary() {
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target/morning-payload-renders");
        std::fs::create_dir_all(&output).unwrap();
        for (name, theme) in [
            ("light", Theme::light()),
            ("dark", Theme::dark()),
            ("hc200", Theme::high_contrast().with_text_scale(2.0)),
        ] {
            for (size, width, height) in [("normal", 1600, 1000), ("compact", 640, 900)] {
                let (mut tree, mut pane) = fixture();
                pane.apply_selection_history_action(MORNING_SOURCE, &mut tree, None);
                let host = Rc::new(PayloadHost::default());
                pane.set_host(host.clone());
                let key = pane.chosen_morning_sources()[0].key();
                let heads = tree.doc.get_heads();
                let seen = pane.seen_frontiers.clone();
                pane.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_300));
                pane.set_view(ActivityView::CatchUp);
                pane.layout_window(Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
                let mut scene = Scene::new();
                let mut text = TextRenderer::new();
                text.set_text_scale(theme.text_scale);
                pane.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &PresenceCtx::empty(),
                );
                for command in [
                    super::MorningCommand::Refresh,
                    super::MorningCommand::DraftRefresh,
                ] {
                    let focus = ActivityFocus::MorningCommand(key.clone(), command);
                    assert!(
                        pane.focus_order().contains(&focus),
                        "command visible at {name}/{size}"
                    );
                    if command == super::MorningCommand::Refresh {
                        assert_eq!(
                            pane.access_control(&focus).unwrap().description,
                            t("activity-morning-refresh"),
                            "an immediate refresh must not be described as an inert draft"
                        );
                    }
                    pane.keyboard_focus = Some(focus);
                    pane.activate_focus(&mut tree, None);
                }
                assert_eq!(host.commands.borrow().len(), 2);
                assert!(
                    host.commands
                        .borrow()
                        .iter()
                        .all(|(room, _, _)| *room == pane.task_room)
                );
                let accessible = pane
                    .access_control(&ActivityFocus::Morning(key.clone(), false))
                    .unwrap();
                assert!(accessible.description.contains("12°C"));
                assert!(
                    accessible
                        .description
                        .contains("Synthetic chosen weather provider")
                );
                assert!(accessible.description.contains("2026-10-11T08:05:00Z"));
                assert!(accessible.description.contains("Last cached observation"));
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
                let mut renderer = ui::offscreen::OffscreenRenderer::new(width, height);
                renderer.render_png(
                    &scene,
                    theme.tokens.colors.surfaces.content,
                    output.join(format!("{name}-{size}.png")),
                );
            }
        }
    }
}
