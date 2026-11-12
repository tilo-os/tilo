//! The display the person makes primary in Settings: across two displays their new windows and the
//! sheets they open go to that one rather than to the one on the left, and the choice is theirs
//! again after a restart.

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

use std::time::Duration;

use crate::common::window::Client;
use settings_store::hardware::{
    DisplayConfig, DisplaySet, DisplayTransaction, OutputConfig as StoredOutput,
};
use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;

const WIDTH: i32 = 1000;
const HEIGHT: i32 = 700;

/// Let the session run until `done` holds.
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    crate::common::wait_for(session, "what the test asked of the session", |session| {
        done(session).then_some(())
    });
}

/// A session whose two displays sit side by side, `HEADLESS-1` on the left and `HEADLESS-2` on the
/// right.
fn docked() -> Session {
    let outputs: Vec<OutputConfig> = (1..=2)
        .map(|index| {
            OutputConfig::new(format!("HEADLESS-{index}"), WIDTH, HEIGHT).at(WIDTH * (index - 1), 0)
        })
        .collect();
    let mut session = crate::common::headless_on(
        &outputs,
        Options {
            collab: false,
            ..Options::default()
        },
    );
    session.step(Duration::from_millis(1)).expect("step");
    session
}

fn client(session: &mut Session) -> Client {
    let mut client = Client::connect(session.compositor());
    client.pump(session, |_, seen| {
        seen.wm_base.is_some() && seen.shm.is_some()
    });
    client
}

/// Open a window of `app_id` and wait until it is mapped.
fn map(session: &mut Session, client: &mut Client, app_id: &str) {
    client.map_plain(session, app_id, (300, 200));
}

/// Which display (0 left, 1 right) the window of `app` opened on; `None` while it is out of sight.
fn display_of(session: &mut Session, app: &str) -> Option<i32> {
    let (_, (x, y, _, _)) = session
        .compositor()
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))?;
    (x >= 0 && y >= 0).then(|| x.div_euclid(WIDTH))
}

/// The rectangle of the right display, `HEADLESS-2` .
fn right() -> (i32, i32, u32, u32) {
    (
        WIDTH,
        0,
        u32::try_from(WIDTH).unwrap(),
        u32::try_from(HEIGHT).unwrap(),
    )
}

/// The displays attached now, as Settings sees them, and the set they make.
fn attached(session: &mut Session) -> (Vec<String>, DisplaySet) {
    let snapshot = session
        .compositor()
        .display_snapshot()
        .cloned()
        .expect("the displays are published");
    let descriptors: Vec<_> = snapshot
        .outputs
        .iter()
        .map(|output| output.descriptor.clone())
        .collect();
    let connectors = descriptors
        .iter()
        .map(|descriptor| descriptor.connector.clone())
        .collect();
    (connectors, DisplaySet::of_descriptors(&descriptors))
}

/// Make `primary` the display the person chose, the way Settings does: the arrangement the displays
/// attached now already have, with that one made primary, asked for as the display change the
/// person made. Changing only the primary leaves no screen dark, so the session keeps it at once
/// instead of waiting for an answer.
fn make_primary(session: &mut Session, primary: &str) {
    let (connectors, set) = attached(session);
    let mut arrangement = DisplayConfig::default();
    for (index, connector) in connectors.iter().enumerate() {
        arrangement.outputs.insert(
            connector.clone(),
            StoredOutput {
                x: WIDTH * i32::try_from(index).expect("a handful of displays"),
                ..StoredOutput::default()
            },
        );
    }
    arrangement.save_for(&set).expect("the arrangement so far");
    let mut candidate = arrangement.clone();
    for (connector, output) in &mut candidate.outputs {
        output.primary = connector == primary;
    }
    DisplayTransaction::request(set, candidate).expect("a display change staged");
}

/// The display on the right of the two, `HEADLESS-2` , as the desk numbers its displays: the one
/// the person makes primary here.
fn right_display(session: &Session) -> Option<u32> {
    session.handle().displays().get(1).copied()
}

/// Whether the desk has been told that the right display is the primary one; until it has, the
/// first display stands in and the primary it names is the left one.
fn primary_reached(session: &Session) -> bool {
    let right = right_display(session);
    right.is_some() && session.handle().primary_display() == right
}

/// The display the sheet would open on right now.
fn sheet_place(session: &Session) -> Option<(i32, i32, u32, u32)> {
    session
        .power_menu()
        .expect("power sheet")
        .presentation()
        .place()
}

#[test]
fn new_windows_and_sheets_open_on_the_primary_the_person_chose() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);

    make_primary(&mut session, "HEADLESS-2");
    until(&mut session, |session| primary_reached(session));

    // The sheet opens on the display the person chose, before anything is open on either of them.
    let power = session.power_menu().expect("power sheet");
    power.presentation().open(false);
    client.pump(&mut session, |session, _| {
        sheet_place(session) == Some(right())
    });
    assert_eq!(
        sheet_place(&session),
        Some(right()),
        "the sheet opens on the primary display"
    );
    session
        .power_menu()
        .expect("power sheet")
        .presentation()
        .close();
    client.pump(&mut session, |session, _| {
        !session
            .power_menu()
            .expect("power sheet")
            .presentation()
            .is_open()
    });

    // So does a window the person opens.
    map(&mut session, &mut client, "org.example.Primary");
    assert_eq!(
        display_of(&mut session, "org.example.Primary"),
        Some(1),
        "a new window opens on the primary display, not the one on the left"
    );
}

#[test]
fn the_display_the_person_made_primary_is_theirs_again_after_a_restart() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    {
        let mut session = docked();
        make_primary(&mut session, "HEADLESS-2");
        until(&mut session, |session| primary_reached(session));
        let (_, set) = attached(&mut session);
        assert_eq!(
            DisplayConfig::load_for(&set)
                .outputs
                .get("HEADLESS-2")
                .map(|output| output.primary),
            Some(true),
            "the choice is kept for the next session"
        );
    }

    let mut session = docked();
    let mut client = client(&mut session);
    until(&mut session, |session| primary_reached(session));
    let power = session.power_menu().expect("power sheet");
    power.presentation().open(false);
    client.pump(&mut session, |session, _| {
        sheet_place(session) == Some(right())
    });
    assert_eq!(
        sheet_place(&session),
        Some(right()),
        "the sheet opens on the display the person chose, in a new session"
    );
    map(&mut session, &mut client, "org.example.Again");
    assert_eq!(
        display_of(&mut session, "org.example.Again"),
        Some(1),
        "and so does a window in a new session"
    );
}
