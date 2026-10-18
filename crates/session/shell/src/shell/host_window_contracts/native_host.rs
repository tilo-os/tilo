//! Publication through the same semantic rows consumed by the native host.
use super::*;
use crate::shell::state::{AppKind, Tab, WindowGroup, WindowWidth};

fn tree_with_tiles(tiles: Vec<TileContent>) -> WorkspaceTree {
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.set_salt_override(0);
    let workspace = &mut tree.desktop.workspaces[0];
    workspace.room = tilo_desktop_model::RoomId::new();
    workspace.strips[0].id = tilo_desktop_model::StripId(40);
    workspace.strips[0].window_groups.push(WindowGroup {
        id: 50,
        tabs: tiles
            .into_iter()
            .enumerate()
            .map(|(index, tile)| Tab {
                id: 100 + index as u64,
                title: "Window".into(),
                tile,
            })
            .collect(),
        active_tab: 100,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    tree
}

#[test]
fn native_host_publishes_local_window_record_and_shell_owned_outside_action() {
    let mut tree = tree_with_tiles(vec![
        TileContent::Foreign {
            app_id: "so.tilo.chat".into(),
            surface_id: 8,
        },
        TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        },
    ]);
    let workspace = &tree.desktop.workspaces[0];
    let room = workspace.room;
    let strip = workspace.strips[0].id;
    tree.sync_room_doc(room);
    tilo_desktop_model::apps::bind_tab_record(
        tree.room_docs.get_mut(&room).unwrap(),
        strip,
        50,
        100,
        "record:chat-conversation",
    )
    .unwrap();
    let contracts = published(&tree);
    assert_eq!(contracts.len(), 2);
    let contract = contracts
        .iter()
        .find(|contract| contract.tab == 100)
        .unwrap();
    let window = TreePath::room(room.to_string())
        .app("so.tilo.shell")
        .kind("host-window")
        .resource("100");
    assert_eq!(contract.window, window);
    assert_eq!(contract.request_model, window.action("requestModel"));
    assert_eq!(contract.kind, ActionKind::Outside);
    assert_eq!(contract.room, room.to_string());
    assert_eq!(contract.app, "so.tilo.chat");
    assert_eq!(contract.record.as_deref(), Some("record:chat-conversation"));
    assert!(
        contracts
            .iter()
            .find(|contract| contract.tab == 101)
            .unwrap()
            .record
            .is_none()
    );
    let rows = window_rows(&tree);
    let row = rows.iter().find(|row| row["tab"] == 100).unwrap();
    assert_eq!(
        row["host_contract"],
        serde_json::to_value(contract).unwrap()
    );
}

#[test]
fn native_host_does_not_publish_placeholder_or_remote_window_authority() {
    let tree = tree_with_tiles(vec![
        TileContent::AppPlaceholder {
            kind: AppKind::Terminal,
            label: "Terminal".into(),
        },
        TileContent::Stub {
            kind: AppKind::Browser,
            label: "Browser".into(),
        },
        TileContent::PeerWindow {
            app_id: "so.tilo.chat".into(),
            label: "Chat".into(),
            note: String::new(),
            missing: None,
        },
        TileContent::HostedWindow {
            app_id: "org.example.Editor".into(),
            label: "Editor".into(),
            host: "11".repeat(32),
            host_label: "Peer".into(),
        },
        TileContent::WorkspaceActivity,
    ]);
    assert!(published(&tree).is_empty());
    let rows = window_rows(&tree);
    assert_eq!(rows.len(), 5, "unpublished windows remain discoverable");
    for row in rows {
        assert!(
            row["host_contract"].is_null(),
            "remote/pending row acquired host authority: {row}"
        );
    }
}

#[test]
fn native_host_withdraws_contract_when_a_local_tab_becomes_remote() {
    let mut tree = tree_with_tiles(vec![TileContent::Foreign {
        app_id: "so.tilo.chat".into(),
        surface_id: 8,
    }]);
    assert_eq!(published(&tree).len(), 1);
    tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile = TileContent::PeerWindow {
        app_id: "so.tilo.chat".into(),
        label: "Chat".into(),
        note: String::new(),
        missing: None,
    };
    assert!(published(&tree).is_empty());
    assert!(window_rows(&tree)[0]["host_contract"].is_null());
}
