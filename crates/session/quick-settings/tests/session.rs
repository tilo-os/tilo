//! The session starts Quick Settings itself: a process of its own, handed a
//! socket the compositor trusts and the layer surface its window is to be.
//! What it draws is the panel's own frame, shown like any client's; what it
//! offers is actions, published to the session and run when the session asks.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "a failed step in a test is the assertion, and the session outlives every call"
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audio::sound::{Graph, Read, SoundService};
use connectivity::Connectivity;
use connectivity::device::{Change, Device, Readings};
use media::audio::{AudioDevice, AudioGraph, GraphControl};
use serde_json::{Map, Value, json};
use tilo_actions::{DoorError, ResourceSpec};
use tilo_compositor::{InputEvent, OutputConfig, Screenshot};
use tilo_service::Started;
use tilo_session::surfaces::{Anchor, Surface};
use tilo_session::{ActionPlane, Machine, Options, Service, Session};

const WIDTH: i32 = 800;
const HEIGHT: i32 = 600;
/// The pointer's primary button, as the compositor names it.
const BTN_LEFT: u32 = 0x110;
/// The desk's overlay stack, which the bar's tray press opens the panel in.
const OVERLAYS: &str = "desktop:overlays";
/// The battery, as the power service publishes it.
const BATTERY: &str = "power:battery";
/// The network, as the connectivity service publishes it.
const NETWORK: &str = "network";
/// The notification centre, as the session publishes it.
const NOTIFICATIONS: &str = "session:notifications";
/// How long the panel may take to show its window.
const OPENING: Duration = Duration::from_secs(90);
/// One display's refresh: how often the session asks its clients for their
/// next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// The panel, as the session starts it.
fn panel() -> Surface {
    tilo_session::quick_settings::surface_of(env!("CARGO_BIN_EXE_tilo-quick-settings"))
}

#[test]
fn the_session_starts_the_panel_as_a_trusted_client_on_its_layer() {
    let mut session = session_with(&[]);
    // The desk as it stands before the panel appears: what the panel leaves
    // alone has to stay exactly this, pixel for pixel.
    let desk = drawn_desk(&mut session);
    // Started by itself, with no slot that places it, it stands where its anchor holds it.
    let anchored = panel().anchor(Anchor::TOP | Anchor::RIGHT);
    session.start_surface(&anchored).expect("the panel starts");

    assert!(
        session.surfaces().running("quick-settings"),
        "the session started the panel: {:?}",
        session.surfaces().names()
    );

    // What it drew is its own frame, composited like any client's: the panel
    // along the output's right edge, and what lies left of it untouched.
    let frame = drawn(&mut session, &desk);
    save(&frame, "session-quick-settings");
    let inside = pixel(&frame, WIDTH - width() / 2, 200);
    assert_ne!(
        inside,
        pixel(&desk, WIDTH - width() / 2, 200),
        "the panel draws its own frame"
    );
    let beside = WIDTH - width() - 20;
    assert_eq!(
        pixel(&frame, beside, 200),
        pixel(&desk, beside, 200),
        "the panel takes exactly its width"
    );

    session.stop_surface("quick-settings");
    assert!(
        !session.surfaces().running("quick-settings"),
        "the panel is gone"
    );
}

#[test]
fn the_panel_opened_from_the_bar_shows_what_the_services_publish() {
    let mut laptop = session_of_device(&[panel()], Machine::Owned);
    let session = &mut laptop.session;
    open_the_panel(session);
    assert!(
        session.quick_settings_open(),
        "the panel is up in the stack"
    );

    // The panel reads the session's services through the plane it was
    // started with: what it offers is what those devices can do.
    assert_eq!(
        until_ready(session, "toggle-wifi"),
        "Wi-Fi",
        "the network's radio is the panel's Wi-Fi control"
    );
    assert_eq!(
        until_ready(session, "set-volume"),
        "Volume",
        "the sound card's sink is the panel's volume control"
    );

    // The battery the session publishes is the charge the panel shows: a
    // pack that moves redraws it.
    let at_42 = settled(session);
    save(&at_42, "session-quick-settings-battery-42");
    laptop.pack.charge_to(84);
    let at_84 = changed(session, &at_42);
    save(&at_84, "session-quick-settings-battery-84");

    session.stop_surface("quick-settings");
}

/// Escape is the panel's own while a page of it is open: the panel goes back
/// a page and answers that it did, and only its main page has nowhere left to
/// go, which is where the session closes it.
#[test]
fn the_panel_answers_whether_it_had_a_page_to_go_back_to() {
    let mut laptop = session_of_device(&[panel()], Machine::Owned);
    let session = &mut laptop.session;
    open_the_panel(session);
    until_ready(session, "toggle-wifi");

    // The controls are the page everything goes back to: nothing is behind
    // them, so Escape closes the panel there.
    let handled = went_back(ask_ended(session, "back", None));
    assert!(!handled, "the controls have nothing behind them");

    // A section's page is the controls' own: Escape returns to them, and the
    // panel stays up to be closed from there.
    until_ready(session, "open-section");
    let opened = ask_ended(session, "open-section", Some(json!({ "section": "wifi" })));
    assert!(
        matches!(opened, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the panel opened its networks: {opened:?}"
    );
    let handled = went_back(ask_ended(session, "back", None));
    assert!(handled, "the networks go back to the controls");
    assert!(
        session.quick_settings_open(),
        "the panel is still up after going back a page"
    );
    assert!(
        !went_back(ask_ended(session, "back", None)),
        "and the controls, reached again, have nothing behind them"
    );

    session.stop_surface("quick-settings");
}

/// Whether the panel answered that it went back a page, from what it said
/// when it ran `back`.
fn went_back(ended: ui::tilo_actions::InvocationUpdate) -> bool {
    let ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } = ended else {
        panic!("the panel did not answer going back: {ended:?}");
    };
    output
        .value
        .as_ref()
        .and_then(|value| value.get("handled"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("the panel says whether it went back: {:?}", output.value))
}

/// The panel on a laptop's screen, the size a person's session gives it: its
/// card stands over the desk rather than on a ground of its own, and it keeps
/// to the output instead of running past its edge.
#[test]
fn the_panel_on_a_laptops_output_stays_inside_it_over_the_desk() {
    const OUTPUT: (i32, i32) = (1280, 800);
    let mut laptop = session_of_device_at(&[panel()], Machine::Owned, OUTPUT);
    let session = &mut laptop.session;
    let desk = at_rest(session);
    open_the_panel(session);
    until_ready(session, "toggle-wifi");
    let frame = at_rest(session);
    save(&frame, "session-quick-settings-1280x800");

    // What the card paints over the desk is not the desk under it: the panel
    // is there.
    let drew = (0..OUTPUT.1).step_by(4).any(|y| {
        let x = OUTPUT.0 - width() / 2;
        pixel(&frame, x, y) != pixel(&desk, x, y)
    });
    assert!(drew, "the panel drew over the desk");

    // It keeps to the output it stands in: the bar above it and the row under
    // its foot are the desk untouched, so no page runs past the screen.
    for x in (OUTPUT.0 - width()..OUTPUT.0).step_by(8) {
        assert_eq!(
            pixel(&frame, x, 0),
            pixel(&desk, x, 0),
            "the panel keeps off the output's top edge at {x}"
        );
        assert_eq!(
            pixel(&frame, x, OUTPUT.1 - 1),
            pixel(&desk, x, OUTPUT.1 - 1),
            "the panel keeps off the output's bottom edge at {x}"
        );
    }
}

#[test]
fn the_panel_opens_each_radios_details_in_place() {
    let mut laptop = session_of_device(&[panel()], Machine::Owned);
    let session = &mut laptop.session;
    open_the_panel(session);
    until_ready(session, "toggle-wifi");
    let controls = at_rest(session);
    save(&controls, "session-quick-settings");

    // Wi-Fi's chevron opens its networks in place: the same window, the same
    // place, and what the network publishes under the tiles.
    until_ready(session, "open-section");
    let ended = ask_ended(session, "open-section", Some(json!({ "section": "wifi" })));
    assert!(
        matches!(ended, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the panel opened its networks: {ended:?}"
    );
    changed(session, &controls);
    let wifi = at_rest(session);
    assert!(
        differing(&controls, &wifi) > 500,
        "the networks stand under the tiles: {} pixels changed",
        differing(&controls, &wifi)
    );
    save(&wifi, "session-quick-settings-wifi");

    // Back gives the page it opened from, down to the frame.
    let ended = ask_ended(session, "close-section", None);
    assert!(
        matches!(ended, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the panel closed its networks: {ended:?}"
    );
    changed(session, &wifi);
    let back = at_rest(session);
    save(&back, "session-quick-settings-closed");
    assert!(
        differing(&back, &controls) * 5 < differing(&controls, &wifi),
        "closing gives back the page it opened from: {} of the {} pixels the \
         section changed",
        differing(&back, &controls),
        differing(&controls, &wifi)
    );

    // Bluetooth's opens the paired accessories the same way.
    until_ready(session, "open-section");
    let ended = ask_ended(
        session,
        "open-section",
        Some(json!({ "section": "bluetooth" })),
    );
    assert!(
        matches!(ended, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the panel opened its accessories: {ended:?}"
    );
    changed(session, &back);
    let devices = at_rest(session);
    assert!(
        differing(&controls, &devices) > 500,
        "the paired accessories stand under the tiles: {} pixels changed",
        differing(&controls, &devices)
    );
    save(&devices, "session-quick-settings-bluetooth");

    // And back again, to the page it opened from.
    let ended = ask_ended(session, "close-section", None);
    assert!(
        matches!(ended, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the panel closed its accessories: {ended:?}"
    );
    changed(session, &devices);
    let back = at_rest(session);
    save(&back, "session-quick-settings-come-back");
    assert!(
        differing(&back, &controls) * 5 < differing(&controls, &devices),
        "closing gives back the page it opened from: {} of the {} pixels the \
         section changed",
        differing(&back, &controls),
        differing(&controls, &devices)
    );
    session.stop_surface("quick-settings");
}

/// How many pixels of `after` differ from `before` by more than a shade.
fn differing(before: &Screenshot, after: &Screenshot) -> usize {
    before
        .rgba
        .chunks_exact(4)
        .zip(after.rgba.chunks_exact(4))
        .filter(|(a, b)| {
            a.iter()
                .zip(b.iter())
                .any(|(a, b)| u8::abs_diff(*a, *b) > 8)
        })
        .count()
}

#[test]
fn the_wifi_tile_is_the_published_networks_action() {
    let mut laptop = session_of_device(&[panel()], Machine::Owned);
    let session = &mut laptop.session;
    open_the_panel(session);
    until_ready(session, "toggle-wifi");
    assert_eq!(
        session.read(NETWORK).expect("the network is published")["wifi"]["on"],
        json!(false),
        "the radios start with Wi-Fi off"
    );

    // A press on the tile is the panel's effect, which is one action of the
    // session's network: the service carries it out and the reading moves.
    ask(session, "toggle-wifi", None);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        session.settle_services();
        pump(session);
        if session.read(NETWORK).expect("the network is published")["wifi"]["on"] == json!(true) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the network never turned Wi-Fi on: {:?}",
            session.read(NETWORK)
        );
    }
    session.stop_surface("quick-settings");
}

#[test]
fn the_quiet_tile_is_the_published_centres_switch() {
    let mut laptop = session_of_device(&[panel()], Machine::Owned);
    let session = &mut laptop.session;
    open_the_panel(session);
    until_ready(session, "set-do-not-disturb");
    assert!(
        session
            .read(NOTIFICATIONS)
            .expect("the centre is published")["quiet"]
            .is_null(),
        "nothing holds notifications back to start with"
    );

    // A press on the tile is the panel's effect, which is one action of the
    // session's notification centre: the centre holds them and says so.
    let ended = ask_ended(session, "set-do-not-disturb", Some(json!({ "on": true })));
    assert!(
        matches!(ended, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the tile's press is carried out: {ended:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        session.settle_services();
        pump(session);
        let centre = session
            .read(NOTIFICATIONS)
            .expect("the centre is published");
        if !centre["quiet"].is_null() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the centre never held notifications back: {centre:?}"
        );
    }
    // The same switch turned off lets them through again: the one list the
    // bell opens says so, as the panel does.
    let ended = ask_ended(session, "set-do-not-disturb", Some(json!({ "on": false })));
    assert!(
        matches!(ended, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the tile's press is carried out: {ended:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        session.settle_services();
        pump(session);
        let centre = session
            .read(NOTIFICATIONS)
            .expect("the centre is published");
        if centre["quiet"].is_null() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the centre never let notifications through: {centre:?}"
        );
    }
    session.stop_surface("quick-settings");
}

#[test]
fn the_lock_tile_locks_the_screens() {
    // A session with a lock screen of its own: a lock is only taken where it
    // can be lifted again.
    let lock_screen = tilo_test_env::sibling_binary("tilo-lock-screen", "tilo-lock-screen");
    let mut laptop = session_of_device_prepared(
        &[panel()],
        Machine::Owned,
        (WIDTH, HEIGHT),
        |options, _root| {
            options.user = "ada".to_owned();
            options.verifier = Arc::new(Sesame);
            options
                .surfaces
                .push(Surface::new("lock", &lock_screen).lock());
        },
    );
    let session = &mut laptop.session;
    open_the_panel(session);
    until_ready(session, "lock-screen");
    assert!(
        !session.compositor().locked(),
        "the screens are open to start with"
    );

    // The tile is one action of the session's own screen: the screens lock,
    // and the session says so on the resource the tile was pressed through.
    let ended = ask_ended(session, "lock-screen", None);
    assert!(
        matches!(ended, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the tile's press is carried out: {ended:?}"
    );
    // The resource says what the screens are a pass after they took it, so
    // the pass that locks them is the one that tells of it.
    let deadline = Instant::now() + OPENING;
    loop {
        session.settle_services();
        pump(session);
        if session.read("session:screen") == Some(json!({ "locked": true })) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the screens never locked: locked={} read={:?}",
            session.compositor().locked(),
            session.read("session:screen")
        );
    }
    assert!(
        session.compositor().locked(),
        "the screens' resource reads what the tile did"
    );
}

#[test]
fn the_night_light_tile_warms_the_screen() {
    let mut laptop = session_of_device(&[panel()], Machine::Owned);
    let session = &mut laptop.session;
    open_the_panel(session);
    until_ready(session, "set-night-light");
    assert_eq!(
        session.read("session:night-light"),
        Some(json!({ "on": false })),
        "the screen is its own colour to start with"
    );

    // A press on the tile is the night light's own action: the screen warms,
    // and the resource it was pressed through reads so.
    let ended = ask_ended(session, "set-night-light", Some(json!({ "on": true })));
    assert!(
        matches!(ended, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the tile's press is carried out: {ended:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        session.settle_services();
        pump(session);
        let light = session.read("session:night-light");
        if light == Some(json!({ "on": true })) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the screen never warmed: {light:?}"
        );
    }
    session.stop_surface("quick-settings");
}

/// The panel on the person's 2x laptop panel: it draws at the panel's own
/// density, twice the logical size its controls lay out at, inside the
/// output it stands in.
#[test]
fn the_panel_draws_at_scale_2() {
    // The laptop's own panel: 2944x1840 physical, 1472x920 logical at 2x.
    const PIXELS: (i32, i32) = (2944, 1840);
    const SCALE: f64 = 2.0;
    let mut laptop = session_of_device_at_scale(&[panel()], Machine::Owned, PIXELS, SCALE);
    let session = &mut laptop.session;
    let desk = at_rest(session);
    open_the_panel(session);
    until_ready(session, "toggle-wifi");
    let frame = drawn_on(session, &desk, PIXELS.0);
    save(&frame, "session-quick-settings-scale-2");

    // The panel covers twice the logical width it lays out at on the 2x
    // panel, along the output's trailing edge.
    let mut drawn = Vec::new();
    for x in (0..PIXELS.0).step_by(4) {
        if (0..PIXELS.1)
            .step_by(8)
            .any(|y| pixel(&frame, x, y) != pixel(&desk, x, y))
        {
            drawn.push(x);
        }
    }
    let (first, last) = (
        *drawn.first().expect("the panel drew"),
        *drawn.last().expect("drew"),
    );
    // The session places the panel 380 logical pixels wide (the popover's
    // width), which a 2x panel draws as 760 physical ones.
    let painted = last - first + 4;
    let expected = 380 * 2;
    assert!(
        (painted - expected).abs() <= 8,
        "the panel paints {painted} pixels, its 380 logical ones at 2x: \
         {first}..={last}"
    );
    // And it keeps to the output: the bar along its top edge is untouched.
    for x in (first..=last).step_by(16) {
        assert_eq!(
            pixel(&frame, x, 0),
            pixel(&desk, x, 0),
            "the panel keeps off the output's top edge at {x}"
        );
    }
    session.stop_surface("quick-settings");
}

#[test]
fn a_press_outside_the_panel_closes_it() {
    let mut laptop = session_of_device(&[panel()], Machine::Owned);
    let session = &mut laptop.session;
    open_the_panel(session);
    until_ready(session, "toggle-wifi");

    // A press beside the panel takes the person elsewhere, and the panel
    // goes with the slot it was placed in.
    press(session, 40.0, f64::from(HEIGHT) / 2.0);
    let deadline = Instant::now() + OPENING;
    while session.quick_settings_open()
        || session
            .compositor()
            .layer_shown(tilo_session::quick_settings::SURFACE_NAMESPACE)
            != Some(false)
    {
        assert!(
            Instant::now() < deadline,
            "the panel stayed up: {:?}",
            session.surfaces().names()
        );
        pump(session);
    }
}

#[test]
fn the_panel_opens_the_same_way_in_a_windowed_session() {
    let mut laptop = session_of_device(&[panel()], Machine::Window);
    let session = &mut laptop.session;
    open_the_panel(session);
    until_ready(session, "toggle-wifi");
    ask(session, "toggle-wifi", None);
    let deadline = Instant::now() + OPENING;
    loop {
        session.settle_services();
        pump(session);
        if session.read(NETWORK).expect("the network is published")["wifi"]["on"] == json!(true) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a session in a window reads the same services: {:?}",
            session.read(NETWORK)
        );
    }
    session.stop_surface("quick-settings");
}

/// The frame once the panel has drawn into it.
fn drawn(session: &mut Session, desk: &Screenshot) -> Screenshot {
    drawn_on(session, desk, WIDTH)
}

/// [`drawn`] on an output `output_width` physical pixels across: the panel's
/// place is measured from that edge, whatever the display's density.
fn drawn_on(session: &mut Session, desk: &Screenshot, output_width: i32) -> Screenshot {
    let deadline = Instant::now() + OPENING;
    let panel = panel_pixel(desk, output_width);
    loop {
        pump(session);
        let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
        if panel_pixel(&frame, output_width) != panel {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "the panel shows nothing here (headless session without a renderer?)"
        );
    }
}

/// A pixel inside the panel, standing against an output `output_width`
/// physical pixels across: half a panel width in from its trailing edge.
fn panel_pixel(frame: &Screenshot, output_width: i32) -> [u8; 4] {
    let physical = i32::try_from(frame.width).unwrap_or(output_width);
    let scale = if output_width == 0 {
        1
    } else {
        physical / output_width.max(1)
    };
    pixel(
        frame,
        physical - width() * scale.max(1) / 2,
        i32::try_from(frame.height).expect("a frame's height fits a pixel coordinate") / 4,
    )
}

/// The width the panel is placed at.
fn width() -> i32 {
    i32::try_from(tilo_quick_settings::resource::PANEL_WIDTH).unwrap()
}

/// A pixel of `frame`, as the colour it shows.
fn pixel(frame: &Screenshot, x: i32, y: i32) -> [u8; 4] {
    let x = u32::try_from(x).unwrap();
    let y = u32::try_from(y).unwrap();
    frame.pixel(x, y).expect("a pixel of the frame")
}

/// The panel open on the desk, its app running: what the bar's tray press
/// does, which is to open the stack's quick-settings slot.
fn open_the_panel(session: &mut Session) {
    // The desk as it stands before the panel, at rest: what changes after it
    // opens is the panel coming up, never the desk still settling.
    let desk = at_rest(session);
    session
        .invoke(
            OVERLAYS,
            "open",
            &arguments(&json!({ "slot": "quick-settings" })),
        )
        .expect("the panel opens");
    pump(session);
    let deadline = Instant::now() + OPENING;
    while !session.surfaces().running("quick-settings") {
        assert!(
            Instant::now() < deadline,
            "the panel did not start: {:?}",
            session.surfaces().names()
        );
        pump(session);
    }
    // The app runs before its first frame reaches the desk, and it is drawn
    // once the desk shows it: a frame read after this is the panel's.
    loop {
        pump(session);
        let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
        if frame.rgba != desk.rgba {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the panel never drew over the desk: {:?}",
            session.surfaces().names()
        );
    }
}

/// A press at `(x, y)` on the desk, wherever that lands.
fn press(session: &mut Session, x: f64, y: f64) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("the pointer moves");
    pump(session);
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed,
            })
            .expect("the button");
        pump(session);
    }
}

/// `value` as an action's arguments.
fn arguments(value: &Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    }
}

/// The action `action` the panel publishes, as the session reads its
/// catalogue: what it offers, with the label it offers it under.
fn offered(session: &Session, action: &str) -> Option<ui::collaboration::ExportedAction> {
    session
        .surfaces()
        .published("quick-settings")
        .into_iter()
        .find(|offered| offered.action == action)
}

/// Run the session until the panel offers `action` as one it can run now, and
/// say how it labels it.
///
/// A control is offered before the reading behind it arrives: the panel draws
/// nothing for Wi-Fi until the network says what it has, and says so. A person
/// presses a tile they can see, so what waits here is that reading.
fn until_ready(session: &mut Session, action: &str) -> String {
    let deadline = Instant::now() + OPENING;
    loop {
        if let Some(offered) = offered(session, action)
            && offered.contract.availability.is_available()
        {
            return offered.action_label;
        }
        assert!(
            Instant::now() < deadline,
            "the panel never offered {action} as one it can run (running: {}, names: {:?}): {:?}",
            session.surfaces().running("quick-settings"),
            session.surfaces().names(),
            session
                .surfaces()
                .published("quick-settings")
                .iter()
                .map(|offered| (
                    offered.action.clone(),
                    offered.contract.availability.clone()
                ))
                .collect::<Vec<_>>()
        );
        session.settle_services();
        pump(session);
    }
}

/// Ask the panel to run `action`, as a person's press or an agent does, and
/// wait for it to answer.
fn ask(session: &mut Session, action: &str, arguments: Option<Value>) {
    let _ended = ask_ended(session, action, arguments);
}

/// Ask the panel to run `action`, and say how it ended.
fn ask_ended(
    session: &mut Session,
    action: &str,
    arguments: Option<Value>,
) -> ui::tilo_actions::InvocationUpdate {
    let Some(offered) = offered(session, action) else {
        panic!("the panel does not offer {action}");
    };
    let (resource, action) = (offered.resource.clone(), offered.action);
    // The arguments a press carries, as the action's named ones: what the
    // panel's published contract checks them against.
    let argument = arguments.map_or(
        ui::tilo_actions::remote::InvokeArgument::None,
        |arguments| match arguments {
            Value::Object(object) => ui::tilo_actions::remote::InvokeArgument::Json(object),
            other => ui::tilo_actions::remote::InvokeArgument::Text(other.to_string()),
        },
    );
    let id = session
        .ask_surface("quick-settings", &resource, &action, argument)
        .expect("the panel is running");
    let deadline = Instant::now() + OPENING;
    loop {
        if let Some(state) = session.surfaces().outcome(id)
            && !matches!(
                state.update,
                ui::tilo_actions::InvocationUpdate::Accepted { .. }
            )
        {
            return state.update.clone();
        }
        assert!(Instant::now() < deadline, "the panel did not answer");
        pump(session);
    }
}

/// The password of the one login this device is: what its lock screen checks.
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

/// A directory for the plane's socket that only this user reaches: the broker
/// refuses to serve one anybody else could open.
fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the plane in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the plane's directory is this user's");
    root
}

/// A laptop's radios in memory: the connectivity service reads and changes
/// them, as it would the machine's own.
#[derive(Default)]
struct Radios {
    readings: Mutex<Readings>,
}

impl Device for Radios {
    fn read(&self) -> Readings {
        self.readings.lock().expect("the radios").clone()
    }

    fn change(&self, change: &Change) -> Result<(), String> {
        match change {
            Change::Wifi(on) => self.readings.lock().expect("the radios").network.wifi.on = *on,
            Change::Bluetooth(on) => {
                self.readings.lock().expect("the radios").bluetooth.on = *on;
            }
            other => return Err(format!("{other:?} is not part of this laptop")),
        }
        Ok(())
    }
}

/// A sound card in memory: one pair of speakers, in use.
struct Card {
    graph: Mutex<AudioGraph>,
}

impl Graph for Card {
    fn read(&self) -> Read {
        Read {
            graph: self.graph.lock().expect("the card").clone(),
            uses: Vec::new(),
        }
    }

    fn control(&self) -> &dyn GraphControl {
        self
    }
}

impl GraphControl for Card {
    fn set_default_sink(&self, node_id: u32) -> Result<(), String> {
        for sink in &mut self.graph.lock().expect("the card").sinks {
            sink.is_default = sink.id == node_id;
        }
        Ok(())
    }

    fn set_default_source(&self, _node_id: u32) -> Result<(), String> {
        Ok(())
    }

    fn route_stream(&self, _stream_id: u32, _target_serial: u32) -> Result<(), String> {
        Ok(())
    }

    fn unroute_stream(&self, _stream_id: u32) -> Result<(), String> {
        Ok(())
    }

    fn set_node_volume(&self, node_id: u32, perceptual: f32) -> Result<(), String> {
        for sink in &mut self.graph.lock().expect("the card").sinks {
            if sink.id == node_id {
                sink.volume = Some(perceptual);
            }
        }
        Ok(())
    }

    fn set_node_mute(&self, _node_id: u32, _muted: bool) -> Result<(), String> {
        Ok(())
    }

    fn set_device_profile(&self, _device_id: u32, _profile_index: u32) -> Result<(), String> {
        Ok(())
    }
}

/// A battery in the person's hands, as the power service publishes it: the
/// test drains it, and the session and the panel read the new charge.
#[derive(Clone, Default)]
struct Pack {
    charge: Arc<Mutex<Charge>>,
}

#[derive(Default)]
struct Charge {
    percent: u8,
    moved: bool,
}

impl Pack {
    /// Charge the pack to `percent`, as a machine would as it runs.
    fn charge_to(&self, percent: u8) {
        let mut charge = self.charge.lock().expect("the pack");
        charge.percent = percent;
        charge.moved = true;
    }
}

impl Service for Pack {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![ResourceSpec::new(BATTERY, BATTERY, "Battery")]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let charge = self.charge.lock().expect("the pack");
        (resource == BATTERY).then(|| {
            json!({
                "present": true,
                "percentage": charge.percent,
                "state": "discharging",
                "minutes_left": null,
            })
        })
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        Err(DoorError::Unknown(tilo_actions::ActionAddress::new(
            resource, action,
        )))
    }

    fn poll(&mut self) -> bool {
        let mut charge = self.charge.lock().expect("the pack");
        std::mem::replace(&mut charge.moved, false)
    }
}

/// A session hosting a laptop in memory — the radios, the sound card and the
/// battery the panel shows — with its services on the socket the panel reads
/// them through, so the app it starts is an app of this session.
struct Laptop {
    session: Session,
    pack: Pack,
    /// The plane's directory, held until the device is dropped: the socket
    /// the panel reaches the session's services on lives in it, so a test
    /// keeps the whole device and never moves the session out of it.
    _root: tempfile::TempDir,
}

/// A headless session on the device in memory, drawing on `machine`.
fn session_of_device(surfaces: &[Surface], machine: Machine) -> Laptop {
    session_of_device_at(surfaces, machine, (WIDTH, HEIGHT))
}

/// A session with a device's services, on an output of `output` pixels.
fn session_of_device_at(surfaces: &[Surface], machine: Machine, output: (i32, i32)) -> Laptop {
    session_of_device_prepared(surfaces, machine, output, |_options, _root| {})
}

/// A session with a device's services, on an output of `output` pixels, as
/// `prepare` hands it the options and the directory its sockets live in
/// before it starts.
fn session_of_device_prepared(
    surfaces: &[Surface],
    machine: Machine,
    output: (i32, i32),
    prepare: impl FnOnce(&mut Options, &std::path::Path),
) -> Laptop {
    session_of_device_prepared_with(surfaces, machine, output, 1.0, prepare)
}

fn session_of_device_prepared_with(
    surfaces: &[Surface],
    machine: Machine,
    output: (i32, i32),
    scale: f64,
    prepare: impl FnOnce(&mut Options, &std::path::Path),
) -> Laptop {
    let radios = Arc::new(Radios::default());
    {
        let mut readings = radios.readings.lock().expect("the radios");
        readings.network.wifi.available = true;
        readings.bluetooth.available = true;
    }
    let mut connectivity = Connectivity::new(radios, None).expect("a connectivity service");
    connectivity.settle();
    let card: Arc<dyn Graph> = Arc::new(Card {
        graph: Mutex::new(AudioGraph {
            sinks: vec![AudioDevice {
                id: 40,
                serial: 40,
                name: "node-40".into(),
                description: "Speakers".into(),
                is_default: true,
                volume: Some(0.4),
                muted: Some(false),
                channels: 2,
                codec: None,
            }],
            ..AudioGraph::default()
        }),
    });
    let mut sound = SoundService::new(card, None).expect("a sound service");
    sound.settle();
    let pack = Pack::default();
    pack.charge_to(42);
    let root = private_home();
    let socket = root.path().join("actions.sock");
    let services: Vec<Box<dyn Service>> = vec![
        Box::new(connectivity),
        Box::new(sound),
        Box::new(pack.clone()),
    ];
    let mut options = Options {
        presentation: ui::headless::fixture_runner_inputs,
        surfaces: surfaces.to_vec(),
        services,
        action_plane: ActionPlane::At(socket),
        machine,
        ..Options::default()
    };
    prepare(&mut options, root.path());
    let session = Session::headless(
        options,
        &[OutputConfig::new("HEADLESS-1", output.0, output.1).with_scale(scale)],
    )
    .expect("session");
    Laptop {
        session,
        pack,
        _root: root,
    }
}

/// A session with a device's services on an output of `output` physical
/// pixels at `scale`, as the person's 2x laptop panel is.
fn session_of_device_at_scale(
    surfaces: &[Surface],
    machine: Machine,
    output: (i32, i32),
    scale: f64,
) -> Laptop {
    session_of_device_prepared_with(surfaces, machine, output, scale, |_options, _root| {})
}

/// The frame once the desk and the panel show nothing new: the same picture
/// twice running, with the panel drawn in it.
fn settled(session: &mut Session) -> Screenshot {
    let deadline = Instant::now() + OPENING;
    let mut last = session.compositor().screenshot("HEADLESS-1").expect("shot");
    loop {
        pump(session);
        let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
        if frame.rgba == last.rgba {
            return frame;
        }
        last = frame;
        assert!(Instant::now() < deadline, "the panel never settled");
    }
}

/// The frame once it shows a change on `before`: the desk settled again with
/// something else on it.
fn changed(session: &mut Session, before: &Screenshot) -> Screenshot {
    let deadline = Instant::now() + OPENING;
    loop {
        let frame = settled(session);
        if frame.rgba != before.rgba {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "the panel shows nothing new: {:?}",
            session.surfaces().names()
        );
    }
}

/// The frame once the panel has stopped moving: a dozen frames the same, so
/// an in-place slide or a page's fade has come to rest.
fn at_rest(session: &mut Session) -> Screenshot {
    const STILL: usize = 12;
    let mut last = session.compositor().screenshot("HEADLESS-1").expect("shot");
    let mut still = 0;
    for _ in 0..600 {
        pump(session);
        let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
        still = if frame.rgba == last.rgba {
            still + 1
        } else {
            0
        };
        last = frame;
        if still >= STILL {
            return last;
        }
    }
    panic!("the panel never came to rest");
}

/// A headless session that starts `surfaces` with it.
fn session_with(surfaces: &[Surface]) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            surfaces: surfaces.to_vec(),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// The desk as it stands before the surface appears: pumped until it has
/// drawn, so what the surface leaves alone can be compared with it.
fn drawn_desk(session: &mut Session) -> Screenshot {
    let deadline = Instant::now() + OPENING;
    let mut last = session.compositor().screenshot("HEADLESS-1").expect("shot");
    loop {
        pump(session);
        let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
        // Settled: the same frame twice running, with something on it.
        if frame.rgba == last.rgba && frame.rgba.iter().any(|byte| *byte != 0) {
            return frame;
        }
        last = frame;
        assert!(Instant::now() < deadline, "the desk never drew");
    }
}

/// One turn of the session's loop.
fn pump(session: &mut Session) {
    session.step(REFRESH).expect("a turn of the session");
}

/// Write `frame` where a person can look at it, when the run asked for
/// shots: `TILO_SURFACE_SHOTS` names the directory.
fn save(frame: &Screenshot, name: &str) {
    let Some(pixels) = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
    else {
        panic!("the frame's pixels do not fill it");
    };
    let mut png = Vec::new();
    pixels
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("the shot");
    let Some(directory) = std::env::var_os("TILO_SURFACE_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&directory).expect("the directory for shots");
    std::fs::write(directory.join(format!("{name}.png")), png).expect("the shot");
}
