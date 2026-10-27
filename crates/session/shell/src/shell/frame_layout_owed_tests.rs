//! What a compositor frame lays out: the desktop is laid out again only
//! when something it lays out changed (its tree, an input it handled, a
//! widget's animation moving what it places), not on every painted frame.
//!
//! `cargo test -p shell --lib frame_layout_owed -- --nocapture` prints what
//! each frame stage cost. Times are for reading; counts are asserted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    reason = "benchmark fixture: failures are the assertion"
)]

use super::*;

fn micros(since: std::time::Instant) -> u64 {
    since.elapsed().as_micros() as u64
}

fn median(mut values: Vec<u64>) -> u64 {
    values.sort_unstable();
    values[values.len() / 2]
}

/// The populated desktop, laid out, painted and at rest: a frame that
/// owes it no layout. The top bar's clock stands still, as its minute
/// would be a real change between two frames.
fn settled() -> (
    crate::shell::test_support::PopulatedShellFixture,
    AutoCommit,
    tilo_test_env::ScopedEnv,
) {
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    let mut frame = crate::shell::test_support::PopulatedShellFixture::new(
        (1280, 800),
        ui::theme::Theme::dark(),
    );
    frame.relayout();
    let _ = frame.paint_with_damage();
    let mut doc = AutoCommit::new();
    assert!(
        (0..50).any(|_| !frame_of(&mut frame, &mut doc)),
        "the populated desktop comes to rest"
    );
    (frame, doc, clock)
}

/// One painted frame as the compositor's loop makes it: the pump, the
/// layout the frame owes, the paint. Whether it laid out.
fn frame_of(
    frame: &mut crate::shell::test_support::PopulatedShellFixture,
    doc: &mut AutoCommit,
) -> bool {
    let _ = frame.pump_apps(doc, &automerge::ROOT);
    let laid_out = frame.relayout_if_owed();
    let _ = frame.paint_with_damage();
    laid_out
}

/// A painted frame nothing on the desktop changed for (a client's new
/// buffer, the cursor, an app drawing in its tile) lays nothing out.
#[test]
fn frame_layout_owed_by_nothing_lays_nothing_out() {
    let (mut frame, mut doc, _clock) = settled();
    let (mut pumps, mut layouts, mut forced, mut paints) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let passes = frame.layout_passes;
    for _ in 0..60 {
        let started = std::time::Instant::now();
        let _ = frame.pump_apps(&mut doc, &automerge::ROOT);
        pumps.push(micros(started));
        let started = std::time::Instant::now();
        assert!(!frame.relayout_if_owed(), "nothing changed to lay out");
        layouts.push(micros(started));
        let started = std::time::Instant::now();
        let _ = frame.paint_with_damage();
        paints.push(micros(started));
    }
    assert_eq!(frame.layout_passes, passes, "no frame laid out");
    // What each of those frames cost before: a full layout.
    for _ in 0..20 {
        let started = std::time::Instant::now();
        frame.relayout();
        forced.push(micros(started));
    }
    eprintln!(
        "shell-frame quiet: pump={}us relayout_if_owed={}us (full relayout={}us) paint={}us",
        median(pumps),
        median(layouts),
        median(forced),
        median(paints),
    );
}

/// A change of the tree (here a workspace added) is laid out in the frame
/// that paints it, and only in that frame.
#[test]
fn frame_layout_owed_by_a_tree_change_lays_out_once() {
    let (mut frame, mut doc, _clock) = settled();
    frame.with_chrome(|chrome| {
        chrome.add_workspace("owed");
    });
    assert!(frame_of(&mut frame, &mut doc), "the change is laid out");
    let passes = frame.layout_passes;
    for _ in 0..5 {
        frame_of(&mut frame, &mut doc);
    }
    assert_eq!(frame.layout_passes, passes, "and only once");
}

/// An input the desktop handled owes a layout; a widget's animation that
/// moved what it lays out owes one too, until it stops noting it.
#[test]
fn frame_layout_owed_by_input_and_by_layout_moves() {
    let (mut frame, mut doc, _clock) = settled();
    frame.owe_layout();
    assert!(frame_of(&mut frame, &mut doc), "what an input changed");
    assert!(!frame_of(&mut frame, &mut doc));
    ui::widget::note_layout_moved();
    assert!(frame_of(&mut frame, &mut doc), "what an update moved");
    assert!(!frame_of(&mut frame, &mut doc), "and nothing after");
}
