//! What the content shows changing ([`NavShellProps::content_key`]): what the content shows
//! now fades in where the one before it stood, asking for the frames it moves over, and lands at
//! once for a person who asked for less motion.

use super::*;
use crate::component::Described as _;

fn props(key: Option<&str>) -> NavShellProps {
    NavShellProps {
        content_kind: ContentKind::Page,
        title: Some("Sound".to_owned()),
        content_key: key.map(str::to_owned),
        sidebar: Some(Probe::boxed("sidebar").0),
        content: Some(Probe::boxed("content").0),
        sidebar_title: Some("Settings".to_owned()),
        ..NavShellProps::default()
    }
}

/// A window whose content is named, at rest: what it shows first is drawn, not faded in.
#[test]
fn a_window_opens_with_its_content_at_rest() {
    let shell = NavShell::from_props("window", props(Some("appearance")));
    let now = crate::motion::frame_now();
    assert_eq!(shell.content_key.as_deref(), Some("appearance"));
    assert_eq!(shell.content_fade.progress(now), 1.0);
    assert!(!shell.content_fade.is_moving(now));
    assert!(!shell.needs_animation_frame());
}

/// The content named again is no change; a new name brings the content it shows in.
#[test]
fn a_new_content_key_brings_the_content_it_shows_in() {
    // A person chose the pane: a press, whose motion follows it.
    crate::motion::note_source(crate::tokens::MotionSource::Pointer);
    let mut shell = NavShell::from_props("window", props(Some("appearance")));
    let now = crate::motion::frame_now();
    assert!(shell.set_props(props(Some("appearance"))));
    assert!(
        !shell.content_fade.is_moving(now),
        "the same content drawn again is no change"
    );

    assert!(shell.set_props(props(Some("sound"))));
    assert_eq!(shell.content_key.as_deref(), Some("sound"));
    assert!(
        shell.content_fade.is_moving(now),
        "the content it shows now arrives"
    );
    assert_eq!(
        shell.content_fade.progress(now),
        0.0,
        "from nothing drawn at first"
    );
    assert!(
        shell.needs_animation_frame(),
        "the fade asks the window for its frames"
    );
}

/// A person who asked for less motion is shown the content at once.
#[test]
fn a_still_window_shows_the_content_at_once() {
    let mut theme = Theme::dark();
    theme.reduce_motion = true;
    let shell = NavShell::from_props("window", props(Some("appearance")));
    let mut h = harness(shell, 1280, 800, theme);
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().set_props(props(Some("sound")));
    let now = crate::motion::frame_now();
    let shell = h.root::<NavShell>();
    assert!(!shell.content_fade.is_moving(now));
    assert_eq!(shell.content_fade.progress(now), 1.0);
}
