//! The wireless display sheet: Super+P puts it up over the desktop with the
//! layouts the attached displays allow and the receivers the projector
//! service found; a receiver that asks for a PIN is chosen first, the PIN is
//! typed into its field, and Enter streams to it through the projector. The
//! sheet shows the stream while it runs, and the same key on the receiver
//! stops it.

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

use std::collections::BTreeSet;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_compositor::{InputEvent, OutputConfig, Screenshot};
use tilo_projector::control::{self, ConnectStage, ProjectorEvent, ProjectorRequest};
use tilo_projector::{ProjectionDevice, ProjectionMode, ProjectionProtocol};
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
use tilo_sheets::displays::{DisplaysSeat, Layout, Link, Sheet};

const WIDTH: i32 = 1000;
const HEIGHT: i32 = 700;

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;
const KEY_1: u32 = 2;
const KEY_2: u32 = 3;
const KEY_4: u32 = 5;
const KEY_8: u32 = 9;
const KEY_P: u32 = 25;
const KEY_ENTER: u32 = 28;
const KEY_DOWN: u32 = 108;
const KEY_LEFT_META: u32 = 125;

/// A session on `outputs`, its wireless display sheet speaking to the
/// projector on `projector` (`None`: the service's own socket).
fn session(outputs: &[OutputConfig], projector: Option<PathBuf>) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            projector,
            ..Options::default()
        },
        outputs,
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("step");
    session
}

/// Two displays side by side.
fn docked() -> Vec<OutputConfig> {
    vec![OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT), {
        let mut right = OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT);
        right.position = Some((WIDTH, 0));
        right
    }]
}

/// Press or release `keycode` and let the session carry it out.
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

/// Press `keys` together, then let go of them in reverse.
fn chord(session: &mut Session, keys: &[u32]) {
    for keycode in keys {
        key(session, *keycode, true);
    }
    for keycode in keys.iter().rev() {
        key(session, *keycode, false);
    }
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

/// The sheet's seat: whether it is up and what it shows.
fn seat(session: &Session) -> &DisplaysSeat {
    session.display_sheet().expect("the display sheet mounted")
}

/// What the sheet shows now.
fn shown(session: &Session) -> Sheet {
    seat(session).sheet()
}

fn up(session: &Session) -> bool {
    session
        .display_sheet()
        .is_some_and(|seat| seat.presentation().is_open())
}

/// A stand-in for the projector service on a real socket: it answers the one
/// connection the session holds with a receiver, and records what crossed.
struct Broker {
    socket: PathBuf,
    log: Arc<Mutex<Vec<String>>>,
    _runtime: tempfile::TempDir,
}

impl Broker {
    /// A broker that finds `receiver`, streams it once it is asked to, and
    /// stops when it is told to.
    fn answering(receiver: ProjectionDevice) -> Self {
        let runtime = tempfile::tempdir().expect("runtime dir");
        let socket = runtime.path().join("control.sock");
        let listener = UnixListener::bind(&socket).expect("broker socket");
        let log = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&log);
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let id = receiver.id.clone();
            while let Ok(Some(request)) = control::read_request(&mut stream) {
                let answer = match request {
                    ProjectorRequest::Discover { .. } => {
                        recorded_push(&recorded, "discover");
                        ProjectorEvent::Devices(vec![receiver.clone()])
                    }
                    ProjectorRequest::Connect { credential, .. } => {
                        let pin = credential.map_or_else(String::new, |c| c.expose().to_owned());
                        recorded_push(&recorded, &format!("connect:{pin}"));
                        let starting = ProjectorEvent::Connecting {
                            device_id: id.clone(),
                            stage: ConnectStage::Starting,
                        };
                        if control::write_event(&mut stream, &starting).is_err() {
                            return;
                        }
                        ProjectorEvent::Connected {
                            device_id: id.clone(),
                        }
                    }
                    ProjectorRequest::Disconnect => {
                        recorded_push(&recorded, "stop");
                        ProjectorEvent::Idle
                    }
                };
                if control::write_event(&mut stream, &answer).is_err() {
                    return;
                }
            }
        });
        Self {
            socket,
            log,
            _runtime: runtime,
        }
    }

    /// What the session asked of the broker since it started, oldest first.
    fn log(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn recorded_push(log: &Arc<Mutex<Vec<String>>>, what: &str) {
    log.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(what.to_owned());
}

/// A living room television that asks for its PIN.
fn television() -> ProjectionDevice {
    ProjectionDevice {
        id: "AirPlay:living-room".to_owned(),
        name: "Living room".to_owned(),
        protocol: ProjectionProtocol::AirPlay,
        detail: "Living room".to_owned(),
        modes: ProjectionMode::ALL.to_vec(),
        addresses: vec!["192.0.2.10".to_owned()],
        port: 7000,
    }
}

#[test]
fn super_p_opens_the_wireless_display_sheet() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&docked(), None);
    assert!(
        session.display_sheet().is_some(),
        "the session mounts the display sheet"
    );
    assert!(!up(&session), "nothing is showing yet");

    chord(&mut session, &[KEY_LEFT_META, KEY_P]);
    until(&mut session, up);
    assert!(
        session
            .display_sheet()
            .is_some_and(|seat| seat.presentation().is_drawn()),
        "the sheet is on screen"
    );

    // Two displays can be arranged every way the sheet offers, and the one
    // they show now is the highlight.
    let open = shown(&session);
    assert_eq!(
        open.layouts
            .iter()
            .map(|row| row.layout)
            .collect::<Vec<_>>(),
        Layout::ALL.to_vec()
    );
    assert!(open.layouts.iter().all(|row| row.enabled));
    assert_eq!(
        open.layouts
            .iter()
            .find(|row| row.current)
            .map(|row| row.layout),
        Some(Layout::Extend),
        "the desktop is extended across both displays"
    );

    tap(&mut session, KEY_ESCAPE);
    until(&mut session, |session| !up(session));
    assert!(
        !session
            .display_sheet()
            .is_some_and(|seat| seat.presentation().is_drawn()),
        "nothing of the sheet is left on screen"
    );
}

#[test]
fn a_chosen_receiver_is_asked_for_its_pin_and_projected_to() {
    let _settled = ui::motion::testing::settled();
    let broker = Broker::answering(television());
    let mut session = session(&docked(), Some(broker.socket.clone()));

    chord(&mut session, &[KEY_LEFT_META, KEY_P]);
    until(&mut session, up);
    // The sheet looks for receivers each time it opens.
    until(&mut session, |session| !shown(session).receivers.is_empty());

    let found = shown(&session);
    assert_eq!(found.receivers.len(), 1);
    let receiver = found.receivers[0].clone();
    assert_eq!(receiver.name, "Living room");
    assert_eq!(receiver.protocol, "AirPlay");
    assert!(receiver.asks_pin, "an AirPlay receiver asks for its PIN");
    assert_eq!(receiver.link, Link::Idle);

    // Down walks from the current layout's row to the receiver, and Enter
    // chooses it: nothing connects until its PIN is typed.
    let rows = found.layouts.len();
    for _ in 0..rows {
        tap(&mut session, KEY_DOWN);
    }
    tap(&mut session, KEY_ENTER);
    until(&mut session, |session| shown(session).asks_pin);

    let asking = shown(&session);
    assert!(
        asking.receivers[0].chosen,
        "the receiver is the one to stream"
    );
    assert_eq!(asking.receivers[0].link, Link::Idle);
    assert_eq!(
        broker.log(),
        vec!["discover"],
        "no session starts before the PIN is given"
    );

    // The PIN field holds the keyboard now: what is typed is what the
    // receiver is given, and Enter connects with it.
    for digit in [KEY_4, KEY_8, KEY_2, KEY_1] {
        tap(&mut session, digit);
    }
    assert_eq!(seat(&session).pin(), "4821");
    tap(&mut session, KEY_ENTER);

    until(&mut session, |session| {
        shown(session).receivers[0].link == Link::Connected
    });
    assert_eq!(broker.log(), vec!["discover", "connect:4821"]);
    // The sheet shows the running stream's options beside the receiver.
    let stream = shown(&session)
        .stream
        .expect("the running stream's options");
    assert_eq!(stream.resolution, "Native");
    assert!(stream.adaptive, "the stream fits the link by itself");
    assert_eq!(stream.fps, 60);
    assert!(stream.audio, "the sound goes with the picture");

    // Down leaves the PIN field for the receiver's row, and Enter on it stops
    // the stream.
    tap(&mut session, KEY_DOWN);
    tap(&mut session, KEY_ENTER);
    until(&mut session, |session| {
        shown(session).receivers[0].link == Link::Idle
    });
    assert_eq!(broker.log(), vec!["discover", "connect:4821", "stop"]);
}

/// What a copy of the whole display shows: what a screenshot, a recording,
/// a share or a remote viewer gets.
#[track_caller]
fn copied(session: &mut Session) -> Screenshot {
    session
        .compositor()
        .read_back_region("HEADLESS-1", (0, 0, WIDTH, HEIGHT))
        .unwrap_or_else(|error| panic!("a copy of HEADLESS-1: {error}"))
}

/// What the person's own display shows.
#[track_caller]
fn displayed(session: &mut Session) -> Screenshot {
    session
        .compositor()
        .screenshot("HEADLESS-1")
        .unwrap_or_else(|error| panic!("a frame of HEADLESS-1: {error}"))
}

/// The colours in `frame` inside `rect`, inset by `margin` pixels.
fn colours_in(frame: &Screenshot, rect: (u32, u32, u32, u32), margin: u32) -> BTreeSet<[u8; 4]> {
    let (x0, y0, x1, y1) = rect;
    ((y0 + margin)..=(y1 - margin))
        .flat_map(|y| ((x0 + margin)..=(x1 - margin)).map(move |x| (x, y)))
        .filter_map(|(x, y)| frame.pixel(x, y))
        .collect()
}

/// The rectangle `(x0, y0, x1, y1)` inside which `after` differs from
/// `before`: what a capture of the same display changed.
#[track_caller]
fn differed(before: &Screenshot, after: &Screenshot) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..after.height {
        for x in 0..after.width {
            if before.pixel(x, y) != after.pixel(x, y) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    assert!(
        x1 > x0 && y1 > y0,
        "the sheet the PIN is in changed in the copy"
    );
    (x0, y0, x1, y1)
}

/// A capture taken while a receiver's PIN sheet is up never shows what the
/// person typed into it: the sheet is one flat placeholder in the copy, and
/// the display in front of them shows it as it is.
#[test]
fn a_capture_of_the_pin_sheet_withholds_the_pin_and_the_display_shows_it() {
    let _settled = ui::motion::testing::settled();
    let broker = Broker::answering(television());
    let mut session = session(&docked(), Some(broker.socket));

    chord(&mut session, &[KEY_LEFT_META, KEY_P]);
    until(&mut session, up);
    until(&mut session, |session| !shown(session).receivers.is_empty());
    let rows = shown(&session).layouts.len();
    for _ in 0..rows {
        tap(&mut session, KEY_DOWN);
    }
    tap(&mut session, KEY_ENTER);
    until(&mut session, |session| shown(session).asks_pin);

    // Nothing secret is typed yet: the copy is the sheet as it stands.
    let asking = copied(&mut session);
    for digit in [KEY_4, KEY_8, KEY_2, KEY_1] {
        tap(&mut session, digit);
    }
    assert_eq!(seat(&session).pin(), "4821");

    let copy = copied(&mut session);
    let display = displayed(&mut session);
    let (x0, y0, x1, y1) = differed(&asking, &copy);
    assert!(
        x1 - x0 > 100 && y1 - y0 > 100,
        "the sheet's own panel left the copy, not a sliver of it: {:?}",
        (x0, y0, x1, y1)
    );
    // Inside the sheet the copy carries nothing but the one placeholder.
    let withheld = colours_in(&copy, (x0, y0, x1, y1), 8);
    assert_eq!(
        withheld.len(),
        1,
        "one flat placeholder where the sheet was: {withheld:?}"
    );
    // The person's own display still shows the sheet, its label and the
    // field the PIN went into.
    let shown_to_the_person = colours_in(&display, (x0, y0, x1, y1), 8);
    assert!(
        shown_to_the_person.len() > 8,
        "the sheet is on the display with its content: {shown_to_the_person:?}"
    );
    assert!(
        !shown_to_the_person.contains(&withheld.iter().next().copied().unwrap()),
        "and is not the placeholder the copy shows"
    );
    assert!(
        session
            .display_sheet()
            .is_some_and(|seat| seat.presentation().is_drawn()),
        "the sheet is still up for the person"
    );
}
