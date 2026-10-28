//! A workspace's layout record, written by each device, and what a change
//! to it did to windows its writer did not make, which is what a room
//! decides; a room document's other maps merge by key.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use automerge::AutoCommit;
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::ownership::{Act, claimed, foreign_acts};
use tilo_desktop_model::projection::{project_workspace_shortcuts, workspace_shortcuts_of};
use tilo_desktop_model::shared_record::device::{Writer, write};
use tilo_desktop_model::{
    AppKind, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, compose_id,
};

const ALICE: u16 = 0x0A11;
const BOB: u16 = 0x0B0B;

fn tab(id: u64, title: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::Stub {
            kind: AppKind::Other(title.into()),
            label: title.into(),
        },
        app_title: None,
    }
}

fn group(id: u64, tabs: Vec<Tab>) -> WindowGroup {
    WindowGroup {
        id,
        active_tab: tabs.first().map_or(0, |t| t.id),
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// Write `workspace` as the person `person` at a device minting in `salt`.
fn write_as(doc: &mut AutoCommit, workspace: &Workspace, person: &str, salt: u16) {
    let mine = move |id: u64| tilo_desktop_model::peer_salt_of(id) == salt;
    write(
        doc,
        workspace.room(),
        workspace,
        &Writer {
            person: Some(person),
            mine: &mine,
            retained: &|_| false,
            carried: tilo_desktop_model::shared_record::device::Writer::NOTHING_CARRIED,
        },
    )
    .unwrap();
    doc.commit();
}

fn acts(before: &AutoCommit, after: &AutoCommit, writer: &str) -> Vec<(u64, Act)> {
    let before = tilo_desktop_model::shared_record::read(before, None).unwrap_or_default();
    let after = tilo_desktop_model::shared_record::read(after, None).unwrap_or_default();
    foreign_acts(&before, &after, writer, None)
        .into_iter()
        .chain(claimed(&before, &after, writer))
        .map(|touched| (touched.id, touched.act))
        .collect()
}

/// Alice's strip holds her notes and Bob's terminal, side by side.
fn room() -> (AutoCommit, Workspace) {
    let room = RoomId::new();
    let mut workspace = Workspace::in_room(WorkspaceId::of_room(room), room, "Team");
    let mut strip = Strip::new(StripId(compose_id(ALICE, 1)), "Main");
    strip.window_groups.push(group(
        compose_id(ALICE, 2),
        vec![tab(compose_id(ALICE, 3), "Notes")],
    ));
    workspace.strips.push(strip);
    let mut doc = AutoCommit::new();
    write_as(&mut doc, &workspace, "alice", ALICE);
    workspace.strips[0].window_groups.push(group(
        compose_id(BOB, 2),
        vec![tab(compose_id(BOB, 3), "Terminal")],
    ));
    write_as(&mut doc, &workspace, "bob", BOB);
    (doc, workspace)
}

#[test]
fn a_person_arranges_their_own_windows_from_any_device_and_others_only_with_leave() {
    let (doc, workspace) = room();
    let notes = compose_id(ALICE, 3);
    let terminal = compose_id(BOB, 3);

    // Alice, from another of her devices, closes her notes: hers to close.
    let mut closed = workspace.clone();
    closed.strips[0].window_groups.remove(0);
    let mut after = doc.clone();
    write_as(&mut after, &closed, "alice", 0x0A12);
    assert!(acts(&doc, &after, "alice").is_empty());
    // Bob doing the same closes someone else's window.
    assert_eq!(acts(&doc, &after, "bob"), vec![(notes, Act::Close)]);

    // Bob moving his own terminal into Alice's group touches nothing of hers.
    let mut moved = workspace;
    let terminal_tab = moved.strips[0].window_groups.remove(1).tabs.remove(0);
    moved.strips[0].window_groups[0].tabs.push(terminal_tab);
    let mut after = doc.clone();
    write_as(&mut after, &moved, "bob", BOB);
    assert!(acts(&doc, &after, "bob").is_empty());
    assert_eq!(acts(&doc, &after, "alice"), vec![(terminal, Act::Move)]);
}

#[test]
fn a_window_added_in_someone_elses_name_is_an_attribution() {
    let (doc, workspace) = room();
    let mut added = workspace;
    let forged = compose_id(BOB, 9);
    added.strips[0]
        .window_groups
        .push(group(compose_id(BOB, 8), vec![tab(forged, "Forged")]));
    // Written by Bob's device, but naming Alice as its maker.
    let mut after = doc.clone();
    write_as(&mut after, &added, "alice", BOB);
    assert_eq!(acts(&doc, &after, "bob"), vec![(forged, Act::Attribute)]);
    assert!(acts(&doc, &after, "alice").is_empty());
}

fn shortcut(id: &str, name: &str) -> tilo_automation::Shortcut {
    serde_json::from_value(serde_json::json!({ "id": id, "name": name, "steps": [] }))
        .expect("a shortcut with no steps")
}

/// Two collaborators saving different shortcuts at once both keep theirs,
/// and a deletion reaches the peer that already held the shortcut.
#[test]
fn shortcuts_merge_by_id_and_deletions_replicate() {
    let mut alice = AutoCommit::new();
    project_workspace_shortcuts(&mut alice, &[shortcut("tidy", "Tidy")]).unwrap();
    let mut bob = alice.fork();
    project_workspace_shortcuts(&mut alice, &[shortcut("tidy", "Tidy"), shortcut("a", "A")])
        .unwrap();
    project_workspace_shortcuts(&mut bob, &[shortcut("tidy", "Tidy"), shortcut("b", "B")]).unwrap();
    alice.merge_incoming(&mut bob).unwrap();
    assert_eq!(
        workspace_shortcuts_of(&alice),
        vec![
            shortcut("a", "A"),
            shortcut("b", "B"),
            shortcut("tidy", "Tidy")
        ]
    );

    let kept: Vec<_> = workspace_shortcuts_of(&alice)
        .into_iter()
        .filter(|shortcut| shortcut.id != "tidy")
        .collect();
    project_workspace_shortcuts(&mut alice, &kept).unwrap();
    bob.merge_incoming(&mut alice).unwrap();
    assert_eq!(
        workspace_shortcuts_of(&bob),
        vec![shortcut("a", "A"), shortcut("b", "B")]
    );
}

/// The room's owner holds what they made on its desktop as every member
/// does: their windows are not released to whoever writes there.
#[test]
fn the_rooms_owner_is_among_its_members() {
    let mut policy = tilo_action_policy::RoomPolicy::new("room", "owner".to_owned());
    policy
        .members
        .insert("ada".into(), tilo_action_policy::Role::Editor);
    let mut doc = AutoCommit::new();
    tilo_desktop_model::projection::project_room_policy(&mut doc, &policy).unwrap();
    let members = tilo_desktop_model::projection::room_members(&doc).unwrap();
    assert!(members.contains("owner"), "{members:?}");
    assert!(members.contains("ada"), "{members:?}");
}
