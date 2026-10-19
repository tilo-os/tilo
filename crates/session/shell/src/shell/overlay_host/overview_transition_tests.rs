use super::*;
use ui::widget::Container;

fn host() -> ShellOverlayHost {
    ShellOverlayHost::new(Box::new(Container::new()))
}

#[test]
fn overview_panel_curve_is_settled_monotonic_and_keeps_windows_inset() {
    /// A window at the strip's max width still has to stop short of the
    /// display edge, or its border/corners/shadow — which live in the last
    /// few pixels — get clipped by the bezel. Both edges keep the same
    /// gutter so the window reads as centred in its canvas.
    ///
    /// The gutter itself is pinned to the spacing scale: 2026-09-17 moved
    /// it from 28 px (off-grid, S1-21) to `s8`. 32 is also the pointer
    /// floor the bar controls already sit on, so the edge margin and a
    /// control's hit rect are one measure.
    fn a_full_width_window_stops_short_of_both_screen_edges() {
        assert_eq!(
            STRIP_EDGE_GUTTER_PX,
            f64::from(ui::tokens::Spacing::BASE.s8),
            "the edge gutter is a spacing step, not a tuned literal",
        );
        assert_eq!(STRIP_TRAILING_GUTTER_PX, STRIP_EDGE_GUTTER_PX);
        let mut h = host();
        h.set_output_geometry(&[OutputGeometry::unscaled(0.0, 0.0, 1600.0, 900.0)]);

        let spans = h.strip_output_spans();
        assert_eq!(spans.len(), 1, "one output in, one span out");
        let span = spans[0];

        // Where the strip starts placing groups: past the leading edge gutter.
        let leading = span.x + STRIP_EDGE_GUTTER_PX;
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
            (trailing - STRIP_EDGE_GUTTER_PX).abs() < 1.0,
            "trailing gutter is {trailing}px against a {STRIP_EDGE_GUTTER_PX}px \
             leading gutter; they should read as the same margin",
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

fn motion(reduced: bool) -> ui::tokens::EffectiveMotion {
    ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), reduced)
}

fn millis(ms: u64) -> std::time::Duration {
    std::time::Duration::from_millis(ms)
}

#[test]
fn the_overview_zoom_flies_reverses_from_where_it_is_drawn_and_follows_its_input() {
    use ui::tokens::MotionSource;
    let durations = ui::tokens::Motion::default().durations;

    // Closed and asked to stay closed: nothing moves, nothing asks for frames.
    let mut h = host();
    assert_eq!(
        h.sync_overview_zoom(false, MotionSource::Pointer, motion(false)),
        0.0
    );
    assert!(!h.overview_zoom_moving(ui::time::now()));

    // A click on "Overview": a whole-view change, at most `slow`, and it
    // asks for frames on the animation clock until it lands exactly.
    let before = ui::time::now();
    h.sync_overview_zoom(true, MotionSource::Pointer, motion(false));
    assert!(
        h.overview_zoom.is_open(),
        "input acts on the open overview at once"
    );
    assert!(h.needs_animation_frame());
    let rest = h
        .overview_zoom
        .settles_at()
        .expect("the windows are flying");
    assert!(rest.saturating_duration_since(before) <= durations.slow() + millis(1));
    assert_eq!(h.overview_zoom.progress(rest), 1.0);
    assert!(!h.overview_zoom.is_moving(rest), "then it costs nothing");

    // Toggled again mid-flight, it reverses from where the windows are.
    let mid = before + millis(120);
    let drawn = h.overview_zoom.progress(mid);
    assert!(drawn > 0.0 && drawn < 1.0, "mid-flight: {drawn}");
    h.overview_zoom.show(
        false,
        ui::motion::Zoom::OVERVIEW,
        motion(false),
        MotionSource::Pointer,
        mid,
    );
    assert!(
        (h.overview_zoom.progress(mid) - drawn).abs() < 1e-9,
        "no jump"
    );

    // Super+Tab: still a flight, as spatial context, never longer than `fast`.
    let mut keyed = host();
    let before = ui::time::now();
    keyed.sync_overview_zoom(true, MotionSource::Keyboard, motion(false));
    let rest = keyed
        .overview_zoom
        .settles_at()
        .expect("a key still flies them");
    assert!(rest.saturating_duration_since(before) <= durations.fast() + millis(1));

    // Reduced motion: a click fades at `fast`, a key lands.
    let mut reduced = host();
    let before = ui::time::now();
    reduced.sync_overview_zoom(true, MotionSource::Pointer, motion(true));
    let rest = reduced.overview_zoom.settles_at().expect("a fade");
    assert!(rest.saturating_duration_since(before) <= durations.fast() + millis(1));
    let mut reduced_key = host();
    assert_eq!(
        reduced_key.sync_overview_zoom(true, MotionSource::Keyboard, motion(true)),
        1.0
    );
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
    // Each draw samples the clock, so hold the zoom halfway.
    fn mid_fade(host: &mut ShellOverlayHost) {
        host.overview_zoom = ui::motion::Presence::held_at(500);
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

    // The close has landed.
    host.overview_zoom.snap(false);
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

    // The open's first sample is still the instant the chord landed, so
    // progress is 0: it paints nothing yet, and is no dense frame either.
    host.overview_zoom = ui::motion::Presence::held_at(0);
    let first = overview_section(&mut host, &mut renderer, &theme, true);
    assert!(
        overview_damage(&closed, &first).is_empty(),
        "the chord's own sample changes no pixel"
    );

    // Open and settled, then halfway through the close: one display.
    host.overview_zoom.snap(true);
    let open = overview_section(&mut host, &mut renderer, &theme, true);
    on_first_display(&overview_damage(&first, &open), "open");
    host.overview_zoom = ui::motion::Presence::held_at(500);
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
    host.sync_overview_zoom(true, ui::tokens::MotionSource::Pointer, motion(false));
    let rest = host
        .overview_zoom
        .settles_at()
        .expect("the windows are flying");
    host.defer_running_transitions(ui::tokens::Motion::default().durations.slow() + millis(50));
    assert!(
        host.overview_zoom.settles_at().expect("still flying") > rest,
        "time spent waiting on a full presenter is not overview progress"
    );
    assert!(
        host.transition_present_pending(),
        "the open stays pending until a frame is presented"
    );

    host.start_workspace_switch_frames(ui::tokens::MotionSource::Pointer);
    // The run has already ended; crediting the presenter's wait brings it back.
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
fn a_key_flies_the_windows_briefly_and_input_acts_on_the_new_state_at_once() {
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;
    use winit::event::Modifiers;
    use winit::keyboard::ModifiersState;

    let fast = ui::tokens::Motion::default().durations.fast();
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
    let mut chord = |h: &mut ShellOverlayHost, repeat: bool| {
        let pressed = ui::time::now();
        assert!(h.handle_shell_keyboard_event(
            &tab,
            super_key,
            repeat,
            &doc,
            &automerge::ROOT,
            &mut handled,
        ));
        pressed
    };

    // Super+Tab opens it at once and flies the windows for `fast` at most.
    let pressed = chord(&mut h, false);
    assert!(chrome.borrow().overlays.overview);
    assert!(h.overview_zoom.is_open());
    assert_eq!(h.overview_selection, h.overview_here_index());
    let rest = h.overview_zoom.settles_at().expect("the windows fly in");
    assert!(rest.saturating_duration_since(pressed) <= fast + millis(1));
    assert!(h.needs_animation_frame(), "the flight asks for frames");

    // A held toggle cannot close it.
    chord(&mut h, true);
    assert!(chrome.borrow().overlays.overview);
    assert!(h.overview_zoom.is_open());

    // A second chord closes it at once, the next opens it again.
    for open in [false, true] {
        chord(&mut h, false);
        assert_eq!(chrome.borrow().overlays.overview, open);
        assert_eq!(h.overview_zoom.is_open(), open);
    }

    // Arrow + Enter goes to another workspace: the overview is closed the
    // moment the key is handled, whatever the windows still show.
    assert!(h.handle_overview_keyboard_nav(&Key::Named(NamedKey::ArrowDown), false, &mut handled));
    let selected = h
        .overview_selection
        .expect("the next workspace is selected");
    let destination = chrome.borrow().tree().desktop.workspaces[selected].id;
    assert!(h.handle_overview_keyboard_nav(&Key::Named(NamedKey::Enter), false, &mut handled));
    assert_eq!(chrome.borrow().tree().desktop.active, destination);
    assert!(!chrome.borrow().overlays.overview);
    assert!(!h.overview_zoom.is_open());

    // Opened with a click, it is a whole-view change; Escape closes it.
    chrome.borrow_mut().overlays.overview = true;
    let clicked = ui::time::now();
    h.latch_overview_transition_from(ui::tokens::MotionSource::Pointer);
    let rest = h.overview_zoom.settles_at().expect("the windows fly in");
    assert!(rest.saturating_duration_since(clicked) > fast);
    assert!(h.handle_shell_escape_chord(&mut handled));
    assert!(!chrome.borrow().overlays.overview);
    assert!(!h.overview_zoom.is_open());
}
