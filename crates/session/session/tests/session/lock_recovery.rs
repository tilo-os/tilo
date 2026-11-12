//! The lock screen when the process that draws it goes away: the session is still locked and starts
//! its screen again, unlocking still works, and one that will not stay up is drawn by the session
//! itself rather than lift the lock, which only the right password lifts.

use super::lock::{
    apart, lock_screen, lockable_session, pictures, private_home,
    take_back_the_notice_of_the_locks_own_view, type_and_enter, until_back, until_drawn,
};
use super::*;

use tilo_session::ActionPlane;

const DISPLAYS: [&str; 2] = ["HEADLESS-1", "HEADLESS-2"];

/// The session locked, with the desk it showed before anything went wrong.
struct LockedSession {
    session: Session,
    /// The window the desk showed, kept open for the whole test.
    _client: TestClient,
    /// The session serves its services in this directory: the lock screen reaches them by it.
    _home: tempfile::TempDir,
    desk: Vec<Vec<f64>>,
}

/// A session on two displays, its desk drawn and then locked behind the lock screen on both of
/// them.
fn locked_session() -> LockedSession {
    let _settled = ui::motion::testing::settled();
    let external = OutputConfig::new("HEADLESS-2", 1024, 768).at(WIDTH, 0);
    let home = private_home();
    let mut session = lockable_session(
        &home.path().join("collab.sock"),
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT), external],
        std::sync::Arc::new(Sesame),
    );
    let client = mapped(&mut session, "org.example.Terminal");
    let desk = pictures(&mut session, &DISPLAYS);
    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    pump(&mut session, &mut [], |session, _| {
        session.compositor().locked()
    });
    until_drawn(&mut session, &DISPLAYS, &desk);
    LockedSession {
        session,
        _client: client,
        _home: home,
        desk,
    }
}

#[test]
fn a_lock_screen_whose_process_goes_comes_back_and_still_unlocks() {
    let LockedSession {
        mut session,
        _client,
        _home,
        desk,
    } = locked_session();

    // The process drawing the lock screen goes away, as it does when it crashes.
    session.stop_surface("lock");
    assert!(
        !session.surfaces().running("lock"),
        "the lock screen's process is gone"
    );

    // The session is still locked, and starts its screen again: the person is not left looking at
    // their desk, and not let into it either.
    pump(&mut session, &mut [], |session, _| {
        session.surfaces().running("lock")
    });
    assert!(
        session.compositor().locked(),
        "the session is still locked when its screen was started again"
    );
    let again = until_drawn(&mut session, &DISPLAYS, &desk);
    for (index, display) in DISPLAYS.iter().enumerate() {
        // The screen says the time, so it is no copy of the one before: what the person gets is a
        // lock screen over their desk, drawing what it shows.
        let over_desk = apart(&again[index], &desk[index]);
        assert!(
            over_desk > 24.0,
            "{display} shows the lock screen again, not the desk it covers: \
             {over_desk:.0} of 255 from the desk"
        );
        let colours: std::collections::BTreeSet<Vec<u8>> = session
            .compositor()
            .screenshot(display)
            .expect("screenshot")
            .rgba
            .chunks(4)
            .map(<[u8]>::to_vec)
            .collect();
        assert!(
            colours.len() > 2,
            "{display} shows the screen's clock and prompt, not only the dim"
        );
    }

    // Typing the password into the screen that came back unlocks.
    type_and_enter(&mut session, "sesame");
    pump(&mut session, &mut [], |session, _| {
        !session.compositor().locked()
    });
    pump(&mut session, &mut [], |session, _| {
        !session.surfaces().running("lock")
    });
    until_back(&mut session, &DISPLAYS, &desk);
}

#[test]
fn a_lock_screen_that_will_not_stay_up_is_drawn_by_the_session() {
    let LockedSession {
        mut session,
        _client,
        _home,
        desk,
        ..
    } = locked_session();

    // The process goes away as soon as it is up, every time: a screen that cannot stay on the
    // screen. The session keeps starting it, and then draws the lock itself: the lock never lifts.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        session.stop_surface("lock");
        session.step(Duration::from_millis(16)).expect("step");
        assert!(
            std::time::Instant::now() < deadline,
            "the lock screen's app never stopped being the one that shows the lock"
        );
        let gone = !session.surfaces().running("lock");
        if gone && apart(&pictures(&mut session, &DISPLAYS)[0], &desk[0]) > 24.0 {
            break;
        }
    }
    assert!(
        session.compositor().session_locked(),
        "the lock holds behind the screen that would not stay up"
    );
    assert!(
        session.compositor().lock_drawn(),
        "the lock the session draws is what the displays show: nothing waits on the app's frame, and a capture of it is the placeholder"
    );
    assert!(
        publisher(&session, "overview").is_none(),
        "the locked session withholds the desktop's actions"
    );

    // What the session draws is the lock screen: a wrong password keeps it locked and says so, and
    // the right one lifts it.
    type_and_enter(&mut session, "wrong");
    pump(&mut session, &mut [], |session, _| {
        let reading = session
            .read("lock")
            .expect("the session publishes the lock");
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
    // The lock the person saw was the session's own drawing, and the desk they come back to says
    // so until they take that back.
    take_back_the_notice_of_the_locks_own_view(&mut session);
    // And the desk is back, which is what the person gets once they unlock.
    until_back(&mut session, &DISPLAYS, &desk);
}

/// The login's record says the session before this one was left locked: the one that starts comes
/// back locked, and only an ordered end publishes a state the next start recovers.
#[test]
fn a_session_left_locked_comes_back_locked() {
    const DISPLAY: &str = "HEADLESS-1";
    let _settled = ui::motion::testing::settled();
    let home = private_home();
    let request = home.path().join("logout-request");
    // What a session that was killed leaves behind: the armed record, never published.
    let killed = tilo_session::recovery::Recovery::begin(&request).expect("a record");
    drop(killed);

    let recovered = |home: &tempfile::TempDir| {
        let recovery = tilo_session::recovery::Recovery::begin(&request).expect("a record");
        assert!(
            recovery.starts_locked(),
            "a session left locked is recovered locked"
        );
        crate::common::headless_on(
            &[OutputConfig::new(DISPLAY, WIDTH, HEIGHT)],
            Options {
                user: "ada".to_owned(),
                verifier: std::sync::Arc::new(Sesame),
                collab: false,
                action_plane: ActionPlane::At(home.path().join("collab.sock")),
                surfaces: vec![lock_screen()],
                recovery: Some(recovery),
                ..Options::default()
            },
        )
    };

    let mut session = recovered(&home);
    assert!(
        session.compositor().session_locked(),
        "the session starts with the lock it was left with"
    );
    assert!(
        !session.surfaces().running("lock"),
        "the lock screen has not started before the session steps"
    );
    let before = pictures(&mut session, &[DISPLAY]);
    pump(&mut session, &mut [], |session, _| {
        session.surfaces().running("lock")
    });
    // The person sees the lock screen, and nothing of the desk, until they unlock it.
    assert!(
        publisher(&session, "overview").is_none(),
        "the recovered lock withholds the desktop's actions"
    );
    until_drawn(&mut session, &[DISPLAY], &before);
    // It ends as it was: the next start still comes back locked.
    session.run(|_| true).expect("an ordered end");
    drop(session);
    let again = tilo_session::recovery::Recovery::begin(&request).expect("a record");
    assert!(
        again.starts_locked(),
        "a session that ended locked leaves the next one locked"
    );
    drop(again);

    // The next one unlocks with the right password, and its ordered end is what the one after it
    // recovers.
    let mut session = recovered(&home);
    let before = pictures(&mut session, &[DISPLAY]);
    pump(&mut session, &mut [], |session, _| {
        session.surfaces().running("lock")
    });
    until_drawn(&mut session, &[DISPLAY], &before);
    type_and_enter(&mut session, "sesame");
    pump(&mut session, &mut [], |session, _| {
        !session.compositor().locked()
    });
    session.run(|_| true).expect("an ordered end");
    drop(session);
    let after = tilo_session::recovery::Recovery::begin(&request).expect("a record");
    assert!(
        !after.starts_locked(),
        "a session that ended unlocked leaves the next one unlocked"
    );
}
