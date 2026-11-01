//! Controllers in the room drive the desktop: a d-pad's directions arrive at
//! the window in front as the keys that move focus, A activates and B backs
//! out, the Guide chord and the buttons the guide draws do what the desktop
//! already does, and the guide explains the controller in the person's hands
//! rather than the keyboard.
//!
//! The controller is a stand-in for a real one (`controllers::Source::Lent`):
//! the events below are what the evdev decoder hands the session, so what is
//! tested here is the session's side of it, from the press to what the person
//! sees.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use os_actions::{ForwardedGamepadEvent, GamepadButton};
use settings_store::hardware::ShortcutConfig;
use tilo_compositor::{Compositor, InputEvent, OutputConfig};
use tilo_quick_settings::QuickSettingsState;
use tilo_quick_settings::host::FixtureHost;
use tilo_session::controller_nav::NavDirection;
use tilo_session::controllers::{ControllerId, ControllerWake, GamepadSources, Source};
use tilo_session::{Options, PanelHost, QuickSettings, Session, SessionConfig};
use tilo_sheets::Seat as _;
use tilo_sheets::shortcut_guide::{Guide, Modality};
use ui::ControllerFamily;
use ui::input::{ControllerButton, KeyLegend, KeyboardBinding, KeyboardModifier};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_keyboard,
    wl_keyboard::WlKeyboard, wl_registry, wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat,
    wl_shm, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;
const KEY_LEFT_ALT: u32 = 56;
const KEY_F1: u32 = 59;
const KEY_RIGHT: u32 = 106;

/// The shortcut table is the person's settings, and the test that changes it
/// saves there: it takes its turn rather than overwrite another's table.
static SHORTCUTS: Mutex<()> = Mutex::new(());

fn take_turns() -> MutexGuard<'static, ()> {
    let guard = SHORTCUTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ShortcutConfig::default()
        .save()
        .expect("a fresh shortcut table");
    guard
}

/// A controller in the room, as the session reads it: the events a test
/// presses on it, and the family whose legends it carries.
#[derive(Clone, Default)]
struct Fake(Rc<RefCell<FakeState>>);

#[derive(Default)]
struct FakeState {
    family: Option<ControllerFamily>,
    events: VecDeque<ForwardedGamepadEvent>,
}

impl Fake {
    fn new(family: ControllerFamily) -> Self {
        let fake = Self::default();
        fake.0.borrow_mut().family = Some(family);
        fake
    }

    fn send(&self, event: ForwardedGamepadEvent) {
        self.0.borrow_mut().events.push_back(event);
    }

    fn hold(&self, button: GamepadButton) {
        self.send(ForwardedGamepadEvent::Button {
            button,
            pressed: true,
        });
    }

    fn let_go(&self, button: GamepadButton) {
        self.send(ForwardedGamepadEvent::Button {
            button,
            pressed: false,
        });
    }

    fn press(&self, button: GamepadButton) {
        self.hold(button);
        self.let_go(button);
    }

    /// Push the D-pad the one way, and let it spring back.
    fn dpad(&self, direction: NavDirection) {
        let (x, y) = match direction {
            NavDirection::Up => (0, -1),
            NavDirection::Down => (0, 1),
            NavDirection::Left => (-1, 0),
            NavDirection::Right => (1, 0),
        };
        self.send(ForwardedGamepadEvent::Hat { x, y });
        self.send(ForwardedGamepadEvent::Hat { x: 0, y: 0 });
    }
}

impl GamepadSources for Fake {
    fn changed(&mut self) -> bool {
        false
    }

    fn attached(&mut self) -> Vec<(ControllerId, ControllerFamily)> {
        self.0
            .borrow()
            .family
            .map(|family| vec![(ControllerId::at("fake"), family)])
            .unwrap_or_default()
    }

    fn read(
        &mut self,
        _: &ControllerId,
        budget: usize,
    ) -> Option<(Vec<ForwardedGamepadEvent>, bool)> {
        let mut state = self.0.borrow_mut();
        let take = budget.min(state.events.len());
        let events: Vec<ForwardedGamepadEvent> = state.events.drain(..take).collect();
        let saturated = !state.events.is_empty();
        Some((events, saturated))
    }

    fn take_wakes(&mut self) -> Vec<ControllerWake> {
        Vec::new()
    }
}

/// A session with `fake` in the room, its presses read as the person's, and
/// quick settings to open.
fn session_with(fake: &Fake) -> Session {
    let host = Rc::new(FixtureHost::new(QuickSettingsState::fixture()));
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            config: SessionConfig::default(),
            quick_settings: Some(QuickSettings::Lent(Rc::clone(&host) as Rc<dyn PanelHost>)),
            controllers: Some(Source::Lent(Box::new(fake.clone()))),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("step");
    session
}

/// Step the session until `done` holds.
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    for _ in 0..500 {
        session.step(Duration::from_millis(2)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("condition not reached");
}

/// Press or release `keycode` on the keyboard and let the session carry it
/// out.
fn key(session: &mut Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::Key { keycode, pressed })
        .expect("the key");
    session.step(Duration::from_millis(1)).expect("step");
}

fn tap(session: &mut Session, keycode: u32) {
    key(session, keycode, true);
    key(session, keycode, false);
}

/// Whether the window menu of the window in front is up.
fn window_menu_up(session: &Session) -> bool {
    !session.window_menu_rows().is_empty()
}

/// What the guide shows now.
fn guide(session: &Session) -> Guide {
    session
        .shortcut_guide()
        .expect("the guide sheet mounted")
        .guide()
}

fn guide_up(session: &Session) -> bool {
    session
        .shortcut_guide()
        .is_some_and(|seat| seat.presentation().is_open())
}

/// What a binding spells: `binding(&["Super"], "left")`.
fn binding(modifiers: &[KeyboardModifier], key: &str) -> KeyboardBinding {
    KeyboardBinding::new(modifiers.to_vec(), KeyLegend::from_name(key))
}

#[derive(Default)]
struct Seen {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    keyboard: Option<WlKeyboard>,
    configured: bool,
    keyboard_focused: bool,
    keys: Vec<(u32, bool)>,
    size: Option<(i32, i32)>,
}

/// One window of a test app, and the keys the session sent it.
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

    /// Attach a buffer of one colour (RGBA) and commit it with a frame
    /// callback, so the window maps.
    fn show(&self, width: i32, height: i32, rgba: [u8; 4]) {
        let qh = self.queue.handle();
        let stride = width * 4;
        let len = usize::try_from(stride * height).expect("a buffer size");
        let fd = rustix::fs::memfd_create("tilo-controller-test", rustix::fs::MemfdFlags::CLOEXEC)
            .expect("a shared memory file");
        let mut file = std::fs::File::from(fd);
        // ARGB8888 is B, G, R, A in memory.
        let pixel = [rgba[2], rgba[1], rgba[0], rgba[3]];
        file.write_all(&pixel.repeat(len / 4))
            .expect("fill the buffer");
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

    /// The keys it heard, pressed and released, in order.
    fn typed(&self) -> Vec<(u32, bool)> {
        self.seen.keys.clone()
    }

    fn focused(&self) -> bool {
        self.seen.keyboard_focused
    }
}

/// Let the session and the clients talk until `done` holds.
#[track_caller]
fn pump(
    session: &mut Session,
    clients: &mut [&mut TestClient],
    mut done: impl FnMut(&mut Session, &[&mut TestClient]) -> bool,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
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
            "condition not reached"
        );
    }
}

/// A window of `app_id`, mapped and with the keyboard.
#[track_caller]
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

/// The window of `app`, and where it is.
fn window(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))
        .map(|(info, rect)| (info.id, rect))
}

/// The app of the window with the keyboard.
fn focused_app(compositor: &Compositor) -> Option<String> {
    compositor.focused_window().and_then(|window| window.app_id)
}

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
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => seen.seat = Some(registry.bind(name, version.min(7), qh, ())),
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
        if let xdg_toplevel::Event::Configure { width, height, .. } = event
            && width > 0
            && height > 0
        {
            seen.size = Some((width, height));
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
            && capabilities.contains(wl_seat::Capability::Keyboard)
            && seen.keyboard.is_none()
        {
            seen.keyboard = Some(seat.get_keyboard(qh, ()));
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
            wl_keyboard::Event::Key { key, state, .. } => seen
                .keys
                .push((key, state == WEnum::Value(wl_keyboard::KeyState::Pressed))),
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);

#[test]
fn a_d_pad_moves_focus_a_activates_and_b_backs_out() {
    let _settled = ui::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);
    let mut client = mapped(&mut session, "org.example.App");
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].focused()
    });

    // A direction is the key an app moves focus by — the toolkit's Alt+arrow
    // — so the window in front moves its focus exactly as from the keyboard.
    fake.dpad(NavDirection::Right);
    pump(&mut session, &mut [&mut client], |_, clients| {
        !clients[0].seen.keys.is_empty()
    });
    assert_eq!(
        client.typed(),
        vec![
            (KEY_LEFT_ALT, true),
            (KEY_RIGHT, true),
            (KEY_RIGHT, false),
            (KEY_LEFT_ALT, false)
        ],
        "the D-pad's right reaches the window in front as Alt+Right"
    );

    // A, the southern face button of an Xbox pad, confirms: Enter.
    client.seen.keys.clear();
    fake.press(GamepadButton::South);
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() >= 2
    });
    assert_eq!(
        client.typed(),
        vec![(KEY_ENTER, true), (KEY_ENTER, false)],
        "A activates what has focus"
    );

    // B backs out: Escape.
    client.seen.keys.clear();
    fake.press(GamepadButton::East);
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() >= 2
    });
    assert_eq!(
        client.typed(),
        vec![(KEY_ESCAPE, true), (KEY_ESCAPE, false)],
        "B goes back"
    );
}

#[test]
fn a_nintendo_pad_confirms_with_the_button_its_legend_calls_a() {
    let _settled = ui::motion::testing::settled();
    // Nintendo prints A east, where an Xbox pad has B.
    let fake = Fake::new(ControllerFamily::Nintendo);
    let mut session = session_with(&fake);
    let mut client = mapped(&mut session, "org.example.App");
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].focused()
    });

    fake.press(GamepadButton::East);
    pump(&mut session, &mut [&mut client], |_, clients| {
        !clients[0].seen.keys.is_empty()
    });
    assert_eq!(
        client.typed().first(),
        Some(&(KEY_ENTER, true)),
        "the button printed A activates"
    );
}

#[test]
fn the_guide_chord_moves_between_windows() {
    let _settled = ui::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);
    let mut left = mapped(&mut session, "org.example.Left");
    let mut right = mapped(&mut session, "org.example.Right");
    pump(&mut session, &mut [&mut left, &mut right], |session, _| {
        focused_app(session.compositor()).as_deref() == Some("org.example.Right")
    });

    // Guide held, the D-pad walks the desk's windows rather than the app's
    // focus.
    fake.hold(GamepadButton::Guide);
    fake.dpad(NavDirection::Left);
    pump(&mut session, &mut [&mut left, &mut right], |session, _| {
        focused_app(session.compositor()).as_deref() == Some("org.example.Left")
    });
    fake.let_go(GamepadButton::Guide);
    pump(&mut session, &mut [&mut left, &mut right], |_, _| true);
    assert_eq!(
        focused_app(session.compositor()).as_deref(),
        Some("org.example.Left"),
        "Guide+Left focuses the window before"
    );
}

#[test]
fn a_bare_guide_tap_opens_quick_settings() {
    let _settled = ui::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);
    assert!(!session.quick_settings_open());

    fake.press(GamepadButton::Guide);
    until(&mut session, Session::quick_settings_open);
    assert!(
        session.quick_settings_open(),
        "a Guide tap on its own opens quick settings"
    );
}

#[test]
fn the_guide_explains_the_controller_that_opened_it() {
    let _settled = ui::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Nintendo);
    let mut session = session_with(&fake);

    // Start opens the guide, as it does from the controller alone.
    fake.press(GamepadButton::Start);
    until(&mut session, guide_up);
    let shown = guide(&session);
    assert_eq!(shown.modality, Modality::Controller);
    assert_eq!(shown.controller, Some(ControllerFamily::Nintendo));
    // Nintendo prints its confirm button east.
    assert_eq!(shown.faces.confirm, ControllerButton::East);
    assert_eq!(shown.faces.cancel, ControllerButton::South);

    // B backs out of it, and a key of the person's own explains the keyboard
    // again.
    fake.press(GamepadButton::South);
    until(&mut session, |session| !guide_up(session));
    tap(&mut session, KEY_F1);
    until(&mut session, guide_up);
    assert_eq!(guide(&session).modality, Modality::Keys);
    assert_eq!(guide(&session).controller, None);
}

#[test]
fn a_controller_press_changes_a_shortcut_the_guide_waits_for() {
    let _settled = ui::motion::testing::settled();
    let _turns = take_turns();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);

    // The guide's first keycap holds the keyboard when it opens, and every
    // row stands two: Tab walks to the keycap, Enter takes it.
    tap(&mut session, KEY_F1);
    until(&mut session, guide_up);
    let action = "previous-tab";
    let shown = guide(&session);
    let row = shown
        .pairs
        .iter()
        .position(|pair| pair.previous.action == action || pair.next.action == action)
        .expect("the guide has the previous-tab row");
    let keycap = row * 2 + usize::from(shown.pairs[row].next.action == action);
    for _ in 0..keycap {
        tap(&mut session, KEY_TAB);
    }
    tap(&mut session, KEY_ENTER);
    until(&mut session, |session| {
        guide(session).listening.as_deref() == Some(action)
    });

    // While it waits, a direction of the controller is the chord the person
    // presses, and it becomes the row's keys.
    fake.dpad(NavDirection::Right);
    until(&mut session, |session| guide(session).listening.is_none());
    assert_eq!(
        guide(&session)
            .pairs
            .iter()
            .find(|pair| pair.previous.action == action)
            .map(|pair| pair.previous.keys.clone()),
        Some(binding(&[], "right")),
        "the D-pad's right is the row's new chord"
    );
}

#[test]
fn the_menu_button_opens_the_window_in_fronts_menu() {
    let _settled = ui::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);
    let mut client = mapped(&mut session, "org.example.App");
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].focused()
    });
    assert!(!window_menu_up(&session), "nothing is up before the press");

    // X, west on an Xbox pad, is the menu of the window in front.
    fake.press(GamepadButton::West);
    until(&mut session, window_menu_up);
    let rows = session.window_menu_rows();
    for row in [
        "Name this window…",
        "Fill the strip",
        "Half the strip",
        "Every window the same",
        "Wider",
        "Narrower",
        "Close this window",
    ] {
        assert!(rows.iter().any(|shown| shown == row), "{row}: {rows:?}");
    }

    // The same button closes it again.
    fake.press(GamepadButton::West);
    until(&mut session, |session| !window_menu_up(session));
}
