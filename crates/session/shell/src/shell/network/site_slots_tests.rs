//! The empty slots repeated joins of one site left go, and nothing that
//! holds anything does: a computer that joined its community every ten
//! minutes holds 26 "Lounge" workspaces, the first binding the saved proof.

use super::*;
use crate::shell::state::{
    Collaborator, FreshId as _, StripId, WindowGroup, WindowWidth, WorkspaceTree, next_id,
};

/// This computer's person.
const PERSON: &str = "61fd5d11de98fe19ddba9ae2149dd14abb8c3c8c72473bb787b0f1552f751241";
/// Someone else.
const OTHER: &str = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";
const SITE_NAME: &str = "Lounge";

/// An empty workspace named `name`, as a join's slot is made, owned by
/// this computer's person (the owner a loaded workspace adopts).
fn slot(tree: &mut WorkspaceTree, name: &str) -> WorkspaceId {
    let id = tree.append_empty_workspace(name);
    workspace_mut(tree, id).policy.owner = Some(PERSON.into());
    id
}

fn workspace_mut(tree: &mut WorkspaceTree, id: WorkspaceId) -> &mut Workspace {
    tree.desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == id)
        .expect("workspace")
}

fn room_of(tree: &WorkspaceTree, id: WorkspaceId) -> RoomId {
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id)
        .expect("workspace")
        .room
}

fn ids(tree: &WorkspaceTree) -> Vec<WorkspaceId> {
    tree.desktop
        .workspaces
        .iter()
        .map(|workspace| workspace.id)
        .collect()
}

/// The laptop's desktop: Home with a window, the slot the saved proof
/// binds, 25 empty slots after it, and beside them workspaces named for
/// the site that each hold one thing, plus an empty one named otherwise.
struct Desktop {
    tree: WorkspaceTree,
    site_room: RoomId,
    home: WorkspaceId,
    keeper: WorkspaceId,
    duplicates: Vec<WorkspaceId>,
    /// Named for the site, each holding something: never taken off.
    held: Vec<WorkspaceId>,
    /// Empty, named otherwise: never taken off.
    unrelated: WorkspaceId,
}

fn window() -> WindowGroup {
    WindowGroup {
        id: next_id(),
        tabs: Vec::new(),
        active_tab: 0,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

fn desktop() -> Desktop {
    let mut tree = WorkspaceTree::default();
    let home = slot(&mut tree, "Home");
    workspace_mut(&mut tree, home).strips[0]
        .window_groups
        .push(window());
    let keeper = slot(&mut tree, SITE_NAME);
    let duplicates: Vec<WorkspaceId> = (0..25).map(|_| slot(&mut tree, SITE_NAME)).collect();

    let mut held = Vec::new();
    // A window.
    let with_window = slot(&mut tree, SITE_NAME);
    workspace_mut(&mut tree, with_window).strips[0]
        .window_groups
        .push(window());
    held.push(with_window);
    // A member.
    let with_member = slot(&mut tree, SITE_NAME);
    workspace_mut(&mut tree, with_member)
        .policy
        .members
        .insert(OTHER.into(), tilo_action_policy::Role::Editor);
    held.push(with_member);
    // Someone present.
    let with_presence = slot(&mut tree, SITE_NAME);
    workspace_mut(&mut tree, with_presence)
        .collaborators
        .push(Collaborator::new("bob", "Bob"));
    held.push(with_presence);
    // A second strip.
    let with_strip = slot(&mut tree, SITE_NAME);
    workspace_mut(&mut tree, with_strip)
        .strips
        .push(crate::shell::state::Strip::new(StripId::new(), "Ideas"));
    held.push(with_strip);
    // Its strip renamed.
    let renamed_strip = slot(&mut tree, SITE_NAME);
    workspace_mut(&mut tree, renamed_strip).strips[0].name = "Plans".into();
    held.push(renamed_strip);
    // Someone else's.
    let theirs = slot(&mut tree, SITE_NAME);
    workspace_mut(&mut tree, theirs).policy.owner = Some(OTHER.into());
    held.push(theirs);
    // A grant to a person.
    let with_grant = slot(&mut tree, SITE_NAME);
    {
        let workspace = workspace_mut(&mut tree, with_grant);
        let mut grant = workspace.policy.grants[0].clone();
        grant.subject = tilo_action_policy::Subject::Person(OTHER.into());
        workspace.policy.grants.push(grant);
    }
    held.push(with_grant);
    // A record held in its room.
    let with_record = slot(&mut tree, SITE_NAME);
    let record_room = room_of(&tree, with_record);
    tree.desktop
        .room_records_held
        .entry(record_room)
        .or_default()
        .insert("note:1".into(), "so.tilo.notes/note".into());
    held.push(with_record);
    // A room another device governs.
    let governed = slot(&mut tree, SITE_NAME);
    let governed_room = room_of(&tree, governed);
    tree.room_authority
        .insert(governed_room, crate::shell::state::RoomAuthority::Unpinned);
    held.push(governed);

    let unrelated = slot(&mut tree, "Research");
    Desktop {
        tree,
        site_room: RoomId::new(),
        home,
        keeper,
        duplicates,
        held,
        unrelated,
    }
}

impl Desktop {
    fn anchor(&self) -> SiteAnchor {
        SiteAnchor {
            site_room: self.site_room,
            slot: Some(self.keeper),
        }
    }
}

#[test]
fn the_empty_slots_beside_the_saved_proof_go_and_everything_holding_anything_stays() {
    let mut desktop = desktop();
    let anchors = [desktop.anchor()];
    let before = ids(&desktop.tree);
    let duplicate_rooms: Vec<RoomId> = desktop
        .duplicates
        .iter()
        .map(|id| room_of(&desktop.tree, *id))
        .collect();

    let found: Vec<WorkspaceId> = duplicate_slots(&desktop.tree, &anchors, PERSON)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        found, desktop.duplicates,
        "exactly the 25 empty slots after the proof's"
    );

    let retired = retire_duplicate_slots(&mut desktop.tree, &anchors, PERSON, |_, _| true);
    assert_eq!(retired, duplicate_rooms);

    let kept = ids(&desktop.tree);
    let mut expected = vec![desktop.home, desktop.keeper];
    expected.extend(&desktop.held);
    expected.push(desktop.unrelated);
    assert_eq!(kept, expected, "the rest stay, in their order");
    assert!(
        before.iter().filter(|id| kept.contains(id)).eq(kept.iter()),
        "no workspace that stays changed its id"
    );
    for room in &duplicate_rooms {
        assert!(!desktop.tree.rooms().contains(room));
    }

    assert!(
        duplicate_slots(&desktop.tree, &anchors, PERSON).is_empty(),
        "a second pass finds nothing"
    );
}

#[test]
fn a_slot_a_proof_binds_or_whose_room_holds_files_stays() {
    let mut desktop = desktop();
    let anchors = [desktop.anchor()];
    let spared = [desktop.duplicates[3], desktop.duplicates[17]];
    let retired = retire_duplicate_slots(&mut desktop.tree, &anchors, PERSON, |id, _| {
        !spared.contains(&id)
    });
    assert_eq!(retired.len(), 23);
    let kept = ids(&desktop.tree);
    assert!(spared.iter().all(|id| kept.contains(id)));
    assert!(kept.contains(&desktop.keeper));
}

#[test]
fn without_a_keeper_nothing_goes() {
    let mut desktop = desktop();
    let before = ids(&desktop.tree);
    // The proof names a slot that is gone, and no workspace holds the room.
    let anchors = [SiteAnchor {
        site_room: desktop.site_room,
        slot: Some(WorkspaceId::new()),
    }];
    assert!(duplicate_slots(&desktop.tree, &anchors, PERSON).is_empty());
    assert!(retire_duplicate_slots(&mut desktop.tree, &[], PERSON, |_, _| true).is_empty());
    assert_eq!(ids(&desktop.tree), before);
}

#[test]
fn the_slot_is_measured_against_this_computers_person() {
    let desktop = desktop();
    let anchors = [desktop.anchor()];
    assert_eq!(
        duplicate_slots(&desktop.tree, &anchors, OTHER),
        vec![(desktop.held[5], room_of(&desktop.tree, desktop.held[5]))],
        "measured against another person, only that person's empty slot matches"
    );
    assert!(duplicate_slots(&desktop.tree, &anchors, "someone").is_empty());
}

#[test]
fn once_the_join_installed_its_workspace_keeps_and_the_empty_slots_go() {
    let mut desktop = desktop();
    // The proof was redeemed: its slot now shows the site's room.
    let site_room = desktop.site_room;
    workspace_mut(&mut desktop.tree, desktop.keeper).set_room(site_room);
    let anchors = [SiteAnchor {
        site_room,
        slot: None,
    }];
    let found: Vec<WorkspaceId> = duplicate_slots(&desktop.tree, &anchors, PERSON)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(found, desktop.duplicates);
    retire_duplicate_slots(&mut desktop.tree, &anchors, PERSON, |_, _| true);
    assert!(desktop.tree.rooms().contains(&site_room));
}

#[test]
fn a_room_holds_files_when_an_app_or_an_index_kept_any() {
    let home = tempfile::tempdir().expect("temp");
    let empty = RoomId::new();
    let apps = RoomId::new();
    let index = RoomId::new();
    std::fs::create_dir_all(home.path().join("tilo-rooms").join(apps.to_string())).unwrap();
    std::fs::create_dir_all(
        home.path()
            .join("so.tilo.shell")
            .join("rooms")
            .join(index.to_string()),
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("so.tilo.notes").join("rooms")).unwrap();
    assert!(!room_holds_files(home.path(), empty));
    assert!(room_holds_files(home.path(), apps));
    assert!(room_holds_files(home.path(), index));
    assert!(
        room_holds_files(&home.path().join("missing"), empty),
        "a data home that cannot be read keeps everything"
    );
}
