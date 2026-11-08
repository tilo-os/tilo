//! A window's close button closes exactly that window.
//! Three windows stand in one strip, and later two windows of one app. Each shows its own close
//! control at the trailing end of its own bar. A press there is the window's, not the strip's or a
//! sibling's: the window that leaves is the one whose control was pressed and its neighbours stay,
//! and that holds at the display's scale as at 1.

use super::*;
use crate::navigation::DeskEffect;
use ui::widget::TileContent;

/// The windows' content: each tab shows the notice its title names.
struct Notices;

impl TileSource for Notices {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

/// A desk of `windows` windows, each an app of its own, and the frame that draws it at `scale` .
pub(super) fn desk_at(windows: usize, scale: f64) -> (ui::headless::FrameHarness, DesktopHandle) {
    let mut minter = IdMinter::new(7);
    let tabs: Vec<Tab> = (0..windows)
        .map(|i| Tab {
            id: minter.mint(),
            title: format!("App{i}"),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: format!("so.tilo.app{i}"),
                label: format!("App{i}"),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        })
        .collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices));
    let mut frame = ui::headless::FrameHarness::for_root_with(
        ui::state::State::new(),
        surface,
        (1280, 800),
        ui::headless::FrameOptions::default(),
    );
    frame
        .set_input_scale(scale)
        .expect("a display's scale is one a frame lays out at");
    for _ in 0..4 {
        frame.relayout();
    }
    (frame, handle)
}

/// The windows the desk holds, in strip order: each window's id and the tab it shows in front.
pub(super) fn windows(frame: &ui::headless::FrameHarness) -> Vec<(GroupId, TabId)> {
    frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .expect("the desk shows its strip")
        .groups
        .iter()
        .map(|group| (group.id, group.active_tab_id()))
        .collect()
}

/// Where `group` 's own close control is drawn, in the desk's logical pixels: the control a person
/// presses to close that window.
pub(super) fn close_of(frame: &ui::headless::FrameHarness, group: GroupId) -> Point {
    let bounds = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .expect("the desk shows its strip")
        .groups
        .iter()
        .find(|held| held.id == group)
        .expect("the window is drawn")
        .presentation_bounds();
    bounds
        .close
        .expect("a window offers its own close")
        .center()
}

/// Click at logical `at` as the person does: the pointer moves there and the left button goes down
/// and up over it. `FrameHarness::click_at` speaks the desk's logical pixels and scales them to the
/// display, as the compositor hands a press to the desk that stands on it.
pub(super) fn click(frame: &mut ui::headless::FrameHarness, at: Point) {
    frame.click_at(at.x, at.y);
}

/// The frames a close takes to drop the window from the desk: the window fades out on the clock it
/// animates by, then the row prunes it.
pub(super) fn settle(frame: &mut ui::headless::FrameHarness) {
    let mut now = ui::time::now();
    for _ in 0..40 {
        now = now.checked_add(Duration::from_millis(16)).expect("a step");
        ui::time::testing::with_monotonic_time(now, || frame.relayout());
    }
}

/// Three windows in one strip, each its own app: pressing the second's close button leaves the
/// first and the third, and only the second goes.
#[test]
fn the_second_windows_close_button_closes_only_that_window() {
    let _settled = ui::motion::testing::settled();
    let (mut frame, _handle) = desk_at(3, 1.0);
    let before = windows(&frame);
    assert_eq!(before.len(), 3, "three windows stand in the strip");
    let (group, tab) = before[1];
    let at = close_of(&frame, group);
    click(&mut frame, at);
    settle(&mut frame);

    let after = windows(&frame);
    assert_eq!(after.len(), 2, "one window went: {after:?}");
    assert!(
        !after.iter().any(|(held, _)| *held == group),
        "the window whose close was pressed is the one that went"
    );
    assert!(
        !after.iter().any(|(_, shown)| *shown == tab),
        "and no sibling took its place"
    );
    assert_eq!(
        after.first().map(|(held, _)| *held),
        Some(before[0].0),
        "the window before it stays"
    );
    assert_eq!(
        after.last().map(|(held, _)| *held),
        Some(before[2].0),
        "the window after it stays"
    );
}

/// The same press on a 2 display: the control is where its window's bar laid it out, so a press at
/// its centre closes that window and no other.
#[test]
fn a_windows_close_button_closes_only_that_window_at_scale_two() {
    let _settled = ui::motion::testing::settled();
    let (mut frame, _handle) = desk_at(3, 2.0);
    let before = windows(&frame);
    assert_eq!(before.len(), 3, "three windows stand in the strip");
    let (group, tab) = before[1];
    let at = close_of(&frame, group);
    click(&mut frame, at);
    settle(&mut frame);

    let after = windows(&frame);
    assert_eq!(after.len(), 2, "one window went: {after:?}");
    assert!(
        !after.iter().any(|(held, _)| *held == group),
        "the window whose close was pressed is the one that went"
    );
    assert!(
        !after.iter().any(|(_, shown)| *shown == tab),
        "and no sibling took its place"
    );
    assert_eq!(
        after.first().map(|(held, _)| *held),
        Some(before[0].0),
        "the window before it stays"
    );
    assert_eq!(
        after.last().map(|(held, _)| *held),
        Some(before[2].0),
        "the window after it stays"
    );
}

/// Closing by the desk's own action names the window it closes: `close-tab` with one tab's id takes
/// that window and no other.
#[test]
fn closing_by_the_action_with_a_window_id_closes_only_that_window() {
    let _settled = ui::motion::testing::settled();
    let (mut frame, handle) = desk_at(3, 1.0);
    let before = windows(&frame);
    let (_, tab) = before[1];

    handle
        .dispatch(DeskEffect::CloseTab {
            tab: tab.to_string(),
        })
        .expect("the desk closes the window it is handed");
    settle(&mut frame);

    let after = windows(&frame);
    assert_eq!(after.len(), 2, "one window went: {after:?}");
    assert!(
        !after.iter().any(|(_, shown)| *shown == tab),
        "the window the action named is the one that went"
    );
    assert_eq!(
        after.iter().map(|(held, _)| *held).collect::<Vec<_>>(),
        vec![before[0].0, before[2].0],
        "its neighbours stay"
    );
}

/// A window of an app that has a second window closes alone: two windows of one app are two windows
/// of the desk, and closing one leaves the other.
#[test]
fn closing_a_window_of_an_app_with_two_windows_leaves_the_other() {
    let _settled = ui::motion::testing::settled();
    let mut minter = IdMinter::new(7);
    let tabs: Vec<Tab> = (0..3)
        .map(|i| Tab {
            id: minter.mint(),
            title: format!("Notes {i}"),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: "so.tilo.notes".to_owned(),
                label: "Notes".to_owned(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        })
        .collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let (surface, _handle) = DesktopSurface::new(tree, 7, Box::new(Notices));
    let mut frame = ui::headless::FrameHarness::for_root_with(
        ui::state::State::new(),
        surface,
        (1280, 800),
        ui::headless::FrameOptions::default(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let before = windows(&frame);
    assert_eq!(before.len(), 3);
    let (group, tab) = before[1];
    let at = close_of(&frame, group);
    click(&mut frame, at);
    settle(&mut frame);

    let after = windows(&frame);
    assert_eq!(
        after.len(),
        2,
        "closing one window of a two-window app leaves the other: {after:?}"
    );
    assert!(
        !after.iter().any(|(held, _)| *held == group),
        "the pressed window went"
    );
    assert!(
        !after.iter().any(|(_, shown)| *shown == tab),
        "and it is the one the press named"
    );
    assert!(
        after.iter().any(|(_, shown)| *shown == before[2].1),
        "the app's other window stays"
    );
}

/// The close button's target is the control, not the window's whole chrome: a press at the bar's
/// leading end, where the tab's own pill begins, leaves every window where it was. The strip
/// reveals a pressed window, so the press aims at the geometry the frame shows then, as the pointer
/// the person aims does.
#[test]
fn a_press_beside_the_close_button_does_not_close_a_window() {
    let _settled = ui::motion::testing::settled();
    let (mut frame, _handle) = desk_at(3, 2.0);
    let (group, _) = windows(&frame)[1];
    let bounds = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .expect("the desk shows its strip")
        .groups
        .iter()
        .find(|held| held.id == group)
        .expect("the window is drawn")
        .presentation_bounds();
    let close = bounds.close.expect("a window offers its own close");
    let bar = bounds.tab_bar;
    let at = Point::new(bar.x0 + 6.0, bar.center().y);
    let maximize = bounds.maximize.expect("a window offers maximize");
    assert!(
        !close.contains(at) && !maximize.contains(at),
        "the bar's leading end is not one of the window's controls: {at:?}"
    );
    let before = windows(&frame).len();
    click(&mut frame, at);
    settle(&mut frame);
    assert_eq!(
        windows(&frame).len(),
        before,
        "a press at the bar's leading end, on the tab's own pill, at {at:?} is not the window's \
         close: every window stays"
    );
}
