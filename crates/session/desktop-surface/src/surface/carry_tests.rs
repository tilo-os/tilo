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
