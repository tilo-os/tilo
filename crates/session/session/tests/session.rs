//! The native session on the headless backend with a real Wayland client: its window becomes a tab
//! of the desk's strip, shown where the strip lays that tab out; the chrome above the windows draws
//! the top bar; closing the tab asks the client to close.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::struct_excessive_bools,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

mod common;

use std::cell::RefCell;
use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tilo_actions::ResourceSpec;
use tilo_app::framework::testing::HeadlessApp;
use tilo_compositor::clipboard_history::TEXT_MIME;
use tilo_compositor::{Compositor, InputEvent, KeyRepeat, OutputConfig, Selection};
use tilo_desktop::desk_verbs::{
    CHROME_RESOURCE, LAUNCHPAD_RESOURCE, WINDOW_KIND, launchpad_card_id,
};
use tilo_onboarding::OnboardingEffect;
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_session::{CLIPBOARD_RESOURCE, Options, Session, quick_settings};
use tilo_sheets::Seat as _;
use tilo_sheets::file_picker::{FileAnswer, FileMode, FilePickerEffect, FileRequest};
use tilo_sheets::portal::{Body, Dialog, PortalEffect};
use tilo_sheets::power::PowerEffect;
use tilo_sheets::workspace_settings::{WorkspaceSettingsApp, WorkspaceSettingsEffect};
use tilo_surface_host::Published;
use tilo_test_env::{PrivateBus, ScopedEnv};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_keyboard,
    wl_keyboard::WlKeyboard, wl_pointer, wl_pointer::WlPointer, wl_registry,
    wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3, zwp_text_input_v3::ZwpTextInputV3,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

#[derive(Default)]
struct Seen {
    globals: Vec<String>,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    text_input_manager: Option<ZwpTextInputManagerV3>,
    text_input: Option<ZwpTextInputV3>,
    configured: bool,
    pointer_inside: bool,
    buttons: Vec<(u32, bool)>,
    keyboard_focused: bool,
    keys: Vec<(u32, bool)>,
    frames_done: u32,
    closed: bool,
    size: Option<(i32, i32)>,
}

/// One client with one toplevel of a solid colour.
struct TestClient {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    surface: Option<WlSurface>,
    toplevel: Option<XdgToplevel>,
}

impl TestClient {
    fn connect(compositor: &mut Compositor) -> Self {
        let socket = compositor.connect_client().expect("client socket");
        let connection = Connection::from_socket(socket).expect("connection");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Self {
            connection,
            queue,
            seen: Seen::default(),
            surface: None,
            toplevel: None,
        }
    }

    /// Send what is queued, read what arrived, without blocking.
    fn exchange(&mut self) {
        self.connection.flush().expect("flush");
        if let Some(guard) = self.connection.prepare_read() {
            // Nothing to read is fine: WouldBlock.
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.seen)
            .expect("dispatch");
        self.connection.flush().expect("flush");
    }

    fn open_window(&mut self, app_id: &str) {
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let xdg = self
            .seen
            .wm_base
            .as_ref()
            .expect("xdg_wm_base")
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        surface.commit();
        self.surface = Some(surface);
        self.toplevel = Some(toplevel);
    }

    /// Close the window: its toplevel goes, its surface stays.
    fn close_window(&mut self) {
        if let Some(toplevel) = self.toplevel.take() {
            toplevel.destroy();
        }
    }

    /// Type into a text field of this window, as a toolkit does when one takes the focus: the field
    /// asks the compositor for typing, which only a keyboard's input method can answer.
    fn type_into_a_field(&mut self) {
        let qh = self.queue.handle();
        if self.seen.text_input.is_none() {
            let manager = self
                .seen
                .text_input_manager
                .as_ref()
                .expect("zwp_text_input_manager_v3");
            let seat = self.seen.seat.as_ref().expect("wl_seat");
            self.seen.text_input = Some(manager.get_text_input(seat, &qh, ()));
        }
        let field = self.seen.text_input.as_ref().expect("a text field");
        field.enable();
        field.commit();
        self.connection.flush().expect("flush");
    }

    /// Attach a buffer of one colour (RGBA) and commit it with a frame callback.
    fn show(&self, width: i32, height: i32, rgba: [u8; 4]) {
        let qh = self.queue.handle();
        let stride = width * 4;
        let len = usize::try_from(stride * height).unwrap();
        let fd = rustix::fs::memfd_create("tilo-compositor-test", rustix::fs::MemfdFlags::CLOEXEC)
            .unwrap();
        let mut file = std::fs::File::from(fd);
        // ARGB8888 is B, G, R, A in memory.
        let pixel = [rgba[2], rgba[1], rgba[0], rgba[3]];
        file.write_all(&pixel.repeat(len / 4)).unwrap();
        let pool = self.seen.shm.as_ref().expect("wl_shm").create_pool(
            file.as_fd(),
            stride * height,
            &qh,
            (),
        );
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        let surface = self.surface.as_ref().expect("surface");
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.frame(&qh, ());
        surface.commit();
        pool.destroy();
    }
}

/// Let the session and clients talk until `done` holds.
/// Counting steps would not do: what the session waits for is often a worker thread (a folder read,
/// a bus call), and a busy machine can take many steps' worth of time to get to it.
#[track_caller]
fn pump(
    session: &mut Session,
    clients: &mut [&mut TestClient],
    mut done: impl FnMut(&mut Session, &[&mut TestClient]) -> bool,
) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        for client in clients.iter_mut() {
            client.exchange();
        }
        session.step(Duration::from_millis(2)).expect("step");
        for client in clients.iter_mut() {
            client.exchange();
        }
        if done(session, clients) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "gave up waiting for the session and its client after {:?}; it looks hung \
             (set TILO_TEST_HANG_BOUND_SECS to wait longer)",
            tilo_test_env::hang_bound()
        );
    }
}

/// Step `session` with `client` until `asked` answers.
#[track_caller]
fn until<T>(
    session: &mut Session,
    client: &mut TestClient,
    mut asked: impl FnMut(&mut Session) -> Option<T>,
) -> Option<T> {
    let mut answer = None;
    pump(session, &mut [client], |session, _| {
        answer = asked(session);
        answer.is_some()
    });
    answer
}

/// The control assistive technology names `name` under `node` , by target.
fn control(node: &ui::a11y::AccessNode, name: &str) -> Option<String> {
    if node.name.as_deref() == Some(name)
        && let Some(target) = &node.action_target
    {
        return Some(target.clone());
    }
    node.children.iter().find_map(|child| control(child, name))
}

fn session() -> Session {
    // Headless tests take no part in the collaboration plane: no endpoint, no workspace.
    crate::common::headless(Options {
        collab: false,
        ..Options::default()
    })
}

/// A session whose app lists are `lists` : what the desk offers to open.
fn session_with_lists(lists: tilo_session::AppLists) -> Session {
    crate::common::headless(Options {
        collab: false,
        search: tilo_session::SearchOptions {
            apps: Some(std::sync::Arc::new(tilo_session::Lists::fixed(lists))),
            ..tilo_session::SearchOptions::default()
        },
        ..Options::default()
    })
}

/// A session that keeps this device's desk under `state` , the way one with a state directory does.
fn session_keeping(state: &std::path::Path) -> Session {
    crate::common::headless(Options {
        state: Some(state.to_path_buf()),
        ..Options::default()
    })
}

/// Connect a client and open a toplevel of `app_id` , drawn at the size it was configured to, and
/// wait until it is mapped.
fn mapped(session: &mut Session, app_id: &str) -> TestClient {
    let mut client = TestClient::connect(session.compositor());
    pump(session, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some() && clients[0].seen.seat.is_some()
    });
    client.open_window(app_id);
    pump(session, &mut [&mut client], |_, clients| {
        clients[0].seen.configured
    });
    let (w, h) = client
        .seen
        .size
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((300, 200));
    client.show(w, h, [200, 40, 40, 255]);
    let app = app_id.to_owned();
    pump(session, &mut [&mut client], |session, _| {
        window(session.compositor(), &app).is_some()
    });
    client
}

/// The window of `app` and its rectangle `(x, y, w, h)` .
fn window(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))
        .map(|(info, rect)| (info.id, rect))
}

/// The resource the desk publishes for the window of `app` , its tab. The client's window is titled
/// by its own app id, so the desk names it as it names any window: its title and the app it shows.
fn window_resource(session: &Session, app: &str) -> Option<ResourceSpec> {
    fn find(resources: &[ResourceSpec], label: &str) -> Option<ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.kind == WINDOW_KIND && resource.label == label {
                Some(resource.clone())
            } else {
                find(&resource.children, label)
            }
        })
    }
    let label = tilo_desktop_search::names::window_name(app, app);
    find(&session.resources(), &label)
}

/// Invoke `action` of `resource` with `arguments` , then let the session run a frame.
fn invoke_with(session: &mut Session, resource: &str, action: &str, arguments: &serde_json::Value) {
    let empty = serde_json::Map::new();
    let arguments = arguments.as_object().unwrap_or(&empty);
    session
        .invoke(resource, action, arguments)
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
    session.step(Duration::from_millis(1)).unwrap();
}

/// Invoke `action` of `resource` with no arguments, then let the session run a frame.
fn invoke(session: &mut Session, resource: &str, action: &str) {
    session
        .invoke(resource, action, &serde_json::Map::new())
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
    session.step(Duration::from_millis(1)).unwrap();
}

#[test]
fn a_window_becomes_a_tab_of_the_strip_and_shows_where_the_strip_lays_it_out() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let _client = mapped(&mut session, "org.example.Terminal");
    let (id, rect) = window(session.compositor(), "org.example.Terminal").unwrap();
    let tab = window_resource(&session, "org.example.Terminal")
        .expect("the desk publishes the window's tab");
    assert!(
        tab.actions.iter().any(|action| action.id == "close-tab"),
        "a window's tab can be closed by its address"
    );
    let layout = session.desktop().with_policy(|policy| policy.layout());
    let (_, shown) = layout
        .shown
        .iter()
        .find(|(window, _)| *window == id)
        .expect("the window's tab is shown");
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a window's place on a screen"
    )]
    let at = (shown.x0.round() as i32, shown.y0.round() as i32);
    assert_eq!(
        (rect.0, rect.1),
        at,
        "the window sits where the strip laid its tab out"
    );
    assert!(
        f64::from(rect.1) >= f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX).floor(),
        "under the top bar: {rect:?}"
    );
}

#[test]
fn the_top_bar_draws_above_the_desk_and_opens_the_overview_by_its_action() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    let names = session.accessible_labels();
    assert!(!names.is_empty(), "the chrome names what it shows");
    let frame = session.compositor().screenshot("HEADLESS-1").unwrap();
    let band = (0..frame.width)
        .step_by(4)
        .filter_map(|x| frame.pixel(x, 12))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(band.len() > 2, "the bar draws in its band: {names:?}");

    invoke(&mut session, CHROME_RESOURCE, "overview");
    session.step(Duration::from_millis(1)).unwrap();
    let open = session.accessible_labels();
    assert_ne!(open, names, "the overview shows over the desk");
    let shot = session.compositor().screenshot("HEADLESS-1").unwrap();
    assert_ne!(
        shot.rgba, frame.rgba,
        "the overview changes what the screen shows"
    );
}

#[test]
fn closing_a_windows_tab_asks_its_client_to_close() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut client = mapped(&mut session, "org.example.Editor");
    let tab = window_resource(&session, "org.example.Editor").unwrap();
    invoke(&mut session, tab.id.as_str(), "close-tab");
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.closed
    });
    assert!(
        window_resource(&session, "org.example.Editor").is_none(),
        "the tab is gone from the desk"
    );
}

/// The colour at the centre of `rect` on the display.
fn centre(session: &mut Session, rect: (i32, i32, i32, i32)) -> [u8; 4] {
    let shot = session.compositor().screenshot("HEADLESS-1").unwrap();
    let x = u32::try_from(rect.0 + rect.2 / 2).unwrap();
    let y = u32::try_from(rect.1 + rect.3 / 2).unwrap();
    shot.pixel(x, y).unwrap()
}

/// The colours across the bar's band on the display.
fn band(session: &mut Session) -> std::collections::BTreeSet<[u8; 4]> {
    let frame = session.compositor().screenshot("HEADLESS-1").unwrap();
    (0..frame.width)
        .step_by(4)
        .filter_map(|x| frame.pixel(x, 12))
        .collect()
}

/// The colour the display shows most across the bar's band.
fn band_colour(session: &mut Session) -> [u8; 4] {
    let frame = session.compositor().screenshot("HEADLESS-1").unwrap();
    let mut counts = std::collections::BTreeMap::new();
    for x in (0..frame.width).step_by(4) {
        if let Some(pixel) = frame.pixel(x, 12) {
            *counts.entry(pixel).or_insert(0_usize) += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(colour, _)| colour)
        .unwrap()
}

/// Step the session and exchange with `client` until `done` holds: the bound is the suite's hang
/// bound, so a busy machine slows the wait instead of failing it.
#[track_caller]
fn within(
    session: &mut Session,
    client: &mut TestClient,
    mut done: impl FnMut(&mut Session) -> bool,
) {
    pump(session, &mut [client], |session, _| done(session));
}

/// Wait for the size the desktop configures the window to, draw it at that size as its client does,
/// and wait until the compositor shows it there.
fn take_size(session: &mut Session, client: &mut TestClient, app: &str, size: (i32, i32)) {
    let until = tilo_test_env::hang_deadline();
    while std::time::Instant::now() < until && client.seen.size != Some(size) {
        client.exchange();
        session.step(Duration::from_millis(4)).expect("step");
    }
    assert_eq!(
        client.seen.size,
        Some(size),
        "the desktop asks the window for its new size"
    );
    client.show(size.0, size.1, [200, 40, 40, 255]);
    let app = app.to_owned();
    within(session, client, move |session| {
        window(session.compositor(), &app)
            .is_some_and(|(_, rect)| (rect.2, rect.3) == (size.0, size.1))
    });
}

/// A window taken across the display covers it, the bar's room included: the bar shows over it for
/// a moment, then goes, and draws again once the window gives the display back.
#[test]
fn a_full_screen_window_covers_the_display_and_the_bar_comes_back() {
    const APP: &str = "org.example.Cinema";
    const RED: [u8; 4] = [200, 40, 40, 255];
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut client = mapped(&mut session, APP);
    let (id, rect) = window(session.compositor(), APP).unwrap();
    assert!(
        f64::from(rect.1) >= f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX).floor(),
        "the window stands under the bar: {rect:?}"
    );
    assert!(
        band(&mut session).len() > 2,
        "and the bar draws in its band"
    );
    assert_ne!(
        band_colour(&mut session),
        RED,
        "the bar's band is not the window"
    );

    assert!(
        session
            .desktop()
            .with_policy(|policy| policy.set_fullscreen(id, true)),
        "the desk holds the window"
    );
    take_size(&mut session, &mut client, APP, (WIDTH, HEIGHT));
    assert_eq!(
        window(session.compositor(), APP),
        Some((id, (0, 0, WIDTH, HEIGHT))),
        "the display from its top edge to its bottom"
    );
    until(&mut session, &mut client, |session| {
        (band_colour(session) == RED).then_some(())
    })
    .expect("left alone, the bar goes and the window has its band");

    assert!(
        session
            .desktop()
            .with_policy(|policy| policy.set_fullscreen(id, false))
    );
    take_size(&mut session, &mut client, APP, (rect.2, rect.3));
    assert_eq!(
        window(session.compositor(), APP),
        Some((id, rect)),
        "the window stands where it stood"
    );
    assert!(band(&mut session).len() > 2, "the bar is back in its band");
    assert_ne!(
        band_colour(&mut session),
        RED,
        "and the band is not the window again"
    );
}

#[test]
fn a_window_fades_in_as_it_opens_and_its_last_frame_fades_out_as_it_closes() {
    const RED: [u8; 4] = [200, 40, 40, 255];
    let mut session = session();
    let mut client = mapped(&mut session, "org.example.Editor");
    let (id, rect) = window(session.compositor(), "org.example.Editor").unwrap();
    assert_ne!(centre(&mut session, rect), RED, "it starts faint");
    within(&mut session, &mut client, |session| {
        centre(session, rect) == RED
    });

    client.close_window();
    within(&mut session, &mut client, |session| {
        session
            .compositor()
            .closing_windows()
            .iter()
            .any(|(info, _)| info.id == id)
    });
    let last = centre(&mut session, rect);
    assert!(
        last[0] > 120 && last[1] < 100,
        "its last frame is still drawn: {last:?}"
    );
    within(&mut session, &mut client, |session| {
        session.compositor().closing_windows().is_empty()
    });
    assert_ne!(centre(&mut session, rect), RED, "and let go once it faded");
}

/// evdev codes the tests press.
mod keys {
    pub const TAB: u32 = 15;
    pub const A: u32 = 30;
    pub const C: u32 = 46;
    pub const D: u32 = 32;
    pub const LEFT_SHIFT: u32 = 42;
    pub const ESCAPE: u32 = 1;
    pub const ENTER: u32 = 28;
    pub const LEFT_META: u32 = 125;
    pub const R: u32 = 19;
    pub const VOLUME_UP: u32 = 115;
    pub const MUTE: u32 = 113;
    /// What the microphone's key sends.
    pub const MICROPHONE_MUTE: u32 = 248;
    pub const BRIGHTNESS_DOWN: u32 = 224;
    pub const PLAY_PAUSE: u32 = 164;
    pub const WLAN: u32 = 238;
    /// What laptops send for their touchpad key.
    pub const F21: u32 = 191;
}

fn key(session: &mut Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::Key { keycode, pressed })
        .unwrap();
    session.step(Duration::from_millis(1)).unwrap();
}

fn tap(session: &mut Session, keycode: u32) {
    key(session, keycode, true);
    key(session, keycode, false);
}

/// A session with the quick settings panel installed: the surface probe this package ships stands
/// in for its app, staying up while the session keeps it. The environment it reads is held for the
/// test's life.
fn session_with_quick_settings() -> (Session, ScopedEnv) {
    let mut environment = ScopedEnv::new();
    environment.set("SURFACE_PROBE_STAY", "1");
    let panel = quick_settings::surface_of(tilo_test_env::sibling_binary(
        "tilo-session",
        "tilo-surface-probe",
    ));
    let session = crate::common::headless(Options {
        surfaces: vec![panel],
        collab: false,
        ..Options::default()
    });
    (session, environment)
}

fn find_resource(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.0 == id {
            Some(resource.clone())
        } else {
            find_resource(&resource.children, id)
        }
    })
}

fn publishes(session: &Session, resource: &str, action: &str) -> bool {
    find_resource(&session.resources(), resource)
        .is_some_and(|found| found.actions.iter().any(|spec| spec.id == action))
}

#[test]
fn a_desktop_chord_works_while_a_window_has_the_keyboard_and_the_window_never_sees_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut client = mapped(&mut session, "org.example.Terminal");
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
    });
    let before = session.accessible_labels();
    key(&mut session, keys::LEFT_META, true);
    tap(&mut session, keys::TAB);
    key(&mut session, keys::LEFT_META, false);
    pump(&mut session, &mut [&mut client], |_, _| true);
    assert_ne!(
        session.accessible_labels(),
        before,
        "Super+Tab opened the overview"
    );
    assert!(
        !client.seen.keys.iter().any(|(code, _)| *code == keys::TAB),
        "the window never saw the chord's key: {:?}",
        client.seen.keys
    );
    // With the overview closed again, plain typing reaches the window.
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "overview",
        &serde_json::json!({"open": false}),
    );
    pump(&mut session, &mut [&mut client], |_, _| true);
    tap(&mut session, keys::A);
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.contains(&(keys::A, true))
    });
}

/// The machine's brightness controls as a test lights them: the display the test session shows, and
/// any other monitor it names.
#[derive(Clone, Default)]
struct Dials(Rc<RefCell<DialsInner>>);

#[derive(Default)]
struct DialsInner {
    levels: Vec<os_actions::brightness::MonitorLevel>,
    /// Every connector set, and to what percentage, in order.
    set: Vec<(String, f32)>,
}

impl Dials {
    fn with(levels: &[(&str, f32)]) -> Self {
        let dials = Self::default();
        dials.0.borrow_mut().levels = levels
            .iter()
            .map(|(connector, level)| os_actions::brightness::MonitorLevel {
                connector: (*connector).into(),
                level: *level,
                kind: os_actions::brightness::BrightnessKind::Backlight,
            })
            .collect();
        dials
    }

    /// What the service asked of the display at `connector` , in percent.
    fn asked(&self, connector: &str) -> Vec<f32> {
        self.0
            .borrow()
            .set
            .iter()
            .filter(|(asked, _)| asked == connector)
            .map(|(_, percent)| *percent)
            .collect()
    }
}

impl tilo_session::displays::Backlights for Dials {
    fn levels(&self) -> Vec<os_actions::brightness::MonitorLevel> {
        self.0.borrow().levels.clone()
    }

    fn set(&self, connector: &str, percent: f32) -> bool {
        let mut inner = self.0.borrow_mut();
        let Some(level) = inner
            .levels
            .iter_mut()
            .find(|level| level.connector == connector)
        else {
            return false;
        };
        level.level = percent / 100.0;
        inner.set.push((connector.to_owned(), percent));
        true
    }

    fn show_only(&self, _connectors: &[String]) {}

    fn software_levels(&self) -> Vec<(String, f32)> {
        // Every display here dims its own light.
        Vec::new()
    }
}

/// The connectivity service as the session reaches it: a device with a Wi-Fi radio, and every
/// change asked of it.
struct Wire {
    network: RefCell<tilo_connectivity_service::model::Network>,
    asked: Rc<RefCell<Vec<String>>>,
}

impl Wire {
    fn on(asked: Rc<RefCell<Vec<String>>>) -> Self {
        let mut network = tilo_connectivity_service::model::Network::default();
        network.wifi.available = true;
        network.wifi.on = true;
        Self {
            network: RefCell::new(network),
            asked,
        }
    }
}

impl tilo_session::Service for Wire {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![tilo_connectivity_service::network_resource(
            &self.network.borrow(),
        )]
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == tilo_connectivity_service::NETWORK)
            .then(|| serde_json::to_value(&*self.network.borrow()).ok())
            .flatten()
    }

    fn invoke(
        &mut self,
        _: &str,
        action: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        self.asked.borrow_mut().push(action.to_owned());
        Ok(tilo_service::Started::Accepted)
    }
}

/// What the session publishes of its displays now.
#[track_caller]
fn displays_reading(session: &Session) -> tilo_session::displays::Reading {
    let published = session
        .read(tilo_session::displays::RESOURCE)
        .expect("the session's displays publish");
    serde_json::from_value(published).expect("the reading reads back")
}

#[test]
fn the_volume_and_brightness_keys_work_with_no_quick_settings_panel() {
    let _settled = ui::motion::testing::settled();
    let asked = Rc::new(RefCell::new(Vec::new()));
    let wireless = Rc::new(RefCell::new(Vec::new()));
    let dials = Dials::with(&[("HEADLESS-1", 0.5)]);
    let mut session = crate::common::headless(Options {
        // No quick settings app: the keys reach the services the resources belong to.
        services: vec![
            Box::new(Audio::speakers(Rc::clone(&asked))),
            Box::new(Wire::on(Rc::clone(&wireless))),
        ],
        backlights: Some(Box::new(dials.clone())),
        collab: false,
        ..Options::default()
    });
    assert!(
        !publishes(&session, tilo_session::QUICK_SETTINGS, "volume-up"),
        "nothing shows the panel's effects here"
    );

    tap(&mut session, keys::MUTE);
    tap(&mut session, keys::BRIGHTNESS_DOWN);
    tap(&mut session, keys::WLAN);
    pump(&mut session, &mut [], |_, _| {
        asked.borrow().iter().any(|call| call == "toggle-muted")
            && !wireless.borrow().is_empty()
            && dials.asked("HEADLESS-1").len() == 1
    });
    assert_eq!(
        *wireless.borrow(),
        ["toggle-airplane-mode"],
        "the wireless key is the connectivity service's"
    );
    assert_eq!(
        *asked.borrow(),
        ["toggle-muted"],
        "the mute key is the audio service's"
    );
    assert_eq!(
        dials.asked("HEADLESS-1"),
        [40.0],
        "the brightness key is the displays service's, one step down"
    );
    let reading = displays_reading(&session);
    assert!(
        reading.displays.iter().any(|display| {
            display.connector == "HEADLESS-1"
                && display
                    .brightness
                    .is_some_and(|brightness| (brightness - 0.4).abs() < 0.001)
        }),
        "the reading follows the change: {reading:?}"
    );

    // A held key repeats into the service, as the panel's effect did.
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_millis(20),
        rate: 100,
    });
    key(&mut session, keys::VOLUME_UP, true);
    let repeats = |asked: &Rc<RefCell<Vec<String>>>| {
        asked
            .borrow()
            .iter()
            .filter(|call| *call == "volume-up")
            .count()
    };
    // The repeat is one per turn the compositor dispatches, however long a turnaround takes on a
    // busy machine: the test holds the key until the sound has been raised more than once, rather
    // than for a span of the host's clock.
    common::wait_for(&mut session, "a held volume key to ramp the sound", |_| {
        (repeats(&asked) > 3).then_some(())
    });
    key(&mut session, keys::VOLUME_UP, false);
    let louder = repeats(&asked);
    assert!(louder > 3, "holding volume up ramps it: {louder}");
    assert!(
        session
            .accessible_labels()
            .iter()
            .any(|label| label.contains("Muted")),
        "the readout shows the sound as the service reads it, here muted: {:?}",
        session.accessible_labels()
    );
    for _ in 0..5 {
        session.step(Duration::from_millis(10)).unwrap();
    }
    let after = asked
        .borrow()
        .iter()
        .filter(|call| *call == "volume-up")
        .count();
    assert_eq!(after, louder, "the release stops the repeat");
}

/// A service the session runs in its process: it publishes what plays,
/// and records what it was asked.
struct Player {
    asked: Rc<RefCell<Vec<String>>>,
}

impl Published for Player {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        let spec = |id: &'static str| {
            tilo_actions::ActionSpec::typed_operation(
                id,
                id,
                String::new(),
                tilo_actions::OperationKind::Effect,
                "test.player",
                Vec::new(),
                move |_| tilo_actions::EncodedAction::encode_operation("test.player", &id).unwrap(),
            )
        };
        vec![
            ResourceSpec::new(tilo_session::MEDIA, "media", "Player")
                .with_action(spec("play-pause"))
                .with_action(spec("next")),
        ]
    }

    fn invoke(
        &mut self,
        address: &tilo_actions::ActionAddress,
        _: &serde_json::Map<String, serde_json::Value>,
        _look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        self.asked.borrow_mut().push(address.action.clone());
        Ok(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

#[test]
fn the_play_key_reaches_whatever_publishes_the_player() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let asked = Rc::new(RefCell::new(Vec::new()));
    session.publish(Rc::new(RefCell::new(Player {
        asked: Rc::clone(&asked),
    })));
    assert!(publishes(&session, tilo_session::MEDIA, "play-pause"));
    tap(&mut session, keys::PLAY_PAUSE);
    assert_eq!(*asked.borrow(), ["play-pause"]);
}

#[test]
fn the_touchpad_key_and_the_sessions_own_action_switch_the_touchpad() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    assert!(session.input_config().touchpad_enabled);
    // The readout that answers the key stays up for a moment of its own, and a machine running a
    // whole suite at once can spend that moment on the turn that pressed the key: the press and the
    // read of what it says happen at one reading of the clock, so what is read is the readout the
    // key raised.
    ui::time::testing::with_monotonic_time(ui::time::now(), || {
        tap(&mut session, keys::F21);
        assert!(
            !session.input_config().touchpad_enabled,
            "the key turned it off"
        );
        assert!(
            session
                .accessible_labels()
                .contains(&"Touchpad off".to_owned()),
            "the readout says so: {:?}",
            session.accessible_labels()
        );
    });
    let mut on = serde_json::Map::new();
    on.insert("on".into(), true.into());
    session
        .invoke(tilo_session::INPUT_RESOURCE, "toggle-touchpad", &on)
        .unwrap();
    assert!(
        session.input_config().touchpad_enabled,
        "the action turned it on"
    );
}

/// Unlocks with "sesame" only.
struct Sesame;

impl tilo_session::unlock::PasswordVerifier for Sesame {
    fn verify(&self, _user: &str, password: &str) -> tilo_session::unlock::Verdict {
        if password == "sesame" {
            tilo_session::unlock::Verdict::Unlocks
        } else {
            tilo_session::unlock::Verdict::Wrong
        }
    }
}

/// A session action runner that records what it was asked to do, so a test can say nothing reached
/// the machine it runs on.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the shape every session action runner has"
)]
fn record_session_action(action: tilo_session::SessionAction) -> Result<(), String> {
    ACTIONS.with_borrow_mut(|actions| actions.push(action));
    Ok(())
}

thread_local! {
    static ACTIONS: RefCell<Vec<tilo_session::SessionAction>> = const { RefCell::new(Vec::new()) };
}

fn taken_actions() -> Vec<tilo_session::SessionAction> {
    ACTIONS.with_borrow_mut(std::mem::take)
}

/// A session in a window of another desktop runs no idle timers: that desktop owns idle, blanking,
/// locking and suspend.
#[test]
fn a_session_in_a_window_runs_no_idle_timers() {
    let _settled = ui::motion::testing::settled();
    let mut hosted = crate::common::headless(Options {
        machine: tilo_session::Machine::Window,
        ..Options::default()
    });
    assert_eq!(
        hosted.compositor().idle_config(),
        tilo_compositor::IdleConfig::never(),
        "a session in a window starts with no idle timers"
    );
    // The settings the person's settings app applies reach every session;
    // here they must not bring the timers back.
    hosted
        .compositor()
        .set_idle_config(tilo_compositor::IdleConfig::default());
    assert_eq!(
        hosted.compositor().idle_config(),
        tilo_compositor::IdleConfig::never(),
        "the host desktop owns idle, whatever this session is told"
    );

    let mut own = session();
    own.compositor()
        .set_idle_config(tilo_compositor::IdleConfig::default());
    assert_eq!(
        own.compositor().idle_config(),
        tilo_compositor::IdleConfig::default(),
        "a session with displays of its own keeps its timers"
    );
}

/// Every power choice a session in a window takes stays in that session: nothing reaches the
/// machine the window runs on, and shutting down ends the session rather than the host.
#[test]
fn a_session_in_a_window_takes_no_power_on_its_host() {
    use tilo_sheets::Seat as _;
    let _settled = ui::motion::testing::settled();
    let mut session = crate::common::headless(Options {
        machine: tilo_session::Machine::Window,
        session_actions: record_session_action,
        user: "ada".to_owned(),
        verifier: std::sync::Arc::new(Sesame),
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).unwrap();
    session
        .power_menu()
        .expect("power menu")
        .presentation()
        .open(false);
    session.step(Duration::from_millis(1)).unwrap();
    let locked = PowerEffect::LOCK.id;
    let menu = publisher(&session, locked)
        .into_iter()
        .find(|resource| resource != tilo_session::SESSION_RESOURCE)
        .expect("the open menu publishes its choices");

    // The machine's own power is not offered, and an action that asks for it anyway is refused
    // here.
    for action in [
        PowerEffect::SWITCH_USER.id,
        PowerEffect::SUSPEND.id,
        PowerEffect::RESTART.id,
    ] {
        assert!(
            session
                .invoke(&menu, action, &serde_json::Map::new())
                .is_err(),
            "{action} is not offered here"
        );
    }
    assert!(
        taken_actions().is_empty(),
        "no session action reached the host"
    );
    assert!(!session.ended(), "and the session runs on");

    // Shutting down ends this session and nothing else: the person's own press on the row is their
    // decision, and here it ends the session rather than the machine it runs on.
    let row = session
        .power_menu()
        .expect("power menu")
        .rows()
        .into_iter()
        .find(|(effect, _)| *effect == PowerEffect::ShutDown)
        .map(|(_, row)| row)
        .expect("the open menu has a shut down row");
    let (x, y) = (f64::midpoint(row.x0, row.x1), f64::midpoint(row.y0, row.y1));
    for event in [
        InputEvent::PointerMotion { x, y },
        InputEvent::PointerButton {
            button: 0x110,
            pressed: true,
        },
        InputEvent::PointerButton {
            button: 0x110,
            pressed: false,
        },
    ] {
        session.compositor().input(event).unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert!(session.ended(), "shutting down ends the session");
    assert!(
        taken_actions().is_empty(),
        "and did not reach the host's machine"
    );
}

/// A logout asked through the login wrapper's channel — what the power menu and the session's
/// services ask of it — ends the session in order: the wrapper tears the login down once this
/// process is gone, and the request it already holds is not written again.
#[test]
fn a_logout_asked_through_the_login_wrapper_ends_the_session() {
    use std::os::unix::fs::PermissionsExt as _;

    let _settled = ui::motion::testing::settled();
    let channel = tempfile::tempdir().expect("a channel");
    std::fs::set_permissions(channel.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the channel is this person's");
    let request = channel.path().join("logout-request");
    let mut environment = ScopedEnv::new();
    environment.set("TILO_SESSION_LOGOUT_REQUEST", request.as_os_str());
    let mut session = session();
    assert!(!session.ended(), "the session runs on");
    os_actions::request_logout().expect("the logout is asked for");
    session
        .step(Duration::from_millis(1))
        .expect("a turn of the session");
    assert!(session.ended(), "the asked logout ends the session");
    assert!(
        request.exists(),
        "the wrapper's request stays for the wrapper to act on"
    );
}

/// Beside the development control socket, the session serves the liveness door: the loop answers a
/// heartbeat asked from outside it, so a session whose loop has stopped can be told apart from a
/// slow machine.
#[test]
fn the_liveness_door_beside_the_control_socket_answers_a_heartbeat() {
    let _settled = ui::motion::testing::settled();
    let directory = tempfile::tempdir().expect("a directory");
    let control = directory.path().join("control.sock");
    let mut session = crate::common::headless(Options {
        collab: false,
        test_control: Some(control.clone()),
        ..Options::default()
    });
    let mut client = std::os::unix::net::UnixStream::connect(tilo_liveness::socket_path(&control))
        .expect("the door is bound beside the control socket");
    client
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("a read deadline");
    client
        .write_all(b"{\"op\":\"heartbeat\",\"budget_ms\":5000}\n")
        .expect("the heartbeat is asked for");
    let mut reader = std::io::BufReader::new(&mut client);
    let mut answer = String::new();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "the door answers: {answer:?}"
        );
        session
            .step(Duration::from_millis(1))
            .expect("a turn of the session");
        match std::io::BufRead::read_line(&mut reader, &mut answer) {
            Ok(0) => panic!("the door hung up: {answer:?}"),
            Ok(_) => break,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("the door does not answer: {error}"),
        }
    }
    assert!(
        answer.contains("\"alive\""),
        "the loop acknowledged the heartbeat: {answer}"
    );
}

/// A session that cannot check a password never locks: the lock screen could not be left again.
/// Locking stays on offer where a password does unlock it.
#[test]
fn a_session_that_cannot_check_a_password_never_locks() {
    use tilo_sheets::Seat as _;
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    // The session's own lock action, as the key or a client sends it.
    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    session.step(Duration::from_millis(1)).unwrap();
    assert!(
        !session.compositor().locked(),
        "no password can be checked here, so the screen stays unlocked"
    );
    // Nor does the power menu offer it.
    session
        .power_menu()
        .expect("power menu")
        .presentation()
        .open(false);
    session.step(Duration::from_millis(1)).unwrap();
    let menu = publisher(&session, PowerEffect::LOCK.id)
        .into_iter()
        .find(|resource| resource != tilo_session::SESSION_RESOURCE)
        .expect("the open menu publishes its choices");
    assert!(
        session
            .invoke(&menu, PowerEffect::LOCK.id, &serde_json::Map::new())
            .is_err(),
        "locking is not offered where nothing could unlock it"
    );
}

/// The power menu opens from the session's published verb, so the panel's press and any other
/// caller offered it put up the same menu.
#[test]
fn the_power_menu_opens_by_its_published_verb() {
    use tilo_sheets::Seat as _;
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    assert!(
        !session
            .power_menu()
            .is_some_and(|seat| seat.presentation().is_open()),
        "the menu is down to begin with"
    );

    invoke(&mut session, tilo_session::SESSION_RESOURCE, "power-menu");
    let menu = session.power_menu().expect("the power menu");
    assert!(menu.presentation().is_open(), "the verb puts the menu up");
    assert!(
        !menu.rows().is_empty(),
        "and it offers the person's choices"
    );
}

/// The power menu's shortcut puts it up: the chord, as a person presses it,
/// reaches the menu the same verb opens.
#[test]
fn the_power_menus_shortcut_opens_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "shortcut",
        &serde_json::json!({"action": "power_menu"}),
    );
    let menu = session.power_menu().expect("the power menu");
    assert!(
        menu.presentation().is_open(),
        "the shortcut puts the menu up"
    );
    assert!(!menu.rows().is_empty(), "with the person's choices");
}

/// The session publishes the actions only it can carry out, with what each is called, so every
/// surface listing them — the Tilo menu, search — offers the same rows; what this machine cannot do
/// is not offered.
#[test]
fn the_session_publishes_the_actions_only_it_can_carry_out() {
    let _settled = ui::motion::testing::settled();
    let mut own = crate::common::headless(Options {
        session_actions: record_session_action,
        user: "ada".to_owned(),
        verifier: std::sync::Arc::new(Sesame),
        ..Options::default()
    });
    own.step(Duration::from_millis(1)).unwrap();
    let offered: Vec<(
        String,
        String,
        Option<tilo_desktop_surface::HostActionSource>,
    )> = own
        .handle()
        .host_actions()
        .into_iter()
        .map(|action| (action.id, action.title, action.source))
        .collect();
    for (id, title) in [
        ("session-lock", "Lock"),
        ("session-log-out", "Log out"),
        ("session-restart", "Restart"),
        ("session-shut-down", "Shut down"),
    ] {
        assert!(
            offered.iter().any(|(held, called, source)| held == id
                && called == title
                && *source == Some(tilo_desktop_surface::HostActionSource::Session)),
            "the session offers {id} as {title}: {offered:?}"
        );
    }

    // A session in a window of another desktop owns neither the machine's power nor its session
    // switching, so it offers none of them.
    let mut windowed = crate::common::headless(Options {
        machine: tilo_session::Machine::Window,
        session_actions: record_session_action,
        user: "ada".to_owned(),
        verifier: std::sync::Arc::new(Sesame),
        ..Options::default()
    });
    windowed.step(Duration::from_millis(1)).unwrap();
    let offered: Vec<String> = windowed
        .handle()
        .host_actions()
        .into_iter()
        .map(|action| action.id)
        .collect();
    for id in ["session-suspend", "session-restart", "session-switch-user"] {
        assert!(
            !offered.iter().any(|held| held == id),
            "a session in a window does not offer {id}: {offered:?}"
        );
    }
    assert!(
        offered.iter().any(|held| held == "session-shut-down"),
        "shutting down is offered everywhere: {offered:?}"
    );
}

#[test]
fn quick_settings_opens_by_its_action_and_a_press_outside_closes_it() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _panel) = session_with_quick_settings();
    invoke(&mut session, CHROME_RESOURCE, "quick-settings");
    assert!(session.quick_settings_open());
    session.step(Duration::from_millis(1)).unwrap();
    // On the desk, far from the panel at the right.
    for event in [
        InputEvent::PointerMotion { x: 100.0, y: 400.0 },
        InputEvent::PointerButton {
            button: 0x110,
            pressed: true,
        },
        InputEvent::PointerButton {
            button: 0x110,
            pressed: false,
        },
    ] {
        session.compositor().input(event).unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert!(
        !session.quick_settings_open(),
        "the press outside closed it"
    );
    // Its shortcut opens it again.
    session
        .invoke(
            CHROME_RESOURCE,
            "shortcut",
            &serde_json::json!({"action": "quick_settings"})
                .as_object()
                .cloned()
                .unwrap(),
        )
        .unwrap();
    assert!(session.quick_settings_open());
}

#[test]
fn the_empty_desks_launchpad_is_published_until_a_window_opens() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    for card in [
        "chat", "notes", "tasks", "meet", "invite", "activity", "apps",
    ] {
        assert!(
            publishes(&session, &launchpad_card_id(card), "launchpad"),
            "the launchpad publishes a row for {card}"
        );
    }
    invoke(&mut session, &launchpad_card_id("apps"), "launchpad");
    let _client = mapped(&mut session, "org.example.Terminal");
    assert!(
        find_resource(&session.resources(), LAUNCHPAD_RESOURCE).is_none(),
        "a desk with a window shows no launchpad"
    );
}

#[test]
fn the_magnifier_zooms_the_screen_about_the_pointer_by_its_actions() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let _client = mapped(&mut session, "org.example.Terminal");
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: 300.0, y: 300.0 })
        .unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    let plain = session.compositor().screenshot("HEADLESS-1").unwrap();
    invoke(&mut session, tilo_session::MAGNIFIER_RESOURCE, "toggle");
    assert!((session.compositor().zoom() - 2.0).abs() < f64::EPSILON);
    invoke(&mut session, tilo_session::MAGNIFIER_RESOURCE, "zoom-in");
    assert!((session.compositor().zoom() - 3.0).abs() < f64::EPSILON);
    let zoomed = session.compositor().screenshot("HEADLESS-1").unwrap();
    assert_ne!(zoomed.rgba, plain.rgba, "the screen shows magnified");
    invoke(&mut session, tilo_session::MAGNIFIER_RESOURCE, "toggle");
    assert!((session.compositor().zoom() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn the_night_light_action_warms_the_displays_and_switches_back() {
    let mut session = session();
    assert_eq!(session.compositor().night_light(), None);
    invoke(&mut session, tilo_session::NIGHT_LIGHT_RESOURCE, "toggle");
    assert!(
        session
            .compositor()
            .night_light()
            .is_some_and(|kelvin| kelvin < 6_500)
    );
    session
        .invoke(
            tilo_session::NIGHT_LIGHT_RESOURCE,
            "toggle",
            &arguments(serde_json::json!({ "on": true })),
        )
        .unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    assert!(session.compositor().night_light().is_some(), "on stays on");
    invoke(&mut session, tilo_session::NIGHT_LIGHT_RESOURCE, "toggle");
    assert_eq!(session.compositor().night_light(), None);
}

#[test]
fn the_night_light_warms_to_the_persons_warmth() {
    let options = Options {
        presentation: ui::headless::fixture_runner_inputs,
        collab: false,
        config: tilo_session::SessionConfig {
            night_light: tilo_session::night_light::Preference {
                on: true,
                temperature: 2_400,
                scheduled: false,
            },
            ..tilo_session::SessionConfig::default()
        },
        ..Options::default()
    };
    let mut session = Session::headless(options, &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)])
        .expect("session");
    assert_eq!(
        session.compositor().night_light(),
        Some(2_400),
        "the warmth the person chose, not one the session picked"
    );
}

fn active_workspace(session: &Session) -> Option<u64> {
    session
        .handle()
        .read(|tree| tree.active_workspace().map(|workspace| workspace.slot.0))
}

/// The resource publishing `action` among everything the session publishes.
fn publisher(session: &Session, action: &str) -> Option<String> {
    fn find(resources: &[ResourceSpec], action: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.actions.iter().any(|offered| offered.id == action) {
                Some(resource.id.as_str().to_owned())
            } else {
                find(&resource.children, action)
            }
        })
    }
    find(&session.resources(), action)
}

/// Every resource publishing `action` .
fn publishers(session: &Session, action: &str) -> Vec<String> {
    fn find(resources: &[ResourceSpec], action: &str, found: &mut Vec<String>) {
        for resource in resources {
            if resource.actions.iter().any(|offered| offered.id == action) {
                found.push(resource.id.as_str().to_owned());
            }
            find(&resource.children, action, found);
        }
    }
    let mut found = Vec::new();
    find(&session.resources(), action, &mut found);
    found
}

/// The resource publishing `action` , once the action is available.
fn offering(session: &Session, action: &str) -> Option<String> {
    fn find(resources: &[ResourceSpec], action: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            let offered = resource.actions.iter().any(|offered| {
                offered.id == action
                    && offered.contract.availability == tilo_actions::Availability::Available
            });
            if offered {
                Some(resource.id.as_str().to_owned())
            } else {
                find(&resource.children, action)
            }
        })
    }
    find(&session.resources(), action)
}

fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

/// The audio service as the session reaches it: it publishes the sound as the service does and
/// records every change asked of it.
struct Audio {
    sound: RefCell<tilo_audio_service::sound::Sound>,
    asked: Rc<RefCell<Vec<String>>>,
}

impl Audio {
    /// Speakers playing quietly and muted, headphones to switch to, a muted microphone.
    fn speakers(asked: Rc<RefCell<Vec<String>>>) -> Self {
        use tilo_audio_service::sound::Device;
        let device = |node, label: &str, chosen| Device {
            node,
            name: format!("node-{node}"),
            label: label.into(),
            chosen,
            ..Device::default()
        };
        Self {
            sound: RefCell::new(tilo_audio_service::sound::Sound {
                output: Some(40),
                volume: Some(0.25),
                muted: true,
                input: Some(50),
                input_muted: true,
                outputs: vec![
                    device(40, "Speakers", true),
                    device(41, "Headphones", false),
                ],
                inputs: vec![device(50, "Microphone", true)],
                ..tilo_audio_service::sound::Sound::default()
            }),
            asked,
        }
    }
}

impl tilo_session::Service for Audio {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![tilo_audio_service::sound::resource(&self.sound.borrow())]
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == tilo_audio_service::sound::SOUND)
            .then(|| serde_json::to_value(&*self.sound.borrow()).expect("the sound serialises"))
    }

    fn invoke(
        &mut self,
        _: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        let named = arguments
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(" ");
        self.asked
            .borrow_mut()
            .push(format!("{action} {named}").trim_end().to_owned());
        Ok(tilo_service::Started::Accepted)
    }
}
/// The sound the session offers is the audio service's, and every change the person makes is the
/// service's own action: the session writes none of it.
#[test]
fn the_sound_is_the_audio_services_and_its_changes_are_the_services_actions() {
    let _settled = ui::motion::testing::settled();
    let asked = Rc::new(RefCell::new(Vec::new()));
    let mut session = crate::common::headless(Options {
        services: vec![Box::new(Audio::speakers(Rc::clone(&asked)))],
        ..Options::default()
    });

    // The sound the service reads is on offer from the moment it reads it.
    pump(&mut session, &mut [], |session, _| {
        offering(session, "set-volume").is_some()
    });

    // Each change is the service's action, with the value the person chose.
    let sound = offering(&session, "set-volume").unwrap();
    session
        .invoke(
            &sound,
            "set-volume",
            &arguments(serde_json::json!({ "level": 0.5 })),
        )
        .expect("set the volume");
    session
        .invoke(&sound, "toggle-muted", &serde_json::Map::new())
        .expect("mute");
    pump(&mut session, &mut [], |_, _| asked.borrow().len() == 2);
    assert_eq!(
        *asked.borrow(),
        ["set-volume level=0.5", "toggle-muted"],
        "each change is the service's, in the order made"
    );

    // The microphone's key is the service's too, and its answer on screen is the state the service
    // reads rather than a guess.
    tap(&mut session, keys::MICROPHONE_MUTE);
    pump(&mut session, &mut [], |session, _| {
        session
            .accessible_labels()
            .contains(&"Microphone muted".to_owned())
    });
    assert!(
        asked.borrow().contains(&"toggle-input-muted".to_owned()),
        "the key reaches the service: {:?}",
        asked.borrow()
    );
    assert_eq!(
        session
            .read(tilo_audio_service::sound::SOUND)
            .and_then(|sound| sound.get("output").cloned()),
        Some(serde_json::json!(40)),
        "the session reads the service's own sound"
    );
}

#[test]
fn setup_runs_over_the_desktop_through_its_actions_and_gives_it_back_when_done() {
    let _settled = ui::motion::testing::settled();
    let mut session = crate::common::headless(Options {
        setup: Some(tilo_session::BeforeDesk::Ceremony(Box::new(
            tilo_session::OnboardingView::new_tour(),
        ))),
        collab: false,
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).unwrap();
    assert!(session.setup_up());
    assert!(
        publisher(&session, "overview").is_none(),
        "setup withholds the desktop's actions"
    );
    assert!(
        session
            .invoke(CHROME_RESOURCE, "overview", &serde_json::Map::new())
            .is_err(),
        "and refuses them"
    );
    assert!(
        !session.accessible_labels().is_empty(),
        "setup names what it shows"
    );

    let skip = OnboardingEffect::SKIP.id;
    let setup = publisher(&session, skip).expect("setup publishes its steps");
    session
        .invoke(&setup, skip, &serde_json::Map::new())
        .expect("skip the tour");
    session.step(Duration::from_millis(1)).unwrap();
    assert!(!session.setup_up(), "skipping the tour finishes setup");
    assert!(
        publisher(&session, "overview").is_some(),
        "the desktop's actions are back"
    );
    assert!(publisher(&session, skip).is_none());
}

/// The device's tooling walks setup by its controls' names, as a screen reader
/// reaches them (`tilo-vm slot create --identity device`, onboarding-walk): the
/// steps of the setup app the ceremony shows are listed by their ids, and a
/// press by that name is the person's press.
#[test]
fn setup_lists_its_steps_by_name_and_a_press_by_name_takes_the_step() {
    let _settled = ui::motion::testing::settled();
    let mut session = crate::common::headless(Options {
        setup: Some(tilo_session::BeforeDesk::Ceremony(Box::new(
            tilo_session::OnboardingView::new_tour(),
        ))),
        collab: false,
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).unwrap();
    assert!(session.setup_up());
    let names: Vec<String> = session
        .controls()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let skip = "onboarding.tour.skip.button";
    assert!(
        names.iter().any(|name| name == skip),
        "setup's tour step is listed by its name: {names:?}"
    );
    session
        .press_action(skip, ui::window_targets::TargetAction::Activate)
        .expect("a press by name");
    session.step(Duration::from_millis(1)).unwrap();
    assert!(!session.setup_up(), "skipping the tour finishes setup");
}

#[test]
fn workspace_settings_renames_the_workspace_through_its_action() {
    use tilo_sheets::Seat as _;
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let seat = session
        .workspace_settings()
        .expect("workspace settings")
        .clone();
    seat.presentation().open(false);
    session.step(Duration::from_millis(1)).unwrap();
    let rename = WorkspaceSettingsEffect::RENAME.id;
    let sheet = publishers(&session, rename)
        .into_iter()
        .find(|resource| resource.starts_with("workspace-settings"))
        .expect("the open sheet publishes renaming");
    session
        .invoke(
            &sheet,
            rename,
            &arguments(serde_json::json!({ "name": "Garden" })),
        )
        .expect("rename");
    session.step(Duration::from_millis(1)).unwrap();
    let name = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.name.clone())
    });
    assert_eq!(name.as_deref(), Some("Garden"));
    // The bar says the name the workspace has now: the person sees the rename where the next frame
    // draws it.
    session.step(Duration::from_millis(1)).unwrap();
    let bar = session.accessible_labels().join("\n");
    assert!(
        bar.contains("Garden: workspaces, overview and strips"),
        "the bar shows the name the workspace has now: {bar}"
    );
}

/// Workspace settings shows the tab the person picked, and moves the workspace among the ones this
/// device has.
#[test]
fn workspace_settings_shows_a_tab_and_moves_the_workspace() {
    use tilo_sheets::Seat as _;
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let seat = session
        .workspace_settings()
        .expect("workspace settings")
        .clone();
    seat.presentation().open(false);
    session.step(Duration::from_millis(1)).unwrap();
    let show = WorkspaceSettingsEffect::SHOW_TAB.id;
    let sheet = publisher(&session, show).expect("the open sheet publishes its tabs");
    session
        .invoke(
            &sheet,
            show,
            &arguments(serde_json::json!({ "tab": "people" })),
        )
        .expect("show tab");
    session.step(Duration::from_millis(1)).unwrap();
    assert_eq!(
        seat.settings().tab,
        tilo_sheets::workspace_settings::Tab::People,
        "the sheet shows the tab the person picked"
    );

    invoke(&mut session, CHROME_RESOURCE, "new-workspace");
    let names: Vec<String> = workspaces(&session)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names.len(), 2, "two workspaces: {names:?}");
    let moved = WorkspaceSettingsEffect::MOVE.id;
    let sheet = publisher(&session, moved).expect("the open sheet publishes moving");
    session
        .invoke(&sheet, moved, &arguments(serde_json::json!({ "step": -1 })))
        .expect("move");
    session.step(Duration::from_millis(1)).unwrap();
    let after: Vec<String> = workspaces(&session)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        after,
        [names[1].clone(), names[0].clone()],
        "the workspace in front moved one place earlier"
    );
}

#[test]
fn a_portal_question_is_answered_through_the_sheets_actions() {
    use tilo_sheets::Seat as _;
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let seat = session.portal().expect("portal sheet").clone();
    seat.presentation().open(false);
    seat.show(Some(Dialog {
        handle: "request-1".to_owned(),
        title: "Share your screen?".to_owned(),
        heading: "Meet".to_owned(),
        detail: "Meet asks to record your screen".to_owned(),
        body: Body::Choices(vec!["Whole screen".to_owned(), "One window".to_owned()]),
        confirm_label: "Allow".to_owned(),
        cancel_label: "Cancel".to_owned(),
        ..Dialog::default()
    }));
    session.step(Duration::from_millis(1)).unwrap();
    let select = PortalEffect::SELECT.id;
    let sheet = publisher(&session, select).expect("the question publishes its answers");
    session
        .invoke(
            &sheet,
            select,
            &arguments(serde_json::json!({ "handle": "request-1", "index": 1 })),
        )
        .expect("select");
    session
        .invoke(
            &sheet,
            PortalEffect::CONFIRM.id,
            &arguments(serde_json::json!({ "handle": "request-1" })),
        )
        .expect("confirm");
    let asked = seat.take_asked();
    assert!(
        asked.contains(&PortalEffect::Select {
            handle: "request-1".to_owned(),
            index: 1
        }),
        "the service reads what was chosen: {asked:?}"
    );
}

#[test]
fn the_file_chooser_answers_the_file_chosen_through_its_actions() {
    use tilo_sheets::Seat as _;
    let _settled = ui::motion::testing::settled();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("beach.png"), b"bytes").unwrap();
    let mut session = session();
    let seat = session.file_picker().expect("file chooser").clone();
    seat.presentation().open(true);
    seat.ask(Some(FileRequest {
        handle: "request-1".to_owned(),
        title: "Choose a picture".to_owned(),
        mode: FileMode::Open,
        initial_dir: home.path().to_path_buf(),
        roots: vec![home.path().to_path_buf()],
        ..FileRequest::default()
    }));
    // The chooser reads the folder off the loop: choose once it has, so the step lands on the
    // listing the person sees.
    pump(&mut session, &mut [], |_, _| {
        let listing = seat.listing();
        !listing.reading
            && listing
                .entries
                .iter()
                .any(|entry| entry.name == "beach.png")
    });
    let beach = home.path().join("beach.png");
    let select = FilePickerEffect::SELECT.id;
    let sheet = publisher(&session, select).expect("the chooser publishes choosing");
    session
        .invoke(
            &sheet,
            select,
            &arguments(serde_json::json!({
                "handle": "request-1",
                "path": beach.to_string_lossy(),
            })),
        )
        .expect("select");
    // Choosing takes the choice at once; confirming checks it off the loop.
    pump(&mut session, &mut [], |_, _| seat.listing().can_confirm);
    session
        .invoke(
            &sheet,
            FilePickerEffect::CONFIRM.id,
            &arguments(serde_json::json!({ "handle": "request-1" })),
        )
        .expect("confirm");
    let mut answer = None;
    pump(&mut session, &mut [], |_, _| {
        answer = answer.take().or_else(|| seat.take_answer());
        answer.is_some()
    });
    assert_eq!(answer, Some(FileAnswer::Chosen(vec![beach])));
}

/// A pixel of a picture, as R, G, B, A.
fn pixel_at(picture: &image::RgbaImage, x: i32, y: i32) -> [u8; 4] {
    picture
        .get_pixel(u32::try_from(x).unwrap(), u32::try_from(y).unwrap())
        .0
}

#[path = "session/banners.rs"]
mod banners;
#[path = "session/bars.rs"]
mod bars;
#[path = "session/chrome.rs"]
mod chrome;
#[path = "session/gestures.rs"]
mod gestures;
#[path = "session/keyboard.rs"]
mod keyboard;
#[path = "session/lock.rs"]
mod lock;
#[path = "session/lock_recovery.rs"]
mod lock_recovery;
#[path = "session/strip.rs"]
mod strip;
#[path = "session/window_ids.rs"]
mod window_ids;
#[path = "session/workspace_settings.rs"]
mod workspace_settings;

#[path = "session/capture_studio.rs"]
mod capture_studio;
#[path = "session/portal_capture.rs"]
mod portal_capture;

#[path = "session/secret_captures.rs"]
mod secret_captures;

#[path = "session/first_ask.rs"]
mod first_ask;
#[path = "session/idle_frames.rs"]
mod idle_frames;
#[path = "session/send_key.rs"]
mod send_key;
#[path = "session/window_warmth.rs"]
mod window_warmth;

impl Dispatch<WlRegistry, ()> for Seen {
    fn event(
        seen: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            seen.globals.push(interface.clone());
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => seen.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "zwp_text_input_manager_v3" => {
                    seen.text_input_manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Seen {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Seen {
    fn event(
        seen: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            seen.configured = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Close => seen.closed = true,
            xdg_toplevel::Event::Configure { width, height, .. } => {
                seen.size = Some((width, height));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for Seen {
    fn event(
        seen: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Pointer) && seen.pointer.is_none() {
                seen.pointer = Some(seat.get_pointer(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) && seen.keyboard.is_none() {
                seen.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { .. } => seen.pointer_inside = true,
            wl_pointer::Event::Leave { .. } => seen.pointer_inside = false,
            wl_pointer::Event::Button { button, state, .. } => {
                seen.buttons.push((
                    button,
                    state == WEnum::Value(wl_pointer::ButtonState::Pressed),
                ));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => seen.keyboard_focused = true,
            wl_keyboard::Event::Leave { .. } => seen.keyboard_focused = false,
            wl_keyboard::Event::Key { key, state, .. } => {
                seen.keys
                    .push((key, state == WEnum::Value(wl_keyboard::KeyState::Pressed)));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        seen.frames_done += 1;
    }
}

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
delegate_noop!(Seen: ignore ZwpTextInputManagerV3);
delegate_noop!(Seen: ignore ZwpTextInputV3);

/// The display key's action steps the displays from side by side to showing the same, to only the
/// built-in one, to only the other.
#[test]
fn the_display_action_steps_through_the_ways_of_using_two_displays() {
    let _settled = ui::motion::testing::settled();
    let external = OutputConfig::new("HDMI-A-1", WIDTH, HEIGHT).at(WIDTH, 0);
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("eDP-1", WIDTH, HEIGHT).at(0, 0), external],
        Options {
            collab: false,
            ..Options::default()
        },
    );
    let arrangement = |session: &mut Session| {
        session
            .compositor()
            .outputs()
            .into_iter()
            .map(|output| (output.name, output.enabled, output.rect.map(|r| (r.0, r.1))))
            .collect::<std::collections::BTreeSet<_>>()
    };
    let on = |name: &str, x: i32| (name.to_owned(), true, Some((x, 0)));
    let off = |name: &str| (name.to_owned(), false, None);
    assert_eq!(
        arrangement(&mut session),
        [on("eDP-1", 0), on("HDMI-A-1", WIDTH)].into()
    );
    invoke(&mut session, tilo_session::displays::RESOURCE, "next-mode");
    assert_eq!(
        arrangement(&mut session),
        [on("eDP-1", 0), on("HDMI-A-1", 0)].into(),
        "mirrored"
    );
    invoke(&mut session, tilo_session::displays::RESOURCE, "next-mode");
    assert_eq!(
        arrangement(&mut session),
        [on("eDP-1", 0), off("HDMI-A-1")].into(),
        "the built-in display only"
    );
    invoke(&mut session, tilo_session::displays::RESOURCE, "next-mode");
    let external_only = arrangement(&mut session);
    assert!(external_only.contains(&off("eDP-1")), "{external_only:?}");
    invoke(&mut session, tilo_session::displays::RESOURCE, "next-mode");
    let extended = arrangement(&mut session);
    assert!(
        extended.iter().all(|(_, enabled, _)| *enabled),
        "every display on again: {extended:?}"
    );
    let corners: std::collections::BTreeSet<_> = extended.iter().map(|output| output.2).collect();
    assert_eq!(corners.len(), 2, "side by side again: {extended:?}");
}

// =====================================================================
// Side-by-side capture harness against the old shell's `shots`
// =====================================================================
//
// Every frame here has an old-shell counterpart rendered by
// `shots --mode offscreen` at the same 1280x800; the two are paired and
// diffed outside the tree. The set is the states a person meets first: the
// empty desk, a window, two windows, the overview over them, quick settings,
// search, a notification banner, a dialog over a window and the lock screen,
// in the dark theme the captures are taken in (and the empty desk in light).
//
// Frames are written to `$PIXEL_PARITY_OUT` (default
// `~/.cache/pixel-parity/new`) and named after the old shell's scenes.

/// Step `frames` frames of a millisecond each, so what a state's arrival animates has arrived
/// (motion reads settled, the panels still lay out).
fn settle(session: &mut Session, frames: u32) {
    for _ in 0..frames {
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// How long the session's loop waits with nothing of its own to do, as [`Session::run`] asks:
/// whatever is due first, or for the person.
fn wait_for(session: &mut Session) -> Option<Duration> {
    let now = Instant::now();
    match (session.compositor().next_wait(), session.next_deadline()) {
        (Some(frame), Some(due)) => Some(frame.min(due.saturating_duration_since(now))),
        (frame, due) => frame.or_else(|| due.map(|due| due.saturating_duration_since(now))),
    }
}

#[path = "session/activities.rs"]
mod activities;

#[path = "session/desk.rs"]
mod desk;
#[path = "session/hung.rs"]
mod hung;

#[path = "session/now_playing.rs"]
mod now_playing;

#[path = "session/readout.rs"]
mod readout;

/// The id of the tab the desk holds for `app` .
fn tab_of(session: &Session, app: &str) -> u64 {
    let id = window_resource(session, app)
        .expect("the desk publishes the window's tab")
        .id;
    id.as_str()
        .strip_prefix("desktop:tab:")
        .and_then(|id| id.parse().ok())
        .expect("a tab's address names its id")
}

/// Make an empty strip called `name` after those the workspace in front has.
fn add_strip(session: &Session, name: &str) {
    let handle = session.handle();
    let mut tree = handle.read(Clone::clone);
    tilo_desktop_model::commands::apply_to_active_workspace(
        &mut tree,
        &tilo_desktop_model::commands::DesktopCommand::CreateStrip { name: name.into() },
        &mut || Ok(handle.mint_id()),
    )
    .expect("a strip is made");
    handle.replace_tree(tree);
}

/// The id of the strip named `name` in the workspace in front.
fn strip_id(session: &Session, name: &str) -> String {
    session.handle().read(|tree| {
        tree.active_workspace()
            .and_then(|workspace| workspace.strips.iter().find(|strip| strip.name == name))
            .unwrap_or_else(|| panic!("no strip named {name}"))
            .id
            .0
            .to_string()
    })
}

/// The names of the workspaces, in order, and the strips of each.
fn workspaces(session: &Session) -> Vec<(String, Vec<String>)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.name.clone(),
                    workspace
                        .strips
                        .iter()
                        .map(|strip| strip.name.clone())
                        .collect(),
                )
            })
            .collect()
    })
}

/// The ids of the windows the workspace `workspace` holds.
fn tabs_in(session: &Session, workspace: u64) -> Vec<u64> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .filter(|held| held.slot.0 == workspace)
            .flat_map(|held| &held.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect()
    })
}

/// The names of the strips the window `tab` stands on.
fn strips_holding(session: &Session, tab: u64) -> Vec<String> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .filter(|strip| {
                strip
                    .window_groups
                    .iter()
                    .any(|group| group.tabs.iter().any(|held| held.id == tab))
            })
            .map(|strip| format!("{}/{}", strip.name, strip.id.0))
            .collect()
    })
}

/// Stepping past the first or the last workspace or strip says which end the person reached, rather
/// than doing nothing.
#[test]
fn stepping_past_an_end_says_which_end_it_is() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "switch-workspace",
        &serde_json::json!({"delta": 1}),
    );
    let said = session.accessible_labels().join("\n");
    assert!(
        said.contains("This is the last workspace"),
        "stepping past the last workspace says so: {said}"
    );
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "switch-workspace",
        &serde_json::json!({"delta": -1}),
    );
    let said = session.accessible_labels().join("\n");
    assert!(
        said.contains("This is the first workspace"),
        "and past the first: {said}"
    );
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "step-strip",
        &serde_json::json!({"steps": 1}),
    );
    let said = session.accessible_labels().join("\n");
    assert!(
        said.contains("This is the last strip"),
        "stepping past the last strip says so: {said}"
    );
}

/// Leaving a workspace the person is not in takes it away and leaves them where they are.
#[test]
fn leaving_another_workspace_keeps_the_person_where_they_are() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let home = active_workspace(&session).expect("the first workspace is open");
    invoke(&mut session, CHROME_RESOURCE, "new-workspace");
    let other = active_workspace(&session).expect("the new workspace is open");
    assert_ne!(home, other, "there are two workspaces");
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "switch-workspace",
        &serde_json::json!({"delta": -1}),
    );
    assert_eq!(active_workspace(&session), Some(home), "back in the first");
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "leave-workspace",
        &serde_json::json!({"workspace": other.to_string()}),
    );
    assert_eq!(workspaces(&session).len(), 1, "the other workspace is gone");
    assert_eq!(
        active_workspace(&session),
        Some(home),
        "the person did not move"
    );
}

/// The next session comes up on the desk this one left: the workspaces,
/// their strips and which one was in front, without the windows.
#[test]
fn the_next_session_comes_up_where_this_one_was() {
    let _settled = ui::motion::testing::settled();
    let state = tempfile::tempdir().expect("a state directory");
    let home = {
        let mut session = session_keeping(state.path());
        session.step(Duration::from_millis(1)).unwrap();
        add_strip(&session, "Second");
        invoke(&mut session, CHROME_RESOURCE, "new-workspace");
        let kept = workspaces(&session);
        assert_eq!(kept.len(), 2, "two workspaces are set up: {kept:?}");
        assert_eq!(
            kept[0].1,
            vec!["Main".to_string(), "Second".to_string()],
            "the first workspace has the strip that was made"
        );
        let open = active_workspace(&session).expect("a workspace is open");
        let index = session.handle().read(|tree| {
            tree.workspaces
                .iter()
                .position(|workspace| workspace.slot.0 == open)
        });
        assert_eq!(index, Some(1), "the second workspace is in front");
        kept
    };

    let mut second = session_keeping(state.path());
    second.step(Duration::from_millis(1)).unwrap();
    assert_eq!(
        workspaces(&second),
        home,
        "the workspaces and their strips came back"
    );
    let open = active_workspace(&second).expect("a workspace is open");
    let index = second.handle().read(|tree| {
        tree.workspaces
            .iter()
            .position(|workspace| workspace.slot.0 == open)
    });
    assert_eq!(index, Some(1), "the workspace that was in front is");
}

/// What the overview offers as a window's own doings — sending it elsewhere, closing it — changes
/// the desk, reachable by address as an agent or the palette does.
#[test]
fn the_desktop_sends_a_window_where_the_overview_offers_to() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let _client = mapped(&mut session, "org.example.Editor");
    let tab = tab_of(&session, "org.example.Editor");
    add_strip(&session, "Second");
    let second = strip_id(&session, "Second");
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "move-tab",
        &serde_json::json!({"tab": tab.to_string(), "to": {"strip": second}}),
    );
    let holding = strips_holding(&session, tab);
    assert_eq!(
        holding.len(),
        1,
        "the window stands on one strip: {holding:?}"
    );
    assert!(
        holding[0].starts_with("Second/"),
        "the window went to the strip the person chose: {holding:?}"
    );

    invoke(&mut session, CHROME_RESOURCE, "new-workspace");
    let other = active_workspace(&session).expect("the new workspace is open");
    let home = {
        let (name, _) = workspaces(&session).remove(0);
        name
    };
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "switch-workspace",
        &serde_json::json!({"delta": -1}),
    );
    let stayed = active_workspace(&session).expect("a workspace is open");
    assert_ne!(stayed, other, "the person is back in the first workspace");
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "move-tab",
        &serde_json::json!({"tab": tab.to_string(), "to": {"workspace": other.to_string()}}),
    );
    let landed = tabs_in(&session, other);
    assert_eq!(landed, [tab], "the same window is in the other workspace");
    assert_eq!(
        active_workspace(&session),
        Some(stayed),
        "the person stays where they were"
    );
    assert!(
        strips_holding(&session, tab)[0].starts_with("Main/"),
        "the window went to a strip of the workspace it moved to"
    );
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "leave-workspace",
        &serde_json::json!({"workspace": other.to_string()}),
    );
    assert_eq!(workspaces(&session).len(), 1, "the workspace was left here");
    assert_eq!(
        workspaces(&session).remove(0).0,
        home,
        "the workspace the person is in is the one left"
    );
    assert_eq!(
        active_workspace(&session),
        Some(stayed),
        "and the person did not move"
    );
    let left = session.accessible_labels().join("\n");
    assert!(left.contains("You left"), "the person is told: {left}");
}

/// Naming and moving strips, as the overview's strip rows ask.
#[test]
fn the_desktop_names_and_moves_the_strips_the_overview_offers() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    add_strip(&session, "Second");
    let second = strip_id(&session, "Second");
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "rename",
        &serde_json::json!({"strip": second, "name": "Later"}),
    );
    let (_, strips) = workspaces(&session).remove(0);
    assert_eq!(strips, ["Main", "Later"], "the strip was renamed");

    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "move-strip",
        &serde_json::json!({"strip": "Later", "before": "Main"}),
    );
    let (_, strips) = workspaces(&session).remove(0);
    assert_eq!(strips, ["Later", "Main"], "the strip moved above the first");

    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "move-strip",
        &serde_json::json!({"strip": "Later"}),
    );
    let (_, strips) = workspaces(&session).remove(0);
    assert_eq!(strips, ["Main", "Later"], "and to the bottom");
}

/// The picture the person chose is the desk's, kept beside the desk for this device.
#[test]
fn the_desk_starts_on_the_picture_the_person_chose() {
    let _settled = ui::motion::testing::settled();
    let state = tempfile::tempdir().expect("a state directory");
    settings_store::appearance::WALLPAPER
        .set(Some("dunes".to_owned()))
        .expect("the choice is written");
    let mut session = session_keeping(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    let kept = std::fs::read_to_string(state.path().join("tilo").join("wallpapers.json"))
        .expect("the choice is kept beside the desk");
    let kept: serde_json::Value = serde_json::from_str(&kept).expect("a record");
    assert_eq!(
        kept["default"], "dunes",
        "every workspace the person gave no picture of its own shows the one chosen: {kept}"
    );
    assert!(
        kept.get("workspaces").is_none(),
        "and none of them has one of its own yet: {kept}"
    );
    settings_store::appearance::WALLPAPER
        .set(None)
        .expect("the choice is cleared");
}

/// The workspace menu offers joining, leaving and settings, and choosing join lists the workspaces
/// this computer holds.
#[test]
fn the_workspace_menu_offers_joining_leaving_and_settings() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    invoke(&mut session, CHROME_RESOURCE, "workspace-menu");
    let menu = session.accessible_labels().join("\n");
    for row in [
        "New workspace",
        "Join a workspace",
        "Leave workspace",
        "Workspace settings",
    ] {
        assert!(menu.contains(row), "the menu offers {row}: {menu}");
    }
    let offered: Vec<String> = session
        .handle()
        .host_actions()
        .into_iter()
        .map(|action| action.id)
        .collect();
    for id in [
        "new-workspace",
        "join-workspace",
        "leave-workspace",
        "workspace-settings",
    ] {
        assert!(
            offered.iter().any(|held| held == id),
            "the session offers {id}: {offered:?}"
        );
    }
    invoke(&mut session, CHROME_RESOURCE, "workspace-menu");
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    let settings = session
        .workspace_settings()
        .expect("the session hosts the workspace settings");
    assert!(
        settings.presentation().is_open(),
        "settings opens the sheet"
    );
    settings.presentation().close();
    invoke(&mut session, CHROME_RESOURCE, "join-workspace");
    let seat = session
        .workspace_directory()
        .expect("the session hosts the workspace directory");
    assert!(seat.presentation().is_open(), "joining opens the directory");
    let held: Vec<String> = workspaces(&session)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(held.len(), 1, "one workspace is held: {held:?}");
    let listed: Vec<String> = seat
        .directory()
        .listings
        .iter()
        .map(|listing| listing.name.clone())
        .collect();
    assert!(
        listed.is_empty(),
        "the directory offers what this computer can join, not what it holds: {listed:?}"
    );
    seat.presentation().close();
    invoke(&mut session, CHROME_RESOURCE, "new-workspace");
    assert_eq!(
        workspaces(&session).len(),
        held.len() + 1,
        "a new workspace joins the desk"
    );
}

/// Whether the clipboard's reading lists a copy of `text` .
fn copy_in_history(session: &Session, text: &str) -> bool {
    session
        .read(CLIPBOARD_RESOURCE)
        .and_then(|history| history.as_array().cloned())
        .is_some_and(|copies| {
            copies
                .iter()
                .any(|copy| copy["text"].as_str() == Some(text))
        })
}

/// The key of the copy whose text is `text` , as the clipboard's reading lists it.
fn copy_key(session: &Session, text: &str) -> String {
    session
        .read(CLIPBOARD_RESOURCE)
        .expect("the session publishes the clipboard")
        .as_array()
        .expect("the history is a list")
        .iter()
        .find(|copy| copy["text"].as_str() == Some(text))
        .map(|copy| copy["key"].as_str().expect("a key").to_owned())
        .expect("the text is in the history")
}

/// A copy made here is in the device's history for the person's next login,
/// and pasting it puts the bytes back on the clipboard and tells the app in
/// front to paste with its own chord.
#[test]
fn a_copy_is_kept_across_a_restart_and_pasted_into_the_app_in_front() {
    let _settled = ui::motion::testing::settled();
    // A copy is kept whole: its second line pastes into the app in front.
    let text = b"meeting notes\nsecond line";
    let copy = std::str::from_utf8(text).expect("the text");
    let mut session = session();
    session
        .compositor()
        .set_selection(
            Selection::Clipboard,
            vec![(TEXT_MIME.to_owned(), text.to_vec())],
        )
        .expect("the app copies");
    pump(&mut session, &mut [], |session, _| {
        copy_in_history(session, copy)
    });
    let key = copy_key(&session, copy);
    drop(session);

    // A new session — the person's next login — still has the copy: the history is in this device's
    // settings, not in the session's memory.
    let mut session = crate::session();
    assert_eq!(copy_key(&session, copy), key);
    let mut client = mapped(&mut session, "org.example.Terminal");
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
    });

    invoke_with(
        &mut session,
        CLIPBOARD_RESOURCE,
        "paste",
        &serde_json::json!({ "copy": key }),
    );
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0]
            .seen
            .keys
            .iter()
            .any(|(keycode, pressed)| *keycode == 47 && *pressed)
    });
    let seen: Vec<(u32, bool)> = client.seen.keys.clone();
    assert!(
        seen.contains(&(29, true)) && seen.contains(&(42, true)),
        "a terminal pastes with Ctrl+Shift+V: {seen:?}"
    );
    assert!(
        seen.contains(&(47, true)) && seen.contains(&(47, false)),
        "the chord is released again: {seen:?}"
    );
    let mut reader = session
        .compositor()
        .receive_selection(Selection::Clipboard, TEXT_MIME)
        .expect("the copy is on the clipboard");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut bytes).expect("read the copy");
    assert_eq!(bytes, text);
}

#[test]
fn the_dictation_key_shows_the_session_s_own_card_and_escape_closes_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut client = mapped(&mut session, "org.example.Editor");
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
    });
    let before = session.accessible_labels();
    key(&mut session, keys::LEFT_META, true);
    key(&mut session, keys::LEFT_SHIFT, true);
    tap(&mut session, keys::D);
    key(&mut session, keys::LEFT_SHIFT, false);
    key(&mut session, keys::LEFT_META, false);
    pump(&mut session, &mut [&mut client], |session, _| {
        session
            .read("dictation:session")
            .is_some_and(|reading| reading["session"].is_number())
    });
    assert_eq!(
        session.read("dictation:session").expect("dictation reads")["status"],
        "preparing",
        "the key asks for a dictation"
    );
    let labels = session.accessible_labels().join("\n");
    assert!(
        labels.contains("Checking the focused editable field"),
        "the card is up on the session's own sheet: {labels}"
    );
    assert_ne!(session.accessible_labels(), before, "and it is new");

    // Escape is the card's to answer, and the person's typing is not: the field they were in keeps
    // seeing every other key.
    tap(&mut session, keys::A);
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.contains(&(keys::A, true))
    });
    tap(&mut session, keys::ESCAPE);
    pump(&mut session, &mut [&mut client], |session, _| {
        session
            .read("dictation:session")
            .is_some_and(|reading| reading["status"] == "idle")
    });
    let labels = session.accessible_labels().join("\n");
    assert!(
        !labels.contains("Checking the focused editable field"),
        "the card is gone: {labels}"
    );
    assert!(
        !client
            .seen
            .keys
            .iter()
            .any(|(code, _)| *code == keys::ESCAPE),
        "Escape was the card's, never the window's: {:?}",
        client.seen.keys
    );
}
