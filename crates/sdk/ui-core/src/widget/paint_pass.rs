//! Whether this thread is painting: the toolkit's own mark of a widget's `draw`, so a write
//! made in the wrong pass says so instead of landing after the frame decided it was quiet and
//! never being shown or synced.

use std::cell::Cell;

thread_local! {
    static DRAW_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// This thread is inside a widget's `draw` until dropped: one per widget
/// drawn, so nested draws (a child, an overlay) count up and down together.
#[derive(Debug)]
pub(crate) struct Painting(());

impl Painting {
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
