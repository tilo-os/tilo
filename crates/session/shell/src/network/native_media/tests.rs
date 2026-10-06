use super::*;
use crate::network::HostIdentity;
use crate::network::actor::native_ingress::NOT_CONNECTED;
use crate::shell::native_workspace_owner::test_enrolled_owner;
use crate::shell::state::RoomId;

fn identity_for(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    key: ed25519_dalek::SigningKey,
) -> HostIdentity {
    HostIdentity {
        device_cert: owner
            .with_current(room, |view| Ok(view.device().clone()))
            .unwrap(),
        device_signing_key: key,
        workspace_device_certs: std::collections::HashMap::default(),
        workspace_revocations: std::collections::HashMap::default(),
        identity_paths: None,
    }
}

fn open(
    registry: &mut Registry,
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    identity: Option<&HostIdentity>,
) -> Result<Reply, String> {
    let (reply, mut answer) = tokio::sync::oneshot::channel();
    registry.handle(
        Command {
            operation: Operation::Open {
                owner: owner.clone(),
                target: CallTarget {
                    workspace: room.to_string(),
                    app: "so.tilo.chat".into(),
                    record: String::new(),
                    channel: "voice".into(),
                },
            },
            reply,
        },
        identity,
        &|_| None,
    );
    answer.try_recv().expect("the actor answers at once")
}

#[test]
fn a_call_opens_only_over_the_rooms_relay_session() {
    let (_directory, owner, room, _, key) = test_enrolled_owner();
    let identity = identity_for(&owner, room, key);
    let mut registry = Registry::default();
    let Err(error) = open(&mut registry, &owner, room, Some(&identity)) else {
        panic!("no relay session, no call");
    };
    assert_eq!(error, NOT_CONNECTED, "the caller may retry once connected");
}

#[test]
fn only_the_rooms_actual_device_custody_opens_a_call() {
    let (_directory, owner, room, _, _) = test_enrolled_owner();
    let wrong = identity_for(
        &owner,
        room,
        ed25519_dalek::SigningKey::from_bytes(&[17; 32]),
    );
    let mut registry = Registry::default();
    for identity in [None, Some(&wrong)] {
        let Err(error) = open(&mut registry, &owner, room, identity) else {
            panic!("another key opened a call");
        };
        assert_ne!(error, NOT_CONNECTED, "refused before any relay route");
    }
}
