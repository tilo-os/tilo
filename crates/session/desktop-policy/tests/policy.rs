//! The policy over the desktop surface in the toolkit's real frame
//! pipeline: windows mapped, focused, closed and moved between strips as the
//! desktop's widgets lay them out.

#![allow(
    clippy::unwrap_used,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use std::cell::Cell;
use std::time::Duration;

use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{GroupId, TileContent};
use tilo_desktop_policy::displays::Output;
use tilo_desktop_policy::{DesktopPolicy, Layout, Window};
use tilo_desktop_surface::navigation::Command;
use ui::kurbo::Rect;

const WIDTH: f64 = 1200.0;
const FRAME: Duration = Duration::from_millis(16);

thread_local! {
    static CLOCK: Cell<Option<ui::time::Instant>> = const { Cell::new(None) };
}

fn on_clock<T>(after: Duration, step: impl FnOnce() -> T) -> T {
    let at = CLOCK
        .get()
        .unwrap_or_else(ui::time::now)
        .checked_add(after)
        .unwrap();
    CLOCK.set(Some(at));
    tilo_clock::testing::with_monotonic_time(at, step)
}

fn mount() -> (ui::headless::FrameHarness, DesktopPolicy) {
    let (policy, surface) = DesktopPolicy::new(7);
    policy.set_viewport_width(WIDTH);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.set_caller_palette_enabled(false);
    driver.relayout();
    (driver, policy)
}

/// Frames until nothing on the desktop moves.
fn settle(driver: &mut ui::headless::FrameHarness) {
    let settled = (0..1_000).any(|_| {
        on_clock(FRAME, || driver.relayout());
        !driver.frame_schedule().needs_animation_frame
    });
    assert!(settled, "the desktop still moves after a thousand frames");
}

/// Step `count` frames: the placement is applied as the change lands, and
/// the fade it starts over the other windows plays out.
fn frames(driver: &mut ui::headless::FrameHarness, count: usize) {
    for _ in 0..count {
        on_clock(FRAME, || driver.relayout());
    }
}

fn window(id: u64, app: &str) -> Window {
    Window {
        id,
        app_id: Some(app.into()),
        title: Some(app.into()),
        ..Window::default()
    }
}

fn map(driver: &mut ui::headless::FrameHarness, policy: &mut DesktopPolicy, id: u64, app: &str) {
    policy.map(window(id, app)).unwrap();
    settle(driver);
}

fn shown(layout: &Layout) -> Vec<u64> {
    layout.shown.iter().map(|(id, _)| *id).collect()
}

#[test]
fn a_mapped_window_opens_focused_and_in_view_beside_the_last() {
    let (mut driver, mut policy) = mount();
    assert_eq!(policy.layout(), Layout::default());
    map(&mut driver, &mut policy, 10, "org.gnome.Terminal");
    map(&mut driver, &mut policy, 11, "org.mozilla.firefox");

    assert_eq!(policy.focused(), Some(11));
    let layout = policy.layout();
    let (_, rect) = layout.shown.iter().find(|(id, _)| *id == 11).unwrap();
    assert!(
        rect.x0 >= 0.0 && rect.x1 <= WIDTH + 0.5 && rect.width() > 0.0 && rect.height() > 0.0,
        "the newest window is in view: {layout:?}"
    );
    let mut all = shown(&layout);
    all.extend(&layout.hidden);
    all.sort_unstable();
    assert_eq!(all, [10, 11], "every window has a place: {layout:?}");
    if let Some((_, first)) = layout.shown.iter().find(|(id, _)| *id == 10) {
        assert!(
            first.x1 <= rect.x0 + 0.5,
            "the later one opens to the right"
        );
    }
}

#[test]
fn focus_brings_a_window_forward_and_unmap_gives_focus_back() {
    let (mut driver, mut policy) = mount();
    for (id, app) in [(1, "a"), (2, "b"), (3, "c")] {
        map(&mut driver, &mut policy, id, app);
    }
    assert!(policy.focus(1));
    settle(&mut driver);
    assert_eq!(policy.focused(), Some(1));
    assert!(shown(&policy.layout()).contains(&1));

    policy.unmap(1);
    settle(&mut driver);
    let layout = policy.layout();
    assert!(!shown(&layout).contains(&1) && !layout.hidden.contains(&1));
    assert!(policy.focused().is_some_and(|id| id != 1));
    assert!(!policy.focus(1), "a window gone has nothing to focus");
    assert!(policy.take_closed().is_empty(), "the window closed itself");
}

#[test]
fn a_window_the_person_closes_is_reported_once_for_the_client_to_close() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 1, "a");
    map(&mut driver, &mut policy, 2, "b");
    assert!(policy.take_closed().is_empty());

    let route = policy.handle().snapshot("g", vec![]).active.unwrap();
    let close: Command =
        serde_json::from_value(serde_json::json!({"kind":"close","generation":"g","route":route}))
            .unwrap();
    policy.handle().dispatch(&close, "g").unwrap();
    settle(&mut driver);

    assert_eq!(policy.take_closed(), [2]);
    assert!(policy.take_closed().is_empty());
    policy.unmap(2);
    assert!(policy.take_closed().is_empty());
}

#[test]
fn windows_on_a_strip_not_shown_are_hidden() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 1, "a");
    policy
        .handle()
        .apply_command(&DesktopCommand::CreateStrip {
            name: "Second".into(),
        })
        .unwrap();
    settle(&mut driver);
    map(&mut driver, &mut policy, 2, "b");

    let layout = policy.layout();
    assert_eq!(shown(&layout), [2]);
    assert_eq!(layout.hidden, [1]);
    assert_eq!(policy.focused(), Some(2));
}

#[test]
fn a_window_keeps_its_title_in_its_tab() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 1, "a");
    policy.retitle(1, "Report.odt");
    settle(&mut driver);
    let titles = policy.handle().read(|tree| {
        tree.workspaces[0]
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.title.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(titles, ["Report.odt"]);
}

#[test]
fn the_desk_names_the_windows_in_front_with_their_apps() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 4, "so.tilo.notes");
    map(&mut driver, &mut policy, 5, "org.gnome.Terminal");
    assert_eq!(
        policy.windows_in_front(),
        [
            (4, "so.tilo.notes".to_owned()),
            (5, "org.gnome.Terminal".to_owned())
        ],
        "each window with the id its app announces itself by"
    );
    policy.unmap(4);
    settle(&mut driver);
    assert_eq!(
        policy.windows_in_front(),
        [(5, "org.gnome.Terminal".to_owned())],
        "a window gone is not one a launch could raise"
    );
}

/// The desk's window groups in order: each with its id, its tabs and the tab
/// in front.
fn groups(policy: &DesktopPolicy) -> Vec<(GroupId, Vec<u64>, u64)> {
    policy.handle().read(|tree| {
        let strip = tree
            .workspaces
            .first()
            .and_then(|workspace| workspace.strips.first());
        strip
            .map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .map(|group| {
                        (
                            group.id,
                            group.tabs.iter().map(|tab| tab.id).collect(),
                            group.active_tab,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The tab the window `window` of the desk sits in.
fn window_tab(policy: &DesktopPolicy, window: u64) -> u64 {
    policy.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|tab| {
                matches!(tab.tile, TileContent::Foreign { surface_id, .. } if surface_id == window)
            })
            .unwrap()
            .id
    })
}

#[test]
fn an_apps_next_window_lands_right_after_its_own() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 10, "so.tilo.notes");
    let (anchor, group, index) = policy.anchor_for("so.tilo.notes").unwrap();
    assert_eq!(anchor, window_tab(&policy, 10));
    assert_eq!(index, 0);
    assert!(policy.place_next_beside("so.tilo.notes", Some(anchor), false));

    // The window announces the short name its package id ends with.
    map(&mut driver, &mut policy, 11, "notes");

    let (first, second) = (window_tab(&policy, 10), window_tab(&policy, 11));
    assert_eq!(
        groups(&policy),
        [(group, vec![first, second], second)],
        "the second window is the app's next tab, in front"
    );
    assert_eq!(policy.focused(), Some(11));
    let layout = policy.layout();
    assert_eq!(shown(&layout), [11], "the window in front is drawn");
    assert_eq!(layout.hidden, [10], "the tab behind it is not");
}

#[test]
fn a_window_placed_behind_takes_no_focus() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 10, "so.tilo.notes");
    map(&mut driver, &mut policy, 12, "org.gnome.Terminal");
    let anchor = policy.anchor_for("so.tilo.notes").unwrap();
    assert!(policy.place_next_beside("so.tilo.notes", Some(anchor.0), true));
    map(&mut driver, &mut policy, 11, "notes");

    let (first, second) = (window_tab(&policy, 10), window_tab(&policy, 11));
    let notes = groups(&policy)
        .into_iter()
        .find(|(_, tabs, _)| tabs.contains(&second))
        .unwrap();
    assert_eq!(notes.1, [first, second]);
    assert_eq!(notes.2, first, "the tab in front stays in front");
    assert_eq!(
        policy.focused(),
        Some(12),
        "the window the person worked in keeps the keyboard"
    );
    assert!(
        policy.layout().hidden.contains(&11),
        "a tab opened behind is not drawn"
    );
}

#[test]
fn a_placed_window_is_the_one_that_takes_the_place() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 10, "so.tilo.notes");
    assert!(policy.place_next_beside("so.tilo.notes", None, false));
    map(&mut driver, &mut policy, 11, "notes");
    map(&mut driver, &mut policy, 12, "notes");
    assert_eq!(
        groups(&policy).len(),
        2,
        "a place is taken by one window; the next lands as a launch does"
    );
}

#[test]
fn an_app_never_opens_beside_another_apps_tab() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 10, "so.tilo.notes");
    assert_eq!(
        policy.anchor_for("so.tilo.chat"),
        None,
        "no tab of the asking app to anchor on"
    );
    assert!(
        !policy.place_next_beside("so.tilo.chat", None, false),
        "no tab of its own to open beside"
    );
    map(&mut driver, &mut policy, 11, "so.tilo.chat");
    assert_eq!(groups(&policy).len(), 2, "it opens a window of its own");
}

#[test]
fn the_app_tab_in_front_answers_a_request_from_that_app() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 10, "so.tilo.notes");
    map(&mut driver, &mut policy, 11, "org.gnome.Terminal");
    assert_eq!(
        policy.anchor_for("so.tilo.notes").map(|(tab, ..)| tab),
        Some(window_tab(&policy, 10)),
        "another app's window in front: the app's own first tab answers"
    );
    assert!(policy.focus(10));
    settle(&mut driver);
    assert_eq!(
        policy.anchor_for("so.tilo.notes").map(|(tab, ..)| tab),
        Some(window_tab(&policy, 10))
    );
}

fn displays(policy: &mut DesktopPolicy) {
    let output = |monitor: &str, x: f64| Output {
        monitor: monitor.into(),
        rect: Rect::new(x, 0.0, x + WIDTH / 2.0, 800.0),
    };
    assert!(policy.set_displays(&[output("left", 0.0), output("right", WIDTH / 2.0)]));
}

#[test]
fn each_display_holds_the_windows_opened_while_it_had_focus() {
    let (mut driver, mut policy) = mount();
    displays(&mut policy);
    map(&mut driver, &mut policy, 1, "a");
    let order = policy.displays();
    assert_eq!(order.len(), 2);
    assert_eq!(policy.display_of(1), Some(order[0]));
    assert!(policy.handle().focus_display(order[1]));
    settle(&mut driver);
    map(&mut driver, &mut policy, 2, "b");
    assert_eq!(policy.display_of(2), Some(order[1]));
    let layout = policy.layout();
    let place = |id| layout.shown.iter().find(|(w, _)| *w == id).unwrap().1;
    assert!(place(1).x1 <= WIDTH / 2.0 + 0.5, "{layout:?}");
    assert!(place(2).x0 >= WIDTH / 2.0, "{layout:?}");

    assert!(policy.handle().step_focus(-1));
    settle(&mut driver);
    assert_eq!(policy.focused(), Some(1), "past the display's first window");
    assert!(policy.handle().move_focused_to_display(1));
    settle(&mut driver);
    assert_eq!(policy.display_of(1), Some(order[1]));
    assert_eq!(policy.focused(), Some(1));
}

#[test]
fn a_window_taken_full_screen_covers_its_display() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 1, "a");
    map(&mut driver, &mut policy, 2, "b");
    let place = |layout: &Layout, id: u64| {
        layout
            .shown
            .iter()
            .find(|(window, _)| *window == id)
            .unwrap()
            .1
    };
    let before = place(&policy.layout(), 1);
    assert!(
        before.width() < WIDTH - 0.5,
        "two windows share the strip: {before:?}"
    );
    assert_eq!(policy.layout().fullscreen, None);

    assert!(policy.set_fullscreen(1, true), "the desktop holds it");
    frames(&mut driver, 30);
    let layout = policy.layout();
    assert_eq!(layout.fullscreen, Some(1), "the desk says which one covers");
    let cover = place(&layout, 1);
    assert!(
        cover.x0.abs() < 0.5
            && cover.y0.abs() < 0.5
            && (cover.x1 - WIDTH).abs() < 0.5
            && cover.height() > before.height() + 0.5,
        "the display from its top edge to its bottom: {cover:?} against {before:?}"
    );

    assert!(policy.set_fullscreen(1, false));
    frames(&mut driver, 30);
    let layout = policy.layout();
    assert_eq!(layout.fullscreen, None, "the display is given back");
    let back = place(&layout, 1);
    assert!(
        (back.x0 - before.x0).abs() < 0.5 && (back.width() - before.width()).abs() < 0.5,
        "the window stands where it stood: {back:?} against {before:?}"
    );
}

#[test]
fn a_dialog_floats_over_its_parent_and_answers_for_it() {
    let (mut driver, mut policy) = mount();
    displays(&mut policy);
    map(&mut driver, &mut policy, 1, "a");
    policy
        .map(Window {
            parent: Some(1),
            ..window(2, "a")
        })
        .unwrap();
    settle(&mut driver);
    assert!(policy.is_floating(2));
    let layout = policy.layout();
    assert_eq!(shown(&layout), [1], "a dialog takes no tab");
    let parent = layout.shown[0].1;
    assert_eq!(layout.floating, [(2, parent)], "over its parent");
    assert_eq!(policy.focused(), Some(2), "the dialog has the keyboard");
    assert!(policy.focus(2), "focusing it brings its parent");

    policy.unmap(1);
    settle(&mut driver);
    let layout = policy.layout();
    assert_eq!(
        layout.floating,
        [(2, Rect::new(0.0, 0.0, WIDTH / 2.0, 800.0))],
        "its parent gone, it floats on the display in use"
    );
    policy.unmap(2);
    assert!(policy.layout().floating.is_empty());
}

#[test]
fn a_launch_with_no_window_behind_it_is_queued_for_the_session() {
    let (mut driver, mut policy) = mount();
    assert!(policy.take_launches().is_empty());

    let launch: Command = serde_json::from_value(serde_json::json!({
        "kind": "launch", "generation": "g", "app": "host:org.example.Probe.desktop"
    }))
    .unwrap();
    policy.handle().dispatch(&launch, "g").unwrap();
    settle(&mut driver);

    assert_eq!(policy.take_launches(), ["host:org.example.Probe.desktop"]);
    assert!(
        policy.take_launches().is_empty(),
        "a launch is handed over once"
    );
    assert_eq!(
        policy.layout(),
        Layout::default(),
        "nothing appeared before the app did"
    );
}

#[test]
fn a_crashed_app_leaves_a_notice_that_asks_for_it_again() {
    let (mut driver, mut policy) = mount();
    let asked = "host:org.example.Probe.desktop";
    policy.crashed(asked, "Probe").unwrap();
    settle(&mut driver);
    assert!(
        policy.take_launches().is_empty(),
        "the notice itself asks for nothing"
    );

    let texts = ui::a11y::texts(driver.root_widget());
    assert!(
        texts
            .iter()
            .any(|text| text.starts_with("Probe stopped working")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|text| text == "Reopen"),
        "the notice offers a way back: {texts:?}"
    );

    driver
        .perform_named(
            tilo_desktop_surface::crashed_app::REOPEN_ID,
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    settle(&mut driver);
    assert_eq!(
        policy.take_launches(),
        [asked],
        "Reopen asks for the same thing that was launched"
    );
}
