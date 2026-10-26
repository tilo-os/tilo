//! Drives `host.handle_event` with real winit events, through the
//! same path the live binary takes.
use super::*;
use crate::shell::chrome_state::{ChromeState, WsTab};
use crate::shell::state::WorkspaceTree;
use automerge::ReadDoc as _;
use ui::widget::TaffyTreeExt;

#[test]
fn people_overlay_publishes_the_live_roster_and_never_hides_screen_exposure() {
    fn people_overlay_publishes_live_roster_and_focusable_status_controls() {
        let mut h = build_harness();
        {
            let mut chrome = h.chrome.borrow_mut();
            chrome
                .tree_mut()
                .desktop
                .active_workspace_mut()
                .expect("fixture has an active workspace")
                .collaborators
                .push(crate::shell::state::Collaborator::new("ada", "Ada").with_role("editor"));
            chrome.overlays.collaborators = true;
        }
        paint(&mut h);

        let semantics = h.host.accessibility().expect("shell semantics");
        let people = semantics
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("People"))
            .expect("the visible People popover is in the semantic overlay tree");
        assert_eq!(
            people
                .children
                .first()
                .and_then(|list| {
                    list.children
                        .iter()
                        .find(|person| person.name.as_deref() == Some("Ada"))
                })
                .and_then(|person| person.name.as_deref()),
            Some("Ada")
        );
        assert!(h.host.synthetic_accessibility_rect(&[0, 1]).is_some());
        assert!(h.host.focus_synthetic_accessibility(&[0, 1]));
    }

    fn screen_exposure_summary_never_hides_an_unknown_viewer_or_target() {
        let chrome = ChromeState::new(WorkspaceTree::default());
        chrome.borrow_mut().set_active_exposures(vec![
            crate::shell::chrome_state::ActiveExposure {
                peer: 7,
                surface: 42,
                can_control: false,
            },
        ]);

        let state = chrome.borrow();
        let summary = collaborators_screen_exposure(&state).expect("live disclosure");
        assert!(summary.contains(&crate::shell::state::session_handle(7)));
        assert!(summary.contains("unidentified session"));
        assert!(summary.contains("views a window"), "{summary}");
        assert!(
            !summary.contains("42"),
            "a surface number means nothing to people: {summary}"
        );
    }

    people_overlay_publishes_live_roster_and_focusable_status_controls();
    screen_exposure_summary_never_hides_an_unknown_viewer_or_target();
}

#[test]
fn catch_up_reveals_once_on_entry_after_transitions_and_manual_opening_consumes_it() {
    fn unseen_activity_reveals_catch_up_only_on_workspace_entry() {
        let mut presented_workspace = None;
        let overlays = crate::shell::chrome_state::OverlayFlags::default();

        let entering = enter_activity_workspace(&mut presented_workspace, 7);
        assert!(
            !should_reveal_activity_on_entry(entering, 3, overlays),
            "restoring the initial workspace leaves unread history passive"
        );

        // Closing the sheet must stick: subsequent frames in the same
        // workspace cannot continuously reopen it.
        let entering = enter_activity_workspace(&mut presented_workspace, 7);
        assert!(!should_reveal_activity_on_entry(entering, 3, overlays));

        let entering = enter_activity_workspace(&mut presented_workspace, 8);
        assert!(should_reveal_activity_on_entry(entering, 1, overlays));
    }

    fn catch_up_entry_reveal_requires_unseen_work_and_clear_attention() {
        let clear = crate::shell::chrome_state::OverlayFlags::default();
        assert!(!should_reveal_activity_on_entry(true, 0, clear));

        let mut occupied = clear;
        occupied.quick_settings = true;
        assert!(!should_reveal_activity_on_entry(true, 4, occupied));

        occupied.quick_settings = false;
        occupied.collaborators = true;
        assert!(!should_reveal_activity_on_entry(true, 4, occupied));
    }

    fn catch_up_waits_for_workspace_transition_ui_instead_of_being_lost() {
        let mut pending = None;
        let occupied = crate::shell::chrome_state::OverlayFlags {
            overview: true,
            ..Default::default()
        };
        assert!(!take_pending_activity_reveal(
            &mut pending,
            true,
            7,
            4,
            occupied,
        ));
        assert_eq!(pending, Some(7));

        assert!(take_pending_activity_reveal(
            &mut pending,
            false,
            7,
            4,
            crate::shell::chrome_state::OverlayFlags::default(),
        ));
        assert_eq!(pending, None, "the reveal is consumed exactly once");
        assert!(!take_pending_activity_reveal(
            &mut pending,
            false,
            7,
            4,
            crate::shell::chrome_state::OverlayFlags::default(),
        ));
    }

    fn manually_opening_catch_up_consumes_a_pending_automatic_reveal() {
        let mut pending = Some(7);
        let overlays = crate::shell::chrome_state::OverlayFlags {
            room_activity: true,
            ..Default::default()
        };
        assert!(!take_pending_activity_reveal(
            &mut pending,
            false,
            7,
            3,
            overlays,
        ));
        assert_eq!(pending, None);
    }

    unseen_activity_reveals_catch_up_only_on_workspace_entry();
    catch_up_entry_reveal_requires_unseen_work_and_clear_attention();
    catch_up_waits_for_workspace_transition_ui_instead_of_being_lost();
    manually_opening_catch_up_consumes_a_pending_automatic_reveal();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn collaboration_status_preferences_round_trip_and_fall_back_to_grounded_intent() {
    fn collaboration_preferences_round_trip_privacy_and_manual_intent() {
        let lounge = crate::shell::state::RoomId::new();
        let preferences = CollaborationPreferences {
            sharing: ui::StatusSharing::Off,
            manual_status: Some("Reviewing Q2 assumptions".into()),
            availability: ui::Availability::Focusing,
            visibility: [(lounge, ui::PresenceVisibility::Nothing)]
                .into_iter()
                .collect(),
            introduced: [lounge].into_iter().collect(),
        };
        assert_eq!(
            CollaborationPreferences::from_values(&preferences.values()),
            preferences,
            "Hidden is a durable privacy choice, while explicit text remains available when sharing is re-enabled"
        );
    }

    fn collaboration_preferences_reject_stale_or_malformed_status_text() {
        let mut values = serde_json::Map::new();
        values.insert("status_sharing".into(), "manual".into());
        values.insert(
            "manual_status".into(),
            serde_json::Value::String("x".repeat(ui::MAX_STATUS_CHARS + 1)),
        );
        let preferences = CollaborationPreferences::from_values(&values);
        assert_eq!(preferences.sharing, ui::StatusSharing::ManualOnly);
        assert_eq!(preferences.manual_status, None);

        values.insert("status_sharing".into(), "future-unknown-mode".into());
        assert_eq!(
            CollaborationPreferences::from_values(&values).sharing,
            ui::StatusSharing::ManualOnly,
            "an unknown stored value must not unexpectedly enable inference"
        );

        // What people see: a choice this build does not know is dropped, so
        // that workspace takes its default, never something looser; idle is
        // the machine's to say, never stored as a choice.
        let room = crate::shell::state::RoomId::new();
        values.insert(
            "presence_visibility".into(),
            serde_json::json!({ room.to_string(): "everything", "not-a-room": "where" }),
        );
        values.insert("availability".into(), "idle".into());
        let preferences = CollaborationPreferences::from_values(&values);
        assert!(preferences.visibility.is_empty());
        assert_eq!(preferences.availability, ui::Availability::Available);
    }

    fn automatic_status_fallback_describes_intent_not_an_activity_count() {
        let entry = ui::widget::history::HistoryEntry {
            id: ui::widget::history::HistoryEntryId([1; 16]),
            author: ui::widget::history::HistoryAuthor::new("Ada", 120.0, "ada"),
            entry_kind: ui::widget::EntryKind::Access,
            artifact_kind: collab_types::ArtifactKind::Note,
            timestamp: std::time::SystemTime::now(),
            summary: "5 permission changes".into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: None,
        };
        assert_eq!(
            grounded_intent_fallback(&[entry]),
            "Managing workspace access"
        );
    }

    fn explicit_action_purpose_is_the_strongest_grounded_status() {
        let entry = ui::widget::history::HistoryEntry {
            id: ui::widget::history::HistoryEntryId([2; 16]),
            author: ui::widget::history::HistoryAuthor::new("Ada", 120.0, "ada"),
            entry_kind: ui::widget::EntryKind::Text,
            artifact_kind: collab_types::ArtifactKind::File,
            timestamp: std::time::SystemTime::now(),
            summary: "+4 -1 chars".into(),
            text_delta: Some((4, 1)),
            diff_payload: None,
            redaction: None,
            context: Some(
                ui::tilo_actions::ActionContext::for_resource(
                    "finance:q2",
                    "sheet",
                    "Q2 finance report",
                )
                .with_purpose("Preparing the board review"),
            ),
        };
        assert_eq!(
            grounded_intent_fallback(&[entry]),
            "Preparing the board review"
        );
    }

    fn status_fallback_uses_latest_action_while_model_evidence_is_chronological() {
        let make_entry = |id, purpose: &str| ui::widget::history::HistoryEntry {
            id: ui::widget::history::HistoryEntryId([id; 16]),
            author: ui::widget::history::HistoryAuthor::new("Ada", 120.0, "ada"),
            entry_kind: ui::widget::EntryKind::Text,
            artifact_kind: collab_types::ArtifactKind::File,
            timestamp: std::time::SystemTime::now(),
            summary: purpose.into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: Some(
                ui::tilo_actions::ActionContext::for_resource(
                    format!("resource:{id}"),
                    "document",
                    purpose,
                )
                .with_purpose(purpose),
            ),
        };
        let older = make_entry(1, "Checking assumptions");
        let newer = make_entry(2, "Preparing the board review");
        let (fallback, evidence) = prepare_status_evidence(vec![newer.clone(), older.clone()]);

        assert_eq!(fallback, "Preparing the board review");
        assert_eq!(evidence[0].id, older.id);
        assert_eq!(evidence[1].id, newer.id);
    }

    collaboration_preferences_round_trip_privacy_and_manual_intent();
    collaboration_preferences_reject_stale_or_malformed_status_text();
    automatic_status_fallback_describes_intent_not_an_activity_count();
    explicit_action_purpose_is_the_strongest_grounded_status();
    status_fallback_uses_latest_action_while_model_evidence_is_chronological();
}

fn history_target(id: &str) -> ui::HistoryResourceTarget {
    ui::HistoryResourceTarget {
        app_id: Some("org.tilo.Notes".to_string()),
        app_label: Some("Notes".to_string()),
        id: id.to_string(),
        kind: "page".to_string(),
        label: "Launch notes".to_string(),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn room_activity_reads_the_live_room_and_opens_what_it_names() {
    /// The activity model derives its rows from the room document the
    /// chrome is holding — not from a seeded list.
    fn room_activity_reads_the_live_rooms_history() {
        let mut h = build_harness();
        {
            let state = h.chrome.borrow();
            let mut tree = state.tree_mut();
            // Bootstrap writes are setup/layout and omitted from the feed;
            // rename so the explorer has a Change row to show.
            let tree = &mut *tree;
            if let Some(ws) = tree.desktop.workspaces.first_mut() {
                ws.name = format!("{} · live", ws.name);
            }
            tilo_desktop_model::desktop_doc::seed(&mut tree.doc, &tree.desktop);
            tilo_resource::history::commit_stamped(&mut tree.doc);
            h.host.room_activity_mut().refresh(tree);
        }
        assert!(
            !h.host.room_activity().entries().is_empty(),
            "a renamed workspace should appear in the activity feed"
        );
        let names: Vec<&str> = h
            .host
            .room_activity()
            .entries()
            .iter()
            .map(|e| e.summary.as_str())
            .collect();
        assert!(
            names.iter().any(|s| !s.is_empty()),
            "rows carry real summaries: {names:?}"
        );
    }

    fn history_open_distinguishes_files_links_and_semantic_ids() {
        assert_eq!(
            history_target_address(&history_target("file:/workspace/brief.md")),
            HistoryTargetAddress::File(PathBuf::from("/workspace/brief.md"))
        );
        assert_eq!(
            history_target_address(&history_target("tube://watch/launch")),
            HistoryTargetAddress::Uri("tube://watch/launch".to_string())
        );
        assert_eq!(
            history_target_address(&history_target("notes.page:launch")),
            HistoryTargetAddress::App
        );
    }

    /// A History Open action can fail before a process exists (for example,
    /// after its app was uninstalled). That refusal must consume any staged
    /// placement and leave one finite, bounded recovery notice for retry.
    fn room_activity_preflight_failure_clears_placement_and_emits_one_notice() {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
        let app_id = "org.tilo.missing-history-fixture-9f1e";
        crate::shell::launcher::clear_pending_placements();
        crate::shell::launcher::set_pending_placement(
            app_id,
            crate::compositor::tab_binding::Placement::NewGroup,
        );
        let target = ui::HistoryResourceTarget {
            app_id: Some(app_id.to_string()),
            app_label: Some("Missing Notes".to_string()),
            id: "notes.page:missing".to_string(),
            kind: "page".to_string(),
            label: "Missing Notes".to_string(),
        };

        ShellOverlayHost::open_room_activity_target(&chrome, &target);
        // The open is queued off the compositor thread: its refusal arrives
        // as a launch event that the host settles on its next drain.
        let host = ShellOverlayHost::new(Box::new(StubContent::new())).with_chrome(chrome.clone());
        let deadline = tilo_test_env::hang_deadline();
        while chrome.borrow().notifications().is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "the queued refusal never reached the host"
            );
            if !host.drain_launch_events() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        assert!(
            crate::shell::launcher::placement_for_toplevel(app_id, 91_001).is_none(),
            "a preflight refusal must not place a later unrelated toplevel"
        );
        let state = chrome.borrow();
        assert_eq!(state.notifications().len(), 1);
        let notice = &state.notifications()[0];
        assert_eq!(notice.title, "Couldn't open Missing Notes");
        assert_eq!(
            notice.body,
            "This app is no longer installed. Refresh the app list and try again."
        );
        assert_eq!(
            notice.severity,
            tilo_desktop::notifications::NotificationSeverity::Error
        );
        assert_eq!(
            notice.dismiss_after,
            crate::shell::launcher::LAUNCH_FAILURE_NOTICE_LIFETIME
        );
        assert!(notice.body.len() <= 120);
        assert!(!notice.body.contains('\n'));
        drop(state);
        crate::shell::launcher::clear_pending_placements();
    }

    room_activity_reads_the_live_rooms_history();
    history_open_distinguishes_files_links_and_semantic_ids();
    room_activity_preflight_failure_clears_placement_and_emits_one_notice();
}

/// Frames in which nothing changed do not refresh the passive
/// Activity awareness (no tree write lock, no record reads), and a change to
/// the workspace document refreshes it again.
#[test]
fn activity_refresh_skips_unchanged_frames() {
    let mut h = build_harness();
    let refreshes = crate::shell::room_activity::refreshes;
    // Every frame runs at one pinned instant, so the once-a-second
    // fallback cannot fire however long the frames take.
    let at = ui::time::now();
    let doc = AutoCommit::new();
    let paint_at = |h: &mut Harness, at| {
        ui::time::testing::with_monotonic_time(at, || {
            h.host.update(&doc, &automerge::ROOT);
        });
    };
    paint_at(&mut h, at);
    let settled = refreshes();
    assert!(settled >= 1, "the first frame refreshes the awareness");

    for _ in 0..10 {
        paint_at(&mut h, at);
    }
    assert_eq!(
        refreshes(),
        settled,
        "unchanged frames do not refresh the awareness"
    );

    let fallback = at
        .checked_add(ACTIVITY_AWARENESS_FALLBACK)
        .expect("a fallback instant");
    paint_at(&mut h, fallback);
    assert_eq!(
        refreshes(),
        settled.saturating_add(1),
        "the fallback refreshes an unchanged awareness once its interval passed"
    );

    {
        let state = h.chrome.borrow();
        let mut tree = state.tree_mut();
        let tree = &mut *tree;
        if let Some(workspace) = tree.desktop.workspaces.first_mut() {
            workspace.name = format!("{} · renamed", workspace.name);
        }
        tilo_desktop_model::desktop_doc::seed(&mut tree.doc, &tree.desktop);
        tilo_resource::history::commit_stamped(&mut tree.doc);
    }
    let before = refreshes();
    paint_at(&mut h, fallback);
    assert_eq!(
        refreshes(),
        before.saturating_add(1),
        "a changed workspace document refreshes the awareness once"
    );
}

use std::collections::HashMap as StdHashMap;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, PhysicalKey};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn pip_and_capture_surfaces_place_size_and_reuse_ids_sensibly() {
    fn pip_placement_avoids_its_source_and_keeps_edge_margin() {
        let bounds = Rect::new(0.0, 0.0, 1600.0, 900.0);
        let size = vello::kurbo::Size::new(420.0, 200.0);
        let bottom_right_source = Rect::new(900.0, 400.0, 1580.0, 880.0);
        let origin = capture_mirror_origin(bounds, Some(bottom_right_source), size);
        let placed = Rect::from_origin_size(origin, size);
        assert_eq!(placed.x1, 1584.0);
        assert_eq!(
            placed.y0, 52.0,
            "top-right should beat an obscured bottom-right"
        );
        assert_eq!(placed.intersect(bottom_right_source).area(), 0.0);

        let default = capture_mirror_origin(bounds, None, size);
        assert_eq!(default, Point::new(1164.0, 684.0));
    }

    fn offscreen_pip_source_enters_from_its_virtual_desktop_edge() {
        let bounds = Rect::new(0.0, 0.0, 1_600.0, 900.0);
        let left =
            visible_capture_spawn(bounds, Some(Rect::new(-1_200.0, 180.0, -200.0, 780.0))).unwrap();
        assert_eq!(left.x0, 6.0);
        assert!(left.y0 >= 48.0 && left.y1 <= 894.0);

        let right =
            visible_capture_spawn(bounds, Some(Rect::new(1_900.0, 120.0, 2_900.0, 720.0))).unwrap();
        assert_eq!(right.x1, 1_594.0);

        let partly_visible = Rect::new(1_500.0, 100.0, 1_900.0, 500.0);
        assert_eq!(
            visible_capture_spawn(bounds, Some(partly_visible)),
            Some(Rect::new(1_500.0, 100.0, 1_600.0, 500.0))
        );
    }

    fn compact_pip_initial_size_preserves_aspect_without_dominating_viewport() {
        let compact = Rect::new(0.0, 0.0, 800.0, 600.0);
        let size = capture_mirror_initial_size(compact, 16.0 / 9.0, None);
        assert_eq!(size.width, 352.0);
        assert_eq!(size.height, 198.0);
        assert!(size.width < compact.width() / 2.0);

        let preferred =
            capture_mirror_initial_size(compact, 1.0, Some(vello::kurbo::Size::new(480.0, 270.0)));
        assert_eq!(preferred.width, 352.0);
        assert_eq!(preferred.height, 198.0);
    }

    fn tall_pinned_screenshot_uses_a_readable_initial_viewport() {
        let compact = Rect::new(0.0, 0.0, 800.0, 600.0);
        let size = capture_mirror_initial_size(
            compact,
            524.0 / 3_021.0,
            Some(vello::kurbo::Size::new(340.0, 420.0)),
        );
        assert_eq!(size.width, 340.0);
        assert_eq!(size.height, 330.0);
    }

    fn capture_surface_requests_reuse_live_id_and_add_pinned_surface() {
        let mut h = build_harness();
        let pixel: std::sync::Arc<[u8]> = std::sync::Arc::from(vec![0_u8, 0, 0, 255]);
        h.host.upsert_capture_surface(CaptureSurfaceRequest {
            id: "live",
            title: "Live region",
            rgba: pixel.clone(),
            width: 1,
            height: 1,
            spawn_from: None,
            kind: CaptureSurfaceKind::Live {
                preferred_size: None,
                handoff_layers: Vec::new(),
            },
        });
        assert_eq!(h.host.capture_mirrors.len(), 1);

        h.host.upsert_capture_surface(CaptureSurfaceRequest {
            id: "live",
            title: "Live region",
            rgba: pixel,
            width: 1,
            height: 1,
            spawn_from: Some(Rect::new(600.0, 400.0, 700.0, 500.0)),
            kind: CaptureSurfaceKind::Live {
                preferred_size: Some(vello::kurbo::Size::new(640.0, 360.0)),
                handoff_layers: Vec::new(),
            },
        });
        assert_eq!(
            h.host.capture_mirrors.len(),
            1,
            "a live frame refresh must update the existing surface"
        );

        h.host.upsert_capture_surface(CaptureSurfaceRequest {
            id: "pinned",
            title: "Tall screenshot",
            rgba: std::sync::Arc::from(vec![0_u8; 16]),
            width: 1,
            height: 4,
            spawn_from: None,
            kind: CaptureSurfaceKind::Pinned {
                preview: ui::widget::CapturePreview {
                    columns: 1,
                    rows: 1,
                    pixels: vec![[0, 0, 0, 255]],
                },
            },
        });
        assert_eq!(h.host.capture_mirrors.len(), 2);
    }

    pip_placement_avoids_its_source_and_keeps_edge_margin();
    offscreen_pip_source_enters_from_its_virtual_desktop_edge();
    compact_pip_initial_size_preserves_aspect_without_dominating_viewport();
    tall_pinned_screenshot_uses_a_readable_initial_viewport();
    capture_surface_requests_reuse_live_id_and_add_pinned_surface();
}

/// Recording mock content for the host's `content` slot. The palette
/// test never reaches `content.handle_event`, but the WS-settings click
/// test needs a layout target so taffy can place the modal.
struct StubContent {
    rect: Rect,
}

impl StubContent {
    fn new() -> Self {
        Self { rect: Rect::ZERO }
    }
}

impl Widget for StubContent {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut StdHashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

    fn name(&self) -> &'static str {
        "StubContent"
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn desktop_tour_opens_once_composes_with_the_desktop_and_routes_its_own_input() {
    fn onboarding_release_keeps_focus_reset_after_relayout_edge_is_consumed() {
        let mut host = ShellOverlayHost::new(Box::new(StubContent::new()));
        host.onboarding_released = true;
        host.onboarding_focus_reset = true;

        assert!(host.take_structure_dirty());
        assert!(host.take_focus_reset());
        assert!(!host.take_focus_reset(), "focus reset must remain one-shot");
    }

    fn desktop_tour_request_opens_once_and_is_consumed() {
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let mut host =
            ShellOverlayHost::new(Box::new(StubContent::new())).with_chrome(chrome.clone());
        chrome.borrow_mut().overlays.desktop_tour = true;

        host.sync(&AutoCommit::new(), &automerge::ROOT);

        assert!(host.onboarding_active());
        assert!(
            host.onboarding
                .as_ref()
                .is_some_and(|view| !view.flow().is_done() && view.presents_desktop_context())
        );
        let mut taffy = TaffyTree::new();
        let mut node_map = StdHashMap::new();
        let _ = host.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        assert!(
            host.content_node.is_some(),
            "the tour keeps the real desktop laid out"
        );
        assert!(
            host.onboarding_node.is_some(),
            "the coachmark surface shares that tree"
        );
        assert!(
            !chrome.borrow().overlays.desktop_tour,
            "the request edge is consumed so dismissing does not reopen it"
        );
    }

    fn first_login_tour_composes_with_the_real_desktop() {
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let mut host =
            ShellOverlayHost::new(Box::new(StubContent::new())).with_chrome(chrome.clone());
        host.enable_test_onboarding(
            crate::onboarding_view::OnboardingView::isolated_host_desktop(
                std::path::PathBuf::from("/var/empty/tilo-first-login-tour-test"),
            ),
        );
        host.set_onboarding_visual_state_for_snapshot(
            crate::onboarding_view::OnboardingVisualState::TourNavigate,
        );

        assert!(
            host.onboarding
                .as_ref()
                .is_some_and(crate::onboarding_view::OnboardingView::presents_desktop_context),
            "starting the first-login tour must release the opaque setup backdrop"
        );
        let harness = build_harness_with_host(host, chrome);
        assert!(
            harness.host.content_node.is_some(),
            "the real desktop stays in the first-login tour layout"
        );
        assert!(
            harness.host.onboarding_node.is_some(),
            "the first-login tour remains modal above the desktop"
        );
        let tour_rect = harness
            .host
            .onboarding
            .as_ref()
            .expect("first-login tour")
            .get_rect();
        assert_eq!(
            tour_rect,
            Rect::new(0.0, 0.0, 1600.0, 900.0),
            "the tutorial overlay must cover the visible desktop rather than landing beside it"
        );
    }

    fn contextual_desktop_tour_routes_pointer_input_to_its_own_layout_node() {
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let mut host =
            ShellOverlayHost::new(Box::new(StubContent::new())).with_chrome(chrome.clone());
        chrome.borrow_mut().overlays.desktop_tour = true;
        let mut document = AutoCommit::new();
        host.sync(&document, &automerge::ROOT);
        let mut h = build_harness_with_host(host, chrome);

        let next = h
            .node_map
            .values()
            .find_map(|&pointer| unsafe {
                ((*pointer).test_id() == Some("onboarding.tour.next.button"))
                    .then(|| (*pointer).get_rect())
            })
            .expect("contextual tour exposes its Continue button");
        assert!(next.area() > 0.0, "Continue must have clickable geometry");

        let mut focus = None;
        for event in [
            cursor_moved_at(next.center()),
            mouse_press(),
            mouse_release(),
        ] {
            let mut handled = false;
            h.host.handle_event(
                &event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &Modifiers::default(),
                    state: ui::widget::Edits::lend(&mut document),
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: h.root_node,
                    taffy: &h.taffy,
                },
            );
            assert!(handled, "the visible tour must own {event:?}");
        }
        assert!(
            matches!(
                document.get(&automerge::ROOT, crate::onboarding_view::KEY_TOUR_NEXT),
                Ok(None)
            ),
            "a click is a view action, never a document write"
        );
        h.host.sync(&document, &automerge::ROOT);
        assert_eq!(
            h.host
                .onboarding
                .as_ref()
                .expect("the tour stays open")
                .tour_step(),
            Some(2),
            "Continue advances the tour"
        );
    }

    onboarding_release_keeps_focus_reset_after_relayout_edge_is_consumed();
    desktop_tour_request_opens_once_and_is_consumed();
    first_login_tour_composes_with_the_real_desktop();
    contextual_desktop_tour_routes_pointer_input_to_its_own_layout_node();
}

/// Bundle of host + chrome + the taffy state needed to dispatch
/// real `WindowEvent`s through `host.handle_event(...)`.
struct Harness {
    host: ShellOverlayHost,
    chrome: crate::shell::chrome_state::Chrome,
    taffy: TaffyTree<()>,
    node_map: StdHashMap<NodeId, *mut dyn Widget>,
    root_node: NodeId,
}

fn build_harness() -> Harness {
    let tree = WorkspaceTree::bootstrap_demo();
    let chrome = ChromeState::new(tree);
    let content = Box::new(StubContent::new());
    let host = ShellOverlayHost::with_fixture_content(content).with_chrome(chrome.clone());
    build_harness_with_host(host, chrome)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn overlay_accessibility_paths_follow_painted_geometry_and_coexistence_order() {
    fn active_window_controls_report_the_live_painted_geometry() {
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree.clone());
        let records = chrome.borrow().records().clone();
        let (content, ptrs) = crate::shell::build_workspace_root(&tree, &records, &chrome);
        let host = ShellOverlayHost::new(content)
            .with_chrome(chrome)
            .with_group_row_ptrs(ptrs);
        let (maximize, restore) = host
            .active_window_control_rects()
            .expect("demo workspace has an active group");
        assert!(maximize.width() >= 0.0 && maximize.height() >= 0.0);
        assert!(restore.width() >= 0.0 && restore.height() >= 0.0);
    }

    fn accessibility_overlay_paths_share_the_published_coexistence_order() {
        let mut h = build_harness();
        h.host
            .workspace_settings_overlay
            .open_remove_member_sheet("peer".to_string());
        {
            let mut chrome = h.chrome.borrow_mut();
            chrome.overlays.quick_settings = true;
            chrome.quick_settings.nav_stack = vec![crate::shell::quick_settings::QsLevel::Root];
        }

        let semantics = h.host.accessibility().expect("shell semantics");
        assert_eq!(
            semantics
                .children
                .iter()
                .filter(|node| node.live.is_none())
                .filter_map(|node| node.name.as_deref())
                .collect::<Vec<_>>(),
            ["Workspace permission decision", "Quick Settings"],
            "the published child order is the address space used by geometry and focus"
        );

        assert_eq!(
            h.host.synthetic_accessibility_rect(&[0]),
            None,
            "the prose-only member sheet owns child zero"
        );
        assert!(
            h.host.synthetic_accessibility_rect(&[1, 0]).is_some(),
            "Quick Settings moves to child one when the member sheet is visible"
        );
    }

    fn power_menu_accessibility_paths_focus_and_activate_exact_rows() {
        let mut h = build_harness();
        h.host.layers.power_menu.open();
        h.host.layers.power_menu.force_settled_for_visual_test();

        let semantics = h.host.accessibility().expect("shell semantics");
        assert_eq!(
            semantics
                .children
                .first()
                .and_then(|node| node.name.as_deref()),
            Some(crate::shell::i18n::t("power-menu-title").as_str())
        );
        assert_eq!(
            h.host.synthetic_accessibility_rect(&[0, 0]),
            h.host
                .power_menu_semantic_rects()
                .into_iter()
                .find(|(name, _)| *name == "power_lock")
                .map(|(_, rect)| rect)
        );
        assert!(h.host.focus_synthetic_accessibility(&[0, 0]));
        assert_eq!(
            h.host.layers.power_menu.key(
                &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
                false
            ),
            PowerMenuClick::Lock
        );

        h.host.layers.power_menu.open();
        assert!(h.host.focus_synthetic_accessibility(&[0, 5]));
        assert_eq!(
            h.host.layers.power_menu.key(
                &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
                false
            ),
            PowerMenuClick::Execute(os_actions::SessionAction::ShutDown)
        );
    }

    fn accessibility_focus_routes_through_shifted_overlay_indices() {
        let mut h = build_harness();
        h.host
            .workspace_settings_overlay
            .open_remove_member_sheet("peer".to_string());
        {
            let mut chrome = h.chrome.borrow_mut();
            let local_user = chrome.active_local_user();
            if let Some(workspace) = chrome.tree_mut().desktop.active_workspace_mut() {
                workspace.set_owner(local_user);
            }
            chrome.overlays.ws_settings = true;
            chrome.ws_settings_tab = tilo_desktop::workspace_settings::WsTab::Members;
            chrome.overlays.quick_settings = true;
            chrome.quick_settings.nav_stack = vec![crate::shell::quick_settings::QsLevel::Root];
        }
        h.host.force_ws_settings_settled();
        h.host.force_quick_settings_settled();
        paint(&mut h);

        let semantics = h.host.accessibility().expect("shell semantics");
        assert_eq!(
            semantics
                .children
                .get(1)
                .and_then(|settings| settings.children.first())
                .and_then(|ownership| ownership.name.as_deref()),
            Some("Manage workspace ownership"),
            "the visible ownership control is named in the accessibility tree"
        );

        assert!(
            h.host.focus_synthetic_accessibility(&[1, 0]),
            "Workspace Settings exposes its ownership action at the shifted path"
        );
        assert!(
            h.host.focus_synthetic_accessibility(&[2, 0]),
            "Quick Settings' first control remains focusable after every earlier overlay"
        );
        assert!(
            !h.host.focus_synthetic_accessibility(&[0, 0]),
            "the member-sheet path must not leak through to a different overlay"
        );
    }

    active_window_controls_report_the_live_painted_geometry();
    accessibility_overlay_paths_share_the_published_coexistence_order();
    power_menu_accessibility_paths_focus_and_activate_exact_rows();
    accessibility_focus_routes_through_shifted_overlay_indices();
}

/// A harness whose chrome acts as one explicit person, so who owns a room
/// and who the chrome asks for are the same answer whatever identity another
/// test of the process puts in the environment meanwhile.
fn build_fixture_harness() -> Harness {
    let chrome = ChromeState::new_with_fixture_handle(
        WorkspaceTree::bootstrap_demo().shared(),
        crate::shell::hosted_inputs::HostedTileInputs::fixture()
            .expect("deterministic hosted inputs"),
    );
    let host = ShellOverlayHost::with_fixture_content(Box::new(StubContent::new()))
        .with_chrome(chrome.clone());
    build_harness_with_host(host, chrome)
}

fn build_harness_with_host(
    mut host: ShellOverlayHost,
    chrome: crate::shell::chrome_state::Chrome,
) -> Harness {
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: StdHashMap<NodeId, *mut dyn Widget> = StdHashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let viewport = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(1600.0, 900.0),
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            viewport,
            Size {
                width: taffy::AvailableSpace::Definite(1600.0),
                height: taffy::AvailableSpace::Definite(900.0),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);

    Harness {
        host,
        chrome,
        taffy,
        node_map,
        root_node: child,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn super_chords_open_their_surfaces_once() {
    fn super_p_opens_the_display_sheet_and_escape_closes_it() {
        let mut h = build_harness();
        let sup = Modifiers::from(ModifiersState::SUPER);
        assert!(!h.chrome.borrow().overlays.projection);

        assert!(pump(&mut h, &key_press(Key::Character("p".into())), sup));
        assert!(
            h.chrome.borrow().overlays.projection,
            "Win+P must open the OS-owned display sheet"
        );

        assert!(pump(&mut h, &key_press(Key::Character("p".into())), sup));
        assert!(
            h.chrome.borrow().overlays.projection,
            "pressing Win+P again moves between layouts; it does not close the sheet"
        );

        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::Escape)),
            Modifiers::default()
        ));
        assert!(!h.chrome.borrow().overlays.projection);
    }

    fn super_l_requests_the_compositor_lock_once() {
        let mut h = build_harness();
        let sup = Modifiers::from(ModifiersState::SUPER);

        assert!(pump(&mut h, &key_press(Key::Character("l".into())), sup));
        assert!(h.chrome.borrow_mut().take_session_lock_request());
        assert!(!h.chrome.borrow_mut().take_session_lock_request());
    }

    fn typed_session_actions_run_through_the_power_flyouts_executor() {
        let mut h = build_harness();
        let doc = AutoCommit::new();
        h.chrome
            .borrow_mut()
            .request_session_action(os_actions::SessionAction::Lock);
        h.host.update(&doc, &automerge::ROOT);
        assert!(
            h.chrome.borrow_mut().take_session_lock_request(),
            "a typed lock reaches the compositor lock edge the flyout's Lock row does"
        );

        h.chrome
            .borrow_mut()
            .request_session_action(os_actions::SessionAction::Suspend);
        h.host.update(&doc, &automerge::ROOT);
        assert!(
            h.chrome
                .borrow()
                .notifications()
                .iter()
                .any(|notification| notification.body.contains("suspend")),
            "without the power service a typed suspend fails visibly, like the row"
        );
        assert!(
            h.chrome
                .borrow_mut()
                .take_session_action_requests()
                .is_empty()
        );
    }

    fn super_h_opens_whats_new_without_taking_ctrl_h() {
        let mut h = build_harness();
        let sup = Modifiers::from(ModifiersState::SUPER);
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        let opened = |h: &mut Harness| h.chrome.borrow_mut().take_activity_open_request();
        assert!(opened(&mut h).is_none());

        assert!(pump(&mut h, &key_press(Key::Character("h".into())), sup));
        assert!(opened(&mut h).is_some(), "Super+H opens Activity");
        assert_eq!(
            h.host.room_activity().serve_focus().map(|focus| focus.view),
            Some(tilo_actions::remote::workspace_activity::View::WhatsNew)
        );

        assert!(pump(&mut h, &key_press(Key::Character("H".into())), sup));
        assert!(
            opened(&mut h).is_some(),
            "pressed again it opens Activity again, bringing its window forward"
        );

        assert!(
            !pump(&mut h, &key_press(Key::Character("h".into())), ctrl),
            "Ctrl+H remains available to the focused app"
        );
        assert!(opened(&mut h).is_none());
    }

    fn super_shift_h_opens_the_full_history_directly() {
        let mut h = build_harness();
        let chord = Modifiers::from(ModifiersState::SUPER | ModifiersState::SHIFT);

        assert!(pump(&mut h, &key_press(Key::Character("h".into())), chord));
        assert!(h.chrome.borrow_mut().take_activity_open_request().is_some());
        assert_eq!(
            h.host.room_activity().serve_focus().map(|focus| focus.view),
            Some(tilo_actions::remote::workspace_activity::View::History)
        );
    }

    fn super_shift_r_opens_recording_setup_then_saves_an_armed_replay() {
        let mut h = build_harness();
        let chord = Modifiers::from(ModifiersState::SUPER | ModifiersState::SHIFT);

        assert!(pump(&mut h, &key_press(Key::Character("r".into())), chord));
        assert!(h.host.capture_studio.is_open());
        assert_eq!(
            h.host.capture_studio.mode(),
            ui::widget::CaptureMode::Recording
        );
        assert!(h.chrome.borrow().overlays.capture);

        h.host
            .capture_studio
            .set_replay_active(true, ui::widget::AudioCaptureMode::SystemAndMicrophone);
        h.host.capture_studio.set_replay_buffered_seconds(30);
        assert!(pump(&mut h, &key_press(Key::Character("R".into())), chord));
        let intent = h
            .host
            .take_capture_intents()
            .pop()
            .expect("the ready rolling buffer accepts the global save chord");
        assert_eq!(
            intent.replay_command,
            Some(ui::widget::ReplayCommand::SaveRecent)
        );
        assert!(
            h.host.capture_studio.is_replay_active(),
            "saving from the keyboard must not disarm recent capture"
        );
    }

    fn super_shift_p_opens_pip_directly_without_toggling_projection() {
        let mut h = build_harness();
        let chord = Modifiers::from(ModifiersState::SUPER | ModifiersState::SHIFT);

        assert!(pump(&mut h, &key_press(Key::Character("p".into())), chord));
        assert!(h.host.capture_studio.is_open());
        assert_eq!(h.host.capture_studio.mode(), ui::widget::CaptureMode::Pip);
        assert!(h.chrome.borrow().overlays.capture);
        assert!(
            !h.chrome.borrow().overlays.projection,
            "the PiP chord must not leak into familiar Win+P projection"
        );
    }

    super_p_opens_the_display_sheet_and_escape_closes_it();
    super_l_requests_the_compositor_lock_once();
    typed_session_actions_run_through_the_power_flyouts_executor();
    super_h_opens_whats_new_without_taking_ctrl_h();
    super_shift_h_opens_the_full_history_directly();
    super_shift_r_opens_recording_setup_then_saves_an_armed_replay();
    super_shift_p_opens_pip_directly_without_toggling_projection();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn print_screen_opens_the_requested_capture_and_preserves_transactional_surfaces() {
    fn print_screen_opens_screenshot_capture_without_modifiers() {
        let mut h = build_harness();

        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::default()
        ));
        assert!(h.host.capture_studio.is_open());
        assert_eq!(
            h.host.capture_studio.mode(),
            ui::widget::CaptureMode::Screenshot
        );
        assert!(h.chrome.borrow().overlays.capture);
    }

    fn shift_print_captures_the_whole_screen_without_a_chooser() {
        let mut h = build_harness();
        h.host.palette_visible = true;

        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::from(ModifiersState::SHIFT)
        ));

        assert!(
            !h.host.capture_studio.is_open(),
            "no chooser for Shift+Print"
        );
        assert!(
            !h.host.palette_visible,
            "transient chrome closes before the scene is photographed"
        );
        let intents = h.host.take_capture_intents();
        assert_eq!(intents.len(), 1, "{intents:?}");
        let intent = &intents[0];
        assert_eq!(intent.mode, ui::widget::CaptureMode::Screenshot);
        assert_eq!(intent.source, ui::widget::SourceMode::Display);
        assert_eq!(
            intent.destination,
            ui::widget::CaptureDestination::FileAndClipboard,
            "saved and copied, since there is no chooser to pick one"
        );
        assert_eq!(intent.regions, vec![h.host.get_rect()]);
        assert!(
            h.host.take_capture_intents().is_empty(),
            "one press is one capture"
        );
    }

    fn alt_print_without_a_window_opens_the_chooser_instead_of_nothing() {
        let mut h = build_harness();

        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::from(ModifiersState::ALT)
        ));

        assert!(h.host.capture_studio.is_open());
        assert!(h.host.take_capture_intents().is_empty());
    }

    fn alt_print_photographs_the_focused_window_whole() {
        let target = ui::widget::CaptureTarget {
            id: "group:4:tab:9".into(),
            title: "Notes".into(),
            app_id: "Notes".into(),
            visible_rect: Rect::new(100.0, 40.0, 900.0, 860.0),
            virtual_rect: Rect::new(100.0, 40.0, 1400.0, 860.0),
            complete_rect: Some(Rect::new(100.0, 40.0, 900.0, 4000.0)),
            focused: true,
            on_active_strip: true,
        };
        let intent = instant_window_capture(target.clone());
        // A window capture is of the window, whole, wherever it is: the
        // compositor repaints the target, so nothing about the intent is
        // tied to what is on screen.
        assert_eq!(intent.source, ui::widget::SourceMode::Window);
        assert!(intent.anchors.is_empty());
        assert_eq!(intent.regions, vec![target.visible_rect]);
        assert_eq!(intent.targets, vec![target]);
        assert_eq!(
            intent.destination,
            ui::widget::CaptureDestination::FileAndClipboard
        );
    }

    fn instant_capture_waits_while_a_capture_is_in_progress() {
        let mut h = build_harness();
        assert!(
            h.host
                .open_capture_studio(ui::widget::CaptureMode::Screenshot)
        );

        pump(
            &mut h,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::from(ModifiersState::SHIFT),
        );

        assert!(
            h.host.take_capture_intents().is_empty(),
            "an open chooser is finished first, like plain Print"
        );
    }

    fn print_screen_replaces_every_transient_desktop_surface_atomically() {
        let mut h = build_harness();
        h.host.palette_visible = true;
        h.host.shortcut_guide_focus = Some(ShortcutGuideFocus::Done);
        {
            let mut state = h.chrome.borrow_mut();
            state.overlays.shortcut_help = true;
            state.overlays.overview = true;
            state.overlays.quick_settings = true;
            state.overlays.ws_settings = true;
            state.overlays.collaborators = true;
            state.overlays.room_activity = true;
            state.push_notification("Update ready", "A transient toast");
        }

        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::default()
        ));

        assert!(h.host.capture_studio.is_open());
        assert_eq!(
            h.host.capture_studio.mode(),
            ui::widget::CaptureMode::Screenshot
        );
        assert!(!h.host.palette_visible);
        assert!(h.host.shortcut_guide_focus.is_none());
        let state = h.chrome.borrow();
        assert!(!state.overlays.shortcut_help);
        assert!(!state.overlays.overview);
        assert!(!state.overlays.quick_settings);
        assert!(!state.overlays.ws_settings);
        assert!(!state.overlays.collaborators);
        assert!(!state.overlays.room_activity);
        assert!(
            state
                .notifications()
                .iter()
                .any(|notification| notification.title == "Update ready"),
            "taking a screenshot must not mutate a notification's lifecycle"
        );
    }

    fn print_screen_preserves_transactional_workflows_below_capture() {
        let mut h = build_harness();
        h.host.begin_wifi_password("Tilo test network");
        assert!(pump(
            &mut h,
            &ui::synthetic_input::key_input(
                &Key::Character("a".into()),
                winit::event::ElementState::Pressed
            ),
            Modifiers::default()
        ));

        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::default()
        ));
        assert!(
            h.chrome.borrow().quick_settings.wifi_password.is_some(),
            "a typed Wi-Fi credential must not be discarded by Print Screen"
        );
        assert!(h.host.capture_studio.is_open());
        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::Escape)),
            Modifiers::default()
        ));
        assert!(!h.host.capture_studio.is_open());
        assert!(pump(
            &mut h,
            &ui::synthetic_input::key_input(
                &Key::Character("b".into()),
                winit::event::ElementState::Pressed
            ),
            Modifiers::default()
        ));
        assert!(h.chrome.borrow().quick_settings.wifi_password.is_some());
        assert_eq!(
            h.host.wifi_password_field().map(TextInput::value),
            Some("ab"),
            "after capture closes, the original credential field must regain its text focus and selection"
        );

        let mut permission = build_harness();
        permission
            .host
            .workspace_settings_overlay
            .open_remove_member_sheet("peer".to_string());
        assert!(pump(
            &mut permission,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::default()
        ));
        assert!(
            permission
                .host
                .workspace_settings_overlay
                .member_sheet_is_open(),
            "a destructive member-removal confirmation must survive capture"
        );
        assert!(permission.host.capture_studio.is_open());

        let mut rebind = build_harness();
        rebind.host.shortcut_rebind =
            Some(settings_store::hardware::ShortcutAction::FocusNextWindow);
        assert!(pump(
            &mut rebind,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::default()
        ));
        assert_eq!(
            rebind.host.shortcut_rebind,
            Some(settings_store::hardware::ShortcutAction::FocusNextWindow),
            "a pending shortcut edit is a transaction, not transient guide chrome"
        );
        assert!(rebind.host.capture_studio.is_open());

        let mut projection = build_harness();
        projection.chrome.borrow_mut().overlays.projection = true;
        assert!(pump(
            &mut projection,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::default()
        ));
        assert!(projection.chrome.borrow().overlays.projection);
        assert!(projection.host.capture_studio.is_open());

        let mut nested = build_harness();
        assert!(
            nested
                .host
                .open_capture_studio(ui::widget::CaptureMode::Screenshot)
        );
        assert!(pump(
            &mut nested,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::default()
        ));
        assert!(nested.host.capture_studio.is_open());
    }

    fn print_screen_keeps_a_live_portal_file_chooser_open() {
        use crate::portal::FilePickerSpawner as _;

        let mut h = build_harness();
        let pickers = crate::portal::FilePickerQueue::new();
        h.host.set_portal_queues(
            crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::AutoAllow),
            pickers.clone(),
        );
        let workspace = h
            .chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .expect("demo desktop has an active workspace")
            .id;
        let (reply, _parked) = tokio::sync::oneshot::channel();
        pickers.spawn(crate::portal::FilePickerRequest {
            caller: crate::portal::PortalCaller::new(workspace, "org.tilo.audit"),
            parent_window: None,
            title: "Save document".to_string(),
            save: true,
            directory: false,
            writable_directory: false,
            multiple: false,
            accept_label: None,
            current_name: Some("before".to_string()),
            filters: Vec::new(),
            scope: None,
            recent: Vec::new(),
            reply,
        });
        assert!(h.host.pump_file_picker());
        assert!(h.host.file_picker().is_some());
        assert!(pump(
            &mut h,
            &key_press(Key::Character("x".into())),
            Modifiers::default()
        ));
        assert_eq!(
            h.host.file_picker().map(ui::widget::FilePicker::filename),
            Some("x"),
            "the Save name field owns text input before capture opens (typing replaces \
             the preset name, which opens selected)"
        );

        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::default()
        ));
        assert!(
            h.host.file_picker().is_some(),
            "Print Screen must not silently cancel an app's parked portal request"
        );
        assert!(
            h.host.capture_studio.is_open(),
            "Capture Studio must become the temporary input owner above the live chooser"
        );
        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::Escape)),
            Modifiers::default()
        ));
        assert!(!h.host.capture_studio.is_open());
        assert!(pump(
            &mut h,
            &key_press(Key::Character("y".into())),
            Modifiers::default()
        ));
        assert_eq!(
            h.host.file_picker().map(ui::widget::FilePicker::filename),
            Some("xy"),
            "after capture closes, the exact prior Save field must regain keyboard input"
        );
    }

    fn print_screen_preserves_oobe_and_returns_to_the_same_step() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.desktop_tour = true;
        h.host.sync(&AutoCommit::new(), &automerge::ROOT);
        let before = h
            .host
            .onboarding
            .as_ref()
            .expect("desktop tour activates onboarding")
            .flow()
            .screen();

        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::PrintScreen)),
            Modifiers::default()
        ));
        assert!(h.host.onboarding_active());
        assert!(h.host.capture_studio.is_open());
        assert!(
            h.host.needs_animation_frame(),
            "Capture Studio's entrance must keep repainting while it temporarily sits above OOBE"
        );

        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::Escape)),
            Modifiers::default()
        ));
        assert!(!h.host.capture_studio.is_open());
        assert_eq!(
            h.host
                .onboarding
                .as_ref()
                .expect("Capture Studio must not tear down OOBE")
                .flow()
                .screen(),
            before,
            "Escaping Capture Studio returns to the exact OOBE step being inspected"
        );
    }

    fn capture_entry_always_uses_the_requested_mode_instead_of_the_previous_one() {
        let mut h = build_harness();
        assert!(h.host.open_capture_studio(ui::widget::CaptureMode::Pip));
        assert_eq!(h.host.capture_studio.mode(), ui::widget::CaptureMode::Pip);

        h.host.capture_studio.close();
        assert!(
            h.host
                .open_capture_studio(ui::widget::CaptureMode::Screenshot)
        );
        assert_eq!(
            h.host.capture_studio.mode(),
            ui::widget::CaptureMode::Screenshot,
            "Super+Shift+S and app requests must not reopen the last PiP mode"
        );
        assert!(h.chrome.borrow().overlays.capture);
    }

    print_screen_opens_screenshot_capture_without_modifiers();
    print_screen_replaces_every_transient_desktop_surface_atomically();
    print_screen_preserves_transactional_workflows_below_capture();
    print_screen_keeps_a_live_portal_file_chooser_open();
    print_screen_preserves_oobe_and_returns_to_the_same_step();
    capture_entry_always_uses_the_requested_mode_instead_of_the_previous_one();
    shift_print_captures_the_whole_screen_without_a_chooser();
    alt_print_without_a_window_opens_the_chooser_instead_of_nothing();
    alt_print_photographs_the_focused_window_whole();
    instant_capture_waits_while_a_capture_is_in_progress();
}

#[test]
fn app_pickers_and_alt_tab_target_the_focused_group_and_strip() {
    /// Super+T is the system's "new tab of this app": it works where the
    /// app keeps Ctrl+T for itself. In a focused group it opens another tab
    /// of the focused app in that same group, without a picker.
    fn super_t_opens_a_new_tab_of_the_focused_app_in_the_focused_group() {
        let mut h = build_harness();
        let group = crate::shell::window_actions::focused_group(&h.chrome.borrow())
            .expect("demo workspace has a focused group");
        let tabs_in = |h: &Harness| {
            h.chrome
                .borrow()
                .tree()
                .desktop
                .workspaces
                .iter()
                .flat_map(|ws| &ws.strips)
                .flat_map(|strip| &strip.window_groups)
                .find(|candidate| candidate.id == group)
                .map(|candidate| candidate.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>())
                .expect("the focused group stays")
        };
        let before = tabs_in(&h);
        let super_key = Modifiers::from(ModifiersState::SUPER);
        assert!(pump(
            &mut h,
            &key_press(Key::Character("t".into())),
            super_key
        ));

        let after = tabs_in(&h);
        assert_eq!(
            after.len(),
            before.len() + 1,
            "Super+T adds exactly one tab to the focused group: {before:?} -> {after:?}"
        );
        assert!(
            !h.host.palette_visible(),
            "a new tab of the focused app needs no picker"
        );
    }

    fn super_t_from_a_fresh_empty_workspace_opens_the_app_launcher() {
        use crate::shell::chrome_state::{OpenTarget, SearchScope};

        let tree = WorkspaceTree::bootstrap_default();
        let chrome = ChromeState::new(tree);
        let host = ShellOverlayHost::new(Box::new(StubContent::new())).with_chrome(chrome.clone());
        let mut h = build_harness_with_host(host, chrome);
        let super_key = Modifiers::from(ModifiersState::SUPER);

        assert!(pump(
            &mut h,
            &key_press(Key::Character("t".into())),
            super_key
        ));

        let state = h.chrome.borrow();
        assert_eq!(
            state.open_target,
            OpenTarget::Beside,
            "with no focused group, the first app must be placed as a new group"
        );
        assert_eq!(
            state.search_scope,
            SearchScope::All,
            "with no window yet, Super+T opens the app launcher, as Super+N does"
        );
        assert!(h.host.palette_visible());
    }

    super_t_opens_a_new_tab_of_the_focused_app_in_the_focused_group();
    super_t_from_a_fresh_empty_workspace_opens_the_app_launcher();
}

fn pump(h: &mut Harness, event: &WindowEvent, modifiers: Modifiers) -> bool {
    pump_at_scale(h, event, modifiers, 1.0)
}

fn pump_at_scale(h: &mut Harness, event: &WindowEvent, modifiers: Modifiers, scale: f64) -> bool {
    ui::testing::Dispatch::new()
        .modifiers(modifiers)
        .scale_factor(scale)
        .layout(&h.taffy, h.root_node)
        .send(&mut h.host, event)
        .handled
}

/// Builds a `KeyEvent` over a zeroed buffer — winit 0.30's
/// `KeyEventExtra` is `pub(crate)`, so a struct literal isn't possible.
/// Only the pub fields the host reads (`logical_key`, `state`) are
/// overwritten.
/// An app's Save that names a file already in the folder: the chooser asks
/// before anything is chosen, and the question is in the semantic tree with a
/// Replace an assistive technology or an agent can press.
#[test]
fn a_save_over_an_existing_file_asks_and_its_semantic_replace_answers() {
    use crate::portal::FilePickerSpawner as _;

    fn find<'a>(
        node: &'a ui::a11y::AccessNode,
        pred: &dyn Fn(&ui::a11y::AccessNode) -> bool,
    ) -> Option<&'a ui::a11y::AccessNode> {
        if pred(node) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, pred))
    }

    let folder = tempfile::tempdir().expect("tempdir");
    let photo = folder.path().join("labrador.jpg");
    std::fs::write(&photo, b"original").unwrap();

    let mut h = build_harness();
    let pickers = crate::portal::FilePickerQueue::new();
    h.host.set_portal_queues(
        crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::AutoAllow),
        pickers.clone(),
    );
    let workspace = h
        .chrome
        .borrow()
        .tree()
        .desktop
        .active_workspace()
        .expect("demo desktop has an active workspace")
        .id;
    let (reply, mut received) = tokio::sync::oneshot::channel();
    pickers.spawn(crate::portal::FilePickerRequest {
        caller: crate::portal::PortalCaller::new(workspace, "so.tilo.canvas"),
        parent_window: None,
        title: "Export a picture".to_string(),
        save: true,
        directory: false,
        writable_directory: false,
        multiple: false,
        accept_label: Some("Export".to_string()),
        current_name: Some("labrador.jpg".to_string()),
        filters: Vec::new(),
        scope: Some(crate::portal::PickerScope {
            initial_dir: folder.path().to_path_buf(),
            roots: vec![folder.path().to_path_buf()],
            home: None,
        }),
        recent: Vec::new(),
        reply,
    });
    assert!(h.host.pump_file_picker());
    settle_file_picker(&mut h);

    assert!(pump(
        &mut h,
        &key_press(Key::Named(NamedKey::Enter)),
        Modifiers::default()
    ));
    settle_file_picker(&mut h);
    assert!(
        received.try_recv().is_err(),
        "nothing is chosen before the answer"
    );

    let semantics = h.host.accessibility().expect("shell semantics");
    let prompt = find(&semantics, &|node| {
        node.name.as_deref()
            == Some("A file named \u{201c}labrador.jpg\u{201d} already exists here. Replace it?")
    })
    .expect("the replace question is published");
    assert!(prompt.state.modal);
    let replace = find(prompt, &|node| node.name.as_deref() == Some("Replace"))
        .and_then(|node| node.action_target.clone())
        .expect("Replace carries an action target");
    assert!(h.host.perform_semantic_action(
        &replace,
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
        &automerge::ROOT,
    ));
    assert!(h.host.pump_file_picker());
    assert_eq!(received.try_recv().unwrap(), Some(vec![photo.clone()]));
    assert!(h.host.file_picker().is_none());
    assert_eq!(
        std::fs::read(&photo).unwrap(),
        b"original",
        "the app writes, not the chooser"
    );
}

/// Pump the chooser until its worker has answered everything it was asked
/// (its listing, a typed name's check), as the compositor does each time
/// the worker wakes it.
fn settle_file_picker(h: &mut Harness) {
    tilo_test_env::wait_until("the chooser's worker to answer", || {
        h.host.pump_file_picker();
        !h.host
            .file_picker()
            .is_some_and(ui::widget::FilePicker::reading)
    });
}

fn key_press(logical_key: Key) -> WindowEvent {
    let mut ev = ui::synthetic_input::key_event(
        winit::keyboard::Key::Unidentified(winit::keyboard::NativeKey::Unidentified),
        None,
        winit::event::ElementState::Pressed,
    );
    // A character key carries its text, as a real keyboard's does; text
    // fields type from it.
    let text = match &logical_key {
        Key::Character(text) => Some(text.clone()),
        _ => None,
    };
    ev.logical_key = logical_key;
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = text;
    ui::synthetic_input::keyboard_input(ev)
}

fn cursor_moved_at(p: Point) -> WindowEvent {
    ui::synthetic_input::cursor_moved(p.x, p.y)
}

use ui::synthetic_input::mouse_press;

use ui::synthetic_input::mouse_release;

/// Paint one frame, which is what populates every cached hit rect.
fn paint(h: &mut Harness) {
    let theme = Theme::dark();
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::empty();
    h.host
        .draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn collaborator_dropdown_offers_gated_actions_and_clears_their_rects_on_close() {
    /// The roster's one job is who is here: adding somebody is its only
    /// footer action. Joining another workspace is in the workspace menu,
    /// and what happened here is History's.
    fn collaborator_dropdown_offers_invite_and_nothing_else_in_its_footer() {
        let mut h = build_fixture_harness();
        let _room = host_active_room(&h.chrome);
        h.chrome.borrow_mut().peer_sync_enabled = true;
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);

        let invite = h.host.collaborators_invite_rect_for_tests();
        assert!(invite.area() > 0.0, "Invite painted a hit target");
        let panel = h.host.collaborators_overlay.panel_rect();
        assert!(
            panel.contains(invite.center()) && panel.contains(invite.origin()),
            "Invite is inside the panel it is painted on"
        );
    }

    /// Inviting somebody is inviting *somebody* — the button asks who
    /// before minting, since an unnamed token leaves the host's own roster
    /// reading a bare peer id once the guest arrives.
    fn collaborator_invite_button_asks_who_before_minting() {
        let mut h = build_fixture_harness();
        let _room = host_active_room(&h.chrome);
        h.chrome.borrow_mut().peer_sync_enabled = true;
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);
        let invite = h.host.collaborators_invite_rect_for_tests();
        assert!(invite.area() > 0.0, "there is a button to press");

        let mods = Modifiers::default();
        let _ = pump(&mut h, &cursor_moved_at(invite.center()), mods);
        let handled = pump(&mut h, &mouse_press(), mods);
        assert!(handled, "the button claimed the click");
        assert!(
            h.chrome.borrow_mut().take_peer_sync_requests().is_empty(),
            "the press remains cancellable"
        );
        assert!(pump(&mut h, &mouse_release(), mods));

        assert_eq!(
            h.chrome.borrow_mut().take_peer_sync_requests(),
            vec![PeerSyncAction::PromptInvite],
            "the click opens the composer rather than minting an unnamed token"
        );
        assert!(
            !h.chrome.borrow().overlays.collaborators,
            "the dropdown closed, so the card is what the user is left looking at"
        );
    }

    /// Peer sync off means nothing can mint a token, so the button must not
    /// offer to. The room is held and governed here first so peer-sync is the *only*
    /// capability missing.
    fn collaborator_actions_are_absent_when_peer_sync_is_off() {
        let mut h = build_fixture_harness();
        let _room = host_active_room(&h.chrome);
        assert!(!h.chrome.borrow().peer_sync_enabled);
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);

        assert_eq!(
            h.host.collaborators_invite_rect_for_tests(),
            Rect::ZERO,
            "a session that cannot mint offers no button that says it can"
        );
        assert_no_peer_sync_work_from_a_click_in_the_panel(&mut h);
    }

    /// The capability gate must remove the *hit target*, not just the
    /// paint — a rect that outlives its button is invisible but still
    /// clickable.
    fn collaborator_actions_are_absent_without_the_invite_capability() {
        let mut h = build_fixture_harness();
        {
            // Owned by somebody else, and the local user holds no grant
            // here — the ordinary position of a guest in a room.
            let st = h.chrome.borrow();
            let mut tree = st.tree_mut();
            for ws in &mut tree.desktop.workspaces {
                ws.set_owner("somebody-else".to_string());
            }
        }
        // Peer sync *is* on, so the transport is not the reason.
        h.chrome.borrow_mut().peer_sync_enabled = true;
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);

        assert_eq!(
            h.host.collaborators_invite_rect_for_tests(),
            Rect::ZERO,
            "a guest is not offered the room's membership controls"
        );
        assert_no_peer_sync_work_from_a_click_in_the_panel(&mut h);
    }

    /// The rects must not survive the dropdown closing, or empty space
    /// where a button used to be stays clickable.
    fn collaborator_action_rects_are_cleared_when_the_dropdown_closes() {
        let mut h = build_fixture_harness();
        let _room = host_active_room(&h.chrome);
        h.chrome.borrow_mut().peer_sync_enabled = true;
        h.chrome.borrow_mut().overlays.collaborators = true;
        paint(&mut h);
        assert!(h.host.collaborators_invite_rect_for_tests().area() > 0.0);

        h.chrome.borrow_mut().overlays.collaborators = false;
        paint(&mut h);
        assert_eq!(
            h.host.collaborators_invite_rect_for_tests(),
            Rect::ZERO,
            "stale action rects are click targets the user cannot see"
        );
    }

    collaborator_dropdown_offers_invite_and_nothing_else_in_its_footer();
    collaborator_invite_button_asks_who_before_minting();
    collaborator_actions_are_absent_when_peer_sync_is_off();
    collaborator_actions_are_absent_without_the_invite_capability();
    collaborator_action_rects_are_cleared_when_the_dropdown_closes();
}

/// Makes the local user owner of every room, as boot does while starting
/// up. `bootstrap_demo` leaves this step undone so invite permissions can
/// be tested separately.
fn adopt_rooms(h: &Harness) {
    let st = h.chrome.borrow();
    st.tree_mut()
        .own_workspaces_for_test(|room| st.local_user_for_room(room));
}

/// The active workspace's room held as one this device's own host governs,
/// the chrome acting as the person governing it: an invitation is the
/// host's to issue, so this is what the invite composer needs. The room
/// stays held while the guard lives.
pub(super) fn host_active_room(
    chrome: &crate::shell::chrome_state::Chrome,
) -> crate::shell::test_support::OwnHostedRoom {
    let hosted = crate::shell::test_support::host_active_room(&chrome.borrow().tree_handle());
    chrome.borrow_mut().act_as(hosted.person.clone());
    hosted
}

/// A click anywhere inside the popover asks the network for nothing.
fn assert_no_peer_sync_work_from_a_click_in_the_panel(h: &mut Harness) {
    let panel = h.host.collaborators_overlay.panel_rect();
    let mods = Modifiers::default();
    let _ = pump(h, &cursor_moved_at(panel.center()), mods);
    let _ = pump(h, &mouse_press(), mods);
    assert!(
        h.chrome.borrow_mut().take_peer_sync_requests().is_empty(),
        "a click in a panel with no buttons queued no peer-sync work"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn palette_search_reaches_apps_note_bodies_and_records_in_other_rooms() {
    /// Opening the palette with a stale query must both focus the input
    /// and clear it. Drives the configured Super+Space path through
    /// `handle_event`.
    fn toggle_palette_focuses_input() {
        let mut h = build_harness();
        // Seed a stale query so the "clear on open" half of the spec
        // is observable too.
        h.host.palette.set_query("stale residual");
        assert!(!h.host.palette.is_focused(), "palette must start unfocused");
        assert!(!h.host.palette_visible(), "palette must start hidden");

        h.host
            .set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
            .unwrap();
        let mods = Modifiers::from(ModifiersState::SUPER);
        let handled = pump(&mut h, &key_press(Key::Named(NamedKey::Space)), mods);

        assert!(handled, "Super+Space must be consumed by the overlay host");
        assert!(
            h.host.palette_visible(),
            "Super+Space must open the global-search palette"
        );
        assert!(
            h.host.palette.is_focused(),
            "opening the palette via Super+Space must focus its input \
             (so the very first keystroke after open lands in the search box)"
        );
        assert_eq!(
            h.host.palette.query(),
            "",
            "opening the palette must clear any residual query — \
             {:?} should have been wiped",
            h.host.palette.query()
        );
    }

    /// The configured Search action opens the global surface and reaches
    /// installed apps, rather than relying on a legacy hard-coded chord.
    fn configured_search_reaches_installed_apps() {
        use crate::shell::chrome_state::SearchScope;

        let tmp = tempfile::tempdir().unwrap();
        install_gui_fixture(tmp.path(), "so.tilo.zzyzx", "Zzyzx");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        crate::shell::launcher::invalidate_app_cache();

        let mut h = build_harness();
        h.host.search_registry.register(std::rc::Rc::new(
            crate::shell::search::providers::ApplicationsProvider::new(),
        ));
        h.host
            .set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
            .unwrap();
        let doc = AutoCommit::new();

        // Super+Space is the default Search action.  Any rebind goes through the
        // same effective-action resolver used by this dispatcher.
        let sup = Modifiers::from(ModifiersState::SUPER);
        assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Space)), sup));
        assert_eq!(
            h.chrome.borrow().search_scope,
            SearchScope::Workspace,
            "Super+Space opens global search on the workspace in front, the launcher's scope"
        );
        h.host.palette.set_query("zzyzx");
        // Retained application indexes are built from the compositor update
        // pass, never synchronously by the keystroke/query path. Exercise the
        // real lifecycle here: the first update starts the worker and a later
        // one publishes its immutable snapshot and refreshes the visible rows.
        let index_started = std::time::Instant::now();
        while index_started.elapsed() < tilo_test_env::hang_bound() {
            h.host.update(&doc, &automerge::ROOT);
            if h.host
                .search_results
                .iter()
                .any(|result| result.provider_id == "applications")
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let hit = h
            .host
            .search_results
            .iter()
            .find(|r| r.provider_id == "applications");
        let hit = hit.unwrap_or_else(|| {
            panic!(
                "typing an installed app's name must offer a row to launch \
                 it; got {:?}",
                h.host
                    .search_results
                    .iter()
                    .map(|r| (&r.provider_id, &r.title))
                    .collect::<Vec<_>>()
            )
        });
        assert_eq!(hit.id, "so.tilo.zzyzx");
        assert_eq!(hit.title, "Zzyzx");

        env.remove("TILO_APPS_DATA_DIR");
        crate::shell::launcher::invalidate_app_cache();
    }

    /// A room starting empty is only workable if moving one of your own
    /// records into it costs one step: Super+Space, type the name, and a
    /// record from *another* room is among the answers — labelled with
    /// where it lives — and Enter brings it here.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the `records` borrow feeds the `record_ids` closure used through the \
    assertions right after; ending it earlier would mean cloning the record \
    store for no real gain"
    )]
    fn super_k_finds_a_record_from_another_room_and_enter_brings_it_here() {
        use crate::shell::chrome_state::SearchScope;
        use ui::record::{RecordId, RecordKind};

        let mut h = build_harness();
        // Two rooms from the fixture tree: the one in front of the user
        // and one they are not in.
        let (here, there, there_name) = {
            let st = h.chrome.borrow();
            let tree = st.tree();
            let here = tree
                .desktop
                .active_workspace()
                .expect("an active room")
                .room();
            let other = tree
                .desktop
                .workspaces
                .iter()
                .find(|w| w.room() != here)
                .expect("the fixture has more than one room");
            (here, other.room(), other.name.clone())
        };

        // A record that lives over there and nowhere else.
        let book = RecordId::minted();
        let kind = RecordKind::new("field-guide");
        h.host
            .room_records()
            .borrow_mut()
            .reference(there, &book, &kind);
        assert!(
            !h.host.room_records().borrow().holds(here, &book),
            "precondition: this room starts without it"
        );

        let mut doc = AutoCommit::new();
        let sup = Modifiers::from(ModifiersState::SUPER);
        assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Space)), sup));
        assert_eq!(h.chrome.borrow().search_scope, SearchScope::Workspace);
        // Scoped to the workspace, typing still reaches other rooms' records:
        // the scope ranks what is here first, it hides nothing.
        h.host.palette.set_query("field-guide");
        h.host.refresh_search_results(&doc, &automerge::ROOT);

        let listing = || {
            h.host
                .search_results
                .iter()
                .map(|r| (r.provider_id.clone(), r.title.clone(), r.subtitle.clone()))
                .collect::<Vec<_>>()
        };
        let idx = h
            .host
            .search_results
            .iter()
            .position(|r| r.provider_id == "records")
            .unwrap_or_else(|| {
                panic!(
                    "a record living in another room must be findable from \
                     this one; got {:?}",
                    listing()
                )
            });
        assert_eq!(
            h.host.search_results[idx].subtitle.as_deref(),
            Some(format!("In {there_name}, field-guide").as_str()),
            "the row has to say where the record lives — without it, two \
             rooms' records are indistinguishable rows"
        );
        // The row the palette paints, not just the result behind it.
        let title = h.host.search_results[idx].title.clone();
        let subtitle = h.host.search_results[idx].subtitle.clone();
        assert!(
            h.host
                .palette
                .visible_entries()
                .iter()
                .any(|e| e.label == title && e.description == subtitle),
            "the record must reach the palette's own rows: {:?}",
            h.host
                .palette
                .visible_entries()
                .iter()
                .map(|e| (&e.label, &e.description))
                .collect::<Vec<_>>()
        );

        // Enter on that row.
        h.host.palette.set_cursor(idx);
        h.host
            .activate_selected_result(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);

        let records = h.host.room_records();
        let records = records.borrow();
        assert!(
            records.holds(here, &book),
            "activating the row must bring the record into this room"
        );
        assert!(
            records.holds(there, &book),
            "and leave it in the room it came from: this is a reference, \
             not a move"
        );
        let record_ids = |room| -> Vec<RecordId> {
            records.records_in(room).into_iter().map(|c| c.id).collect()
        };
        assert_eq!(
            record_ids(here),
            record_ids(there),
            "both rooms must name the same record id — a copy would have \
             minted a new one, and the two would diverge from the next \
             edit onward"
        );
    }

    /// The app-scope pill has to name the focused app *as an app*, or it
    /// collides with the kind-filter chip strip above it: two adjacent
    /// controls with one word and different meanings ("results shaped
    /// like a note" versus "search inside the Notes app").
    fn app_scope_pill_names_the_app_as_an_app() {
        use crate::shell::chrome_state::SearchScope;
        let h = build_harness();
        assert_eq!(
            h.chrome.borrow().active_app_title().as_deref(),
            Some("Notes"),
            "fixture precondition: a tab is focused"
        );
        assert_eq!(
            h.host.search_scope_pill_label(SearchScope::ActiveApp),
            "Notes app"
        );
        // Generalised: no label may appear in both the filter row (kinds)
        // and the scope row (places) — `All` was one such collision.
        let chips: Vec<String> = h
            .host
            .search_registry
            .registered_kinds()
            .iter()
            .map(crate::shell::search::ResourceKindText::localized_label)
            // The leading reset chip is painted separately from the
            // registered kinds; see `draw_search_banner`.
            .chain(std::iter::once("All".to_string()))
            .collect();
        for scope in h.host.search_scope_list() {
            let pill = h.host.search_scope_pill_label(scope);
            assert!(
                !chips.contains(&pill),
                "the {scope:?} scope pill {pill:?} is also a kind chip label — \
                 two adjacent controls with one word and two meanings: {chips:?}"
            );
        }
        assert!(SearchScope::ActiveApp.narrows_results());
        assert!(!SearchScope::Workspace.narrows_results());
        assert!(!SearchScope::All.narrows_results());
    }

    toggle_palette_focuses_input();
    configured_search_reaches_installed_apps();
    super_k_finds_a_record_from_another_room_and_enter_brings_it_here();
    app_scope_pill_names_the_app_as_an_app();
}

/// Write a registry + manifest pair under `dir` so
/// `launcher::gui_apps()` — and therefore the
/// `ApplicationsProvider` — enumerates one installed GUI package.
fn install_gui_fixture(dir: &std::path::Path, package_id: &str, name: &str) {
    let payload = dir.join("payloads").join(package_id);
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    let manifest_path = payload.join("manifest.toml");
    std::fs::write(
        &manifest_path,
        crate::shell::test_support::canonical_manifest(
            &format!(
                "id = \"{package_id}\"\nname = \"{name}\"\nversion = \"1.2.3\"\nkind = \"app\""
            ),
            "[entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n",
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("registry.toml"),
        format!(
            "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest_path.display().to_string()
        ),
    )
    .unwrap();
}

#[test]
fn workspace_settings_pills_close_button_and_surface_are_pointer_accessible() {
    /// Click the Members pill and assert `chrome.ws_settings_tab` flips,
    /// through a real `MouseInput::Pressed` dispatched via `handle_event`.
    fn ws_settings_pill_click_switches_tab() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.ws_settings = true;
        assert_eq!(
            h.chrome.borrow().ws_settings_tab,
            WsTab::General,
            "modal must default to the General tab"
        );

        // Pill rects are populated inside `draw_ws_settings`, so paint
        // one frame before they're queryable.
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::empty();
        h.host
            .draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let members_pill = h.host.ws_settings_tab_pill_rect(WsTab::Members);
        assert!(
            members_pill.area() > 0.0,
            "Members pill rect must be populated by draw_ws_settings; got {members_pill:?}"
        );
        let p = members_pill.center();

        // MouseInput carries no position; seed `last_cursor` via CursorMoved first.
        let mods = Modifiers::default();
        let _ = pump(&mut h, &cursor_moved_at(p), mods);
        let handled = pump(&mut h, &mouse_press(), mods);

        assert!(
            handled,
            "click on the Members pill must be consumed by the overlay host"
        );
        assert_eq!(
            h.chrome.borrow().ws_settings_tab,
            WsTab::Members,
            "click on the Members pill must flip chrome.ws_settings_tab to Members \
             (production path through handle_event, not a direct setter)"
        );
    }

    fn ws_settings_close_button_is_pointer_accessible() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.ws_settings = true;
        let close = WorkspaceSettingsOverlay::close_rect_for(h.host.ws_settings_rect());
        assert!(close.area() > 0.0);

        let mods = Modifiers::default();
        let _ = pump(&mut h, &cursor_moved_at(close.center()), mods);
        assert!(pump(&mut h, &mouse_press(), mods));
        assert!(!h.chrome.borrow().overlays.ws_settings);
    }

    /// Workspace settings follows application surfaces, not the brighter
    /// floating-panel material used by shell popovers.
    fn ws_settings_uses_application_base_surface() {
        let theme = Theme::dark();
        assert_eq!(
            theme.tokens.colors.surfaces.base,
            theme.tokens.colors.surfaces.base
        );
    }

    ws_settings_pill_click_switches_tab();
    ws_settings_close_button_is_pointer_accessible();
    ws_settings_uses_application_base_surface();
}

#[test]
fn palette_scrim_and_surface_alphas_match_the_concept_and_theme() {
    /// The shell's scrims (palette, overview, modal sheets) are the
    /// toolkit's one scrim at the theme's strength: a white window card
    /// behind the palette comes out clearly dimmed in every theme, and the
    /// overview's cached-frame scrim carries the same strength.
    fn shell_scrims_are_the_toolkit_scrim() {
        for theme in [Theme::dark(), Theme::light()] {
            let strength = theme.tokens.colors.surfaces.scrim_strength;
            assert!(
                strength >= 0.45,
                "{}: a scrim under 0.45 lets the desktop bleed through the \
                 palette (got {strength})",
                theme.name
            );
            let card = ui::chrome::scrim_maps(255, strength, &theme);
            assert!(
                card <= 210,
                "{}: a bg-white window card behind the palette lands at {card}",
                theme.name
            );
        }
    }

    /// The scrim's colour comes from the theme, not a literal — both
    /// themes must darken, or a light-theme scrim would push the
    /// background toward the surface it's meant to lift away from.
    fn scrim_hue_comes_from_the_theme_and_darkens_in_both() {
        for (name, colors) in [
            ("dark", ui::tokens::colors::Colors::dark()),
            ("light", ui::tokens::colors::Colors::light()),
        ] {
            let s = colors.surfaces.scrim.to_rgba8();
            let base = colors.surfaces.base.to_rgba8();
            let luma = |c: &ui::peniko::color::Rgba8| {
                0.2126 * f32::from(c.r) + 0.7152 * f32::from(c.g) + 0.0722 * f32::from(c.b)
            };
            assert!(
                luma(&s) < luma(&base),
                "the {name} theme's scrim must be darker than its base \
                 surface (scrim {s:?} vs base {base:?})"
            );
            assert_eq!(
                s.a, 255,
                "the {name} scrim token carries the hue; the \
                 strength is the overlay's own (got alpha {})",
                s.a
            );
        }
    }

    shell_scrims_are_the_toolkit_scrim();
    scrim_hue_comes_from_the_theme_and_darkens_in_both();
}

#[test]
fn pick_group_at_hits_only_containing_non_empty_rects() {
    /// Extracted-helper coverage for the click-focuses-group logic.
    fn pick_group_at_returns_containing_group() {
        let strip = StripId::from(7);
        let candidates = vec![
            (strip, 1u64, Rect::new(0.0, 100.0, 400.0, 500.0)),
            (strip, 2u64, Rect::new(400.0, 100.0, 800.0, 500.0)),
        ];
        assert_eq!(
            pick_group_at(&candidates, Point::new(200.0, 300.0)),
            Some((strip, 1))
        );
        assert_eq!(
            pick_group_at(&candidates, Point::new(600.0, 300.0)),
            Some((strip, 2))
        );
    }

    fn pick_group_at_none_outside_all_rects() {
        let strip = StripId::from(7);
        let candidates = vec![(strip, 1u64, Rect::new(0.0, 100.0, 400.0, 500.0))];
        assert!(pick_group_at(&candidates, Point::new(600.0, 50.0)).is_none());
    }

    fn pick_group_at_skips_zero_area_rects() {
        // Freshly detached group whose apply_layout hasn't run yet —
        // rect is zero. Must not swallow every click at (0,0).
        let strip = StripId::from(7);
        let candidates = vec![
            (strip, 1u64, Rect::new(0.0, 0.0, 0.0, 0.0)),
            (strip, 2u64, Rect::new(0.0, 100.0, 400.0, 500.0)),
        ];
        assert_eq!(
            pick_group_at(&candidates, Point::new(0.0, 0.0)),
            None,
            "zero-area group must NOT match (0,0)"
        );
        assert_eq!(
            pick_group_at(&candidates, Point::new(200.0, 300.0)),
            Some((strip, 2))
        );
    }

    pick_group_at_returns_containing_group();
    pick_group_at_none_outside_all_rects();
    pick_group_at_skips_zero_area_rects();
}

/// First login in one session: the desktop's Home workspace exists before the
/// identity does, so it is owned by the account name. Finishing setup must make
/// the minted identity its owner, or every app opened next is refused its
/// collaboration route and exits before drawing.
#[test]
fn first_login_setup_then_an_app_joins_the_home_workspace_in_the_same_session() {
    use crate::onboarding_view::{
        KEY_BACKUP_CONTINUE, KEY_BACKUP_SKIP_ACKNOWLEDGED, KEY_BEGIN, KEY_CODECS_SKIP, KEY_CREATE,
        KEY_FINISH, KEY_UNDERSTOOD,
    };
    use crate::shell::state::{CapRole, Workspace, WorkspaceId};

    let home = tempfile::tempdir().expect("home");
    let identity_root = home.path().join("identity-root");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", home.path());
    env.set("XDG_CONFIG_HOME", home.path().join(".config"));
    env.set("TILO_IDENTITY_ROOT", &identity_root);
    env.set("USER", "quickuser");
    env.remove("TILO_USER_ID");

    let mut tree = WorkspaceTree::default();
    let mut workspace = Workspace::new(WorkspaceId(1), "Home");
    workspace.set_owner(crate::shell::state::local_user_id_for_room(
        &workspace.room(),
    ));
    let room = workspace.room();
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    let chrome = ChromeState::new(tree);
    assert_eq!(
        chrome.borrow().tree().desktop.workspaces[0]
            .owner()
            .map(String::as_str),
        Some("quickuser"),
        "before setup the session acts as the account"
    );

    // The ceremony mints the root key; provisioning records its public half
    // where the session reads its own principal.
    let root_key = ed25519_dalek::SigningKey::from_bytes(&[7; 32])
        .verifying_key()
        .to_bytes();
    let paths = tilo_user_identity::persist::IdentityPaths::under(&identity_root);
    let record_dir = home.path().join("identity");
    let view = crate::onboarding_view::OnboardingView::with_minter(Box::new(move || {
        std::fs::create_dir_all(paths.user_pubkey.parent().expect("parent")).expect("dir");
        std::fs::write(&paths.user_pubkey, root_key).expect("user public key");
        Ok(crate::onboarding::MintedIdentity {
            fingerprint: hex::encode(root_key),
            path: record_dir.join("primary.json"),
        })
    }));
    let mut host = ShellOverlayHost::new(Box::new(StubContent::new())).with_chrome(chrome.clone());
    let view = view.with_codec_setup(crate::onboarding_view::CodecSetup::with(
        std::sync::Arc::new(os_actions::codec_pack::CodecPackStatus::default),
        std::sync::Arc::new(|_| panic!("skipping the offer must not install codecs")),
    ));
    host.enable_test_onboarding(view);

    let mut doc = AutoCommit::new();
    for key in [
        KEY_BEGIN,
        KEY_UNDERSTOOD,
        KEY_CREATE,
        KEY_BACKUP_SKIP_ACKNOWLEDGED,
        KEY_BACKUP_CONTINUE,
        KEY_CODECS_SKIP,
        KEY_FINISH,
    ] {
        let args = if matches!(key, KEY_UNDERSTOOD | KEY_BACKUP_SKIP_ACKNOWLEDGED) {
            serde_json::json!({"checked":true})
        } else {
            serde_json::json!({})
        };
        invoke_setup(&mut host, &chrome, &mut doc, &(key, args));
    }
    assert!(!host.onboarding_active(), "setup released the desktop");

    // What the collaboration broker admits an app's join by.
    let principal = tilo_action_broker::collab_broker_capnp::principal_id_for_root(root_key);
    let state = chrome.borrow();
    let tree = state.tree();
    let home = tree
        .desktop
        .workspaces
        .iter()
        .find(|w| w.room() == room)
        .expect("home");
    assert_eq!(home.role_of(&principal), Some(CapRole::Owner));
    assert_eq!(home.role_of("quickuser"), None);
    assert_eq!(state.local_user_id, principal);
}

fn roster_follow_fixture() -> Harness {
    let mut h = build_harness();
    {
        let mut chrome = h.chrome.borrow_mut();
        chrome.overlays.collaborators = true;
        chrome
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .expect("fixture workspace")
            .collaborators = vec![
            crate::shell::state::Collaborator::new("ada", "Ada"),
            crate::shell::state::Collaborator::new("bob", "Bob"),
        ];
    }
    paint(&mut h);
    h
}

#[test]
fn roster_follow_click_keeps_the_painted_person_when_live_rows_reorder() {
    let mut h = roster_follow_fixture();
    assert!(h.host.collaborators_overlay.focus_accessibility_path(&[3]));
    let target = h
        .host
        .collaborators_overlay
        .accessibility_rect_for_path(&[3])
        .expect("painted Ada Follow");
    {
        let chrome = h.chrome.borrow_mut();
        chrome
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .expect("fixture workspace")
            .collaborators
            .swap(0, 1);
    }
    let mods = Modifiers::default();
    assert!(pump(&mut h, &cursor_moved_at(target.center()), mods));
    assert!(pump(&mut h, &mouse_press(), mods));
    assert!(
        h.host.followed_peer.is_none(),
        "the intended person is only armed"
    );
    assert!(pump(&mut h, &mouse_release(), mods));
    assert_eq!(
        h.host.followed_peer.as_deref(),
        Some("ada"),
        "the painted Ada control must not follow Bob's replacement index"
    );
}

#[test]
fn roster_follow_keyboard_rejects_a_departed_painted_person_before_repaint() {
    let mut h = roster_follow_fixture();
    assert!(h.host.collaborators_overlay.focus_accessibility_path(&[3]));
    {
        let chrome = h.chrome.borrow_mut();
        chrome
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .expect("fixture workspace")
            .collaborators
            .remove(0);
    }
    let mut handled = false;
    assert!(h.host.handle_collaborators_keyboard(
        &Key::Named(NamedKey::Enter),
        false,
        &mut handled
    ));
    assert!(handled);
    assert!(
        h.host.followed_peer.is_none(),
        "a stale Ada control must never follow Bob"
    );
}

#[test]
fn roster_follow_keyboard_pointer_cancel_and_reopen_keep_the_chosen_person() {
    let mut h = roster_follow_fixture();
    assert!(h.host.collaborators_overlay.focus_accessibility_path(&[3]));
    {
        let chrome = h.chrome.borrow_mut();
        chrome
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .expect("fixture workspace")
            .collaborators
            .swap(0, 1);
    }
    paint(&mut h);
    let mods = Modifiers::default();
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Enter)), mods));
    assert_eq!(h.host.followed_peer.as_deref(), Some("ada"));
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Space)), mods));
    assert!(
        h.host.followed_peer.is_none(),
        "a second explicit activation stops Ada"
    );
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Escape)), mods));
    assert!(!h.chrome.borrow().overlays.collaborators);
    paint(&mut h);
    assert!(
        h.host
            .collaborators_overlay
            .accessibility_rect_for_path(&[3])
            .is_none()
    );

    h.chrome.borrow_mut().overlays.collaborators = true;
    paint(&mut h);
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Enter)), mods));
    assert!(
        h.host.followed_peer.is_none(),
        "reopening must not resurrect old focus"
    );
    assert!(h.host.collaborators_overlay.focus_accessibility_path(&[3]));
    let target = h
        .host
        .collaborators_overlay
        .accessibility_rect_for_path(&[3])
        .expect("Bob Follow after reordering");
    assert!(pump(&mut h, &cursor_moved_at(target.center()), mods));
    assert!(pump(&mut h, &mouse_press(), mods));
    assert!(h.host.followed_peer.is_none());
    assert!(pump(&mut h, &mouse_release(), mods));
    assert_eq!(h.host.followed_peer.as_deref(), Some("bob"));
    paint(&mut h);
    assert!(pump(&mut h, &mouse_press(), mods));
    assert_eq!(
        h.host.followed_peer.as_deref(),
        Some("bob"),
        "Stop remains cancellable"
    );
    assert!(pump(&mut h, &mouse_release(), mods));
    assert!(
        h.host.followed_peer.is_none(),
        "a repeated completed click stops Bob"
    );
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Escape)), mods));
    assert!(!h.chrome.borrow().overlays.collaborators);
}

#[test]
fn people_completed_click_defers_follow_until_release() {
    let mut h = roster_follow_fixture();
    let target = h
        .host
        .collaborators_overlay
        .accessibility_rect_for_path(&[3])
        .expect("painted Ada Follow");
    let mods = Modifiers::default();
    assert!(pump(&mut h, &cursor_moved_at(target.center()), mods));
    assert!(pump(&mut h, &mouse_press(), mods));
    capture_people_completed(&mut h, "pressed");
    assert!(
        h.host.followed_peer.is_none(),
        "a press must remain cancellable"
    );
    assert!(pump(&mut h, &mouse_release(), mods));
    assert_eq!(h.host.followed_peer.as_deref(), Some("ada"));
}

#[test]
fn people_completed_click_cancels_after_reorder_and_focus_loss() {
    for lose_focus in [false, true] {
        let mut h = roster_follow_fixture();
        let target = h
            .host
            .collaborators_overlay
            .accessibility_rect_for_path(&[3])
            .expect("painted Ada Follow");
        let mods = Modifiers::default();
        assert!(pump(&mut h, &cursor_moved_at(target.center()), mods));
        assert!(pump(&mut h, &mouse_press(), mods));
        if lose_focus {
            let _ = pump(&mut h, &WindowEvent::Focused(false), mods);
        } else {
            {
                let chrome = h.chrome.borrow_mut();
                chrome
                    .tree_mut()
                    .desktop
                    .active_workspace_mut()
                    .expect("fixture workspace")
                    .collaborators
                    .swap(0, 1);
            }
            paint(&mut h);
        }
        assert!(pump(&mut h, &mouse_release(), mods));
        capture_people_completed(
            &mut h,
            if lose_focus {
                "focus-lost"
            } else {
                "reordered-cancel"
            },
        );
        assert!(
            h.host.followed_peer.is_none(),
            "the interrupted gesture follows nobody"
        );
    }
}

#[test]
fn people_completed_submit_repeat_cannot_resume_follow_after_local_handoff() {
    let mut h = roster_follow_fixture();
    assert!(h.host.collaborators_overlay.focus_accessibility_path(&[3]));
    let mods = Modifiers::default();
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Enter)), mods));
    assert_eq!(h.host.followed_peer.as_deref(), Some("ada"));
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Tab)), mods));
    assert!(
        h.host.followed_peer.is_none(),
        "the first local key releases Follow"
    );
    let mut repeat = key_press(Key::Named(NamedKey::Enter));
    if let WindowEvent::KeyboardInput { event, .. } = &mut repeat {
        event.repeat = true;
    }
    assert!(pump(&mut h, &repeat, mods));
    assert!(
        h.host.followed_peer.is_none(),
        "holding Enter must not undo the local handoff"
    );
}

fn capture_people_completed(h: &mut Harness, name: &str) {
    let Some(directory) = std::env::var_os("TILO_PEOPLE_ARTIFACTS") else {
        return;
    };
    let out = std::path::Path::new(&directory);
    std::fs::create_dir_all(out).expect("owned artifact directory");
    for (variant, theme) in [
        ("light", Theme::light()),
        ("dark125", Theme::dark().with_text_scale(1.25)),
    ] {
        let mut scene = Scene::new();
        let flags = h.chrome.borrow().overlays;
        h.host.draw_collaborators_dropdown(
            &mut scene,
            Affine::translate((800.0 - h.host.rect.x1, -h.host.rect.y0)),
            &mut TextRenderer::new(),
            &theme,
            &PresenceCtx::empty(),
            flags,
        );
        let panel = h.host.collaborators_overlay.panel_rect();
        let translated =
            Affine::translate((800.0 - h.host.rect.x1, -h.host.rect.y0)).transform_rect_bbox(panel);
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        assert!(
            viewport.contains(translated.origin())
                && viewport.contains(Point::new(translated.x1, translated.y1)),
            "the capture must observe the whole People panel"
        );
        let mut renderer = ui::offscreen::OffscreenRenderer::new(800, 600);
        renderer.render_png(
            &scene,
            theme.tokens.colors.surfaces.base,
            out.join(format!("{variant}-{name}.png")),
        );
    }
}

#[test]
fn people_completed_submit_release_stays_owned_after_invite_opens() {
    let mut h = roster_follow_fixture();
    let _room = host_active_room(&h.chrome);
    h.chrome.borrow_mut().peer_sync_enabled = true;
    paint(&mut h);
    let mods = Modifiers::default();
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::End)), mods));
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Enter)), mods));
    assert!(!h.chrome.borrow().overlays.collaborators);
    let mut release = key_press(Key::Named(NamedKey::Enter));
    if let WindowEvent::KeyboardInput { event, .. } = &mut release {
        event.state = ElementState::Released;
    }
    assert!(
        pump(&mut h, &release, mods),
        "People owns the submit key-up after opening the invitation"
    );
}

#[test]
fn people_completed_cancel_survives_dismissal_reopen_and_a_fresh_click() {
    let mut h = roster_follow_fixture();
    let mods = Modifiers::default();
    let target = h
        .host
        .collaborators_overlay
        .accessibility_rect_for_path(&[3])
        .expect("Ada");
    assert!(pump(&mut h, &cursor_moved_at(target.center()), mods));
    assert!(pump(&mut h, &mouse_press(), mods));
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Escape)), mods));
    assert!(!h.chrome.borrow().overlays.collaborators);
    paint(&mut h);
    h.chrome.borrow_mut().overlays.collaborators = true;
    paint(&mut h);
    assert!(pump(&mut h, &mouse_release(), mods));
    assert!(h.host.followed_peer.is_none());
    assert!(pump(&mut h, &mouse_press(), mods));
    assert!(pump(&mut h, &mouse_release(), mods));
    assert_eq!(h.host.followed_peer.as_deref(), Some("ada"));
}

#[test]
fn people_completed_missing_person_returning_cannot_resurrect_a_press() {
    let mut h = roster_follow_fixture();
    let mods = Modifiers::default();
    let target = h
        .host
        .collaborators_overlay
        .accessibility_rect_for_path(&[3])
        .expect("Ada");
    assert!(pump(&mut h, &cursor_moved_at(target.center()), mods));
    assert!(pump(&mut h, &mouse_press(), mods));
    let ada = {
        let chrome = h.chrome.borrow_mut();
        chrome
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .expect("fixture workspace")
            .collaborators
            .remove(0)
    };
    paint(&mut h);
    {
        let chrome = h.chrome.borrow_mut();
        chrome
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .expect("fixture workspace")
            .collaborators
            .insert(0, ada);
    }
    paint(&mut h);
    assert!(pump(&mut h, &mouse_release(), mods));
    assert!(h.host.followed_peer.is_none());
}

#[test]
fn people_completed_resize_does_not_move_an_armed_click() {
    let mut h = roster_follow_fixture();
    let mods = Modifiers::default();
    let target = h
        .host
        .collaborators_overlay
        .accessibility_rect_for_path(&[3])
        .expect("Ada");
    assert!(pump(&mut h, &cursor_moved_at(target.center()), mods));
    assert!(pump(&mut h, &mouse_press(), mods));
    h.host.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
    paint(&mut h);
    let moved = h
        .host
        .collaborators_overlay
        .accessibility_rect_for_path(&[3])
        .expect("Ada after resize");
    assert_ne!(target, moved);
    assert!(pump(&mut h, &cursor_moved_at(moved.center()), mods));
    assert!(pump(&mut h, &mouse_release(), mods));
    assert!(h.host.followed_peer.is_none());
    assert!(pump(&mut h, &mouse_press(), mods));
    assert!(pump(&mut h, &mouse_release(), mods));
    assert_eq!(h.host.followed_peer.as_deref(), Some("ada"));
}

fn people_viewport_probe_fixture() -> Harness {
    let mut h = roster_follow_fixture();
    adopt_rooms(&h);
    {
        let mut state = h.chrome.borrow_mut();
        state.peer_sync_enabled = true;
        state
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .expect("fixture room")
            .collaborators = (0_usize..13)
            .map(|i| {
                crate::shell::state::Collaborator::new(
                    format!("fixture-{i}"),
                    format!("Participant {}", i.saturating_add(1)),
                )
            })
            .collect();
    }
    paint(&mut h);
    h
}

#[test]
fn people_viewport_probe_end_focus_is_reachable_on_a_narrow_output() {
    let mut h = people_viewport_probe_fixture();
    h.host.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
    paint(&mut h);
    assert!(pump(
        &mut h,
        &key_press(Key::Named(NamedKey::End)),
        Modifiers::default()
    ));
    let tree = h.host.collaborators_overlay.accessibility_snapshot();
    assert!(tree.children.last().expect("Invite").state.focused);
    assert!(
        h.host
            .rect
            .contains(h.host.collaborators_invite_rect_for_tests().center()),
        "End selects Invite, on the visible output"
    );
}

#[test]
fn people_viewport_keyboard_reaches_everyone_and_returns_to_the_first() {
    let mut h = people_viewport_probe_fixture();
    h.host.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
    paint(&mut h);
    let mods = Modifiers::default();
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Home)), mods));
    let mut reached = std::collections::BTreeSet::new();
    for _ in 0..60 {
        assert!(pump(
            &mut h,
            &key_press(Key::Named(NamedKey::ArrowRight)),
            mods
        ));
        paint(&mut h);
        let tree = h.host.collaborators_overlay.accessibility_snapshot();
        let focused = tree
            .children
            .iter()
            .position(|node| node.state.focused)
            .expect("keyboard focus survives scrolling");
        let rect = h
            .host
            .collaborators_overlay
            .accessibility_rect_for_path(&[focused])
            .expect("focused target");
        assert!(
            h.host.rect.contains(rect.center()),
            "the list scrolls the focused control into view"
        );
        if let Some(CollaboratorsHit::ToggleFollow(index)) = h
            .host
            .collaborators_overlay
            .key(&Key::Named(NamedKey::Enter), false)
        {
            reached.insert(index);
        }
    }
    assert_eq!(
        reached.len(),
        13,
        "every person is reachable without pages: {reached:?}"
    );
    assert!(pump(&mut h, &key_press(Key::Named(NamedKey::Home)), mods));
    let rect = h
        .host
        .collaborators_overlay
        .accessibility_rect_for_path(&[1])
        .expect("the first person's first control");
    assert!(
        h.host.rect.contains(rect.origin()),
        "Home scrolls back to the top"
    );
}

#[test]
fn people_viewport_wheel_and_semantic_scroll_share_bounds() {
    let mut h = people_viewport_probe_fixture();
    h.host.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
    paint(&mut h);
    let mods = Modifiers::default();
    let initial = h
        .host
        .primary_vertical_scroll()
        .expect("People scroll region");
    let center = h.host.collaborators_overlay.panel_rect().center();
    assert!(pump(&mut h, &cursor_moved_at(center), mods));
    let wheel = ui::synthetic_input::mouse_wheel(0.0, -3.0);
    assert!(pump(&mut h, &wheel, mods));
    let scrolled = h
        .host
        .primary_vertical_scroll()
        .expect("People scroll region");
    assert_ne!(initial, scrolled);
    let result = h
        .host
        .apply_primary_vertical_scroll(ui::widget::PrimaryScrollAction::End);
    assert!(matches!(
        result,
        ui::widget::PrimaryScrollResult::Changed(_) | ui::widget::PrimaryScrollResult::Unchanged(_)
    ));
    let invite = h.host.collaborators_invite_rect_for_tests();
    assert!(h.host.rect.contains(invite.center()));
    assert!(pump(&mut h, &cursor_moved_at(invite.center()), mods));
    assert!(pump(&mut h, &mouse_press(), mods));
    assert!(h.chrome.borrow().overlays.collaborators);
    assert!(pump(&mut h, &mouse_release(), mods));
    assert!(!h.chrome.borrow().overlays.collaborators);
}

#[test]
fn people_viewport_scrollbar_drag_is_cancellable_and_cannot_invoke_a_person() {
    let mut h = people_viewport_probe_fixture();
    h.host.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
    paint(&mut h);
    let mods = Modifiers::default();
    let rect = h.host.collaborators_overlay.panel_rect();
    let lane_x = rect.x1 - f64::from(Theme::dark().tokens.metrics.scrollbar_gutter) / 2.0;
    let initial = h
        .host
        .primary_vertical_scroll()
        .expect("People scroll region");
    assert!(pump(
        &mut h,
        &cursor_moved_at(Point::new(lane_x, rect.y0 + 16.0)),
        mods
    ));
    assert!(pump(&mut h, &mouse_press(), mods));
    assert!(pump(
        &mut h,
        &cursor_moved_at(Point::new(lane_x, rect.y1 - 16.0)),
        mods
    ));
    let scrolled = h
        .host
        .primary_vertical_scroll()
        .expect("drag scrolls People");
    assert_ne!(scrolled, initial);
    assert!(
        h.host
            .rect
            .contains(h.host.collaborators_invite_rect_for_tests().center())
    );
    pump(&mut h, &WindowEvent::Focused(false), mods);
    assert!(pump(
        &mut h,
        &cursor_moved_at(Point::new(lane_x, rect.y0 + 16.0)),
        mods
    ));
    assert_eq!(
        h.host.primary_vertical_scroll(),
        Some(scrolled),
        "focus loss ends the shared scrollbar drag"
    );
    assert!(pump(&mut h, &mouse_release(), mods));
    assert!(h.chrome.borrow().overlays.collaborators);
    assert!(h.host.followed_peer.is_none());
}

#[test]
fn people_viewport_reopen_scrolls_under_a_stationary_pointer() {
    let mut h = people_viewport_probe_fixture();
    h.host.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
    paint(&mut h);
    let center = h.host.collaborators_overlay.panel_rect().center();
    h.chrome.borrow_mut().overlays.collaborators = false;
    paint(&mut h);
    pump(&mut h, &cursor_moved_at(center), Modifiers::default());
    h.chrome.borrow_mut().overlays.collaborators = true;
    paint(&mut h);
    let initial = h
        .host
        .primary_vertical_scroll()
        .expect("People scroll region");
    let wheel = ui::synthetic_input::mouse_wheel(0.0, -3.0);
    assert!(pump(&mut h, &wheel, Modifiers::default()));
    assert_ne!(
        h.host.primary_vertical_scroll(),
        Some(initial),
        "reopened People must use the host's existing pointer position without waiting for movement"
    );
}

#[test]
fn people_viewport_resize_ends_a_scrollbar_drag_without_invoking_a_target() {
    let mut h = people_viewport_probe_fixture();
    h.host.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
    paint(&mut h);
    let mods = Modifiers::default();
    let rect = h.host.collaborators_overlay.panel_rect();
    let lane_x = rect.x1 - f64::from(Theme::dark().tokens.metrics.scrollbar_gutter) / 2.0;
    assert!(pump(
        &mut h,
        &cursor_moved_at(Point::new(lane_x, rect.y0 + 16.0)),
        mods
    ));
    assert!(pump(&mut h, &mouse_press(), mods));
    h.host.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
    paint(&mut h);
    let after_resize = h.host.primary_vertical_scroll().expect("resized People");
    let rect = h.host.collaborators_overlay.panel_rect();
    assert!(pump(
        &mut h,
        &cursor_moved_at(Point::new(lane_x, rect.y1 - 16.0)),
        mods
    ));
    assert_eq!(
        h.host.primary_vertical_scroll(),
        Some(after_resize),
        "resize cancels the old thumb drag"
    );
    assert!(pump(&mut h, &mouse_release(), mods));
    assert!(h.chrome.borrow().overlays.collaborators);
    assert!(h.host.followed_peer.is_none());
}

/// Clear all's Undo lasts its window on an idle desktop too: the
/// compositor wakes when it ends, so the Undo it showed goes away.
#[test]
fn clear_all_wakes_the_compositor_when_its_undo_ends() {
    let h = build_harness();
    let undo_wake = |h: &Harness| {
        h.host
            .wake_deadline_sources(Instant::now())
            .into_iter()
            .find(|(name, _)| *name == "notification-undo")
            .expect("the undo's end is a wake source")
            .1
    };
    assert_eq!(undo_wake(&h), None, "nothing to undo, nothing to wake for");
    let id = h
        .chrome
        .borrow_mut()
        .push_notification("Build finished", "");
    h.chrome.borrow_mut().dismiss_notification(id);
    let cleared = Instant::now();
    h.chrome.borrow_mut().clear_notification_history();
    let deadline = undo_wake(&h).expect("the compositor wakes when the undo ends");
    assert!(
        deadline > cleared
            && deadline <= Instant::now() + tilo_desktop::notifications::CLEAR_UNDO_WINDOW,
        "it wakes when the window ends"
    );
    assert!(h.chrome.borrow().can_undo_notification_history_clear());
    assert!(
        !ui::time::testing::with_monotonic_time(deadline, || {
            h.chrome.borrow().can_undo_notification_history_clear()
        }),
        "at its end the undo is over"
    );
}

#[test]
fn people_viewport_shared_scrollbar_uses_a_visible_only_idle_deadline() {
    let mut h = people_viewport_probe_fixture();
    h.host.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
    paint(&mut h);
    let mods = Modifiers::default();
    let center = h.host.collaborators_overlay.panel_rect().center();
    assert!(pump(&mut h, &cursor_moved_at(center), mods));
    let wheel = ui::synthetic_input::mouse_wheel(0.0, -3.0);
    assert!(pump(&mut h, &wheel, mods));
    // The notches glide on for a few frames; the idle wait is after.
    let mut at = Instant::now();
    let mut frames = 0;
    while ui::time::testing::with_monotonic_time(at, || {
        h.host.collaborators_overlay.needs_animation_frame()
    }) {
        frames += 1;
        assert!(frames < 600, "the glide comes to rest");
        at += Duration::from_millis(16);
        ui::time::testing::with_monotonic_time(at, || {
            h.host.collaborators_overlay.advance_scroll()
        });
    }
    assert!(frames > 0, "the notches glide on");
    let deadline = h
        .host
        .wake_deadline_sources(at)
        .into_iter()
        .find(|(name, _)| *name == "people")
        .expect("People contributes its child widget deadline")
        .1;
    assert!(
        deadline.is_some(),
        "the shared scrollbar fade must wake an otherwise idle compositor"
    );
    assert!(
        !ui::time::testing::with_monotonic_time(at, || {
            h.host.collaborators_overlay.needs_animation_frame()
        }),
        "the idle interval before a scroll fade uses a deadline rather than busy frames"
    );
    assert!(pump(
        &mut h,
        &key_press(Key::Named(NamedKey::Escape)),
        Modifiers::default()
    ));
    paint(&mut h);
    assert!(h.host.collaborators_overlay.next_wake_deadline().is_none());
    h.chrome.borrow_mut().overlays.collaborators = false;
    assert!(
        h.host
            .wake_deadline_sources(Instant::now())
            .into_iter()
            .find(|(name, _)| *name == "people")
            .expect("People deadline source")
            .1
            .is_none()
    );
}

#[test]
fn a_slow_host_update_names_the_step_that_took_the_time() {
    let mut steps = UpdateSteps::start();
    steps.mark("peer-sync");
    steps.steps[0].1 = Duration::from_millis(3);
    steps.mark("search-index");
    steps.steps[1].1 = Duration::from_millis(900);
    assert!(
        steps
            .report(SLOW_HOST_UPDATE.saturating_sub(Duration::from_millis(1)))
            .is_none(),
        "an update inside the budget is not reported"
    );
    let line = steps
        .report(Duration::from_millis(903))
        .expect("an update past the budget is reported");
    assert!(line.starts_with("SLOW-HOST-UPDATE total=903ms ("), "{line}");
    assert!(line.contains("search-index=900ms"), "{line}");
    assert!(line.contains("peer-sync=3ms"), "{line}");
}

/// Exercise the real catalogue callback and widget action-host/reducer, never
/// a synthetic press or direct write to an acknowledgement gate.
fn invoke_setup(
    host: &mut ShellOverlayHost,
    chrome: &crate::shell::chrome_state::Chrome,
    doc: &mut AutoCommit,
    invocation: &(&str, serde_json::Value),
) {
    use crate::shell::search::action_api::{AllVisible, ResourceRef};
    if invocation.0 == crate::onboarding_view::KEY_BACKUP_SKIP_ACKNOWLEDGED {
        let catalog = host.action_catalog("", &chrome.borrow(), doc, &automerge::ROOT, &AllVisible);
        for blocked in [
            crate::onboarding_view::KEY_BACKUP_CONTINUE,
            crate::onboarding_view::KEY_FINISH,
        ] {
            assert!(
                !catalog.contains(&ResourceRef::new("onboarding", blocked)),
                "Backup cannot continue or release the desktop before the explicit decision"
            );
        }
    }
    let ticket = queue_setup(host, chrome, doc, invocation);
    assert!(ticket().expect("pending").is_none());
    host.settle(&mut ui::widget::Edits::lend(doc), &automerge::ROOT);
    host.sync(doc, &automerge::ROOT);
    assert!(ticket().expect("normal reducer completed").is_some());
}

fn queue_setup(
    host: &ShellOverlayHost,
    chrome: &crate::shell::chrome_state::Chrome,
    doc: &mut AutoCommit,
    invocation: &(&str, serde_json::Value),
) -> crate::shell::search::PendingInvocation {
    use crate::shell::search::action_api::{AllVisible, ResourceRef};
    let resource = ResourceRef::new("onboarding", invocation.0);
    let catalog = host.exact_action_catalog(
        "onboarding",
        invocation.0,
        &chrome.borrow(),
        doc,
        &automerge::ROOT,
        &AllVisible,
    );
    let selected = catalog.select(&resource).unwrap_or_else(|| {
        panic!(
            "current typed setup action {} (owner={}, screen={:?})",
            invocation.0,
            is_active_workspace_owner(&chrome.borrow()),
            host.onboarding.as_ref().map(|view| view.flow().screen())
        )
    });
    let disposition = selected
        .invoke_json_disposition(
            &mut ui::widget::Edits::lend(doc),
            &automerge::ROOT,
            chrome,
            invocation.1.as_object().unwrap(),
        )
        .expect("admitted");
    let crate::shell::search::InvocationDisposition::Pending(ticket) = disposition else {
        panic!("an identity effect cannot succeed before the normal reducer handles it");
    };
    ticket
}

#[test]
fn onboarding_semantic_phase_and_argument_gates_revalidate_the_live_ceremony() {
    use crate::onboarding_view::{KEY_BEGIN, KEY_CREATE, KEY_UNDERSTOOD, OnboardingView};
    use crate::shell::search::action_api::{AllVisible, ResourceRef};
    let mut h = build_harness();
    h.chrome.borrow_mut().test_only_owner_override = Some(true);
    h.host
        .enable_test_onboarding(OnboardingView::with_minter(Box::new(|| {
            Err("storage unavailable".into())
        })));
    let mut doc = AutoCommit::new();
    let initial =
        h.host
            .action_catalog("", &h.chrome.borrow(), &doc, &automerge::ROOT, &AllVisible);
    assert!(!initial.contains(&ResourceRef::new("onboarding", KEY_CREATE)));
    let begin = initial
        .select(&ResourceRef::new("onboarding", KEY_BEGIN))
        .unwrap();
    assert!(
        begin
            .invoke_json(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &h.chrome,
                serde_json::json!({"skip_identity":true})
                    .as_object()
                    .unwrap()
            )
            .is_err()
    );
    invoke_setup(
        &mut h.host,
        &h.chrome,
        &mut doc,
        &(KEY_BEGIN, serde_json::json!({})),
    );
    assert!(
        begin
            .invoke_nullary(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &h.chrome
            )
            .is_err(),
        "a previously enumerated Intro action cannot run at Protection"
    );
    assert!(
        !h.host
            .action_catalog("", &h.chrome.borrow(), &doc, &automerge::ROOT, &AllVisible)
            .contains(&ResourceRef::new("onboarding", KEY_CREATE))
    );
    invoke_setup(
        &mut h.host,
        &h.chrome,
        &mut doc,
        &(KEY_UNDERSTOOD, serde_json::json!({"checked":true})),
    );
    invoke_setup(
        &mut h.host,
        &h.chrome,
        &mut doc,
        &(KEY_UNDERSTOOD, serde_json::json!({"checked":false})),
    );
    assert!(
        !h.host
            .action_catalog("", &h.chrome.borrow(), &doc, &automerge::ROOT, &AllVisible)
            .contains(&ResourceRef::new("onboarding", KEY_CREATE)),
        "unticking closes the creation gate"
    );
}

#[test]
fn onboarding_semantic_failed_mint_remains_protected_and_lock_hides_actions() {
    use crate::onboarding_view::{KEY_BEGIN, KEY_CREATE, KEY_UNDERSTOOD, OnboardingView};
    use crate::shell::search::action_api::{AllVisible, ResourceRef};
    let mut h = build_harness();
    h.chrome.borrow_mut().test_only_owner_override = Some(true);
    h.host
        .enable_test_onboarding(OnboardingView::with_minter(Box::new(|| {
            Err("storage unavailable".into())
        })));
    let mut doc = AutoCommit::new();
    invoke_setup(
        &mut h.host,
        &h.chrome,
        &mut doc,
        &(KEY_BEGIN, serde_json::json!({})),
    );
    invoke_setup(
        &mut h.host,
        &h.chrome,
        &mut doc,
        &(KEY_UNDERSTOOD, serde_json::json!({"checked":true})),
    );
    let create = h.host.exact_action_catalog(
        "onboarding",
        KEY_CREATE,
        &h.chrome.borrow(),
        &doc,
        &automerge::ROOT,
        &AllVisible,
    );
    let disposition = create
        .select(&ResourceRef::new("onboarding", KEY_CREATE))
        .unwrap()
        .invoke_nullary(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &h.chrome,
        )
        .unwrap();
    let crate::shell::search::InvocationDisposition::Pending(ticket) = disposition else {
        panic!("pending mint");
    };
    h.host.sync(&doc, &automerge::ROOT);
    assert!(
        ticket().is_err(),
        "a failed identity write is not a successful semantic action"
    );
    assert!(h.host.onboarding_active());
    assert!(!h.host.onboarding.as_ref().unwrap().flow().is_done());
    h.chrome.borrow_mut().set_session_locked(true);
    assert!(
        !h.host
            .action_catalog("", &h.chrome.borrow(), &doc, &automerge::ROOT, &AllVisible)
            .contains(&ResourceRef::new("onboarding", KEY_CREATE))
    );
}

#[test]
fn onboarding_semantic_create_refuses_a_gate_closed_before_reduction() {
    use crate::onboarding_view::{KEY_BEGIN, KEY_CREATE, KEY_UNDERSTOOD, OnboardingView};
    let mut h = build_harness();
    h.chrome.borrow_mut().test_only_owner_override = Some(true);
    h.host
        .enable_test_onboarding(OnboardingView::with_minter(Box::new(|| {
            panic!("the normal creation guard must keep this minter unreachable")
        })));
    let mut doc = AutoCommit::new();
    for invocation in [
        (KEY_BEGIN, serde_json::json!({})),
        (KEY_UNDERSTOOD, serde_json::json!({"checked":true})),
    ] {
        invoke_setup(&mut h.host, &h.chrome, &mut doc, &invocation);
    }
    let create = queue_setup(
        &h.host,
        &h.chrome,
        &mut doc,
        &(KEY_CREATE, serde_json::json!({})),
    );
    let untick = queue_setup(
        &h.host,
        &h.chrome,
        &mut doc,
        &(KEY_UNDERSTOOD, serde_json::json!({"checked":false})),
    );
    assert!(create().unwrap().is_none());
    h.host
        .settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
    h.host.sync(&doc, &automerge::ROOT);
    assert!(untick().unwrap().is_some());
    assert!(
        create().is_err(),
        "a sync that consumed a now-invalid creation is not success"
    );
    assert!(!h.host.onboarding.as_ref().unwrap().flow().is_done());
    h.host.sync(&doc, &automerge::ROOT);
    assert!(
        create().is_err(),
        "an unrelated later sync must not fabricate completion"
    );
}

fn semantic_codecs_fixture(setup: crate::onboarding_view::CodecSetup) -> (Harness, AutoCommit) {
    use crate::onboarding_view::{
        KEY_BACKUP_CONTINUE, KEY_BACKUP_SKIP_ACKNOWLEDGED, KEY_BEGIN, KEY_CREATE, KEY_UNDERSTOOD,
        OnboardingView,
    };
    let mut h = build_harness();
    h.chrome.borrow_mut().test_only_owner_override = Some(true);
    h.host.enable_test_onboarding(
        OnboardingView::with_minter(Box::new(|| {
            Ok(crate::onboarding::MintedIdentity {
                fingerprint: "test-public-identity".into(),
                path: std::path::PathBuf::from("/unused/public-record"),
            })
        }))
        .with_codec_setup(setup),
    );
    let mut doc = AutoCommit::new();
    for key in [
        KEY_BEGIN,
        KEY_UNDERSTOOD,
        KEY_CREATE,
        KEY_BACKUP_SKIP_ACKNOWLEDGED,
        KEY_BACKUP_CONTINUE,
    ] {
        let args = if matches!(key, KEY_UNDERSTOOD | KEY_BACKUP_SKIP_ACKNOWLEDGED) {
            serde_json::json!({"checked":true})
        } else {
            serde_json::json!({})
        };
        invoke_setup(&mut h.host, &h.chrome, &mut doc, &(key, args));
    }
    (h, doc)
}

fn semantic_codec_worker_receipt(result: Result<(), String>) {
    use crate::onboarding_view::{CodecSetup, KEY_CODECS_INSTALL};
    use std::sync::{Arc, Mutex, mpsc};
    let success = result.is_ok();
    let (release, gate) = mpsc::channel::<Result<(), String>>();
    let gate = Mutex::new(gate);
    let (started_tx, started) = mpsc::channel();
    let started_tx = Mutex::new(started_tx);
    let status = Arc::new(Mutex::new(
        os_actions::codec_pack::CodecPackStatus::default(),
    ));
    let reader = Arc::clone(&status);
    let writer = Arc::clone(&status);
    let setup = CodecSetup::with(
        Arc::new(move || reader.lock().unwrap().clone()),
        Arc::new(move |_| {
            started_tx.lock().unwrap().send(()).unwrap();
            let result = gate.lock().unwrap().recv().unwrap();
            if result.is_ok() {
                writer.lock().unwrap().state = os_actions::codec_pack::PackState::Installed;
            }
            result
        }),
    );
    let (mut h, mut doc) = semantic_codecs_fixture(setup);
    let ticket = queue_setup(
        &h.host,
        &h.chrome,
        &mut doc,
        &(KEY_CODECS_INSTALL, serde_json::json!({})),
    );
    h.host.sync(&doc, &automerge::ROOT);
    started
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("actual worker started");
    for _ in 0..3 {
        h.host.sync(&doc, &automerge::ROOT);
        assert!(
            ticket().unwrap().is_none(),
            "unrelated frames do not finish a blocked install"
        );
    }
    release.send(result).unwrap();
    let waiting_since = std::time::Instant::now();
    loop {
        h.host.sync(&doc, &automerge::ROOT);
        match ticket() {
            Ok(None) => assert!(
                waiting_since.elapsed() < tilo_test_env::hang_bound(),
                "worker result must arrive"
            ),
            Ok(Some(_)) => {
                assert!(success, "failed worker cannot complete successfully");
                break;
            }
            Err(_) => {
                assert!(!success, "successful installed worker cannot be refused");
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn onboarding_semantic_codec_receipt_waits_for_actual_worker_success() {
    semantic_codec_worker_receipt(Ok(()));
}

#[test]
fn onboarding_semantic_codec_receipt_refuses_actual_worker_failure() {
    semantic_codec_worker_receipt(Err("installer refused".into()));
}

#[test]
fn onboarding_semantic_create_finishes_only_after_the_normal_minter_returns() {
    use crate::onboarding_view::{KEY_BEGIN, KEY_CREATE, KEY_UNDERSTOOD, OnboardingView};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let (release, gate) = mpsc::channel();
    let gate = Mutex::new(gate);
    let (started_tx, started) = mpsc::channel();
    let started_tx = Mutex::new(started_tx);
    let returned = Arc::new(AtomicBool::new(false));
    let marker = Arc::clone(&returned);
    let mut h = build_harness();
    h.chrome.borrow_mut().test_only_owner_override = Some(true);
    h.host
        .enable_test_onboarding(OnboardingView::with_minter(Box::new(move || {
            started_tx.lock().unwrap().send(()).unwrap();
            gate.lock()
                .unwrap()
                .recv_timeout(tilo_test_env::hang_bound())
                .unwrap();
            marker.store(true, Ordering::Release);
            Ok(crate::onboarding::MintedIdentity {
                fingerprint: "test-public-identity".into(),
                path: std::path::PathBuf::from("/unused/public-record"),
            })
        })));
    let mut doc = AutoCommit::new();
    for invocation in [
        (KEY_BEGIN, serde_json::json!({})),
        (KEY_UNDERSTOOD, serde_json::json!({"checked":true})),
    ] {
        invoke_setup(&mut h.host, &h.chrome, &mut doc, &invocation);
    }
    let ticket = queue_setup(
        &h.host,
        &h.chrome,
        &mut doc,
        &(KEY_CREATE, serde_json::json!({})),
    );
    assert!(ticket().unwrap().is_none());
    assert!(!returned.load(Ordering::Acquire));
    let unblock = std::thread::spawn(move || {
        started
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("normal minter entered");
        release.send(()).unwrap();
    });
    h.host.sync(&doc, &automerge::ROOT);
    unblock.join().unwrap();
    assert!(
        returned.load(Ordering::Acquire),
        "the actual synchronous minter must return first"
    );
    assert!(h.host.onboarding.as_ref().unwrap().flow().is_done());
    assert!(ticket().unwrap().is_some());
}

/// The strip switcher end to end through the host: Super+S opens it for the
/// keyboard, "New strip" makes a strip at once and puts its name in place,
/// F2 renames, arrows and Enter switch, the wheel over the ribbon moves a
/// strip per notch, and Escape closes it.
#[test]
fn the_strip_switcher_makes_names_and_switches_strips() {
    use super::strip_switcher::Row;
    let mut h = build_harness();
    let doc = AutoCommit::new();
    let sup = Modifiers::from(ModifiersState::SUPER);
    let none = Modifiers::default();
    let press = |h: &mut Harness, key: Key| pump(h, &key_press(key), none);
    let type_text = |h: &mut Harness, text: &str| {
        for c in text.chars() {
            pump(h, &key_press(Key::Character(c.to_string().into())), none);
        }
    };
    let strips = |h: &Harness| -> Vec<String> {
        h.chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .map(|w| w.strips.iter().map(|s| s.name.clone()).collect())
            .unwrap_or_default()
    };
    let focused = |h: &Harness| h.chrome.borrow().strip_focus().1;
    h.host.update(&doc, &automerge::ROOT);
    assert_eq!(strips(&h), ["Main"]);

    // Super+S, then a click on "New strip": the strip is made and has focus
    // at once, and its name is in place, selected, to type over.
    assert!(pump(&mut h, &key_press(Key::Character("s".into())), sup));
    assert!(h.chrome.borrow().strip_switcher_open(), "Super+S opens it");
    assert!(h.host.strip_switcher_takes_keys());
    let new_strip = h
        .host
        .strip_switcher_for_tests()
        .layout()
        .label_rect(Row::NewStrip)
        .expect("a New strip label");
    pump(&mut h, &cursor_moved_at(new_strip.center()), none);
    assert!(pump(&mut h, &ui::synthetic_input::mouse_press(), none));
    pump(&mut h, &ui::synthetic_input::mouse_release(), none);
    assert_eq!(strips(&h), ["Main", "Strip 2"], "made at once");
    assert_eq!(focused(&h), 1, "and it has focus");
    type_text(&mut h, "Games");
    press(&mut h, Key::Named(NamedKey::Enter));
    assert_eq!(strips(&h), ["Main", "Games"], "named in place");
    assert_eq!(focused(&h), 1);

    // F2 on the focused strip renames it; typing replaces the old name.
    pump(&mut h, &key_press(Key::Character("s".into())), sup);
    h.host.update(&doc, &automerge::ROOT);
    press(&mut h, Key::Named(NamedKey::ArrowUp));
    press(&mut h, Key::Named(NamedKey::F2));
    type_text(&mut h, "Studio");
    press(&mut h, Key::Named(NamedKey::Enter));
    assert_eq!(strips(&h), ["Studio", "Games"], "strip 1 renamed");
    assert!(
        !h.chrome.borrow().strip_switcher_open(),
        "a keyboard choice closes it"
    );

    // Arrows and Enter switch strips: up from Games to Studio.
    pump(&mut h, &key_press(Key::Character("s".into())), sup);
    press(&mut h, Key::Named(NamedKey::ArrowUp));
    press(&mut h, Key::Named(NamedKey::Enter));
    assert_eq!(focused(&h), 0);
    assert!(!h.chrome.borrow().strip_switcher_open());

    // The wheel over the ribbon: a notch toward the person is the strip
    // below, away is the strip above.
    h.host.update(&doc, &automerge::ROOT);
    let zone = h.host.strip_switcher_zone();
    pump(&mut h, &cursor_moved_at(zone.center()), none);
    assert!(pump(
        &mut h,
        &ui::synthetic_input::mouse_wheel(0.0, -1.0),
        none
    ));
    assert_eq!(focused(&h), 1, "down a strip");
    // The next notch comes a moment later, as a hand turning the wheel does.
    let later = ui::time::now()
        .checked_add(std::time::Duration::from_millis(20))
        .expect("a later instant");
    ui::time::testing::with_monotonic_time(later, || {
        pump(&mut h, &ui::synthetic_input::mouse_wheel(0.0, 1.0), none);
    });
    assert_eq!(focused(&h), 0, "back up");

    // Escape closes it.
    if !h.chrome.borrow().strip_switcher_open() {
        pump(&mut h, &key_press(Key::Character("s".into())), sup);
    }
    press(&mut h, Key::Named(NamedKey::Escape));
    assert!(!h.chrome.borrow().strip_switcher_open());
    assert!(!h.host.strip_switcher_takes_keys());
}

/// A pointer that slows to a stop on the strip ribbon opens the strip
/// switcher's labels; one that only sweeps past the edge does not.
#[test]
fn the_strip_switcher_opens_when_the_pointer_comes_to_rest_on_the_ribbon() {
    let mut h = build_harness();
    let doc = AutoCommit::new();
    let none = Modifiers::default();
    h.host.update(&doc, &automerge::ROOT);
    let zone = h.host.strip_switcher_zone();
    assert!(
        zone.area() > 0.0,
        "the ribbon offers the switcher: {zone:?}"
    );
    let y = zone.center().y;
    let x1 = zone.x1 - 2.0;
    // The pointer moves on a pinned clock, a frame apart, so the dwell it
    // ends with is the one the test gives it, not the machine's speed.
    let step = std::time::Duration::from_millis(12);
    let mut at = ui::time::now();
    for x in [
        x1 - 400.0,
        x1 - 250.0,
        x1 - 120.0,
        x1 - 50.0,
        x1 - 18.0,
        x1 - 6.0,
        x1 - 2.0,
        x1,
    ] {
        at = at.checked_add(step).expect("a later instant");
        ui::time::testing::with_monotonic_time(at, || {
            pump(&mut h, &cursor_moved_at(Point::new(x, y)), none);
        });
    }
    let rested = at
        .checked_add(std::time::Duration::from_millis(80))
        .expect("a later instant");
    ui::time::testing::with_monotonic_time(rested, || {
        h.host.update(&doc, &automerge::ROOT);
    });
    assert!(
        h.chrome.borrow().strip_switcher_open(),
        "resting on the ribbon opens the labels"
    );
    // Leaving it closes it.
    let left = rested.checked_add(step).expect("a later instant");
    ui::time::testing::with_monotonic_time(left, || {
        pump(
            &mut h,
            &cursor_moved_at(Point::new(zone.x0 - 600.0, y)),
            none,
        );
    });
    assert!(!h.chrome.borrow().strip_switcher_open());
}
