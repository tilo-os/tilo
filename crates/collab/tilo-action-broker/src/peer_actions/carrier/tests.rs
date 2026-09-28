#![allow(
    clippy::similar_names,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    clippy::unused_self,
    reason = "fixture keys pair host/governor names, epoch offsets are small constants, and guards stay scoped to their assertions"
)]
use super::*;
use crate::peer_actions::OwnerPolicyClock;
use ed25519_dalek::SigningKey;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_delivery_protocol::delivery_v3::{
    auth::*,
    http_proof::{HttpSessionAdmission, RequestProof, session_binding_digest},
    private_wire::{self, PrivateOperation, PrivateScope},
};
use tilo_delivery_protocol::peer_actions::{ActionMessage, DirectedAction};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use uuid::Uuid;
struct Fixture {
    peer: tilo_peer_state::StatePeer,
    binding: Binding,
    device: DeviceCert,
    host: DeviceCert,
    device_key: SigningKey,
    host_key: SigningKey,
    revocations: RevocationList,
    current: AtomicBool,
}
impl Authority for Fixture {
    fn authorize(
        &self,
        binding: &Binding,
        device: &DeviceCert,
        host: &DeviceCert,
        _: u64,
    ) -> Result<(), AuthError> {
        if !self.current.load(Ordering::SeqCst)
            || binding.workspace != self.binding.workspace
            || device != &self.device
            || host != &self.host
        {
            Err(AuthError::PolicyDenied)
        } else {
            Ok(())
        }
    }
}
impl Fixture {
    fn new(capabilities: u64) -> Self {
        let root = SigningKey::from_bytes(&[10; 32]);
        let device_key = SigningKey::from_bytes(&[11; 32]);
        let host_key = SigningKey::from_bytes(&[12; 32]);
        let cert = |key: &SigningKey, capabilities| {
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: key.verifying_key().to_bytes(),
                device_label: "private HTTP".into(),
                capabilities,
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &root).unwrap();
            cert
        };
        let device = cert(&device_key, DeviceCapabilities::primary());
        let host = cert(&host_key, DeviceCapabilities::host());
        let governor_key = SigningKey::from_bytes(&[13; 32]);
        let governor = cert(&governor_key, DeviceCapabilities::primary());
        let roster = vec![governor.clone(), host.clone(), device.clone()];
        let room = Uuid::from_u128(1).to_string();
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(100);
        let mut peer = tilo_peer_state::StatePeer::new(
            &room,
            governor.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.install_seed(
            tilo_peer_state::SeedState {
                room_changes: tilo_peer_state::seed_room(&room, &governor, &roster).unwrap(),
                documents: vec![],
            },
            &governor_key,
            now,
        )
        .unwrap();
        let binding = Binding {
            workspace: tilo_relay::room_workspace_id(&Uuid::from_u128(1).to_string()).unwrap(),
            workspace_root: root.verifying_key().to_bytes(),
            device: device.device_public_key,
            device_root: device.user_public_key,
            host: host.device_public_key,
            host_root: host.user_public_key,
            origin: "https://host.test".into(),
            descriptor_generation: 1,
            descriptor_digest: [8; 32],
            server_boot_id: [9; 16],
            carrier: Carrier::Https,
            requested_capabilities: capabilities,
            mandatory_capabilities: capabilities,
        };
        Self {
            peer,
            binding,
            device,
            host,
            device_key,
            host_key,
            revocations: RevocationList::new(),
            current: AtomicBool::new(true),
        }
    }
    fn context(&self) -> VerificationContext<'_> {
        VerificationContext {
            binding: &self.binding,
            capabilities: CapabilityPolicy {
                supported: 31,
                allowed: 31,
            },
            credentials: Credentials {
                device: &self.device,
                host: &self.host,
                device_revocations: &self.revocations,
                host_revocations: &self.revocations,
            },
            authority: self,
            now_ns: 100,
        }
    }
    fn session(&self) -> (HttpSessionAdmission, [u8; 16], [u8; 32]) {
        let context = self.context();
        let (challenge, server) = Challenge::issue(&context, [2; 32], &self.host_key).unwrap();
        let client = verify_server(
            &ClientExpectations {
                scope: ClientScope::from(&self.binding),
                capabilities: context.capabilities,
                credentials: context.credentials,
                authority: self,
                now_ns: 100,
            },
            [2; 32],
            &server,
        )
        .unwrap()
        .client_proof(&self.device_key)
        .unwrap();
        let session = challenge.finish(&client, &context).unwrap();
        let id = session.session_id();
        let digest = session_binding_digest(
            session.binding(),
            id,
            session.capabilities(),
            session.expires_at_ns(),
        )
        .unwrap();
        (
            HttpSessionAdmission::new(session, &context).unwrap(),
            id,
            digest,
        )
    }
    fn action(&self) -> DirectedAction {
        DirectedAction {
            room: Uuid::from_u128(1),
            request_id: Uuid::from_u128(2),
            source: self.binding.device,
            target: self.peer.authority().device_public_key,
            message: ActionMessage::Status {
                invocation: tilo_actions::InvocationId(Uuid::from_u128(3)),
            },
        }
    }
    fn body(&self, action: &DirectedAction) -> Vec<u8> {
        private_wire::encode_request(
            PrivateOperation::Status,
            PrivateScope {
                room: action.room,
                source: action.source,
                target: action.target,
            },
            action,
        )
        .unwrap()
    }
}

fn certified() -> CertifiedAction {
    // Real certificate/challenge/device request signatures. This fixture proves
    // queue custody only; it does not qualify a production owner installation.
    let f = Fixture::new(2);
    let action = f.action();
    let body = f.body(&action);
    let request = tilo_delivery_protocol::delivery_v3::http_proof::Request::new(
        "POST",
        PrivateOperation::Status.path(),
        &body,
    )
    .unwrap();
    let (mut session, id, digest) = f.session();
    let proof = RequestProof::sign(id, digest, &request, 100, 1000, &f.device_key).unwrap();
    CertifiedAction::from_http_request(
        session.admit(&proof, &request, &f.context()).unwrap(),
        &f.peer,
    )
    .unwrap()
}
#[tokio::test]
async fn owner_policy_clock_carrier_retains_exact_origin_and_original_capacity() {
    let evidence = certified();
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
    let evidence = certified();
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
