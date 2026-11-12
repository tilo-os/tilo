//! The window menu's rows and the commands they mean.

use super::*;
use crate::TileSource;
use crate::navigation::{ChromeEffect, DeskEffect, Verb};
use crate::surface::tests::{desk_with, one_window_desk};
use crate::surface::{DesktopHandle, DesktopSurface};
use std::collections::HashMap;
use tilo_desktop_model::width::{
    WIDTH_NARROW_PERMILLE, WIDTH_NORMAL_PERMILLE, WIDTH_WIDE_PERMILLE,
};
use tilo_desktop_model::{
    Capability, DesktopTree, Display, GRANT_ALWAYS, Strip, StripId, TabId, TileContent,
    WindowWidth, Workspace, WorkspaceSlot,
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

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str, _: Option<TabId>) -> Option<Tab> {
        Some(Tab {
            id,
            title: app.to_owned(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: app.into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
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

/// The tab of the `group` th window of the first strip.
fn tab_at(handle: &DesktopHandle, group: usize) -> TabId {
    handle.read(|tree| tree.workspaces[0].strips[0].window_groups[group].tabs[0].id)
}

/// The group at `route` , as the tree holds it.
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

/// The row `id` , in the menu or in one of its submenus.
fn row<'a>(rows: &'a [Row], id: &str) -> Option<&'a Row> {
    rows.iter().find_map(|row| row.find(id))
}

/// The ids of the rows the menu lists, its rules and the rows of its submenus included, in the
/// order a person reads them.
fn ids(rows: &[Row]) -> Vec<&str> {
    let mut found = Vec::new();
    for row in rows {
        if !row.separator {
            found.push(row.id.as_str());
        }
        found.extend(ids(&row.children));
    }
    found
}

/// A window offers every row the desktop has for it, in the groups the desktop shows them in, and
/// each means the command it names.
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
            FLOAT,
            WIDTH,
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
    let groups: Vec<Vec<&str>> = rows
        .split(|row| row.separator)
        .map(|group| group.iter().map(|row| row.id.as_str()).collect())
        .collect();
    assert_eq!(
        groups,
        vec![
            vec![FULLSCREEN, MAXIMIZE],
            vec![FLOAT],
            vec![WIDTH],
            vec![NEW_TAB],
            vec![CLOSE_WINDOW],
        ],
        "covering and filling the window, lifting it over the desk, its share of
            the strip, what it shows, and closing it"
    );
    // Nowhere to move it to: the place offers no row at all, rather than one that cannot be chosen.
    for place in [MOVE_TO_STRIP, MOVE_TO_WORKSPACE, MOVE_TO_DISPLAY] {
        assert!(row(&rows, place).is_none(), "{place}");
    }
    assert_eq!(
        row(&rows, WIDTH).map(|row| row.label.as_str()),
        Some("Width"),
        "the shares are nested under one row"
    );
    for listed in rows.iter().filter(|row| !row.separator) {
        assert!(listed.icon.is_some(), "every row carries its picture");
        if !listed.children.is_empty() {
            assert!(
                command(&tree, &route, &listed.id, Some("A name")).is_none(),
                "a row that opens a submenu is not itself a command"
            );
            continue;
        }
        assert!(
            command(&tree, &route, &listed.id, Some("A name")).is_some(),
            "{listed:?} means a command"
        );
        assert!(
            listed.children.is_empty(),
            "{listed:?} is a command, not a submenu"
        );
    }
    // A window of one tab is its own tab: it offers no split and no close of the tab alone.
    assert!(row(&rows, CLOSE).is_none());
    assert!(row(&rows, SPLIT_RIGHT).is_none());
    assert_eq!(row(&rows, CLOSE_WINDOW).unwrap().label, "Close this window");
    // A Tilo app's window titles itself, so it offers no name.
    assert!(row(&rows, RENAME).is_none());
}

/// The row of a window asking for the person takes them to it, and is gone once they are there.
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
        command(&tree, &route, REVEAL, None),
        Some(Verb::Desk(DeskEffect::Focus { .. }))
    ));

    let mut frame = present(surface);
    handle
        .choose_window_menu(tab, REVEAL, None)
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

/// The full screen row covers the display with the window and gives it back, and the tree keeps
/// which window covers it.
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
        .choose_window_menu(route.tab, FULLSCREEN, None)
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
        .choose_window_menu(route.tab, EXIT_FULLSCREEN, None)
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

/// The float row lifts the window over the desk at the floating size, and the row then offers to
/// put it back into its strip.
#[test]
fn the_float_row_lifts_the_window_over_the_desk_and_puts_it_back() {
    let (surface, handle) = apps_desk(2);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let floating = |handle: &DesktopHandle| {
        handle.read(|tree| {
            tree.workspaces
                .iter()
                .any(|workspace| workspace.arrangement.float_of(route.group).is_some())
        })
    };
    let rect_of = |handle: &DesktopHandle, tab: TabId| {
        handle
            .shown_tabs()
            .into_iter()
            .find(|(drawn, _)| *drawn == tab)
            .map(|(_, rect)| rect)
            .expect("the window is drawn")
    };

    assert!(!floating(&handle));
    let was = rect_of(&handle, route.tab);
    assert!(row(&handle.window_menu(route.tab), FLOAT).is_some());
    assert!(row(&handle.window_menu(route.tab), SNAP_BACK).is_none());

    handle.choose_window_menu(route.tab, FLOAT, None).unwrap();
    settle(&mut frame);
    assert!(floating(&handle), "the record has it floating");
    let floated = rect_of(&handle, route.tab);
    assert!(
        (floated.width() - 640.0).abs() < 1.0,
        "it takes the floating width: {floated:?}"
    );
    let after = rect_of(&handle, tab_at(&handle, 1));
    assert!(
        (after.x0 - was.x0).abs() < 16.0,
        "the window after it takes the place it had in the strip: {after:?}"
    );
    let rows = handle.window_menu(route.tab);
    assert!(row(&rows, FLOAT).is_none(), "it reads as snapping back");
    assert!(row(&rows, SNAP_BACK).is_some());

    handle
        .choose_window_menu(route.tab, SNAP_BACK, None)
        .unwrap();
    settle(&mut frame);
    assert!(!floating(&handle), "the record has it back in its strip");
    let back = rect_of(&handle, route.tab);
    assert!(
        (back.x0 - was.x0).abs() < 0.5,
        "it stands where it stood in the strip again: {back:?}"
    );
    let beside = rect_of(&handle, tab_at(&handle, 1));
    assert!(
        (beside.width() - back.width()).abs() < 1.0,
        "the two windows share the strip again: {back:?} and {beside:?}"
    );
    assert_eq!(row(&handle.window_menu(route.tab), SNAP_BACK), None);
}

/// Filling the strip is offered, and once filled the row gives the strip back.
#[test]
fn filling_the_strip_is_offered_and_then_giving_it_back() {
    let (surface, handle) = apps_desk(1);
    let mut frame = present(surface);
    let route = first_route(&handle);
    assert!(row(&handle.window_menu(route.tab), MAXIMIZE).is_some());
    handle
        .choose_window_menu(route.tab, MAXIMIZE, None)
        .unwrap();
    settle(&mut frame);
    assert!(handle.read(|tree| group_at(tree, route).maximized));
    let rows = handle.window_menu(route.tab);
    assert!(row(&rows, MAXIMIZE).is_none());
    assert!(row(&rows, RESTORE).is_some());
    handle.choose_window_menu(route.tab, RESTORE, None).unwrap();
    settle(&mut frame);
    assert!(!handle.read(|tree| group_at(tree, route).maximized));
}

/// Each width row gives the window the share of its strip it names, and the wider and narrower rows
/// step between the same shares, stopping at both ends with a row that says why.
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
                command(&tree, &route, id, None),
                Some(Verb::Chrome(ChromeEffect::SetWidth {
                    preset: Some(_),
                    ..
                }))
            ),
            "{id} is the share it names"
        );
        handle.choose_window_menu(route.tab, id, None).expect(id);
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
    assert_eq!(widest.reason.as_deref(), Some("Widest"), "the row says why");
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, NARROWER, None),
        Some(Verb::Chrome(ChromeEffect::SetWidth { permille: Some(permille), .. })) if permille == WIDTH_NORMAL_PERMILLE
    ));
    handle
        .choose_window_menu(route.tab, NARROWER, None)
        .unwrap();
    settle(&mut frame);
    assert_eq!(
        handle.read(|tree| group_at(tree, route).width.permille()),
        WIDTH_NORMAL_PERMILLE
    );
    handle
        .choose_window_menu(route.tab, NARROWER, None)
        .unwrap();
    settle(&mut frame);
    assert_eq!(
        handle.read(|tree| group_at(tree, route).width.permille()),
        WIDTH_NARROW_PERMILLE
    );
    let rows = handle.window_menu(route.tab);
    let narrowest = row(&rows, NARROWER).unwrap();
    assert!(!narrowest.enabled, "no narrower than a window goes");
    assert_eq!(narrowest.reason.as_deref(), Some("Narrowest"));
    assert!(
        handle
            .choose_window_menu(route.tab, NARROWER, None)
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
        .choose_window_menu(route.tab, EQUALIZE, None)
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
    assert_eq!(
        across.label, "Research",
        "the submenu names the places it goes to"
    );
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, &across.id, None),
        Some(Verb::Chrome(ChromeEffect::MoveTab { to: Destination { strip: Some(ref strip), .. }, .. })) if strip == "90"
    ));
    handle
        .choose_window_menu(route.tab, &across.id, None)
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
        .push(Workspace::new(WorkspaceSlot(91), "Studio"));
    handle.replace_tree(tree);
    settle(&mut frame);

    let rows = handle.window_menu(route.tab);
    let across = row(&rows, &format!("{MOVE_TO_WORKSPACE}:91")).expect("the workspace is offered");
    assert!(across.enabled, "{across:?}");
    assert_eq!(
        across.label, "Studio",
        "the submenu names the workspaces it goes to"
    );
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, &across.id, None),
        Some(Verb::Chrome(ChromeEffect::MoveTab { to: Destination { workspace: Some(ref workspace), .. }, .. })) if workspace == "91"
    ));
    handle
        .choose_window_menu(route.tab, &across.id, None)
        .unwrap();
    settle(&mut frame);
    let home: Vec<WorkspaceSlot> = handle.read(|tree| {
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
            .map(|workspace| workspace.slot)
            .collect()
    });
    assert_eq!(home, vec![WorkspaceSlot(91)], "the window is there now");
}

/// Renders every tab as a notice of its own, and records the windows the session is asked to carry,
/// as the session's own source does.
struct Carrying {
    carried: std::rc::Rc<std::cell::RefCell<Vec<(TabId, WorkspaceSlot)>>>,
}

impl TileSource for Carrying {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> ui::widget::TileHost {
        ui::widget::TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn carry(&self, tab: TabId, to: WorkspaceSlot) -> bool {
        self.carried.borrow_mut().push((tab, to));
        true
    }
}

/// The window of an app this device started runs in the workspace that started it, with its grants
/// and its records: its tab does not cross by itself, and the desk hands the move to the session
/// instead, leaving the tab where it is until the session carries it.
#[test]
fn a_running_apps_window_is_handed_to_the_session_instead_of_crossing_by_its_tab() {
    let carried = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let (surface, handle) = desk_with(
        1,
        Box::new(Carrying {
            carried: std::rc::Rc::clone(&carried),
        }),
    );
    let mut frame = present(surface);
    let route = first_route(&handle);
    let mut tree = handle.read(Clone::clone);
    let home = tree.workspaces[0].slot;
    tree.workspaces
        .push(Workspace::new(WorkspaceSlot(91), "Studio"));
    // What the compositor mapped: a window of an app this device started.
    tree.workspaces[0].strips[0].window_groups[0].tabs[0].tile = TileContent::Foreign {
        app_id: "so.tilo.notes".into(),
        surface_id: 11,
    };
    handle.replace_tree(tree);
    settle(&mut frame);

    handle
        .choose_window_menu(route.tab, &format!("{MOVE_TO_WORKSPACE}:91"), None)
        .unwrap();
    settle(&mut frame);

    assert_eq!(
        *carried.borrow(),
        vec![(route.tab, WorkspaceSlot(91))],
        "the session is asked to carry the window where the person put it"
    );
    let below: Vec<WorkspaceSlot> = handle.read(|tree| {
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
            .map(|workspace| workspace.slot)
            .collect()
    });
    assert_eq!(
        below,
        vec![home],
        "the tab stays where it was until the app runs in the other workspace"
    );
}

/// A program of this machine runs with no workspace around it: no grants, no records. The session
/// carries its window at once, as it does a Tilo app's after the app has started again — the
/// surface only ever hands the move over, and the session decides what crosses.
#[test]
fn a_programs_window_is_handed_over_too_and_the_session_moves_it_at_once() {
    let carried = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let (surface, handle) = desk_with(
        1,
        Box::new(Carrying {
            carried: std::rc::Rc::clone(&carried),
        }),
    );
    let mut frame = present(surface);
    let route = first_route(&handle);
    let mut tree = handle.read(Clone::clone);
    tree.workspaces
        .push(Workspace::new(WorkspaceSlot(91), "Studio"));
    tree.workspaces[0].strips[0].window_groups[0].tabs[0].tile = TileContent::Foreign {
        app_id: "org.gnome.Nautilus".into(),
        surface_id: 12,
    };
    handle.replace_tree(tree);
    settle(&mut frame);

    handle
        .choose_window_menu(route.tab, &format!("{MOVE_TO_WORKSPACE}:91"), None)
        .unwrap();
    settle(&mut frame);

    assert_eq!(
        *carried.borrow(),
        vec![(route.tab, WorkspaceSlot(91))],
        "the window crosses only through the session"
    );
}

/// A window shows on another display, by that display's name, as the keyboard chord does.
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
    assert_eq!(
        across.label, "Display 2",
        "the submenu names the displays it goes to"
    );
    let tree = handle.read(Clone::clone);
    assert!(matches!(
        command(&tree, &route, &across.id, None),
        Some(Verb::Chrome(ChromeEffect::MoveTab { to: Destination { display: Some(ref display), .. }, .. })) if display == "2"
    ));
    handle
        .choose_window_menu(route.tab, &across.id, None)
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
    handle.choose_window_menu(route.tab, NEW_TAB, None).unwrap();
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
        command(&tree, &route, SHARE, None),
        Some(Verb::Chrome(ChromeEffect::ShareWindow { on: true, .. }))
    ));
    assert!(
        handle.choose_window_menu(route.tab, SHARE, None).is_ok(),
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
        command(&tree, &route, UN_SHARE, None),
        Some(Verb::Chrome(ChromeEffect::ShareWindow { on: false, .. }))
    ));
}

/// Only a host (non-Tilo) window's tab offers a name; it names the window in place, and a name too
/// long to show is refused.
#[test]
fn only_a_host_windows_tab_offers_a_name() {
    let (surface, handle) = apps_desk(1);
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1280, 800),
        ui::Theme::dark(),
    );
    frame.relayout();
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
        command(&tree, &route, RENAME, Some("Shell")),
        Some(Verb::Chrome(ChromeEffect::Rename { .. }))
    ));
    assert!(
        command(&tree, &route, RENAME, None).is_none(),
        "a row without a name is no command"
    );
    let long = "x".repeat(crate::navigation::MAX_NAME_CHARS.saturating_add(1));
    assert!(
        handle
            .choose_window_menu(route.tab, RENAME, Some(&long))
            .is_err(),
        "a name too long to show is refused"
    );
    handle
        .choose_window_menu(route.tab, RENAME, Some("Shell"))
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

    // A person types the name where the tab's title stands: the menu's Rename or a double click
    // opens the field in the window's bar, the desk holds the keyboard while it is open (so the
    // keys never reach the app), Enter names the tab and Escape leaves it as it was.
    let shown = |frame: &ui::headless::FrameHarness| {
        let surface = frame.root::<DesktopSurface>();
        let tab = surface
            .rows
            .values()
            .flat_map(|row| row.groups.iter())
            .flat_map(|group| group.tabs().iter())
            .find(|tab| tab.id == route.tab)
            .cloned()
            .unwrap();
        (tab.title, tab.named)
    };

    handle.choose_window_menu(route.tab, RENAME, None).unwrap();
    settle(&mut frame);
    assert!(handle.wants_keyboard(), "the name field holds the keyboard");
    frame.type_text("Shell 2");
    frame.key("Enter");
    settle(&mut frame);
    assert!(!handle.wants_keyboard(), "Enter gives the keyboard back");
    assert_eq!(shown(&frame), ("Shell 2".to_owned(), true));
    assert!(handle.read(|tree| tree.tab(route.tab).unwrap().is_named()));

    handle.choose_window_menu(route.tab, RENAME, None).unwrap();
    settle(&mut frame);
    frame.type_text("Nope");
    frame.key("Escape");
    settle(&mut frame);
    assert!(!handle.wants_keyboard(), "Escape gives the keyboard back");
    assert_eq!(
        shown(&frame),
        ("Shell 2".to_owned(), true),
        "and names nothing"
    );

    // A double click on the tab opens the same field.
    let bar = frame
        .root::<DesktopSurface>()
        .rows
        .values()
        .flat_map(|row| row.groups.iter())
        .next()
        .unwrap()
        .presentation_bounds()
        .tab_bar;
    frame.double_click_at(bar.x0 + 40.0, bar.center().y);
    settle(&mut frame);
    assert!(handle.wants_keyboard(), "the double click opens the field");
    frame.type_text("Build");
    frame.key("Enter");
    settle(&mut frame);
    assert_eq!(shown(&frame), ("Build".to_owned(), true));
}

/// Split right takes the tab out into a window of its own, right after the one it was in.
#[test]
fn split_right_splits_a_tab_into_the_window_beside_it() {
    let (surface, handle) = one_window_desk(2);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let rows = handle.window_menu(route.tab);
    assert!(row(&rows, SPLIT_RIGHT).is_some());
    handle
        .choose_window_menu(route.tab, SPLIT_RIGHT, None)
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

/// Move to a new window takes the tab to a window of its own at the strip's end.
#[test]
fn a_tab_moves_to_a_new_window_at_the_strips_end() {
    let (surface, handle) = one_window_desk(3);
    let mut frame = present(surface);
    let route = first_route(&handle);
    let rows = handle.window_menu(route.tab);
    assert!(row(&rows, NEW_GROUP).is_some());
    handle
        .choose_window_menu(route.tab, NEW_GROUP, None)
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

/// A tabbed window offers closing the tab and closing the window; closing the window takes every
/// tab with it, and a reopen brings it back.
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

    handle.choose_window_menu(route.tab, CLOSE, None).unwrap();
    settle(&mut frame);
    let group = handle.read(|tree| group_at(tree, route));
    assert_eq!(group.tabs.len(), 1, "the tab closed, the window stayed");
    assert_eq!(group.tabs[0].id, other, "the other tab is what is left");

    handle
        .choose_window_menu(other, CLOSE_WINDOW, None)
        .unwrap();
    settle(&mut frame);
    assert!(
        handle.read(|tree| tree.workspaces[0].strips[0].window_groups.is_empty()),
        "the window and its tab are gone"
    );
    assert!(handle.snapshot("g", vec![]).entries.is_empty());
    handle.dispatch(ChromeEffect::ReopenClosed).unwrap();
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
        command(&tree, &route, STOP_DESKTOP_CONTROL, None),
        Some(Verb::Chrome(ChromeEffect::StopDesktopControl { .. }))
    ));
    handle
        .choose_window_menu(route.tab, STOP_DESKTOP_CONTROL, None)
        .unwrap();
    settle(&mut frame);
    assert!(
        !handle.read(|tree| tree.workspaces[0]
            .holds_desktop_control_at("so.tilo.app0", tilo_clock::unix_now())),
        "the grant is taken back"
    );
    assert!(row(&handle.window_menu(route.tab), STOP_DESKTOP_CONTROL).is_none());
}

/// A window someone else holds is theirs to move and to close: those rows say whose it is, and
/// choosing one is refused.
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
        assert_eq!(held.reason.as_deref(), Some("Ben's window"));
    }
    assert!(
        row(&rows, MAXIMIZE).unwrap().enabled,
        "still theirs to fill"
    );
    assert!(
        handle.choose_window_menu(route.tab, CLOSE, None).is_err(),
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
    assert!(command(&tree, &route, CLOSE, None).is_none());
}
