//! Interaction tests for shell chrome widgets (`OverviewView`, the search
//! palette, `QuickSettings`, `WorkspaceSettings`, `SidebarWheelDisc`, `TopBar`, …).

use shell::shell::state::FreshId as _;
use shell::shell::state::WorkspaceAuthority as _;
use std::collections::HashMap;

use shell::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
use tilo_desktop::overview::OverviewView;

/// Caller-supplied workspace id, so each test keeps a stable shape
/// without minting fresh monotonic ids.
fn mk_workspace(id: u64, name: &str) -> Workspace {
    Workspace::new(WorkspaceId::from(id), name)
}

#[test]
fn overview_opens_by_cmd_tab_activates_on_click_and_uses_a_large_header() {
    // Phase `overview_view_click_activates_workspace`.
    {
        let a = mk_workspace(1, "Alpha");
        let b = mk_workspace(2, "Beta");
        let b_id = b.id;

        let mut tree = WorkspaceTree {
            desktop: shell::shell::state::DesktopTree {
                workspaces: vec![a, b],
                active: WorkspaceId::from(0),
                active_strip: HashMap::default(),
                active_group: HashMap::default(),
                collaborators: vec![],
                ..Default::default()
            },
            ..Default::default()
        };
        tree.desktop.active = tree.desktop.workspaces[0].id;

        let mut ov = OverviewView::new();
        ov.sync(&tree.desktop);
        assert_eq!(ov.card_count(), 2);
        ov.click_card(1, &mut tree.desktop);
        assert_eq!(tree.desktop.active, b_id);
    }

    // Phase `overview_header_font_size_is_at_least_32px`.
    // The overview opens by naming the room you are in — the largest thing
    // on the panel. Pinned at >= 32px so a later step down to
    // `scale.x3l`/`base` is caught here, not in a screenshot review.
    {
        use shell::shell::state::WorkspaceTree;
        use ui::theme::Theme;
        let tree = WorkspaceTree::bootstrap_demo();
        let (host, _chrome) = shell::shell::build_tilo_overlay_host_with_chrome(&tree);
        let theme = Theme::dark();
        let size = host.overview_header_font_size(&theme);
        assert!(
            size >= 32.0,
            "the current room's name must be set at ≥ 32 px; got {size}"
        );
    }
}

#[test]
fn super_space_opens_and_escape_closes_search() {
    use shell::shell::overlay_host::ShellOverlayHost;
    let tree = WorkspaceTree::bootstrap_demo();
    let (mut host, _chrome) = shell::shell::build_tilo_overlay_host_with_chrome(&tree);
    host.set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
        .unwrap();
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        host,
        (1280, 800),
        ui::theme::Theme::dark(),
    );
    assert!(!frame.root::<ShellOverlayHost>().palette_visible());
    frame.set_modifiers(&["super".to_owned()]);
    frame.key("Space");
    assert!(frame.root::<ShellOverlayHost>().palette_visible());
    frame.set_modifiers(&[]);
    frame.key("Escape");
    assert!(!frame.root::<ShellOverlayHost>().palette_visible());
}

#[test]
fn workspace_settings_tabs_edit_the_name_roster_and_capabilities() {
    // Phase `workspace_settings_general_edits_name_and_removes_collaborator`.
    {
        use shell::shell::state::{Collaborator, Workspace, WorkspaceId};
        use tilo_desktop::workspace_settings::{WorkspaceSettings, WsTab};
        let mut ws = Workspace::new(WorkspaceId::new(), "Design");
        ws.collaborators = vec![
            Collaborator::new("u1", "Ada"),
            Collaborator::new("u2", "Bo"),
        ];
        let mut modal = WorkspaceSettings::new(ws.id);
        modal.set_tab(WsTab::General);
        modal.rename(&mut ws, "Concept Room");
        assert_eq!(ws.name, "Concept Room");
        modal.remove_collaborator(&mut ws, "u1");
        assert_eq!(ws.collaborators.len(), 1);
        assert_eq!(ws.collaborators[0].id, "u2");
    }

    // Phase `workspace_settings_members_tab_reads_the_real_roster`.
    {
        // What a member may do is their role in the room,
        // and that is what the Members pane renders.
        use shell::shell::state::{CapRole, Workspace, WorkspaceId};
        use tilo_desktop::workspace_settings::{WsTab, members_pane_rows};
        let host_str = "host".to_string();
        let mut ws = Workspace::new(WorkspaceId::new(), "Design");
        ws.set_owner("host");
        ws.govern_as_at(
            &host_str,
            tilo_action_policy::Change::AddMember {
                person: ("viewer-user").to_string(),
                role: tilo_action_policy::Role::Viewer,
            },
            tilo_clock::unix_now(),
        )
        .expect("the owner may write a membership");

        let mut modal = tilo_desktop::workspace_settings::WorkspaceSettings::new(ws.id);
        modal.set_tab(WsTab::Members);
        assert_eq!(modal.active_tab(), WsTab::Members);

        let pane = vello::kurbo::Rect::new(0.0, 0.0, 640.0, 480.0);
        let rows = members_pane_rows(&ws, pane);
        assert_eq!(rows.len(), 2, "owner + the invited viewer");
        assert_eq!(rows[0].user, "host");
        assert!(rows[0].is_workspace_owner);
        assert_eq!(rows[0].role, CapRole::Owner);
        assert_eq!(rows[1].user, "viewer-user");
        assert_eq!(rows[1].role, CapRole::Viewer);
    }

    // Phase `workspace_settings_permissions_tab_lists_granted_capabilities`.
    {
        use shell::shell::state::{
            Access, CapOverride, Expiry, Scope, Subject, Workspace, WorkspaceId,
        };
        use tilo_desktop::workspace_settings::{SubjectKind, WsTab, permissions_pane_layout};
        let host_str = "host".to_string();
        let mut ws = Workspace::new(WorkspaceId::new(), "Design");
        ws.set_owner("host");
        ws.grant_as(
            &host_str,
            CapOverride::grant(
                Subject::app("Terminal"),
                Scope::FsPath("/var/workspace".into()),
                Access::Write,
            )
            .expiring(Expiry::EndOfSession),
        )
        .expect("the owner may grant");

        let mut modal = tilo_desktop::workspace_settings::WorkspaceSettings::new(ws.id);
        modal.set_tab(WsTab::Permissions);

        let pane = vello::kurbo::Rect::new(0.0, 0.0, 660.0, 480.0);
        let layout =
            permissions_pane_layout(&ws, pane, &shell::shell::launcher::ShellApps, "viewer-user");
        assert_eq!(
            layout.rows.len(),
            1,
            "the owner's membership is not a permission"
        );
        let row = &layout.rows[0];
        assert_eq!(row.subject.kind, SubjectKind::App);
        assert_eq!(row.subject.id, "Terminal");
        // `capability_allows` phrases a workspace-projected `FsPath` scope as
        // "Edit workspace files" rather than exposing the FUSE mount point — no
        // internal labels in user-facing chrome.
        assert_eq!(row.allows, "Edit workspace files");
        assert_eq!(row.expiry, "Session");
        // The composer is laid out whether or not anything is granted yet.
        assert!(layout.choice_rect.area() > 0.0);
        assert!(layout.submit_rect.area() > 0.0);
    }
}

#[test]
fn topbar_chrome_renders_tray_avatars_clock_glyph_and_dots() {
    // Phase `topbar_chrome_maps_two_peers_to_two_distinct_avatar_hues`.
    {
        use shell::shell::state::{Collaborator, Workspace, WorkspaceId, WorkspaceTree};
        use shell::shell::topbar_chrome::TopBarChrome;
        let collab_1 = Collaborator::new("alice", "Ada");
        let collab_2 = Collaborator::new("bob", "Bo");
        let mut workspace = Workspace::new(WorkspaceId::new(), "A");
        workspace.collaborators = vec![collab_1, collab_2];
        let mut tree = WorkspaceTree {
            desktop: shell::shell::state::DesktopTree {
                workspaces: vec![workspace],
                active: WorkspaceId::from(0),
                active_strip: HashMap::default(),
                active_group: HashMap::default(),
                ..Default::default()
            },
            ..Default::default()
        };
        tree.desktop.active = tree.desktop.workspaces[0].id;
        let chrome = TopBarChrome::from_tree(&tree.desktop);
        let colors = chrome.collaborator_colors();
        assert_eq!(colors.len(), 2);
        // Two peers with distinct ids must resolve to distinct hues; otherwise
        // overlapping avatars collapse into a single indigo blob.
        assert_ne!(colors[0], colors[1]);
    }

    // Phase `topbar_chrome_clock_renders_at_bold_weight`.
    // The top-bar clock must draw at Bold (700) to read as the focal
    // "glance value" — pins the explicit `draw_text_weighted` swap against
    // a revert to `draw_text`, which defaults to Regular.
    {
        use shell::shell::topbar_chrome::TopBarChrome;
        use ui::text_render::TextWeight;
        let tree = WorkspaceTree {
            desktop: shell::shell::state::DesktopTree {
                workspaces: vec![mk_workspace(0, "A")],
                active: WorkspaceId::from(0),
                active_strip: HashMap::default(),
                active_group: HashMap::default(),
                collaborators: vec![],
                ..Default::default()
            },
            ..Default::default()
        };
        let chrome = TopBarChrome::from_tree(&tree.desktop);
        assert_eq!(
            chrome.clock_text_weight(),
            TextWeight::Bold,
            "clock must render at Bold (700) per concept §2.2 / visual-gap #7",
        );
        // Workspace-name pill sits one tier below the clock — Semibold (600).
        assert_eq!(
            chrome.workspace_name_text_weight(),
            TextWeight::Semibold,
            "workspace-name pill must render at Semibold (600) per concept §2.1",
        );
    }

    // Phase `topbar_chrome_overview_glyph_is_lucide_layout_grid`.
    // The overview button paints lucide `LayoutGrid` via `ui::icons`, not a
    // hand-rolled quad — routed through `overview_glyph_icon()` so a revert
    // fails here without needing a pixel snapshot harness.
    {
        use shell::shell::topbar_chrome::TopBarChrome;
        use ui::icons::Icon;
        assert_eq!(
            TopBarChrome::overview_glyph_icon(),
            Icon::LayoutGrid,
            "overview glyph must be lucide LayoutGrid per visual-gap #5",
        );
    }

    // Phase `topbar_chrome_active_center_dot_is_larger_than_inactive`.
    // The center-band active dot must read as a focused indicator, not a
    // slightly tinted sibling — converged radii lose the "where are we" cue.
    {
        use shell::shell::topbar_chrome::TopBarChrome;
        let tree = WorkspaceTree {
            desktop: shell::shell::state::DesktopTree {
                workspaces: vec![mk_workspace(0, "A")],
                active: WorkspaceId::from(0),
                active_strip: HashMap::default(),
                active_group: HashMap::default(),
                collaborators: vec![],
                ..Default::default()
            },
            ..Default::default()
        };
        let chrome = TopBarChrome::from_tree(&tree.desktop);
        assert!(
            chrome.active_dot_radius() > chrome.inactive_dot_radius(),
            "active dot radius {} must exceed inactive {}",
            chrome.active_dot_radius(),
            chrome.inactive_dot_radius(),
        );
    }
}

fn topbar_editor_actions(frame: &ui::headless::FrameHarness) -> Vec<ui::a11y::AccessNode> {
    frame
        .focus_access_nodes()
        .into_iter()
        .flat_map(|node| node.children)
        .filter(|node| {
            node.action_target.as_deref().is_some_and(|target| {
                target.starts_with("topbar:editor:") && target != "topbar:editor:open"
            })
        })
        .collect()
}

#[test]
fn topbar_editor_semantic_open_focus_resize_and_escape_use_the_native_host() {
    use shell::shell::overlay_host::ShellOverlayHost;
    use ui::{a11y::Action, headless::FrameHarness, state::State, theme::Theme};
    let tree = WorkspaceTree::bootstrap_demo();
    let (host, chrome) = shell::shell::build_tilo_overlay_host_with_chrome(&tree);
    let mut frame = FrameHarness::for_root(State::new(), host, (1280, 800), Theme::dark());
    frame
        .perform_semantic_named("topbar:editor:open", None, &Action::Activate)
        .unwrap();
    let controls = topbar_editor_actions(&frame);
    assert!(
        !controls.is_empty(),
        "the production editor publishes actions"
    );
    for control in controls {
        let bounds = control.bounds.expect("painted editor bounds").to_rect();
        assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
    }
    assert!(frame.inspection_json().unwrap().contains("Edit top bar"));
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/topbar-editor-native");
    std::fs::create_dir_all(&out).unwrap();
    frame.render_png(&out.join("native-open.png"));
    frame.resize((640, 600)).unwrap();
    frame.render_png(&out.join("native-resized-open.png"));
    let open = frame.render_rgba();
    frame.key("Escape");
    let closed = frame.render_rgba();
    assert!(topbar_editor_actions(&frame).is_empty());
    assert!(
        ui::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(&open, &closed) > 2000,
        "Esc removes the actual editor pixels"
    );
    frame.render_png(&out.join("native-resized-closed.png"));
    assert!(!frame.root::<ShellOverlayHost>().palette_visible());
    assert!(!chrome.borrow().overlays.overview);
    // Registry requests use the same production native adapter.
    chrome.borrow_mut().request_topbar_editor();
    frame.relayout();
    assert!(!topbar_editor_actions(&frame).is_empty());
    frame.key("Escape");
    assert!(topbar_editor_actions(&frame).is_empty());
}
