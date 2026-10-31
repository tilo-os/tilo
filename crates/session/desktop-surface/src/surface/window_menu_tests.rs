//! The window menu's rows and the commands they mean.

use super::*;
use crate::TileSource;
use crate::surface::tests::{desk_with, one_window_desk};
use crate::surface::{DesktopHandle, DesktopSurface};
use std::collections::HashMap;
use tilo_desktop_model::width::{
    WIDTH_NARROW_PERMILLE, WIDTH_NORMAL_PERMILLE, WIDTH_WIDE_PERMILLE,
};
use tilo_desktop_model::{
    Capability, DesktopTree, Display, GRANT_ALWAYS, Strip, StripId, TabId, TileContent,
    WindowWidth, Workspace, WorkspaceId,
};
use ui::widget::TabHolder;
use ui::window_share::OpenWindow;

fn desk(windows: usize) -> (DesktopSurface, DesktopHandle) {
    crate::surface::tests::desktop(windows)
}

/// The apps this desk runs, each opening a tab of its own app.
struct Apps;

impl TileSource for Apps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> ui::widget::TileHost {
        ui::widget::TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str) -> Option<Tab> {
        Some(Tab {
            id,
            title: app.to_owned(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: app.into(),
                note: String::new(),
            },
            app_title: None,
        })
    }
}

fn apps_desk(windows: usize) -> (DesktopSurface, DesktopHandle) {
    desk_with(windows, Box::new(Apps))
}

fn present(surface: DesktopSurface) -> ui::headless::FrameHarness {
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    frame.relayout();
    frame
}

/// The passes a tree change takes to reach the windows.
fn settle(frame: &mut ui::headless::FrameHarness) {
    for _ in 0..4 {
        frame.relayout();
    }
}

fn first_route(handle: &DesktopHandle) -> RouteIds {
    handle.read(|tree| {
        let tab = tree.workspaces[0].strips[0].window_groups[0].tabs[0].id;
        crate::surface::route_of(tree, tab).unwrap()
    })
}

/// The tab of the `group`th window of the first strip.
fn tab_at(handle: &DesktopHandle, group: usize) -> TabId {
    handle.read(|tree| tree.workspaces[0].strips[0].window_groups[group].tabs[0].id)
}

/// The group at `route`, as the tree holds it.
fn group_at(tree: &DesktopTree, route: RouteIds) -> tilo_desktop_model::WindowGroup {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .find(|strip| strip.id == route.strip)
        .and_then(|strip| {
            strip
                .window_groups
                .iter()
                .find(|group| group.id == route.group)
        })
        .cloned()
        .expect("the window is in the tree")
}

fn row<'a>(rows: &'a [Row], id: &str) -> Option<&'a Row> {
    rows.iter().find(|row| row.id == id)
}

fn ids(rows: &[Row]) -> Vec<&str> {
    rows.iter().map(|row| row.id.as_str()).collect()
}

/// A window offers every row the desktop has for it, in the order the
/// desktop shows them, and each means the command it names.
#[test]
fn every_row_the_desktop_has_for_a_window_is_offered_in_its_order() {
    let (_surface, handle) = apps_desk(1);
    let (route, tree) = (first_route(&handle), handle.read(Clone::clone));
    let rows = super::rows(&tree, &route, &Facts::default());
    assert_eq!(
        ids(&rows),
        vec![
            FULLSCREEN,
            MAXIMIZE,
            MOVE_TO_STRIP,
            MOVE_TO_WORKSPACE,
            MOVE_TO_DISPLAY,
            HALF,
            WIDTH_NARROW,
            WIDTH_NORMAL,
            WIDTH_WIDE,
            EQUALIZE,
            WIDER,
            NARROWER,
            NEW_TAB,
            CLOSE_WINDOW,
        ]
    );
    // Nowhere to move it to: the rows stay, disabled, saying why.
    let strip = row(&rows, MOVE_TO_STRIP).unwrap();
    assert!(!strip.enabled);
    assert_eq!(
        strip.reason.as_deref(),
        Some("This workspace has no other strip")
    );
    let workspace = row(&rows, MOVE_TO_WORKSPACE).unwrap();
    assert!(!workspace.enabled);
    assert_eq!(
        workspace.reason.as_deref(),
        Some("There is no other workspace")
    );
    let display = row(&rows, MOVE_TO_DISPLAY).unwrap();
    assert!(!display.enabled);
    assert_eq!(
        display.reason.as_deref(),
        Some("This window is on the only display")
    );
    for offered in rows.iter().filter(|row| row.enabled) {
        assert!(
            command(&tree, &route, &offered.id, Some("A name"), "g").is_some(),
            "{offered:?} means a command"
        );
    }
    // A window of one tab is its own tab: it offers no split and no close of
    // the tab alone.
    assert!(row(&rows, CLOSE).is_none());
    assert!(row(&rows, SPLIT_RIGHT).is_none());
    assert_eq!(row(&rows, CLOSE_WINDOW).unwrap().label, "Close this window");
    // A Tilo app's window titles itself, so it offers no name.
    assert!(row(&rows, RENAME).is_none());
}

/// The row of a window asking for the person takes them to it, and is gone
/// once they are there.
#[test]
fn a_window_asking_for_the_person_offers_to_be_gone_to_first() {
    let (surface, handle) = desk(2);
    let tab = tab_at(&handle, 1);
    assert!(handle.ask_attention(tab));
    let rows = handle.window_menu(tab);
    assert_eq!(rows.first().map(|row| row.id.as_str()), Some(REVEAL));
    assert_eq!(rows[0].label, "Go to this window");
    let route = first_route(&handle);
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, REVEAL, None, "g"),
        Some(Command::Select { .. })
    ));

    let mut frame = present(surface);
    handle
        .choose_window_menu(tab, REVEAL, None, "g")
        .expect("going to the window is offered");
    settle(&mut frame);
    assert_eq!(
        handle.read(|tree| crate::navigation::active_route(tree).map(|route| route.tab)),
        Some(tab),
        "the person is looking at it"
    );
    assert!(!handle.asks_attention(tab), "and it has them");
    assert!(row(&handle.window_menu(tab), REVEAL).is_none());
}

/// The full screen row covers the display with the window and gives it
/// back, and the tree keeps which window covers it.
#[test]
fn the_full_screen_row_covers_the_display_and_gives_it_back() {
    let (surface, handle) = desk(2);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let covering =
        |handle: &DesktopHandle| handle.read(tilo_desktop_model::DesktopTree::fullscreen_group);
    assert_eq!(covering(&handle), None);
    assert!(row(&handle.window_menu(route.tab), FULLSCREEN).is_some());

    handle
        .choose_window_menu(route.tab, FULLSCREEN, None, "g")
        .unwrap();
    settle(&mut frame);
    assert_eq!(covering(&handle), Some(route.group), "the tree holds it");
    let rect = handle
        .shown_tabs()
        .into_iter()
        .find(|(tab, _)| *tab == route.tab)
        .expect("the window is drawn")
        .1;
    assert!(
        rect.x0.abs() < 0.5
            && rect.y0.abs() < 0.5
            && (rect.width() - 1280.0).abs() < 0.5
            && (rect.height() - 800.0).abs() < 0.5,
        "the whole display, gutters and all: {rect:?}"
    );
    let rows = handle.window_menu(route.tab);
    assert!(row(&rows, FULLSCREEN).is_none(), "it reads as its exit");
    assert!(row(&rows, EXIT_FULLSCREEN).is_some());

    handle
        .choose_window_menu(route.tab, EXIT_FULLSCREEN, None, "g")
        .unwrap();
    settle(&mut frame);
    assert_eq!(covering(&handle), None, "the display is given back");
    let back = handle
        .shown_tabs()
        .into_iter()
        .find(|(tab, _)| *tab == route.tab)
        .expect("the window is still drawn")
        .1;
    assert!(
        back.width() < 1280.0 - 0.5,
        "it takes its place on the strip again: {back:?}"
    );
}

/// Filling the strip is offered, and once filled the row gives the strip
/// back.
#[test]
fn filling_the_strip_is_offered_and_then_giving_it_back() {
    let (surface, handle) = apps_desk(1);
    let mut frame = present(surface);
    let route = first_route(&handle);
    assert!(row(&handle.window_menu(route.tab), MAXIMIZE).is_some());
    handle
        .choose_window_menu(route.tab, MAXIMIZE, None, "g")
        .unwrap();
    settle(&mut frame);
    assert!(handle.read(|tree| group_at(tree, route).maximized));
    let rows = handle.window_menu(route.tab);
    assert!(row(&rows, MAXIMIZE).is_none());
    assert!(row(&rows, RESTORE).is_some());
    handle
        .choose_window_menu(route.tab, RESTORE, None, "g")
        .unwrap();
    settle(&mut frame);
    assert!(!handle.read(|tree| group_at(tree, route).maximized));
}

/// Each width row gives the window the share of its strip it names, and the
/// wider and narrower rows step between the same shares, stopping at both
/// ends with a row that says why.
#[test]
fn every_width_row_gives_the_window_the_share_it_names() {
    let (surface, handle) = apps_desk(1);
    let mut frame = present(surface);
    let route = first_route(&handle);
    for (id, width) in [
        (HALF, WindowWidth::Fraction(500)),
        (WIDTH_NARROW, WindowWidth::Narrow),
        (WIDTH_NORMAL, WindowWidth::Normal),
        (WIDTH_WIDE, WindowWidth::Wide),
    ] {
        let tree = handle.read(Clone::clone);
        assert!(
            matches!(
                command(&tree, &route, id, None, "g"),
                Some(Command::WidthPreset { .. })
            ),
            "{id} is the share it names"
        );
        handle
            .choose_window_menu(route.tab, id, None, "g")
            .expect(id);
        settle(&mut frame);
        assert_eq!(
            handle.read(|tree| group_at(tree, route).width),
            width,
            "{id}"
        );
    }
    let rows = handle.window_menu(route.tab);
    let widest = row(&rows, WIDER).unwrap();
    assert!(!widest.enabled, "no wider than the strip");
    assert_eq!(
        widest.reason.as_deref(),
        Some("As wide as the strip"),
        "the row says why"
    );
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, NARROWER, None, "g"),
        Some(Command::Width { permille, .. }) if permille == WIDTH_NORMAL_PERMILLE
    ));
    handle
        .choose_window_menu(route.tab, NARROWER, None, "g")
        .unwrap();
    settle(&mut frame);
    assert_eq!(
        handle.read(|tree| group_at(tree, route).width.permille()),
        WIDTH_NORMAL_PERMILLE
    );
    handle
        .choose_window_menu(route.tab, NARROWER, None, "g")
        .unwrap();
    settle(&mut frame);
    assert_eq!(
        handle.read(|tree| group_at(tree, route).width.permille()),
        WIDTH_NARROW_PERMILLE
    );
    let rows = handle.window_menu(route.tab);
    let narrowest = row(&rows, NARROWER).unwrap();
    assert!(!narrowest.enabled, "no narrower than a window goes");
    assert_eq!(
        narrowest.reason.as_deref(),
        Some("As narrow as a window goes")
    );
    assert!(
        handle
            .choose_window_menu(route.tab, NARROWER, None, "g")
            .is_err(),
        "and it means no command"
    );
    let _ = WIDTH_WIDE_PERMILLE;
}

/// Equalising gives every window of the strip the same share of it.
#[test]
fn equalizing_gives_every_window_of_the_strip_the_same_share() {
    let (surface, handle) = apps_desk(2);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let mut tree = handle.read(Clone::clone);
    tree.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Narrow;
    tree.workspaces[0].strips[0].window_groups[1].width = WindowWidth::Wide;
    handle.replace_tree(tree);
    settle(&mut frame);

    handle
        .choose_window_menu(route.tab, EQUALIZE, None, "g")
        .unwrap();
    settle(&mut frame);
    let widths: Vec<WindowWidth> = handle.read(|tree| {
        tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|group| group.width)
            .collect()
    });
    assert_eq!(
        widths,
        vec![WindowWidth::Fraction(500), WindowWidth::Fraction(500)],
        "two windows share the strip evenly"
    );
}

/// A window moves into another strip of its workspace, by that strip's name.
#[test]
fn a_window_moves_to_another_strip_by_its_name() {
    let (surface, handle) = apps_desk(1);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let mut tree = handle.read(Clone::clone);
    tree.workspaces[0]
        .strips
        .push(Strip::new(StripId(90), "Research"));
    handle.replace_tree(tree);
    settle(&mut frame);

    let rows = handle.window_menu(route.tab);
    let across = row(&rows, &format!("{MOVE_TO_STRIP}:90")).expect("the strip is offered");
    assert!(across.enabled, "{across:?}");
    assert_eq!(across.label, "Move to strip Research");
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, &across.id, None, "g"),
        Some(Command::MoveToStrip { ref strip, .. }) if strip == "90"
    ));
    handle
        .choose_window_menu(route.tab, &across.id, None, "g")
        .unwrap();
    settle(&mut frame);
    let strips: Vec<StripId> = handle.read(|tree| {
        tree.workspaces[0]
            .strips
            .iter()
            .filter(|strip| {
                strip
                    .window_groups
                    .iter()
                    .any(|group| group.id == route.group)
            })
            .map(|strip| strip.id)
            .collect()
    });
    assert_eq!(strips, vec![StripId(90)], "the window is there now");
}

/// A window moves into another workspace, by that workspace's name.
#[test]
fn a_window_moves_to_another_workspace_by_its_name() {
    let (surface, handle) = apps_desk(1);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let mut tree = handle.read(Clone::clone);
    tree.workspaces
        .push(Workspace::new(WorkspaceId(91), "Studio"));
    handle.replace_tree(tree);
    settle(&mut frame);

    let rows = handle.window_menu(route.tab);
    let across = row(&rows, &format!("{MOVE_TO_WORKSPACE}:91")).expect("the workspace is offered");
    assert!(across.enabled, "{across:?}");
    assert_eq!(across.label, "Move to workspace Studio");
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, &across.id, None, "g"),
        Some(Command::MoveToWorkspace { ref workspace, .. }) if workspace == "91"
    ));
    handle
        .choose_window_menu(route.tab, &across.id, None, "g")
        .unwrap();
    settle(&mut frame);
    let home: Vec<WorkspaceId> = handle.read(|tree| {
        tree.workspaces
            .iter()
            .filter(|workspace| {
                workspace.strips.iter().any(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .any(|group| group.tabs.iter().any(|tab| tab.id == route.tab))
                })
            })
            .map(|workspace| workspace.id)
            .collect()
    });
    assert_eq!(home, vec![WorkspaceId(91)], "the window is there now");
}

/// A window shows on another display, by that display's name, as the
/// keyboard chord does.
#[test]
fn a_window_moves_to_another_display_by_its_name() {
    let (surface, handle) = apps_desk(1);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let mut tree = handle.read(Clone::clone);
    let displays = vec![
        Display {
            id: 1,
            monitor: "left".into(),
            x: 0.0,
            y: 0.0,
            width: 1280.0,
            height: 800.0,
            primary: true,
        },
        Display {
            id: 2,
            monitor: "right".into(),
            x: 1280.0,
            y: 0.0,
            width: 1280.0,
            height: 800.0,
            primary: false,
        },
    ];
    tree.outputs.reconcile(&tree.workspaces, &displays);
    tree.outputs.place_unhomed(&tree.workspaces);
    handle.replace_tree(tree);
    settle(&mut frame);
    assert_eq!(
        handle.read(|tree| tree.outputs.display_of(route.group)),
        Some(1),
        "it starts on the first display"
    );

    let rows = handle.window_menu(route.tab);
    let across = row(&rows, &format!("{MOVE_TO_DISPLAY}:2")).expect("the display is offered");
    assert!(across.enabled, "{across:?}");
    assert_eq!(across.label, "Move to display Display 2");
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, &across.id, None, "g"),
        Some(Command::MoveToDisplay { display: 2, .. })
    ));
    handle
        .choose_window_menu(route.tab, &across.id, None, "g")
        .unwrap();
    settle(&mut frame);
    assert_eq!(
        handle.read(|tree| tree.outputs.display_of(route.group)),
        Some(2),
        "the tree shows it there now"
    );
    // On the display it is on, the row offers the other one instead.
    let rows = handle.window_menu(route.tab);
    assert!(row(&rows, &format!("{MOVE_TO_DISPLAY}:2")).is_none());
    assert!(row(&rows, &format!("{MOVE_TO_DISPLAY}:1")).is_some());
}

/// The new tab row opens another tab of the window's app, right beside it.
#[test]
fn a_new_tab_row_opens_another_tab_of_its_app_beside_it() {
    let (surface, handle) = apps_desk(1);
    let mut frame = present(surface);
    let route = first_route(&handle);
    handle
        .choose_window_menu(route.tab, NEW_TAB, None, "g")
        .unwrap();
    settle(&mut frame);
    let group = handle.read(|tree| group_at(tree, route));
    assert_eq!(group.tabs.len(), 2, "the tab opened beside it");
    assert_eq!(group.tabs[1].id, group.active_tab, "and in front");
    assert_eq!(
        tilo_desktop_model::apps::canonical_app_id(&group.tabs[0].tile),
        tilo_desktop_model::apps::canonical_app_id(&group.tabs[1].tile),
        "it is the same app"
    );
    // Two tabs make the window a tabbed one: it offers the tab's own rows.
    let rows = handle.window_menu(route.tab);
    assert!(row(&rows, CLOSE).is_some());
    assert!(row(&rows, SPLIT_RIGHT).is_some());
}

/// The share row is offered exactly for the windows the sharing app lists,
/// and choosing it makes the request the window bar's item makes.
#[test]
fn only_a_window_the_sharing_app_lists_offers_the_share_row() {
    let (_surface, handle) = apps_desk(1);
    let route = first_route(&handle);
    assert!(
        row(&handle.window_menu(route.tab), SHARE).is_none(),
        "a window the sharing app does not list offers no share"
    );

    let window = OpenWindow {
        tab: route.tab,
        app: "so.tilo.app0".into(),
        app_title: "App0".into(),
        title: "App0".into(),
        record: String::new(),
        kind: String::new(),
        item: None,
    };
    handle.shared.borrow_mut().window_offers = vec![window.clone()];
    let rows = handle.window_menu(route.tab);
    let share = row(&rows, SHARE).expect("the window is listed: it offers the share");
    assert_eq!(
        share.label,
        ui::window_share::host::menu_item(&window, &ui::window_share::shared()).name
    );
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, SHARE, None, "g"),
        Some(Command::ShareWindow { shared: true, .. })
    ));
    assert!(
        handle
            .choose_window_menu(route.tab, SHARE, None, "g")
            .is_ok(),
        "it is sent to the sharing app as the bar's item is"
    );

    // A window the sharing app holds is offered the way back instead.
    let shared = Facts {
        sharing: Some(Sharing {
            label: "Stop sharing".into(),
            shared: true,
        }),
        ..Facts::default()
    };
    let rows = super::rows(&tree, &route, &shared);
    assert!(row(&rows, SHARE).is_none());
    assert_eq!(
        row(&rows, UN_SHARE).map(|row| row.label.as_str()),
        Some("Stop sharing")
    );
    assert!(matches!(
        command(&tree, &route, UN_SHARE, None, "g"),
        Some(Command::ShareWindow { shared: false, .. })
    ));
}

/// Only a host (non-Tilo) window's tab offers a name; it names the window
/// in place, and a name too long to show is refused.
#[test]
fn only_a_host_windows_tab_offers_a_name() {
    let (surface, handle) = apps_desk(1);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let mut tree = handle.read(Clone::clone);
    tree.workspaces[0].strips[0].window_groups[0].tabs[0].tile = TileContent::Foreign {
        app_id: "Alacritty".into(),
        surface_id: 1,
    };
    handle.replace_tree(tree);
    settle(&mut frame);

    let rows = handle.window_menu(route.tab);
    let name = row(&rows, RENAME).expect("a host window's tab is named");
    assert_eq!(name.label, "Name this window…");
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, RENAME, Some("Shell"), "g"),
        Some(Command::Rename { .. })
    ));
    assert!(
        command(&tree, &route, RENAME, None, "g").is_none(),
        "a row without a name is no command"
    );
    let long = "x".repeat(crate::navigation::MAX_NAME_CHARS.saturating_add(1));
    assert!(
        handle
            .choose_window_menu(route.tab, RENAME, Some(&long), "g")
            .is_err(),
        "a name too long to show is refused"
    );
    handle
        .choose_window_menu(route.tab, RENAME, Some("Shell"), "g")
        .unwrap();
    settle(&mut frame);
    let named = handle
        .snapshot("g", vec![])
        .entries
        .into_iter()
        .find(|entry| entry.route.tab == route.tab.to_string())
        .map(|entry| entry.label)
        .unwrap();
    assert_eq!(named, "Shell");
}

/// Split right takes the tab out into a window of its own, right after the
/// one it was in.
#[test]
fn split_right_splits_a_tab_into_the_window_beside_it() {
    let (surface, handle) = one_window_desk(2);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let rows = handle.window_menu(route.tab);
    assert!(row(&rows, SPLIT_RIGHT).is_some());
    handle
        .choose_window_menu(route.tab, SPLIT_RIGHT, None, "g")
        .unwrap();
    settle(&mut frame);
    let groups: Vec<(u64, Vec<TabId>)> = handle.read(|tree| {
        tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
            .collect()
    });
    assert_eq!(
        groups.len(),
        2,
        "the strip holds the source and the new window"
    );
    assert_eq!(groups[0].0, route.group, "the source stays where it was");
    assert_eq!(
        groups[1].1,
        vec![route.tab],
        "the tab has a window to itself"
    );
    assert_eq!(
        handle.read(|tree| tree.active_group.get(&route.strip).copied()),
        Some(groups[1].0),
        "and the strip shows it"
    );
}

/// Move to a new window takes the tab to a window of its own at the strip's
/// end.
#[test]
fn a_tab_moves_to_a_new_window_at_the_strips_end() {
    let (surface, handle) = one_window_desk(3);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let rows = handle.window_menu(route.tab);
    assert!(row(&rows, NEW_GROUP).is_some());
    handle
        .choose_window_menu(route.tab, NEW_GROUP, None, "g")
        .unwrap();
    settle(&mut frame);
    let groups: Vec<(u64, Vec<TabId>)> = handle.read(|tree| {
        tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
            .collect()
    });
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].0, route.group);
    assert_eq!(groups[0].1.len(), 2, "the other tabs stay together");
    assert_eq!(
        groups.last().map(|(_, tabs)| tabs.as_slice()),
        Some([route.tab].as_slice()),
        "the tab is in a window of its own at the end"
    );
}

/// A tabbed window offers closing the tab and closing the window; closing
/// the window takes every tab with it, and a reopen brings it back.
#[test]
fn closing_the_tab_leaves_the_window_that_closing_it_ends() {
    let (surface, handle) = one_window_desk(2);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let other = handle.read(|tree| {
        tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .iter()
            .map(|tab| tab.id)
            .find(|tab| *tab != route.tab)
            .unwrap()
    });
    let rows = handle.window_menu(route.tab);
    assert!(row(&rows, CLOSE).is_some());
    assert!(row(&rows, CLOSE_WINDOW).is_some());

    handle
        .choose_window_menu(route.tab, CLOSE, None, "g")
        .unwrap();
    settle(&mut frame);
    let group = handle.read(|tree| group_at(tree, route));
    assert_eq!(group.tabs.len(), 1, "the tab closed, the window stayed");
    assert_eq!(group.tabs[0].id, other, "the other tab is what is left");

    handle
        .choose_window_menu(other, CLOSE_WINDOW, None, "g")
        .unwrap();
    settle(&mut frame);
    assert!(
        handle.read(|tree| tree.workspaces[0].strips[0].window_groups.is_empty()),
        "the window and its tab are gone"
    );
    assert!(handle.snapshot("g", vec![]).entries.is_empty());
    handle
        .dispatch(
            &Command::Reopen {
                generation: "g".into(),
            },
            "g",
        )
        .unwrap();
    settle(&mut frame);
    assert_eq!(
        handle.read(|tree| tree.workspaces[0].strips[0].window_groups.len()),
        1,
        "a reopen undoes the close"
    );
}

/// A window running an app that controls the desktop offers to end that,
/// and the row is gone once the grant is taken back.
#[test]
fn stopping_desktop_control_takes_the_grant_back() {
    let (surface, handle) = apps_desk(1);
    let mut frame = present(surface);
    let route = first_route(&handle);
    assert!(row(&handle.window_menu(route.tab), STOP_DESKTOP_CONTROL).is_none());
    let mut tree = handle.read(Clone::clone);
    tree.workspaces[0].grant_app(
        "so.tilo.app0",
        &Capability::DesktopControl {
            scope: GRANT_ALWAYS,
        },
    );
    handle.replace_tree(tree);
    let rows = handle.window_menu(route.tab);
    let stop = row(&rows, STOP_DESKTOP_CONTROL).expect("the app controls the desktop");
    assert!(stop.enabled);
    assert_eq!(stop.label, "Stop controlling the desktop");
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, STOP_DESKTOP_CONTROL, None, "g"),
        Some(Command::StopDesktopControl { .. })
    ));
    handle
        .choose_window_menu(route.tab, STOP_DESKTOP_CONTROL, None, "g")
        .unwrap();
    settle(&mut frame);
    assert!(
        !handle.read(|tree| tree.workspaces[0]
            .holds_desktop_control_at("so.tilo.app0", tilo_clock::unix_now())),
        "the grant is taken back"
    );
    assert!(row(&handle.window_menu(route.tab), STOP_DESKTOP_CONTROL).is_none());
}

/// A window someone else holds is theirs to move and to close: those rows
/// say whose it is, and choosing one is refused.
#[test]
fn someone_elses_window_offers_no_move_nor_close() {
    let (_surface, handle) = one_window_desk(2);
    let route = first_route(&handle);
    assert!(handle.set_holders(HashMap::from([(
        route.tab,
        TabHolder {
            name: "Ben".into(),
            rgb: (200, 120, 40),
        }
    )])));
    let rows = handle.window_menu(route.tab);
    for id in [CLOSE, CLOSE_WINDOW, SPLIT_RIGHT, NEW_GROUP, NEW_TAB] {
        let held = row(&rows, id).unwrap_or_else(|| panic!("{id} is offered"));
        assert!(!held.enabled, "{id} is theirs: {held:?}");
        assert_eq!(held.reason.as_deref(), Some("Ben opened this window"));
    }
    assert!(
        row(&rows, MAXIMIZE).unwrap().enabled,
        "still theirs to fill"
    );
    assert!(
        handle
            .choose_window_menu(route.tab, CLOSE, None, "g")
            .is_err(),
        "choosing it is refused"
    );
}

/// A window the tree no longer holds offers no rows and no command.
#[test]
fn a_window_that_is_gone_offers_nothing() {
    let (_surface, handle) = desk(1);
    let route = first_route(&handle);
    let mut gone = handle.read(Clone::clone);
    gone.workspaces[0].strips[0]
        .window_groups
        .retain(|group| group.id != route.group);
    handle.replace_tree(gone);
    let tree = handle.read(Clone::clone);
    assert!(super::rows(&tree, &route, &Facts::default()).is_empty());
    assert!(command(&tree, &route, CLOSE, None, "g").is_none());
}
