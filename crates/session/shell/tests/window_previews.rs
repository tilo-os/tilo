//! Live window previews end to end, through the production chain:
//!
//! ```text
//!   tab_binding::adopt_toplevel → foreign::reconcile_foreign_tabs
//!     → ForeignFrameStore::publish_rgba       (what a client commit fills)
//!     → the overview opens / Alt+Tab is held  (chrome now wants previews)
//!     → WindowPreviewRenderer::refresh        (what CaptureExecutor runs each frame)
//!     → ShellOverlayHost::window_previews     (what the cards paint)
//! ```
//!
//! Assertions are on the preview pixels and sizes the cards receive.

use std::time::Instant;

use shell::compositor::foreign::{self, store};
use shell::compositor::frame::FrameBuilder;
use shell::compositor::tab_binding;
use shell::compositor::window_previews::WindowPreviewRenderer;
use shell::shell::state::{TileContent, WorkspaceTree};
use shell::shell::window_previews::{
    MAX_PREVIEW_HEIGHT, MAX_PREVIEW_WIDTH, MAX_PREVIEWS, REFRESH_INTERVAL, RENDERS_PER_REFRESH,
};
use tilo_desktop::window_switcher::WindowThumbnails as _;
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

const W: u32 = 1600;
const H: u32 = 900;

fn solid(width: u32, height: u32, rgb: (u8, u8, u8)) -> Vec<u8> {
    let count = usize::try_from(width.saturating_mul(height)).unwrap_or(0);
    let mut out = Vec::with_capacity(count.saturating_mul(4));
    for _ in 0..count {
        out.extend_from_slice(&[rgb.0, rgb.1, rgb.2, 0xFF]);
    }
    out
}

fn frame(fb: &mut FrameBuilder) {
    let mut doc = automerge::AutoCommit::new();
    fb.pump_apps(&mut doc, &automerge::ROOT);
    let _ = fb.paint_settled();
}

fn hosts_surface(tile: &TileContent, surface: u64) -> bool {
    matches!(tile, TileContent::Foreign { surface_id, .. } if *surface_id == surface)
}

/// A frame builder whose current room holds the demo windows plus one
/// client per `(surface, app id)`, the first of them focused.
fn desktop(clients: &[(u64, &str)]) -> FrameBuilder {
    let mut protocol = WorkspaceTree::bootstrap_demo();
    for (surface, app_id) in clients {
        store().forget(*surface);
        tab_binding::adopt_toplevel(&mut protocol, *surface, Some((*app_id).to_string()));
    }
    let mut fb = FrameBuilder::new((W, H));
    {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        assert!(foreign::reconcile_foreign_tabs(&protocol, &mut rendered));
        if let Some((first, _)) = clients.first() {
            let focus = rendered
                .desktop
                .workspaces
                .iter()
                .flat_map(|ws| ws.strips.iter())
                .flat_map(|strip| strip.window_groups.iter().map(move |g| (strip.id, g)))
                .find(|(_, group)| {
                    group
                        .tabs
                        .iter()
                        .any(|tab| hosts_surface(&tab.tile, *first))
                })
                .map(|(strip, group)| (strip, group.id));
            let (strip, group) = focus.expect("the first client's group");
            rendered.desktop.set_active_group(strip, group);
        }
    }
    fb.with_chrome(shell::shell::chrome_state::ChromeState::bump);
    frame(&mut fb);
    fb
}

fn tab_of(fb: &FrameBuilder, surface: u64) -> u64 {
    let chrome = fb.chrome();
    let tree = chrome.tree();
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|ws| ws.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find(|tab| hosts_surface(&tab.tile, surface))
        .map(|tab| tab.id)
        .expect("the client's tab is in the rendered tree")
}

fn open_overview(fb: &mut FrameBuilder, open: bool) {
    fb.with_chrome(|state| {
        state.overlays.overview = open;
        state.bump();
    });
    frame(fb);
}

fn key_event(key: Key, state: ElementState) -> WindowEvent {
    let event = ui::synthetic_input::key_event(key, None, state);
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
}

/// Press Tab with Alt held, and keep Alt down: the switcher opens.
fn hold_alt_tab(fb: &mut FrameBuilder) {
    let mut doc = automerge::AutoCommit::new();
    let mut focus = None;
    let alt = Modifiers::from(ModifiersState::ALT);
    for (key, state) in [
        (Key::Named(NamedKey::Alt), ElementState::Pressed),
        (Key::Named(NamedKey::Tab), ElementState::Pressed),
        (Key::Named(NamedKey::Tab), ElementState::Released),
    ] {
        let _ = fb.dispatch_event(
            &key_event(key, state),
            &alt,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        );
    }
    assert!(
        fb.host().window_switcher_active(),
        "Alt+Tab opens the switcher"
    );
}

fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
    let at =
        usize::try_from(y.saturating_mul(width).saturating_add(x).saturating_mul(4)).unwrap_or(0);
    let px = rgba
        .get(at..at.saturating_add(4))
        .expect("the pixel is inside the preview");
    (px[0], px[1], px[2], px[3])
}

fn near(a: (u8, u8, u8, u8), rgb: (u8, u8, u8)) -> bool {
    a.0.abs_diff(rgb.0) <= 6 && a.1.abs_diff(rgb.1) <= 6 && a.2.abs_diff(rgb.2) <= 6
}

#[test]
fn a_client_window_gets_a_downscaled_live_preview_at_most_ten_times_a_second() {
    let surface = 0x9E_0001;
    let mut fb = desktop(&[(surface, "org.example.previewed")]);
    let tab = tab_of(&fb, surface);
    store().publish_rgba(surface, &solid(1600, 1000, (0xE0, 0x30, 0x90)), 1600, 1000);

    let mut renderer = WindowPreviewRenderer::default();
    let start = Instant::now();
    // Nothing is asked for, and nothing held, while no chrome shows previews.
    assert_eq!(renderer.refresh(&mut fb, start).visited, 0);
    assert!(fb.host().window_previews().is_empty());
    assert!(!renderer.holds_gpu_target());

    open_overview(&mut fb, true);
    let first = renderer.refresh(&mut fb, start);
    assert!(
        first.rendered >= 1,
        "the first round renders the client: {first:?}"
    );
    let previews = fb.host().window_previews();
    let (width, height) = previews.size(tab).expect("the client has a preview");
    // 1600×1000 is 16:10, scaled to fit the preview bound.
    assert_eq!((width, height), (MAX_PREVIEW_WIDTH, 220));
    assert!(width <= MAX_PREVIEW_WIDTH && height <= MAX_PREVIEW_HEIGHT);
    let pixels = previews.pixels(tab).expect("pixels");
    assert_eq!(
        pixels.len(),
        usize::try_from(width * height * 4).unwrap_or(0)
    );
    for (x, y) in [(width / 2, height / 2), (2, 2), (width - 3, height - 3)] {
        let sample = pixel(&pixels, width, x, y);
        assert!(
            near(sample, (0xE0, 0x30, 0x90)),
            "the preview shows the client's pixels at ({x}, {y}): {sample:?}"
        );
    }

    // Throttled: a round inside the interval does nothing at all.
    let soon = start + REFRESH_INTERVAL / 2;
    assert_eq!(renderer.refresh(&mut fb, soon).visited, 0);

    // A new commit is picked up by the next round, in the window's new shape.
    store().publish_rgba(surface, &solid(800, 1000, (0x20, 0xB0, 0x40)), 800, 1000);
    let later = start + REFRESH_INTERVAL;
    let _ = renderer.refresh(&mut fb, later);
    let previews = fb.host().window_previews();
    let (width, height) = previews.size(tab).expect("still previewed");
    assert_eq!((width, height), (176, 220), "a tall window keeps its shape");
    let pixels = previews.pixels(tab).expect("pixels");
    assert!(near(
        pixel(&pixels, width, width / 2, height / 2),
        (0x20, 0xB0, 0x40)
    ));

    // An unchanged buffer is not rendered again.
    let stamp = fb.host().window_preview_stamp(tab);
    let _ = renderer.refresh(&mut fb, later + REFRESH_INTERVAL);
    assert_eq!(fb.host().window_preview_stamp(tab), stamp);

    // Closing the overview drops every preview and the GPU target.
    open_overview(&mut fb, false);
    let _ = renderer.refresh(&mut fb, later + REFRESH_INTERVAL * 2);
    let previews = fb.host().window_previews();
    let (emptied, memory_bytes) = (previews.is_empty(), previews.memory_bytes());
    drop(fb);
    assert!(emptied);
    assert_eq!(memory_bytes, 0);
    assert!(!renderer.holds_gpu_target());
    store().forget(surface);
}

#[test]
fn a_password_manager_never_gets_a_preview_and_the_switcher_says_so() {
    let vault = 0x9E_0002;
    let editor = 0x9E_0003;
    let mut fb = desktop(&[
        (vault, "org.keepassxc.KeePassXC"),
        (editor, "org.example.editor"),
    ]);
    let vault_tab = tab_of(&fb, vault);
    let editor_tab = tab_of(&fb, editor);
    store().publish_rgba(vault, &solid(640, 400, (0xFF, 0x00, 0x00)), 640, 400);
    store().publish_rgba(editor, &solid(640, 400, (0x00, 0x00, 0xFF)), 640, 400);
    hold_alt_tab(&mut fb);

    let mut renderer = WindowPreviewRenderer::default();
    let start = Instant::now();
    for round in 0..6_u32 {
        let _ = renderer.refresh(&mut fb, start + REFRESH_INTERVAL * round);
    }
    let previews = fb.host().window_previews();
    assert!(previews.has(editor_tab), "an ordinary window is previewed");
    assert!(
        !previews.has(vault_tab),
        "a password manager is never pictured"
    );
    assert_eq!(fb.host().window_preview_stamp(vault_tab), None);

    // state-dump reports each card's preview, never its pixels.
    let dump = fb.host().window_switcher_state_json();
    drop(fb);
    assert_eq!(dump["open"], true);
    let windows = dump["windows"].as_array().expect("windows");
    let preview_of = |tab: u64| {
        windows
            .iter()
            .find(|window| window["tab"] == tab)
            .map(|window| window["preview"].clone())
            .expect("the window is listed")
    };
    assert_eq!(preview_of(editor_tab), "live");
    assert_eq!(preview_of(vault_tab), "withheld");
    assert!(
        !dump.to_string().contains("rgba"),
        "the dump carries no pixel data: {dump}"
    );
    store().forget(vault);
    store().forget(editor);
}

#[test]
fn previews_stay_within_their_memory_bound() {
    let surfaces: Vec<(u64, String)> = (0..20_u64)
        .map(|i| (0x9E_1000 + i, format!("org.example.window{i}")))
        .collect();
    let clients: Vec<(u64, &str)> = surfaces
        .iter()
        .map(|(surface, app)| (*surface, app.as_str()))
        .collect();
    let mut fb = desktop(&clients);
    for (surface, _) in &clients {
        store().publish_rgba(*surface, &solid(640, 360, (0x80, 0x80, 0x80)), 640, 360);
    }
    hold_alt_tab(&mut fb);
    let mut renderer = WindowPreviewRenderer::default();
    let start = Instant::now();
    for round in 0..12_u32 {
        let report = renderer.refresh(&mut fb, start + REFRESH_INTERVAL * round);
        assert!(
            report.rendered <= RENDERS_PER_REFRESH,
            "a round renders a bounded number of windows: {report:?}"
        );
    }
    let previews = fb.host().window_previews();
    let (count, memory_bytes) = (previews.len(), previews.memory_bytes());
    drop(fb);
    let bound =
        MAX_PREVIEWS * usize::try_from(MAX_PREVIEW_WIDTH * MAX_PREVIEW_HEIGHT * 4).unwrap_or(0);
    assert!(
        count <= MAX_PREVIEWS,
        "at most {MAX_PREVIEWS} previews, got {count}"
    );
    assert!(
        count > RENDERS_PER_REFRESH * 2,
        "rounds take turns until the windows past the first are pictured too, got {count}"
    );
    assert!(memory_bytes <= bound);
    for (surface, _) in &clients {
        store().forget(*surface);
    }
}
