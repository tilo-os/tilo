use super::*;
use ui::widget::Container;

fn host() -> ShellOverlayHost {
    ShellOverlayHost::new(Box::new(Container::new()))
}

#[test]
fn a_full_width_window_keeps_its_gutters() {
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

    a_full_width_window_stops_short_of_both_screen_edges();
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
    // Each start is pinned to the instant it is measured from, so a slow
    // machine cannot stretch the flight it reads back.
    let before = ui::time::now();
    ui::time::testing::with_monotonic_time(before, || {
        h.sync_overview_zoom(true, MotionSource::Pointer, motion(false))
    });
    assert!(
        h.overview_zoom.is_open(),
        "input acts on the open overview at once"
    );
    assert!(h.needs_animation_frame());
    let rest = h
        .overview_zoom
        .settles_at()
        .expect("the windows are flying");
    assert!(rest.saturating_duration_since(before) <= durations.slow());
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
    ui::time::testing::with_monotonic_time(before, || {
        keyed.sync_overview_zoom(true, MotionSource::Keyboard, motion(false))
    });
    let rest = keyed
        .overview_zoom
        .settles_at()
        .expect("a key still flies them");
    assert!(rest.saturating_duration_since(before) <= durations.fast());

    // Reduced motion: a click fades at `fast`, a key lands.
    let mut reduced = host();
    let before = ui::time::now();
    ui::time::testing::with_monotonic_time(before, || {
        reduced.sync_overview_zoom(true, MotionSource::Pointer, motion(true))
    });
    let rest = reduced.overview_zoom.settles_at().expect("a fade");
    assert!(rest.saturating_duration_since(before) <= durations.fast());
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
    // What a frame does before it draws: the flag followed, the app laid
    // out over the display it covers.
    if let Some(chrome) = host.chrome.clone() {
        chrome.borrow_mut().overlays.overview = open;
    }
    host.update(&AutoCommit::new(), &automerge::ROOT);
    let _ = crate::shell::test_support::lay_out_in_viewport(host, 3200.0, 1080.0, theme);
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
    mid_fade(&mut host);
    let again = overview_section(&mut host, &mut renderer, &theme, true);
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

    host.start_workspace_switch(ui::tokens::MotionSource::Pointer);
    host.defer_running_transitions(std::time::Duration::from_millis(
        u64::from(ui::tokens::Durations::default().slow_ms) + 50,
    ));
    assert!(
        host.transition_present_pending(),
        "a workspace switch stays pending until its frame is presented"
    );
    host.frame_presented();
    assert!(
        !host.workspace_switch_owed(),
        "a presented frame pays what the switch owed"
    );
}

/// A pointer switch's slide is held like the overview's zoom while the
/// presenter is full: the windows resume where they stood instead of
/// jumping to where the clock says they should be by now.
#[test]
fn a_full_presenter_does_not_finish_a_workspace_slide() {
    let mut host = host();
    let now = ui::motion::frame_now();
    host.workspace_slide = workspace_slide::WorkspaceSlide::start(
        7,
        true,
        ui::tokens::EffectiveMotion::default(),
        ui::tokens::MotionSource::Pointer,
        now,
    );
    assert!(host.workspace_slide_moving(now), "a pointer switch slides");
    // Far past any slide's length.
    let later = now + std::time::Duration::from_secs(5);
    assert!(
        !host.workspace_slide_moving(later),
        "test setup: the slide ends within five seconds"
    );
    let held = std::time::Duration::from_secs(10);
    host.defer_running_transitions(held);
    assert!(
        host.workspace_slide_moving(later),
        "time spent waiting on a full presenter is not slide progress"
    );
    assert!(
        !host.workspace_slide_moving(later + held),
        "a held slide still comes to rest"
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
    tree.create_hosted_workspace("Second");
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
        assert!(ui::time::testing::with_monotonic_time(pressed, || {
            h.handle_shell_keyboard_event(
                &tab,
                super_key,
                repeat,
                &doc,
                &automerge::ROOT,
                &mut handled,
            )
        }));
        pressed
    };

    // Super+Tab opens it at once and flies the windows for `fast` at most.
    let pressed = chord(&mut h, false);
    assert!(chrome.borrow().overlays.overview);
    assert!(h.overview_zoom.is_open());
    let rest = h.overview_zoom.settles_at().expect("the windows fly in");
    assert!(rest.saturating_duration_since(pressed) <= fast);
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

    // Going to another workspace from it closes the overview the moment it
    // is chosen, whatever the windows still show.
    let destination = chrome.borrow().tree().desktop.workspaces[1].id;
    h.carry_out_overview_pick(tilo_overview::OverviewPick::Workspace(destination));
    assert_eq!(chrome.borrow().tree().desktop.active, destination);
    assert!(!chrome.borrow().overlays.overview);
    assert!(!h.overview_zoom.is_open());

    // Opened with a click, it is a whole-view change; Escape closes it.
    chrome.borrow_mut().overlays.overview = true;
    let clicked = ui::time::now();
    ui::time::testing::with_monotonic_time(clicked, || {
        h.latch_overview_transition_from(ui::tokens::MotionSource::Pointer);
    });
    let rest = h.overview_zoom.settles_at().expect("the windows fly in");
    assert!(rest.saturating_duration_since(clicked) > fast);
    assert!(h.handle_shell_escape_chord(&mut handled));
    assert!(!chrome.borrow().overlays.overview);
    assert!(!h.overview_zoom.is_open());
}

/// A window on the display's screen flies into the overview (and the strip
/// stops drawing it meanwhile, so it is never seen twice); one off screen or
/// with no size fades in at its place in the picture instead.
#[test]
fn only_a_window_on_screen_flies_into_the_overview() {
    let output = Rect::new(0.0, 0.0, 1600.0, 900.0);
    assert!(ShellOverlayHost::flies_from(
        Rect::new(100.0, 40.0, 900.0, 860.0),
        output
    ));
    assert!(
        ShellOverlayHost::flies_from(Rect::new(1200.0, 40.0, 2000.0, 860.0), output),
        "partly on screen still flies from where it is seen"
    );
    assert!(!ShellOverlayHost::flies_from(
        Rect::new(1700.0, 40.0, 2500.0, 860.0),
        output
    ));
    assert!(!ShellOverlayHost::flies_from(
        Rect::new(-900.0, 40.0, -100.0, 860.0),
        output
    ));
    assert!(!ShellOverlayHost::flies_from(
        Rect::new(100.0, 40.0, 100.0, 860.0),
        output
    ));
}

/// Super+Up and Super+Down are told to the desktop as keyboard motion
/// before they act, so the window lands maximized or restored at once.
#[test]
fn maximize_and_restore_keys_land() {
    use settings_store::hardware::ShortcutAction as A;
    for action in [A::MaximizeWindow, A::RestoreWindow, A::ToggleMaximize] {
        assert!(
            ShellOverlayHost::is_navigation_shortcut(action),
            "{action:?}"
        );
    }
}

/// A host over the populated desktop with the overview's app mounted and
/// the desktop lent to it.
fn mounted() -> (ShellOverlayHost, crate::shell::chrome_state::Chrome) {
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;

    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.create_hosted_workspace("Second");
    let chrome = ChromeState::new(tree);
    let mut h = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(chrome.clone());
    h.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    h.update(&AutoCommit::new(), &automerge::ROOT);
    assert!(
        h.layers.overview.is_mounted(),
        "mounted before it is ever opened"
    );
    (h, chrome)
}

fn lent(h: &ShellOverlayHost) -> tilo_overview::OverviewDesk {
    h.layers.overview.seat().desk().1
}

/// The overview is lent every workspace, and the one you are in with each
/// strip under its own name and every window the switcher lists; the
/// strips this machine names can be renamed and moved from it.
#[test]
fn the_overview_is_lent_every_workspace_and_this_ones_strips_and_windows() {
    let (mut h, chrome) = mounted();
    let second = chrome
        .borrow_mut()
        .create_strip("Research")
        .expect("a strip");
    h.update(&AutoCommit::new(), &automerge::ROOT);
    let desk = lent(&h);
    let (workspaces, strips, windows) = {
        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.desktop.active_workspace().expect("a workspace");
        (
            tree.desktop.workspaces.len(),
            workspace
                .strips
                .iter()
                .map(|strip| (strip.id, strip.name.clone()))
                .collect::<Vec<_>>(),
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .map(|group| group.tabs.len())
                .sum::<usize>(),
        )
    };
    assert_eq!(desk.cards.len(), workspaces);
    let here = desk
        .cards
        .iter()
        .find(|card| card.is_current)
        .expect("here");
    assert_eq!(
        here.strips
            .iter()
            .map(|strip| (strip.id, strip.name.clone()))
            .collect::<Vec<_>>(),
        strips,
        "one row per strip, in the workspace's order, under its own name"
    );
    let pictured: usize = here
        .strips
        .iter()
        .flat_map(|strip| &strip.groups)
        .map(|group| group.tabs.len())
        .sum();
    assert_eq!(pictured, windows, "every window is pictured");
    assert_eq!(desk.pictured.len(), windows, "and drawn by the compositor");
    assert!(
        desk.renamable.contains(&second),
        "this machine names its strips"
    );
    assert!(desk.can_add && desk.can_join);
}

/// What the overview asks is carried out through the shell's own
/// commands: a strip renamed and moved is the strip the switcher lists, a
/// new workspace or the directory leaves the overview for it.
#[test]
fn what_the_overview_asks_is_carried_out_through_the_shells_commands() {
    use tilo_overview::OverviewPick;
    let (mut h, chrome) = mounted();
    chrome
        .borrow_mut()
        .create_strip("Research")
        .expect("a strip");
    chrome.borrow_mut().focus_strip_index(0);
    let strips = |chrome: &crate::shell::chrome_state::Chrome| {
        chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| {
                workspace
                    .strips
                    .iter()
                    .map(|strip| (strip.id, strip.name.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    chrome.borrow_mut().overlays.overview = true;
    h.update(&AutoCommit::new(), &automerge::ROOT);
    assert!(h.overview_open(), "the app follows the flag up");

    let first = strips(&chrome)[0].0;
    h.carry_out_overview_pick(OverviewPick::RenameStrip {
        strip: first,
        name: "Studio".into(),
    });
    assert_eq!(strips(&chrome)[0].1, "Studio");
    h.carry_out_overview_pick(OverviewPick::MoveStrip {
        strip: first,
        before: None,
    });
    assert_eq!(strips(&chrome).last().map(|(id, _)| *id), Some(first));
    assert!(chrome.borrow().overlays.overview, "the overview stays up");

    h.carry_out_overview_pick(OverviewPick::JoinWorkspace);
    assert_eq!(
        chrome.borrow_mut().take_peer_sync_requests(),
        vec![PeerSyncAction::BrowseDirectory],
        "the directory of public workspaces, not the paste field"
    );
    assert!(!chrome.borrow().overlays.overview);

    chrome.borrow_mut().overlays.overview = true;
    h.update(&AutoCommit::new(), &automerge::ROOT);
    let before = chrome.borrow().tree().desktop.workspaces.len();
    h.carry_out_overview_pick(OverviewPick::NewWorkspace);
    assert_eq!(chrome.borrow().tree().desktop.workspaces.len(), before + 1);
    assert!(
        !chrome.borrow().overlays.overview,
        "creating a workspace leaves the overview so navigation reaches it"
    );
}

/// The app taking itself down (Escape, a choice, a press on nothing) clears
/// the overview's flag; the flag going down takes the app down, and the
/// pictures land back before it is gone.
#[test]
fn the_overview_and_its_flag_follow_each_other() {
    use tilo_sheets::{Phase, Seat as _};
    let (mut h, chrome) = mounted();
    chrome.borrow_mut().overlays.overview = true;
    h.latch_overview_transition();
    h.update(&AutoCommit::new(), &automerge::ROOT);
    let presentation = h.layers.overview.seat().presentation().clone();
    assert_eq!(presentation.phase(), Phase::Open);

    presentation.dismissed();
    h.update(&AutoCommit::new(), &automerge::ROOT);
    assert!(!chrome.borrow().overlays.overview, "it took itself down");
    assert!(!h.overview_zoom.is_open(), "the windows fly back");

    chrome.borrow_mut().overlays.overview = true;
    h.latch_overview_transition();
    h.update(&AutoCommit::new(), &automerge::ROOT);
    assert_eq!(presentation.phase(), Phase::Open, "and comes up again");
    // Drawn open, so its pictures have somewhere to fly back from.
    h.force_overview_settled(true);
    chrome.borrow_mut().overlays.overview = false;
    h.latch_overview_transition();
    h.update(&AutoCommit::new(), &automerge::ROOT);
    assert_eq!(
        presentation.phase(),
        Phase::Leaving,
        "the shell took it down"
    );
    assert!(!chrome.borrow().overlays.overview, "and it stays down");
    h.force_overview_settled(false);
    assert_eq!(presentation.phase(), Phase::Closed, "landed, it is gone");
}
