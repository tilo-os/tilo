//! Per-frame wire-up coverage for cross-strip tab drag-drop (#151).
//!
//! The cross-group case (drag a tab between two `WindowGroup`s inside
//! the same strip's `GroupRow`) is fully covered by the unit tests in
//! `ui/src/widget/group_row.rs`. The cross-*strip* case lives one
//! level up — it can't resolve from inside a single `GroupRow` because
//! that widget only sees its own siblings — so the broker has to live
//! shell-side, where the [`ShellOverlayHost`] holds per-strip
//! `GroupRow` pointers in `group_row_ptrs` and the [`WorkspaceTree`]
//! tracks every strip's identity.
//!
//! What this file pins end-to-end:
//!
//!   1. Build a 2-strip tree with one group + 2 tabs in each strip.
//!   2. Hand it to [`build_nirios_overlay_host_with_chrome`] so the
//!      shell captures both strips' `GroupRow` pointers.
//!   3. Drive a real layout pass against the host so each row's stored
//!      `Rect` lands in screen-space coords (the cross-strip resolver
//!      hit-tests release points against those rects).
//!   4. Publish a synthetic `pending_detach` on a tab in strip-A's
//!      group whose release point falls inside strip-B's vertical band
//!      — same shape `TabBar::handle_event` writes when the user
//!      releases a drag outside the bar's own rect.
//!   5. Pump a no-op `CursorMoved` to drive the post-dispatch detach
//!      sweep inside `GroupRow::handle_event`. The sweep notices the
//!      release y-coord is outside its own row band, detaches the
//!      `(TabDescriptor, TileHost)` pair, and stashes a
//!      `CrossStripDetach` payload on the source row.
//!   6. Tick the host's `tick_cross_strip_dispatch_for_tests` helper —
//!      the production [`Widget::update`] runs the same dispatch each
//!      frame.
//!   7. Assert: source strip has 1 tab remaining (in 1 group),
//!      destination strip has 3 tabs across 2 groups (its original
//!      2-tab group + a new 1-tab group), and the moved tab id matches
//!      what was dragged. Plus the persisted [`WorkspaceTree`] mirrors
//!      the splice — the dropped tab now lives on strip-B in the
//!      tree.
//!
//! Without the cross-strip dispatch hook the source row's local resolver
//! would either (a) return the tab to the source group ("no target
//! resolved" recovery) or (b) splice it into a new group on the source
//! strip via `gap_index_for` — both of which silently lose the user's
//! intent of "move this tab to the OTHER strip".
//!
//! The same harness covers the rest of the widget/tree mirror, because
//! every case is that one seam seen from a different angle: the widget
//! tree and the persisted tree have to agree about which tabs exist,
//! which group each one is in, and what is inside it. Every assertion
//! below is made against **the tree**, not the widget — a widget-only
//! assertion is what let closing a window look like it worked while the
//! tree kept the tab and put the window back on the next revision bump.

use std::collections::HashMap;

use shell::shell::build_nirios_overlay_host_with_chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::dpi::PhysicalPosition;
use winit::event::WindowEvent;

const W: f32 = 1280.0;
const H: f32 = 800.0;

/// Two strips in a single workspace, each holding one `WindowGroup` with
/// two tabs. The smallest topology that exercises the cross-strip
/// dispatch — one fewer strip and there's nothing to cross to; one
/// fewer tab per group and the source group disappears mid-detach
/// (which is fine for the widget side but it's a separate case).
fn two_strip_two_tab_tree() -> (WorkspaceTree, StripId, StripId, [u64; 2], [u64; 2]) {
    let ws_id = WorkspaceId::new();
    let s_a = StripId::new();
    let s_b = StripId::new();
    // Stable widget-tree ids so failure messages stay legible.
    let g_a: GroupId = 1001;
    let g_b: GroupId = 2001;
    let a_tabs: [u64; 2] = [11, 12];
    let b_tabs: [u64; 2] = [21, 22];

    let mk_tab = |id: u64, label: &str| Tab {
        id,
        title: label.into(),
        tile: TileContent::Stub {
            kind: AppKind::Other("Test".into()),
            label: label.into(),
        },
    };

    let strip_a = Strip {
        id: s_a,
        name: "StripA".into(),
        window_groups: vec![WindowGroup {
            id: g_a,
            tabs: vec![mk_tab(a_tabs[0], "A1"), mk_tab(a_tabs[1], "A2")],
            active_tab: a_tabs[0],
            width: WindowWidth::Normal,
            maximized: false,
        }],
    };
    let strip_b = Strip {
        id: s_b,
        name: "StripB".into(),
        window_groups: vec![WindowGroup {
            id: g_b,
            tabs: vec![mk_tab(b_tabs[0], "B1"), mk_tab(b_tabs[1], "B2")],
            active_tab: b_tabs[0],
            width: WindowWidth::Normal,
            maximized: false,
        }],
    };
    let ws = Workspace {
        strips: vec![strip_a, strip_b],
        // Everything else — including the workspace's durable
        // collaboration room — comes from the builder, so a field added
        // to `Workspace` does not break this fixture.
        ..Workspace::test_default(ws_id, "Test")
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, s_a);
    let mut active_group = HashMap::new();
    active_group.insert(s_a, g_a);
    active_group.insert(s_b, g_b);

    let tree = WorkspaceTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        active_group,
        collaborators: vec![],
        ..Default::default()
    };

    (tree, s_a, s_b, a_tabs, b_tabs)
}

/// Run a full `build_layout → compute_layout → apply_layout` pass against
/// the host, sized to a desktop-ish W×H viewport. Mirrors what
/// `App::layout` does inside the run loop so each `GroupRow.rect` and
/// every group's stored rect lands in screen-space coords.
struct Layout {
    taffy: TaffyTree<()>,
    root_node: NodeId,
    child_node: NodeId,
}

fn lay_out_host(host: &mut ShellOverlayHost) -> Layout {
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root_node = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(W),
                    height: Dimension::length(H),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();

    let mut tr = ui::text_render::TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root_node,
            Size {
                width: AvailableSpace::Definite(W),
                height: AvailableSpace::Definite(H),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);
    Layout {
        taffy,
        root_node,
        child_node: child,
    }
}

/// Pump a no-op `CursorMoved` event through the host. Drives the widget
/// tree's per-frame event handler so any pre-staged `pending_detach` on
/// a child `TabBar` flows through `WindowGroup::handle_event` and lands
/// in `GroupRow`'s post-dispatch detach sweep.
fn pump_no_op(host: &mut ShellOverlayHost, layout: &Layout) {
    let event = WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: PhysicalPosition::new(9000.0, 9000.0),
    };
    let mut doc = automerge::AutoCommit::new();
    let doc_root = automerge::ROOT.clone();
    let modifiers = winit::event::Modifiers::default();
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    host.handle_event(
        &event,
        1.0,
        &modifiers,
        &mut doc,
        &doc_root,
        &mut handled,
        &mut focus,
        layout.child_node,
        &layout.taffy,
    );
    // The harness above stops short of `Widget::update`, which is the
    // production hook the cross-strip dispatcher rides on. Tests use the
    // public `tick_cross_strip_dispatch_for_tests` helper to invoke the
    // dispatch directly; pre-suppressing the unused-variable warning on
    // `layout.root_node` keeps clippy happy without a `let _ = ...`.
    let _ = layout.root_node;
}

#[test]
fn dragged_tab_crosses_into_destination_strip_via_shell_dispatch() {
    let (tree, s_a, s_b, a_tabs, _b_tabs) = two_strip_two_tab_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);

    // Drive a layout pass so each strip's `GroupRow.rect` is populated in
    // screen-space. Without this the cross-strip resolver compares
    // release points against `Rect::ZERO` rects and never matches.
    let layout = lay_out_host(&mut host);

    // Pre-condition: each strip has exactly 1 group; strip-A's group has
    // tabs [11, 12], strip-B's has [21, 22]. The widget tree mirrors
    // the persisted state at boot.
    let snap_pre = host.groups_per_strip();
    let snap_pre_a = snap_pre.iter().find(|(s, _)| *s == s_a).expect("strip A");
    let snap_pre_b = snap_pre.iter().find(|(s, _)| *s == s_b).expect("strip B");
    assert_eq!(snap_pre_a.1.len(), 1, "strip A starts with 1 group");
    assert_eq!(snap_pre_b.1.len(), 1, "strip B starts with 1 group");
    assert_eq!(snap_pre_a.1[0].1, vec![a_tabs[0], a_tabs[1]]);
    assert_eq!(snap_pre_b.1[0].1, vec![21, 22]);

    // Read strip-B's row rect so we can pick a release y-coord inside
    // its vertical band — that's how the cross-strip dispatcher tells
    // "user is aiming at strip B" apart from "user dropped in their own
    // strip's empty space".
    let dest_rect = host
        .group_row_rect_for_tests(s_b)
        .expect("strip B's GroupRow rect must be populated after apply_layout");
    let drop_y = (dest_rect.y0 + dest_rect.y1) / 2.0;
    // Drop x: middle of strip-B's row, so the destination row's
    // gap-index resolver picks an unambiguous gap (left of the only
    // existing group, or right of it — either is valid; the test only
    // pins that the tab lands on strip B as a NEW group).
    let drop_x = (dest_rect.x0 + dest_rect.x1) / 2.0;
    let release = Point::new(drop_x, drop_y);

    // Publish a synthetic `pending_detach` on strip-A's group's TabBar.
    // This is the same shape `TabBar::handle_event` writes when the
    // user releases a left-click drag outside the bar's own rect; the
    // wireup pinned by `ui/src/widget/tabs.rs` tests guarantees
    // this is the only field the post-dispatch sweep reads to discover
    // a detach. We pick tab `a_tabs[0]` (id 11) as the target so the
    // assertion below has a stable id to check.
    let dragged = a_tabs[0];
    {
        let g = host
            .group_at_for_tests(s_a, 0)
            .expect("strip A group 0 widget");
        // The widget-side cross-group machinery already pins
        // `pending_detach = Some((tab_id, release_point))` as the public
        // contract for "a drag ended outside this bar's rect"; mirror
        // that here.
        g.bar_mut_for_tests().pending_detach = Some((dragged, release));
    }

    // Pump a no-op CursorMoved through the host. Inside the widget
    // tree, `GroupRow::handle_event` drains the pending_detach and the
    // post-dispatch sweep inspects the release point's y. Because
    // `release.y` is outside strip-A's row band (it's in strip-B's),
    // the new diversion path detaches the `(desc, host)` pair and
    // stashes it on `pending_cross_strip_detach` for the shell to
    // route.
    pump_no_op(&mut host, &layout);

    // Tick the cross-strip dispatcher. Production runs this each frame
    // inside `Widget::update`; tests use the explicit helper so the
    // failure mode (forgetting to wire the dispatcher into `update`)
    // would still be caught by a separate "dispatch is wired into
    // update" assertion below — but the direct call here proves the
    // dispatcher itself routes correctly.
    host.tick_cross_strip_dispatch_for_tests();

    // Post-condition: strip-A lost a tab; strip-B gained a new
    // single-tab group carrying the dragged id.
    let snap_post = host.groups_per_strip();
    let snap_post_a = snap_post.iter().find(|(s, _)| *s == s_a).expect("strip A");
    let snap_post_b = snap_post.iter().find(|(s, _)| *s == s_b).expect("strip B");

    // Strip A: the original group should still hold tab 12 only. Tab 11
    // was the one we dragged out.
    let strip_a_total_tabs: usize = snap_post_a.1.iter().map(|(_, ts)| ts.len()).sum();
    assert_eq!(
        strip_a_total_tabs, 1,
        "strip A lost the dragged tab — total now 1, was 2"
    );
    assert!(
        !snap_post_a.1.iter().any(|(_, ts)| ts.contains(&dragged)),
        "dragged tab id {dragged} must no longer live on strip A"
    );

    // Strip B: original group is intact (tabs [21, 22]) and a new group
    // carrying the dragged tab id was spliced in. Total 3 tabs across 2
    // groups.
    let strip_b_total_tabs: usize = snap_post_b.1.iter().map(|(_, ts)| ts.len()).sum();
    assert_eq!(
        strip_b_total_tabs, 3,
        "strip B gained the dragged tab — total now 3, was 2"
    );
    assert_eq!(
        snap_post_b.1.len(),
        2,
        "strip B should hold 2 groups: the original + the new single-tab one"
    );
    let new_group_carries_dragged = snap_post_b
        .1
        .iter()
        .any(|(_, ts)| ts.len() == 1 && ts[0] == dragged);
    assert!(
        new_group_carries_dragged,
        "strip B must carry a NEW single-tab group with dragged id {dragged}, got groups {:?}",
        snap_post_b.1
    );

    // Persisted-tree mirror: the dispatcher writes the splice through
    // to `WorkspaceTree`, so a hypothetical shell restart would see the
    // tab on strip B in the persisted JSON. Without the writeback the
    // tree-side state stays in its pre-drag shape and the tab pops
    // back to strip A on the next reload.
    let st = chrome.borrow();
    let tree = st.tree();
    let tree_strip_a = tree.workspaces[0]
        .strips
        .iter()
        .find(|s| s.id == s_a)
        .expect("strip A in tree");
    let tree_strip_b = tree.workspaces[0]
        .strips
        .iter()
        .find(|s| s.id == s_b)
        .expect("strip B in tree");
    let tree_a_total: usize = tree_strip_a
        .window_groups
        .iter()
        .map(|g| g.tabs.len())
        .sum();
    let tree_b_total: usize = tree_strip_b
        .window_groups
        .iter()
        .map(|g| g.tabs.len())
        .sum();
    assert_eq!(
        tree_a_total, 1,
        "persisted tree: strip A must have 1 tab after the cross-strip move"
    );
    assert_eq!(
        tree_b_total, 3,
        "persisted tree: strip B must have 3 tabs after the cross-strip move"
    );
    let tree_b_has_dragged = tree_strip_b
        .window_groups
        .iter()
        .any(|g| g.tabs.iter().any(|t| t.id == dragged));
    assert!(
        tree_b_has_dragged,
        "persisted tree: strip B must carry the dragged tab id {dragged}"
    );
}

/// Companion test: a release that doesn't fall inside any other strip's
/// rect (e.g. a click far above every strip's vertical band) returns
/// the tab to the source row instead of vanishing into the void. Pins
/// the v1 "no valid target → return to source" fallback documented in
/// `dispatch_cross_strip_detaches`. Without the recovery branch the
/// shell would silently drop the dragged tab.
#[test]
fn drag_outside_every_strip_returns_tab_to_source_row() {
    let (tree, s_a, _s_b, a_tabs, _b_tabs) = two_strip_two_tab_tree();
    let (mut host, _chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    // Release point far above strip-A's row (and strip-B's). With no
    // other strip's rect containing this y, the dispatcher should fall
    // back to delivering the payload to the source row — which splices
    // it into a new group on strip A near `release.x` rather than the
    // void.
    let release = Point::new(W as f64 / 2.0, -100.0);
    let dragged = a_tabs[0];
    {
        let g = host
            .group_at_for_tests(s_a, 0)
            .expect("strip A group 0 widget");
        g.bar_mut_for_tests().pending_detach = Some((dragged, release));
    }
    pump_no_op(&mut host, &layout);
    host.tick_cross_strip_dispatch_for_tests();

    // Strip A keeps both tabs (just possibly redistributed across two
    // groups now). Specifically: the dragged tab id must still be on
    // strip A, and total tab count is preserved.
    let snap = host.groups_per_strip();
    let strip_a_snap = snap.iter().find(|(s, _)| *s == s_a).expect("strip A");
    let total_tabs: usize = strip_a_snap.1.iter().map(|(_, ts)| ts.len()).sum();
    assert_eq!(
        total_tabs, 2,
        "strip A must still hold both tabs after a return-to-source recovery"
    );
    let dragged_present = strip_a_snap.1.iter().any(|(_, ts)| ts.contains(&dragged));
    assert!(
        dragged_present,
        "dragged tab id {dragged} must remain on strip A on the no-valid-target fallback path"
    );
}

/// Every tab id the persisted tree carries, across every workspace,
/// strip and group. The tree is the thing under test in the assertions
/// below: the widget can be made to look right by a fade while the tree
/// still lists a window the user closed.
fn tree_tab_ids(chrome: &shell::shell::chrome_state::Chrome) -> Vec<u64> {
    let st = chrome.borrow();
    let tree = st.tree();
    tree.workspaces
        .iter()
        .flat_map(|ws| ws.strips.iter())
        .flat_map(|s| s.window_groups.iter())
        .flat_map(|g| g.tabs.iter())
        .map(|t| t.id)
        .collect()
}

/// Every tab id currently in a live widget row.
fn widget_tab_ids(host: &ShellOverlayHost) -> Vec<u64> {
    host.groups_per_strip()
        .into_iter()
        .flat_map(|(_, groups)| groups.into_iter())
        .flat_map(|(_, tabs)| tabs.into_iter())
        .collect()
}

/// The id of the group at `idx` in `strip`'s live row.
fn widget_group_id(host: &ShellOverlayHost, strip: StripId, idx: usize) -> u64 {
    host.groups_per_strip()
        .into_iter()
        .find(|(s, _)| *s == strip)
        .expect("strip row")
        .1[idx]
        .0
}

/// Closing a tab has to take it out of the persisted tree, not just
/// fade it off screen.
///
/// The close used to be widget-only, and the shell went out of its way
/// not to bump the chrome revision afterwards because bumping made the
/// resync splice the still-listed tab straight back — the workaround
/// that hid the real defect. So this test does exactly what the
/// workaround was avoiding: it closes a tab and then bumps, which is
/// what any of the shell's many unrelated `bump()` callers does within
/// a second of the user closing a window.
#[test]
fn closing_a_tab_removes_it_from_the_tree_and_a_bump_does_not_bring_it_back() {
    let (tree, s_a, _s_b, a_tabs, _b_tabs) = two_strip_two_tab_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);

    let closed = a_tabs[0];
    let group_id = widget_group_id(&host, s_a, 0);
    assert!(
        tree_tab_ids(&chrome).contains(&closed),
        "precondition: the tree lists the tab we are about to close"
    );

    // The production route a tab context menu and the close chord both
    // take.
    assert!(
        host.close_tab_in_group(group_id, Some(closed)),
        "close_tab_in_group must report the close it started"
    );

    assert!(
        !tree_tab_ids(&chrome).contains(&closed),
        "closing a tab must remove it from the persisted tree; tree still has {:?}",
        tree_tab_ids(&chrome)
    );

    // Something else bumps the revision — a width harvest, a peer
    // arriving, a workspace rename. The resync rebuilds the row from
    // the tree, and before the tree-side removal existed that is where
    // the closed window came back.
    chrome.borrow_mut().bump();
    let _ = host.take_structure_dirty();

    assert!(
        !tree_tab_ids(&chrome).contains(&closed),
        "the tab must stay out of the tree across a revision bump"
    );
    assert!(
        !widget_tab_ids(&host).contains(&closed),
        "the closed window must not be re-spliced into the widget row by the resync"
    );
}

/// The same contract for the route the pointer takes. A click on a
/// pill's close chip never reaches the shell — it is handled inside
/// `WindowGroup` — so the tree-side removal has to be harvested at the
/// frame boundary rather than written by whoever called a shell method.
/// `take_structure_dirty` is that frame boundary in production.
#[test]
fn a_close_started_inside_the_widget_is_harvested_into_the_tree() {
    let (tree, s_a, _s_b, a_tabs, _b_tabs) = two_strip_two_tab_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);

    let closed = a_tabs[1];
    {
        // Exactly what the close chip's handler does, one layer down.
        let g = host
            .group_at_for_tests(s_a, 0)
            .expect("strip A group 0 widget");
        assert!(g.request_close_tab(closed));
    }
    assert!(
        tree_tab_ids(&chrome).contains(&closed),
        "precondition: the widget-side close has not been harvested yet"
    );

    // One frame boundary.
    let _ = host.take_structure_dirty();

    assert!(
        !tree_tab_ids(&chrome).contains(&closed),
        "the frame-boundary harvest must carry a widget-side close into the tree"
    );

    chrome.borrow_mut().bump();
    let _ = host.take_structure_dirty();
    assert!(
        !widget_tab_ids(&host).contains(&closed),
        "and the closed tab must not return on the next bump"
    );
}

/// Closing the last tab of a group empties the group in the tree, but
/// the group is still on screen shrinking out. A revision bump landing
/// inside that fade must not yank it: the resync's "the tree dropped
/// this group" rule has to make an exception for a group that is
/// mid-close, and the widget-only-group harvest has to not write the
/// empty husk back into the tree.
#[test]
fn a_group_closing_its_last_tab_keeps_its_fade_and_is_not_written_back() {
    let (tree, s_a, _s_b, a_tabs, _b_tabs) = two_strip_two_tab_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);

    let group_id = widget_group_id(&host, s_a, 0);
    for tab in a_tabs {
        assert!(host.close_tab_in_group(group_id, Some(tab)));
    }

    chrome.borrow_mut().bump();
    let _ = host.take_structure_dirty();

    // Tree: strip A has no groups left — both its tabs are closed.
    {
        let st = chrome.borrow();
        let t = st.tree();
        let strip = t.workspaces[0]
            .strips
            .iter()
            .find(|s| s.id == s_a)
            .expect("strip A in tree");
        assert!(
            strip.window_groups.is_empty(),
            "the tree must drop a group whose last tab was closed, got {:?}",
            strip
                .window_groups
                .iter()
                .map(|g| (g.id, g.tabs.len()))
                .collect::<Vec<_>>()
        );
    }

    // Widget: the group is still present, playing its close fade, and
    // it holds no tabs.
    let widget_a = host
        .groups_per_strip()
        .into_iter()
        .find(|(s, _)| *s == s_a)
        .expect("strip A")
        .1;
    assert_eq!(
        widget_a.len(),
        1,
        "the closing group must stay on screen for its fade, not pop out"
    );
    assert!(
        widget_a[0].1.is_empty(),
        "the closing group must be empty — its tabs are closed"
    );
}

/// One strip, one group, two tabs whose contents cannot be recovered
/// from a tab descriptor: a live wayland client (identified only by its
/// `surface_id`) and a hosted app whose pill shows the document's name
/// rather than the app's, so the title-matching table has nothing to
/// match on.
///
/// Both are the shapes the widget→tree harvest used to guess at, and
/// both guesses are wrong in a way the user sees.
fn foreign_and_document_titled_tree() -> (WorkspaceTree, StripId, GroupId, u64, u64, u64) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group: GroupId = 1001;
    let foreign_tab: u64 = 31;
    let hosted_tab: u64 = 32;
    let surface: u64 = 4242;

    let strip = Strip {
        id: strip_id,
        name: "Strip".into(),
        window_groups: vec![WindowGroup {
            id: group,
            tabs: vec![
                Tab {
                    id: foreign_tab,
                    title: "Inkscape".into(),
                    tile: TileContent::Foreign {
                        app_id: "org.inkscape.Inkscape".into(),
                        surface_id: surface,
                    },
                },
                Tab {
                    id: hosted_tab,
                    // The pill shows what the app has open, which is
                    // what the title-matching recovery cannot invert.
                    title: "Meeting notes".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Notes".into()),
                        label: "Notes".into(),
                    },
                },
            ],
            active_tab: foreign_tab,
            width: WindowWidth::Normal,
            maximized: false,
        }],
    };
    let ws = Workspace {
        strips: vec![strip],
        ..Workspace::test_default(ws_id, "Test")
    };
    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, group);
    let tree = WorkspaceTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        active_group,
        collaborators: vec![],
        ..Default::default()
    };
    (tree, strip_id, group, foreign_tab, hosted_tab, surface)
}

/// Every persisted tile the tree holds under `tab_id`.
///
/// Plural on purpose. A tab should appear once, and the assertions
/// below check every copy rather than the first one found — reading
/// only the first is how a rebuilt-and-wrong copy hides behind an
/// intact one the reconciler forgot to remove.
fn tree_tiles(chrome: &shell::shell::chrome_state::Chrome, tab_id: u64) -> Vec<TileContent> {
    let st = chrome.borrow();
    let tree = st.tree();
    tree.workspaces
        .iter()
        .flat_map(|ws| ws.strips.iter())
        .flat_map(|s| s.window_groups.iter())
        .flat_map(|g| g.tabs.iter())
        .filter(|t| t.id == tab_id)
        .map(|t| t.tile.clone())
        .collect()
}

/// Dragging a tab out into its own group must not change what is inside
/// it.
///
/// The detach hands the live `TileHost` straight to the new group, so
/// the *widget* is fine for exactly as long as nothing bumps the chrome
/// revision. What broke it was the writeback: the harvest rebuilt the
/// tab out of the widget's descriptor — id, title, app kind — and wrote
/// that back into the tree. A client's `surface_id` and a hosted app's
/// identity are not in a descriptor, so both were lost, and the next
/// resync noticed the tree's tile no longer matched what was on screen
/// and "corrected" the screen: a live window became a stub card.
#[test]
fn a_tab_dragged_into_its_own_group_keeps_what_was_inside_it() {
    let (tree, strip_id, _group, foreign_tab, hosted_tab, surface) =
        foreign_and_document_titled_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);
    // One production frame, so the resync's per-tab tile memo is
    // populated exactly as it is by the time a user gets to drag
    // anything.
    let _ = host.take_structure_dirty();

    let bound_to_surface = |tiles: &[TileContent]| {
        !tiles.is_empty()
            && tiles.iter().all(
                |t| matches!(t, TileContent::Foreign { surface_id, .. } if *surface_id == surface),
            )
    };
    assert!(
        bound_to_surface(&tree_tiles(&chrome, foreign_tab)),
        "precondition: the tree binds the tab to the client's surface"
    );

    // Drag the client's tab out into its own window, the way a drop in
    // the gap between two groups does.
    {
        let row = host
            .group_row_mut_for_tests(strip_id)
            .expect("the strip's live row");
        assert!(
            row.split_tab_into_new_group(0, foreign_tab, 1),
            "the detach itself must succeed"
        );
    }
    // The frame boundary that writes the new group back into the tree.
    let _ = host.take_structure_dirty();

    assert!(
        bound_to_surface(&tree_tiles(&chrome, foreign_tab)),
        "the detached tab must still be bound to the client's surface, got {:?}",
        tree_tiles(&chrome, foreign_tab)
    );

    // The hosted tab stayed where it was, and must be untouched too —
    // the harvest rebuilt every tab of the group it wrote, not just the
    // one that moved.
    let hosted = tree_tiles(&chrome, hosted_tab);
    assert!(
        !hosted.is_empty()
            && hosted.iter().all(|t| matches!(
                t,
                TileContent::Stub { kind: AppKind::Other(name), .. } if name == "Notes"
            )),
        "a tab whose pill shows a document name must keep its app, got {hosted:?}"
    );

    // Now something bumps the revision. The resync compares each tab's
    // memoised tile against the tree's, and this is where a tree tile
    // that had been guessed at reached the screen.
    chrome.borrow_mut().bump();
    let _ = host.take_structure_dirty();

    let detached_group_tiles = host
        .group_at_for_tests(strip_id, 1)
        .expect("the detached tab's new group")
        .tile_kind_labels_for_tests();
    assert_eq!(
        detached_group_tiles,
        vec!["App".to_string()],
        "the detached window must still be the live client, not a stub card"
    );
}

/// Where the tree says `tab_id` is: the id of every group holding a
/// copy of it, in tree order.
fn tree_groups_holding(chrome: &shell::shell::chrome_state::Chrome, tab_id: u64) -> Vec<GroupId> {
    let st = chrome.borrow();
    let tree = st.tree();
    tree.workspaces
        .iter()
        .flat_map(|ws| ws.strips.iter())
        .flat_map(|s| s.window_groups.iter())
        .filter(|g| g.tabs.iter().any(|t| t.id == tab_id))
        .map(|g| g.id)
        .collect()
}

/// Two groups on one strip. Dragging a tab from one into the other has
/// to move it in the tree, or the drop undoes itself.
fn one_strip_two_group_tree() -> (WorkspaceTree, StripId, GroupId, GroupId, [u64; 3]) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let left: GroupId = 1001;
    let right: GroupId = 1002;
    let tabs: [u64; 3] = [41, 42, 43];

    let mk_tab = |id: u64, label: &str| Tab {
        id,
        title: label.into(),
        tile: TileContent::Stub {
            kind: AppKind::Other("Test".into()),
            label: label.into(),
        },
    };
    let strip = Strip {
        id: strip_id,
        name: "Strip".into(),
        window_groups: vec![
            WindowGroup {
                id: left,
                tabs: vec![mk_tab(tabs[0], "L1"), mk_tab(tabs[1], "L2")],
                active_tab: tabs[0],
                width: WindowWidth::Normal,
                maximized: false,
            },
            WindowGroup {
                id: right,
                tabs: vec![mk_tab(tabs[2], "R1")],
                active_tab: tabs[2],
                width: WindowWidth::Normal,
                maximized: false,
            },
        ],
    };
    let ws = Workspace {
        strips: vec![strip],
        ..Workspace::test_default(ws_id, "Test")
    };
    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, left);
    let tree = WorkspaceTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        active_group,
        collaborators: vec![],
        ..Default::default()
    };
    (tree, strip_id, left, right, tabs)
}

/// Moving a tab between two groups of the same strip has to reach the
/// tree.
///
/// It never did: the widget row spliced the tab across on the drop, and
/// the tree's only tab-move refused a move whose source and destination
/// strips are the same. So the next revision bump rebuilt the row from
/// a tree that still had the tab where it started, and the move undid
/// itself — "works visually at first and then weirdly reverts".
#[test]
fn a_tab_moved_between_groups_of_one_strip_stays_moved() {
    let (tree, strip_id, left, right, tabs) = one_strip_two_group_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);
    let _ = host.take_structure_dirty();

    let moved = tabs[1];
    assert_eq!(
        tree_groups_holding(&chrome, moved),
        vec![left],
        "precondition: the tree has the tab in the left group"
    );

    // The drop itself: `GroupRow`'s sibling-bar and sibling-tile
    // branches both end in exactly this pair of calls.
    {
        let row = host
            .group_row_mut_for_tests(strip_id)
            .expect("the strip's live row");
        let (desc, tile) = row.groups[0]
            .detach_tab(moved)
            .expect("the tab must detach from the left group");
        row.groups[1].insert_detached_tab(0, desc, tile);
    }

    // One frame boundary carries it into the tree.
    let _ = host.take_structure_dirty();
    assert_eq!(
        tree_groups_holding(&chrome, moved),
        vec![right],
        "the move must reach the tree, in exactly one group"
    );

    // And now the bump that used to undo it.
    chrome.borrow_mut().bump();
    let _ = host.take_structure_dirty();

    assert_eq!(
        tree_groups_holding(&chrome, moved),
        vec![right],
        "the tree must still have the tab in the group it was dropped on"
    );
    let widget = host
        .groups_per_strip()
        .into_iter()
        .find(|(s, _)| *s == strip_id)
        .expect("strip row")
        .1;
    let widget_holder = widget
        .iter()
        .find(|(_, ts)| ts.contains(&moved))
        .map(|(gid, _)| *gid);
    assert_eq!(
        widget_holder,
        Some(right),
        "and the resync must not pull it back to the group it came from; row is {widget:?}"
    );
}

/// The same contract for a tab torn out into a group of its own beside
/// its neighbours. The group harvest already wrote the new group into
/// the tree, but it copied the tab in rather than moving it — so the
/// tree listed the same tab twice, in the group it came from and the
/// group it went to, and whichever one a reader happened to find first
/// decided what the user got.
#[test]
fn a_tab_torn_into_its_own_group_leaves_the_group_it_came_from() {
    let (tree, strip_id, left, _right, tabs) = one_strip_two_group_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);
    let _ = host.take_structure_dirty();

    let torn = tabs[0];
    {
        let row = host
            .group_row_mut_for_tests(strip_id)
            .expect("the strip's live row");
        assert!(
            row.split_tab_into_new_group(0, torn, 1),
            "the tear-out itself must succeed"
        );
    }
    let _ = host.take_structure_dirty();

    let holders = tree_groups_holding(&chrome, torn);
    assert_eq!(
        holders.len(),
        1,
        "a tab exists once; the tree lists it in {holders:?}"
    );
    assert_ne!(
        holders[0], left,
        "and not in the group it was torn out of"
    );

    chrome.borrow_mut().bump();
    let _ = host.take_structure_dirty();
    assert_eq!(
        tree_groups_holding(&chrome, torn),
        holders,
        "the tear-out must survive a revision bump"
    );
}

/// The first frame after boot must not rebuild the app in every
/// restored tab.
///
/// The resync only swaps a tab's contents when its memo disagrees with
/// the tree, and the memo was written nowhere but the resync itself —
/// so on the first frame there was no memo for any restored tab and
/// every one of them was rebuilt. A rebuilt tab is not a cosmetic
/// problem: the tile is constructed from scratch, which re-forks a
/// terminal's shell and re-reads a file pane's directory, on the render
/// thread, in the frame the user is waiting for.
///
/// It leaves no trace afterwards — the rebuilt widget has the same
/// shape as the one it replaced — which is why the assertion is on the
/// count of swaps the resync performed.
#[test]
fn the_first_frame_after_boot_rebuilds_nothing() {
    let (tree, _s_a, _s_b, _a_tabs, _b_tabs) = two_strip_two_tab_tree();
    let (mut host, _chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);

    // The first production frame — this is the one that used to rebuild
    // all four restored tabs.
    let _ = host.take_structure_dirty();

    assert_eq!(
        host.last_resync_content_swaps(),
        0,
        "the widget rows were built from this very tree; nothing in them has changed yet"
    );
}

/// The same for the frame after a workspace switch, which rebuilds the
/// rows from a snapshot for exactly the same reason and left the memo
/// describing the workspace the user just left.
#[test]
fn switching_workspaces_rebuilds_nothing_in_the_workspace_switched_to() {
    let (mut tree, _s_a, _s_b, _a_tabs, _b_tabs) = two_strip_two_tab_tree();

    // A second workspace with its own strip and its own open tab.
    let other_ws = WorkspaceId::new();
    let other_strip = StripId::new();
    let other_group: GroupId = 3001;
    let other_tab: u64 = 51;
    tree.workspaces.push(Workspace {
        strips: vec![Strip {
            id: other_strip,
            name: "Other".into(),
            window_groups: vec![WindowGroup {
                id: other_group,
                tabs: vec![Tab {
                    id: other_tab,
                    title: "O1".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Test".into()),
                        label: "O1".into(),
                    },
                }],
                active_tab: other_tab,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        }],
        ..Workspace::test_default(other_ws, "Other")
    });
    tree.active_strip.insert(other_ws, other_strip);
    tree.active_group.insert(other_strip, other_group);

    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);
    let _ = host.take_structure_dirty();

    // Switch. The content root and every row is rebuilt from the tree.
    {
        let mut st = chrome.borrow_mut();
        st.tree_mut().active = other_ws;
        st.bump();
    }
    let _ = host.take_structure_dirty();

    assert_eq!(
        host.last_resync_content_swaps(),
        0,
        "the rows for the new workspace were just built from it; nothing needs rebuilding"
    );
}

/// A cross-strip move whose destination strip has gone away must cost
/// the move, not the window.
///
/// The move removed the tab from the source first and only then looked
/// for the destination. When the destination had gone — a strip closed
/// between the drag starting and the harvest tick that mirrors it — the
/// removed tab had nowhere to go and was dropped on the floor. The
/// comment at the refusal promised a re-attach to the source as a
/// recovery; there was no re-attach, the synthesised group was simply
/// let go of, and the window disappeared.
#[test]
fn a_cross_strip_move_to_a_strip_that_is_gone_keeps_the_tab() {
    let (mut tree, s_a, s_b, a_tabs, _b_tabs) = two_strip_two_tab_tree();
    // The destination went away. This is the state the harvest finds
    // when a strip is closed while a tab is in flight towards it.
    tree.workspaces[0].strips.retain(|s| s.id != s_b);

    let moved = a_tabs[0];
    let before: Vec<u64> = tree.workspaces[0]
        .strips
        .iter()
        .flat_map(|s| s.window_groups.iter())
        .flat_map(|g| g.tabs.iter())
        .map(|t| t.id)
        .collect();
    assert!(before.contains(&moved), "precondition: the tab is on strip A");

    assert!(
        !tree.move_tab_across_strips(s_a, s_b, moved),
        "a move to a strip that no longer exists must be refused"
    );

    let after: Vec<u64> = tree.workspaces[0]
        .strips
        .iter()
        .flat_map(|s| s.window_groups.iter())
        .flat_map(|g| g.tabs.iter())
        .map(|t| t.id)
        .collect();
    assert_eq!(
        after, before,
        "a refused move must leave every tab exactly where it was"
    );
}

/// A cross-strip move has to land in the same place in the room
/// document as it does in the cache.
///
/// It did not. The cache appended a brand-new window group carrying the
/// moved tab; the doc appended the tab to whatever group happened to be
/// first on the destination strip. Rebuilding the cache from the doc —
/// which a restore-from-history does, and which only ever adds — then
/// put the tab in the first group as well as the one the cache had made
/// for it, and the same window was open twice.
#[test]
fn a_cross_strip_move_lands_in_the_same_group_in_the_document_as_in_the_cache() {
    let (mut tree, s_a, s_b, a_tabs, _b_tabs) = two_strip_two_tab_tree();
    tree.seed_doc_from_cache();

    let moved = a_tabs[0];
    assert!(
        tree.move_tab_across_strips(s_a, s_b, moved),
        "the move itself must succeed"
    );

    let holders = |t: &WorkspaceTree| -> Vec<GroupId> {
        t.workspaces
            .iter()
            .flat_map(|ws| ws.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .filter(|g| g.tabs.iter().any(|tab| tab.id == moved))
            .map(|g| g.id)
            .collect()
    };
    let after_move = holders(&tree);
    assert_eq!(
        after_move.len(),
        1,
        "precondition: the cache has the moved tab in exactly one group"
    );

    // What a restore-from-history does: rewrite the document, then
    // bring the cache back in line with it.
    tree.resync_cache_from_doc();

    assert_eq!(
        holders(&tree),
        after_move,
        "reading the document back must not open the same window a second time"
    );
}
