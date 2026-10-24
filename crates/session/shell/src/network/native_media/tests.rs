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

/// A bridge's participations are its guests, each as the person it
/// carries, never the bridge's own person; the bridge itself listening
/// only says the call is bridged.
#[test]
fn a_bridged_call_lists_its_guests_apart_from_its_people() {
    use tilo_relay_protocol::voice::{
        Bridged, CallState, CallSummary, Directions, Showing, VoiceParticipant,
    };
    let ana = [0xa1; 32];
    let bridge = [0xb2; 32];
    let participant = |user: [u8; 32], n: u8, bridged: Option<Bridged>| VoiceParticipant {
        device_public_key: [n; 32],
        user_public_key: user,
        namespace: format!("{}/{}", "c".repeat(64), hex::encode([n; 32])),
        publishes: true,
        showing: Showing::default(),
        bridged,
    };
    let discord = |account: Option<&str>, name: &str| Bridged {
        service: "discord".into(),
        account: account.map(str::to_owned),
        name: name.into(),
        avatar: None,
        linked: None,
    };
    let call = |channel: &str, participants| CallSummary {
        state: CallState {
            call: [0xcc; 32],
            epoch: 1,
            active: true,
            allowed: Directions::default(),
        },
        app_id: "so.tilo.chat".into(),
        record_id: String::new(),
        channel_id: channel.into(),
        participants,
    };
    let calls = [
        call(
            "lounge",
            vec![participant(bridge, 9, Some(discord(None, "Tilo")))],
        ),
        call(
            "voice",
            vec![
                participant(ana, 1, None),
                participant(bridge, 2, Some(discord(Some("7"), "Dana"))),
                participant(bridge, 3, Some(discord(None, "Tilo"))),
            ],
        ),
    ];
    let rosters = registry::rosters_of(&calls, "so.tilo.chat", "");
    assert_eq!(
        rosters
            .iter()
            .map(|roster| (
                roster.channel_id.as_str(),
                roster.people.clone(),
                roster
                    .guests
                    .iter()
                    .map(|guest| (guest.id.clone(), guest.name.clone()))
                    .collect::<Vec<_>>(),
                roster.bridges.clone(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("lounge", vec![], vec![], vec!["discord".to_owned()]),
            (
                "voice",
                vec![hex::encode(ana)],
                vec![("discord:7".to_owned(), "Dana".to_owned())],
                vec!["discord".to_owned()],
            ),
        ]
    );
}
