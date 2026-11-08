//! The lock screen as the session starts it: a process of its own, showing
//! what the session publishes as the lock's state on every display, and taking
//! the person's password back as actions on that resource.
//!
//! These start the lock screen the way locking does — through the session's
//! own surface machinery, over a plane only this test reaches — and type into
//! it through the compositor: what a person at the machine does. What the
//! screen shows and what it says is read from the session's published resource,
//! which is what any app of the session reads.

use super::readout::readout_saying;
use super::*;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tilo_session::ActionPlane;
use tilo_session::surfaces::Surface;

/// The resource the session publishes the lock as.
const LOCK: &str = "lock";

/// One display's refresh, while waiting for the lock screen to draw.
const REFRESH: Duration = Duration::from_millis(16);

/// The key that wakes the lock screen and types nothing into it: shift on its own changes no letter
/// and moves no control.
const WAKING_KEY: u32 = 42;

/// How long a wait may leave the lock screen alone: it goes quiet twenty seconds after the last
/// input, and what it draws then is faded from the screen.
const WAKING_EVERY: Duration = Duration::from_secs(5);

/// Keeps the lock screen awake while a test waits on it: a wait of its own must see what the screen
/// draws, not the quiet it keeps for a person who went away.
struct Waking {
    at: Instant,
}

impl Waking {
    /// A wait, starting now.
    fn new() -> Self {
        Self { at: Instant::now() }
    }

    /// Press the waking key if this wait has left the screen alone a while.
    fn keep(&mut self, session: &mut Session) {
        if self.at.elapsed() >= WAKING_EVERY {
            tap(session, WAKING_KEY);
            self.at = Instant::now();
        }
    }
}

/// Enter, as a US keyboard sends it.
const ENTER: u32 = 28;

/// The keycodes `word` is typed on, as a US keyboard sends them.
fn spelled(word: &str) -> Vec<u32> {
    word.chars()
        .map(|letter| match letter {
            'a' => 30,
            'e' => 18,
            'g' => 34,
            'm' => 50,
            'n' => 49,
            'o' => 24,
            'r' => 19,
            's' => 31,
            'w' => 17,
            other => panic!("the tests type no {other}"),
        })
        .collect()
}

/// Type `word` into the lock screen's field, as the person in front of it does, without pressing
/// Enter: what the field holds stays the screen's.
pub(super) fn type_word(session: &mut Session, word: &str) {
    for keycode in spelled(word) {
        tap(session, keycode);
    }
}

/// Type `word` into the lock screen's field and press Enter, as the person in front of it does.
pub(super) fn type_and_enter(session: &mut Session, word: &str) {
    type_word(session, word);
    tap(session, ENTER);
}

/// The lock screen the session starts: the app whose window asks the compositor for the session
/// lock.
pub(super) fn lock_screen() -> Surface {
    Surface::new("lock", screen_binary()).person("ada").lock()
}

/// A directory for the plane's socket that only this user reaches: the broker refuses to serve one
/// anybody else could open.
pub(super) fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the plane in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the plane's directory is this user's");
    root
}

/// The lock screen's binary: the app of its own package, which this run builds the first time a
/// test locks into it.
fn screen_binary() -> PathBuf {
    tilo_test_env::sibling_binary("tilo-lock-screen", "tilo-lock-screen")
}

/// A verifier the test answers, so the screen is seen saying it is checking a password while the
/// answer is still on its way.
struct Held {
    answer: std::sync::Mutex<Option<tilo_session::unlock::Verdict>>,
    given: std::sync::Condvar,
}

impl Held {
    fn new() -> Self {
        Self {
            answer: std::sync::Mutex::new(None),
            given: std::sync::Condvar::new(),
        }
    }

    /// Let the check that is waiting answer `verdict` .
    fn answer(&self, verdict: tilo_session::unlock::Verdict) {
        *self.answer.lock().expect("the verifier's answer") = Some(verdict);
        self.given.notify_all();
    }
}

impl tilo_session::unlock::PasswordVerifier for Held {
    fn verify(&self, _user: &str, _password: &str) -> tilo_session::unlock::Verdict {
        let mut answer = self.answer.lock().expect("the verifier's answer");
        while answer.is_none() {
            answer = self.given.wait(answer).expect("the verifier's answer");
        }
        answer.take().expect("the test answered")
    }
}

/// The options a session that starts its lock screen locks with, `verifier` answering the password
/// it is given.
fn locking_options(
    verifier: std::sync::Arc<dyn tilo_session::unlock::PasswordVerifier>,
) -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        user: "ada".to_owned(),
        verifier,
        collab: false,
        surfaces: vec![lock_screen()],
        ..Options::default()
    }
}

/// A session that starts its lock screen when it locks, serving its services on `socket` and
/// showing on `outputs` .
pub(super) fn lockable_session(
    socket: &Path,
    outputs: &[OutputConfig],
    verifier: std::sync::Arc<dyn tilo_session::unlock::PasswordVerifier>,
) -> Session {
    Session::headless(
        Options {
            action_plane: ActionPlane::At(socket.to_path_buf()),
            ..locking_options(verifier)
        },
        outputs,
    )
    .expect("session")
}

/// What the displays show, as a grid each: what the person in front of them sees.
pub(super) fn pictures(session: &mut Session, displays: &[&str]) -> Vec<Vec<f64>> {
    displays
        .iter()
        .map(|display| {
            picture(
                &session
                    .compositor()
                    .screenshot(display)
                    .expect("screenshot"),
            )
        })
        .collect()
}

/// Step the session until the lock screen's own frame is on `displays` and is not changing any
/// more: the screen itself, not the blank or dim a locked output shows before the client's first
/// frame arrives.
/// The lock screen is an app of its own, drawing on its own clock: the frame it covers the desk
/// with is not yet what it drew, and how many turns of the session pass before it rests is the
/// app's to decide. What settles it is the rule [`until_still`] reads — a screen that stopped
/// changing — never a count of turns of the session.
#[track_caller]
pub(super) fn until_drawn(
    session: &mut Session,
    displays: &[&str],
    was: &[Vec<f64>],
) -> Vec<Vec<f64>> {
    let deadline = tilo_test_env::hang_deadline();
    let mut waking = Waking::new();
    loop {
        waking.keep(session);
        let now = session
            .compositor()
            .lock_drawn()
            .then(|| pictures(session, displays));
        let covers = now.as_ref().is_some_and(|now| {
            now.iter()
                .zip(was)
                .all(|(now, was)| apart(now, was) > CLOSE)
        });
        if covers {
            for display in displays {
                until_still(session, display);
            }
            let drawn = pictures(session, displays);
            if drawn
                .iter()
                .zip(was)
                .all(|(now, was)| apart(now, was) > CLOSE)
            {
                return drawn;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the lock screen never drew on {displays:?}"
        );
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// Step the session until `displays` show what `was` showed again, settled: the desk, back.
#[track_caller]
pub(super) fn until_back(
    session: &mut Session,
    displays: &[&str],
    was: &[Vec<f64>],
) -> Vec<Vec<f64>> {
    let deadline = tilo_test_env::hang_deadline();
    let mut settled: Option<Vec<Vec<f64>>> = None;
    loop {
        let now = pictures(session, displays);
        let back = now.iter().zip(was).all(|(now, was)| apart(now, was) < 24.0);
        if back && settled.as_ref() == Some(&now) {
            return now;
        }
        settled = Some(now);
        assert!(
            Instant::now() < deadline,
            "{displays:?} never showed the desk again"
        );
        session.step(REFRESH).expect("a turn of the session");
    }
}

#[test]
fn a_password_typed_into_the_lock_screen_unlocks_the_session() {
    const DISPLAY: &str = "HEADLESS-1";
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let held = std::sync::Arc::new(Held::new());
    let mut session = lockable_session(
        &home.path().join("collab.sock"),
        &[OutputConfig::new(DISPLAY, WIDTH, HEIGHT)],
        std::sync::Arc::clone(&held) as std::sync::Arc<dyn tilo_session::unlock::PasswordVerifier>,
    );
    let _client = mapped(&mut session, "org.example.Terminal");
    let desk = pictures(&mut session, &[DISPLAY]);

    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    pump(&mut session, &mut [], |session, _| {
        session.compositor().locked()
    });
    let locked = until_drawn(&mut session, &[DISPLAY], &desk);
    assert!(
        session.surfaces().running("lock"),
        "the session locked with its lock screen: {:?}",
        session.surfaces().names()
    );
    let colours: std::collections::BTreeSet<Vec<u8>> = session
        .compositor()
        .screenshot(DISPLAY)
        .expect("screenshot")
        .rgba
        .chunks(4)
        .map(<[u8]>::to_vec)
        .collect();
    assert!(
        colours.len() > 2,
        "the lock screen draws its clock and prompt, not only the dim"
    );
    assert!(
        publisher(&session, "overview").is_none(),
        "the locked session withholds the desktop's actions"
    );
    // What the screen shows is the session's own resource: the same reading any app of the session
    // reads.
    let reading = session.read(LOCK).expect("the session publishes the lock");
    assert_eq!(reading["person"], "ada", "it says whose session this is");
    assert_eq!(reading["unlocking"], false);
    assert!(reading["error"].is_null(), "nothing was refused yet");
    assert!(
        reading["clock"]
            .as_str()
            .is_some_and(|clock| !clock.is_empty()),
        "it shows the time: {reading}"
    );

    // A wrong password is refused, and the screen goes on showing it. The screen says it is
    // checking while the answer is on its way.
    for keycode in spelled("wrong") {
        tap(&mut session, keycode);
    }
    tap(&mut session, ENTER);
    pump(&mut session, &mut [], |session, _| {
        let reading = session.read(LOCK).expect("the session publishes the lock");
        assert!(
            !reading.to_string().contains("wrong"),
            "the password the person typed is in no reading the session publishes: {reading}"
        );
        reading["checking"] == true
    });
    held.answer(tilo_session::unlock::Verdict::Wrong);
    let mut leaked = false;
    pump(&mut session, &mut [], |session, _| {
        let reading = session.read(LOCK).expect("the session publishes the lock");
        leaked |= reading.to_string().contains("wrong");
        reading["error"].as_str() == Some(tilo_sheets::lock::wrong_password().as_str())
    });
    assert!(
        !leaked,
        "the password the person typed is in no reading the session publishes"
    );
    assert!(
        session.compositor().locked(),
        "a wrong password keeps the session locked"
    );

    // The right one unlocks it, and the desk comes back.
    type_and_enter(&mut session, "sesame");
    held.answer(tilo_session::unlock::Verdict::Unlocks);
    pump(&mut session, &mut [], |session, _| {
        !session.compositor().locked()
    });
    pump(&mut session, &mut [], |session, _| {
        !session.surfaces().running("lock")
    });
    let after = until_back(&mut session, &[DISPLAY], &desk);
    let back = apart(&after[0], &desk[0]);
    let was = apart(&after[0], &locked[0]);
    assert!(
        back * 4.0 < was && back < 24.0,
        "unlocking clears the lock screen: what the display shows is {back:.0} of 255 from the \
         desk it showed before the lock, and {was:.0} from the lock screen"
    );
    assert!(
        publisher(&session, "overview").is_some(),
        "the unlocked session publishes the desktop's actions again"
    );
}

/// The colours `frame` is drawn in.
fn colours(frame: &image::RgbaImage) -> std::collections::BTreeSet<[u8; 4]> {
    frame.pixels().map(|pixel| pixel.0).collect()
}

/// The displays as the door's snapshot draws them: the PNG the session writes under `home` for
/// `name` , read back.
#[track_caller]
fn door_picture(session: &mut Session, door: &Path, home: &Path, name: &str) -> image::RgbaImage {
    let path = home.join(format!("{name}.png"));
    let asked = super::common::control(
        session,
        door,
        &serde_json::json!({
            "op": "snapshot",
            "path": path.to_str().expect("a path the socket carries"),
        }),
    );
    // The answer also says whether the picture is of the lock screen, which is what the lock's own
    // tests read of it.
    assert_eq!(asked["ok"]["frame"], 0, "the door draws {name}: {asked}");
    image::open(&path)
        .unwrap_or_else(|error| panic!("{name}: {error}"))
        .to_rgba8()
}

/// The lock the session holds is what the displays show, and a check reads it
/// there: the door's snapshot draws the lock screen the app put on the display,
/// while a copy of the display that leaves the session is the one placeholder.
/// Unlocking puts the desk back in both.
#[test]
fn a_check_reads_the_lock_screen_the_displays_show_and_unlocking_puts_the_desk_back() {
    const DISPLAY: &str = "HEADLESS-1";
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let door = home.path().join("test-control.sock");
    let mut session = Session::headless(
        Options {
            action_plane: ActionPlane::At(home.path().join("collab.sock")),
            test_control: Some(door.clone()),
            ..locking_options(std::sync::Arc::new(Sesame))
        },
        &[OutputConfig::new(DISPLAY, WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("a turn");

    // The desk, as the door draws it before anything locks.
    let desk = door_picture(&mut session, &door, home.path(), "desk");
    assert!(colours(&desk).len() > 8, "the desk, not a flat frame");

    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    pump(&mut session, &mut [], |session, _| {
        session.compositor().lock_drawn()
    });
    // The screen arrives on its own motion: what a person sees is the frame it settles at, not the
    // first one the app drew on its way in.
    until_still(&mut session, DISPLAY);
    assert!(
        session.surfaces().running("lock"),
        "the session locked with the lock screen it starts: {:?}",
        session.surfaces().names()
    );

    // The lock screen is on the display, drawn by the app: more than a placeholder's one colour,
    // and not the placeholder a copy gets.
    let locked = door_picture(&mut session, &door, home.path(), "lock-screen");
    assert!(
        colours(&locked).len() > 8,
        "the lock screen is on the display: {} colours in it",
        colours(&locked).len()
    );
    assert_ne!(
        colours(&locked),
        colours(&desk),
        "the display shows the lock screen, not the desk it covered"
    );
    let copy: std::collections::BTreeSet<[u8; 4]> = session
        .compositor()
        .read_back_region(DISPLAY, (0, 0, WIDTH, HEIGHT))
        .expect("a copy of the display")
        .rgba
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect();
    assert_eq!(copy.len(), 1, "a copy withholds the lock screen: {copy:?}");
    assert_ne!(
        colours(&locked),
        copy,
        "the door draws the display, not the copy's placeholder"
    );

    // The right password unlocks it, and the desk comes back to the door and to a copy of the
    // display alike.
    type_and_enter(&mut session, "sesame");
    pump(&mut session, &mut [], |session, _| {
        !session.compositor().locked()
    });
    let back = door_picture(&mut session, &door, home.path(), "desk-again");
    assert!(
        colours(&back).len() > 8,
        "the desk is back on the display, not the lock screen"
    );
    let copy: std::collections::BTreeSet<[u8; 4]> = session
        .compositor()
        .read_back_region(DISPLAY, (0, 0, WIDTH, HEIGHT))
        .expect("a copy of the display")
        .rgba
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect();
    assert!(copy.len() > 8, "a copy is the desk again");
}

/// A session whose lock screen's app is not installed still locks, and its own view of the lock is
/// what shows it: locking without one would leave the person with a desktop they cannot come back
/// to, so the session draws the lock itself, and only the right password lifts it.
#[test]
fn a_session_with_no_lock_screen_locks_and_its_own_view_unlocks_it() {
    const DISPLAY: &str = "HEADLESS-1";
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let mut session = crate::common::headless_on(
        &[OutputConfig::new(DISPLAY, WIDTH, HEIGHT)],
        Options {
            user: "ada".to_owned(),
            verifier: std::sync::Arc::new(Sesame),
            collab: false,
            action_plane: ActionPlane::At(home.path().join("collab.sock")),
            surfaces: Vec::new(),
            ..Options::default()
        },
    );
    let _client = mapped(&mut session, "org.example.Terminal");
    let desk = pictures(&mut session, &[DISPLAY]);
    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    pump(&mut session, &mut [], |session, _| {
        session.compositor().locked()
    });
    assert!(
        !session.surfaces().running("lock"),
        "no screen's app is installed: {:?}",
        session.surfaces().names()
    );
    assert!(
        publisher(&session, "overview").is_none(),
        "the locked session withholds the desktop's actions"
    );
    // What shows the lock is the session's own view of it, over the desk.
    until_drawn(&mut session, &[DISPLAY], &desk);
    assert!(
        session.compositor().locked(),
        "the lock holds while the session's own view shows it"
    );

    // A wrong password keeps it locked and says so; the right one lifts it.
    type_and_enter(&mut session, "wrong");
    pump(&mut session, &mut [], |session, _| {
        let reading = session.read(LOCK).expect("the session publishes the lock");
        assert!(
            !reading.to_string().contains("wrong"),
            "the password the person typed is in no reading the session publishes: {reading}"
        );
        reading["error"].as_str() == Some(tilo_sheets::lock::wrong_password().as_str())
    });
    assert!(
        session.compositor().locked(),
        "a wrong password keeps the session locked"
    );
    type_and_enter(&mut session, "sesame");
    pump(&mut session, &mut [], |session, _| {
        !session.compositor().locked()
    });
    let after = until_back(&mut session, &[DISPLAY], &desk);
    assert!(
        apart(&after[0], &desk[0]) < 24.0,
        "unlocking clears the lock and shows the desk again"
    );
    assert!(
        publisher(&session, "overview").is_some(),
        "the unlocked session publishes the desktop's actions again"
    );
}

/// A session on two displays, the second smaller than the first: the lock screen covers every one
/// of them while the session is locked — the same screen on both, of whatever size they are — and a
/// press on either of them reaches it. A password unlocks both.
#[test]
fn every_display_shows_the_lock_screen_while_the_session_is_locked() {
    const DISPLAYS: [&str; 2] = ["HEADLESS-1", "HEADLESS-2"];
    let _settled = ui::motion::testing::settled();
    // What the desk shows before the lock and after it is the same desk: the bar's own minute
    // turning over in between is not a difference.
    let _clock = tilo_test_env::held_wall_clock();
    let external = OutputConfig::new("HEADLESS-2", 1024, 768).at(WIDTH, 0);
    let home = private_home();
    let mut session = lockable_session(
        &home.path().join("collab.sock"),
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT), external],
        std::sync::Arc::new(Sesame),
    );
    let _client = mapped(&mut session, "org.example.Terminal");
    let desk = pictures(&mut session, &DISPLAYS);

    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    pump(&mut session, &mut [], |session, _| {
        session.compositor().locked()
    });
    let locked = until_drawn(&mut session, &DISPLAYS, &desk);
    // Every display shows the same screen, each scaled to the display it is on: the picture is the
    // same on both, of whatever size they are.
    let shown = apart(&locked[0], &locked[1]);
    assert!(
        shown < CLOSE,
        "every display shows the lock screen, but their pictures differ by {shown:.0} of 255"
    );

    // A press on the display the lock screen has no surface of its own on brings up the screen's
    // own keyboard: the screen is what the person touches, on whichever display they touch.
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: f64::from(WIDTH) + 500.0,
            y: 300.0,
        })
        .expect("motion");
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: 0x110,
            pressed: true,
        })
        .expect("press");
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: 0x110,
            pressed: false,
        })
        .expect("release");
    let touched = until_drawn(&mut session, &DISPLAYS, &locked);
    assert!(
        apart(&touched[0], &touched[1]) < CLOSE,
        "both displays show the screen the press raised"
    );

    type_and_enter(&mut session, "sesame");
    pump(&mut session, &mut [], |session, _| {
        !session.compositor().locked()
    });
    pump(&mut session, &mut [], |session, _| {
        !session.surfaces().running("lock")
    });
    until_back(&mut session, &DISPLAYS, &desk);
}

/// The lock the power menu offers is the session's own: choosing it starts the lock screen, as the
/// session's own action does.
#[test]
fn the_power_menus_lock_starts_the_lock_screen() {
    use tilo_sheets::Seat as _;
    use tilo_sheets::power::PowerEffect;
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let mut session = lockable_session(
        &home.path().join("collab.sock"),
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        std::sync::Arc::new(Sesame),
    );
    session.step(Duration::from_millis(1)).expect("step");
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
    session.step(Duration::from_millis(1)).expect("step");
    let menu = sheets(&session)
        .pop()
        .expect("the open menu publishes its choices");
    session
        .invoke(&menu, lock, &serde_json::Map::new())
        .expect("lock");
    pump(&mut session, &mut [], |session, _| {
        session.compositor().locked()
    });
    assert!(
        session.surfaces().running("lock"),
        "the session locked with its lock screen: {:?}",
        session.surfaces().names()
    );
}

/// What plays reaches the screen in its reading, and the transport the screen
/// offers steers the player the session hosts: the locked screen's transport,
/// as the unlocked one's.
#[test]
fn the_lock_screens_transport_steers_whatever_plays() {
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let asked = Rc::new(RefCell::new(Vec::new()));
    let published = Publishing::default();
    let mut session = crate::common::headless(Options {
        services: vec![Box::new(PlayerService::new(
            Rc::clone(&asked),
            published.clone(),
        ))],
        user: "ada".to_owned(),
        verifier: std::sync::Arc::new(Sesame),
        collab: false,
        action_plane: ActionPlane::At(home.path().join("collab.sock")),
        surfaces: vec![lock_screen()],
        ..Options::default()
    });
    // The services publish what plays and what the pack holds; the session reads both, and the
    // screen shows what it read.
    published.set_playing(Some(song("Clair de lune", "Claude Debussy", true, true)));
    published.set_battery(62, true);
    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    pump(&mut session, &mut [], |session, _| {
        session.compositor().locked()
    });
    pump(&mut session, &mut [], |session, _| {
        session.read(LOCK).is_some_and(|reading| {
            reading["media"]["title"] == "Clair de lune"
                // What the device says about itself reaches the screen too.
                && reading["status"]["battery"]["percent"] == 62
                && reading["status"]["battery"]["charging"] == true
        })
    });

    for action in ["play-pause", "next-track"] {
        invoke(&mut session, LOCK, action);
    }
    pump(&mut session, &mut [], |_, _| asked.borrow().len() == 2);
    assert_eq!(
        *asked.borrow(),
        ["play-pause", "next"],
        "the locked screen's transport reaches the player, as the unlocked one does"
    );

    // With nothing playing there is no transport, and no player is steered.
    published.set_playing(None);
    for _ in 0..20 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    invoke(&mut session, LOCK, "play-pause");
    for _ in 0..20 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(
        *asked.borrow(),
        ["play-pause", "next"],
        "nothing plays, so nothing is asked of a player"
    );
}

/// What the services publish, as the test changes it: the players' state and the pack's charge.
#[derive(Clone, Default)]
pub(super) struct Publishing {
    /// What plays, as the audio service reports it: the players, and nothing while nothing plays.
    playing: Rc<RefCell<Option<serde_json::Value>>>,
    /// The charge of the pack this machine runs on, as the power service reports it; `None` while
    /// it has not read one.
    battery: Rc<RefCell<Option<serde_json::Value>>>,
    /// Whether what it publishes moved since the session last read it.
    moved: Rc<Cell<bool>>,
}

impl Publishing {
    /// Publish `playing` as what plays.
    pub(super) fn set_playing(&self, playing: Option<serde_json::Value>) {
        *self.playing.borrow_mut() = playing;
        self.moved.set(true);
    }

    /// Publish the charge the pack reports.
    fn set_battery(&self, percent: u8, charging: bool) {
        *self.battery.borrow_mut() = Some(serde_json::json!({
            "present": true,
            "percentage": percent,
            "state": if charging { "charging" } else { "discharging" },
            "on_battery": !charging,
        }));
        self.moved.set(true);
    }
}

/// The player, as a system service the session hosts: it publishes what plays and what the pack
/// holds, and records what it was asked.
pub(super) struct PlayerService {
    asked: Rc<RefCell<Vec<String>>>,
    published: Publishing,
}

impl PlayerService {
    /// A player that publishes nothing until the test hands it what plays.
    pub(super) fn new(asked: Rc<RefCell<Vec<String>>>, published: Publishing) -> Self {
        Self { asked, published }
    }
}

impl tilo_session::Service for PlayerService {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
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
            tilo_actions::ResourceSpec::new(tilo_session::MEDIA, "media", "Player")
                .with_action(spec("play-pause"))
                .with_action(spec("next"))
                .with_action(spec("previous")),
            tilo_actions::ResourceSpec::new(
                tilo_session_power::BATTERY,
                "power:battery",
                "Battery",
            ),
        ]
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        match resource {
            tilo_session::MEDIA => self.published.playing.borrow().clone(),
            tilo_session_power::BATTERY => self.published.battery.borrow().clone(),
            _ => None,
        }
    }

    fn poll(&mut self) -> bool {
        self.published.moved.replace(false)
    }

    fn invoke(
        &mut self,
        _resource: &str,
        action: &str,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        self.asked.borrow_mut().push(action.to_owned());
        Ok(tilo_service::Started::Done(
            tilo_actions::ActionOutput::default(),
        ))
    }
}

/// A song the player plays, as the audio service publishes it: what the lock screen app names on
/// its card, and what it offers to do with it.
fn song(title: &str, subtitle: &str, playing: bool, controls: bool) -> serde_json::Value {
    serde_json::json!({
        "players": [{
            "identity": "Music",
            "title": title,
            "artist": subtitle,
            "playing": playing,
            "paused": !playing,
            "can-play": controls,
            "can-pause": controls,
            "can-previous": controls,
            "can-next": controls,
        }]
    })
}

/// How far a pixel's colour has to move for the screen to have drawn something there, rather than
/// the same shape edge antialiased again.
const DIFFERS: u8 = 24;

/// How long a screen draws the same frame before what it drew is what it settled at: an appearance,
/// a layout and a fade take frames to finish, and a busy machine may draw none of them for a while.
const STILL_FOR: Duration = Duration::from_secs(1);

/// The width the lock screen draws its card at, in logical pixels: the box it lays out what it
/// shows of the session in.
const CARD: u32 = 400;

/// The pixels of `now` that differ from `was` by more than [`DIFFERS`]: their bounding box and how
/// many there are, `None` when the two frames show the same screen.
fn redrawn(
    now: &tilo_compositor::Screenshot,
    was: &tilo_compositor::Screenshot,
) -> Option<((u32, u32, u32, u32), u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    let mut count = 0;
    for y in 0..now.height.min(was.height) {
        for x in 0..now.width.min(was.width) {
            let (Some(here), Some(there)) = (now.pixel(x, y), was.pixel(x, y)) else {
                continue;
            };
            let moved = here
                .iter()
                .zip(there.iter())
                .map(|(here, there)| here.abs_diff(*there))
                .max()
                .unwrap_or(0);
            if moved <= DIFFERS {
                continue;
            }
            count += 1;
            bounds = Some(match bounds {
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                None => (x, y, x, y),
            });
        }
    }
    bounds.map(|bounds| (bounds, count))
}

/// Step the session until `display` has drawn the same frame for a while, and return it: what the
/// screen settled at.
/// A screen paints in stages, and a locked output shows the frame it has until the next arrives:
/// what a screen drew of a reading is the frame it stops changing at, not the first one that
/// differs.
#[track_caller]
fn until_still(session: &mut Session, display: &str) -> tilo_compositor::Screenshot {
    let deadline = tilo_test_env::hang_deadline();
    let mut waking = Waking::new();
    let mut last = session
        .compositor()
        .screenshot(display)
        .expect("screenshot");
    let mut since = Instant::now();
    loop {
        waking.keep(session);
        session.step(REFRESH).expect("a turn of the session");
        let now = session
            .compositor()
            .screenshot(display)
            .expect("screenshot");
        if redrawn(&now, &last).is_some() {
            since = Instant::now();
        } else if since.elapsed() >= STILL_FOR {
            return now;
        }
        assert!(
            Instant::now() < deadline,
            "the lock screen never stopped changing what it draws"
        );
        last = now;
    }
}

/// Step the session until `display` draws something other than `was` and returns the frame it
/// settles at: the lock screen's answer to a reading.
/// The screen lays out and paints a reading over the frames that follow it, so a frame that differs
/// from `was` is not yet what it drew of it.
#[track_caller]
fn until_redrawn(
    session: &mut Session,
    display: &str,
    was: &tilo_compositor::Screenshot,
) -> tilo_compositor::Screenshot {
    let deadline = tilo_test_env::hang_deadline();
    let mut waking = Waking::new();
    let mut last = was.clone();
    let mut since = Instant::now();
    loop {
        waking.keep(session);
        let now = session
            .compositor()
            .screenshot(display)
            .expect("screenshot");
        if redrawn(&now, was).is_some() {
            if redrawn(&now, &last).is_some() {
                since = Instant::now();
            } else if since.elapsed() >= STILL_FOR {
                return now;
            }
        } else {
            since = Instant::now();
        }
        assert!(
            Instant::now() < deadline,
            "the lock screen drew nothing new for the reading the session published"
        );
        last = now;
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// Step the session until the lock's reading says `media` , which is what the session publishes for
/// the screen: the reading any app of it reads.
#[track_caller]
fn until_media(session: &mut Session, media: impl Fn(&serde_json::Value) -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    let mut waking = Waking::new();
    loop {
        if session
            .read(LOCK)
            .is_some_and(|reading| media(&reading["media"]))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the session published no such reading of what plays"
        );
        waking.keep(session);
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// The display the lock screen shows on.
const DISPLAY: &str = "HEADLESS-1";

/// A locked session whose lock screen is the app, with a player the session hosts: what it
/// publishes of what plays, and what the player was asked.
pub(super) struct Locked {
    /// The window on the desk the lock came over, which keeps the session's compositor showing a
    /// desk under it.
    _client: TestClient,
    pub(super) session: Session,
    asked: Rc<RefCell<Vec<String>>>,
    /// What the player publishes, which the session shows on the lock.
    published: Publishing,
    /// What the app drew of the session with nothing playing.
    pub(super) quiet: tilo_compositor::Screenshot,
    _home: tempfile::TempDir,
}

impl Locked {
    /// Lock a session whose lock screen is the app this repository builds,
    /// and read the screen it drew with nothing playing.
    pub(super) fn new() -> Self {
        Self::themed(ui::headless::fixture_runner_inputs)
    }

    /// The same, drawn by a desk presenting itself through `presentation` .
    pub(super) fn themed(presentation: fn() -> ui::application::ComponentRunnerInputs) -> Self {
        let home = private_home();
        let asked = Rc::new(RefCell::new(Vec::new()));
        let published = Publishing::default();
        let mut session = Session::headless(
            Options {
                presentation,
                services: vec![Box::new(PlayerService::new(
                    Rc::clone(&asked),
                    published.clone(),
                ))],
                user: "ada".to_owned(),
                verifier: std::sync::Arc::new(Sesame),
                collab: false,
                action_plane: ActionPlane::At(home.path().join("collab.sock")),
                surfaces: vec![lock_screen()],
                ..Options::default()
            },
            &[OutputConfig::new(DISPLAY, WIDTH, HEIGHT)],
        )
        .expect("session");
        let client = mapped(&mut session, "org.example.Terminal");
        let desk = pictures(&mut session, &[DISPLAY]);
        invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
        pump(&mut session, &mut [], |session, _| {
            session.compositor().locked()
        });
        until_drawn(&mut session, &[DISPLAY], &desk);
        let quiet = until_still(&mut session, DISPLAY);
        Self {
            _client: client,
            session,
            asked,
            published,
            quiet,
            _home: home,
        }
    }

    /// Play `now` in the player the session follows and read the screen the app drew in answer to
    /// what it drew in `was` : the session says what plays in its reading, and the app draws that
    /// reading.
    pub(super) fn plays(
        &mut self,
        now: Option<serde_json::Value>,
        media: impl Fn(&serde_json::Value) -> bool,
        was: &tilo_compositor::Screenshot,
    ) -> tilo_compositor::Screenshot {
        self.published.set_playing(now);
        until_media(&mut self.session, media);
        until_redrawn(&mut self.session, DISPLAY, was)
    }
}

/// What plays reaches the lock screen the session runs as an app of its own,
/// and the transport it draws steers the player the session hosts.
///
/// The screen is a process of its own, so what it draws is read from its
/// frame: each change of the reading the session publishes changes the card
/// it drew, and the card goes when nothing plays.
#[test]
fn the_lock_screen_app_draws_what_plays_and_its_transport_steers_the_player() {
    let _settled = ui::motion::testing::settled();
    // The session publishes the time it is to the minute and the screen draws it: held at the
    // minute the test runs in, the screen changes for what plays and for nothing else.
    let at = ui::time::wall()
        .checked_sub(Duration::from_secs(
            u64::try_from(ui::clock::local_now().timestamp().rem_euclid(60))
                .expect("a second of the minute"),
        ))
        .expect("the minute the test runs in");
    ui::time::testing::with_wall(at, || {
        let mut locked = Locked::new();
        let quiet = locked.quiet.clone();

        // A track starts playing: the session publishes it with the lock, and the app draws the
        // card it was given, under the clock and the field, no wider than the card the screen has.
        let playing = locked.plays(
            Some(song("Clair de lune", "Claude Debussy", true, true)),
            |media| {
                media["title"] == "Clair de lune"
                    && media["playing"] == true
                    && media["can_toggle"] == true
                    && media["can_next"] == true
                    && media["can_previous"] == true
            },
            &quiet,
        );
        let ((x0, _, x1, _), drawn) = redrawn(&playing, &quiet).expect("the app drew what plays");
        assert!(
            drawn > 500,
            "the lock screen app drew the song, and not {drawn} pixels of it"
        );
        assert!(
            x1 - x0 <= CARD + 40,
            "the app drew the card of what plays, {} wide, and the ground around it",
            x1 - x0
        );
        assert!(
            u32::abs_diff(x0 + x1, u32::try_from(WIDTH).unwrap()) <= 80,
            "the card is centred on the screen the session shows it on: {x0}..{x1}"
        );
        assert!(
            quiet.pixel(2, 2) == playing.pixel(2, 2),
            "the app drew the card on the ground it stands on, rather than repainting that ground"
        );

        // The transport the app draws asks the player the session hosts for the song it shows: the
        // actions the screen publishes are the ones it runs.
        let published = locked.session.surfaces().published("lock");
        let transport = published
            .iter()
            .find(|published| published.action == "play-pause")
            .unwrap_or_else(|| panic!("the lock screen app publishes no transport: {published:?}"))
            .resource
            .clone();
        assert!(
            published.iter().any(
                |published| published.action == "next-track" && published.resource == transport
            ),
            "the transport is one resource of the screen's: {published:?}"
        );
        for action in ["play-pause", "next-track"] {
            assert!(
                locked
                    .session
                    .ask_surface(
                        "lock",
                        &transport,
                        action,
                        tilo_actions::remote::InvokeArgument::None
                    )
                    .is_some(),
                "the lock screen app runs its own {action}"
            );
        }
        pump(&mut locked.session, &mut [], |_, _| {
            locked.asked.borrow().len() == 2
        });
        assert_eq!(
            *locked.asked.borrow(),
            ["play-pause", "next"],
            "the transport the lock screen app draws reaches the player the session hosts"
        );

        // Only the title changes: the screen draws the song its reading names.
        let retitled = locked.plays(
            Some(song("Nocturne", "Claude Debussy", true, true)),
            |media| media["title"] == "Nocturne",
            &playing,
        );

        // Only who it is by changes: the line under it is drawn too.
        let credited = locked.plays(
            Some(song("Nocturne", "Frédéric Chopin", true, true)),
            |media| media["subtitle"] == "Frédéric Chopin",
            &retitled,
        );

        // What the player can be asked changes: the screen draws the transport it is told of, and
        // none where the player offers none.
        let stilled = locked.plays(
            Some(song("Nocturne", "Frédéric Chopin", false, false)),
            |media| {
                media["playing"] == false
                    && media["can_toggle"] == false
                    && media["can_next"] == false
            },
            &credited,
        );

        // Nothing plays: the card goes.
        locked.plays(None, serde_json::Value::is_null, &stilled);
    });
}

/// How far apart two pictures of a screen are, of 255: the most one cell of the grid they are
/// compared on differs by.
pub(super) fn apart(one: &[f64], other: &[f64]) -> f64 {
    one.iter()
        .zip(other)
        .map(|(here, there)| (here - there).abs())
        .fold(0.0_f64, f64::max)
}

/// How much two pictures of the same screen may differ and still be the same screen: the grid's
/// cells may be resampled differently on displays of different sizes.
pub(super) const CLOSE: f64 = 6.0;

/// What a screenshot shows, as the mean brightness of each cell of a grid laid over it: a screen
/// filling two displays of different sizes fills the same part of each.
pub(super) fn picture(shot: &tilo_compositor::Screenshot) -> Vec<f64> {
    const COLUMNS: u32 = 16;
    const ROWS: u32 = 10;
    let mut cells = Vec::with_capacity((COLUMNS * ROWS) as usize);
    for row in 0..ROWS {
        for column in 0..COLUMNS {
            let left = column * shot.width / COLUMNS;
            let right = (column + 1) * shot.width / COLUMNS;
            let top = row * shot.height / ROWS;
            let bottom = (row + 1) * shot.height / ROWS;
            let mut sum = 0.0;
            let mut channels = 0.0;
            for y in top..bottom {
                for x in left..right {
                    let pixel = shot.pixel(x, y).expect("inside the image");
                    for channel in &pixel[..3] {
                        sum += f64::from(*channel);
                        channels += 1.0;
                    }
                }
            }
            cells.push(sum / channels);
        }
    }
    cells
}

/// The box every pixel `after` changed from `before` covers, `None` when the two pictures are the
/// same.
pub(super) fn changed(
    before: &tilo_compositor::Screenshot,
    after: &tilo_compositor::Screenshot,
) -> Option<(usize, usize, usize, usize)> {
    let (width, height) = (after.width as usize, after.height as usize);
    let mut box_of: Option<(usize, usize, usize, usize)> = None;
    for y in 0..height {
        for x in 0..width {
            let at = (y * width + x) * 4;
            if before.rgba[at..at + 4] == after.rgba[at..at + 4] {
                continue;
            }
            box_of = Some(match box_of {
                None => (x, y, x + 1, y + 1),
                Some((left, top, right, bottom)) => {
                    (left.min(x), top.min(y), right.max(x + 1), bottom.max(y + 1))
                }
            });
        }
    }
    box_of
}

/// A level key pressed on the lock screen is answered there: the readout shows over the lock, on
/// the display the person is at, and the lock keeps every key and pointer press it always did.
#[test]
fn a_level_key_pressed_on_the_lock_screen_answers_over_it() {
    const DISPLAY: &str = "HEADLESS-1";
    let _settled = ui::motion::testing::settled();
    // The screen's clock is the machine's, and it turns over when it does: held, the pictures
    // either side of the key show the same minute, and what changed between them is what the key
    // drew.
    let _clock = tilo_test_env::held_wall_clock();
    let home = private_home();
    let held = std::sync::Arc::new(Held::new());
    let asked = Rc::new(RefCell::new(Vec::new()));
    let mut session = crate::common::headless_on(
        &[OutputConfig::new(DISPLAY, WIDTH, HEIGHT)],
        Options {
            user: "ada".to_owned(),
            verifier: std::sync::Arc::clone(&held)
                as std::sync::Arc<dyn tilo_session::unlock::PasswordVerifier>,
            collab: false,
            action_plane: ActionPlane::At(home.path().join("collab.sock")),
            surfaces: vec![lock_screen()],
            services: vec![Box::new(Audio::speakers(Rc::clone(&asked)))],
            ..Options::default()
        },
    );
    let _client = mapped(&mut session, "org.example.Terminal");
    let desk = pictures(&mut session, &[DISPLAY]);
    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    pump(&mut session, &mut [], |session, _| {
        session.compositor().locked()
    });
    let locked = until_drawn(&mut session, &[DISPLAY], &desk);
    let before = session
        .compositor()
        .screenshot(DISPLAY)
        .expect("screenshot");
    assert!(
        apart(&picture(&before), &locked[0]) < CLOSE,
        "the locked screen is still the one drawn under the key"
    );

    // The sound key works here: it changes a level and reveals nothing behind
    // the lock. Its readout answers on the screen the key was pressed at.
    //
    // The readout stands for a moment of its own, and a machine running the
    // whole suite can spend that moment on the turn that raised it: the key,
    // the words it answers with and the frame that draws them are read at one
    // reading of the clock, so the frame measured is the one the key drew.
    let after = ui::time::testing::with_monotonic_time(ui::time::now(), || {
        tap(&mut session, keys::VOLUME_UP);
        assert_eq!(
            readout_saying(&mut session, "Speakers"),
            "Speakers, Muted",
            "the locked session answers the device key, naming the output"
        );
        // The frame the answer is drawn in, over the lock's own: step until a frame differs from
        // the lock's and the next one is the same, so what is measured is the readout at rest
        // rather than a frame of its arrival.
        let mut last: Option<tilo_compositor::Screenshot> = None;
        pump(&mut session, &mut [], |session, _| {
            let now = session
                .compositor()
                .screenshot(DISPLAY)
                .expect("screenshot");
            let drawn = changed(&before, &now).is_some();
            let settled = last.as_ref().is_some_and(|last| last.rgba == now.rgba);
            last = Some(now);
            drawn && settled
        });
        last.expect("the readout's frame")
    });
    assert!(
        !asked.borrow().is_empty(),
        "the key reached the sound service while locked"
    );
    // The lock's own frame stays: what changed is the readout drawn over it,
    // across the middle of the display's foot.
    let (left, top, right, bottom) =
        changed(&before, &after).expect("the readout paints over the lock");
    // The readout stands on the middle of the screen, as one band: its frame is what the system
    // settings draw, so what is held is that it is centred and of the readout's size, never the
    // width of a frame of its arrival.
    assert!(
        left.midpoint(right).abs_diff(WIDTH as usize / 2) <= 24 && right - left <= 500,
        "the readout is across the middle of the screen: {left}..{right}"
    );
    assert!(
        (640..=760).contains(&top) && (640..=800).contains(&bottom),
        "the readout is on the screen's foot, over the lock: {top}..{bottom}"
    );
}
