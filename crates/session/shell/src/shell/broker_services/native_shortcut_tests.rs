//! A shortcut saved through the library is the governor's signed write of
//! the room document; the tree only mirrors it.
use super::fixture;
use crate::shell::{broker_services::NativeShortcutLibrary, native_workspace_owner};
use tilo_action_broker::automation::ShortcutChange;

fn shortcut(id: &str, saved_by: &str) -> tilo_automation::Shortcut {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": id,
        "steps": [],
        "triggers": [tilo_automation::TriggerSpec::Schedule {
            schedule: tilo_automation::Schedule::Interval { seconds: 3600 },
            catch_up: tilo_automation::CatchUp::Skip,
        }],
        "saved_by": saved_by,
    }))
    .expect("shortcut JSON")
}

fn mirrored(
    tree: &crate::shell::state::WorkspaceTreeHandle,
    room: crate::shell::state::RoomId,
) -> Vec<tilo_automation::Shortcut> {
    tree.read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map(|workspace| tilo_automation::decode_saved(&workspace.shortcuts))
        .unwrap_or_default()
}

#[test]
fn a_saved_shortcut_lives_in_the_room_through_later_commits_and_a_restart() {
    let fixture = fixture();
    let tree = native_workspace_owner::test_tree_of(&fixture.owner);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let save = |actor: &str, shortcut: tilo_automation::Shortcut| {
        runtime.block_on(NativeShortcutLibrary::commit(
            tree.clone(),
            None,
            fixture.room,
            actor.to_owned(),
            ShortcutChange::Save(Box::new(shortcut)),
        ))
    };
    let me = hex::encode(fixture.certificate.user_public_key);

    save(&me, shortcut("water", &me)).expect("this device saves its person's shortcut");
    let saved = mirrored(&tree, fixture.room);
    assert_eq!(saved.len(), 1);
    assert_eq!(
        tilo_peer_state::room::proven_saver(
            &fixture.room.to_string(),
            &saved[0],
            &tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns(),
        ),
        Some(me.clone()),
        "saved with this device's proof"
    );
    let in_room = |owner: &tilo_peer_state::owner::NativeWorkspaceOwner| {
        owner
            .with_current(fixture.room, |view| {
                Ok(tilo_automation::decode_saved(
                    &tilo_desktop_model::projection::workspace_shortcuts_of(
                        view.peer().admitted_room().ok_or("no room")?,
                    ),
                ))
            })
            .unwrap()
    };
    assert_eq!(in_room(&fixture.owner), saved, "the tree mirrors the room");

    // The next owner commit projects the room again; the shortcut stays.
    save(&me, shortcut("ferns", &me)).unwrap();
    let both = mirrored(&tree, fixture.room);
    assert_eq!(
        both.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["ferns", "water"]
    );

    // A save the room refuses is not shown as saved.
    let other = "42".repeat(32);
    assert!(save(&other, shortcut("lamp", &other)).is_err());
    assert_eq!(mirrored(&tree, fixture.room), both);

    drop(runtime);
    drop(tree);
    drop(fixture.broker);
    crate::shell::persist::wait_for_saves();
    drop(fixture.owner);
    let mut stored = crate::shell::persist::load_tree_from(&fixture.path).unwrap();
    stored.persist_path = Some(fixture.path.clone());
    let stored = stored.shared();
    let reopened = native_workspace_owner::open(stored.clone()).unwrap();
    assert_eq!(in_room(&reopened), both, "the room keeps them on restart");
    assert_eq!(mirrored(&stored, fixture.room), both);
    drop(reopened);
    drop(fixture.governor);
    drop(fixture.home);
}
