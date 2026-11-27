//! Drawing for a presenter that keeps its canvas between frames. Such a presenter rasterizes a strip
//! once into a canvas as wide as its content and shows the strip's pan by reading that canvas at an
//! offset.

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

/// Start drawing one frame for such a presenter.
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

    tilo_test_env::scenario! {
        fn scroll_is_unbaked_only_while_a_retained_scene_asks_for_it;

        phase scoped_draw_restores_nested_policy_after_unwind;
        {
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

        phase strips_are_unbaked_only_between_begin_and_end;
        {
            assert!(!unbake_scroll());
            begin_retained_scene(false);
            assert!(!unbake_scroll());
            begin_retained_scene(true);
            assert!(unbake_scroll());
            end_retained_scene();
            assert!(!unbake_scroll());
        }
    }
}
