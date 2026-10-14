//! A governed room's administration answers as `tilo.workspace` actions over
//! the relay's own remote-action envelope, by the same decision as its wire
//! doors: the owner's device invites; a member the room grants nothing is not
//! offered the action.
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHostError, CatalogueQuery, InvokeArgument, InvokeRequest, ROOM_GOVERNANCE_PROVIDER,
    ReceiptStatus, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};
use tilo_relay::transport::REMOTE_ACTION_ENVELOPE_VERSION;

use super::*;

/// A session of `device` subscribed to `workspace_id`.
async fn subscribed(relay: &Relay, device: &TestDevice, workspace_id: u64) -> RelayClient {
    let client = RelayClient::connect_to_pinned_host(
        device_endpoint(device).await,
        relay.node_addr(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("the room admits the device", async {
        while !matches!(
            client.recv().await.expect("the session stays open"),
            Envelope::Ready { .. }
        ) {}
    })
    .await;
    client
}

/// What the relay answers `message`, sent by `client` to the relay's own
/// device `host` about `workspace_id`.
async fn ask(
    client: &RelayClient,
    host: &TestDevice,
    workspace_id: u64,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    client
        .send(Envelope::RemoteAction {
            workspace_id,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: host.device_pubkey(),
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame: RemoteActionFrame::new(message).encode().unwrap(),
        })
        .unwrap();
    tilo_test_env::within("the relay answers the action", async {
        loop {
            match client.recv().await.expect("the session stays open") {
                Envelope::RemoteAction { frame, .. } => {
                    break RemoteActionFrame::decode(&frame).unwrap().message;
                }
                Envelope::RemoteActionUndeliverable { reason, .. } => {
                    panic!("the relay did not answer: {reason}")
                }
                _ => {}
            }
        }
    })
    .await
}

fn invite(room: &str) -> RemoteActionMessage {
    let mut argument = serde_json::Map::new();
    argument.insert("role".into(), "editor".into());
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::room_governance(room, "invite"),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owner_invites_through_the_rooms_action_and_an_editor_cannot() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Design review").await.unwrap();
    rooms.enter(&room.room, &laptop.cert).await.unwrap();
    super::topology_namespace::admit(&relay, &host, &phone, &room.room).await;

    // Alice's laptop invites through the action: the room records it, as
    // the administration door would have.
    let owner = subscribed(&relay, &laptop, room.workspace_id).await;
    let RemoteActionMessage::Receipt(receipt) =
        ask(&owner, &host, room.workspace_id, invite(&room.room)).await
    else {
        panic!("an invocation is answered with a receipt");
    };
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the owner invites: {:?}", receipt.status);
    };
    let invite_id = output.value.unwrap()["invite"].as_str().unwrap().to_owned();
    assert!(
        rooms
            .pending(&room.room, &host.cert)
            .await
            .unwrap()
            .iter()
            .any(|pending| hex::encode(pending.invite_id) == invite_id)
    );

    // The room offers its governance on its own resource.
    let RemoteActionMessage::CatalogueResponse { result, .. } = ask(
        &owner,
        &host,
        room.workspace_id,
        RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: CatalogueQuery {
                provider: Some(ROOM_GOVERNANCE_PROVIDER.into()),
                ..CatalogueQuery::default()
            },
        },
    )
    .await
    else {
        panic!("a catalogue request is answered with a page");
    };
    let resource = format!("workspace.permissions:{}", room.room);
    assert!(result.unwrap().rows.iter().any(|row| {
        row.resource.id == resource
            && row
                .action
                .as_ref()
                .is_some_and(|action| action.address.action == "invite")
    }));

    // Bob, an Editor the room grants nothing, is not offered it.
    let member = subscribed(&relay, &phone, room.workspace_id).await;
    let RemoteActionMessage::Receipt(receipt) =
        ask(&member, &host, room.workspace_id, invite(&room.room)).await
    else {
        panic!("an invocation is answered with a receipt");
    };
    assert_eq!(
        receipt.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::UnknownAction
        }
    );
    relay.shutdown().await;
}
