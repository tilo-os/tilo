//! The host's own operations as `tilo.host` actions: offered to and run for
//! the devices of the relay's own person only, decided as every door
//! decides a host operation, and asked with the certificate the transport
//! verified.

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, CatalogueQuery, InvokeRequest, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_relay_session::rooms::{
    CREATE_ROOM, CreatedRoom, ENTER_ROOM, EnteredRoom, GRANT_APP_AUTHORSHIP, HOST_ACTIONS_PROVIDER,
    HOST_ROOMS_RESOURCE, LIST_ROOMS, RoomListing,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use super::super::tests::{caller, settled};
use super::super::{GovernedPlane, HostRoomActions};
use crate::app_serving::AppServingPlane as _;
use crate::rooms::tests::{governor, guest_device, view_read_relay};

/// Another device of the relay's own person: the one `governor` certifies
/// the relay for.
fn owners_laptop() -> DeviceCert {
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[5; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "laptop".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    cert
}

fn host_request(action: &str, argument: Value) -> InvokeRequest {
    let mut request = super::super::tests::request("", action, argument);
    request.target = RemoteActionTarget {
        provider: HOST_ACTIONS_PROVIDER.into(),
        address: ActionAddress::new(HOST_ROOMS_RESOURCE, action),
    };
    request
}

/// What `device` is offered of the host's operations, asked on `workspace`.
async fn offered(host: &HostRoomActions, device: &DeviceCert, caller: &Caller) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(HOST_ACTIONS_PROVIDER.into()),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    settled(host.in_workspace_as(7, device).query(caller, query))
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect()
}

/// `request` by `device`, its certificate as the transport verified it.
async fn invoke(
    host: &HostRoomActions,
    device: &DeviceCert,
    request: InvokeRequest,
) -> Result<Value, ActionHostError> {
    let receipt = settled(
        host.in_workspace_as(7, device)
            .invoke(&caller(device), request),
    )
    .await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("a host operation completes at once: {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn the_relays_person_creates_and_lists_rooms_as_actions() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let host = HostRoomActions::new();
    host.attach(rooms.clone());
    let laptop = owners_laptop();

    assert_eq!(
        offered(&host, &laptop, &caller(&laptop)).await,
        [CREATE_ROOM, LIST_ROOMS, ENTER_ROOM, GRANT_APP_AUTHORSHIP].map(str::to_owned)
    );

    // Creating the room issues the invitation the share is for, with
    // exactly the terms asked.
    let (_, guest) = guest_device();
    let before = tilo_clock::unix_now_ns();
    let created: CreatedRoom = serde_json::from_value(
        invoke(
            &host,
            &laptop,
            host_request(
                CREATE_ROOM,
                json!({
                    "name": "Plans",
                    "invite_role": "commenter",
                    "invite_recipient": hex::encode(guest.user_public_key),
                    "invitee_name": "Grace",
                    "invite_expires_in_seconds": 120,
                }),
            ),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let after = tilo_clock::unix_now_ns();
    assert_eq!(created.room.name, "Plans");
    let first = created.invitation.expect("asked for").decode().unwrap();
    let pending = rooms
        .pending(&created.room.room, &owner)
        .await
        .unwrap()
        .into_iter()
        .find(|pending| pending.invite_id == first.invite_id)
        .expect("the first invitation is pending");
    assert_eq!(pending.role, MemberRole::Commenter);
    assert_eq!(
        pending.recipient_user_public_key,
        Some(guest.user_public_key)
    );
    assert_eq!(pending.invitee_name.as_deref(), Some("Grace"));
    let lifetime: u64 = 120_000_000_000;
    assert!(
        (before.saturating_add(lifetime)..=after.saturating_add(lifetime))
            .contains(&pending.expires_at_ns)
    );

    // Without terms, a room is created with no invitation.
    let bare: CreatedRoom = serde_json::from_value(
        invoke(
            &host,
            &laptop,
            host_request(CREATE_ROOM, json!({ "name": "Bare" })),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(bare.invitation.is_none());

    let listed: Vec<RoomListing> = serde_json::from_value(
        invoke(&host, &laptop, host_request(LIST_ROOMS, json!({})))
            .await
            .unwrap(),
    )
    .unwrap();
    for room in [&created.room, &bare.room] {
        assert!(
            listed
                .iter()
                .any(|listed| listed.room == room.room && listed.name == room.name),
            "{listed:?}"
        );
    }
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn the_relays_person_enters_a_room_and_lets_an_app_author_it_as_actions() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let host = HostRoomActions::new();
    host.attach(rooms.clone());
    let laptop = owners_laptop();
    let created = rooms.create("Plans").await.unwrap();

    // Entering admits the laptop and answers the invitation it redeems,
    // restricted to its own person.
    let enter = || host_request(ENTER_ROOM, json!({ "room": created.room }));
    let entered: EnteredRoom =
        serde_json::from_value(invoke(&host, &laptop, enter()).await.unwrap()).unwrap();
    assert!(entered.changed);
    let own = entered.invitation.decode().unwrap();
    let verified = decode_and_verify_invitation(
        &own.signed_invitation,
        Some(laptop.user_public_key),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert_eq!(
        verified.grant().recipient_user_public_key,
        Some(laptop.user_public_key)
    );
    let again: EnteredRoom =
        serde_json::from_value(invoke(&host, &laptop, enter()).await.unwrap()).unwrap();
    assert!(!again.changed, "already admitted");

    // Holding the room, the laptop lets an app author its records.
    let granted = invoke(
        &host,
        &laptop,
        host_request(
            GRANT_APP_AUTHORSHIP,
            json!({ "room": created.room, "app": "so.tilo.notes" }),
        ),
    )
    .await
    .unwrap();
    assert!(
        granted["generation"].as_u64() > Some(created.generation),
        "{granted}"
    );
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn no_one_else_and_nothing_uncertified_runs_the_hosts_operations() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let host = HostRoomActions::new();
    host.attach(rooms.clone());
    let room = rooms.create("Mine").await.unwrap();
    let laptop = owners_laptop();

    // Another person's device is offered nothing, and invoking is unknown.
    let (_, stranger) = guest_device();
    assert!(
        offered(&host, &stranger, &caller(&stranger))
            .await
            .is_empty()
    );
    for (action, argument) in [
        (CREATE_ROOM, json!({ "name": "Theirs" })),
        (LIST_ROOMS, json!({})),
        (ENTER_ROOM, json!({ "room": room.room })),
        (
            GRANT_APP_AUTHORSHIP,
            json!({ "room": room.room, "app": "so.tilo.notes" }),
        ),
    ] {
        assert_eq!(
            invoke(&host, &stranger, host_request(action, argument)).await,
            Err(ActionHostError::UnknownAction),
            "{action}"
        );
    }
    assert_eq!(rooms.list().await.len(), 1, "nothing was created");

    // An agent on the person's own key governs nothing.
    let agent = Caller::delegated_agent("writer", laptop.user_public_key, laptop.device_public_key);
    assert!(offered(&host, &laptop, &agent).await.is_empty());
    let refused = settled(
        host.in_workspace_as(7, &laptop)
            .invoke(&agent, host_request(LIST_ROOMS, json!({}))),
    )
    .await;
    assert_eq!(refused.unwrap_err(), ActionHostError::UnknownAction);

    // A certificate that is not the caller's speaks for nobody.
    let refused = settled(
        host.in_workspace_as(7, &laptop)
            .invoke(&caller(&stranger), host_request(LIST_ROOMS, json!({}))),
    )
    .await;
    assert_eq!(refused.unwrap_err(), ActionHostError::UnknownAction);

    // Without the certificate the transport verified, the host's operations
    // are not there at all.
    let unverified = settled(
        host.in_workspace(7)
            .invoke(&caller(&laptop), host_request(LIST_ROOMS, json!({}))),
    )
    .await;
    assert_eq!(unverified.unwrap_err(), ActionHostError::UnknownAction);

    // The plane hands the certificate on: through it, the laptop lists.
    let plane = GovernedPlane {
        governance: host.clone(),
        apps: None,
    };
    let list = RemoteActionMessage::Invoke(host_request(LIST_ROOMS, json!({})));
    assert!(plane.answers(&caller(&laptop), &list));
    let answer = plane
        .serve_certified(7, &caller(&laptop), &laptop, list)
        .expect("an invocation is answered");
    let RemoteActionMessage::Receipt(receipt) = settled(answer).await else {
        panic!("a receipt");
    };
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{:?}",
        receipt.status
    );
    relay.shutdown().await;
}
