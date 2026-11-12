//! Whether this thread is painting: the toolkit's own mark of a widget's
//! `draw`, so a write made in the wrong pass says so instead of landing after
//! the frame decided it was quiet and never being shown or synced.
//!
//! A frame reads the document in its `sync` pass and writes what it took up
//! in its `settle` pass, both before it paints. A surface that dispatches an
//! edit from `draw` writes after both: the frame has already decided what it
//! reads, and a host with nothing else pending has no reason to run another
//! one. The write then waits for an unrelated event, or is never shown.
//!
//! Debug builds stop on it (`Dispatcher::submit`, `submit_now` and
//! `apply_now` assert in `tilo-app`). Release builds pay nothing: the mark
//! exists only where it is checked.

use std::cell::Cell;

thread_local! {
    static DRAW_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// This thread is inside a widget's `draw` until dropped: one per widget
/// drawn, so nested draws (a child, an overlay) count up and down together.
#[derive(Debug)]
pub(crate) struct Painting(());

impl Painting {
    /// Mark the draw that is beginning.
    pub(crate) fn enter() -> Self {
        DRAW_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self(())
    }
}

impl Drop for Painting {
    fn drop(&mut self) {
        DRAW_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Whether this thread is inside a widget's `draw`.
#[must_use]
pub fn painting() -> bool {
    DRAW_DEPTH
        .try_with(|depth| depth.get() > 0)
        .unwrap_or(false)
}
