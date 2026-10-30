//! `active_app_title`/`active_tab_id` must return `None`, not
//! `strips.first()`, when no real focus target is tracked.
use super::*;
use crate::shell::state::FreshId as _;
use crate::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
    WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree,
};
use std::collections::HashMap as StdMap;

fn tree_with(active_tab_id: u64) -> WorkspaceTree {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group_id: GroupId = 8;
    let strip = Strip {
        id: strip_id,
        name: "Solo".into(),
        window_groups: vec![StateGroup {
            id: group_id,
            tabs: vec![
                Tab {
                    id: 100,
                    title: "First".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Notes".into()),
                        label: "first".into(),
                    },
                    app_title: None,
                },
                Tab {
                    id: 200,
                    title: "Second".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Notes".into()),
                        label: "second".into(),
                    },
                    app_title: None,
                },
            ],
            active_tab: active_tab_id,
            width: StateWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    };
    let mut ws = Workspace::new(ws_id, "Home");
    ws.strips = vec![strip];
    let mut active_strip = StdMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = StdMap::new();
    active_group.insert(strip_id, group_id);
    WorkspaceTree {
        desktop: crate::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            ..Default::default()
        },
        ..WorkspaceTree::default()
    }
}

#[test]
fn active_app_title_follows_group_active_tab() {
    let handle = crate::shell::state::WorkspaceTreeHandle::new(tree_with(200));
    let chrome = ChromeState::new_with_handle(handle);
    let st = chrome.borrow();
    assert_eq!(st.active_app_title().as_deref(), Some("Second"));
}

#[test]
fn active_tab_id_matches_the_active_tab_pointer() {
    let handle = crate::shell::state::WorkspaceTreeHandle::new(tree_with(100));
    let chrome = ChromeState::new_with_handle(handle);
    let st = chrome.borrow();
    assert_eq!(st.active_tab_id(), Some(100));
}

#[test]
fn no_active_returns_none_instead_of_first_group() {
    let handle = crate::shell::state::WorkspaceTreeHandle::new(WorkspaceTree::default());
    let chrome = ChromeState::new_with_handle(handle);
    let st = chrome.borrow();
    assert_eq!(st.active_tab_id(), None);
    assert!(st.active_app_title().is_none());
}

#[test]
fn without_active_strip_returns_none_not_first_strip() {
    let mut tree = tree_with(100);
    tree.desktop.active_strip.clear();
    let handle = crate::shell::state::WorkspaceTreeHandle::new(tree);
    let chrome = ChromeState::new_with_handle(handle);
    let st = chrome.borrow();
    assert_eq!(st.active_tab_id(), None);
}

#[test]
fn switching_strips_moves_focus_and_clamps_at_the_ends() {
    let cell = ChromeState::new(WorkspaceTree::bootstrap_demo());
    let mut st = cell.borrow_mut();
    {
        let mut tree = st.tree_mut();
        let ws = tree.desktop.active_workspace_mut().expect("a workspace");
        let base = ws.strips.len();
        for n in 0..3u64 {
            let id = StripId(9000 + n);
            ws.strips.push(Strip::new(id, format!("Extra {n}")));
        }
        drop(tree);
        assert!(base + 3 >= 4, "fixture needs at least four strips");
    }
    st.bump();

    let (count, focused) = st.strip_focus();
    assert!(
        count >= 4,
        "fixture should hold at least four strips, saw {count}"
    );
    assert_eq!(focused, 0, "a fresh workspace focuses its first strip");

    assert!(
        st.switch_strip(1),
        "moving down a strip should change focus"
    );
    assert_eq!(st.strip_focus().1, 1);

    assert!(st.switch_strip(1));
    assert_eq!(st.strip_focus().1, 2);

    assert!(st.switch_strip(-1));
    assert_eq!(st.strip_focus().1, 1);

    // Clamps rather than wrapping, matching workspace switching —
    // and a step past the end says where the person is instead of
    // looking like a chord that reached nothing.
    while st.switch_strip(-1) {}
    assert_eq!(st.strip_focus().1, 0);
    assert!(!st.switch_strip(-1), "focus wrapped past the first strip");
    assert_eq!(
        st.acknowledger
            .current(std::time::Instant::now())
            .map(|ack| ack.text().to_string()),
        Some(crate::shell::i18n::t("strip-first-reached")),
        "the refused step says it is the first strip"
    );

    while st.switch_strip(1) {}
    assert_eq!(st.strip_focus().1, count - 1);
    assert!(!st.switch_strip(1), "focus wrapped past the last strip");
    assert_eq!(
        st.acknowledger
            .current(std::time::Instant::now())
            .map(|ack| ack.text().to_string()),
        Some(crate::shell::i18n::t("strip-last-reached"))
    );
}

/// A workspace step past either end is answered, not swallowed: the
/// person cannot otherwise tell "I am at the end" from "the chord
/// did not register", and on one workspace the chord is a permanent
/// no-op.
#[test]
fn stepping_past_the_workspace_list_says_where_the_person_is() {
    let cell = ChromeState::new(WorkspaceTree::bootstrap_demo());
    let mut st = cell.borrow_mut();
    while st.switch_workspace(-1) {}
    assert!(!st.switch_workspace(-1));
    assert_eq!(
        st.acknowledger
            .current(std::time::Instant::now())
            .map(|ack| ack.text().to_string()),
        Some(crate::shell::i18n::t("workspace-first-reached"))
    );
    while st.switch_workspace(1) {}
    assert!(!st.switch_workspace(1));
    assert_eq!(
        st.acknowledger
            .current(std::time::Instant::now())
            .map(|ack| ack.text().to_string()),
        Some(crate::shell::i18n::t("workspace-last-reached"))
    );
}

#[test]
fn focusing_a_strip_by_index_selects_that_strip() {
    let cell = ChromeState::new(WorkspaceTree::bootstrap_demo());
    let mut st = cell.borrow_mut();
    {
        let mut tree = st.tree_mut();
        let ws = tree.desktop.active_workspace_mut().expect("a workspace");
        for n in 0..3u64 {
            ws.strips
                .push(Strip::new(StripId(9100 + n), format!("S{n}")));
        }
        drop(tree);
    }
    let (count, _) = st.strip_focus();
    for i in 0..count {
        st.focus_strip_index(i);
        assert_eq!(st.strip_focus().1, i, "focusing index {i} landed elsewhere");
    }
    // Out of range is a no-op, not a panic and not a silent clamp.
    let before = st.strip_focus().1;
    assert!(!st.focus_strip_index(count + 5));
    assert_eq!(st.strip_focus().1, before);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn focus_and_workspace_names_survive_restart_and_replicate() {
    fn focus_tab_and_strip_focus_survive_restart() {
        let dir = tempfile::tempdir().expect("state dir");
        let path = dir.path().join("workspace.json");
        let mut tree = tree_with(100);
        let workspace = tree.desktop.active;
        let first_strip = tree.desktop.workspaces[0].strips[0].id;
        let first_group = tree.desktop.workspaces[0].strips[0].window_groups[0].id;
        let second_strip = StripId::new();
        let second_group = 81;
        tree.desktop.workspaces[0].strips.push(Strip {
            id: second_strip,
            name: "Other".into(),
            window_groups: vec![StateGroup {
                id: second_group,
                tabs: vec![Tab {
                    id: 300,
                    title: "Third".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Notes".into()),
                        label: "third".into(),
                    },
                    app_title: None,
                }],
                active_tab: 300,
                width: StateWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            }],
        });
        tree.persist_path = Some(path.clone());
        tree.queue_save();
        let chrome = ChromeState::new(tree);

        chrome
            .borrow_mut()
            .focus_tab(workspace, first_strip, first_group, 200);
        let restarted = crate::shell::persist::load_tree_from(&path).expect("focus state");
        assert_eq!(
            restarted.desktop.active_group_for(first_strip),
            Some(first_group)
        );
        assert_eq!(
            restarted.desktop.workspaces[0].strips[0].window_groups[0].active_tab,
            200
        );

        assert!(chrome.borrow_mut().focus_strip_index(1));
        let restarted = crate::shell::persist::load_tree_from(&path).expect("strip focus");
        assert_eq!(
            restarted.desktop.active_strip.get(&workspace),
            Some(&second_strip)
        );
    }

    fn committed_workspace_name_survives_restart_and_is_asked_of_the_governor() {
        let dir = tempfile::tempdir().expect("state dir");
        let path = dir.path().join("workspace.json");
        let mut tree = tree_with(100);
        let room = tree.desktop.workspaces[0].room();
        tree.persist_path = Some(path.clone());
        tree.own_workspaces_for_test(|_| "shell-fixture".to_string());
        tree.queue_save();

        let chrome = ChromeState::new_with_fixture_handle(
            tree.shared(),
            crate::shell::hosted_inputs::HostedTileInputs::fixture()
                .expect("hosted fixture inputs"),
        );
        {
            let mut state = chrome.borrow_mut();
            assert!(state.can_rename_active_workspace());
            state.editing_workspace_name = Some("  Project Aurora  ".into());
            state.commit_workspace_name();
        }

        let restarted = crate::shell::persist::load_tree_from(&path).expect("renamed state");
        assert_eq!(
            restarted.desktop.active_workspace().unwrap().name,
            "Project Aurora"
        );
        let asked = chrome.borrow().tree_mut().take_governance_requests();
        assert!(
            matches!(
                asked.as_slice(),
                [(
                    asked_room,
                    crate::shell::state::GovernanceRequest::Command(
                        tilo_desktop_model::commands::DesktopCommand::RenameWorkspace { name }
                    )
                )] if *asked_room == room && name == "Project Aurora"
            ),
            "the rename is asked of the room's governor, which every member reads it from"
        );
    }

    focus_tab_and_strip_focus_survive_restart();
    committed_workspace_name_survives_restart_and_is_asked_of_the_governor();
}

/// A tree with one strip/group holding `tile`, plus the ids to
/// drive `grant_dropped_file_access` against it.
fn tree_with_single_tab(tile: TileContent) -> (WorkspaceTree, StripId, GroupId, u64) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group_id: GroupId = 8;
    let tab_id = 4242u64;
    let strip = Strip {
        id: strip_id,
        name: "Solo".into(),
        window_groups: vec![StateGroup {
            id: group_id,
            tabs: vec![Tab {
                id: tab_id,
                title: "Dropped".into(),
                tile,
                app_title: None,
            }],
            active_tab: tab_id,
            width: StateWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    };
    let mut ws = Workspace::new(ws_id, "Home");
    ws.strips = vec![strip];
    let mut active_strip = StdMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = StdMap::new();
    active_group.insert(strip_id, group_id);
    let tree = WorkspaceTree {
        desktop: crate::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            ..Default::default()
        },
        ..WorkspaceTree::default()
    };
    (tree, strip_id, group_id, tab_id)
}

#[test]
fn an_app_ended_for_memory_is_named_and_reopens_where_it_was() {
    let (tree, _strip_id, group_id, tab_id) = tree_with_single_tab(TileContent::Foreign {
        app_id: "so.tilo.mail".into(),
        surface_id: 77,
    });
    let handle = crate::shell::state::WorkspaceTreeHandle::new(tree);
    let chrome = ChromeState::new_with_handle(handle);
    let lost = {
        // The window goes away with the app: its place stays, as the app's
        // card, and nothing opens the app again on its own.
        let st = chrome.borrow();
        let mut tree = st.tree_mut();
        assert!(
            crate::shell::tab_binding::keep_tab_of_vanished_surface(&mut tree, 78, None).is_none(),
            "no tab showed another window"
        );
        crate::shell::tab_binding::keep_tab_of_vanished_surface(&mut tree, 77, Some("so.tilo.mail"))
            .expect("the window's tab stays")
    };
    assert_eq!(lost, tab_id);
    assert!(
        matches!(
            &chrome.borrow().tree().desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile,
            TileContent::LocalApp { note, .. } if note == crate::shell::peer_windows::STOPPED
        ),
        "the card says the app stopped"
    );
    let notice = crate::shell::launcher::memory_notice(
        "Mail",
        &crate::shell::launcher::MemoryEnd {
            cause: ::launcher::memory_pressure::Cause::Kernel,
            job: None,
        },
    );
    assert_eq!(notice.title, "Mail ran out of memory and was closed");
    let id = chrome
        .borrow_mut()
        .push_out_of_memory_notice(notice, vec![lost]);
    assert!(chrome.borrow_mut().activate_notification(id), "Reopen");
    let st = chrome.borrow();
    let tree = st.tree();
    let group = &tree.desktop.workspaces[0].strips[0].window_groups[0];
    assert_eq!(group.id, group_id, "in its own group, where it was");
    assert_eq!(group.tabs[0].id, tab_id, "the same tab, with its document");
    assert!(
        matches!(&group.tabs[0].tile, TileContent::LocalApp { app_id, note, .. } if app_id == "so.tilo.mail" && note.is_empty()),
        "opened again as the same app: {:?}",
        group.tabs[0].tile
    );
    drop(tree);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn dropped_files_grant_the_real_app_id() {
    /// Gap-2 proof: when the drop target is a real adopted Wayland client,
    /// the recorded grant is keyed by that client's actual
    /// `xdg_toplevel::set_app_id` value — not a re-derived display label.
    fn dropped_file_grant_keyed_by_foreign_tabs_real_wayland_app_id() {
        let (tree, strip_id, group_id, tab_id) = tree_with_single_tab(TileContent::Foreign {
            app_id: "org.gimp.GIMP".into(),
            surface_id: 77,
        });
        let ws_id = tree.desktop.active;
        let handle = crate::shell::state::WorkspaceTreeHandle::new(tree);
        let chrome = ChromeState::new_with_handle(handle);
        let st = chrome.borrow();

        let path = std::path::Path::new("/drop/photo.xcf");
        st.grant_dropped_file_access(path, strip_id, group_id, tab_id);

        let guard = st.tree();
        let ws = guard
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == ws_id)
            .unwrap();
        let grants = ws.app_grants_snapshot();
        drop(guard);
        let caps = grants.get("org.gimp.GIMP").unwrap_or_else(|| {
            panic!(
                "grant must be keyed by the foreign client's real app-id; \
                 grants = {grants:?}"
            )
        });
        assert!(
            caps.iter().any(|c| matches!(
                c,
                Capability::Filesystem { path: p, .. } if p == path
            )),
            "the dropped path must be recorded as a Filesystem grant: {caps:?}"
        );
        assert!(
            !grants.contains_key("Foreign"),
            "grant must not be keyed by a display label: {grants:?}"
        );
    }

    /// A built-in app (no `set_app_id`) is keyed by its canonical
    /// reverse-DNS app-id, not the display label.
    fn dropped_file_grant_keyed_by_builtin_canonical_app_id() {
        let (tree, strip_id, group_id, tab_id) =
            tree_with_single_tab(TileContent::AppPlaceholder {
                kind: AppKind::Other("Editor".into()),
                label: "notes.ts".into(),
            });
        let ws_id = tree.desktop.active;
        let handle = crate::shell::state::WorkspaceTreeHandle::new(tree);
        let chrome = ChromeState::new_with_handle(handle);
        let st = chrome.borrow();

        let path = std::path::Path::new("/drop/notes.ts");
        st.grant_dropped_file_access(path, strip_id, group_id, tab_id);

        let guard = st.tree();
        let ws = guard
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == ws_id)
            .unwrap();
        let grants = ws.app_grants_snapshot();
        drop(guard);
        assert!(
            grants.contains_key("so.tilo.code-editor"),
            "built-in Editor must be keyed by its canonical app-id; \
             grants = {grants:?}"
        );
        assert!(
            !grants.contains_key("Editor"),
            "the display label 'Editor' must no longer be a grant key: {grants:?}"
        );
    }

    dropped_file_grant_keyed_by_foreign_tabs_real_wayland_app_id();
    dropped_file_grant_keyed_by_builtin_canonical_app_id();
}

#[test]
fn chrome_invalidation_and_exposure_snapshots_are_edge_triggered() {
    fn broker_tree_epoch_invalidates_chrome_exactly_once() {
        let cell = ChromeState::new(WorkspaceTree::default());
        let mut state = cell.borrow_mut();
        let initial = state.revision;
        assert!(!state.sync_external_tree_revision());
        state.tree_mut().desktop.external_revision = 1;
        assert!(state.sync_external_tree_revision());
        assert_eq!(state.revision, initial.wrapping_add(1));
        assert!(!state.sync_external_tree_revision());
        assert_eq!(state.revision, initial.wrapping_add(1));
    }

    fn active_exposure_snapshot_is_sorted_deduplicated_and_edge_triggered() {
        let cell = ChromeState::new(WorkspaceTree::default());
        let mut state = cell.borrow_mut();
        let initial = state.revision;
        let a = ActiveExposure {
            peer: 9,
            surface: 42,
            can_control: true,
        };
        let b = ActiveExposure {
            peer: 3,
            surface: 77,
            can_control: false,
        };
        state.set_active_exposures(vec![a, b, a]);
        assert_eq!(state.active_exposures(), &[b, a]);
        assert_eq!(state.revision, initial.wrapping_add(1));

        state.set_active_exposures(vec![a, b]);
        assert_eq!(
            state.revision,
            initial.wrapping_add(1),
            "an unchanged authority snapshot must not force a frame"
        );
    }

    broker_tree_epoch_invalidates_chrome_exactly_once();
    active_exposure_snapshot_is_sorted_deduplicated_and_edge_triggered();
}

#[test]
fn acting_as_a_principal_pins_every_room_to_it() {
    let tree = tree_with(100);
    let room = tree.desktop.workspaces[0].room();
    let chrome = ChromeState::new_with_fixture_handle(
        tree.shared(),
        crate::shell::hosted_inputs::HostedTileInputs::fixture().expect("hosted fixture inputs"),
    );
    chrome.borrow_mut().act_as("renderer".into());
    let state = chrome.borrow();
    assert_eq!(state.local_user_id, "renderer");
    assert_eq!(state.local_user_for_room(&room), "renderer");
    assert_eq!(state.active_local_user(), "renderer");
}
