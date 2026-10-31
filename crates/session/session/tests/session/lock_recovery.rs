//! The lock screen when the app that draws it goes away: the session puts it
//! up again on every display, unlocking still works, and a screen that will
//! not stay up lifts the lock rather than leave the machine locked with
//! nothing able to unlock it.

use super::lock::{CLOSE, apart, picture, settled_display};
use super::*;

/// The session locked, with the desk and the lock screen as it showed them
/// before anything went wrong.
struct LockedSession {
    session: Session,
    /// The window the desk showed, kept open for the whole test.
    _client: TestClient,
    desk: Vec<tilo_compositor::Screenshot>,
    locked: Vec<tilo_compositor::Screenshot>,
}

const DISPLAYS: [&str; 2] = ["HEADLESS-1", "HEADLESS-2"];

/// A session on two displays, its desk drawn and then locked behind the lock
/// screen on both of them.
fn locked_session() -> LockedSession {
    let external = OutputConfig::new("HEADLESS-2", 1024, 768).at(WIDTH, 0);
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            user: "ada".to_owned(),
            verifier: std::sync::Arc::new(Sesame),
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT), external],
    )
    .expect("session");
    let client = mapped(&mut session, "org.example.Terminal");
    for display in DISPLAYS {
        settled_display(&mut session, display);
    }
    let desk = shots(&mut session);
    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    step_until(&mut session, |session| {
        session.compositor().lock_screen_drawn()
    });
    for display in DISPLAYS {
        settled_display(&mut session, display);
    }
    let locked = shots(&mut session);
    LockedSession {
        session,
        _client: client,
        desk,
        locked,
    }
}

fn shots(session: &mut Session) -> Vec<tilo_compositor::Screenshot> {
    DISPLAYS
        .iter()
        .map(|display| {
            session
                .compositor()
                .screenshot(display)
                .expect("screenshot")
        })
        .collect()
}

/// Step the session until `done` holds. A locked session's checks are timed
/// (it gives the lock screen a moment to come back), so the steps give it
/// the time that passes rather than nothing at all.
#[track_caller]
fn step_until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !done(session) {
        session.step(Duration::from_millis(50)).expect("step");
        assert!(
            std::time::Instant::now() < deadline,
            "the session never got there"
        );
    }
}

/// Every display shows the lock screen again, and none of them shows the desk
/// that was there before the lock.
fn assert_the_lock_screen_covers(session: &mut Session, desk: &[tilo_compositor::Screenshot]) {
    for (index, display) in DISPLAYS.iter().enumerate() {
        settled_display(session, display);
        let shot = session
            .compositor()
            .screenshot(display)
            .expect("screenshot");
        let desk_showing = apart(&picture(&shot), &picture(&desk[index]));
        assert!(
            desk_showing > 24.0,
            "{display} shows the desk it showed before the lock, not the lock screen: \
             {desk_showing:.0} of 255 apart"
        );
        let colours: std::collections::BTreeSet<&[u8]> = shot.rgba.chunks(4).collect();
        assert!(
            colours.len() > 2,
            "{display} draws the lock screen's clock and prompt, not only its backdrop"
        );
    }
}

#[test]
fn a_lock_screen_whose_app_goes_comes_back_and_still_unlocks() {
    let _settled = ui::motion::testing::settled();
    let LockedSession {
        mut session,
        _client,
        desk,
        locked,
    } = locked_session();
    for (index, display) in DISPLAYS.iter().enumerate() {
        assert_ne!(
            locked[index].rgba, desk[index].rgba,
            "the lock screen hides the desk on {display}"
        );
    }

    // The app drawing the lock screen goes away, as it says so when nothing
    // of it is on the screen any more.
    let presentation = session
        .lock_screen()
        .expect("the lock screen")
        .presentation()
        .clone();
    presentation.gone();
    assert!(
        !session.compositor().lock_screen_drawn(),
        "nothing of the lock screen is on the screen once its app is gone"
    );

    // The session is still locked, and puts its screen up again on every
    // display: the person is not left looking at their desk.
    step_until(&mut session, |session| {
        session.compositor().lock_screen_drawn()
    });
    assert!(
        session.compositor().locked(),
        "the session is still locked when its screen came back"
    );
    assert!(
        session
            .lock_screen()
            .expect("the lock screen")
            .presentation()
            .is_open(),
        "the lock screen is up again"
    );
    assert_the_lock_screen_covers(&mut session, &desk);
    for (index, display) in DISPLAYS.iter().enumerate() {
        let shot = session
            .compositor()
            .screenshot(display)
            .expect("screenshot");
        let same = apart(&picture(&shot), &picture(&locked[index]));
        assert!(
            same < CLOSE,
            "{display} shows the lock screen it showed before, not another one: \
             {same:.0} of 255 apart"
        );
    }

    // Typing the password into the screen that came back unlocks.
    let unlock = LockEffect::UNLOCK.id;
    let screen = publisher(&session, unlock).expect("the lock screen offers unlocking again");
    session
        .invoke(
            &screen,
            LockEffect::TYPE_PASSWORD.id,
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
        "the lock screen is gone once it unlocked"
    );
    for (index, display) in DISPLAYS.iter().enumerate() {
        settled_display(&mut session, display);
        let after = session
            .compositor()
            .screenshot(display)
            .expect("screenshot");
        let back = apart(&picture(&after), &picture(&desk[index]));
        assert!(
            back < 24.0,
            "unlocking brings the desk back on {display}: {back:.0} of 255 apart"
        );
    }
}

#[test]
fn a_lock_screen_that_will_not_stay_up_lifts_the_lock() {
    let _settled = ui::motion::testing::settled();
    let LockedSession {
        mut session,
        _client,
        desk,
        locked,
    } = locked_session();
    let presentation = session
        .lock_screen()
        .expect("the lock screen")
        .presentation()
        .clone();
    // The app drawing the lock screen goes away as soon as it is up, every
    // time: a screen that cannot stay on the screen.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while session.compositor().locked() {
        presentation.gone();
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
    for (index, display) in DISPLAYS.iter().enumerate() {
        settled_display(&mut session, display);
        let after = session
            .compositor()
            .screenshot(display)
            .expect("screenshot");
        let was = apart(&picture(&after), &picture(&locked[index]));
        assert!(
            was > 24.0,
            "{display} shows the desk again, not the lock screen: {was:.0} of 255 apart"
        );
        let back = apart(&picture(&after), &picture(&desk[index]));
        assert!(
            back < 24.0,
            "{display} shows the desk it showed before the lock: {back:.0} of 255 apart"
        );
    }
}
