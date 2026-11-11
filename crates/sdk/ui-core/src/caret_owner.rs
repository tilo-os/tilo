//! Who the keyboard belongs to: the one rule every caret follows.
//!
//! A field's text and selection stay where the person left them when another
//! surface takes the keyboard; its caret is what says "typing lands here", so
//! only the surface holding the keyboard paints one. Hosts say which surface
//! they are drawing for: [`holding`] around a tree's frame, from the fact the
//! host already answers for typing (`SurfaceHost::wants_keyboard` for the
//! session's own surfaces, `WindowEvent::Focused` for a window). Painters of a
//! caret ask [`shown`]; nothing else has to know about the surface above them.
//!
//! A tree drawn without a host saying anything keeps its carets: capturing a
//! widget on its own, and a tool that draws one frame, is not a surface losing
//! the keyboard.

use std::cell::Cell;

thread_local! {
    /// Whether the tree being drawn holds the keyboard.
    static HELD: Cell<bool> = const { Cell::new(true) };
}

/// Whether the tree being drawn holds the keyboard, and so paints carets.
#[must_use]
pub fn shown() -> bool {
    HELD.with(Cell::get)
}

/// Draw within `draw` as a tree that (`held`) does or does not hold the
/// keyboard, putting back what was true around it when the guard drops.
#[must_use = "the tree keeps the keyboard only while the guard lives"]
pub fn holding(held: bool) -> Held {
    let before = HELD.with(Cell::get);
    HELD.with(|state| state.set(held));
    Held(before)
}

/// [`holding`]'s put-back.
#[derive(Debug)]
pub struct Held(bool);

impl Drop for Held {
    fn drop(&mut self) {
        let before = self.0;
        HELD.with(|state| state.set(before));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_no_host_named_keeps_its_carets() {
        assert!(shown(), "nothing said otherwise");
    }

    #[test]
    fn a_held_keyboard_shows_carets_and_gives_them_back() {
        let outer = shown();
        {
            let _without = holding(false);
            assert!(!shown());
        }
        assert_eq!(shown(), outer, "the surface around keeps what it had");
    }
}
