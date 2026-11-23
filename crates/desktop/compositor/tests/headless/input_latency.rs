//! The frame trace times each input to the frame that answers it, however the input reached the
//! desktop: a device's own events and another device's link are routed past
//! [`Compositor::input`], and a frame held for the display's blank still answers what came before.

use super::*;

/// Run `operation` at `instant` on the displays' clock.
fn at<T>(instant: Instant, operation: impl FnOnce() -> T) -> T {
    tilo_clock::testing::with_monotonic_time(instant, operation)
}

/// The turn just ended, as the frame trace has it.
fn last_turn(compositor: &Compositor) -> tilo_frame_metrics::FrameSample {
    *compositor
        .recorded_frames()
        .last()
        .expect("the turn is recorded")
}

tilo_test_env::scenario! {
    /// The frame that answers input carries how long after the input it was drawn.
    fn the_frame_that_answers_input_carries_its_latency;

    phase input_routed_past_the_api_is_timed_to_its_frame;
    // Another of the person's devices moves the pointer: it reaches the desktop through the
    // firewall, as a keyboard's and a pointer's own events reach it from the display backend,
    // without passing `Compositor::input`.
    {
        let mut compositor = headless(Box::new(StackPolicy));
        let mut client = mapped_client(
            &mut compositor,
            "org.example.answered",
            (200, 100),
            [0, 255, 0, 255],
        );
        compositor.draw_frame().expect("the window's first frame");
        compositor.record_frames();
        compositor.begin_tick();
        compositor
            .input_remote(InputEvent::PointerMotion { x: 50.0, y: 40.0 })
            .expect("the motion is routed");
        client.show(200, 100, [0, 0, 255, 255]);
        pump(&mut compositor, &mut [&mut client], |compositor, _| {
            compositor.owes_frame()
        });
        compositor.draw_frame().expect("the frame that answers it");
        compositor.end_tick();
        let turn = last_turn(&compositor);
        assert!(turn.painted, "the turn drew the answer: {turn:?}");
        assert!(
            turn.input_latency_us.is_some(),
            "the frame that answers the motion carries its latency: {turn:?}"
        );
    }

    phase a_frame_held_for_the_blank_still_answers_the_input_before_it;
    // A change that arrives between two blanks waits for the next one; the turns that find nothing
    // due yet do not close the input's measurement, the frame the blank shows does.
    {
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
        let mut config = OutputConfig::new("HEADLESS-1", 640, 480);
        // A tenth of a second per blank.
        config.refresh_mhz = 10_000;
        let grid = tilo_clock::now();
        at(grid, || compositor.add_output(&config)).expect("output");
        let mut client = mapped_client(
            &mut compositor,
            "org.example.held",
            (200, 100),
            [0, 255, 0, 255],
        );
        at(grid, || compositor.render()).expect("the window's first frame");
        compositor.record_frames();
        compositor.begin_tick();
        compositor
            .input_remote(InputEvent::PointerMotion { x: 60.0, y: 30.0 })
            .expect("the motion is routed");
        client.show(200, 100, [0, 0, 255, 255]);
        pump(&mut compositor, &mut [&mut client], |compositor, _| {
            compositor.owes_frame()
        });
        let due = compositor.frame_deadline().expect("the blank that shows it");
        let before = due
            .checked_sub(Duration::from_millis(1))
            .expect("an instant before the blank");
        let shown = compositor.frames_presented();
        at(before, || compositor.render()).expect("a turn before the blank");
        assert_eq!(compositor.frames_presented(), shown, "no frame before the blank");
        compositor.end_tick();
        compositor.begin_tick();
        at(due, || compositor.render()).expect("the frame the blank shows");
        compositor.end_tick();
        let turn = last_turn(&compositor);
        assert!(turn.painted, "the blank's turn drew the answer: {turn:?}");
        assert!(
            turn.input_latency_us.is_some(),
            "the frame the blank shows answers the motion before it: {turn:?}"
        );
    }
}
