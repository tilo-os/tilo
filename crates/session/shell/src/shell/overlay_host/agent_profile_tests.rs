use super::*;
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::state::{CapOverride, Subject, Verb, Workspace, WorkspaceId, WorkspaceTree};
use std::sync::Arc;
use tilo_desktop::workspace_settings::{AgentLivePhase, AgentTaskAvailability};
use ui::widget::{Container, PrimaryScrollAction, PrimaryScrollResult};

pub(crate) fn fixture(count: usize) -> (ShellOverlayHost, Chrome) {
    let mut workspace = Workspace::new(WorkspaceId(887), "Agent profiles");
    workspace.set_owner("quinten");
    let room = workspace.room.to_string();
    for index in 0..count {
        workspace
            .grant_as(
                &"quinten".into(),
                CapOverride::membership(
                    Subject::agent(format!("scribe-{index:02}")),
                    room.clone(),
                    Verb::Write,
                ),
            )
            .expect("owner admits fixture Agent");
    }
    let mut tree = WorkspaceTree::default();
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    let chrome = ChromeState::new_with_fixture_handle(
        crate::shell::state::WorkspaceTreeHandle::new(tree),
        crate::shell::hosted_inputs::HostedTileInputs::default(),
    );
    chrome.borrow_mut().act_as("quinten".into());
    chrome.borrow_mut().overlays.ws_settings = true;
    chrome.borrow_mut().ws_settings_tab = WsTab::Agents;
    let host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(chrome.clone());
    (host, chrome)
}

fn interaction() -> tilo_actions::SemanticInteraction {
    tilo_actions::SemanticInteraction {
        version: tilo_actions::INTERACTION_VERSION,
        invocation: tilo_actions::InvocationId::new(),
        sequence: 1,
        actor: tilo_actions::InteractionActor {
            id: "agent:scribe-00".into(),
            kind: tilo_actions::InteractionActorKind::Agent,
        },
        phase: tilo_actions::InteractionPhase::Progress {
            label: "PRIVATE progress".into(),
            completed: Some(9),
            total: Some(10),
        },
        action: tilo_actions::InteractionAction {
            id: "edit".into(),
            label: "PRIVATE action".into(),
            app: None,
        },
        resource: tilo_actions::ActionResource {
            id: "PRIVATE record".into(),
            kind: "document".into(),
            label: "PRIVATE title".into(),
        },
        anchor: tilo_actions::InteractionAnchor::Resource,
        group: None,
    }
}

pub(crate) fn paint(
    host: &mut ShellOverlayHost,
    presence: &PresenceCtx,
) -> (taffy::TaffyTree<()>, NodeId) {
    paint_with(host, presence, &Theme::dark(), 1200.0, 800.0)
}

pub(crate) fn paint_with(
    host: &mut ShellOverlayHost,
    presence: &PresenceCtx,
    theme: &Theme,
    width: f64,
    height: f64,
) -> (taffy::TaffyTree<()>, NodeId) {
    host.force_ws_settings_settled();
    let layout =
        super::overlay_event_absorb_tests::lay_out_host_with_theme(host, width, height, theme);
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    host.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut text,
        theme,
        presence,
    );
    layout
}

pub(crate) fn find<'a>(
    node: &'a ui::a11y::AccessNode,
    key: &str,
) -> Option<&'a ui::a11y::AccessNode> {
    if node.semantic_key.as_deref() == Some(key) {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, key))
}

#[test]
fn agent_profile_phase_requires_current_exact_room_member_and_expires() {
    let (mut host, chrome) = fixture(1);
    let room = chrome
        .borrow()
        .tree()
        .desktop
        .active_workspace()
        .expect("room")
        .room
        .to_string();
    let mut presence = PresenceCtx::empty();
    presence
        .observe_interaction(interaction())
        .expect("unbound observation");
    host.refresh_agent_profile_activity(&presence);
    assert!(host.agent_profile_activity[0].live_phase.is_none());
    let mut presence = PresenceCtx::empty();
    presence
        .observe_interaction_in_room(interaction(), "wrong-room")
        .expect("other room");
    host.refresh_agent_profile_activity(&presence);
    assert!(host.agent_profile_activity[0].live_phase.is_none());
    let mut presence = PresenceCtx::empty();
    crate::shell::peer_presence::fold_typed_presence(
        vec![(
            "person:quinten".into(),
            PresenceKey::Workspace { id: room.clone() },
            ui::collaboration::PresencePayload::Interaction(Box::new(interaction())),
        )],
        &mut presence,
        &room,
    );
    host.refresh_agent_profile_activity(&presence);
    assert!(
        host.agent_profile_activity[0].live_phase.is_none(),
        "claimed Agent actor cannot replace authenticated sender"
    );
    let mut presence = PresenceCtx::empty();
    presence
        .observe_interaction_in_room(interaction(), &room)
        .expect("routed observation");
    host.refresh_agent_profile_activity(&presence);
    assert_eq!(
        host.agent_profile_activity[0].live_phase,
        Some(AgentLivePhase::Progress)
    );
    assert!(host.agent_profile_expiry.is_some());
    presence.prune_interactions(ui::time::now() + std::time::Duration::from_secs(120));
    host.refresh_agent_profile_activity(&presence);
    assert!(host.agent_profile_activity[0].live_phase.is_none());
    assert!(host.agent_profile_expiry.is_none());
    chrome
        .borrow_mut()
        .tree_mut()
        .desktop
        .active_workspace_mut()
        .expect("room")
        .capabilities
        .clear();
    host.refresh_agent_profile_activity(&presence);
    assert!(
        host.agent_profile_activity.is_empty(),
        "revoked admission retires the profile projection"
    );
}

#[test]
fn profile_history_and_live_accessibility_exclude_private_payloads() {
    let (mut host, chrome) = fixture(1);
    let room = {
        let state = chrome.borrow_mut();
        let mut tree = state.tree_mut();
        let workspace = tree.desktop.active_workspace_mut().expect("room");
        for (actor, at) in [("agent:scribe-00", 42), ("agent:other", 999)] {
            workspace
                .effect_receipts
                .push(tilo_desktop_model::WorkspaceEffectReceipt {
                    invocation: tilo_actions::InvocationId::new(),
                    actor: actor.into(),
                    context: tilo_actions::ActionContext::default(),
                    status: tilo_actions::EffectReceiptStatus::Succeeded,
                    completed_at: at,
                });
        }
        workspace.tasks.push(tilo_desktop_model::WorkspaceTask {
            id: "PRIVATE task".into(),
            title: "PRIVATE instruction".into(),
            created_by: "quinten".into(),
            assigned_agent: Some("scribe-00".into()),
            route: tilo_desktop_model::WorkspaceTaskRoute::Manual,
            status: tilo_desktop_model::WorkspaceTaskStatus::Queued,
            resource: None,
            created_at: 1000,
            updated_at: 2000,
            audit: vec![
                tilo_desktop_model::WorkspaceTaskAuditEvent {
                    at: 43,
                    actor: "agent:scribe-00".into(),
                    summary: "PRIVATE audit".into(),
                },
                tilo_desktop_model::WorkspaceTaskAuditEvent {
                    at: 3000,
                    actor: "quinten".into(),
                    summary: "PRIVATE human".into(),
                },
            ],
        });
        let room = workspace.room.to_string();
        drop(tree);
        room
    };
    let mut presence = PresenceCtx::empty();
    presence
        .observe_interaction_in_room(interaction(), &room)
        .expect("room observation");
    host.refresh_agent_profile_activity(&presence);
    assert_eq!(host.agent_profile_activity[0].last_activity_at, Some(43));
    assert_eq!(
        host.agent_profile_activity[0].task_availability,
        AgentTaskAvailability::Unknown
    );
    let _ = paint(&mut host, &presence);
    let semantics = host.accessibility().expect("profile semantics");
    let node =
        find(&semantics, "workspace-profile:agent:scribe-00").expect("ordinary visible Agent row");
    let row = &host.workspace_settings_overlay.visible_agent_rows()[0];
    let (_, detail) = tilo_desktop::workspace_settings::agent_profile_presentation(row);
    assert_eq!(
        node.description.as_deref(),
        Some(detail.as_str()),
        "paint and AT share one observation/formatter"
    );
    let live =
        find(node, "workspace-profile:agent:scribe-00:activity").expect("status live region");
    assert!(!format!("{semantics:?}").contains("PRIVATE"));
    assert_eq!(live.live, Some(ui::a11y::Politeness::Polite));
    let previous = live.value.clone();
    let _ = paint(&mut host, &presence);
    let unchanged = host.accessibility().expect("unchanged semantics");
    assert_eq!(
        find(&unchanged, "workspace-profile:agent:scribe-00:activity")
            .expect("stable live region")
            .value,
        previous
    );
}

#[test]
fn settled_profile_paints_do_not_query_and_tree_change_refreshes_once() {
    let (mut host, chrome) = fixture(1);
    let presence = PresenceCtx::empty();
    // Mount the fixture's retained workspace before measuring a settled
    // authority snapshot; its initial presentation is not a settled frame.
    let _ = paint(&mut host, &presence);
    let membership = chrome.borrow().tree_handle();
    host.attach_app_action_broker(crate::shell::broker_services::test_broker_with_membership(
        membership,
    ));
    host.refresh_agent_profile_activity(&presence);
    assert_eq!(
        host.agent_profile_query_count, 1,
        "initial actual broker query"
    );
    assert_eq!(
        host.agent_profile_activity[0].task_availability,
        AgentTaskAvailability::Unavailable
    );
    for _ in 0..3 {
        let _ = paint(&mut host, &presence);
        host.refresh_agent_profile_activity(&presence);
    }
    assert_eq!(
        host.agent_profile_query_count, 1,
        "settled frames cannot poll broker authority"
    );
    chrome
        .borrow_mut()
        .tree_mut()
        .desktop
        .active_workspace_mut()
        .expect("room")
        .name = "Updated room".into();
    host.refresh_agent_profile_activity(&presence);
    assert_eq!(
        host.agent_profile_query_count, 2,
        "one tree generation edge refreshes once"
    );
    host.refresh_agent_profile_activity(&presence);
    assert_eq!(host.agent_profile_query_count, 2);
    chrome.borrow_mut().overlays.ws_settings = false;
    host.refresh_agent_profile_activity(&presence);
    assert!(host.agent_profile_expiry.is_none());
}

#[test]
fn semantic_profile_scroll_reaches_last_row_without_offscreen_accessibility() {
    let (mut host, _) = fixture(13);
    let presence = PresenceCtx::empty();
    host.refresh_agent_profile_activity(&presence);
    let _ = paint(&mut host, &presence);
    let first = host.accessibility().expect("first page");
    assert!(find(&first, "workspace-profile:agent:scribe-00").is_some());
    assert!(find(&first, "workspace-profile:agent:scribe-12").is_none());
    assert!(matches!(
        host.apply_primary_vertical_scroll(PrimaryScrollAction::End),
        PrimaryScrollResult::Changed(_)
    ));
    let _ = paint(&mut host, &presence);
    let last = host.accessibility().expect("last page");
    assert!(find(&last, "workspace-profile:agent:scribe-12").is_some());
    assert!(find(&last, "workspace-profile:agent:scribe-00").is_none());
}

fn dispatch(host: &mut ShellOverlayHost, event: &WindowEvent) -> bool {
    crate::shell::test_support::with_scratch_event_ctx(|ctx| {
        host.handle_event(event, ctx);
        *ctx.handled
    })
}

fn key(named: NamedKey) -> WindowEvent {
    let logical = Key::Named(named);
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event: KeyEvent::synthetic(
            winit::event::RawKeyEvent {
                physical_key: winit::keyboard::PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified,
                ),
                state: ElementState::Pressed,
            },
            logical.clone(),
            None,
            winit::keyboard::KeyLocation::Standard,
            false,
            None,
            logical,
        ),
        is_synthetic: false,
    }
}

#[test]
fn profile_page_keys_and_stationary_pointer_first_wheel_use_shared_region() {
    let (mut host, chrome) = fixture(13);
    let presence = PresenceCtx::empty();
    host.refresh_agent_profile_activity(&presence);
    let _ = paint(&mut host, &presence);
    let origin = host
        .primary_vertical_scroll()
        .expect("actual profile scroll region")
        .metrics
        .offset;
    assert!(dispatch(&mut host, &key(NamedKey::PageDown)));
    assert!(
        host.primary_vertical_scroll()
            .expect("region")
            .metrics
            .offset
            > origin
    );
    assert!(dispatch(&mut host, &key(NamedKey::PageUp)));
    assert_eq!(
        host.primary_vertical_scroll()
            .expect("region")
            .metrics
            .offset,
        origin
    );
    assert!(dispatch(&mut host, &key(NamedKey::End)));
    let _ = paint(&mut host, &presence);
    assert!(
        find(
            &host.accessibility().expect("AT"),
            "workspace-profile:agent:scribe-12"
        )
        .is_some()
    );
    assert!(dispatch(&mut host, &key(NamedKey::Home)));
    let _ = paint(&mut host, &presence);
    let point = host.workspace_settings_overlay.visible_agent_rows()[0]
        .row_rect
        .center();
    let movement = WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    };
    assert!(dispatch(&mut host, &movement));
    chrome.borrow_mut().overlays.ws_settings = false;
    host.refresh_agent_profile_activity(&presence);
    let _ = paint(&mut host, &presence);
    chrome.borrow_mut().overlays.ws_settings = true;
    host.refresh_agent_profile_activity(&presence);
    let _ = paint(&mut host, &presence);
    assert!(
        host.workspace_settings_overlay
            .visible_agent_rows()
            .iter()
            .any(|row| row.row_rect.contains(point)),
        "the reopened clipped viewport must still be underneath the stationary pointer"
    );
    let reopened = host.primary_vertical_scroll().expect("reopened region");
    assert!(
        reopened.metrics.content > reopened.metrics.viewport,
        "the reopened region must still have scrollable content"
    );
    let reopened_origin = reopened.metrics.offset;
    let wheel = WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(0.0, -3.0),
        phase: winit::event::TouchPhase::Moved,
    };
    assert!(
        dispatch(&mut host, &wheel),
        "first wheel after reopen needs no new pointer move"
    );
    assert!(
        host.primary_vertical_scroll()
            .expect("region")
            .metrics
            .offset
            > reopened_origin,
        "the first stationary-pointer wheel must move the reopened region"
    );
}

#[test]
fn compact_high_contrast_profile_boundary_keys_are_absorbed_and_controls_activate() {
    let (mut host, chrome) = fixture(13);
    let presence = PresenceCtx::empty();
    host.refresh_agent_profile_activity(&presence);
    let theme = Theme::high_contrast().with_text_scale(2.0);
    let _ = paint_with(&mut host, &presence, &theme, 800.0, 600.0);
    for key_name in [NamedKey::Home, NamedKey::ArrowUp, NamedKey::PageUp] {
        assert!(
            dispatch(&mut host, &key(key_name)),
            "clamped input stays inside profile"
        );
        assert_eq!(
            host.primary_vertical_scroll()
                .expect("region")
                .metrics
                .offset,
            0.0
        );
    }
    assert!(dispatch(&mut host, &key(NamedKey::End)));
    let end = host
        .primary_vertical_scroll()
        .expect("region")
        .metrics
        .offset;
    for key_name in [NamedKey::End, NamedKey::ArrowDown, NamedKey::PageDown] {
        assert!(dispatch(&mut host, &key(key_name)));
        assert_eq!(
            host.primary_vertical_scroll()
                .expect("region")
                .metrics
                .offset,
            end
        );
    }
    let _ = paint_with(&mut host, &presence, &theme, 800.0, 600.0);
    let tree = host.accessibility().expect("AT");
    assert!(find(&tree, "workspace-profile:agent:scribe-12").is_some());
    assert!(find(&tree, "workspace-profile:agent:scribe-00").is_none());
    assert!(dispatch(&mut host, &key(NamedKey::Home)));
    let target = "workspace-profile-control:agent:scribe-00:review";
    for _ in 0..4 {
        assert!(dispatch(&mut host, &key(NamedKey::ArrowDown)));
        let _ = paint_with(&mut host, &presence, &theme, 800.0, 600.0);
        if find(&host.accessibility().expect("AT"), target).is_some() {
            break;
        }
    }
    let tree = host.accessibility().expect("AT");
    let control =
        find(&tree, target).expect("shared compact scroll exposes the complete Review control");
    assert!(
        control
            .bounds
            .expect("clipped control bounds")
            .to_rect()
            .area()
            > 0.0
    );
    crate::shell::test_support::with_scratch_event_ctx(|ctx| {
        assert!(host.perform_semantic_action(
            target,
            &ui::a11y::Action::Activate,
            &mut ctx.state,
            ctx.path
        ));
    });
    assert_eq!(chrome.borrow().ws_settings_tab, WsTab::Permissions);
    assert!(
        host.perform_profile_control(target, &ui::a11y::Action::Activate),
        "stale target is consumed"
    );
    assert!(dispatch(&mut host, &key(NamedKey::Escape)));
    assert!(
        !chrome.borrow().overlays.ws_settings,
        "Escape restores the underlying desktop route"
    );
}

#[test]
fn compact_profile_page_traversal_exposes_every_member_and_clamps_wheel() {
    let (mut host, _) = fixture(13);
    let presence = PresenceCtx::empty();
    let theme = Theme::high_contrast().with_text_scale(2.0);
    host.refresh_agent_profile_activity(&presence);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..32 {
        let _ = paint_with(&mut host, &presence, &theme, 800.0, 600.0);
        let tree = host.accessibility().expect("AT");
        for index in 0..13 {
            if find(&tree, &format!("workspace-profile:agent:scribe-{index:02}")).is_some() {
                seen.insert(index);
            }
        }
        let before = host
            .primary_vertical_scroll()
            .expect("region")
            .metrics
            .offset;
        assert!(dispatch(&mut host, &key(NamedKey::PageDown)));
        if host
            .primary_vertical_scroll()
            .expect("region")
            .metrics
            .offset
            == before
        {
            break;
        }
    }
    assert_eq!(
        seen,
        (0..13).collect(),
        "every real row remains reachable at HC200"
    );
    let row = host
        .workspace_settings_overlay
        .visible_agent_rows()
        .last()
        .expect("last clipped row");
    let point = row.row_rect.center();
    assert!(dispatch(
        &mut host,
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(point.x, point.y),
        }
    ));
    let before = host
        .primary_vertical_scroll()
        .expect("region")
        .metrics
        .offset;
    assert!(
        dispatch(
            &mut host,
            &WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(0.0, -3.0),
                phase: winit::event::TouchPhase::Moved,
            }
        ),
        "clamped wheel cannot reach the desktop underneath"
    );
    assert_eq!(
        host.primary_vertical_scroll()
            .expect("region")
            .metrics
            .offset,
        before
    );
}

#[test]
fn profile_semantic_focus_blur_scroll_and_row_activation_are_truthful() {
    let (mut host, chrome) = fixture(13);
    let presence = PresenceCtx::empty();
    host.refresh_agent_profile_activity(&presence);
    let _ = paint(&mut host, &presence);
    let target = "workspace-profile-control:agent:scribe-00:review";
    crate::shell::test_support::with_scratch_event_ctx(|ctx| {
        assert!(host.perform_semantic_action(
            target,
            &ui::a11y::Action::Focus,
            &mut ctx.state,
            ctx.path
        ));
    });
    assert!(
        find(&host.accessibility().expect("AT"), target)
            .expect("focused button")
            .state
            .focused
    );
    host.set_focus(false);
    assert!(
        !find(&host.accessibility().expect("AT"), target)
            .expect("blurred button")
            .state
            .focused
    );
    assert!(host.perform_profile_control(target, &ui::a11y::Action::Focus));
    assert!(dispatch(&mut host, &key(NamedKey::End)));
    assert!(host.profile_control_focus.is_none());
    let _ = paint(&mut host, &presence);
    assert!(host.perform_profile_control(target, &ui::a11y::Action::Focus));
    assert!(
        host.profile_control_focus.is_none(),
        "offscreen stale target cannot focus"
    );
    let row = "workspace-profile:agent:scribe-12";
    crate::shell::test_support::with_scratch_event_ctx(|ctx| {
        assert!(host.perform_semantic_action(
            row,
            &ui::a11y::Action::Activate,
            &mut ctx.state,
            ctx.path
        ));
    });
    let _ = paint(&mut host, &presence);
    assert!(
        find(&host.accessibility().expect("AT"), row)
            .expect("exact selected row")
            .state
            .selected
    );
    assert!(dispatch(&mut host, &key(NamedKey::Escape)));
    assert!(!chrome.borrow().overlays.ws_settings);
    assert!(host.profile_control_focus.is_none());
}

#[test]
fn profile_buttons_are_real_and_owner_self_controls_are_disabled() {
    let (mut host, chrome) = fixture(1);
    let presence = PresenceCtx::empty();
    host.refresh_agent_profile_activity(&presence);
    let _ = paint(&mut host, &presence);
    assert!(
        find(
            &host.accessibility().expect("AT"),
            "workspace-profile-control:agent:scribe-00:role"
        )
        .is_none(),
        "Admitted pill is not an action"
    );
    let add = "workspace-profile-control:agents:add";
    assert!(find(&host.accessibility().expect("AT"), add).is_some());
    assert!(host.perform_profile_control(add, &ui::a11y::Action::Activate));
    assert!(
        host.palette_visible,
        "Add agent uses existing typed palette flow"
    );
    host.palette_visible = false;
    chrome.borrow_mut().open_workspace_settings(WsTab::Agents);
    let _ = paint(&mut host, &presence);
    let remove = "workspace-profile-control:agent:scribe-00:remove";
    assert!(host.perform_profile_control(remove, &ui::a11y::Action::Activate));
    assert!(
        chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .expect("workspace")
            .agent_members_in_room_at(
                &host.agent_profile_room.clone().expect("room"),
                tilo_clock::unix_now()
            )
            .is_empty()
    );
    chrome.borrow_mut().open_workspace_settings(WsTab::Members);
    let _ = paint(&mut host, &presence);
    for kind in ["role", "remove"] {
        let target = format!("workspace-profile-control:person:quinten:{kind}");
        let tree = host.accessibility().expect("AT");
        let node = find(&tree, &target).expect("painted disabled control remains discoverable");
        assert!(node.state.disabled);
        assert!(!node.state.focusable);
        assert!(node.action_target.is_none());
        crate::shell::test_support::with_scratch_event_ctx(|ctx| {
            assert!(host.perform_semantic_action(
                &target,
                &ui::a11y::Action::Focus,
                &mut ctx.state,
                ctx.path
            ));
            assert!(host.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut ctx.state,
                ctx.path
            ));
        });
        assert!(host.profile_control_focus.is_none());
        assert!(
            !host.workspace_settings_overlay.member_sheet_is_open(),
            "disabled self/owner controls never open sheets"
        );
    }
}

#[test]
fn stale_room_profile_activity_never_reaches_new_room_paint_or_accessibility() {
    let (mut host, chrome) = fixture(1);
    let presence = PresenceCtx::empty();
    host.refresh_agent_profile_activity(&presence);
    host.agent_profile_activity[0].task_availability = AgentTaskAvailability::Available;
    host.agent_profile_activity[0].live_phase = Some(AgentLivePhase::Applying);
    host.agent_profile_activity[0].last_activity_at = Some(1_000);
    let old_room = host.agent_profile_room.clone().expect("room");
    let mut next = Workspace::new(WorkspaceId(888), "Different exact room");
    next.set_owner("quinten");
    next.grant_as(
        &"quinten".into(),
        CapOverride::membership(
            Subject::agent("scribe-00"),
            next.room.to_string(),
            Verb::Write,
        ),
    )
    .expect("independent current-room membership");
    let new_room = next.room.to_string();
    {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        tree.desktop.active = next.id;
        tree.desktop.workspaces.push(next);
    }
    assert_ne!(old_room, new_room);
    assert!(
        ShellOverlayHost::agent_profile_activity_in_room(
            host.agent_profile_room.as_deref(),
            &host.agent_profile_activity,
            &new_room,
        )
        .is_empty()
    );
    // Paint before the next authority pump, as a room-switch input can do.
    let _ = paint(&mut host, &presence);
    let row = &host.workspace_settings_overlay.visible_agent_rows()[0];
    assert_eq!(
        row.profile_activity.task_availability,
        AgentTaskAvailability::Unknown
    );
    assert!(row.profile_activity.live_phase.is_none());
    assert!(row.profile_activity.last_activity_at.is_none());
    let tree = host.accessibility().expect("AT");
    let agent = find(&tree, "workspace-profile:agent:scribe-00").expect("fresh new-room row");
    assert_eq!(
        agent.description,
        Some(tilo_desktop::workspace_settings::agent_profile_presentation(row).1)
    );
    let live = find(&tree, "workspace-profile:agent:scribe-00:activity").expect("new-room status");
    assert_eq!(
        live.value,
        Some(row.activity_presentation.announcement.clone())
    );
}

#[test]
fn compact_semantic_profile_focus_paints_shared_ring_and_blur_removes_it() {
    let (mut host, _) = fixture(1);
    let theme = Theme::high_contrast().with_text_scale(2.0);
    host.refresh_agent_profile_activity(&PresenceCtx::empty());
    let _ = paint_with(&mut host, &PresenceCtx::empty(), &theme, 800.0, 600.0);
    let mut baseline = Scene::new();
    host.draw_ws_settings_overlay(
        &mut baseline,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        true,
    );
    let target = "workspace-profile-control:agents:add";
    crate::shell::test_support::with_scratch_event_ctx(|ctx| {
        assert!(host.perform_semantic_action(
            target,
            &ui::a11y::Action::Focus,
            &mut ctx.state,
            ctx.path
        ));
    });
    assert!(
        find(&host.accessibility().expect("AT"), target)
            .expect("exact focused control")
            .state
            .focused
    );
    let mut focused = Scene::new();
    host.draw_ws_settings_overlay(
        &mut focused,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        true,
    );
    assert!(
        focused.encoding().draw_tags.len() > baseline.encoding().draw_tags.len(),
        "semantic focus emits the shared painted ring, not metadata alone"
    );
    host.set_focus(false);
    let mut blurred = Scene::new();
    host.draw_ws_settings_overlay(
        &mut blurred,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        true,
    );
    assert_eq!(
        blurred.encoding().draw_tags.len(),
        baseline.encoding().draw_tags.len(),
        "blur removes only focus paint"
    );
}

#[test]
fn profile_relative_activity_ages_without_new_authority_queries() {
    let (mut host, chrome) = fixture(1);
    let membership = chrome.borrow().tree_handle();
    host.attach_app_action_broker(crate::shell::broker_services::test_broker_with_membership(
        membership,
    ));
    host.refresh_agent_profile_activity(&PresenceCtx::empty());
    assert_eq!(host.agent_profile_query_count, 1);
    let instant = ui::time::now();
    let wall = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    host.agent_profile_activity[0].last_activity_at = Some(1_000);
    assert!(host.refresh_agent_profile_presentation(instant, wall, true));
    let _ = paint(&mut host, &PresenceCtx::empty());
    let before = host.accessibility().expect("AT");
    let old = find(&before, "workspace-profile:agent:scribe-00:activity")
        .expect("live activity")
        .value
        .clone();
    assert!(host.refresh_agent_profile_presentation(
        instant + Duration::from_secs(60),
        wall + Duration::from_secs(60),
        false
    ));
    let _ = paint(&mut host, &PresenceCtx::empty());
    let after = host.accessibility().expect("AT");
    assert_ne!(
        old,
        find(&after, "workspace-profile:agent:scribe-00:activity")
            .expect("aged activity")
            .value
    );
    assert_eq!(
        host.agent_profile_query_count, 1,
        "time-only wake cannot reobserve authority"
    );
    chrome.borrow_mut().overlays.ws_settings = false;
    host.refresh_agent_profile_activity(&PresenceCtx::empty());
    assert!(
        host.agent_profile_presentation_expiry.is_none(),
        "closed profile never schedules relative-age wakes"
    );
}

#[test]
fn independent_agents_have_stable_live_names_and_combined_status_values() {
    let (mut host, _) = fixture(2);
    let presence = PresenceCtx::empty();
    host.refresh_agent_profile_activity(&presence);
    for activity in &mut host.agent_profile_activity {
        activity.task_availability = AgentTaskAvailability::Available;
        activity.live_phase = Some(AgentLivePhase::Waiting);
    }
    let _ = paint(&mut host, &presence);
    let before = host.accessibility().expect("AT");
    let first =
        find(&before, "workspace-profile:agent:scribe-00:activity").expect("first live region");
    let second =
        find(&before, "workspace-profile:agent:scribe-01:activity").expect("second live region");
    assert_ne!(
        first.name, second.name,
        "live identity does not collapse equal statuses"
    );
    assert_eq!(first.value, second.value);
    host.agent_profile_activity[0].task_availability = AgentTaskAvailability::Unavailable;
    let _ = paint(&mut host, &presence);
    let after = host.accessibility().expect("AT");
    let changed =
        find(&after, "workspace-profile:agent:scribe-00:activity").expect("changed region");
    let unchanged =
        find(&after, "workspace-profile:agent:scribe-01:activity").expect("other region");
    assert_eq!(first.name, changed.name);
    assert_ne!(
        first.value, changed.value,
        "availability announces even with unchanged live phase"
    );
    assert_eq!(second.name, unchanged.name);
    assert_eq!(second.value, unchanged.value);
    let _ = paint(&mut host, &presence);
    assert_eq!(
        find(
            &host.accessibility().expect("AT"),
            "workspace-profile:agent:scribe-00:activity"
        )
        .expect("settled region")
        .value,
        changed.value
    );
}

#[test]
fn availability_watch_wakes_only_for_change_and_closure_and_requeries_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (mut host, chrome) = fixture(1);
    let membership = chrome.borrow().tree_handle();
    host.attach_app_action_broker(crate::shell::broker_services::test_broker_with_membership(
        membership,
    ));
    let (sender, changes) = tokio::sync::watch::channel(0);
    host.agent_profile_changes = Some(changes);
    let wakes = Arc::new(AtomicUsize::new(0));
    let (signal, receiver) = std::sync::mpsc::channel();
    let count = Arc::clone(&wakes);
    host.set_agent_profile_wake(Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        signal
            .send(())
            .expect("test retains watcher signal receiver");
    }));
    let presence = PresenceCtx::empty();
    host.refresh_agent_profile_activity(&presence);
    assert_eq!(host.agent_profile_query_count, 1);
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        0,
        "idle subscription does not tick"
    );
    sender.send(1).expect("real watch generation edge");
    receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("watch edge wakes native loop");
    host.refresh_agent_profile_activity(&presence);
    assert_eq!(host.agent_profile_query_count, 2);
    host.refresh_agent_profile_activity(&presence);
    assert_eq!(host.agent_profile_query_count, 2);
    drop(sender);
    receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("closure wakes once");
    host.refresh_agent_profile_activity(&presence);
    assert_eq!(
        host.agent_profile_activity[0].task_availability,
        AgentTaskAvailability::Unknown
    );
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    assert!(
        receiver.try_recv().is_err(),
        "closed watcher terminates instead of looping"
    );
}
