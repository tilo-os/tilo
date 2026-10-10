//! Pins the modal event-absorb gate: every pointer event over an open
//! overlay must short-circuit rather than leak to the content widget
//! underneath. The test runs `host.handle_event` with a
//! [`RecordingContent`] sink.
use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::osd::review_offscreen as offscreen;
use crate::shell::state::WorkspaceTree;
use std::cell::RefCell;
use std::collections::HashMap as StdHashMap;
use std::rc::Rc;
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use tilo_desktop::i18n::t as desktop_text;
use ui::widget::TaffyTreeExt;
use vello::Scene;
use vello::kurbo::Affine;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

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
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = ui::widget::placed(l, origin);
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

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

    fn get_rect(&self) -> Rect {
        self.rect
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

/// Test-only stand-in for the shell's directory cache.  It has the same
/// inert `SearchResult` shape the live `ActionDirectory` bridge emits, so
/// palette layout is exercised without smuggling a capability or widget
/// reference into this synchronous render harness.
struct DirectoryPaletteFixtureProvider;

impl crate::shell::search::SearchProvider for DirectoryPaletteFixtureProvider {
    fn id(&self) -> &'static str {
        "directory-fixture"
    }

    fn name(&self) -> &'static str {
        "Directory"
    }

    fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
        const KINDS: [crate::shell::search::ResourceKind; 1] =
            [crate::shell::search::ResourceKind::Action];
        &KINDS
    }

    fn available_in_active_app_scope(&self) -> bool {
        true
    }

    fn query(
        &self,
        context: crate::shell::search::SearchContext<'_>,
    ) -> Vec<crate::shell::search::SearchResult> {
        if !context.needle.is_empty() && !"open project workspace".contains(context.needle) {
            return Vec::new();
        }
        vec![crate::shell::search::SearchResult {
            kind: crate::shell::search::ResourceKind::Action,
            operation_kind: tilo_actions::OperationKind::View,
            provider_id: "directory-fixture".into(),
            id: "directory\u{1f}7\u{1f}r1\u{1f}code.project\u{1f}open".into(),
            title: "Open project workspace".into(),
            subtitle: Some("Example project · Code Editor".into()),
            badge: None,
            score: 100,
            params: Vec::new(),
            contract: Some(tilo_actions::ActionContract::view()),
            on_open: Rc::new(|_, _, _, _| Ok(())),
            on_invoke: None,
            on_begin_invoke: None,
        }]
    }
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
        const KINDS: [crate::shell::search::ResourceKind; 2] = [
            crate::shell::search::ResourceKind::Application,
            crate::shell::search::ResourceKind::HostApplication,
        ];
        &KINDS
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
        }
        if context.wanted.contains(&ResourceKind::HostApplication) {
            rows.push(SearchResult::nullary(
                ResourceKind::HostApplication,
                self.id(),
                "fixture-host-app",
                "Open Host fixture",
                Some("Not sandboxed · System".into()),
                100,
                |_, _, _| {},
            ));
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
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: StdHashMap<NodeId, *mut dyn Widget> = StdHashMap::new();
    let child = host.build_layout(&mut taffy, theme, &mut node_map);
    let viewport = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(f64_to_f32_lossy(vw)),
                    height: Dimension::length(f64_to_f32_lossy(vh)),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            viewport,
            Size {
                width: AvailableSpace::Definite(f64_to_f32_lossy(vw)),
                height: AvailableSpace::Definite(f64_to_f32_lossy(vh)),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);
    (taffy, child)
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

/// Render a live-faithful directory action through the normal registry,
/// palette refresh, layout, and draw paths.  This is deliberately not a
/// direct `CommandPalette::set_entries` shortcut: it covers the title,
/// subtitle, grouping, focus, and narrow-card geometry used by the shell.
fn directory_palette_fixture(vw: f64, vh: f64) -> AbsorbHarness {
    let mut harness = build_harness_sized(vw, vh);
    harness
        .host
        .search_registry
        .register(Rc::new(DirectoryPaletteFixtureProvider));
    harness.host.set_palette_visible(true);
    harness.host.set_palette_focus(true);
    harness.host.force_palette_settled();
    // Deliberately specific: the result under visual review is a live
    // directory action, not an incidental built-in search result that
    // happened to share the word "project".
    harness.host.palette.set_query("open project workspace");
    let mut doc = AutoCommit::new();
    harness
        .host
        .refresh_search_results_for_shots(&mut doc, &automerge::ROOT);
    settle(&mut harness, vw, vh);
    harness
}

fn render_directory_palette_fixture(
    harness: &mut AbsorbHarness,
    width: u32,
    height: u32,
    path: &std::path::Path,
) {
    let theme = Theme::dark();
    let mut text_renderer = TextRenderer::new();
    let mut scene = vello::Scene::new();
    harness.host.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &ui::presence::PresenceCtx::default(),
    );
    let mut renderer = offscreen::OffscreenRenderer::new(width, height);
    renderer.render_png(&scene, vello::peniko::Color::BLACK, path);
}

#[test]
fn directory_action_rows_paint_label_and_subtitle_at_normal_and_narrow_sizes() {
    fn directory_action_row_is_painted_with_label_and_subtitle_at_normal_and_narrow_sizes() {
        for (vw, vh) in [(1280.0, 800.0), (800.0, 600.0)] {
            let mut harness = directory_palette_fixture(vw, vh);
            let rows = harness.host.palette_rows_detailed();
            assert!(
                rows.iter().any(|(_, label, subtitle)| {
                    label == "Open project workspace"
                        && subtitle.as_deref() == Some("Example project · Code Editor")
                }),
                "directory action must reach the painted palette rows at {vw}x{vh}: {rows:?}"
            );
            assert!(
                harness.host.palette.is_focused(),
                "open directory palette must retain keyboard focus at {vw}x{vh}"
            );
            let palette = harness.host.palette.get_rect();
            assert!(
                palette.width() > 0.0 && palette.height() > 0.0,
                "directory palette must retain a drawable card at {vw}x{vh}: {palette:?}"
            );
            paint(&mut harness);
            assert!(
                harness.host.palette_visible(),
                "drawing the directory row must not dismiss the focused palette"
            );
            let semantics = harness.host.accessibility().expect("shell semantics");
            let palette = semantics
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Command palette"))
                .expect("the visible palette must be in the accessibility tree");
            let result = palette
                .children
                .get(1)
                .and_then(|list| list.children.first())
                .expect("the directory action must be a semantic palette result");
            assert_eq!(result.name.as_deref(), Some("Open project workspace"));
            assert_eq!(
                result.description.as_deref(),
                Some("Example project · Code Editor")
            );
            assert!(
                harness.host.focus_synthetic_accessibility(&[0, 1, 0]),
                "the directory action must be reachable through the host accessibility path"
            );
        }
    }

    fn directory_palette_fixture_writes_normal_and_narrow_pngs() {
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target/directory-palette-review");
        for (name, vw, vh, width, height) in [
            ("normal.png", 1280.0, 800.0, 1280, 800),
            ("narrow.png", 800.0, 600.0, 800, 600),
        ] {
            let mut harness = directory_palette_fixture(vw, vh);
            let path = output.join(name);
            render_directory_palette_fixture(&mut harness, width, height, &path);
            assert!(
                path.is_file(),
                "directory palette screenshot missing: {}",
                path.display()
            );
        }
    }

    directory_action_row_is_painted_with_label_and_subtitle_at_normal_and_narrow_sizes();
    directory_palette_fixture_writes_normal_and_narrow_pngs();
}

fn cursor_moved_at(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: PhysicalPosition::new(p.x, p.y),
    }
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
            .append_empty_workspace("Second");
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
            let second = tree.append_empty_workspace("Second");
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
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
}

fn key_released(key: Key) -> WindowEvent {
    let event = ui::synthetic_input::key_event(key, None, ElementState::Released);
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
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

/// Paints (see [`paint`]), then clicks a point.
fn paint_then_click(h: &mut AbsorbHarness, point_of: impl Fn(&ShellOverlayHost) -> Point) {
    paint(h);
    let p = point_of(&h.host);
    pump(h, &cursor_moved_at(p));
    pump(h, &mouse_press());
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
fn filter_band_controls_are_bounded_semantic_targets_that_lift_on_selection() {
    /// A chip or scope segment rests at the §9.6 Selector height and grows
    /// with its measured label — at 200 % text the label sat visibly
    /// outside a fixed 24 px pill — but stops `s1` short of the band's
    /// row, so two rows of chips keep a gutter rather than touching.
    fn filter_controls_grow_with_their_label_and_stay_inside_the_row() {
        let mut tr = TextRenderer::new();
        let row_h = CommandPalette::new().filter_strip_row_height();
        let base = Theme::dark();
        let at_base = SearchFilters::filter_control_height(&base, &mut tr, row_h);
        assert_eq!(
            at_base,
            ui::search::system::filters::SELECTOR_HEIGHT_PX,
            "resting height is the Selector's"
        );

        let large = Theme::dark().with_text_scale(2.0);
        // The palette's filter rows grow with the text scale it lays out at.
        let row_h = row_h * f64::from(large.text_scale);
        let (_, lh) = tr.measure_style("Ag", None, &SearchFilters::filter_label_style(&large));
        let at_large = SearchFilters::filter_control_height(&large, &mut tr, row_h);
        assert!(
            at_large > at_base,
            "a 200% label needs a taller control: {at_large} vs {at_base}"
        );
        assert!(
            at_large >= f64::from(lh),
            "the control ({at_large}) must be at least its label's line ({lh})"
        );
        let s1 = f64::from(large.tokens.spacing.s1);
        assert!(
            at_large <= row_h - s1,
            "the control ({at_large}) must leave an s1 gutter inside its {row_h} px row"
        );
    }

    /// Every kind chip, the "All" reset and every scope segment is a named,
    /// bounded semantic node carrying the rect production `draw` painted it
    /// at — so a screen reader, a script and the UX bench can name a chip
    /// rather than guess where the strip wrapped it to.
    fn filter_chips_and_scope_segments_are_bounded_semantic_targets() {
        use crate::shell::search::ResourceKind;
        use ui::a11y::Role;
        let (vw, vh) = (1280.0, 800.0);
        let mut h = build_harness_sized(vw, vh);
        h.host.set_palette_visible(true);
        // Open (focused), as Super+K leaves it: only then does the palette
        // publish its query and result list, which the band follows.
        h.host.set_palette_focus(true);
        h.host.force_palette_settled();
        settle(&mut h, vw, vh);

        let tree = h.host.accessibility().expect("the host publishes a tree");
        let palette = tree
            .children
            .iter()
            .find(|n| n.name.as_deref() == Some("Command palette"))
            .expect("the open palette is in the tree");
        let band_index = h
            .host
            .search_filters_child_index()
            .expect("an open palette places the band after its own children");
        assert_eq!(band_index, 2, "query, results, then the band");
        let filters = palette
            .children
            .get(band_index)
            .expect("the filter band is the palette's trailing child");
        assert_eq!(filters.name.as_deref(), Some("Search filters"));
        let (kinds_group, scope_group) = (&filters.children[0], &filters.children[1]);

        let kinds = h.host.search_registry.registered_kinds();
        let more_painted = h.host.search_filters.search_more_rect.area() > 0.0;
        assert_eq!(
            kinds_group.children.len(),
            kinds.len() + 1 + usize::from(more_painted),
            "All, every kind, and More when the row overflowed"
        );
        let all = &kinds_group.children[0];
        assert_eq!(all.role, Role::ToggleButton);
        assert_eq!(all.name.as_deref(), Some("Filter: All"));
        assert_eq!(
            all.state.checked,
            Some(true),
            "the palette starts unfiltered"
        );
        // 2026-09-17 (S1-28): a chip's published bounds are its pointer
        // target — the painted pill grown to the §9.6 floor, the rect a
        // press is hit-tested against — not the pill itself. The pill sits
        // inside it, horizontally identical.
        let floor = f64::from(Theme::dark().tokens.metrics.min_pointer_target);
        let pill_inside_target = |pill: Rect, target: Rect| {
            (pill.x0 - target.x0).abs() < 0.01
                && (pill.x1 - target.x1).abs() < 0.01
                && target.y0 <= pill.y0 + 0.01
                && target.y1 >= pill.y1 - 0.01
                && target.height() + 0.01 >= floor
                && (target.center().y - pill.center().y).abs() < 0.01
        };
        let all_target = all
            .bounds
            .map(ui::AccessBounds::to_rect)
            .expect("All is bounded");
        assert_eq!(all_target, h.host.search_filters.search_all_chip_hit_rect());
        assert!(
            pill_inside_target(h.host.search_filters.search_all_chip_rect, all_target),
            "All's target {all_target:?} must wrap its pill {:?} to the {floor} px floor",
            h.host.search_filters.search_all_chip_rect
        );
        for (i, kind) in kinds.iter().enumerate() {
            let chip = &kinds_group.children[i + 1];
            assert_eq!(
                chip.name.as_deref(),
                Some(format!("Filter: {}", kind.localized_label()).as_str())
            );
            assert_ne!(
                chip.name.as_deref(),
                Some(kind.localized_label().as_str()),
                "a chip must not publish the bare kind label — a window title may share it"
            );
            assert_eq!(chip.state.checked, Some(false));
            let pill = h.host.search_filters.search_chip_rects[i];
            if pill.area() <= 0.0 {
                assert!(
                    chip.state.hidden,
                    "a kind behind More stays named but hidden until More opens"
                );
                continue;
            }
            let target = chip
                .bounds
                .map(ui::AccessBounds::to_rect)
                .expect("chip is bounded");
            assert_eq!(
                target,
                h.host.search_filters.search_chip_hit_rect(i),
                "chip {i} carries its pointer target"
            );
            assert!(
                pill_inside_target(pill, target),
                "chip {i}'s target {target:?} must wrap its pill {pill:?} to the {floor} px floor"
            );
            assert_eq!(
                h.host
                    .synthetic_accessibility_rect(&[0, band_index, 0, i + 1]),
                Some(target),
                "the routed geometry for chip {i} is the same rect"
            );
            assert!(
                chip.action_target.as_deref().is_some_and(
                    |t| t.starts_with("palette:filter:") && t.matches(':').count() == 2
                ),
                "chip {i} carries a single-segment address: {:?}",
                chip.action_target
            );
        }
        if more_painted {
            let more = kinds_group.children.last().expect("More is last");
            assert_eq!(more.name.as_deref(), Some("Filter: More"));
            assert_eq!(
                more.bounds.map(ui::AccessBounds::to_rect),
                Some(h.host.search_filters.search_more_hit_rect())
            );
        }

        let scopes = h.host.search_scope_list();
        assert_eq!(scope_group.children.len(), scopes.len());
        let checked = scope_group
            .children
            .iter()
            .filter(|n| n.state.checked == Some(true))
            .count();
        assert_eq!(checked, 1, "exactly one scope is current");
        for (idx, node) in scope_group.children.iter().enumerate() {
            assert_eq!(node.role, Role::RadioButton);
            let hit = h.host.search_filters.search_scope_segment_hit_rect(idx);
            assert_eq!(node.bounds.map(ui::AccessBounds::to_rect), Some(hit));
            let track = h.host.search_filters.search_scope_track_rect();
            assert!(
                hit.contains(h.host.search_filters.search_scope_pill_rect(idx).center())
                    && (hit.height() - track.height().max(floor)).abs() < 0.01
                    && (hit.center().y - track.center().y).abs() < 0.01,
                "segment {idx}'s target is its slice of the track grown to the {floor} px floor: {hit:?} vs {track:?}"
            );
            assert!(
                node.name
                    .as_deref()
                    .is_some_and(|n| n.starts_with("Scope: ")),
                "scope names are prefixed: {:?}",
                node.name
            );
        }

        // The controls share one centre line: chips and the scope switch
        // are laid out to the same control height in the band's first row.
        // Addresses stay unique and single-segment even for hostile labels.
        let odd = [
            ResourceKind::Custom("Foo Bar".into()),
            ResourceKind::Custom("foo  bar".into()),
            ResourceKind::Custom("CI: Builds".into()),
        ];
        let keys: Vec<String> = (0..odd.len())
            .map(|i| SearchFilters::search_filter_key(&odd, i))
            .collect();
        assert_eq!(keys[0], "palette:filter:foo-bar");
        assert_ne!(
            keys[0], keys[1],
            "a case/whitespace twin gets its own address"
        );
        assert_eq!(keys[2], "palette:filter:ci-builds");

        // Paint and target are two rects with one centre line: the pills
        // share top and bottom edges at the Selector height, and their
        // pointer targets share top and bottom edges at the floor.
        let all_rect = h.host.search_filters.search_all_chip_rect;
        let track = h.host.search_filters.search_scope_track_rect();
        assert!(
            (all_rect.y0 - track.y0).abs() < 0.5 && (all_rect.y1 - track.y1).abs() < 0.5,
            "All chip {all_rect:?} and scope track {track:?} do not share top and bottom edges"
        );
        let all_target = h.host.search_filters.search_all_chip_hit_rect();
        let first_scope = h.host.search_filters.search_scope_segment_hit_rect(0);
        assert!(
            (all_target.y0 - first_scope.y0).abs() < 0.5
                && (all_target.y1 - first_scope.y1).abs() < 0.5,
            "All target {all_target:?} and scope target {first_scope:?} do not share top and bottom edges"
        );
        assert!(
            (all_rect.height() - f64::from(Theme::dark().tokens.metrics.selector_height)).abs()
                < 0.01,
            "a chip rests at the Selector height (`metrics.selector_height`), got {}",
            all_rect.height()
        );
        assert!(
            all_target.height() + 0.01 >= floor,
            "a chip's target meets the pointer floor {floor}, got {}",
            all_target.height()
        );
    }

    /// At 200 % text on a compact card the two scope labels need more than
    /// the share of the band the switch may take beside the chips, so the
    /// switch takes the first row alone, whole, and the chips start on the
    /// next row: no "Works…", no overlap, one more reserved row (S1-29).
    /// At 100 % on a wide card the switch shares the first row as before.
    fn a_scope_switch_too_wide_to_share_the_first_row_takes_its_own() {
        let (vw, vh) = (800.0, 700.0);
        let mut h = build_harness_sized(vw, vh);
        h.host.set_palette_visible(true);
        h.host.set_palette_focus(true);
        h.host.force_palette_settled();
        let theme = Theme::dark().with_text_scale(2.0);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        // Paint, relayout, paint: the first paint measures the switch and
        // asks the palette for the extra row; the second lays out with it.
        for _ in 0..3 {
            let (tree, root) = lay_out_host_with_theme(&mut h.host, vw, vh, &theme);
            h.taffy = tree;
            h.root_node = root;
            h.host.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &ui::presence::PresenceCtx::default(),
            );
        }
        assert!(
            h.host.search_filters.scope_switch_has_own_row(),
            "the measured switch ({}) exceeds its share of the {} px band",
            h.host.search_filters.search_scope_measured_track_w(),
            h.host.search_filters.search_banner_rect().width()
        );
        let band = h.host.search_filters.search_banner_rect();
        let row_h = h.host.palette.filter_strip_row_height();
        let track = h.host.search_filters.search_scope_track_rect();
        assert!(
            (track.center().y - (band.y0 + row_h / 2.0)).abs() < 0.5,
            "the switch sits on the band's first row: {track:?} in {band:?}"
        );
        let measured = h.host.search_filters.search_scope_measured_track_w();
        assert!(
            (track.width() - measured.min(band.width())).abs() < 0.5,
            "on its own row the switch is its full measured width ({measured}), capped only by the band ({}), not by the shared-row share: {track:?}",
            band.width()
        );
        assert!(
            measured <= band.width(),
            "two scope labels at 200 % fit an 800 px card once the label is drawn at the person's size, not double-scaled: {measured} vs {}",
            band.width()
        );
        for idx in 0..h.host.search_scope_list().len() {
            let seg = h.host.search_filters.search_scope_pill_rect(idx);
            let label = h
                .host
                .search_scope_pill_label(h.host.search_scope_list()[idx]);
            let style = SearchFilters::filter_label_style(&theme);
            let (w, _) = tr.measure_style(&label, None, &style);
            assert!(
                f64::from(w) + f64::from(theme.tokens.spacing.s2) <= seg.width(),
                "{label:?} ({w} px) fits its segment {seg:?} whole"
            );
        }
        let all = h.host.search_filters.search_all_chip_rect;
        assert!(
            all.y0 >= band.y0 + row_h - 0.5,
            "the chips start on the second row: All {all:?} under row 1 of {band:?}"
        );
        assert!(
            h.host.search_filters.search_chip_rows >= 2,
            "the band reserved a row for the switch plus the chips' rows"
        );
        for (i, chip) in h.host.search_filters.search_chip_rects.iter().enumerate() {
            assert!(
                chip.intersect(track).is_zero_area(),
                "chip {i} {chip:?} does not run under the switch {track:?}"
            );
        }

        // Wide card, 100 %: the switch shares the first row with "All".
        let (vw, vh) = (1280.0, 800.0);
        let mut wide = build_harness_sized(vw, vh);
        wide.host.set_palette_visible(true);
        wide.host.set_palette_focus(true);
        wide.host.force_palette_settled();
        settle(&mut wide, vw, vh);
        assert!(!wide.host.search_filters.scope_switch_has_own_row());
        let all = wide.host.search_filters.search_all_chip_rect;
        let track = wide.host.search_filters.search_scope_track_rect();
        assert!((all.center().y - track.center().y).abs() < 0.5);
    }

    /// Below 100 % text the palette's row (`36·text_scale`) is shorter than
    /// the unscaled pointer floor, so a target grown to the floor would
    /// overlap its neighbours' and the first row's would protrude above
    /// the band. The floor is clamped to `row_h − s1` and every target is
    /// clipped to the band: rows never share a pixel, nothing leaves the
    /// band, and every target still wraps its pill.
    fn selector_targets_never_overlap_or_leave_the_band_at_small_text() {
        let (vw, vh) = (900.0, 700.0);
        let mut h = build_harness_sized(vw, vh);
        h.host.set_palette_visible(true);
        h.host.set_palette_focus(true);
        h.host.force_palette_settled();
        let theme = Theme::dark().with_text_scale(0.75);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(0.75);
        for _ in 0..3 {
            let (tree, root) = lay_out_host_with_theme(&mut h.host, vw, vh, &theme);
            h.taffy = tree;
            h.root_node = root;
            h.host.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &ui::presence::PresenceCtx::default(),
            );
        }
        let band = h.host.search_filters.search_banner_rect();
        let row_h = h.host.palette.filter_strip_row_height();
        let s1 = f64::from(theme.tokens.spacing.s1);
        assert!(
            h.host.search_filters.search_pointer_floor_h <= row_h - s1 + 0.01,
            "the floor ({}) is clamped to the row less its gutter ({})",
            h.host.search_filters.search_pointer_floor_h,
            row_h - s1
        );
        let mut targets: Vec<(Rect, Rect)> = vec![(
            h.host.search_filters.search_all_chip_rect,
            h.host.search_filters.search_all_chip_hit_rect(),
        )];
        for i in 0..h.host.search_filters.search_chip_rects.len() {
            let pill = h.host.search_filters.search_chip_rects[i];
            if pill.area() <= 0.0 {
                continue;
            }
            targets.push((pill, h.host.search_filters.search_chip_hit_rect(i)));
        }
        if h.host.search_filters.search_more_rect.area() > 0.0 {
            targets.push((
                h.host.search_filters.search_more_rect,
                h.host.search_filters.search_more_hit_rect(),
            ));
        }
        for idx in 0..h.host.search_scope_list().len() {
            targets.push((
                h.host.search_filters.search_scope_pill_rect(idx),
                h.host.search_filters.search_scope_segment_hit_rect(idx),
            ));
        }
        assert!(targets.len() >= 4, "the band has chips and scopes to check");
        for (pill, target) in &targets {
            assert!(
                target.y0 >= band.y0 - 0.01 && target.y1 <= band.y1 + 0.01,
                "target {target:?} stays inside the band {band:?}"
            );
            assert!(
                target.y0 <= pill.y0 + 0.01 && target.y1 >= pill.y1 - 0.01,
                "target {target:?} still wraps its pill {pill:?}"
            );
        }
        for (i, (_, a)) in targets.iter().enumerate() {
            for (_, b) in targets.iter().skip(i + 1) {
                let overlap = a.intersect(*b);
                assert!(
                    overlap.is_zero_area() || overlap.width() <= 1.0 || overlap.height() <= 1.0,
                    "targets overlap by more than a pixel: {a:?} vs {b:?}"
                );
            }
        }
    }

    /// The band's "which one is chosen" is the shared selection tier, not
    /// the accent: the chosen chip's body is the full lift, an idle one has
    /// no body, and the ink on each is legible against what is under it
    /// (HIG §9.3 / §9.4; the accent is reserved for live state).
    fn filter_band_paints_selection_as_a_lift_never_as_accent() {
        use ui::a11y::contrast_ratio;
        let theme = Theme::dark();
        let colors = &theme.tokens.colors;
        let chosen = ui::chrome::selection_tier(&theme, ui::chrome::SelectionState::active(true));
        let idle = ui::chrome::selection_tier(&theme, ui::chrome::SelectionState::active(false));
        assert_eq!(chosen.fill, colors.lift(1.0));
        assert_ne!(
            chosen.fill, colors.intent.inverted,
            "a chosen chip is lifted, not accent-filled"
        );
        assert!(chosen.border.is_some() && chosen.inner_glow.is_some());
        assert_eq!(
            idle.fill.components[3], 0.0,
            "an idle chip has no body of its own"
        );
        // The ink the painter picks for each state clears the fill it sits on.
        let ground = ui::chrome::shell::panel_ground(&theme);
        let chosen_ink = colors.ink_on(
            chosen.fill,
            colors.text_primary,
            ui::tokens::colors::ON_FILL_MIN_CONTRAST,
        );
        let idle_ink = colors.ink_on(
            ground,
            colors.text_dim,
            ui::tokens::colors::ON_FILL_MIN_CONTRAST,
        );
        assert!(
            contrast_ratio(chosen_ink, chosen.fill) >= ui::tokens::colors::ON_FILL_MIN_CONTRAST
        );
        assert!(contrast_ratio(idle_ink, ground) >= ui::tokens::colors::ON_FILL_MIN_CONTRAST);
        // And the same holds on the light and high-contrast presets.
        for theme in [Theme::light(), Theme::high_contrast()] {
            let colors = &theme.tokens.colors;
            let chosen =
                ui::chrome::selection_tier(&theme, ui::chrome::SelectionState::active(true));
            let ink = colors.ink_on(
                chosen.fill,
                colors.text_primary,
                ui::tokens::colors::ON_FILL_MIN_CONTRAST,
            );
            assert!(
                contrast_ratio(ink, chosen.fill) >= ui::tokens::colors::ON_FILL_MIN_CONTRAST,
                "chosen-chip ink must clear its lift on every preset"
            );
        }
    }

    filter_controls_grow_with_their_label_and_stay_inside_the_row();
    filter_chips_and_scope_segments_are_bounded_semantic_targets();
    a_scope_switch_too_wide_to_share_the_first_row_takes_its_own();
    selector_targets_never_overlap_or_leave_the_band_at_small_text();
    filter_band_paints_selection_as_a_lift_never_as_accent();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn launcher_chips_fit_and_stay_reachable_at_every_width_and_text_size() {
    fn compact_large_text_launcher_chips_fit_full_labels_and_viewport() {
        let mut h = build_harness_sized(800.0, 700.0);
        h.host.set_palette_visible(true);
        h.host.force_palette_settled();
        let theme = Theme::dark().with_text_scale(2.0);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        for _ in 0..4 {
            let (tree, root) = lay_out_host_with_theme(&mut h.host, 800.0, 700.0, &theme);
            h.taffy = tree;
            h.root_node = root;
            h.host.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &ui::presence::PresenceCtx::default(),
            );
        }
        assert!(h.host.palette.get_rect().y1 <= 684.0);
        assert!(
            h.host.search_filters.search_more_rect.area() > 0.0,
            "2× text on a compact card must keep leftover kinds behind More"
        );
        let style = SearchFilters::filter_label_style(&theme);
        for (kind, rect) in h
            .host
            .search_registry
            .registered_kinds()
            .iter()
            .zip(&h.host.search_filters.search_chip_rects)
        {
            if rect.area() <= 0.0 {
                continue;
            }
            let (w, height) = tr.measure_style(&kind.localized_label(), None, &style);
            assert!(
                f64::from(w) + f64::from(theme.tokens.spacing.s3) / 2.0 <= rect.width(),
                "{} needs readable width: {:?}",
                kind.localized_label(),
                rect
            );
            assert!(
                f64::from(height) <= rect.height(),
                "{} needs text height: {:?}",
                kind.label(),
                rect
            );
        }
        for (i, scope) in h.host.search_scope_list().iter().enumerate() {
            let label = h.host.search_scope_pill_label(*scope);
            let (w, height) = tr.measure_style(&label, None, &style);
            let rect = h.host.search_filters.search_scope_pill_rect(i);
            assert!(
                f64::from(w) + 10.0 <= rect.width(),
                "{label} scope truncated"
            );
            assert!(f64::from(height) <= rect.height());
        }
    }

    fn every_kind_is_reachable_at_every_width() {
        // 800 is the narrowest viewport `shots --sweep` renders, where the
        // palette card hits its 560px floor and the filter band is tightest.
        for (vw, vh) in [(800.0, 600.0), (1280.0, 800.0), (1600.0, 900.0)] {
            let mut h = build_harness_sized(vw, vh);
            h.host.set_palette_visible(true);
            h.host.force_palette_settled();
            settle(&mut h, vw, vh);

            let kinds = h.host.search_registry.registered_kinds();
            let rects = h.host.search_filters.search_chip_rects.clone();
            assert_eq!(
                rects.len(),
                kinds.len(),
                "at {vw}x{vh} the strip painted {} of {} kinds — an \
                 unpainted chip has no rect, so the filter it stands for \
                 cannot be clicked at all: {:?}",
                rects.len(),
                kinds.len(),
                kinds
                    .iter()
                    .map(super::super::search::ResourceKind::label)
                    .collect::<Vec<_>>()
            );

            let band = h.host.search_filters.search_banner_rect();
            let all = h.host.search_filters.search_all_chip_rect;
            let track = h.host.search_filters.search_scope_track_rect();
            let mut painted: Vec<(String, Rect)> = vec![("All".to_string(), all)];
            painted.extend(
                kinds
                    .iter()
                    .map(crate::shell::search::ResourceKindText::localized_label)
                    .zip(rects.iter().copied()),
            );
            if painted.iter().any(|(_, r)| r.area() <= 0.0) {
                assert!(
                    h.host.search_filters.search_more_rect.area() > 0.0,
                    "at {vw}x{vh} leftover kinds must sit behind More"
                );
                h.host.search_filters.more_open = true;
                settle(&mut h, vw, vh);
                let opened = &h.host.search_filters.search_chip_rects;
                assert!(
                    opened.iter().all(|r| r.area() > 0.0),
                    "at {vw}x{vh} every kind is clickable once More is open"
                );
            }
            let painted: Vec<(String, Rect)> = painted
                .into_iter()
                .filter(|(_, r)| r.area() > 0.0)
                .collect();
            for (label, r) in &painted {
                assert!(
                    r.area() > 0.0,
                    "at {vw}x{vh} the {label} chip has no area: {r:?}"
                );
                assert!(
                    r.y0 >= band.y0 - 0.5 && r.y1 <= band.y1 + 0.5,
                    "at {vw}x{vh} the {label} chip {r:?} is outside the \
                     band {band:?} — it would paint over the results"
                );
                assert!(
                    r.x0 >= band.x0 - 0.5 && r.x1 <= band.x1 + 0.5,
                    "at {vw}x{vh} the {label} chip {r:?} runs past the \
                     band's edges {band:?}"
                );
            }
            // Nothing overlaps anything: two chips sharing pixels means
            // one of them eats the other's clicks.
            for i in 0..painted.len() {
                for j in (i + 1)..painted.len() {
                    let (a, b) = (painted[i].1, painted[j].1);
                    assert!(
                        a.intersect(b).area() <= 0.0,
                        "at {vw}x{vh} the {} and {} chips overlap: {a:?} / {b:?}",
                        painted[i].0,
                        painted[j].0
                    );
                }
                assert!(
                    painted[i].1.intersect(track).area() <= 0.0,
                    "at {vw}x{vh} the {} chip sits under the scope switch: \
                     {:?} / {track:?}",
                    painted[i].0,
                    painted[i].1
                );
            }
        }
    }

    /// Clicking the last chip at the narrowest viewport must apply its
    /// filter — driven through the production `handle_event` router against
    /// the rect `draw` actually cached, so a mis-placed chip fails here too.
    fn the_last_chip_can_be_clicked_at_the_narrowest_width() {
        let (vw, vh) = (800.0, 600.0);
        let mut h = build_harness_sized(vw, vh);
        h.host.set_palette_visible(true);
        h.host.force_palette_settled();
        settle(&mut h, vw, vh);

        let last_kind = h
            .host
            .search_registry
            .registered_kinds()
            .last()
            .cloned()
            .expect("the registry has kinds");
        assert!(
            h.host.active_kind_filter.is_empty(),
            "the palette starts unfiltered"
        );
        if h.host
            .search_filters
            .search_chip_rects
            .last()
            .is_some_and(|rect| rect.area() <= 0.0)
        {
            let more = h.host.search_filters.search_more_rect;
            assert!(more.area() > 0.0, "the last kind is reached through More");
            pump(&mut h, &cursor_moved_at(more.center()));
            pump(&mut h, &mouse_press());
            h.host.search_filters.more_open = true;
            settle(&mut h, vw, vh);
        }
        let target = *h
            .host
            .search_filters
            .search_chip_rects
            .last()
            .expect("the last kind must have been painted");
        assert!(
            target.area() > 0.0,
            "opening More must give the last kind a clickable rect"
        );
        pump(&mut h, &cursor_moved_at(target.center()));
        pump(&mut h, &mouse_press());

        assert_eq!(
            h.host.active_kind_filter,
            vec![last_kind.clone()],
            "clicking the last chip at {vw}x{vh} must filter to {:?}; the \
             palette instead holds {:?}",
            last_kind.label(),
            h.host
                .active_kind_filter
                .iter()
                .map(super::super::search::ResourceKind::label)
                .collect::<Vec<_>>()
        );
        assert!(
            h.host.palette_visible(),
            "and the click must not be taken for a dismissal"
        );
    }

    /// Every filter must be reachable at every width. An unpainted chip has
    /// no rect, so its filter is unclickable even though the registry lists
    /// it — at 1600px five kinds exactly filled the row, hiding a sixth.
    /// Asserts on the rects production `draw` cached, since that's what a
    /// click is hit-tested against.
    fn palette_height_only_resize_requests_a_corrected_layout() {
        let mut h = build_harness_sized(800.0, 900.0);
        h.host.set_palette_visible(true);
        h.host.force_palette_settled();
        let theme = Theme::dark().with_text_scale(2.0);
        settle_with_theme(&mut h, 800.0, 900.0, &theme);
        h.host.palette_layout_dirty = false;
        let _ = lay_out_host_with_theme(&mut h.host, 800.0, 500.0, &theme);
        assert!(
            h.host.palette_layout_dirty,
            "height-only resize must recompute palette cap from new viewport"
        );
        let _ = lay_out_host_with_theme(&mut h.host, 800.0, 500.0, &theme);
        assert!(h.host.palette.get_rect().y1 <= 484.0);
    }

    compact_large_text_launcher_chips_fit_full_labels_and_viewport();
    every_kind_is_reachable_at_every_width();
    the_last_chip_can_be_clicked_at_the_narrowest_width();
    palette_height_only_resize_requests_a_corrected_layout();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn palette_chips_and_scopes_filter_without_dismissing_and_enter_activates_the_selection() {
    /// Clicking a kind chip or scope switch must filter, not be mistaken
    /// for a dismissal click outside the card. Drives the production
    /// `handle_event` router against rects `draw` just cached.
    fn clicking_a_kind_chip_filters_without_dismissing_the_palette() {
        let mut h = build_harness();
        h.host.set_palette_visible(true);
        h.host.force_palette_settled();
        settle(&mut h, 1600.0, 900.0);

        assert!(
            h.host.active_kind_filter.is_empty(),
            "the palette starts unfiltered"
        );
        if !h
            .host
            .search_filters
            .search_chip_rects
            .iter()
            .any(|rect| rect.area() > 0.0)
        {
            h.host.search_filters.more_open = true;
            settle(&mut h, 1600.0, 900.0);
        }
        paint_then_click(&mut h, |host| {
            host.search_filters
                .search_chip_rects
                .iter()
                .find(|rect| rect.area() > 0.0)
                .copied()
                .expect("the chip strip must have painted at least one chip")
                .center()
        });

        assert!(
            h.host.palette_visible(),
            "a click on a chip inside the card must not dismiss the palette"
        );
        assert_eq!(
            h.host.active_kind_filter.len(),
            1,
            "the first chip must have toggled a kind on: {:?}",
            h.host.active_kind_filter
        );

        // …and the "All" chip clears it again.
        paint_then_click(&mut h, |host| {
            host.search_filters.search_all_chip_rect.center()
        });
        assert!(
            h.host.palette_visible(),
            "the All chip must not dismiss the palette either"
        );
        assert!(
            h.host.active_kind_filter.is_empty(),
            "All must clear the kind filter, got {:?}",
            h.host.active_kind_filter
        );
    }

    /// A new-window picker deliberately begins with both launch sources so
    /// either kind of program can be opened. Selecting Host Apps from that
    /// state must replace the source selection, not toggle host rows off and
    /// leave Applications lit (the live picker regression this covers).
    fn host_apps_chip_exclusively_filters_a_new_window_picker() {
        use crate::shell::search::ResourceKind;

        let mut h = build_harness();
        h.host
            .search_registry
            .register(Rc::new(AppSourceFilterFixtureProvider));
        h.host.active_kind_filter = app_kind_filter();
        h.host.set_palette_visible(true);
        h.host.force_palette_settled();
        settle(&mut h, 1600.0, 900.0);

        let host_chip = h
            .host
            .search_registry
            .registered_kinds()
            .iter()
            .position(|kind| *kind == ResourceKind::HostApplication)
            .expect("Host Apps chip is registered");
        if h.host
            .search_filters
            .search_chip_rects
            .get(host_chip)
            .is_some_and(|rect| rect.area() <= 0.0)
        {
            h.host.search_filters.more_open = true;
            settle(&mut h, 1600.0, 900.0);
        }
        paint_then_click(&mut h, |host| {
            host.search_filters.search_chip_rects[host_chip].center()
        });

        assert_eq!(
            h.host.active_kind_filter,
            vec![ResourceKind::HostApplication]
        );
        assert!(
            h.host.search_results.iter().any(|row| {
                row.provider_id == "app-source-filter-fixture" && row.id == "fixture-host-app"
            }),
            "the selected Host Apps source must reach the production palette query"
        );
        assert!(
            h.host
                .search_results
                .iter()
                .all(|row| row.kind == ResourceKind::HostApplication),
            "Host Apps must not leave Tilo application rows visible: {:?}",
            h.host
                .search_results
                .iter()
                .map(|row| (&row.title, &row.kind))
                .collect::<Vec<_>>()
        );
    }

    fn app_search_arrows_and_enter_activate_the_visibly_selected_row() {
        use crate::shell::search::ResourceKind;

        let mut h = build_harness();
        h.host
            .search_registry
            .register(Rc::new(AppSourceFilterFixtureProvider));
        h.host.active_kind_filter = vec![ResourceKind::Application, ResourceKind::HostApplication];
        h.host.set_palette_visible(true);
        h.host.set_palette_focus(true);
        h.host.force_palette_settled();
        h.host.palette.set_query("Open");
        let doc = AutoCommit::new();
        h.host.refresh_search_results(&doc, &automerge::ROOT);

        let (_, _, initially_selected, rows) = h.host.palette_snapshot();
        assert!(
            rows.len() >= 2,
            "fixture must expose both app sources: {rows:?}"
        );
        let initially_selected = initially_selected.expect("first row is visibly selected");
        assert_eq!(initially_selected, rows[0].0);

        assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::ArrowDown)),));
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

        assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Enter)),));
        assert!(
            !h.host.palette_visible(),
            "Enter's activation path must complete the selected app action"
        );
        assert_ne!(
            selected_after_arrow, initially_selected,
            "activation must follow the arrow-selected row, not stale result zero"
        );
    }

    /// Same for the scope switch, which shares the band.
    fn clicking_a_scope_segment_switches_scope_without_dismissing() {
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
        assert_eq!(
            h.host.search_scope_pill_label(SearchScope::Workspace),
            "‹ Book of Tilo",
            "the selected place pill doubles as a visible resource breadcrumb"
        );

        paint_then_click(&mut h, |host| {
            let workspace = host
                .search_scope_list()
                .iter()
                .position(|scope| *scope == SearchScope::Workspace)
                .expect("workspace scope segment");
            host.search_filters
                .search_scope_pill_rect(workspace)
                .center()
        });
        assert!(
            h.chrome.borrow().search_resource_scope.is_none(),
            "clicking the visible breadcrumb leaves the nested resource scope"
        );

        // The last segment is All (the scope list is
        // [ActiveApp?, Workspace, All]).
        paint_then_click(&mut h, |host| {
            let last = host.search_scope_list().len() - 1;
            host.search_filters.search_scope_pill_rect(last).center()
        });

        assert!(h.host.palette_visible(), "the palette must stay open");
        assert_eq!(
            h.chrome.borrow().search_scope,
            SearchScope::All,
            "clicking the trailing segment must select the All scope"
        );
        assert!(
            h.chrome.borrow().search_resource_scope.is_none(),
            "choosing a place leaves the nested resource scope"
        );
    }

    clicking_a_kind_chip_filters_without_dismissing_the_palette();
    host_apps_chip_exclusively_filters_a_new_window_picker();
    app_search_arrows_and_enter_activate_the_visibly_selected_row();
    clicking_a_scope_segment_switches_scope_without_dismissing();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn open_overlays_absorb_their_input_and_leave_the_rest_to_content() {
    fn overlay_absorbs_cursor_moved_when_palette_open() {
        let mut h = build_harness();
        h.host.set_palette_visible(true);
        let palette_rect = h.host.palette.get_rect();
        assert!(
            palette_rect.area() > 0.0,
            "palette must have a non-empty rect after layout"
        );
        let p = palette_rect.center();
        let cm = cursor_moved_at(p);
        let handled = pump(&mut h, &cm);

        assert!(
            handled,
            "CursorMoved over the open palette must be absorbed (handled = true)"
        );
        let log = h.log.borrow();
        assert!(
            !log.iter().any(|s| s == "CursorMoved"),
            "content.handle_event must NOT see CursorMoved when the palette absorbs it; log = {:?}",
            *log
        );
    }

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

    overlay_absorbs_cursor_moved_when_palette_open();
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
        assert!(h.host.layers.power_menu.is_open());
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
            h.host.layers.power_menu.is_open(),
            "clicking the visibly transformed Power tile must open its menu"
        );
        assert!(!h.chrome.borrow().overlays.quick_settings);
    }

    fn compact_quick_settings_drill_ins_own_wheel_scrolling() {
        use crate::shell::quick_settings::{BtDevice, QsLevel, WifiNetwork};

        let (vw, vh) = (960.0, 540.0);
        let wheel = MouseScrollDelta::LineDelta(0.0, -2.0);

        let mut wifi = build_harness_sized(vw, vh);
        {
            let mut state = wifi.chrome.borrow_mut();
            state.overlays.quick_settings = true;
            state.quick_settings.wifi = true;
            state.quick_settings.wifi_networks = (0..18)
                .map(|index| WifiNetwork {
                    ssid: format!("Network {index}"),
                    signal: 3,
                    secure: false,
                    security: os_actions::WifiSecurity::Open,
                    connected: false,
                })
                .collect();
            state.quick_settings.push_level(QsLevel::Wifi);
        }
        wifi.host.force_quick_settings_settled();
        settle(&mut wifi, vw, vh);
        let wifi_center = wifi.host.quick_settings_rect().center();
        assert!(pump(&mut wifi, &cursor_moved_at(wifi_center)));
        assert!(wifi.host.handle_qs_drill_in_wheel(wheel, 1.0));
        let wifi_names = wifi
            .host
            .quick_settings_semantic_rects()
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert!(wifi_names.contains(&"quick_settings_wifi_network[2]".to_string()));
        assert!(!wifi_names.contains(&"quick_settings_wifi_network[0]".to_string()));

        let mut bluetooth = build_harness_sized(vw, vh);
        {
            let mut state = bluetooth.chrome.borrow_mut();
            state.overlays.quick_settings = true;
            state.quick_settings.bt_devices = (0..18)
                .map(|index| BtDevice {
                    name: format!("Device {index}"),
                    address: format!("02:00:00:00:00:{index:02X}"),
                    battery: 70,
                    connected: false,
                })
                .collect();
            state.quick_settings.push_level(QsLevel::Bluetooth);
        }
        bluetooth.host.force_quick_settings_settled();
        settle(&mut bluetooth, vw, vh);
        let bluetooth_center = bluetooth.host.quick_settings_rect().center();
        assert!(pump(&mut bluetooth, &cursor_moved_at(bluetooth_center)));
        assert!(bluetooth.host.handle_qs_drill_in_wheel(wheel, 1.0));
        let bluetooth_names = bluetooth
            .host
            .quick_settings_semantic_rects()
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert!(bluetooth_names.contains(&"quick_settings_bluetooth_device[2]".to_string()));
        assert!(!bluetooth_names.contains(&"quick_settings_bluetooth_device[0]".to_string()));

        let mut vpn = build_harness_sized(vw, vh);
        {
            let mut state = vpn.chrome.borrow_mut();
            state.overlays.quick_settings = true;
            state.quick_settings.vpns = (0..18)
                .map(|index| os_actions::VpnConnectionInfo {
                    name: format!("Profile {index}"),
                    uuid: format!("profile-{index}"),
                    active: false,
                    kind: "wireguard".into(),
                })
                .collect();
            state.quick_settings.push_level(QsLevel::Vpn);
        }
        vpn.host.force_quick_settings_settled();
        settle(&mut vpn, vw, vh);
        let vpn_center = vpn.host.quick_settings_rect().center();
        assert!(pump(&mut vpn, &cursor_moved_at(vpn_center)));
        assert!(vpn.host.handle_qs_drill_in_wheel(wheel, 1.0));
        let vpn_names = vpn
            .host
            .quick_settings_semantic_rects()
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert!(vpn_names.contains(&"quick_settings_vpn_profile[2]".to_string()));
        assert!(!vpn_names.contains(&"quick_settings_vpn_profile[0]".to_string()));
    }

    quick_settings_power_tile_opens_the_power_menu_on_the_first_click();
    quick_settings_power_tile_tracks_its_entrance_animation();
    compact_quick_settings_drill_ins_own_wheel_scrolling();
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
            .push_level(crate::shell::quick_settings::QsLevel::Notifications);
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
        clippy::arithmetic_side_effects,
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
            "Warning. From System. 3 min ago. Failed"
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
                panel.y1 <= vh - 16.0,
                "panel must stay within the bottom inset"
            );

            assert!(pump(&mut h, &cursor_moved_at(panel.center())));
            let wheel = WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(0.0, -2.0),
                phase: winit::event::TouchPhase::Moved,
            };
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
                description.starts_with("Error. From System. ")
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
            &crate::shell::quick_settings::QsLevel::Notifications,
        );
        assert!(matches!(
            h.chrome.borrow().quick_settings.current_level(),
            crate::shell::quick_settings::QsLevel::Root
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
#[allow(
    clippy::expect_used,
    reason = "the opt-in visual harness must fail loudly for a missing GPU or output directory"
)]
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
#[allow(
    clippy::expect_used,
    reason = "a review fixture must fail loudly for a missing GPU or output directory"
)]
#[expect(
    clippy::too_many_lines,
    reason = "an end-to-end test scenario that reads top to bottom"
)]
fn s1_review_fixtures_are_written_in_dark_and_light() {
    use crate::shell::quick_settings::{BtDevice, QsLevel, WifiNetwork};
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
    fn quick_settings_open(h: &AbsorbHarness, level: QsLevel) {
        let mut st = h.chrome.borrow_mut();
        st.overlays.quick_settings = true;
        st.quick_settings.push_level(level);
        st.bump();
    }
    let scenes: [AbsorbScene; 5] = [
        ("qs-wifi-drillin", |h| {
            {
                let mut st = h.chrome.borrow_mut();
                st.quick_settings.radios.wifi_available = true;
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
            quick_settings_open(h, QsLevel::Wifi);
        }),
        ("qs-bt-drillin", |h| {
            {
                let mut st = h.chrome.borrow_mut();
                st.quick_settings.bluetooth_available = true;
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
            quick_settings_open(h, QsLevel::Bluetooth);
        }),
        ("qs-vpn-long-labels", |h| {
            {
                let mut st = h.chrome.borrow_mut();
                st.quick_settings.vpns = vec![
                    vpn("Corporate WireGuard tunnel for the Berlin office", true),
                    vpn("Home", false),
                ];
            }
            quick_settings_open(h, QsLevel::Vpn);
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
                // The first list row rests on the Control floor; the
                // panel padding beside it is the Panel ground.
                let panel = harness.host.quick_settings_rect();
                let list_top = if name == "qs-wifi-drillin" {
                    crate::shell::quick_settings::wifi::QuickSettingsWifiPane::LIST_TOP
                } else {
                    qs::LIST_TOP
                };
                let mut list = DrillInList::default();
                list.layout_from(panel, 1, list_top);
                let row = list.rows()[0];
                let rgba = renderer.render_rgba_resilient(&scene);
                expect(
                    &rgba,
                    Point::new(row.x0 + row.width() * 0.55, row.y0 + 3.0),
                    floor,
                    &format!("{theme_name}/{name}: a drill-in row's interior is the Control floor"),
                );
                expect(
                    &rgba,
                    Point::new(panel.x0 + qs::PANEL_PAD / 2.0, row.center().y),
                    panel_ground,
                    &format!("{theme_name}/{name}: the padding beside a row is the Panel ground"),
                );
            }
        }
        // The notification history, with more records than fit.
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
            // Dismissed toasts retire into the history through the
            // same path a real dismissal takes.
            for index in 0..3 {
                let id = st.push_notification_full(tilo_desktop::notifications::Notification::new(
                    format!("Backup {index} finished"),
                    format!("Snapshot {index} is on the workspace host."),
                ));
                st.dismiss_notification(id);
            }
            assert_eq!(st.notification_history().len(), 3);
            st.overlays.quick_settings = true;
            st.quick_settings.push_level(QsLevel::Notifications);
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
        assert!(
            (layout.panel().y1 - layout.row(2).y1 - qs::PANEL_PAD).abs() < 1e-6,
            "{theme_name}: a history that fits ends on the panel padding, not a pager band: {:?}",
            layout.panel()
        );
        // The palette over the desktop.
        let mut harness = directory_palette_fixture(vw, vh);
        settle_with_theme(&mut harness, vw, vh, &theme);
        let scene = paint_scene(&mut harness, &theme);
        let path = dir.join("palette.png");
        renderer.render_png(&scene, ground, &path);
        assert!(path.is_file(), "fixture missing: {}", path.display());
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
        assert_eq!(lens.x0, 20.0, "lens docks to the left inset");
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
        assert_eq!(lens.x0, 20.0);
        assert_eq!(lens.x1, 580.0);
        assert_eq!(lens.y1, 680.0);
    }

    fn compact_shortcut_help_keeps_content_inside_a_comfortable_footer_inset() {
        let h = build_harness_sized(800.0, 600.0);
        let panel = h.host.shortcut_help_panel_rect();
        let done = h.host.shortcut_help_done_rect();
        assert_eq!(panel.x0, 32.0);
        assert_eq!(panel.x1, 768.0);
        assert_eq!(panel.y0, 32.0);
        assert_eq!(panel.y1, 568.0);
        assert!(done.y0 >= panel.y0 + 16.0);
        assert!(done.y1 <= panel.y1 - 18.0);

        let body_bottom = panel.y1 - 22.0;
        let body_top = panel.y0 + 88.0;
        let navigation_height = 154.0;
        let essentials_y = body_top + navigation_height + 12.0;
        let compact_final_pill_bottom =
            essentials_y + 18.0 + 29.0 + 5.0 * shortcut_reference_step(true) + 31.0;
        assert!(
            compact_final_pill_bottom <= body_bottom,
            "compact help's final recovery shortcut must fit without clipping: pill={compact_final_pill_bottom}, body={body_bottom}"
        );
    }

    fn wide_shortcut_help_remains_a_contextual_card() {
        let h = build_harness_sized(1280.0, 800.0);
        let panel = h.host.shortcut_help_panel_rect();
        assert_eq!(panel.width(), 960.0);
        assert_eq!(panel.height(), 600.0);
        assert!(panel.x0 >= 150.0 && panel.x1 <= 1130.0);
        assert!(panel.y0 >= 100.0 && panel.y1 <= 700.0);
    }

    workspace_lens_leaves_wide_workspace_visible_and_interactive();
    workspace_lens_promotes_to_one_inset_sheet_on_narrow_outputs();
    compact_shortcut_help_keeps_content_inside_a_comfortable_footer_inset();
    wide_shortcut_help_remains_a_contextual_card();
}

#[test]
fn shortcut_guide_tab_stops_only_land_on_painted_bindings() {
    let (vw, vh) = (1280.0, 800.0);
    let mut h = build_harness_sized(vw, vh);
    h.chrome.borrow_mut().overlays.shortcut_help = true;
    settle(&mut h, vw, vh);
    let painted: Vec<settings_store::hardware::ShortcutAction> = h
        .host
        .shortcut_binding_rects
        .iter()
        .map(|(action, _)| *action)
        .collect();
    assert!(!painted.is_empty(), "the guide paints its bindings");

    let mut focused = Vec::new();
    for _ in 0..64 {
        let mut handled = false;
        assert!(h.host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::Tab),
            winit::event::Modifiers::default(),
            false,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
        ));
        match h.host.shortcut_guide_focus {
            Some(ShortcutGuideFocus::Binding(action)) => focused.push(action),
            Some(ShortcutGuideFocus::Done) => break,
            _ => {}
        }
    }
    let hidden: Vec<_> = focused
        .iter()
        .filter(|action| !painted.contains(action))
        .collect();
    assert!(
        hidden.is_empty(),
        "Tab must never focus a binding the guide does not draw: {hidden:?}"
    );
    assert_eq!(
        focused, painted,
        "Tab walks the painted bindings in painted order"
    );
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
    for key in [NamedKey::ArrowDown, NamedKey::ArrowRight, NamedKey::Enter] {
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
        crate::shell::quick_settings::QsLevel::Notifications
    ));
    settle(&mut h, width, height);
    let read = h
        .host
        .quick_settings_semantic_rects()
        .into_iter()
        .find(|(name, _)| name == "quick_settings_notification_read[0]")
        .expect("read button remains after returning")
        .1;
    assert!(pump(
        &mut h,
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(read.center().x, read.center().y),
        }
    ));
    assert!(pump(&mut h, &mouse_press()));
    assert!(h.host.notification_center.is_reading());
    settle(&mut h, width, height);
    let back = h.host.qs_notification_back_rect();
    assert!(pump(
        &mut h,
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(back.center().x, back.center().y),
        }
    ));
    assert!(pump(&mut h, &mouse_press()));
    assert!(!h.host.notification_center.is_reading());
    assert_eq!(h.chrome.borrow().notification_history().len(), original);
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(matches!(
        h.chrome.borrow().quick_settings.current_level(),
        crate::shell::quick_settings::QsLevel::Root
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
        for key in [NamedKey::Tab, NamedKey::Tab, NamedKey::Enter] {
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

#[path = "launcher_ranking_tests.rs"]
mod launcher_ranking_tests;

// Deterministic owned-room fixture: no policy override and no ambient identity.
fn workspace_name_owner_harness(width: f64, height: f64) -> (AbsorbHarness, tempfile::TempDir) {
    let profile = tempfile::tempdir().unwrap();
    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.adopt_ownerless_workspaces(&"shell-fixture".to_string());
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
            let (hint_width, hint_height) = footer_text.measure_style(
                &desktop_text("workspace-settings-edit-hint"),
                None,
                &theme.text(TextRole::Caption),
            );
            let inset = f64::from(theme.tokens.spacing.s6);
            let footer = Rect::new(
                panel.x1 - inset - f64::from(hint_width),
                panel.y1 - inset - f64::from(hint_height),
                panel.x1 - inset,
                panel.y1 - inset,
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
        let point = h.host.notification_stack.rect().center();
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
            let point = h.host.notification_stack.rect().center();
            pump(&mut h, &cursor_moved_at(point));
            assert!(pump(&mut h, &left_button_pressed()));
            assert!(pump(&mut h, &left_button_released()));
            assert!(h.chrome.borrow().notifications().is_empty());
            assert_eq!(h.chrome.borrow().notification_history().len(), 1);
        }
    }
}

fn capture_banner_pointer_state(
    h: &mut AbsorbHarness,
    theme: &Theme,
    width: u32,
    path: &std::path::Path,
) {
    let bounds = h.host.notification_stack.rect();
    let mut scene = Scene::new();
    h.host.notification_stack.draw(
        &ChromeNotifications::new(&h.chrome),
        &mut scene,
        Affine::translate((24.0 - bounds.x0, 24.0 - bounds.y0)),
        &mut TextRenderer::new(),
        theme,
    );
    offscreen::OffscreenRenderer::new(width, 300).render_png(
        &scene,
        theme.tokens.colors.surfaces.base,
        path,
    );
    assert!(path.is_file(), "the banner capture must exist");
}

fn banner_pointer_capture_variants() -> [(&'static str, Theme, f64, u32); 6] {
    [
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
    let out = std::env::var_os("TILO_BANNER_POINTER_EVIDENCE").map_or_else(
        || std::path::PathBuf::from("target/banner-pointer-review"),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&out).unwrap();
    let mut missing_feedback = Vec::new();
    for (name, theme, viewport, width) in banner_pointer_capture_variants() {
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
        let card = h.host.notification_stack.rect();
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
    }
    assert!(
        missing_feedback.is_empty(),
        "the actual banner pointer path must feed shared close hover: {missing_feedback:?}"
    );
}

#[test]
fn notification_banner_lost_release_does_not_capture_a_fresh_content_click() {
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
        let point = h.host.notification_stack.rect().center();
        pump(&mut h, &cursor_moved_at(point));
        assert!(pump(&mut h, &left_button_pressed()));
        if lost_focus {
            pump(&mut h, &WindowEvent::Focused(false));
            pump(&mut h, &WindowEvent::Focused(true));
        }
        // The old release is absent. A fresh click belongs to content.
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
