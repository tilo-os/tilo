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
        assert!(
            h.needs_animation_frame(),
            "the transition still asks for frames on the animation clock"
        );
        let now = std::time::Instant::now();
        if let Some(deadline) = h.next_wake_deadline() {
            assert!(
                deadline.saturating_duration_since(now) > std::time::Duration::from_millis(2),
                "a running overview transition must not wake the shell every millisecond"
            );
        }
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
        assert!(h.transition_started.is_none(), "transition should clear");
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

#[test]
fn overview_transition_uploads_the_panel_not_both_outputs() {
    // Each draw samples the wall clock, so pin the fade halfway just before it.
    fn mid_fade(host: &mut ShellOverlayHost) {
        host.tick_overview_transition(1.0, false);
        host.transition_started = Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(OVERVIEW_TRANSITION_MS / 2))
                .expect("the clock is older than half a transition"),
        );
    }

    let mut host = host();
    host.rect = Rect::new(0.0, 0.0, 3200.0, 1080.0);
    host.set_output_geometry(&[
        OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0),
        OutputGeometry::unscaled(1920.0, 0.0, 1280.0, 800.0),
    ]);
    mid_fade(&mut host);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    let mut scene = Scene::new();
    host.draw_overview_overlay(&mut scene, Affine::IDENTITY, &mut renderer, &theme, true);
    let patch = host
        .overview_fast_patch()
        .expect("a mid-fade overview publishes a panel patch");
    let desktop = host.rect.width() * host.rect.height();
    let panel = f64::from(patch.panel.width) * f64::from(patch.panel.height);
    assert!(
        panel * 4.0 < desktop,
        "the uploaded region is {panel} px on a {desktop} px desktop — that is both outputs, not the panel"
    );
    assert!(
        patch.panel.width < 1920,
        "the upload is one panel ({} px wide), not a 1920 or 3200 px scanout",
        patch.panel.width
    );
    let fills = host.overview_panel_cache_fills();
    assert_eq!(fills, 1, "the first fade frame records the panel once");
    let mut again = Scene::new();
    mid_fade(&mut host);
    host.draw_overview_overlay(&mut again, Affine::IDENTITY, &mut renderer, &theme, true);
    assert_eq!(
        host.overview_panel_cache_fills(),
        fills,
        "the next fade frame reuses the panel recording"
    );
    assert!(
        host.overview_fast_patch()
            .is_some_and(|patch| patch.draw_panel),
        "a mid-fade frame still draws the panel"
    );

    host.tick_overview_transition(0.0, false);
    let started = host
        .transition_started
        .expect("closing the overview latches a transition");
    host.transition_started = Some(
        started
            .checked_sub(std::time::Duration::from_millis(OVERVIEW_TRANSITION_MS + 1))
            .expect("the clock is older than the transition"),
    );
    let mut closing = Scene::new();
    host.draw_overview_overlay(&mut closing, Affine::IDENTITY, &mut renderer, &theme, false);
    let restore = host
        .overview_fast_patch()
        .expect("the frame that finishes the close restores the panel clip");
    assert!(
        !restore.draw_panel,
        "the closing frame puts the desktop back without rendering the panel"
    );
    assert!(
        restore.panel.width < 1920,
        "the restore stays inside the panel clip"
    );
    let mut settled = Scene::new();
    host.draw_overview_overlay(&mut settled, Affine::IDENTITY, &mut renderer, &theme, false);
    assert!(
        host.overview_fast_patch().is_none(),
        "a settled closed overview stops replacing the desktop"
    );
}

#[test]
fn a_full_presenter_does_not_finish_the_transition() {
    let mut host = host();
    host.tick_overview_transition(1.0, false);
    assert!(host.transition_started.is_some());
    host.defer_running_transitions(std::time::Duration::from_millis(
        OVERVIEW_TRANSITION_MS + 50,
    ));
    let progress = host.tick_overview_transition(1.0, false);
    assert!(
        progress < 1.0,
        "time spent waiting on a full presenter is not overview progress ({progress})"
    );
    assert!(
        host.transition_started.is_some(),
        "the open stays pending until a frame is presented"
    );

    host.start_workspace_switch_frames();
    // The run has already ended; crediting the presenter's wait brings it back.
    host.workspace_switch = ui::Transition::open();
    host.workspace_switch_until =
        std::time::Instant::now().checked_sub(std::time::Duration::from_millis(1));
    assert!(
        !host.workspace_switch_holding(),
        "a finished switch is not a frame run"
    );
    host.defer_running_transitions(std::time::Duration::from_millis(WORKSPACE_SWITCH_MS + 50));
    assert!(
        host.workspace_switch_holding(),
        "a workspace switch stays pending while the presenter is full"
    );
}

#[test]
fn both_overview_directions_stay_on_the_panel_clip() {
    fn panel_is_not_both_outputs(host: &ShellOverlayHost) {
        let patch = host
            .overview_fast_patch()
            .expect("this sample publishes a panel patch");
        let desktop = host.rect.width() * host.rect.height();
        let panel = f64::from(patch.panel.width) * f64::from(patch.panel.height);
        assert!(
            panel * 4.0 < desktop,
            "the uploaded region is {panel} px on a {desktop} px desktop"
        );
        assert!(
            patch.panel.width < 1920,
            "the upload is one panel ({} px wide), not a scanout",
            patch.panel.width
        );
    }

    let mut host = host();
    host.rect = Rect::new(0.0, 0.0, 3200.0, 1080.0);
    host.set_output_geometry(&[
        OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0),
        OutputGeometry::unscaled(1920.0, 0.0, 1280.0, 800.0),
    ]);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();

    // The open's first sample is still the millisecond the chord landed, so
    // progress is 0. That frame must clip to the panel, same as the close.
    host.tick_overview_transition(1.0, false);
    host.overview_t = 0.0;
    host.transition_from = 0.0;
    host.overview_target = 1.0;
    host.transition_started = Some(std::time::Instant::now());
    let mut opening = Scene::new();
    host.draw_overview_overlay(&mut opening, Affine::IDENTITY, &mut renderer, &theme, true);
    panel_is_not_both_outputs(&host);

    // Halfway through the close, still the panel, not both outputs.
    host.overview_t = 1.0;
    host.overview_target = 1.0;
    host.transition_from = 1.0;
    host.transition_started = None;
    host.tick_overview_transition(0.0, false);
    let started = host
        .transition_started
        .expect("closing latches a transition");
    host.transition_started = Some(
        started
            .checked_sub(std::time::Duration::from_millis(OVERVIEW_TRANSITION_MS / 2))
            .expect("half a transition fits in the clock"),
    );
    let mut closing = Scene::new();
    host.draw_overview_overlay(&mut closing, Affine::IDENTITY, &mut renderer, &theme, false);
    panel_is_not_both_outputs(&host);
    assert!(
        host.overview_fast_patch()
            .is_some_and(|patch| patch.draw_panel),
        "a mid-close frame still draws the panel"
    );
}
