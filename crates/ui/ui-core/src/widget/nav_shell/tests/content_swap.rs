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
