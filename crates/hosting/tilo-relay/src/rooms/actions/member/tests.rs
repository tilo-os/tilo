//! A member's own business as `tilo.member` actions: offered to every
//! person the room admitted, and decided by the room methods the wire door
//! ran.

use serde_json::{Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, CatalogueQuery, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_relay_session::rooms::{
    CLAIM_TOPOLOGY_NAMESPACE, MEMBER_ACTIONS_PROVIDER, PREPARE_APPLICATION_RECORD,
    RECORD_EFFECT_RECEIPT, member_resource,
};
use tilo_user_identity::DeviceCert;

use super::super::HostRoomActions;
use super::super::tests::{admit, caller, governed, settled};
use crate::rooms::tests::{governor, guest_device, view_read_relay};

fn member_request(room: &str, action: &str, argument: Value) -> InvokeRequest {
    let mut request = super::super::tests::request(room, action, argument);
    request.target = RemoteActionTarget {
        provider: MEMBER_ACTIONS_PROVIDER.into(),
        address: ActionAddress::new(member_resource(room), action),
    };
    request
}

async fn offered(host: &HostRoomActions, workspace_id: u64, caller: &Caller) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(MEMBER_ACTIONS_PROVIDER.into()),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    settled(host.in_workspace(workspace_id).query(caller, query))
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect()
}

async fn invoke(
    host: &HostRoomActions,
    workspace_id: u64,
    device: &DeviceCert,
    request: InvokeRequest,
) -> Result<Value, ActionHostError> {
    let receipt = settled(
        host.in_workspace(workspace_id)
            .invoke(&caller(device), request),
    )
    .await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("a member's own business completes at once: {other:?}"),
    }
}

/// `person`'s own terminal receipt of `invocation`, redacted as the broker
/// redacts it.
fn terminal_audit(person: &[u8; 32], invocation: uuid::Uuid) -> String {
    let mut context =
        tilo_actions::ActionContext::for_resource("workspace", "workspace", "Private fixture");
    context.facets = vec![
        tilo_actions::ActionFacet::new("tilo.receipt", "effect"),
        tilo_actions::ActionFacet::new("tilo.invocation", invocation.to_string()),
    ];
    format!(
        "{{\"invocation\":{},\"actor\":{},\"context\":{},\"status\":{},\"completed_at\":1}}",
        serde_json::to_string(&invocation).unwrap(),
        serde_json::to_string(&hex::encode(person)).unwrap(),
        serde_json::to_string(&context).unwrap(),
        serde_json::to_string(&tilo_actions::EffectReceiptStatus::Succeeded).unwrap(),
    )
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn a_member_claims_their_namespace_and_records_their_receipt_as_actions() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let (room, workspace, host) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;

    assert_eq!(
        offered(&host, workspace, &caller(&ada)).await,
        [
            CLAIM_TOPOLOGY_NAMESPACE,
            RECORD_EFFECT_RECEIPT,
            PREPARE_APPLICATION_RECORD
        ]
        .map(str::to_owned)
    );

    // Ada, an Editor, claims her own namespace; the room signs it.
    let claim =
        |salt: u64| member_request(&room, CLAIM_TOPOLOGY_NAMESPACE, json!({ "salt": salt }));
    let claimed = invoke(&host, workspace, &ada, claim(42)).await.unwrap();
    assert!(claimed["generation"].as_u64().is_some(), "{claimed}");
    assert_eq!(
        invoke(&host, workspace, &ada, claim(42)).await.unwrap(),
        claimed,
        "a retry keeps the committed generation"
    );
    for salt in [0, 65_536] {
        assert!(matches!(
            invoke(&host, workspace, &ada, claim(salt)).await,
            Err(ActionHostError::InvalidRequest { .. })
        ));
    }

    // Her own receipt is recorded; one claiming another actor is refused.
    let invocation = uuid::Uuid::new_v4();
    let receipt = member_request(
        &room,
        RECORD_EFFECT_RECEIPT,
        json!({
            "invocation": invocation.to_string(),
            "redacted": terminal_audit(&ada.user_public_key, invocation),
        }),
    );
    let recorded = invoke(&host, workspace, &ada, receipt).await.unwrap();
    assert!(recorded["generation"].as_u64().is_some(), "{recorded}");
    let spoof = uuid::Uuid::new_v4();
    let impersonating = member_request(
        &room,
        RECORD_EFFECT_RECEIPT,
        json!({
            "invocation": spoof.to_string(),
            "redacted": terminal_audit(&owner.user_public_key, spoof),
        }),
    );
    assert!(matches!(
        invoke(&host, workspace, &ada, impersonating).await,
        Err(ActionHostError::Refused { .. })
    ));

    // Someone the room never admitted is offered nothing and runs nothing,
    // and an agent holds no business of its own.
    let (_, stranger) = guest_device();
    assert!(
        offered(&host, workspace, &caller(&stranger))
            .await
            .is_empty()
    );
    assert_eq!(
        invoke(&host, workspace, &stranger, claim(7)).await,
        Err(ActionHostError::UnknownAction)
    );
    let agent = Caller::delegated_agent("writer", ada.user_public_key, ada.device_public_key);
    assert!(offered(&host, workspace, &agent).await.is_empty());

    // The member's business is that room's only.
    let mut elsewhere = claim(8);
    elsewhere.target.address =
        ActionAddress::new(member_resource("another"), CLAIM_TOPOLOGY_NAMESPACE);
    assert_eq!(
        invoke(&host, workspace, &ada, elsewhere).await,
        Err(ActionHostError::UnknownAction)
    );
    relay.shutdown().await;
}
