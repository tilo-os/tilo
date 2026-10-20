//! Headless end-to-end snapshot harness for tilo-shell flows. Drives the
//! widget tree through synthesised `winit::WindowEvent`s and a full
//! build/layout/draw cycle (no window, no compositor), writing frames to
//! `target/tilo-screenshots/e2e-*.png`. Captures decode to pixels; settled
//! decision states are checked against committed goldens.
//!
//! No clock test hook, so animations are driven by sleeping past
//! `OVERVIEW_TRANSITION_MS` between click and "settled" render.
//! `OffscreenRenderer::new` panics with no wgpu adapter (e.g. sandboxed CI);
//! each test catches that and skips rather than failing in a way that masks
//! unrelated regressions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use shell::shell::build_tilo_overlay_host_with_inputs;
use shell::shell::hosted_inputs::HostedTileInputs;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::room_activity::RoomActivityPane;
use shell::shell::state::WorkspaceTree;
use shell::shell::topbar_chrome::TopBarChrome;
use tilo_desktop::chrome::ClockSnapshot;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    AppKind, FloatingSurfaceProvider, TabBar, TabDescriptor, TileContent, Widget, WindowGroup,
    WindowWidth,
};

use ui::synthetic_input::{
    cursor_moved, key_press, mouse_input, mouse_press, mouse_release, named_key,
};

use crate::support::host::{Layout, dispatch, dispatch_in, lay_out};
use crate::support::snapshot::{SnapshotAssertion, pinned_golden_path};
use tilo_lock::MutexRecover as _;
use ui::offscreen::OffscreenRenderer;

/// Where explicit golden-update candidates are written for review.
fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("e2e")
}

// Shared harness plumbing

const W: f32 = 1600.0;
const H: f32 = 900.0;
// `as u32` on a compile-time canvas-size literal: 1600/900 are exactly
// representable, well under f32's 24-bit mantissa headroom, and both are
// already non-negative.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "W/H are small compile-time constants, exactly representable as u32"
)]
const W_U32: u32 = W as u32;
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "W/H are small compile-time constants, exactly representable as u32"
)]
const H_U32: u32 = H as u32;

/// PNG output root, under `target/tilo-screenshots/` so it's gitignored.
fn screenshots_dir() -> PathBuf {
    // CARGO_TARGET_DIR respects workspace overrides; default to `target/`
    // under the crate root, which `cargo test` uses.
    let root = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(3)
                .unwrap_or_else(|| Path::new("."))
                .join("target")
        },
        PathBuf::from,
    );
    root.join("tilo-screenshots")
}

/// `taffy` intentionally outlives each event so re-renders after mutation
/// can reuse it.
struct Harness {
    root: Box<dyn Widget>,
    taffy: TaffyTree<()>,
    root_node: NodeId,
    theme: Theme,
    tr: TextRenderer,
    doc: AutoCommit,
    chrome: shell::shell::chrome_state::Chrome,
}

impl Harness {
    fn new(tree: &WorkspaceTree) -> Self {
        Self::new_at(tree, W, H)
    }

    fn new_at(tree: &WorkspaceTree, width: f32, height: f32) -> Self {
        // The production source consumes the fixture's owned clock rather than
        // an ambient environment pin, which owned fixtures deliberately ignore.
        let inputs = HostedTileInputs::fixture()
            .expect("owned hosted inputs")
            .with_fixture_clock(ClockSnapshot {
                clock: "09:41".into(),
                ..ClockSnapshot::default()
            })
            .expect("owned fixture clock");
        let mut owned = tree.clone();
        // Rooms need recorded access, or access-reporting surfaces assert against
        // a state no user has ever been in.
        owned.adopt_ownerless_workspaces(&shell::shell::state::local_user_id());
        let (mut host, chrome) = build_tilo_overlay_host_with_inputs(
            owned.clone().shared(),
            false,
            ui::record::RecordStore::in_memory(),
            inputs,
        );
        // Headless snapshots must not inherit the developer machine's persisted
        // seen-frontier. That changes the activity-glance headline while leaving
        // the UI under test untouched, producing a machine- and run-dependent
        // golden. Production still uses the persistent pane.
        *host.room_activity_mut() = RoomActivityPane::new();
        let active_workspace = owned.desktop.active.raw();
        host.room_activity_mut().refresh(&mut owned);
        host.room_activity_mut().finish_view(active_workspace);
        let mut root = Box::new(host) as Box<dyn Widget>;
        let Layout { taffy, child_node } = lay_out(&mut *root, width, height);

        Self {
            root,
            taffy,
            root_node: child_node,
            theme: Theme::dark(),
            tr: TextRenderer::new(),
            doc: AutoCommit::new(),
            chrome,
        }
    }

    /// No taffy rebuild — that would discard animation state.
    fn draw_to(&mut self, renderer: Option<&mut OffscreenRenderer>, path: &Path) {
        self.root
            .apply_layout(&self.taffy, self.root_node, Point::ORIGIN);
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        self.root.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut self.tr,
            &self.theme,
            &presence,
        );
        if let Some(r) = renderer {
            // Opaque slate-950 so chrome composites over a consistent base everywhere.
            let bg = Color::from_rgb8(0x02, 0x06, 0x14);
            r.render_png(&scene, bg, path);
        }
    }

    fn click(&mut self, x: f64, y: f64) {
        self.pump(&cursor_moved(x, y));
        self.pump(&mouse_press());
        self.pump(&mouse_release());
    }

    fn pump(&mut self, event: &WindowEvent) -> bool {
        let modifiers = Modifiers::default();
        let (root, taffy) = (&mut *self.root, &self.taffy);
        dispatch_in(&mut self.doc, root, taffy, self.root_node, modifiers, event)
    }

    fn key(&mut self, key: Key) -> bool {
        self.pump(&key_press(key, None))
    }
}

// Test serialisation: tests share a wgpu/vello/cosmic-text static cache and
// have SIGSEGV'd in parallel, so each holds `TEST_LOCK` for its duration.
// `unwrap_or_else(|e| e.into_inner())` so a panicking test doesn't poison
// the mutex for the rest of the suite.
static TEST_LOCK: Mutex<()> = Mutex::new(());

// Test 1: open + close overview

/// The toolkit's clock (`ui::time`) for a golden run: every step reads the
/// next frame, `FRAME` apart, from one start, however long the machine
/// takes. A Notes tile's startup save is still unconfirmed when the run
/// begins, and on a slow builder the wall clock passed the save surface's
/// stall bound before the shot, so the overview golden sometimes caught the
/// "Saved" that ends a stalled save. On this clock the run spans a few
/// hundred milliseconds, every time. The overview's own crossfade keeps the
/// wall clock (`std::time`), which the run still sleeps through.
struct FrameClock {
    start: ui::time::Instant,
    frame: u32,
}

impl FrameClock {
    const FRAME: Duration = Duration::from_millis(50);

    fn new() -> Self {
        Self {
            start: ui::time::now(),
            frame: 0,
        }
    }

    /// Run `step` at the next frame's instant.
    fn step<T>(&mut self, step: impl FnOnce() -> T) -> T {
        let at = Self::FRAME
            .checked_mul(self.frame)
            .and_then(|offset| self.start.checked_add(offset))
            .expect("a run of frames stays within the clock's range");
        self.frame = self.frame.saturating_add(1);
        ui::time::testing::with_monotonic_time(at, step)
    }
}

#[test]
fn e2e_open_close_overview() {
    let _guard = TEST_LOCK.lock_recover();
    let mut clock = FrameClock::new();
    // bootstrap_demo already seeds 3 workspaces, so the Overview grid has
    // cards to paint without further appends.
    let tree = WorkspaceTree::bootstrap_demo();

    let theme = Theme::dark();
    let mut offscreen = OffscreenRenderer::try_new(W_U32, H_U32)
        .expect("overview pixel oracle requires an offscreen adapter");
    let out_dir = screenshots_dir();

    let mut h = clock.step(|| Harness::new(&tree));
    clock.step(|| h.draw_to(Some(&mut offscreen), &out_dir.join("e2e-01-desktop.png")));

    // The probe only computes the workspace name's centre; the click flows
    // through `ShellOverlayHost::handle_event` and opens the workspace
    // menu, whose Overview row opens the overview.
    let hit = clock.step(|| workspace_pill_hit(&tree, &theme, h.chrome.clone()));
    clock.step(|| h.click(hit.x, hit.y));
    clock.step(|| {
        let host = h
            .root
            .as_any_mut()
            .and_then(|widget| widget.downcast_mut::<ShellOverlayHost>())
            .expect("the root is the shell overlay host");
        assert!(
            host.workspace_menu_labels().is_some(),
            "root-level click on the workspace name should open the workspace menu"
        );
        assert!(host.choose_in_workspace_menu("Overview"));
    });
    assert!(
        h.chrome.borrow().overlays.overview,
        "the workspace menu's Overview row should open the overview overlay"
    );

    // `tick_overview_transition` only latches `transition_started` on the
    // first draw after the target changes, so `needs_animation_frame()`
    // reads `false` until that draw runs — without this priming draw the
    // loop below exits immediately and captures the pre-fade desktop.
    clock.step(|| {
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        h.root
            .draw(&mut scene, Affine::IDENTITY, &mut h.tr, &h.theme, &presence);
    });

    // `needs_animation_frame()` is a weak loop guard so this can't sleep forever.
    let deadline = Instant::now() + Duration::from_millis(600);
    while clock.step(|| h.root.needs_animation_frame()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        // Re-draw so the tick functions see elapsed time — they advance on draw,
        // not on wall clock.
        clock.step(|| {
            let mut scene = Scene::new();
            let presence = PresenceCtx::default();
            h.root
                .draw(&mut scene, Affine::IDENTITY, &mut h.tr, &h.theme, &presence);
        });
    }

    clock.step(|| {
        h.draw_to(
            Some(&mut offscreen),
            &out_dir.join("e2e-02-overview-open.png"),
        );
    });

    clock.step(|| h.pump(&named_key(NamedKey::Escape)));
    assert!(
        !h.chrome.borrow().overlays.overview,
        "Escape should close the overview overlay"
    );

    clock.step(|| {
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        h.root
            .draw(&mut scene, Affine::IDENTITY, &mut h.tr, &h.theme, &presence);
    });

    let deadline = Instant::now() + Duration::from_millis(600);
    while clock.step(|| h.root.needs_animation_frame()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        clock.step(|| {
            let mut scene = Scene::new();
            let presence = PresenceCtx::default();
            h.root
                .draw(&mut scene, Affine::IDENTITY, &mut h.tr, &h.theme, &presence);
        });
    }
    clock.step(|| {
        h.draw_to(
            Some(&mut offscreen),
            &out_dir.join("e2e-03-overview-closed.png"),
        );
    });

    // The pixel oracle must observe the actual rendered production frames.
    for name in [
        "e2e-01-desktop.png",
        "e2e-02-overview-open.png",
        "e2e-03-overview-closed.png",
    ] {
        assert_png_nonempty(&out_dir.join(name));
    }
    assert_frames_visibly_differ(
        &out_dir.join("e2e-01-desktop.png"),
        &out_dir.join("e2e-02-overview-open.png"),
    );
    // Only the settled open-overview frame is pinned — pre-click and post-Esc
    // are the same desktop steady state.
    let rendered = out_dir.join("e2e-02-overview-open.png");
    SnapshotAssertion::new(
        rendered,
        goldens_dir().join("e2e-open-close-overview-settled.png"),
    )
    .assert();
}

/// Production notification-stack review seed for failed application opens.
/// This deliberately goes through `Harness` (the same overlay host, layout,
/// and desktop layer used by the other e2e shots), rather than drawing a
/// notification card in isolation. Keep the PNGs out of goldens: the purpose
/// is a human 1:1 review at both normal and accessibility contrast.
#[test]
#[ignore = "visual review: writes production overlay PNGs and needs an offscreen adapter"]
fn e2e_launch_failure_notifications_visual_review() {
    let _guard = TEST_LOCK.lock_recover();
    let tree = WorkspaceTree::bootstrap_demo();
    let out_dir = screenshots_dir().join("launch-failure-review");
    std::fs::create_dir_all(&out_dir).expect("create launch notification review directory");
    for (width, height, layout_width, layout_height) in [
        (1280_u32, 720_u32, 1280.0_f32, 720.0_f32),
        (1600, 900, 1600.0, 900.0),
    ] {
        let Some(mut renderer) = OffscreenRenderer::try_new(width, height).ok() else {
            eprintln!("offscreen: no wgpu adapter — launch notification review skipped");
            return;
        };
        let mut h = Harness::new_at(&tree, layout_width, layout_height);
        {
            let mut chrome = h.chrome.borrow_mut();
            chrome.push_notification_full(
                tilo_desktop::notifications::Notification::new(
                    "Couldn't open LibreOffice Calc — profile initialization failed after a very long diagnostic suffix",
                    "The app stopped before opening. Open Applications and try again.",
                )
                .with_severity(tilo_desktop::notifications::NotificationSeverity::Error)
                .with_app_name("Applications"),
            );
            chrome.push_notification_full(
                tilo_desktop::notifications::Notification::new(
                    "Couldn't open Notes",
                    "The app launcher is unavailable. Open Applications and try again.",
                )
                .with_severity(tilo_desktop::notifications::NotificationSeverity::Error)
                .with_app_name("Applications"),
            );
        }
        let dark = out_dir.join(format!("{width}x{height}-dark.png"));
        let high_contrast = out_dir.join(format!("{width}x{height}-high-contrast.png"));
        h.draw_to(Some(&mut renderer), &dark);
        h.theme = Theme::high_contrast();
        h.draw_to(Some(&mut renderer), &high_contrast);
        for path in [&dark, &high_contrast] {
            assert_png_nonempty(path);
        }
    }
    println!("launch-failure review PNGs: {}", out_dir.display());
}

/// Centre of the topbar's workspace name, from a sibling `TopBarChrome` —
/// topbar geometry is width-driven, so its rect matches.
fn workspace_pill_hit(
    tree: &WorkspaceTree,
    theme: &Theme,
    chrome: shell::shell::chrome_state::Chrome,
) -> Point {
    let (_, centre) = probe_topbar(
        tree,
        theme,
        chrome,
        shell::shell::topbar_chrome::TopBarChrome::workspace_menu_rect,
    );
    centre
}

fn tray_pill_hit(
    tree: &WorkspaceTree,
    theme: &Theme,
    chrome: shell::shell::chrome_state::Chrome,
) -> Point {
    let (_, centre) = probe_topbar(
        tree,
        theme,
        chrome,
        shell::shell::topbar_chrome::TopBarChrome::tray_pill_rect,
    );
    centre
}

/// Shared probe helper: lays out a sibling `TopBarChrome` at the shell's
/// topbar origin/width and returns the `extract`-selected rect + its
/// centre. Matching viewport width is what lines up the right-edge
/// cluster (tray pill, avatar stack) with the live root.
fn probe_topbar(
    tree: &WorkspaceTree,
    theme: &Theme,
    chrome: shell::shell::chrome_state::Chrome,
    extract: impl FnOnce(&TopBarChrome) -> vello::kurbo::Rect,
) -> (vello::kurbo::Rect, Point) {
    let mut bar = TopBarChrome::from_tree(&tree.desktop)
        .with_chrome(shell::shell::topbar_chrome::native_source(chrome));
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, theme, &mut map);
    // The shell's Container + Column use `with_no_padding`, so the topbar is
    // flush at y=0/x=0 and the probe matches at zero offset.
    let topbar_offset = 0.0_f64;
    let probe_w = W;
    let vp = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(probe_w, H),
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    taffy
        .compute_layout_with_measure(
            vp,
            Size {
                width: AvailableSpace::Definite(probe_w),
                height: AvailableSpace::Definite(H),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .expect("compute_layout");
    bar.apply_layout(&taffy, node, Point::new(topbar_offset, topbar_offset));
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    bar.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    let r = extract(&bar);
    let centre = Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1));
    (r, centre)
}

// Test 2: tab drag-to-reorder (Files -> after Browser)

/// The standalone `WindowGroup` for [`e2e_tab_drag_reorder`], split out to
/// keep that test short. Four stub tabs, avoiding full-root plumbing; tab 1
/// (Files) is the drag source, tab 3 (Browser) the drop target, and tab 2 is
/// active so the reorder can't also change the active id.
fn setup_drag_reorder_group() -> (WindowGroup, TaffyTree<()>, NodeId, TextRenderer, Theme) {
    let tabs: Vec<(TabDescriptor, TileContent)> = vec![
        tab_stub(1, "Notes", AppKind::Editor, "notes"),
        tab_stub(2, "Files", AppKind::Files, "files"),
        tab_stub(3, "Terminal", AppKind::Terminal, "terminal"),
        tab_stub(4, "Browser", AppKind::Browser, "browser"),
    ];
    let mut standalone = WindowGroup::new(100, tabs, /* active_id */ 1, WindowWidth::Normal);
    // A width is a fraction of its strip; with no `GroupRow`, tell the group the
    // 960 px capture surface *is* its strip, or it uses the toolkit fallback.
    standalone.set_strip_usable_px(960.0);
    let mut group = standalone;

    let Layout {
        taffy,
        child_node: child,
    } = lay_out(&mut group, 960.0, 540.0);
    (group, taffy, child, TextRenderer::new(), Theme::dark())
}

/// Bundles the widget/layout/doc plumbing [`pump`] needs, so
/// [`e2e_tab_drag_reorder`]'s drag-gesture helpers don't each need eight
/// parameters of their own.
struct DragCtx<'a> {
    group: &'a mut WindowGroup,
    taffy: &'a TaffyTree<()>,
    child: NodeId,
    doc: &'a mut AutoCommit,
}

impl DragCtx<'_> {
    fn pump_event(&mut self, event: &WindowEvent) {
        let modifiers = Modifiers::default();
        dispatch_in(
            self.doc, self.group, self.taffy, self.child, modifiers, event,
        );
    }

    fn move_to(&mut self, p: Point) {
        self.pump_event(&cursor_moved(p.x, p.y));
    }

    fn button(&mut self, state: ElementState) {
        self.pump_event(&mouse_input(state, MouseButton::Left));
    }
}

/// Press on `from` and drag to `to` in three steps, crossing the 8 px
/// `DRAG_START` threshold — everything [`e2e_tab_drag_reorder`]'s gesture
/// needs before its midway screenshot. Split out to keep that test short.
fn press_and_drag_to(ctx: &mut DragCtx<'_>, from: Point, to: Point) {
    ctx.move_to(from);
    ctx.button(ElementState::Pressed);
    for step in [0.33_f64, 0.66, 1.0] {
        ctx.move_to(Point::new(
            from.x + (to.x - from.x) * step,
            from.y + (to.y - from.y) * step,
        ));
    }
}

/// Draw `group`'s current state and save it as a PNG, when an offscreen
/// renderer is available. Split out of [`e2e_tab_drag_reorder`] to keep
/// that test short — its three capture points are otherwise identical.
fn render_frame_to(
    group: &mut WindowGroup,
    tr: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    renderer_opt: &mut Option<OffscreenRenderer>,
    path: &Path,
) {
    if let Some(r) = renderer_opt.as_mut() {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, tr, theme, presence);
        if group.drag_in_flight() {
            FloatingSurfaceProvider::draw_floating_surfaces(
                group,
                &mut s,
                Affine::IDENTITY,
                tr,
                theme,
                presence,
            );
        }
        r.render_png(&s, Color::from_rgb8(0x02, 0x06, 0x14), path);
    }
}

#[test]
fn e2e_tab_drag_reorder() {
    let _guard = TEST_LOCK.lock_recover();
    let (mut group, taffy, child, mut tr, theme) = setup_drag_reorder_group();
    let group_rect = group.get_rect();
    let content_rect = group
        .active_host_rect_for_tests()
        .expect("the active tab must have laid-out content");
    assert_eq!(group_rect, vello::kurbo::Rect::new(0.0, 0.0, 640.0, 540.0));
    assert!(
        content_rect.width() >= 639.0 && content_rect.height() >= 490.0,
        "the drag harness must render a real window body, got {content_rect:?}",
    );
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    group.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let mut renderer_opt = OffscreenRenderer::try_new(960, 540).ok();
    let out_dir = screenshots_dir();
    render_frame_to(
        &mut group,
        &mut tr,
        &theme,
        &presence,
        &mut renderer_opt,
        &out_dir.join("e2e-04-tabs-initial.png"),
    );

    // `bar` isn't exposed, so read tab geometry from a parallel TabBar.
    let tab_centers = probe_tab_centers();
    let from = tab_centers[1]; // Files pill centre
    let to = tab_centers[3]; // Browser pill centre

    let mut doc = AutoCommit::new();

    // Cross the 8 px DRAG_START threshold in steps, capturing one mid-drag
    // frame so the drop-zone highlight is visible. Each `DragCtx` below is a
    // temporary borrowing `group`/`doc`, so it releases them again
    // before the render call in between needs its own borrow of `group`.
    press_and_drag_to(
        &mut DragCtx {
            group: &mut group,
            taffy: &taffy,
            child,
            doc: &mut doc,
        },
        from,
        to,
    );

    // Pressing the dragged pill also activates it. Production rebuilds the
    // group for that new active host before the next frame; retaining the old
    // Notes-only taffy child made both the midway and settled golden paint no
    // app body at all.
    let Layout {
        taffy,
        child_node: child,
    } = lay_out(&mut group, 960.0, 540.0);

    render_frame_to(
        &mut group,
        &mut tr,
        &theme,
        &presence,
        &mut renderer_opt,
        &out_dir.join("e2e-05-tab-drag-midway.png"),
    );

    // `MouseInput::Released` reads the cached `last_cursor`, so the drag-to
    // step above must already have landed on `to`.
    DragCtx {
        group: &mut group,
        taffy: &taffy,
        child,
        doc: &mut doc,
    }
    .button(ElementState::Released);
    // Keep the settled golden independent of hover-reveal wall time. The
    // pointer has completed the drag and production would ordinarily move on
    // to the app body; parking it there also proves the app body is present.
    DragCtx {
        group: &mut group,
        taffy: &taffy,
        child,
        doc: &mut doc,
    }
    .move_to(Point::new(900.0, 500.0));

    assert_eq!(
        group.take_pending_tab_reorder(),
        Some((1, 3)),
        "the gesture itself must publish Files -> after Browser; a parallel probe cannot prove it",
    );
    assert_eq!(
        group.tile_kind_labels_for_tests(),
        vec!["Editor", "Terminal", "Browser", "Files"],
        "the actual content hosts must follow the reordered tab strip",
    );
    lay_out(&mut group, 960.0, 540.0);
    group.bar_mut_for_tests().force_settled_for_tests();
    lay_out(&mut group, 960.0, 540.0);
    assert!(
        group.bar_mut_for_tests().is_settled_for_tests(),
        "the golden must sample a static tab row, not wall-clock animation"
    );
    // One event-less draw primes title shaping at the pinned resting state.
    {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
    }
    lay_out(&mut group, 960.0, 540.0);

    render_frame_to(
        &mut group,
        &mut tr,
        &theme,
        &presence,
        &mut renderer_opt,
        &out_dir.join("e2e-06-tabs-reordered.png"),
    );

    assert_drag_reorder_goldens(renderer_opt.as_ref(), &out_dir);
}

/// The three captures' non-emptiness plus the settled-frame golden, or a
/// skip notice with no wgpu adapter. Split out of [`e2e_tab_drag_reorder`]
/// to keep that test short.
fn assert_drag_reorder_goldens(renderer_opt: Option<&OffscreenRenderer>, out_dir: &Path) {
    if renderer_opt.is_some() {
        for name in [
            "e2e-04-tabs-initial.png",
            "e2e-05-tab-drag-midway.png",
            "e2e-06-tabs-reordered.png",
        ] {
            assert_png_nonempty(&out_dir.join(name));
        }
        assert_frames_visibly_differ(
            &out_dir.join("e2e-04-tabs-initial.png"),
            &out_dir.join("e2e-06-tabs-reordered.png"),
        );
        // Only the post-spring frame is pinned — midway is frame-timer driven and
        // inherently flaky.
        let rendered = out_dir.join("e2e-06-tabs-reordered.png");
        SnapshotAssertion::new(
            rendered,
            pinned_golden_path(goldens_dir().join("e2e-tab-drag-reorder-settled.png")),
        )
        .assert();
    } else {
        eprintln!("offscreen: no wgpu adapter — e2e_tab_drag_reorder asserted drop-order only");
    }
}

fn tab_stub(id: u64, title: &str, kind: AppKind, label: &str) -> (TabDescriptor, TileContent) {
    (
        TabDescriptor {
            id,
            title: title.into(),
            app_kind: Some(kind),
            icon: None,
        },
        TileContent::Stub {
            kind,
            label: label.into(),
        },
    )
}

/// Each tab pill's centre, via a parallel `TabBar` run through the same
/// build/compute/apply/draw sequence as `WindowGroup`.
/// Build layout, measure, apply, and draw once — the boilerplate shared by
/// both tab-bar probes in [`probe_tab_centers`]. Split out to keep that
/// function short.
fn build_measure_and_draw(widget: &mut dyn Widget, theme: &Theme, presence: &PresenceCtx) {
    lay_out(widget, 960.0, 40.0);
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    widget.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, presence);
}

fn probe_tab_centers() -> [Point; 4] {
    let descs = vec![
        TabDescriptor {
            id: 1,
            title: "Notes".into(),
            app_kind: Some(AppKind::Editor),
            icon: None,
        },
        TabDescriptor {
            id: 2,
            title: "Files".into(),
            app_kind: Some(AppKind::Files),
            icon: None,
        },
        TabDescriptor {
            id: 3,
            title: "Terminal".into(),
            app_kind: Some(AppKind::Terminal),
            icon: None,
        },
        TabDescriptor {
            id: 4,
            title: "Browser".into(),
            app_kind: Some(AppKind::Browser),
            icon: None,
        },
    ];
    let theme = Theme::dark();
    let presence = PresenceCtx::default();
    let mut bar: Box<dyn Widget> = Box::new(TabBar::new("g100", descs.clone(), 1));
    build_measure_and_draw(&mut *bar, &theme, &presence);

    // `tab_rects` isn't reachable through `Box<dyn Widget>`; rebuild as concrete.
    let mut concrete = TabBar::new("g100", descs, 1);
    build_measure_and_draw(&mut concrete, &theme, &presence);
    let rs = concrete.tab_rects();
    // Fall back to evenly-spaced guesses if layout returned fewer than 4 rects,
    // so a failure reports usefully instead of panicking on an index.
    // `i` is only ever one of the literal 0..=3 below, never large enough to
    // make the widening-to-narrowing round trip lossy.
    #[allow(
        clippy::cast_precision_loss,
        reason = "i is one of the literal tab indices 0..=3 below, never large"
    )]
    let pick = |i: usize| {
        rs.get(i).copied().unwrap_or_else(|| {
            let gap = 960.0 / 4.0;
            vello::kurbo::Rect::new(i as f64 * gap, 0.0, (i as f64 + 1.0) * gap, 40.0)
        })
    };
    [
        center(pick(0)),
        center(pick(1)),
        center(pick(2)),
        center(pick(3)),
    ]
}

fn center(r: vello::kurbo::Rect) -> Point {
    Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
}

// Test 3: open quick settings via the tray pill

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn overlay_host_keys_and_corner_clicks_open_close_and_navigate_overlays() {
    // Phase `overlay_host_keys_open_close_and_navigate_search_overview_and_tabs`.
    {
        // Phase `e2e_super_k_opens_search_palette_in_live_binary_path`.
        {
            let _guard = TEST_LOCK.lock_recover();
            // Use the concrete-type sibling of what the live binary builds.
            let tree = WorkspaceTree::bootstrap_demo();
            let (mut host, _chrome) = shell::shell::build_tilo_overlay_host_with_chrome(&tree);
            assert!(!host.palette_visible(), "palette starts closed");

            let mods = Modifiers::from(winit::keyboard::ModifiersState::SUPER);
            let handled = pump_host(&mut host, &named_key(NamedKey::Space), mods);
            assert!(handled, "Super+Space must be consumed by the overlay host");
            assert!(
                host.palette_visible(),
                "Super+Space must open the global-search palette in the live-binary event path"
            );
        }

        // Phase `e2e_escape_closes_search_palette`.
        {
            let _guard = TEST_LOCK.lock_recover();
            let tree = WorkspaceTree::bootstrap_demo();
            let (mut host, _chrome) = shell::shell::build_tilo_overlay_host_with_chrome(&tree);

            // Open via Super+Space rather than bypassing the host's state machine, guarding
            // against a mutation-ordering regression.
            let mods = Modifiers::from(winit::keyboard::ModifiersState::SUPER);
            let _ = pump_host(&mut host, &named_key(NamedKey::Space), mods);
            assert!(host.palette_visible());

            // Escape priority order: palette > overview > ws_settings > quick_settings.
            let no_mods = Modifiers::default();
            let handled = pump_host(&mut host, &named_key(NamedKey::Escape), no_mods);
            assert!(handled, "Escape must be consumed while the palette is open");
            assert!(
                !host.palette_visible(),
                "Escape must close the global-search palette"
            );
        }

        // Phase `e2e_ctrl_tab_cycles_tabs_without_opening_overview`.
        {
            let _guard = TEST_LOCK.lock_recover();
            let tree = WorkspaceTree::bootstrap_demo();
            let (mut host, chrome) = shell::shell::build_tilo_overlay_host_with_chrome(&tree);
            assert!(!chrome.borrow().overlays.overview);
            let before =
                chrome.borrow().tree().desktop.workspaces[0].strips[0].window_groups[0].active_tab;

            // Ctrl+Tab belongs to the focused app first (keyboard_focus_wire
            // and app_tabs cover it); the desktop's own spelling adds Super.
            let mods = Modifiers::from(
                winit::keyboard::ModifiersState::SUPER | winit::keyboard::ModifiersState::CONTROL,
            );
            let handled = pump_host(&mut host, &named_key(NamedKey::Tab), mods);
            assert!(
                handled,
                "Super+Ctrl+Tab must be consumed by the overlay host"
            );
            assert!(
                !chrome.borrow().overlays.overview,
                "Super+Ctrl+Tab is tab navigation; only Super+Tab opens the overview"
            );
            let after =
                chrome.borrow().tree().desktop.workspaces[0].strips[0].window_groups[0].active_tab;
            assert_ne!(after, before, "Super+Ctrl+Tab must select the next tab");

            let backwards = Modifiers::from(
                winit::keyboard::ModifiersState::SUPER
                    | winit::keyboard::ModifiersState::CONTROL
                    | winit::keyboard::ModifiersState::SHIFT,
            );
            assert!(pump_host(&mut host, &named_key(NamedKey::Tab), backwards));
            assert_eq!(
                chrome.borrow().tree().desktop.workspaces[0].strips[0].window_groups[0].active_tab,
                before,
                "Super+Ctrl+Shift+Tab must return to the previous tab"
            );
        }

        // Phase `e2e_super_tab_toggles_overview_in_live_binary_path`.
        {
            let _guard = TEST_LOCK.lock_recover();
            let tree = WorkspaceTree::bootstrap_demo();
            let (mut host, chrome) = shell::shell::build_tilo_overlay_host_with_chrome(&tree);
            assert!(!chrome.borrow().overlays.overview);

            let mods = Modifiers::from(winit::keyboard::ModifiersState::SUPER);
            let handled = pump_host(&mut host, &named_key(NamedKey::Tab), mods);
            assert!(handled, "Super+Tab must be consumed by the overlay host");
            assert!(
                chrome.borrow().overlays.overview,
                "Super+Tab must toggle the overview overlay in the live-binary event path"
            );
        }

        // Phase `e2e_overview_arrows_select_and_enter_switches_room`.
        // The overview finishes on the keyboard: arrows walk the room list, Enter
        // commits and closes.
        {
            let _guard = TEST_LOCK.lock_recover();
            let tree = WorkspaceTree::bootstrap_demo();
            let started_on = tree.desktop.active;
            assert!(
                tree.desktop.workspaces.len() > 1,
                "precondition: the fixture has somewhere else to go"
            );
            let (mut host, chrome) = shell::shell::build_tilo_overlay_host_with_chrome(&tree);

            let super_key = Modifiers::from(winit::keyboard::ModifiersState::SUPER);
            assert!(pump_host(&mut host, &named_key(NamedKey::Tab), super_key));
            assert!(
                chrome.borrow().overlays.overview,
                "precondition: panel is open"
            );

            let no_mods = Modifiers::default();
            let handled = pump_host(&mut host, &named_key(NamedKey::ArrowDown), no_mods);
            assert!(
                handled,
                "the open panel must consume the arrow instead of letting the \
                 desktop underneath it scroll"
            );

            let handled = pump_host(&mut host, &named_key(NamedKey::Enter), no_mods);
            assert!(handled, "Enter must be consumed by the open panel");
            assert!(
                !chrome.borrow().overlays.overview,
                "committing to a room closes the switcher"
            );
            assert_ne!(
                chrome.borrow().tree().desktop.active,
                started_on,
                "Enter must actually go to the room the arrow selected"
            );
        }

        // Phase `e2e_overview_arrows_are_not_swallowed_while_the_panel_is_closed`.
        // Arrows belong to the switcher only while it's open. Driven through the
        // root/layout since the claim concerns a key the shell declines — the
        // event must actually reach the content.
        {
            let _guard = TEST_LOCK.lock_recover();
            let tree = WorkspaceTree::bootstrap_demo();
            let mut h = Harness::new(&tree);
            assert!(
                !h.chrome.borrow().overlays.overview,
                "precondition: panel shut"
            );

            let before = h.chrome.borrow().tree().desktop.active;
            h.key(Key::Named(NamedKey::ArrowDown));
            h.key(Key::Named(NamedKey::Enter));
            assert!(
                !h.chrome.borrow().overlays.overview,
                "arrows and Enter must not open the switcher"
            );
            assert_eq!(
                h.chrome.borrow().tree().desktop.active,
                before,
                "with the panel shut, Enter must not switch rooms"
            );
        }
    }

    // Phase `e2e_quick_settings_open_from_the_tray_and_the_top_right_corner`.
    {
        // Phase `e2e_open_quick_settings`.
        {
            let _guard = TEST_LOCK.lock_recover();
            let tree = WorkspaceTree::bootstrap_demo();
            let theme = Theme::dark();

            let mut renderer_opt = OffscreenRenderer::try_new(W_U32, H_U32).ok();
            let out_dir = screenshots_dir();

            let mut h = Harness::new(&tree);
            assert!(!h.chrome.borrow().overlays.quick_settings);

            // A draw pass populates `tray_pill_rect`, which handle_event hit-tests.
            h.draw_to(
                renderer_opt.as_mut(),
                &out_dir.join("e2e-06-quick-settings-pre.png"),
            );

            // The click reaches TopBarChrome.
            let hit = tray_pill_hit(&tree, &theme, h.chrome.clone());
            h.click(hit.x, hit.y);
            assert!(
                h.chrome.borrow().overlays.quick_settings,
                "root-level click on the three-dot tray pill should open the quick-settings overlay"
            );

            // The open intent is stored in shared chrome state, while the retained
            // host owns the transition. Pin that production transition at its resting
            // value so this assertion captures Quick Settings rather than a valid
            // state whose first animation frame is still fully transparent.
            h.root
                .as_any_mut()
                .and_then(|widget| widget.downcast_mut::<ShellOverlayHost>())
                .expect("shell integration root remains a ShellOverlayHost")
                .force_quick_settings_settled();

            h.draw_to(
                renderer_opt.as_mut(),
                &out_dir.join("e2e-07-quick-settings-open.png"),
            );
            if renderer_opt.is_some() {
                let before = out_dir.join("e2e-06-quick-settings-pre.png");
                let rendered = out_dir.join("e2e-07-quick-settings-open.png");
                assert_png_nonempty(&rendered);
                let panel = h
                    .root
                    .as_any_mut()
                    .and_then(|widget| widget.downcast_mut::<ShellOverlayHost>())
                    .expect("shell integration root remains a ShellOverlayHost")
                    .quick_settings_rect_for_tests();
                assert_region_visibly_differs(&before, &rendered, panel, 1_000);
                // The topbar clock (chrono::Local) shows behind the quick-settings
                // popover, making this render wall-clock-dependent — a golden would flake
                // once a minute. Compare only the panel's deterministic footprint so
                // a boolean open state can never pass while the overlay is absent.
            } else {
                eprintln!(
                    "offscreen: no wgpu adapter — e2e_open_quick_settings asserted state only"
                );
            }
        }

        // Phase `e2e_top_right_corner_opens_quick_settings`.
        // The visual status pill is intentionally inset, but the screen corner is
        // the fastest acquisition target. A hurried click at the literal top-right
        // pixel must therefore reach the same production root path as the pill.
        {
            let _guard = TEST_LOCK.lock_recover();
            let tree = WorkspaceTree::bootstrap_demo();
            let mut h = Harness::new(&tree);
            assert!(!h.chrome.borrow().overlays.quick_settings);

            // Populate the retained top-bar geometry before the pointer event, exactly
            // as the compositor does on its first frame.
            h.draw_to(None, Path::new(""));
            h.click(f64::from(W_U32 - 1), 1.0);

            assert!(
                h.chrome.borrow().overlays.quick_settings,
                "the literal top-right screen corner must acquire Quick Settings even though the visible pill is inset"
            );
        }
    }
}

// Test 4: a maximized window keeps its bar, with Restore in it
//
// Standalone `WindowGroup`, not the full shell tree: `WindowGroup.maximized`
// isn't reachable through the shell's `Box<dyn Widget>` tree. This still
// exercises the full click pipeline (control hit-test -> pending_max ->
// take_max -> flip maximized -> the maximized bar's draw).
/// The standalone `WindowGroup` for
/// [`e2e_maximized_window_keeps_its_bar`], split out to keep that test
/// short.
fn setup_maximize_test_group() -> (WindowGroup, TaffyTree<()>, NodeId, TextRenderer, Theme) {
    let tabs: Vec<(TabDescriptor, TileContent)> = vec![
        tab_stub(1, "Notes", AppKind::Editor, "notes"),
        tab_stub(2, "Files", AppKind::Files, "files"),
        tab_stub(3, "Terminal", AppKind::Terminal, "terminal"),
        tab_stub(4, "Browser", AppKind::Browser, "browser"),
    ];
    let mut group = WindowGroup::new(200, tabs, /* active_id */ 1, WindowWidth::Normal);
    // See `e2e_tab_drag_reorder`: no row here, so the capture surface is the strip.
    group.set_strip_usable_px(960.0);
    assert!(!group.maximized, "group starts un-maximized");

    let Layout {
        taffy,
        child_node: child,
    } = lay_out(&mut group, 960.0, 540.0);
    (group, taffy, child, TextRenderer::new(), Theme::dark())
}

/// Click the window's own Maximize control at the end of its bar, leaving
/// `group.maximized == true`. Split out of
/// [`e2e_maximized_window_keeps_its_bar`] to keep that test short.
fn click_maximize_control(group: &mut WindowGroup, taffy: &TaffyTree<()>, child: NodeId) {
    // `apply_layout` places the bar's controls.
    group.apply_layout(taffy, child, Point::ORIGIN);
    let max_center = group
        .presentation_bounds()
        .maximize
        .expect("the window's bar offers Maximize")
        .center();
    let mut doc = AutoCommit::new();
    // Direct `group.handle_event` (not the boxed `pump`) keeps `.maximized`
    // reachable on the concrete type.
    let mut pump_ev = |group: &mut WindowGroup, event: WindowEvent| {
        dispatch_in(&mut doc, group, taffy, child, Modifiers::default(), &event);
    };
    pump_ev(group, cursor_moved(max_center.x, max_center.y));
    pump_ev(group, mouse_press());
    // The control acts on release over itself, like every button.
    pump_ev(group, mouse_release());

    assert!(
        group.maximized,
        "clicking the window's Maximize control should toggle `maximized = true`"
    );
}

/// Render `group`, save the PNG, and check it against the golden (or note
/// the offscreen skip). Split out of
/// [`e2e_maximized_window_keeps_its_bar`] to keep that test short.
fn assert_maximized_tile_golden(
    group: &mut WindowGroup,
    tr: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    renderer_opt: &mut Option<OffscreenRenderer>,
    out_dir: &Path,
) {
    if let Some(r) = renderer_opt.as_mut() {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, tr, theme, presence);
        r.render_png(
            &s,
            Color::from_rgb8(0x02, 0x06, 0x14),
            out_dir.join("e2e-maximized-tile-controls.png"),
        );
        let rendered = out_dir.join("e2e-maximized-tile-controls.png");
        assert_png_nonempty(&rendered);
        // The maximized window's bar is what regresses if its controls drift.
        SnapshotAssertion::new(
            rendered,
            pinned_golden_path(goldens_dir().join("e2e-maximized-tile-controls.png")),
        )
        .assert();
    } else {
        eprintln!(
            "offscreen: no wgpu adapter — e2e_maximized_window_keeps_its_bar asserted state only"
        );
    }
}

#[test]
fn e2e_maximized_window_keeps_its_bar() {
    let _guard = TEST_LOCK.lock_recover();
    let (mut group, taffy, child, mut tr, theme) = setup_maximize_test_group();

    // Prime the bar.
    let presence = PresenceCtx::default();
    {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    click_maximize_control(&mut group, &taffy, child);

    // Maximizing switches the group from its tiled fraction to the full
    // viewport. Exercise the same rebuild the shell schedules after the
    // maximize state change.
    lay_out(&mut group, 960.0, 540.0);
    let maximized_rect = group.get_rect();
    assert_eq!(
        maximized_rect,
        vello::kurbo::Rect::new(0.0, 0.0, 960.0, 540.0),
        "maximized content must cover the complete output",
    );

    // A maximized window keeps its bar, the same bar in the same place, its
    // maximize control now Restore; the client stands under it.
    let bounds = group.presentation_bounds();
    let restore = bounds
        .restore
        .expect("the maximized window's bar offers Restore");
    assert!(
        bounds.tab_bar.height() > 0.0 && bounds.tab_bar.contains(restore.center()),
        "Restore stands in the window's bar: {restore:?} in {:?}",
        bounds.tab_bar
    );
    let client = bounds.client.expect("the client is laid out");
    assert!(
        client.y0 >= bounds.tab_bar.y1 - 0.5 && client.y1 <= maximized_rect.y1 + 0.5,
        "the client stands under the bar: {client:?}",
    );

    // Defensive: settle any spawn-fade timers still pumping.
    std::thread::sleep(Duration::from_millis(50));
    {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    let mut renderer_opt = OffscreenRenderer::try_new(960, 540).ok();
    let out_dir = screenshots_dir();
    assert_maximized_tile_golden(
        &mut group,
        &mut tr,
        &theme,
        &presence,
        &mut renderer_opt,
        &out_dir,
    );
}

fn assert_png_nonempty(path: &Path) {
    let meta = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("expected PNG at {}: {e}", path.display()));
    assert!(
        meta.len() > 8,
        "PNG at {} should be non-empty",
        path.display()
    );
    let image = decode_rgba(path);
    assert!(
        image.width > 16 && image.height > 16,
        "{} decoded to an implausibly small frame ({}x{})",
        path.display(),
        image.width,
        image.height,
    );
    let first = image
        .rgba
        .chunks_exact(4)
        .next()
        .expect("decoded PNG has pixels");
    assert!(
        image
            .rgba
            .chunks_exact(4)
            .any(|pixel| pixel[..3] != first[..3]),
        "{} decoded but contains only one RGB colour; expected a rendered UI",
        path.display(),
    );
}

struct DecodedRgba {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn decode_rgba(path: &Path) -> DecodedRgba {
    let decoded =
        ui::png::read_rgba8(path).unwrap_or_else(|e| panic!("decode PNG {}: {e}", path.display()));
    DecodedRgba {
        width: decoded.width,
        height: decoded.height,
        rgba: decoded.rgba,
    }
}

fn assert_frames_visibly_differ(before: &Path, after: &Path) {
    let before = decode_rgba(before);
    let after = decode_rgba(after);
    assert_eq!(
        (before.width, before.height),
        (after.width, after.height),
        "state-changing frames must have matching dimensions",
    );
    let changed = before
        .rgba
        .chunks_exact(4)
        .zip(after.rgba.chunks_exact(4))
        .filter(|(left, right)| left[..3] != right[..3])
        .count();
    assert!(
        changed > 100,
        "state-changing frames differ by only {changed} pixels; expected a visible UI transition",
    );
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    reason = "the region is clamped to finite image bounds before its intentional pixel-address conversion and indexing"
)]
fn assert_region_visibly_differs(before: &Path, after: &Path, region: Rect, minimum: usize) {
    let before = decode_rgba(before);
    let after = decode_rgba(after);
    assert_eq!(
        (before.width, before.height),
        (after.width, after.height),
        "region-changing frames must have matching dimensions",
    );
    let x0 = region.x0.floor().max(0.0) as u32;
    let y0 = region.y0.floor().max(0.0) as u32;
    let x1 = region.x1.ceil().clamp(0.0, f64::from(before.width)) as u32;
    let y1 = region.y1.ceil().clamp(0.0, f64::from(before.height)) as u32;
    assert!(
        x0 < x1 && y0 < y1,
        "comparison region must intersect the frame"
    );
    let mut changed = 0usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let offset = ((y * before.width + x) * 4) as usize;
            if before.rgba[offset..offset + 3] != after.rgba[offset..offset + 3] {
                changed += 1;
            }
        }
    }
    assert!(
        changed >= minimum,
        "expected at least {minimum} changed pixels inside {region:?}, found {changed}",
    );
}

// Test 5: live-binary global hotkey routing
//
// Guards Cmd+K / Cmd+Tab / Escape as the running binary actually wires
// them: `main.rs` bypasses `Shell::dispatch_key` entirely, so the only
// hotkey path a real user hits is the pre-pass inside
// `ShellOverlayHost::handle_event`.

/// Throwaway `AutoCommit` + empty taffy tree, so the hotkey pre-pass
/// short-circuits before walking children.
fn pump_host(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
    event: &WindowEvent,
    modifiers: Modifiers,
) -> bool {
    let taffy: TaffyTree<()> = TaffyTree::new();
    // `node_id` is only dereferenced when walking children; the pre-pass returns first.
    let dummy_node = NodeId::from(0_u64);
    dispatch(host, &taffy, dummy_node, modifiers, event)
}
