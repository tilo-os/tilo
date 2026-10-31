//! The lock screen on the session's sheets: what the person sees while
//! the session is locked, on every display, and how a password brings the
//! desk back.

use super::*;

#[test]
fn the_lock_action_locks_the_session_behind_the_lock_screen_which_a_password_unlocks() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            user: "ada".to_owned(),
            verifier: std::sync::Arc::new(Sesame),
            collab: false,
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
    let colours: std::collections::BTreeSet<&[u8]> = locked.rgba.chunks(4).collect();
    assert!(
        colours.len() > 2,
        "the lock screen draws its clock and prompt, not only the dim"
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

/// A session on two displays, the second smaller than the first: the lock
/// screen covers every one of them while the session is locked, and is gone
/// from all of them once it is unlocked.
#[test]
fn every_display_shows_the_lock_screen_while_the_session_is_locked() {
    const DISPLAYS: [&str; 2] = ["HEADLESS-1", "HEADLESS-2"];
    let _settled = ui::motion::testing::settled();
    let mut external = OutputConfig::new("HEADLESS-2", 1024, 768);
    external.position = (WIDTH, 0);
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
    let _client = mapped(&mut session, "org.example.Terminal");
    for display in DISPLAYS {
        settled_display(&mut session, display);
    }
    let desk: Vec<tilo_compositor::Screenshot> = DISPLAYS
        .iter()
        .map(|display| {
            session
                .compositor()
                .screenshot(display)
                .expect("screenshot")
        })
        .collect();

    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    for _ in 0..10 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert!(session.compositor().locked());
    let locked: Vec<tilo_compositor::Screenshot> = DISPLAYS
        .iter()
        .map(|display| {
            session
                .compositor()
                .screenshot(display)
                .expect("screenshot")
        })
        .collect();

    for (index, display) in DISPLAYS.iter().enumerate() {
        let shot = &locked[index];
        assert_ne!(
            shot.rgba, desk[index].rgba,
            "the lock screen covers {display}"
        );
        let colours: std::collections::BTreeSet<&[u8]> = shot.rgba.chunks(4).collect();
        assert!(
            colours.len() > 2,
            "{display} shows the lock screen's clock and prompt, not only its backdrop"
        );
    }
    // Every display shows the same screen, each scaled to the display it is
    // on: the picture is the same on both, of whatever size they are.
    let shown = apart(&picture(&locked[0]), &picture(&locked[1]));
    assert!(
        shown < CLOSE,
        "every display shows the lock screen, but their pictures differ by {shown:.0} of 255"
    );

    let unlock = LockEffect::UNLOCK.id;
    let screen = publisher(&session, unlock).expect("the lock screen publishes unlocking");
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
    for (index, display) in DISPLAYS.iter().enumerate() {
        settled_display(&mut session, display);
        let after = session
            .compositor()
            .screenshot(display)
            .expect("screenshot");
        let back = apart(&picture(&after), &picture(&desk[index]));
        let was_locked = apart(&picture(&after), &picture(&locked[index]));
        assert!(
            back * 4.0 < was_locked && back < 24.0,
            "unlocking clears the lock screen from {display}: what it shows is {back:.0} of 255 \
             from the desk it showed before the lock, and {was_locked:.0} from the lock screen"
        );
    }
}

/// How far apart two pictures of a screen are, of 255: the most one cell of
/// the grid they are compared on differs by.
fn apart(one: &[f64], other: &[f64]) -> f64 {
    one.iter()
        .zip(other)
        .map(|(here, there)| (here - there).abs())
        .fold(0.0_f64, f64::max)
}

/// How much two pictures of the same screen may differ and still be the
/// same screen: the grid's cells may be resampled differently on displays of
/// different sizes.
const CLOSE: f64 = 6.0;

/// Step the session until `display` shows the same thing twice running:
/// what it will show has been drawn, animation and all.
fn settled_display(session: &mut Session, display: &str) {
    const STEPS: usize = 120;
    let mut was = session
        .compositor()
        .screenshot(display)
        .expect("screenshot")
        .rgba;
    for _ in 0..STEPS {
        session.step(Duration::from_millis(16)).expect("step");
        let now = session
            .compositor()
            .screenshot(display)
            .expect("screenshot")
            .rgba;
        if now == was {
            return;
        }
        was = now;
    }
    panic!("{display} never stopped changing");
}

/// What a screenshot shows, as the mean brightness of each cell of a grid
/// laid over it: a screen filling two displays of different sizes fills the
/// same part of each.
fn picture(shot: &tilo_compositor::Screenshot) -> Vec<f64> {
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
