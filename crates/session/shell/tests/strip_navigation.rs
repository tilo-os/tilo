//! Navigating along a strip, and the affordances that must not.
//!
//! A strip is a horizontal run of window groups wider than the display, so
//! "where am I looking" (`GroupRow::scroll_x`) is real state several
//! unrelated interactions can move. A bare wheel does *not* move the view
//! (see `desktop_env_interactions::focus_scrolls_groups_into_view_but_the_wheel_does_not_pan`) —
//! the view travels through focus changes, so a sideways scroll here steps
//! focus rather than free-panning the canvas.
//!
//! The harness builds a `ShellOverlayHost`/`WorkspaceTree`, runs a layout
//! pass, and drives `WindowEvent`s.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use shell::shell::state::FreshId as _;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use shell::shell::build_tilo_overlay_host_with_chrome;
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

/// Just over half the strip's usable width (the display minus the wheel-disc
/// gutter and trailing gutter) each, so the second group's trailing controls
/// remain clickable while the group itself still crosses the viewport edge
/// and the third is entirely off screen — content that fits cannot
/// demonstrate anything about panning.
const GROUP_WIDTH_PERMILLE: u16 = 515;

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
            width: WindowWidth::Fraction(GROUP_WIDTH_PERMILLE),
            maximized: false,
        })
        .collect();

    let ws = Workspace {
        strips: vec![Strip {
            id: strip_id,
            name: "Main".into(),
            window_groups,
        }],
        ..Workspace::new(ws_id, "Test")
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, groups[0]);

    let tree = WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            collaborators: vec![],
            ..Default::default()
        },
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
        Self::with_tree(&tree, strip, groups, tabs)
    }

    fn with_tree(
        tree: &WorkspaceTree,
        strip: StripId,
        groups: Vec<GroupId>,
        tabs: Vec<[u64; 2]>,
    ) -> Self {
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(tree);
        // This fixture minted the tabs, so it explicitly creates their
        // documents before presentation attempts existing-only admission.
        let (records, rendered_tabs) = {
            let chrome = chrome.borrow();
            let rendered = chrome.tree();
            let tabs = rendered
                .desktop
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .map(|tab| tab.id)
                .collect::<Vec<_>>();
            (chrome.records().clone(), tabs)
        };
        for tab in rendered_tabs {
            let checkout = shell::shell::tab_documents::scratch_tab_record(tab);
            shell::shell::tab_documents::create_scratch_tab_record(&records, tab)
                .expect("the fixture must create each rendered tab's document");
            assert_eq!(
                records.read(&checkout.id, ui::record::identity),
                Some(Some(checkout)),
                "the rendered tab must have its own existing document before input"
            );
        }
        host.set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
            .expect("the built-in shortcut table is valid");
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

    /// The same calls the shell frame makes, so every group's rect is in
    /// the screen space a click carries.
    fn lay_out(&mut self) {
        // The frame names the display before every build, so window-group
        // width fractions resolve against it on the very first pass. Without
        // it the first pass sizes groups against the toolkit's fallback and
        // the desktop rightly refuses to frame a selection on that stale
        // geometry until a second layout settles it.
        self.host
            .set_output_geometry(&[shell::shell::overlay_host::OutputGeometry::unscaled(
                0.0,
                0.0,
                f64::from(W),
                f64::from(H),
            )]);
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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: self.child,
                taffy: &self.taffy,
            },
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
        self.host.flush_active_tab_writeback();
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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: self.child,
                taffy: &self.taffy,
            },
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
        let gid = tree
            .desktop
            .active_group_for(self.strip)
            .expect("an active group");
        let ws = tree
            .desktop
            .active_workspace()
            .expect("an active workspace");
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

/// [`strip_tree`] with a second strip below holding one group, `resident`,
/// and the document seeded as production trees carry it.
fn two_strip_harness() -> (Harness, StripId, StripId, GroupId, Vec<GroupId>) {
    let (mut tree, upper, groups, tabs) = strip_tree();
    let lower = StripId::new();
    let resident: GroupId = 2001;
    tree.desktop.workspaces[0].strips.push(Strip {
        id: lower,
        name: "Below".into(),
        window_groups: vec![WindowGroup {
            id: resident,
            tabs: [91, 92]
                .into_iter()
                .map(|id| Tab {
                    id,
                    title: format!("Below {id}"),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Test".into()),
                        label: format!("Below {id}"),
                    },
                })
                .collect(),
            active_tab: 91,
            width: WindowWidth::Fraction(GROUP_WIDTH_PERMILLE),
            maximized: false,
        }],
    });
    tree.desktop.active_group.insert(lower, resident);
    // The local person owns the desk, so rearranging it is theirs to do.
    tree.adopt_ownerless_workspaces(&shell::shell::state::local_user_id());
    tree.seed_doc_from_cache();
    let h = Harness::with_tree(&tree, upper, groups.clone(), tabs);
    (h, upper, lower, resident, groups)
}

/// Each strip's groups, the focused strip and its focused group.
type Plane = (Vec<(StripId, Vec<GroupId>)>, StripId, Option<GroupId>);

fn plane(h: &Harness) -> Plane {
    let st = h.chrome.borrow();
    let tree = st.tree();
    let active_strip = tree.desktop.active_strip[&tree.desktop.active];
    let strips = tree.desktop.workspaces[0]
        .strips
        .iter()
        .map(|strip| {
            (
                strip.id,
                strip.window_groups.iter().map(|g| g.id).collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let focused = tree.desktop.active_group_for(active_strip);
    (strips, active_strip, focused)
}

/// Invoke shell action `id` through the action door, as `tilo-control
/// actions invoke` and the palette do.
fn invoke(
    h: &Harness,
    id: &str,
    args: &serde_json::Value,
) -> Result<(), shell::shell::search::action_api::ActionError> {
    use shell::shell::search::action_api::ActionError;
    use shell::shell::search::action_api::{AllVisible, ResourceRef};
    let mut doc = automerge::AutoCommit::new();
    let root = automerge::ROOT;
    let catalog = {
        let st = h.chrome.borrow();
        h.host
            .exact_action_catalog("actions", id, &st, &doc, &root, &AllVisible)
    };
    let selected = catalog
        .select(&ResourceRef::new("actions", id))
        .ok_or_else(|| ActionError::Refused(format!("{id} is not on the action door")))?;
    let args = args.as_object().cloned().unwrap_or_default();
    selected
        .invoke_json(&mut doc, &root, &h.chrome, &args)
        .map(drop)
}

/// Super+Up/Down moves focus between strips and Super+Shift+Up/Down carries
/// the focused group the same way, whole, into the durable tree; Super+Up no
/// longer maximizes.
#[test]
fn shift_arrows_carry_the_focused_group_between_strips() {
    let (mut h, upper, lower, resident, groups) = two_strip_harness();
    let layout = plane;
    let upper_maximized = |h: &mut Harness| {
        h.host
            .group_row_mut_for_tests(upper)
            .expect("the upper strip is live")
            .groups
            .iter()
            .any(|group| group.maximized)
    };

    h.key(
        Key::Named(NamedKey::ArrowDown),
        ModifiersState::SUPER | ModifiersState::SHIFT,
    );
    let (strips, active, focused) = layout(&h);
    assert_eq!(strips[0], (upper, groups[1..].to_vec()));
    assert_eq!(
        strips[1],
        (lower, vec![resident, groups[0]]),
        "the carried group lands after the destination's focused group"
    );
    assert_eq!(active, lower, "focus follows the carried group");
    assert_eq!(focused, Some(groups[0]));

    h.key(
        Key::Named(NamedKey::ArrowDown),
        ModifiersState::SUPER | ModifiersState::SHIFT,
    );
    assert_eq!(
        layout(&h).0,
        strips,
        "carrying past the last strip clamps rather than creating one"
    );

    h.key(Key::Named(NamedKey::ArrowUp), ModifiersState::SUPER);
    let (after_up, active, focused) = layout(&h);
    assert_eq!(after_up, strips, "Super+Up moves focus, not windows");
    assert_eq!(active, upper);
    assert_eq!(
        focused,
        Some(groups[1]),
        "the strip a group left keeps focus on its neighbour"
    );
    assert!(!upper_maximized(&mut h), "Super+Up no longer maximizes");
    h.key(Key::Character("m".into()), ModifiersState::SUPER);
    assert!(
        upper_maximized(&mut h),
        "Super+M maximizes the focused group"
    );
}

/// The relative window actions run the operation their chord runs, on the
/// overlay host's next update, with no key event.
#[test]
fn window_actions_run_the_operations_their_chords_run() {
    let (mut h, upper, lower, resident, groups) = two_strip_harness();
    let none = serde_json::json!({});

    invoke(&h, "group.move-to-strip-next", &none).unwrap();
    assert_eq!(
        plane(&h).0[1],
        (lower, vec![resident]),
        "an action runs on the host's update, not inside the invocation"
    );
    h.host.run_requested_shortcut_actions();
    let (strips, active, focused) = plane(&h);
    assert_eq!(strips[1], (lower, vec![resident, groups[0]]));
    assert_eq!((active, focused), (lower, Some(groups[0])));

    invoke(&h, "strip.focus-previous", &none).unwrap();
    h.host.run_requested_shortcut_actions();
    assert_eq!(plane(&h).1, upper);

    invoke(&h, "window.focus-nth", &serde_json::json!({ "index": 3 })).unwrap();
    h.host.run_requested_shortcut_actions();
    assert_eq!(
        plane(&h).2,
        Some(groups[3]),
        "the third group of the upper strip, which now starts at the second"
    );
    for index in [0, 10] {
        assert!(
            invoke(
                &h,
                "window.focus-nth",
                &serde_json::json!({ "index": index })
            )
            .is_err(),
            "index {index} names no Super+N chord"
        );
    }

    invoke(&h, "group.move-right", &none).unwrap();
    h.host.run_requested_shortcut_actions();
    assert_eq!(
        plane(&h).0[0].1,
        vec![groups[1], groups[2], groups[4], groups[3]],
        "group.move-right is Super+Shift+Right"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn the_plus_chip_opens_an_apps_picker_without_moving_the_wide_strip() {
    // Phase `the_fixture_strip_is_wider_than_its_viewport`.
    // The strip must actually overflow, or nothing below is testing panning.
    {
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

    // Phase `the_plus_chip_does_not_move_the_viewport`.
    // Clicking a group's `+` opens the picker and leaves the desktop where it
    // was. Reported from a live session as "if I click the + in a window
    // group, somehow the viewport changes".
    {
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
        let row = h
            .host
            .group_row_rect_for_tests(h.strip)
            .expect("strip row must be laid out");
        let click = Point::new(plus.x0 + 1.0, plus.center().y);
        assert!(
            row.contains(click),
            "fixture must leave group 1's + chip clickable ({plus:?} outside {row:?})"
        );
        if let Some(sidebar) = h.host.sidebar_rect_for_tests() {
            assert!(
                !sidebar.contains(click),
                "fixture must leave the click clear of sidebar chrome ({click:?} inside {sidebar:?})"
            );
        }
        h.click(click);
        h.tick();

        assert!(
            (h.scroll_x() - parked).abs() <= f64::EPSILON,
            "clicking + moved the strip from {parked} to {}",
            h.scroll_x()
        );
        let (visible, ..) = h.host.palette_snapshot();
        assert!(visible, "clicking + must open the picker");
    }

    // Phase `the_plus_chip_opens_the_picker_filtered_to_apps`.
    // The picker offers apps, not everything the machine can find: "+ on a
    // window group" is a request for a program.
    {
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

    // Phase `the_apps_filter_does_not_outlive_the_picker`.
    // The filter the `+` seeds belongs to the picker it opened; left standing
    // it would silently decide what an unrelated later query can find.
    {
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
}

#[test]
fn keyboard_edits_resize_reorder_and_toggle_the_guide_after_flushed_clicks() {
    // Phase `a_clicked_tab_can_be_flushed_before_the_next_input_event`.
    // A tab click mutates the widget-owned selection before the next regular
    // update. The compositor flushes that writeback between backend events so a
    // key already queued behind the click resolves to the newly visible tab.
    {
        let mut h = Harness::new();
        let second = h.tabs[0][1];
        let pill = h
            .host
            .group_at_for_tests(h.strip, 0)
            .and_then(|group| group.tab_rects().get(1).copied())
            .expect("the second tab must have a laid-out pill");

        h.click(pill.center());
        assert_eq!(
            h.active_window().1,
            h.tabs[0][0],
            "the durable tree is intentionally stale until the input-boundary flush"
        );
        h.host.flush_active_tab_writeback();
        assert_eq!(
            h.active_window().1,
            second,
            "the next input event must see the tab selected by the click"
        );
    }

    // Phase `keyboard_resize_changes_the_persisted_fraction`.
    {
        let mut h = Harness::new();
        let before = {
            let state = h.chrome.borrow();
            state.tree().desktop.workspaces[0].strips[0].window_groups[0]
                .width
                .permille()
        };
        h.key(Key::Character("=".into()), ModifiersState::SUPER);
        let after = {
            let state = h.chrome.borrow();
            state.tree().desktop.workspaces[0].strips[0].window_groups[0]
                .width
                .permille()
        };
        assert_eq!(after, before + 50);
    }

    // Phase `shifted_arrow_reorders_the_focused_group`.
    {
        let mut h = Harness::new();
        h.key(
            Key::Named(NamedKey::ArrowRight),
            ModifiersState::SUPER | ModifiersState::SHIFT,
        );
        let order: Vec<_> = {
            let state = h.chrome.borrow();
            state.tree().desktop.workspaces[0].strips[0]
                .window_groups
                .iter()
                .map(|group| group.id)
                .collect()
        };
        assert_eq!(order[0], h.groups[1]);
        assert_eq!(order[1], h.groups[0]);
    }

    // Phase `f1_toggles_the_desktop_guide`.
    {
        let mut h = Harness::new();
        assert!(!h.chrome.borrow().overlays.shortcut_help);
        h.key(Key::Named(NamedKey::F1), ModifiersState::empty());
        assert!(h.chrome.borrow().overlays.shortcut_help);
        h.key(Key::Named(NamedKey::F1), ModifiersState::empty());
        assert!(!h.chrome.borrow().overlays.shortcut_help);
    }
}

// Travelling along the strip: three ways in, one operation.

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn keyboard_and_swipe_stepping_walks_the_strip_without_wrapping() {
    // Phase `super_right_walks_the_whole_strip`.
    // Super+Right walks forward through the strip's windows and does not stop
    // at a group boundary — the strip is one sequence of windows, not a set
    // of independent tab bars.
    {
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

    // Phase `super_left_is_the_inverse_and_neither_wraps`.
    // Super+Left is its inverse, and neither wraps: wrapping would teleport
    // the viewport the whole length of the strip for what the user asked to
    // be one step.
    {
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

    // Phase `stepping_to_an_offscreen_window_pans_the_strip`.
    // Focus travelling off-screen brings the strip with it; without this the
    // chord silently focuses a window the user cannot see.
    {
        let mut h = Harness::new();
        // App's bounded composition settlement admits the final output-width
        // basis before accepting input. This manual harness needs its next
        // layout pass too, before advancing the resulting focus animation.
        h.tick();
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

    // Phase `a_horizontal_swipe_and_the_chord_are_one_operation`.
    // A sideways two-finger swipe is the same operation as the chord —
    // literally. Two navigation models agreeing on the easy case and
    // diverging at a group boundary is the failure this pins.
    {
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

    // Phase `ctrl_tab_cycles_tabs_inside_the_focused_group`.
    {
        let mut h = Harness::new();
        assert_eq!(h.active_window(), (h.groups[0], h.tabs[0][0]));

        h.key(Key::Named(NamedKey::Tab), ModifiersState::CONTROL);
        assert_eq!(h.active_window(), (h.groups[0], h.tabs[0][1]));

        h.key(Key::Named(NamedKey::Tab), ModifiersState::CONTROL);
        assert_eq!(
            h.active_window(),
            (h.groups[0], h.tabs[0][0]),
            "tab cycling wraps within the group rather than leaving it"
        );
    }

    // Phase `conventional_workspace_page_keys_use_the_same_clamped_workspace_axis`.
    {
        let mut h = Harness::new();
        let first = h.chrome.borrow().tree().desktop.active;
        let second = WorkspaceId::new();
        h.chrome
            .borrow_mut()
            .tree_mut()
            .desktop
            .workspaces
            .push(Workspace::new(second, "Second"));

        h.key(Key::Named(NamedKey::PageDown), ModifiersState::SUPER);
        assert_eq!(h.chrome.borrow().tree().desktop.active, second);
        h.key(Key::Named(NamedKey::PageDown), ModifiersState::SUPER);
        assert_eq!(
            h.chrome.borrow().tree().desktop.active,
            second,
            "Super+PageDown clamps at the final workspace"
        );
        h.key(Key::Named(NamedKey::PageUp), ModifiersState::SUPER);
        assert_eq!(h.chrome.borrow().tree().desktop.active, first);
    }
}

// The rail: the thing that says there is more strip.

#[test]
fn the_rail_maps_the_strip_and_travels_on_click() {
    // Phase `the_rail_maps_the_strip_and_marks_what_is_on_screen`.
    // The rail reports what the strip holds and which part is on screen —
    // the fact whose absence produced "how do I change windows?".
    {
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

    // Phase `clicking_a_rail_segment_travels_to_that_window`.
    // And it is a control: clicking a segment goes there, including to an
    // off-screen window — the one pointer action allowed to pan the strip,
    // because its target may not be visible.
    {
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
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn every_close_route_commits_the_exact_surface_durably() {
    // Phase `clicking_close_commits_the_exact_surface_and_survives_restart`.
    // The visible close button must be the same durable operation as Super+W.
    // This was a separate widget-only path: it faded the tab, never removed it
    // from the tree and never sent `xdg_toplevel.close`, so the next unrelated
    // revision visibly restored the window.
    {
        let mut h = Harness::new();
        let state_dir = tempfile::tempdir().expect("temporary state directory");
        let state_path = state_dir.path().join("state.json");
        let tab = h.tabs[0][0];
        {
            let state = h.chrome.borrow_mut();
            let mut tree = state.tree_mut();
            tree.persist_path = Some(state_path.clone());
            tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile =
                TileContent::Foreign {
                    app_id: "com.openai.chatgpt".into(),
                    surface_id: 41,
                };
            // Real sessions always carry a room document. Lifecycle operations
            // intentionally refuse a cache-only mutation because the next peer
            // revision would undo it.
            tree.seed_doc_from_cache();
            tree.queue_save();
        }

        let close = h
            .host
            .group_at_for_tests(h.strip, 0)
            .and_then(|group| group.close_rect_for_tests(tab))
            .expect("active tab must expose a real close hit target after layout");
        h.click(close.center());

        {
            let state = h.chrome.borrow();
            let tree = state.tree();
            let live_tabs = &tree.desktop.workspaces[0].strips[0].window_groups[0].tabs;
            assert!(
                live_tabs.iter().all(|candidate| candidate.id != tab),
                "the pointer close must remove the tab from the durable tree in the same event"
            );
            drop(tree);
        }
        assert_eq!(
            h.chrome.borrow_mut().take_surface_close_requests(),
            vec![41],
            "the close must address the exact surface, not an app id shared by other windows"
        );

        // Both historical resurrection edges: a later chrome revision and a
        // fresh compositor load.
        h.chrome.borrow_mut().bump();
        h.tick();
        assert!(
            h.chrome.borrow().tree().desktop.workspaces[0].strips[0].window_groups[0]
                .tabs
                .iter()
                .all(|candidate| candidate.id != tab),
            "an unrelated revision must not restore the clicked-close tab"
        );
        let restarted = shell::shell::persist::load_tree_from(&state_path)
            .expect("pointer close must atomically persist a loadable state");
        assert!(
            restarted.desktop.workspaces[0].strips[0].window_groups[0]
                .tabs
                .iter()
                .all(|candidate| candidate.id != tab),
            "a normal restart must keep the clicked-close tab absent"
        );

        // Close the remaining tab through its newly laid-out hit target. The
        // whole group must disappear durably as one consequence of its last
        // window closing; this is the live-session "closed group came back"
        // failure, not merely the two-tabs case above.
        let remaining = h.tabs[0][1];
        let close = h
            .host
            .group_at_for_tests(h.strip, 0)
            .and_then(|group| group.close_rect_for_tests(remaining))
            .expect("remaining tab must expose a close hit target after resync");
        h.click(close.center());
        assert!(
            h.chrome.borrow().tree().desktop.workspaces[0].strips[0]
                .window_groups
                .iter()
                .all(|group| group.id != h.groups[0]),
            "closing the last tab must remove its whole group in the same event"
        );
        h.chrome.borrow_mut().bump();
        h.tick();
        let restarted = shell::shell::persist::load_tree_from(&state_path)
            .expect("last-tab close must atomically persist a loadable state");
        assert!(
            restarted.desktop.workspaces[0].strips[0]
                .window_groups
                .iter()
                .all(|group| group.id != h.groups[0]),
            "revision plus restart must not resurrect a group closed by pointer"
        );
    }

    // Phase `alt_f4_uses_the_same_exact_durable_close_as_the_frame_and_super_w`.
    {
        let mut h = Harness::new();
        let first = h.tabs[0][0];
        let second = h.tabs[0][1];
        {
            let state = h.chrome.borrow_mut();
            let ws = state.tree().desktop.active;
            let mut tree = state.tree_mut();
            let group = tree.desktop.workspaces[0].strips[0]
                .window_groups
                .iter_mut()
                .find(|group| group.id == h.groups[0])
                .expect("focused group");
            group.tabs[0].tile = TileContent::Foreign {
                surface_id: 0xF401,
                app_id: "org.example.First".into(),
            };
            group.tabs[1].tile = TileContent::Foreign {
                surface_id: 0xF402,
                app_id: "org.example.Second".into(),
            };
            group.active_tab = first;
            tree.desktop.active = ws;
        }

        h.key(Key::Named(NamedKey::F4), ModifiersState::ALT);
        assert_eq!(
            h.chrome.borrow_mut().take_surface_close_requests(),
            vec![0xF401]
        );
        assert_eq!(h.active_window(), (h.groups[0], second));
        assert!(
            h.chrome.borrow().tree().desktop.workspaces[0].strips[0].window_groups[0]
                .tabs
                .iter()
                .all(|tab| tab.id != first),
            "Alt+F4 must remove the exact durable tab before asking the client to close"
        );
    }

    // Phase `pointer_then_keyboard_close_leave_navigation_and_restart_state_coherent`.
    // Drive the two independent close affordances and then ordinary group
    // navigation as one user session.  This deliberately runs a frame between
    // each mutation: historically the stale widget tree written back on the
    // following frame was enough to resurrect a just-closed group.
    {
        let mut h = Harness::new();
        let state_dir = tempfile::tempdir().expect("temporary state directory");
        let state_path = state_dir.path().join("state.json");
        {
            let state = h.chrome.borrow_mut();
            let mut tree = state.tree_mut();
            tree.persist_path = Some(state_path.clone());
            for (tab, surface_id) in [(h.tabs[0][0], 0xA11CE_u64), (h.tabs[0][1], 0xA11CF_u64)] {
                let tab = tree.desktop.workspaces[0].strips[0].window_groups[0]
                    .tabs
                    .iter_mut()
                    .find(|candidate| candidate.id == tab)
                    .expect("fixture tab");
                tab.tile = TileContent::Foreign {
                    app_id: "org.example.ExternalEditor".into(),
                    surface_id,
                };
            }
            tree.seed_doc_from_cache();
            tree.queue_save();
        }

        let first = h.tabs[0][0];
        let close = h
            .host
            .group_at_for_tests(h.strip, 0)
            .and_then(|group| group.close_rect_for_tests(first))
            .expect("pointer close target");
        h.click(close.center());
        assert_eq!(
            h.chrome.borrow_mut().take_surface_close_requests(),
            vec![0xA11CE],
            "the pointer path must issue one exact client close"
        );
        h.tick();

        h.key(Key::Character("w".into()), ModifiersState::SUPER);
        assert_eq!(
            h.chrome.borrow_mut().take_surface_close_requests(),
            vec![0xA11CF],
            "Super+W must issue one exact client close for the remaining tab"
        );
        h.tick();

        assert!(
            !h.chrome.borrow().tree().desktop.workspaces[0].strips[0]
                .window_groups
                .iter()
                .any(|group| group.id == h.groups[0]),
            "closing the last tab removes the complete group before later input"
        );
        assert_eq!(h.active_window().0, h.groups[1]);

        h.key(
            Key::Named(NamedKey::ArrowRight),
            ModifiersState::SUPER | ModifiersState::SHIFT,
        );
        h.tick();
        let reordered: Vec<_> = h.chrome.borrow().tree().desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|group| group.id)
            .collect();
        assert_eq!(&reordered[..2], &[h.groups[2], h.groups[1]]);
        assert_eq!(h.active_window().0, h.groups[1]);

        h.key(Key::Named(NamedKey::ArrowRight), ModifiersState::SUPER);
        h.tick();
        assert_eq!(
            h.active_window(),
            (h.groups[1], h.tabs[1][1]),
            "focus first advances within the moved live group"
        );
        h.key(Key::Named(NamedKey::ArrowRight), ModifiersState::SUPER);
        h.tick();
        assert_eq!(
            h.active_window().0,
            h.groups[3],
            "focus navigation must continue past the moved group, never a closed one"
        );

        h.chrome.borrow_mut().bump();
        h.tick();
        let restarted = shell::shell::persist::load_tree_from(&state_path)
            .expect("the complete interaction sequence persists a loadable state");
        let restarted_groups = &restarted.desktop.workspaces[0].strips[0].window_groups;
        assert!(restarted_groups.iter().all(|group| group.id != h.groups[0]));
        assert_eq!(
            restarted_groups
                .iter()
                .map(|group| group.id)
                .take(2)
                .collect::<Vec<_>>(),
            vec![h.groups[2], h.groups[1]],
            "restart retains keyboard reorder after pointer and keyboard closes"
        );
        let active = restarted
            .desktop
            .active_group_for(h.strip)
            .expect("restart keeps an active group");
        assert!(restarted_groups.iter().any(|group| group.id == active));
    }
}

/// Super+N focuses the Nth window group of the current strip, on the tab it
/// last showed; a number past the end of the strip changes nothing.
#[test]
fn super_number_jumps_to_that_window_in_the_strip() {
    let mut h = Harness::new();
    h.key(Key::Character("3".into()), ModifiersState::SUPER);
    h.tick();
    assert_eq!(h.active_window(), (h.groups[2], h.tabs[2][0]));

    h.key(Key::Character("1".into()), ModifiersState::SUPER);
    h.tick();
    assert_eq!(h.active_window(), (h.groups[0], h.tabs[0][0]));

    h.key(Key::Character("9".into()), ModifiersState::SUPER);
    h.tick();
    assert_eq!(
        h.active_window(),
        (h.groups[0], h.tabs[0][0]),
        "the strip has {GROUPS} windows; Super+9 must not move focus"
    );
}

#[test]
fn super_v_and_super_a_open_clipboard_history_and_notifications() {
    let mut h = Harness::new();
    h.key(Key::Character("v".into()), ModifiersState::SUPER);
    {
        let st = h.chrome.borrow();
        assert!(st.overlays.quick_settings);
        assert!(matches!(
            st.quick_settings.current_level(),
            shell::shell::quick_settings::QsLevel::Clipboard
        ));
    }
    // Quick Settings is modal and owns every key while open.
    h.chrome.borrow_mut().overlays.quick_settings = false;
    h.key(Key::Character("a".into()), ModifiersState::SUPER);
    let st = h.chrome.borrow();
    assert!(st.overlays.quick_settings);
    assert!(matches!(
        st.quick_settings.current_level(),
        shell::shell::quick_settings::QsLevel::Notifications
    ));
}

#[test]
fn super_f_asks_the_compositor_to_toggle_full_screen() {
    let mut h = Harness::new();
    assert!(h.chrome.borrow().fullscreen_group().is_none());
    h.key(Key::Character("f".into()), ModifiersState::SUPER);
    assert!(
        h.chrome.borrow().fullscreen_group().is_some(),
        "Super+F must set chrome group fullscreen on the key, not a later compositor wipe"
    );
    h.key(Key::Character("f".into()), ModifiersState::SUPER);
    assert!(
        h.chrome.borrow().fullscreen_group().is_none(),
        "a second Super+F restores"
    );
}

#[test]
fn ctrl_alt_delete_opens_the_power_menu() {
    let mut h = Harness::new();
    assert!(h.host.power_menu_semantic_rects().is_empty());
    h.key(
        Key::Named(NamedKey::Delete),
        ModifiersState::CONTROL | ModifiersState::ALT,
    );
    assert!(
        !h.host.power_menu_semantic_rects().is_empty(),
        "the session action sheet is open"
    );
}

#[test]
fn ctrl_alt_t_opens_a_terminal() {
    let mut h = Harness::new();
    let terminals = |h: &Harness| -> usize {
        h.chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .map_or(0, |ws| {
                ws.strips
                    .iter()
                    .flat_map(|s| &s.window_groups)
                    .flat_map(|g| &g.tabs)
                    .filter(|t| t.title == "Terminal")
                    .count()
            })
    };
    assert_eq!(terminals(&h), 0);
    h.key(
        Key::Character("t".into()),
        ModifiersState::CONTROL | ModifiersState::ALT,
    );
    assert_eq!(terminals(&h), 1, "one press opens one terminal");
}

/// Super+Alt+= and Super+Alt+- zoom the magnifier one step, Super+Alt+8
/// turns it off and back on at the zoom it had, and each change shows the
/// zoom and is saved where the frame and Settings read it.
#[test]
fn magnifier_shortcuts_step_the_zoom_show_it_and_save_it() {
    use settings_store::magnifier::{self, MagnifierState};
    use shell::shell::osd::OsdKind;

    let config = tempfile::tempdir().expect("temporary config directory");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", config.path());
    let mut h = Harness::new();
    let super_alt = ModifiersState::SUPER | ModifiersState::ALT;
    let shown = |h: &Harness| h.chrome.borrow().osd.map(|osd| osd.kind);
    let saved = || {
        let path = settings_store::settings_path().expect("settings path");
        MagnifierState::from_section(&settings_store::load_section_at(&path, magnifier::SECTION))
            .zoom
    };

    h.key(Key::Character("=".into()), super_alt);
    assert_eq!(shown(&h), Some(OsdKind::Magnifier { zoom: 2 }));
    h.key(Key::Character("=".into()), super_alt);
    assert_eq!(shown(&h), Some(OsdKind::Magnifier { zoom: 3 }));
    assert_eq!(saved(), 3);
    assert!(
        h.host.take_magnifier_changed(),
        "the frame is told to re-read the magnifier now"
    );

    h.key(Key::Character("-".into()), super_alt);
    assert_eq!(shown(&h), Some(OsdKind::Magnifier { zoom: 2 }));

    h.key(Key::Character("8".into()), super_alt);
    assert_eq!(shown(&h), Some(OsdKind::Magnifier { zoom: 1 }));
    assert_eq!(saved(), 1);
    h.key(Key::Character("8".into()), super_alt);
    assert_eq!(
        shown(&h),
        Some(OsdKind::Magnifier { zoom: 2 }),
        "the toggle comes back at the zoom it left"
    );
}
