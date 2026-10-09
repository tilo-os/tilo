#![allow(
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "deadline offsets are small constants, and guards stay scoped to their assertions"
)]
use super::*;
use crate::peer_actions::OwnerPolicyClock;
use ed25519_dalek::SigningKey;
use tilo_delivery_protocol::peer_actions::{ActionMessage, DirectedAction};
use tilo_delivery_protocol::transport_v2::EnvelopeV2;
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::{Envelope, RelayClient, RelayConfig, WorkspaceBinding};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use uuid::Uuid;

const CALLER: u8 = 11;
const RECIPIENT: u8 = 12;

fn device(seed: u8, host: bool) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[10; 32]);
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "carrier test device".into(),
        capabilities: if host {
            DeviceCapabilities::host()
        } else {
            DeviceCapabilities::primary()
        },
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

async fn client(relay: &Relay, seed: u8, room: Uuid) -> RelayClient {
    let (key, cert) = device(seed, false);
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .bind()
        .await
        .unwrap();
    let client = RelayClient::connect_v2_to_enrolled_home_peer(
        endpoint,
        relay.node_addr(),
        relay.own_user_public_key(),
        relay.handle().mesh_profile().device,
        cert,
        key,
        RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: tilo_relay::room_workspace_id(&room.to_string()).unwrap(),
        })
        .unwrap();
    tilo_test_env::within("the relay client to be ready", async {
        while !matches!(client.recv().await.unwrap(), Envelope::Ready { .. }) {}
    })
    .await;
    client
        .send_action(EnvelopeV2::EnableActions { room })
        .unwrap();
    assert!(matches!(
        tilo_test_env::within("`client.recv_action()`", client.recv_action()).await,
        Some(EnvelopeV2::ActionsReady { .. })
    ));
    client
}

/// Evidence minted by the only production constructor: a relay session that
/// verified the caller's certificate and delivered its directed request. This
/// fixture proves queue custody only; it does not qualify an owner installation.
async fn certified() -> CertifiedAction {
    let relay_root = tempfile::tempdir().unwrap();
    let room = Uuid::from_u128(1);
    let ws = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    let (relay_key, relay_cert) = device(13, true);
    let relay = Relay::start(
        RelayConfig {
            state_root: relay_root.path().into(),
            workspaces: vec![WorkspaceBinding {
                id: ws,
                pin: true,
                replication: tilo_relay::ReplicationIntent::FullWorkspace,
                admitted_users: vec![],
                shell_authority: None,
                checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::default(),
            }],
            ..Default::default()
        },
        RelayIdentity {
            user_public_key: relay_cert.user_public_key,
            device_signing_key: relay_key,
            device_cert: relay_cert,
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    relay
        .set_workspace_membership(
            ws,
            true,
            [CALLER, RECIPIENT]
                .into_iter()
                .map(|seed| device(seed, false).1.device_public_key)
                .collect(),
            relay_root.path().into(),
        )
        .await
        .unwrap();
    let caller = client(&relay, CALLER, room).await;
    caller
        .send_action(EnvelopeV2::EnableBrokerClient { room })
        .unwrap();
    let recipient = client(&relay, RECIPIENT, room).await;
    recipient
        .send_action(EnvelopeV2::RegisterBroker {
            room,
            host: Uuid::from_u128(4),
        })
        .unwrap();
    assert!(matches!(
        tilo_test_env::within("the broker registration", recipient.recv_action()).await,
        Some(EnvelopeV2::ActionsReady { .. })
    ));
    let action = DirectedAction {
        room,
        request_id: Uuid::from_u128(2),
        source: device(CALLER, false).1.device_public_key,
        target: device(RECIPIENT, false).1.device_public_key,
        message: ActionMessage::Status {
            invocation: tilo_actions::InvocationId(Uuid::from_u128(3)),
        },
    };
    caller
        .send_action(EnvelopeV2::Directed(action.clone()))
        .unwrap();
    let Some(tilo_relay::client::CertifiedActionEvent::Delivered(evidence)) =
        tilo_test_env::within("the certified delivery", recipient.recv_certified_action()).await
    else {
        panic!("real certified request required")
    };
    assert_eq!(evidence.action(), &action);
    caller.close().await;
    recipient.close().await;
    relay.shutdown().await;
    evidence
}
#[tokio::test]
async fn owner_policy_clock_carrier_retains_exact_origin_and_original_capacity() {
    let evidence = certified().await;
    let clock = OwnerPolicyClock::new().unwrap();
    let origin = clock.new_origin();
    let time = origin.capture().unwrap();
    let (queue, mut receiver) = PeerActionQueue::channel();
    let deadline = Instant::now() + std::time::Duration::from_secs(4);
    let mut pending = queue
        .submit_certified_with_policy_time(evidence.clone(), deadline, time.clone())
        .unwrap();
    let request = receiver.recv().await.unwrap();
    assert!(
        request
            .policy_time()
            .is_some_and(|stored| stored.same_origin(&time))
    );
    assert!(
        pending
            .policy_time()
            .is_some_and(|stored| stored.same_origin(&time))
    );
    assert_eq!(request.deadline, deadline);
    assert_eq!(pending.deadline(), deadline);
    let mut other = Vec::new();
    for _ in 0..31 {
        other.push(queue.submit_certified(evidence.clone(), deadline).unwrap());
    }
    assert_eq!(queue.permits.available_permits(), 0);
    drop(request);
    assert_eq!(
        queue.permits.available_permits(),
        0,
        "unconsumed pending reply retains original permit"
    );
    origin.invalidate();
    assert!(pending.policy_time().unwrap().now_ns().is_err());
    assert!(pending.try_reply().is_err());
    assert!(
        queue
            .submit_certified_with_policy_time(evidence.clone(), deadline, time)
            .is_err()
    );
    drop(pending);
    assert_eq!(queue.permits.available_permits(), 1);
    drop(other);
}
#[tokio::test]
async fn owner_policy_clock_transition_preserves_original_context_and_legacy_none() {
    let evidence = certified().await;
    let clock = OwnerPolicyClock::new().unwrap();
    let original = clock.new_origin();
    let replacement = clock.new_origin();
    let time = original.capture().unwrap();
    let invocation = evidence.action().message.invocation().unwrap();
    let transition = CarrierTransition {
        source: evidence.clone(),
        update: tilo_actions::InvocationUpdate::Progress {
            invocation,
            sequence: 1,
            phase: "running".into(),
            completed: None,
            total: None,
            message: None,
        },
    };
    let (queue, mut receiver) = PeerActionQueue::channel();
    let deadline = Instant::now() + std::time::Duration::from_secs(4);
    let pending = queue
        .submit_transition_with_policy_time(evidence.clone(), transition, deadline, time.clone())
        .unwrap();
    let request = receiver.recv().await.unwrap();
    assert_eq!(request.poll, Some(invocation));
    assert!(
        request
            .policy_time()
            .is_some_and(|stored| original.owns(stored) && !replacement.owns(stored))
    );
    drop(original);
    assert!(request.policy_time().unwrap().now_ns().is_err());
    assert!(pending.policy_time().unwrap().now_ns().is_err());
    let legacy = queue.submit_certified(evidence, deadline).unwrap();
    let request = receiver.recv().await.unwrap();
    assert!(request.policy_time().is_none());
    assert!(legacy.policy_time().is_none());
}
