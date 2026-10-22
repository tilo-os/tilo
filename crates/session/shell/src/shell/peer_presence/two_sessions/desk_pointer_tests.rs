//! A live member's pointer over the empty desktop, on a desk laid out
//! otherwise than theirs.
use super::*;
use tilo_relay_protocol::live::{
    LiveFrame, LivePointer, LiveRecord, PointerLayout, PresenceFrame, ShownStrip,
};

/// Two layouts of one room: a browser tab on a wide screen, where the
/// note's window is 800 px wide at the corner, and this desktop, where the
/// same note's window is 600 px wide further in. A pointer right of the
/// note's window on the tab lands as far right of it here; one on the tab's
/// strip, with no window shown there, lands on the same strip here, panned
/// as it is here; one beside a window this desk does not show, or over a
/// window (that window's app draws it), is not drawn by the desktop. This
/// desk never points anywhere and runs as a window-hosted shell: which
/// record its window shows comes from the window instance its app was
/// launched for, and the strip from the desk it shares.
#[test]
fn a_pointer_over_the_empty_desktop_lands_beside_the_same_window_on_another_layout() {
    let mut pair = pair("desk-pointer");
    let room = pair.room.to_string();
    let note = LiveRecord {
        app: "so.tilo.notes".into(),
        record: "note-1".into(),
    };
    // This desk: the note's window, its app launched for window instance
    // 77, which holds the note's record. Nobody here has pointed in it.
    let tab = 0x51;
    let tiles = vec![(tab, vello::kurbo::Rect::new(100.0, 50.0, 700.0, 650.0))];
    let tree = pair.host.roster_tree.clone();
    a_window_of(&tree, pair.room, tab, &note.app);
    let held = tilo_action_broker::route::CrdtRoute::record(&note.app, &room, &note.record);
    pair.host.window_instances = Box::new(move || vec![(77, held.clone())]);
    assert_eq!(pair.host.window_record(tab), None, "never pointed in");
    let desk = Desk {
        tiles,
        pointer: None,
        strip: Some(ShownStrip {
            id: 9,
            area: [0.0, 40.0, 1200.0, 800.0],
            scroll_x: 100.0,
        }),
    };
    let key = PresenceKey::Workspace { id: room };
    let ada = pair.guest_user();
    let mut seq = 0;
    let mut say = |pointer: Option<LivePointer>| {
        seq += 1;
        let frame = LiveFrame::Presence(PresenceFrame {
            seq,
            pointer,
            ..PresenceFrame::default()
        });
        pair.host_net.take_live_frame(
            pair.room,
            pair.guest_key(),
            &frame.encode().expect("a frame"),
        );
    };
    let mut on_host = PresenceCtx::default();

    // The tab's layout says where its pointer is.
    let tab_desk = PointerLayout {
        windows: vec![(note, [0.0, 0.0, 800.0, 600.0])],
        strip: Some(ShownStrip {
            id: 4,
            area: [0.0, 0.0, 1440.0, 900.0],
            scroll_x: 0.0,
        }),
    };
    let beside = tab_desk.anchor([900.0, 300.0]).expect("beside the note");
    assert!(matches!(beside, LivePointer::Beside { .. }));
    say(Some(beside));
    assert!(pair.host.frame(&mut on_host, &desk, &tree));
    assert_eq!(
        on_host.pointer_for(&key, &ada),
        Some((800.0, 350.0)),
        "100 px right of the note's window, halfway down it"
    );

    // Over the window, it is the note's app's to draw.
    say(tab_desk.anchor([400.0, 300.0]));
    pair.host.frame(&mut on_host, &desk, &tree);
    assert_eq!(on_host.pointer_for(&key, &ada), None);

    // On a tab showing no window, on the strip this desk shows too.
    let bare = PointerLayout {
        windows: Vec::new(),
        strip: Some(ShownStrip {
            id: 9,
            area: [0.0, 0.0, 1024.0, 700.0],
            scroll_x: 0.0,
        }),
    };
    let on_strip = bare.anchor([300.0, 100.0]).expect("on the strip");
    assert!(matches!(on_strip, LivePointer::Strip { strip: 9, .. }));
    say(Some(on_strip));
    assert!(pair.host.frame(&mut on_host, &desk, &tree));
    assert_eq!(
        on_host.pointer_for(&key, &ada),
        Some((200.0, 140.0)),
        "at the same place along the strip, as panned here"
    );

    // Beside a window of a record this desk does not show: nowhere here.
    say(Some(LivePointer::Beside {
        record: LiveRecord {
            app: "so.tilo.sheets".into(),
            record: "budget".into(),
        },
        edge: [1.0, 0.5],
        at: [40.0, 0.0],
    }));
    pair.host.frame(&mut on_host, &desk, &tree);
    assert_eq!(on_host.pointer_for(&key, &ada), None);
    let _ = std::fs::remove_dir_all(&pair.dir);
}

/// Show `app` in tab `tab` of `room`'s workspace in `tree`, launched for
/// window instance 77.
fn a_window_of(tree: &crate::shell::state::WorkspaceTreeHandle, room: RoomId, tab: u64, app: &str) {
    {
        use crate::shell::state::FreshId as _;
        let mut tree = tree.write();
        let workspace = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room() == room)
            .expect("the room's workspace");
        workspace.strips.push(crate::shell::state::Strip {
            id: crate::shell::state::StripId::new(),
            name: "Notes".into(),
            window_groups: vec![crate::shell::state::WindowGroup {
                id: 4242,
                tabs: vec![crate::shell::state::Tab {
                    id: tab,
                    title: "Note".into(),
                    tile: crate::shell::state::TileContent::LocalApp {
                        app_id: app.to_owned(),
                        label: "Notes".into(),
                        note: String::new(),
                    },
                }],
                active_tab: tab,
                width: crate::shell::state::WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            }],
        });
        tree.remember_local_app_record(tab, 77);
    }
}
