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
