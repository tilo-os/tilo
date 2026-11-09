//! What makes an output owe a frame: a change to what it shows, wherever the
//! change is made — the client's commit, or the policy placing its window.

use super::*;

/// A floating stack whose windows a test places, as a desk's strip pans them.
#[derive(Default)]
struct Movable {
    at: Rc<RefCell<Option<Point<i32, Logical>>>>,
}

impl WindowPolicy for Movable {
    fn initial_size(
        &mut self,
        window: &WindowInfo,
        outputs: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)> {
        StackPolicy.initial_size(window, outputs)
    }

    fn place(
        &mut self,
        window: &WindowInfo,
        size: (i32, i32),
        outputs: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        let at = *self.at.borrow();
        at.unwrap_or_else(|| StackPolicy.place(window, size, outputs))
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, Rectangle<i32, Logical>)],
        _outputs: &[Rectangle<i32, Logical>],
    ) -> Option<Vec<Arrangement>> {
        let at = (*self.at.borrow())?;
        Some(
            windows
                .iter()
                .map(|(window, _)| Arrangement {
                    id: window.id,
                    location: at,
                    size: None,
                })
                .collect(),
        )
    }
}

/// A window the policy moves back onto an output is drawn there: the output
/// owes the frame that shows it, with nothing else having damaged the screen.
#[test]
fn a_window_the_policy_moves_is_drawn_where_it_moved_to() {
    let policy = Movable::default();
    let at = Rc::clone(&policy.at);
    let mut compositor = headless(Box::new(policy));
    let _client = mapped_client(&mut compositor, "moved", (200, 100), [255, 0, 0, 255]);
    compositor.draw_frame().expect("a frame");

    // Off every output, as a window whose well stands off the strip is left.
    *at.borrow_mut() = Some(Point::from((-4000, 0)));
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_ne!(
        frame.pixel(320, 240),
        Some([255, 0, 0, 255]),
        "a window off the output is on no frame"
    );

    // Back on it: what the output shows changed, so it owes that frame. The move is applied by
    // the frame that asks the policy for its places again.
    *at.borrow_mut() = Some(Point::from((20, 20)));
    compositor.draw_frame().expect("a frame");
    assert!(
        compositor.owes_frame(),
        "the output owes the frame that shows the moved window"
    );
    for _ in 0..8 {
        if !compositor.owes_frame() {
            break;
        }
        compositor.draw_frame().expect("a frame");
    }
    assert!(!compositor.owes_frame(), "and draws it");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(100, 60),
        Some([255, 0, 0, 255]),
        "the window is drawn where the policy moved it to"
    );
}
