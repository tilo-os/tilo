//! The readout that answers a media or mode key: what it says, where it stands, and what it leaves
//! alone.
//! Key presses go through the seat the way a person's do, and what the readout says is read from
//! the session's accessible labels — the same words a screen reader gets.

use super::*;

/// Step the session until its readout says `words` , and what it says there.
/// The readout is up for a moment of its own, and a machine running a whole suite at once can spend
/// that moment on the turn that raised it: the turns that wait for it and the read of what it says
/// happen at one reading of the clock, so the readout the key raised is the one read.
#[track_caller]
pub(super) fn readout_saying(session: &mut Session, words: &str) -> String {
    ui::time::testing::with_monotonic_time(ui::time::now(), || {
        pump(session, &mut [], |session, _| {
            session
                .accessible_labels()
                .iter()
                .any(|label| label.contains(words))
        });
    });
    session
        .accessible_labels()
        .iter()
        .find(|label| label.contains(words))
        .cloned()
        .unwrap_or_default()
}

#[test]
fn a_volume_key_names_the_output_it_changed() {
    let _settled = ui::motion::testing::settled();
    let asked = Rc::new(RefCell::new(Vec::new()));
    let headphones = Audio::speakers(Rc::clone(&asked));
    {
        let mut sound = headphones.sound.borrow_mut();
        sound.volume = Some(0.65);
        sound.muted = false;
        sound.outputs = vec![tilo_audio_service::sound::Device {
            node: 40,
            name: "bluez_output.AC_80_0A_0B_0C_0D.1".to_owned(),
            label: "Studio headphones".to_owned(),
            chosen: true,
            ..tilo_audio_service::sound::Device::default()
        }];
    }
    let mut session = crate::common::headless(Options {
        services: vec![Box::new(headphones)],
        collab: false,
        ..Options::default()
    });
    tap(&mut session, keys::VOLUME_UP);
    assert_eq!(
        readout_saying(&mut session, "Studio headphones"),
        "Studio headphones, 65%",
        "the readout says which output it changed, and its level"
    );
}

#[test]
fn a_brightness_key_answers_on_the_display_it_changed() {
    let _settled = ui::motion::testing::settled();
    let dials = Dials::with(&[("eDP-1", 0.5), ("HEADLESS-2", 0.5)]);
    let mut session = crate::common::headless_on(
        &[
            OutputConfig::new("eDP-1", WIDTH, HEIGHT),
            OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT),
        ],
        Options {
            backlights: Some(Box::new(dials.clone())),
            collab: false,
            ..Options::default()
        },
    );
    pump(&mut session, &mut [], |_, _| true);
    let first = session
        .compositor()
        .screenshot("eDP-1")
        .expect("screenshot")
        .rgba;
    let second = session
        .compositor()
        .screenshot("HEADLESS-2")
        .expect("screenshot")
        .rgba;
    tap(&mut session, keys::BRIGHTNESS_DOWN);
    let said = readout_saying(&mut session, "Built-in display");
    assert_eq!(
        said, "Built-in display, 40%",
        "the readout names the display it dimmed"
    );
    assert_eq!(
        dials.asked("eDP-1"),
        [40.0],
        "the display the person works on is the one dimmed"
    );
    pump(&mut session, &mut [], |_, _| true);
    assert_ne!(
        session
            .compositor()
            .screenshot("eDP-1")
            .expect("screenshot")
            .rgba,
        first,
        "the display whose brightness changed answers for itself"
    );
    assert_eq!(
        session
            .compositor()
            .screenshot("HEADLESS-2")
            .expect("screenshot")
            .rgba,
        second,
        "the other display is left alone, with no readout of its own"
    );
}

#[test]
fn a_mode_change_is_confirmed_in_words() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    invoke(&mut session, tilo_session::NIGHT_LIGHT_RESOURCE, "toggle");
    assert_eq!(
        readout_saying(&mut session, "Night light"),
        "Night light on",
        "warmth on is confirmed, not a level"
    );
    invoke(&mut session, tilo_session::MAGNIFIER_RESOURCE, "toggle");
    assert_eq!(
        readout_saying(&mut session, "Magnifier"),
        "Magnifier on",
        "the magnifier says it is on"
    );
    invoke_with(
        &mut session,
        "session:notifications",
        "set-quiet",
        &serde_json::json!({"on": true}),
    );
    assert_eq!(
        readout_saying(&mut session, "disturb"),
        "Do not disturb on",
        "whoever flipped the switch, the readout says so"
    );
    invoke_with(
        &mut session,
        "session:input",
        "toggle-touchpad",
        &serde_json::json!({"on": false}),
    );
    assert_eq!(
        readout_saying(&mut session, "Touchpad"),
        "Touchpad off",
        "the touchpad says what it now does"
    );
}

#[test]
fn a_brightness_key_on_a_machine_that_dims_nothing_says_so() {
    let _settled = ui::motion::testing::settled();
    let mut session = crate::common::headless(Options {
        collab: false,
        ..Options::default()
    });
    tap(&mut session, keys::BRIGHTNESS_DOWN);
    pump(&mut session, &mut [], |session, _| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label == "No adjustable display")
    });
    let said = session.accessible_labels().join("\n");
    assert!(
        !said.contains("0%"),
        "a machine with nothing to dim reports no level, never zero percent: {said}"
    );
}
