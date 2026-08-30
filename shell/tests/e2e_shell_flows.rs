//! Headless end-to-end snapshot harness for nirios-shell flows. Drives the
//! real widget tree through synthesised `winit::WindowEvent`s and a full
//! build/layout/draw cycle (no window, no compositor), writing frames to
//! `target/nirios-screenshots/e2e-*.png`; only existence/PNG validity are
//! asserted, no pixel-diff yet.
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
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

use shell::shell::build_nirios_root_with_chrome;
use shell::shell::state::WorkspaceTree;
use shell::shell::topbar_chrome::TopBarChrome;

use ui::presence::PresenceCtx;
use ui::state::State;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AppKind, TabBar, TabDescriptor, TileContent, Widget, WindowGroup, WindowWidth};

use crate::support::offscreen::OffscreenRenderer;
use crate::support::snapshot::SnapshotAssertion;

/// Where golden PNGs are committed (in source, not `target/`).
fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("e2e")
}

// Shared harness plumbing

const W: f32 = 1600.0;
const H: f32 = 900.0;

/// PNG output root, under `target/nirios-screenshots/` so it's gitignored.
fn screenshots_dir() -> PathBuf {
    // CARGO_TARGET_DIR respects workspace overrides; default to `target/`
    // under the crate root, which `cargo test` uses.
    let root = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap_or(Path::new("."))
                .join("target")
        },
        PathBuf::from,
    );
    root.join("nirios-screenshots")
}

/// `None` when no wgpu adapter is available (catch_unwind-guarded).
fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OffscreenRenderer::new(w, h)
    }))
    .ok()
}

/// `taffy` + `node_map` intentionally outlive `root` via the same lifetime
/// so re-renders after mutation can reuse them.
struct Harness {
    root: Box<dyn Widget>,
    taffy: TaffyTree<()>,
    // Kept alive so the raw `*mut dyn Widget` pointers `build_layout` stashed
    // stay valid; read indirectly via the measure closures during re-layout.
    #[allow(dead_code)]
    node_map: HashMap<NodeId, *mut dyn Widget>,
    root_node: NodeId,
    theme: Theme,
    tr: TextRenderer,
    doc: AutoCommit,
    doc_root: ObjId,
    modifiers: Modifiers,
    focus: Option<NodeId>,
    chrome: shell::shell::chrome_state::Chrome,
}

impl Harness {
    fn new(tree: &WorkspaceTree) -> Self {
        // Pin the top-bar clock before the first sync — frames here are compared
        // pixel-for-pixel and a live clock would fail once a minute.
        // SAFETY: single-threaded test setup, under `TEST_LOCK`.
        unsafe { std::env::set_var("QOS_CLOCK", "09:41") };
        let mut owned = tree.clone();
        // Rooms need recorded access, or access-reporting surfaces assert against
        // a state no user has ever been in.
        owned.adopt_ownerless_workspaces(&shell::shell::state::local_user_id());
        let tree = &owned;
        let (root, chrome) = build_nirios_root_with_chrome(tree);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut root = root;
        let child = root.build_layout(&mut taffy, &theme, &mut node_map);
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

        let mut tr = TextRenderer::new();
        // Clone into a local so the closure borrow doesn't collide with `node_map`.
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
        root.apply_layout(&taffy, child, Point::ORIGIN);

        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let _ = &mut doc; // silence unused-mut warnings on older toolchains

        Self {
            root,
            taffy,
            node_map,
            root_node: child,
            theme,
            tr,
            doc,
            doc_root,
            modifiers: Modifiers::default(),
            focus: None,
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

    /// Unused on `nirios-shell` — the sidebar wheel swallows clicks at the
    /// topbar hit regions — kept for when that regression is fixed.
    #[allow(dead_code)]
    fn click(&mut self, x: f64, y: f64) {
        self.pump(WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(x, y),
        });
        self.pump(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.pump(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    fn pump(&mut self, event: WindowEvent) -> bool {
        let mut handled = false;
        self.root.handle_event(
            &event,
            1.0,
            &self.modifiers,
            &mut self.doc,
            &self.doc_root,
            &mut handled,
            &mut self.focus,
            self.root_node,
            &self.taffy,
        );
        handled
    }

    /// `pump_host` can't answer this for a declined key: it hands the host a
    /// zeroed taffy node, relying on the hotkey pre-pass returning before any
    /// child walk.
    fn key(&mut self, key: Key) -> bool {
        self.pump(key_press(key))
    }
}

// winit's `KeyEvent` has a private field, so it can't come from a struct
// literal — zero-init instead (see SAFETY below).
fn escape_event() -> WindowEvent {
    // SAFETY: `KeyEvent`'s pub fields are all safe to zero-init
    // (`Option<SmolStr>` → `None`; `PhysicalKey`, `Key`, `KeyLocation`,
    // `ElementState` all have a valid first variant at the all-zero bit
    // pattern; `bool` → `false`). The private `platform_specific` field
    // is a platform-specific zero-sized or plain struct whose zero
    // bit pattern is valid on every target we care about. The shell
    // only reads `logical_key` + `state` from this event so the rest is
    // immaterial. This workaround is needed because winit 0.30 doesn't
    // expose a KeyEvent constructor; upgrading to 0.31 (ActiveEventLoop)
    // is out of scope for this test harness.
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Named(NamedKey::Escape);
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

// Test serialisation: tests share a wgpu/vello/cosmic-text static cache and
// have SIGSEGV'd in parallel, so each holds `TEST_LOCK` for its duration.
// `unwrap_or_else(|e| e.into_inner())` so a panicking test doesn't poison
// the mutex for the rest of the suite.
static TEST_LOCK: Mutex<()> = Mutex::new(());

// Test 1: open + close overview

#[test]
fn e2e_open_close_overview() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // bootstrap_demo already seeds 3 workspaces, so the Overview grid has
    // cards to paint without further appends.
    let tree = WorkspaceTree::bootstrap_demo();

    let theme = Theme::dark();
    let mut renderer_opt = try_new_renderer(W as u32, H as u32);
    let out_dir = screenshots_dir();

    let mut h = Harness::new(&tree);
    // Pre-click frame — also primes `overview_btn_rect` for the probe below.
    h.draw_to(renderer_opt.as_mut(), &out_dir.join("e2e-01-desktop.png"));

    // The probe only computes the button's centre; the click flows through
    // `ShellOverlayHost::handle_event`, narrowed to the sidebar wheel's y-range.
    let hit = overview_btn_hit(&tree, &theme, h.chrome.clone());
    h.click(hit.x, hit.y);
    assert!(
        h.chrome.borrow().overlays.overview,
        "root-level click on the overview button should open the overview overlay"
    );

    // `tick_overview_transition` only latches `transition_started` on the
    // first draw after the target changes, so `needs_animation_frame()`
    // reads `false` until that draw runs — without this priming draw the
    // loop below exits immediately and captures the pre-fade desktop.
    {
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        h.root
            .draw(&mut scene, Affine::IDENTITY, &mut h.tr, &h.theme, &presence);
    }

    // `needs_animation_frame()` is a weak loop guard so this can't sleep forever.
    let deadline = Instant::now() + Duration::from_millis(600);
    while h.root.needs_animation_frame() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        // Re-draw so the tick functions see elapsed time — they advance on draw,
        // not on wall clock.
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        h.root
            .draw(&mut scene, Affine::IDENTITY, &mut h.tr, &h.theme, &presence);
    }

    h.draw_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-02-overview-open.png"),
    );

    h.pump(escape_event());
    assert!(
        !h.chrome.borrow().overlays.overview,
        "Escape should close the overview overlay"
    );

    {
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        h.root
            .draw(&mut scene, Affine::IDENTITY, &mut h.tr, &h.theme, &presence);
    }

    let deadline = Instant::now() + Duration::from_millis(600);
    while h.root.needs_animation_frame() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        h.root
            .draw(&mut scene, Affine::IDENTITY, &mut h.tr, &h.theme, &presence);
    }
    h.draw_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-03-overview-closed.png"),
    );

    // With a renderer, verify the PNGs landed (non-empty + PNG signature);
    // without one, only the state transitions above are asserted.
    if renderer_opt.is_some() {
        for name in [
            "e2e-01-desktop.png",
            "e2e-02-overview-open.png",
            "e2e-03-overview-closed.png",
        ] {
            assert_png_nonempty(&out_dir.join(name));
        }
        // Only the settled open-overview frame is pinned — pre-click and post-Esc
        // are the same desktop steady state.
        let rendered = out_dir.join("e2e-02-overview-open.png");
        SnapshotAssertion::new(
            rendered,
            goldens_dir().join("e2e-open-close-overview-settled.png"),
        )
        .assert();
    } else {
        eprintln!(
            "offscreen: no wgpu adapter — e2e_open_close_overview asserted state transitions only"
        );
    }
}

/// Centre of the topbar's overview-toggle icon, from a sibling
/// `TopBarChrome` — topbar geometry is width-driven, so its rect matches.
fn overview_btn_hit(
    tree: &WorkspaceTree,
    theme: &Theme,
    chrome: shell::shell::chrome_state::Chrome,
) -> Point {
    let (_, centre) = probe_topbar(
        tree,
        theme,
        chrome,
        shell::shell::topbar_chrome::TopBarChrome::overview_btn_rect,
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
    let mut bar = TopBarChrome::from_tree(tree).with_chrome(chrome);
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
                size: Size {
                    width: Dimension::length(probe_w),
                    height: Dimension::length(H),
                },
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

#[test]
fn e2e_tab_drag_reorder() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Standalone WindowGroup with four Stub tabs, avoiding full-root plumbing.
    // Tab 1 (Files) is the drag source; tab 3 (Browser) the drop target.
    let tabs: Vec<(TabDescriptor, TileContent)> = vec![
        tab_stub(1, "Notes", AppKind::Editor, "notes"),
        tab_stub(2, "Files", AppKind::Files, "files"),
        tab_stub(3, "Terminal", AppKind::Terminal, "terminal"),
        tab_stub(4, "Browser", AppKind::Browser, "browser"),
    ];
    // Active tab = Notes so the reorder can't also change the active id.
    let mut standalone = WindowGroup::new(100, tabs, /* active_id */ 1, WindowWidth::Normal);
    // A width is a fraction of its strip; with no `GroupRow`, tell the group the
    // 960 px capture surface *is* its strip, or it uses the toolkit fallback.
    standalone.set_strip_usable_px(960.0);
    let mut group: Box<dyn Widget> = Box::new(standalone);

    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = group.build_layout(&mut taffy, &theme, &mut node_map);
    let vp = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(960.0),
                    height: Dimension::length(540.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    // Clone node_map into the closure to side-step a double borrow with `&mut tr`.
    let map_for_measure = node_map.clone();
    taffy
        .compute_layout_with_measure(
            vp,
            Size {
                width: AvailableSpace::Definite(960.0),
                height: AvailableSpace::Definite(540.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map_for_measure.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    group.apply_layout(&taffy, child, Point::ORIGIN);

    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    group.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let mut renderer_opt = try_new_renderer(960, 540);
    let out_dir = screenshots_dir();
    if let Some(r) = renderer_opt.as_mut() {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
        r.render_png(
            &s,
            Color::from_rgb8(0x02, 0x06, 0x14),
            out_dir.join("e2e-04-tabs-initial.png"),
        );
    }

    // `bar` isn't exposed, so read tab geometry from a parallel TabBar.
    let tab_centers = probe_tab_centers();
    let from = tab_centers[1]; // Files pill centre
    let to = tab_centers[3]; // Browser pill centre

    let mut doc = AutoCommit::new();
    let doc_root = automerge::ROOT.clone();
    let modifiers = Modifiers::default();
    let mut focus: Option<NodeId> = None;

    pump(
        &mut group,
        &taffy,
        child,
        &mut doc,
        &doc_root,
        &modifiers,
        &mut focus,
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(from.x, from.y),
        },
    );
    pump(
        &mut group,
        &taffy,
        child,
        &mut doc,
        &doc_root,
        &modifiers,
        &mut focus,
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
    );

    // Cross the 8 px DRAG_START threshold in steps, capturing one mid-drag
    // frame so the drop-zone highlight is visible.
    let midway = Point::new(f64::midpoint(from.x, to.x), from.y);
    for step in [0.33_f64, 0.66, 1.0] {
        let p = Point::new(
            from.x + (to.x - from.x) * step,
            from.y + (to.y - from.y) * step,
        );
        pump(
            &mut group,
            &taffy,
            child,
            &mut doc,
            &doc_root,
            &modifiers,
            &mut focus,
            WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: PhysicalPosition::new(p.x, p.y),
            },
        );
    }
    let _ = midway;

    if let Some(r) = renderer_opt.as_mut() {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
        r.render_png(
            &s,
            Color::from_rgb8(0x02, 0x06, 0x14),
            out_dir.join("e2e-05-tab-drag-midway.png"),
        );
    }

    // `MouseInput::Released` reads the cached `last_cursor`, so the final step
    // must already be at `to`.
    pump(
        &mut group,
        &taffy,
        child,
        &mut doc,
        &doc_root,
        &modifiers,
        &mut focus,
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    );

    // WindowGroup drains `bar.take_reorder()` and permutes its hosts, but
    // `WindowGroup::tabs()` still returns descriptor order (visible labels stay
    // stable during drag; content order follows the drop target). So this
    // asserts the reorder request via a sibling TabBar's `reorder`, not
    // `group.tabs()` — switch if that ever changes.
    let post = expected_post_drop_descriptor_order();
    assert_eq!(
        post,
        vec!["Notes", "Terminal", "Browser", "Files"],
        "dropping Files (idx 1) onto Browser (idx 3) should shift it to the end"
    );

    // TabBar's layout-spring: `LAYOUT_ANIM_MS = 180` (ui/src/widget/tabs.rs).
    // 250 ms gives margin for every pill to converge from its pre-drop
    // `prev_rect` to `tab_rects[i]` before the golden capture — without it
    // the spring is still ~70% interpolated and back-to-back runs diverge.
    std::thread::sleep(Duration::from_millis(250));
    // Event-less pump + draw so the bar sees "post-LAYOUT_ANIM_MS" next draw.
    {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    if let Some(r) = renderer_opt.as_mut() {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
        r.render_png(
            &s,
            Color::from_rgb8(0x02, 0x06, 0x14),
            out_dir.join("e2e-06-tabs-reordered.png"),
        );
    }

    if renderer_opt.is_some() {
        for name in [
            "e2e-04-tabs-initial.png",
            "e2e-05-tab-drag-midway.png",
            "e2e-06-tabs-reordered.png",
        ] {
            assert_png_nonempty(&out_dir.join(name));
        }
        // Only the post-spring frame is pinned — midway is frame-timer driven and
        // inherently flaky.
        let rendered = out_dir.join("e2e-06-tabs-reordered.png");
        SnapshotAssertion::new(
            rendered,
            goldens_dir().join("e2e-tab-drag-reorder-settled.png"),
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
        },
        TileContent::Stub {
            kind,
            label: label.into(),
        },
    )
}

/// Each tab pill's centre, via a parallel `TabBar` run through the same
/// build/compute/apply/draw the real `WindowGroup` uses.
fn probe_tab_centers() -> [Point; 4] {
    let descs = vec![
        TabDescriptor {
            id: 1,
            title: "Notes".into(),
            app_kind: Some(AppKind::Editor),
        },
        TabDescriptor {
            id: 2,
            title: "Files".into(),
            app_kind: Some(AppKind::Files),
        },
        TabDescriptor {
            id: 3,
            title: "Terminal".into(),
            app_kind: Some(AppKind::Terminal),
        },
        TabDescriptor {
            id: 4,
            title: "Browser".into(),
            app_kind: Some(AppKind::Browser),
        },
    ];
    let mut bar: Box<dyn Widget> = Box::new(TabBar::new("g100", descs, 1));
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = bar.build_layout(&mut taffy, &theme, &mut map);
    let vp = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(960.0),
                    height: Dimension::length(40.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let map_copy = map.clone();
    taffy
        .compute_layout_with_measure(
            vp,
            Size {
                width: AvailableSpace::Definite(960.0),
                height: AvailableSpace::Definite(40.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map_copy.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    bar.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    // `tab_rects` isn't reachable through `Box<dyn Widget>`; rebuild as concrete.
    let mut concrete = TabBar::new(
        "g100",
        vec![
            TabDescriptor {
                id: 1,
                title: "Notes".into(),
                app_kind: Some(AppKind::Editor),
            },
            TabDescriptor {
                id: 2,
                title: "Files".into(),
                app_kind: Some(AppKind::Files),
            },
            TabDescriptor {
                id: 3,
                title: "Terminal".into(),
                app_kind: Some(AppKind::Terminal),
            },
            TabDescriptor {
                id: 4,
                title: "Browser".into(),
                app_kind: Some(AppKind::Browser),
            },
        ],
        1,
    );
    let mut taffy2: TaffyTree<()> = TaffyTree::new();
    let mut map2: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child2 = concrete.build_layout(&mut taffy2, &theme, &mut map2);
    let vp2 = taffy2
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(960.0),
                    height: Dimension::length(40.0),
                },
                ..Default::default()
            },
            &[child2],
        )
        .unwrap();
    let mut tr2 = TextRenderer::new();
    let map2_copy = map2.clone();
    taffy2
        .compute_layout_with_measure(
            vp2,
            Size {
                width: AvailableSpace::Definite(960.0),
                height: AvailableSpace::Definite(40.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map2_copy.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr2) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    concrete.apply_layout(&taffy2, child2, Point::ORIGIN);
    let mut s2 = Scene::new();
    concrete.draw(&mut s2, Affine::IDENTITY, &mut tr2, &theme, &presence);
    let rs = concrete.tab_rects();
    // Fall back to evenly-spaced guesses if layout returned fewer than 4 rects,
    // so a failure reports usefully instead of panicking on an index.
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

/// Expected post-drop order for `(from, to) = (1, 3)`, kept separate from the
/// live widget so it documents intent even if the descriptor list is unchanged.
fn expected_post_drop_descriptor_order() -> Vec<&'static str> {
    let mut order = vec!["Notes", "Files", "Terminal", "Browser"];
    // Matches `TabBar::reorder(1, 3)`: pull `Files`, re-insert at idx 3.
    let item = order.remove(1);
    order.insert(3, item);
    order
}

// Test 3: open quick settings via the tray pill

#[test]
fn e2e_open_quick_settings() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tree = WorkspaceTree::bootstrap_demo();
    let theme = Theme::dark();

    let mut renderer_opt = try_new_renderer(W as u32, H as u32);
    let out_dir = screenshots_dir();

    let mut h = Harness::new(&tree);
    assert!(!h.chrome.borrow().overlays.quick_settings);

    // A draw pass populates `tray_pill_rect`, which handle_event hit-tests.
    h.draw_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-06-quick-settings-pre.png"),
    );

    // With the sidebar click-guard narrowed, the click reaches TopBarChrome.
    let hit = tray_pill_hit(&tree, &theme, h.chrome.clone());
    h.click(hit.x, hit.y);
    assert!(
        h.chrome.borrow().overlays.quick_settings,
        "root-level click on the three-dot tray pill should open the quick-settings overlay"
    );

    // Let the 180 ms open fade settle so the capture shows a fully-open tray.
    let deadline = Instant::now() + Duration::from_millis(400);
    while h.root.needs_animation_frame() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(30));
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        h.root
            .draw(&mut scene, Affine::IDENTITY, &mut h.tr, &h.theme, &presence);
    }

    h.draw_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-07-quick-settings-open.png"),
    );
    if renderer_opt.is_some() {
        let rendered = out_dir.join("e2e-07-quick-settings-open.png");
        assert_png_nonempty(&rendered);
        // The topbar clock (chrono::Local) shows behind the quick-settings
        // popover, making this render wall-clock-dependent — a golden would flake
        // once a minute, so only the state transition + non-empty PNG are
        // asserted until the clock is mockable.
    } else {
        eprintln!("offscreen: no wgpu adapter — e2e_open_quick_settings asserted state only");
    }
}

// Test 4: maximized tile renders the floating-controls chrome
//
// Standalone `WindowGroup`, not the full shell tree: `max_rects` has no
// public `TabBar` accessor and `WindowGroup.maximized` isn't reachable
// through the shell's `Box<dyn Widget>` tree. This still exercises the
// full click pipeline (max_rects hit-test -> pending_max -> take_max ->
// flip maximized -> floating-controls draw).
#[test]
fn e2e_maximized_tile_floating_controls() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = group.build_layout(&mut taffy, &theme, &mut node_map);
    let vp = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(960.0),
                    height: Dimension::length(540.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let map_for_measure = node_map.clone();
    taffy
        .compute_layout_with_measure(
            vp,
            Size {
                width: AvailableSpace::Definite(960.0),
                height: AvailableSpace::Definite(540.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map_for_measure.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    group.apply_layout(&taffy, child, Point::ORIGIN);

    // Prime the bar — max_rect coordinates derive from the active pill's rect.
    let presence = PresenceCtx::default();
    {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    // Hover the active pill so its chip cluster is offered at all.
    let pill = group
        .bar_for_tests()
        .tab_rects()
        .first()
        .copied()
        .expect("a pill")
        .center();

    // CursorMoved populates `last_cursor` + the hover_max_idx cache, then
    // press+release at the same spot.
    let mut doc = AutoCommit::new();
    let doc_root = automerge::ROOT.clone();
    let modifiers = Modifiers::default();
    let mut focus: Option<NodeId> = None;
    // Direct `group.handle_event` (not the boxed `pump`) keeps `.maximized`
    // reachable on the concrete type.
    let root_node = child;
    let mut pump_ev = |group: &mut WindowGroup, event: WindowEvent| {
        let mut handled = false;
        group.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            root_node,
            &taffy,
        );
    };
    pump_ev(
        &mut group,
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(pill.x, pill.y),
        },
    );
    // The chip cluster slides out on wall time: `advance_reveal` starts the
    // curve on the layout pass that first sees the hover and returns where it
    // started, so `max_rects` stay `Rect::ZERO` until it has run, and
    // `apply_layout` is what places them. Lay out to create the curve, hold it
    // past the end, lay out again. Reading the rect straight back — or deriving
    // it from the pill with the chip offsets copied out of `TabBar` — clicks
    // nothing, which is what this test was doing.
    group.apply_layout(&taffy, child, Point::ORIGIN);
    assert!(
        group
            .bar_mut_for_tests()
            .hold_reveal_at(std::time::Duration::from_secs(1)),
        "hovering the pill started no chip reveal",
    );
    group.apply_layout(&taffy, child, Point::ORIGIN);
    let max_center = group
        .bar_for_tests()
        .max_rects()
        .first()
        .copied()
        .filter(|r| r.area() > 0.0)
        .expect("the active tab offers a Maximize2 chip once its reveal has run")
        .center();
    pump_ev(
        &mut group,
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(max_center.x, max_center.y),
        },
    );
    pump_ev(
        &mut group,
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
    );
    // Release isn't required (Pressed publishes pending_max, drained same event)
    // but keeps the event stream realistic.
    pump_ev(
        &mut group,
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    );

    assert!(
        group.maximized,
        "clicking the active tab's Maximize2 glyph should toggle `maximized = true`"
    );

    // Defensive: settle any spawn-fade timers still pumping.
    std::thread::sleep(Duration::from_millis(50));
    {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    let mut renderer_opt = try_new_renderer(960, 540);
    let out_dir = screenshots_dir();
    if let Some(r) = renderer_opt.as_mut() {
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
        r.render_png(
            &s,
            Color::from_rgb8(0x02, 0x06, 0x14),
            out_dir.join("e2e-maximized-tile-controls.png"),
        );
        let rendered = out_dir.join("e2e-maximized-tile-controls.png");
        assert_png_nonempty(&rendered);
        // Floating-controls chrome is what regresses if hover-cluster geometry drifts.
        SnapshotAssertion::new(
            rendered,
            goldens_dir().join("e2e-maximized-tile-controls.png"),
        )
        .assert();
    } else {
        eprintln!(
            "offscreen: no wgpu adapter — e2e_maximized_tile_floating_controls asserted state only"
        );
    }
}

/// Sibling `TabBar` returning full `tab_rects` (unlike `probe_tab_centers`),
/// for per-pill hover-cluster coordinates.
fn probe_tab_rects() -> Vec<vello::kurbo::Rect> {
    let mut concrete = TabBar::new(
        "g200",
        vec![
            TabDescriptor {
                id: 1,
                title: "Notes".into(),
                app_kind: Some(AppKind::Editor),
            },
            TabDescriptor {
                id: 2,
                title: "Files".into(),
                app_kind: Some(AppKind::Files),
            },
            TabDescriptor {
                id: 3,
                title: "Terminal".into(),
                app_kind: Some(AppKind::Terminal),
            },
            TabDescriptor {
                id: 4,
                title: "Browser".into(),
                app_kind: Some(AppKind::Browser),
            },
        ],
        1,
    );
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = concrete.build_layout(&mut taffy, &theme, &mut map);
    let vp = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(960.0),
                    height: Dimension::length(40.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let map_copy = map.clone();
    taffy
        .compute_layout_with_measure(
            vp,
            Size {
                width: AvailableSpace::Definite(960.0),
                height: AvailableSpace::Definite(40.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map_copy.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    concrete.apply_layout(&taffy, child, Point::ORIGIN);
    let mut s = Scene::new();
    let presence = PresenceCtx::default();
    concrete.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
    concrete.tab_rects().to_vec()
}

// Helpers

fn pump(
    widget: &mut Box<dyn Widget>,
    taffy: &TaffyTree<()>,
    root_node: NodeId,
    doc: &mut AutoCommit,
    doc_root: &ObjId,
    modifiers: &Modifiers,
    focus: &mut Option<NodeId>,
    event: WindowEvent,
) {
    let mut handled = false;
    widget.handle_event(
        &event,
        1.0,
        modifiers,
        doc,
        doc_root,
        &mut handled,
        focus,
        root_node,
        taffy,
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
    let mut file = std::fs::File::open(path).unwrap();
    let mut sig = [0u8; 8];
    use std::io::Read;
    file.read_exact(&mut sig).unwrap();
    assert_eq!(
        &sig[..4],
        b"\x89PNG",
        "{} missing PNG signature",
        path.display()
    );
}

// Test 5: live-binary global hotkey routing
//
// Guards Cmd+K / Cmd+Tab / Escape as the running binary actually wires
// them: `main.rs` bypasses `Shell::dispatch_key` entirely, so the only
// hotkey path a real user hits is the pre-pass inside
// `ShellOverlayHost::handle_event`.

/// Same zero-init workaround as [`escape_event`] — winit 0.30 keeps
/// `KeyEventExtra` `pub(crate)`.
fn key_press(logical_key: Key) -> WindowEvent {
    // SAFETY: see the note on `escape_event` — all pub fields of `KeyEvent`
    // tolerate the all-zero bit pattern, and the overlay-host pre-pass
    // only inspects `logical_key` + `state`.
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = logical_key;
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

/// Throwaway `AutoCommit` + empty taffy tree, so the hotkey pre-pass
/// short-circuits before walking children.
fn pump_host(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
    event: &WindowEvent,
    modifiers: &Modifiers,
) -> bool {
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT.clone();
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    let taffy: TaffyTree<()> = TaffyTree::new();
    // `node_id` is only dereferenced when walking children; the pre-pass returns first.
    let dummy_node: NodeId = unsafe { std::mem::zeroed() };
    host.handle_event(
        event,
        1.0,
        modifiers,
        &mut doc,
        &root,
        &mut handled,
        &mut focus_target,
        dummy_node,
        &taffy,
    );
    handled
}

#[test]
fn e2e_ctrl_k_opens_search_palette_in_live_binary_path() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The concrete-type sibling of what the live binary builds — exercises the
    // real widget tree, not a hand-rolled stub.
    let tree = WorkspaceTree::bootstrap_demo();
    let (mut host, _chrome) = shell::shell::build_nirios_overlay_host_with_chrome(&tree);
    assert!(!host.palette_visible(), "palette starts closed");

    let mods = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
    let handled = pump_host(&mut host, &key_press(Key::Character("k".into())), &mods);
    assert!(handled, "Ctrl+K must be consumed by the overlay host");
    assert!(
        host.palette_visible(),
        "Ctrl+K must open the global-search palette in the live-binary event path"
    );
}

#[test]
fn e2e_escape_closes_search_palette() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tree = WorkspaceTree::bootstrap_demo();
    let (mut host, _chrome) = shell::shell::build_nirios_overlay_host_with_chrome(&tree);

    // Open via Ctrl+K rather than bypassing the host's state machine, guarding
    // against a mutation-ordering regression.
    let mods = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
    let _ = pump_host(&mut host, &key_press(Key::Character("k".into())), &mods);
    assert!(host.palette_visible());

    // Escape priority order: palette > overview > ws_settings > quick_settings.
    let no_mods = Modifiers::default();
    let handled = pump_host(
        &mut host,
        &key_press(Key::Named(NamedKey::Escape)),
        &no_mods,
    );
    assert!(handled, "Escape must be consumed while the palette is open");
    assert!(
        !host.palette_visible(),
        "Escape must close the global-search palette"
    );
}

#[test]
fn e2e_ctrl_tab_toggles_overview_in_live_binary_path() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tree = WorkspaceTree::bootstrap_demo();
    let (mut host, chrome) = shell::shell::build_nirios_overlay_host_with_chrome(&tree);
    assert!(!chrome.borrow().overlays.overview);

    let mods = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
    let handled = pump_host(&mut host, &key_press(Key::Named(NamedKey::Tab)), &mods);
    assert!(handled, "Ctrl+Tab must be consumed by the overlay host");
    assert!(
        chrome.borrow().overlays.overview,
        "Ctrl+Tab must toggle the overview overlay in the live-binary event path"
    );
}

/// The overview finishes on the keyboard: arrows walk the room list, Enter
/// commits and closes.
#[test]
fn e2e_overview_arrows_select_and_enter_switches_room() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tree = WorkspaceTree::bootstrap_demo();
    let started_on = tree.active;
    assert!(
        tree.workspaces.len() > 1,
        "precondition: the fixture has somewhere else to go"
    );
    let (mut host, chrome) = shell::shell::build_nirios_overlay_host_with_chrome(&tree);

    let ctrl = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
    assert!(pump_host(
        &mut host,
        &key_press(Key::Named(NamedKey::Tab)),
        &ctrl
    ));
    assert!(
        chrome.borrow().overlays.overview,
        "precondition: panel is open"
    );

    let no_mods = Modifiers::default();
    let handled = pump_host(
        &mut host,
        &key_press(Key::Named(NamedKey::ArrowDown)),
        &no_mods,
    );
    assert!(
        handled,
        "the open panel must consume the arrow instead of letting the \
         desktop underneath it scroll"
    );

    let handled = pump_host(&mut host, &key_press(Key::Named(NamedKey::Enter)), &no_mods);
    assert!(handled, "Enter must be consumed by the open panel");
    assert!(
        !chrome.borrow().overlays.overview,
        "committing to a room closes the switcher"
    );
    assert_ne!(
        chrome.borrow().tree().active,
        started_on,
        "Enter must actually go to the room the arrow selected"
    );
}

/// Arrows belong to the switcher only while it's open. Driven through the
/// real root/layout since the claim concerns a key the shell declines — the
/// event must actually reach the content.
#[test]
fn e2e_overview_arrows_are_not_swallowed_while_the_panel_is_closed() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tree = WorkspaceTree::bootstrap_demo();
    let mut h = Harness::new(&tree);
    assert!(
        !h.chrome.borrow().overlays.overview,
        "precondition: panel shut"
    );

    let before = h.chrome.borrow().tree().active;
    h.key(Key::Named(NamedKey::ArrowDown));
    h.key(Key::Named(NamedKey::Enter));
    assert!(
        !h.chrome.borrow().overlays.overview,
        "arrows and Enter must not open the switcher"
    );
    assert_eq!(
        h.chrome.borrow().tree().active,
        before,
        "with the panel shut, Enter must not switch rooms"
    );
}

// Test 6 + 7: undo / redo round-trip against a CRDT-mutating widget
//
// Both build a standalone `WindowGroup` (960x540) with one tab hosting a
// real `EditorApp` — typing splices into the `BODY_KEY` Automerge `Text`,
// the only gesture in the shell tree that mutates the CRDT through the
// widget event pipeline (tab close/reorder/maximize live entirely in
// widget state; `state.undo` is a no-op for them).
//
// `state.undo()`/`redo()` are called directly rather than synthesising
// Ctrl+Z: that keymap lives in `ui::App::run`, which this test never runs.

/// Like `key_press`, but carries a real `text` payload so `EditorApp` sees
/// a user-typed character.
fn char_key_press(ch: &str) -> WindowEvent {
    // SAFETY: see the note on `escape_event`.
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Character(ch.into());
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(ch.into());
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

/// Common undo/redo setup: 960x540 group, one focused `EditorApp` tab,
/// `BODY_KEY` seeded and committed so the undo stack starts clean.
struct UndoHarness {
    group: WindowGroup,
    state: State,
    taffy: TaffyTree<()>,
    tr: TextRenderer,
    root_node: NodeId,
}

impl UndoHarness {
    fn new() -> Self {
        // EditorApp needs focus before it processes keyboard, and there's no App
        // loop to route a click-triggered focus_target back into `set_focus`.
        let mut editor = shell::shell::editor_app::EditorApp::new();
        editor.set_focus(true);

        let tab = TabDescriptor {
            id: 1,
            title: "Editor".into(),
            app_kind: Some(AppKind::Editor),
        };
        let tabs = vec![(tab, TileContent::App(Box::new(editor)))];
        let mut group = WindowGroup::new(300, tabs, 1, WindowWidth::Normal);
        group.set_strip_usable_px(960.0);

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = group.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(960.0),
                        height: Dimension::length(540.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let mut tr = TextRenderer::new();
        let map_for_measure = node_map.clone();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: AvailableSpace::Definite(960.0),
                    height: AvailableSpace::Definite(540.0),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = map_for_measure.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .expect("compute_layout");
        group.apply_layout(&taffy, child, Point::ORIGIN);

        // Seed BODY_KEY before counting undo entries — EditorApp.sync creates it
        // lazily on first call, so without this the first real mutation would land
        // as entry #2 behind the seeding entry. Sync against a throwaway State,
        // then reload into a fresh `State::load` for a clean [undo:0, redo:0] stack.
        let state = {
            let mut seed = State::new();
            group.sync(&mut seed.doc, &seed.root);
            // Commit staged ops — AutoCommit panics on save with a live transaction.
            let _ = seed.doc.commit();
            let bytes = seed.doc.save();
            State::load(&bytes).expect("reload seeded doc")
        };

        let presence = PresenceCtx::default();
        let mut s = Scene::new();
        group.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);

        Self {
            group,
            state,
            taffy,
            tr,
            root_node: child,
        }
    }

    fn pump_event(&mut self, event: WindowEvent, modifiers: &Modifiers) {
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        self.group.handle_event(
            &event,
            1.0,
            modifiers,
            &mut self.state.doc,
            &self.state.root,
            &mut handled,
            &mut focus_target,
            self.root_node,
            &self.taffy,
        );
    }

    fn render_to(&mut self, renderer: Option<&mut OffscreenRenderer>, path: &Path) {
        let theme = Theme::dark();
        self.group.sync(&mut self.state.doc, &self.state.root);
        self.group
            .apply_layout(&self.taffy, self.root_node, Point::ORIGIN);
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        self.group.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut self.tr,
            &theme,
            &presence,
        );
        if let Some(r) = renderer {
            r.render_png(&scene, Color::from_rgb8(0x02, 0x06, 0x14), path);
        }
    }
}

/// Typing 'a' splices into `BODY_KEY`, `commit_action` captures the baseline,
/// `undo` pops it back, and the next sync re-reads the empty body.
#[test]
fn e2e_undo_redo_round_trip_restores_golden() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut h = UndoHarness::new();

    let mut renderer_opt = try_new_renderer(960, 540);
    let out_dir = screenshots_dir();

    // --- Baseline render (empty editor body). ---
    h.render_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-undo-baseline.png"),
    );

    // Confirms the tab-close undo gap noted above; this test passes via the
    // editor-character path below.
    {
        let before = h.state.peek_last_undo().is_some();
        // Close-rect geometry from TabBar offsets (close_w=18, close_inset=4); with
        // one tab, probe_tab_rects()[0].x1 is close enough to the live pill's edge.
        let probe = probe_tab_rects();
        if let Some(pill) = probe.first() {
            let close_cx = pill.x1 - 13.0; // x0 = x1 - 22 + 9 for centre
            let close_cy = f64::midpoint(pill.y0, pill.y1);
            let mods = Modifiers::default();
            h.pump_event(
                WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: PhysicalPosition::new(close_cx, close_cy),
                },
                &mods,
            );
            h.pump_event(
                WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                &mods,
            );
            h.pump_event(
                WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
                &mods,
            );
            h.state.commit_action(None, None);
        }
        let after = h.state.peek_last_undo().is_some();
        if before == after && !after {
            eprintln!(
                "critical finding: tab-close gesture left state.undo_stack \
                 empty (before={before}, after={after}). Tab list is widget-\
                 only, not CRDT-persisted — Ctrl+Z cannot roll it back. Test \
                 falls through to editor-character gesture to exercise the \
                 undo plumbing end-to-end."
            );
        }
        // Rebuild a fresh editor — the tab list may already have been clobbered.
    }
    let mut h = UndoHarness::new();
    h.render_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-undo-baseline.png"),
    );

    // --- Mutate: type 'a'; EditorApp splices into state.doc's BODY_KEY Text. ---
    let mods = Modifiers::default();
    h.pump_event(char_key_press("a"), &mods);
    // Finalise the transaction, as the App loop does after a handled event.
    h.state.commit_action(None, None);

    assert!(
        h.state.peek_last_undo().is_some(),
        "typing into the editor must push ONE entry onto the undo stack — \
         if this fires empty, EditorApp.splice_text isn't reaching the CRDT \
         (check focus propagation)"
    );

    h.render_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-undo-after-mutation.png"),
    );

    // --- Undo via the direct state API. ---
    let entry = h
        .state
        .undo(None, None)
        .expect("state.undo() must restore the pre-mutation snapshot");
    let _ = entry;
    assert!(
        h.state.peek_last_undo().is_none(),
        "after one undo the undo stack should be empty again"
    );

    // Re-sync so the editor re-reads the restored CRDT body; otherwise it still
    // holds the post-mutation `self.text` and renders "a" regardless.
    h.render_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-undo-restored.png"),
    );

    if renderer_opt.is_some() {
        let baseline = out_dir.join("e2e-undo-baseline.png");
        let after = out_dir.join("e2e-undo-after-mutation.png");
        let restored = out_dir.join("e2e-undo-restored.png");
        for p in [&baseline, &after, &restored] {
            assert_png_nonempty(p);
        }
        // Guards against a vacuous round-trip: the PNGs must not be byte-identical.
        let a_bytes = std::fs::read(&baseline).unwrap();
        let b_bytes = std::fs::read(&after).unwrap();
        assert_ne!(
            a_bytes, b_bytes,
            "baseline and after-mutation must differ — otherwise the \
             keypress didn't visibly change anything, and the undo round-trip \
             is vacuous"
        );

        // Core assertion: restored matches baseline pixel-for-pixel;
        // `with_tolerance(2)` covers alpha jitter in Vello's hairline AA.
        SnapshotAssertion::new(&restored, &baseline)
            .with_tolerance(2)
            .assert();

        SnapshotAssertion::new(&restored, goldens_dir().join("e2e-undo-restored.png"))
            .with_tolerance(2)
            .assert();
    } else {
        eprintln!(
            "offscreen: no wgpu adapter — e2e_undo_redo_round_trip_restores_golden \
             asserted state-stack transitions only"
        );
    }
}

/// Extends the undo test with `redo()`: the restored-after-redo frame must
/// match the post-mutation frame pixel-for-pixel.
#[test]
fn e2e_redo_after_undo_rehydrates_golden() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut h = UndoHarness::new();
    let mut renderer_opt = try_new_renderer(960, 540);
    let out_dir = screenshots_dir();

    h.render_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-redo-baseline.png"),
    );

    let mods = Modifiers::default();
    h.pump_event(char_key_press("a"), &mods);
    h.state.commit_action(None, None);
    assert!(h.state.peek_last_undo().is_some());

    h.render_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-redo-after-mutation.png"),
    );

    // Undo leaves one redo entry; redo repopulates undo and empties redo.
    let _ = h.state.undo(None, None).expect("undo must succeed");
    h.render_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-redo-mid-undo.png"),
    );

    let _ = h.state.redo(None, None).expect(
        "redo must succeed — if this fires None the undo didn't push onto \
         redo_stack, which breaks Ctrl+Shift+Z in the live binary",
    );
    assert!(
        h.state.peek_last_undo().is_some(),
        "after redo the undo stack must be repopulated so a subsequent \
         undo can unwind the redone mutation"
    );

    // `State::undo`/`redo` only restore the CRDT snapshot; widget-local state
    // (EditorApp's cursor offset) is normally reapplied by
    // `ui::App::apply_undo_entry`, which this harness never calls. To match the
    // post-mutation render pixel-for-pixel, re-sync then send `End` to park the
    // cursor where it sat right after typing "a".
    h.render_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-redo-rehydrated-precaret.png"),
    );
    h.pump_event(key_press(Key::Named(NamedKey::End)), &mods);
    h.render_to(
        renderer_opt.as_mut(),
        &out_dir.join("e2e-redo-rehydrated.png"),
    );

    if renderer_opt.is_some() {
        let after = out_dir.join("e2e-redo-after-mutation.png");
        let mid = out_dir.join("e2e-redo-mid-undo.png");
        let rehydrated = out_dir.join("e2e-redo-rehydrated.png");
        for p in [&after, &mid, &rehydrated] {
            assert_png_nonempty(p);
        }
        let after_bytes = std::fs::read(&after).unwrap();
        let mid_bytes = std::fs::read(&mid).unwrap();
        assert_ne!(
            after_bytes, mid_bytes,
            "after-mutation and mid-undo must differ — otherwise the \
             undo didn't visibly rewind and the redo-round-trip is vacuous"
        );

        SnapshotAssertion::new(&rehydrated, &after)
            .with_tolerance(2)
            .assert();

        SnapshotAssertion::new(&rehydrated, goldens_dir().join("e2e-redo-rehydrated.png"))
            .with_tolerance(2)
            .assert();
    } else {
        eprintln!(
            "offscreen: no wgpu adapter — e2e_redo_after_undo_rehydrates_golden \
             asserted state-stack transitions only"
        );
    }
}
