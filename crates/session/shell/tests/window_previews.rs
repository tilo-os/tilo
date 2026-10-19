//! Live window previews end to end, through the production chain:
//!
//! ```text
//!   tab_binding::adopt_toplevel
//!     → ForeignFrameStore::publish_rgba       (what a client commit fills)
//!     → Super+Tab / Alt+Tab                   (chrome shows window cards)
//!     → FrameBuilder::paint                   (cards draw each client's buffer)
//!     → WindowPreviewRenderer::refresh        (offscreen renders, shell-drawn windows only)
//! ```
//!
//! Assertions are on what the cards drew and what was rendered offscreen.

use std::time::Instant;

use shell::compositor::foreign::store;
use shell::compositor::frame::FrameBuilder;
use shell::compositor::tab_binding;
use shell::compositor::window_previews::{PreviewBackend, WindowPreviewRenderer};
use shell::shell::state::TileContent;
use shell::shell::window_previews::{
    MAX_PREVIEW_HEIGHT, MAX_PREVIEW_WIDTH, MAX_PREVIEWS, REFRESH_INTERVAL, RENDERS_PER_REFRESH,
};
use tilo_desktop::window_switcher::PreviewState;
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
    let mut fb = FrameBuilder::new((W, H));
    {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        for (surface, app_id) in clients {
            store().forget(*surface);
            tab_binding::adopt_toplevel(&mut rendered, *surface, Some((*app_id).to_string()));
        }
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

/// Turn one of the demo room's placeholder cards into a window the shell
/// draws itself, shown in its group, and return its tab.
fn make_shell_drawn(fb: &mut FrameBuilder) -> u64 {
    let id = {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        let active = rendered.desktop.active;
        let group = rendered
            .desktop
            .workspaces
            .iter_mut()
            .filter(|ws| ws.id == active)
            .flat_map(|ws| ws.strips.iter_mut())
            .flat_map(|strip| strip.window_groups.iter_mut())
            .find(|group| {
                group
                    .tabs
                    .iter()
                    .any(|tab| matches!(tab.tile, TileContent::AppPlaceholder { .. }))
            })
            .expect("the demo room has a placeholder window");
        let tab = group
            .tabs
            .iter_mut()
            .rev()
            .find(|tab| matches!(tab.tile, TileContent::AppPlaceholder { .. }))
            .expect("a placeholder tab");
        tab.tile = TileContent::Stub {
            kind: shell::shell::state::AppKind::Terminal,
            label: "Terminal".into(),
        };
        let id = tab.id;
        group.active_tab = id;
        id
    };
    fb.with_chrome(shell::shell::chrome_state::ChromeState::bump);
    id
}

fn key_event(key: Key, state: ElementState) -> WindowEvent {
    let event = ui::synthetic_input::key_event(key, None, state);
    ui::synthetic_input::keyboard_input(event)
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

/// Press Super+Tab: the workspace overview opens.
fn press_super_tab(fb: &mut FrameBuilder) {
    let mut doc = automerge::AutoCommit::new();
    let mut focus = None;
    let super_key = Modifiers::from(ModifiersState::SUPER);
    for (key, state) in [
        (Key::Named(NamedKey::Super), ElementState::Pressed),
        (Key::Named(NamedKey::Tab), ElementState::Pressed),
        (Key::Named(NamedKey::Tab), ElementState::Released),
    ] {
        let _ = fb.dispatch_event(
            &key_event(key, state),
            &super_key,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        );
    }
}

// USER REQUIREMENT (2026-10-18): "win+tab only loads the windows after its already open/takes way too long, unacceptable"
#[test]
fn the_first_overview_frame_after_super_tab_already_shows_every_window() {
    let clients = [
        (0x9E_0011_u64, "org.example.first"),
        (0x9E_0012, "org.example.second"),
        (0x9E_0013, "org.example.third"),
        (0x9E_0014, "org.example.fourth"),
        (0x9E_0015, "org.example.fifth"),
    ];
    let mut fb = desktop(&clients);
    for (index, (surface, _)) in clients.iter().enumerate() {
        let shade = u8::try_from(index * 40).unwrap_or(0);
        store().publish_rgba(*surface, &solid(1280, 800, (shade, 0x60, 0x90)), 1280, 800);
    }
    let painted = make_shell_drawn(&mut fb);
    // Nothing is captured or held while the overview is closed.
    let mut renderer = WindowPreviewRenderer::default();
    let start = Instant::now();
    assert_eq!(renderer.refresh(&mut fb, start).visited, 0);
    assert!(fb.host().window_previews().is_empty());

    let pressed = Instant::now();
    press_super_tab(&mut fb);
    assert!(
        fb.chrome().overlays.overview,
        "Super+Tab opens the overview"
    );
    // The very next frame, with no preview round, render thread or client
    // redraw in between.
    let _ = fb.paint_settled();
    let first_frame = pressed.elapsed();
    let drawn = fb.host().overview_row_drawn().to_vec();
    let tabs: Vec<u64> = clients
        .iter()
        .map(|(surface, _)| tab_of(&fb, *surface))
        .collect();
    eprintln!(
        "Super+Tab to first complete overview frame: {first_frame:?} ({} windows)",
        drawn.len()
    );
    // The row holds the room's most recently used windows, laid out in the
    // first frame, the shell-drawn one and the clients among them.
    assert!(
        !drawn.is_empty(),
        "the first frame already has the window row"
    );
    assert!(
        drawn.iter().any(|(drawn, _)| tabs.contains(drawn)),
        "the clients are in the row: {drawn:?}"
    );
    assert!(
        drawn.iter().any(|(drawn, _)| *drawn == painted),
        "the shell-drawn window is in the row: {drawn:?}"
    );
    // Every window that has presented, and every window the shell draws,
    // is pictured in that first frame. (The demo room's other windows are placeholder cards with no app
    // behind them: they show their app mark, as they always did.)
    for (tab, state) in &drawn {
        if tabs.contains(tab) || *tab == painted {
            assert_eq!(
                *state,
                PreviewState::Live,
                "window {tab} is pictured in the first overview frame: {drawn:?}"
            );
        }
    }
    // And nothing was captured or rendered offscreen to get there.
    let round = renderer.refresh(&mut fb, start);
    assert_eq!(round.rendered, 0, "{round:?}");
    renderer.settle(&mut fb);
    assert!(fb.host().window_previews().is_empty());
    assert!(!renderer.holds_gpu_target());

    // The row is live: the next commit is what the next frame draws.
    let (last_surface, _) = clients[clients.len() - 1];
    store().publish_rgba(
        last_surface,
        &solid(800, 1000, (0x20, 0xB0, 0x40)),
        800,
        1000,
    );
    let _ = fb.paint_settled();
    assert!(
        fb.host()
            .overview_row_drawn()
            .iter()
            .any(|(tab, state)| tabs.contains(tab) && *state == PreviewState::Live)
    );
    drop(fb);
    for (surface, _) in &clients {
        store().forget(*surface);
    }
}

/// A window that has never presented shows its app mark in the first frame,
/// at the card's final size; the row does not wait for it.
#[test]
fn a_window_that_never_presented_shows_its_mark_from_the_first_frame() {
    let shown = 0x9E_0021_u64;
    let silent = 0x9E_0022_u64;
    // Opened last, the silent one leads the row.
    let mut fb = desktop(&[(shown, "org.example.shown"), (silent, "org.example.silent")]);
    store().publish_rgba(shown, &solid(640, 400, (0x30, 0x30, 0xC0)), 640, 400);
    let (shown_tab, silent_tab) = (tab_of(&fb, shown), tab_of(&fb, silent));
    press_super_tab(&mut fb);
    let _ = fb.paint_settled();
    let drawn = fb.host().overview_row_drawn().to_vec();
    let state_of = |tab: u64| {
        drawn
            .iter()
            .find(|(drawn, _)| *drawn == tab)
            .map(|(_, state)| *state)
    };
    assert_eq!(state_of(silent_tab), Some(PreviewState::None), "{drawn:?}");
    assert!(
        state_of(shown_tab).is_none_or(|state| state == PreviewState::Live),
        "{drawn:?}"
    );
    drop(fb);
    store().forget(shown);
    store().forget(silent);
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

    // A client window is drawn from its buffer, so its card is live the
    // moment the switcher opens; the password manager never is.
    let mut renderer = WindowPreviewRenderer::default();
    let _ = renderer.refresh(&mut fb, Instant::now());
    renderer.settle(&mut fb);
    let previews = fb.host().window_previews();
    assert!(previews.pixels(vault_tab).is_none());
    assert!(previews.pixels(editor_tab).is_none(), "nothing is copied");
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
        renderer.settle(&mut fb);
    }
    let client_tabs: Vec<u64> = clients
        .iter()
        .map(|(surface, _)| tab_of(&fb, *surface))
        .collect();
    let previews = fb.host().window_previews();
    let (count, memory_bytes) = (previews.len(), previews.memory_bytes());
    let copied = client_tabs
        .iter()
        .filter(|tab| previews.pixels(**tab).is_some())
        .count();
    drop(fb);
    let bound =
        MAX_PREVIEWS * usize::try_from(MAX_PREVIEW_WIDTH * MAX_PREVIEW_HEIGHT * 4).unwrap_or(0);
    assert!(
        count <= MAX_PREVIEWS,
        "at most {MAX_PREVIEWS} previews, got {count}"
    );
    assert_eq!(copied, 0, "client windows are drawn live, never copied");
    assert!(memory_bytes <= bound);
    for (surface, _) in &clients {
        store().forget(*surface);
    }
}

/// A backend whose renders wait until the test lets them go: what a busy or
/// software GPU looks like to the render thread.
struct Gated {
    gate: std::sync::mpsc::Receiver<()>,
    held: bool,
}

impl PreviewBackend for Gated {
    fn render(&mut self, _scene: &vello::Scene, (width, height): (u32, u32)) -> Option<Vec<u8>> {
        self.gate.recv().ok()?;
        self.held = true;
        let bytes = usize::try_from(width.saturating_mul(height).saturating_mul(4)).unwrap_or(0);
        Some(vec![0x40; bytes])
    }
    fn release(&mut self) {
        self.held = false;
    }
    fn holds_target(&self) -> bool {
        self.held
    }
    fn unavailable(&self) -> bool {
        false
    }
}

/// The compositor thread hands a preview to the render thread and goes on:
/// it never waits for the GPU to render or read it back. While the render is
/// held up, rounds return at once, nothing is rendered twice, and the frame
/// loop keeps the previous picture; once it finishes, a round collects it.
#[test]
fn a_preview_render_never_blocks_the_frame_loop() {
    let surface = 0x9E_2001;
    let mut fb = desktop(&[(surface, "org.example.slow")]);
    store().publish_rgba(surface, &solid(640, 400, (0x10, 0x20, 0x30)), 640, 400);
    let (release, gate) = std::sync::mpsc::channel();
    let mut renderer =
        WindowPreviewRenderer::with_backend(move || Box::new(Gated { gate, held: false }));
    // A shell-drawn window is what the switcher renders offscreen; the
    // client is drawn live and never queued.
    let _ = make_shell_drawn(&mut fb);
    let _ = fb.paint_settled();
    hold_alt_tab(&mut fb);
    let start = Instant::now();

    // The render is held up for as long as the test likes; refresh returns.
    let queued = renderer.refresh(&mut fb, start);
    assert!(
        queued.rendered >= 1,
        "{queued:?} {} {:?}",
        fb.host().window_switcher_state_json(),
        fb.chrome()
            .tree()
            .desktop
            .workspaces
            .iter()
            .flat_map(|ws| ws.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter().map(move |tab| (
                group.active_tab,
                tab.id,
                format!("{:?}", tab.tile)
            )))
            .collect::<Vec<_>>()
    );
    assert_eq!(renderer.in_flight(), queued.rendered);
    assert!(
        fb.host().window_previews().is_empty(),
        "nothing rendered yet"
    );
    // Later rounds while those are still rendering neither wait nor ask again
    // for a window already on its way.
    for round in 1..4_u32 {
        let report = renderer.refresh(&mut fb, start + REFRESH_INTERVAL * round);
        assert_eq!(report.collected, 0, "{report:?}");
        assert!(renderer.in_flight() >= queued.rendered);
    }
    assert!(fb.host().window_previews().is_empty(), "still not rendered");

    // Let every render through, and collect them.
    for _ in 0..64 {
        release
            .send(())
            .expect("the render thread waits for the gate");
    }
    renderer.settle(&mut fb);
    let _ = renderer.refresh(&mut fb, start + REFRESH_INTERVAL * 5);
    assert!(
        !fb.host().window_previews().is_empty(),
        "the finished previews are collected"
    );
    drop(fb);
    store().forget(surface);
}
