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

fn window(id: u64, app: &str) -> Window {
    Window {
        id,
        app_id: Some(app.into()),
        title: Some(app.into()),
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
