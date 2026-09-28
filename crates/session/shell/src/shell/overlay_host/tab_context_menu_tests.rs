//! The tab right-click menu on the production host: opened by the tab
//! bar's request through `handle_event`, moved by the arrow keys,
//! closed by Escape, acted on by a click, and reached through the
//! semantic route. Every assertion reads the host's own state or the
//! accessibility tree it publishes — what a reader observes.
use super::overlay_event_absorb_tests::lay_out_host_with_theme;
use super::*;
use crate::shell::build_tilo_overlay_host_with_chrome;
use crate::shell::state::FreshId as _;
use crate::shell::state::{
    AppKind, GroupId, SharedSurfaces, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
    WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree,
};
use std::collections::HashMap as StdHashMap;
use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, Role};
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

const TAB_A: u64 = 0xa1;
const TAB_B: u64 = 0xa2;
const GROUP: GroupId = 7001;

/// One strip, one group, `tabs` stub tabs ("Notes", "Files").
fn tree_with_tabs(tabs: usize) -> (WorkspaceTree, StripId) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let mut group_tabs = vec![Tab {
        id: TAB_A,
        title: "Notes".into(),
        tile: TileContent::Stub {
            kind: AppKind::Other("Test".into()),
            label: "notes".into(),
        },
    }];
    if tabs > 1 {
        group_tabs.push(Tab {
            id: TAB_B,
            title: "Files".into(),
            tile: TileContent::Stub {
                kind: AppKind::Other("Test".into()),
                label: "files".into(),
            },
        });
    }
    let strip = Strip {
        id: strip_id,
        name: "Main".into(),
        window_groups: vec![StateGroup {
            id: GROUP,
            tabs: group_tabs,
            active_tab: TAB_A,
            width: StateWidth::Normal,
            maximized: false,
        }],
    };
    let ws = Workspace {
        owner: None,
        pending_ownership_transfer: None,
        ownership_transfer_audit: Vec::new(),
        ownership_transfer_cancellations: Vec::new(),
        permission_profiles: Vec::new(),
        capabilities: Vec::new(),
        default_apps: desktop_policy::WorkspaceDefaults::default(),
        tasks: Vec::new(),
        effect_receipts: Vec::new(),
        shortcuts: Vec::new(),
        schedule_claims: Vec::new(),
        id: ws_id,
        room: crate::shell::state::RoomId::new(),
        name: "Home".into(),
        strips: vec![strip],
        collaborators: vec![],
        shared_surfaces: SharedSurfaces::default(),
    };
    let mut active_strip = StdHashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = StdHashMap::new();
    active_group.insert(strip_id, GROUP);
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
    (tree, strip_id)
}

struct Harness {
    host: ShellOverlayHost,
    chrome: Chrome,
    strip: StripId,
    group: GroupId,
    /// The fixture group's tab ids in bar order.
    tabs: Vec<u64>,
    /// The fixture group's tab titles in bar order.
    titles: Vec<String>,
    taffy: TaffyTree<()>,
    root: NodeId,
    theme: Theme,
    doc: AutoCommit,
}

const VW: f64 = 1280.0;
const VH: f64 = 800.0;

/// The production host over a real tree. Two or more tabs uses the
/// demo workspace (Notes, Files, Terminal, Document in one group),
/// whose Automerge document mirrors its structure — a close is a
/// document deletion first (`tab_binding::remove_tab`), so a hand-built
/// tree with no mirror refuses it. The single-tab case, which never
/// closes, uses the hand-built tree. One structure pass settles the
/// host's chrome revision the way the first frame's `update` does.
fn harness(tabs: usize) -> Harness {
    harness_built_with(tabs, build_tilo_overlay_host_with_chrome)
}

/// [`harness`] over explicit hosted inputs, for tests that need the
/// production Files services (document-open capability) wired in.
fn harness_with_inputs(
    tabs: usize,
    inputs: crate::shell::hosted_inputs::HostedTileInputs,
) -> Harness {
    harness_built_with(tabs, |tree| {
        crate::shell::build_tilo_overlay_host_with_inputs(
            tree.clone().shared(),
            false,
            ui::record::RecordStore::in_memory(),
            inputs,
        )
    })
}

fn harness_built_with(
    tabs: usize,
    build: impl FnOnce(&WorkspaceTree) -> (ShellOverlayHost, Chrome),
) -> Harness {
    let tree = if tabs >= 2 {
        WorkspaceTree::bootstrap_demo()
    } else {
        tree_with_tabs(1).0
    };
    let (strip, group, tab_ids, titles) = {
        let strip = &tree.desktop.workspaces[0].strips[0];
        let group = &strip.window_groups[0];
        (
            strip.id,
            group.id,
            group.tabs.iter().map(|t| t.id).collect::<Vec<_>>(),
            group
                .tabs
                .iter()
                .map(|t| t.title.clone())
                .collect::<Vec<_>>(),
        )
    };
    assert!(
        tab_ids.len() >= tabs,
        "fixture has the tabs the test asked for"
    );
    let (mut host, chrome) = build(&tree);
    let theme = Theme::dark();
    let (taffy, root) = lay_out_host_with_theme(&mut host, VW, VH, &theme);
    let _ = host.take_structure_dirty();
    Harness {
        host,
        chrome,
        strip,
        group,
        tabs: tab_ids,
        titles,
        taffy,
        root,
        theme,
        doc: AutoCommit::new(),
    }
}

impl Harness {
    fn send(&mut self, event: &WindowEvent) -> bool {
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        self.host.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: &mut self.doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: self.root,
                taffy: &self.taffy,
            },
        );
        handled
    }

    fn move_to(&mut self, p: Point) {
        let _ = self.send(&WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(p.x, p.y),
        });
    }

    fn press(&mut self, button: MouseButton) -> bool {
        let pressed = self.send(&WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button,
        });
        let _ = self.send(&WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button,
        });
        pressed
    }

    fn key(&mut self, key: Key) -> bool {
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = key;
        event.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = None;
        self.send(&WindowEvent::KeyboardInput {
            device_id: DeviceId::dummy(),
            event,
            is_synthetic: false,
        })
    }

    /// Centre of tab pill `index` in the fixture's only group, as the
    /// production tab bar laid it out.
    fn pill_centre(&mut self, index: usize) -> Point {
        let group = self
            .host
            .group_at_for_tests(self.strip, 0)
            .expect("the fixture's group is live");
        let rect = group.tab_rects()[index];
        assert!(rect.area() > 0.0, "pill {index} was laid out: {rect:?}");
        rect.center()
    }

    fn right_click_pill(&mut self, index: usize) {
        let p = self.pill_centre(index);
        self.move_to(p);
        let handled = self.press(MouseButton::Right);
        assert!(handled, "the tab bar claims the right-click");
    }

    fn menu_node(&self) -> Option<AccessNode> {
        self.host
            .accessibility()
            .expect("the host publishes a tree")
            .children
            .into_iter()
            .find(|n| n.role == Role::Menu)
    }

    fn group_ids(&self) -> Vec<u64> {
        let st = self.chrome.borrow();
        let tree = st.tree();
        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|g| g.id)
            .collect()
    }

    fn tab_ids(&self, group: usize) -> Vec<u64> {
        let st = self.chrome.borrow();
        let tree = st.tree();
        tree.desktop.workspaces[0].strips[0].window_groups[group]
            .tabs
            .iter()
            .map(|t| t.id)
            .collect()
    }
}

fn files_document_open_capability(
    chrome: &crate::shell::chrome_state::Chrome,
) -> ui::widget::DocumentOpenCapability {
    let state = chrome.borrow();
    let services = state
        .hosted_inputs
        .file_services()
        .expect("the production harness provides hosted Files services");
    let file_explorer::files_app::DocumentOpenRoute::Deferred(capability) =
        &services.document_opener
    else {
        panic!("the production harness routes Files opens through the deferred owner");
    };
    capability.clone()
}

fn tab_count(chrome: &crate::shell::chrome_state::Chrome) -> usize {
    chrome
        .borrow()
        .tree()
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .map(|group| group.tabs.len())
        .sum()
}

#[test]
fn stale_files_document_open_is_cancelled_before_record_or_tab_publication() {
    let directory = tempfile::tempdir().unwrap();
    let h = harness_with_inputs(
        4,
        crate::shell::hosted_inputs::HostedTileInputs::for_host_directory(directory.path())
            .expect("owned Files directory"),
    );
    let chrome = h.chrome.clone();
    let source = h.tabs[1]; // Files in the production demo workspace.
    let replacement = h.tabs[0];
    let capability = files_document_open_capability(&chrome);
    let tabs_before = tab_count(&chrome);
    let path = directory.path().join("queued.txt");
    std::fs::write(&path, "must not be read after source replacement\n").unwrap();
    let path = path.canonicalize().unwrap();
    let record = ui::record::RecordId::for_file(&path);
    let reply = std::rc::Rc::new(std::cell::RefCell::new(
        ui::widget::DocumentOpenReply::pending(41),
    ));
    let queued = ui::widget::AppHostIntent::OpenDocument {
        path,
        capability,
        token: 41,
        reply: std::rc::Rc::downgrade(&reply),
    };
    let workspace = chrome.borrow().tree().desktop.active;
    chrome
        .borrow_mut()
        .focus_tab(workspace, h.strip, h.group, replacement);

    let ui::widget::AppHostIntent::OpenDocument {
        path,
        capability,
        token,
        ..
    } = queued
    else {
        unreachable!("the queued intent has the Files shape");
    };
    ShellOverlayHost::complete_document_open_request(
        &chrome,
        source,
        &path,
        &capability,
        token,
        &mut reply.borrow_mut(),
    );

    assert_eq!(
        reply.borrow_mut().take_result(token),
        Some(ui::widget::DocumentOpenOutcome::Cancelled)
    );
    assert!(!chrome.borrow().records().is_ready(&record));
    assert_eq!(tab_count(&chrome), tabs_before);
    assert_eq!(
        crate::shell::window_actions::active_tab_id(&chrome.borrow()),
        Some(replacement),
        "a stale Files request must not replace the current tab"
    );
}

#[test]
fn non_files_source_cannot_use_the_hosted_files_capability() {
    let directory = tempfile::tempdir().unwrap();
    let h = harness_with_inputs(
        4,
        crate::shell::hosted_inputs::HostedTileInputs::for_host_directory(directory.path())
            .expect("owned Files directory"),
    );
    let chrome = h.chrome.clone();
    let source = h.tabs[0]; // Notes, not the Files tile at index 1.
    let capability = files_document_open_capability(&chrome);
    let tabs_before = tab_count(&chrome);
    let path = directory.path().join("forged.txt");
    std::fs::write(&path, "a non-Files app must not open this\n").unwrap();
    let path = path.canonicalize().unwrap();
    let record = ui::record::RecordId::for_file(&path);
    let reply = std::rc::Rc::new(std::cell::RefCell::new(
        ui::widget::DocumentOpenReply::pending(42),
    ));
    let workspace = chrome.borrow().tree().desktop.active;
    chrome
        .borrow_mut()
        .focus_tab(workspace, h.strip, h.group, source);

    ShellOverlayHost::complete_document_open_request(
        &chrome,
        source,
        &path,
        &capability,
        42,
        &mut reply.borrow_mut(),
    );

    assert_eq!(
        reply.borrow_mut().take_result(42),
        Some(ui::widget::DocumentOpenOutcome::Cancelled)
    );
    assert!(!chrome.borrow().records().is_ready(&record));
    assert_eq!(tab_count(&chrome), tabs_before);
    assert_eq!(
        crate::shell::window_actions::active_tab_id(&chrome.borrow()),
        Some(source),
        "a non-Files intent must not publish a replacement tab"
    );
}

#[test]
fn wrong_document_open_capability_cannot_use_the_current_files_source() {
    let directory = tempfile::tempdir().unwrap();
    let h = harness_with_inputs(
        4,
        crate::shell::hosted_inputs::HostedTileInputs::for_host_directory(directory.path())
            .expect("owned Files directory"),
    );
    let chrome = h.chrome.clone();
    let source = h.tabs[1]; // Files in the production demo workspace.
    let tabs_before = tab_count(&chrome);
    let path = directory.path().join("forged-capability.txt");
    std::fs::write(&path, "a mismatched capability must not read this\n").unwrap();
    let path = path.canonicalize().unwrap();
    let record = ui::record::RecordId::for_file(&path);
    let reply = std::rc::Rc::new(std::cell::RefCell::new(
        ui::widget::DocumentOpenReply::pending(43),
    ));
    let workspace = chrome.borrow().tree().desktop.active;
    chrome
        .borrow_mut()
        .focus_tab(workspace, h.strip, h.group, source);

    ShellOverlayHost::complete_document_open_request(
        &chrome,
        source,
        &path,
        &ui::widget::DocumentOpenCapability::issue(),
        43,
        &mut reply.borrow_mut(),
    );

    assert_eq!(
        reply.borrow_mut().take_result(43),
        Some(ui::widget::DocumentOpenOutcome::Cancelled)
    );
    assert!(!chrome.borrow().records().is_ready(&record));
    assert_eq!(tab_count(&chrome), tabs_before);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn tab_context_menu_opens_navigates_dismisses_and_paints_only_while_open() {
    /// The whole pointer story, on the production `handle_event` path: a
    /// right-click on a pill opens the menu at the pointer with every
    /// action bounded and named; Down arms the first row, Down again the
    /// second; Escape closes it and the tree forgets it.
    fn right_click_on_a_tab_opens_the_menu_arrows_move_the_highlight_and_escape_closes() {
        let mut h = harness(2);
        assert!(!h.host.tab_context_menu_is_open());
        assert!(h.menu_node().is_none(), "no menu is published while closed");

        let p = h.pill_centre(1);
        h.right_click_pill(1);
        assert!(
            h.host.tab_context_menu_is_open(),
            "the right-click opened the menu"
        );
        let rect = h
            .host
            .tab_context_menu_rect()
            .expect("an open menu has a rect");
        assert!(
            (rect.x0 - p.x).abs() < 0.5 && (rect.y0 - p.y).abs() < 0.5,
            "the menu hangs off the pointer ({p:?}), not a fixed spot: {rect:?}"
        );
        assert!(
            rect.x1 <= VW && rect.y1 <= VH,
            "the menu stays inside the host: {rect:?}"
        );

        let menu = h
            .menu_node()
            .expect("the open menu is the first overlay in the tree");
        let expected_name = format!("{} tab menu", h.titles[1]);
        assert_eq!(menu.name.as_deref(), Some(expected_name.as_str()));
        assert_eq!(menu.state.expanded, Some(true));
        let labels: Vec<_> = menu
            .children
            .iter()
            .filter_map(|c| c.name.as_deref())
            .collect();
        assert_eq!(labels, ["Close Tab", "Split Right", "Move to New Group"]);
        for (i, item) in menu.children.iter().enumerate() {
            assert_eq!(item.role, Role::MenuItem);
            assert!(item.state.focusable);
            assert!(
                !item.state.focused,
                "row {i} is not armed before any hover or key"
            );
            let bounds = item
                .bounds
                .map(ui::AccessBounds::to_rect)
                .expect("row is bounded");
            assert!(
                rect.contains(bounds.center()),
                "row {i} lies inside the menu"
            );
            assert_eq!(
                h.host.synthetic_accessibility_rect(&[0, i]),
                Some(bounds),
                "the routed geometry for row {i} is its published rect"
            );
            assert!(
                item.action_target
                    .as_deref()
                    .is_some_and(|t| t.starts_with("tabmenu:")),
                "row {i} carries a semantic address: {:?}",
                item.action_target
            );
        }

        assert!(
            h.key(Key::Named(NamedKey::ArrowDown)),
            "the menu owns the keyboard"
        );
        let focused: Vec<bool> = h
            .menu_node()
            .expect("still open")
            .children
            .iter()
            .map(|c| c.state.focused)
            .collect();
        assert_eq!(focused, [true, false, false], "Down arms the first row");

        h.key(Key::Named(NamedKey::ArrowDown));
        let focused: Vec<bool> = h
            .menu_node()
            .expect("still open")
            .children
            .iter()
            .map(|c| c.state.focused)
            .collect();
        assert_eq!(
            focused,
            [false, true, false],
            "Down again moves to the second row"
        );

        assert!(h.key(Key::Named(NamedKey::Escape)));
        assert!(!h.host.tab_context_menu_is_open(), "Escape closes the menu");
        assert!(h.menu_node().is_none(), "a closed menu leaves the tree");
        assert_eq!(h.tab_ids(0), h.tabs, "nothing was acted on");
    }

    /// A left press outside the menu dismisses it and is absorbed — the
    /// window beneath must not also be activated by the dismissing click.
    fn a_click_outside_the_menu_dismisses_it_and_goes_no_further() {
        let mut h = harness(2);
        h.right_click_pill(0);
        assert!(h.host.tab_context_menu_is_open());
        h.move_to(Point::new(VW - 40.0, VH - 40.0));
        let handled = h.press(MouseButton::Left);
        assert!(handled, "the dismissing click is absorbed");
        assert!(!h.host.tab_context_menu_is_open());
        assert_eq!(h.tab_ids(0), h.tabs);
    }

    /// A single-tab group is offered Close only: splitting its one tab
    /// would dissolve and re-create the group, a no-op that reads as a
    /// glitch. The split targets refuse through every route.
    fn a_single_tab_group_is_offered_close_only() {
        let mut h = harness(1);
        h.right_click_pill(0);
        let labels: Vec<_> = h
            .menu_node()
            .expect("open")
            .children
            .iter()
            .filter_map(|c| c.name.clone())
            .collect();
        assert_eq!(labels, ["Close Tab"]);
        assert!(!h.host.perform_semantic_action(
            "tabmenu:split-right",
            &ui::a11y::Action::Activate,
            &mut h.doc,
            &automerge::ROOT,
        ));
        assert!(
            h.host.tab_context_menu_is_open(),
            "a refused target leaves the menu up"
        );
    }

    /// The menu paints through the shared widget with the live theme and
    /// an `e4` shadow following its outline, and never paints when closed.
    fn the_open_menu_paints_and_the_closed_one_does_not() {
        let mut h = harness(2);
        let mut tr = TextRenderer::new();
        let presence = ui::presence::PresenceCtx::default();
        let mut closed = Scene::new();
        let outputs = h.host.layer_outputs();
        h.host.layers.draw(
            LayerId::TabContextMenu,
            &mut LayerPaint {
                scene: &mut closed,
                transform: Affine::IDENTITY,
                text_renderer: &mut tr,
                theme: &h.theme,
                presence: &presence,
                geometry: h.host.layer_geometry(&outputs),
                thumbnails: &mut crate::shell::window_previews::WindowPreviews::default(),
            },
        );
        assert!(
            closed.encoding().is_empty(),
            "a closed menu contributes nothing to the scene"
        );
        h.right_click_pill(1);
        let mut open = Scene::new();
        let outputs = h.host.layer_outputs();
        h.host.layers.draw(
            LayerId::TabContextMenu,
            &mut LayerPaint {
                scene: &mut open,
                transform: Affine::IDENTITY,
                text_renderer: &mut tr,
                theme: &h.theme,
                presence: &presence,
                geometry: h.host.layer_geometry(&outputs),
                thumbnails: &mut crate::shell::window_previews::WindowPreviews::default(),
            },
        );
        assert!(
            !open.encoding().is_empty(),
            "an open menu paints its surface, rows and shadow"
        );
    }

    right_click_on_a_tab_opens_the_menu_arrows_move_the_highlight_and_escape_closes();
    a_click_outside_the_menu_dismisses_it_and_goes_no_further();
    a_single_tab_group_is_offered_close_only();
    the_open_menu_paints_and_the_closed_one_does_not();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn tab_context_menu_actions_edit_the_tree_the_same_way_from_every_route() {
    /// Clicking "Close Tab" runs the same durable lifecycle as the `✕` and
    /// the chord: the tab is gone from the persisted tree before the event
    /// returns.
    fn clicking_close_tab_removes_that_tab_from_the_tree() {
        let mut h = harness(2);
        h.right_click_pill(1);
        let close = h
            .menu_node()
            .expect("open")
            .children
            .into_iter()
            .find(|c| c.name.as_deref() == Some("Close Tab"))
            .and_then(|c| c.bounds.map(ui::AccessBounds::to_rect))
            .expect("Close Tab is a bounded row");
        h.move_to(close.center());
        assert!(h.press(MouseButton::Left));
        assert!(
            !h.host.tab_context_menu_is_open(),
            "a selection closes the menu"
        );
        let closed = h.tabs[1];
        let remaining: Vec<u64> = h.tabs.iter().copied().filter(|t| *t != closed).collect();
        assert_eq!(
            h.tab_ids(0),
            remaining,
            "the right-clicked tab was closed, the rest remain"
        );
    }

    /// "Split Right" through Enter is tree-first (`window_actions::split_tab`:
    /// cache, replicated document, chrome bump) with the live tab host moved
    /// under the tree's new group id in the same dispatch — so the tree is
    /// right before any frame, and the resync the bump triggers finds the
    /// group present and keeps the window state.
    fn split_right_by_keyboard_moves_the_tab_into_a_new_group_beside_it() {
        let mut h = harness(2);
        h.right_click_pill(1);
        h.key(Key::Named(NamedKey::ArrowDown)); // Close Tab
        h.key(Key::Named(NamedKey::ArrowDown)); // Split Right
        assert!(h.key(Key::Named(NamedKey::Enter)));
        assert!(!h.host.tab_context_menu_is_open());
        // Tree-first: the tree is already split before any structure pass.
        assert_eq!(
            h.group_ids().len(),
            2,
            "the tree split in the same dispatch"
        );
        let _ = h.host.take_structure_dirty();
        let groups = h.group_ids();
        assert_eq!(groups.len(), 2, "a second group appeared: {groups:?}");
        assert_eq!(groups[0], h.group, "the source group keeps its slot");
        let moved = h.tabs[1];
        let remaining: Vec<u64> = h.tabs.iter().copied().filter(|t| *t != moved).collect();
        assert_eq!(
            h.tab_ids(0),
            remaining,
            "the tree's source group lost the moved tab"
        );
        assert_eq!(
            h.tab_ids(1),
            [moved],
            "the split tab is the new group's only tab"
        );

        // The next chrome revision runs the tree→row resync. Before
        // 2026-09-17 the mirror only inserted the new group, so the tree
        // still listed the moved tab in its source and this resync
        // re-created it there as a second, hostless card — for the menu and
        // for the drag-out split alike. Both live groups must stay exact.
        h.chrome.borrow_mut().bump();
        let _ = h.host.take_structure_dirty();
        let live_source: Vec<u64> = h
            .host
            .group_at_for_tests(h.strip, 0)
            .expect("source group is live")
            .tabs()
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(
            live_source, remaining,
            "no duplicate of the moved tab in the source"
        );
        let live_new: Vec<u64> = h
            .host
            .group_at_for_tests(h.strip, 1)
            .expect("new group is live")
            .tabs()
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(live_new, [moved]);
        assert_eq!(
            h.group_ids().len(),
            2,
            "the resync neither revived nor evicted a group"
        );
    }

    /// The registry door: `tab.split-right` runs `window_actions::split_active_tab`
    /// on the focused tab with no menu and no live host in hand; the tree
    /// splits at once and the resync builds the widget side — one group
    /// per tree group, the moved tab in the new one, nothing duplicated.
    fn the_registry_split_action_lands_in_the_tree_and_the_resync_builds_the_group() {
        let mut h = harness(2);
        let active = crate::shell::window_actions::active_tab_id(&h.chrome.borrow())
            .expect("the demo workspace has a focused tab");
        let new_group = {
            let mut st = h.chrome.borrow_mut();
            crate::shell::window_actions::split_active_tab(
                &mut st,
                crate::shell::window_actions::SplitPlacement::Right,
            )
            .expect("a multi-tab group splits")
        };
        assert_eq!(
            h.group_ids(),
            [h.group, new_group],
            "tree-first, beside the source"
        );
        assert_eq!(h.tab_ids(1), [active]);
        assert!(
            !h.tab_ids(0).contains(&active),
            "the source lost the moved tab"
        );
        // The bump `split_active_tab` made drives the resync.
        let _ = h.host.take_structure_dirty();
        let live: Vec<Vec<u64>> = (0..2)
            .map(|i| {
                h.host
                    .group_at_for_tests(h.strip, i)
                    .expect("both groups are live")
                    .tabs()
                    .iter()
                    .map(|t| t.id)
                    .collect()
            })
            .collect();
        assert_eq!(
            live[1],
            [active],
            "the resync built the new group around the tab"
        );
        assert!(!live[0].contains(&active), "and took it out of the source");
        // The refusal path: a single-tab group is not split, and says why.
        let single = harness(1);
        let err = {
            let mut st = single.chrome.borrow_mut();
            crate::shell::window_actions::split_active_tab(
                &mut st,
                crate::shell::window_actions::SplitPlacement::Right,
            )
            .expect_err("a group's only tab is not split")
        };
        assert_eq!(err, crate::shell::window_actions::TabActionError::OnlyTab);
        assert_eq!(single.group_ids().len(), 1, "nothing changed");
    }

    /// The semantic door: an assistive technology or a script activates
    /// `tabmenu:new-group` on the open menu and gets the same operation.
    fn the_semantic_route_performs_the_same_action_as_the_pointer() {
        let mut h = harness(2);
        h.right_click_pill(1);
        let performed = h.host.perform_semantic_action(
            "tabmenu:new-group",
            &ui::a11y::Action::Activate,
            &mut h.doc,
            &automerge::ROOT,
        );
        assert!(performed, "an offered row activates");
        assert!(!h.host.tab_context_menu_is_open());
        let _ = h.host.take_structure_dirty();
        assert_eq!(h.group_ids().len(), 2);
        assert_eq!(h.tab_ids(1), [h.tabs[1]]);

        // A target that is not on the menu — or a menu that is not open —
        // refuses rather than acting on a stale tab.
        assert!(!h.host.perform_semantic_action(
            "tabmenu:close",
            &ui::a11y::Action::Activate,
            &mut h.doc,
            &automerge::ROOT,
        ));
    }

    clicking_close_tab_removes_that_tab_from_the_tree();
    split_right_by_keyboard_moves_the_tab_into_a_new_group_beside_it();
    the_registry_split_action_lands_in_the_tree_and_the_resync_builds_the_group();
    the_semantic_route_performs_the_same_action_as_the_pointer();
}
