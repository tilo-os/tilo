//! A screenshot freezes the windows' content when it opens: every window
//! keeps showing (and is captured from) the frame it had then, while the
//! windows themselves still move.
#![allow(
    clippy::significant_drop_tightening,
    reason = "the frame builder's chrome guards are held for one short step each"
)]

use super::*;
use crate::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree, compose_id,
};
use automerge::{AutoCommit, ROOT};
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

const SALT: u16 = 1;
const fn id(counter: u64) -> u64 {
    compose_id(SALT, counter)
}
const STRIP: StripId = StripId(id(91));
const STUB: GroupId = id(3003);
const SIZE: (u32, u32) = (1600, 900);
/// The window focused when the screenshot opens, on screen.
const FOCUSED: u64 = 0xF2EE_0001;
/// The window beside it, partly past the right edge of the screen.
const BESIDE: u64 = 0xF2EE_0002;
/// A window drawn on the GPU.
const GPU: u64 = 0xF2EE_0003;
/// What each window showed when the screenshot opened.
const OLD_FOCUSED: [u8; 4] = [0xE0, 0x18, 0xA0, 0xFF];
const OLD_BESIDE: [u8; 4] = [0x20, 0x40, 0xE0, 0xFF];
/// What the clients commit while the screenshot is open.
const NEW: [u8; 4] = [0x18, 0xC0, 0x40, 0xFF];

const fn old(surface: u64) -> [u8; 4] {
    if surface == FOCUSED {
        OLD_FOCUSED
    } else {
        OLD_BESIDE
    }
}

/// A window that is no client's, focused, alone on the strip: the client
/// windows join it as they are adopted.
fn tree() -> WorkspaceTree {
    let ws_id = WorkspaceId(9);
    let mut ws = Workspace::new(ws_id, "Desk");
    let tab = STUB.saturating_add(5000);
    ws.strips = vec![Strip {
        id: STRIP,
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: STUB,
            tabs: vec![Tab {
                id: tab,
                title: "Stub".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Test".into()),
                    label: "Stub".into(),
                },
                app_title: None,
            }],
            active_tab: tab,
            width: WindowWidth::Fraction(700),
            maximized: false,
            focus_history: Vec::new(),
        }],
    }];
    let mut tree = WorkspaceTree::default();
    tree.set_salt_override(SALT);
    tree.desktop.workspaces = vec![ws];
    tree.desktop.active = ws_id;
    tree.desktop.active_strip.insert(ws_id, STRIP);
    tree.desktop.active_group.insert(STRIP, STUB);
    tree
}

/// The hold is the process's, as the frame store is: tests that hold the
/// windows take turns.
pub(super) fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The desktop as the compositor's loop drives it: each frame is painted,
/// then the capture executor runs over it.
struct Desk {
    fb: FrameBuilder,
    executor: CaptureExecutor,
    modifiers: Modifiers,
    /// Each client window's tab.
    tabs: [(u64, u64); 2],
    _pictures: tempfile::TempDir,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl Desk {
    /// Both client windows showing their first frame.
    fn new() -> Self {
        let pictures = tempfile::tempdir().expect("a pictures directory");
        let mut desk = Self {
            fb: FrameBuilder::with_tree(tree(), SIZE),
            executor: CaptureExecutor {
                pictures: Some(pictures.path().to_path_buf()),
                ..CaptureExecutor::default()
            },
            modifiers: Modifiers::default(),
            tabs: [(FOCUSED, 0), (BESIDE, 0)],
            _pictures: pictures,
            _serial: serial(),
        };
        let store = crate::shell::foreign::store();
        store.release_frames();
        for surface in [FOCUSED, BESIDE] {
            store.forget(surface);
            store.note_adopted(surface);
        }
        desk.adopt_clients();
        desk.settle();
        for surface in [FOCUSED, BESIDE] {
            desk.commit(surface, old(surface));
        }
        desk.settle();
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline
            && !(store.is_presented(FOCUSED) && store.is_presented(BESIDE))
        {
            desk.settle();
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
        // Past the first frames' cross-fade in, and the windows grown to
        // their width; then each client draws at its window's size.
        std::thread::sleep(std::time::Duration::from_millis(300));
        desk.settle();
        for surface in [FOCUSED, BESIDE] {
            desk.resting(surface);
        }
        for surface in [FOCUSED, BESIDE] {
            desk.commit(surface, old(surface));
        }
        desk.settle();
        for surface in [FOCUSED, BESIDE] {
            assert!(
                desk.shows(old(surface)),
                "precondition: {surface:#x} shows its first frame"
            );
        }
        desk
    }

    /// Adopt both clients as the compositor does a new toplevel, then lay
    /// the strip out as: the focused client window, the one beside it, the
    /// stub; each most of the screen wide.
    fn adopt_clients(&mut self) {
        {
            let chrome = self.fb.chrome();
            let mut tree = chrome.tree_mut();
            for (surface, tab) in &mut self.tabs {
                *tab = crate::shell::tab_binding::adopt_toplevel_at(
                    &mut tree,
                    *surface,
                    Some(format!("org.example.freeze.{surface:x}")),
                    crate::shell::tab_binding::Placement::NewGroup,
                );
            }
            let tabs = self.tabs;
            let strip = tree
                .desktop
                .workspaces
                .iter_mut()
                .flat_map(|ws| ws.strips.iter_mut())
                .find(|strip| strip.id == STRIP)
                .expect("the strip");
            let rank = |group: &WindowGroup| {
                tabs.iter()
                    .position(|(_, tab)| group.tabs.iter().any(|t| t.id == *tab))
                    .unwrap_or(tabs.len())
            };
            strip.window_groups.sort_by_key(rank);
            for group in &mut strip.window_groups {
                group.width = WindowWidth::Fraction(700);
            }
            let focused = strip.window_groups[0].id;
            tree.desktop.active_group.insert(STRIP, focused);
        }
        self.fb
            .with_chrome(crate::shell::chrome_state::ChromeState::bump);
        self.settle();
        self.fb.host_mut().set_group_widths(700);
    }

    fn tab_of(&self, surface: u64) -> u64 {
        self.tabs
            .iter()
            .find(|(s, _)| *s == surface)
            .map(|(_, tab)| *tab)
            .expect("a client window")
    }

    /// The group showing `surface`.
    fn group_of(&self, surface: u64) -> GroupId {
        let tab = self.tab_of(surface);
        self.fb
            .chrome()
            .tree()
            .desktop
            .workspaces
            .iter()
            .flat_map(|ws| &ws.strips)
            .flat_map(|strip| &strip.window_groups)
            .find(|group| group.tabs.iter().any(|t| t.id == tab))
            .map(|group| group.id)
            .expect("the client window's group")
    }

    /// The client commits a buffer filled with `texel`, sized to its window.
    fn commit(&mut self, surface: u64, texel: [u8; 4]) {
        let store = crate::shell::foreign::store();
        let (width, height) = store
            .viewport(surface)
            .or_else(|| {
                self.fb.foreign_surface_rect(surface).map(|rect| {
                    (
                        rect.width().round().max(1.0) as u32,
                        rect.height().round().max(1.0) as u32,
                    )
                })
            })
            .unwrap_or((800, 600));
        store.publish_rgba(
            surface,
            &texel.repeat((width * height) as usize),
            width,
            height,
        );
    }

    /// A few frames of the compositor's loop.
    fn settle(&mut self) {
        for _ in 0..4 {
            let mut doc = AutoCommit::new();
            self.fb.pump_apps(&mut doc, &ROOT);
            self.fb.relayout();
            let mut frame = self.fb.paint();
            let _ = self.executor.execute_ready(&mut self.fb, &mut frame);
        }
    }

    fn dispatch(&mut self, event: &WindowEvent) {
        let mut doc = AutoCommit::new();
        let mut focus = None;
        self.fb.dispatch_event(
            event,
            &self.modifiers,
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
            &mut focus,
        );
    }

    fn key(&mut self, key: &str) {
        self.dispatch(&ui::synthetic_input::char_key(key));
        self.settle();
    }

    fn open_screenshot(&mut self) {
        assert!(
            self.fb
                .host_mut()
                .open_capture_studio(CaptureMode::Screenshot)
        );
        self.settle();
        assert!(self.fb.capture_choosing_screenshot());
    }

    /// Whether the frame on screen draws a window filled with `texel`.
    fn shows(&mut self, texel: [u8; 4]) -> bool {
        draws(&self.fb.paint(), texel)
    }

    /// Whether the window the shell runs itself, last on the strip and off
    /// screen, is held still.
    fn stub_held(&mut self) -> bool {
        let host = self.fb.host_mut();
        let last = (0..8)
            .filter_map(|index| host.group_at_for_tests(STRIP, index).map(|_| index))
            .last()
            .expect("the strip has windows");
        host.group_at_for_tests(STRIP, last)
            .and_then(|group| group.active_host())
            .is_some_and(ui::widget::TileHost::content_held)
    }

    /// Where `surface`'s window is, on screen or off it.
    fn window(&self, surface: u64) -> Rect {
        let suffix = format!(":tab:{}", self.tab_of(surface));
        let targets = self.fb.host().capture_targets();
        targets
            .iter()
            .find(|t| t.id.ends_with(&suffix))
            .map_or_else(
                || panic!("{surface:#x} ({suffix}) is laid out: {targets:#?}"),
                |t| t.virtual_rect,
            )
    }

    /// Settle until `surface`'s window stops moving (the strip's standard
    /// motion runs on the wall clock), and return where it rests.
    fn resting(&mut self, surface: u64) -> Rect {
        let mut last = self.window(surface);
        for _ in 0..120 {
            std::thread::sleep(std::time::Duration::from_millis(16));
            self.settle();
            let now = self.window(surface);
            if (now.x0 - last.x0).abs() < 0.01 && (now.x1 - last.x1).abs() < 0.01 {
                return now;
            }
            last = now;
        }
        last
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        let store = crate::shell::foreign::store();
        store.release_frames();
        for surface in [FOCUSED, BESIDE] {
            store.forget(surface);
        }
    }
}

/// Whether `scene` draws an image filled with `texel`: a client frame,
/// shared with the frame store or cut from it.
fn draws(scene: &Scene, texel: [u8; 4]) -> bool {
    scene
        .encoding()
        .resources
        .patches
        .iter()
        .any(|patch| match patch {
            vello_encoding::Patch::Image { image, .. } => {
                let data = image.data.data();
                data.len() >= 4 && data.chunks_exact(4).take(64).all(|px| px == texel)
            }
            _ => false,
        })
}

fn intent(
    source: SourceMode,
    regions: Vec<Rect>,
    targets: Vec<ui::widget::CaptureTarget>,
) -> CaptureIntent {
    CaptureIntent {
        mode: CaptureMode::Screenshot,
        source,
        destination: CaptureDestination::FileAndClipboard,
        regions,
        anchors: Vec::new(),
        include_pointer: false,
        audio: AudioCaptureMode::None,
        pip_layout: PipLayout::Auto,
        targets,
        recording_command: None,
        replay_command: None,
        screenshot_command: None,
    }
}

// A client that commits a new buffer after the screenshot opened shows
// its old frame on screen, whatever target is chosen, and every capture
// (the window, the strip, a selection, the screen) holds the old frame.
#[test]
fn a_screenshot_freezes_every_window_it_opens_on_for_every_target() {
    let mut desk = Desk::new();
    desk.open_screenshot();
    let store = crate::shell::foreign::store();
    assert!(
        store.frames_held(),
        "opening the screenshot holds the windows"
    );
    assert!(
        desk.stub_held(),
        "and the app window the shell runs itself, off screen beyond them"
    );

    for surface in [FOCUSED, BESIDE] {
        desk.commit(surface, NEW);
    }
    desk.settle();
    // Whatever the target, the windows stay as they were.
    for (key, source) in [
        ("1", SourceMode::Region),
        ("2", SourceMode::Window),
        ("4", SourceMode::Strip),
        ("3", SourceMode::Display),
    ] {
        desk.key(key);
        assert_eq!(desk.fb.host().capture_studio_source_for_tests(), source);
        assert!(store.frames_held(), "{source:?}: still held");
        assert!(
            desk.shows(OLD_FOCUSED),
            "{source:?}: the screen shows the old frame"
        );
        assert!(!desk.shows(NEW), "{source:?}: never the new one");
    }

    let clean = desk.fb.paint();
    let targets = desk.fb.host().capture_targets();
    let tabs: std::collections::HashMap<u64, u64> = desk.tabs.into_iter().collect();
    let target = |surface: u64| {
        targets
            .iter()
            .find(|t| t.id.ends_with(&format!(":tab:{}", tabs[&surface])))
            .cloned()
            .unwrap_or_else(|| panic!("{surface:#x} is a target: {targets:?}"))
    };
    let focused = target(FOCUSED);
    let beside = target(BESIDE);
    let strip: Vec<_> = targets
        .iter()
        .filter(|t| t.on_active_strip)
        .cloned()
        .collect();
    let screen = Rect::new(0.0, 0.0, f64::from(SIZE.0), f64::from(SIZE.1));
    let selection = focused
        .visible_rect
        .inset(-40.0)
        .intersect(focused.visible_rect);
    for (what, intent, held) in [
        (
            "the focused window",
            intent(
                SourceMode::Window,
                vec![focused.visible_rect],
                vec![focused],
            ),
            OLD_FOCUSED,
        ),
        (
            "the window partly off screen, whole",
            intent(SourceMode::Window, vec![beside.visible_rect], vec![beside]),
            OLD_BESIDE,
        ),
        (
            "the strip",
            intent(SourceMode::Strip, Vec::new(), strip),
            OLD_BESIDE,
        ),
        (
            "a selection",
            intent(SourceMode::Region, vec![selection], Vec::new()),
            OLD_FOCUSED,
        ),
        (
            "the screen",
            intent(SourceMode::Display, vec![screen], Vec::new()),
            OLD_FOCUSED,
        ),
    ] {
        let (source, regions) = capture_source(&mut desk.fb, &clean, &intent);
        assert!(
            !regions.is_empty() && regions.iter().all(|r| r.area() > 0.0),
            "{what} resolves to pixels: {regions:?}"
        );
        assert!(
            draws(&source, held),
            "{what} is captured as it was when the screenshot opened"
        );
        assert!(!draws(&source, NEW), "{what} never holds the newer frame");
    }
}

// The hold ends with the screenshot: closed, or taken, the windows show
// what their clients committed meanwhile, and the frames only the hold
// kept go back.
#[test]
fn the_held_windows_are_released_when_the_screenshot_closes_or_is_taken() {
    let store = crate::shell::foreign::store();
    // Closed.
    let mut desk = Desk::new();
    desk.open_screenshot();
    desk.commit(FOCUSED, NEW);
    desk.settle();
    assert!(desk.shows(OLD_FOCUSED), "precondition: held");
    desk.dispatch(&ui::synthetic_input::key_press(
        Key::Named(NamedKey::Escape),
        None,
    ));
    desk.settle();
    assert!(
        !desk.fb.host().capture_studio_open(),
        "Escape closed the tool"
    );
    assert!(!store.frames_held(), "closing releases the held frames");
    assert!(!desk.stub_held(), "and the shell's own app window");
    assert!(desk.shows(NEW), "the window shows its new frame");
    assert!(!desk.shows(OLD_FOCUSED), "and no longer the held one");
    drop(desk);

    // Taken.
    let selection = crate::compositor::selection::SharedSelection::default();
    let _host = ui::host::enter(&crate::compositor::selection::host_services(selection));
    let mut desk = Desk::new();
    desk.open_screenshot();
    desk.key("3");
    desk.commit(FOCUSED, NEW);
    desk.settle();
    assert!(desk.shows(OLD_FOCUSED), "precondition: held");
    desk.dispatch(&ui::synthetic_input::key_press(
        Key::Named(NamedKey::Enter),
        None,
    ));
    desk.settle();
    assert!(
        !desk.fb.capture_choosing_screenshot(),
        "Enter took the screenshot"
    );
    assert!(
        !store.frames_held(),
        "taking the screenshot releases the held frames"
    );
    assert!(desk.shows(NEW), "the window shows its new frame");
    drop(desk);

    // A GPU frame held lives exactly as long as the hold.
    let mut context = ui::gpu::RenderContext::new();
    let Some(device) = pollster::block_on(context.device(None)) else {
        eprintln!("no wgpu adapter; skipping the GPU frame");
        return;
    };
    let handle = context.devices.get(device).expect("the device just made");
    let frame = |texel: [u8; 4]| {
        crate::shell::gpu_frames::frame_for_test(
            &handle.device,
            &handle.queue,
            0,
            (8, 8),
            texel,
            GPU,
        )
    };
    let _serial = serial();
    store.forget(GPU);
    store.note_adopted(GPU);
    let held = frame(OLD_FOCUSED);
    let shown = std::sync::Arc::downgrade(&held);
    let _ = store.publish_gpu_for_test(GPU, held);
    assert!(store.hold_frames());
    let _ = store.publish_gpu_for_test(GPU, frame(NEW));
    assert!(
        shown.upgrade().is_some(),
        "the client moved on, the hold still draws the frame it showed"
    );
    assert!(store.release_frames());
    assert!(shown.upgrade().is_none(), "released with the hold");
    store.forget(GPU);
    crate::shell::gpu_frames::forget_surface(GPU);
}

// Frozen windows still move: moving focus to the window beside scrolls
// the strip to it as usual, and it comes into view as it was when the
// screenshot opened.
#[test]
fn the_strip_scrolls_to_the_picked_window_while_the_windows_are_frozen() {
    let mut desk = Desk::new();
    let before = desk.window(BESIDE);
    assert!(
        before.x1 > f64::from(SIZE.0),
        "precondition: the window beside is partly off screen: {before:?}"
    );
    desk.open_screenshot();
    desk.key("2");
    desk.commit(BESIDE, NEW);
    desk.settle();

    desk.modifiers = Modifiers::from(ModifiersState::SUPER);
    desk.dispatch(&ui::synthetic_input::key_press(
        Key::Named(NamedKey::ArrowRight),
        None,
    ));
    desk.modifiers = Modifiers::default();
    let shown = desk.resting(BESIDE);
    assert_eq!(
        desk.fb.chrome().tree().desktop.active_group_for(STRIP),
        Some(desk.group_of(BESIDE)),
        "focus moved to the window beside"
    );
    assert!(
        shown.x0 >= -0.5 && shown.x1 <= f64::from(SIZE.0) + 0.5,
        "the strip scrolled it into view: {shown:?} (from {before:?})"
    );
    assert!(
        desk.fb.host().capture_studio_open(),
        "the screenshot is still open"
    );
    assert!(
        crate::shell::foreign::store().frames_held(),
        "and the windows still held"
    );
    assert!(
        desk.shows(OLD_BESIDE),
        "it shows the frame it had when the screenshot opened"
    );
    assert!(!desk.shows(NEW), "not the one committed since");
}
