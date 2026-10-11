//! Guards the shared ease-out-cubic curve used by the workspace-settings
//! modal and the command palette (`ui::Transition::modal_curve`, which both
//! draw paths call) — keeps the *source* honest so a later refactor
//! can't silently regress the rest state or the animation's start scale.

use super::*;

/// A transition that has just begun opening, so `ui::Transition::modal_curve`
/// can be sampled anywhere across the enter window.
fn opening() -> ui::Transition {
    let mut t = ui::Transition::closed();
    t.set_open(
        true,
        Duration::from_millis(MODAL_OPEN_ANIM_MS),
        ui::Curve::EaseOut,
    );
    t
}

#[test]
fn modal_curve_rests_starts_and_settles_on_schedule() {
    fn modal_curve_is_at_rest_when_fully_open() {
        let t = ui::Transition::open();
        let (alpha, dy, scale) = t.modal_curve(Instant::now(), 0.95);
        assert_eq!(alpha, 1.0);
        assert_eq!(dy, 0.0);
        assert_eq!(scale, 1.0);
    }

    fn modal_curve_starts_from_initial_values() {
        // At t=0 the panel must be invisible, 20px low, and at its start
        // scale — otherwise it pops in rather than animating.
        let start = Instant::now();
        let t = opening();
        let (alpha, dy, scale) = t.modal_curve(start, 0.9);
        assert!(alpha < 0.05, "alpha at t=0 should be ~0, got {alpha}");
        assert!(
            (dy - 20.0).abs() < 0.5,
            "dy at t=0 should be ~20 px, got {dy}"
        );
        assert!(
            (scale - 0.9).abs() < 0.01,
            "scale at t=0 should equal start_scale 0.9, got {scale}"
        );
    }

    fn modal_curve_settles_at_rest_after_duration() {
        let start = Instant::now();
        let t = opening();
        let past = start + Duration::from_millis(MODAL_OPEN_ANIM_MS + 50);
        let (alpha, dy, scale) = t.modal_curve(past, 0.95);
        assert_eq!(alpha, 1.0);
        assert_eq!(dy, 0.0);
        assert_eq!(scale, 1.0);
    }

    modal_curve_is_at_rest_when_fully_open();
    modal_curve_starts_from_initial_values();
    modal_curve_settles_at_rest_after_duration();
}

#[test]
fn modal_transitions_pump_only_the_frames_they_need() {
    fn palette_exit_interpolates_not_pops() {
        // Deterministic proof (no wall-clock sleep) that the palette fades
        // out rather than popping, by sampling openness at explicit instants.
        let mut t = ui::Transition::open();
        t.set_open(
            false,
            Duration::from_millis(MODAL_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
        let start = Instant::now();
        let at_start = t.value(start);
        let at_mid = t.value(start + Duration::from_millis(MODAL_OPEN_ANIM_MS / 3));
        let at_end = t.value(start + Duration::from_millis(MODAL_OPEN_ANIM_MS + 50));
        assert!(at_start > 0.95, "close begins near fully-open: {at_start}");
        assert!(
            at_mid > 0.01 && at_mid < 0.99,
            "mid-close openness must be a partial fade, not a pop: {at_mid}"
        );
        assert!(at_end < 0.01, "close settles fully closed: {at_end}");
        // And ease-out means it's past the linear midpoint at 1/3 in.
        assert!(at_mid < at_start, "openness decreases while closing");
    }

    fn palette_open_transition_pumps_frames_and_is_visible() {
        use ui::widget::Container;
        let mut h = ShellOverlayHost::new(Box::new(Container::new()));
        // One instant for the whole test. Reading the clock afresh per
        // assertion made this a statement about machine load: the open
        // runs for MODAL_OPEN_ANIM_MS, so a scheduler stall longer than
        // that between starting it and asking whether it was running
        // let it finish first, and the last assertion flipped. That is
        // an ordinary stall in a loaded parallel run, and it is what
        // made this test flaky.
        let now = Instant::now();
        assert!(!h.palette_transition.is_visible(now));
        // Drive the open transition the way `draw` does.
        h.palette_transition.set_open_at(
            true,
            Duration::from_millis(MODAL_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
            now,
        );
        assert!(h.palette_transition.is_visible(now));
        assert!(
            h.needs_animation_frame_at(now),
            "an in-flight palette open must pump redraws"
        );
        // The pinned clock has teeth: asked past the animation window
        // the same host reports settled, so the assertion above is
        // reading the curve rather than returning true unconditionally.
        assert!(
            !h.needs_animation_frame_at(now + Duration::from_millis(MODAL_OPEN_ANIM_MS + 50)),
            "a settled palette must let the event loop go idle"
        );
    }

    fn osd_sleeps_while_opaque_and_pumps_only_its_fade_tail() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::osd::{OSD_FADE, OSD_FADE_IN, OSD_HOLD, Osd};
        use crate::shell::state::WorkspaceTree;
        use ui::widget::Container;

        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        let host = ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome.clone());
        let osd = Osd::brightness(0.4);
        let raised_at = osd.raised_at;
        chrome.borrow_mut().show_osd(osd);

        assert!(
            host.needs_animation_frame_at(raised_at),
            "the fade-in pumps its frames"
        );
        assert!(
            !host.needs_animation_frame_at(raised_at + OSD_FADE_IN),
            "the static hold must not repaint continuously"
        );
        let expiry = raised_at.checked_add(OSD_HOLD).unwrap();
        assert!(host.needs_animation_frame_at(expiry.checked_sub(OSD_FADE).unwrap()));
        assert!(
            host.needs_animation_frame_at(expiry.checked_sub(Duration::from_millis(1)).unwrap())
        );
        assert!(
            !host.needs_animation_frame_at(raised_at + OSD_HOLD),
            "the expired readout must let the compositor idle"
        );
    }

    /// The draw path consults `self.last_cursor` against each row rect, so
    /// the hit-test must stay honest with that geometry; `hit_test_overview`
    /// and `overview_row_rect` are the shared oracle.
    fn overview_hit_test_returns_card_index_when_cursor_is_over_card() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTree;
        use ui::widget::Container;

        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.append_empty_workspace("Second");
        tree.append_empty_workspace("Third");
        let chrome = ChromeState::new(tree);
        let mut h = ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome);
        h.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

        // Aim at the first row's centre via the same geometry helper the
        // draw path uses, so this test doesn't re-derive row math.
        let first = h.overview_row_rect(0);
        assert_eq!(h.hit_test_overview(first.center()), Some(0));

        // Second row should hit with index 1.
        let second = h.overview_row_rect(1);
        assert_eq!(h.hit_test_overview(second.center()), Some(1));

        // A point far outside the column (top-left corner) must return None.
        assert_eq!(h.hit_test_overview(Point::new(5.0, 5.0)), None);
    }

    palette_exit_interpolates_not_pops();
    palette_open_transition_pumps_frames_and_is_visible();
    osd_sleeps_while_opaque_and_pumps_only_its_fade_tail();
    overview_hit_test_returns_card_index_when_cursor_is_over_card();
}
