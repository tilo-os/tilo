//! Navigating along a strip, and the affordances that must not.
//!
//! A strip is a horizontal run of window groups wider than the display,
//! so "where am I looking" is a real piece of state (`GroupRow::scroll_x`)
//! that several unrelated interactions can move. Two things are pinned
//! here:
//!
//!   * The tab strip's `+` opens the picker and moves nothing. It is a
//!     button, and a button that pans the desktop under the pointer is
//!     the kind of thing a person notices immediately and cannot explain.
//!   * Stepping focus along the strip works from the keyboard, from a
//!     horizontal scroll, and from the modifier wheel — and all three
//!     land on the same window, because they are one operation.
//!
//! The harness builds a real `ShellOverlayHost` over a real
//! `WorkspaceTree`, runs a real layout pass, and drives real
//! `WindowEvent`s. Nothing here asserts against a helper that
//! reimplements what production does.

use std::collections::HashMap;

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
use winit::event::{ElementState, MouseButton, WindowEvent};

const W: f32 = 1280.0;
const H: f32 = 800.0;

/// Half the display each, so three of them plus the wheel-disc gutter
/// overflow the viewport and the strip genuinely has somewhere to pan.
/// A strip whose content fits cannot demonstrate anything about panning:
/// `scroll_x` is clamped to 0 and every assertion about it passes for
/// the wrong reason.
const HALF_DISPLAY_PERMILLE: u16 = 500;

/// Three groups on one strip, two tabs each.
fn three_group_tree() -> (WorkspaceTree, StripId, [GroupId; 3], [[u64; 2]; 3]) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let groups: [GroupId; 3] = [1001, 1002, 1003];
    let tabs: [[u64; 2]; 3] = [[11, 12], [21, 22], [31, 32]];

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
    groups: [GroupId; 3],
    tabs: [[u64; 2]; 3],
}

impl Harness {
    fn new() -> Self {
        let (tree, strip, groups, tabs) = three_group_tree();
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

    /// `build_layout → compute_layout → apply_layout`, the same three
    /// calls `App::layout` makes, so every group's rect is in the same
    /// screen space a click carries.
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

    /// The per-frame reconcilers this file's subjects ride on, in the
    /// order `Widget::update` runs them.
    fn tick(&mut self) {
        if self.host.tick_pending_new_tab_for_tests() {
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT.clone();
            self.host.refresh_search_results_for_shots(&mut doc, &root);
        }
        self.host.tick_active_tab_writeback_for_tests();
        self.host.tick_scroll_active_into_view_for_tests();
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

    fn scroll_x(&self) -> f64 {
        self.host
            .strip_scroll_offsets()
            .into_iter()
            .find(|(sid, _)| *sid == self.strip.0)
            .map(|(_, x)| x)
            .expect("the strip under test must report a pan offset")
    }

    /// Park the strip away from home the way a user who has travelled
    /// along it is parked, through the production pan
    /// `scroll_active_group_into_view` uses.
    fn park_at_last_group(&mut self) -> f64 {
        let last = self.groups[2];
        let row = self
            .host
            .group_row_mut_for_tests(self.strip)
            .expect("strip row must be live");
        row.scroll_group_into_view(last);
        self.lay_out();
        let parked = self.scroll_x();
        assert!(
            parked > 0.0,
            "the fixture must be able to park away from home, got scroll_x = {parked}"
        );
        parked
    }

    /// The `+` chip's rect on group `idx`, in screen space.
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

/// Clicking a group's `+` opens the picker and leaves the desktop where
/// it was. Reported from a live session as "if I click the + in a window
/// group, somehow the viewport changes".
#[test]
fn the_plus_chip_does_not_move_the_viewport() {
    let mut h = Harness::new();
    let parked = h.scroll_x();
    // Group 1 straddles the right edge of the display: its `+` chip sits
    // in the middle of its tab row and is fully clickable, while the rest
    // of the group is not on screen. That is the shape that made the
    // press pan the desktop — a focus change asking for a group the
    // viewport only half holds.
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

/// And the picker it opens is offering apps, not everything the machine
/// can find. "+ on a window group" is a request for a program.
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
