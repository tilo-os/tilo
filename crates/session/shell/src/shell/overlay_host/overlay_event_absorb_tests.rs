//! Pins the modal event-absorb gate: every pointer event over an open
//! overlay must short-circuit rather than leak to the content widget
//! underneath. The test runs `host.handle_event` with a
//! [`RecordingContent`] sink.
use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::osd::review_offscreen as offscreen;
use crate::shell::state::{FreshId as _, WorkspaceTree};
use std::cell::RefCell;
use std::collections::HashMap as StdHashMap;
use std::rc::Rc;
use taffy::{NodeId, Size, Style, TaffyTree};
use ui::widget::TaffyTreeExt;
use vello::Scene;
use vello::kurbo::Affine;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

#[path = "notification_header_tests.rs"]
mod notification_header_tests;
#[path = "overlay_event_absorb_tests/picker_scroll_tests.rs"]
mod picker_scroll_tests;
#[path = "overlay_event_absorb_tests/quick_settings_controls_tests.rs"]
mod quick_settings_controls_tests;

/// Mock content widget recording every [`Widget::handle_event`] call by
/// event-variant name, so tests can assert which events reached it —
/// and that absorbed ones did not.
struct RecordingContent {
    rect: Rect,
    log: Rc<RefCell<Vec<String>>>,
}

impl RecordingContent {
    fn new(log: Rc<RefCell<Vec<String>>>) -> Self {
        Self {
            rect: Rect::ZERO,
            log,
        }
    }
}

impl Widget for RecordingContent {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut StdHashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

    fn handle_event(&mut self, event: &WindowEvent, _ctx: &mut ui::widget::EventCtx<'_>) {
        let label = match event {
            WindowEvent::CursorMoved { .. } => "CursorMoved",
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => "MouseInput::Pressed",
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => "MouseInput::Released",
            WindowEvent::MouseWheel { .. } => "MouseWheel",
            _ => "Other",
        };
        self.log.borrow_mut().push(label.to_string());
    }

    fn name(&self) -> &'static str {
        "RecordingContent"
    }
}

/// Builds a host wrapping [`RecordingContent`], laid out at a known
/// viewport; `log` is shared with the mock for post-event assertions.
struct AbsorbHarness {
    host: ShellOverlayHost,
    chrome: crate::shell::chrome_state::Chrome,
    taffy: TaffyTree<()>,
    root_node: NodeId,
    log: Rc<RefCell<Vec<String>>>,
}

/// A synchronous two-source app provider for chip-routing tests. The
/// production registry invokes it through the same `wanted` kinds that
/// reach the real Applications and Host Applications providers.
struct AppSourceFilterFixtureProvider;

impl crate::shell::search::SearchProvider for AppSourceFilterFixtureProvider {
    fn id(&self) -> &'static str {
        "app-source-filter-fixture"
    }

    fn name(&self) -> &'static str {
        "App source fixture"
    }

    fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
        &[crate::shell::search::ResourceKind::Application]
    }

    fn query(
        &self,
        context: crate::shell::search::SearchContext<'_>,
    ) -> Vec<crate::shell::search::SearchResult> {
        use crate::shell::search::{ResourceKind, SearchResult};

        let mut rows = Vec::new();
        if context.wanted.contains(&ResourceKind::Application) {
            rows.push(SearchResult::nullary(
                ResourceKind::Application,
                self.id(),
                "fixture-tilo-app",
                "Open Tilo fixture",
                None,
                100,
                |_, _, _| {},
            ));
            rows.push(
                SearchResult::nullary(
                    ResourceKind::Application,
                    self.id(),
                    "fixture-host-app",
                    "Open Host fixture",
                    None,
                    100,
                    |_, _, _| {},
                )
                .with_facts(["System".to_owned()]),
            );
        }
        rows
    }
}

fn build_harness() -> AbsorbHarness {
    build_harness_sized(1600.0, 900.0)
}

/// One full layout pass over the host, as the compositor's frame does
/// it. Separate from the constructor so a test can lay out *again*
/// after a paint — what the shell does when a draw finds the palette
/// needs more room than the last layout gave it.
fn lay_out_host(host: &mut ShellOverlayHost, vw: f64, vh: f64) -> (TaffyTree<()>, NodeId) {
    lay_out_host_with_theme(host, vw, vh, &Theme::dark())
}

/// Same compositor layout pass with the caller's production theme. This
/// keeps the visual-review fixtures honest for large-text and
/// high-contrast variants without changing their chrome state.
pub(super) fn lay_out_host_with_theme(
    host: &mut ShellOverlayHost,
    vw: f64,
    vh: f64,
    theme: &Theme,
) -> (TaffyTree<()>, NodeId) {
    let crate::shell::test_support::LaidOut { taffy, node, .. } =
        crate::shell::test_support::lay_out_in_viewport(
            host,
            ui::num::f32_of_f64(vw),
            ui::num::f32_of_f64(vh),
            theme,
        );
    (taffy, node)
}

/// The harness at an explicit viewport, for the geometry that only
/// goes wrong at one end of the range — the palette card is a
/// fraction of the viewport, so its filter band is only as tight as
/// the screen is narrow.
fn build_harness_sized(vw: f64, vh: f64) -> AbsorbHarness {
    let tree = WorkspaceTree::bootstrap_demo();
    let chrome = ChromeState::new(tree);
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let content = Box::new(RecordingContent::new(log.clone()));
    let mut host = ShellOverlayHost::new(content).with_chrome(chrome.clone());
    let (taffy, child) = lay_out_host(&mut host, vw, vh);

    AbsorbHarness {
        host,
        chrome,
        taffy,
        root_node: child,
        log,
    }
}

fn cursor_moved_at(p: Point) -> WindowEvent {
    ui::synthetic_input::cursor_moved(p.x, p.y)
}

fn touch_at(id: u64, phase: winit::event::TouchPhase, x: f64, y: f64) -> WindowEvent {
    WindowEvent::Touch(winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: PhysicalPosition::new(x, y),
        force: None,
        id,
    })
}

use ui::synthetic_input::{
    mouse_press as left_button_pressed, mouse_release, mouse_release as left_button_released,
};

fn pump(h: &mut AbsorbHarness, event: &WindowEvent) -> bool {
    pump_with_modifiers(h, event, winit::event::Modifiers::default())
}

fn pump_with_modifiers(
    h: &mut AbsorbHarness,
    event: &WindowEvent,
    modifiers: winit::event::Modifiers,
) -> bool {
    ui::testing::Dispatch::new()
        .modifiers(modifiers)
        .layout(&h.taffy, h.root_node)
        .send(&mut h.host, event)
        .handled
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn keys_and_touch_gestures_cancel_cleanly_without_terminal_actions() {
    fn first_local_key_releases_follow_without_also_running_a_shell_chord() {
        let mut h = build_harness();
        // Follow now requires a live participant. Exercise the key handoff
        // against an admitted roster entry, not an already-disconnected id.
        {
            let state = h.chrome.borrow();
            let mut tree = state.tree_mut();
            let workspace = &mut tree.desktop.workspaces[0];
            let tab = workspace.strips[0].window_groups[0].active_tab;
            workspace.collaborators.push(
                crate::shell::state::Collaborator::with_active_tab("ada", "Ada", tab)
                    .with_session(77),
            );
        }
        h.host.set_followed_peer(Some("ada".into()));
        assert!(
            h.host.followed_peer.is_some(),
            "fixture is following a live peer"
        );
        let before = h.chrome.borrow().tree().desktop.workspaces[0].strips[0].window_groups[0]
            .tabs
            .len();
        let modifiers = winit::event::Modifiers::from(winit::keyboard::ModifiersState::SUPER);

        assert!(pump_with_modifiers(
            &mut h,
            &key_pressed(Key::Character("w".into())),
            modifiers,
        ));
        assert!(h.host.followed_peer.is_none(), "the key exits Follow mode");
        assert_eq!(
            h.chrome.borrow().tree().desktop.workspaces[0].strips[0].window_groups[0]
                .tabs
                .len(),
            before,
            "the handoff key must not also close the focused tab"
        );
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "the test renders bounded monotonic millisecond timestamps through the UI's f64 chart interface"
    )]
    fn lock_and_device_removal_cancel_partial_touch_gestures_without_terminal_actions() {
        let mut h = build_harness();
        h.chrome
            .borrow_mut()
            .tree_mut()
            .create_hosted_workspace("Second");
        let active_before = h.chrome.borrow().tree().desktop.active;

        // This is the exact lock-boundary call made by native.rs after it
        // cancels the backend stream. Late End events must not turn a partial
        // vertical four-finger swipe into a workspace switch.
        for id in 0..4 {
            let _ = pump(
                &mut h,
                &touch_at(
                    id,
                    winit::event::TouchPhase::Started,
                    400.0 + id as f64 * 8.0,
                    500.0,
                ),
            );
        }
        for id in 0..4 {
            assert!(pump(
                &mut h,
                &touch_at(
                    id,
                    winit::event::TouchPhase::Moved,
                    400.0 + id as f64 * 8.0,
                    350.0
                )
            ));
        }
        h.host.cancel_direct_touch_gestures();
        for id in 0..4 {
            let _ = pump(
                &mut h,
                &touch_at(
                    id,
                    winit::event::TouchPhase::Ended,
                    400.0 + id as f64 * 8.0,
                    350.0,
                ),
            );
        }

        // Physical removal runs through the raw Cancelled phase instead of
        // the explicit lock helper. It must have the same no-terminal-action
        // property even if stale End edges arrive after unplug.
        for id in 10..14 {
            let _ = pump(
                &mut h,
                &touch_at(
                    id,
                    winit::event::TouchPhase::Started,
                    500.0 + (id - 10) as f64 * 8.0,
                    500.0,
                ),
            );
        }
        for id in 10..14 {
            assert!(pump(
                &mut h,
                &touch_at(
                    id,
                    winit::event::TouchPhase::Moved,
                    500.0 + (id - 10) as f64 * 8.0,
                    350.0
                )
            ));
        }
        for id in 10..14 {
            assert!(pump(
                &mut h,
                &touch_at(
                    id,
                    winit::event::TouchPhase::Cancelled,
                    500.0 + (id - 10) as f64 * 8.0,
                    350.0
                )
            ));
        }
        for id in 10..14 {
            let _ = pump(
                &mut h,
                &touch_at(
                    id,
                    winit::event::TouchPhase::Ended,
                    500.0 + (id - 10) as f64 * 8.0,
                    350.0,
                ),
            );
        }

        let chrome = h.chrome.borrow();
        assert_eq!(
            chrome.tree().desktop.active,
            active_before,
            "a cancelled touch stream cannot switch workspaces"
        );
        assert!(
            !chrome.overlays.overview,
            "a cancelled touch stream cannot commit overview"
        );
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "the test renders bounded contact indices as touch coordinates"
    )]
    fn touchscreen_swipes_take_strips_with_three_fingers_and_workspaces_with_four() {
        fn swipe_up(h: &mut AbsorbHarness, first_id: u64, fingers: u64) {
            let ids = first_id..first_id + fingers;
            let x = |id: u64| 400.0 + (id - first_id) as f64 * 8.0;
            for id in ids.clone() {
                let _ = pump(
                    h,
                    &touch_at(id, winit::event::TouchPhase::Started, x(id), 500.0),
                );
            }
            for id in ids.clone() {
                let _ = pump(
                    h,
                    &touch_at(id, winit::event::TouchPhase::Moved, x(id), 250.0),
                );
            }
            for id in ids {
                let _ = pump(
                    h,
                    &touch_at(id, winit::event::TouchPhase::Ended, x(id), 250.0),
                );
            }
        }
        let mut h = build_harness();
        {
            let mut chrome = h.chrome.borrow_mut();
            let mut tree = chrome.tree_mut();
            let second = tree.create_hosted_workspace("Second").0;
            tree.desktop.active = second;
            let ws = tree.desktop.active_workspace_mut().expect("a workspace");
            ws.strips.push(crate::shell::state::Strip::new(
                crate::shell::state::StripId(9_000),
                "Below".to_owned(),
            ));
            drop(tree);
            let (_, focused) = chrome.strip_focus();
            if focused == 0 {
                assert!(chrome.switch_strip(1), "fixture focuses the lower strip");
            }
        }
        let workspace = h.chrome.borrow().tree().desktop.active;
        let (_, strip) = h.chrome.borrow().strip_focus();
        assert!(strip > 0, "there is a strip above to swipe to");

        swipe_up(&mut h, 0, 3);
        {
            let chrome = h.chrome.borrow();
            assert_eq!(
                chrome.strip_focus().1,
                strip - 1,
                "three fingers up focus the strip above, as Super+↑ does"
            );
            assert_eq!(
                chrome.tree().desktop.active,
                workspace,
                "three fingers stay on the workspace"
            );
        }

        swipe_up(&mut h, 10, 4);
        assert_ne!(
            h.chrome.borrow().tree().desktop.active,
            workspace,
            "four fingers up go one level out, to the previous workspace"
        );
    }

    first_local_key_releases_follow_without_also_running_a_shell_chord();
    lock_and_device_removal_cancel_partial_touch_gestures_without_terminal_actions();
    touchscreen_swipes_take_strips_with_three_fingers_and_workspaces_with_four();
}

use ui::synthetic_input::mouse_press;

fn key_pressed(key: Key) -> WindowEvent {
    let event = ui::synthetic_input::key_event(key, None, ElementState::Pressed);
    ui::synthetic_input::keyboard_input(event)
}

fn key_released(key: Key) -> WindowEvent {
    let event = ui::synthetic_input::key_event(key, None, ElementState::Released);
    ui::synthetic_input::keyboard_input(event)
}

/// One production paint, so the chip / scope rects a click router
/// (or an assertion) reads are the ones that were actually drawn.
fn paint_scene(h: &mut AbsorbHarness, theme: &Theme) -> vello::Scene {
    let mut tr = TextRenderer::new();
    let mut scene = vello::Scene::new();
    let presence = ui::presence::PresenceCtx::default();
    h.host
        .draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    scene
}

fn paint(h: &mut AbsorbHarness) {
    let _ = paint_scene(h, &Theme::dark());
}

/// Paint, re-lay-out, paint again — the two-frame settle the shell
/// itself performs when a draw discovers the filter band needs more
/// rows than the last layout reserved.
fn settle(h: &mut AbsorbHarness, vw: f64, vh: f64) {
    paint(h);
    let (taffy, child) = lay_out_host(&mut h.host, vw, vh);
    h.taffy = taffy;
    h.root_node = child;
    paint(h);
}

fn settle_with_theme(h: &mut AbsorbHarness, vw: f64, vh: f64, theme: &Theme) {
    let _ = paint_scene(h, theme);
    let (taffy, child) = lay_out_host_with_theme(&mut h.host, vw, vh, theme);
    h.taffy = taffy;
    h.root_node = child;
    let _ = paint_scene(h, theme);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn app_search_lists_apps_under_one_kind_and_runs_the_selected_row() {
    /// Clicking a kind chip or scope switch must filter, not be mistaken
    /// for a dismissal click outside the card. Drives the production
    /// `handle_event` router against rects `draw` just cached.
    fn a_new_window_picker_lists_every_kind_of_app_under_one_apps_chip() {
        use crate::shell::search::ResourceKind;

        let mut h = build_harness();
        h.host
            .search_registry
            .register(Rc::new(AppSourceFilterFixtureProvider));
        h.host.active_kind_filter = app_kind_filter();
        h.host.set_palette_visible(true);
        h.host.force_palette_settled();
        h.host.search.set_query("Open");
        let doc = AutoCommit::new();
        h.host.refresh_search_results(&doc, &automerge::ROOT);
        settle(&mut h, 1600.0, 900.0);

        assert_eq!(h.host.active_kind_filter, vec![ResourceKind::Application]);
        let app_chips = h
            .host
            .search_kinds()
            .iter()
            .filter(|(kind, _)| *kind == ResourceKind::Application)
            .count();
        assert_eq!(app_chips, 1);
        let ids: Vec<&str> = h
            .host
            .search_results
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        assert!(ids.contains(&"fixture-tilo-app") && ids.contains(&"fixture-host-app"));
        let groups: std::collections::BTreeSet<Option<String>> = h
            .host
            .search
            .visible_entries()
            .iter()
            .map(|entry| entry.group.clone())
            .collect();
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups.first(), Some(&Some("Apps".to_owned())));
    }

    fn app_search_arrows_and_enter_activate_the_visibly_selected_row() {
        use crate::shell::search::ResourceKind;

        let mut h = build_harness();
        h.host
            .search_registry
            .register(Rc::new(AppSourceFilterFixtureProvider));
        h.host.active_kind_filter = vec![ResourceKind::Application];
        h.host.set_palette_visible(true);
        h.host.force_palette_settled();
        h.host.search.set_query("Open");
        let doc = AutoCommit::new();
        h.host.refresh_search_results(&doc, &automerge::ROOT);

        let (_, _, initially_selected, rows) = h.host.palette_snapshot();
        assert!(
            rows.len() >= 2,
            "fixture must expose both app sources: {rows:?}"
        );
        let initially_selected = initially_selected.expect("first row is visibly selected");
        assert_eq!(initially_selected, rows[0].0);

        // The launcher's ArrowDown selects the next row.
        h.host.search.set_cursor(1);
        let (_, _, selected_after_arrow, rows_after_arrow) = h.host.palette_snapshot();
        let selected_after_arrow = selected_after_arrow.expect("arrow keeps a row selected");
        assert_eq!(
            rows_after_arrow, rows,
            "navigation must not reorder search results"
        );
        assert_eq!(
            selected_after_arrow, rows[1].0,
            "ArrowDown must move the visible selection to the second app"
        );

        h.host.choose_selected_as_launcher(None);
        assert!(
            !h.host.palette_visible(),
            "Enter's activation path must complete the selected app action"
        );
        assert_ne!(
            selected_after_arrow, initially_selected,
            "activation must follow the arrow-selected row, not stale result zero"
        );
    }

    /// A search narrowed to one resource names it in the field's token, a
    /// breadcrumb Backspace leaves.
    fn backspace_leaves_a_resource_scope_named_in_the_token() {
        use crate::shell::chrome_state::SearchScope;
        let mut h = build_harness();
        h.host.set_palette_visible(true);
        h.host.force_palette_settled();
        h.chrome.borrow_mut().search_scope = SearchScope::Workspace;
        h.chrome.borrow_mut().search_resource_scope =
            Some(crate::shell::chrome_state::SearchResourceScope {
                peer_id: 7,
                cluster_id: "room:test".into(),
                app_id: "so.tilo.reader".into(),
                resource: "book:42".into(),
                label: "Book of Tilo".into(),
                kind: "ebook.book".into(),
            });
        h.host
            .refresh_open_palette(&AutoCommit::new(), &automerge::ROOT);
        assert_eq!(
            h.host.search.scope_token(),
            Some("‹ Book of Tilo"),
            "the token doubles as a visible resource breadcrumb"
        );
        // Backspace in the launcher's field takes the token away.
        h.host.ask_as_launcher(launcher_app::source::Ask::Widen);
        assert!(
            h.chrome.borrow().search_resource_scope.is_none(),
            "Backspace leaves the nested resource scope"
        );
        assert!(h.host.palette_visible(), "the palette must stay open");
    }

    a_new_window_picker_lists_every_kind_of_app_under_one_apps_chip();
    app_search_arrows_and_enter_activate_the_visibly_selected_row();
    backspace_leaves_a_resource_scope_named_in_the_token();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn open_overlays_absorb_their_input_and_leave_the_rest_to_content() {
    fn overlay_absorbs_released_inside_qs() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.quick_settings = true;
        let qs_rect = h.host.quick_settings_rect();
        assert!(qs_rect.area() > 0.0, "qs rect must be non-empty");
        let p = qs_rect.center();
        // Released doesn't carry position data, so seed `last_cursor` via
        // a CursorMoved first. Clear the log so the Released assertion is
        // unambiguous.
        pump(&mut h, &cursor_moved_at(p));
        h.log.borrow_mut().clear();

        let released = mouse_release();
        let handled = pump(&mut h, &released);

        assert!(
            handled,
            "MouseInput::Released over the open quick-settings popover must be absorbed"
        );
        let log = h.log.borrow();
        assert!(
            !log.iter().any(|s| s == "MouseInput::Released"),
            "content.handle_event must NOT see Released when QS absorbs it; log = {:?}",
            *log
        );
    }

    fn overlay_absorbs_keyboard_text_and_release_when_qs_open() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.quick_settings = true;
        h.log.borrow_mut().clear();

        assert!(pump(&mut h, &key_pressed(Key::Character("x".into()))));
        assert!(pump(&mut h, &key_released(Key::Character("x".into()))));
        let log = h.log.borrow();
        assert!(
            log.is_empty(),
            "keyboard input must not reach the focused client while QS is open; log = {:?}",
            *log
        );
    }

    fn overlay_absorbs_cursor_moved_inside_ws_settings() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.ws_settings = true;
        let ws_rect = h.host.ws_settings_rect();
        assert!(ws_rect.area() > 0.0, "ws-settings rect must be non-empty");
        let p = ws_rect.center();

        let cm = cursor_moved_at(p);
        let handled = pump(&mut h, &cm);

        assert!(
            handled,
            "CursorMoved over the open ws-settings modal must be absorbed"
        );
        let log = h.log.borrow();
        assert!(
            !log.iter().any(|s| s == "CursorMoved"),
            "content.handle_event must NOT see CursorMoved when ws-settings absorbs it; log = {:?}",
            *log
        );
    }

    fn cursor_moved_outside_open_overlay_still_reaches_content() {
        // Negative test: outside every overlay's rect the absorb gate must
        // NOT fire — the gate is bounded by rect, not "any pointer event
        // while an overlay is open".
        let mut h = build_harness();
        h.host.set_palette_visible(true);
        // A point in the top-left corner that the palette (centered modal)
        // can't possibly contain.
        let p = Point::new(8.0, 8.0);
        let cm = cursor_moved_at(p);
        let _ = pump(&mut h, &cm);

        let log = h.log.borrow();
        assert!(
            log.iter().any(|s| s == "CursorMoved"),
            "outside-overlay CursorMoved must still reach content; log = {:?}",
            *log
        );
    }

    fn ordinary_desktop_has_no_bottom_right_help_hit_target() {
        let mut h = build_harness();
        // This point was inside the persistent Help pill. On an ordinary
        // desktop it must remain available to the focused client.
        let p = Point::new(h.host.rect.x1 - 20.0, h.host.rect.y1 - 20.0);
        assert!(!h.host.cursor_over_open_overlay(p));

        let _ = pump(&mut h, &cursor_moved_at(p));
        h.log.borrow_mut().clear();
        assert!(
            !pump(&mut h, &mouse_press()),
            "a click in the old Help-pill corner must not be claimed by shell chrome"
        );
        assert!(
            h.log
                .borrow()
                .iter()
                .any(|event| event == "MouseInput::Pressed"),
            "the old Help-pill corner must reach client content"
        );
        assert!(
            !h.chrome.borrow().overlays.shortcut_help,
            "the click must not open desktop help"
        );
    }

    overlay_absorbs_released_inside_qs();
    overlay_absorbs_keyboard_text_and_release_when_qs_open();
    overlay_absorbs_cursor_moved_inside_ws_settings();
    cursor_moved_outside_open_overlay_still_reaches_content();
    ordinary_desktop_has_no_bottom_right_help_hit_target();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn quick_settings_power_tile_and_drill_ins_own_their_pointer_input() {
    fn quick_settings_power_tile_opens_the_power_menu_on_the_first_click() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.quick_settings = true;
        h.host.force_quick_settings_settled();
        paint(&mut h);
        let power = h
            .host
            .quick_settings_semantic_rects()
            .into_iter()
            .find_map(|(name, rect)| (name == "quick_settings_power").then_some(rect))
            .expect("the visible Power tile has semantic geometry");

        let _ = pump(&mut h, &cursor_moved_at(power.center()));
        assert!(pump(&mut h, &mouse_press()));
        assert!(h.host.power_menu_open());
        assert!(!h.chrome.borrow().overlays.quick_settings);
    }

    fn quick_settings_power_tile_tracks_its_entrance_animation() {
        let mut h = build_harness();
        h.chrome.borrow_mut().overlays.quick_settings = true;
        h.host.set_quick_settings_capture_openness(500);
        paint(&mut h);
        let stable_power = h
            .host
            .quick_settings_semantic_rects()
            .into_iter()
            .find_map(|(name, rect)| (name == "quick_settings_power").then_some(rect))
            .expect("the visible Power tile has semantic geometry");
        let painted_power = h
            .host
            .transform_quick_settings_point_for_test(stable_power.center(), Instant::now());

        let _ = pump(&mut h, &cursor_moved_at(painted_power));
        assert!(pump(&mut h, &mouse_press()));
        assert!(
            h.host.power_menu_open(),
            "clicking the visibly transformed Power tile must open its menu"
        );
        assert!(!h.chrome.borrow().overlays.quick_settings);
    }

    fn a_long_open_section_scrolls_the_compact_panel_by_wheel() {
        use tilo_quick_settings::{QsSection, WifiNetwork};

        let (vw, vh) = (960.0, 540.0);
        let wheel = ui::synthetic_input::mouse_wheel(0.0, -4.0);
        let mut wifi = build_harness_sized(vw, vh);
        {
            let mut state = wifi.chrome.borrow_mut();
            state.overlays.quick_settings = true;
            state.quick_settings.wifi = true;
            state.quick_settings.radios.wifi_available = true;
            state.quick_settings.radios.wifi_enabled = true;
            state.quick_settings.wifi_networks = (0..18)
                .map(|index| WifiNetwork {
                    ssid: format!("Network {index:02}"),
                    signal: 3,
                    secure: false,
                    security: os_actions::WifiSecurity::Open,
                    connected: false,
                    known: false,
                })
                .collect();
            state.quick_settings.section = Some(QsSection::Wifi);
        }
        wifi.host.force_quick_settings_settled();
        settle(&mut wifi, vw, vh);
        let names = |h: &AbsorbHarness| {
            h.host
                .quick_settings_semantic_rects()
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        };
        let last = "quick_settings_wifi_network_Network 17".to_string();
        assert!(
            !names(&wifi).contains(&last),
            "the last network starts below the compact panel"
        );
        let center = wifi.host.quick_settings_rect().center();
        assert!(pump(&mut wifi, &cursor_moved_at(center)));
        for _ in 0..6 {
            let _ = pump(&mut wifi, &wheel);
            settle(&mut wifi, vw, vh);
        }
        assert!(
            names(&wifi).contains(&last),
            "the wheel scrolls the page to it"
        );
    }

    quick_settings_power_tile_opens_the_power_menu_on_the_first_click();
    quick_settings_power_tile_tracks_its_entrance_animation();
    a_long_open_section_scrolls_the_compact_panel_by_wheel();
}

fn open_notification_history(h: &mut AbsorbHarness, count: usize, vw: f64, vh: f64) {
    {
        let mut state = h.chrome.borrow_mut();
        for index in 0..count {
            state.push_notification_full(
                tilo_desktop::notifications::Notification::new(
                    format!("Sync error {index}"),
                    format!("Detailed recovery guidance for retained notification {index}."),
                )
                .with_severity(tilo_desktop::notifications::NotificationSeverity::Error),
            );
        }
        state.overlays.quick_settings = true;
        state
            .quick_settings
            .push_level(tilo_quick_settings::QsLevel::Notifications);
        state.bump();
    }
    h.host.force_quick_settings_settled();
    settle(h, vw, vh);
    // The active toast cap moves the oldest records through the same
    // history path that real errors take, instead of seeding internals.
    assert!(
        h.chrome.borrow().notification_history().len() > 7,
        "fixture must exceed the old seven-row ceiling"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn notification_history_is_labelled_navigable_paged_and_recoverably_cleared() {
    #[allow(
        clippy::unchecked_time_subtraction,
        reason = "the deterministic notification-age fixture subtracts known earlier test instants"
    )]
    fn notification_history_labels_age_and_severity_concisely() {
        let now = Instant::now();
        let notification = tilo_desktop::notifications::Notification::new("Backup", "Failed")
            .with_severity(tilo_desktop::notifications::NotificationSeverity::Warning)
            .with_created_at(now - Duration::from_secs(3 * 60));

        assert_eq!(
            tilo_desktop::notifications::relative_time(&notification, now),
            "3 min ago"
        );
        assert_eq!(
            tilo_desktop::notifications::accessible_description(&notification, now),
            "Warning. 3 min ago. Failed"
        );
    }

    fn notification_history_wheel_reaches_older_retained_errors_on_compact_layouts() {
        for (vw, vh) in [(960.0, 540.0), (1280.0, 720.0)] {
            let mut h = build_harness_sized(vw, vh);
            open_notification_history(&mut h, 18, vw, vh);
            let visible = h.host.qs_notification_visible_rows();
            let total = h.chrome.borrow().notification_history().len();
            assert!(visible < total, "the fixture must overflow at {vw}x{vh}");
            let panel = h.host.quick_settings_rect();
            assert!(
                panel.y1 <= vh - f64::from(Theme::dark().tokens.spacing.s2),
                "panel must stay within the bottom inset"
            );

            assert!(pump(&mut h, &cursor_moved_at(panel.center())));
            let wheel = ui::synthetic_input::mouse_wheel(0.0, -2.0);
            assert!(pump(&mut h, &wheel), "notification wheel must be consumed");
            assert_eq!(
                h.host.notification_center.first_visible_row(),
                2,
                "wheel down reveals older retained errors at {vw}x{vh}"
            );
            assert!(
                !h.log.borrow().iter().any(|entry| entry == "MouseWheel"),
                "a notification-pane wheel must not scroll content underneath"
            );
        }
    }

    fn notification_history_keyboard_reaches_both_ends_and_exposes_navigation() {
        let mut h = build_harness_sized(1280.0, 720.0);
        open_notification_history(&mut h, 22, 1280.0, 720.0);
        let total = h.chrome.borrow().notification_history().len();
        let max_start = total.saturating_sub(h.host.qs_notification_visible_rows());

        assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::End))));
        assert_eq!(h.host.notification_center.first_visible_row(), max_start);
        assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Home))));
        assert_eq!(h.host.notification_center.first_visible_row(), 0);
        assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::PageDown))));
        assert_eq!(
            h.host.notification_center.first_visible_row(),
            h.host.qs_notification_visible_rows().min(max_start),
            "Page Down advances by exactly the visible card count"
        );

        paint(&mut h);
        let semantics = h.host.accessibility().expect("notification pane semantics");
        let notifications = semantics
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Notifications"))
            .expect("Quick Settings exposes the notification drill-in as a dialog");
        assert!(
            notifications
                .description
                .as_deref()
                .is_some_and(|description| description.contains("Page Up and Page Down")),
            "screen-reader instructions must name retained-history navigation"
        );
        let retained = notifications
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Retained notifications"))
            .expect("the visible notification cards must be exposed as a list");
        assert_eq!(
            retained.children.len(),
            h.host.qs_notification_visible_rows().min(total),
            "screen readers receive every currently visible title/body card"
        );
        let state = h.chrome.borrow();
        let visible = h.host.qs_notification_visible_rows();
        let start = h
            .host
            .notification_center
            .first_visible_row()
            .min(total.saturating_sub(visible));
        for (item, notification) in retained.children.iter().zip(
            state
                .notification_history()
                .iter()
                .rev()
                .skip(start)
                .take(retained.children.len()),
        ) {
            let description = item
                .description
                .as_deref()
                .expect("every retained card exposes its hierarchy");
            assert!(
                description.starts_with("Error. ")
                    && description.contains("Detailed recovery guidance")
                    && description.contains(&notification.body),
                "retained cards must expose the same severity, age, source, and body hierarchy as the visual rows; got {description:?}"
            );
        }
    }

    fn notification_history_back_control_stays_wide_and_clickable_with_large_text() {
        let (width, height) = (960.0, 540.0);
        let mut h = build_harness_sized(width, height);
        open_notification_history(&mut h, 18, width, height);
        let theme = Theme::dark().with_text_scale(1.3);
        settle_with_theme(&mut h, width, height, &theme);
        let back = h.host.qs_notification_back_rect();
        let clear = h.host.qs_notification_clear_rect();
        assert!(back.width() >= 64.0, "Back needs a textual tap target");
        assert!(
            back.intersect(clear).area() <= 0.0,
            "Back and Clear must remain distinct at large text"
        );
        // The production click router uses this exact drill-in handler after
        // it has classified the current Quick Settings level.
        h.host.handle_shell_quick_settings_drill_in_click(
            back.center(),
            &tilo_quick_settings::QsLevel::Notifications,
        );
        assert!(matches!(
            h.chrome.borrow().quick_settings.current_level(),
            tilo_quick_settings::QsLevel::Root
        ));
    }

    /// The notification page sizes itself and counts its rows by one rule:
    /// the pager band exists only when there is something to page to. A
    /// history that fits ends on the panel padding and shows every record;
    /// one that overflows gives the pager its room and shows exactly the
    /// rows above it.
    fn notification_history_reserves_the_pager_only_when_it_overflows() {
        let theme = Theme::dark();
        let type_px = tilo_desktop::notifications::CenterLayout::type_lines(&theme);
        let fits = tilo_desktop::notifications::CenterLayout::preferred_height(3, 3, type_px);
        let short = tilo_desktop::notifications::CenterLayout::new(
            Rect::new(0.0, 0.0, 400.0, fits),
            type_px,
        );
        assert_eq!(short.visible_rows(3), 3, "every record fits, none is paged");
        assert!(
            (short.panel().y1 - short.row(2).y1 - qs::PANEL_PAD).abs() < 1e-6,
            "no pager band under a history that fits"
        );
        let overflows = tilo_desktop::notifications::CenterLayout::preferred_height(7, 18, type_px);
        let long = tilo_desktop::notifications::CenterLayout::new(
            Rect::new(0.0, 0.0, 400.0, overflows),
            type_px,
        );
        assert_eq!(
            long.visible_rows(18),
            7,
            "the seven sized-for rows are the visible ones"
        );
        assert!(
            long.row(6).y1 + qs::RELATED_GAP <= long.footer().y0,
            "the last visible row clears the pager band"
        );
        assert!(
            overflows - fits > 4.0 * long.row_h(),
            "the overflow page is taller by its extra rows and the pager"
        );
    }

    fn notification_history_clear_all_is_recoverable_by_pointer_keyboard_and_accessibility() {
        let (width, height) = (960.0, 540.0);
        let mut h = build_harness_sized(width, height);
        open_notification_history(&mut h, 18, width, height);
        // Retire the still-visible capped toasts before clearing history. A
        // later arrival can then contribute exactly one new history record,
        // rather than first evicting an unrelated live toast as well.
        let live_ids: Vec<_> = h
            .chrome
            .borrow()
            .notifications()
            .iter()
            .map(|notification| notification.id)
            .collect();
        for id in live_ids {
            h.chrome.borrow_mut().dismiss_notification(id);
        }
        let original: Vec<_> = h
            .chrome
            .borrow()
            .notification_history()
            .iter()
            .map(|notification| notification.id)
            .collect();

        assert!(h.host.focus_synthetic_accessibility(&[0, 0, 1]));
        assert!(
            h.host
                .handle_qs_notification_keyboard(&Key::Named(NamedKey::Enter))
        );
        assert!(h.chrome.borrow().notification_history().is_empty());
        assert!(h.chrome.borrow().can_undo_notification_history_clear());
        let arrived = h
            .chrome
            .borrow_mut()
            .push_notification("Arrived during undo", "Still visible");
        h.chrome.borrow_mut().dismiss_notification(arrived);
        let during_undo: Vec<_> = h
            .chrome
            .borrow()
            .notification_history()
            .iter()
            .map(|notification| notification.id)
            .collect();
        assert_eq!(during_undo.last(), Some(&arrived));
        assert!(
            h.host
                .qs_notification_undo_rect()
                .intersect(h.host.qs_notification_back_rect())
                .area()
                <= 0.0
        );

        let semantics = h.host.accessibility().expect("notification pane semantics");
        let dialog = semantics
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Notifications"))
            .expect("notification dialog");
        assert!(dialog.children.iter().any(|node| {
            node.name.as_deref() == Some("Notifications cleared") && node.live.is_some()
        }));
        assert!(
            dialog
                .children
                .iter()
                .any(|node| node.name.as_deref() == Some("Undo clear notifications"))
        );

        assert_eq!(
            h.host.synthetic_accessibility_rect(&[0, 0, 3]),
            Some(h.host.qs_notification_undo_rect())
        );
        assert!(h.host.focus_synthetic_accessibility(&[0, 0, 3]));
        assert!(
            h.host
                .handle_qs_notification_keyboard(&Key::Named(NamedKey::Enter))
        );
        let restored: Vec<_> = h
            .chrome
            .borrow()
            .notification_history()
            .iter()
            .map(|notification| notification.id)
            .collect();
        assert_eq!(&restored[..original.len()], original);
        assert_eq!(&restored[original.len()..], during_undo);
    }

    notification_history_labels_age_and_severity_concisely();
    notification_history_wheel_reaches_older_retained_errors_on_compact_layouts();
    notification_history_keyboard_reaches_both_ends_and_exposes_navigation();
    notification_history_back_control_stays_wide_and_clickable_with_large_text();
    notification_history_reserves_the_pager_only_when_it_overflows();
    notification_history_clear_all_is_recoverable_by_pointer_keyboard_and_accessibility();
}

/// Keep visual review on the exact production draw path at 1:1 pixels.
/// This is intentionally opt-in because it needs a wgpu adapter and
/// creates review PNGs, but its fixture has more than the former
/// seven-card ceiling and exercises the same status/footer treatment as
/// the normal event tests above.
#[test]
#[ignore = "visual review: needs an offscreen wgpu adapter and writes PNGs"]
fn render_notification_history_accessibility_matrix_at_1x() {
    use std::path::PathBuf;
    use ui::offscreen::OffscreenRenderer;
    use vello::peniko::Color;

    let root = std::env::var_os("TILO_NOTIFICATION_HISTORY_SHOT_DIR").map_or_else(
        || std::env::temp_dir().join("tilo-notification-history-shots"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&root).expect("create notification-history screenshot directory");

    for (layout, width, height, theme) in [
        ("960x540-normal", 960_u32, 540_u32, Theme::dark()),
        (
            "960x540-large-text",
            960_u32,
            540_u32,
            Theme::dark().with_text_scale(1.3),
        ),
        (
            "1280x720-high-contrast",
            1280_u32,
            720_u32,
            Theme::high_contrast().with_ui_scale(1.25),
        ),
    ] {
        let mut h = build_harness_sized(f64::from(width), f64::from(height));
        open_notification_history(&mut h, 18, f64::from(width), f64::from(height));
        settle_with_theme(&mut h, f64::from(width), f64::from(height), &theme);
        let panel = h.host.quick_settings_rect();
        let visible = h.host.qs_notification_visible_rows();
        let total = h.chrome.borrow().notification_history().len();
        let max_start = total.saturating_sub(h.host.qs_notification_visible_rows());
        assert!(visible < total);
        assert!(
            panel.y1 <= f64::from(height) - 16.0,
            "{layout}: panel must stay above the desktop bottom inset: {panel:?}"
        );
        let rows = h.host.qs_notification_layout();
        assert!(
            rows.row(visible.saturating_sub(1)).y1 + qs::RELATED_GAP <= rows.footer().y0,
            "{layout}: cards must leave the visible navigation footer clear"
        );
        for (position, start) in [
            ("newest", 0),
            ("middle", max_start / 2),
            ("oldest", max_start),
        ] {
            h.host.notification_center.set_first_visible_row(start);
            let scene = paint_scene(&mut h, &theme);
            let mut renderer = OffscreenRenderer::new(width, height);
            renderer.render_png(
                &scene,
                Color::BLACK,
                root.join(format!("{layout}-{position}.png")),
            );
        }
        h.chrome.borrow_mut().clear_notification_history();
        let scene = paint_scene(&mut h, &theme);
        let mut renderer = OffscreenRenderer::new(width, height);
        renderer.render_png(
            &scene,
            Color::BLACK,
            root.join(format!("{layout}-after-clear.png")),
        );
    }
    println!("notification-history review PNGs: {}", root.display());
}

/// S1 review fixtures (1× polish, 2026-09-17): the Quick Settings pages
/// the ux-bench cannot reach by a named click on macOS — the Wi-Fi,
/// Bluetooth and VPN drill-ins and the notification history — plus the
/// shortcut guide, the room overview and the palette, photographed from
/// the production draw path at 1× on the dark and light presets. The
/// pixels are asserted, not just the files: a drill-in row's interior
/// is the Control floor and the panel padding beside it is the Panel
/// ground, so a row is a surface on the panel on both presets (HIG
/// §9.5); the same holds for a notification-history row.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "an end-to-end test scenario that reads top to bottom"
)]
fn s1_review_fixtures_are_written_in_dark_and_light() {
    use tilo_quick_settings::{BtDevice, QsSection, WifiNetwork};
    use ui::offscreen::OffscreenRenderer;

    fn net(ssid: &str, signal: u8, secure: bool, connected: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.into(),
            signal,
            secure,
            security: if secure {
                os_actions::WifiSecurity::Wpa2
            } else {
                os_actions::WifiSecurity::Open
            },
            connected,
            known: connected,
        }
    }
    fn vpn(name: &str, active: bool) -> os_actions::VpnConnectionInfo {
        os_actions::VpnConnectionInfo {
            name: name.into(),
            uuid: format!("s1-{name}"),
            active,
            kind: "wireguard".into(),
        }
    }
    type AbsorbScene = (&'static str, fn(&mut AbsorbHarness));
    fn quick_settings_open(h: &AbsorbHarness, section: QsSection) {
        let mut st = h.chrome.borrow_mut();
        st.overlays.quick_settings = true;
        st.quick_settings.section = Some(section);
        st.bump();
    }
    let scenes: [AbsorbScene; 5] = [
        ("qs-wifi", |h| {
            {
                let mut st = h.chrome.borrow_mut();
                st.quick_settings.radios.wifi_available = true;
                st.quick_settings.wifi = true;
                st.quick_settings.wifi_networks = vec![
                    net("Home Fibre", 4, true, true),
                    net(
                        "Coffee shop guest network with a very long name",
                        3,
                        false,
                        false,
                    ),
                    net("Neighbour", 1, true, false),
                ];
            }
            quick_settings_open(h, QsSection::Wifi);
        }),
        ("qs-bt", |h| {
            {
                let mut st = h.chrome.borrow_mut();
                st.quick_settings.bluetooth_available = true;
                st.quick_settings.bluetooth = true;
                st.quick_settings.bt_devices = vec![
                    BtDevice {
                        address: "00:11:22:33:44:55".into(),
                        name: "Headphones".into(),
                        battery: 73,
                        connected: true,
                    },
                    BtDevice {
                        address: "00:11:22:33:44:66".into(),
                        name: "Keyboard".into(),
                        battery: 15,
                        connected: false,
                    },
                ];
            }
            quick_settings_open(h, QsSection::Bluetooth);
        }),
        ("qs-vpn-long-labels", |h| {
            {
                let mut st = h.chrome.borrow_mut();
                st.quick_settings.vpns = vec![
                    vpn("Corporate WireGuard tunnel for the Berlin office", true),
                    vpn("Home", false),
                ];
            }
            quick_settings_open(h, QsSection::Vpn);
        }),
        ("shortcut-guide", |h| {
            let mut st = h.chrome.borrow_mut();
            st.overlays.shortcut_help = true;
            st.bump();
        }),
        ("overview", |h| {
            {
                let mut st = h.chrome.borrow_mut();
                st.overlays.overview = true;
                st.bump();
            }
            h.host.force_overview_settled(true);
        }),
    ];

    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ux1x/s1/fixtures");
    let (w, h) = (1280_u32, 800_u32);
    let (vw, vh) = (f64::from(w), f64::from(h));
    let mut renderer = OffscreenRenderer::new(w, h);
    let sample = |rgba: &[u8], p: Point| -> [u8; 3] {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "probe points are inside a positive viewport"
        )]
        let i = ((p.y as u32 * w + p.x as u32) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    };
    let expect = |rgba: &[u8], p: Point, c: Color, what: &str| {
        let got = sample(rgba, p);
        let e = c.to_rgba8();
        assert!(
            got[0].abs_diff(e.r) <= 2 && got[1].abs_diff(e.g) <= 2 && got[2].abs_diff(e.b) <= 2,
            "{what} at {p:?}: got {got:?} expected ({},{},{})",
            e.r,
            e.g,
            e.b
        );
    };
    for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let dir = out.join(theme_name);
        std::fs::create_dir_all(&dir).expect("fixture directory");
        let ground = theme.tokens.colors.surfaces.base;
        let floor = qs::control_fill(&theme, false);
        let panel_ground = ui::chrome::shell::panel_ground(&theme);
        for (name, arrange) in scenes {
            let mut harness = build_harness_sized(vw, vh);
            arrange(&mut harness);
            harness.host.force_quick_settings_settled();
            settle_with_theme(&mut harness, vw, vh, &theme);
            let scene = paint_scene(&mut harness, &theme);
            let path = dir.join(format!("{name}.png"));
            renderer.render_png(&scene, ground, &path);
            assert!(path.is_file(), "fixture missing: {}", path.display());
            if name.starts_with("qs-") {
                // A section's resting row lies on the panel's own ground,
                // as the panel padding beside it does: no box inside the
                // panel.
                let panel = harness.host.quick_settings_rect();
                let row = harness
                    .host
                    .quick_settings_target("quick_settings_section_settings")
                    .expect("the open section's last row");
                let rgba = renderer.render_rgba_resilient(&scene);
                expect(
                    &rgba,
                    Point::new(row.x0 + row.width() * 0.55, row.y0 + 3.0),
                    panel_ground,
                    &format!("{theme_name}/{name}: a section row lies on the panel's ground"),
                );
                expect(
                    &rgba,
                    Point::new(panel.x0 + qs::PANEL_PAD / 2.0, row.center().y),
                    panel_ground,
                    &format!("{theme_name}/{name}: the padding beside a row is the Panel ground"),
                );
            }
        }
        let mut harness = build_harness_sized(vw, vh);
        open_notification_history(&mut harness, 18, vw, vh);
        settle_with_theme(&mut harness, vw, vh, &theme);
        let scene = paint_scene(&mut harness, &theme);
        let path = dir.join("qs-notifications.png");
        renderer.render_png(&scene, ground, &path);
        assert!(path.is_file(), "fixture missing: {}", path.display());
        let layout = harness.host.qs_notification_layout();
        let row = layout.row(0);
        let rgba = renderer.render_rgba_resilient(&scene);
        expect(
            &rgba,
            Point::new(row.x0 + row.width() * 0.55, row.y0 + 3.0),
            floor,
            &format!("{theme_name}: a history row's interior is the Control floor"),
        );
        expect(
            &rgba,
            Point::new(layout.panel().x0 + qs::PANEL_PAD / 2.0, row.center().y),
            panel_ground,
            &format!("{theme_name}: the padding beside a history row is the Panel ground"),
        );
        // A history that fits: every record shown, no pager, the page
        // ends on the panel padding under the last row (no blank band).
        let mut harness = build_harness_sized(vw, vh);
        {
            let mut st = harness.chrome.borrow_mut();
            for index in 0..3 {
                let id = st.push_notification_full(tilo_desktop::notifications::Notification::new(
                    format!("Backup {index} finished"),
                    format!("Snapshot {index} is on the workspace host."),
                ));
                st.dismiss_notification(id);
            }
            assert_eq!(st.notification_history().len(), 3);
            st.overlays.quick_settings = true;
            st.quick_settings
                .push_level(tilo_quick_settings::QsLevel::Notifications);
            st.bump();
        }
        harness.host.force_quick_settings_settled();
        settle_with_theme(&mut harness, vw, vh, &theme);
        let scene = paint_scene(&mut harness, &theme);
        let path = dir.join("qs-notifications-short.png");
        renderer.render_png(&scene, ground, &path);
        assert!(path.is_file(), "fixture missing: {}", path.display());
        let layout = harness.host.qs_notification_layout();
        assert_eq!(harness.host.qs_notification_visible_rows(), 3);
        let last = {
            let chrome = harness
                .host
                .chrome
                .as_ref()
                .expect("the harness has chrome");
            harness
                .host
                .notification_center
                .semantic_rects(&ChromeNotifications::new(chrome), &layout, "")
                .into_iter()
                .find(|(name, _)| name == "notification_row[2]")
                .expect("the last row shows")
                .1
        };
        assert!(
            (layout.panel().y1 - last.y1 - qs::PANEL_PAD).abs() < 1e-6,
            "{theme_name}: a history that fits ends on the panel padding, not a pager band: {:?}",
            layout.panel()
        );
    }
}

#[test]
fn workspace_lens_and_shortcut_help_adapt_between_wide_and_compact_outputs() {
    fn workspace_lens_leaves_wide_workspace_visible_and_interactive() {
        let mut h = build_harness_sized(1600.0, 900.0);
        h.chrome
            .borrow_mut()
            .open_workspace_settings(WsTab::General);
        let lens = h.host.ws_settings_rect();
        assert_eq!(lens.x0, 24.0, "lens docks to the left inset");
        assert!(lens.x1 < 900.0, "workspace remains visible beside {lens:?}");

        let workspace_point = Point::new(1000.0, 300.0);
        assert!(!lens.contains(workspace_point));
        pump(&mut h, &cursor_moved_at(workspace_point));
        h.log.borrow_mut().clear();
        assert!(!pump(&mut h, &mouse_press()));
        assert!(
            h.log
                .borrow()
                .iter()
                .any(|event| event == "MouseInput::Pressed")
        );
        assert!(h.chrome.borrow().overlays.ws_settings);
    }

    fn workspace_lens_promotes_to_one_inset_sheet_on_narrow_outputs() {
        let h = build_harness_sized(600.0, 700.0);
        let lens = h.host.ws_settings_rect();
        assert_eq!(lens.x0, 24.0);
        assert_eq!(lens.x1, 576.0);
        assert_eq!(lens.y1, 676.0);
    }

    workspace_lens_leaves_wide_workspace_visible_and_interactive();
    workspace_lens_promotes_to_one_inset_sheet_on_narrow_outputs();
}

/// The window sizes every overlay is checked at: a phone-class portrait
/// window, the small landscape ones, a laptop, and the user's 2944x1840
/// panel at scale 2 (1472x920 logical).
const OVERLAY_SIZES: [(f64, f64); 6] = [
    (360.0, 640.0),
    (640.0, 400.0),
    (800.0, 600.0),
    (900.0, 520.0),
    (1280.0, 800.0),
    (1472.0, 920.0),
];

/// One shell overlay under test: its name, how to open it on a harness, and
/// the rect it reports for itself once laid out and painted.
struct OverlayCase {
    name: &'static str,
    open: fn(&mut AbsorbHarness),
    rect: fn(&AbsorbHarness) -> Rect,
    /// A modal dims the whole window behind it with the scrim token.
    modal: bool,
}

fn overlay_cases() -> [OverlayCase; 5] {
    [
        OverlayCase {
            name: "shortcut-guide",
            open: |h| {
                let mut st = h.chrome.borrow_mut();
                st.overlays.shortcut_help = true;
                st.bump();
            },
            rect: |h| h.host.layers.shortcut_guide.area(),
            modal: true,
        },
        OverlayCase {
            name: "quick-settings",
            open: |h| {
                {
                    let mut st = h.chrome.borrow_mut();
                    st.overlays.quick_settings = true;
                    st.bump();
                }
                h.host.force_quick_settings_settled();
            },
            rect: |h| h.host.quick_settings_rect(),
            modal: false,
        },
        OverlayCase {
            name: "workspace-lens",
            open: |h| {
                h.chrome
                    .borrow_mut()
                    .open_workspace_settings(WsTab::General);
                h.host.force_ws_settings_settled();
            },
            rect: |h| h.host.ws_settings_rect(),
            modal: true,
        },
        OverlayCase {
            name: "people",
            open: |h| {
                let mut st = h.chrome.borrow_mut();
                st.overlays.collaborators = true;
                st.bump();
            },
            rect: |h| h.host.collaborators_overlay.panel_rect(),
            modal: false,
        },
        OverlayCase {
            name: "power-menu",
            open: |h| h.host.force_power_menu_for_visual_test(),
            rect: |h| {
                h.host
                    .power_menu_semantic_rects()
                    .into_iter()
                    .map(|(_, rect)| rect)
                    .reduce(|all, rect| all.union(rect))
                    .unwrap_or(Rect::ZERO)
            },
            modal: false,
        },
    ]
}

fn assert_inside(what: &str, rect: Rect, window: Rect) {
    assert!(
        rect.area() > 0.0
            && rect.x0 >= window.x0
            && rect.x1 <= window.x1
            && rect.y0 >= window.y0
            && rect.y1 <= window.y1,
        "{what} {rect:?} leaves {window:?}"
    );
}

/// Every anchored shell overlay stays inside the window at every size from a
/// phone-class window to the desktop: the regression net for placement that
/// runs off a short or narrow window.
#[test]
fn anchored_overlays_stay_inside_the_window_at_every_size() {
    for (vw, vh) in OVERLAY_SIZES {
        let window = Rect::new(0.0, 0.0, vw, vh);
        for case in overlay_cases() {
            let mut h = build_harness_sized(vw, vh);
            (case.open)(&mut h);
            settle(&mut h, vw, vh);
            assert_inside(
                &format!("{} at {vw}x{vh}", case.name),
                (case.rect)(&h),
                window,
            );
        }
        // The window menu opens at the pointer; from each corner it flips
        // and shifts back inside the window.
        for at in [
            Point::new(vw - 2.0, vh - 2.0),
            Point::new(2.0, vh - 2.0),
            Point::new(vw - 2.0, 2.0),
        ] {
            let mut menu = super::tab_menu::TabMenuLayer::default();
            menu.open(
                super::tab_menu::MenuSubject::Window(super::tab_menu::TabMenuTarget {
                    strip: crate::shell::state::StripId::new(),
                    group: 7001,
                    tab: 0xa1,
                }),
                crate::shell::window_menu::window_menu_rows(
                    &crate::shell::window_menu::WindowMenuFacts {
                        tab_count: 3,
                        ..Default::default()
                    },
                    &settings_store::hardware::ShortcutConfig::default(),
                ),
                at,
                "Files".into(),
                window,
            );
            let rect = menu.rect().expect("the window menu is open");
            assert_inside(&format!("window menu opened at {at:?}"), rect, window);
        }
    }
}

/// Rendered, every overlay's ink stays inside its own panel and the window:
/// no label, row or button runs past the panel it belongs to. Drawn at the
/// user's 2944x1840 panel at scale 2 and at small windows, the way the
/// compositor scales the shell's scene for a `HiDPI` output. The ink is what
/// opening the overlay changed, at a contrast no shadow or scrim reaches.
/// Set `TILO_OVERLAY_SHOT_DIR` to keep the renders.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one size-by-overlay render matrix that reads top to bottom"
)]
fn overlays_render_inside_the_window_at_the_users_scale_and_small_sizes() {
    use crate::shell::osd::review_offscreen::OffscreenRenderer;
    /// A channel step only ink makes: text and controls on the panel, not
    /// the floating shadow or a modal's scrim over the empty desktop.
    const INK: u8 = 96;
    let shots = std::env::var_os("TILO_OVERLAY_SHOT_DIR").map(std::path::PathBuf::from);
    if let Some(dir) = &shots {
        std::fs::create_dir_all(dir).expect("overlay shot directory");
    }
    let runs = [
        ((1472.0, 920.0), 2.0, Theme::dark()),
        ((1472.0, 920.0), 2.0, Theme::light()),
        ((800.0, 600.0), 1.0, Theme::dark()),
        ((640.0, 400.0), 1.0, Theme::dark()),
    ];
    for ((vw, vh), scale, theme) in runs {
        let light = theme.tokens.colors.surfaces.base.to_rgba8().r > 128;
        let (pw, ph) = (
            u32::try_from(ui::num::usize_of_f64(vw * scale)).expect("width"),
            u32::try_from(ui::num::usize_of_f64(vh * scale)).expect("height"),
        );
        let mut renderer = OffscreenRenderer::new(pw, ph);
        let ground = theme.tokens.colors.surfaces.base;
        let ground_rgb = {
            let c = ground.to_rgba8();
            [c.r, c.g, c.b]
        };
        let scaled = |h: &mut AbsorbHarness| {
            let mut scaled = vello::Scene::new();
            scaled.append(&paint_scene(h, &theme), Some(Affine::scale(scale)));
            scaled
        };
        for case in overlay_cases() {
            // A modal's scrim over a light desktop is itself a large step;
            // the dark pass covers the modals.
            if light && case.modal {
                continue;
            }
            let mut h = build_harness_sized(vw, vh);
            settle_with_theme(&mut h, vw, vh, &theme);
            let closed = renderer.render_resilient(&scaled(&mut h), ground);
            (case.open)(&mut h);
            settle_with_theme(&mut h, vw, vh, &theme);
            let open_scene = scaled(&mut h);
            let open = renderer.render_resilient(&open_scene, ground);
            let panel = (case.rect)(&h);
            if let Some(dir) = &shots {
                renderer.render_png(
                    &open_scene,
                    ground,
                    dir.join(format!(
                        "{}-{}x{}@{scale}-{}.png",
                        case.name,
                        vw,
                        vh,
                        if light { "light" } else { "dark" }
                    )),
                );
            }
            let width = usize::try_from(pw).expect("width");
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0_usize, 0_usize);
            for (i, (a, b)) in closed.chunks_exact(4).zip(open.chunks_exact(4)).enumerate() {
                // Changed by opening, and not merely the desktop covered by
                // the panel's shadow: ink stands off the ground as well.
                let ink = a[..3]
                    .iter()
                    .zip(&b[..3])
                    .any(|(a, b)| a.abs_diff(*b) > INK)
                    && b[..3]
                        .iter()
                        .zip(ground_rgb)
                        .any(|(b, g)| b.abs_diff(g) > INK);
                if ink {
                    let (x, y) = (
                        i.checked_rem(width).expect("a non-empty row"),
                        i.checked_div(width).expect("a non-empty row"),
                    );
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x.saturating_add(1));
                    y1 = y1.max(y.saturating_add(1));
                }
            }
            let what = format!(
                "{} at {vw}x{vh}@{scale} ({})",
                case.name,
                if light { "light" } else { "dark" }
            );
            assert!(x1 > x0 && y1 > y0, "{what} painted no ink");
            let ink = Rect::new(
                f64::from(u32::try_from(x0).expect("x0")),
                f64::from(u32::try_from(y0).expect("y0")),
                f64::from(u32::try_from(x1).expect("x1")),
                f64::from(u32::try_from(y1).expect("y1")),
            );
            let allowed = Affine::scale(scale)
                .transform_rect_bbox(panel)
                .inflate(2.0, 2.0)
                .intersect(Rect::new(0.0, 0.0, f64::from(pw), f64::from(ph)));
            assert!(
                ink.x0 >= allowed.x0
                    && ink.y0 >= allowed.y0
                    && ink.x1 <= allowed.x1
                    && ink.y1 <= allowed.y1,
                "{what}: ink {ink:?} spills outside its panel {allowed:?}"
            );
        }
    }
}

#[test]
fn notification_reader_native_keys_scroll_back_cancel_and_pointer_are_modal() {
    let (width, height) = (960.0, 540.0);
    let mut h = build_harness_sized(width, height);
    open_notification_history(&mut h, 18, width, height);
    let id = h.chrome.borrow_mut().push_notification_full(
        tilo_desktop::notifications::Notification::new(
            "Recovery instructions",
            "Your document is safe. Reopen Search and try the application again.\n".repeat(30),
        ),
    );
    h.chrome.borrow_mut().dismiss_notification(id);
    settle(&mut h, width, height);
    let original = h.chrome.borrow().notification_history().len();
    let live_before: Vec<_> = h
        .chrome
        .borrow()
        .notifications()
        .iter()
        .map(|n| n.id)
        .collect();
    for key in [NamedKey::ArrowDown, NamedKey::Enter] {
        assert!(pump(&mut h, &key_pressed(Key::Named(key))));
    }
    assert!(h.host.notification_center.is_reading());
    settle(&mut h, width, height);
    let region = h
        .host
        .primary_vertical_scroll()
        .expect("reader exposes accessible scrolling");
    assert!(region.metrics.max_scroll() > 0.0);
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::End))));
    let at_end = h.host.primary_vertical_scroll().unwrap();
    assert_eq!(at_end.metrics.offset, at_end.metrics.max_scroll());
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::PageUp))));
    assert!(h.host.primary_vertical_scroll().unwrap().metrics.offset < at_end.metrics.offset);
    for key in [NamedKey::Enter, NamedKey::Delete] {
        assert!(pump(&mut h, &key_pressed(Key::Named(key))));
        assert!(h.host.notification_center.is_reading());
        assert_eq!(h.chrome.borrow().notification_history().len(), original);
    }
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(!h.host.notification_center.is_reading());
    assert!(matches!(
        h.chrome.borrow().quick_settings.current_level(),
        tilo_quick_settings::QsLevel::Notifications
    ));
    settle(&mut h, width, height);
    let read = h
        .host
        .quick_settings_semantic_rects()
        .into_iter()
        .find(|(name, _)| name == "quick_settings_notification_row[0]")
        .expect("the row stays the way into the reader after returning")
        .1;
    assert!(pump(
        &mut h,
        &ui::synthetic_input::cursor_moved(read.center().x, read.center().y)
    ));
    assert!(pump(&mut h, &mouse_press()));
    assert!(h.host.notification_center.is_reading());
    settle(&mut h, width, height);
    let back = h.host.qs_notification_back_rect();
    assert!(pump(
        &mut h,
        &ui::synthetic_input::cursor_moved(back.center().x, back.center().y)
    ));
    assert!(pump(&mut h, &mouse_press()));
    assert!(!h.host.notification_center.is_reading());
    assert_eq!(h.chrome.borrow().notification_history().len(), original);
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(matches!(
        h.chrome.borrow().quick_settings.current_level(),
        tilo_quick_settings::QsLevel::Root
    ));
    assert_eq!(
        h.chrome
            .borrow()
            .notifications()
            .iter()
            .map(|n| n.id)
            .collect::<Vec<_>>(),
        live_before,
        "covered banners never receive history presses"
    );
    assert!(
        h.log.borrow().is_empty(),
        "reader input cannot reach application content"
    );
}

#[test]
fn notification_reader_native_capture_and_resize_keep_the_last_line_reachable() {
    let out = std::env::var_os("TILO_NOTIFICATION_READER_SHOTS").map_or_else(
        || std::path::PathBuf::from("target/notification-reader-native"),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&out).unwrap();
    let variants = vec![
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
        ("large-text", Theme::dark().with_text_scale(2.0)),
        (
            "ui-text",
            Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
        ),
    ];
    for (name, theme) in variants {
        let (width, height) = (960_u32, 540_u32);
        let mut h = build_harness_sized(f64::from(width), f64::from(height));
        open_notification_history(&mut h, 18, f64::from(width), f64::from(height));
        let id = h.chrome.borrow_mut().push_notification_full(
            tilo_desktop::notifications::Notification::new(
                "Complete recovery instructions",
                format!(
                    "{}\nRecovery complete: your document is safe.",
                    "Your document is safe. Reopen Search and try the app again.\n".repeat(24)
                ),
            ),
        );
        h.chrome.borrow_mut().dismiss_notification(id);
        settle_with_theme(&mut h, f64::from(width), f64::from(height), &theme);
        for key in [NamedKey::Tab, NamedKey::Enter] {
            assert!(pump(&mut h, &key_pressed(Key::Named(key))));
        }
        assert!(h.host.notification_center.is_reading());
        settle_with_theme(&mut h, f64::from(width), f64::from(height), &theme);
        let mut renderer = offscreen::OffscreenRenderer::new(width, height);
        for state in ["reading-start", "reading-end", "back-focused", "resized"] {
            match state {
                "reading-end" => {
                    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::End))));
                }
                "back-focused" => {
                    assert!(h.host.focus_synthetic_accessibility(&[0, 0, 0]));
                }
                "resized" => {
                    settle_with_theme(&mut h, 800.0, 480.0, &theme);
                    assert!(h.host.focus_synthetic_accessibility(&[0, 0, 1]));
                    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::End))));
                }
                _ => {}
            }
            let scene = paint_scene(&mut h, &theme);
            let region = h
                .host
                .primary_vertical_scroll()
                .expect("complete reader scroll surface");
            assert!(region.metrics.offset <= region.metrics.max_scroll());
            if state == "reading-end" || state == "resized" {
                assert_eq!(region.metrics.offset, region.metrics.max_scroll());
            }
            let path = out.join(format!("{name}-{state}.png"));
            if state == "resized" {
                offscreen::OffscreenRenderer::new(800, 480).render_png(
                    &scene,
                    theme.tokens.colors.surfaces.base,
                    &path,
                );
            } else {
                renderer.render_png(&scene, theme.tokens.colors.surfaces.base, &path);
            }
        }
        assert_eq!(
            h.chrome.borrow().notification_history().last().unwrap().id,
            id
        );
    }
}

#[path = "launcher_recovery_tests.rs"]
mod launcher_recovery_tests;

#[path = "launcher_keystroke_cost_tests.rs"]
mod launcher_keystroke_cost_tests;
#[path = "launcher_ranking_tests.rs"]
mod launcher_ranking_tests;

#[path = "palette_steps_tests.rs"]
mod palette_steps_tests;

struct HoverRefreshProvider {
    revision: Rc<std::cell::Cell<u64>>,
    activated: Rc<RefCell<Vec<String>>>,
}

impl crate::shell::search::SearchProvider for HoverRefreshProvider {
    fn id(&self) -> &'static str {
        "refreshing-panes"
    }
    fn name(&self) -> &'static str {
        "Pane refresh fixture"
    }
    fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
        &[crate::shell::search::ResourceKind::Application]
    }
    fn search_revision(&self, _: &crate::shell::search::SearchContext<'_>) -> String {
        self.revision.get().to_string()
    }
    fn query(
        &self,
        _: crate::shell::search::SearchContext<'_>,
    ) -> Vec<crate::shell::search::SearchResult> {
        let ids = if self.revision.get() == 0 {
            vec!["Appearance", "Network"]
        } else {
            vec!["Bluetooth", "Appearance", "Network"]
        };
        ids.into_iter()
            .enumerate()
            .map(|(index, id)| {
                let activated = Rc::clone(&self.activated);
                crate::shell::search::SearchResult::nullary(
                    crate::shell::search::ResourceKind::Application,
                    self.id(),
                    id,
                    id,
                    None,
                    100_i32
                        .checked_sub(i32::try_from(index).unwrap())
                        .expect("three fixture rows fit the score range"),
                    move |_, _, _| activated.borrow_mut().push(id.to_owned()),
                )
            })
            .collect()
    }
}

struct HoverRefreshFixture {
    h: AbsorbHarness,
    revision: Rc<std::cell::Cell<u64>>,
    activated: Rc<RefCell<Vec<String>>>,
}

fn hover_refresh_fixture(width: f64, height: f64) -> HoverRefreshFixture {
    let revision = Rc::new(std::cell::Cell::new(0));
    let activated = Rc::new(RefCell::new(Vec::new()));
    let mut h = build_harness_sized(width, height);
    h.host
        .search_registry
        .register(Rc::new(HoverRefreshProvider {
            revision: Rc::clone(&revision),
            activated: Rc::clone(&activated),
        }));
    h.host.active_kind_filter = vec![crate::shell::search::ResourceKind::Application];
    h.host.set_palette_visible(true);
    h.host.force_palette_settled();
    h.host.search.set_query("fixture");
    h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    settle(&mut h, width, height);
    h.log.borrow_mut().clear();
    HoverRefreshFixture {
        h,
        revision,
        activated,
    }
}

#[test]
fn background_refresh_preserves_the_selected_action_identity() {
    let HoverRefreshFixture {
        mut h,
        revision,
        activated,
    } = hover_refresh_fixture(1600.0, 900.0);
    let network = h
        .host
        .search_results
        .iter()
        .position(|row| row.id == "Network")
        .expect("Network is initially published");
    // The person selects it in the launcher.
    assert!(h.host.search.select(&format!("result:{network}")));
    revision.set(1);
    h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    let selected = h
        .host
        .search
        .selected_id()
        .expect("selection survives refresh");
    let index: usize = selected.strip_prefix("result:").unwrap().parse().unwrap();
    assert_eq!(
        h.host.search_results[index].id, "Network",
        "background catalogue arrival must not replace the selected action"
    );
    h.host.choose_selected_as_launcher(None);
    assert_eq!(&*activated.borrow(), &["Network"]);
    assert!(h.log.borrow().is_empty(), "nothing reached app content");
}

// Deterministic owned-room fixture: no policy override and no ambient identity.
fn workspace_name_owner_harness(width: f64, height: f64) -> (AbsorbHarness, tempfile::TempDir) {
    let profile = tempfile::tempdir().unwrap();
    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.own_workspaces_for_test(|_| "shell-fixture".to_string());
    let inputs = crate::shell::hosted_inputs::HostedTileInputs::fixture_in(profile.path()).unwrap();
    let chrome = ChromeState::new_with_fixture_handle(tree.shared(), inputs);
    assert!(is_active_workspace_owner(&chrome.borrow()));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut host = ShellOverlayHost::new(Box::new(RecordingContent::new(log.clone())))
        .with_chrome(chrome.clone());
    let (taffy, root_node) = lay_out_host(&mut host, width, height);
    (
        AbsorbHarness {
            host,
            chrome,
            taffy,
            root_node,
            log,
        },
        profile,
    )
}

#[test]
fn workspace_name_pointer_keys_cancel_reopen_resize_and_commit() {
    let (mut h, _profile) = workspace_name_owner_harness(1600.0, 900.0);
    h.chrome
        .borrow_mut()
        .open_workspace_settings(WsTab::General);
    h.host.force_ws_settings_settled();
    settle(&mut h, 1600.0, 900.0);
    let original = h
        .chrome
        .borrow()
        .tree()
        .desktop
        .active_workspace()
        .unwrap()
        .name
        .clone();
    let point = h.host.workspace_settings_overlay.name_rect().center();
    assert!(pump(&mut h, &cursor_moved_at(point)));
    assert!(pump(&mut h, &mouse_press()));
    assert!(pump(&mut h, &mouse_release()));
    assert_eq!(
        h.chrome.borrow().editing_workspace_name.as_deref(),
        Some(original.as_str())
    );
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Home))));
    assert_eq!(h.chrome.borrow().editing_workspace_name_selection.cursor, 0);
    assert!(pump(&mut h, &key_pressed(Key::Character("prefix ".into()))));
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::End))));
    assert!(h.host.commit_into_chrome_field(" 家"));
    settle(&mut h, 600.0, 700.0);
    assert!(
        h.chrome
            .borrow()
            .editing_workspace_name
            .as_ref()
            .unwrap()
            .ends_with(" 家")
    );
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(h.chrome.borrow().editing_workspace_name.is_none());
    assert!(h.chrome.borrow().overlays.ws_settings);
    assert_eq!(
        h.chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .unwrap()
            .name,
        original
    );
    let point = h.host.workspace_settings_overlay.name_rect().center();
    assert!(pump(&mut h, &cursor_moved_at(point)));
    assert!(pump(&mut h, &mouse_press()));
    assert!(pump(&mut h, &mouse_release()));
    assert!(pump_with_modifiers(
        &mut h,
        &key_pressed(Key::Character("a".into())),
        winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL),
    ));
    assert!(pump(
        &mut h,
        &key_pressed(Key::Character("Renamed 家".into()))
    ));
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Enter))));
    assert!(h.chrome.borrow().editing_workspace_name.is_none());
    assert_eq!(
        h.chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .unwrap()
            .name,
        "Renamed 家"
    );
    let mut held_enter = key_pressed(Key::Named(NamedKey::Enter));
    if let WindowEvent::KeyboardInput { event, .. } = &mut held_enter {
        event.repeat = true;
    }
    assert!(pump(&mut h, &held_enter));
    assert!(pump(&mut h, &key_released(Key::Named(NamedKey::Enter))));
    assert_eq!(
        h.chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .unwrap()
            .name,
        "Renamed 家"
    );
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(!h.chrome.borrow().overlays.ws_settings);
    assert!(
        h.log.borrow().is_empty(),
        "modal rename input cannot reach app content"
    );
}

fn assert_workspace_control_label_pixels(
    path: &std::path::Path,
    bounds: Rect,
    clip: Rect,
    minimum_contrast: u8,
) {
    let image = image::open(path).unwrap().to_rgba8();
    let (mut low, mut high, mut observed) = (u8::MAX, 0_u8, 0_usize);
    for (x, y, pixel) in image.enumerate_pixels() {
        let point = Point::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
        if bounds.contains(point) && clip.contains(point) {
            low = low.min(pixel.0[1]);
            high = high.max(pixel.0[1]);
            observed = observed.saturating_add(1);
        }
    }
    assert!(observed > 100, "control must be visible");
    assert!(
        high.saturating_sub(low) >= minimum_contrast,
        "control label must contrast with its fill"
    );
}

#[test]
fn workspace_name_native_render_and_resize_matrix() {
    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/workspace-name-native");
    std::fs::create_dir_all(&out).unwrap();
    for (label, width, height, theme) in [
        ("dark", 1280_u32, 800_u32, Theme::dark()),
        ("light-narrow", 600, 700, Theme::light()),
        (
            "ui-text",
            1280,
            800,
            Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
        ),
    ] {
        let (mut h, _profile) = workspace_name_owner_harness(f64::from(width), f64::from(height));
        h.chrome
            .borrow_mut()
            .open_workspace_settings(WsTab::General);
        h.host.force_ws_settings_settled();
        settle_with_theme(&mut h, f64::from(width), f64::from(height), &theme);
        let point = h.host.workspace_settings_overlay.name_rect().center();
        assert!(pump(&mut h, &cursor_moved_at(point)));
        assert!(pump(&mut h, &mouse_press()));
        assert!(pump(&mut h, &mouse_release()));
        assert!(h.chrome.borrow().editing_workspace_name.is_some());
        assert!(pump_with_modifiers(
            &mut h,
            &key_pressed(Key::Character("a".into())),
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL)
        ));
        assert!(
            h.host
                .commit_into_chrome_field(&format!("Project {} final name", "W".repeat(38)))
        );
        let mut renderer = offscreen::OffscreenRenderer::new(width, height);
        for state in ["end", "selected-home"] {
            if state == "selected-home" {
                assert!(pump_with_modifiers(
                    &mut h,
                    &key_pressed(Key::Named(NamedKey::Home)),
                    winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT)
                ));
                assert_eq!(h.chrome.borrow().editing_workspace_name_selection.cursor, 0);
                assert!(
                    !h.chrome
                        .borrow()
                        .editing_workspace_name_selection
                        .is_collapsed()
                );
            }
            settle_with_theme(&mut h, f64::from(width), f64::from(height), &theme);
            let scene = paint_scene(&mut h, &theme);
            let path = out.join(format!("{label}-{state}.png"));
            renderer.render_png(&scene, vello::peniko::Color::BLACK, &path);
            let invite = h.host.workspace_settings_overlay.invite_rect();
            let inset = 8.0 * f64::from(theme.ui_scale);
            assert_workspace_control_label_pixels(
                &path,
                Rect::new(
                    invite.x0 + inset,
                    invite.y0 + inset,
                    invite.x1 - inset,
                    invite.y1 - inset,
                ),
                h.host.workspace_settings_overlay.general_body_rect(),
                80,
            );
            let panel = h.host.workspace_settings_overlay.rect();
            let mut footer_text = TextRenderer::new();
            let footer = WorkspaceSettingsOverlay::footer_hint_rect(
                panel,
                &WorkspaceSettingsOverlay::footer_hints(true),
                &mut footer_text,
                &theme,
            );
            assert!(
                h.host.workspace_settings_overlay.general_body_rect().y1
                    + f64::from(theme.tokens.spacing.s2)
                    <= footer.y0 + 0.01
            );
            assert_workspace_control_label_pixels(&path, footer, panel, 40);
            assert!(std::fs::metadata(path).unwrap().len() > 1024);
        }
        assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
        assert!(h.chrome.borrow().editing_workspace_name.is_none());
    }
}

#[test]
fn workspace_name_submit_ownership_ends_on_release_focus_loss_and_fresh_press() {
    for reset in ["release", "focus-loss", "fresh-press"] {
        let (mut h, _profile) = workspace_name_owner_harness(1280.0, 800.0);
        h.chrome
            .borrow_mut()
            .open_workspace_settings(WsTab::General);
        h.host.force_ws_settings_settled();
        settle(&mut h, 1280.0, 800.0);
        let point = h.host.workspace_settings_overlay.name_rect().center();
        assert!(pump(&mut h, &cursor_moved_at(point)));
        assert!(pump(&mut h, &mouse_press()));
        assert!(pump(&mut h, &mouse_release()));
        assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Enter))));
        let mut held = key_pressed(Key::Named(NamedKey::Enter));
        if let WindowEvent::KeyboardInput { event, .. } = &mut held {
            event.repeat = true;
        }
        assert!(pump(&mut h, &held));
        assert!(h.log.borrow().is_empty());
        match reset {
            "release" => {
                assert!(pump(&mut h, &key_released(Key::Named(NamedKey::Enter))));
            }
            "focus-loss" => {
                let _ = pump(&mut h, &WindowEvent::Focused(false));
                h.log.borrow_mut().clear();
            }
            _ => {}
        }
        assert!(!pump(&mut h, &key_pressed(Key::Named(NamedKey::Enter))));
        assert_eq!(
            h.log.borrow().as_slice(),
            ["Other"],
            "a fresh key still reaches app content beside the contextual lens"
        );
        assert!(h.chrome.borrow().overlays.ws_settings);
    }
}

#[test]
fn notification_banner_completed_click_and_interrupted_gestures() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    for interruption in ["outside", "escape", "focus", "resize", "retired"] {
        let mut h = build_harness_sized(960.0, 540.0);
        let id = h.chrome.borrow_mut().push_notification_full(
            tilo_desktop::notifications::Notification::new(
                "Backup failed",
                "Try again from Search.",
            ),
        );
        let theme = Theme::dark().with_reduce_motion(true);
        settle_with_theme(&mut h, 960.0, 540.0, &theme);
        let point = h.host.notification_banners_rect().center();
        pump(&mut h, &cursor_moved_at(point));
        assert!(pump(&mut h, &left_button_pressed()));
        assert_eq!(
            h.chrome.borrow().notifications().len(),
            1,
            "a press must not activate or retire the banner: {interruption}"
        );
        match interruption {
            "outside" => {
                pump(&mut h, &cursor_moved_at(Point::new(80.0, 400.0)));
            }
            "escape" => {
                assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
            }
            "focus" => {
                pump(&mut h, &WindowEvent::Focused(false));
            }
            "resize" => {
                settle_with_theme(&mut h, 780.0, 540.0, &theme);
            }
            "retired" => {
                h.chrome.borrow_mut().dismiss_notification(id);
            }
            _ => unreachable!(),
        }
        assert!(
            pump(&mut h, &left_button_released()),
            "the banner owns its release"
        );
        assert_eq!(
            h.chrome.borrow().notifications().len(),
            usize::from(interruption != "retired")
        );
        assert!(
            !h.log
                .borrow()
                .iter()
                .any(|event| event == "MouseInput::Released"),
            "an interrupted banner release must not reach content"
        );
        pump(&mut h, &cursor_moved_at(Point::new(80.0, 400.0)));
        h.log.borrow_mut().clear();
        pump(&mut h, &left_button_pressed());
        pump(&mut h, &left_button_released());
        assert!(
            h.log
                .borrow()
                .iter()
                .any(|event| event == "MouseInput::Released"),
            "the next ordinary desktop click is not captured"
        );
        if interruption != "retired" {
            settle_with_theme(&mut h, 780.0, 540.0, &theme);
            let point = h.host.notification_banners_rect().center();
            pump(&mut h, &cursor_moved_at(point));
            assert!(pump(&mut h, &left_button_pressed()));
            assert!(pump(&mut h, &left_button_released()));
            assert!(h.chrome.borrow().notifications().is_empty());
            assert_eq!(h.chrome.borrow().notification_history().len(), 1);
        }
    }
}

/// The Notifications chord (Super+A) moves keyboard focus into the newest
/// banner: it is held there, Escape dismisses it and gives focus back, and
/// a key the banner does not act on goes on to the window. With no banner,
/// the chord opens the centre as before.
#[test]
fn notifications_chord_focuses_the_newest_banner_and_escape_dismisses_it() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    let mut h = build_harness_sized(960.0, 540.0);
    let theme = Theme::dark().with_reduce_motion(true);
    let older = h.chrome.borrow_mut().push_notification_full(
        tilo_desktop::notifications::Notification::new("Lena shared a file", "You can edit it."),
    );
    let newest = h.chrome.borrow_mut().push_notification_full(
        tilo_desktop::notifications::Notification::new("Ada mentioned you", "Can you look?"),
    );
    settle_with_theme(&mut h, 960.0, 540.0, &theme);
    let chord = winit::event::Modifiers::from(winit::keyboard::ModifiersState::SUPER);
    assert!(pump_with_modifiers(
        &mut h,
        &key_pressed(Key::Character("a".into())),
        chord
    ));
    assert!(h.host.banner_has_keyboard());
    assert!(!h.chrome.borrow().overlays.quick_settings);
    assert!(
        h.chrome.borrow().notification_log().held(),
        "a focused banner does not age"
    );
    assert!(h.host.banner_owns_key(&Key::Named(NamedKey::Escape)));
    assert!(!h.host.banner_owns_key(&Key::Character("x".into())));
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(!h.host.banner_has_keyboard());
    let live: Vec<u64> = h
        .chrome
        .borrow()
        .notifications()
        .iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(live, vec![older], "Escape dismissed the newest banner");
    assert!(
        h.chrome
            .borrow()
            .notification_history()
            .iter()
            .any(|n| n.id == newest)
    );

    settle_with_theme(&mut h, 960.0, 540.0, &theme);
    assert!(pump_with_modifiers(
        &mut h,
        &key_pressed(Key::Character("a".into())),
        chord
    ));
    assert!(h.host.banner_has_keyboard());
    h.log.borrow_mut().clear();
    let _ = pump(&mut h, &key_pressed(Key::Character("x".into())));
    assert!(!h.host.banner_has_keyboard(), "typing on gives focus back");
    assert_eq!(h.chrome.borrow().notifications().len(), 1);

    h.chrome.borrow_mut().dismiss_notification(older);
    settle_with_theme(&mut h, 960.0, 540.0, &theme);
    assert!(pump_with_modifiers(
        &mut h,
        &key_pressed(Key::Character("a".into())),
        chord
    ));
    assert!(!h.host.banner_has_keyboard());
    assert!(
        h.chrome.borrow().overlays.quick_settings,
        "with no banner the chord opens the centre"
    );
}

fn capture_banner_pointer_state(
    h: &mut AbsorbHarness,
    theme: &Theme,
    width: u32,
    path: &std::path::Path,
) {
    let bounds = h.host.notification_banners_rect();
    let mut scene = Scene::new();
    h.host.draw_banners(
        &mut scene,
        Affine::translate((24.0 - bounds.x0, 24.0 - bounds.y0)),
        &mut TextRenderer::new(),
        theme,
        &PresenceCtx::default(),
    );
    offscreen::OffscreenRenderer::new(width, 300).render_png(
        &scene,
        theme.tokens.colors.surfaces.base,
        path,
    );
    assert!(path.is_file(), "the banner capture must exist");
}

fn banner_pointer_capture_variants() -> Vec<(&'static str, Theme, f64, u32)> {
    vec![
        ("dark", Theme::dark(), 640.0, 440),
        ("light", Theme::light(), 640.0, 440),
        ("large-text", Theme::dark().with_text_scale(2.0), 640.0, 440),
        (
            "contrast",
            Theme::high_contrast().with_text_scale(2.0),
            640.0,
            440,
        ),
        (
            "combined",
            Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
            800.0,
            440,
        ),
        ("narrow", Theme::light().with_text_scale(2.0), 320.0, 320),
    ]
}

#[test]
fn notification_banner_pointer_feedback_matches_shared_close_face() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    let out = std::env::var_os("TILO_BANNER_POINTER_EVIDENCE").map_or_else(
        || std::path::PathBuf::from("target/banner-pointer-review"),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&out).unwrap();
    let mut missing_feedback = Vec::new();
    for (name, theme, viewport, width) in banner_pointer_capture_variants() {
        // The banner's own clock stands still: rendering the captures is
        // slow on a busy machine, and an error banner left alone for its
        // dwell would leave before the press this test makes on it.
        ui::time::testing::with_monotonic_time(ui::time::now(), || {
            let theme = theme.with_reduce_motion(true);
            let mut h = build_harness_sized(viewport, 540.0);
            h.chrome.borrow_mut().push_notification_full(
                tilo_desktop::notifications::Notification::new(
                    "Backup failed",
                    "Try again from Search.",
                )
                .with_app_name("Files")
                .with_severity(tilo_desktop::notifications::NotificationSeverity::Error),
            );
            settle_with_theme(&mut h, viewport, 540.0, &theme);
            let card = h.host.notification_banners_rect();
            assert!(card.area() > 0.0);
            let hit = ui::widget::toast::paint_card(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                card,
                &ui::widget::toast::ToastCard::new(
                    "Backup failed",
                    ui::widget::toast::ToastKind::Error,
                )
                .body("Try again from Search.")
                .caption("Files")
                .closable(true),
            );
            let idle = out.join(format!("{name}-idle.png"));
            capture_banner_pointer_state(&mut h, &theme, width, &idle);
            pump(&mut h, &cursor_moved_at(hit.close_rect.center()));
            let hover = out.join(format!("{name}-hover.png"));
            capture_banner_pointer_state(&mut h, &theme, width, &hover);
            if std::fs::read(&idle).unwrap() == std::fs::read(&hover).unwrap() {
                missing_feedback.push(name);
            }
            assert!(pump(&mut h, &left_button_pressed()));
            assert_eq!(h.chrome.borrow().notifications().len(), 1);
            capture_banner_pointer_state(
                &mut h,
                &theme,
                width,
                &out.join(format!("{name}-pressed.png")),
            );
            assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
            assert!(pump(&mut h, &left_button_released()));
            assert_eq!(
                h.chrome.borrow().notifications().len(),
                1,
                "cancelled close gestures retain the error"
            );
        });
    }
    assert!(
        missing_feedback.is_empty(),
        "the actual banner pointer path must feed shared close hover: {missing_feedback:?}"
    );
}

#[test]
fn notification_banner_lost_release_does_not_capture_a_fresh_content_click() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = ui::motion::testing::settled();
    for lost_focus in [true, false] {
        let mut h = build_harness_sized(960.0, 540.0);
        h.chrome.borrow_mut().push_notification_full(
            tilo_desktop::notifications::Notification::new(
                "Backup failed",
                "Try again from Search.",
            ),
        );
        let theme = Theme::dark().with_reduce_motion(true);
        settle_with_theme(&mut h, 960.0, 540.0, &theme);
        let point = h.host.notification_banners_rect().center();
        pump(&mut h, &cursor_moved_at(point));
        assert!(pump(&mut h, &left_button_pressed()));
        if lost_focus {
            pump(&mut h, &WindowEvent::Focused(false));
            pump(&mut h, &WindowEvent::Focused(true));
        }
        pump(&mut h, &cursor_moved_at(Point::new(8.0, 400.0)));
        h.log.borrow_mut().clear();
        pump(&mut h, &left_button_pressed());
        assert!(
            h.log
                .borrow()
                .iter()
                .any(|event| event == "MouseInput::Pressed")
        );
        pump(&mut h, &left_button_released());
        assert!(
            h.log
                .borrow()
                .iter()
                .any(|event| event == "MouseInput::Released"),
            "a lost banner release cannot steal a fresh click: lost_focus={lost_focus}"
        );
        assert_eq!(h.chrome.borrow().notifications().len(), 1);
        assert!(h.chrome.borrow().notification_history().is_empty());
    }
}

fn qs_details_focused(node: &ui::a11y::AccessNode, name: &str) -> bool {
    (node.name.as_deref() == Some(name) && node.state.focused)
        || node
            .children
            .iter()
            .any(|child| qs_details_focused(child, name))
}
fn qs_details_tap(h: &mut AbsorbHarness, key: NamedKey) -> bool {
    let handled = pump(h, &key_pressed(Key::Named(key)));
    pump(h, &key_released(Key::Named(key)));
    handled
}

fn open_radio_details_in_harness(
    h: &mut AbsorbHarness,
    input: &str,
    name: &str,
    target: &str,
    action_id: &str,
) {
    match input {
        "keyboard" => {
            for _ in 0..32 {
                assert!(qs_details_tap(h, NamedKey::Tab));
                if qs_details_focused(&h.host.accessibility().unwrap(), name) {
                    break;
                }
            }
            assert!(qs_details_focused(&h.host.accessibility().unwrap(), name));
            assert!(qs_details_tap(h, NamedKey::Enter));
        }
        "accessibility" => {
            let index = h
                .host
                .qs_root
                .accessibility_snapshot(&h.chrome.borrow().quick_settings)
                .children
                .iter()
                .position(|node| node.name.as_deref() == Some(name))
                .expect("details accessibility child");
            assert!(focus_quick_settings_accessibility(&mut h.host, &[index]));
            assert!(qs_details_tap(h, NamedKey::Enter));
        }
        "semantic" => {
            let provider = crate::shell::search::providers::actions::ActionsProvider::new();
            let document = AutoCommit::new();
            let results = crate::shell::search::SearchProvider::query(
                &provider,
                crate::shell::search::SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "",
                    wanted: &[crate::shell::search::ResourceKind::Action],
                    chrome: &h.chrome.borrow(),
                    doc: &document,
                    doc_root: &automerge::ROOT,
                },
            );
            let result = results
                .iter()
                .find(|result| result.id == action_id)
                .expect("published details action");
            let mut document = AutoCommit::new();
            (result.on_open)(
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
                &h.chrome,
                &tilo_actions::ActionArgs::none(),
            )
            .unwrap();
        }
        _ => {
            let rect = h
                .host
                .quick_settings_semantic_rects()
                .into_iter()
                .find_map(|(id, rect)| (id == target).then_some(rect))
                .expect("painted details target");
            pump(h, &cursor_moved_at(rect.center()));
            assert!(pump(h, &left_button_pressed()));
            assert!(pump(h, &left_button_released()));
        }
    }
}

#[test]
fn radio_details_use_native_keyboard_pointer_and_action_routes() {
    for (name, target, action_id, section) in [
        (
            "Wi-Fi networks",
            "quick_settings_network_details",
            "quick-settings.network",
            tilo_quick_settings::QsSection::Wifi,
        ),
        (
            "Bluetooth devices",
            "quick_settings_bluetooth_details",
            "quick-settings.bluetooth",
            tilo_quick_settings::QsSection::Bluetooth,
        ),
    ] {
        for input in ["keyboard", "pointer", "accessibility", "semantic"] {
            let actions = super::qs_device_state_tests::Recorder::default();
            let mut h = build_harness_sized(1000.0, 980.0);
            h.host = h.host.with_qs_actions(Box::new(actions.clone()));
            h.host.qs_root = tilo_quick_settings::root::QuickSettingsRoot::default();
            {
                let mut state = h.chrome.borrow_mut();
                state.overlays.quick_settings = true;
                state.quick_settings = tilo_quick_settings::QuickSettingsState::fixture();
                state.quick_settings.radios.wifi_available = true;
                state.quick_settings.bluetooth_available = true;
            }
            h.host.force_quick_settings_settled();
            settle(&mut h, 1000.0, 980.0);
            open_radio_details_in_harness(&mut h, input, name, target, action_id);
            assert_eq!(
                h.chrome.borrow().quick_settings.section,
                Some(section),
                "{input}"
            );
            assert_eq!(
                h.chrome.borrow().quick_settings.current_level(),
                &tilo_quick_settings::QsLevel::Root
            );
            assert!(
                actions.calls().is_empty(),
                "opening a list must not write radio state"
            );
            // Escape closes the section before the panel.
            assert!(qs_details_tap(&mut h, NamedKey::Escape));
            assert_eq!(h.chrome.borrow().quick_settings.section, None);
            assert!(h.chrome.borrow().overlays.quick_settings);
            settle(&mut h, 780.0, 920.0);
            if input == "keyboard" {
                assert!(qs_details_tap(&mut h, NamedKey::Enter));
                assert_eq!(h.chrome.borrow().quick_settings.section, Some(section));
                assert!(qs_details_tap(&mut h, NamedKey::Escape));
            }
            assert!(qs_details_tap(&mut h, NamedKey::Escape));
            assert!(!h.chrome.borrow().overlays.quick_settings);
            assert!(
                actions.calls().is_empty(),
                "cancel and resize never toggle a device"
            );
        }
    }
}

#[test]
fn radio_details_navigation_owns_its_release_after_escape() {
    let actions = super::qs_device_state_tests::Recorder::default();
    let mut h = build_harness_sized(1000.0, 980.0);
    h.host = h.host.with_qs_actions(Box::new(actions.clone()));
    {
        let mut state = h.chrome.borrow_mut();
        state.overlays.quick_settings = true;
        state.quick_settings.radios.wifi_available = true;
    }
    h.host.force_quick_settings_settled();
    settle(&mut h, 1000.0, 980.0);
    let rect = h
        .host
        .quick_settings_semantic_rects()
        .into_iter()
        .find_map(|(id, rect)| (id == "quick_settings_network_details").then_some(rect))
        .expect("Wi-Fi details");
    pump(&mut h, &cursor_moved_at(rect.center()));
    assert!(pump(&mut h, &left_button_pressed()));
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(!h.chrome.borrow().overlays.quick_settings);
    pump(&mut h, &cursor_moved_at(Point::new(8.0, 900.0)));
    let releases = || {
        h.log
            .borrow()
            .iter()
            .filter(|name| *name == "MouseInput::Released")
            .count()
    };
    let before = releases();
    assert!(pump(&mut h, &left_button_released()));
    assert_eq!(
        h.log
            .borrow()
            .iter()
            .filter(|name| *name == "MouseInput::Released")
            .count(),
        before
    );
    assert!(actions.calls().is_empty());
    // The next ordinary desktop click remains deliverable.
    pump(&mut h, &left_button_pressed());
    pump(&mut h, &left_button_released());
    assert!(
        h.log
            .borrow()
            .iter()
            .filter(|name| *name == "MouseInput::Released")
            .count()
            > before
    );
}

#[test]
fn radio_details_lost_release_does_not_capture_a_fresh_content_click() {
    for lost_focus in [true, false] {
        for target in [
            "quick_settings_network_details",
            "quick_settings_bluetooth_details",
        ] {
            let actions = super::qs_device_state_tests::Recorder::default();
            let mut h = build_harness_sized(1000.0, 980.0);
            h.host = h.host.with_qs_actions(Box::new(actions.clone()));
            {
                let mut state = h.chrome.borrow_mut();
                state.overlays.quick_settings = true;
                state.quick_settings.radios.wifi_available = true;
                state.quick_settings.bluetooth_available = true;
            }
            h.host.force_quick_settings_settled();
            settle(&mut h, 1000.0, 980.0);
            let rect = h
                .host
                .quick_settings_semantic_rects()
                .into_iter()
                .find_map(|(id, rect)| (id == target).then_some(rect))
                .expect("radio details");
            pump(&mut h, &cursor_moved_at(rect.center()));
            assert!(pump(&mut h, &left_button_pressed()));
            if lost_focus {
                pump(&mut h, &WindowEvent::Focused(false));
                pump(&mut h, &WindowEvent::Focused(true));
            }
            assert!(qs_details_tap(&mut h, NamedKey::Escape));
            assert!(qs_details_tap(&mut h, NamedKey::Escape));
            assert!(!h.chrome.borrow().overlays.quick_settings);
            // The navigation release is deliberately absent. The next press
            // begins an independent content gesture, with or without focus loss.
            pump(&mut h, &cursor_moved_at(Point::new(8.0, 900.0)));
            h.log.borrow_mut().clear();
            pump(&mut h, &left_button_pressed());
            assert!(
                h.log
                    .borrow()
                    .iter()
                    .any(|event| event == "MouseInput::Pressed"),
                "the fresh content press is delivered: {target}, lost_focus={lost_focus}"
            );
            pump(&mut h, &left_button_released());
            assert!(
                h.log
                    .borrow()
                    .iter()
                    .any(|event| event == "MouseInput::Released"),
                "its matching release must also arrive: {target}, lost_focus={lost_focus}"
            );
            assert!(
                actions.calls().is_empty(),
                "navigation never writes a radio"
            );
        }
    }
}
