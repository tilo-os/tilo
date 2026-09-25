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

#[test]
fn explicitly_admitted_empty_strip_survives_normal_projection_but_default_empty_strip_stays_local()
{
    let mut doc = AutoCommit::new();
    let strip = Strip {
        id: StripId((2 << 48) | 100),
        name: "Explicit destination".into(),
        window_groups: vec![],
    };
    RoomProjection::project_owned_contents(&mut doc, std::slice::from_ref(&strip), 2, None);
    assert!(RoomProjection::contents_of(&doc).is_empty());
    RoomProjection::project_owned_command_contents(&mut doc, std::slice::from_ref(&strip), 2);
    assert_eq!(RoomProjection::contents_of(&doc).len(), 1);
    let mut restored = AutoCommit::load(&doc.save()).unwrap();
    RoomProjection::project_owned_contents(&mut restored, std::slice::from_ref(&strip), 2, None);
    let projected = RoomProjection::contents_of(&restored);
    assert_eq!(
        projected.len(),
        1,
        "a later ordinary native projection must retain admitted strip lifecycle"
    );
    assert_eq!(projected[0].id, strip.id.0);
    RoomProjection::project_owned_contents(&mut restored, &[], 2, None);
    assert!(
        RoomProjection::contents_of(&restored).is_empty(),
        "explicit removal still sweeps owned strip"
    );
}

/// Two peers whose room documents start empty (the legacy, unadmitted room
/// path) each project first: each minted its own root `contents` map, and a
/// merge showed only one peer's strips. Reads join every candidate, and a
/// later projection still sweeps a peer's removed strip wherever it lives.
#[test]
fn concurrent_first_projections_on_two_peers_both_survive() {
    for _ in 0..8 {
        let mut base = AutoCommit::new();
        base.commit();
        let mut a = base.fork();
        let mut b = base.fork();
        RoomProjection::project_owned_contents(&mut a, &[strip(1, 10, "A")], 1, None);
        RoomProjection::project_owned_contents(&mut b, &[strip(2, 10, "B")], 2, None);
        a.merge(&mut b).unwrap();
        let mut names: Vec<_> = RoomProjection::contents_of(&a)
            .into_iter()
            .filter_map(|s| s.name)
            .collect();
        names.sort();
        assert_eq!(names, ["A", "B"], "both peers' strips survive the merge");
        let tabs: usize = RoomProjection::contents_of(&a)
            .iter()
            .flat_map(|s| &s.groups)
            .map(|g| g.tabs.len())
            .sum();
        assert_eq!(tabs, 2);

        // Each peer removes its strip; neither leaves a ghost behind.
        RoomProjection::project_owned_contents(&mut a, &[], 1, None);
        let mut b2 = a.fork();
        RoomProjection::project_owned_contents(&mut b2, &[], 2, None);
        assert!(RoomProjection::contents_of(&b2).is_empty());
    }
}
