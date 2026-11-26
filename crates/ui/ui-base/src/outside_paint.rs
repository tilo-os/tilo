//! Whether a drawing read state outside the widgets that made it. A widget is clean until something it
//! reads changes, and most widgets say so themselves: they hold what they paint, and their setters and
//! event routes mark them dirty.

use std::cell::Cell;

thread_local! {
    static PAINTED: Cell<bool> = const { Cell::new(false) };
    static ON_SCREEN: Cell<bool> = const { Cell::new(false) };
}

/// This drawing read state that lives outside the widgets that paint it.
pub fn note() {
    PAINTED.with(|painted| painted.set(true));
    ON_SCREEN.with(|shown| shown.set(true));
}

/// Whether the frame on screen stands on a reading from outside the tree, and so is stale as soon as
/// input reaches it ([`crate::input_generation`]).
#[must_use]
pub fn on_screen() -> bool {
    ON_SCREEN.with(Cell::get)
}

/// The next frame's drawing has begun: nothing stands on screen yet.
pub fn forget_on_screen() {
    ON_SCREEN.with(|shown| shown.set(false));
}

/// Start recording a drawing: answers whether an enclosing brace had already seen such a reading, and
/// clears the mark for this one.
#[must_use]
pub fn begin() -> bool {
    PAINTED.with(|painted| painted.replace(false))
}

/// Finish recording a drawing that [`begin`] started: answers whether it, or anything it drew, read
/// outside state, and leaves the enclosing brace with what it had plus this drawing's reading.
pub fn end(enclosing: bool) -> bool {
    let painted = PAINTED.with(Cell::get);
    PAINTED.with(|mark| mark.set(enclosing));
    if painted {
        note();
        enclosing || painted
    } else {
        enclosing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_brace_keeps_what_its_drawing_read() {
        let enclosing = begin();
        assert!(
            !end(enclosing),
            "a drawing that read nothing outside says so"
        );

        let enclosing = begin();
        note();
        assert!(end(enclosing), "the reading reached the drawing's owner");
        assert!(
            begin(),
            "and the brace around it answers that it read outside too"
        );
    }

    #[test]
    fn a_drawing_inside_a_reading_brace_is_not_lost() {
        let outer = begin();
        note();
        let inner = begin();
        assert!(inner, "the inner brace stood on an outer reading");
        assert!(end(inner), "and keeps it");
        assert!(
            end(outer),
            "the outer brace answers for the reading it held"
        );
    }
}
