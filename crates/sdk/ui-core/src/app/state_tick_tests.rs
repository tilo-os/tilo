// Included in `ordinary_confirmation_tests` (the Fixture widget and its
// managed root live there).
//
// An app window nobody draws still runs the passes a frame would run before
// it paints. On the desktop this is what an occluded window relies on: macOS
// delivers no `RedrawRequested` to a window on another Space, behind a
// full-screen app or otherwise not on screen, so work a wake queued would
// otherwise wait for a frame that never comes.

#[test]
fn a_window_that_is_never_drawn_still_runs_a_woken_state_pass() {
    let (widget, ledger, _) = fixture_widget(tilo_actions::ConfirmationPolicy::Confirm, source());
    let mut app =
        App::from_managed_root_with_controlled_inputs(State::new(), managed(Box::new(widget)))
            .unwrap();
    layout(&mut app);
    assert!(ledger.borrow().is_empty());
    // Something the app hears without a frame asked for the sync a frame
    // would have run.
    app.needs_sync = true;
    assert!(app.state_tick_pending());
    app.run_state_tick();
    assert!(
        !app.needs_sync,
        "the tick ran the sync the missing frame would have"
    );
    assert!(
        app.root_widget.get_rect().width() > 0.0,
        "the tree it synced is the one laid out"
    );
    assert!(ledger.borrow().is_empty(), "no invocation was waiting");
}

#[test]
fn a_window_with_nothing_pending_owes_no_state_pass() {
    let (widget, ledger, _) = fixture_widget(tilo_actions::ConfirmationPolicy::Confirm, source());
    let mut app =
        App::from_managed_root_with_controlled_inputs(State::new(), managed(Box::new(widget)))
            .unwrap();
    layout(&mut app);
    assert!(
        !app.state_tick_pending(),
        "a window a frame would draw leaves the passes to the frame"
    );
    app.run_state_tick();
    assert!(ledger.borrow().is_empty());
}
