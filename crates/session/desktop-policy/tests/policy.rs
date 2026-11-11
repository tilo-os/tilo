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
use tilo_desktop_surface::navigation::{ChromeEffect, DeskEffect};
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

/// The tabs the desk holds, in order: each tab's id and what it shows.
fn tabs(policy: &DesktopPolicy) -> Vec<(u64, TileContent)> {
    policy.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| (tab.id, tab.tile.clone()))
            .collect()
    })
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
    let close = DeskEffect::CloseTab { tab: route.tab };
    policy.handle().dispatch(close).unwrap();
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
fn a_window_is_named_by_what_it_shows_and_by_the_person_above_that() {
    use ui::window_toolbar::{ToolbarLocation, WindowToolbar};
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 1, "so.tilo.notes");
    let tab = window_tab(&policy, 1);
    // The note the window shows, as its app puts it in the bar's location.
    let bar = Some(WindowToolbar {
        location: Some(ToolbarLocation {
            label: "Garden plan".into(),
            ..ToolbarLocation::default()
        }),
        ..WindowToolbar::default()
    });
    policy.handle().declare_bar(tab, 7, bar);
    settle(&mut driver);
    policy.retitle(1, "Notes");
    settle(&mut driver);
    assert_eq!(
        title_of(&policy, tab),
        "Garden plan",
        "the window's own title only stands where its app names nothing"
    );
    // The person's own name stands in front of it, and clearing it goes
    // back to what the window shows.
    let group = groups(&policy)
        .into_iter()
        .find(|(_, tabs, _)| tabs.contains(&tab))
        .map(|(group, _, _)| group.to_string())
        .unwrap();
    let rename = |name: &str| ChromeEffect::Rename {
        tab: None,
        group: Some(group.clone()),
        strip: None,
        workspace: None,
        name: name.into(),
    };
    policy.handle().dispatch(rename("Read later")).unwrap();
    settle(&mut driver);
    assert_eq!(title_of(&policy, tab), "Read later");
    policy.handle().dispatch(rename("")).unwrap();
    settle(&mut driver);
    assert_eq!(title_of(&policy, tab), "Garden plan");
    // Nothing shown to name: the window goes back to its own title.
    policy.handle().declare_bar(tab, 7, None);
    settle(&mut driver);
    policy.retitle(1, "Notes");
    settle(&mut driver);
    assert_eq!(title_of(&policy, tab), "Notes");
    // A window that names nothing keeps the name it has: the bar is never
    // left with no title in it.
    policy.retitle(1, "");
    settle(&mut driver);
    assert_eq!(title_of(&policy, tab), "Notes");
}

/// What the tab `tab` is called.
fn title_of(policy: &DesktopPolicy, tab: u64) -> String {
    policy.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|held| held.id == tab)
            .unwrap()
            .title
            .clone()
    })
}

#[test]
fn the_desk_names_the_tabs_it_has_for_apps_with_their_apps() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 4, "so.tilo.notes");
    map(&mut driver, &mut policy, 5, "org.gnome.Terminal");
    let notes = policy.tab_of(4).expect("the window has its tab");
    let terminal = policy.tab_of(5).expect("the window has its tab");
    assert_eq!(
        policy.app_tabs_in_front(),
        [
            (notes, "so.tilo.notes".to_owned()),
            (terminal, "org.gnome.Terminal".to_owned())
        ],
        "each window with the id its app announces itself by"
    );
    // A launch the desk holds a place for has a tab from the frame it is
    // asked for, and that tab is the app's window on its way: the next
    // launch of the app goes there instead of starting a second process.
    assert!(
        policy.hold_beside("so.tilo.notes", None, false),
        "a place beside the window the app has"
    );
    // The place shows a window that has not come, which never stops moving.
    frames(&mut driver, 2);
    let place = policy
        .app_tabs_in_front()
        .into_iter()
        .find_map(|(tab, app)| (tab != notes && app == "so.tilo.notes").then_some(tab))
        .expect("the place the launch holds is one of the app's tabs");
    assert!(policy.holds_a_place(place), "the tab shows the place");
    assert!(
        policy.focus_tab(place),
        "the place comes forward with the window that fills it"
    );
    // The launch started nothing: its place goes, and the app's own window is
    // the one the desk has.
    policy.release_held("so.tilo.notes");
    frames(&mut driver, 2);
    assert_eq!(
        policy.app_tabs_in_front(),
        [
            (notes, "so.tilo.notes".to_owned()),
            (terminal, "org.gnome.Terminal".to_owned())
        ]
    );
    policy.unmap(4);
    settle(&mut driver);
    assert_eq!(
        policy.app_tabs_in_front(),
        [(terminal, "org.gnome.Terminal".to_owned())],
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
        primary: false,
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

    let launch = ChromeEffect::OpenApp {
        app: "host:org.example.Probe.desktop".into(),
        beside: None,
        background: None,
        float: None,
    };
    policy.handle().dispatch(launch).unwrap();
    settle(&mut driver);

    let asked = policy.take_launches();
    assert_eq!(asked.len(), 1, "one launch for one open");
    assert_eq!(asked[0].app, "host:org.example.Probe.desktop");
    assert!(
        asked[0].tab.is_some(),
        "the desk holds the tab the app's window takes"
    );
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
fn a_desk_launch_holds_the_tab_the_apps_window_takes() {
    let (mut driver, mut policy) = mount();
    let launch = ChromeEffect::OpenApp {
        app: "so.tilo.notes".into(),
        beside: None,
        background: None,
        float: None,
    };
    policy.handle().dispatch(launch).unwrap();
    settle(&mut driver);

    let asked = policy.take_launches();
    assert_eq!(asked.len(), 1, "one launch for one open");
    let tab = asked[0]
        .tab
        .expect("the desk holds the tab the window takes");
    assert_eq!(
        tabs(&policy),
        vec![(
            tab,
            TileContent::AppPlaceholder {
                kind: tilo_desktop_model::AppKind::Other("so.tilo.notes".into()),
                label: "so.tilo.notes".into(),
            },
        )],
        "the app's place is there from this frame, in the tab the launch names"
    );
    assert_eq!(
        title_of(&policy, tab),
        "Notes",
        "the tab opens with the app's name, never with its id"
    );
    assert_eq!(
        policy.layout(),
        Layout::default(),
        "and no window is here before the app's is"
    );
}

#[test]
fn a_conversation_opens_beside_its_sender() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 1, "so.tilo.chat");
    let sender = window_tab(&policy, 1);
    assert!(
        policy.hold_window("so.tilo.chat", false),
        "the desk takes the place beside the sender"
    );
    settle(&mut driver);

    assert!(
        policy.take_launches().is_empty(),
        "the session starts the window the send landed in; the desk only holds its place"
    );
    let tab = tabs(&policy)
        .into_iter()
        .find(|(_, tile)| matches!(tile, TileContent::AppPlaceholder { .. }))
        .map(|(tab, _)| tab)
        .expect("the desk holds the tab the conversation's window takes");
    let made = policy.take_held_launches();
    assert_eq!(
        made.len(),
        1,
        "the session starts the window the place waits for"
    );
    assert_eq!(made[0].app, "so.tilo.chat");
    assert_eq!(
        made[0].tab, tab,
        "the tab the window takes is the instance it is known by"
    );
    let groups = groups(&policy);
    let (sender_group, _, _) = groups
        .iter()
        .find(|(_, tabs, _)| tabs.contains(&sender))
        .expect("the window that sent stands in a group");
    let (group, _, active) = groups
        .iter()
        .find(|(_, tabs, _)| tabs.contains(&tab))
        .expect("the conversation's tab sits in a group");
    assert_ne!(
        group, sender_group,
        "the conversation opens beside the sender, not in its window"
    );
    assert_eq!(*active, tab, "the conversation is the tab in front");
    let widths: Vec<tilo_desktop_model::WindowWidth> = policy.handle().read(|tree| {
        tree.workspaces
            .first()
            .and_then(|workspace| workspace.strips.first())
            .map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .map(|group| group.width)
                    .collect()
            })
            .unwrap_or_default()
    });
    assert_eq!(
        widths.len(),
        2,
        "the sender and the conversation stand side by side: {widths:?}"
    );
    assert!(
        widths
            .iter()
            .all(|width| matches!(width, tilo_desktop_model::WindowWidth::Fraction(_))),
        "the two share the strip, so the sender keeps what the person was reading in view: {widths:?}"
    );

    // It stands over the desk, where its strip gave it room: beside the
    // sender, never over the message list the person was reading.
    let place = policy
        .handle()
        .read(|tree| tree.workspaces.first()?.arrangement.float_of(*group));
    assert!(
        place.is_some(),
        "the conversation stands over the desk, in the workspace's record"
    );
    let sender_content = policy
        .content_rect(sender)
        .expect("the sender's content stands on the desk");
    let content = policy
        .content_rect(tab)
        .expect("the conversation's content stands on the desk");
    assert!(
        content.x0 >= sender_content.x1,
        "the conversation stands beside its sender, never over what the person was reading: \
         sender {sender_content:?}, conversation {content:?}"
    );
    assert_eq!(
        policy.float_covers().len(),
        0,
        "a place no window fills yet draws nothing over the desk"
    );

    // The session gives the place up when nothing came to fill it: the tab
    // goes, and the window it was held for has nowhere to land.
    policy.release_held("so.tilo.chat");
    settle(&mut driver);
    assert_eq!(
        tabs(&policy).len(),
        1,
        "the place held for a window that never came is gone"
    );
}

#[test]
fn a_floated_windows_cover_is_where_the_desk_draws_it() {
    let (mut driver, mut policy) = mount();
    map(&mut driver, &mut policy, 1, "so.tilo.chat");
    policy
        .handle()
        .dispatch(ChromeEffect::OpenApp {
            app: "so.tilo.notes".into(),
            beside: None,
            background: None,
            float: Some(true),
        })
        .unwrap();
    settle(&mut driver);
    assert!(
        policy.float_covers().is_empty(),
        "a place no window fills draws nothing over the desk"
    );
    // The window the desk held the place for takes it.
    map(&mut driver, &mut policy, 2, "so.tilo.notes");
    settle(&mut driver);
    let covers = policy.float_covers();
    assert_eq!(covers.len(), 1, "one window floats: {covers:?}");
    let (window, cover) = covers[0];
    assert_eq!(window, 2, "the cover is of the window that floats");
    // A window an app opens floating stands as a card beside the window it
    // opened next to: the cover is that card and what the desk draws round it.
    let (card_width, card_height) = ui::widget::FLOAT_CARD_SIZE;
    assert_eq!(
        (cover.width() > card_width, cover.height() > card_height),
        (true, true),
        "the whole of its bar and body and the lift it stands on, as the desk draws it: {cover:?}"
    );
    assert!(
        policy.layout().covers == covers,
        "the layout a surface publishes carries the covers"
    );
}

#[test]
fn a_window_that_names_no_title_is_tabbed_under_its_apps_name() {
    let (mut driver, mut policy) = mount();
    policy
        .handle()
        .dispatch(ChromeEffect::OpenApp {
            app: "so.tilo.notes".into(),
            beside: None,
            background: None,
            float: None,
        })
        .unwrap();
    settle(&mut driver);
    let tab = policy.take_launches()[0].tab.expect("a held tab");
    // A window that has not said what it shows takes the held tab under the
    // name of its app.
    policy
        .map(Window {
            title: None,
            ..window(1, "so.tilo.notes")
        })
        .unwrap();
    settle(&mut driver);
    assert_eq!(title_of(&policy, tab), "Notes");
    // The toolkit then labels the window with the app's unlocalised name:
    // the tab keeps the name this desk gives the app.
    policy.retitle(1, "Notes");
    settle(&mut driver);
    assert_eq!(title_of(&policy, tab), "Notes");
}

#[test]
fn a_launch_that_opens_no_window_takes_its_held_tab_back() {
    let (mut driver, mut policy) = mount();
    let launch = ChromeEffect::OpenApp {
        app: "so.tilo.notes".into(),
        beside: None,
        background: None,
        float: None,
    };
    policy.handle().dispatch(launch).unwrap();
    settle(&mut driver);
    let tab = policy.take_launches()[0].tab.expect("a held tab");

    // The session started nothing for it: the place it held goes.
    policy.release_tab(tab);
    settle(&mut driver);
    assert!(tabs(&policy).is_empty(), "the held tab is not left behind");
    assert_eq!(policy.layout(), Layout::default());
    assert!(
        policy.take_launches().is_empty(),
        "and nothing is started for it again"
    );
}

#[test]
fn an_app_that_did_not_start_says_why_in_its_tab_and_tries_again() {
    let (mut driver, mut policy) = mount();
    let launch = ChromeEffect::OpenApp {
        app: "so.tilo.notes".into(),
        beside: None,
        background: None,
        float: None,
    };
    policy.handle().dispatch(launch).unwrap();
    settle(&mut driver);
    let tab = policy.take_launches()[0].tab.expect("a held tab");
    assert!(
        policy.holds_a_place(tab),
        "no window came for it: the tab still holds the app's place"
    );

    // The launch could not start it: the tab the person opened stays, and
    // says what happened and why instead of vanishing.
    let answer = tilo_desktop_surface::outcome::Outcome::new("Notes could not open")
        .saying("it is not installed here")
        .retryable("Try again");
    policy.failed_at(tab, "so.tilo.notes", answer);
    settle(&mut driver);
    let texts = ui::a11y::texts(driver.root_widget());
    assert!(
        texts.iter().any(|text| text == "Notes could not open"),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|text| text == "it is not installed here"),
        "{texts:?}"
    );
    assert_eq!(tabs(&policy).len(), 1, "the tab the person opened stays");
    assert_eq!(policy.layout(), Layout::default(), "no window is here");

    // Trying again asks for the app with the answer's own tab as its place,
    // so a window that comes fills it rather than opening a second one.
    driver
        .perform_named(
            tilo_desktop_surface::outcome::RETRY_ID,
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    settle(&mut driver);
    let asked = policy.take_launches();
    assert_eq!(asked.len(), 1, "Try again asks for the app once");
    assert_eq!(asked[0].tab, Some(tab), "in the tab the answer stands in");
    assert_eq!(tabs(&policy).len(), 1, "and opens no other tab");

    // The window comes: the answer gives its tab up to it.
    map(&mut driver, &mut policy, 41, "so.tilo.notes");
    settle(&mut driver);
    assert!(
        !policy.holds_a_place(tab),
        "the window filled the place the tab held"
    );
    assert_eq!(
        tabs(&policy),
        vec![(
            tab,
            TileContent::Foreign {
                app_id: "so.tilo.notes".into(),
                surface_id: 41,
            },
        )],
        "the window took the tab, and the answer is gone"
    );
}

#[test]
fn a_stopped_app_leaves_a_notice_that_asks_for_it_again() {
    let (mut driver, mut policy) = mount();
    let asked = "host:org.example.Probe.desktop";
    policy
        .stopped(
            asked,
            "Probe",
            tilo_desktop_surface::outcome::Closed::Crashed,
        )
        .unwrap();
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
        "the answer offers a way back: {texts:?}"
    );

    driver
        .perform_named(
            tilo_desktop_surface::outcome::RETRY_ID,
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    settle(&mut driver);
    let reopened = policy.take_launches();
    assert_eq!(
        reopened
            .iter()
            .map(|ask| ask.app.as_str())
            .collect::<Vec<_>>(),
        [asked],
        "Reopen asks for the same thing that was launched"
    );
    assert!(
        reopened[0].tab.is_some(),
        "the window that comes takes the tab the answer stands in"
    );
}

/// A window whose process was launched into a workspace that is not in front (an agent's open of
/// an app in its own workspace) opens there: the workspace in front shows only its own windows,
/// and a second launch of the same app in front finds no window of it there to raise.
#[test]
fn a_window_launched_into_another_workspace_opens_there() {
    use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceSlot};
    let (seeded, _) = DesktopPolicy::new(7);
    let mut tree = seeded.handle().read(Clone::clone);
    tree.workspaces
        .push(Workspace::new(WorkspaceSlot(2), "Garden"));
    let garden = tree.workspaces[1].id().to_string();
    let front = tree.active;
    let (mut policy, surface) = DesktopPolicy::with_tree(tree, 7);
    policy.set_viewport_width(WIDTH);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.relayout();
    policy.reads_launch_workspaces(std::rc::Rc::new(move |pid, app| {
        (pid == 50 && app == "so.tilo.calculator").then(|| garden.clone())
    }));
    policy
        .map(Window {
            pid: Some(50),
            ..window(5, "so.tilo.calculator")
        })
        .unwrap();
    settle(&mut driver);
    policy.carry_launched();
    settle(&mut driver);
    policy
        .map(Window {
            pid: Some(60),
            ..window(6, "so.tilo.notes")
        })
        .unwrap();
    settle(&mut driver);
    let placed = |id: u64| {
        policy.handle().read(|tree| {
            tree.workspaces
                .iter()
                .find(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .flat_map(|strip| &strip.window_groups)
                        .flat_map(|group| &group.tabs)
                        .any(|tab| matches!(tab.tile, TileContent::Foreign { surface_id, .. } if surface_id == id))
                })
                .map(|workspace| workspace.slot)
        })
    };
    assert_eq!(
        placed(5),
        Some(WorkspaceSlot(2)),
        "launched into Garden: opens there"
    );
    assert_eq!(
        placed(6),
        Some(front),
        "launched into the one in front: opens in front"
    );
    assert_eq!(
        policy.handle().read(|tree: &DesktopTree| tree.active),
        front,
        "the person stays"
    );
    assert!(
        policy
            .app_tabs_in_front()
            .iter()
            .all(|(_, app)| app != "so.tilo.calculator"),
        "the workspace in front shows no calculator of Garden's"
    );
}

/// What every notice on the desk says, by the window it stands for: the name and the description
/// of each accessibility node that describes itself.
fn notices(root: &dyn ui::widget::Widget) -> Vec<(String, String)> {
    fn node(access: &ui::a11y::AccessNode, out: &mut Vec<(String, String)>) {
        if let Some(said) = &access.description {
            out.push((access.name.clone().unwrap_or_default(), said.clone()));
        }
        for child in &access.children {
            node(child, out);
        }
    }
    fn visit(widget: &dyn ui::widget::Widget, out: &mut Vec<(String, String)>) {
        if let Some(access) = widget.accessibility() {
            node(&access, out);
        }
        widget.for_each_child(&mut |child| visit(child, out));
    }
    let mut out = Vec::new();
    visit(root, &mut out);
    out
}

/// What the one notice standing for the window called `title` says.
#[track_caller]
fn notice_of(driver: &ui::headless::FrameHarness, title: &str) -> String {
    let mut said: Vec<String> = notices(driver.root_widget())
        .into_iter()
        .filter(|(window, _)| window == title)
        .map(|(_, said)| said)
        .collect();
    said.dedup();
    assert_eq!(said.len(), 1, "one notice stands for {title}: {said:?}");
    said.remove(0)
}

/// What a window open on a device this one cannot name says.
const ELSEWHERE: &str = "Open on another device. Share the window there to see it on this one.";

/// A window of the workspace called `title`, open on the device that mints its ids under `salt`:
/// its tab as this device's desk holds it.
fn open_on(salt: u16, title: &str) -> tilo_desktop_model::Tab {
    tilo_desktop_model::Tab {
        id: tilo_desktop_model::ids::compose_id(salt, 1),
        title: title.into(),
        tile: TileContent::PeerWindow {
            app_id: "so.tilo.notes".into(),
            label: title.into(),
            note: String::new(),
            missing: None,
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

/// The desk of the device minting under `7` over `tree`, before its first frame.
fn desk_over(tree: tilo_desktop_model::DesktopTree) -> (ui::headless::FrameHarness, DesktopPolicy) {
    let (policy, surface) = DesktopPolicy::with_tree(tree, 7);
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
    (driver, policy)
}

/// The desk of the device minting under `7` holding `tabs`, each in a window of its own, before
/// its first frame.
fn desk_with(tabs: Vec<tilo_desktop_model::Tab>) -> (ui::headless::FrameHarness, DesktopPolicy) {
    let mut minter = tilo_desktop_model::IdMinter::new(7);
    desk_over(tilo_desktop_surface::seed_tree(&mut minter, "Desk", tabs))
}

/// A window of the workspace that runs on another device has no surface on
/// this one and none is on its way. Its tab says where the window is and what
/// brings its picture here: it does not stand empty, as a window still coming
/// does for a moment. A device this one heard no name of is another device.
#[test]
fn a_window_open_on_another_device_says_where_it_is() {
    let mut minter = tilo_desktop_model::IdMinter::new(7);
    let elsewhere = tilo_desktop_model::Tab {
        id: minter.mint(),
        title: "Quarterly plan".into(),
        tile: TileContent::PeerWindow {
            app_id: "so.tilo.notes".into(),
            label: "Quarterly plan".into(),
            note: String::new(),
            missing: None,
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let tab = elsewhere.id;
    let tree = tilo_desktop_surface::seed_tree(&mut minter, "Desk", vec![elsewhere]);
    let (mut driver, mut policy) = desk_over(tree);
    driver.relayout();
    settle(&mut driver);

    let texts = ui::a11y::texts(driver.root_widget());
    assert!(
        texts.iter().any(|text| text == "Quarterly plan"),
        "the tab names the window: {texts:?}"
    );
    assert_eq!(
        notice_of(&driver, "Quarterly plan"),
        ELSEWHERE,
        "the tab says where the window is and what brings it here"
    );
    assert_eq!(
        policy.layout(),
        Layout::default(),
        "no window of this device stands in its place"
    );
    assert_eq!(
        tabs(&policy).len(),
        1,
        "the window keeps its tab: {tab} is still the peer's"
    );

    // What another device is called says nothing of this window's.
    policy.name_devices([(0x21, "Ada's laptop".to_owned())].into());
    settle(&mut driver);
    assert_eq!(
        notice_of(&driver, "Quarterly plan"),
        ELSEWHERE,
        "a device nobody named stays another device"
    );
}

/// The tab of a window open on another device carries the id that device gave it. Once the
/// session heard what the device is called, the tab says which device the window is open on.
#[test]
fn a_window_open_on_a_device_the_workspace_heard_names_the_device() {
    let (mut driver, mut policy) = desk_with(vec![open_on(0x21, "Quarterly plan")]);
    policy.name_devices([(0x21, "Ada's laptop".to_owned())].into());
    driver.relayout();
    settle(&mut driver);

    assert_eq!(
        notice_of(&driver, "Quarterly plan"),
        "Open on Ada's laptop. Share the window there to see it on this one.",
        "the tab names the device the window is open on"
    );
    assert_eq!(
        policy.layout(),
        Layout::default(),
        "no window of this device stands in its place"
    );
    assert_eq!(tabs(&policy).len(), 1, "the window keeps its tab");
}

/// Each window open elsewhere names its own device, a window of a device nobody heard of names
/// none, and the tabs follow what the devices are called: a name heard after the tab was drawn
/// shows in it, a new name takes the old one's place, and a device no longer named is another
/// device again.
#[test]
fn each_window_open_elsewhere_names_its_own_device() {
    let (mut driver, mut policy) = desk_with(vec![
        open_on(0x21, "Quarterly plan"),
        open_on(0x22, "Budget"),
        open_on(0x23, "Roadmap"),
    ]);
    driver.relayout();
    settle(&mut driver);
    for window in ["Quarterly plan", "Budget", "Roadmap"] {
        assert_eq!(
            notice_of(&driver, window),
            ELSEWHERE,
            "no device is named before one is heard"
        );
    }

    policy.name_devices(
        [
            (0x21, "Ada's laptop".to_owned()),
            (0x22, "Grace's desktop".to_owned()),
        ]
        .into(),
    );
    settle(&mut driver);
    assert_eq!(
        notice_of(&driver, "Quarterly plan"),
        "Open on Ada's laptop. Share the window there to see it on this one."
    );
    assert_eq!(
        notice_of(&driver, "Budget"),
        "Open on Grace's desktop. Share the window there to see it on this one."
    );
    assert_eq!(
        notice_of(&driver, "Roadmap"),
        ELSEWHERE,
        "a device nobody heard of is another device"
    );

    // Ada renames her laptop, and Grace's desktop is no longer heard.
    policy.name_devices([(0x21, "Workshop".to_owned())].into());
    settle(&mut driver);
    assert_eq!(
        notice_of(&driver, "Quarterly plan"),
        "Open on Workshop. Share the window there to see it on this one."
    );
    assert_eq!(notice_of(&driver, "Budget"), ELSEWHERE);
    assert_eq!(notice_of(&driver, "Roadmap"), ELSEWHERE);
    assert_eq!(tabs(&policy).len(), 3, "every window keeps its tab");
}

/// A device's name is a person's text. The tab shows it as the text it is: nothing in it is read
/// as the catalogue's syntax, breaks the sentence over lines or turns the words around it, and a
/// name of nothing but space names no device.
#[test]
fn a_devices_name_is_shown_as_the_text_it_is() {
    let (mut driver, mut policy) = desk_with(vec![open_on(0x21, "Quarterly plan")]);
    driver.relayout();
    settle(&mut driver);

    policy.name_devices(
        [(
            0x21,
            "  { $device }\n{ -brand }\t\u{202E}<b>Ada</b>  ".to_owned(),
        )]
        .into(),
    );
    settle(&mut driver);
    assert_eq!(
        notice_of(&driver, "Quarterly plan"),
        "Open on { $device } { -brand } <b>Ada</b>. Share the window there to see it on this one."
    );

    policy.name_devices([(0x21, " \n\u{200F} ".to_owned())].into());
    settle(&mut driver);
    assert_eq!(
        notice_of(&driver, "Quarterly plan"),
        ELSEWHERE,
        "a name that shows nothing names no device"
    );
}
