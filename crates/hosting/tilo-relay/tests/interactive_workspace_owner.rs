//! A primary device serves only its exact governed room on the shell endpoint.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]

use iroh::endpoint::presets;
use tilo_delivery_protocol::workspace_join::{
    IrohWorkspaceHostHintV1, WorkspaceHostMode, WorkspaceHostTrust,
};
use tilo_peer_state::GovernedAccess;
use tilo_peer_state::join::{InviteGrant, MemberRole};
use tilo_relay::{
    CheckpointPrefixRecovery, Envelope, Relay, RelayClient, RelayConfig, ReplicationIntent,
    WorkspaceBinding,
};
use tilo_relay_protocol::retention::RetentionAuthority;
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint};

fn interactive_binding(
    workspace_id: u64,
    room: &uuid::Uuid,
    governor: tilo_user_identity::DeviceCert,
) -> WorkspaceBinding {
    WorkspaceBinding {
        id: workspace_id,
        pin: true,
        replication: ReplicationIntent::FullWorkspace,
        admitted_users: Vec::new(),
        shell_authority: Some(RetentionAuthority::new(&room.to_string(), governor).unwrap()),
        checkpoint_prefix_recovery: CheckpointPrefixRecovery::NativeOwnerDurableBaseline,
    }
}

fn seed_durable_snapshot(state_root: &std::path::Path, workspace_id: u64) {
    let store = tilo_relay::store::WorkspaceStore::open(state_root, workspace_id, true).unwrap();
    let mut document = automerge::AutoCommit::new();
    store.save_snapshot(&document.save()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interactive_owner_is_exactly_cert_and_room_scoped() {
    let owner = TestUser::fresh("Owner");
    let guest = TestUser::fresh("Guest");
    let outsider = TestUser::fresh("Outsider");
    let owner_device = owner.enrol("primary", DeviceCapabilities::primary());
    let guest_device = guest.enrol("guest", DeviceCapabilities::primary());
    let outsider_device = outsider.enrol("outsider", DeviceCapabilities::primary());
    let room = uuid::Uuid::new_v4();
    let workspace_id = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    let state = tempfile::tempdir().unwrap();
    seed_durable_snapshot(state.path(), workspace_id);
    let endpoint = iroh::Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let mut binding = WorkspaceBinding {
        id: workspace_id,
        pin: true,
        replication: ReplicationIntent::FullWorkspace,
        admitted_users: Vec::new(),
        shell_authority: Some(
            RetentionAuthority::new(&room.to_string(), owner_device.cert.clone()).unwrap(),
        ),
        checkpoint_prefix_recovery: CheckpointPrefixRecovery::NativeOwnerDurableBaseline,
    };
    let relay = Relay::start_native_interactive_workspace_on_endpoint_with_protocols(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            device_role: tilo_relay::DeviceRole::Interactive,
            workspaces: vec![binding.clone()],
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: owner.user_pk,
            device_signing_key: owner_device.sk.clone(),
            device_cert: owner_device.cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint,
        Vec::new(),
    )
    .await
    .unwrap();
    let pre_reconcile = RelayClient::connect_v2_to_interactive_workspace_owner(
        client_endpoint().await,
        relay.node_addr(),
        relay.native_interactive_owner_trust().unwrap(),
        owner_device.cert.clone(),
        owner_device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    pre_reconcile
        .send(Envelope::Subscribe { workspace_id })
        .unwrap();
    assert!(matches!(
        pre_reconcile.recv().await,
        Some(Envelope::NotAuthorised { .. })
    ));
    drop(pre_reconcile);
    relay
        .replace_governed_workspace_access(
            workspace_id,
            true,
            GovernedAccess {
                member_devices: vec![owner_device.device_pubkey(), guest_device.device_pubkey()],
                member_user_roots: vec![owner.user_pk, guest.user_pk],
            },
            state.path().to_path_buf(),
        )
        .await
        .unwrap();
    let unrelated_workspace = workspace_id.wrapping_add(1);
    assert!(
        matches!(
            relay
                .replace_governed_workspace_access(
                    unrelated_workspace,
                    true,
                    GovernedAccess {
                        member_devices: vec![guest_device.device_pubkey()],
                        member_user_roots: vec![guest.user_pk],
                    },
                    state.path().to_path_buf(),
                )
                .await,
            Err(tilo_relay::RelayError::GovernedAccess(_))
        ),
        "interactive owner reconciliation must reject every unconfigured workspace"
    );
    assert!(
        !state
            .path()
            .join("workspaces")
            .join(unrelated_workspace.to_string())
            .exists(),
        "rejected reconciliation must not create durable state for another room"
    );
    assert!(
        matches!(
            relay
                .set_workspace_membership(
                    unrelated_workspace,
                    true,
                    vec![guest_device.device_pubkey()],
                    state.path().to_path_buf(),
                )
                .await,
            Err(tilo_relay::RelayError::GovernedAccess(_))
        ),
        "device-only membership replacement must obey the interactive room ceiling"
    );
    assert!(
        !state
            .path()
            .join("workspaces")
            .join(unrelated_workspace.to_string())
            .exists(),
        "rejected device-only membership must not create another room"
    );

    let host_hint = IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: room.to_string(),
        workspace_id,
        governing_authority: owner_device.cert.clone(),
        endpoint_addr: relay.node_addr(),
        host_user_public_key: owner.user_pk,
        host_device_certificate: owner_device.cert.clone(),
        packet_relay_url: "disabled".into(),
    };
    let signed_hint = host_hint.transport_hint(&owner_device.sk).unwrap();
    let verified_hint = IrohWorkspaceHostHintV1::from_transport_hint(&signed_hint).unwrap();
    let trust = verified_hint
        .validate_invitation(&InviteGrant {
            workspace_id,
            room: room.to_string(),
            authority: owner_device.cert.clone(),
            issuer: owner_device.cert.clone(),
            delegation: None,
            role: MemberRole::Editor,
            nonce: [7; 16],
            recipient_user_public_key: Some(guest.user_pk),
            invitee_name: None,
            expires_at_ns: u64::MAX,
            transport: vec![signed_hint],
        })
        .unwrap();
    let WorkspaceHostTrust::InteractiveOwner(trust) = trust else {
        panic!("interactive hint produced certified-host trust")
    };
    assert_eq!(relay.native_interactive_owner_trust(), Some(trust.clone()));

    let owner_client = RelayClient::connect_v2_to_interactive_workspace_owner(
        client_endpoint().await,
        relay.node_addr(),
        trust.clone(),
        owner_device.cert.clone(),
        owner_device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    owner_client
        .send(Envelope::Subscribe { workspace_id })
        .unwrap();
    assert!(matches!(
        owner_client.recv().await,
        Some(Envelope::Ready { .. })
    ));
    owner_client
        .send_action(tilo_delivery_protocol::transport_v2::EnvelopeV2::EnableActions { room })
        .unwrap();
    assert!(matches!(
        owner_client.recv_action().await,
        Some(tilo_delivery_protocol::transport_v2::EnvelopeV2::ActionsReady {
            room: ready_room,
            capability: tilo_delivery_protocol::transport_v2::ActionCapability::TransportOnly,
        }) if ready_room == room
    ));
    let broker_host = uuid::Uuid::from_u128(9);
    owner_client
        .send_action(
            tilo_delivery_protocol::transport_v2::EnvelopeV2::RegisterBroker {
                room,
                host: broker_host,
            },
        )
        .unwrap();
    assert!(matches!(
        owner_client.recv_action().await,
        Some(tilo_delivery_protocol::transport_v2::EnvelopeV2::ActionsReady {
            room: ready_room,
            capability: tilo_delivery_protocol::transport_v2::ActionCapability::DurableBroker { host },
        }) if ready_room == room && host == broker_host
    ));

    assert!(
        RelayClient::connect_to_pinned_host(
            client_endpoint().await,
            relay.node_addr(),
            guest_device.cert.clone(),
            guest_device.sk.clone(),
            RevocationList::new(),
        )
        .await
        .is_err(),
        "generic pinned-host auth must still require a host-capable certificate"
    );
    assert!(
        RelayClient::connect_v2_to_interactive_workspace_owner(
            client_endpoint().await,
            relay.node_addr(),
            trust.clone(),
            outsider_device.cert,
            outsider_device.sk,
            RevocationList::new(),
        )
        .await
        .is_err(),
        "rejected workspace mutations must not widen the global root allowlist"
    );
    let client = RelayClient::connect_v2_to_interactive_workspace_owner(
        client_endpoint().await,
        relay.node_addr(),
        trust,
        guest_device.cert,
        guest_device.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    assert!(matches!(client.recv().await, Some(Envelope::Ready { .. })));
    assert!(
        client
            .send(Envelope::Subscribe {
                workspace_id: workspace_id.wrapping_add(1),
            })
            .is_err(),
        "interactive trust cannot subscribe outside its signed workspace"
    );
    assert!(
        client
            .send_action(
                tilo_delivery_protocol::transport_v2::EnvelopeV2::EnableActions {
                    room: uuid::Uuid::new_v4(),
                }
            )
            .is_err(),
        "interactive trust cannot address actions outside its signed room"
    );
    client
        .outbound
        .try_send(Envelope::Subscribe {
            workspace_id: workspace_id.wrapping_add(1),
        })
        .unwrap();
    assert!(
        tilo_test_env::within("client to close", client.recv())
            .await
            .is_none()
    );
    drop(client);
    drop(owner_client);
    relay.shutdown().await;

    let ambiguous_endpoint = iroh::Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    assert!(
        Relay::start_native_interactive_workspace_on_endpoint_with_protocols(
            RelayConfig {
                state_root: state.path().join("ambiguous"),
                device_role: tilo_relay::DeviceRole::Interactive,
                workspaces: vec![binding.clone(), binding.clone()],
                ..RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: owner.user_pk,
                device_signing_key: owner_device.sk.clone(),
                device_cert: owner_device.cert.clone(),
                revocations: RevocationList::new(),
            },
            ambiguous_endpoint,
            Vec::new(),
        )
        .await
        .is_err(),
        "interactive owner mode cannot be widened to multiple bindings"
    );

    binding.id = workspace_id.wrapping_add(1);
    let invalid_endpoint = iroh::Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    assert!(
        Relay::start_native_interactive_workspace_on_endpoint_with_protocols(
            RelayConfig {
                state_root: state.path().join("invalid"),
                device_role: tilo_relay::DeviceRole::Interactive,
                workspaces: vec![binding],
                ..RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: owner.user_pk,
                device_signing_key: owner_device.sk,
                device_cert: owner_device.cert,
                revocations: RevocationList::new(),
            },
            invalid_endpoint,
            Vec::new(),
        )
        .await
        .is_err(),
        "a shell authority cannot be replayed for another workspace"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_owner_requires_an_existing_durable_snapshot() {
    let owner = TestUser::fresh("Owner");
    let owner_device = owner.enrol("primary", DeviceCapabilities::primary());
    let room = uuid::Uuid::new_v4();
    let workspace_id = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    let state = tempfile::tempdir().unwrap();
    let endpoint = iroh::Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();

    let result = Relay::start_native_interactive_workspace_on_endpoint_with_protocols(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            device_role: tilo_relay::DeviceRole::Interactive,
            workspaces: vec![interactive_binding(
                workspace_id,
                &room,
                owner_device.cert.clone(),
            )],
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: owner.user_pk,
            device_signing_key: owner_device.sk,
            device_cert: owner_device.cert,
            revocations: RevocationList::new(),
        },
        endpoint,
        Vec::new(),
    )
    .await;

    assert!(
        matches!(result, Err(tilo_relay::RelayError::Identity(message)) if message.contains("durable snapshot")),
        "an empty state root must not become an authoritative empty workspace"
    );
}
