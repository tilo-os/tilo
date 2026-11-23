//! The desktop's reopen chord on the real chrome: a window the person closed comes back where it
//! was, and the close is undone once.

use crate::support::group;
use crate::support::hold;
use crate::support::in_front;
use crate::support::press;
use crate::support::shell_and_chrome;
use crate::support::window;
use crate::support::windows_of;
use tilo_desktop_model::{Layout, Strip, StripId, WindowGroup, Workspace, WorkspaceId};
use tilo_session_core::desktop::navigation::LayoutEffect;
use winit::keyboard::{Key, ModifiersState};

fn layout_tree(groups: Vec<WindowGroup>) -> Layout {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: groups,
    }];
    let mut tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// A window closed from its ✕ comes back where it was when the person asks for it back: at its old
/// index in its group, in front, and only the once.
#[test]
fn the_reopen_chord_brings_a_closed_window_back_where_it_was() {
    let tree = layout_tree(vec![
        group(
            3,
            vec![
                window(20, "Notes", "so.tilo.notes"),
                window(30, "Chat", "so.tilo.chat"),
                window(40, "Files", "so.tilo.files"),
            ],
        ),
        group(4, vec![window(50, "Mail", "so.tilo.mail")]),
    ]);
    let (mut shell, mut frame, handle) = shell_and_chrome(&tree);
    assert_eq!(windows_of(&handle, "3"), ["20", "30", "40"]);

    // The window's ✕ asks the desktop to close it, as a page's does.
    let closed = handle.snapshot("g", vec![]).entries[1].route.clone();
    assert_eq!(closed.group, "3");
    handle
        .dispatch(LayoutEffect::CloseTab { tab: closed.tab })
        .unwrap();
    for _ in 0..4 {
        shell.relayout();
    }
    assert_eq!(windows_of(&handle, "3"), ["20", "40"]);

    hold(&mut frame, ModifiersState::SUPER | ModifiersState::SHIFT);
    press(&mut frame, Key::Character("t".into()));
    for _ in 0..4 {
        shell.relayout();
    }
    assert_eq!(
        windows_of(&handle, "3"),
        ["20", "30", "40"],
        "back at its old index in its group"
    );
    assert_eq!(in_front(&handle), Some(30), "and in front");

    // The close has been undone: the chord brings back nothing a second time, and the desktop still
    // answers it.
    press(&mut frame, Key::Character("t".into()));
    for _ in 0..4 {
        shell.relayout();
    }
    assert_eq!(windows_of(&handle, "3"), ["20", "30", "40"]);
    assert_eq!(windows_of(&handle, "4"), ["50"]);
}

/// The notice a close leaves says what closed, and its Undo is a button assistive technology and
/// the session's controls press by name: the press reopens the window as the chord does.
#[test]
fn a_reader_presses_the_undo_a_close_offers() {
    let tree = layout_tree(vec![
        group(3, vec![window(20, "Notes", "so.tilo.notes")]),
        group(4, vec![window(50, "Mail", "so.tilo.mail")]),
    ]);
    let (mut shell, mut frame, handle) = shell_and_chrome(&tree);
    let closed = handle.snapshot("g", vec![]).entries[0].route.clone();
    handle
        .dispatch(LayoutEffect::CloseTab { tab: closed.tab })
        .unwrap();
    for _ in 0..4 {
        shell.relayout();
    }
    assert!(windows_of(&handle, "3").is_empty());

    frame.relayout();
    crate::support::draw_chrome(&mut frame);
    frame.relayout();
    let undo = frame
        .a11y_node_named("Undo")
        .expect("the close offers Undo by name");
    frame
        .perform_a11y_action(undo, &ui::a11y::Action::Activate)
        .expect("a reader presses it");
    for _ in 0..4 {
        shell.relayout();
    }
    assert_eq!(windows_of(&handle, "3"), ["20"], "Undo reopens the window");
    assert!(
        frame.a11y_node_named("Undo").is_none(),
        "and the notice goes"
    );
}
