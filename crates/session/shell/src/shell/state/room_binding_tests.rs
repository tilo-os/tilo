//! The one record of rooms and workspaces, on a copy of a real state file:
//! the laptop's of 2026-10-20 (identity tokens and the room's replica
//! stripped), whose "Lounge" workspace was left on a local placeholder room
//! while its join into tilo.coffee's Lounge was in flight, and whose
//! previous Lounge (from before tilo.coffee's host was set up again) was held
//! with no workspace.

use super::super::{RoomId, WorkspaceId, WorkspaceTree};

const LAPTOP: &str = include_str!("../../../tests/fixtures/laptop-2026-10-20/state.json");
const HOSTED_ROOMS: &str =
    include_str!("../../../tests/fixtures/laptop-2026-10-20/hosted-rooms.json");

const LOUNGE: WorkspaceId = WorkspaceId(17_471_151_804_430_418_299);
const HOME: WorkspaceId = WorkspaceId(17_471_151_804_430_417_921);
/// The windows of the Lounge, in order.
const LOUNGE_TABS: [u64; 5] = [
    17_471_151_804_430_418_305,
    17_471_151_804_430_418_318,
    17_471_151_804_430_418_469,
    17_471_151_804_430_418_369,
    17_471_151_804_430_418_328,
];

fn room(text: &str) -> RoomId {
    RoomId::parse(text).expect("room id")
}

/// tilo.coffee's Lounge now: the room the site names (hosted-rooms.json)
/// and the join in flight installs into.
fn site_room() -> RoomId {
    room("302d59e0-f586-42e1-a59e-3aa613f0f866")
}

/// The local placeholder the Lounge workspace was left on.
fn placeholder() -> RoomId {
    room("4e50ef68-e572-4543-9dd5-16471fe3c712")
}

/// The Lounge from before tilo.coffee's host was set up again, held with no
/// workspace.
fn previous_lounge() -> RoomId {
    room("07848ffe-1898-4f72-8419-0b54c93e6dad")
}

fn laptop() -> WorkspaceTree {
    serde_json::from_str(LAPTOP).expect("the laptop's state file reads")
}

fn tabs_of(tree: &WorkspaceTree, workspace: WorkspaceId) -> Vec<u64> {
    tree.desktop
        .workspaces
        .iter()
        .chain(&tree.archived_workspaces)
        .find(|w| w.id == workspace)
        .expect("the workspace is there")
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| tab.id)
        .collect()
}

/// The workspace that is `room`, shown or archived.
fn workspace_of(tree: &WorkspaceTree, room: RoomId) -> Option<&super::super::Workspace> {
    tree.desktop
        .workspaces
        .iter()
        .chain(&tree.archived_workspaces)
        .find(|w| w.room() == room)
}

/// The rooms of the workspaces the switcher shows.
fn shown(tree: &WorkspaceTree) -> Vec<RoomId> {
    tree.desktop
        .workspaces
        .iter()
        .map(super::super::Workspace::room)
        .collect()
}

fn archived(tree: &WorkspaceTree) -> Vec<RoomId> {
    tree.archived_workspaces
        .iter()
        .map(super::super::Workspace::room)
        .collect()
}

#[test]
fn the_fixture_is_the_laptop_as_it_was() {
    let tree = laptop();
    assert_eq!(tree.room_of(LOUNGE), Some(placeholder()));
    assert_eq!(tabs_of(&tree, LOUNGE), LOUNGE_TABS);
    assert!(tree.native_authority.holds(previous_lounge()));
    assert!(workspace_of(&tree, previous_lounge()).is_none());
    assert_eq!(
        tree.native_authority.joins().collect::<Vec<_>>(),
        vec![(site_room(), Some(LOUNGE))]
    );
    let hosted: serde_json::Value = serde_json::from_str(HOSTED_ROOMS).unwrap();
    assert_eq!(
        hosted["rooms"][site_room().to_string()]["site"],
        "https://tilo.coffee/",
        "the site names the room the join installs into"
    );
    assert!(!tree.one_record_violations().is_empty());
}

/// The person's rule for every migration: never delete data, never change
/// ids. The Lounge ends on tilo.coffee's Lounge as the site names it now
/// (302d59e0, the room its join in flight installs into), with its windows;
/// the Lounge from before the site's host was set up again (07848ffe) keeps
/// what this computer holds of it, as a workspace of its own.
#[test]
fn the_laptops_lounge_becomes_the_sites_room_with_its_windows_and_its_id() {
    let mut tree = laptop();
    let ids_before: Vec<WorkspaceId> = tree.desktop.workspaces.iter().map(|w| w.id).collect();
    // As on the laptop: the placeholder's apps left their data on disk.
    let binding = tree.bind_workspaces_to_rooms(&|held| held == placeholder());

    assert_eq!(binding.joined, vec![(LOUNGE, placeholder(), site_room())]);
    assert_eq!(
        tree.room_of(LOUNGE),
        Some(site_room()),
        "the Lounge is the site's room"
    );
    assert_eq!(
        tabs_of(&tree, LOUNGE),
        LOUNGE_TABS,
        "its windows moved with it, ids kept"
    );
    assert_eq!(
        tree.desktop
            .workspaces
            .iter()
            .find(|w| w.id == LOUNGE)
            .unwrap()
            .name,
        "Lounge"
    );
    assert!(
        tree.is_joining(site_room()),
        "it says Joining… until the room installs"
    );
    assert!(tree.requires_native_owner(site_room()));
    assert!(!tree.holds_room(site_room()));
    for id in ids_before {
        assert!(
            tree.desktop.workspaces.iter().any(|w| w.id == id),
            "workspace {id:?} keeps its id"
        );
    }
    assert_eq!(
        tree.room_of(HOME),
        Some(room("5ba25f73-87c5-447f-a5a6-880dc4249a81"))
    );
    assert_eq!(tree.desktop.active, LOUNGE, "the Lounge stays on screen");

    // The placeholder holds files: kept, as an archived workspace of its own.
    let (kept_room, kept) = binding.kept[0];
    assert_eq!(kept_room, placeholder());
    assert!(
        tree.archived_workspaces
            .iter()
            .any(|w| w.id == kept && w.strips.iter().all(|s| s.window_groups.is_empty()))
    );
    assert!(binding.dropped.is_empty());

    // The previous Lounge, held with no workspace, has one, under the id its
    // record names (the room's own).
    let previous = workspace_of(&tree, previous_lounge()).expect("the held room has a workspace");
    assert_eq!(previous.id, WorkspaceId(16_546_964_187_606_345_987));
    assert_eq!(previous.id, WorkspaceId::of_room(previous_lounge()));
    assert_eq!(binding.made, vec![(previous_lounge(), previous.id)]);

    assert_eq!(tree.one_record_violations(), Vec::<String>::new());
}

#[test]
fn the_laptop_shows_exactly_one_lounge_and_keeps_the_other_two_archived() {
    let mut tree = laptop();
    let previous_record = tree
        .native_authority
        .held()
        .find(|(held, _)| *held == previous_lounge())
        .map(|(_, workspace)| workspace);
    tree.bind_workspaces_to_rooms(&|held| held == placeholder());

    let lounges: Vec<_> = tree
        .desktop
        .workspaces
        .iter()
        .filter(|w| w.name == "Lounge")
        .map(|w| (w.id, w.room()))
        .collect();
    assert_eq!(
        lounges,
        vec![(LOUNGE, site_room())],
        "one Lounge shown, tilo.coffee's"
    );
    assert_eq!(tabs_of(&tree, LOUNGE), LOUNGE_TABS, "with its windows");
    assert_eq!(
        shown(&tree),
        vec![room("5ba25f73-87c5-447f-a5a6-880dc4249a81"), site_room()]
    );
    assert_eq!(archived(&tree), vec![placeholder(), previous_lounge()]);
    let previous = workspace_of(&tree, previous_lounge()).unwrap();
    assert_eq!(
        Some(previous.id),
        previous_record,
        "under the id its record names"
    );
    assert!(
        tree.native_authority.holds(previous_lounge()),
        "its room intact"
    );
    assert!(workspace_of(&tree, placeholder()).is_some());
    assert!(
        tree.desktop
            .workspaces
            .iter()
            .all(|w| w.id != tree.archived_workspaces[0].id
                && w.id != tree.archived_workspaces[1].id),
        "archived workspaces are off the switcher and the desktop"
    );
    assert_eq!(tree.one_record_violations(), Vec::<String>::new());

    // Restore brings one back, on screen; it was never changed.
    let restored = tree.restore_archived(previous_lounge()).unwrap();
    assert_eq!(tree.desktop.active, restored);
    assert_eq!(Some(restored), previous_record);
    assert_eq!(archived(&tree), vec![placeholder()]);
    assert_eq!(tree.one_record_violations(), Vec::<String>::new());
}

#[test]
fn a_placeholder_that_holds_nothing_is_dropped_and_nothing_else_goes() {
    let mut tree = laptop();
    let binding = tree.bind_workspaces_to_rooms(&|_| false);
    assert_eq!(binding.dropped, vec![placeholder()]);
    assert!(binding.kept.is_empty());
    assert!(workspace_of(&tree, placeholder()).is_none());
    assert_eq!(tree.room_of(LOUNGE), Some(site_room()));
    assert_eq!(tabs_of(&tree, LOUNGE), LOUNGE_TABS);
    assert_eq!(
        shown(&tree),
        vec![room("5ba25f73-87c5-447f-a5a6-880dc4249a81"), site_room()]
    );
    assert_eq!(
        archived(&tree),
        vec![previous_lounge()],
        "the previous Lounge, archived"
    );
    assert_eq!(tree.one_record_violations(), Vec::<String>::new());
}

#[test]
fn binding_twice_changes_nothing_more() {
    let mut tree = laptop();
    tree.bind_workspaces_to_rooms(&|held| held == placeholder());
    let rooms: Vec<(WorkspaceId, RoomId)> = tree
        .desktop
        .workspaces
        .iter()
        .map(|w| (w.id, w.room()))
        .collect();
    let again = tree.bind_workspaces_to_rooms(&|held| held == placeholder());
    assert!(again.is_empty(), "{again:?}");
    let after: Vec<(WorkspaceId, RoomId)> = tree
        .desktop
        .workspaces
        .iter()
        .map(|w| (w.id, w.room()))
        .collect();
    assert_eq!(after, rooms);
}

#[test]
fn loading_the_laptops_file_binds_its_workspaces_to_their_rooms() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, LAPTOP).unwrap();
    let tree = crate::shell::persist::load_tree_from(&path).expect("the laptop's file loads");
    assert_eq!(tree.room_of(LOUNGE), Some(site_room()));
    assert_eq!(tabs_of(&tree, LOUNGE), LOUNGE_TABS);
    assert!(workspace_of(&tree, previous_lounge()).is_some());
    assert_eq!(tree.one_record_violations(), Vec::<String>::new());
}

/// No workspace without its room, no room without its workspace: through a
/// join, its installation's absence, and letting the room go.
#[test]
fn a_joined_workspace_and_its_room_never_part() {
    let mut tree = WorkspaceTree::default();
    let room = RoomId::new();
    let joined = tree
        .join_native_room("Lounge", room)
        .expect("a room is joined as a workspace of its own");
    assert_eq!(
        joined,
        WorkspaceId::of_room(room),
        "a new workspace has its room's id"
    );
    assert_eq!(tree.room_of(joined), Some(room));
    assert!(tree.is_joining(room));
    assert!(!tree.holds_room(room));
    assert_eq!(tree.one_record_violations(), Vec::<String>::new());
    assert_eq!(
        tree.join_native_room("Lounge", room),
        Some(joined),
        "a second join is the same workspace"
    );
    assert_eq!(
        tree.desktop
            .workspaces
            .iter()
            .filter(|w| w.room() == room)
            .count(),
        1
    );
    assert!(
        tree.delete_workspace(joined).is_none(),
        "a host's room goes only with its record, through the native owner"
    );
    tree.forget_personal_room(room);
    assert!(!tree.is_joining(room));
    assert_eq!(tree.one_record_violations(), Vec::<String>::new());
}

#[test]
fn an_id_a_joined_slot_carried_names_the_rooms_workspace() {
    let mut tree = WorkspaceTree::default();
    let room = RoomId::new();
    let workspace = tree.join_native_room("Lounge", room).unwrap();
    let old = WorkspaceId(42);
    tree.workspace_aliases.insert(old, room);
    assert_eq!(tree.resolve_workspace(old), Some(workspace));
    assert_eq!(tree.resolve_workspace(workspace), Some(workspace));
    assert_eq!(tree.resolve_workspace(WorkspaceId(43)), None);
}

/// Archiving is this device's presentation: the workspace leaves the
/// switcher whole, its room untouched, and comes back unchanged; joining
/// its room again brings it back rather than making a second one.
#[test]
fn an_archived_workspace_leaves_the_switcher_whole_and_comes_back() {
    let mut tree = WorkspaceTree::bootstrap_default();
    let room = RoomId::new();
    let joined = tree.join_room("Lounge", room).unwrap();
    let before = tree.desktop.workspaces.len();
    assert!(tree.archive_workspace(joined));
    assert_eq!(tree.desktop.workspaces.len(), before - 1);
    assert_ne!(
        tree.desktop.active, joined,
        "the desktop shows another workspace"
    );
    assert!(
        tree.has_workspace(room),
        "the room keeps its workspace, archived"
    );
    assert_eq!(tree.one_record_violations(), Vec::<String>::new());
    let last = tree.desktop.active;
    assert!(
        !tree.archive_workspace(last),
        "the last shown workspace stays"
    );

    assert_eq!(
        tree.join_room("Lounge", room),
        Some(joined),
        "the same workspace"
    );
    assert!(tree.archived_workspaces.is_empty());
    assert_eq!(tree.desktop.active, joined);

    assert!(tree.archive_workspace(joined));
    let workspace = tree.archived_workspaces.remove(0);
    assert!(
        tree.keep_archived(workspace),
        "a deletion undone archives it again"
    );
    assert_eq!(archived(&tree), vec![room]);
}
