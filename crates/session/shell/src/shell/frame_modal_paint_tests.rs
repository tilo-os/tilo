//! The shell's modal layers, painted by the one paint path both the screen
//! and every dense renderer use.
//!
//! The emoji picker and the keep-or-revert display prompt take the keyboard
//! for as long as they are up. A paint path that skipped one would still
//! hand it every key press, so what it drew has to be the same pixels the
//! screen shows.

use super::*;

use super::retained_tests::{legacy_and_segmented_pixels, settle_desk, settled_builder};

/// Both recordings must agree, and opening the surface must change what the
/// dense recording paints — otherwise "they match" would hold for two
/// recordings that agree on drawing nothing.
fn assert_open_changes_the_frame(frame: &mut FrameBuilder, label: &str, closed: &[u8]) -> Vec<u8> {
    let open = legacy_and_segmented_pixels(frame, label, false);
    assert!(open != closed, "{label} must change actual output pixels");
    open
}

#[test]
fn native_4k_modal_layers_match_between_dense_and_retained_frames() {
    let (mut frame, _clock) = settled_builder();
    settle_desk(&mut frame);

    let closed = legacy_and_segmented_pixels(&mut frame, "emoji picker closed", false);
    frame.host.toggle_emoji_picker();
    assert!(frame.host.emoji_picker_open(), "the chord opens the picker");
    frame.relayout();
    assert_open_changes_the_frame(&mut frame, "emoji picker open", &closed);
    frame.host.toggle_emoji_picker();
    assert!(!frame.host.emoji_picker_open(), "the chord closes it again");
    frame.relayout();
    let reopened = legacy_and_segmented_pixels(&mut frame, "emoji picker closed again", false);
    assert!(
        reopened == closed,
        "closing the picker must take back exactly what opening it added"
    );

    // No transaction is staged for this id, so the prompt asks about a
    // change nothing will answer: it stays up however long the paint takes.
    let before_prompt = legacy_and_segmented_pixels(&mut frame, "display prompt absent", false);
    frame.host.show_display_confirmation(1, None);
    assert!(
        frame.host.display_confirmation_visible(),
        "showing the prompt asks its question"
    );
    frame.relayout();
    assert_open_changes_the_frame(&mut frame, "display prompt open", &before_prompt);
}
