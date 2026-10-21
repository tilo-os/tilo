//! Drawing for a presenter that keeps the desktop between frames.
//!
//! Such a presenter rasterizes a strip once into a canvas as wide as its
//! content and shows the strip's pan by reading that canvas at an offset. The
//! strip is then drawn in canvas coordinates: where its windows sit with no
//! pan, so the encoded scene does not change while the pan does, and a pan
//! draws nothing.

use std::cell::Cell;

thread_local! {
    static UNBAKE_SCROLL: Cell<bool> = const { Cell::new(false) };
}

/// Restores the enclosing draw policy even on an early return or unwind.
pub struct RetainedScene {
    previous: bool,
}

impl RetainedScene {
    pub fn begin(unbake_scroll: bool) -> Self {
        Self {
            previous: UNBAKE_SCROLL.replace(unbake_scroll),
        }
    }
}

impl Drop for RetainedScene {
    fn drop(&mut self) {
        UNBAKE_SCROLL.set(self.previous);
    }
}

/// Start drawing one frame for such a presenter. `unbake_scroll` draws strip
/// content in canvas coordinates. The flag is thread-local and cleared by
/// [`end_retained_scene`].
pub fn begin_retained_scene(unbake_scroll: bool) {
    UNBAKE_SCROLL.set(unbake_scroll);
}

/// The frame is drawn.
pub fn end_retained_scene() {
    UNBAKE_SCROLL.set(false);
}

/// Whether strip content should be drawn in canvas coordinates.
pub fn unbake_scroll() -> bool {
    UNBAKE_SCROLL.get()
}

thread_local! {
    static PAN_REPLAY: Cell<PanFrame> = const { Cell::new(PanFrame::Drawn) };
    static PAN_FRAMES: Cell<u64> = const { Cell::new(0) };
    static REPLAYED: Cell<usize> = const { Cell::new(0) };
}

/// What the frame being drawn lets a strip drawn in screen space do with
/// the drawings it keeps of its windows ([`crate::widget::GroupRow`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanFrame {
    /// Anything may have changed: every window is drawn, and the drawings
    /// are dropped.
    Drawn,
    /// The frame only moved laid-out content, but every window is drawn
    /// (its host is about to read what they note while drawing). Nothing in
    /// them changed, so the drawings stay good for the frames after.
    Kept,
    /// The frame only moved laid-out content: a window at rest may show its
    /// last drawing moved to where the window is now.
    Replayed,
    /// The frame only moved laid-out content, and its host does not draw
    /// again before input: a window at rest may show its last drawing only
    /// where it stays where it was drawn (a strip drawn in canvas
    /// coordinates while the presenter pans it), so what its widgets noted
    /// while drawing still holds.
    Moved,
}

impl PanFrame {
    /// Nothing but laid-out positions changed since the frame before.
    #[must_use]
    pub fn only_moved(self) -> bool {
        self != Self::Drawn
    }
}

/// A frame that only moved laid-out content: nothing reached a widget, the
/// document, focus or presence since the last frame, and layout re-applied
/// positions without measuring. While one is drawn ([`PanFrame::Replayed`]),
/// a strip drawn in screen space may show a window at rest by replaying its
/// last drawing moved to where the window is now. Every other frame draws
/// every window.
///
/// A replayed window did not draw, so what its widgets note while drawing
/// (where a control was painted, where a caret is) still describes where
/// the window was. The host draws again before anything reads that: before
/// input reaches the widgets, and before it reads the frame's semantics
/// ([`crate::headless::FrameHarness::settle_replayed_draws`]).
pub struct PanReplay {
    previous: (PanFrame, u64),
}

impl PanReplay {
    /// Begin drawing one frame of the given kind, the host's frame number
    /// `number`. A host that replays begins one for every frame it draws,
    /// numbered in order, so a drawing can tell whether it was made in that
    /// host's frame just before ([`pan_frame`]).
    pub fn begin(frame: PanFrame, number: u64) -> Self {
        Self {
            previous: (PAN_REPLAY.replace(frame), PAN_FRAMES.replace(number)),
        }
    }
}

impl Drop for PanReplay {
    fn drop(&mut self) {
        PAN_REPLAY.set(self.previous.0);
        PAN_FRAMES.set(self.previous.1);
    }
}

/// What the frame being drawn lets a strip do with its windows' drawings
/// ([`PanReplay`]).
pub fn pan_frame_kind() -> PanFrame {
    PAN_REPLAY.get()
}

/// Whether the frame being drawn may replay windows at rest ([`PanReplay`]).
pub fn pan_replay() -> bool {
    PAN_REPLAY.get() == PanFrame::Replayed
}

/// The number of the frame being drawn ([`PanReplay::begin`]).
pub fn pan_frame() -> u64 {
    PAN_FRAMES.get()
}

/// Count one window shown by replaying its drawing.
pub fn note_replayed() {
    REPLAYED.set(REPLAYED.get().saturating_add(1));
}

/// Windows replayed since the last call.
pub fn take_replayed() -> usize {
    REPLAYED.replace(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_draw_restores_nested_policy_after_unwind() {
        assert!(!unbake_scroll());
        let outer = RetainedScene::begin(true);
        assert!(unbake_scroll());
        let failed = std::panic::catch_unwind(|| {
            let _inner = RetainedScene::begin(false);
            assert!(!unbake_scroll());
            panic!("draw failed");
        });
        assert!(failed.is_err());
        assert!(unbake_scroll());
        drop(outer);
        assert!(!unbake_scroll());
    }

    #[test]
    fn strips_are_unbaked_only_between_begin_and_end() {
        assert!(!unbake_scroll());
        begin_retained_scene(false);
        assert!(!unbake_scroll());
        begin_retained_scene(true);
        assert!(unbake_scroll());
        end_retained_scene();
        assert!(!unbake_scroll());
    }
}
