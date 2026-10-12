//! The device's desktop document records exactly what the tree holds, and
//! each narrow edit mirrors the matching tree change.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjId, PatchAction, Prop, ROOT, ReadDoc as _};
use collab_types::caps::{Access, CapOverride, Scope, Subject};
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::commands::{DesktopCommand, TabLocation, apply_command};
use tilo_desktop_model::desktop_doc;
use tilo_desktop_model::{
    DesktopTree, DropTarget, IdMinter, RoomId, Strip, StripId, Tab, TileContent, WindowGroup,
    WindowWidth, Workspace, WorkspaceId,
};

fn tab(id: u64) -> Tab {
    Tab {
        id,
        title: format!("tab {id}"),
        tile: TileContent::LocalApp {
            app_id: "org.example.App".into(),
            label: "App".into(),
            note: String::new(),
        },
    }
}

fn group(id: u64, tabs: &[u64]) -> WindowGroup {
    WindowGroup {
        id,
        tabs: tabs.iter().copied().map(tab).collect(),
        active_tab: tabs[0],
        width: WindowWidth::Wide,
        maximized: true,
    }
}

/// Workspace 1: strip 1 holds groups 10 [100, 101] and 11 [110]; strip 2
/// holds group 20 [200].
fn tree() -> DesktopTree {
    let mut ws = Workspace::new(WorkspaceId(1), "Desk");
    ws.set_room(RoomId::parse("00000000-0000-4000-8000-000000000042").unwrap());
    ws.set_owner("ada");
    let mut a = Strip::new(StripId(1), "A");
    a.window_groups = vec![group(10, &[100, 101]), group(11, &[110])];
    let mut b = Strip::new(StripId(2), "B");
    b.window_groups = vec![group(20, &[200])];
    ws.strips = vec![a, b];
    DesktopTree {
        workspaces: vec![ws],
        active: WorkspaceId(1),
        ..DesktopTree::default()
    }
}

/// `(strip, [(group, [tab])])` for the first workspace.
type Layout = Vec<(u64, Vec<(u64, Vec<u64>)>)>;

fn layout(tree: &DesktopTree) -> Layout {
    tree.workspaces[0]
        .strips
        .iter()
        .map(|s| {
            (
                s.id.raw(),
                s.window_groups
                    .iter()
                    .map(|g| (g.id, g.tabs.iter().map(|t| t.id).collect()))
                    .collect(),
            )
        })
        .collect()
}

/// What a fresh reader sees: the document folded into an empty tree.
fn read_back(doc: &AutoCommit) -> DesktopTree {
    let mut fresh = DesktopTree::default();
    desktop_doc::rebuild(doc, &mut fresh);
    fresh
}

fn seeded() -> (DesktopTree, AutoCommit) {
    let tree = tree();
    let mut doc = AutoCommit::new();
    desktop_doc::seed(&mut doc, &tree);
    (tree, doc)
}

#[test]
fn desktop_document_round_trips_the_desktop_and_governance() {
    // Phase `a_seeded_document_reads_back_as_the_same_desktop`.
    {
        let (tree, doc) = seeded();
        let back = read_back(&doc);
        assert_eq!(back.active, tree.active);
        assert_eq!(layout(&back), layout(&tree));
        let ws = &back.workspaces[0];
        assert_eq!(ws.room, tree.workspaces[0].room);
        assert_eq!(ws.owner().map(String::as_str), Some("ada"));
        assert_eq!(ws.sandbox, tree.workspaces[0].sandbox);
        let sorted = |policy: &tilo_action_policy::RoomPolicy| {
            let mut policy = policy.clone();
            policy.grants.sort_by(|a, b| a.id.cmp(&b.id));
            policy
        };
        assert_eq!(sorted(&ws.policy), sorted(&tree.workspaces[0].policy));
        // Presentation state is this device's and is not in the document.
        assert!(!ws.strips[0].window_groups[0].maximized);
        assert_eq!(ws.strips[0].window_groups[0].width, WindowWidth::Normal);
    }
    // Phase `rebuilding_keeps_local_presentation_for_known_groups`.
    {
        let (mut tree, doc) = seeded();
        desktop_doc::rebuild(&doc, &mut tree);
        assert!(tree.workspaces[0].strips[0].window_groups[0].maximized);
    }
    // Phase `governance_edits_round_trip`.
    {
        let (_, mut doc) = seeded();
        let cap = CapOverride::grant(
            Subject::app("org.example.App"),
            Scope::FsPath("/srv".into()),
            Access::Read,
        )
        .with_id("cap-x");
        assert!(desktop_doc::insert_sandbox(&mut doc, WorkspaceId(1), &cap));
        assert!(desktop_doc::set_owner(&mut doc, WorkspaceId(1), "bob"));
        let ws = &read_back(&doc).workspaces[0];
        assert!(ws.sandbox.iter().any(|c| c.id == "cap-x"));
        assert_eq!(ws.owner().map(String::as_str), Some("bob"));

        assert!(desktop_doc::revoke_sandbox(
            &mut doc,
            WorkspaceId(1),
            "cap-x"
        ));
        assert!(
            !read_back(&doc).workspaces[0]
                .sandbox
                .iter()
                .any(|c| c.id == "cap-x")
        );
        assert!(!desktop_doc::set_owner(&mut doc, WorkspaceId(9), "eve"));
    }
}

#[test]
fn topology_edits_are_mirrored_into_the_document() {
    // Phase `a_cross_strip_move_is_mirrored_exactly`.
    {
        let (mut tree, mut doc) = seeded();
        let target = DropTarget::ExistingGroup {
            group_id: 20,
            tab_index: 1,
        };
        let moved = tree
            .move_tab_across_strips(StripId(1), StripId(2), 110, 99, target)
            .unwrap();
        assert!(desktop_doc::apply_tab_move(
            &mut doc,
            StripId(1),
            StripId(2),
            &moved
        ));
        assert_eq!(layout(&read_back(&doc)), layout(&tree));
    }
    // Phase `a_split_is_mirrored_and_a_stale_move_leaves_the_document_alone`.
    {
        let (mut tree, mut doc) = seeded();
        let mut ids = IdMinter::resume(0, 500);
        let moved = tree
            .split_tab_into_new_group(StripId(1), 101, 2, &mut ids)
            .unwrap();
        assert!(desktop_doc::apply_tab_move(
            &mut doc,
            StripId(1),
            StripId(1),
            &moved
        ));
        assert_eq!(layout(&read_back(&doc)), layout(&tree));

        let before = layout(&read_back(&doc));
        let mut stale = moved;
        stale.source_group = 404;
        assert!(!desktop_doc::apply_tab_move(
            &mut doc,
            StripId(1),
            StripId(2),
            &stale
        ));
        assert_eq!(layout(&read_back(&doc)), before);
    }
    // Phase `reorders_removals_and_appends_follow_the_tree`.
    {
        let (mut tree, mut doc) = seeded();

        let apply = |tree: &mut DesktopTree, command: DesktopCommand| {
            apply_command(
                &mut tree.workspaces[0],
                &command,
                &mut || Err("a reorder mints nothing".into()),
                &mut |_, _| Err("a reorder creates nothing".into()),
            )
            .unwrap();
        };
        apply(
            &mut tree,
            DesktopCommand::ReorderGroup {
                strip: 1,
                group: 10,
                before: None,
            },
        );
        let moved = tree.workspaces[0].strips[0].window_groups[1].clone();
        assert!(desktop_doc::reorder_group(&mut doc, StripId(1), &moved, 1));
        apply(
            &mut tree,
            DesktopCommand::ReorderTab {
                location: TabLocation {
                    strip: 1,
                    group: 10,
                    tab: 100,
                },
                before: None,
            },
        );
        let tabs = tree.workspaces[0].strips[0].window_groups[1].tabs.clone();
        assert!(desktop_doc::rewrite_group_tabs(&mut doc, 10, &tabs));
        assert_eq!(layout(&read_back(&doc)), layout(&tree));

        assert!(desktop_doc::remove_tab(
            &mut doc,
            StripId(1),
            10,
            101,
            false
        ));
        assert!(desktop_doc::remove_tab(&mut doc, StripId(2), 20, 200, true));
        assert!(desktop_doc::append_tab(
            &mut doc,
            WorkspaceId(1),
            None,
            300,
            "new".into(),
            Vec::new(),
        ));
        // A fresh reader learns strips through their groups, so the emptied
        // strip 2 does not appear.
        let back = layout(&read_back(&doc));
        assert_eq!(back, vec![(1, vec![(11, vec![110, 300]), (10, vec![100])])]);
    }
}

#[test]
fn a_group_moved_to_another_strip_follows_the_tree() {
    {
        let (mut tree, mut doc) = seeded();
        apply_command(
            &mut tree.workspaces[0],
            &DesktopCommand::MoveGroup {
                group: 10,
                strip: 2,
                before: Some(20),
            },
            &mut || Err("a move mints nothing".into()),
            &mut |_, _| Err("a move creates nothing".into()),
        )
        .unwrap();
        assert_eq!(
            layout(&tree),
            vec![
                (1, vec![(11, vec![110])]),
                (2, vec![(10, vec![100, 101]), (20, vec![200])]),
            ]
        );
        let moved = tree.workspaces[0].strips[1].window_groups[0].clone();
        assert!(desktop_doc::move_group(&mut doc, StripId(2), &moved, 0));
        assert_eq!(layout(&read_back(&doc)), layout(&tree));

        // Nothing moves when the destination strip is not in the document.
        let before = layout(&read_back(&doc));
        assert!(!desktop_doc::move_group(&mut doc, StripId(9), &moved, 0));
        assert_eq!(layout(&read_back(&doc)), before);
    }
}

fn shortcut(id: &str, name: &str) -> tilo_automation::Shortcut {
    serde_json::from_value(serde_json::json!({ "id": id, "name": name, "steps": [] }))
        .expect("a shortcut with no steps")
}

#[test]
fn shortcuts_are_seeded_saved_replaced_and_deleted() {
    let mut tree = tree();
    tree.workspaces[0].shortcuts = vec![shortcut("morning", "Morning")];
    let mut doc = AutoCommit::new();
    desktop_doc::seed(&mut doc, &tree);
    assert_eq!(
        read_back(&doc).workspaces[0].shortcuts,
        vec![shortcut("morning", "Morning")]
    );

    assert!(desktop_doc::upsert_shortcut(
        &mut doc,
        WorkspaceId(1),
        &shortcut("evening", "Evening")
    ));
    assert!(desktop_doc::upsert_shortcut(
        &mut doc,
        WorkspaceId(1),
        &shortcut("morning", "Wake up")
    ));
    assert_eq!(
        read_back(&doc).workspaces[0].shortcuts,
        vec![
            shortcut("evening", "Evening"),
            shortcut("morning", "Wake up")
        ]
    );

    assert!(desktop_doc::remove_shortcut(
        &mut doc,
        WorkspaceId(1),
        "morning"
    ));
    assert!(!desktop_doc::remove_shortcut(
        &mut doc,
        WorkspaceId(1),
        "morning"
    ));
    assert_eq!(
        read_back(&doc).workspaces[0].shortcuts,
        vec![shortcut("evening", "Evening")]
    );
    assert!(!desktop_doc::upsert_shortcut(
        &mut doc,
        WorkspaceId(9),
        &shortcut("x", "X")
    ));
}

/// [`tree`] plus workspace 2 ("Lab"): strip 3 holds group 30 [300].
fn two_workspaces() -> DesktopTree {
    let mut desktop = tree();
    let mut lab = Workspace::new(WorkspaceId(2), "Lab");
    lab.set_room(RoomId::parse("00000000-0000-4000-8000-000000000043").unwrap());
    let mut strip = Strip::new(StripId(3), "C");
    strip.window_groups = vec![group(30, &[300])];
    lab.strips = vec![strip];
    desktop.workspaces.push(lab);
    desktop
}

fn committed_seed(desktop: &DesktopTree) -> AutoCommit {
    let mut doc = AutoCommit::new();
    desktop_doc::seed(&mut doc, desktop);
    doc.commit();
    doc
}

fn child(doc: &AutoCommit, parent: &ObjId, prop: impl Into<Prop>) -> ObjId {
    doc.get(parent, prop).unwrap().unwrap().1
}

fn workspace_list(doc: &AutoCommit) -> ObjId {
    child(doc, &ROOT, "workspaces")
}

/// The object of each workspace entry, in document order.
fn workspace_entries(doc: &AutoCommit) -> Vec<ObjId> {
    let list = workspace_list(doc);
    (0..doc.length(&list))
        .map(|i| child(doc, &list, i))
        .collect()
}

/// The object of the tab entry at `path` = (workspace, strip, group, tab)
/// positions.
fn tab_entry(doc: &AutoCommit, path: [usize; 4]) -> ObjId {
    let [ws, strip, group, tab] = path;
    let ws = child(doc, &workspace_list(doc), ws);
    let strip = child(doc, &child(doc, &ws, "strips"), strip);
    let group = child(doc, &child(doc, &strip, "window_groups"), group);
    child(doc, &child(doc, &group, "tabs"), tab)
}

fn names(desktop: &DesktopTree) -> Vec<(u64, String)> {
    desktop
        .workspaces
        .iter()
        .map(|ws| (ws.id.raw(), ws.name.clone()))
        .collect()
}

#[test]
fn reseeding_an_unchanged_desktop_writes_nothing() {
    let desktop = two_workspaces();
    let mut doc = committed_seed(&desktop);
    let heads = doc.get_heads();
    desktop_doc::seed(&mut doc, &desktop);
    assert_eq!(
        doc.pending_ops(),
        0,
        "every write is a history row; an unchanged tree must not produce any"
    );
    assert_eq!(doc.commit(), None);
    assert_eq!(doc.get_heads(), heads);
}

/// `value` as JSON with every object's keys in reverse order.
fn reversed_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let fields: Vec<String> = map
                .iter()
                .rev()
                .map(|(k, v)| format!("{}:{}", serde_json::to_string(k).unwrap(), reversed_json(v)))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        other => other.to_string(),
    }
}

/// A blob is compared as JSON: a map read back from a peer's bytes may
/// serialise its keys in another order, and that is not an edit.
#[test]
fn reseeding_does_not_rewrite_an_equivalent_blob() {
    let desktop = tree();
    let mut doc = committed_seed(&desktop);
    let tile = serde_json::to_value(&desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile)
        .unwrap();
    let reordered = reversed_json(&tile).into_bytes();
    assert_ne!(reordered, serde_json::to_vec(&tile).unwrap());
    let entry = tab_entry(&doc, [0, 0, 0, 0]);
    doc.put(
        &entry,
        "tile_json",
        automerge::ScalarValue::Bytes(reordered),
    )
    .unwrap();
    doc.commit();

    desktop_doc::seed(&mut doc, &desktop);
    assert_eq!(doc.pending_ops(), 0);
}

#[test]
fn a_rename_writes_only_the_name() {
    let mut desktop = two_workspaces();
    let mut doc = committed_seed(&desktop);
    let list = workspace_list(&doc);
    let entries = workspace_entries(&doc);
    let tab = tab_entry(&doc, [1, 0, 0, 0]);
    let before = doc.get_heads();

    desktop.workspaces[0].name = "Studio".into();
    desktop_doc::seed(&mut doc, &desktop);
    assert_eq!(
        doc.pending_ops(),
        1,
        "a rename is one put, not a list rewrite"
    );
    doc.commit();
    let after = doc.get_heads();
    let patches = doc.diff(&before, &after);
    assert_eq!(patches.len(), 1, "{patches:?}");
    assert_eq!(patches[0].obj, entries[0]);
    assert!(
        matches!(&patches[0].action, PatchAction::PutMap { key, .. } if key == "name"),
        "{patches:?}"
    );
    assert_eq!(workspace_list(&doc), list, "the list object survives");
    assert_eq!(
        workspace_entries(&doc),
        entries,
        "every entry keeps its object"
    );
    assert_eq!(tab_entry(&doc, [1, 0, 0, 0]), tab);
    assert_eq!(
        names(&read_back(&doc)),
        vec![(1, "Studio".into()), (2, "Lab".into())]
    );
}

#[test]
fn seed_splices_inserted_removed_and_reordered_entries() {
    let mut desktop = two_workspaces();
    let mut doc = committed_seed(&desktop);
    let entries = workspace_entries(&doc);

    // Close tab 101, open 102 in group 11, add group 21 to strip 2, add a
    // third workspace, and put workspace 2 first.
    let ws = &mut desktop.workspaces[0];
    ws.strips[0].window_groups[0].tabs.truncate(1);
    ws.strips[0].window_groups[1].tabs.push(tab(111));
    ws.strips[1].window_groups.push(group(21, &[210, 211]));
    desktop
        .workspaces
        .push(Workspace::new(WorkspaceId(3), "New"));
    desktop.workspaces.swap(0, 1);
    desktop_doc::seed(&mut doc, &desktop);
    doc.commit();

    let back = read_back(&doc);
    assert_eq!(
        names(&back),
        vec![(2, "Lab".into()), (1, "Desk".into()), (3, "New".into())]
    );
    let desk = back
        .workspaces
        .iter()
        .find(|w| w.id == WorkspaceId(1))
        .unwrap();
    let desk_layout: Layout = desk
        .strips
        .iter()
        .map(|s| {
            (
                s.id.raw(),
                s.window_groups
                    .iter()
                    .map(|g| (g.id, g.tabs.iter().map(|t| t.id).collect()))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        desk_layout,
        vec![
            (1, vec![(10, vec![100]), (11, vec![110, 111])]),
            (2, vec![(20, vec![200]), (21, vec![210, 211])]),
        ]
    );
    let now = workspace_entries(&doc);
    assert_ne!(
        now[0], entries[1],
        "a list has no move; the entry moved forward is re-inserted"
    );
    assert_eq!(
        now[1], entries[0],
        "the entry it jumped over keeps its object"
    );
    assert_eq!(now.len(), 3);

    // Without a reorder, a splice leaves every surviving object in place.
    let mut desktop = two_workspaces();
    let mut doc = committed_seed(&desktop);
    let desk = workspace_entries(&doc)[0].clone();
    let untouched_tab = tab_entry(&doc, [0, 0, 0, 0]);
    let neighbour_tab = tab_entry(&doc, [0, 0, 1, 0]);
    desktop.workspaces[0].strips[0].window_groups[1]
        .tabs
        .push(tab(111));
    desktop.workspaces.remove(1);
    desktop_doc::seed(&mut doc, &desktop);
    assert_eq!(workspace_entries(&doc), vec![desk]);
    assert_eq!(tab_entry(&doc, [0, 0, 0, 0]), untouched_tab);
    assert_eq!(tab_entry(&doc, [0, 0, 1, 0]), neighbour_tab);
    assert_eq!(names(&read_back(&doc)), vec![(1, "Desk".into())]);
}

#[test]
fn seed_deletes_fields_and_map_keys_the_tree_dropped() {
    let mut desktop = tree();
    let keep = CapOverride::grant(
        Subject::app("org.example.App"),
        Scope::FsPath("/srv".into()),
        Access::Read,
    )
    .with_id("cap-keep");
    let drop = CapOverride::grant(
        Subject::app("org.example.App"),
        Scope::FsPath("/tmp".into()),
        Access::Read,
    )
    .with_id("cap-drop");
    desktop.workspaces[0].sandbox = vec![keep.clone(), drop];
    let mut doc = committed_seed(&desktop);
    let entry = workspace_entries(&doc)[0].clone();
    let caps = child(&doc, &entry, "sandbox");

    desktop.workspaces[0].sandbox = vec![keep.clone()];
    desktop.workspaces[0].policy.owner = None;
    desktop_doc::seed(&mut doc, &desktop);
    doc.commit();

    assert_eq!(
        child(&doc, &entry, "sandbox"),
        caps,
        "the map is edited, not replaced"
    );
    assert_eq!(
        doc.keys(&caps).collect::<Vec<_>>(),
        vec!["cap-keep".to_string()]
    );
    assert!(doc.get(&entry, "owner").unwrap().is_none());
    let back = read_back(&doc);
    assert_eq!(back.workspaces[0].sandbox, vec![keep]);
    assert_eq!(back.workspaces[0].owner(), None);
}

/// The reason seeding is a splice: a whole-list rewrite deletes the entry a
/// peer edited concurrently, so the peer's edit lands on a tombstone.
#[test]
fn a_concurrent_edit_to_another_workspace_survives_a_reseed() {
    let mut local = two_workspaces();
    let mut doc = committed_seed(&local);
    let mut peer_doc = doc.fork();
    let mut peer = two_workspaces();

    peer.workspaces[1].name = "Peer lab".into();
    desktop_doc::seed(&mut peer_doc, &peer);
    peer_doc.commit();
    local.workspaces[0].name = "Studio".into();
    desktop_doc::seed(&mut doc, &local);
    doc.commit();

    doc.merge_incoming(&mut peer_doc).unwrap();
    assert_eq!(
        names(&read_back(&doc)),
        vec![(1, "Studio".into()), (2, "Peer lab".into())]
    );
}
