//! The policy over the desktop's model, as the session runs it: windows mapped, focused,
//! closed and moved between strips as the desktop's own pass applies the verbs queued for it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{GroupId, TileContent, WindowWidth};
use tilo_desktop_policy::displays::Output;
use tilo_desktop_policy::{DesktopPolicy, Placement, Window};
use tilo_session_core::desktop::navigation::{ChromeEffect, LayoutEffect};
use ui::kurbo::Rect;
use ui::widget::{HANDLE_SLOT_PX, STRIP_EDGE_GUTTER_PX};

const WIDTH: f64 = 1200.0;

/// A desktop on a screen [`WIDTH`] wide, as the tests measure it.
fn mount() -> DesktopPolicy {
    let policy = DesktopPolicy::new(7);
    policy.set_viewport_width(WIDTH);
    policy
}

/// Passes of the desktop until nothing it shows changes.
fn settle(policy: &mut DesktopPolicy) {
    let settled = (0..1_000).any(|_| !policy.step());
    assert!(settled, "the desktop still changes after a thousand passes");
}

fn window(id: u64, app: &str) -> Window {
    Window {
        id,
        app_id: Some(app.into()),
        title: Some(app.into()),
        ..Window::default()
    }
}

fn map(policy: &mut DesktopPolicy, id: u64, app: &str) {
    policy.map(window(id, app)).unwrap();
    settle(policy);
}

/// The windows the policy shows, in the order it places them.
fn shown(layout: &Placement) -> Vec<u64> {
    layout.shown.iter().map(|(id, _)| *id).collect()
}

/// Where window `id` stands as of the last layout pass.
fn shown_rect(policy: &DesktopPolicy, id: u64) -> Rect {
    let placement = policy.placement();
    let (_, rect) = placement
        .shown
        .iter()
        .find(|(window, _)| *window == id)
        .unwrap_or_else(|| panic!("window {id} is shown: {placement:?}"));
    *rect
}

/// The windows the shell holds tabs of, in tree order.
fn windows(policy: &DesktopPolicy) -> Vec<u64> {
    policy.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter_map(|tab| match tab.tile {
                TileContent::Foreign { surface_id, .. } => Some(surface_id),
                _ => None,
            })
            .collect()
    })
}

/// The tabs the shell holds, in order: each tab's id and what it shows.
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

/// The shell's window groups in order: each with its id, its tabs and the tab
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

/// The window group the tab `tab` sits in.
fn group_of(policy: &DesktopPolicy, tab: u64) -> GroupId {
    policy
        .handle()
        .read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .find(|group| group.tabs.iter().any(|candidate| candidate.id == tab))
                .map(|group| group.id)
        })
        .expect("the tab sits in a window group")
}

/// The share of its strip the window group `group` keeps.
fn group_width(policy: &DesktopPolicy, group: GroupId) -> WindowWidth {
    policy
        .handle()
        .read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .find(|held| held.id == group)
                .map(|held| held.width)
        })
        .expect("the window group stands on a strip")
}

/// The tab the window `window` of the shell sits in.
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
fn a_mapped_window_opens_focused_and_in_view_beside_the_last() {
    let mut policy = mount();
    assert!(windows(&policy).is_empty(), "nothing is on the shell yet");
    assert_eq!(policy.placement(), Placement::default());
    map(&mut policy, 10, "org.gnome.Terminal");
    map(&mut policy, 11, "org.mozilla.firefox");

    assert_eq!(policy.focused(), Some(11));
    let first = policy.tab_of(10).expect("the first window has a tab");
    let second = policy.tab_of(11).expect("the second window has a tab");
    let order: Vec<(Vec<u64>, u64)> = groups(&policy)
        .into_iter()
        .map(|(_, tabs, active)| (tabs, active))
        .collect();
    assert_eq!(
        order,
        [(vec![first], first), (vec![second], second)],
        "each window stands in a window of its own, the later one to the right"
    );
    let layout = policy.placement();
    let rect = shown_rect(&policy, 11);
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
    let mut policy = mount();
    for (id, app) in [(1, "a"), (2, "b"), (3, "c")] {
        map(&mut policy, id, app);
    }
    assert!(policy.focus(1));
    settle(&mut policy);
    assert_eq!(policy.focused(), Some(1));
    assert!(shown(&policy.placement()).contains(&1));

    policy.unmap(1);
    settle(&mut policy);
    assert_eq!(policy.tab_of(1), None, "the window's tab is gone");
    let layout = policy.placement();
    assert!(!shown(&layout).contains(&1) && !layout.hidden.contains(&1));
    assert!(policy.focused().is_some_and(|id| id != 1));
    assert!(!policy.focus(1), "a window gone has nothing to focus");
    assert!(policy.take_closed().is_empty(), "the window closed itself");
}

#[test]
fn a_window_the_person_closes_is_reported_once_for_the_client_to_close() {
    let mut policy = mount();
    map(&mut policy, 1, "a");
    map(&mut policy, 2, "b");
    assert!(policy.take_closed().is_empty());

    let route = policy.handle().snapshot("g", vec![]).active.unwrap();
    let close = LayoutEffect::CloseTab { tab: route.tab };
    policy.handle().dispatch(close).unwrap();
    settle(&mut policy);

    assert_eq!(policy.take_closed(), [2]);
    assert!(policy.take_closed().is_empty());
    policy.unmap(2);
    assert!(policy.take_closed().is_empty());
}

#[test]
fn windows_on_a_strip_not_shown_are_hidden() {
    let mut policy = mount();
    map(&mut policy, 1, "a");
    policy
        .handle()
        .apply_command(&DesktopCommand::CreateStrip {
            name: "Second".into(),
        })
        .unwrap();
    settle(&mut policy);
    map(&mut policy, 2, "b");

    let layout = policy.placement();
    assert_eq!(shown(&layout), [2]);
    assert_eq!(layout.hidden, [1]);
    assert_eq!(policy.focused(), Some(2));
}

#[test]
fn a_window_keeps_its_title_in_its_tab() {
    let mut policy = mount();
    map(&mut policy, 1, "a");
    policy.retitle(1, "Report.odt");
    settle(&mut policy);
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
    let mut policy = mount();
    map(&mut policy, 1, "so.tilo.notes");
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
    settle(&mut policy);
    policy.retitle(1, "Notes");
    settle(&mut policy);
    assert_eq!(
        title_of(&policy, tab),
        "Garden plan",
        "the window's own title only stands where its app names nothing"
    );
    // The person's own name stands in front of it, and clearing it goes back to what the
    // window shows.
    let group = group_of(&policy, tab).to_string();
    let rename = |name: &str| ChromeEffect::Rename {
        tab: None,
        group: Some(group.clone()),
        strip: None,
        workspace: None,
        name: name.into(),
    };
    policy.handle().dispatch(rename("Read later")).unwrap();
    settle(&mut policy);
    assert_eq!(title_of(&policy, tab), "Read later");
    policy.handle().dispatch(rename("")).unwrap();
    settle(&mut policy);
    assert_eq!(title_of(&policy, tab), "Garden plan");
    // Nothing shown to name: the window goes back to its own title.
    policy.handle().declare_bar(tab, 7, None);
    settle(&mut policy);
    policy.retitle(1, "Notes");
    settle(&mut policy);
    assert_eq!(title_of(&policy, tab), "Notes");
    // A window that names nothing keeps the name it has: the tab is never
    // left with no title in it.
    policy.retitle(1, "");
    settle(&mut policy);
    assert_eq!(title_of(&policy, tab), "Notes");
}

#[test]
fn the_shell_names_the_tabs_it_has_for_apps_with_their_apps() {
    let mut policy = mount();
    map(&mut policy, 4, "so.tilo.notes");
    map(&mut policy, 5, "org.gnome.Terminal");
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
    // A launch the shell holds a place for has a tab from the pass it is
    // asked for, and that tab is the app's window on its way: the next
    // launch of the app goes there instead of starting a second process.
    assert!(
        policy.hold("so.tilo.notes", None, (false, false), None),
        "a place beside the window the app has"
    );
    settle(&mut policy);
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
    // the one the shell has.
    policy.release_held("so.tilo.notes");
    settle(&mut policy);
    assert_eq!(
        policy.app_tabs_in_front(),
        [
            (notes, "so.tilo.notes".to_owned()),
            (terminal, "org.gnome.Terminal".to_owned())
        ]
    );
    policy.unmap(4);
    settle(&mut policy);
    assert_eq!(
        policy.app_tabs_in_front(),
        [(terminal, "org.gnome.Terminal".to_owned())],
        "a window gone is not one a launch could raise"
    );
}

#[test]
fn an_apps_next_window_lands_right_after_its_own() {
    let mut policy = mount();
    map(&mut policy, 10, "so.tilo.notes");
    let (anchor, group, index) = policy.anchor_for("so.tilo.notes").unwrap();
    assert_eq!(anchor, window_tab(&policy, 10));
    assert_eq!(index, 0);
    assert!(policy.place_next_beside("so.tilo.notes", Some(anchor), false));

    // The window announces the short name its package id ends with.
    map(&mut policy, 11, "notes");

    let (first, second) = (window_tab(&policy, 10), window_tab(&policy, 11));
    assert_eq!(
        groups(&policy),
        [(group, vec![first, second], second)],
        "the second window is the app's next tab, in front"
    );
    assert_eq!(policy.focused(), Some(11));
    let layout = policy.placement();
    assert_eq!(shown(&layout), [11], "the window in front is drawn");
    assert_eq!(layout.hidden, [10], "the tab behind it is not");
}

#[test]
fn a_window_placed_behind_takes_no_focus() {
    let mut policy = mount();
    map(&mut policy, 10, "so.tilo.notes");
    map(&mut policy, 12, "org.gnome.Terminal");
    let anchor = policy.anchor_for("so.tilo.notes").unwrap();
    assert!(policy.place_next_beside("so.tilo.notes", Some(anchor.0), true));
    map(&mut policy, 11, "notes");

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
        policy.placement().hidden.contains(&11),
        "a tab opened behind is not drawn"
    );
}

#[test]
fn a_placed_window_is_the_one_that_takes_the_place() {
    let mut policy = mount();
    map(&mut policy, 10, "so.tilo.notes");
    assert!(policy.place_next_beside("so.tilo.notes", None, false));
    map(&mut policy, 11, "notes");
    map(&mut policy, 12, "notes");
    assert_eq!(
        groups(&policy).len(),
        2,
        "a place is taken by one window; the next lands as a launch does"
    );
}

#[test]
fn an_app_never_opens_beside_another_apps_tab() {
    let mut policy = mount();
    map(&mut policy, 10, "so.tilo.notes");
    assert_eq!(
        policy.anchor_for("so.tilo.chat"),
        None,
        "no tab of the asking app to anchor on"
    );
    assert!(
        !policy.place_next_beside("so.tilo.chat", None, false),
        "no tab of its own to open beside"
    );
    map(&mut policy, 11, "so.tilo.chat");
    assert_eq!(groups(&policy).len(), 2, "it opens a window of its own");
}

#[test]
fn the_app_tab_in_front_answers_a_request_from_that_app() {
    let mut policy = mount();
    map(&mut policy, 10, "so.tilo.notes");
    map(&mut policy, 11, "org.gnome.Terminal");
    assert_eq!(
        policy.anchor_for("so.tilo.notes").map(|(tab, ..)| tab),
        Some(window_tab(&policy, 10)),
        "another app's window in front: the app's own first tab answers"
    );
    assert!(policy.focus(10));
    settle(&mut policy);
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
    let mut policy = mount();
    displays(&mut policy);
    map(&mut policy, 1, "a");
    let order = policy.displays();
    assert_eq!(order.len(), 2);
    assert_eq!(policy.display_of(1), Some(order[0]));
    assert!(policy.handle().focus_display(order[1]));
    settle(&mut policy);
    map(&mut policy, 2, "b");
    assert_eq!(policy.display_of(2), Some(order[1]));
    let layout = policy.placement();
    let place = |id| {
        layout
            .shown
            .iter()
            .find(|(w, _)| *w == id)
            .unwrap_or_else(|| panic!("window {id} is shown: {layout:?}"))
            .1
    };
    assert!(place(1).x1 <= WIDTH / 2.0 + 0.5, "{layout:?}");
    assert!(place(2).x0 >= WIDTH / 2.0, "{layout:?}");

    assert!(policy.handle().step_focus(-1));
    settle(&mut policy);
    assert_eq!(policy.focused(), Some(1), "past the display's first window");
    assert!(policy.handle().move_focused_to_display(1));
    settle(&mut policy);
    assert_eq!(policy.display_of(1), Some(order[1]));
    assert_eq!(policy.focused(), Some(1));
}

#[test]
fn a_window_opened_beside_another_shares_the_display_with_it() {
    let mut policy = mount();
    map(&mut policy, 1, "a");
    let usable = WIDTH - 2.0 * STRIP_EDGE_GUTTER_PX;
    let alone = shown_rect(&policy, 1);
    assert!(
        (alone.width() - usable).abs() < 0.5,
        "a window alone takes the strip: {alone:?}"
    );

    map(&mut policy, 2, "b");
    let ids: Vec<GroupId> = groups(&policy).iter().map(|(id, ..)| *id).collect();
    assert_eq!(ids.len(), 2, "each window stands in a group of its own");
    let (first, second) = (group_width(&policy, ids[0]), group_width(&policy, ids[1]));
    // Two thirds of the strip for the window the person was in leaves too little beside it for
    // the new one, so the two split the display evenly.
    let half = (usable - HANDLE_SLOT_PX) / 2.0;
    assert!(
        matches!(first, WindowWidth::Share(_)) && first == second,
        "the two share the strip equally: {first:?}, {second:?}"
    );
    assert!(
        first.resolve(usable) <= half && first.resolve(usable) > half - 2.0,
        "each takes half of the strip: {first:?} of {usable}"
    );

    let (left, right) = (shown_rect(&policy, 1), shown_rect(&policy, 2));
    assert!(
        (left.width() - first.resolve(usable)).abs() < 0.5
            && (right.width() - second.resolve(usable)).abs() < 0.5,
        "each is drawn at its share: {left:?} {right:?}"
    );
    assert!(
        (left.x0 - STRIP_EDGE_GUTTER_PX).abs() < 0.5,
        "the window the person was in stays where it stood: {left:?}"
    );
    assert!(
        (right.x0 - left.x1 - HANDLE_SLOT_PX).abs() < 0.5,
        "the new window opens right beside it: {left:?} {right:?}"
    );
    assert!(
        right.x1 <= WIDTH - STRIP_EDGE_GUTTER_PX + 0.5,
        "both stand whole on the display: {right:?}"
    );
}

#[test]
fn a_window_taken_full_screen_covers_its_display() {
    let mut policy = mount();
    map(&mut policy, 1, "a");
    map(&mut policy, 2, "b");
    let before = shown_rect(&policy, 1);
    assert!(
        before.width() < WIDTH - 0.5,
        "two windows share the strip: {before:?}"
    );
    assert_eq!(policy.covering_window(), None);
    assert_eq!(policy.placement().fullscreen, None);

    assert!(policy.set_fullscreen(1, true), "the desktop holds it");
    settle(&mut policy);
    assert_eq!(
        policy.covering_window(),
        Some(1),
        "the shell says which one covers"
    );
    assert_eq!(policy.placement().fullscreen, Some(1));
    let cover = shown_rect(&policy, 1);
    assert!(
        cover.x0.abs() < 0.5
            && cover.y0.abs() < 0.5
            && (cover.x1 - WIDTH).abs() < 0.5
            && cover.height() > before.height() + 0.5,
        "the display from its top edge to its bottom: {cover:?} against {before:?}"
    );

    assert!(policy.set_fullscreen(1, false));
    settle(&mut policy);
    assert_eq!(policy.covering_window(), None, "the display is given back");
    assert_eq!(policy.placement().fullscreen, None);
    let back = shown_rect(&policy, 1);
    assert!(
        (back.x0 - before.x0).abs() < 0.5 && (back.width() - before.width()).abs() < 0.5,
        "the window stands where it stood: {back:?} against {before:?}"
    );
}

/// A program's own top-level window takes a tab whatever size it keeps: an X11 settings window
/// that fixes its size names no parent, so it is no dialog. One that names the window it belongs
/// to floats over it, its size aside.
#[test]
fn a_fixed_size_window_without_a_parent_takes_a_tab() {
    let mut policy = mount();
    displays(&mut policy);
    map(&mut policy, 1, "a");
    let fixed = Window {
        min_size: Some((400, 240)),
        max_size: Some((400, 240)),
        ..window(2, "b")
    };
    policy.map(fixed).unwrap();
    settle(&mut policy);
    assert!(!policy.is_floating(2), "a fixed size alone is no dialog");
    assert!(policy.tab_of(2).is_some(), "it gets a tab of its own");
    assert_eq!(
        groups(&policy).len(),
        2,
        "the tab stands beside the one before it"
    );
    assert_eq!(
        shown(&policy.placement()),
        [1, 2],
        "the tab stands beside the one before it"
    );

    policy
        .map(Window {
            parent: Some(1),
            min_size: Some((400, 240)),
            max_size: Some((400, 240)),
            ..window(3, "c")
        })
        .unwrap();
    settle(&mut policy);
    assert!(policy.is_floating(3), "its parent decides, not its size");
    assert!(policy.tab_of(3).is_none(), "a dialog takes no tab");
    assert_eq!(groups(&policy).len(), 2, "and opens no window of its own");
    assert_eq!(shown(&policy.placement()), [1, 2], "a dialog takes no tab");
}

#[test]
fn a_dialog_floats_over_its_parent_and_answers_for_it() {
    let mut policy = mount();
    displays(&mut policy);
    map(&mut policy, 1, "a");
    policy
        .map(Window {
            parent: Some(1),
            ..window(2, "a")
        })
        .unwrap();
    settle(&mut policy);
    assert!(policy.is_floating(2));
    assert!(policy.tab_of(2).is_none(), "a dialog takes no tab");
    let layout = policy.placement();
    assert_eq!(shown(&layout), [1], "a dialog takes no tab");
    let parent = shown_rect(&policy, 1);
    assert_eq!(layout.floating, [(2, parent)], "over its parent");
    assert_eq!(policy.focused(), Some(2), "the dialog has the keyboard");
    assert!(policy.focus(2), "focusing it brings its parent");

    policy.unmap(1);
    settle(&mut policy);
    assert!(
        policy.is_floating(2),
        "its parent gone, it floats on the display in use"
    );
    assert_eq!(
        policy.placement().floating,
        [(2, Rect::new(0.0, 0.0, WIDTH / 2.0, 800.0))],
        "its parent gone, it floats on the display in use"
    );
    policy.unmap(2);
    assert!(!policy.is_floating(2));
    assert!(policy.tab_of(2).is_none());
    assert!(policy.placement().floating.is_empty());
}

#[test]
fn a_launch_with_no_window_behind_it_is_queued_for_the_session() {
    let mut policy = mount();
    assert!(policy.take_launches().is_empty());

    let launch = ChromeEffect::OpenApp {
        app: "host:org.example.Probe.desktop".into(),
        beside: None,
        background: None,
        float: None,
    };
    policy.handle().dispatch(launch).unwrap();
    settle(&mut policy);

    let asked = policy.take_launches();
    assert_eq!(asked.len(), 1, "one launch for one open");
    assert_eq!(asked[0].app, "host:org.example.Probe.desktop");
    assert!(
        asked[0].tab.is_some(),
        "the shell holds the tab the app's window takes"
    );
    assert!(
        policy.take_launches().is_empty(),
        "a launch is handed over once"
    );
    assert!(
        windows(&policy).is_empty(),
        "nothing appeared before the app did"
    );
    assert_eq!(
        policy.placement(),
        Placement::default(),
        "nothing appeared before the app did"
    );
}

#[test]
fn a_shell_launch_holds_the_tab_the_apps_window_takes() {
    let mut policy = mount();
    let launch = ChromeEffect::OpenApp {
        app: "so.tilo.notes".into(),
        beside: None,
        background: None,
        float: None,
    };
    policy.handle().dispatch(launch).unwrap();
    settle(&mut policy);

    let asked = policy.take_launches();
    assert_eq!(asked.len(), 1, "one launch for one open");
    let tab = asked[0]
        .tab
        .expect("the shell holds the tab the window takes");
    assert_eq!(
        tabs(&policy),
        vec![(
            tab,
            TileContent::AppPlaceholder {
                kind: tilo_desktop_model::AppKind::Other("so.tilo.notes".into()),
                label: "so.tilo.notes".into(),
            },
        )],
        "the app's place is there from this pass, in the tab the launch names"
    );
    assert_eq!(
        title_of(&policy, tab),
        "Notes",
        "the tab opens with the app's name, never with its id"
    );
    assert!(
        windows(&policy).is_empty(),
        "and no window is here before the app's is"
    );
    assert_eq!(
        policy.placement(),
        Placement::default(),
        "and no window is here before the app's is"
    );
}

/// A program whose window names its app otherwise than its launch did (`glxgears` from the
/// desktop entry `kg-glxgears`) takes the tab held for its launch, which the session knows by the
/// window's process; a window of no launch the session knows lands in a tab of its own.
#[test]
fn a_window_takes_the_tab_of_its_own_launch_whatever_it_calls_its_app() {
    let mut policy = mount();
    policy
        .handle()
        .dispatch(ChromeEffect::OpenApp {
            app: "kg-glxgears".into(),
            beside: None,
            background: None,
            float: None,
        })
        .unwrap();
    settle(&mut policy);
    let asked = policy.take_launches();
    let tab = asked[0]
        .tab
        .expect("the shell holds the tab the window takes");
    // The session knows process 501 as one of that launch.
    policy.reads_launch_tabs(std::rc::Rc::new(move |pid| (pid == 501).then_some(tab)));

    policy
        .map(Window {
            pid: Some(777),
            ..window(9, "stranger")
        })
        .unwrap();
    settle(&mut policy);
    policy
        .map(Window {
            pid: Some(501),
            ..window(10, "glxgears")
        })
        .unwrap();
    settle(&mut policy);
    assert_eq!(policy.tab_of(10), Some(tab), "the launch's own tab");
    assert_ne!(policy.tab_of(9), Some(tab));
    assert_eq!(tabs(&policy).len(), 2, "no tab is left waiting");
}

/// The tab held for a launch leaves the layout before its window maps (a slow window, while the
/// workspace's layout is laid over the shell): the window takes a tab of its own, and its app,
/// which names its window by the held tab, is still answered for that window. The host hears of
/// it.
#[test]
fn a_window_whose_held_tab_left_is_still_its_launchs_window() {
    let mut policy = mount();
    policy
        .handle()
        .dispatch(ChromeEffect::OpenApp {
            app: "so.tilo.settings".into(),
            beside: None,
            background: None,
            float: None,
        })
        .unwrap();
    settle(&mut policy);
    let held = policy.take_launches()[0]
        .tab
        .expect("the shell holds the tab the window takes");
    policy.reads_launch_tabs(std::rc::Rc::new(move |pid| (pid == 501).then_some(held)));
    let mut tree = policy.handle().read(Clone::clone);
    assert!(
        tree.close_tab(held).is_some(),
        "the held tab was on the shell"
    );
    policy.handle().replace_tree(tree);
    settle(&mut policy);

    policy
        .map(Window {
            pid: Some(501),
            ..window(10, "so.tilo.settings")
        })
        .unwrap();
    settle(&mut policy);
    let tab = policy.tab_of(10).expect("the window has a tab");
    assert_ne!(
        tab, held,
        "the held tab is gone, so the window took another"
    );
    assert_eq!(
        policy.tab_of_instance("so.tilo.settings", held),
        Some(tab),
        "what the app tells of the window it was launched for reaches that window"
    );
    assert_eq!(
        policy.take_missed_holds(),
        vec![tilo_desktop_policy::MissedHold { window: 10, held }],
        "the host hears that the window missed its held tab"
    );

    policy.unmap(10);
    settle(&mut policy);
    assert_eq!(
        policy.tab_of_instance("so.tilo.settings", held),
        None,
        "a window that went names no tab"
    );
}

#[test]
fn a_conversation_opens_beside_its_sender() {
    let mut policy = mount();
    map(&mut policy, 1, "so.tilo.chat");
    let sender = window_tab(&policy, 1);
    assert!(
        policy.hold("so.tilo.chat", None, (false, true), Some("dm")),
        "the shell takes the place beside the sender"
    );
    settle(&mut policy);
    // The shell asks for the float; the session's layout makes the ask.
    policy.handle().arrange_asked_floats();
    settle(&mut policy);

    let tab = tabs(&policy)
        .into_iter()
        .find(|(_, tile)| matches!(tile, TileContent::AppPlaceholder { .. }))
        .map(|(tab, _)| tab)
        .expect("the shell holds the tab the conversation's window takes");
    let asked = policy.take_launches();
    assert_eq!(
        asked.len(),
        1,
        "the session starts the window the place waits for"
    );
    assert_eq!(asked[0].app, "held:so.tilo.chat");
    assert_eq!(
        asked[0].tab,
        Some(tab),
        "the tab the window takes is the instance it is known by"
    );
    assert!(asked[0].beside, "a window of the app's own");
    // Born showing the conversation, which every device's window of it opens on.
    assert_eq!(policy.tab_showing("so.tilo.chat", "dm"), Some(tab));
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
    let widths: Vec<WindowWidth> = policy.handle().read(|tree| {
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
            .all(|width| matches!(width, WindowWidth::Share(_) | WindowWidth::Fraction(_))),
        "the two share the strip, so the sender keeps what the person was reading in view: {widths:?}"
    );

    // It stands over the shell, where its strip gave it room: beside the
    // sender, never over the message list the person was reading.
    let place = policy
        .handle()
        .read(|tree| tree.workspaces.first()?.layout.float_of(*group));
    assert!(
        place.is_some(),
        "the conversation stands over the shell, in the workspace's record"
    );
    let sender_content = policy
        .content_rect(sender)
        .expect("the sender's content stands on the shell");
    let content = policy
        .content_rect(tab)
        .expect("the conversation's content stands on the shell");
    assert!(
        content.x0 >= sender_content.x1,
        "the conversation stands beside its sender, never over what the person was reading: \
         sender {sender_content:?}, conversation {content:?}"
    );

    // The session gives the place up when nothing came to fill it: the tab
    // goes, and the window it was held for has nowhere to land.
    policy.release_held("so.tilo.chat");
    settle(&mut policy);
    assert_eq!(
        tabs(&policy).len(),
        1,
        "the place held for a window that never came is gone"
    );
}

#[test]
fn a_window_that_names_no_title_is_tabbed_under_its_apps_name() {
    let mut policy = mount();
    policy
        .handle()
        .dispatch(ChromeEffect::OpenApp {
            app: "so.tilo.notes".into(),
            beside: None,
            background: None,
            float: None,
        })
        .unwrap();
    settle(&mut policy);
    let tab = policy.take_launches()[0].tab.expect("a held tab");
    // A window that has not said what it shows takes the held tab under the
    // name of its app.
    policy
        .map(Window {
            title: None,
            ..window(1, "so.tilo.notes")
        })
        .unwrap();
    settle(&mut policy);
    assert_eq!(title_of(&policy, tab), "Notes");
    // The toolkit then labels the window with the app's unlocalised name:
    // the tab keeps the name this shell gives the app.
    policy.retitle(1, "Notes");
    settle(&mut policy);
    assert_eq!(title_of(&policy, tab), "Notes");
}

#[test]
fn a_launch_that_opens_no_window_takes_its_held_tab_back() {
    let mut policy = mount();
    let launch = ChromeEffect::OpenApp {
        app: "so.tilo.notes".into(),
        beside: None,
        background: None,
        float: None,
    };
    policy.handle().dispatch(launch).unwrap();
    settle(&mut policy);
    let tab = policy.take_launches()[0].tab.expect("a held tab");

    // The session started nothing for it: the place it held goes.
    policy.release_tab(tab);
    settle(&mut policy);
    assert!(tabs(&policy).is_empty(), "the held tab is not left behind");
    assert!(windows(&policy).is_empty(), "no window is here");
    assert_eq!(policy.placement(), Placement::default());
    assert!(
        policy.take_launches().is_empty(),
        "and nothing is started for it again"
    );
}

#[test]
fn an_app_that_did_not_start_keeps_the_place_for_its_window() {
    let mut policy = mount();
    let launch = ChromeEffect::OpenApp {
        app: "so.tilo.notes".into(),
        beside: None,
        background: None,
        float: None,
    };
    policy.handle().dispatch(launch).unwrap();
    settle(&mut policy);
    let tab = policy.take_launches()[0].tab.expect("a held tab");
    assert!(
        policy.holds_a_place(tab),
        "no window came for it: the tab still holds the app's place"
    );

    // The launch could not start it: the tab the person opened stays, and
    // says what happened and why instead of vanishing.
    let answer = tilo_session_core::desktop::outcome::Outcome::new("Notes could not open")
        .saying("it is not installed here")
        .retryable("Try again");
    policy.failed_at(tab, "so.tilo.notes", answer);
    settle(&mut policy);
    assert_eq!(tabs(&policy).len(), 1, "the tab the person opened stays");
    assert!(windows(&policy).is_empty(), "no window is here");
    assert_eq!(
        policy.placement(),
        Placement::default(),
        "no window is here"
    );

    // The window comes: the answer gives its tab up to it.
    map(&mut policy, 41, "so.tilo.notes");
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
fn a_stopped_app_leaves_a_notice_in_a_tab_of_its_own() {
    let mut policy = mount();
    let asked = "host:org.example.Probe.desktop";
    policy
        .stopped(
            asked,
            "Probe",
            tilo_session_core::desktop::outcome::Closed::Crashed,
            None,
        )
        .unwrap();
    settle(&mut policy);
    assert!(
        policy.take_launches().is_empty(),
        "the notice itself asks for nothing"
    );
    assert_eq!(policy.name_of(asked), "Probe", "the app's name it had");
    let tabs = tabs(&policy);
    assert_eq!(tabs.len(), 1, "the notice stands in a tab of its own");
    assert!(
        policy.holds_a_place(tabs[0].0),
        "the tab says what became of the app"
    );
    assert!(windows(&policy).is_empty(), "no window stands behind it");
}

/// An app that stopped working says so in the tab its window was in, between the same
/// neighbours, whether its window went away before the end was known or not.
#[test]
fn a_stopped_app_says_so_where_its_window_was() {
    let mut policy = mount();
    map(&mut policy, 31, "org.example.Left");
    map(&mut policy, 32, "org.example.Probe");
    map(&mut policy, 33, "org.example.Right");
    let before: Vec<u64> = tabs(&policy).iter().map(|(tab, _)| *tab).collect();
    let tab = policy.tab_of(32).expect("the window's tab");

    policy.unmap(32);
    settle(&mut policy);
    policy
        .stopped(
            "org.example.Probe",
            "Probe",
            tilo_session_core::desktop::outcome::Closed::Crashed,
            Some(tab),
        )
        .unwrap();
    settle(&mut policy);
    let after: Vec<u64> = tabs(&policy).iter().map(|(tab, _)| *tab).collect();
    assert_eq!(after, before, "the tab is back where it was");

    // An end known while the window is still there takes its tab at once; the window leaving
    // after does not close it.
    let other = policy.tab_of(33).expect("the other window's tab");
    policy
        .stopped(
            "org.example.Right",
            "Right",
            tilo_session_core::desktop::outcome::Closed::Crashed,
            Some(other),
        )
        .unwrap();
    policy.unmap(33);
    settle(&mut policy);
    assert_eq!(
        tabs(&policy)
            .iter()
            .map(|(tab, _)| *tab)
            .collect::<Vec<_>>(),
        before
    );
}

/// A window whose process was launched into a workspace that is not in front (an agent's open of
/// an app in its own workspace) opens there: the workspace in front shows only its own windows,
/// and a second launch of the same app in front finds no window of it there to raise.
#[test]
fn a_window_launched_into_another_workspace_opens_there() {
    use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
    let seeded = DesktopPolicy::new(7);
    let mut tree = seeded.handle().read(Clone::clone);
    tree.workspaces
        .push(Workspace::new(WorkspaceId::from_u128(2), "Garden"));
    let garden = tree.workspaces[1].id().to_string();
    let front = tree.active;
    let mut policy = DesktopPolicy::with_tree(tree, 7);
    policy.reads_launch_workspaces(std::rc::Rc::new(move |pid, app| {
        (pid == 50 && app == "so.tilo.calculator").then(|| garden.clone())
    }));
    policy
        .map(Window {
            pid: Some(50),
            ..window(5, "so.tilo.calculator")
        })
        .unwrap();
    settle(&mut policy);
    policy.carry_launched();
    settle(&mut policy);
    policy
        .map(Window {
            pid: Some(60),
            ..window(6, "so.tilo.notes")
        })
        .unwrap();
    settle(&mut policy);
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
                .map(tilo_desktop_model::Workspace::id)
        })
    };
    assert_eq!(
        placed(5),
        Some(WorkspaceId::from_u128(2)),
        "launched into Garden: opens there"
    );
    assert_eq!(
        placed(6),
        Some(front),
        "launched into the one in front: opens in front"
    );
    assert_eq!(
        policy.handle().read(|tree: &Layout| tree.active),
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

/// A window of a strip spread over two displays is cut to its own tab, and so to its own display:
/// the part of a window wider than its tab, or panned past its display's edge, is never drawn on
/// the neighbouring one.
#[test]
fn a_window_is_cut_to_the_display_it_shows_on() {
    let mut policy = mount();
    displays(&mut policy);
    map(&mut policy, 1, "a");
    let order = policy.displays();
    assert!(policy.handle().focus_display(order[1]));
    settle(&mut policy);
    map(&mut policy, 2, "b");
    let layout = policy.placement();
    let clip = |id| layout.clips.iter().find(|(w, _)| *w == id).map(|c| c.1);
    let shown = |id| layout.shown.iter().find(|(w, _)| *w == id).map(|c| c.1);
    for id in [1, 2] {
        assert_eq!(
            clip(id),
            shown(id),
            "a window is cut to exactly the tab it shows in: {layout:?}"
        );
    }
    assert!(
        clip(1).is_some_and(|clip| clip.x1 <= WIDTH / 2.0),
        "the first display's tab ends at its own display: {layout:?}"
    );
    assert!(
        clip(2).is_some_and(|clip| clip.x0 >= WIDTH / 2.0),
        "the second display's tab starts at its own display: {layout:?}"
    );
}

/// A window of `app` an app keeper (`keeper`) shows, named `tag`.
fn kept(id: u64, app: &str, keeper: u32, tag: &str) -> Window {
    Window {
        pid: Some(keeper),
        tag: Some(tag.into()),
        ..window(id, app)
    }
}

/// After an in-place restart each window of a kept app takes exactly the tab it stood in, in
/// whatever order its windows come back; a window opened while the desktop was away, and one of
/// the same app no keeper names, take no held tab. What each kept window is told of its tab
/// follows it.
#[test]
fn each_kept_window_takes_back_exactly_its_own_tab() {
    let mut policy = mount();
    for id in 1..=3 {
        map(&mut policy, id, "org.example.Kept");
    }
    let saved: Vec<u64> = (1..=3)
        .map(|window| policy.tab_of(window).expect("a tab"))
        .collect();
    // The desktop restarted: each tab waits for the app's window, as a saved one does.
    let mut tree = policy.handle().read(Clone::clone);
    for tab in tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .flat_map(|strip| &mut strip.window_groups)
        .flat_map(|group| &mut group.tabs)
    {
        tab.tile = TileContent::LocalApp {
            app_id: "org.example.Kept".into(),
            label: "Kept".into(),
            note: String::new(),
        };
    }
    policy.handle().replace_tree(tree);
    settle(&mut policy);
    for (tab, tag) in saved.iter().zip(["w1", "w2", "w3"]) {
        let window = tilo_desktop_policy::KeptWindow {
            keeper: 50,
            tag: tag.into(),
        };
        assert!(policy.adopt("org.example.Kept", *tab, Some(window)));
    }
    // Neither a window that opened while the desktop was away nor one of another process takes
    // a tab held for a named window.
    policy.map(window(20, "org.example.Kept")).unwrap();
    policy.map(kept(21, "org.example.Kept", 50, "w9")).unwrap();
    settle(&mut policy);
    for (id, tag) in [(13, "w3"), (11, "w1"), (12, "w2")] {
        policy.map(kept(id, "org.example.Kept", 50, tag)).unwrap();
        settle(&mut policy);
    }
    let placed: Vec<Option<u64>> = [11, 12, 13].map(|window| policy.tab_of(window)).into();
    assert_eq!(placed, saved.iter().copied().map(Some).collect::<Vec<_>>());
    for window in [20, 21] {
        let tab = policy.tab_of(window).expect("a tab of its own");
        assert!(!saved.contains(&tab), "window {window} takes a new tab");
    }
    let mut told = policy.kept_placements();
    told.sort();
    let told: Vec<(String, u64)> = told
        .into_iter()
        .map(|(window, tab)| (window.tag, tab))
        .collect();
    assert_eq!(
        told,
        [
            ("w1".to_owned(), saved[0]),
            ("w2".to_owned(), saved[1]),
            ("w3".to_owned(), saved[2]),
            ("w9".to_owned(), policy.tab_of(21).expect("its tab")),
        ]
    );
    policy.unmap(12);
    assert_eq!(
        policy.kept_placements().len(),
        3,
        "a closed window is told nothing more"
    );
}

/// A kept window that stood in a tab before an in-place restart takes that tab back and its tab
/// closes with it when the program ends. A fixed size is no dialog's mark, so a setup wizard
/// comes back into its tab rather than floating over the strip.
#[test]
fn a_kept_window_takes_back_its_tab_and_closes_with_it() {
    let mut policy = mount();
    map(&mut policy, 1, "org.example.Setup");
    let saved = policy.tab_of(1).expect("a tab");
    let mut tree = policy.handle().read(Clone::clone);
    for tab in tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .flat_map(|strip| &mut strip.window_groups)
        .flat_map(|group| &mut group.tabs)
    {
        tab.tile = TileContent::LocalApp {
            app_id: "org.example.Setup".into(),
            label: "Setup".into(),
            note: String::new(),
        };
    }
    policy.handle().replace_tree(tree);
    settle(&mut policy);
    let held = tilo_desktop_policy::KeptWindow {
        keeper: 50,
        tag: "wizard".into(),
    };
    assert!(policy.adopt("org.example.Setup", saved, Some(held)));
    let wizard = Window {
        min_size: Some((500, 380)),
        max_size: Some((500, 380)),
        ..kept(11, "org.example.Setup", 50, "wizard")
    };
    assert!(
        !tilo_desktop_policy::floating::floats(&wizard),
        "a fixed size is no dialog's mark"
    );
    policy.map(wizard).unwrap();
    settle(&mut policy);
    assert_eq!(
        policy.tab_of(11),
        Some(saved),
        "the kept wizard is back in its tab"
    );
    policy.unmap(11);
    settle(&mut policy);
    assert!(
        !policy.handle().read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .any(|tab| tab.id == saved)
        }),
        "its tab closes with it"
    );
}

/// A window that keeps a smallest size is never cut off by a tab narrower than it: a
/// fixed-size window — `PuTTY`'s Configuration is 456 px wide — takes a tab at least that wide.
/// This is the same after an in-place restart, when the window comes back into the tab it stood
/// in before whatever share that tab had.
#[test]
fn a_fixed_size_window_widens_a_tab_narrower_than_it() {
    let mut policy = mount();
    assert!(policy.set_displays(&[Output {
        monitor: "only".into(),
        rect: Rect::new(0.0, 0.0, WIDTH, 800.0),
        primary: true,
    }]));
    let usable = WIDTH - 2.0 * ui::widget::STRIP_EDGE_GUTTER_PX;
    map(&mut policy, 1, "org.example.PuTTY");
    let tab = policy.tab_of(1).expect("a tab");
    let group = group_of(&policy, tab);
    policy
        .handle()
        .dispatch(ChromeEffect::SetWidth {
            group: group.to_string(),
            permille: Some(300),
            preset: None,
        })
        .expect("the tab takes a narrow share");
    settle(&mut policy);
    let narrow = group_width(&policy, group);
    assert_eq!(narrow, WindowWidth::Fraction(300));
    assert!(
        narrow.resolve(usable) < 456.0,
        "the tab starts narrower than the dialog: {narrow:?}"
    );
    let drawn_narrow = shown_rect(&policy, 1);
    assert!(
        drawn_narrow.width() < 456.0,
        "the tab starts narrower than the dialog: {drawn_narrow:?}"
    );

    assert!(
        policy.honour_minimum_widths([(1, Some((456, 442)))].into_iter()),
        "the tab is widened to what the dialog keeps"
    );
    settle(&mut policy);
    let wide = group_width(&policy, group);
    assert!(
        wide.resolve(usable) >= 456.0,
        "the dialog shows whole: {wide:?}"
    );
    assert!(
        wide.resolve(usable) < usable,
        "the tab takes what the dialog needs, not the whole strip: {wide:?}"
    );
    let drawn_wide = shown_rect(&policy, 1);
    assert!(
        drawn_wide.width() >= 456.0,
        "the dialog shows whole: {drawn_wide:?}"
    );
    assert!(
        drawn_wide.width() < WIDTH,
        "the tab takes what the dialog needs, not the strip: {drawn_wide:?}"
    );
    assert!(
        !policy.honour_minimum_widths([(1, Some((456, 442)))].into_iter()),
        "a tab already wide enough is left as the person set it"
    );
    assert!(
        !policy.honour_minimum_widths([(1, None)].into_iter()),
        "a window that keeps no size leaves its tab alone"
    );
}

/// A window wider than its whole strip still gets the whole strip: the smallest size the
/// program keeps cannot be met by any share, so the tab is as wide as the strip allows.
#[test]
fn a_window_wider_than_the_strip_takes_the_whole_strip() {
    let mut policy = mount();
    assert!(policy.set_displays(&[Output {
        monitor: "only".into(),
        rect: Rect::new(0.0, 0.0, WIDTH, 800.0),
        primary: true,
    }]));
    let usable = WIDTH - 2.0 * ui::widget::STRIP_EDGE_GUTTER_PX;
    map(&mut policy, 2, "org.example.Wide");
    let group = group_of(&policy, policy.tab_of(2).expect("a tab"));
    assert!(policy.honour_minimum_widths([(2, Some((4000, 600)))].into_iter()));
    settle(&mut policy);
    let wide = group_width(&policy, group);
    assert!(
        wide.resolve(usable) >= 4000.0,
        "the tab is as wide as the window keeps, and the strip scrolls: {wide:?}"
    );
    let rect = shown_rect(&policy, 2);
    assert!(
        rect.width() > WIDTH,
        "the tab is as wide as the window keeps, and the strip scrolls: {rect:?}"
    );
}

/// A strip narrower than the window's smallest size already gives every window the whole
/// strip and nothing more: a wider share cannot widen it, so the width is left as it is
/// rather than asked for again on every pass.
#[test]
fn a_strip_narrower_than_the_window_is_left_alone() {
    let mut policy = mount();
    assert!(policy.set_displays(&[Output {
        monitor: "phone".into(),
        rect: Rect::new(0.0, 0.0, 360.0, 800.0),
        primary: true,
    }]));
    let usable = 360.0 - 2.0 * ui::widget::STRIP_EDGE_GUTTER_PX;
    map(&mut policy, 3, "org.example.Wide");
    let group = group_of(&policy, policy.tab_of(3).expect("a tab"));
    let before = group_width(&policy, group);
    assert!(
        before.resolve(usable) >= usable - 0.5,
        "the tab already takes the whole strip: {before:?}"
    );
    assert!(
        !policy.honour_minimum_widths([(3, Some((600, 400)))].into_iter()),
        "the tab already takes the whole strip"
    );
    let drawn = policy.placement();
    settle(&mut policy);
    assert_eq!(group_width(&policy, group), before);
    assert_eq!(
        policy.placement(),
        drawn,
        "nothing is laid out again for it"
    );
    assert!(
        shown(&drawn).contains(&3),
        "the window stands on the strip: {drawn:?}"
    );
}

/// Only the windows that stand on a display are in view: a strip lays out every window on it, and
/// the ones scrolled past stand off the display. What sets an app's tier (and its memory floor) is
/// this, so an app whose windows are all scrolled away yields to the ones the person sees.
#[test]
fn the_windows_scrolled_off_the_display_are_not_in_view() {
    let mut policy = mount();
    assert!(policy.set_displays(&[Output {
        monitor: "only".into(),
        rect: Rect::new(0.0, 0.0, WIDTH, 800.0),
        primary: true,
    }]));
    for id in 1..=6 {
        map(&mut policy, id, &format!("app-{id}"));
    }
    let layout = policy.placement();
    let in_view = policy.in_view();
    assert!(
        in_view.contains(&6),
        "the newest window is in view: {layout:?}"
    );
    let off: Vec<u64> = layout
        .shown
        .iter()
        .filter(|(_, rect)| rect.x1 <= 0.0 || rect.x0 >= WIDTH)
        .map(|(id, _)| *id)
        .collect();
    assert!(
        !off.is_empty(),
        "six windows do not fit one display: {layout:?}"
    );
    for id in &off {
        assert!(
            !in_view.contains(id),
            "{id} stands off the display: {layout:?}"
        );
    }
    assert_eq!(in_view.len() + off.len(), layout.shown.len(), "{layout:?}");
}

#[test]
fn the_keyboard_waits_for_the_window_a_launch_in_front_of_the_person_brings() {
    let mut policy = mount();
    // An empty desktop: the launch the person starts is where their keys go, before the shell
    // has even made its tab.
    assert!(!policy.awaits_launched_window(), "nothing is on its way");
    assert!(policy.hold_open("so.tilo.notes", false, None));
    assert!(
        policy.awaits_launched_window(),
        "the keys wait for the window of the launch"
    );
    settle(&mut policy);
    assert!(
        policy.awaits_launched_window(),
        "the place the launch holds is in front of the person"
    );
    map(&mut policy, 4, "so.tilo.notes");
    assert!(
        !policy.awaits_launched_window(),
        "the window came: keys go to it"
    );

    // A launch in the background brings nothing to where the person is.
    assert!(policy.hold_open("org.gnome.Terminal", true, None));
    settle(&mut policy);
    assert!(!policy.awaits_launched_window());
    map(&mut policy, 5, "org.gnome.Terminal");

    // One started in front of a window: the place it holds comes forward, and the keys wait
    // for its window there.
    assert!(policy.hold_open("so.tilo.files", false, None));
    settle(&mut policy);
    assert!(policy.awaits_launched_window());
    // It started nothing: its place goes, and the keys with it.
    policy.release_held("so.tilo.files");
    settle(&mut policy);
    assert!(!policy.awaits_launched_window());
}
