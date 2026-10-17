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
        let widest = f64::from(ui::widget::WIDTH_WIDE_PERMILLE) / 1000.0 * span.usable;
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

/// A desktop of two displays side by side, with the chrome that says which
/// one has focus: the overview opens on that display, as it does live.
fn two_display_host() -> ShellOverlayHost {
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;

    let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
    let mut host =
        ShellOverlayHost::with_fixture_content(Box::new(Container::new())).with_chrome(chrome);
    host.rect = Rect::new(0.0, 0.0, 3200.0, 1080.0);
    host.set_output_geometry(&[
        OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0).with_id(0),
        OutputGeometry::unscaled(1920.0, 0.0, 1280.0, 800.0).with_id(1),
    ]);
    assert_eq!(
        host.overlay_area(),
        Rect::new(0.0, 0.0, 1920.0, 1080.0),
        "the overview opens on the focused first display"
    );
    host
}

/// The overview drawn into a tracked section, as the frame builder draws it,
/// with the parts the retained-damage producer compares.
fn overview_section(
    host: &mut ShellOverlayHost,
    renderer: &mut TextRenderer,
    theme: &Theme,
    open: bool,
) -> (Scene, ui::scene_parts::SectionParts) {
    // What the layout pass does before every draw: the panel's metrics for
    // the display it is on.
    host.resolve_overview_layout(theme);
    let mut scene = Scene::new();
    let ((), parts) = ui::scene_parts::record(&mut scene, |scene| {
        host.draw_overview_overlay(scene, Affine::IDENTITY, renderer, theme, open);
    });
    (scene, parts.expect("the overview's parts are well nested"))
}

/// The damage between two overview frames: what the producer hands the
/// presenter. A dense frame (no proof) fails here.
fn overview_damage(
    previous: &(Scene, ui::scene_parts::SectionParts),
    current: &(Scene, ui::scene_parts::SectionParts),
) -> Vec<Rect> {
    ui::scene_parts::damage(&previous.0, &previous.1, &current.0, &current.1)
        .unwrap_or_else(|why| panic!("an overview frame must stay sparse, not dense: {why:?}"))
}

/// Damage that repaints the display the overview dims, and not the other.
fn on_first_display(damage: &[Rect], sample: &str) {
    assert!(!damage.is_empty(), "{sample}: the fade repaints something");
    for rect in damage {
        assert!(
            rect.x1 <= 1920.0,
            "{sample}: {rect:?} reaches past the first display into the other one"
        );
    }
}

#[test]
fn overview_transition_damages_one_display_not_both() {
    // Each draw samples the wall clock, so pin the fade halfway just before it.
    fn mid_fade(host: &mut ShellOverlayHost) {
        host.tick_overview_transition(1.0, false);
        host.transition_started = Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(OVERVIEW_TRANSITION_MS / 2))
                .expect("the clock is older than half a transition"),
        );
    }

    let mut host = two_display_host();
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    let closed = overview_section(&mut host, &mut renderer, &theme, false);
    assert!(
        closed.0.encoding().draw_tags.is_empty(),
        "a settled closed overview draws nothing"
    );
    mid_fade(&mut host);
    let opening = overview_section(&mut host, &mut renderer, &theme, true);
    on_first_display(&overview_damage(&closed, &opening), "mid-open");
    let fills = host.overview_panel_cache_fills();
    assert_eq!(fills, 1, "the first fade frame records the panel once");
    mid_fade(&mut host);
    let again = overview_section(&mut host, &mut renderer, &theme, true);
    assert_eq!(
        host.overview_panel_cache_fills(),
        fills,
        "the next fade frame reuses the panel recording"
    );
    for rect in overview_damage(&opening, &again) {
        assert!(rect.x1 <= 1920.0, "a later fade frame stays on one display");
    }

    host.tick_overview_transition(0.0, false);
    let started = host
        .transition_started
        .expect("closing the overview latches a transition");
    host.transition_started = Some(
        started
            .checked_sub(std::time::Duration::from_millis(OVERVIEW_TRANSITION_MS + 1))
            .expect("the clock is older than the transition"),
    );
    let finished = overview_section(&mut host, &mut renderer, &theme, false);
    on_first_display(&overview_damage(&again, &finished), "finished close");
    let settled = overview_section(&mut host, &mut renderer, &theme, false);
    assert!(
        overview_damage(&finished, &settled).is_empty(),
        "a settled closed overview repaints nothing"
    );
}

#[test]
fn both_overview_directions_damage_only_the_overview_display() {
    let mut host = two_display_host();
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    let closed = overview_section(&mut host, &mut renderer, &theme, false);

    // The open's first sample is still the millisecond the chord landed, so
    // progress is 0: it paints nothing yet, and is no dense frame either.
    host.tick_overview_transition(1.0, false);
    host.overview_t = 0.0;
    host.transition_from = 0.0;
    host.overview_target = 1.0;
    host.transition_started = Some(std::time::Instant::now());
    let first = overview_section(&mut host, &mut renderer, &theme, true);
    assert!(
        overview_damage(&closed, &first).is_empty(),
        "the chord's own sample changes no pixel"
    );

    // Open and settled, then halfway through the close: one display.
    host.overview_t = 1.0;
    host.overview_target = 1.0;
    host.transition_from = 1.0;
    host.transition_started = None;
    let open = overview_section(&mut host, &mut renderer, &theme, true);
    on_first_display(&overview_damage(&first, &open), "open");
    host.tick_overview_transition(0.0, false);
    let started = host
        .transition_started
        .expect("closing latches a transition");
    host.transition_started = Some(
        started
            .checked_sub(std::time::Duration::from_millis(OVERVIEW_TRANSITION_MS / 2))
            .expect("half a transition fits in the clock"),
    );
    let closing = overview_section(&mut host, &mut renderer, &theme, false);
    on_first_display(&overview_damage(&open, &closing), "mid-close");
    assert!(
        !closing.0.encoding().draw_tags.is_empty(),
        "a mid-close frame still draws the panel"
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

    host.start_workspace_switch_frames(ui::tokens::MotionSource::Pointer);
    // The run has already ended; crediting the presenter's wait brings it back.
    host.workspace_switch = ui::Transition::open();
    host.workspace_switch_until =
        std::time::Instant::now().checked_sub(std::time::Duration::from_millis(1));
    assert!(
        !host.workspace_switch_holding(),
        "a finished switch is not a frame run"
    );
    host.defer_running_transitions(std::time::Duration::from_millis(
        u64::from(ui::tokens::Durations::default().slow_ms) + 50,
    ));
    assert!(
        host.workspace_switch_holding(),
        "a workspace switch stays pending while the presenter is full"
    );
}

#[test]
fn keyboard_overview_arrives_immediately_and_pointer_motion_can_resume() {
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;
    use winit::event::Modifiers;
    use winit::keyboard::ModifiersState;

    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.append_empty_workspace("Second");
    let chrome = ChromeState::new(tree);
    let mut h = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(chrome.clone());
    h.set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
        .expect("default shortcut bindings are valid");
    let doc = AutoCommit::new();
    let mut handled = false;
    let super_key = Modifiers::from(ModifiersState::SUPER);
    let tab = Key::Named(NamedKey::Tab);

    assert!(h.handle_shell_keyboard_event(
        &tab,
        super_key,
        false,
        &doc,
        &automerge::ROOT,
        &mut handled,
    ));
    assert!(handled);
    assert!(chrome.borrow().overlays.overview);
    assert_eq!(h.overview_t, 1.0, "the first frame is fully arrived");
    assert!(h.transition_started.is_none());
    assert_eq!(h.overview_selection, h.overview_here_index());
    assert!(!h.needs_animation_frame());

    // A held toggle cannot close the panel or start decorative motion.
    assert!(h.handle_shell_keyboard_event(
        &tab,
        super_key,
        true,
        &doc,
        &automerge::ROOT,
        &mut handled,
    ));
    assert!(chrome.borrow().overlays.overview);
    assert_eq!(h.overview_t, 1.0);
    assert!(h.transition_started.is_none());

    // A second chord closes immediately, and the next one opens immediately.
    for target in [0.0, 1.0] {
        assert!(h.handle_shell_keyboard_event(
            &tab,
            super_key,
            false,
            &doc,
            &automerge::ROOT,
            &mut handled,
        ));
        assert_eq!(h.overview_t, target);
        assert!(h.transition_started.is_none());
        assert!(!h.needs_animation_frame());
    }

    // Arrow + Enter activates another workspace and leaves no closing fade.
    assert!(h.handle_overview_keyboard_nav(&Key::Named(NamedKey::ArrowDown), false, &mut handled));
    let selected = h
        .overview_selection
        .expect("the next workspace is selected");
    let destination = chrome.borrow().tree().desktop.workspaces[selected].id;
    assert!(h.handle_overview_keyboard_nav(&Key::Named(NamedKey::Enter), false, &mut handled));
    assert_eq!(chrome.borrow().tree().desktop.active, destination);
    assert!(!chrome.borrow().overlays.overview);
    assert_eq!(h.overview_t, 0.0);
    assert!(h.transition_started.is_none());

    // Pointer opening still animates. Escape cancels that flight immediately.
    chrome.borrow_mut().overlays.overview = true;
    h.latch_overview_transition();
    assert!(h.transition_started.is_some());
    assert!(h.handle_shell_escape_chord(&mut handled));
    assert!(!chrome.borrow().overlays.overview);
    assert_eq!(h.overview_t, 0.0);
    assert!(h.transition_started.is_none());

    // Settling a keyboard event never sets a persistent reduced-motion flag.
    chrome.borrow_mut().overlays.overview = true;
    h.latch_overview_transition();
    assert!(h.transition_started.is_some());
    assert_eq!(h.overview_target, 1.0);
}
