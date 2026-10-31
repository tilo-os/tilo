//! The lock screen as the session starts it: a process of its own, showing
//! what the session publishes as the lock's state on every display, and taking
//! the person's password back as actions on that resource.
//!
//! These start the lock screen the way locking does — through the session's
//! own surface machinery, over a plane only this test reaches — and type into
//! it through the compositor: what a person at the machine does. What the
//! screen shows and what it says is read from the session's published resource,
//! which is what any app of the session reads.

use super::*;

use std::path::{Path, PathBuf};
use std::time::Instant;

use tilo_session::ActionPlane;
use tilo_session::surfaces::Surface;

/// The resource the session publishes the lock as.
const LOCK: &str = "lock";

/// How long the lock screen may take to show itself: a whole app starts here.
const OPENING: Duration = Duration::from_secs(90);

/// One display's refresh, while waiting for the lock screen to draw.
const REFRESH: Duration = Duration::from_millis(16);

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

/// Type `word` into the lock screen's field, as the person in front of it
/// does, without pressing Enter: what the field holds stays the screen's.
pub(super) fn type_word(session: &mut Session, word: &str) {
    for keycode in spelled(word) {
        tap(session, keycode);
    }
}

/// Type `word` into the lock screen's field and press Enter, as the person in
/// front of it does.
pub(super) fn type_and_enter(session: &mut Session, word: &str) {
    type_word(session, word);
    tap(session, ENTER);
}

/// The lock screen the session starts: the app whose window asks the
/// compositor for the session lock.
pub(super) fn lock_screen() -> Surface {
    let binary = tilo_test_env::package_binary!("tilo-session").with_file_name("tilo-lock-screen");
    assert!(
        binary.exists(),
        "expected the lock screen at {}; build the workspace first",
        binary.display()
    );
    Surface::new("lock", binary).person("ada").lock()
}

/// A directory for the plane's socket that only this user reaches: the broker
/// refuses to serve one anybody else could open.
pub(super) fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the plane in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the plane's directory is this user's");
    root
}

/// The lock screen's binary, built beside the session's.
fn screen_binary() -> PathBuf {
    tilo_test_env::package_binary!("tilo-session").with_file_name("tilo-lock-screen")
}

/// A verifier the test answers, so the screen is seen saying it is checking
/// a password while the answer is still on its way.
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

    /// Let the check that is waiting answer `verdict`.
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

/// A session that starts its lock screen when it locks, serving its services
/// on `socket` and showing on `outputs`.
pub(super) fn lockable_session(
    socket: &Path,
    outputs: &[OutputConfig],
    verifier: std::sync::Arc<dyn tilo_session::unlock::PasswordVerifier>,
) -> Session {
    assert!(screen_binary().exists(), "the workspace is built first");
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            user: "ada".to_owned(),
            verifier,
            collab: false,
            action_plane: ActionPlane::At(socket.to_path_buf()),
            surfaces: vec![lock_screen()],
            ..Options::default()
        },
        outputs,
    )
    .expect("session")
}

/// What the displays show, as a grid each: what the person in front of them
/// sees.
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

/// Step the session until the lock screen's own frame is on `displays` and is
/// not changing any more: the screen itself, not the blank or dim a locked
/// output shows before the client's first frame arrives.
#[track_caller]
pub(super) fn until_drawn(session: &mut Session, displays: &[&str], was: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let deadline = Instant::now() + OPENING;
    let mut settled: Option<Vec<Vec<f64>>> = None;
    loop {
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
            if settled.as_ref() == now.as_ref() {
                return now.expect("the lock screen drew");
            }
            settled = now;
        } else {
            settled = None;
        }
        assert!(
            Instant::now() < deadline,
            "the lock screen never drew on {displays:?}"
        );
        session.step(REFRESH).expect("a turn of the session");
    }
}

/// Step the session until `displays` show what `was` showed again, settled:
/// the desk, back.
#[track_caller]
pub(super) fn until_back(session: &mut Session, displays: &[&str], was: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let deadline = Instant::now() + OPENING;
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
        publisher(&session, "toggle-overview").is_none(),
        "the locked session withholds the desktop's actions"
    );
    // What the screen shows is the session's own resource: the same reading
    // any app of the session reads.
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

    // A wrong password is refused, and the screen goes on showing it. The
    // screen says it is checking while the answer is on its way.
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
        publisher(&session, "toggle-overview").is_some(),
        "the unlocked session publishes the desktop's actions again"
    );
}

/// A session on two displays, the second smaller than the first: the lock
/// screen covers every one of them while the session is locked — the same
/// screen on both, of whatever size they are — and a press on either of them
/// reaches it. A password unlocks both.
#[test]
fn every_display_shows_the_lock_screen_while_the_session_is_locked() {
    const DISPLAYS: [&str; 2] = ["HEADLESS-1", "HEADLESS-2"];
    let _settled = ui::motion::testing::settled();
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
    // Every display shows the same screen, each scaled to the display it is
    // on: the picture is the same on both, of whatever size they are.
    let shown = apart(&locked[0], &locked[1]);
    assert!(
        shown < CLOSE,
        "every display shows the lock screen, but their pictures differ by {shown:.0} of 255"
    );

    // A press on the display the lock screen has no surface of its own on
    // brings up the screen's own keyboard: the screen is what the person
    // touches, on whichever display they touch.
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

/// The lock the power menu offers is the session's own: choosing it starts the
/// lock screen, as the session's own action does.
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
    let host = Rc::new(FixtureHost::new(QuickSettingsState::fixture()));
    let asked = Rc::new(RefCell::new(Vec::new()));
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            quick_settings: Some(QuickSettings::Lent(
                Rc::clone(&host) as Rc<dyn tilo_session::PanelHost>
            )),
            services: vec![Box::new(PlayerService {
                asked: Rc::clone(&asked),
            })],
            user: "ada".to_owned(),
            verifier: std::sync::Arc::new(Sesame),
            collab: false,
            action_plane: ActionPlane::At(home.path().join("collab.sock")),
            surfaces: vec![lock_screen()],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    // What plays reaches the app the panel is, once the session lends it its
    // readings: the session reads them from the same place.
    host.update(|state| {
        state.now_playing = Some(tilo_quick_settings::NowPlaying::fixture());
        state.status.battery = Some(tilo_power_mgmt::BatterySnapshot {
            present: true,
            percent: 62,
            state: tilo_power_mgmt::BatteryState::Charging,
        });
    });
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
    host.update(|state| state.now_playing = None);
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

/// How far apart two pictures of a screen are, of 255: the most one cell of
/// the grid they are compared on differs by.
pub(super) fn apart(one: &[f64], other: &[f64]) -> f64 {
    one.iter()
        .zip(other)
        .map(|(here, there)| (here - there).abs())
        .fold(0.0_f64, f64::max)
}

/// How much two pictures of the same screen may differ and still be the
/// same screen: the grid's cells may be resampled differently on displays of
/// different sizes.
pub(super) const CLOSE: f64 = 6.0;

/// What a screenshot shows, as the mean brightness of each cell of a grid
/// laid over it: a screen filling two displays of different sizes fills the
/// same part of each.
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
