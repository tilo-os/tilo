//! Until the room has synced with its workspace's host, a tilolang app says so where it would
//! say the room is empty, exactly as a native app does: the platform's empty state follows what
//! the shell tells the window (`ui::room_sync`), and the app's own sentence shows only once the
//! room has synced.

use ui::framework::testing::HostedApp;
use ui::headless::InspectionNode;
use ui::room_sync::{RoomSync, RoomSyncState};

/// What the shell tells a window in the workspace "Lounge" about its room.
fn told(state: RoomSyncState) -> RoomSync {
    RoomSync {
        state,
        workspace: "Lounge".into(),
        writable: None,
    }
}

/// Everything the window says, as a screen reader hears it: each node's accessible name.
fn said(app: &mut HostedApp) -> Vec<String> {
    fn walk(node: &InspectionNode, out: &mut Vec<String>) {
        if let Some(name) = node
            .semantics
            .as_ref()
            .and_then(|access| access.name.clone())
        {
            out.push(name);
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    app.harness().relayout();
    let mut out = Vec::new();
    walk(&app.harness().inspection().root, &mut out);
    out
}

/// The window of the installed Notes package over a room with nothing in it yet.
fn empty_notebook() -> HostedApp {
    let mut app = HostedApp::open(crate::support::installed("notes"), "ada").unwrap();
    for _ in 0..12 {
        app.harness()
            .settle_for_capture(ui::time::Duration::from_millis(300));
        app.harness().relayout();
    }
    app
}

#[test]
fn an_unsynced_room_says_it_is_connecting_and_a_synced_empty_room_says_it_is_empty() {
    let mut app = empty_notebook();
    let connecting_to_lounge =
        |said: &[String]| said.iter().filter(|said| said.contains("Lounge")).count();

    ui::room_sync::set(Some(told(RoomSyncState::Connecting)));
    let connecting = said(&mut app);
    assert!(
        !connecting.iter().any(|said| said == "No notes yet"),
        "before the first sync nothing claims the notebook is empty: {connecting:?}"
    );
    assert!(
        connecting_to_lounge(&connecting) >= 2,
        "the list and the note's column each say the room is connecting: {connecting:?}"
    );

    ui::room_sync::set(Some(told(RoomSyncState::HostNeedsUpdate)));
    let behind = said(&mut app);
    assert!(
        !behind
            .iter()
            .any(|said| said == "No notes yet" || said.contains("Lounge")),
        "nor while the host has to update first, which says so instead: {behind:?}"
    );

    ui::room_sync::set(Some(told(RoomSyncState::Synced)));
    let synced = said(&mut app);
    assert!(
        synced.iter().any(|said| said == "No notes yet") && connecting_to_lounge(&synced) == 0,
        "once the room synced, its emptiness is the app's to say: {synced:?}"
    );

    ui::room_sync::set(None);
    assert!(
        said(&mut app).iter().any(|said| said == "No notes yet"),
        "and in a workspace that syncs with no host"
    );
}
