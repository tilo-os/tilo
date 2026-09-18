use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use qos_desktop::{projection::RoomProjection, *};

fn strip(salt: u64, counter: u64, label: &str) -> Strip {
    let id = (salt << 48) | counter;
    Strip {
        id: StripId::from(id),
        name: label.into(),
        window_groups: vec![WindowGroup {
            id: id + 1,
            active_tab: id + 2,
            width: WindowWidth::Normal,
            maximized: false,
            tabs: vec![Tab {
                id: id + 2,
                title: label.into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other("dev.qos.notes".into()),
                    label: label.into(),
                },
            }],
        }],
    }
}
#[test]
fn admitted_projection_keeps_local_view_and_unpublished_content_but_removes_foreign() {
    let room = RoomId::new();
    let mut workspace = Workspace::new(WorkspaceId::from(100), "A");
    workspace.room = room;
    workspace.strips = vec![strip(2, 10, "Remote"), strip(1, 20, "Private")];
    workspace.strips[0].window_groups[0].width = WindowWidth::Fraction(70);
    workspace.strips[0].window_groups[0].maximized = true;
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    tree.active = WorkspaceId::from(100);
    let mut remote = strip(2, 10, "Changed");
    let tile = remote.window_groups[0].tabs[0].tile.clone();
    remote.window_groups[0].tabs.push(Tab {
        id: (2 << 48) | 13,
        title: "Second".into(),
        tile,
    });
    let mut doc = AutoCommit::new();
    RoomProjection::project_owned_contents(&mut doc, &[remote], 2, None);
    assert!(RoomProjection::merge_room_contents(
        &mut tree, room, &doc, 1
    ));
    assert_eq!(tree.workspaces[0].strips.len(), 2);
    let group = &tree.workspaces[0].strips[0].window_groups[0];
    assert_eq!(group.tabs.len(), 2);
    assert_eq!(group.active_tab, (2 << 48) | 12);
    assert_eq!(group.width, WindowWidth::Fraction(70));
    assert!(group.maximized);
    RoomProjection::project_owned_contents(&mut doc, &[], 2, None);
    RoomProjection::merge_room_contents(&mut tree, room, &doc, 1);
    assert_eq!(tree.workspaces[0].strips.len(), 1);
    assert_eq!(tree.workspaces[0].strips[0].name, "Private");
}
#[test]
fn projection_orders_ties_by_id_and_never_publishes_view_or_prejoin_ids() {
    let mut doc = AutoCommit::new();
    RoomProjection::project_owned_contents(
        &mut doc,
        &[strip(2, 20, "B"), strip(2, 10, "A"), strip(2, 1, "Old")],
        2,
        Some(10),
    );
    let (_, contents) = doc.get(ROOT, "contents").unwrap().unwrap();
    assert_eq!(doc.keys(&contents).count(), 2);
    for key in doc.keys(&contents).collect::<Vec<_>>() {
        let (_, entry) = doc.get(&contents, key).unwrap().unwrap();
        doc.put(entry, "order", 0_u64).unwrap();
    }
    let room = RoomId::new();
    let mut tree = WorkspaceTree::default();
    let mut ws = Workspace::new(WorkspaceId::from(1), "room");
    ws.room = room;
    tree.workspaces.push(ws);
    RoomProjection::merge_room_contents(&mut tree, room, &doc, 3);
    assert_eq!(
        tree.workspaces[0]
            .strips
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["A", "B"]
    );
    let json = serde_json::to_string(&automerge::AutoSerde::from(&doc)).unwrap();
    assert!(!json.contains("active_tab"));
    assert!(!json.contains("maximized"));
    assert!(!json.contains("width"));
}
