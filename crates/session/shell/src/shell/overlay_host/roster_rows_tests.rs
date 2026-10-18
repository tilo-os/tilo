use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::FreshId as _;
use crate::shell::state::{Collaborator, Workspace, WorkspaceId, WorkspaceTree};
use ui::theme::Theme;
use ui::widget::Container;
use vello::kurbo::{Affine, Rect};

fn tree_with_room() -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::new();
    let mut ws = Workspace::new(id, "Demo");
    ws.set_owner("ada");
    tree.desktop.workspaces.push(ws);
    tree.desktop.active = id;
    tree
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn roster_agent_rows_report_only_granted_live_agents_with_their_recorded_role() {
    fn an_ungranted_room_contributes_no_agent_row() {
        let mut tree = tree_with_room();
        tree.desktop
            .collaborators
            .push(Collaborator::new("bob", "bob"));
        let rows = tilo_desktop::collaborators::collaborator_roster_rows(
            &tree.desktop,
            &crate::shell::state::local_user_id(),
        );
        assert_eq!(rows.len(), 1, "only the peer is here: {rows:?}");
        assert!(!rows[0].is_agent);
        assert_eq!(rows[0].label, "bob");
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
            .push(Collaborator::new("bob", "bob"));
        {
            let ws = tree
                .desktop
                .active_workspace_mut()
                .expect("fixture has a room");
            ws.admit_agent_preset(
                "scribe",
                tilo_action_policy::roles::AgentPreset::Operator,
                &tilo_action_policy::Subject::person("ada".to_string()),
            );
        }

        let rows = tilo_desktop::collaborators::collaborator_roster_rows(
            &tree.desktop,
            &crate::shell::state::local_user_id(),
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].label, "bob");
        assert!(!rows[0].is_agent, "a peer must never be tagged as an agent");
    }

    fn a_connected_headless_agent_is_tagged_without_a_fake_follow_target() {
        let mut tree = tree_with_room();
        {
            let ws = tree
                .desktop
                .active_workspace_mut()
                .expect("fixture has a room");
            ws.admit_agent_preset(
                "scribe",
                tilo_action_policy::roles::AgentPreset::Operator,
                &tilo_action_policy::Subject::person("ada".to_string()),
            );
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

    /// A row shows what its participant may do here, and nothing where
    /// the room says nothing — an agent's role is the verb on the
    /// capability that admitted it; a peer's comes from the room's own
    /// `Collaborator` list, never a default.
    fn a_row_carries_the_role_the_room_recorded_and_no_other() {
        let mut tree = tree_with_room();
        {
            let ws = tree
                .desktop
                .active_workspace_mut()
                .expect("fixture has a room");
            ws.collaborators.push(Collaborator::new("peer@7", "peer@7"));
            ws.collaborators
                .push(Collaborator::new("bob", "bob").with_role("editor"));
            ws.admit_agent_preset(
                "scribe",
                tilo_action_policy::roles::AgentPreset::Contributor,
                &tilo_action_policy::Subject::person("ada".to_string()),
            );
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
            rows[2].role.as_deref(),
            Some("Commenter"),
            "an agent's row must show the verb its membership grant carries, \
             which is the whole of what revoking it takes back: {rows:?}",
        );
    }

    an_ungranted_room_contributes_no_agent_row();
    a_granted_but_absent_agent_is_not_reported_as_live();
    a_connected_headless_agent_is_tagged_without_a_fake_follow_target();
    a_row_carries_the_role_the_room_recorded_and_no_other();
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
        host.qs_motion = super::qs_motion::QsMotion::opened();
        host.qs_motion.follow(&super::qs_motion::QsFrame {
            open: false,
            stack: &[crate::shell::quick_settings::QsLevel::Root],
            height: 300.0,
            motion: ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false),
            source: ui::tokens::MotionSource::Pointer,
            now: ui::time::now(),
        });
        assert!(
            host.shortcut_help_button_is_suppressed(),
            "a closing popover keeps the hint hidden until its last fading frame"
        );
        host.qs_motion = super::qs_motion::QsMotion::closed();

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
