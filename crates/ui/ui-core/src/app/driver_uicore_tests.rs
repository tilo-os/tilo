//! The display-less driver's own facilities: the pointers a host places over its surface, the
//! scene's damage parts, and the frame read as its composition.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use super::driver::Driver;
use crate::permissions::MemberId;
use crate::presence::{PlacedPointer, PresenceCtx};
use crate::theme::Theme;
use crate::widget::{Spacer, Text};

fn pointer(presence: &PresenceCtx, at: (f64, f64)) -> crate::presence::PeerCursor {
    presence.placed_cursor(PlacedPointer {
        user: MemberId::person("ada"),
        at,
        name: None,
        activity: None,
        chat: None,
        reaction: None,
    })
}

#[test]
fn a_placed_pointer_is_kept_while_it_is_on_the_surface() {
    let mut driver = Driver::new_with_theme(
        crate::State::new(),
        Box::new(Spacer::new()),
        (200, 120),
        Theme::dark(),
    );
    assert!(driver.placed_pointers().is_empty());
    let presence = PresenceCtx::empty();
    assert!(driver.set_placed_pointers(vec![pointer(&presence, (40.0, 30.0))]));
    driver.relayout();
    assert_eq!(driver.placed_pointers().len(), 1);
    assert_eq!(
        driver.drawn_placed_pointers().len(),
        1,
        "the frame draws the pointer it was handed"
    );

    // A pointer outside the surface shows nothing of itself, so it is not kept.
    assert!(driver.set_placed_pointers(vec![pointer(&presence, (500.0, 30.0))]));
    assert!(driver.placed_pointers().is_empty());
    assert!(!driver.set_placed_pointers(Vec::new()), "nothing to change");
}

#[test]
fn the_composition_reads_the_widgets_the_frame_laid_out() {
    let mut driver = Driver::new(crate::State::new(), Box::new(Text::new("hello")), (200, 60));
    driver.relayout();
    let composition = driver.composition();
    assert_eq!(composition.viewport, [200, 60]);
    fn widgets(node: &crate::composition::Composed, out: &mut Vec<String>) {
        out.push(node.widget.clone());
        for child in &node.children {
            widgets(child, out);
        }
    }
    let mut named = Vec::new();
    widgets(&composition.root, &mut named);
    assert!(
        named.iter().any(|widget| widget == "Text"),
        "the laid-out text is composed: {named:?}"
    );
    assert!(composition.root.rect[2] > 0.0, "a composed box has area");
}

#[test]
fn the_scenes_damage_parts_are_recorded_only_when_asked() {
    let mut driver = Driver::new(crate::State::new(), Box::new(Text::new("hello")), (200, 60));
    driver.relayout();
    assert!(driver.scene_parts().is_none());
    driver.track_scene_parts(true);
    driver.relayout();
    assert!(
        driver.scene_parts().is_some(),
        "a tracked scene's parts are kept"
    );
    driver.track_scene_parts(false);
    assert!(driver.scene_parts().is_none());
}
