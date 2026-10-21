#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel counts over a bounded fixture canvas"
)]

//! Other people's pointers a host placed over its surface are drawn by the
//! frame itself: the shell's arrow in the person's colour with their name
//! tag, laid out so the whole name stays on screen at the surface's edge.

use tilo_ui::State;
use tilo_ui::headless::FrameHarness;
use tilo_ui::permissions::UserId;
use tilo_ui::presence::{PlacedPointer, PresenceCtx};
use tilo_ui::theme::Theme;
use tilo_ui::widget::Spacer;

const SIZE: (u32, u32) = (420, 200);
const PINK: (u8, u8, u8) = (0xee, 0x83, 0xaf);

/// A frame showing Ada's pointer at `at`, and how many of its pixels wear
/// her colour.
fn frame_with_pointer_at(at: (f64, f64)) -> (FrameHarness, usize) {
    let mut harness =
        FrameHarness::new_with_theme(State::new(), Box::new(Spacer::new()), SIZE, Theme::dark());
    let mut presence = PresenceCtx::empty();
    let ada = UserId("ada-device".into());
    presence.set_display_name(ada.clone(), "Eager Dolphin");
    presence.set_color_override(ada.clone(), "#ee83af");
    let cursor = presence.placed_cursor(PlacedPointer {
        user: ada,
        at,
        name: None,
        agent: false,
        activity: None,
        chat: None,
        reaction: None,
    });
    assert!(harness.set_placed_pointers(vec![cursor]));
    harness.relayout();
    let rgba = harness.render_rgba();
    let near = |p: &[u8]| {
        p[0].abs_diff(PINK.0) < 24 && p[1].abs_diff(PINK.1) < 24 && p[2].abs_diff(PINK.2) < 24
    };
    let worn = rgba.chunks_exact(4).filter(|p| near(p)).count();
    (harness, worn)
}

#[test]
fn a_placed_pointer_is_drawn_whole_in_its_colour_even_at_the_edge() {
    let (harness, middle) = frame_with_pointer_at((120.0, 80.0));
    assert_eq!(harness.placed_pointers().len(), 1);
    assert!(
        middle > 300,
        "the arrow and the name tag are drawn in the person's colour ({middle} px)"
    );
    // Right at the edge the tag moves inside rather than running off it:
    // as much of it is drawn as in the middle of the surface.
    let (_, edge) = frame_with_pointer_at((f64::from(SIZE.0) - 4.0, 80.0));
    assert!(
        edge * 10 >= middle * 9,
        "the name tag stays whole on screen at the edge ({edge} of {middle} px)"
    );
}

#[test]
fn pointers_outside_the_surface_or_taken_away_are_not_drawn() {
    let (mut harness, _) = frame_with_pointer_at((120.0, 80.0));
    assert!(
        harness.set_placed_pointers(Vec::new()),
        "taking them down is a change"
    );
    assert!(harness.placed_pointers().is_empty());
    assert!(
        !harness.set_placed_pointers(Vec::new()),
        "nothing shown, nothing to redraw"
    );
    let presence = PresenceCtx::empty();
    let outside = presence.placed_cursor(PlacedPointer {
        user: UserId("far".into()),
        at: (f64::from(SIZE.0) + 50.0, 10.0),
        name: None,
        agent: false,
        activity: None,
        chat: None,
        reaction: None,
    });
    harness.set_placed_pointers(vec![outside]);
    assert!(harness.placed_pointers().is_empty());
}
