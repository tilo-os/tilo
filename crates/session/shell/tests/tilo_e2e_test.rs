//! End-to-end integration coverage for the tilo shell chrome. Exercises
//! the root widget tree from [`build_tilo_root`] against a bootstrap
//! workspace + shared chrome state, verifying cross-widget wiring holds
//! together in one pass (no live compositor; plain crate APIs only).
//!
//! Complements the per-widget unit tests in `tests/shell_chrome_test.rs`,
//! and `crates/sdk/ui-core/src/widget/tabs.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::chrome_state::{Chrome, ChromeState, QuickSettingsState, SearchScope};
use shell::shell::state::{Collaborator, WorkspaceTree};
use shell::shell::topbar_chrome::{CenterWidget, TopBarChrome};
use shell::shell::{build_tilo_root, build_tilo_root_with_chrome, state::TileContent};

use ui::widget::Widget;

#[test]
fn bootstrap_root_builds_and_seeds_the_demo_tabs_in_order() {
    // Phase `root_builds_without_panic_for_bootstrap_tree`.
    {
        let tree = WorkspaceTree::bootstrap_default();
        let _root = build_tilo_root(&tree);
    }

    // Phase `bootstrap_demo_seeds_four_demo_tabs_in_order`.
    {
        // A first boot starts empty, so `bootstrap_demo` is the fixture with a tab
        // to act on.
        let tree = WorkspaceTree::bootstrap_demo();
        let ws = tree.desktop.active_workspace().expect("active workspace");
        let strip = ws.strips.first().expect("at least one strip");
        let group = strip.window_groups.first().expect("at least one group");
        let titles: Vec<&str> = group.tabs.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["Notes", "Files", "Terminal", "Document"]);
        // A window the user never opened, showing an app that doesn't exist, is
        // worse than not offering it — nothing seeded here may be a Stub.
        for tab in &group.tabs {
            match &tab.tile {
                TileContent::AppPlaceholder { .. } => {}
                other => panic!(
                    "the default workspace seeded a non-hydratable tile for the \
                     {:?} tab: {:?}. Seed only apps the shell can actually host.",
                    tab.title, other
                ),
            }
        }
        for tab in &group.tabs {
            // The terminal is the one tab whose hydration can legitimately fail (no
            // /dev/ptmx in a sandbox), and it says so in its label.
            if tab.title == "Terminal" {
                continue;
            }
            assert_eq!(
                shell::shell::resolve_tile_shape_for_test(&tab.tile),
                "App",
                "the {:?} tab hydrated into a stub card instead of a real app",
                tab.title,
            );
        }
    }
}

#[test]
fn chrome_state_is_shared_defaults_sensibly_and_mutates_through_chrome() {
    // Phase `chrome_state_shared_across_sibling_widgets`.
    {
        let tree = WorkspaceTree::bootstrap_default();
        let chrome = ChromeState::new(tree);

        // Two clones stand for two chrome widgets holding the same state.
        let a = chrome.clone();
        let b = chrome;

        a.borrow_mut().overlays.overview = true;
        assert!(b.borrow().overlays.overview);

        let before = b.borrow().revision;
        a.borrow_mut().bump();
        assert_eq!(b.borrow().revision, before.wrapping_add(1));
    }

    // Phase `quick_settings_state_defaults_and_mutates_through_chrome`.
    {
        let tree = WorkspaceTree::bootstrap_default();
        let chrome = ChromeState::new(tree);
        // Defaults are deliberate: brightness > 0 so the slider has a visible fill,
        // wifi on / bluetooth off to match most fresh installs.
        let defaults = QuickSettingsState::default();
        assert_eq!(chrome.borrow().quick_settings, defaults);
        assert!(defaults.wifi && !defaults.bluetooth);

        // Writes go through the shared handle so every chrome widget sees them.
        {
            let mut st = chrome.borrow_mut();
            st.quick_settings.brightness = 0.25;
            st.quick_settings.bluetooth = true;
            st.bump();
        }
        // `QuickSettingsState` isn't `Copy` (the drill-in vectors), so clone out
        // from behind the `Ref` before asserting.
        let qs = chrome.borrow().quick_settings.clone();
        assert!((qs.brightness - 0.25).abs() < 1e-5);
        assert!(qs.bluetooth);
    }

    // Phase `search_scope_defaults_to_workspace_and_mutates_through_chrome`.
    {
        let tree = WorkspaceTree::bootstrap_default();
        let chrome = ChromeState::new(tree);
        // Default narrows Cmd-K to the current workspace so peers don't leak
        // cross-workspace results by accident.
        assert_eq!(chrome.borrow().search_scope, SearchScope::Workspace);

        {
            let mut st = chrome.borrow_mut();
            st.search_scope = SearchScope::All;
            st.bump();
        }
        assert_eq!(chrome.borrow().search_scope, SearchScope::All);
        assert_eq!(SearchScope::Workspace.label(), "Workspace");
        assert_eq!(SearchScope::All.label(), "Everywhere");
    }

    // Phase `collaborators_overlay_defaults_off_and_toggles_through_chrome`.
    {
        let tree = WorkspaceTree::bootstrap_default();
        let chrome = ChromeState::new(tree);
        assert!(!chrome.borrow().overlays.collaborators);
        assert!(!chrome.borrow().overlays.quick_settings);
        {
            let mut st = chrome.borrow_mut();
            st.overlays.collaborators = true;
            st.bump();
        }
        assert!(chrome.borrow().overlays.collaborators);
        // Sibling overlays: opening one doesn't implicitly close the other (the
        // outside-click handler does).
        {
            let mut st = chrome.borrow_mut();
            st.overlays.quick_settings = true;
            st.bump();
        }
        assert!(chrome.borrow().overlays.collaborators);
        assert!(chrome.borrow().overlays.quick_settings);
    }

    // Phase `center_widget_cycles_forward_and_backward`.
    {
        // Forward scroll cycles Clock → SystemMonitor → Calendar → Clock.
        assert_eq!(CenterWidget::Clock.next(), CenterWidget::SystemMonitor);
        assert_eq!(CenterWidget::SystemMonitor.next(), CenterWidget::Calendar);
        assert_eq!(CenterWidget::Calendar.next(), CenterWidget::Clock);
        assert_eq!(CenterWidget::Clock.prev(), CenterWidget::Calendar);
        assert_eq!(CenterWidget::Calendar.prev(), CenterWidget::SystemMonitor);
        assert_eq!(CenterWidget::SystemMonitor.prev(), CenterWidget::Clock);
        // Dot-click hit-test maps 0→Clock, 1→SystemMonitor, 2→Calendar; wraps on 3+.
        assert_eq!(CenterWidget::from_index(0), CenterWidget::Clock);
        assert_eq!(CenterWidget::from_index(1), CenterWidget::SystemMonitor);
        assert_eq!(CenterWidget::from_index(2), CenterWidget::Calendar);
        assert_eq!(CenterWidget::from_index(3), CenterWidget::Clock);
        let tree = WorkspaceTree::bootstrap_default();
        assert_eq!(
            TopBarChrome::from_tree(&tree.desktop).center_widget(),
            CenterWidget::Clock
        );
    }
}

#[test]
fn workspace_rename_commits_for_the_owner_but_not_a_guest() {
    // Phase `commit_workspace_name_applies_buffer_to_active_workspace`.
    {
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.adopt_ownerless_workspaces_with(shell::shell::state::local_user_id_for_room);
        let chrome = ChromeState::new(tree);

        {
            let mut st = chrome.borrow_mut();
            st.editing_workspace_name = Some("Research".to_string());
        }
        chrome.borrow_mut().commit_workspace_name();
        let st = chrome.borrow();
        assert_eq!(st.editing_workspace_name, None);
        assert_eq!(
            st.tree().desktop.active_workspace().unwrap().name,
            "Research"
        );

        // An empty/whitespace buffer must *not* overwrite the name.
        drop(st);
        {
            let mut st = chrome.borrow_mut();
            st.editing_workspace_name = Some("   ".to_string());
        }
        chrome.borrow_mut().commit_workspace_name();
        assert_eq!(
            chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .unwrap()
                .name,
            "Research"
        );
    }

    // Phase `guest_cannot_commit_a_workspace_rename`.
    {
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.adopt_ownerless_workspaces(&"another-room-owner".into());
        let chrome = ChromeState::new(tree);
        let original = chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .unwrap()
            .name
            .clone();

        {
            let mut st = chrome.borrow_mut();
            st.editing_workspace_name = Some("Misleading local rename".into());
            assert!(!st.can_rename_active_workspace());
        }
        chrome.borrow_mut().commit_workspace_name();

        let st = chrome.borrow();
        assert_eq!(st.editing_workspace_name, None);
        assert_eq!(st.tree().desktop.active_workspace().unwrap().name, original);
    }
}

/// Activity and Replay read the tree document's history; rewriting the
/// workspace list on a rename made every workspace look deleted and re-added.
#[test]
fn a_workspace_rename_writes_only_the_name_into_the_tree_document() {
    use automerge::ReadDoc as _;
    fn workspace_objects(doc: &automerge::AutoCommit) -> (automerge::ObjId, Vec<automerge::ObjId>) {
        let list = doc.get(&automerge::ROOT, "workspaces").unwrap().unwrap().1;
        let entries = (0..doc.length(&list))
            .map(|i| doc.get(&list, i).unwrap().unwrap().1)
            .collect();
        (list, entries)
    }

    let mut tree = WorkspaceTree::bootstrap_default();
    tree.adopt_ownerless_workspaces_with(shell::shell::state::local_user_id_for_room);
    tree.desktop
        .workspaces
        .push(shell::shell::state::Workspace::new(
            shell::shell::state::WorkspaceId(0x7e57_0002),
            "Lab",
        ));
    let chrome = ChromeState::new(tree);
    let (before, objects) = {
        let st = chrome.borrow();
        let mut tree = st.tree_mut();
        tree.seed_doc_from_cache();
        (tree.current_heads(), workspace_objects(&tree.doc))
    };

    chrome.borrow_mut().editing_workspace_name = Some("Studio".into());
    chrome.borrow_mut().commit_workspace_name();

    let (name, patches, objects_after) = {
        let st = chrome.borrow();
        let mut tree = st.tree_mut();
        let after = tree.current_heads();
        (
            tree.desktop.workspaces[0].name.clone(),
            tree.doc.diff(&before, &after),
            workspace_objects(&tree.doc),
        )
    };
    assert_eq!(name, "Studio");
    assert_eq!(
        patches.len(),
        1,
        "a rename is one write, not a rewrite of the workspace list: {patches:?}"
    );
    assert_eq!(patches[0].obj, objects.1[0]);
    assert!(
        matches!(
            &patches[0].action,
            automerge::PatchAction::PutMap { key, .. } if key == "name"
        ),
        "{patches:?}"
    );
    assert_eq!(
        objects_after, objects,
        "the list and every workspace entry keep their objects"
    );
}

#[test]
fn topbar_avatars_cap_at_three_and_never_leak_into_an_empty_workspace() {
    // Phase `topbar_renders_up_to_three_avatars_then_overflow_chip`.
    {
        // The top bar reads the active workspace's roster first (tree-level
        // fallback only when it is empty), so seed onto the active workspace.
        let mut tree = WorkspaceTree::bootstrap_default();
        let active_id = tree.desktop.active;
        let active_ws = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == active_id)
            .expect("active workspace");

        // 2 peers → 2 avatars, no overflow.
        active_ws.collaborators =
            vec![Collaborator::new("a", "Ada"), Collaborator::new("b", "Ben")];
        let top = TopBarChrome::from_tree(&tree.desktop);
        assert_eq!(top.collaborator_initials(), &['A', 'B']);
        assert_eq!(top.collaborator_overflow(), 0);

        // 5 peers → first 3 initials drawn + "+2" chip.
        let active_ws = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == active_id)
            .expect("active workspace");
        active_ws.collaborators = vec![
            Collaborator::new("a", "Ada"),
            Collaborator::new("b", "Ben"),
            Collaborator::new("c", "Cae"),
            Collaborator::new("d", "Dia"),
            Collaborator::new("e", "Eli"),
        ];
        let top = TopBarChrome::from_tree(&tree.desktop);
        assert_eq!(top.collaborator_initials(), &['A', 'B', 'C']);
        assert_eq!(top.collaborator_overflow(), 2);
    }

    // Phase `topbar_does_not_leak_tree_collaborators_into_an_empty_workspace`.
    {
        // The active workspace roster is authoritative. A tree-level roster may
        // describe people elsewhere, but must not create phantom room presence.
        let mut tree = WorkspaceTree::bootstrap_default();
        let active_id = tree.desktop.active;
        let active_ws = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == active_id)
            .expect("active workspace");
        active_ws.collaborators.clear();
        tree.desktop.collaborators = vec![Collaborator::new("a", "Ada")];

        let top = TopBarChrome::from_tree(&tree.desktop);
        assert!(top.collaborator_initials().is_empty());
        assert_eq!(top.collaborator_overflow(), 0);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn tab_presence_seeds_per_tab_peers_and_colours_without_phantom_carets() {
    // Phase `sync_tab_presence_seeds_per_tab_peer_registrations`.
    {
        // A `Collaborator` with `active_tab` must reach
        // `PresenceCtx.peers_on(PresenceKey::Tab{hex(id)})` after sync — this is
        // what lights the tab-pill dots, whatever the top-bar count says.
        use shell::shell::sync_tab_presence_from_tree;
        use ui::permissions::UserId;
        use ui::presence::{PresenceCtx, PresenceKey};

        let mut tree = WorkspaceTree::bootstrap_demo();
        // A real tab id from the tree, so the actual id formatting is exercised.
        let ws = tree.desktop.active_workspace().expect("active workspace");
        let strip = ws.strips.first().expect("strip");
        let grp = strip.window_groups.first().expect("group");
        let tab_a = grp.tabs[0].id;
        let tab_b = grp.tabs[1].id;

        // Attribute Ada to tab_a, Ben to tab_b, Cae to no tab at all.
        let ws_mut = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == tree.desktop.active)
            .expect("active ws mut");
        ws_mut.collaborators = vec![
            Collaborator::with_active_tab("a", "Ada", tab_a),
            Collaborator::with_active_tab("b", "Ben", tab_b),
            Collaborator::new("c", "Cae"),
        ];

        let mut ctx = PresenceCtx::empty();
        sync_tab_presence_from_tree(&tree, &mut ctx);

        let key_a = PresenceKey::Tab {
            id: format!("{tab_a:x}"),
        };
        let key_b = PresenceKey::Tab {
            id: format!("{tab_b:x}"),
        };
        assert_eq!(ctx.peers_on(&key_a), &[UserId("a".into())]);
        assert_eq!(ctx.peers_on(&key_b), &[UserId("b".into())]);

        // Cae has no `active_tab`, so no tab id may attribute her.
        let all_peers: Vec<&UserId> = ctx.here.values().flat_map(|v| v.iter()).collect();
        assert!(
            all_peers.iter().all(|u| u.0 != "c"),
            "collaborator with no active_tab must not register anywhere: {all_peers:?}"
        );
    }

    // Phase `sync_tab_presence_seeds_per_user_color_overrides_from_collaborators`.
    {
        // A `cursor_color` override must copy into `PresenceCtx::color_overrides`
        // during sync so every affordance reads one authoritative colour; peers
        // without one fall through to the palette-pinned `user_color`.
        use shell::shell::sync_tab_presence_from_tree;
        use ui::permissions::UserId;
        use ui::presence::PresenceCtx;

        let mut tree = WorkspaceTree::bootstrap_default();
        let ws_mut = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == tree.desktop.active)
            .expect("active ws mut");
        // Ada has an override, Ben does not; Cae is on the tree-level roster, and
        // her override must be seeded too.
        ws_mut.collaborators = vec![
            Collaborator::new("a", "Ada").with_cursor_color("#abcdef"),
            Collaborator::new("b", "Ben"),
        ];
        tree.desktop.collaborators =
            vec![Collaborator::new("c", "Cae").with_cursor_color("#112233")];

        let mut ctx = PresenceCtx::empty();
        sync_tab_presence_from_tree(&tree, &mut ctx);

        assert_eq!(
            ctx.color_for(&UserId("a".into())),
            (0xab, 0xcd, 0xef),
            "Ada's #abcdef override should be honoured",
        );
        assert_eq!(
            ctx.color_for(&UserId("c".into())),
            (0x11, 0x22, 0x33),
            "tree-level collaborator's override should also seed",
        );
        // Ben has no override → falls through to the palette-pinned colour.
        assert_eq!(
            ctx.color_for(&UserId("b".into())),
            ui::presence::user_color("b"),
            "no-override peer must still use palette fallback",
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn root_clicks_and_drops_toggle_overlays_and_bump_revisions() {
    // Phase `chrome_file_drop_appends_tab_and_bumps_revision`.
    {
        // Production-path counterpart: `ChromeState::handle_file_drop` (invoked on
        // `WindowEvent::DroppedFile`) and `apply_workspace_drop_from_path`.
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let before_rev = chrome.borrow().revision;
        let before_tab_count = chrome.borrow().tree().desktop.workspaces[0].strips[0].window_groups
            [0]
        .tabs
        .len();

        let tab_id = chrome
            .borrow_mut()
            .handle_file_drop(std::path::Path::new("/tmp/main.ts"))
            .expect("tab_id");

        let st = chrome.borrow();
        assert!(
            st.revision > before_rev,
            "handle_file_drop should bump the chrome revision so widgets re-sync"
        );
        let group = &st.tree().desktop.workspaces[0].strips[0].window_groups[0];
        assert_eq!(group.active_tab, tab_id);
        assert_eq!(group.tabs.len(), before_tab_count + 1);
        assert_eq!(group.tabs.last().unwrap().title, "main.ts");
    }

    // Phase `overview_button_click_toggles_overview_overlay`.
    // Clicking the top-bar overview button must toggle
    // `chrome.overlays.overview`, driven end-to-end. Reproduces the
    // user-reported "overview button doesn't work" bug: if dispatch is
    // intercepted before `TopBarChrome` sees the click, `overview` stays false.
    {
        use automerge::AutoCommit;
        use taffy::NodeId;
        use winit::dpi::PhysicalPosition;
        use winit::event::{ElementState, MouseButton, WindowEvent};

        let (mut bar, chrome, taffy, node, hit) = topbar_with_overview_btn_hit();

        // The widget tracks the cursor itself, because winit delivers MouseInput
        // without a position.
        let mut state = AutoCommit::new();
        let root = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let modifiers = winit::event::Modifiers::default();

        let cm = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(hit.x, hit.y),
        };
        bar.handle_event(
            &cm,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: ui::widget::Edits::lend(&mut state),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        let mi = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        bar.handle_event(
            &mi,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: ui::widget::Edits::lend(&mut state),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );

        assert!(
            handled,
            "click on overview button should be handled by the top bar"
        );
        assert!(
            chrome.borrow().overlays.overview,
            "first click should open the overview overlay"
        );

        let mut handled2 = false;
        bar.handle_event(
            &mi,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: ui::widget::Edits::lend(&mut state),
                path: &root,
                handled: &mut handled2,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert!(handled2);
        assert!(
            !chrome.borrow().overlays.overview,
            "second click should close the overview overlay again"
        );
    }

    // Phase `top_right_corner_click_opens_quick_settings`.
    // The painted tray remains inset, but the physical top-right screen corner
    // is part of its interaction target so a fast edge throw opens Quick Settings.
    {
        use automerge::AutoCommit;
        use taffy::NodeId;
        use winit::dpi::PhysicalPosition;
        use winit::event::{ElementState, MouseButton, WindowEvent};

        let (mut bar, chrome, taffy, node, _) = topbar_with_overview_btn_hit();
        let mut state = AutoCommit::new();
        let root = automerge::ROOT.clone();
        let mut focus: Option<NodeId> = None;
        let modifiers = winit::event::Modifiers::default();
        let mut handled = false;
        bar.handle_event(
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: PhysicalPosition::new(1599.0, 1.0),
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: ui::widget::Edits::lend(&mut state),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        bar.handle_event(
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: ui::widget::Edits::lend(&mut state),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );

        assert!(
            handled,
            "the top-right corner must be an active tray target"
        );
        assert!(chrome.borrow().overlays.quick_settings);
    }

    // Phase `full_root_click_toggles_overview_via_topbar_button`.
    // Drives the **full** `build_tilo_root` tree, where routing via
    // `ShellOverlayHost::handle_event` once failed to reach `TopBarChrome`.
    // The button rect comes from a throwaway sibling `TopBarChrome` in the
    // same viewport — topbar geometry is DPI/width-driven, so the rects match.
    {
        fn overview_hit(widget: &dyn Widget) -> Option<Point> {
            widget
                .test_rects()
                .into_iter()
                .find(|(name, rect)| name == "topbar_overview_button" && rect.area() > 0.0)
                .map(|(_, rect)| rect.center())
                .or_else(|| widget.children().into_iter().find_map(overview_hit))
        }
        use automerge::AutoCommit;
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
        use ui::presence::PresenceCtx;
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;
        use vello::Scene;
        use vello::kurbo::{Affine, Point};

        const W: f32 = 1600.0;
        const H: f32 = 900.0;

        let tree = WorkspaceTree::bootstrap_default();

        let (mut root, chrome) = build_tilo_root_with_chrome(&tree);
        assert!(!chrome.borrow().overlays.overview);

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root_node = root.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(W),
                        height: Dimension::length(H),
                    },
                    ..Default::default()
                },
                &[root_node],
            )
            .unwrap();
        let mut tr = TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: AvailableSpace::Definite(W),
                    height: AvailableSpace::Definite(H),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = node_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .expect("compute_layout");
        root.apply_layout(&taffy, root_node, Point::ORIGIN);

        // A draw pass is required so the inner TopBarChrome populates its
        // overview_btn_rect, which handle_event hit-tests against.
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        root.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let hit = overview_hit(root.as_ref()).expect("painted root exports its overview button");

        let mut state = AutoCommit::new();
        let root_path = automerge::ROOT.clone();
        let modifiers = winit::event::Modifiers::default();
        let mut focus: Option<NodeId> = None;

        click_full_root(
            &mut root, &taffy, root_node, &mut state, &root_path, &mut focus, modifiers, hit,
        );
        assert!(
            chrome.borrow().overlays.overview,
            "click on overview button via full root must open the overview overlay"
        );

        // A second click must close it wherever the handler lives —
        // `ShellOverlayHost` currently swallows any click while `flags.overview`
        // is set, so only the end effect matters.
        click_full_root(
            &mut root, &taffy, root_node, &mut state, &root_path, &mut focus, modifiers, hit,
        );
        assert!(
            !chrome.borrow().overlays.overview,
            "second click on the overview button must close the overview overlay"
        );
    }

    // Phase `overview_toggle_signals_animation_frames_until_settled`.
    // Clicking must start an animation the host keeps ticking:
    // `needs_animation_frame()` stays true through the transition and `App`
    // polls it to schedule redraws. Without it the crossfade paints one frame
    // and freezes (the "overview button doesn't work" report).
    {
        use automerge::AutoCommit;
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
        use ui::presence::PresenceCtx;
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;
        use vello::Scene;
        use vello::kurbo::{Affine, Point};

        const W: f32 = 1600.0;
        const H: f32 = 900.0;

        let tree = WorkspaceTree::bootstrap_default();
        let (mut root, chrome) = build_tilo_root_with_chrome(&tree);

        assert!(
            !root.needs_animation_frame(),
            "static root should not request animation frames"
        );

        // Toggled directly through the chrome handle — only the animation signal
        // is under test here.
        chrome.borrow_mut().overlays.overview = true;
        chrome.borrow_mut().bump();

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = root.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(W),
                        height: Dimension::length(H),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        let mut tr = TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: AvailableSpace::Definite(W),
                    height: AvailableSpace::Definite(H),
                },
                |known, available, nid, _, _| {
                    if let Some(&ptr) = node_map.get(&nid) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .expect("compute_layout");
        root.apply_layout(&taffy, node, Point::ORIGIN);

        // First draw: sets `overview_target = 1.0` and arms `transition_started`.
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        let _ = AutoCommit::new();
        root.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        assert!(
            root.needs_animation_frame(),
            "host must request animation frames while the overview crossfade is in flight"
        );

        // Sleep past the configured transition duration, then draw once more so
        // `tick_overview_transition`'s elapsed check clears `transition_started`.
        std::thread::sleep(std::time::Duration::from_millis(400));
        root.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        assert!(
            !root.needs_animation_frame(),
            "host must stop requesting animation frames once the crossfade settles"
        );
    }
}

/// Builds a `TopBarChrome` over a fresh bootstrap tree, lays it out at a
/// fixed 1600x900 viewport and draws once so `overview_btn_rect` is
/// populated, returning the pieces a caller needs to dispatch a synthetic
/// click plus the button's screen-space centre. Split out of
/// [`overview_button_click_toggles_overview_overlay`] to keep it under the
/// line limit.
fn topbar_with_overview_btn_hit() -> (
    TopBarChrome,
    Chrome,
    taffy::TaffyTree<()>,
    taffy::NodeId,
    vello::kurbo::Point,
) {
    use std::collections::HashMap;
    use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
    use ui::presence::PresenceCtx;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use vello::Scene;
    use vello::kurbo::{Affine, Point};

    let tree = WorkspaceTree::bootstrap_default();
    let chrome = ChromeState::new(tree.clone());
    assert!(
        !chrome.borrow().overlays.overview,
        "precondition: overview starts closed"
    );

    let mut bar = TopBarChrome::from_tree(&tree.desktop)
        .with_chrome(shell::shell::topbar_chrome::native_source(chrome.clone()));

    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
    let viewport = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(1600.0),
                    height: Dimension::length(900.0),
                },
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    taffy
        .compute_layout_with_measure(
            viewport,
            Size {
                width: AvailableSpace::Definite(1600.0),
                height: AvailableSpace::Definite(900.0),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .expect("compute_layout");
    bar.apply_layout(&taffy, node, Point::ORIGIN);

    // Draw once to populate `overview_btn_rect`.
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let btn = bar.overview_btn_rect();
    assert!(
        btn.area() > 0.0,
        "draw should have populated overview_btn_rect"
    );
    let hit = Point::new(f64::midpoint(btn.x0, btn.x1), f64::midpoint(btn.y0, btn.y1));

    (bar, chrome, taffy, node, hit)
}

/// One cursor-move + left-press at `hit`, mirroring how a real click event
/// pair reaches the widget tree. Split out of
/// [`full_root_click_toggles_overview_via_topbar_button`] to keep it under
/// the line limit.
fn click_full_root(
    root: &mut Box<dyn Widget>,
    taffy: &taffy::TaffyTree<()>,
    root_node: taffy::NodeId,
    state: &mut automerge::AutoCommit,
    root_path: &automerge::ObjId,
    focus: &mut Option<taffy::NodeId>,
    modifiers: winit::event::Modifiers,
    hit: vello::kurbo::Point,
) {
    use winit::dpi::PhysicalPosition;
    use winit::event::{ElementState, MouseButton, WindowEvent};

    let cm = WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: PhysicalPosition::new(hit.x, hit.y),
    };
    let mut handled = false;
    root.handle_event(
        &cm,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &modifiers,
            state: ui::widget::Edits::lend(state),
            path: root_path,
            handled: &mut handled,
            focus_target: focus,
            node_id: root_node,
            taffy,
        },
    );

    let mi = WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    };
    let mut handled = false;
    root.handle_event(
        &mi,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &modifiers,
            state: ui::widget::Edits::lend(state),
            path: root_path,
            handled: &mut handled,
            focus_target: focus,
            node_id: root_node,
            taffy,
        },
    );
}
