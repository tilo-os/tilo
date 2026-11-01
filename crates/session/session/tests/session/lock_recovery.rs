//! The lock screen when the process that draws it goes away: the session is
//! still locked and starts its screen again, unlocking still works, and a
//! screen that will not stay up lifts the lock rather than leave the machine
//! locked with nothing able to unlock it.

use super::lock::{
    apart, lockable_session, pictures, private_home, type_and_enter, until_back, until_drawn,
};
use super::*;

const DISPLAYS: [&str; 2] = ["HEADLESS-1", "HEADLESS-2"];

/// The session locked, with the desk it showed before anything went wrong.
struct LockedSession {
    session: Session,
    /// The window the desk showed, kept open for the whole test.
    _client: TestClient,
    /// The session serves its services in this directory: the lock screen
    /// reaches them by it.
    _home: tempfile::TempDir,
    desk: Vec<Vec<f64>>,
}

/// A session on two displays, its desk drawn and then locked behind the lock
/// screen on both of them.
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

    // The process drawing the lock screen goes away, as it does when it
    // crashes.
    session.stop_surface("lock");
    assert!(
        !session.surfaces().running("lock"),
        "the lock screen's process is gone"
    );

    // The session is still locked, and starts its screen again: the person is
    // not left looking at their desk, and not let into it either.
    pump(&mut session, &mut [], |session, _| {
        session.surfaces().running("lock")
    });
    assert!(
        session.compositor().locked(),
        "the session is still locked when its screen was started again"
    );
    let again = until_drawn(&mut session, &DISPLAYS, &desk);
    for (index, display) in DISPLAYS.iter().enumerate() {
        // The screen says the time, so it is no copy of the one before: what
        // the person gets is a lock screen over their desk, drawing what it
        // shows.
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
fn a_lock_screen_that_will_not_stay_up_lifts_the_lock() {
    let LockedSession {
        mut session,
        _client,
        _home,
        desk,
        ..
    } = locked_session();

    // The process goes away as soon as it is up, every time: a screen that
    // cannot stay on the screen.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while session.compositor().locked() {
        session.stop_surface("lock");
        session.step(Duration::from_millis(50)).expect("step");
        assert!(
            std::time::Instant::now() < deadline,
            "the session stayed locked behind a lock screen that never stayed up"
        );
    }
    assert!(
        !session.compositor().session_locked(),
        "the lock is lifted rather than left with nothing to unlock it"
    );
    // And the desk is back, which is what the person gets instead.
    until_back(&mut session, &DISPLAYS, &desk);
}
