use super::*;
use ui::widget::Container;

fn host() -> ShellOverlayHost {
    ShellOverlayHost::new(Box::new(Container::new()))
}

#[test]
fn overview_panel_curve_is_settled_monotonic_and_keeps_windows_inset() {
    /// A window at the strip's max width still has to stop short of the
    /// display edge, or its border/corners/shadow — which live in the last
    /// few pixels — get clipped by the bezel. Mirrors the gutter the near
    /// edge already reserves beside the wheel disc.
    ///
    /// The gutter itself is pinned to the spacing scale: 2026-09-17 moved
    /// it from 28 px (off-grid, S1-21) to `s8`. `s8` rather than `s6`
    /// because the disc's rim shadow reaches 26 px past its footprint and
    /// `s6` (24) would lay it over the first window; 32 is also the
    /// pointer floor the bar controls already sit on, so the wheel-to-
    /// window margin and a control's hit rect are one measure.
    fn a_full_width_window_stops_short_of_both_screen_edges() {
        assert_eq!(
            WHEEL_TO_STRIP_GAP_PX,
            f64::from(ui::tokens::Spacing::BASE.s8),
            "the wheel gutter is a spacing step, not a tuned literal",
        );
        const {
            assert!(
                WHEEL_TO_STRIP_GAP_PX > tilo_desktop::sidebar_wheel_disc::RIM_SHADOW_REACH,
                "the gutter must hold the disc's rim shadow with wallpaper to spare",
            );
        }
        assert_eq!(STRIP_TRAILING_GUTTER_PX, WHEEL_TO_STRIP_GAP_PX);
        let mut h = host().with_sidebar(Box::new(Container::new()));
        h.set_output_geometry(&[OutputGeometry::unscaled(0.0, 0.0, 1600.0, 900.0)]);

        let spans = h.strip_output_spans();
        assert_eq!(spans.len(), 1, "one output in, one span out");
        let span = spans[0];

        // Where the strip starts placing groups: past the wheel slot.
        let leading = f64::from(sidebar_width_for_viewport(900.0)) + WHEEL_TO_STRIP_GAP_PX;
        // The widest a group can be, and therefore where the widest one ends.
        let widest = f64::from(ui::widget::WIDTH_MAX_PERMILLE) / 1000.0 * span.usable;
        let right_edge = leading + widest;

        assert!(
            right_edge < span.x + span.width,
            "the widest window reaches x={right_edge} on a {}px display — it \
             has to stop short of the edge or its border and corners fall off \
             the screen",
            span.width,
        );
        // And the gap it leaves should match the one on the other side, so
        // the window looks centred in its canvas rather than shoved right.
        let trailing = span.x + span.width - right_edge;
        assert!(
            (trailing - WHEEL_TO_STRIP_GAP_PX).abs() < 1.0,
            "trailing gutter is {trailing}px against a {WHEEL_TO_STRIP_GAP_PX}px \
             gutter beside the wheel; they should read as the same margin",
        );
    }

    /// At rest the panel is either absent or fully arrived — no
    /// half-drawn surface sits on screen when nothing is animating.
    fn the_panel_curve_is_settled_at_both_ends() {
        let (alpha, dy, scale) = overview_panel_curve(0.0);
        assert_eq!(alpha, 0.0);
        assert_eq!(dy, OVERVIEW_PANEL_RISE_PX);
        assert_eq!(scale, OVERVIEW_PANEL_START_SCALE);

        let (alpha, dy, scale) = overview_panel_curve(1.0);
        assert_eq!(alpha, 1.0);
        assert_eq!(dy, 0.0);
        assert_eq!(scale, 1.0);
    }

    /// Mid-flight the panel is partly transparent, still low, and still
    /// small — all three moving together, so it reads as one object
    /// arriving rather than three properties animating.
    fn the_panel_curve_moves_alpha_rise_and_scale_together() {
        let (alpha, dy, scale) = overview_panel_curve(0.5);
        assert!(alpha > 0.0 && alpha < 1.0, "alpha mid-flight: {alpha}");
        assert!(
            dy > 0.0 && dy < OVERVIEW_PANEL_RISE_PX,
            "rise mid-flight: {dy}"
        );
        assert!(
            scale > OVERVIEW_PANEL_START_SCALE && scale < 1.0,
            "scale mid-flight: {scale}"
        );
    }

    /// The curve is monotonic, so the panel never backs up on its way in
    /// or stutters on its way out.
    fn the_panel_curve_never_reverses() {
        let mut prev = overview_panel_curve(0.0);
        for step in 1_i16..=20 {
            let cur = overview_panel_curve(f32::from(step) / 20.0);
            assert!(cur.0 >= prev.0, "alpha went backwards at {step}");
            assert!(cur.1 <= prev.1, "rise went backwards at {step}");
            assert!(cur.2 >= prev.2, "scale went backwards at {step}");
            prev = cur;
        }
    }

    a_full_width_window_stops_short_of_both_screen_edges();
    the_panel_curve_is_settled_at_both_ends();
    the_panel_curve_moves_alpha_rise_and_scale_together();
    the_panel_curve_never_reverses();
}

#[test]
fn overview_transition_latches_completes_reverses_and_snaps_with_reduced_motion() {
    fn idle_holds_zero_until_target_changes() {
        let mut h = host();
        for _ in 0..5 {
            let t = h.tick_overview_transition(0.0, false);
            assert_eq!(t, 0.0);
        }
    }

    fn target_change_latches_transition_and_completes() {
        let mut h = host();
        h.tick_overview_transition(1.0, false);
        assert!(h.transition_started.is_some(), "should latch transition");
        assert_eq!(h.overview_target, 1.0);
        // Past the full duration, value settles exactly at the target.
        // Backdate the latched start instead of sleeping through the
        // transition: `tick_overview_transition` measures `started.elapsed()`.
        let started = h
            .transition_started
            .expect("the toggle latched a transition");
        h.transition_started = Some(
            started
                .checked_sub(std::time::Duration::from_millis(
                    OVERVIEW_TRANSITION_MS + 30,
                ))
                .expect("the monotonic clock is older than one transition"),
        );
        let t = h.tick_overview_transition(1.0, false);
        assert!((t - 1.0).abs() < f32::EPSILON);
        assert!(
            h.transition_started.is_some(),
            "visual settle is not the same as the frame run: keep requesting"
        );
        for _ in 0..MIN_TRANSITION_FRAMES {
            let _ = h.tick_overview_transition(1.0, false);
        }
        assert!(
            h.transition_started.is_none(),
            "transition clears after the frame run, not the wall timer alone"
        );
    }

    fn mid_flight_re_toggle_picks_up_from_current_t() {
        // Flip target mid-fade; `transition_from` should capture the
        // current `overview_t`, not 0, so the reverse starts from there.
        let mut h = host();
        h.tick_overview_transition(1.0, false);
        // Backdate the latched start by half the transition instead of
        // sleeping for it; the tick reads `started.elapsed()`.
        let started = h
            .transition_started
            .expect("the toggle latched a transition");
        h.transition_started = Some(
            started
                .checked_sub(std::time::Duration::from_millis(OVERVIEW_TRANSITION_MS / 2))
                .expect("the monotonic clock is older than half a transition"),
        );
        let t_mid = h.tick_overview_transition(1.0, false);
        assert!(
            t_mid > 0.0 && t_mid < 1.0,
            "mid-flight t should be between 0 and 1, got {t_mid}"
        );
        h.tick_overview_transition(0.0, false);
        assert!(
            (h.transition_from - t_mid).abs() < 0.01,
            "reverse should start from current alpha, got from={} t_mid={}",
            h.transition_from,
            t_mid
        );
        assert_eq!(h.overview_target, 0.0);
    }

    fn reduced_motion_snaps_overview_to_each_target() {
        let mut h = host();
        assert_eq!(h.tick_overview_transition(1.0, true), 1.0);
        assert!(h.transition_started.is_none());
        assert_eq!(h.tick_overview_transition(0.0, true), 0.0);
        assert!(h.transition_started.is_none());
    }

    idle_holds_zero_until_target_changes();
    target_change_latches_transition_and_completes();
    mid_flight_re_toggle_picks_up_from_current_t();
    reduced_motion_snaps_overview_to_each_target();
}
