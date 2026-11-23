// Included in `control_fixture_tests` (the Fixture widget and its
// managed root live there).
//
// An app window nobody draws still runs the passes a frame would run before
// it paints. On a computer this is what an occluded window relies on: macOS
// delivers no `RedrawRequested` to a window on another Space, behind a
// full-screen app or otherwise not on screen, so work a wake queued would
// otherwise wait for a frame that never comes.

#[test]
fn a_window_that_is_never_drawn_still_runs_a_woken_state_pass() {
    let (widget, log, _) = fixture_widget(tilo_actions::ConfirmationPolicy::Confirm, source());
    let mut app =
        App::from_managed_root_with_controlled_inputs(State::new(), managed(Box::new(widget)))
            .unwrap();
    layout(&mut app);
    assert!(log.borrow().is_empty());
    // Something the app hears without a frame asked for the sync a frame
    // would have run.
    app.needs_sync = true;
    app.run_state_tick();
    assert!(
        !app.needs_sync,
        "the tick ran the sync the missing frame would have"
    );
    assert!(
        app.root_widget.get_rect().width() > 0.0,
        "the tree it synced is the one laid out"
    );
    assert!(log.borrow().is_empty(), "no invocation was waiting");
}

/// A surface whose last frame's callback never comes (a hidden layer surface hears none) still
/// answers what it is asked: the windowing system holds every redraw back, and the state passes
/// run without one.
#[test]
fn a_surface_that_hears_no_frame_callback_answers_an_invoked_action() {
    use crate::render_thread::retry::{UNDRAWN_TICK_INTERVAL, UndrawnTicks};
    use std::sync::atomic::Ordering;
    let (widget, calls, _) = deferred_fixture();
    widget.resources.borrow_mut()[0].actions[0]
        .type_mut()
        .contract
        .confirmation = tilo_actions::ConfirmationPolicy::None;
    let log = widget.log.clone();
    let mut app =
        App::from_managed_root_with_controlled_inputs(State::new(), managed(Box::new(widget)))
            .unwrap();
    layout(&mut app);
    app.refresh_semantic_tree();
    let mut ticks = UndrawnTicks::default();
    ticks.presented();

    assert!(
        app.invoke_semantic_action(
            &crate::a11y_tree::SemanticId("r:ordinary:fixture".into()),
            "run",
            None
        )
        .unwrap()
    );
    assert!(
        app.needs_sync,
        "the invocation asks for the passes a frame runs"
    );
    let now = crate::time::now();
    assert!(
        !ticks.due(false, app.needs_sync, now),
        "a frame that comes in time runs them"
    );
    let later = now + UNDRAWN_TICK_INTERVAL;
    assert_eq!(ticks.deadline(), Some(later), "the loop wakes for them");
    assert!(ticks.due(false, app.needs_sync, later), "no frame came");
    app.run_state_tick();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        &*log.borrow(),
        &[serde_json::json!(0)],
        "answered without a frame"
    );
    assert!(!ticks.due(false, app.needs_sync, later + UNDRAWN_TICK_INTERVAL));

    // A window that draws leaves the passes to its frames.
    ticks.drew();
    assert!(!ticks.due(false, true, later));
    assert_eq!(ticks.deadline(), None);
}
