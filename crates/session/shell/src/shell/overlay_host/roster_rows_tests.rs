use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::FreshId as _;
use crate::shell::state::{
    CapOverride, Collaborator, Subject, Verb, Workspace, WorkspaceId, WorkspaceTree,
};
use ui::theme::Theme;
use ui::widget::Container;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

fn tree_with_room() -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::new();
    let mut ws = Workspace::new(id, "Demo");
    ws.set_owner("quinten");
    tree.desktop.workspaces.push(ws);
    tree.desktop.active = id;
    tree
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn roster_agent_rows_report_only_granted_live_agents_with_capability_presets() {
    fn an_ungranted_room_contributes_no_agent_row() {
        let mut tree = tree_with_room();
        tree.desktop
            .collaborators
            .push(Collaborator::new("xaver", "Xaver"));
        let rows = tilo_desktop::collaborators::collaborator_roster_rows(
            &tree.desktop,
            &crate::shell::state::local_user_id(),
        );
        assert_eq!(rows.len(), 1, "only the peer is here: {rows:?}");
        assert!(!rows[0].is_agent);
        assert_eq!(rows[0].label, "Xaver");
        assert!(rows[0].followable);
        assert_eq!(
            rows[0].activity, None,
            "the live avatar already communicates presence; the action line stays empty until a real view or semantic action exists"
        );
    }

    fn a_granted_but_absent_agent_is_not_reported_as_live() {
        let mut tree = tree_with_room();
        tree.desktop
            .collaborators
            .push(Collaborator::new("xaver", "Xaver"));
        {
            let ws = tree
                .desktop
                .active_workspace_mut()
                .expect("fixture has a room");
            let room = ws.id.raw().to_string();
            ws.grant_as(
                &"quinten".to_string(),
                CapOverride::membership(Subject::agent("scribe"), room, Verb::Write),
            )
            .expect("the owner may admit an agent");
        }

        let rows = tilo_desktop::collaborators::collaborator_roster_rows(
            &tree.desktop,
            &crate::shell::state::local_user_id(),
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].label, "Xaver");
        assert!(!rows[0].is_agent, "a peer must never be tagged as an agent");
    }

    fn a_connected_headless_agent_is_tagged_without_a_fake_follow_target() {
        let mut tree = tree_with_room();
        {
            let ws = tree
                .desktop
                .active_workspace_mut()
                .expect("fixture has a room");
            let room = ws.id.raw().to_string();
            ws.grant_as(
                &"quinten".to_string(),
                CapOverride::membership(Subject::agent("scribe"), room, Verb::Write),
            )
            .expect("the owner may admit an agent");
            ws.collaborators.push(Collaborator::new("peer@9", "scribe"));
        }

        let rows = tilo_desktop::collaborators::collaborator_roster_rows(
            &tree.desktop,
            &crate::shell::state::local_user_id(),
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].label, "scribe");
        assert!(
            rows[0].is_agent,
            "an agent listed without its tag is the OS reporting a person",
        );
        assert_eq!(rows[0].initial, 'S');
        assert!(
            !rows[0].followable,
            "a connected headless agent has no viewport to follow"
        );
        assert_eq!(
            rows[0].activity, None,
            "presence alone must not be presented as a semantic action"
        );
    }

    /// A person row reports the role the room recorded. An agent row never
    /// promotes its admission verb into a human role; it reports the explicit
    /// capability preset that Workspace Settings will review instead.
    fn rows_separate_human_roles_from_agent_capability_presets() {
        let mut tree = tree_with_room();
        {
            let ws = tree
                .desktop
                .active_workspace_mut()
                .expect("fixture has a room");
            ws.collaborators.push(Collaborator::new("peer@7", "peer@7"));
            ws.collaborators
                .push(Collaborator::new("xaver", "Xaver").with_role("editor"));
            let room = ws.id.raw().to_string();
            ws.grant_as(
                &"quinten".to_string(),
                CapOverride::membership(Subject::agent("scribe"), room, Verb::Comment),
            )
            .expect("the owner may admit an agent");
            ws.collaborators.push(Collaborator::new("peer@9", "scribe"));
        }

        let rows = tilo_desktop::collaborators::collaborator_roster_rows(
            &tree.desktop,
            &crate::shell::state::local_user_id(),
        );
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(
            rows[0].role, None,
            "a peer the room recorded nothing about must not acquire a role",
        );
        assert_eq!(rows[1].role.as_deref(), Some("Editor"));
        assert_eq!(
            rows[2].role, None,
            "an agent must not be presented as inheriting a human role: {rows:?}",
        );
        assert_eq!(
            rows[2].authority.as_deref(),
            Some("Capability preset: Contributor"),
            "the admission marker is described as a reviewable capability preset: {rows:?}",
        );
    }

    an_ungranted_room_contributes_no_agent_row();
    a_granted_but_absent_agent_is_not_reported_as_live();
    a_connected_headless_agent_is_tagged_without_a_fake_follow_target();
    rows_separate_human_roles_from_agent_capability_presets();
}

#[test]
fn live_roster_rows_open_their_durable_settings_destination() {
    fn paint_people(host: &mut ShellOverlayHost, rows: &[RosterRow]) {
        host.collaborators_overlay.draw(
            &mut vello::Scene::new(),
            Affine::IDENTITY,
            &mut ui::text_render::TextRenderer::new(),
            &Theme::dark(),
            &tilo_desktop::collaborators::CollaboratorsModel {
                host: Rect::new(0.0, 0.0, 1200.0, 800.0),
                rows,
                own_initial: 'Q',
                own_color: Color::from_rgb8(114, 139, 180),
                unavailable: None,
                may_invite: false,
                followed_id: None,
                following_pairs: &[],
                own_status: None,
                own_activity: None,
                own_status_automatic: false,
                status_sharing: ui::StatusSharing::Automatic,
                status_editing: false,
                screen_exposure: None,
            },
            None,
        );
    }

    fn person_opens_members_and_agent_opens_agents() {
        let mut tree = tree_with_room();
        {
            let workspace = tree
                .desktop
                .active_workspace_mut()
                .expect("fixture has a room");
            workspace
                .collaborators
                .push(Collaborator::new("ada", "Ada"));
            let room = workspace.id.raw().to_string();
            workspace
                .grant_as(
                    &"quinten".to_string(),
                    CapOverride::membership(Subject::agent("scribe"), room, Verb::Read),
                )
                .expect("the owner may admit an agent");
            workspace
                .collaborators
                .push(Collaborator::new("agent@9", "scribe"));
        }
        let chrome = ChromeState::new(tree);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(chrome.clone());
        let rows = tilo_desktop::collaborators::collaborator_roster_rows(
            &chrome.borrow().tree().desktop,
            "quinten",
        );
        assert_eq!(rows.len(), 2, "{rows:?}");
        paint_people(&mut host, &rows);

        let mut handled = false;
        host.apply_collaborators_hit(CollaboratorsHit::OpenSettings(0), &mut handled);
        assert!(handled);
        assert_eq!(
            chrome.borrow().ws_settings_tab,
            tilo_desktop::workspace_settings::WsTab::Members
        );

        chrome.borrow_mut().overlays.collaborators = true;
        paint_people(&mut host, &rows);
        handled = false;
        host.apply_collaborators_hit(CollaboratorsHit::OpenSettings(1), &mut handled);
        assert!(handled);
        assert_eq!(
            chrome.borrow().ws_settings_tab,
            tilo_desktop::workspace_settings::WsTab::Agents
        );
        assert!(!chrome.borrow().overlays.collaborators);
    }

    fn a_stale_row_cannot_open_settings_for_a_replaced_identity() {
        let mut tree = tree_with_room();
        tree.desktop
            .active_workspace_mut()
            .expect("fixture has a room")
            .collaborators
            .push(Collaborator::new("ada", "Ada"));
        let chrome = ChromeState::new(tree);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(chrome.clone());
        let rows = tilo_desktop::collaborators::collaborator_roster_rows(
            &chrome.borrow().tree().desktop,
            "quinten",
        );
        paint_people(&mut host, &rows);
        chrome
            .borrow_mut()
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .expect("fixture has a room")
            .collaborators
            .clear();

        let mut handled = false;
        host.apply_collaborators_hit(CollaboratorsHit::OpenSettings(0), &mut handled);
        assert!(handled, "the stale overlay action is still absorbed");
        assert!(
            !chrome.borrow().overlays.ws_settings,
            "a stale row cannot navigate using its former identity"
        );
    }

    person_opens_members_and_agent_opens_agents();
    a_stale_row_cannot_open_settings_for_a_replaced_identity();
}

#[test]
fn quick_settings_bounds_cover_shadows_and_hints_yield_to_modal_surfaces() {
    fn settled_quick_settings_bounds_cover_a_high_elevation_panel_shadow() {
        let mut theme = Theme::dark();
        theme.tokens.elevation.floating.blur = 96.0;
        theme.tokens.elevation.floating.offset_y = 24.0;
        let rect = Rect::new(100.0, 200.0, 500.0, 700.0);
        let bounds = quick_settings_settled_bounds(rect, &theme, Affine::IDENTITY);
        let panel_reach = ui::surface::std_dev_for(theme.tokens.elevation.floating.blur) * 3.0
            + f64::from(theme.tokens.elevation.floating.offset_y)
            + 0.5;
        assert!(bounds.x0 <= rect.x0 - panel_reach);
        assert!(bounds.y0 <= rect.y0 - panel_reach);
        assert!(bounds.x1 >= rect.x1 + panel_reach);
        assert!(bounds.y1 >= rect.y1 + panel_reach);
    }

    fn persistent_shortcut_hint_is_suppressed_by_modal_and_popover_surfaces() {
        // The floating hint belongs to the empty desktop; app workspaces
        // now suppress it to keep content and pointer targets unobscured.
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(chrome.clone());
        assert!(
            !host.shortcut_help_button_is_suppressed(),
            "the desktop affordance is visible with no transient shell surface"
        );

        chrome.borrow_mut().overlays.quick_settings = true;
        assert!(
            host.shortcut_help_button_is_suppressed(),
            "the hint cannot paint across a popover"
        );
        chrome.borrow_mut().overlays.quick_settings = false;
        host.qs_transition = ui::Transition::open();
        host.qs_transition.set_open(
            false,
            Duration::from_millis(QS_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
        assert!(
            host.shortcut_help_button_is_suppressed(),
            "a closing popover keeps the hint hidden until its last fading frame"
        );
        host.qs_transition = ui::Transition::closed();

        host.palette_visible = true;
        assert!(
            host.shortcut_help_button_is_suppressed(),
            "the hint cannot paint above a modal palette"
        );
        host.palette_visible = false;

        host.layers.power_menu.open();
        assert!(
            host.shortcut_help_button_is_suppressed(),
            "the hint cannot paint above a session-action sheet"
        );
    }

    settled_quick_settings_bounds_cover_a_high_elevation_panel_shadow();
    persistent_shortcut_hint_is_suppressed_by_modal_and_popover_surfaces();
}
