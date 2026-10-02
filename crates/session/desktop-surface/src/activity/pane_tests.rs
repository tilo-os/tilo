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
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};
    use tilo_automerge::ApplyIncoming as _;
    use ui::widget::op_class_is_revertible;

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
                    Verb::Read,
                ),
            )
            .expect("the owner may grant in their own room");
        tree.grant_in_workspace(
            ws_id,
            &user,
            CapOverride::grant(
                Subject::app("terminal"),
                Scope::Device("camera".to_string()),
                Verb::Read,
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
                    "Export",
                    "document:brief",
                    "document",
                    "Release brief",
                )
                .with_app("org.tilo.Writer", "Writer"),
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
        assert_eq!(entry.summary, "Completed · Export · Release brief");
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
                "Export",
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
        let record = RecordId::for_file("campaign-brief.qdoc");
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
                    action,
                    record.as_str(),
                    "document",
                    "Campaign brief",
                )
                .with_app("org.tilo.Writer", "Writer")
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
                    summary: "Rewrote the launch story".to_string(),
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
        assert_eq!(task.steps[0].summary, "Rewrote the launch story");
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
        assert!(description.contains("Rewrote the launch story"));
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
                Verb::Read,
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
            op_class: OpClass::EditMeta,
            artifact_kind: ArtifactKind::File,
            timestamp: SystemTime::UNIX_EPOCH,
            summary: "Edited launch brief".into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: Some(
                ui::tilo_actions::ActionContext::for_resource(
                    "Edit",
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
            created_at: 42,
            updated_at: 42,
            audit: vec![tilo_desktop_model::WorkspaceTaskAuditEvent {
                at: 42,
                actor: "agent:scribe".into(),
                summary: "Reviewed section 2".into(),
            }],
        };

        let byline = task_assignee_label(&task);
        let audit = task_audit_line(&task.audit[0], event_time, event_time);
        let accessible = task_accessibility_description(&task, event_time);

        assert_eq!(byline, "▣ AGENT · scribe");
        assert!(audit.starts_with("scribe · Agent · Reviewed section 2"));
        assert!(accessible.contains("▣ AGENT · scribe"));
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
            pane.entries().len() >= 4,
            "bootstrap + owner + two grants + a revoke is at least four changes, got {}",
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
    }

    #[test]
    fn app_native_handoff_focuses_the_exact_version_without_marking_it_unseen() {
        let (mut tree, workspace, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let unread_before = pane.passive_unread;
        let id = HistoryEntryId::from_bytes([9; 16]);
        let resource = "signed-file-record".to_string();
        let context = ui::tilo_actions::ActionContext::for_resource(
            "Edited",
            resource.clone(),
            "files.entry",
            "Launch brief.md",
        )
        .with_app("org.tilo.Files", "Files");
        pane.open_external_entry(HistoryEntry {
            id,
            author: ui::widget::HistoryAuthor::new("Ada", 280.0, "01010101"),
            op_class: collab_types::OpClass::Compaction,
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
                Verb::Read,
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
        let owner_rows: Vec<String> = pane
            .entries()
            .iter()
            .filter(|e| e.summary.contains("Set the workspace owner"))
            .map(|e| e.summary.clone())
            .collect();
        assert!(
            owner_rows.len() > 1,
            "the fixture has several workspaces adopting an owner: {owner_rows:?}"
        );
        assert!(
            owner_rows
                .iter()
                .any(|s| s.starts_with("Research \u{00b7} ")),
            "a row must name the room it happened in: {owner_rows:?}"
        );
        let distinct: std::collections::HashSet<&String> = owner_rows.iter().collect();
        assert_eq!(
            distinct.len(),
            owner_rows.len(),
            "no two rows may read identically: {owner_rows:?}"
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
        let scoped_flushes = pane
            .entries()
            .iter()
            .filter(|e| e.summary.starts_with("Recorded "))
            .count();
        assert!(scoped_flushes > 0, "the fixture flushed at least once");
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        let widened_flushes = pane
            .entries()
            .iter()
            .filter(|e| e.summary.starts_with("Recorded "))
            .count();
        assert_eq!(
            scoped_flushes, widened_flushes,
            "a flush is unattributed, so widening must not add or drop one"
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

        let week_ago = SystemTime::now() - Duration::from_secs(604_800);
        let week_ago_secs = i64::try_from(
            week_ago
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
            9001,
            "Last week's window".to_string(),
            b"{}".to_vec(),
        ));
        tilo_resource::history::commit_stamped_at(&mut tree.doc, week_ago_secs)
            .expect("the append left a transaction to stamp");

        tree.grant_in_workspace(
            ws_id,
            &user,
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath("/home/quinten/today".to_string()),
                Verb::Read,
            ),
        )
        .expect("owner may grant");

        let mut pane = pane_over(&mut tree);
        let all = pane.visible_len();
        assert!(all >= 3, "expected the full history, got {all}");

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
    fn action_type_chips_narrow_by_op_class() {
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
                pane.visible_entry(i).unwrap().op_class,
                OpClass::Govern,
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
                !op_class_is_revertible(e.op_class),
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
        assert_eq!(grant.op_class, OpClass::Govern);
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
        for i in 0..240u64 {
            assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
                50_000 + i,
                format!("Window {i}"),
                b"{}".to_vec(),
            ));
            tree.commit_room_change();
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
                .any(|e| e.summary.contains("Window 239")),
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

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        }
    }

    fn key_press(logical_key: winit::keyboard::Key) -> WindowEvent {
        // SAFETY: as in `ui`'s own widget tests — every pub field of `KeyEvent`
        // tolerates the all-zero bit pattern, and winit 0.30 keeps
        // `KeyEventExtra` crate-private so a struct literal is unavailable.
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = winit::event::ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
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
            .find(|i| pane.timeline().group_at_row(*i).is_some())
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

    /// The header band equals the authored 76 px at 100 % and grows with the
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
                active.fill, theme.tokens.colors.accent[5],
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
                        action,
                        format!("test:{group_id}"),
                        "document",
                        group_label,
                    )
                    .with_app("org.tilo.Writer", "Writer")
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
            op_class: OpClass::Splice,
            artifact_kind: ArtifactKind::File,
            timestamp,
            summary: "Edited assumptions".into(),
            text_delta: Some((4, 1)),
            diff_payload: None,
            redaction: None,
            context: Some(
                ActionContext::for_resource("Edit", "finance:q2", "sheet", "Q2 finance report")
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
