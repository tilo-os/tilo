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
use serde_json::json;
use settings_store::hardware::ShortcutConfig;
use tilo_compositor::{Compositor, InputEvent};
use tilo_session::controller_nav::NavDirection;
use tilo_session::controllers::{ControllerId, ControllerWake, GamepadSources, Source};
use tilo_session::{Options, Session, SessionConfig};
use tilo_sheets::SheetState as _;
use tilo_sheets::shortcut_guide::{Guide, Modality};
use tilo_ui_base::ControllerFamily;
use tilo_ui_base::{
    input::ControllerButton, input::KeyLegend, input::KeyboardBinding, input::KeyboardModifier,
};

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

/// A session with `fake` in the room, its presses read as the person's, and quick settings to open.
fn session_with(fake: &Fake) -> Session {
    let mut session = crate::common::headless(Options {
        collab: false,
        config: SessionConfig::default(),
        controllers: Some(Source::Lent(Box::new(fake.clone()))),
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("step");
    session
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
        .is_some_and(|state| state.presentation().is_open())
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);
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
    let _settled = tilo_ui_base::motion::testing::settled();
    // Nintendo prints A east, where an Xbox pad has B.
    let fake = Fake::new(ControllerFamily::Nintendo);
    let mut session = session_with(&fake);
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

    // The mirror's other half: the button Nintendo prints B is south, where an Xbox pad has A,
    // and it backs out. The press before it is let go first, so the keys read are this one's.
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() >= 2
    });
    assert_eq!(
        client.seen.keys.clone(),
        vec![(KEY_ENTER, true), (KEY_ENTER, false)],
        "the button printed A is let go as it was pressed, and types nothing else"
    );
    client.seen.keys.clear();
    fake.press(GamepadButton::South);
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keys.len() >= 2
    });
    assert_eq!(
        client.seen.keys.clone(),
        vec![(KEY_ESCAPE, true), (KEY_ESCAPE, false)],
        "the button printed B goes back"
    );
}

#[test]
fn the_guide_chord_moves_between_windows() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);
    let mut left = mapped(&mut session, "org.example.Left");
    let mut right = mapped(&mut session, "org.example.Right");
    pump(&mut session, &mut [&mut left, &mut right], |session, _| {
        focused_app(session.compositor()).as_deref() == Some("org.example.Right")
    });

    // Guide held, the D-pad walks the shell's windows rather than the app's focus.
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
    let _settled = tilo_ui_base::motion::testing::settled();
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
    let _settled = tilo_ui_base::motion::testing::settled();
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let _turns = take_turns();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);

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
    let _settled = tilo_ui_base::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);
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

/// The tilolang shell's window, as assistive technology names it.
const SHELL: &str = "Desktop shell";

/// The control the shell's focus ring stands on now, as assistive technology reads it: its name,
/// or `None` while the ring is not in the shell (the shell's window itself holds the focus).
fn shell_focus(session: &Session) -> Option<String> {
    let windows = session.a11y_windows();
    let (_, _, tree) = windows.iter().find(|(_, name, _)| name == SHELL)?;
    let (_, node) = tree.nodes.iter().find(|(id, _)| *id == tree.focus)?;
    (node.role() != ui::accesskit::Role::Window)
        .then(|| node.label().unwrap_or_default().to_owned())
}

/// Step the session until the shell has drawn the control `name` somewhere on it.
#[track_caller]
fn shell_shows(session: &mut Session, name: &str) {
    until(session, |session| {
        session.a11y_windows().iter().any(|(_, window, tree)| {
            window == SHELL
                && tree
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some(name))
        })
    });
}

/// Step the session until the shell's focus ring stands on the control `name`, walking it there
/// with the D-pad at most `presses` times: down through the page, and right along each row.
#[track_caller]
fn walk_to(session: &mut Session, fake: &Fake, name: &str, presses: usize) {
    for at in 0..presses {
        if shell_focus(session).as_deref() == Some(name) {
            return;
        }
        let direction = if at % 3 == 2 {
            NavDirection::Right
        } else {
            NavDirection::Down
        };
        fake.dpad(direction);
        for _ in 0..3 {
            session.step(Duration::from_millis(1)).expect("step");
        }
    }
    panic!(
        "the D-pad never reached {name:?}: the ring stands on {:?}",
        shell_focus(session)
    );
}

/// Whether the launcher's search field stands on the shell: what the person sees when the
/// launcher is up.
fn launcher_up(session: &Session) -> bool {
    session.a11y_windows().iter().any(|(_, window, tree)| {
        window == SHELL
            && tree
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some(crate::common::LAUNCHER_SEARCH))
    })
}

#[test]
fn a_controller_seats_the_shells_focus_ring_and_moves_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);
    shell_shows(&mut session, "Open an app");
    assert_eq!(
        shell_focus(&session),
        None,
        "nothing of the shell holds the focus before the D-pad moves"
    );

    // The first direction seats the ring on the shell's first control: an empty desktop is the
    // shell's, so the controller reaches its bar and dock.
    fake.dpad(NavDirection::Down);
    let first = crate::common::wait_for(&mut session, "the shell's focus ring", |session| {
        shell_focus(session).filter(|name| !name.is_empty())
    });
    assert!(
        session.compositor().surface_takes_keys(),
        "the shell takes the keys while its own controls hold the focus"
    );

    // A further direction moves the ring to another control of the shell's, not out of it.
    let mut moved = None;
    for _ in 0..6 {
        fake.dpad(NavDirection::Down);
        if let Some(next) =
            crate::common::steps_within(&mut session, Duration::from_millis(400), |session| {
                shell_focus(session).filter(|name| *name != first)
            })
        {
            moved = Some(next);
            break;
        }
    }
    let moved = moved.unwrap_or_else(|| panic!("the D-pad moved the ring off {first:?}"));
    assert!(!moved.is_empty(), "the ring stands on a named control");
}

#[test]
fn the_controller_focus_ring_is_the_shells_own_in_light_and_dark() {
    // The ring the controller moves is the toolkit's: it belongs to the shell like any control's,
    // and the person's theme draws it. A controller leaves it standing through a theme change.
    let _settled = tilo_ui_base::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = crate::common::headless(Options {
        collab: false,
        config: SessionConfig::default(),
        controllers: Some(Source::Lent(Box::new(fake.clone()))),
        services: vec![Box::new(settings_sections::service::SettingsService::new())],
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("step");
    crate::common::wait_for(&mut session, "the appearance section", |session| {
        session.read("settings/appearance")
    });
    shell_shows(&mut session, "Open an app");
    fake.dpad(NavDirection::Down);
    let first = crate::common::wait_for(&mut session, "the shell's focus ring", |session| {
        shell_focus(session).filter(|name| !name.is_empty())
    });

    for (theme, mode, name) in [
        ("tilo-dark", "dark", "controller-focus-dark"),
        ("paper-light", "light", "controller-focus-light"),
    ] {
        let key = if mode == "light" {
            "light_theme"
        } else {
            "dark_theme"
        };
        appearance(&mut session, key, json!(theme));
        appearance(&mut session, "mode", json!(mode));
        for _ in 0..8 {
            session.step(Duration::from_millis(1)).expect("step");
        }
        crate::common::shot(&mut session, name);
        assert_eq!(
            shell_focus(&session).as_deref(),
            Some(first.as_str()),
            "in the {mode} theme the ring stands where the controller left it"
        );
    }
}

/// Set a field of the person's appearance section through its own published setter.
fn appearance(session: &mut Session, field: &str, value: serde_json::Value) {
    let arguments = json!({ "value": value })
        .as_object()
        .expect("an object")
        .clone();
    session
        .invoke(
            "settings/appearance",
            &settings_store::workspace::verb(field),
            &arguments,
        )
        .expect("the appearance section takes it");
}

#[test]
fn the_launcher_opens_from_the_control_the_controller_reached_and_back_closes_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);
    shell_shows(&mut session, "Open an app");
    fake.dpad(NavDirection::Down);
    crate::common::wait_for(&mut session, "the shell's focus ring", |session| {
        shell_focus(session).filter(|name| !name.is_empty())
    });

    // The person walks the ring down to the shell's way into the launcher, and A opens it.
    walk_to(&mut session, &fake, "Open an app", 16);
    fake.press(GamepadButton::South);
    until(&mut session, launcher_up);

    // B backs out of it, as Escape does: the launcher is a surface of the shell's.
    fake.press(GamepadButton::East);
    until(&mut session, |session| !launcher_up(session));
}

#[test]
fn a_direction_the_shell_leaves_over_focuses_the_window_beside_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let fake = Fake::new(ControllerFamily::Xbox);
    let mut session = session_with(&fake);
    let mut left = mapped(&mut session, "org.example.Left");
    let mut right = mapped(&mut session, "org.example.Right");
    pump(&mut session, &mut [&mut left, &mut right], |session, _| {
        focused_app(session.compositor()).as_deref() == Some("org.example.Right")
    });
    let window = crate::common::window::window_of(session.compositor(), "org.example.Right")
        .expect("the window in front")
        .0;
    let before = session
        .desktop()
        .with_policy(|policy| policy.tab_of(window))
        .expect("the window in front is the layout's");

    // The notification centre stands over the windows with the shell's own controls focused: the
    // shell holds the keys, so the controller's directions are its views' until they run out.
    let arguments = json!({ "open": true });
    session
        .invoke(
            tilo_session_core::desktop::verbs::CHROME_RESOURCE,
            "notifications",
            arguments.as_object().expect("an object"),
        )
        .expect("the centre opens");
    crate::common::wait_for(&mut session, "the centre to take the keys", |session| {
        session.compositor().surface_takes_keys().then_some(())
    });

    // A direction with nothing of the shell's left that way is the desktop's: the focus moves to
    // the window beside the shell's controls.
    let mut focused = before;
    for at in 0..24 {
        if at % 2 == 0 {
            fake.dpad(NavDirection::Right);
        } else {
            fake.dpad(NavDirection::Down);
        }
        if let Some(next) =
            crate::common::steps_within(&mut session, Duration::from_millis(400), |session| {
                session
                    .desktop()
                    .with_policy(|policy| policy.focused_tab())
                    .filter(|tab| *tab != before)
            })
        {
            focused = next;
            break;
        }
    }
    assert_ne!(focused, before, "the focus left the shell for a window");

    // Back closes the centre, and the window it reached answers the controller itself.
    fake.press(GamepadButton::East);
    crate::common::wait_for(&mut session, "the centre to close", |session| {
        (!session.compositor().surface_takes_keys()).then_some(())
    });
    for client in [&mut left, &mut right] {
        client.seen.keys.clear();
    }
    fake.dpad(NavDirection::Right);
    pump(&mut session, &mut [&mut left, &mut right], |_, clients| {
        clients.iter().any(|client| {
            client.seen.keys
                == vec![
                    (KEY_LEFT_ALT, true),
                    (KEY_RIGHT, true),
                    (KEY_RIGHT, false),
                    (KEY_LEFT_ALT, false),
                ]
        })
    });
}
