//! Real owner retry behavior; no serialized data is treated as a live witness.
use qos_peer_state::{desktop_command::*, owner_commit::*};
use qos_session_peer::SessionOwner;

fn opening(owner: &SessionOwner) -> OwnerCommitRequest {
    OwnerCommitRequest {
        room: owner.peer().room().into(),
        operation: CommitOperation::DesktopCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: owner.peer().authority().clone(),
            command: BoundCommand {
                source: CommandSource {
                    room: owner.peer().room().into(),
                    heads: owner
                        .peer()
                        .admitted_room()
                        .unwrap()
                        .clone()
                        .get_heads()
                        .into_iter()
                        .map(|h| h.0)
                        .collect(),
                },
                command: DesktopCommand::OpenTab {
                    app: "dev.qos.notes".into(),
                    placement: TabPlacement::EmptyWorkspace,
                },
            },
        },
    }
}
#[test]
fn selected_creation_exact_retry_survives_restart_without_a_second_factory_effect() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize_workspace(&path, "Creation retry").unwrap();
    owner.activate_delivery().unwrap();
    owner.activate_creation().unwrap();
    let request = opening(&owner);
    owner.commit_desktop_command(&request).unwrap();
    let status = owner.status();
    let bytes = std::fs::read(path.join("session.bin")).unwrap();
    owner
        .commit_desktop_command(&request)
        .expect("exact retained creation should replay without applying stale source");
    assert_eq!(owner.status(), status);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    owner.commit_desktop_command(&request).unwrap();
    assert_eq!(owner.status(), status);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
    assert_eq!(owner.status().records.len(), 1);
}
