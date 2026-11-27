//! What the content shows changing ([`NavShellProps::content_key`]): what the content shows
//! now fades in where the one before it stood, asking for the frames it moves over, and lands at
//! once for a person who asked for less motion.

use super::*;
use tilo_ui_base::component::Described as _;

fn props(key: Option<&str>) -> NavShellProps {
    NavShellProps {
        content_kind: ContentKind::Page,
        title: Some("Sound".to_owned()),
        content_key: key.map(str::to_owned),
        sidebar: Some(TestLeaf::fill("sidebar").boxed().0),
        content: Some(TestLeaf::fill("content").boxed().0),
        sidebar_title: Some("Settings".to_owned()),
        ..NavShellProps::default()
    }
}

/// The content named again is no change; a new name brings the content it shows in.
#[test]
fn a_new_content_key_brings_the_content_it_shows_in() {
    // A person chose the pane: the content eases in on the press's motion.
    tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
    let mut shell = NavShell::from_props("window", props(Some("appearance")));
    let now = tilo_ui_base::motion::frame_now();
    assert!(shell.set_props(props(Some("appearance"))));
    assert!(
        !shell.test_content_fade_moving(now),
        "the same content drawn again is no change"
    );

    assert!(shell.set_props(props(Some("sound"))));
    assert_eq!(shell.test_content_key(), Some("sound"));
    assert!(
        shell.test_content_fade_moving(now),
        "the content it shows now arrives"
    );
    assert_eq!(
        shell.test_content_fade_progress(now),
        0.0,
        "from nothing drawn at first"
    );
    assert!(
        shell.needs_animation_frame(),
        "the fade asks the window for its frames"
    );
}

/// A person who asked for less motion is shown the new content at once, and so is a pane an
/// action opened (a search row, an agent): only a press eases it in.
#[test]
fn a_still_theme_or_an_action_lands_the_content_at_once() {
    let mut still = Theme::dark();
    still.reduce_motion = true;
    let mut h = harness(
        NavShell::from_props("window", props(Some("appearance"))),
        1280,
        800,
        still,
    );
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().set_props(props(Some("sound")));
    let now = tilo_ui_base::motion::frame_now();
    let shell = h.root::<NavShell>();
    assert!(
        !shell.test_content_fade_moving(now),
        "a still theme lands it"
    );
    assert_eq!(shell.test_content_fade_progress(now), 1.0);

    let mut shell = NavShell::from_props("window", props(Some("appearance")));
    tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Action);
    assert!(shell.set_props(props(Some("sound"))));
    let now = tilo_ui_base::motion::frame_now();
    assert!(!shell.test_content_fade_moving(now), "an action lands it");
    assert_eq!(shell.test_content_fade_progress(now), 1.0);
}
