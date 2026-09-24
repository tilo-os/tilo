//! Interaction tests for shell chrome widgets (`OverviewView`, `GlobalSearch`,
//! `QuickSettings`, `WorkspaceSettings`, `SidebarWheelDisc`, `TopBar`, …).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::state::FreshId as _;
use shell::shell::state::WorkspaceAuthority as _;
use std::collections::HashMap;

use shell::shell::overview::OverviewView;
use shell::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

/// Caller-supplied workspace id, so each test keeps a stable shape
/// without minting fresh monotonic ids.
fn mk_workspace(id: u64, name: &str) -> Workspace {
    Workspace::new(WorkspaceId::from(id), name)
}

#[test]
fn overview_view_click_activates_workspace() {
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
    ov.sync(&tree);
    assert_eq!(ov.card_count(), 2);
    ov.click_card(1, &mut tree);
    assert_eq!(tree.desktop.active, b_id);
}

use shell::shell::global_search::GlobalSearch;

#[test]
fn global_search_seeds_commands_and_filters() {
    let mut s = GlobalSearch::new();
    s.seed_default_commands(); // "Create Workspace", "Rename Workspace", "Open Settings", ...
    s.palette_mut().set_query("create");
    assert!(
        s.palette()
            .visible_entries()
            .iter()
            .any(|e| e.label.contains("Create"))
    );
}

#[test]
fn global_search_context_filter_narrows_results() {
    let mut s = GlobalSearch::new();
    s.seed_default_commands();
    s.set_context_kind(shell::shell::global_search::SearchContext::Files);
    let labels: Vec<String> = s
        .palette()
        .visible_entries()
        .iter()
        .map(|e| e.label.clone())
        .collect();
    assert!(
        labels
            .iter()
            .all(|l| l.to_lowercase().contains("file") || l.to_lowercase().contains("open"))
    );
}

#[test]
fn workspace_settings_general_edits_name_and_removes_collaborator() {
    use shell::shell::state::{Collaborator, Workspace, WorkspaceId};
    use shell::shell::workspace_settings::{WorkspaceSettings, WsTab};
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

#[test]
fn workspace_settings_members_tab_reads_the_real_roster() {
    // What a member may do is the verb on their membership capability,
    // and that is what the Members pane renders.
    use shell::shell::state::{CapOverride, CapRole, Subject, Verb, Workspace, WorkspaceId};
    use shell::shell::workspace_settings::{WsTab, members_pane_rows};
    let host_str = "host".to_string();
    let mut ws = Workspace::new(WorkspaceId::new(), "Design");
    ws.set_owner("host");
    let room = ws.id.raw().to_string();
    ws.grant_as(
        &host_str,
        CapOverride::membership(Subject::person("viewer-user"), room, Verb::Read),
    )
    .expect("the owner may write a membership");

    let mut modal = shell::shell::workspace_settings::WorkspaceSettings::new(ws.id);
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

#[test]
fn workspace_settings_permissions_tab_lists_granted_capabilities() {
    use shell::shell::state::{CapOverride, Expiry, Scope, Subject, Verb, Workspace, WorkspaceId};
    use shell::shell::workspace_settings::{SubjectKind, WsTab, permissions_pane_layout};
    let host_str = "host".to_string();
    let mut ws = Workspace::new(WorkspaceId::new(), "Design");
    ws.set_owner("host");
    ws.grant_as(
        &host_str,
        CapOverride::grant(
            Subject::app("Terminal"),
            Scope::FsPath("/var/workspace".into()),
            Verb::Write,
        )
        .expiring(Expiry::EndOfSession),
    )
    .expect("the owner may grant");

    let mut modal = shell::shell::workspace_settings::WorkspaceSettings::new(ws.id);
    modal.set_tab(WsTab::Permissions);

    let pane = vello::kurbo::Rect::new(0.0, 0.0, 660.0, 480.0);
    let layout = permissions_pane_layout(&ws, pane);
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

#[test]
fn topbar_leftnav_navigates_workspaces() {
    use shell::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
    use shell::shell::topbar::{TopBar, TopBarEvent};
    let ws_a = Workspace::new(WorkspaceId::new(), "A");
    let ws_b = Workspace::new(WorkspaceId::new(), "B");
    let ws_c = Workspace::new(WorkspaceId::new(), "C");

    let mut tree = WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![ws_a, ws_b, ws_c],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: vec![],
            ..Default::default()
        },
        ..Default::default()
    };
    tree.desktop.active = tree.desktop.workspaces[1].id;
    let mut tb = TopBar::new();
    tb.emit(TopBarEvent::WorkspaceUp, &mut tree);
    assert_eq!(tree.desktop.active, tree.desktop.workspaces[0].id);
    tb.emit(TopBarEvent::WorkspaceDown, &mut tree);
    tb.emit(TopBarEvent::WorkspaceDown, &mut tree);
    assert_eq!(tree.desktop.active, tree.desktop.workspaces[2].id);
    // clamped at edges
    tb.emit(TopBarEvent::WorkspaceDown, &mut tree);
    assert_eq!(tree.desktop.active, tree.desktop.workspaces[2].id);
}

#[test]
fn topbar_workspace_name_button_emits_settings_request() {
    use shell::shell::topbar::{TopBar, TopBarOutput};
    let mut tb = TopBar::new();
    let out = tb.click_workspace_name();
    assert!(matches!(out, TopBarOutput::OpenWorkspaceSettings));
}

#[test]
fn topbar_right_tray_renders_collaborators_and_opens_quick_settings() {
    use shell::shell::state::{Collaborator, Workspace, WorkspaceId, WorkspaceTree};
    use shell::shell::topbar::{TopBar, TopBarOutput};
    let collab_1 = Collaborator::new("u1", "Ada");
    let collab_2 = Collaborator::new("u2", "Bo");
    let collab_3 = Collaborator::new("u3", "Cy");
    let collab_4 = Collaborator::new("u4", "Di");

    let mut tree = WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: HashMap::default(),
            active_group: HashMap::default(),
            collaborators: vec![collab_1, collab_2, collab_3, collab_4],
            ..Default::default()
        },
        ..Default::default()
    };
    tree.desktop.active = tree.desktop.workspaces[0].id;
    let mut tb = TopBar::new();
    let avatars = tb.visible_avatars(&tree, 3);
    assert_eq!(avatars.len(), 3);
    assert_eq!(tb.overflow_count(&tree, 3), 1);
    let out = tb.click_tray_settings();
    assert!(matches!(out, TopBarOutput::OpenQuickSettings));
}

#[test]
fn topbar_chrome_maps_two_peers_to_two_distinct_avatar_hues() {
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
    let chrome = TopBarChrome::from_tree(&tree);
    let colors = chrome.collaborator_colors();
    assert_eq!(colors.len(), 2);
    // Two peers with distinct ids must resolve to distinct hues; otherwise
    // overlapping avatars collapse into a single indigo blob.
    assert_ne!(colors[0], colors[1]);
}

/// The top-bar clock must draw at Bold (700) to read as the focal
/// "glance value" — pins the explicit `draw_text_weighted` swap against
/// a revert to `draw_text`, which defaults to Regular.
#[test]
fn topbar_chrome_clock_renders_at_bold_weight() {
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
    let chrome = TopBarChrome::from_tree(&tree);
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

/// The `GlobalSearch` palette surface is opaque (a tint would read as
/// chrome bleeding through long result rows), leaning on the scrim below
/// for depth. Pinned against `overlay_host::MODAL_SURFACE_ALPHA`, since
/// the palette and workspace-settings modal share one value.
#[test]
fn global_search_surface_alpha_is_opaque_and_matches_the_modal_tint() {
    use shell::shell::global_search::GlobalSearch;
    use shell::shell::overlay_host::MODAL_SURFACE_ALPHA;
    let a = GlobalSearch::surface_alpha();
    assert!(
        (a - 1.0).abs() < f32::EPSILON,
        "GlobalSearch surface alpha must be 1.0 so long result rows stay \
         legible over a busy desktop; got {a}"
    );
    assert!(
        (a - MODAL_SURFACE_ALPHA).abs() < f32::EPSILON,
        "the palette tint must stay in step with MODAL_SURFACE_ALPHA \
         ({MODAL_SURFACE_ALPHA}); got {a}"
    );
}

/// The overview button paints lucide `LayoutGrid` via `ui::icons`, not a
/// hand-rolled quad — routed through `overview_glyph_icon()` so a revert
/// fails here without needing a pixel snapshot harness.
#[test]
fn topbar_chrome_overview_glyph_is_lucide_layout_grid() {
    use shell::shell::topbar_chrome::TopBarChrome;
    use ui::icons::Icon;
    assert_eq!(
        TopBarChrome::overview_glyph_icon(),
        Icon::LayoutGrid,
        "overview glyph must be lucide LayoutGrid per visual-gap #5",
    );
}

/// The center-band active dot must read as a focused indicator, not a
/// slightly tinted sibling — converged radii lose the "where are we" cue.
#[test]
fn topbar_chrome_active_center_dot_is_larger_than_inactive() {
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
    let chrome = TopBarChrome::from_tree(&tree);
    assert!(
        chrome.active_dot_radius() > chrome.inactive_dot_radius(),
        "active dot radius {} must exceed inactive {}",
        chrome.active_dot_radius(),
        chrome.inactive_dot_radius(),
    );
}

#[test]
fn topbar_center_widget_cycles_with_wheel_cooldown() {
    use shell::shell::topbar::{CenterWidget, TopBar};
    use std::time::Duration;
    let mut tb = TopBar::new();
    assert_eq!(tb.center_widget(), CenterWidget::Clock);
    tb.on_center_wheel(1, Duration::from_millis(0));
    assert_eq!(tb.center_widget(), CenterWidget::SystemMonitor);
    // second wheel within 200ms cooldown: ignored
    tb.on_center_wheel(1, Duration::from_millis(100));
    assert_eq!(tb.center_widget(), CenterWidget::SystemMonitor);
    tb.on_center_wheel(1, Duration::from_millis(300));
    assert_eq!(tb.center_widget(), CenterWidget::Calendar);
    // wrap
    tb.on_center_wheel(1, Duration::from_millis(700));
    assert_eq!(tb.center_widget(), CenterWidget::Clock);
    // negative direction
    tb.on_center_wheel(-1, Duration::from_secs(1));
    assert_eq!(tb.center_widget(), CenterWidget::Calendar);
}

#[test]
fn shell_root_starts_with_every_overlay_closed() {
    use shell::shell::Shell;
    let shell = Shell::new_for_test();
    assert!(!shell.overview_visible());
    assert!(!shell.quick_settings_visible());
}

// Shell-level global hotkeys + shell routing

#[test]
fn cmd_k_opens_and_escape_closes_search() {
    use shell::shell::Shell;
    use shell::shell::test_support::{Mods, key_press};
    let mut shell = Shell::new_for_test();
    shell.dispatch_key(key_press("K", Mods::CMD));
    assert!(shell.search_visible());
    shell.dispatch_key(key_press("Escape", Mods::NONE));
    assert!(!shell.search_visible());
}

#[test]
fn cmd_tab_toggles_overview() {
    use shell::shell::Shell;
    use shell::shell::test_support::{Mods, key_press};
    let mut shell = Shell::new_for_test();
    shell.dispatch_key(key_press("Tab", Mods::CMD));
    assert!(shell.overview_visible());
    shell.dispatch_key(key_press("Tab", Mods::CMD));
    assert!(!shell.overview_visible());
}

#[test]
fn workspace_rename_fires_info_toast() {
    use shell::shell::Shell;
    use ui::widget::toast::ToastKind;
    let mut shell = Shell::new_for_test();
    let b0 = shell.toast_count();
    shell.simulate_rename_active_workspace("New Name");
    let kinds: Vec<ToastKind> = shell.toast_kinds();
    assert!(kinds.iter().any(|kind| matches!(kind, ToastKind::Info)));
    assert_eq!(shell.toast_count(), b0 + 1);
}

#[test]
fn sidebar_wheel_event_routes_through_shell() {
    use shell::shell::Shell;
    let mut shell = Shell::new_for_test();
    assert_eq!(shell.append_empty_strip_for_test("Second"), 1);
    assert_eq!(shell.active_strip_index(), 0);
    assert_eq!(shell.simulate_sidebar_wheel(1), 1);
    assert_eq!(shell.active_strip_index(), 1);
    assert_eq!(shell.simulate_sidebar_wheel(-1), 0);
    assert_eq!(shell.active_strip_index(), 0);
    assert!(!shell.search_visible());
    assert!(!shell.overview_visible());
}

#[test]
fn tray_gear_toggles_quick_settings_via_shell() {
    use shell::shell::Shell;
    let mut shell = Shell::new_for_test();
    assert!(!shell.quick_settings_visible());
    shell.simulate_tray_gear_click();
    assert!(shell.quick_settings_visible());
    shell.simulate_tray_gear_click();
    assert!(!shell.quick_settings_visible());
}

// Tooltip wiring: topbar workspace-name button tooltip

#[test]
fn topbar_ws_name_tooltip_reveals_after_delay() {
    use shell::shell::topbar::TopBar;
    use std::time::Duration;
    let mut tb = TopBar::new();
    tb.hover_ws_name(true);
    tb.tick(Duration::from_millis(700));
    assert!(tb.ws_name_tooltip_visible());
    tb.hover_ws_name(false);
    assert!(!tb.ws_name_tooltip_visible());
}

// ContextMenu wiring: right-click on tile -> default menu

#[test]
fn right_click_on_tile_opens_default_context_menu() {
    use shell::shell::Shell;
    let mut shell = Shell::new_for_test();
    assert!(!shell.context_menu_is_open());
    shell.simulate_right_click_on_active_tile([120.0, 200.0]);
    assert!(shell.context_menu_is_open());
    assert_eq!(shell.context_menu_position(), Some([120.0, 200.0]));
    let labels = shell.context_menu_labels();
    assert!(labels.iter().any(|l| l == "Close Tab"));
    assert!(labels.iter().any(|l| l == "Split Right"));
    assert!(labels.iter().any(|l| l == "Move to New Group"));
}

// The overview opens by naming the room you are in — the largest thing
// on the panel. Pinned at >= 32px so a later step down to
// `scale.x3l`/`base` is caught here, not in a screenshot review.
#[test]
fn overview_header_font_size_is_at_least_32px() {
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
