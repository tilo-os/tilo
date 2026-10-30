//! The native session on the headless backend with a real Wayland client:
//! its window becomes a tab of the desk's strip, shown where the strip lays
//! that tab out; the chrome above the windows draws the top bar; closing
//! the tab asks the client to close.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::struct_excessive_bools,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::cell::RefCell;
use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::time::Duration;

use tilo_actions::ResourceSpec;
use tilo_compositor::{Compositor, GestureEvent, InputEvent, KeyRepeat, OutputConfig};
use tilo_desktop::desktop_actions::{CHROME_RESOURCE, LAUNCHPAD_RESOURCE, WINDOW_KIND};
use tilo_onboarding::OnboardingEffect;
use tilo_quick_settings::QsEffect;
use tilo_quick_settings::QuickSettingsState;
use tilo_quick_settings::device::Device;
use tilo_quick_settings::device::testing::{Kept, Machine};
use tilo_quick_settings::host::FixtureHost;
use tilo_quick_settings::scenarios;
use tilo_session::{DeviceHost, Options, QuickSettings, Session};
use tilo_sheets::file_picker::{FileAnswer, FileMode, FilePickerEffect, FileRequest};
use tilo_sheets::portal::{Body, Dialog, PortalEffect};
use tilo_sheets::power::PowerEffect;
use tilo_sheets::workspace_settings::WorkspaceSettingsEffect;
use tilo_surface_host::Published;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_keyboard,
    wl_keyboard::WlKeyboard, wl_pointer, wl_pointer::WlPointer, wl_registry,
    wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
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

    /// Attach a buffer of one colour (RGBA) and commit it with a frame
    /// callback.
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
#[track_caller]
fn pump(
    session: &mut Session,
    clients: &mut [&mut TestClient],
    mut done: impl FnMut(&mut Session, &[&mut TestClient]) -> bool,
) {
    for _ in 0..500 {
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
    }
    panic!("condition not reached");
}

fn session() -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// Connect a client and open a toplevel of `app_id`, drawn at the size it
/// was configured to, and wait until it is mapped.
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

/// The window of `app` and its rectangle `(x, y, w, h)`.
fn window(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))
        .map(|(info, rect)| (info.id, rect))
}

/// The resource the desk publishes for the window of `app`, its tab.
fn window_resource(session: &Session, app: &str) -> Option<ResourceSpec> {
    fn find(resources: &[ResourceSpec], app: &str) -> Option<ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.kind == WINDOW_KIND && resource.label == app {
                Some(resource.clone())
            } else {
                find(&resource.children, app)
            }
        })
    }
    find(&session.resources(), app)
}

/// Invoke `action` of `resource` with no arguments, then let the session
/// run a frame.
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
        tab.actions.iter().any(|action| action.id == "close"),
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

    invoke(&mut session, CHROME_RESOURCE, "toggle-overview");
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
    invoke(&mut session, tab.id.as_str(), "close");
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

/// Step the session until `done` holds, for up to three seconds of its
/// clock.
#[track_caller]
fn within(
    session: &mut Session,
    client: &mut TestClient,
    mut done: impl FnMut(&mut Session) -> bool,
) {
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < until {
        client.exchange();
        session.step(Duration::from_millis(4)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("condition not reached in time");
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
    pub const LEFT_META: u32 = 125;
    pub const VOLUME_UP: u32 = 115;
    pub const MUTE: u32 = 113;
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

/// A session with quick settings over stand-in readings, whose effects the
/// returned host records.
fn session_with_quick_settings() -> (Session, Rc<FixtureHost>) {
    let host = Rc::new(FixtureHost::new(QuickSettingsState::fixture()));
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            quick_settings: Some(QuickSettings::Lent(
                Rc::clone(&host) as Rc<dyn tilo_session::PanelHost>
            )),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    (session, host)
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
    invoke(&mut session, CHROME_RESOURCE, "hide-overview");
    pump(&mut session, &mut [&mut client], |_, _| true);
    tap(&mut session, keys::A);
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.contains(&(keys::A, true))
    });
}

#[test]
fn hardware_keys_run_the_published_effects_and_a_held_volume_key_repeats() {
    let _settled = ui::motion::testing::settled();
    let (mut session, host) = session_with_quick_settings();
    assert!(
        publishes(&session, tilo_session::QUICK_SETTINGS, "volume-up"),
        "quick settings publishes its effects: {:#?}",
        session
            .resources()
            .iter()
            .map(|resource| resource.id.clone())
            .collect::<Vec<_>>()
    );
    tap(&mut session, keys::MUTE);
    tap(&mut session, keys::BRIGHTNESS_DOWN);
    tap(&mut session, keys::WLAN);
    assert_eq!(
        host.ran(),
        [
            QsEffect::ToggleMuted,
            QsEffect::BrightnessDown,
            QsEffect::ToggleAirplaneMode
        ]
    );

    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_millis(20),
        rate: 100,
    });
    key(&mut session, keys::VOLUME_UP, true);
    let held = std::time::Instant::now();
    while held.elapsed() < Duration::from_millis(200) {
        session.step(Duration::from_millis(5)).unwrap();
    }
    key(&mut session, keys::VOLUME_UP, false);
    let louder = host
        .ran()
        .iter()
        .filter(|effect| **effect == QsEffect::VolumeUp)
        .count();
    assert!(louder > 3, "holding volume up ramps it: {louder}");
    assert!(
        session
            .accessible_labels()
            .iter()
            .any(|label| label.starts_with("Volume ")),
        "the readout shows the volume: {:?}",
        session.accessible_labels()
    );
    for _ in 0..5 {
        session.step(Duration::from_millis(10)).unwrap();
    }
    let after = host
        .ran()
        .iter()
        .filter(|effect| **effect == QsEffect::VolumeUp)
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
    assert!(publishes(
        &session,
        tilo_session::MEDIA,
        "play-pause"
    ));
    tap(&mut session, keys::PLAY_PAUSE);
    assert_eq!(*asked.borrow(), ["play-pause"]);
}

#[test]
fn the_touchpad_key_and_the_sessions_own_action_switch_the_touchpad() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    assert!(session.input_config().touchpad_enabled);
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

#[test]
fn the_lock_action_locks_the_session_behind_the_lock_screen_which_a_password_unlocks() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            user: "ada".to_owned(),
            verifier: std::sync::Arc::new(Sesame),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    let _client = mapped(&mut session, "org.example.Terminal");
    let unlocked = session.compositor().screenshot("HEADLESS-1").unwrap();
    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    session.step(Duration::from_millis(1)).unwrap();
    assert!(session.compositor().locked());
    let unlock = tilo_sheets::lock::LockEffect::UNLOCK.id;
    assert!(
        publisher(&session, "toggle-overview").is_none(),
        "the locked session withholds the desktop's actions"
    );
    let screen = publisher(&session, unlock).expect("the lock screen publishes unlocking");
    let locked = session.compositor().screenshot("HEADLESS-1").unwrap();
    assert_ne!(
        locked.rgba, unlocked.rgba,
        "the lock screen hides the desktop"
    );

    let type_password = tilo_sheets::lock::LockEffect::TYPE_PASSWORD.id;
    session
        .invoke(
            &screen,
            type_password,
            &arguments(serde_json::json!({ "password": "wrong" })),
        )
        .expect("type");
    session
        .invoke(&screen, unlock, &serde_json::Map::new())
        .expect("unlock");
    pump(&mut session, &mut [], |session, _| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label.contains("did not unlock"))
    });
    assert!(
        session.compositor().locked(),
        "a wrong password keeps it locked"
    );

    session
        .invoke(
            &screen,
            type_password,
            &arguments(serde_json::json!({ "password": "sesame" })),
        )
        .expect("type");
    session
        .invoke(&screen, unlock, &serde_json::Map::new())
        .expect("unlock");
    pump(&mut session, &mut [], |session, _| {
        !session.compositor().locked()
    });
    assert!(
        publisher(&session, unlock).is_none(),
        "the lock screen is gone"
    );
    assert!(publisher(&session, "toggle-overview").is_some());
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

#[test]
fn quick_settings_opens_by_its_action_and_a_press_outside_closes_it() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _host) = session_with_quick_settings();
    invoke(&mut session, CHROME_RESOURCE, "toggle-quick-settings");
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
            publishes(&session, LAUNCHPAD_RESOURCE, card),
            "the launchpad publishes {card}"
        );
    }
    invoke(&mut session, LAUNCHPAD_RESOURCE, "apps");
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

fn active_workspace(session: &Session) -> Option<u64> {
    session
        .handle()
        .read(|tree| tree.active_workspace().map(|workspace| workspace.id.0))
}

fn gesture(session: &mut Session, events: &[GestureEvent]) {
    for event in events {
        session
            .compositor()
            .input(InputEvent::Gesture(*event))
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

#[test]
fn touchpad_gestures_the_desktop_takes_switch_workspaces_and_open_the_overview() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let first = active_workspace(&session);
    invoke(&mut session, CHROME_RESOURCE, "new-workspace");
    let second = active_workspace(&session);
    assert_ne!(first, second, "a new workspace opened");
    // Four fingers down go to the workspace before, under natural
    // scrolling (the default): the workspaces follow the fingers.
    gesture(
        &mut session,
        &[
            GestureEvent::SwipeBegin { fingers: 4 },
            GestureEvent::SwipeUpdate { dx: 0.0, dy: 60.0 },
            GestureEvent::SwipeUpdate { dx: 0.0, dy: 60.0 },
            GestureEvent::SwipeEnd { cancelled: false },
        ],
    );
    assert_eq!(active_workspace(&session), first, "back to the first");

    let before = session.accessible_labels();
    gesture(
        &mut session,
        &[
            GestureEvent::PinchBegin { fingers: 3 },
            GestureEvent::PinchUpdate {
                dx: 0.0,
                dy: 0.0,
                scale: 0.6,
                rotation: 0.0,
            },
            GestureEvent::PinchEnd { cancelled: false },
        ],
    );
    session.step(Duration::from_millis(1)).unwrap();
    assert_ne!(
        session.accessible_labels(),
        before,
        "a pinch in opened the overview"
    );
}

/// The resource publishing `action` among everything the session
/// publishes.
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

/// Every resource publishing `action`.
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

/// The resource publishing `action`, once the action is available.
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

#[test]
fn quick_settings_publishes_its_effects_and_they_reach_the_device() {
    let _settled = ui::motion::testing::settled();
    let host = std::rc::Rc::new(FixtureHost::new(scenarios::readings(Some("controls"))));
    let lent: std::rc::Rc<dyn tilo_session::PanelHost> = host.clone();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            quick_settings: Some(QuickSettings::Lent(lent)),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).unwrap();
    let closed = session.compositor().screenshot("HEADLESS-1").unwrap();

    // Its actions are there whether or not the panel is open: an agent sets
    // the volume without opening anything.
    let volume = QsEffect::SET_VOLUME.id;
    let panel = publisher(&session, volume).expect("quick settings publishes its volume");
    session
        .invoke(
            &panel,
            volume,
            &arguments(serde_json::json!({ "level": 0.25 })),
        )
        .expect("set the volume");
    assert_eq!(host.ran(), [QsEffect::SetVolume { level: 0.25 }]);

    invoke(&mut session, CHROME_RESOURCE, "toggle-quick-settings");
    assert!(session.quick_settings_open());
    session.step(Duration::from_millis(1)).unwrap();
    let open = session.compositor().screenshot("HEADLESS-1").unwrap();
    assert_ne!(open.rgba, closed.rgba, "the panel shows under the bar");
}

#[test]
fn quick_settings_reads_the_device_and_its_power_menu_opens_the_sheet() {
    use tilo_sheets::Seat as _;
    let _settled = ui::motion::testing::settled();
    let machine = Machine::laptop();
    let reads = machine.clone();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            quick_settings: Some(QuickSettings::Device(Box::new(move |wake| {
                DeviceHost::new(
                    Device {
                        reads: std::sync::Arc::new(reads.clone()),
                        dispatcher: std::sync::Arc::new(reads),
                        preferences: Box::new(Kept::default()),
                    },
                    wake,
                )
            }))),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");

    // The device's readings land on the session's loop: the panel then
    // offers turning Bluetooth off, which the machine has.
    let bluetooth = QsEffect::SET_BLUETOOTH.id;
    pump(&mut session, &mut [], |session, _| {
        offering(session, bluetooth).is_some()
    });
    let panel = offering(&session, bluetooth).unwrap();
    session
        .invoke(
            &panel,
            bluetooth,
            &arguments(serde_json::json!({ "on": false })),
        )
        .expect("turn Bluetooth off");
    pump(&mut session, &mut [], |_, _| {
        machine.calls().contains(&"bluetooth:false".to_owned())
    });

    // What only the desktop does comes back to the session: the power menu.
    let menu = QsEffect::POWER_MENU.id;
    session
        .invoke(&panel, menu, &serde_json::Map::new())
        .expect("power menu");
    pump(&mut session, &mut [], |session, _| {
        session
            .power_menu()
            .is_some_and(|seat| seat.presentation().is_open())
    });
}

#[test]
fn setup_runs_over_the_desktop_through_its_actions_and_gives_it_back_when_done() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            setup: Some(tilo_session::OnboardingView::new_tour()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).unwrap();
    assert!(session.setup_up());
    assert!(
        publisher(&session, "toggle-overview").is_none(),
        "setup withholds the desktop's actions"
    );
    assert!(
        session
            .invoke(CHROME_RESOURCE, "toggle-overview", &serde_json::Map::new())
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
        publisher(&session, "toggle-overview").is_some(),
        "the desktop's actions are back"
    );
    assert!(publisher(&session, skip).is_none());
}

#[test]
fn the_power_menu_is_a_sheet_whose_lock_locks_the_screen() {
    use tilo_sheets::Seat as _;
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let lock = PowerEffect::LOCK.id;
    // The session's own screen resource locks too; the sheet's is another.
    let sheets = |session: &Session| {
        publishers(session, lock)
            .into_iter()
            .filter(|resource| resource != tilo_session::SESSION_RESOURCE)
            .collect::<Vec<_>>()
    };
    assert!(
        sheets(&session).is_empty(),
        "a sheet that is down offers nothing"
    );
    session
        .power_menu()
        .expect("power menu")
        .presentation()
        .open(false);
    session.step(Duration::from_millis(1)).unwrap();
    let menu = sheets(&session)
        .pop()
        .expect("the open menu publishes its choices");
    session
        .invoke(&menu, lock, &serde_json::Map::new())
        .expect("lock");
    assert!(session.compositor().locked(), "the screen is locked");
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
    let sheet = publisher(&session, rename).expect("the open sheet publishes renaming");
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
            &arguments(serde_json::json!({ "index": 1 })),
        )
        .expect("select");
    session
        .invoke(&sheet, PortalEffect::CONFIRM.id, &serde_json::Map::new())
        .expect("confirm");
    let asked = seat.take_asked();
    assert!(
        asked.contains(&PortalEffect::Select { index: 1 }),
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
    pump(&mut session, &mut [], |_, _| {
        seat.listing()
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
            &arguments(serde_json::json!({ "path": beach.to_string_lossy() })),
        )
        .expect("select");
    pump(&mut session, &mut [], |_, _| seat.listing().can_confirm);
    session
        .invoke(
            &sheet,
            FilePickerEffect::CONFIRM.id,
            &serde_json::Map::new(),
        )
        .expect("confirm");
    let mut answer = None;
    pump(&mut session, &mut [], |_, _| {
        answer = answer.take().or_else(|| seat.take_answer());
        answer.is_some()
    });
    assert_eq!(answer, Some(FileAnswer::Chosen(vec![beach])));
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
            seen.globals.push(interface.clone());
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

/// The display key's action steps the displays from side by side to
/// showing the same, to only the built-in one, to only the other.
#[test]
fn the_display_action_steps_through_the_ways_of_using_two_displays() {
    let _settled = ui::motion::testing::settled();
    let mut external = OutputConfig::new("HDMI-A-1", WIDTH, HEIGHT);
    external.position = (WIDTH, 0);
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            ..Options::default()
        },
        &[OutputConfig::new("eDP-1", WIDTH, HEIGHT), external],
    )
    .expect("session");
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
    invoke(&mut session, tilo_session::DISPLAYS_RESOURCE, "next-mode");
    assert_eq!(
        arrangement(&mut session),
        [on("eDP-1", 0), on("HDMI-A-1", 0)].into(),
        "mirrored"
    );
    invoke(&mut session, tilo_session::DISPLAYS_RESOURCE, "next-mode");
    assert_eq!(
        arrangement(&mut session),
        [on("eDP-1", 0), off("HDMI-A-1")].into(),
        "the built-in display only"
    );
    invoke(&mut session, tilo_session::DISPLAYS_RESOURCE, "next-mode");
    let external_only = arrangement(&mut session);
    assert!(external_only.contains(&off("eDP-1")), "{external_only:?}");
    invoke(&mut session, tilo_session::DISPLAYS_RESOURCE, "next-mode");
    let extended = arrangement(&mut session);
    assert!(
        extended.iter().all(|(_, enabled, _)| *enabled),
        "every display on again: {extended:?}"
    );
    let corners: std::collections::BTreeSet<_> = extended.iter().map(|output| output.2).collect();
    assert_eq!(corners.len(), 2, "side by side again: {extended:?}");
}

/// A screenshot the portal asks for is the session's frames: the loop is
/// woken and answers with a PNG of every display, side by side.
#[test]
fn the_portals_screenshot_is_a_picture_of_every_display() {
    use tilo_portal::Capture as _;

    let _settled = ui::motion::testing::settled();
    let directory = tempfile::tempdir().unwrap();
    let wake = tilo_service::LoopWake::default();
    let (capture, captures) =
        tilo_session::capture::channel_to(directory.path().to_owned(), wake.handle());
    let mut external = OutputConfig::new("HDMI-A-1", WIDTH, HEIGHT);
    external.position = (WIDTH, 0);
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            wake,
            captures: Some(captures),
            ..Options::default()
        },
        &[OutputConfig::new("eDP-1", WIDTH, HEIGHT), external],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).unwrap();
    let asked = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(capture.screenshot())
    });
    let started = std::time::Instant::now();
    while !asked.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the screenshot was never answered"
        );
        session.step(Duration::from_secs(1)).unwrap();
    }
    let path = asked.join().unwrap().expect("a screenshot");
    assert!(path.starts_with(directory.path()));
    let picture = image::open(&path).unwrap().to_rgba8();
    assert_eq!(
        picture.dimensions(),
        (
            u32::try_from(WIDTH * 2).unwrap(),
            u32::try_from(HEIGHT).unwrap()
        )
    );
}
