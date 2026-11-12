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

/// Draw the frames a change to the desk is shown by, and rest it.
///
/// A frame asks the policy for its places, so the one that finds a move is the one that owes the
/// frame showing it. When the display is due, that frame draws the move in the same pass; when it
/// is not, the output keeps owing it until the next blank the display builds, which is the frame
/// after. So both are drawn here: a desk rests after them whatever pace the machine ran at.
fn settle(compositor: &mut Compositor) {
    compositor
        .draw_frame()
        .expect("the frame that asks the policy for its places");
    compositor
        .draw_frame()
        .expect("the frame the display paces the change to");
    assert!(!compositor.owes_frame(), "the desk draws what it owes");
}

/// A window the policy moves is drawn where it moved to: the output draws it
/// there, with nothing else having damaged the screen.
#[test]
fn a_window_the_policy_moves_is_drawn_where_it_moved_to() {
    let policy = Movable::default();
    let at = Rc::clone(&policy.at);
    let mut compositor = headless(Box::new(policy));
    let _client = mapped_client(&mut compositor, "moved", (200, 100), [255, 0, 0, 255]);
    settle(&mut compositor);
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(320, 240),
        Some([255, 0, 0, 255]),
        "the window is drawn where it mapped"
    );

    // Off every output, as a window whose well stands off the strip is left.
    *at.borrow_mut() = Some(Point::from((-4000, 0)));
    settle(&mut compositor);
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_ne!(
        frame.pixel(320, 240),
        Some([255, 0, 0, 255]),
        "a window off the output is on no frame"
    );

    // Back on it: what the output shows changed, so it owes the frame that shows the move, though
    // no client drew anything.
    *at.borrow_mut() = Some(Point::from((20, 20)));
    settle(&mut compositor);
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(100, 60),
        Some([255, 0, 0, 255]),
        "the window is drawn where the policy moved it to"
    );
}

/// A key the focused client takes changes nothing the compositor draws: the client answers it with
/// a commit, and only that owes a frame, in the turn the commit arrives in. A key that owed a frame
/// of its own drew an empty frame first, which then held the answer back to the next refresh.
#[test]
fn a_key_for_a_client_owes_no_frame_of_its_own() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "typed", (200, 100), [0, 0, 255, 255]);
    settle(&mut compositor);
    for pressed in [true, false] {
        compositor
            .input(InputEvent::Key {
                keycode: 30,
                pressed,
            })
            .unwrap();
    }
    assert!(!compositor.owes_frame(), "a key alone owes no frame");
    // The client's answer arrives while the loop works out its frame, and that frame shows it.
    client.show(200, 100, [255, 0, 0, 255]);
    client.exchange();
    assert!(
        compositor.catch_up().unwrap(),
        "the commit answering it does"
    );
    compositor.draw_frame().expect("a frame");
    compositor
        .input(InputEvent::PointerMotion { x: 50.0, y: 50.0 })
        .unwrap();
    assert!(compositor.owes_frame(), "the pointer moving does");
}

/// Each window's client commits are counted against that window: a desk that draws nothing keeps
/// every count, and an app that draws moves its own and no one else's. What a tool reads to prove a
/// window is still, or to count an app's frames in a real session.
#[test]
fn a_window_counts_the_frames_its_client_draws() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut quiet = mapped_client(&mut compositor, "quiet", (200, 100), [0, 0, 255, 255]);
    let mut drawing = mapped_client(&mut compositor, "drawing", (200, 100), [255, 0, 0, 255]);
    settle(&mut compositor);
    let id_of = |compositor: &Compositor, app_id: &str| {
        compositor
            .windows()
            .into_iter()
            .find(|(info, _)| info.app_id.as_deref() == Some(app_id))
            .map(|(info, _)| info.id)
            .expect("the window is mapped")
    };
    let quiet_frames = compositor.window_frames(id_of(&compositor, "quiet"));
    let drawing_id = id_of(&compositor, "drawing");
    let drawing_frames = compositor.window_frames(drawing_id);
    assert!(quiet_frames > 0, "its first frame is counted");

    // The desk draws again with neither client drawing: no window's count moves.
    settle(&mut compositor);
    assert_eq!(
        compositor.window_frames(id_of(&compositor, "quiet")),
        quiet_frames
    );
    assert_eq!(compositor.window_frames(drawing_id), drawing_frames);

    // One client draws, and only its window's count follows.
    drawing.show(200, 100, [0, 255, 0, 255]);
    pump(&mut compositor, &mut [&mut drawing], |compositor, _| {
        compositor.window_frames(drawing_id) > drawing_frames
    });
    assert_eq!(
        compositor.window_frames(id_of(&compositor, "quiet")),
        quiet_frames,
        "the window that drew nothing keeps its count"
    );
    quiet.show(200, 100, [0, 0, 255, 255]);
    pump(&mut compositor, &mut [&mut quiet], |compositor, _| {
        compositor.window_frames(id_of(compositor, "quiet")) > quiet_frames
    });
}
