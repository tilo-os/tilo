//! The person's workspace list on a host that keeps workspaces of people's
//! own: one of their devices keeps it, any key of theirs reads it back with
//! the workspaces this host keeps for them, an agent's grant reads only what
//! it reaches, and nobody keeps or reads another person's.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay handle a test binds is kept until the test ends; dropping it earlier closes the host it drives"
)]

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{ActionHost as _, ActionHostError, InvokeRequest, ReceiptStatus};
use tilo_relay_session::workspaces::{
    CREATE_OWN_ROOM, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, KEEP_PERSONAL,
    LIST_PERSONAL, PersonalList,
};
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};

use super::super::HostWorkspaceActions;
use super::super::tests::{caller, settled};
use crate::workspaces::OwnWorkspaceOffer;
use crate::workspaces::tests::{governor, test_host};

fn host_request(action: &str, argument: Value) -> InvokeRequest {
    let mut request = super::super::tests::request("", action, argument);
    request.target = tilo_actions::remote::RemoteActionTarget {
        provider: HOST_ACTIONS_PROVIDER.into(),
        address: tilo_actions::ActionAddress::new(HOST_ROOMS_RESOURCE, action),
    };
    request
}

async fn invoke(
    host: &HostWorkspaceActions,
    device: &DeviceCert,
    action: &str,
    argument: Value,
) -> Result<Value, ActionHostError> {
    let receipt = settled(
        host.as_host(device)
            .invoke(&caller(device), host_request(action, argument)),
    )
    .await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("a host operation completes at once: {other:?}"),
    }
}

async fn read(host: &HostWorkspaceActions, device: &DeviceCert) -> PersonalList {
    serde_json::from_value(
        invoke(host, device, LIST_PERSONAL, json!({}))
            .await
            .unwrap(),
    )
    .unwrap()
}

/// A key of the person `root`, with `capabilities`.
fn key_of(root: &SigningKey, label: &str, capabilities: DeviceCapabilities) -> DeviceCert {
    let key = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: tilo_clock::unix_now_ns(),
        // A grant holds for at most 90 days.
        expires_at_ns: Some(tilo_clock::unix_now_ns().saturating_add(24 * 60 * 60 * 1_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

const DESK: &str = "11111111-1111-4111-8111-111111111111";
const LAPTOP: &str = "22222222-2222-4222-8222-222222222222";

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one person's list from its first keep through a restart"
)]
async fn a_persons_keys_read_the_list_their_devices_kept_with_the_workspaces_here() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    // The site let the person in: an invitation into its home names them.
    let home = workspaces.create("Lounge").await.unwrap().workspace;
    assert!(workspaces.admit_own_through(&home));
    workspaces
        .issue(
            &home,
            workspaces.serving_certificate(),
            crate::workspaces::IssueInvitationRequest {
                request_id: rand::random(),
                role: tilo_peer_state::join::MemberRole::Viewer,
                recipient_user_public_key: Some(root.verifying_key().to_bytes()),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let desktop = key_of(&root, "studio", DeviceCapabilities::member());
    let agent = key_of(
        &root,
        "Claude",
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );
    let pinned = key_of(
        &root,
        "one workspace",
        DeviceCapabilities::agent_grant(AgentScope::parse(DESK).unwrap()),
    );

    // Nothing kept yet: a key reads only the workspaces this host keeps.
    let created: CreatedWorkspace = serde_json::from_value(
        invoke(
            &host,
            &desktop,
            CREATE_OWN_ROOM,
            json!({"name": "On the relay"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let here = &created.workspace.workspace;
    let listed = read(&host, &agent).await;
    assert_eq!(listed.workspaces.len(), 1, "{listed:?}");
    assert_eq!(&listed.workspaces[0].workspace, here);
    assert_eq!(listed.workspaces[0].role.as_deref(), Some("owner"));
    let address = listed.workspaces[0].host.as_ref().unwrap();
    assert_eq!(address.name, "tilo.test");
    assert_eq!(
        address.hint, None,
        "a workspace this host keeps is reached where the list was asked"
    );

    // The desktop keeps the person's list: its own workspaces, hosted on it.
    let kept = json!({"workspaces": [
        {"workspace": DESK, "name": "Desk", "host": {"name": "studio", "hint": "00"}},
        {"workspace": LAPTOP, "name": "Laptop"},
        {"workspace": here, "name": "On the relay"},
    ]});
    assert_eq!(
        invoke(&host, &desktop, KEEP_PERSONAL, json!({"list": kept}))
            .await
            .unwrap(),
        json!({"kept": 3})
    );
    let listed = read(&host, &agent).await;
    let names: Vec<_> = listed
        .workspaces
        .iter()
        .map(|one| {
            (
                one.workspace.as_str(),
                one.host.as_ref().map(|host| host.name.as_str()),
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            (DESK, Some("studio")),
            (LAPTOP, None),
            (here.as_str(), Some("tilo.test")),
        ],
        "the person's order, each workspace here as this host knows it"
    );

    // A grant for one workspace reads that one alone.
    let listed = read(&host, &pinned).await;
    assert_eq!(
        listed
            .workspaces
            .iter()
            .map(|one| one.workspace.as_str())
            .collect::<Vec<_>>(),
        [DESK]
    );

    // A grant keeps nothing: the list is what the person's own devices hold.
    assert!(
        invoke(
            &host,
            &agent,
            KEEP_PERSONAL,
            json!({"list": {"workspaces": []}})
        )
        .await
        .is_err()
    );
    // A list that names something other than workspaces is refused.
    assert!(
        invoke(
            &host,
            &desktop,
            KEEP_PERSONAL,
            json!({"list": {"workspaces": [{"workspace": "not a workspace", "name": "x"}]}})
        )
        .await
        .is_err()
    );

    // Someone else reads only theirs.
    let stranger = key_of(
        &SigningKey::from_bytes(&rand::random()),
        "stranger",
        DeviceCapabilities::member(),
    );
    assert!(read(&host, &stranger).await.workspaces.is_empty());
    // Nor keeps one: the site never let them in, and nothing here is theirs.
    assert!(
        invoke(
            &host,
            &stranger,
            KEEP_PERSONAL,
            json!({"list": {"workspaces": []}})
        )
        .await
        .is_err()
    );

    // The list stays across a restart.
    relay.shutdown().await;
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    assert_eq!(read(&host, &agent).await.workspaces.len(), 3);
    relay.shutdown().await;
}
