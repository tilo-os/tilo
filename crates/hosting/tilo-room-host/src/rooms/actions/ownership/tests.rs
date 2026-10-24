//! Moving the room as governance actions: offered to its owner, accepted by
//! a device of the person it is offered to, and to nobody else.

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, CatalogueQuery, InvokeRequest,
    ROOM_GOVERNANCE_PROVIDER, ReceiptStatus,
};

use super::{ACCEPT, CANCEL, OWNERSHIP_VERBS, PROPOSE};
use crate::rooms::actions::HostRoomActions;
use crate::rooms::actions::tests::{admit, admit_device, caller, governed, request, settled};
use crate::rooms::ownership::tests::HostDevice;
use crate::rooms::tests::{governor, test_host};

async fn invoke(
    host: &HostRoomActions,
    workspace_id: u64,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<Value, ActionHostError> {
    let receipt = settled(host.in_workspace(workspace_id).invoke(caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("governance completes at once: {other:?}"),
    }
}

/// The ways of moving the room `caller` is offered.
async fn offered(host: &HostRoomActions, workspace_id: u64, caller: &Caller) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(ROOM_GOVERNANCE_PROVIDER.into()),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    settled(host.in_workspace(workspace_id).query(caller, query))
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .filter(|action| OWNERSHIP_VERBS.iter().any(|verb| verb.id == action))
        .collect()
}

#[tokio::test]
async fn the_owner_offers_the_room_and_the_recipients_device_accepts_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, workspace, host) = governed(rooms).await;
    let recipient = HostDevice::new();
    admit_device(rooms, &room, &owner, 1, (&recipient.key, &recipient.cert)).await;
    let member = admit(rooms, &room, &owner, 2).await;

    assert_eq!(offered(&host, workspace, &caller(&owner)).await, [PROPOSE]);
    assert!(
        offered(&host, workspace, &caller(&recipient.cert))
            .await
            .is_empty()
    );
    assert!(offered(&host, workspace, &caller(&member)).await.is_empty());
    assert!(matches!(
        invoke(
            &host,
            workspace,
            &caller(&member),
            request(
                &room,
                PROPOSE,
                json!({ "person": hex::encode(member.user_public_key) })
            ),
        )
        .await,
        Err(ActionHostError::UnknownAction)
    ));

    let offered_now = invoke(
        &host,
        workspace,
        &caller(&owner),
        request(
            &room,
            PROPOSE,
            json!({ "person": hex::encode(recipient.cert.user_public_key) }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        offered_now["person"],
        json!(hex::encode(recipient.cert.user_public_key))
    );
    assert_eq!(
        offered(&host, workspace, &caller(&owner)).await,
        [PROPOSE, CANCEL]
    );
    assert_eq!(
        offered(&host, workspace, &caller(&recipient.cert)).await,
        [ACCEPT]
    );
    assert!(offered(&host, workspace, &caller(&member)).await.is_empty());

    let offer = {
        let state = rooms.room(&room).await.unwrap();
        let offer = state.lock().await.peer().governance().ownership_offer();
        offer.unwrap()
    };
    let acceptance = postcard::to_stdvec(&recipient.accept(&offer)).unwrap();
    invoke(
        &host,
        workspace,
        &caller(&recipient.cert),
        request(
            &room,
            ACCEPT,
            json!({ "acceptance": hex::encode(acceptance) }),
        ),
    )
    .await
    .unwrap();

    // Moved: nobody moves it from here again.
    for device in [&owner, &recipient.cert, &member] {
        assert!(offered(&host, workspace, &caller(device)).await.is_empty());
    }
    relay.shutdown().await;
}
