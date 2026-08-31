//! Navigating along a strip, and the affordances that must not.
//!
//! A strip is a horizontal run of window groups wider than the display, so
//! "where am I looking" (`GroupRow::scroll_x`) is real state several
//! unrelated interactions can move. A bare wheel does *not* move the view
//! (see `desktop_env_interactions::wheel_over_strip_does_not_move_it`) —
//! the view travels through focus changes, so a sideways scroll here steps
//! focus rather than free-panning the canvas.
//!
//! The harness builds a real `ShellOverlayHost`/`WorkspaceTree`, runs a
//! real layout pass, and drives real `WindowEvent`s.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use shell::shell::build_nirios_overlay_host_with_chrome;
use shell::shell::chrome_state::Chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

const W: f32 = 1280.0;
const H: f32 = 800.0;

/// Half the display each, so three plus the wheel-disc gutter overflow the
/// viewport — content that fits cannot demonstrate anything about panning.
const HALF_DISPLAY_PERMILLE: u16 = 500;

/// Enough groups that travelling to the far end pushes the near end
/// entirely off screen; with fewer, the first would still poke in at
/// maximum scroll.
const GROUPS: usize = 5;

fn strip_tree() -> (WorkspaceTree, StripId, Vec<GroupId>, Vec<[u64; 2]>) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let groups: Vec<GroupId> = (0..GROUPS)
        .map(|i| 1001u64.saturating_add(i as u64))
        .collect();
    let tabs: Vec<[u64; 2]> = (0..GROUPS)
        .map(|i| {
            let base = (i as u64).saturating_add(1).saturating_mul(10);
            [base.saturating_add(1), base.saturating_add(2)]
        })
        .collect();

    let mk_tab = |id: u64, label: &str| Tab {
        id,
        title: label.into(),
        tile: TileContent::Stub {
            kind: AppKind::Other("Test".into()),
            label: label.into(),
        },
    };

    let window_groups = groups
        .iter()
        .zip(tabs.iter())
        .enumerate()
        .map(|(i, (&gid, ids))| WindowGroup {
            id: gid,
            tabs: vec![
                mk_tab(ids[0], &format!("G{i}-A")),
                mk_tab(ids[1], &format!("G{i}-B")),
            ],
            active_tab: ids[0],
            width: WindowWidth::Fraction(HALF_DISPLAY_PERMILLE),
            maximized: false,
        })
        .collect();

    let ws = Workspace {
        strips: vec![Strip {
            id: strip_id,
            name: "Main".into(),
            window_groups,
        }],
        ..Workspace::test_default(ws_id, "Test")
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, groups[0]);

    let tree = WorkspaceTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        active_group,
        collaborators: vec![],
        ..Default::default()
    };
    (tree, strip_id, groups, tabs)
}

struct Harness {
    host: ShellOverlayHost,
    chrome: Chrome,
    taffy: TaffyTree<()>,
    child: NodeId,
    strip: StripId,
    groups: Vec<GroupId>,
    tabs: Vec<[u64; 2]>,
}

impl Harness {
    fn new() -> Self {
        let (tree, strip, groups, tabs) = strip_tree();
        let (host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
        let mut h = Harness {
            host,
            chrome,
            taffy: TaffyTree::new(),
            child: NodeId::new(0),
            strip,
            groups,
            tabs,
        };
        h.lay_out();
        h
    }

    /// The same three calls `App::layout` makes, so every group's rect is in
    /// the screen space a click carries.
    fn lay_out(&mut self) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = self.host.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
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
                root,
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
        self.host.apply_layout(&taffy, child, Point::ORIGIN);
        self.taffy = taffy;
        self.child = child;
    }

    fn event(&mut self, event: &WindowEvent) -> bool {
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT.clone();
        let modifiers = winit::event::Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        self.host.handle_event(
            event,
            1.0,
            &modifiers,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            self.child,
            &self.taffy,
        );
        handled
    }

    /// The per-frame reconcilers this file's subjects ride on, in the order
    /// `Widget::update` runs them.
    fn tick(&mut self) {
        if self.host.tick_pending_new_tab_for_tests() {
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT.clone();
            self.host.refresh_search_results_for_shots(&mut doc, &root);
        }
        self.host.tick_clear_stale_open_target_for_tests();
        self.host.tick_active_tab_writeback_for_tests();
        self.host.tick_scroll_active_into_view_for_tests();
        // Run the focus-follow pan (an eased curve on wall time) to its end via
        // `tick_snap_at`, rather than asserting mid-animation.
        if let Some(row) = self.host.group_row_mut_for_tests(self.strip) {
            row.tick_snap_at(
                Instant::now()
                    .checked_add(Duration::from_secs(2))
                    .unwrap_or_else(Instant::now),
            );
        }
        self.lay_out();
    }

    fn click(&mut self, p: Point) {
        self.event(&WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        });
        self.event(&WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.event(&WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    fn key(&mut self, logical: Key, mods: ModifiersState) {
        // winit's `KeyEvent` carries a private platform field, so the struct is
        // zero-initialised and every public field overwritten.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        let event = WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        };
        let modifiers = Modifiers::from(mods);
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        self.host.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            self.child,
            &self.taffy,
        );
    }

    /// A horizontally-dominant wheel event over the strip, the way a
    /// two-finger sideways swipe arrives.
    fn scroll_horizontally(&mut self, dx: f32) {
        self.event(&WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(600.0, 500.0),
        });
        self.event(&WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: MouseScrollDelta::LineDelta(dx, 0.0),
            phase: winit::event::TouchPhase::Moved,
        });
    }

    fn scroll_x(&self) -> f64 {
        self.host
            .strip_scroll_offsets()
            .into_iter()
            .find(|(sid, ..)| *sid == self.strip.0)
            .map(|(_, x, _)| x)
            .expect("the strip under test must report a pan offset")
    }

    fn plus_rect(&mut self, idx: usize) -> Rect {
        let r = self
            .host
            .group_at_for_tests(self.strip, idx)
            .expect("group must be live in the widget tree")
            .new_tab_rect();
        assert!(
            r.width() > 0.0 && r.height() > 0.0,
            "group {idx}'s + chip has no rect; the layout pass did not reach it"
        );
        r
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "st's only use is producing tree via tree(), read from through the function's \
                  last statement; nothing to drop earlier"
    )]
    fn active_window(&self) -> (GroupId, u64) {
        let st = self.chrome.borrow();
        let tree = st.tree();
        let gid = tree.active_group_for(self.strip).expect("an active group");
        let ws = tree.active_workspace().expect("an active workspace");
        let strip = ws
            .strips
            .iter()
            .find(|s| s.id == self.strip)
            .expect("the strip under test");
        let g = strip
            .window_groups
            .iter()
            .find(|g| g.id == gid)
            .expect("the active group in the tree");
        (gid, g.active_tab)
    }
}

/// The strip must actually overflow, or nothing below is testing panning.
#[test]
fn the_fixture_strip_is_wider_than_its_viewport() {
    let mut h = Harness::new();
    let max = h
        .host
        .group_row_mut_for_tests(h.strip)
        .expect("strip row must be live")
        .max_scroll_x();
    assert!(
        max > 0.0,
        "fixture must overflow the viewport, else every pan assertion \
         passes because there is nowhere to pan (max_scroll_x = {max})"
    );
}

/// Clicking a group's `+` opens the picker and leaves the desktop where it
/// was. Reported from a live session as "if I click the + in a window
/// group, somehow the viewport changes".
#[test]
fn the_plus_chip_does_not_move_the_viewport() {
    let mut h = Harness::new();
    let parked = h.scroll_x();
    // Group 1 straddles the right edge, so its `+` is fully clickable while
    // the rest of the group is off screen — the shape that made the press pan
    // the desktop.
    let row_right = h
        .host
        .group_row_rect_for_tests(h.strip)
        .expect("strip row must be laid out")
        .x1;
    let g1 = h
        .host
        .group_at_for_tests(h.strip, 1)
        .expect("group 1 must be live")
        .get_rect();
    assert!(
        g1.x0 < row_right && g1.x1 > row_right,
        "fixture must leave group 1 straddling the viewport edge \
         (group {g1:?}, viewport ends at {row_right}); otherwise the \
         press has nothing to reveal and the test proves nothing"
    );
    let plus = h.plus_rect(1);
    h.click(plus.center());
    h.tick();

    assert_eq!(
        h.scroll_x(),
        parked,
        "clicking + moved the strip from {parked} to {}",
        h.scroll_x()
    );
    let (visible, ..) = h.host.palette_snapshot();
    assert!(visible, "clicking + must open the picker");
}

/// The picker offers apps, not everything the machine can find: "+ on a
/// window group" is a request for a program.
#[test]
fn the_plus_chip_opens_the_picker_filtered_to_apps() {
    let mut h = Harness::new();
    let plus = h.plus_rect(0);
    h.click(plus.center());
    h.tick();

    let filter = h.host.palette_kind_filter();
    assert_eq!(
        filter,
        vec!["Applications".to_string(), "Host Apps".to_string()],
        "the + chip must pre-filter the picker to the kinds that can be a \
         tab, and to nothing else"
    );
    let (visible, ..) = h.host.palette_snapshot();
    assert!(visible, "clicking + must open the picker");
}

// Travelling along the strip: three ways in, one operation.

/// Super+Right walks forward through the strip's windows and does not stop
/// at a group boundary — the strip is one sequence of windows, not a set
/// of independent tab bars.
#[test]
fn super_right_walks_the_whole_strip() {
    let mut h = Harness::new();
    assert_eq!(h.active_window(), (h.groups[0], h.tabs[0][0]));

    // Built from the fixture so the expectation cannot drift out of step.
    let expected: Vec<(GroupId, u64)> = h
        .groups
        .iter()
        .zip(h.tabs.iter())
        .flat_map(|(&g, ids)| [(g, ids[0]), (g, ids[1])])
        .skip(1)
        .collect();
    for (step, want) in expected.iter().enumerate() {
        h.key(Key::Named(NamedKey::ArrowRight), ModifiersState::SUPER);
        h.tick();
        assert_eq!(
            h.active_window(),
            *want,
            "step {} of Super+Right landed on the wrong window",
            step + 1
        );
    }
}

/// Super+Left is its inverse, and neither wraps: wrapping would teleport
/// the viewport the whole length of the strip for what the user asked to
/// be one step.
#[test]
fn super_left_is_the_inverse_and_neither_wraps() {
    let mut h = Harness::new();
    for _ in 0..GROUPS * 2 + 3 {
        h.key(Key::Named(NamedKey::ArrowRight), ModifiersState::SUPER);
        h.tick();
    }
    assert_eq!(
        h.active_window(),
        (h.groups[GROUPS - 1], h.tabs[GROUPS - 1][1]),
        "pushing past the end must clamp at the last window, not wrap"
    );

    for _ in 0..GROUPS * 2 + 3 {
        h.key(Key::Named(NamedKey::ArrowLeft), ModifiersState::SUPER);
        h.tick();
    }
    assert_eq!(
        h.active_window(),
        (h.groups[0], h.tabs[0][0]),
        "pushing past the start must clamp at the first window, not wrap"
    );
}

/// Focus travelling off-screen brings the strip with it; without this the
/// chord silently focuses a window the user cannot see.
#[test]
fn stepping_to_an_offscreen_window_pans_the_strip() {
    let mut h = Harness::new();
    let home = h.scroll_x();
    // The last group is entirely off the right of the viewport at rest.
    let g2 = h
        .host
        .group_at_for_tests(h.strip, GROUPS - 1)
        .expect("the last group must be live")
        .get_rect();
    let row_right = h
        .host
        .group_row_rect_for_tests(h.strip)
        .expect("strip row")
        .x1;
    assert!(
        g2.x0 > row_right,
        "fixture must start with its last group off screen (group {g2:?}, viewport ends {row_right})"
    );

    for _ in 0..GROUPS * 2 {
        h.key(Key::Named(NamedKey::ArrowRight), ModifiersState::SUPER);
        h.tick();
    }
    assert_eq!(
        h.active_window().0,
        h.groups[GROUPS - 1],
        "focus must have reached the last group"
    );
    assert!(
        h.scroll_x() > home,
        "the strip must follow focus off screen; still parked at {home}"
    );
}

/// A sideways two-finger swipe is the same operation as the chord —
/// literally. Two navigation models agreeing on the easy case and
/// diverging at a group boundary is the failure this pins.
#[test]
fn a_horizontal_swipe_and_the_chord_are_one_operation() {
    let by_key = {
        let mut h = Harness::new();
        for _ in 0..3 {
            h.key(Key::Named(NamedKey::ArrowRight), ModifiersState::SUPER);
            h.tick();
        }
        h.active_window()
    };
    let by_swipe = {
        let mut h = Harness::new();
        for _ in 0..3 {
            // Positive dx steps forward, matching the chord's direction.
            h.scroll_horizontally(4.0);
            h.tick();
        }
        h.active_window()
    };
    assert_eq!(
        by_key, by_swipe,
        "three swipes and three Super+Rights must land on the same window"
    );
    // …and across a group boundary, so this is not two implementations
    // agreeing inside one tab bar.
    let h = Harness::new();
    assert_ne!(
        by_key.0,
        h.active_window().0,
        "the fixture must carry the walk across a group boundary"
    );
}

// The rail: the thing that says there is more strip.

/// The rail reports what the strip holds and which part is on screen —
/// the fact whose absence produced "how do I change windows?".
#[test]
fn the_rail_maps_the_strip_and_marks_what_is_on_screen() {
    let mut h = Harness::new();
    let rail = h.host.strip_rail_for_tests();
    assert_eq!(rail.count, GROUPS, "one segment per window group");
    assert!(rail.is_meaningful(), "three groups is worth drawing");
    assert_eq!(rail.focused, 0, "focus starts on the first group");
    assert_eq!(
        rail.visible,
        vec![0, 1],
        "at rest the viewport holds groups 0 and 1; the third is the \
         one the rail exists to advertise"
    );

    // Travel to the far end and the lit run follows.
    for _ in 0..GROUPS * 2 {
        h.key(Key::Named(NamedKey::ArrowRight), ModifiersState::SUPER);
        h.tick();
    }
    let rail = h.host.strip_rail_for_tests();
    assert_eq!(rail.focused, GROUPS - 1, "focus reached the last group");
    assert!(
        rail.visible.contains(&(GROUPS - 1)),
        "the group the user travelled to must read as on screen, got {:?}",
        rail.visible
    );
    assert!(
        !rail.visible.contains(&0),
        "the group they left behind must not, got {:?}",
        rail.visible
    );
}

/// And it is a control: clicking a segment goes there, including to an
/// off-screen window — the one pointer action allowed to pan the strip,
/// because its target may not be visible.
#[test]
fn clicking_a_rail_segment_travels_to_that_window() {
    let mut h = Harness::new();
    let home = h.scroll_x();
    let seg = h
        .host
        .strip_rail_for_tests()
        .hit_rect(GROUPS - 1)
        .expect("the rail must offer a target for the last group");
    h.click(seg.center());
    h.tick();

    assert_eq!(
        h.active_window().0,
        h.groups[GROUPS - 1],
        "clicking the last segment must focus the last group"
    );
    assert!(
        h.scroll_x() > home,
        "and bring it into view; the strip is still parked at {home}"
    );
}

/// The filter the `+` seeds belongs to the picker it opened; left standing
/// it would silently decide what an unrelated later query can find.
#[test]
fn the_apps_filter_does_not_outlive_the_picker() {
    let mut h = Harness::new();
    let plus = h.plus_rect(0);
    h.click(plus.center());
    h.tick();
    assert!(
        !h.host.palette_kind_filter().is_empty(),
        "precondition: the + seeded a filter"
    );

    h.key(Key::Named(NamedKey::Escape), ModifiersState::empty());
    h.tick();
    let (visible, ..) = h.host.palette_snapshot();
    assert!(!visible, "Escape must dismiss the picker");
    assert!(
        h.host.palette_kind_filter().is_empty(),
        "the filter must go with it, got {:?}",
        h.host.palette_kind_filter()
    );
}
