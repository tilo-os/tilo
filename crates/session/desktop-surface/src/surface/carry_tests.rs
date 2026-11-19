//! A window carried by its bar: what its bar carries, and what a window carried from another of
//! the person's devices does when it is let go over a window here.

use super::tests::{desktop, resource_of};
use super::*;
use ui::headless::FrameHarness;
use ui::state::State;

/// A shell of `windows` windows, laid out on a screen: what its bars carry once a pass has set
/// them.
fn framed(windows: usize) -> (FrameHarness, DesktopHandle) {
    let (surface, handle) = desktop(windows);
    let resources = handle.read(|tree| {
        tree.tabs()
            .map(|tab| (tab.id, resource_of(tab.id)))
            .collect()
    });
    handle.set_window_resources(resources);
    let mut frame = FrameHarness::for_root(State::new(), surface, (1280, 800), Theme::dark());
    frame.relayout();
    (frame, handle)
}

#[test]
fn a_windows_bar_carries_the_resource_its_place_shows() {
    let (frame, _handle) = framed(2);
    let row = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .expect("a strip stands");
    assert_eq!(row.groups.len(), 2, "fixture: two windows side by side");
    for group in &row.groups {
        assert_eq!(
            group.carries(),
            Some(resource_of(group.active_tab_id()).as_str()),
            "the bar lifts the resource its place in front shows"
        );
    }
}

#[test]
fn a_press_on_a_windows_title_carries_it_with_the_pointer() {
    let (mut frame, _handle) = framed(1);
    let (at, carried, group) = {
        let row = frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .expect("a strip stands");
        let group = &row.groups[0];
        assert_eq!(group.tabs().len(), 1, "fixture: a window of one tab");
        let pill = group.tab_rects()[0];
        // The pill's chips and its edges are its own: the point on the title that lifts the window
        // is found on it.
        let at = [0.1, 0.25, 0.5, 0.75, 0.9]
            .into_iter()
            .map(|at| Point::new(pill.x0 + pill.width() * at, pill.center().y))
            .find(|at| group.bar_grab_hit(*at))
            .expect("the window of one tab is lifted by its title");
        (
            at,
            group
                .carries()
                .expect("the bar carries what its place shows")
                .to_owned(),
            group.id,
        )
    };
    // The press travels across the bar as a hand's does.
    frame.send(&ui::synthetic_input::cursor_moved(at.x, at.y));
    frame.send(&ui::synthetic_input::mouse_press());
    for step in 1..=4 {
        frame.send(&ui::synthetic_input::cursor_moved(
            at.x + 60.0 * f64::from(step) / 4.0,
            at.y,
        ));
    }
    let flight = ui::widget::drag_bus::current().expect("the bar carries the window");
    assert_eq!(flight.source, format!("window:{group}"));
    let ui::dnd::DragPayload::Custom { blob, .. } = flight.payload else {
        panic!("a window's bar carries a resource");
    };
    assert_eq!(
        String::from_utf8(blob).expect("a resource is text"),
        carried
    );
    frame.send(&ui::synthetic_input::mouse_release());
    assert!(
        ui::widget::drag_bus::current().is_none(),
        "letting go here takes the window off the bus"
    );
}

#[test]
fn a_place_in_another_workspace_joins_the_window_it_was_let_go_over() {
    // The second window stands in a workspace behind, showing what its place there shows.
    let (surface, handle) = desktop(2);
    let dragged = {
        let mut tree = handle.read(Clone::clone);
        let mut elsewhere = tilo_desktop_model::Workspace::new(WorkspaceId::new(), "Elsewhere");
        elsewhere.strips.push(tilo_desktop_model::Strip::new(
            StripId(handle.mint_id()),
            "Main",
        ));
        let moving = tree.workspaces[0].strips[0]
            .window_groups
            .pop()
            .expect("fixture: a second window");
        let dragged = moving.active_tab;
        elsewhere.strips[0].window_groups.push(moving);
        tree.workspaces.push(elsewhere);
        handle.replace_tree(tree.clone());
        let resources = tree
            .tabs()
            .map(|tab| (tab.id, resource_of(tab.id)))
            .collect();
        handle.set_window_resources(resources);
        dragged
    };
    let mut frame = FrameHarness::for_root(State::new(), surface, (1280, 800), Theme::dark());
    frame.relayout();
    let onto = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .expect("a strip stands")
        .groups[0]
        .get_rect()
        .center();
    ui::widget::drag_bus::end(Some(ui::widget::drag_bus::Landed {
        source: "carried-here".into(),
        payload: ui::dnd::DragPayload::Custom {
            kind: ui::widget::RESOURCE_PAYLOAD.to_owned(),
            blob: resource_of(dragged).into_bytes(),
        },
        pointer: onto,
    }));
    frame.relayout();
    let front = handle.read(|tree| tree.workspaces[0].clone());
    let group = &front.strips[0].window_groups[0];
    assert!(
        group.tabs.iter().any(|tab| tab.id == dragged),
        "the place joins the window it was let go over, from the workspace it stood in: {group:?}"
    );
    assert_eq!(group.active_tab, dragged, "and it stands in front there");
    let behind = handle.read(|tree| tree.workspaces[1].clone());
    assert!(
        behind
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .all(|group| group.tabs.iter().all(|tab| tab.id != dragged)),
        "the place left the workspace it stood in: {behind:?}"
    );
}

#[test]
fn a_window_carried_here_joins_the_window_it_was_let_go_over() {
    let (mut frame, handle) = framed(2);
    let (onto, carried, dragged) = {
        let row = frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .expect("a strip stands");
        let dragged = row.groups[1].active_tab_id();
        let carried = row.groups[1]
            .carries()
            .expect("the window's bar carries what it shows")
            .to_owned();
        (row.groups[0].get_rect().center(), carried, dragged)
    };
    // The release of a window another of the person's devices carried here.
    ui::widget::drag_bus::end(Some(ui::widget::drag_bus::Landed {
        source: "carried-here".into(),
        payload: ui::dnd::DragPayload::Custom {
            kind: ui::widget::RESOURCE_PAYLOAD.to_owned(),
            blob: carried.into_bytes(),
        },
        pointer: onto,
    }));
    frame.relayout();
    let workspace = handle.read(|tree| tree.workspaces[0].clone());
    let group = &workspace.strips[0].window_groups[0];
    assert!(
        group.tabs.iter().any(|tab| tab.id == dragged),
        "the place showing the carried resource is a tab of the window it was let go over: {group:?}"
    );
    assert_eq!(
        group.active_tab, dragged,
        "and it stands in front of that window"
    );
}
