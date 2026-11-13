//! Controllers in the room drive the desktop: a d-pad's directions arrive at the window in front as
//! the keys that move focus, A activates and B backs out, the Guide chord and the buttons the guide
//! draws do what the desktop already does, and the guide explains the controller in the person's
//! hands rather than the keyboard.
//! The controller is a stand-in for a real one (`controllers::Source::Lent`): the events below are
//! what the evdev decoder hands the session, so what is tested here is the session's side of it,
//! from the press to what the person sees.

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
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::common::window::{Client, pump};
use os_actions::{ForwardedGamepadEvent, GamepadButton};
use settings_store::hardware::ShortcutConfig;
use tilo_compositor::{Compositor, InputEvent};
use tilo_session::controller_nav::NavDirection;
use tilo_session::controllers::{ControllerId, ControllerWake, GamepadSources, Source};
use tilo_session::surfaces::Surface as SurfaceSpec;
use tilo_session::{Options, Session, SessionConfig, quick_settings};
use tilo_sheets::Seat as _;
use tilo_sheets::shortcut_guide::{Guide, Modality};
use tilo_test_env::ScopedEnv;
use ui::ControllerFamily;
use ui::input::{ControllerButton, KeyLegend, KeyboardBinding, KeyboardModifier};

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;
const KEY_LEFT_ALT: u32 = 56;
const KEY_F1: u32 = 59;
const KEY_RIGHT: u32 = 106;

/// The shortcut table is the person's settings, and the test that changes it saves there: it takes
/// its turn rather than overwrite another's table.
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

/// A controller in the room, as the session reads it: the events a test presses on it, and the
/// family whose legends it carries.
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

/// The panel's app for this package's tests: the surface probe the session package ships, standing
/// in for a view that draws the panel and staying up while the session keeps it. The environment it
/// reads is held by the test for as long as its app may be started.
fn panel() -> (SurfaceSpec, ScopedEnv) {
    let mut environment = ScopedEnv::new();
    environment.set("SURFACE_PROBE_STAY", "1");
    (
        quick_settings::surface_of(tilo_test_env::sibling_binary(
            "tilo-session",
            "tilo-surface-probe",
        )),
        environment,
    )
}

/// A session with `fake` in the room, its presses read as the person's, and quick settings to open.
fn session_with(fake: &Fake) -> (Session, ScopedEnv) {
    let (panel, environment) = panel();
    let mut session = crate::common::headless(Options {
        collab: false,
        config: SessionConfig::default(),
        surfaces: vec![panel],
        controllers: Some(Source::Lent(Box::new(fake.clone()))),
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("step");
    (session, environment)
}

/// Step the session until `done` holds.
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    crate::common::wait_for(session, "what the test asked for", |session| {
        done(session).then_some(())
    });
}

/// Press or release `keycode` on the keyboard and let the session carry it out.
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

/// What a binding spells: `binding(&["Super"], "left")` .
fn binding(modifiers: &[KeyboardModifier], key: &str) -> KeyboardBinding {
    KeyboardBinding::new(modifiers.to_vec(), KeyLegend::from_name(key))
}

/// A window of `app_id` , mapped and with the keyboard.
#[track_caller]
fn mapped(session: &mut Session, app_id: &str) -> Client {
    let mut client = Client::connect(session.compositor());
    pump(session, &mut [&mut client], |_, clients| {
        let seen = &clients[0].seen;
        seen.wm_base.is_some() && seen.seat.is_some()
    });
    client.map_plain(session, app_id, (300, 200));
    client
}

/// The app of the window with the keyboard.
fn focused_app(compositor: &Compositor) -> Option<String> {
    compositor.focused_window().and_then(|window| window.app_id)
}

#[test]
fn a_d_pad_moves_focus_a_activates_and_b_backs_out() {
    let _settled = ui::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let (mut session, _panel) = session_with(&fake);
    let mut client = mapped(&mut session, "org.example.App");
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
    });

    // A direction is the key an app moves focus by — the toolkit's Alt+arrow — so the window in
    // front moves its focus exactly as from the keyboard.
    fake.dpad(NavDirection::Right);
    pump(&mut session, &mut [&mut client], |_, clients| {
        !clients[0].seen.keys.is_empty()
    });
    assert_eq!(
        client.seen.keys.clone(),
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
        client.seen.keys.clone(),
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
        client.seen.keys.clone(),
        vec![(KEY_ESCAPE, true), (KEY_ESCAPE, false)],
        "B goes back"
    );
}

#[test]
fn a_nintendo_pad_confirms_with_the_button_its_legend_calls_a() {
    let _settled = ui::motion::testing::settled();
    // Nintendo prints A east, where an Xbox pad has B.
    let fake = Fake::new(ControllerFamily::Nintendo);
    let (mut session, _panel) = session_with(&fake);
    let mut client = mapped(&mut session, "org.example.App");
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
    });

    fake.press(GamepadButton::East);
    pump(&mut session, &mut [&mut client], |_, clients| {
        !clients[0].seen.keys.is_empty()
    });
    assert_eq!(
        client.seen.keys.clone().first(),
        Some(&(KEY_ENTER, true)),
        "the button printed A activates"
    );
}

#[test]
fn the_guide_chord_moves_between_windows() {
    let _settled = ui::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let (mut session, _panel) = session_with(&fake);
    let mut left = mapped(&mut session, "org.example.Left");
    let mut right = mapped(&mut session, "org.example.Right");
    pump(&mut session, &mut [&mut left, &mut right], |session, _| {
        focused_app(session.compositor()).as_deref() == Some("org.example.Right")
    });

    // Guide held, the D-pad walks the desk's windows rather than the app's focus.
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
    let (mut session, _panel) = session_with(&fake);
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
    let (mut session, _panel) = session_with(&fake);

    // Start opens the guide, as it does from the controller alone.
    fake.press(GamepadButton::Start);
    until(&mut session, guide_up);
    let shown = guide(&session);
    assert_eq!(shown.modality, Modality::Controller);
    assert_eq!(shown.controller, Some(ControllerFamily::Nintendo));
    // Nintendo prints its confirm button east.
    assert_eq!(shown.faces.confirm, ControllerButton::East);
    assert_eq!(shown.faces.cancel, ControllerButton::South);

    // B backs out of it, and a key of the person's own explains the keyboard again.
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
    let (mut session, _panel) = session_with(&fake);

    // The guide's first keycap holds the keyboard when it opens, and every row stands two: Tab
    // walks to the keycap, Enter takes it.
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

    // While it waits, a direction of the controller is the chord the person presses, and it becomes
    // the row's keys.
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
    let (mut session, _panel) = session_with(&fake);
    let mut client = mapped(&mut session, "org.example.App");
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
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
