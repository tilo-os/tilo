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

/// The content named again is no change; a new name brings the content it shows in.
#[test]
fn a_new_content_key_brings_the_content_it_shows_in() {
    // A person chose the pane: the content eases in on the press's motion.
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

/// A person who asked for less motion is shown the content at once, and so is a pane an action
/// opened: a key or an agent lands, as every control of the theme does.
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

/// A pane an action opened — a search row, an agent — lands at once: the theme's motions land
/// for anything that is not the pointer, and the content is there when the next frame draws.
#[test]
fn an_action_lands_the_content_at_once() {
    let mut shell = NavShell::from_props("window", props(Some("appearance")));
    crate::motion::note_source(crate::tokens::MotionSource::Action);
    assert!(shell.set_props(props(Some("sound"))));
    let now = crate::motion::frame_now();
    assert!(
        !shell.content_fade.is_moving(now),
        "an action lands the content at once"
    );
    assert_eq!(shell.content_fade.progress(now), 1.0);
}
