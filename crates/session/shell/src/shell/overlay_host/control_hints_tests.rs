//! The desktop's control hints follow the person's record through the
//! session's settings changes: a frame never reads the record again.

use crate::shell::frame::FrameBuilder;
use settings_store::control_hints::{LEARNED, Learned};

/// Another of the person's devices turned hints off: frames before the
/// settings change reaches the desktop leave the hints as they were, and the
/// change itself turns them off. Hints a test put to rest stay at rest for
/// as long as the test runs.
#[test]
fn the_hint_record_arrives_with_its_settings_change_not_with_a_frame() {
    let _settings = crate::shell::test_support::install_settings(std::sync::Arc::new(
        settings_store::HostDocument::new(serde_json::Map::new(), |_| Ok(())),
    ));
    let mut frame = FrameBuilder::with_fixture_content(Box::new(ui::widget::Container::new()));
    let mut doc = automerge::AutoCommit::new();
    frame.pump_apps(&mut doc, &automerge::ROOT);
    assert!(
        frame.host().control_hints_enabled(),
        "hints show by default"
    );

    LEARNED
        .set(Learned {
            enabled: false,
            ..Learned::default()
        })
        .expect("keep the record");
    for _ in 0..3 {
        frame.pump_apps(&mut doc, &automerge::ROOT);
    }
    assert!(
        frame.host().control_hints_enabled(),
        "a frame reads no settings: the record waits for its change"
    );

    frame
        .host_mut()
        .follow_control_hints(&settings_store::load());
    assert!(
        !frame.host().control_hints_enabled(),
        "the settings change brings the other device's choice"
    );

    LEARNED.set(Learned::default()).expect("keep the record");
    frame
        .host_mut()
        .follow_control_hints(&settings_store::load());
    assert!(frame.host().control_hints_enabled());
    frame.host_mut().rest_control_hints();
    for _ in 0..3 {
        frame.pump_apps(&mut doc, &automerge::ROOT);
    }
    assert!(
        !frame.host().control_hints_enabled(),
        "hints put to rest stay at rest"
    );
}
