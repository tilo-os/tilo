#![allow(
    clippy::similar_names,
    reason = "paired test fixtures are named after the roles they mirror"
)]
use super::*;
use ed25519_dalek::SigningKey;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_delivery_protocol::delivery_v3::{
    auth::*,
    http_proof::*,
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
        let now = web_time::SystemTime::UNIX_EPOCH
            .checked_add(web_time::Duration::from_nanos(100))
            .unwrap();
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
            workspace: crate::room_workspace_id(&Uuid::from_u128(1).to_string()).unwrap(),
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
    #[expect(
        clippy::unused_self,
        reason = "fixture method keeps call sites uniform across the scenario helpers"
    )]
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
#[test]
fn http_action_factory_consumes_real_signed_admission_and_preserves_exact_request() {
    let f = Fixture::new(2);
    let action = f.action();
    assert_ne!(f.binding.host, action.target);
    let body = f.body(&action);
    let request = Request::new("POST", PrivateOperation::Status.path(), &body).unwrap();
    let (mut session, id, digest) = f.session();
    let proof = RequestProof::sign(id, digest, &request, 100, 1000, &f.device_key).unwrap();
    let admitted = session.admit(&proof, &request, &f.context()).unwrap();
    let certified = CertifiedAction::from_http_request(admitted, &f.peer).unwrap();
    assert_eq!(certified.action(), &action);
    assert_eq!(certified.certificate(), &f.device);
    assert!(session.admit(&proof, &request, &f.context()).is_err());
}
#[test]
fn http_action_factory_rejects_signed_wrong_source_target_room_direction_and_capability() {
    for field in 0..6 {
        let f = Fixture::new(if field == 5 { 1 } else { 2 });
        let mut action = f.action();
        match field {
            0 => action.source = [20; 32],
            1 => action.target = [21; 32],
            2 => action.room = Uuid::from_u128(8),
            _ => {}
        }
        let body = f.body(&action);
        let request = Request::new(
            if field == 3 { "GET" } else { "POST" },
            if field == 4 {
                PrivateOperation::Cancel.path()
            } else {
                PrivateOperation::Status.path()
            },
            &body,
        )
        .unwrap();
        let (mut session, id, digest) = f.session();
        let proof = RequestProof::sign(id, digest, &request, 100, 1000, &f.device_key).unwrap();
        let admitted = session.admit(&proof, &request, &f.context()).unwrap();
        assert!(
            CertifiedAction::from_http_request(admitted, &f.peer).is_err(),
            "field {field}"
        );
    }
}
#[test]
fn http_action_factory_has_no_admission_after_current_authority_withdrawal() {
    let f = Fixture::new(2);
    let body = f.body(&f.action());
    let request = Request::new("POST", PrivateOperation::Status.path(), &body).unwrap();
    let (mut session, id, digest) = f.session();
    let proof = RequestProof::sign(id, digest, &request, 100, 1000, &f.device_key).unwrap();
    f.current.store(false, Ordering::SeqCst);
    assert!(session.admit(&proof, &request, &f.context()).is_err());
}
