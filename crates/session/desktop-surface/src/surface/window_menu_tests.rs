//! The window menu's rows and the commands they mean.

use super::*;
use crate::surface::{DesktopHandle, DesktopSurface};
use std::collections::HashMap;
use tilo_desktop_model::width::{WIDTH_NARROW_PERMILLE, WIDTH_WIDE_PERMILLE};
use tilo_desktop_model::{DesktopTree, WindowWidth};
use ui::widget::TabHolder;

fn desk(windows: usize) -> (DesktopSurface, DesktopHandle) {
    crate::surface::tests::desktop(windows)
}

fn first_route(handle: &DesktopHandle) -> (RouteIds, DesktopTree) {
    handle.read(|tree| {
        let tab = tree.workspaces[0].strips[0].window_groups[0].tabs[0].id;
        (crate::surface::route_of(tree, tab).unwrap(), tree.clone())
    })
}

fn row<'a>(rows: &'a [Row], id: &str) -> Option<&'a Row> {
    rows.iter().find(|row| row.id == id)
}

/// Every row a window offers means a command the desktop answers.
#[test]
fn a_windows_menu_offers_what_the_desktop_can_do_to_it() {
    let (_surface, handle) = desk(2);
    let (route, tree) = first_route(&handle);
    let rows = super::rows(&tree, &route, None);
    let ids: Vec<&str> = rows.iter().map(|row| row.id).collect();
    assert_eq!(ids, vec![RENAME, MAXIMIZE, WIDER, NARROWER, CLOSE]);
    assert!(rows.iter().all(|row| row.enabled), "{rows:?}");
    assert_eq!(row(&rows, RENAME).unwrap().label, "Name this window…");
    assert_eq!(row(&rows, CLOSE).unwrap().label, "Close");
    for offered in &rows {
        assert!(
            command(&tree, &route, offered.id, Some("A name"), "g").is_some(),
            "{offered:?} means a command"
        );
    }
    // Filling the strip is offered; once filled the menu gives it back.
    let fill = command(&tree, &route, MAXIMIZE, None, "g").unwrap();
    assert!(matches!(
        fill,
        Command::Maximize {
            maximized: true,
            ..
        }
    ));
    let mut filled = tree;
    filled.workspaces[0].strips[0].window_groups[0].maximized = true;
    let rows = super::rows(&filled, &route, None);
    assert!(row(&rows, MAXIMIZE).is_none());
    assert!(row(&rows, RESTORE).is_some());
}

/// A window widens and narrows one step at a time, and stops at both ends
/// with a row that says why.
#[test]
fn widths_step_between_the_three_a_window_takes() {
    let (_surface, handle) = desk(1);
    let (route, tree) = first_route(&handle);
    let step = |tree: &DesktopTree, id: &str| match command(tree, &route, id, None, "g") {
        Some(Command::Width { permille, .. }) => {
            let mut next = tree.clone();
            next.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Fraction(permille);
            Some((permille, next))
        }
        _ => None,
    };
    let (wide, wider) = step(&tree, WIDER).expect("wider from the normal width");
    assert_eq!(wide, WIDTH_WIDE_PERMILLE);
    let rows = super::rows(&wider, &route, None);
    let widest = row(&rows, WIDER).unwrap();
    assert!(!widest.enabled, "no wider than the strip");
    assert_eq!(
        widest.reason.as_deref(),
        Some("As wide as the strip"),
        "the row says why"
    );
    assert!(step(&wider, WIDER).is_none(), "and it means no command");
    let (normal, narrower) = step(&wider, NARROWER).expect("narrower again");
    assert!(normal < wide);
    let (narrow, narrowest) = step(&narrower, NARROWER).expect("narrower again");
    assert_eq!(narrow, WIDTH_NARROW_PERMILLE);
    let rows = super::rows(&narrowest, &route, None);
    assert!(!row(&rows, NARROWER).unwrap().enabled);
    assert!(step(&narrowest, NARROWER).is_none());
}

/// A window someone else holds cannot be named or closed, and its rows say
/// whose it is.
#[test]
fn someone_elses_window_offers_neither_a_name_nor_a_close() {
    let (_surface, handle) = desk(1);
    let (route, _tree) = first_route(&handle);
    let tab = route.tab;
    assert!(handle.set_holders(HashMap::from([(
        tab,
        TabHolder {
            name: "Ben".into(),
            rgb: (200, 120, 40),
        }
    )])));
    let rows = handle.window_menu(tab);
    let name = row(&rows, RENAME).unwrap();
    assert!(!name.enabled);
    assert_eq!(name.reason.as_deref(), Some("Ben opened this window"));
    assert!(!row(&rows, CLOSE).unwrap().enabled);
    assert!(
        row(&rows, MAXIMIZE).unwrap().enabled,
        "still theirs to fill"
    );
    assert!(
        handle.choose_window_menu(tab, CLOSE, None, "g").is_err(),
        "choosing it is refused"
    );
}

/// Choosing a row names, fills and closes the window it is for.
#[test]
fn choosing_a_row_names_and_closes_the_window_it_is_for() {
    let (surface, handle) = desk(2);
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    frame.relayout();
    let (route, _tree) = first_route(&handle);
    handle
        .choose_window_menu(route.tab, RENAME, Some("Quarterly plan"), "g")
        .unwrap();
    for _ in 0..4 {
        frame.relayout();
    }
    let named = |handle: &DesktopHandle| {
        handle
            .snapshot("g", vec![])
            .entries
            .into_iter()
            .find(|entry| entry.route.tab == route.tab.to_string())
            .map(|entry| entry.label)
            .unwrap()
    };
    assert_eq!(named(&handle), "Quarterly plan");
    handle
        .choose_window_menu(route.tab, MAXIMIZE, None, "g")
        .unwrap();
    for _ in 0..4 {
        frame.relayout();
    }
    assert!(handle.read(|tree| tree.workspaces[0].strips[0].window_groups[0].maximized));
    assert!(row(&handle.window_menu(route.tab), RESTORE).is_some());
    handle
        .choose_window_menu(route.tab, CLOSE, None, "g")
        .unwrap();
    for _ in 0..4 {
        frame.relayout();
    }
    assert_eq!(
        handle.snapshot("g", vec![]).entries.len(),
        1,
        "the window closed"
    );
    assert!(
        handle
            .choose_window_menu(route.tab, CLOSE, None, "g")
            .is_err(),
        "a closed window offers nothing"
    );
}

/// A name too long to show is no command at all, and a rename row without a
/// name is not one either.
#[test]
fn a_name_too_long_to_show_is_no_command() {
    let (_surface, handle) = desk(1);
    let (route, tree) = first_route(&handle);
    let long = "x".repeat(crate::navigation::MAX_NAME_CHARS.saturating_add(1));
    assert!(command(&tree, &route, RENAME, Some(&long), "g").is_none());
    assert!(command(&tree, &route, RENAME, None, "g").is_none());
}

/// A window the tree no longer holds offers no rows and no command.
#[test]
fn a_window_that_is_gone_offers_nothing() {
    let (_surface, handle) = desk(1);
    let (route, _tree) = first_route(&handle);
    let mut gone = handle.read(Clone::clone);
    gone.workspaces[0].strips[0]
        .window_groups
        .retain(|group| group.id != route.group);
    handle.replace_tree(gone);
    let tree = handle.read(Clone::clone);
    assert!(super::rows(&tree, &route, None).is_empty());
    assert!(command(&tree, &route, CLOSE, None, "g").is_none());
}
