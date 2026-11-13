use super::*;
#[allow(
    unused_imports,
    reason = "the store-backed history refresh is a method of this trait"
)]
use crate::activity::testing::StoreRecords as _;
use tilo_actions::remote::workspace_activity as wire;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    //! Every fixture mutates the workspace through the *production* mirrors and reads the pane back
    //! — nothing writes a synthetic `HistoryEntry` .

    use super::*;
    use crate::activity::testing::{TestTree as WorkspaceTree, next_id};
    use tilo_desktop_model::WorkspaceSlot;
    use ui::record::RecordStore;

    /// The pane with a device that may mutate the workspace.
    fn native_pane() -> WorkspaceActivityPane {
        let mut pane = WorkspaceActivityPane::new();
        pane.set_host(Rc::new(crate::activity::testing::TestHost::default()));
        pane
    }
    use automerge::ActorId;
    use automerge::transaction::{CommitOptions, Transactable};
    use collab_types::caps::{Access, CapOverride, Scope, Subject};
    use tilo_automerge::ApplyIncoming as _;

    /// A plausible half-hour: owner, one grant, one revoke, one rename,
    /// one window opened.
    fn busy_workspace() -> (WorkspaceTree, WorkspaceSlot, String) {
        busy_workspace_written_by(None)
    }

    /// [`busy_workspace`], but the half-hour is a collaborator's: what Catch up exists to show.
    fn peers_busy_workspace() -> (WorkspaceTree, WorkspaceSlot, String) {
        busy_workspace_written_by(Some(ActorId::from(b"ada-device".as_slice())))
    }

    fn busy_workspace_written_by(peer: Option<ActorId>) -> (WorkspaceTree, WorkspaceSlot, String) {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let reader = tree.doc.get_actor().clone();
        if let Some(peer) = peer {
            tree.doc.set_actor(peer);
        }
        let user: String = "test-owner".to_string();
        tree.adopt_ownerless_workspaces(&user);
        let slot = tree.desktop.active;

        let granted = tree
            .grant_in_workspace(
                slot,
                &user,
                CapOverride::grant(
                    Subject::app("files"),
                    Scope::FsPath("/home/user/notes".to_string()),
                    Access::Read,
                ),
            )
            .expect("the owner may grant in their own workspace");
        tree.grant_in_workspace(
            slot,
            &user,
            CapOverride::grant(
                Subject::app("terminal"),
                Scope::Device("camera".to_string()),
                Access::Read,
            ),
        )
        .expect("the owner may grant in their own workspace");
        assert!(tree.revoke_in_workspace(slot, &user, &granted));

        // A workspace's name is its own to govern, and its layout is a record beside it.
        if let Some(ws) = tree.desktop.workspaces.iter_mut().find(|w| w.slot == slot) {
            ws.name = "Design review".to_string();
        }
        tree.govern(slot);
        tree.seed_doc_from_cache();
        tree.doc.set_actor(reader);
        (tree, slot, user)
    }

    #[test]
    fn a_workspace_switch_reads_only_its_own_change_and_derives_nothing() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let first = tree.active;
        let second = tree.append_empty_workspace("Second");
        tree.commit_workspace_change();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        let reads = || {
            (
                WORKSPACE_CHANGE_READS.with(Cell::get),
                crate::activity::replay::REPLAY_CHECKPOINT_READS.with(Cell::get),
            )
        };
        let derived = format!("{:?}", pane.derived);
        for n in 0..12 {
            tree.set_active(if n % 2 == 0 { second } else { first });
            tree.commit_workspace_change();
            let (diffs, forks) = reads();
            let logged = pane.history_log.changes_read();
            pane.refresh(&mut tree);
            let (diffs_after, forks_after) = reads();
            // The switch's change is read once and passed over: no row to describe, nothing derived
            // again.
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
            // The first visit to "Second" checkpoints the change that created it, a row of that
            // workspace; nothing else forks.
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
        let frames = |pane: &WorkspaceActivityPane| {
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

    /// Switching takes no place in Activity's window: switching back and forth 250 times used to
    /// fill all 200 places with switches, and the rename before them, the one thing anyone did, was
    /// gone.
    #[test]
    fn switching_workspaces_never_pushes_what_people_did_out_of_activity() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let first = tree.active;
        let second = tree.append_empty_workspace("Second");
        tree.commit_workspace_change();
        if let Some(workspace) = tree.desktop.workspaces.iter_mut().find(|w| w.slot == first) {
            workspace.name = "Renamed before switching".to_string();
        }
        tree.govern(first);
        tree.seed_doc_from_cache();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        let renamed = |pane: &WorkspaceActivityPane| {
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
            tree.commit_workspace_change();
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

    /// A change that switches workspaces and edits a workspace at once does show a desktop of its
    /// own: replay reads it from the document, as for any other change.
    #[test]
    fn a_switch_written_with_an_edit_replays_the_edit() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let second = tree.append_empty_workspace("Second");
        tree.commit_workspace_change();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        tree.set_active(second);
        if let Some(workspace) = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.slot == second)
        {
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
            governance: HashMap::new(),
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
        let active = tree.desktop.active;
        tree.govern(active);
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

    fn pane_over(tree: &mut WorkspaceTree) -> WorkspaceActivityPane {
        let mut pane = native_pane();
        // Legacy timeline tests exercise the granular surface explicitly;
        // production opens on the outcome-first Catch up tab.
        pane.view = ActivityView::Explore;
        pane.refresh(tree);
        pane
    }

    fn agent_task_fixture() -> (WorkspaceTree, RecordStore, WorkspaceActivityPane, RecordId) {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let workspace = tree.desktop.active_workspace().expect("workspace").id();
        let records = RecordStore::in_memory();
        let record = RecordId::for_file("campaign-brief.tilo-doc");
        let checkout = ui::RecordCheckout::new(record.clone(), ui::RecordKind::new("document"));
        assert!(records.open_record(&checkout));
        let agent = ActorId::from(b"writing-agent".as_slice());
        let issuer = ActorId::from(b"ada-identity".as_slice());
        let agent_hex = agent.to_hex_string();
        let issuer_hex = issuer.to_hex_string();
        records.with(&record, |doc| {
            ui::record::init(doc, &record, &checkout.kind, &workspace.to_string());
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
            let key = ui::record_run_with_details(
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
            assert!(ui::history_source::runs::set_run_task(doc, &key, "task:1"));
            tilo_resource::history::commit_stamped_at(doc, 23);
        });
        tree.desktop
            .workspace_records_held
            .entry(workspace)
            .or_default()
            .insert(record.to_string(), checkout.kind.to_string());

        let mut pane = native_pane();
        pane.view = ActivityView::Explore;
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

    /// An agent's run is served as the timeline of what it did: the agent, never a person, what
    /// it was asked and why, and each record it changed as an action whose view is that record
    /// (here a file) and whose changes are the history's own, so undo names them.
    #[test]
    fn a_served_run_is_the_agents_timeline_of_actions_over_the_historys_changes() {
        let (tree, _records, mut pane, record) = agent_task_fixture();
        let served = pane.serve_runs(&wire::RunsQuery::default(), &tree);
        assert_eq!(served.total, 1);
        let run = &served.runs[0];
        assert_eq!(run.agent.name, "Quill");
        assert_eq!(run.agent.relation, wire::Relation::Software);
        assert_eq!(run.agent.on_behalf_of.as_deref(), Some("Ada"));
        assert_eq!(
            run.instruction,
            "Rewrite the opening and preserve the pricing table"
        );
        assert_eq!(
            run.decision.as_deref(),
            Some("The opening was vague; the pricing table was already correct.")
        );
        assert_eq!(run.actions.len(), 1, "one record changed, one action");
        let action = &run.actions[0];
        assert_eq!(action.summary, "Rewrite the launch story");
        assert_eq!(
            (action.app_id.as_str(), action.record.as_str()),
            ("org.tilo.Writer", record.as_str())
        );
        let wire::ActionView::File { path, target, .. } = &action.view else {
            panic!(
                "a file the agent changed opens as that file: {:?}",
                action.view
            );
        };
        assert!(path.ends_with("campaign-brief.tilo-doc"), "{path}");
        assert_eq!(target.app_id.as_deref(), Some("org.tilo.Writer"));
        assert!(
            action.undo.is_some(),
            "its record is open here, so it undoes alone"
        );
        // The action's changes and the run's are ranges of the feed's own entries.
        for range in [&action.range, &run.range] {
            assert!(
                pane.select_range(range),
                "{range:?} names changes the history lists"
            );
        }
        assert!(run.started_ms <= run.last_ms);
    }

    /// An app whose content is the workspace's own document (Notes' notebook) keeps it beside its
    /// records: what was done in it, by a person or an agent's task, is still part of the
    /// workspace's history.
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
                        && item.origin.record().is_some()
                }) && entry.context.as_ref().is_some_and(|context| {
                    context
                        .group
                        .as_ref()
                        .is_some_and(|group| group.kind == "agent-run")
                })
            })
            .expect("three-change task has an interior history entry");
        pane.timeline.set_selection(Some((middle, middle)));
        let task = pane
            .selected_agent_run()
            .map(|indexed| &indexed.run)
            .expect("selected task receipt");
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
        let (mut tree, _records, mut pane, _record) = agent_task_fixture();
        let host = Rc::new(super::tasks_link_tests::UndoHost::default());
        pane.set_host(host.clone());
        show_summary(&mut pane);
        assert!(pane.can_undo_agent_run());
        let outcome = pane.apply_detail_action(DetailAction::UndoTask, &mut tree);
        assert!(outcome.handled);
        let undo = host
            .undos
            .borrow()
            .last()
            .cloned()
            .expect("the pane asks the host to undo the run in the documents it changed");
        assert_eq!(undo.task_id, pane.agent_runs[0].run.task.clone().unwrap());
        assert_eq!(undo.steps.len(), 1);
        assert!(pane.undo_pending.is_some(), "one undo at a time");
    }

    /// The same record, admitted the way the shell admits it: as an app record read from a detached
    /// document rather than a mounted store.
    fn detached_records(
        records: &RecordStore,
        record: &RecordId,
    ) -> Vec<crate::activity::DetachedRecordHistory> {
        vec![crate::activity::DetachedRecordHistory {
            app: "org.tilo.Writer".to_string(),
            checkout: ui::RecordCheckout::new(record.clone(), ui::RecordKind::new("document")),
            document: records.read(record, Clone::clone),
        }]
    }

    fn detached_row(pane: &WorkspaceActivityPane, change: HistoryEntryId) -> usize {
        pane.timeline
            .entries()
            .iter()
            .position(|entry| {
                pane.projected_origins.get(&entry.id).is_some_and(|origin| {
                    matches!(
                        origin.origin,
                        crate::activity::index::HistoryOrigin::AppRecord { .. }
                    ) && origin.change == change
                })
            })
            .expect("the app record's row")
    }

    /// A record's history is its app's to write: the row is selectable and
    /// its change is readable, but restoring or undoing it here writes
    /// nothing.
    #[test]
    fn an_app_records_rows_are_restored_and_undone_by_its_app() {
        let (mut tree, records, mut pane, record) = agent_task_fixture();
        pane.refresh_detached_records(&mut tree, detached_records(&records, &record));
        let newest = pane.agent_runs[0].run.steps[0].run.end;
        let row = detached_row(&pane, newest);
        pane.timeline.set_selection(Some((row, row)));
        let before = records
            .with(&record, automerge::AutoCommit::get_heads)
            .unwrap();
        let undone = pane.apply_detail_action(DetailAction::Undo, &mut tree);
        let restored = pane.apply_detail_action(DetailAction::Restore, &mut tree);
        assert!(!undone.reverted && !restored.reverted);
        assert_eq!(
            records
                .with(&record, automerge::AutoCommit::get_heads)
                .unwrap(),
            before,
            "the record's document is untouched"
        );
    }

    #[test]
    fn a_detached_record_row_selects_the_agent_task_receipt() {
        let (mut tree, records, mut pane, record) = agent_task_fixture();
        pane.refresh_detached_records(&mut tree, detached_records(&records, &record));
        assert!(
            !pane.agent_runs.is_empty(),
            "the refresh the shell uses reads the runs"
        );
        let end = pane.agent_runs[0].run.steps[0].run.end;
        let row = detached_row(&pane, end);
        pane.timeline.set_selection(Some((row, row)));
        let task = pane
            .selected_agent_run()
            .map(|indexed| &indexed.run)
            .expect("the run's receipt");
        assert_eq!(task.steps[0].sentence(), "Rewrite the launch story");
        assert!(pane.can_undo_agent_run());
        let outcome = pane.apply_detail_action(DetailAction::UndoTask, &mut tree);
        assert!(
            !outcome.reverted,
            "the pane asks the host, it writes no record"
        );
        assert!(
            pane.agent_undo_notice.is_some(),
            "a host with no writer for the record says so"
        );
    }

    /// A record the history could not read is counted as missing, never silently left out: the feed
    /// says it is not complete.
    #[test]
    fn detached_record_coverage_counts_what_it_could_not_read() {
        let workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceSlot(9), "Design");
        let mut doc = automerge::AutoCommit::new();
        doc.put(
            automerge::ROOT,
            tilo_desktop_model::projection::WORKSPACE_ID_KEY,
            workspace.id().to_string(),
        )
        .unwrap();
        doc.commit();
        let mut tree = crate::activity::ActivitySnapshot::new(
            tilo_desktop_model::DesktopTree {
                active: workspace.slot,
                workspaces: vec![workspace],
                ..Default::default()
            },
            doc,
        )
        .unwrap();
        let mut record = automerge::AutoCommit::new();
        record
            .put(automerge::ROOT, "text", "shared causal hash")
            .unwrap();
        record.commit();
        let checkout = ui::RecordCheckout::new(RecordId::for_tab(1), ui::RecordKind::new("test"));
        let input = |app: &str, document| crate::activity::DetachedRecordHistory {
            app: app.into(),
            checkout: checkout.clone(),
            document,
        };
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        pane.refresh_detached_records(
            &mut tree,
            vec![
                input("app-a", Some(record.clone())),
                input("app-b", Some(record)),
                input("missing", None),
            ],
        );
        assert_eq!(pane.feed_coverage.referenced_records, 3);
        assert_eq!(pane.feed_coverage.indexed_records, 2);
        assert!(
            !pane.feed_coverage.is_complete(),
            "a record it could not read leaves the history incomplete"
        );
    }

    /// A record grows by a few changes between refreshes, however long its history: each refresh
    /// reads what was added since, from the document as the host reads it again, and never the
    /// whole log over.
    #[test]
    fn a_refresh_reads_only_what_a_long_record_added_since() {
        let workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceSlot(9), "Notes");
        let mut doc = automerge::AutoCommit::new();
        doc.put(
            automerge::ROOT,
            tilo_desktop_model::projection::WORKSPACE_ID_KEY,
            workspace.id().to_string(),
        )
        .unwrap();
        doc.commit();
        let mut tree = crate::activity::ActivitySnapshot::new(
            tilo_desktop_model::DesktopTree {
                active: workspace.slot,
                workspaces: vec![workspace],
                ..Default::default()
            },
            doc,
        )
        .unwrap();
        let mut record = automerge::AutoCommit::new();
        let body = record
            .put_object(automerge::ROOT, "body", automerge::ObjType::Text)
            .unwrap();
        let write = |record: &mut automerge::AutoCommit, lines: usize| {
            for line in 0..lines {
                let at = automerge::ReadDoc::length(record, &body);
                record
                    .splice_text(&body, at, 0, &format!("line {line}\n"))
                    .unwrap();
                record.commit();
            }
        };
        write(&mut record, 2_000);
        // The host reads the record from its store on every refresh: a document of its own.
        let read = |record: &mut automerge::AutoCommit| crate::activity::DetachedRecordHistory {
            app: "so.tilo.notes".into(),
            checkout: ui::RecordCheckout::package_primary(ui::RecordKind::new("so.tilo.notes")),
            document: Some(automerge::AutoCommit::load(&record.save()).unwrap()),
        };
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        pane.refresh_detached_records(&mut tree, vec![read(&mut record)]);
        for _ in 0..3 {
            write(&mut record, 3);
            let before = pane.history_index.changes_read();
            pane.refresh_detached_records(&mut tree, vec![read(&mut record)]);
            assert_eq!(
                pane.history_index.changes_read() - before,
                3,
                "a refresh reads the three changes made since"
            );
        }
        let newest = ui::history_source::entry_id_from_hash(
            &record.get_heads().first().expect("a head").to_string(),
        );
        assert!(
            pane.history_index
                .entries()
                .iter()
                .any(|row| row.id.change == newest),
            "the newest change is listed"
        );
    }

    #[test]
    fn a_window_someone_opened_is_listed_in_history_but_is_not_news() {
        let (mut tree, workspace, _) = busy_workspace();
        let host = Rc::new(crate::activity::testing::TestHost::default());
        let mut pane = WorkspaceActivityPane::new();
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
        assert_eq!(pane.passive_unread, 0, "opening a window is nobody's news");
        pane.begin_view(workspace.raw());
        assert!(pane.digest.briefs.is_empty());
    }

    #[test]
    fn a_workspace_first_seen_on_this_device_has_nothing_new() {
        let (mut tree, workspace, user) = busy_workspace();
        let host = Rc::new(crate::activity::testing::TestHost::default());
        let mut pane = WorkspaceActivityPane::new();
        pane.set_host(host.clone());
        pane.load_frontiers();
        pane.refresh(&mut tree);
        assert_eq!(
            pane.passive_unread, 0,
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
        .expect("the owner may grant in their own workspace");
        tree.seed_doc_from_cache();
        tree.doc.set_actor(reader);
        pane.refresh(&mut tree);
        let after_change = pane.passive_unread;
        assert!(after_change > 0, "a change after arriving is");

        let mut next_session = WorkspaceActivityPane::new();
        next_session.set_host(host);
        next_session.load_frontiers();
        next_session.refresh(&mut tree);
        assert_eq!(
            next_session.passive_unread, after_change,
            "the baseline persists, so the next session still counts only that change"
        );
    }

    #[test]
    fn closing_unpainted_catch_up_keeps_unseen_work_pending() {
        let (mut tree, workspace, _) = peers_busy_workspace();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        let unread = pane.passive_unread;
        assert!(unread > 0);
        pane.begin_view(workspace.raw());
        pane.finish_view(workspace.raw());
        pane.refresh(&mut tree);
        assert_eq!(pane.passive_unread, unread);
    }

    #[test]
    fn catch_up_is_default_and_closing_advances_only_private_frontier() {
        let (mut tree, workspace, _) = peers_busy_workspace();
        let before = tree.doc.get_heads();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        assert_eq!(pane.view, ActivityView::CatchUp);

        assert!(pane.passive_unread > 0);
        pane.begin_view(workspace.raw());
        assert!(!pane.digest.briefs.is_empty());
        show_summary(&mut pane);
        pane.finish_view(workspace.raw());
        assert_eq!(pane.passive_unread, 0);
        assert_eq!(
            tree.doc.get_heads(),
            before,
            "reading history is not a workspace edit"
        );
    }

    #[test]
    fn every_catch_up_card_cites_exactly_the_changes_it_summarises() {
        let (mut tree, workspace, _) = peers_busy_workspace();
        let mut pane = native_pane();
        pane.begin_view(workspace.raw());
        pane.refresh(&mut tree);
        show_summary(&mut pane);
        let digest = pane.digest.clone();
        assert!(
            !digest.briefs.is_empty(),
            "the seeded session leaves work for Catch Up to stand for"
        );
        // A card is one work session: the evidence it cites is the changes it opens onto, and the
        // count it states is that many.
        for brief in &digest.briefs {
            assert!(
                !brief.title.trim().is_empty(),
                "a card says what came of the session"
            );
            assert!(
                !brief.entry_ids.is_empty(),
                "a card stands for the changes that made it: {brief:?}"
            );
            assert_eq!(
                brief.actions.len(),
                brief.entry_ids.len(),
                "every cited change is one sub-action of its card"
            );
            assert_eq!(
                brief.changes,
                brief.entry_ids.len(),
                "the count the card states is the count of rows it opens"
            );
            assert!(
                brief
                    .actions
                    .iter()
                    .all(|action| !action.summary.trim().is_empty()
                        && !action.author.name.trim().is_empty()),
                "a sub-action says what was done and who did it: {:?}",
                brief.actions
            );
        }
        // And no change is cited by two cards, so opening one cannot show work another one already
        // owns.
        for (at, brief) in digest.briefs.iter().enumerate() {
            for id in &brief.entry_ids {
                assert_eq!(
                    digest
                        .briefs
                        .iter()
                        .filter(|other| other.entry_ids.contains(id))
                        .count(),
                    1,
                    "change {id:?} is cited only by the card at {at}"
                );
            }
        }
    }

    #[test]
    fn refreshed_catch_up_keeps_the_open_session_and_does_not_acknowledge_late_work() {
        let (mut tree, workspace, user) = peers_busy_workspace();
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
            pane.passive_unread > 0,
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
        assert_eq!(pane.passive_unread, 0);
        pane.view = ActivityView::CatchUp;
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
        assert_eq!(pane.passive_unread, 1);
        show_summary(&mut pane);
        pane.finish_view(workspace);
        assert_eq!(pane.passive_unread, 0);
        assert!(
            pane.digest.briefs.is_empty(),
            "old causal heads remain the baseline"
        );
        pane.begin_view(workspace);
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        assert_eq!(pane.passive_unread, 0);
        assert!(pane.digest.briefs.is_empty());
    }

    #[test]
    fn catch_up_persists_only_what_the_app_showed() {
        let (mut tree, workspace, _) = peers_busy_workspace();
        let records = RecordStore::in_memory();
        let identity = tree.active_workspace().expect("workspace").id();
        let identity_text = identity.to_string();
        for n in 0..12 {
            let id = RecordId::for_file(format!("result-{n}.note"));
            let kind = ui::RecordKind::new("note");
            assert!(records.open_record(&ui::RecordCheckout::new(id.clone(), kind.clone())));
            records.with(&id, |doc| {
                ui::record::init(doc, &id, &kind, &identity_text);
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
                .workspace_records_held
                .entry(identity)
                .or_default()
                .insert(id.to_string(), kind.to_string());
        }
        let host = Rc::new(crate::activity::testing::TestHost::default());
        let mut pane = WorkspaceActivityPane::new();
        pane.set_host(host.clone());
        pane.begin_view(workspace.raw());
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        pane.persistent_frontiers = true;
        let before = pane.passive_unread;
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
        pane.finish_view(workspace.raw());
        assert_eq!(
            tree.doc.get_heads(),
            heads,
            "review is private viewer state"
        );
        let mut next = WorkspaceActivityPane::new();
        next.set_host(host);
        next.load_frontiers();
        next.refresh(&mut tree);
        next.refresh_records(&mut tree, &records);
        assert_eq!(next.passive_unread, before - shown);
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

    /// What the recap holds after leaving the view of a workspace and coming back `elapsed` later,
    /// with the workspace changed meanwhile or not.
    fn away_change_count_after(elapsed: Duration, changed: bool) -> usize {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let records = RecordStore::in_memory();
        let home = tree.desktop.active;
        let departed_at = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(10_000))
            .expect("fixed test timestamp");
        let mut pane = native_pane();
        pane.persistent_frontiers = false;
        pane.set_now_for_test(departed_at);
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);

        // Departure follows the page the person was shown, not an indexed head.
        show_summary(&mut pane);
        pane.finish_view(home.raw());

        if changed {
            let reader = tree.doc.get_actor().clone();
            tree.doc.set_actor(ActorId::from(b"ada-device".as_slice()));
            tree.desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.slot == home)
                .expect("home workspace")
                .name = "Board review".to_string();
            tree.govern(home);
            tree.seed_doc_from_cache();
            tree.doc.set_actor(reader);
        }

        pane.set_now_for_test(
            departed_at
                .checked_add(elapsed)
                .expect("test elapsed time fits SystemTime"),
        );
        pane.refresh(&mut tree);
        pane.refresh_records(&mut tree, &records);
        pane.begin_view(home.raw());
        if pane.away_workspace == Some(home.raw()) {
            pane.digest.total_changes
        } else {
            0
        }
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
    fn rows_come_from_the_workspaces_own_change_graph() {
        let (mut tree, _, _) = busy_workspace();
        let pane = pane_over(&mut tree);
        assert!(
            pane.entries().len() >= 3,
            "the workspace being made, its grants, a revoke and the rename stay; setup writes do \
             not, got {}",
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
        // A workspace being made is setup, and its history keeps it out of the feed.
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
    fn a_selected_row_publishes_diff_and_restore_as_actions() {
        let (mut tree, _, _) = busy_workspace();
        let mut pane = pane_over(&mut tree);
        show_summary(&mut pane);
        // A row of the desk's own document: a governed row is read-only, and offers neither.
        let idx = (0..pane.timeline.row_count())
            .find(|&i| {
                pane.timeline
                    .row_entry(i)
                    .is_some_and(|entry| !pane.governance_rows.contains(&entry.id))
            })
            .expect("a change row");
        let index = pane.timeline.row_entry_index(idx).expect("entry");
        pane.timeline.set_selection(Some((index, index)));
        let entry = pane.timeline.entries()[index].id;
        let range = wire::Range::one(&wire::entry_hex(entry.as_bytes()));
        let offers = pane
            .serve_detail(
                &wire::DetailQuery {
                    range: range.clone(),
                    compare: false,
                },
                &mut tree,
            )
            .offers;
        let ids: Vec<wire::DetailAction> = offers.iter().map(|offer| offer.action).collect();
        assert!(
            ids.contains(&wire::DetailAction::Compare)
                && ids.contains(&wire::DetailAction::Restore),
            "selected row must offer compare and restore, got {ids:?}"
        );
        // Compare is an action, not a click-only chip: asking for it materialises the exact two
        // sides of the selected change, so the diff the person sees comes from the history layer,
        // not a click.
        assert!(
            pane.comparison.is_none(),
            "nothing is compared before Compare"
        );
        let compared = pane.serve_detail(
            &wire::DetailQuery {
                range,
                compare: true,
            },
            &mut tree,
        );
        assert!(
            matches!(compared.comparison, Some(wire::Comparison::Changes { .. })),
            "Compare reports the comparison it made"
        );
        let comparison = pane
            .comparison
            .as_ref()
            .expect("the comparison Compare made");
        assert_eq!(
            comparison.entries.len(),
            1,
            "one selected change compares one change: {:?}",
            comparison.entries
        );
        assert!(
            !comparison.before_document.is_empty() && !comparison.after_document.is_empty(),
            "both sides are full snapshots a resource renderer can load"
        );
        assert_ne!(
            comparison.before_document, comparison.after_document,
            "a change that changed nothing would compare identical sides"
        );
    }

    /// A second workspace with its own governance: each workspace governs itself in a document of
    /// its own.
    fn two_workspace_tree() -> (WorkspaceTree, WorkspaceSlot, WorkspaceSlot) {
        let (mut tree, first, user) = busy_workspace();
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
        .expect("owner may grant in the second workspace too");
        tree.set_active(first);
        (tree, first, second)
    }

    #[test]
    fn this_workspaces_own_document_is_the_only_governance_it_reads() {
        let (mut tree, _first, second) = two_workspace_tree();
        let mut pane = pane_over(&mut tree);
        assert_eq!(
            pane.scope,
            WorkspaceScope::ThisWorkspace,
            "scoped by default"
        );

        let scoped: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(
            !scoped.iter().any(|s| s.contains("research.example")),
            "the other workspace's network grant leaked into this one: {scoped:?}"
        );
        assert!(
            scoped.iter().any(|s| s.contains("/home/user/notes")),
            "this workspace's own grant is present: {scoped:?}"
        );

        // The other workspace's own document is read when it is the one in front.
        tree.set_active(second);
        pane.refresh(&mut tree);
        let there: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(
            there.iter().any(|s| s.contains("research.example")),
            "the other workspace's grant is its own: {there:?}"
        );
        assert!(
            !there.iter().any(|s| s.contains("/home/user/notes")),
            "the first workspace's grant is not the second's: {there:?}"
        );
    }

    #[test]
    fn a_widened_row_names_the_workspace_it_happened_in() {
        let (mut tree, first, _second) = two_workspace_tree();
        // A window opened in the workspace in front, whose grant its own document holds: a row of
        // the layout and a row of the governance.
        assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
            next_id(),
            "Brief".into(),
            Vec::new()
        ));
        let mut pane = pane_over(&mut tree);
        pane.set_scope(WorkspaceScope::AllWorkspaces, &tree);
        let named_rows: Vec<String> = pane
            .entries()
            .iter()
            .filter(|e| e.summary.contains("/home/user/notes") || e.summary.contains("Brief"))
            .map(|e| e.summary.clone())
            .collect();
        assert_eq!(named_rows.len(), 2, "both rows are here: {named_rows:?}");
        assert!(
            named_rows
                .iter()
                .any(|s| s.contains("Home") && s.contains("/home/user/notes")),
            "a governed row names the workspace it happened in: {named_rows:?}"
        );
        let _ = first;
    }

    #[test]
    fn switching_the_active_workspace_re_scopes_without_a_document_change() {
        let (mut tree, first, second) = two_workspace_tree();
        let mut pane = pane_over(&mut tree);
        let in_first: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(in_first.iter().any(|s| s.contains("/home/user/notes")));

        tree.set_active(second);
        pane.refresh(&mut tree);
        let in_second: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(
            in_second.iter().any(|s| s.contains("research.example")),
            "the second workspace's own grant: {in_second:?}"
        );
        assert!(
            !in_second.iter().any(|s| s.contains("/home/user/notes")),
            "the first workspace's grant must not follow us: {in_second:?}"
        );
        let _ = first;
    }

    #[test]
    fn a_whole_tree_flush_belongs_to_no_single_workspace() {
        let (mut tree, _first, _second) = two_workspace_tree();
        let mut pane = pane_over(&mut tree);
        assert!(
            pane.entries()
                .iter()
                .all(|e| !e.summary.starts_with("Recorded ")),
            "a whole-tree flush is setup bookkeeping, not a feed change"
        );
        pane.set_scope(WorkspaceScope::AllWorkspaces, &tree);
        assert!(
            pane.entries()
                .iter()
                .all(|e| !e.summary.starts_with("Recorded ")),
            "widening must not bring setup flushes back"
        );
    }

    #[test]
    fn the_local_users_own_rows_are_attributed_to_them() {
        let (mut tree, _, _) = busy_workspace();
        let pane = pane_over(&mut tree);
        let me = crate::activity::testing::LOCAL_USER.to_string();
        assert!(!me.is_empty(), "there is always a local user id");
        assert!(
            pane.entries()
                .iter()
                .all(|e| e.author.name != ui::history_source::UNKNOWN_AUTHOR_NAME),
            "local changes resolved to an unknown participant: {:?}",
            pane.contributors
        );
        assert_eq!(
            pane.contributors.len(),
            1,
            "one person did all of this: {:?}",
            pane.contributors
        );
    }

    #[test]
    fn every_row_carries_a_real_timestamp() {
        // The whole date filter rests on this — an epoch row can never be selected by a date range.
        let (mut tree, _, _) = busy_workspace();
        let pane = pane_over(&mut tree);
        assert!(!pane.entries().is_empty());
        for e in pane.entries() {
            assert!(
                e.timestamp > SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000),
                "row {:?} is stamped at {:?} — the workspace's commits are not carrying a clock",
                e.summary,
                e.timestamp
            );
        }
    }

    #[test]
    fn date_range_filter_partitions_the_real_history() {
        // One change stamped a week back via `commit_stamped_at` , the rest at now: "Today" drops
        // the old one, "30 days" keeps everything.
        let mut tree = WorkspaceTree::bootstrap_demo();
        let user: String = "test-owner".to_string();
        tree.adopt_ownerless_workspaces(&user);
        let slot = tree.desktop.active;

        let week_ago_secs = i64::try_from(ui::time::try_unix_now().unwrap() - 604_800).unwrap();
        tree.grant_uncommitted(
            slot,
            &user,
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath("/home/user/last-week".to_string()),
                Access::Read,
            ),
        )
        .expect("owner may grant");
        tilo_resource::history::commit_stamped_at(tree.governance_of(slot), week_ago_secs)
            .expect("the grant left a transaction to stamp");

        tree.grant_in_workspace(
            slot,
            &user,
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath("/home/user/today".to_string()),
                Access::Read,
            ),
        )
        .expect("owner may grant");

        let mut pane = pane_over(&mut tree);
        let all = pane.timeline.visible_len();
        assert!(
            all >= 2,
            "expected last week's grant and today's, got {all}"
        );

        pane.period = Period::Today;
        pane.reinstall_filter();
        let today = pane.timeline.visible_len();
        assert!(today > 0, "today's changes must survive the day filter");
        assert!(
            today < all,
            "the week-old change must be filtered out: {today} of {all} survived"
        );
        for i in 0..pane.timeline.visible_len() {
            let e = pane.timeline.visible_entry(i).unwrap();
            assert!(
                !e.summary.contains("last-week"),
                "the week-old row leaked through the day filter: {}",
                e.summary
            );
        }

        pane.period = Period::Month;
        pane.reinstall_filter();
        assert_eq!(
            pane.timeline.visible_len(),
            all,
            "a 30-day window covers a change from seven days ago"
        );
    }

    #[test]
    fn contributor_filter_narrows_to_one_person() {
        let (mut tree, _, _) = busy_workspace();

        // A peer of the same workspace renames it: its own document, applied here as the
        // workspace hands it back.
        let slot = tree.desktop.active;
        let mut peer = tree.governance_of(slot).fork();
        peer.set_actor(automerge::ActorId::from("bob".as_bytes()));
        {
            use automerge::transaction::Transactable;
            peer.put(automerge::ROOT, "name", "Bob's rename").unwrap();
        }
        tilo_resource::history::commit_stamped(&mut peer).expect("the peer's edit commits");
        let blobs: Vec<Vec<u8>> = peer
            .get_changes(&tree.governance_of(slot).get_heads())
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect();
        tree.apply_governance_changes(slot, &blobs)
            .expect("the peer's rename lands");

        let mut pane = pane_over(&mut tree);
        assert!(
            pane.contributors.len() >= 2,
            "two actors wrote to this workspace; contributors = {:?}",
            pane.contributors
        );
        // bootstrap_demo seeds a Research collaborator id "bob" named "Bob" with a cursor colour.
        // seed_roster_colours registers that display name on the handle actor, so history shows
        // "Bob" rather than the raw handle — same person, roster wins for colour and label.
        let bob = pane
            .contributors
            .iter()
            .find(|c| c.eq_ignore_ascii_case("bob"))
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "the peer's self-declared handle resolves to a contributor; got {:?}",
                    pane.contributors
                )
            });

        let everyone = pane.timeline.visible_len();
        pane.author = Some(bob.clone());
        pane.reinstall_filter();
        let just_bob = pane.timeline.visible_len();
        assert!(just_bob > 0, "bob authored at least one row");
        assert!(
            just_bob < everyone,
            "filtering to bob must hide the local actor's rows: {just_bob} of {everyone}"
        );
        for i in 0..pane.timeline.visible_len() {
            assert_eq!(pane.timeline.visible_entry(i).unwrap().author.name, bob);
        }

        pane.author = None;
        pane.reinstall_filter();
        assert_eq!(
            pane.timeline.visible_len(),
            everyone,
            "clearing restores every row"
        );
    }

    #[test]
    fn action_type_chips_narrow_by_entry_kind() {
        use ui::widget::activity_timeline::ChipKind;
        let (mut tree, _, _) = busy_workspace();
        let mut pane = pane_over(&mut tree);
        let all = pane.timeline.visible_len();

        pane.timeline.set_active_chip(ChipKind::Governance);
        let gov = pane.timeline.visible_len();
        assert!(
            gov > 0,
            "grants and revokes are governance; the workspace has some"
        );
        assert!(gov < all, "governance is a subset: {gov} of {all}");
        for i in 0..pane.timeline.visible_len() {
            assert_eq!(
                pane.timeline.visible_entry(i).unwrap().entry_kind,
                EntryKind::Access,
                "the Governance chip must only show governance rows"
            );
        }

        pane.timeline.set_active_chip(ChipKind::All);
        assert_eq!(pane.timeline.visible_len(), all);
    }

    #[test]
    fn reverting_a_row_writes_a_new_forward_change() {
        let (mut tree, slot, user) = busy_workspace();
        let mut pane = pane_over(&mut tree);

        let before_name = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.slot == slot)
            .map(|w| w.name.clone())
            .unwrap();
        assert_eq!(before_name, "Design review");
        let versions_before = pane.entries().len();

        // Another window opens, so there is a change of the desk's own document to revert.
        assert!(
            tree.doc_append_tab_to_first_group_of_active_workspace(
                next_id(),
                "Opened by mistake".into(),
                Vec::new()
            ),
            "the layout record takes the window"
        );
        pane.refresh(&mut tree);
        assert!(pane.entries().len() > versions_before);

        let target = pane
            .entries()
            .iter()
            .find(|entry| entry.summary.contains("Opened by mistake"))
            .map(|entry| entry.id)
            .expect("the window's row");
        assert!(
            pane.revert_to(&mut tree, &target),
            "the revert wrote something"
        );

        let windows = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.slot == slot)
            .map(|w| {
                w.strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .filter(|tab| tab.title == "Opened by mistake")
                    .count()
            });
        assert_eq!(windows, Some(0), "the window the row named is gone again");
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
        let (mut tree, _, _) = busy_workspace();
        let mut pane = pane_over(&mut tree);
        let before = tree.doc.get_heads();
        assert!(!pane.revert_to(&mut tree, &HistoryEntryId([0x5a; 16])));
        assert_eq!(tree.doc.get_heads(), before, "nothing was written");
    }

    #[test]
    fn governance_rows_never_offer_revert() {
        let (mut tree, _, _) = busy_workspace();
        let mut pane = pane_over(&mut tree);
        pane.timeline
            .set_active_chip(ui::widget::activity_timeline::ChipKind::Governance);
        assert!(pane.timeline.visible_len() > 0);
        for i in 0..pane.timeline.visible_len() {
            let e = pane.timeline.visible_entry(i).unwrap();
            assert!(
                !e.entry_kind.is_revertible(),
                "{:?} offered a revert chip",
                e.summary
            );
            assert!(
                pane.timeline.revert_chip_rect(i).is_none(),
                "row {i} ({:?}) laid out a revert chip",
                e.summary
            );
        }
    }

    #[test]
    fn a_capability_is_described_in_the_workspaces_vocabulary() {
        let (mut tree, _, _) = busy_workspace();
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
    fn an_empty_workspace_and_a_filtered_one_do_not_read_the_same() {
        let mut empty = WorkspaceTree::default();
        let pane = pane_over(&mut empty);
        let (title, detail) = pane.timeline.empty_state_lines();
        assert_eq!(title, "No activity yet");
        assert!(detail.is_none(), "nothing to explain when nothing happened");

        let (mut tree, _, _) = busy_workspace();
        let mut pane = pane_over(&mut tree);
        pane.author = Some("nobody-by-that-name".to_string());
        pane.reinstall_filter();
        assert_eq!(pane.timeline.visible_len(), 0);
        let (title, detail) = pane.timeline.empty_state_lines();
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
        let (mut tree, _, _) = busy_workspace();
        tree.desktop
            .collaborators
            .push(tilo_desktop_model::Collaborator::new(
                "ghost",
                "Never Edited",
            ));
        let pane = pane_over(&mut tree);
        assert!(
            !pane.contributors.iter().any(|c| c == "Never Edited"),
            "a peer who never wrote must not be listed: {:?}",
            pane.contributors
        );
    }

    #[test]
    fn activity_header_digest_reduces_the_same_scoped_rows() {
        let (mut tree, _, _) = peers_busy_workspace();
        let pane = pane_over(&mut tree);
        let others = pane
            .entries()
            .iter()
            .filter(|entry| !author_is_local(&entry.author, &pane.local_user()))
            .count();
        assert!(others > 0, "the fixture is a collaborator's work");
        assert_eq!(pane.digest.total_changes, others);
        assert!(!pane.digest.briefs.is_empty());
        assert!(pane.digest.headline().is_some());
    }

    #[test]
    fn catch_up_leaves_out_the_readers_own_work() {
        let (mut tree, workspace, _) = busy_workspace();
        let mut pane = native_pane();
        pane.refresh(&mut tree);
        assert_eq!(pane.view, ActivityView::CatchUp);
        assert!(
            !pane.entries().is_empty(),
            "the reader's own work stays in the timeline"
        );
        assert_eq!(
            pane.passive_unread, 0,
            "your own setup and edits are not news"
        );
        pane.begin_view(workspace.raw());
        assert_eq!(pane.digest.total_changes, 0);
        assert!(pane.digest.briefs.is_empty());
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
        pane.view = ActivityView::Replay;
        assert_eq!(pane.serve_replay().frames.len(), pane.replay_frames.len());

        let mut presence = PresenceCtx::empty();
        let ada = ui::permissions::UserId("ada".into());
        presence.names.insert(ada.clone(), "Ada".into());
        presence.set_pointer(
            &ui::presence::PresenceKey::desktop_pointer("design-workspace"),
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
    fn a_long_lived_workspace_stays_bounded() {
        // Well past the producer's ceiling, all of it real: each iteration is the same mirror the
        // drop handler calls when a file is dropped into a workspace. The pane must show the recent
        // end of the history rather than growing without limit.
        let mut tree = WorkspaceTree::bootstrap_demo();
        let user = "test-owner".to_string();
        tree.adopt_ownerless_workspaces(&user);
        let slot = tree.desktop.active;
        for i in 0..240u64 {
            tree.grant_in_workspace(
                slot,
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
            "the pane derived {} rows from a 240-change workspace",
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
    fn the_workspaces_history_reads_as_bands_rather_than_one_row_per_change() {
        let (mut tree, _, _) = busy_workspace();
        let pane = pane_over(&mut tree);
        assert!(pane.entries().len() >= 4);
        // Bands, not a wall: every rendered line is either a band header or a change, and there are
        // fewer lines than changes.
        let bands = (0..pane.timeline.row_count())
            .filter(|i| pane.timeline.group_at_row(*i).is_some())
            .count();
        assert!(
            bands >= 1,
            "a workspace with governance, edits and a deletion must produce at least one band"
        );
        // Every band describes itself without a name, and describes itself honestly — a count it
        // did not measure would be the failure here.
        for i in 0..pane.timeline.row_count() {
            if let Some(title) = pane.timeline.group_title_at_row(i) {
                assert!(!title.is_empty(), "band {i} has no title");
                assert!(
                    !title.contains("{}"),
                    "band {i} rendered a format placeholder: {title:?}"
                );
            }
        }
    }

    #[test]
    fn naming_a_band_round_trips_through_the_workspace_document() {
        let (mut tree, _, _) = busy_workspace();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        let (start, end) = (entries[1].id, entries[3].id);

        let wrote = pane.apply_name(
            &mut tree,
            &GroupNameRequest {
                record_id: None,
                start,
                end,
                label: "Set up the workspace".to_string(),
            },
        );
        assert!(wrote, "the name went into the document");

        // It is in the document, not in the widget: a fresh pane over the same tree finds it.
        let ranges = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].label, "Set up the workspace");
        assert_eq!(ranges[0].start, start);
        assert_eq!(ranges[0].end, Some(end));

        let mut fresh = native_pane();
        fresh.refresh(&mut tree);
        let titles: Vec<&str> = (0..fresh.timeline.row_count())
            .filter_map(|i| fresh.timeline.group_title_at_row(i))
            .collect();
        assert!(
            titles.contains(&"Set up the workspace"),
            "the band is on screen for anyone who opens the pane: {titles:?}"
        );
    }

    #[test]
    fn naming_a_band_does_not_add_a_row_of_its_own() {
        // The name is already on screen as the band's title. A "1 field" row directly under it
        // would be noise, so the producer filters the marker change out — and the change count must
        // not move.
        let (mut tree, _, _) = busy_workspace();
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
        let (mut tree, _, _) = busy_workspace();
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
        let (mut tree, _, _) = busy_workspace();
        let mut pane = pane_over(&mut tree);
        let entries = pane.entries().to_vec();
        let (start, end) = (entries[1].id, entries[3].id);

        // A peer forks the workspace document, names the same run differently, and syncs back — the
        // shape a remote replica's sync round has.
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

        // Merge each way round and compare. Both sides have to land on the same single band,
        // whichever order the names arrived in.
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
        let (mut tree, _, _) = busy_workspace();
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
        let named = (0..pane.timeline.row_count())
            .find(|i| pane.timeline.group_title_at_row(*i) == Some("Ada's pass"))
            .expect("the peer's band is on screen");
        let sub = pane.timeline.group_subtitle_at_row(named).unwrap();
        assert!(
            sub.contains("named by"),
            "a name someone else wrote must say so: {sub:?}"
        );
        assert!(
            !sub.contains(&format!("named by {}", pane.local_user())),
            "and it must not be attributed to the reader: {sub:?}"
        );
    }

    #[test]
    fn restoring_to_a_band_boundary_lands_on_the_state_at_that_boundary() {
        // The band header's [Restore] passes the band's last change, so this is the production path
        // for "restore to the end of this version".
        let (mut tree, slot, _) = busy_workspace();
        let mut pane = pane_over(&mut tree);

        // Find the change that named the workspace, and the state just after it.
        let renamed_at = pane
            .entries()
            .iter()
            .position(|e| e.summary.contains("Design review"))
            .expect("the fixture renames the workspace");
        let boundary = pane.entries()[renamed_at].id;

        // Move on: rename again, so the document is no longer in that state.
        if let Some(ws) = tree.desktop.workspaces.iter_mut().find(|w| w.slot == slot) {
            ws.name = "Something else".to_string();
        }
        tree.govern(slot);
        tree.seed_doc_from_cache();
        pane.invalidate();
        pane.refresh(&mut tree);
        assert_eq!(
            tree.desktop
                .workspaces
                .iter()
                .find(|w| w.slot == slot)
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
                .find(|w| w.slot == slot)
                .map(|w| w.name.clone()),
            Some("Design review".to_string()),
            "the workspace is back in the state that boundary produced"
        );
        // Forward, not a rewind: the change we rolled past is still in the graph.
        assert!(
            pane.entries().len() > renamed_at + 1,
            "history was appended to, not rewritten"
        );
    }

    /// The app shows what is new and says so ([`wire::MARK_VIEWED`]), to be counted seen once the
    /// person stops reading.
    fn show_summary(pane: &mut WorkspaceActivityPane) {
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
        let (mut tree, _, _) = busy_workspace();
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
        pane.timeline.set_selection(Some((2, 2)));
        assert!(pane.split_selection_group(&mut tree));
        let split = ui::history_groups::read_named_ranges(&tree.doc, &AuthorDirectory::new());
        assert_eq!(split.len(), 2);
        assert!(
            split
                .iter()
                .any(|range| { range.start == entries[1].id && range.end == Some(entries[1].id) })
        );
        assert!(split.iter().any(|range| range.start == entries[2].id));

        pane.timeline.set_selection(Some((1, 3)));
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
        let (mut tree, _, _) = peers_busy_workspace();
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
        pane.view = ActivityView::CatchUp;

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
        pane.view = ActivityView::CatchUp;
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
            "receipt-backed history is attributed to its authenticated action actor, not the workspace host"
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
        reason = "one workspace, one labelled kind of change per step, each compared with a fresh pane"
    )]
    fn incremental_refresh_matches_full_replay_for_feed_undo_and_attribution() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let home = tree.active;
        let second = tree.append_empty_workspace("Second");
        tree.commit_workspace_change();
        let mut pane = native_pane();
        pane.refresh(&mut tree);

        let mut peer = tree.clone();
        peer.doc.set_actor(ActorId::from(b"bea-device".as_slice()));
        let receipt = || {
            tilo_actions::ActionContext::for_resource("document:brief", "document", "Release brief")
                .with_app("org.tilo.Writer", "Writer")
                .with_described(crate::activity::testing::verb("Export"))
        };

        // Every kind of row the steps make, once, before reads are counted: the first row of a kind
        // can load a catalogue, which moves the i18n revision, and the log then rightly reads its
        // window again in the new words.
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
        // The catalogues those rows loaded were added after this refresh read the log; the next
        // change reads it under them.
        tree.set_active(second);
        tree.commit_workspace_change();
        pane.refresh(&mut tree);
        tree.set_active(home);
        tree.commit_workspace_change();
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
                    tree.commit_workspace_change();
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
                    tree.commit_workspace_change();
                    "open a tab"
                }
                2 => {
                    // The peer acts while this device switches back: two concurrent changes, merged
                    // in either order.
                    assert!(peer.append_workspace_effect_receipt(
                        home,
                        tilo_actions::InvocationId::new(),
                        "bea",
                        receipt(),
                        tilo_actions::EffectReceiptStatus::Succeeded,
                    ));
                    tree.set_active(home);
                    tree.commit_workspace_change();
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
                    peer.commit_workspace_change();
                    let theirs = peer.export_changes_since(&synced);
                    tree.apply_changes(&theirs).expect("the peer's tab merges");
                    "a peer's tab"
                }
                4 => {
                    tree.set_active(second);
                    tree.commit_workspace_change();
                    tree.set_active(home);
                    tree.commit_workspace_change();
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
            let frames = |pane: &WorkspaceActivityPane| {
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
        let local = "kai";
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
    use crate::activity::TaskRunDocument;
    use crate::activity::testing::TestTree as WorkspaceTree;
    use ui::record::RecordStore;
    #[derive(Default)]
    pub(super) struct UndoHost {
        pub(super) undos: RefCell<Vec<TaskRunUndo>>,
        ended: RefCell<Option<Result<Vec<TaskRunDocument>, String>>>,
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
        fn poll_task_run_undo(
            &self,
            _invocation: uuid::Uuid,
        ) -> Option<Result<Vec<TaskRunDocument>, String>> {
            self.ended.borrow_mut().take()
        }
    }
    fn fixture() -> (WorkspaceTree, WorkspaceActivityPane, Rc<UndoHost>) {
        let mut tree = WorkspaceTree::bootstrap_default();
        let host = Rc::new(UndoHost::default());
        let mut pane = WorkspaceActivityPane::new();
        pane.set_host(host.clone());
        pane.refresh(&mut tree);
        pane.view = ActivityView::Explore;
        (tree, pane, host)
    }

    /// Once an agent's run of a task recorded what it changed under the task, undoing it asks the
    /// host to undo the run in each changed document, and tells how that ended.
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
            undo.workspace,
            tree.active_workspace().unwrap().id().to_string()
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

        host.ended.replace(Some(Ok(vec![TaskRunDocument {
            app: "so.tilo.notes".into(),
            record: "file:/notes/release.note".into(),
        }])));
        pane.refresh(&mut tree);
        assert!(pane.undo_pending.is_none());
        assert_eq!(
            pane.agent_undo_notice
                .as_ref()
                .map(|(_, notice)| notice.clone()),
            Some(t("activity-undo-task-done"))
        );
        assert!(pane.unreverted.is_empty(), "every record came back");
    }

    /// Each record of the run can be undone on its own from the task view: the host is asked for
    /// the task, in that one document only.
    #[test]
    fn one_record_of_a_run_is_undone_alone() {
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
            &[
                ui::RunStep {
                    app: "so.tilo.notes".into(),
                    record: "file:/notes/release.note".into(),
                    run: ui::ChangeRun::new(change, change),
                    what: Vec::new(),
                },
                ui::RunStep {
                    app: "so.tilo.sheets".into(),
                    record: "file:/sheets/budget.tilo-sheet".into(),
                    run: ui::ChangeRun::new(change, change),
                    what: Vec::new(),
                },
            ],
        )
        .unwrap();
        assert!(ui::history_source::runs::set_run_task(doc, &key, "task:1"));
        doc.commit();
        pane.refresh_records(&mut tree, &RecordStore::in_memory());

        assert!(pane.undo_task_record(
            "task:1",
            "so.tilo.sheets",
            "file:/sheets/budget.tilo-sheet"
        ));
        let undo = host.undos.borrow().last().cloned().unwrap();
        assert_eq!(undo.task_id, "task:1");
        assert_eq!(
            undo.steps,
            vec![(
                "so.tilo.sheets".to_owned(),
                "file:/sheets/budget.tilo-sheet".to_owned()
            )],
            "only the record the person picked is undone"
        );
    }

    /// The agent composer follows the task, not the history: it names the run by its task, and the
    /// same one record is undone, nothing else; a task that changed nothing is refused.
    #[test]
    fn one_record_of_a_run_is_undone_by_its_task() {
        use tilo_actions::remote::workspace_activity as wire;
        let (mut tree, mut pane, host) = fixture();
        let change = ui::history_source::entry_id_from_hash(&"ef".repeat(32));
        let doc = tree.document_mut();
        let key = ui::record_run_with_details(
            doc,
            "Move the card to Done",
            Some("agent:scribe"),
            Some("local"),
            None,
            None,
            &[
                ui::RunStep {
                    app: "so.tilo.notes".into(),
                    record: "file:/notes/release.note".into(),
                    run: ui::ChangeRun::new(change, change),
                    what: Vec::new(),
                },
                ui::RunStep {
                    app: "so.tilo.lang.board".into(),
                    record: String::new(),
                    run: ui::ChangeRun::new(change, change),
                    what: Vec::new(),
                },
            ],
        )
        .unwrap();
        assert!(ui::history_source::runs::set_run_task(
            doc,
            &key,
            "tasks.tracker/issues/release"
        ));
        doc.commit();
        pane.refresh_records(&mut tree, &RecordStore::in_memory());

        let query = |task: &str| {
            serde_json::to_value(wire::RecordQuery {
                task: Some(task.into()),
                app_id: "so.tilo.lang.board".into(),
                ..wire::RecordQuery::default()
            })
            .unwrap()
            .as_object()
            .cloned()
            .unwrap()
        };
        let outcome = pane
            .serve_action(
                wire::UNDO_RECORD,
                &query("tasks.tracker/issues/release"),
                &mut tree,
            )
            .unwrap();
        assert!(outcome.handled);
        let undo = host.undos.borrow().last().cloned().unwrap();
        assert_eq!(undo.task_id, "tasks.tracker/issues/release");
        assert_eq!(
            undo.steps,
            vec![("so.tilo.lang.board".to_owned(), String::new())],
            "only that app's record of the task is undone"
        );
        assert!(
            pane.serve_action(
                wire::UNDO_RECORD,
                &query("tasks.tracker/issues/other"),
                &mut tree
            )
            .is_err()
        );
    }

    /// A record the host did not undo is named, not dropped: the task view says what did not come
    /// back.
    #[test]
    fn a_record_undo_did_not_reach_is_reported() {
        let (mut tree, mut pane, host) = fixture();
        let change = ui::history_source::entry_id_from_hash(&"cd".repeat(32));
        let doc = tree.document_mut();
        let key = ui::record_run_with_details(
            doc,
            "Review release notes",
            Some("agent:scribe"),
            Some("local"),
            None,
            None,
            &[
                ui::RunStep {
                    app: "so.tilo.notes".into(),
                    record: "file:/notes/release.note".into(),
                    run: ui::ChangeRun::new(change, change),
                    what: Vec::new(),
                },
                ui::RunStep {
                    app: "so.tilo.sheets".into(),
                    record: "file:/sheets/budget.tilo-sheet".into(),
                    run: ui::ChangeRun::new(change, change),
                    what: Vec::new(),
                },
            ],
        )
        .unwrap();
        assert!(ui::history_source::runs::set_run_task(doc, &key, "task:1"));
        doc.commit();
        pane.refresh_records(&mut tree, &RecordStore::in_memory());

        assert!(pane.undo_task_run("task:1"));
        // The host reached only the notes document.
        host.ended.replace(Some(Ok(vec![TaskRunDocument {
            app: "so.tilo.notes".into(),
            record: "file:/notes/release.note".into(),
        }])));
        pane.poll_undo();
        assert_eq!(
            pane.agent_undo_notice
                .as_ref()
                .map(|(_, notice)| notice.clone()),
            Some(t("activity-undo-task-partial-records"))
        );
        assert_eq!(pane.unreverted.len(), 1, "the sheets record is named");
        assert!(
            pane.unreverted[0].contains("so.tilo.sheets"),
            "the record's app is said: {:?}",
            pane.unreverted
        );
    }

    /// A run the host cannot undo says so, in the host's words, and reports no record as come
    /// back.
    #[test]
    fn a_run_the_host_cannot_undo_is_reported_as_refused() {
        let (mut tree, mut pane, host) = fixture();
        let change = ui::history_source::entry_id_from_hash(&"12".repeat(32));
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
        let why = "open the documents this task changed to undo it";
        host.ended.replace(Some(Err(why.into())));
        pane.poll_undo();
        assert_eq!(
            pane.agent_undo_notice
                .as_ref()
                .map(|(_, notice)| notice.clone()),
            Some(crate::i18n::t_args(
                "activity-undo-task-refused",
                &[("detail", why.into())]
            ))
        );
        assert!(pane.unreverted.is_empty(), "nothing came back");
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
        let mut pane = WorkspaceActivityPane::new();
        pane.set_host(Rc::new(VisitorHost));
        pane.refresh(&mut tree);
        pane.view = ActivityView::CatchUp;
        assert!(
            !pane.digest.briefs.is_empty(),
            "the change itself stays visible"
        );
        for brief in &pane.digest.briefs {
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

    fn fixture() -> (TestTree, WorkspaceActivityPane) {
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
        let mut pane = WorkspaceActivityPane::new();
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

    /// What the selected row offers, as the app reads it.
    fn selection_offers(
        pane: &mut WorkspaceActivityPane,
        tree: &mut TestTree,
    ) -> Vec<wire::DetailAction> {
        let (start, end) = pane.timeline.selection().expect("a selected change");
        let range = wire::Range {
            from: wire::entry_hex(pane.timeline.entries()[start].id.as_bytes()),
            to: wire::entry_hex(pane.timeline.entries()[end].id.as_bytes()),
        };
        pane.serve_detail(
            &wire::DetailQuery {
                range,
                compare: false,
            },
            tree,
        )
        .offers
        .iter()
        .map(|offer| offer.action)
        .collect()
    }

    /// Choose the selected change's resource as this device's morning source.
    fn choose_source(
        pane: &mut WorkspaceActivityPane,
        tree: &mut TestTree,
    ) -> WorkspaceActivityOutcome {
        let (start, end) = pane.timeline.selection().expect("a selected change");
        let range = wire::Range {
            from: wire::entry_hex(pane.timeline.entries()[start].id.as_bytes()),
            to: wire::entry_hex(pane.timeline.entries()[end].id.as_bytes()),
        };
        let arguments = serde_json::to_value(range)
            .unwrap()
            .as_object()
            .cloned()
            .expect("a range is an object");
        pane.serve_action(wire::CHOOSE_SOURCE, &arguments, tree)
            .expect("the selected change names a source")
    }

    #[test]
    fn morning_source_choice_is_a_semantic_action_on_real_resource_history() {
        let (mut tree, mut pane) = fixture();
        let entry = pane.timeline.selection().expect("a selected change").0;
        let entry = pane.timeline.entries()[entry].id;
        let offers = pane
            .serve_detail(
                &wire::DetailQuery {
                    range: wire::Range::one(&wire::entry_hex(entry.as_bytes())),
                    compare: false,
                },
                &mut tree,
            )
            .offers;
        assert!(
            offers
                .iter()
                .any(|offer| offer.action == wire::DetailAction::ChooseSource),
            "a resource change offers itself as a source"
        );
        let heads = tree.doc.get_heads();
        assert!(choose_source(&mut pane, &mut tree).handled);
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
    fn morning_sources_survive_restart_without_crossing_workspaces_or_acknowledging_work() {
        use tilo_actions::remote::workspace_activity::SourceCommand;
        let (mut tree, mut pane) = fixture();
        let host = Rc::new(Host::default());
        pane.set_host(host.clone());
        assert!(pane.chosen_morning_sources().is_empty());
        let before = pane.seen_frontiers.clone();
        choose_source(&mut pane, &mut tree);
        assert_eq!(pane.chosen_morning_sources().len(), 1);
        assert_eq!(pane.seen_frontiers, before);
        let key = pane.chosen_morning_sources()[0].key();
        let workspace = tree.workspaces[0].id();
        let mut restarted = WorkspaceActivityPane::new();
        restarted.set_host(host.clone());
        restarted.load_frontiers();
        restarted.refresh(&mut tree);
        assert_eq!(restarted.chosen_morning_sources().len(), 1);
        let outcome = run_source(&mut restarted, &mut tree, &key, SourceCommand::Open);
        assert_eq!(outcome.open_target.unwrap().id, "weather.location:fixture");
        tree.workspaces[0] =
            same_id_in(&tree.workspaces[0], tilo_desktop_model::WorkspaceId::new());
        restarted.refresh(&mut tree);
        assert!(
            restarted.chosen_morning_sources().is_empty(),
            "even the same workspace id cannot borrow another workspace's choices"
        );
        assert!(
            run_source(&mut restarted, &mut tree, &key, SourceCommand::Open)
                .open_target
                .is_none(),
            "a source of another workspace identity does not open here"
        );
        tree.workspaces[0] = same_id_in(&tree.workspaces[0], workspace);
        restarted.refresh(&mut tree);
        assert_eq!(restarted.chosen_morning_sources().len(), 1);
        restarted.timeline.set_entries(Vec::new());
        assert!(
            restarted.morning_target(&key).is_none(),
            "missing or revoked resource evidence is unavailable, never fabricated"
        );
        run_source(&mut restarted, &mut tree, &key, SourceCommand::Remove);
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
        choose_source(&mut pane, &mut tree);
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
                    !selection_offers(&mut pane, &mut tree)
                        .contains(&wire::DetailAction::ChooseSource),
                    "no more sources can be chosen at the bound"
                );
            }
            choose_source(&mut pane, &mut tree);
        }
        assert_eq!(pane.chosen_morning_sources().len(), 8);
    }

    fn run_source(
        pane: &mut WorkspaceActivityPane,
        tree: &mut TestTree,
        key: &str,
        command: tilo_actions::remote::workspace_activity::SourceCommand,
    ) -> WorkspaceActivityOutcome {
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
        )
        .expect("a chosen source runs its command")
    }

    #[test]
    fn morning_payload_commands_reach_only_this_workspace_and_change_no_history() {
        use tilo_actions::remote::workspace_activity::SourceCommand;
        let (mut tree, mut pane) = fixture();
        choose_source(&mut pane, &mut tree);
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
                .all(|(workspace, _, _)| *workspace == pane.workspace)
        );
        assert_eq!(tree.doc.get_heads(), heads);
        assert_eq!(pane.seen_frontiers, seen);
    }

    #[test]
    fn morning_payload_paging_keeps_large_sources_and_removed_evidence_reachable() {
        use tilo_actions::remote::workspace_activity::SourceCommand;
        let (mut tree, mut pane) = fixture();
        choose_source(&mut pane, &mut tree);
        let host = Rc::new(PayloadHost {
            noisy: true,
            ..Default::default()
        });
        pane.set_host(host.clone());
        for index in 1..crate::activity::morning::MAX_SOURCES {
            let workspace = pane.workspace.clone();
            pane.morning_sources
                .get_mut(&pane.morning_workspace)
                .unwrap()
                .toggle(
                    &workspace,
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
            _workspace: &str,
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
            workspace: &str,
            source: &super::MorningSource,
            command: super::MorningCommand,
        ) -> Result<(), String> {
            self.commands
                .borrow_mut()
                .push((workspace.into(), source.clone(), command));
            Ok(())
        }
    }
}

#[cfg(test)]
mod reader_tests {
    use super::*;

    /// The person's own changes are never news to them, even when the writer named them differently
    /// from the device's user name: history marks them as the reader's.
    #[test]
    fn the_readers_own_changes_never_reach_the_passive_activity() {
        let own = ui::widget::history::HistoryAuthor::new("Workspace setup", 10.0, "a1b2")
            .with_relation(ui::widget::history::AuthorRelation::Reader);
        assert!(author_is_local(&own, "kai"));
        assert!(
            author_is_local(&own, ""),
            "known as the reader without a name"
        );
        let other = ui::widget::history::HistoryAuthor::new("Ada", 10.0, "c3d4");
        assert!(!author_is_local(&other, "kai"));
    }
}

/// A workspace with `workspace` 's id in another workspace, with its windows and its grants
/// rehomed: what a device holds when one id has named two workspaces (a workspace id never changes
/// its workspace).
fn same_id_in(
    workspace: &tilo_desktop_model::Workspace,
    other_workspace: tilo_desktop_model::WorkspaceId,
) -> tilo_desktop_model::Workspace {
    let mut other = tilo_desktop_model::Workspace::in_workspace(
        workspace.slot,
        other_workspace,
        workspace.name.clone(),
    );
    other.strips.clone_from(&workspace.strips);
    other.policy = workspace.policy.clone();
    other.policy.rehome(other_workspace.to_string());
    other
}
