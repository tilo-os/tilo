//! The person's settings reaching the compositor, and what the compositor
//! reports reaching Settings: their arrangement of the displays applied and
//! kept, a change that could darken a screen waiting for their answer and
//! being put back when it does not come, and the keyboard typing in the
//! input source they chose.

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

use settings_store::hardware::{
    DisplayConfig, DisplaySet, DisplayTransaction, DisplayTransactionState, OutputConfig,
    OutputMode, OutputTransform,
};
use tilo_compositor::OutputConfig as CompositorOutput;
use tilo_session::{Options, Session, SessionConfig};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// A session with `count` displays side by side.
fn session(count: usize) -> Session {
    let outputs: Vec<CompositorOutput> = (1..=count)
        .map(|index| {
            CompositorOutput::new(format!("HEADLESS-{index}"), WIDTH, HEIGHT)
                .at(WIDTH * i32::try_from(index - 1).unwrap(), 0)
        })
        .collect();
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            ..Options::default()
        },
        &outputs,
    )
    .expect("session")
}

/// The monitors attached now, as Settings sees them, and the set they make.
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

/// Whether the display `connector` is on.
fn lit(session: &mut Session, connector: &str) -> bool {
    session
        .compositor()
        .outputs()
        .into_iter()
        .find(|output| output.name == connector)
        .is_none_or(|output| output.enabled)
}

/// The scale clients render on the display `connector` at; `None` when no
/// display says.
fn scale(session: &mut Session, connector: &str) -> Option<f64> {
    session
        .compositor()
        .display_snapshot()
        .and_then(|snapshot| {
            snapshot
                .outputs
                .iter()
                .find(|output| output.descriptor.connector == connector)
                .map(|output| output.scale)
        })
}

/// Let the session run until `done` holds.
fn until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    for _ in 0..500 {
        session.step(Duration::from_millis(2)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("condition not reached");
}

#[test]
fn a_scale_chosen_in_settings_reaches_the_displays_and_is_kept() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session(1);
    let (connectors, set) = attached(&mut session);
    let connector = connectors[0].clone();

    let mut baseline = DisplayConfig::default();
    baseline
        .outputs
        .insert(connector.clone(), OutputConfig::default());
    baseline.save_for(&set).expect("the arrangement so far");

    let mut candidate = DisplayConfig::default();
    candidate.outputs.insert(
        connector.clone(),
        OutputConfig {
            scale: 1.5,
            ..OutputConfig::default()
        },
    );
    DisplayTransaction::request(set.clone(), candidate).expect("a change staged");

    until(&mut session, |session| {
        scale(session, &connector) == Some(1.5)
    });
    assert_eq!(
        DisplayTransaction::load().map(|transaction| transaction.state),
        Some(DisplayTransactionState::Confirmed),
        "a scale cannot darken a screen, so it stays at once"
    );
    assert!(
        (DisplayConfig::load_for(&set).outputs[&connector].scale - 1.5).abs() < f64::EPSILON,
        "the choice outlasts this session"
    );
}

#[test]
fn a_change_that_could_dark_a_screen_waits_and_is_put_back() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session(2);
    let (connectors, set) = attached(&mut session);
    let (left, right) = (connectors[0].clone(), connectors[1].clone());

    let mut candidate = DisplayConfig::default();
    candidate.outputs.insert(
        left.clone(),
        OutputConfig {
            x: 0,
            y: 0,
            ..OutputConfig::default()
        },
    );
    candidate.outputs.insert(
        right.clone(),
        OutputConfig {
            enabled: false,
            ..OutputConfig::default()
        },
    );
    DisplayTransaction::request(set, candidate).expect("a change staged");

    until(&mut session, |session| !lit(session, &right));
    let mut transaction = DisplayTransaction::load().expect("the change is staged");
    assert_eq!(
        transaction.state,
        DisplayTransactionState::AwaitingConfirmation,
        "turning a display off waits for the person"
    );
    let deadline = transaction
        .deadline_unix_ms
        .expect("a deadline to answer by");
    assert!(
        deadline.abs_diff(unix_now_ms()) <= DisplayTransaction::CONFIRM_TIMEOUT_MS,
        "the answer has fifteen seconds"
    );

    // The person does not answer; the deadline passes.
    transaction.deadline_unix_ms = Some(unix_now_ms());
    transaction.save().expect("the deadline moved");
    until(&mut session, |session| lit(session, &right));
    assert_eq!(
        DisplayTransaction::load().map(|transaction| transaction.state),
        Some(DisplayTransactionState::Reverted),
        "the previous arrangement is back"
    );
    assert!(
        lit(&mut session, &left),
        "the screen in use never went dark"
    );
}

#[test]
fn the_mode_scale_and_rotation_chosen_in_settings_are_back_after_a_restart() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut first = session(1);
    let (connectors, set) = attached(&mut first);
    let connector = connectors[0].clone();
    let chosen_mode = OutputMode {
        width: 800,
        height: 600,
        refresh_mhz: 75_000,
    };
    let mut chosen = DisplayConfig::default();
    chosen.outputs.insert(
        connector.clone(),
        OutputConfig {
            scale: 1.25,
            transform: OutputTransform::Rotate90,
            mode: Some(chosen_mode),
            ..OutputConfig::default()
        },
    );
    chosen.save_for(&set).expect("the arrangement is kept");
    drop(first);

    // The next session starts from nothing and reads the same choices.
    let mut restarted = session(1);
    until(&mut restarted, |session| {
        scale(session, &connector) == Some(1.25)
    });
    let shown = restarted
        .compositor()
        .display_snapshot()
        .and_then(|snapshot| {
            snapshot
                .outputs
                .iter()
                .find(|output| output.descriptor.connector == connector)
                .map(|output| output.current_mode)
        })
        .expect("the display is published");
    assert_eq!(shown, chosen_mode, "the mode and refresh rate hold");
    let rect = restarted
        .compositor()
        .outputs()
        .into_iter()
        .find(|output| output.name == connector)
        .and_then(|output| output.rect)
        .expect("the display is on");
    assert_eq!(
        (rect.2, rect.3),
        (480, 640),
        "800x600 at 1.25 turned on its side is 480x640: {rect:?}"
    );
}

#[test]
fn the_keyboard_types_in_the_next_input_source_and_keeps_the_choice() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    settings_store::update_section(
        settings_store::hardware::INPUT_SECTION,
        serde_json::Map::from_iter([
            (
                "keyboard_layout".to_owned(),
                serde_json::Value::String("us".into()),
            ),
            (
                "keyboard_layouts".to_owned(),
                serde_json::json!(["us", "de"]),
            ),
        ]),
    )
    .expect("the person's sources are kept");
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            config: SessionConfig::load(),
            ..Options::default()
        },
        &[CompositorOutput::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    assert_eq!(
        session.active_input_source(),
        "us",
        "the person types in the source they chose: {:?}",
        session.input_sources()
    );
    assert_eq!(
        session.compositor().keyboard_layout().as_deref(),
        Some("English (US)")
    );

    session
        .invoke("session:input", "next-source", &serde_json::Map::new())
        .expect("the session publishes the source switch");
    until(&mut session, |session| {
        session.active_input_source() == "de"
    });
    assert_eq!(
        session.compositor().keyboard_layout().as_deref(),
        Some("German"),
        "the seat types in German now"
    );
    assert_eq!(
        settings_store::load_section(settings_store::hardware::INPUT_SECTION)
            .get("keyboard_layout")
            .cloned(),
        Some(serde_json::Value::String("de".into())),
        "the choice survives the session"
    );
}

/// Milliseconds since the Unix epoch.
fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}
