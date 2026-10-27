//! Personal Settings lives in the person's personal workspace. Opened while
//! a shared workspace is shown, its window still lands in the personal one,
//! never on the shared workspace's strip, so the collaborators in that room
//! never receive it as a peer window. A Settings window can't be moved onto
//! a shared strip either.
//!
//! Two trees stand for two machines in one room: the device whose person
//! made it, which also has a workspace only its person is in, and a
//! collaborator admitted to it. What crosses between them is the room's
//! layout record as the first device writes it, as its record route carries
//! it.

use shell::shell::state::{RoomId, TabId, TileContent, WorkspaceId, WorkspaceTree};
use shell::shell::tab_binding;

const SETTINGS: &str = "so.tilo.settings";
const GUEST: &str = "bob";
/// The person's own machine and the collaborator's mint ids in ranges of
/// their own, so the person's windows are a peer's to the collaborator.
const HOST_SALT: u16 = 0xA11C;
const GUEST_SALT: u16 = 0xBEEF;
/// The key of the person's own machine, which signs its layout record.
const HOST_DEVICE: [u8; 32] = [0xA1; 32];

struct Room {
    host: WorkspaceTree,
    guest: WorkspaceTree,
    personal: WorkspaceId,
    shared: WorkspaceId,
    id: RoomId,
}

/// The host has its personal workspace and a shared one the guest was
/// admitted to; the shared one is shown.
fn shared_room() -> Room {
    let mut host = WorkspaceTree::default();
    host.set_peer_salt(HOST_SALT);
    let (personal, _) = host.create_hosted_workspace("Mine");
    let (shared, _) = host.create_hosted_workspace("Together");
    host.own_workspaces_for_test(shell::shell::state::local_user_id_for_room);
    let room = host
        .room_of(shared)
        .expect("the shared workspace is a room");
    host.desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == shared)
        .expect("the shared workspace")
        .admit(GUEST, tilo_action_policy::Role::Editor);
    host.set_active(shared);

    let mut guest = WorkspaceTree::default();
    guest.set_peer_salt(GUEST_SALT);
    guest
        .join_native_room("Together", room)
        .expect("the guest joins the room");
    Room {
        host,
        guest,
        personal,
        shared,
        id: room,
    }
}

/// The person's layout of the room, written into its layout record and taken
/// in on the guest's machine as the room's record route delivers it.
fn ship(room: &mut Room) {
    room.guest
        .hear_layout(&mut room.host, room.id, HOST_DEVICE)
        .expect("the guest takes in the room's layout");
}

/// The workspace holding `tab` in `tree`.
fn workspace_of(tree: &WorkspaceTree, tab: TabId) -> Option<WorkspaceId> {
    tree.desktop.workspaces.iter().find_map(|workspace| {
        workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .any(|candidate| candidate.id == tab)
            .then_some(workspace.id)
    })
}

/// Every tab of Settings `tree` shows in `room`'s workspace, whatever form
/// it takes there (a window, a peer's window, a held place).
fn settings_tabs_in(tree: &WorkspaceTree, room: RoomId) -> Vec<TabId> {
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| workspace.room() == room)
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter(|tab| {
            tilo_desktop_model::apps::canonical_app_id(&tab.tile).as_deref() == Some(SETTINGS)
        })
        .map(|tab| tab.id)
        .collect()
}

#[test]
fn settings_opened_from_a_shared_workspace_lands_in_the_personal_one() {
    let mut room = shared_room();

    // Settings' window arrives while the shared workspace is shown: once
    // with no launch attributing it, once attributed to the shared room
    // the launch was asked from.
    let unattributed = tab_binding::adopt_toplevel(&mut room.host, 7_001, Some(SETTINGS.into()));
    assert_eq!(
        workspace_of(&room.host, unattributed),
        Some(room.personal),
        "Settings opens in the personal workspace, not the shared one shown"
    );
    let attributed =
        tab_binding::adopt_toplevel_in_room(&mut room.host, 7_002, Some(SETTINGS.into()), room.id);
    if let Some(tab) = attributed {
        assert_eq!(workspace_of(&room.host, tab), Some(room.personal));
    }
    assert!(
        settings_tabs_in(&room.host, room.id).is_empty(),
        "no Settings window on the shared strip"
    );

    // What the collaborator's machine receives from the room.
    ship(&mut room);
    assert!(
        settings_tabs_in(&room.guest, room.id).is_empty(),
        "the collaborator never receives Settings as a peer window"
    );
}

#[test]
fn a_held_place_for_settings_is_held_in_the_personal_workspace() {
    let mut room = shared_room();
    let held = tab_binding::hold_place_for_launch(
        &mut room.host,
        SETTINGS,
        "Settings",
        tab_binding::Placement::NewGroup,
    );
    assert_eq!(workspace_of(&room.host, held), Some(room.personal));
    assert_eq!(
        room.host.desktop.active, room.personal,
        "the view goes to the personal workspace, where Settings opens"
    );
    ship(&mut room);
    assert!(settings_tabs_in(&room.guest, room.id).is_empty());
}

#[test]
fn a_settings_window_never_moves_onto_a_shared_strip() {
    let mut room = shared_room();
    room.host.set_active(room.personal);
    let tab = tab_binding::adopt_toplevel(&mut room.host, 7_003, Some(SETTINGS.into()));
    assert_eq!(workspace_of(&room.host, tab), Some(room.personal));

    let source = strip_of(&room.host, room.personal);
    let destination = strip_of(&room.host, room.shared);
    assert!(
        !room.host.move_tab_across_strips(
            source,
            destination,
            tab,
            0xF00D,
            shell::shell::state::DropTarget::GroupGap { gap_index: 0 },
        ),
        "a Settings window is refused a shared strip"
    );
    assert_eq!(workspace_of(&room.host, tab), Some(room.personal));
    ship(&mut room);
    assert!(settings_tabs_in(&room.guest, room.id).is_empty());

    // Onto another workspace only the person is in, the same move goes.
    let (also_mine, _) = room.host.create_hosted_workspace("Also mine");
    room.host
        .own_workspaces_for_test(shell::shell::state::local_user_id_for_room);
    assert!(
        room.host.move_tab_across_strips(
            source,
            strip_of(&room.host, also_mine),
            tab,
            0xF00E,
            shell::shell::state::DropTarget::GroupGap { gap_index: 0 },
        ),
        "a personal workspace takes the Settings window"
    );
    assert_eq!(workspace_of(&room.host, tab), Some(also_mine));
}

/// The first strip of `workspace`, made when it has none.
fn strip_of(tree: &WorkspaceTree, workspace: WorkspaceId) -> shell::shell::state::StripId {
    tree.desktop
        .workspaces
        .iter()
        .find(|candidate| candidate.id == workspace)
        .and_then(|candidate| candidate.strips.first())
        .map(|strip| strip.id)
        .expect("the workspace has a strip")
}

/// A tab in a shared workspace stays a tab there: the rule is Settings'
/// alone, and a workspace only its person is in takes it as before.
#[test]
fn other_apps_still_open_in_the_shared_workspace() {
    let mut room = shared_room();
    let tab = tab_binding::adopt_toplevel(&mut room.host, 7_004, Some("so.tilo.notes".into()));
    assert_eq!(workspace_of(&room.host, tab), Some(room.shared));
    assert!(matches!(
        room.host
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|candidate| candidate.id == tab)
            .map(|candidate| &candidate.tile),
        Some(TileContent::Foreign { .. })
    ));
}
