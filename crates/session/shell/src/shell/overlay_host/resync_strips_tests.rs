//! A `chrome.bump()` after pushing fresh `WindowGroup`s must propagate
//! to the live `GroupRow` widgets on the next `take_structure_dirty`
//! tick, even without `update()` — the `shots` driver's relayout path
//! skips it.
use super::*;
use crate::shell::build_tilo_overlay_host_with_chrome;
use crate::shell::state::FreshId as _;
use crate::shell::state::{
    AppKind, GroupId, PeerOrigin, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
    WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree,
};
use std::collections::HashMap as StdHashMap;

fn one_group_seed() -> (WorkspaceTree, StripId, GroupId) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group_id: GroupId = 100;
    let tab_id: u64 = 1;
    let strip = Strip {
        id: strip_id,
        name: "Solo".into(),
        window_groups: vec![StateGroup {
            id: group_id,
            tabs: vec![Tab {
                id: tab_id,
                title: "T".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Test".into()),
                    label: "s".into(),
                },
            }],
            active_tab: tab_id,
            width: StateWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    };
    let ws = Workspace {
        id: ws_id,
        name: "Test".into(),
        strips: vec![strip],
        ..Workspace::new(ws_id, String::new())
    };
    let mut active_strip = StdHashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = StdHashMap::new();
    active_group.insert(strip_id, group_id);
    let tree = WorkspaceTree {
        desktop: crate::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            collaborators: vec![],
            ..Default::default()
        },
        ..Default::default()
    };
    (tree, strip_id, group_id)
}

#[test]
fn closing_a_window_is_quiet_and_still_reopens_its_exact_tab() {
    let (mut tree, strip_id, group_id) = one_group_seed();
    tree.seed_doc_from_cache();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let counts = {
        let state = chrome.borrow();
        (
            state.notification_count(),
            state.notification_history().len(),
        )
    };
    assert!(host.close_tab_in_group(group_id, Some(1)));
    {
        let state = chrome.borrow();
        assert!(
            state.tree().desktop.workspaces[0].strips[0]
                .window_groups
                .is_empty()
        );
        assert_eq!(
            state.notification_count(),
            counts.0,
            "closing is its own feedback, not a notification"
        );
        assert_eq!(state.notification_history().len(), counts.1);
    }
    host.set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
        .expect("valid default shortcut configuration");
    let app_first =
        winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::SHIFT;
    let mut handled = false;
    assert!(
        !host.handle_shell_keyboard_event(
            &winit::keyboard::Key::Character("T".into()),
            app_first.into(),
            false,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
        ) && !handled,
        "Ctrl+Shift+T goes to the focused app first; the desktop reopens only what it leaves"
    );
    let modifiers = winit::keyboard::ModifiersState::SUPER | winit::keyboard::ModifiersState::SHIFT;
    assert!(host.handle_shell_keyboard_event(
        &winit::keyboard::Key::Character("T".into()),
        modifiers.into(),
        false,
        &AutoCommit::new(),
        &automerge::ROOT,
        &mut handled,
    ));
    assert!(
        handled,
        "the desktop owns Super+Shift+T before the app receives it"
    );
    let state = chrome.borrow();
    let tree = state.tree();
    let group = &tree.desktop.workspaces[0].strips[0].window_groups[0];
    assert_eq!((group.id, group.active_tab), (group_id, 1));
    assert_eq!(
        tree.desktop.active_strip.get(&tree.desktop.active),
        Some(&strip_id)
    );
    drop(tree);
    assert_eq!(state.notification_count(), counts.0);
}

#[test]
fn the_last_window_keeps_only_its_close_visual_until_retirement() {
    let (mut tree, strip_id, group_id) = one_group_seed();
    tree.seed_doc_from_cache();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let mut theme = Theme::dark();
    // Hold the observation interval while cold mounted discovery settles. Normal
    // base-token retirement is proved separately without this fixture override.
    theme.tokens.motion.durations.base_ms = 3000;
    layout_close_fixture(&mut host, &theme);
    assert!(host.close_tab_in_group(group_id, Some(1)));
    assert!(
        chrome.borrow().tree().desktop.workspaces[0].strips[0]
            .window_groups
            .is_empty()
    );
    let group = host
        .group_at_for_tests(strip_id, 0)
        .expect("a final window retains its closing visual");
    assert!(
        group.is_closing(),
        "close must start before canonical resync"
    );
    assert_eq!(group.keyboard_input_scope(), None);
    chrome.borrow_mut().bump();
    let _ = host.take_structure_dirty();
    assert!(
        host.group_at_for_tests(strip_id, 0)
            .is_some_and(|group| group.is_closing())
    );
    assert!(
        chrome.borrow().tree().desktop.workspaces[0].strips[0]
            .window_groups
            .is_empty(),
        "a visual must never resurrect canonical state"
    );
}

fn layout_close_fixture(host: &mut ShellOverlayHost, theme: &Theme) {
    let mut taffy = TaffyTree::new();
    let mut nodes = HashMap::new();
    let root = host.build_layout(&mut taffy, theme, &mut nodes);
    taffy
        .compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(1280.0),
                height: taffy::AvailableSpace::Definite(800.0),
            },
        )
        .expect("close fixture layout");
    host.apply_layout(&taffy, root, Point::ORIGIN);
}

#[test]
fn window_close_retires_without_input_and_undo_during_fade_is_live() {
    let theme = Theme::dark();
    let (mut tree, strip, group) = one_group_seed();
    tree.seed_doc_from_cache();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    layout_close_fixture(&mut host, &theme);
    assert!(host.close_tab_in_group(group, Some(1)));
    for _ in 0..3 {
        host.update(&AutoCommit::new(), &automerge::ROOT);
        let _ = host.take_structure_dirty();
        assert!(
            chrome.borrow().tree().desktop.workspaces[0].strips[0]
                .window_groups
                .is_empty(),
            "a retained visual cannot be harvested as a window"
        );
    }
    std::thread::sleep(
        theme
            .tokens
            .motion
            .durations
            .base()
            .saturating_add(Duration::from_millis(40)),
    );
    host.update(&AutoCommit::new(), &automerge::ROOT);
    let _ = host.take_structure_dirty();
    assert!(
        host.group_at_for_tests(strip, 0).is_none(),
        "frame ticks retire without another input"
    );
    assert!(chrome.borrow_mut().reopen_closed_tab().is_some());
    let _ = host.take_structure_dirty();
    assert!(!host.group_at_for_tests(strip, 0).unwrap().is_closing());
    assert!(host.close_tab_in_group(group, Some(1)));
    assert!(chrome.borrow_mut().reopen_closed_tab().is_some());
    let _ = host.take_structure_dirty();
    let restored = host
        .group_at_for_tests(strip, 0)
        .expect("restored group is visible");
    assert!(
        !restored.is_closing(),
        "undo during the fade owns a fresh live host"
    );
    assert_eq!(restored.keyboard_input_scope(), Some(1));
}

#[test]
fn closing_slots_animate_without_overlapping_their_live_neighbour() {
    for mut theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        theme.tokens.motion.durations.base_ms = 3000;
        let (mut tree, strip, group) = one_group_seed();
        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .push(make_stub_group(101, "Neighbour"));
        tree.seed_doc_from_cache();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        layout_close_fixture(&mut host, &theme);
        let before = host.group_at_for_tests(strip, 0).unwrap().layout_width_px();
        assert!(before > 0.0, "the mounted window has a real slot");
        assert!(host.close_tab_in_group(group, Some(1)));
        let _ = host.take_structure_dirty();
        layout_close_fixture(&mut host, &theme);
        let closing = host.group_at_for_tests(strip, 0).unwrap();
        assert_eq!(
            closing.id, group,
            "the closing window retains its original strip slot"
        );
        let width = closing.layout_width_px();
        let closing_rect = closing.get_rect();
        assert!(
            width > 0.0 && width < before,
            "closing must step the existing spring, not snap its slot: {before} -> {width}"
        );
        let neighbour = host.group_at_for_tests(strip, 1).unwrap();
        assert_eq!(neighbour.id, 101);
        assert!(
            neighbour.get_rect().x0 >= closing_rect.x1,
            "the real strip layout keeps the live neighbour outside the closing slot"
        );
        assert_eq!(
            chrome.borrow().tree().desktop.workspaces[0].strips[0]
                .window_groups
                .len(),
            1,
            "only the neighbour remains canonical"
        );
    }
}

#[test]
fn reduced_motion_close_retires_in_the_next_frame() {
    let (mut tree, strip, group) = one_group_seed();
    tree.seed_doc_from_cache();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    layout_close_fixture(&mut host, &Theme::light().with_reduce_motion(true));
    assert!(host.close_tab_in_group(group, Some(1)));
    assert!(host.group_at_for_tests(strip, 0).unwrap().close_complete());
    let _ = host.take_structure_dirty();
    assert!(host.group_at_for_tests(strip, 0).is_none());
    assert!(
        chrome.borrow().tree().desktop.workspaces[0].strips[0]
            .window_groups
            .is_empty()
    );
}

fn make_stub_group(id: GroupId, label: &str) -> StateGroup {
    let tab_id = id.saturating_add(1000);
    StateGroup {
        id,
        tabs: vec![Tab {
            id: tab_id,
            title: label.into(),
            tile: TileContent::Stub {
                kind: AppKind::Other("Test".into()),
                label: label.into(),
            },
        }],
        active_tab: tab_id,
        width: StateWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn history_nested_openings_retry_retain_and_cancel_exactly() {
    fn history_nested_open_retries_exact_new_record_after_the_parent_loan() {
        let (tree, _, _) = one_group_seed();
        let store = ui::record::RecordStore::in_memory();
        let parent_record = crate::shell::tab_documents::scratch_tab_record(1);
        store.create_record(&parent_record).unwrap();
        let (mut host, chrome) =
            crate::shell::build_tilo_overlay_host_with_records(tree.shared(), false, store.clone());
        let new_id = store
            .with(&parent_record.id, |_| {
                crate::shell::window_actions::open_hosted(
                    &mut chrome.borrow_mut(),
                    AppKind::Other("Notes".into()),
                    "Nested note",
                    crate::shell::window_actions::Destination::Here,
                )
            })
            .unwrap();
        assert!(store.read(&ui::RecordId::for_tab(new_id), |_| ()).is_none());
        host.update(&AutoCommit::new(), &automerge::ROOT);
        assert!(
            store.read(&ui::RecordId::for_tab(new_id), |_| ()).is_some(),
            "normal update must finish this exact action-owned creation after the loan ends"
        );
        assert!(store.read(&parent_record.id, |_| ()).is_some());
    }

    fn history_nested_busy_burst_retains_overflow_and_cancels_closed_or_changed_tabs() {
        let (tree, _, _) = one_group_seed();
        let store = ui::record::RecordStore::in_memory();
        let parent = crate::shell::tab_documents::scratch_tab_record(1);
        store.create_record(&parent).unwrap();
        let (mut host, chrome) =
            crate::shell::build_tilo_overlay_host_with_records(tree.shared(), false, store.clone());
        let ids = store
            .with(&parent.id, |_| {
                let mut state = chrome.borrow_mut();
                let ids: Vec<_> = (0..132)
                    .map(|_| {
                        crate::shell::window_actions::open_hosted(
                            &mut state,
                            AppKind::Other("Test".into()),
                            "Nested",
                            crate::shell::window_actions::Destination::Here,
                        )
                    })
                    .collect();
                {
                    let mut tree = state.tree_mut();
                    for group in tree
                        .desktop
                        .workspaces
                        .iter_mut()
                        .flat_map(|workspace| &mut workspace.strips)
                        .flat_map(|strip| &mut strip.window_groups)
                    {
                        group
                            .tabs
                            .retain(|tab| tab.id != ids[0] && tab.id != ids[100]);
                        for tab in &mut group.tabs {
                            if tab.id == ids[1] || tab.id == ids[101] {
                                tab.tile = TileContent::AppPlaceholder {
                                    kind: AppKind::Other("Changed".into()),
                                    label: "Changed".into(),
                                };
                            }
                        }
                    }
                }
                // A full busy drain must retain authorization and rotate fairly.
                state.retry_new_tab_records();
                ids
            })
            .unwrap();
        host.update(&AutoCommit::new(), &automerge::ROOT);
        let ready_after_first = ids
            .iter()
            .filter(|id| store.is_ready(&ui::RecordId::for_tab(**id)))
            .count();
        assert_eq!(
            ready_after_first, 64,
            "one ordinary update has a bounded creation budget"
        );
        for _ in 0..3 {
            host.update(&AutoCommit::new(), &automerge::ROOT);
        }
        for (index, id) in ids.iter().enumerate() {
            assert_eq!(
                store.is_ready(&ui::RecordId::for_tab(*id)),
                ![0, 1, 100, 101].contains(&index),
                "valid overflow ticket {index} must eventually create; canceled tickets must stay absent"
            );
        }
    }

    fn history_nested_creation_collision_stays_unavailable_without_touching_saved_content() {
        let (tree, strip, _) = one_group_seed();
        let store = ui::record::RecordStore::in_memory();
        let parent = crate::shell::tab_documents::scratch_tab_record(1);
        store.create_record(&parent).unwrap();
        let (mut host, chrome) =
            crate::shell::build_tilo_overlay_host_with_records(tree.shared(), false, store.clone());
        let new_id = store
            .with(&parent.id, |_| {
                crate::shell::window_actions::open_hosted(
                    &mut chrome.borrow_mut(),
                    AppKind::Other("Word".into()),
                    "New document",
                    crate::shell::window_actions::Destination::Here,
                )
            })
            .unwrap();
        let collision = crate::shell::tab_documents::scratch_tab_record(new_id);
        store.create_record(&collision).unwrap();
        let before = store
            .with(&collision.id, |doc| {
                use automerge::transaction::Transactable;
                doc.put(automerge::ROOT, "body", "existing private content")
                    .unwrap();
                doc.get_heads()
            })
            .unwrap();
        for pass in 0..2 {
            if pass == 1 {
                let mut state = chrome.borrow_mut();
                state.tree_mut().desktop.workspaces[0]
                    .strips
                    .push(Strip::new(StripId::new(), "Rebuild"));
                state.bump();
            }
            host.update(&AutoCommit::new(), &automerge::ROOT);
            let group = host.group_at_for_tests(strip, 0).unwrap();
            group.bind_tab_records(&store, crate::shell::tab_documents::scratch_tab_record);
            let tile = group.host_for_tab_mut(new_id).unwrap();
            assert!(
                tile.history_owner().is_none(),
                "failed creation cannot attach the colliding saved record"
            );
            assert!(
                matches!(tile.content(), ui::widget::TileContent::Notice { .. }),
                "failed creation must not display an app over collided content"
            );
            assert_eq!(
                store.with(&collision.id, AutoCommit::get_heads).unwrap(),
                before
            );
        }
        let (mut reopened, _) = crate::shell::build_tilo_overlay_host_with_records(
            chrome.borrow().tree_handle(),
            false,
            store,
        );
        reopened.update(&AutoCommit::new(), &automerge::ROOT);
        assert!(
            reopened
                .group_at_for_tests(strip, 0)
                .unwrap()
                .host_for_tab_mut(new_id)
                .unwrap()
                .history_owner()
                .is_some(),
            "a separately reopened existing resource is not globally blacklisted"
        );
    }

    fn history_nested_creation_pins_final_setup_and_cancels_changed_tickets() {
        for cancellation in ["none", "closed", "reference", "owner", "room", "app"] {
            let (tree, _, _) = one_group_seed();
            let store = ui::record::RecordStore::in_memory();
            let parent = crate::shell::tab_documents::scratch_tab_record(1);
            store.create_record(&parent).unwrap();
            let (mut host, chrome) = crate::shell::build_tilo_overlay_host_with_records(
                tree.shared(),
                false,
                store.clone(),
            );
            let new_id = store
                .with(&parent.id, |_| {
                    let id = crate::shell::window_actions::open_hosted_after_setup(
                        &mut chrome.borrow_mut(),
                        AppKind::Other("Notes".into()),
                        "Nested",
                        crate::shell::window_actions::Destination::Here,
                        |state, tab| {
                            state.tree_mut().native_document_tabs.insert(
                                tab,
                                crate::shell::state::NativeDocumentReference {
                                    provider_id: "dev.tilo.notes".into(),
                                    path: "/untouched/original.txt".into(),
                                },
                            );
                        },
                    );
                    // Even a premature bounded retry cannot acquire the loaned store.
                    chrome.borrow_mut().retry_new_tab_records();
                    id
                })
                .unwrap();
            let replacement = ui::record::RecordStore::in_memory();
            {
                let mut state = chrome.borrow_mut();
                if cancellation == "owner" {
                    state.set_records(replacement.clone());
                }
                let mut tree = state.tree_mut();
                if cancellation == "closed" {
                    for workspace in &mut tree.desktop.workspaces {
                        for strip in &mut workspace.strips {
                            for group in &mut strip.window_groups {
                                group.tabs.retain(|tab| tab.id != new_id);
                            }
                        }
                    }
                }
                if cancellation == "reference" {
                    tree.native_document_tabs.get_mut(&new_id).unwrap().path =
                        "/untouched/different.txt".into();
                }
                if cancellation == "room" {
                    tree.desktop.workspaces[0].set_room(crate::shell::state::RoomId::new());
                }
                if cancellation == "app" {
                    for tab in tree
                        .desktop
                        .workspaces
                        .iter_mut()
                        .flat_map(|workspace| &mut workspace.strips)
                        .flat_map(|strip| &mut strip.window_groups)
                        .flat_map(|group| &mut group.tabs)
                    {
                        if tab.id == new_id {
                            tab.tile = TileContent::AppPlaceholder {
                                kind: AppKind::Other("Editor".into()),
                                label: "Different app".into(),
                            };
                        }
                    }
                }
            }
            host.update(&AutoCommit::new(), &automerge::ROOT);
            assert_eq!(
                store.read(&ui::RecordId::for_tab(new_id), |_| ()).is_some(),
                cancellation == "none",
                "ticket case {cancellation}"
            );
            assert!(
                replacement
                    .read(&ui::RecordId::for_tab(new_id), |_| ())
                    .is_none()
            );
        }
    }

    fn history_nested_owner_replacement_retires_an_already_mounted_pending_binding() {
        let (tree, strip, _) = one_group_seed();
        let old_store = ui::record::RecordStore::in_memory();
        let parent = crate::shell::tab_documents::scratch_tab_record(1);
        old_store.create_record(&parent).unwrap();
        let (mut host, chrome) = crate::shell::build_tilo_overlay_host_with_records(
            tree.shared(),
            false,
            old_store.clone(),
        );
        let new_id = old_store
            .with(&parent.id, |_| {
                let id = crate::shell::window_actions::open_hosted(
                    &mut chrome.borrow_mut(),
                    AppKind::Other("Word".into()),
                    "Nested",
                    crate::shell::window_actions::Destination::Here,
                );
                host.take_structure_dirty();
                id
            })
            .unwrap();
        let replacement = ui::record::RecordStore::in_memory();
        chrome.borrow_mut().set_records(replacement.clone());
        host.update(&AutoCommit::new(), &automerge::ROOT);
        old_store
            .create_record(&crate::shell::tab_documents::scratch_tab_record(new_id))
            .unwrap();
        host.update(&AutoCommit::new(), &automerge::ROOT);
        assert!(
            host.group_at_for_tests(strip, 0)
                .unwrap()
                .host_for_tab_mut(new_id)
                .unwrap()
                .history_owner()
                .is_none(),
            "late old-store hydration cannot revive a replaced pending owner"
        );
        assert!(
            replacement
                .read(&ui::RecordId::for_tab(new_id), |_| ())
                .is_none()
        );
    }

    history_nested_open_retries_exact_new_record_after_the_parent_loan();
    history_nested_busy_burst_retains_overflow_and_cancels_closed_or_changed_tabs();
    history_nested_creation_collision_stays_unavailable_without_touching_saved_content();
    history_nested_creation_pins_final_setup_and_cancels_changed_tickets();
    history_nested_owner_replacement_retires_an_already_mounted_pending_binding();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn resync_follows_document_revisions_for_groups_and_tabs() {
    fn resync_picks_up_new_groups_added_after_bump() {
        let (tree, strip_id, _) = one_group_seed();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        assert!(host.group_at_for_tests(strip_id, 0).is_some());
        assert!(host.group_at_for_tests(strip_id, 1).is_none());
        // Mirror what the `shots` multi-group scene does: push two
        // fresh `WindowGroup`s into the active strip and bump.
        {
            let mut st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            let strip = tree.desktop.workspaces[0]
                .strips
                .iter_mut()
                .find(|s| s.id == strip_id)
                .expect("seed strip");
            strip.window_groups.push(make_stub_group(201, "Stub-A"));
            strip.window_groups.push(make_stub_group(202, "Stub-B"));
            drop(tree);
            st.bump();
        }
        // Single take_structure_dirty tick must pump the resync diff
        // and splice the new groups into the live row.
        let _ = host.take_structure_dirty();
        assert!(
            host.group_at_for_tests(strip_id, 1).is_some(),
            "stub-A spliced into row at index 1"
        );
        assert!(
            host.group_at_for_tests(strip_id, 2).is_some(),
            "stub-B spliced into row at index 2"
        );
        assert!(
            host.group_at_for_tests(strip_id, 3).is_none(),
            "no phantom fourth group"
        );
    }

    fn newly_active_group_retries_reveal_after_its_first_layout() {
        const LAUNCHED_GROUP: GroupId = 999;
        let (mut tree, strip_id, _) = one_group_seed();
        for id in 201..=206 {
            tree.desktop.workspaces[0].strips[0]
                .window_groups
                .push(make_stub_group(id, &format!("Existing-{id}")));
        }
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let root = host.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        taffy
            .compute_layout(
                root,
                Size {
                    width: taffy::AvailableSpace::Definite(1280.0),
                    height: taffy::AvailableSpace::Definite(800.0),
                },
            )
            .expect("initial wide-strip layout");
        host.apply_layout(&taffy, root, Point::ORIGIN);
        let row = host
            .group_row_mut_for_tests(strip_id)
            .expect("seed strip has a live row");
        row.set_scroll_x_for_test(row.max_scroll_x());

        {
            let mut st = chrome.borrow_mut();
            let mut live = st.tree_mut();
            live.desktop.workspaces[0].strips[0]
                .window_groups
                .push(make_stub_group(LAUNCHED_GROUP, "Launched"));
            live.desktop.active_group.insert(strip_id, LAUNCHED_GROUP);
            drop(live);
            st.bump();
        }

        assert!(host.take_structure_dirty(), "launch splices a new group");
        host.tick_scroll_active_into_view_for_tests();

        let mut rebuilt = TaffyTree::new();
        let mut rebuilt_map = HashMap::new();
        let rebuilt_root = host.build_layout(&mut rebuilt, &Theme::dark(), &mut rebuilt_map);
        rebuilt
            .compute_layout(
                rebuilt_root,
                Size {
                    width: taffy::AvailableSpace::Definite(1280.0),
                    height: taffy::AvailableSpace::Definite(800.0),
                },
            )
            .expect("post-launch layout");
        host.apply_layout(&rebuilt, rebuilt_root, Point::ORIGIN);
        host.tick_scroll_active_into_view_for_tests();

        let row = host
            .group_row_mut_for_tests(strip_id)
            .expect("launched strip remains live");
        assert!(
            row.needs_animation_frame(),
            "revealing the off-screen launch must start the strip pan"
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the resync regression retains the complete frame-by-frame topology scenario"
    )]
    fn resync_drops_groups_removed_after_bump() {
        let (mut tree, strip_id, _) = one_group_seed();
        // Seed two extra groups so the first frame already has 3.
        tree.desktop.workspaces[0]
            .strips
            .iter_mut()
            .find(|s| s.id == strip_id)
            .unwrap()
            .window_groups
            .push(make_stub_group(301, "Drop-A"));
        tree.desktop.workspaces[0]
            .strips
            .iter_mut()
            .find(|s| s.id == strip_id)
            .unwrap()
            .window_groups
            .push(make_stub_group(302, "Drop-B"));
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        assert!(host.group_at_for_tests(strip_id, 2).is_some());
        // Remove one group and bump.
        {
            let mut st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            let strip = tree.desktop.workspaces[0]
                .strips
                .iter_mut()
                .find(|s| s.id == strip_id)
                .unwrap();
            strip.window_groups.retain(|g| g.id != 301);
            drop(tree);
            st.bump();
        }
        let _ = host.take_structure_dirty();
        // Row now has 2 groups (original + Drop-B).
        assert!(host.group_at_for_tests(strip_id, 0).is_some());
        assert!(host.group_at_for_tests(strip_id, 1).is_some());
        assert!(
            host.group_at_for_tests(strip_id, 2).is_none(),
            "removed group dropped from row"
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the resync regression keeps the complete document-revision and frame-topology scenario together"
    )]
    fn resync_reorders_existing_groups_and_tabs_after_a_document_revision() {
        let (mut tree, strip_id, first_group) = one_group_seed();
        let state_dir = tempfile::tempdir().expect("state directory");
        let state_path = state_dir.path().join("state.json");
        tree.persist_path = Some(state_path.clone());
        let strip = tree
            .desktop
            .workspaces
            .first_mut()
            .and_then(|workspace| {
                workspace
                    .strips
                    .iter_mut()
                    .find(|strip| strip.id == strip_id)
            })
            .expect("seed strip");
        let first = strip
            .window_groups
            .iter_mut()
            .find(|group| group.id == first_group)
            .expect("seed group");
        first.tabs.extend([
            Tab {
                id: 2,
                title: "Second".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Browser,
                    label: "second".into(),
                },
            },
            Tab {
                id: 3,
                title: "Third".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Terminal,
                    label: "third".into(),
                },
            },
        ]);
        strip.window_groups.push(make_stub_group(201, "Other-A"));
        strip.window_groups.push(make_stub_group(202, "Other-B"));
        tree.seed_doc_from_cache();

        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        assert_eq!(
            host.group_row_mut_for_tests(strip_id)
                .expect("live row")
                .group_ids(),
            vec![first_group, 201, 202],
            "precondition: boot row follows the seed order"
        );

        // This models a later peer/document revision: all ids are still
        // present, so the old membership-only resync would have been a
        // no-op and continued painting the previous topology.
        {
            let mut st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            assert!(tree.reorder_group_for_tests(strip_id, 202, 0));
            assert!(tree.reorder_tab_for_tests(first_group, 2, 0));
            drop(tree);
            st.bump();
        }
        assert!(
            host.take_structure_dirty(),
            "document order change must dirty the live structure"
        );

        let row = host
            .group_row_mut_for_tests(strip_id)
            .expect("live row after revision");
        assert_eq!(
            row.group_ids(),
            vec![202, first_group, 201],
            "existing groups follow the replicated strip order without a restart"
        );
        let first = row
            .groups
            .iter()
            .find(|group| group.id == first_group)
            .expect("reordered group still mounted");
        assert_eq!(
            first.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(),
            vec![3, 1, 2],
            "tab pills and their hosts follow the replicated order together"
        );
        assert_eq!(
            first.tile_kind_labels_for_tests(),
            vec!["Terminal", "Settings", "Browser"],
            "the content hosts moved with their matching tab pills"
        );
        assert_eq!(
            first.active_tab_id(),
            1,
            "a reordering revision must preserve this device's selected tab"
        );

        // Exercise the reverse direction too. A local drag already moves
        // the widget before committing the tree; this is the later
        // replicated/restart path where the tree has to move it back.
        {
            let mut st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            assert!(tree.reorder_group_for_tests(strip_id, 202, 2));
            assert!(tree.reorder_tab_for_tests(first_group, 0, 2));
            drop(tree);
            st.bump();
        }
        assert!(
            host.take_structure_dirty(),
            "reverse document order change must also dirty the live structure"
        );
        let row = host
            .group_row_mut_for_tests(strip_id)
            .expect("live row after reverse revision");
        assert_eq!(row.group_ids(), vec![first_group, 201, 202]);
        let first = row
            .groups
            .iter()
            .find(|group| group.id == first_group)
            .expect("restored group still mounted");
        assert_eq!(
            first.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(first.active_tab_id(), 1);

        // The result has to be durable, not just a live widget permutation.
        chrome.borrow().tree().queue_save();
        let restarted = crate::shell::persist::load_tree_from(&state_path)
            .expect("reordered layout survives a process restart");
        let strip = restarted.desktop.workspaces[0]
            .strips
            .iter()
            .find(|strip| strip.id == strip_id)
            .expect("persisted strip");
        assert_eq!(
            strip
                .window_groups
                .iter()
                .map(|group| group.id)
                .collect::<Vec<_>>(),
            vec![first_group, 201, 202]
        );
        let first = strip
            .window_groups
            .iter()
            .find(|group| group.id == first_group)
            .expect("persisted first group");
        assert_eq!(
            first.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the accepted-room reorder regression retains the complete remote document and frame assertions"
    )]
    fn accepted_room_reorder_resyncs_existing_remote_groups_and_tabs() {
        const HOST_SALT: u16 = 0x2222;
        const PEER_SALT: u16 = 0x3333;
        let remote_id = |counter| (u64::from(PEER_SALT) << 48) | counter;

        let mut local = WorkspaceTree::bootstrap_demo();
        local.set_salt_override(HOST_SALT);
        let local_workspace = local.desktop.active;
        let room = local.room_of(local_workspace).expect("local room");
        local.adopt_ownerless_workspaces(&"owner".to_string());

        let mut peer = WorkspaceTree::bootstrap_demo();
        peer.set_salt_override(PEER_SALT);
        let peer_workspace = peer.desktop.active;
        assert!(peer.set_room(peer_workspace, room));
        // The fixed ids below model windows this peer owns in the shared
        // room. They predate this process's global allocator, so make the
        // fixture publish them instead of treating them as pre-join tabs.
        peer.room_share_floor.remove(&room);
        // Matching governance makes this a legitimate member layout
        // update, not a rejected attempt to replace the room owner.
        peer.adopt_ownerless_workspaces(&"owner".to_string());
        let remote_strip = StripId::from(remote_id(1));
        let remote_group = remote_id(2);
        let tab = |id, title: &str, kind| Tab {
            id,
            title: title.into(),
            tile: TileContent::Stub {
                kind,
                label: title.into(),
            },
        };
        peer.desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == peer_workspace)
            .expect("peer workspace")
            .strips = vec![Strip {
            id: remote_strip,
            name: "Remote".into(),
            window_groups: vec![
                StateGroup {
                    id: remote_group,
                    tabs: vec![
                        tab(remote_id(10), "A", AppKind::Other("Remote".into())),
                        tab(remote_id(11), "B", AppKind::Browser),
                        tab(remote_id(12), "C", AppKind::Terminal),
                    ],
                    active_tab: remote_id(10),
                    width: StateWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                },
                make_stub_group(remote_id(3), "Remote-B"),
                make_stub_group(remote_id(4), "Remote-C"),
            ],
        }];

        let seed = peer
            .export_room_changes_since(room, &[])
            .expect("peer room changes");
        assert!(
            local
                .apply_room_changes(room, &seed, &PeerOrigin::hub(1))
                .expect("legitimate remote layout merges"),
            "initial remote layout is accepted"
        );
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&local);
        let peer_base = peer.room_heads(room);
        assert!(peer.reorder_group_for_tests(remote_strip, remote_id(4), 0));
        assert!(peer.reorder_tab_for_tests(remote_group, 2, 0));
        let delta = peer
            .export_room_changes_since(room, &peer_base)
            .expect("reorder room delta");
        {
            let mut state = chrome.borrow_mut();
            assert!(
                state
                    .tree_mut()
                    .apply_room_changes(room, &delta, &PeerOrigin::hub(1))
                    .expect("accepted reorder delta"),
                "room merger reports the remote reorder"
            );
            assert!(
                !state
                    .tree_mut()
                    .apply_room_changes(room, &seed, &PeerOrigin::hub(1))
                    .expect("an already-superseded room snapshot parses"),
                "a stale room snapshot cannot undo the newer accepted reorder"
            );
            state.bump();
        }
        assert!(host.take_structure_dirty());
        let row = host
            .group_row_mut_for_tests(remote_strip)
            .expect("remote strip remains live");
        assert_eq!(
            row.group_ids(),
            vec![remote_id(4), remote_group, remote_id(3)]
        );
        let group = row
            .groups
            .iter()
            .find(|group| group.id == remote_group)
            .expect("remote tab group remains live");
        assert_eq!(
            group.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(),
            vec![remote_id(12), remote_id(10), remote_id(11)]
        );
        assert_eq!(group.active_tab_id(), remote_id(10));
        assert_eq!(
            group.tile_kind_labels_for_tests(),
            vec!["Terminal", "Settings", "Browser"]
        );
    }

    fn close_then_revision_resync_does_not_restore_the_window_group() {
        let (mut tree, strip_id, group_id) = one_group_seed();
        let state_dir = tempfile::tempdir().expect("temporary state directory");
        let state_path = state_dir.path().join("state.json");
        tree.persist_path = Some(state_path.clone());
        tree.seed_doc_from_cache();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        assert!(host.close_tab_in_group(group_id, Some(1)));

        assert!(
            chrome.borrow().tree().desktop.workspaces[0].strips[0]
                .window_groups
                .is_empty(),
            "close is a tree mutation, not merely a widget fade"
        );
        // This is the exact edge that used to restore the stale group: `+`
        // and many unrelated actions bump chrome, then strip resync reads the
        // persisted tree.
        chrome.borrow_mut().bump();
        let _ = host.take_structure_dirty();
        assert!(
            host.group_at_for_tests(strip_id, 0)
                .is_some_and(|group| group.is_closing()),
            "resync keeps only the dying visual, never canonical window state"
        );
        std::thread::sleep(
            Theme::dark()
                .tokens
                .motion
                .durations
                .base()
                .saturating_add(Duration::from_millis(40)),
        );
        let _ = host.take_structure_dirty();
        assert!(
            host.group_at_for_tests(strip_id, 0).is_none(),
            "the closing visual retires by its motion-token deadline"
        );

        // A fresh compositor must observe the same close. This catches the
        // crash window between the visual animation and a later incidental
        // state write; the close itself has to create the durable file.
        let restarted = crate::shell::persist::load_tree_from(&state_path)
            .expect("the close flushes valid state");
        assert!(
            restarted.desktop.workspaces[0].strips[0]
                .window_groups
                .is_empty(),
            "a fresh load must not restore the group closed before restart"
        );

        // Then drive an unrelated revision through a fresh widget tree. The
        // disk round trip and later resync are independent resurrection
        // hazards, and both must preserve the absence.
        let (mut restarted_host, restarted_chrome) =
            build_tilo_overlay_host_with_chrome(&restarted);
        restarted_chrome.borrow_mut().bump();
        let _ = restarted_host.take_structure_dirty();
        assert!(
            restarted_host.group_at_for_tests(strip_id, 0).is_none(),
            "an unrelated post-restart revision must not recreate the closed group"
        );
    }

    fn resync_no_op_when_revision_unchanged() {
        let (tree, strip_id, group_id) = one_group_seed();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let id_before = host.group_at_for_tests(strip_id, 0).map_or(0, |g| g.id);
        assert_eq!(id_before, group_id);
        // First tick observes initial revision and caches it.
        let _ = host.take_structure_dirty();
        // Second tick without any chrome.bump() must be a no-op —
        // the persisted group id stays the same and no spurious
        // insert happens.
        let _ = host.take_structure_dirty();
        let _st = chrome.borrow();
        assert_eq!(
            host.group_at_for_tests(strip_id, 0).map(|g| g.id),
            Some(group_id),
            "stable revision leaves row identity intact"
        );
        assert!(
            host.group_at_for_tests(strip_id, 1).is_none(),
            "no phantom group inserted by repeated no-op ticks"
        );
    }

    resync_picks_up_new_groups_added_after_bump();
    newly_active_group_retries_reveal_after_its_first_layout();
    resync_drops_groups_removed_after_bump();
    resync_reorders_existing_groups_and_tabs_after_a_document_revision();
    accepted_room_reorder_resyncs_existing_remote_groups_and_tabs();
    close_then_revision_resync_does_not_restore_the_window_group();
    resync_no_op_when_revision_unchanged();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn focus_changes_are_flushed_before_restart_and_never_resurrect_removed_tabs() {
    fn stale_focus_target_cannot_write_a_removed_tab_into_the_tree() {
        let (tree, strip_id, group_id) = one_group_seed();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);

        assert!(
            !host.focus_window_on_strip(strip_id, group_id, 99),
            "a target absent from the live row is stale, not a focus change"
        );
        let state = chrome.borrow();
        let tree = state.tree();
        let group = tree.desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .find(|group| group.id == group_id)
            .expect("seed group remains");
        assert_eq!(
            group.active_tab, 1,
            "stale keyboard/rail focus cannot leave durable state pointing at a deleted tab"
        );
        assert_eq!(
            tree.desktop.active_group_for(strip_id),
            Some(group_id),
            "stale focus must not dislodge the selected group either"
        );
        drop(tree);
        drop(state);

        // The inverse race is just as important: a close may be durable
        // before the next widget resync, leaving the old tab momentarily
        // mounted. Its stale click/chord must still be rejected by the tree.
        {
            let state = chrome.borrow_mut();
            let mut tree = state.tree_mut();
            tree.desktop.workspaces[0].strips[0].window_groups.clear();
            tree.desktop.active_group.remove(&strip_id);
        }
        assert!(
            !host.focus_window_on_strip(strip_id, group_id, 1),
            "a tab left live during close cannot recreate durable selection"
        );
        let state = chrome.borrow();
        assert_eq!(
            state.tree().desktop.active_group_for(strip_id),
            None,
            "a missing group remains unselected until the resync drops its widget"
        );
    }

    fn rail_or_keyboard_focus_is_flushed_before_restart() {
        let (mut tree, strip_id, first_group) = one_group_seed();
        let state_dir = tempfile::tempdir().expect("state directory");
        let state_path = state_dir.path().join("state.json");
        let second_group = 202;
        let second_tab = 1_202;
        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .push(make_stub_group(second_group, "Second"));
        tree.persist_path = Some(state_path.clone());
        tree.queue_save();
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);

        assert!(host.focus_window_on_strip(strip_id, second_group, second_tab));
        let restarted = crate::shell::persist::load_tree_from(&state_path)
            .expect("focused window is restartable");
        assert_eq!(
            restarted.desktop.active_group_for(strip_id),
            Some(second_group)
        );
        let group = restarted.desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .find(|group| group.id == second_group)
            .expect("second group");
        assert_eq!(group.active_tab, second_tab);
        assert_ne!(
            restarted.desktop.active_group_for(strip_id),
            Some(first_group)
        );
    }

    fn pointer_group_focus_is_flushed_before_restart() {
        let (mut tree, strip_id, first_group) = one_group_seed();
        let state_dir = tempfile::tempdir().expect("state directory");
        let state_path = state_dir.path().join("state.json");
        let second_group = 202;
        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .push(make_stub_group(second_group, "Second"));
        tree.persist_path = Some(state_path.clone());
        tree.queue_save();
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let mut taffy = TaffyTree::new();
        let mut nodes = StdHashMap::new();
        let child = host.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(1600.0),
                        height: Dimension::length(900.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        taffy
            .compute_layout(
                root,
                Size {
                    width: taffy::AvailableSpace::Definite(1600.0),
                    height: taffy::AvailableSpace::Definite(900.0),
                },
            )
            .unwrap();
        host.apply_layout(&taffy, child, Point::ORIGIN);
        let point = host
            .group_at_for_tests(strip_id, 1)
            .expect("second group")
            .get_rect()
            .center();

        host.stamp_active_group_from_click(point);

        let restarted = crate::shell::persist::load_tree_from(&state_path)
            .expect("pointer focus is restartable");
        assert_eq!(
            restarted.desktop.active_group_for(strip_id),
            Some(second_group)
        );
        assert_ne!(
            restarted.desktop.active_group_for(strip_id),
            Some(first_group)
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the frame-writeback regression keeps input, persistence, and next-frame assertions together"
    )]
    fn active_tab_focus_is_flushed_before_the_next_frame() {
        let (mut tree, strip_id, group_id) = one_group_seed();
        let state_dir = tempfile::tempdir().expect("state directory");
        let state_path = state_dir.path().join("state.json");
        tree.persist_path = Some(state_path.clone());
        tree.desktop.workspaces[0].strips[0].window_groups[0]
            .tabs
            .push(Tab {
                id: 2,
                title: "Second".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Browser,
                    label: "second".into(),
                },
            });
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
        assert!(host.focus_window_on_strip(strip_id, group_id, 2));

        let restarted = crate::shell::persist::load_tree_from(&state_path)
            .expect("selection writeback creates valid durable state");
        assert_eq!(
            restarted.desktop.workspaces[0].strips[0].window_groups[0].active_tab, 2,
            "a crash after the input boundary must restore the newly selected tab"
        );
        assert_eq!(restarted.desktop.active_group_for(strip_id), Some(group_id));
    }

    fn closing_focused_group_persists_focus_on_the_surviving_window() {
        let (mut tree, strip_id, first_group) = one_group_seed();
        let state_dir = tempfile::tempdir().expect("temporary state directory");
        let state_path = state_dir.path().join("state.json");
        let second_group = 202;
        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .push(make_stub_group(second_group, "Second"));
        tree.persist_path = Some(state_path.clone());
        tree.seed_doc_from_cache();
        tree.queue_save();

        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
        assert!(host.close_tab_in_group(first_group, Some(1)));
        host.tick_active_group_reconcile_for_tests();

        let restarted = crate::shell::persist::load_tree_from(&state_path)
            .expect("focus hand-off is durable during the close fade");
        assert_eq!(
            restarted.desktop.active_group_for(strip_id),
            Some(second_group),
            "restart must focus the surviving sibling, not the group whose close animation began"
        );
        assert_eq!(
            restarted.desktop.workspaces[0].strips[0]
                .window_groups
                .iter()
                .map(|group| group.id)
                .collect::<Vec<_>>(),
            vec![second_group],
            "the close and its focus hand-off must reach disk as one observable state"
        );
    }

    stale_focus_target_cannot_write_a_removed_tab_into_the_tree();
    rail_or_keyboard_focus_is_flushed_before_restart();
    pointer_group_focus_is_flushed_before_restart();
    active_tab_focus_is_flushed_before_the_next_frame();
    closing_focused_group_persists_focus_on_the_surviving_window();
}

#[test]
#[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
fn group_actions_target_exactly_their_group_or_surface() {
    fn closing_one_of_two_same_app_windows_targets_only_its_surface() {
        let (mut tree, strip_id, group_id) = one_group_seed();
        let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
        group.tabs = vec![
            Tab {
                id: 1,
                title: "Chat".into(),
                tile: TileContent::Foreign {
                    app_id: "com.openai.chatgpt".into(),
                    surface_id: 41,
                },
            },
            Tab {
                id: 2,
                title: "Chat".into(),
                tile: TileContent::Foreign {
                    app_id: "com.openai.chatgpt".into(),
                    surface_id: 42,
                },
            },
        ];
        group.active_tab = 1;
        tree.seed_doc_from_cache();

        crate::shell::foreign::store().note_adopted(41);
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        assert!(host.close_tab_in_group(group_id, Some(1)));
        assert_eq!(
            chrome.borrow_mut().take_surface_close_requests(),
            vec![41],
            "close requests are keyed by surface, never by shared app id"
        );
        let _ = host.take_structure_dirty();
        let retained = host
            .group_at_for_tests(strip_id, 0)
            .expect("live close decision");
        assert_eq!(
            retained.active_tab_id(),
            1,
            "a deferred close retains focus"
        );
        assert_eq!(retained.tab_count(), 2, "both client hosts remain visible");
        assert!(
            chrome.borrow_mut().reopen_closed_tab().is_none(),
            "no Undo before destruction"
        );
        crate::shell::tab_binding::close_tab_for_destroyed_surface_and_persist(
            &mut chrome.borrow_mut().tree_mut(),
            41,
        );
        crate::shell::foreign::store().forget(41);
        let remaining = chrome.borrow().tree().desktop.workspaces[0].strips[0].window_groups[0]
            .tabs
            .iter()
            .filter_map(|tab| match tab.tile {
                TileContent::Foreign { surface_id, .. } => Some(surface_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec![42]);

        chrome.borrow_mut().bump();
        let _ = host.take_structure_dirty();
        let group = host
            .group_at_for_tests(strip_id, 0)
            .expect("the sibling window group remains");
        assert_eq!(group.active_tab_id(), 2);
    }

    fn a_deferred_close_click_keeps_the_window_and_animates_once_at_exit() {
        let (mut tree, strip_id, group_id) = one_group_seed();
        let _ = group_id;
        let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
        group.tabs = vec![Tab {
            id: 1,
            title: "Editor".into(),
            tile: TileContent::Foreign {
                app_id: "org.example.Editor".into(),
                surface_id: 51,
            },
        }];
        group.active_tab = 1;
        tree.seed_doc_from_cache();
        crate::shell::foreign::store().note_adopted(51);
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let mut theme = Theme::dark();
        theme.tokens.motion.durations.base_ms = 40;
        layout_close_fixture(&mut host, &theme);

        // The close button: the widget starts retiring, the shell asks the
        // client, which has not exited.
        assert!(
            host.group_at_for_tests(strip_id, 0)
                .expect("window")
                .click_close_for_tests(1)
        );
        host.harvest_pending_tab_closes();
        assert_eq!(chrome.borrow_mut().take_surface_close_requests(), vec![51]);
        let group = host.group_at_for_tests(strip_id, 0).expect("window");
        assert!(
            !group.is_closing(),
            "a client that has not exited keeps its window; no retirement runs"
        );
        assert_eq!(group.tab_count(), 1);

        // The refusal period outlasts the close duration: nothing animates
        // away, is pruned, or is rebuilt with a spawn animation.
        std::thread::sleep(Duration::from_millis(100));
        chrome.borrow_mut().bump();
        let _ = host.take_structure_dirty();
        let group = host.group_at_for_tests(strip_id, 0).expect("still there");
        assert!(!group.is_closing() && group.tab_count() == 1);

        // The client finally exits: exactly one retirement, from the live tab.
        crate::shell::tab_binding::close_tab_for_destroyed_surface_and_persist(
            &mut chrome.borrow_mut().tree_mut(),
            51,
        );
        crate::shell::foreign::store().forget(51);
        chrome.borrow_mut().bump();
        let _ = host.take_structure_dirty();
        assert!(
            host.group_at_for_tests(strip_id, 0)
                .is_some_and(|group| group.is_closing()),
            "the destroy starts the single retirement"
        );
        std::thread::sleep(Duration::from_millis(100));
        let _ = host.take_structure_dirty();
        assert!(host.group_at_for_tests(strip_id, 0).is_none());
    }

    fn group_plus_is_a_workspace_scoped_app_picker_targeting_that_group() {
        use crate::shell::chrome_state::{OpenTarget, SearchScope};
        use crate::shell::search::ResourceKind;

        let (tree, _strip_id, group_id) = one_group_seed();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        host.open_new_tab_palette_for_group(group_id);

        let st = chrome.borrow();
        assert_eq!(st.open_target, OpenTarget::Here(group_id));
        assert_eq!(
            st.search_scope,
            SearchScope::Workspace,
            "a local group affordance must not open machine-wide search"
        );
        assert_eq!(
            host.active_kind_filter,
            vec![ResourceKind::Application, ResourceKind::HostApplication],
            "the group + is an app picker, not generic search"
        );
        assert!(host.palette_visible(), "the picker opens immediately");
    }

    closing_one_of_two_same_app_windows_targets_only_its_surface();
    a_deferred_close_click_keeps_the_window_and_animates_once_at_exit();
    group_plus_is_a_workspace_scoped_app_picker_targeting_that_group();
}
