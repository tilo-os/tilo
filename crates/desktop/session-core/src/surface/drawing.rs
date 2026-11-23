//! When the draws a surface hands to another thread start.
//!
//! The shell prepares a frame in more than one pass over its surfaces: a surface drawing
//! translucent material first draws a stand-in for what lies below it, and draws again once the
//! shell has blurred that and handed it back. Only the last of those pictures is the frame's. A
//! draw handed over while the shell prepares a frame ([`preparing`]) waits until the preparation is
//! over, so a later picture of the same surface replaces it before anything is drawn, and nothing
//! the frame does not show is ever shown.

use std::cell::RefCell;

/// A draw handed over, to start.
type Draw = Box<dyn FnOnce()>;

thread_local! {
    /// The draws handed over while a frame is prepared on this thread, or `None` outside one.
    static HELD: RefCell<Option<Vec<Draw>>> = const { RefCell::new(None) };
}

/// A frame is being prepared on this thread until the returned guard is dropped: the draws handed
/// over meanwhile ([`start`]) start then, in the order they were handed over.
#[must_use = "the preparation ends when the guard is dropped"]
pub fn preparing() -> Preparing {
    let outer = HELD.with(|held| {
        let mut held = held.borrow_mut();
        let outer = held.is_some();
        if !outer {
            *held = Some(Vec::new());
        }
        outer
    });
    Preparing { outer }
}

/// Start a draw: now, or once the frame being prepared on this thread is ([`preparing`]).
pub fn start(draw: impl FnOnce() + 'static) {
    let draw = HELD.with(|held| match held.borrow_mut().as_mut() {
        Some(waiting) => {
            waiting.push(Box::new(draw));
            None
        }
        None => Some(draw),
    });
    if let Some(draw) = draw {
        draw();
    }
}

/// The preparation of a frame on this thread; dropping it starts the draws handed over during it.
pub struct Preparing {
    /// Another preparation was under way when this one began: its draws wait for that one.
    outer: bool,
}

impl Preparing {
    /// End the preparation without starting what was handed over during it: the draws are
    /// started when the returned [`Held`] is.
    pub fn hold(mut self) -> Held {
        let draws = if self.outer {
            Vec::new()
        } else {
            HELD.with(|held| held.borrow_mut().take())
                .unwrap_or_default()
        };
        // Nothing is left for the drop to start.
        self.outer = true;
        Held(draws)
    }
}

/// Draws handed over while a frame was prepared, held back by [`Preparing::hold`].
#[must_use = "held draws start only when started"]
pub struct Held(Vec<Draw>);

impl Held {
    /// Start them, in the order they were handed over.
    pub fn start(self) {
        for draw in self.0 {
            draw();
        }
    }
}

impl Drop for Preparing {
    fn drop(&mut self) {
        if self.outer {
            return;
        }
        let waiting = HELD
            .with(|held| held.borrow_mut().take())
            .unwrap_or_default();
        for draw in waiting {
            draw();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{preparing, start};

    #[test]
    fn a_draw_handed_over_while_a_frame_is_prepared_starts_when_it_is_done() {
        let started = Rc::new(RefCell::new(Vec::new()));
        let log = |name: &'static str| {
            let started = Rc::clone(&started);
            move || started.borrow_mut().push(name)
        };
        start(log("outside"));
        assert_eq!(
            *started.borrow(),
            ["outside"],
            "outside a frame it starts at once"
        );
        {
            let _frame = preparing();
            start(log("stand-in"));
            start(log("material"));
            assert_eq!(
                *started.borrow(),
                ["outside"],
                "nothing starts while the frame is prepared"
            );
        }
        assert_eq!(*started.borrow(), ["outside", "stand-in", "material"]);

        let frame = preparing();
        start(log("held"));
        let held = frame.hold();
        start(log("after"));
        assert_eq!(
            started.borrow().last(),
            Some(&"after"),
            "a held draw waits while the next one starts"
        );
        held.start();
        assert_eq!(started.borrow().last(), Some(&"held"));
    }
}
