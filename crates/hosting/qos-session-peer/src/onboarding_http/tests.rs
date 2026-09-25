use super::*;
use crate::{committed_authority::CommittedAuthority, https::ingress_channel};
use ed25519_dalek::{Signer, SigningKey};
use tilo_relay::delivery_v3::{
    media_wire::split_signature,
    onboarding_wire::{OnboardingRequest, OnboardingScope},
};
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

struct Fixture {
    _dir: tempfile::TempDir,
    owner: SessionOwner,
    current: CommittedAuthority,
    registry: OnboardingRegistry,
    ingress: crate::https::Ingress,
    device: SigningKey,
    certificate: DeviceCert,
    now: u64,
    origin: String,
}
impl Fixture {
    fn new() -> Self {
        Self::at_origin("https://localhost:9443")
    }
    fn at_origin(origin: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
        owner.activate_delivery().unwrap();
        let now = tilo_relay::clock::now_ns();
        owner
            .install_delivery_transport(origin, 0, at(now))
            .unwrap();
        let current = CommittedAuthority::new(&owner);
        let root = SigningKey::from_bytes(&[131; 32]);
        let device = SigningKey::from_bytes(&[132; 32]);
        let mut certificate = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Independent invited person".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now - 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut certificate, &root).unwrap();
        assert_ne!(
            certificate.user_public_key,
            owner.peer().authority().user_public_key
        );
        let (ingress, _receiver) = ingress_channel();
        let registry = OnboardingRegistry::new([133; 16], ingress.host_bytes()).unwrap();
        Self {
            _dir: dir,
            owner,
            current,
            registry,
            ingress,
            device,
            certificate,
            now,
            origin: origin.into(),
        }
    }
    fn wire(&self, nonce: [u8; 32]) -> Vec<u8> {
        self.wire_for(
            &JoinTransportRequest::Inspect {
                signed_invitation: vec![1, 2, 3],
            },
            nonce,
        )
    }
    fn wire_for(&self, inner: &JoinTransportRequest, nonce: [u8; 32]) -> Vec<u8> {
        let body = postcard::to_stdvec(inner).unwrap();
        let certificate = postcard::to_stdvec(&self.certificate).unwrap();
        let mut request = OnboardingRequest {
            version: 3,
            operation: match inner {
                JoinTransportRequest::Inspect { .. } => OnboardingOperation::Inspect,
                JoinTransportRequest::Challenge(_) => OnboardingOperation::Challenge,
                JoinTransportRequest::Complete(_) => OnboardingOperation::Complete,
                JoinTransportRequest::MembershipChallenge(_) => {
                    OnboardingOperation::MembershipChallenge
                }
                JoinTransportRequest::MembershipComplete(_) => {
                    OnboardingOperation::MembershipComplete
                }
            },
            scope: OnboardingScope {
                origin: &self.origin,
                host: self.owner.host_certificate().device_public_key,
                workspace: self.owner.workspace_id(),
                root: self.owner.peer().authority().user_public_key,
                room: self.owner.peer().room().parse().unwrap(),
            },
            certificate: &certificate,
            nonce,
            issued_at_ns: self.now,
            expires_at_ns: self.now + 60_000_000_000,
            body_digest: *blake3::hash(&body).as_bytes(),
            body: &body,
            signature: [[0; 32]; 2],
        };
        request.signature = split_signature(
            self.device
                .sign(&request.signing_digest().unwrap())
                .to_bytes(),
        );
        request.encode().unwrap()
    }
    async fn call(
        &mut self,
        service: &crate::collaboration::CollaborationService,
        inner: JoinTransportRequest,
        nonce: u8,
    ) -> qos_peer_state::join::JoinTransportResponse {
        use crate::https::onboarding_tests::request;
        use tilo_relay::delivery_v3::onboarding_wire::*;
        let wire = self.wire_for(&inner, [nonce; 32]);
        let original = OnboardingRequest::decode(&wire).unwrap();
        let (buffered, probe) = request(&self.ingress, original.operation.path(), wire.clone());
        let mut exchange = {
            let view = self.current.read().unwrap();
            self.registry
                .admit(&self.owner, view.view(), buffered, self.now)
                .unwrap()
        };
        let outcome = self
            .current
            .with_owner(&mut self.owner, |owner| {
                exchange.invoke(owner, service, self.now)
            })
            .unwrap()
            .unwrap();
        let view = self.current.read().unwrap();
        assert_eq!(
            view.view().generation(),
            self.owner.committed_generation(),
            "actual committed authority published before reply"
        );
        self.registry
            .finish(
                exchange,
                &self.owner,
                view.view(),
                outcome.response,
                self.now,
            )
            .unwrap();
        drop(view);
        let (status, encoded) = probe.receive().await;
        assert_eq!(status, hyper::StatusCode::OK);
        let response = OnboardingResponse::decode(&encoded).unwrap();
        response.verify_host_signature(&original, self.now).unwrap();
        let inner = match response.body_kind {
            ReplyBodyKind::InlineLegacy => response.body.to_vec(),
            ReplyBodyKind::HandoffManifest => {
                let manifest = HandoffManifest::decode(response.body).unwrap();
                manifest
                    .verify_host_signature(
                        original.scope,
                        self.certificate.device_public_key,
                        original.request_digest().unwrap(),
                        original.nonce,
                        self.now,
                    )
                    .unwrap();
                self.hydrate(&manifest).await
            }
        };
        postcard::from_bytes(&inner).unwrap()
    }
    async fn hydrate(
        &mut self,
        manifest: &tilo_relay::delivery_v3::onboarding_wire::HandoffManifest<'_>,
    ) -> Vec<u8> {
        use crate::https::onboarding_tests::request;
        use tilo_relay::delivery_v3::onboarding_wire::*;
        let mut bytes = Vec::new();
        while bytes.len() < manifest.total_bytes as usize {
            let remaining = manifest.total_bytes as usize - bytes.len();
            let mut fetch = HandoffRequest {
                version: 1,
                manifest_digest: manifest.manifest_digest().unwrap(),
                device: self.certificate.device_public_key,
                offset: bytes.len() as u64,
                length: remaining.min(MAX_HANDOFF_RANGE_BYTES) as u32,
                nonce: *blake3::hash(
                    &[
                        manifest.manifest_digest().unwrap().as_slice(),
                        bytes.len().to_le_bytes().as_slice(),
                    ]
                    .concat(),
                )
                .as_bytes(),
                issued_at_ns: self.now,
                expires_at_ns: self.now + 20_000_000_000,
                signature: [[0; 32]; 2],
            };
            fetch.signature = split_signature(
                self.device
                    .sign(&fetch.signing_digest().unwrap())
                    .to_bytes(),
            );
            let (buffered, probe) =
                request(&self.ingress, HANDOFF_CHUNKS_PATH, fetch.encode().unwrap());
            {
                let view = self.current.read().unwrap();
                self.registry
                    .chunk(&self.owner, view.view(), buffered, self.now)
                    .unwrap();
            }
            let (status, encoded) = probe.receive().await;
            assert_eq!(status, hyper::StatusCode::OK);
            let chunk = HandoffChunk::decode(&encoded).unwrap();
            chunk
                .verify_host_signature(&manifest, &fetch, self.now)
                .unwrap();
            assert_eq!(chunk.offset, bytes.len() as u64);
            bytes.extend_from_slice(chunk.body);
        }
        assert_eq!(blake3::hash(&bytes).as_bytes(), &manifest.content_digest);
        let mut offset = 0;
        for segment in &manifest.segments {
            let end = offset + segment.length as usize;
            assert_eq!(
                blake3::hash(&bytes[offset..end]).as_bytes(),
                &segment.digest
            );
            offset = end;
        }
        assert_eq!(offset, bytes.len());
        bytes
    }
}

#[cfg(feature = "media")]
#[path = "tests/https.rs"]
mod https;
fn at(now: u64) -> web_time::SystemTime {
    web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now)
}

#[test]
fn onboarding_possession_authenticates_distinct_root_without_minting_membership() {
    let mut f = Fixture::new();
    let body = f.wire([1; 32]);
    let view = f.current.read().unwrap();
    let admitted = f
        .registry
        .verify(
            &f.owner,
            view.view(),
            OnboardingOperation::Inspect.path(),
            &body,
            f.now,
        )
        .unwrap();
    assert_eq!(admitted.certificate, f.certificate);
    assert!(matches!(
        admitted.request,
        Some(JoinTransportRequest::Inspect { .. })
    ));
    assert!(
        f.owner
            .peer()
            .permissions(f.certificate.device_public_key, at(f.now))
            .is_err()
    );
    assert!(
        f.registry
            .verify(
                &f.owner,
                view.view(),
                OnboardingOperation::Inspect.path(),
                &body,
                f.now
            )
            .is_err(),
        "same possession nonce is one-use"
    );
}

#[test]
fn onboarding_rejects_wrong_route_signature_and_expired_proof() {
    let mut f = Fixture::new();
    let body = f.wire([2; 32]);
    let view = f.current.read().unwrap();
    assert!(
        f.registry
            .verify(
                &f.owner,
                view.view(),
                OnboardingOperation::Challenge.path(),
                &body,
                f.now
            )
            .is_err()
    );
    let mut bad = OnboardingRequest::decode(&body).unwrap();
    bad.signature[0][0] ^= 1;
    assert!(
        f.registry
            .verify(
                &f.owner,
                view.view(),
                OnboardingOperation::Inspect.path(),
                &bad.encode().unwrap(),
                f.now
            )
            .is_err()
    );
    assert!(
        f.registry
            .verify(
                &f.owner,
                view.view(),
                OnboardingOperation::Inspect.path(),
                &body,
                f.now + 60_000_000_000
            )
            .is_err()
    );
}

#[test]
fn onboarding_signed_scope_substitution_and_current_host_revocation_fail_closed() {
    let mut f = Fixture::new();
    let body = f.wire([6; 32]);
    for changed in 0..5 {
        let mut wire = OnboardingRequest::decode(&body).unwrap();
        match changed {
            0 => wire.scope.origin = "https://localhost:9444",
            1 => wire.scope.host = [19; 32],
            2 => wire.scope.root = [20; 32],
            3 => wire.scope.workspace += 1,
            _ => wire.scope.room = uuid::Uuid::from_bytes([21; 16]),
        }
        wire.signature = split_signature(f.device.sign(&wire.signing_digest().unwrap()).to_bytes());
        let view = f.current.read().unwrap();
        assert!(
            f.registry
                .verify(
                    &f.owner,
                    view.view(),
                    OnboardingOperation::Inspect.path(),
                    &wire.encode().unwrap(),
                    f.now
                )
                .is_err()
        );
    }
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: f.owner.identity.root.verifying_key().to_bytes(),
        revoked_device_public_key: f.owner.host_certificate().device_public_key,
        revoked_at_ns: f.now,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &f.owner.identity.root).unwrap();
    f.current
        .with_owner(&mut f.owner, |owner| {
            let mut candidate = owner.peer().clone();
            candidate
                .publish_governed_revocations(&[revoked], &owner.identity.native, at(f.now))
                .unwrap();
            owner
                .commit_mutation_candidate(candidate, owner.directory().to_vec())
                .unwrap();
        })
        .unwrap();
    let view = f.current.read().unwrap();
    assert!(
        f.registry
            .verify(
                &f.owner,
                view.view(),
                OnboardingOperation::Inspect.path(),
                &body,
                f.now
            )
            .is_err()
    );
}

#[tokio::test]
async fn onboarding_pool_pressure_preserves_control_floor_and_reports_unavailable() {
    let mut f = Fixture::new();
    let (buffered, probe) = crate::https::onboarding_tests::request(
        &f.ingress,
        OnboardingOperation::Inspect.path(),
        f.wire([7; 32]),
    );
    let pool = f.ingress.host_bytes();
    let pressure = pool
        .try_acquire_bulk(pool.available_bytes() - crate::host_bytes::CONTROL_RESERVE_BYTES)
        .unwrap();
    let generation = f.owner.committed_generation();
    let view = f.current.read().unwrap();
    assert!(
        f.registry
            .admit(&f.owner, view.view(), buffered, f.now)
            .is_err()
    );
    drop(view);
    let (status, bytes) = probe.receive().await;
    assert_eq!(status, hyper::StatusCode::SERVICE_UNAVAILABLE);
    assert!(pool.available_bytes() >= crate::host_bytes::CONTROL_RESERVE_BYTES);
    assert_eq!(generation, f.owner.committed_generation());
    drop(bytes);
    drop(pressure);
}

#[test]
fn onboarding_borrowed_input_rejects_inflated_nested_lengths_before_owned_decode() {
    // Inspect tag followed by a canonical usize::MAX sequence length with no
    // backing bytes. The borrowing preflight must reject without allocating it.
    let mut bytes = vec![0];
    bytes.extend(postcard::to_stdvec(&usize::MAX).unwrap());
    assert!(input::Request::decode(&bytes).is_err());
    let f = Fixture::new();
    let body = f.wire([3; 32]);
    let wire = OnboardingRequest::decode(&body).unwrap();
    let borrowed = input::Request::decode(wire.body).unwrap();
    let heap = borrowed.heap();
    let owned = borrowed.into_owned();
    assert_eq!(postcard::to_stdvec(&owned).unwrap(), wire.body);
    match owned {
        JoinTransportRequest::Inspect { signed_invitation } => {
            assert_eq!(signed_invitation.capacity(), heap)
        }
        _ => panic!("inspect"),
    }
}

#[tokio::test]
async fn onboarding_charged_request_and_reply_keep_custody_on_cancel_and_completion() {
    use crate::https::onboarding_tests::request;
    use qos_peer_state::join::JoinTransportResponse;
    let mut f = Fixture::new();
    let before = f.ingress.host_bytes().available_bytes();
    let (buffered, probe) = request(
        &f.ingress,
        OnboardingOperation::Inspect.path(),
        f.wire([4; 32]),
    );
    let charged = f.ingress.host_bytes().available_bytes();
    assert!(charged < before);
    let view = f.current.read().unwrap();
    let mut exchange = f
        .registry
        .admit(&f.owner, view.view(), buffered, f.now)
        .unwrap();
    probe.cancel();
    assert!(exchange.is_cancelled());
    assert!(
        f.ingress.host_bytes().available_bytes() < before,
        "cancelled caller cannot retire native input still owned by exchange"
    );
    drop(exchange);
    assert_eq!(f.ingress.host_bytes().available_bytes(), before);
    drop(probe);
    let (buffered, probe) = request(
        &f.ingress,
        OnboardingOperation::Inspect.path(),
        f.wire([5; 32]),
    );
    let mut exchange = f
        .registry
        .admit(&f.owner, view.view(), buffered, f.now)
        .unwrap();
    drop(view);
    let service = crate::collaboration::CollaborationService::new(vec![]);
    let outcome = f
        .current
        .with_owner(&mut f.owner, |owner| {
            exchange.invoke(owner, &service, f.now)
        })
        .unwrap()
        .unwrap();
    assert!(!outcome.reconcile);
    let view = f.current.read().unwrap();
    f.registry
        .finish(exchange, &f.owner, view.view(), outcome.response, f.now)
        .unwrap();
    drop(view);
    let (status, bytes) = probe.receive().await;
    assert_eq!(status, hyper::StatusCode::OK);
    assert!(
        f.ingress.host_bytes().available_bytes() < before,
        "undrained encoded reply retains request charge"
    );
    let reply =
        tilo_relay::delivery_v3::onboarding_wire::OnboardingResponse::decode(&bytes).unwrap();
    reply
        .verify_host_signature(&OnboardingRequest::decode(&f.wire([5; 32])).unwrap(), f.now)
        .unwrap();
    assert!(matches!(
        postcard::from_bytes::<JoinTransportResponse>(reply.body).unwrap(),
        JoinTransportResponse::Error { code: 403, .. }
    ));
    drop(bytes);
    assert_eq!(f.ingress.host_bytes().available_bytes(), before);
}

#[tokio::test]
async fn onboarding_five_operations_use_real_distinct_person_join_and_current_bootstrap() {
    use crate::collaboration::{CollaborationService, IssueInvitationRequest};
    use qos_peer_state::join::*;
    let mut f = Fixture::new();
    let service = CollaborationService::new(vec![]);
    let administrator = f.owner.peer().authority().device_public_key;
    let invitation = f
        .current
        .with_owner(&mut f.owner, |owner| {
            service.issue(
                owner,
                administrator,
                IssueInvitationRequest {
                    request_id: [14; 32],
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(f.certificate.user_public_key),
                    invitee_name: Some("Distinct person".into()),
                    expires_at_ns: f.now + 120_000_000_000,
                },
                f.now,
            )
        })
        .unwrap()
        .unwrap();
    assert!(matches!(
        f.call(
            &service,
            JoinTransportRequest::Inspect {
                signed_invitation: invitation.signed_invitation.clone()
            },
            14
        )
        .await,
        JoinTransportResponse::Inspection(_)
    ));
    let JoinTransportResponse::Challenge(challenge) = f
        .call(
            &service,
            JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: invitation.signed_invitation,
                device_cert: f.certificate.clone(),
                request_id: [15; 32],
            }),
            15,
        )
        .await
    else {
        panic!("actual challenge")
    };
    let proof = sign_join_proof(challenge.challenge, &f.device).unwrap();
    let complete = JoinTransportRequest::Complete(JoinCompleteRequest { proof });
    let JoinTransportResponse::Complete(joined) = f.call(&service, complete.clone(), 16).await
    else {
        panic!("actual join")
    };
    assert_eq!(joined.device_cert, f.certificate);
    assert!(
        f.owner
            .peer()
            .permissions(f.certificate.device_public_key, at(f.now))
            .is_ok()
    );
    assert_eq!(
        joined.governance_messages,
        f.owner
            .outgoing()
            .iter()
            .map(|v| v.bytes.to_vec())
            .collect::<Vec<_>>()
    );
    // Lost response retry changes only transport possession, preserving the
    // existing durable control ID and exact inner proof.
    let generation = f.owner.committed_generation();
    let JoinTransportResponse::Complete(retry) = f.call(&service, complete, 17).await else {
        panic!("idempotent real join")
    };
    assert_eq!(retry, joined);
    assert_eq!(f.owner.committed_generation(), generation);
    let JoinTransportResponse::MembershipChallenge(challenge) = f
        .call(
            &service,
            JoinTransportRequest::MembershipChallenge(MembershipChallengeRequest {
                room: f.owner.peer().room().into(),
                workspace_id: f.owner.workspace_id(),
                device_cert: f.certificate.clone(),
                request_id: [18; 32],
            }),
            18,
        )
        .await
    else {
        panic!("current membership challenge")
    };
    let proof = sign_membership_proof(challenge.challenge, &f.device).unwrap();
    let JoinTransportResponse::MembershipComplete(current) = f
        .call(
            &service,
            JoinTransportRequest::MembershipComplete(MembershipCompleteRequest { proof }),
            19,
        )
        .await
    else {
        panic!("current membership completion")
    };
    assert_eq!(
        current.governance_messages,
        f.owner
            .outgoing()
            .iter()
            .map(|v| v.bytes.to_vec())
            .collect::<Vec<_>>()
    );
    let path = f.owner.root().to_path_buf();
    drop(f.owner);
    let reopened = SessionOwner::open(&path).unwrap();
    assert!(
        reopened
            .peer()
            .permissions(f.certificate.device_public_key, at(f.now))
            .is_ok()
    );
}

fn large_publication(f: &mut Fixture) {
    use rand::{RngCore, SeedableRng};
    use ui::automerge::transaction::Transactable;
    let mut input = vec![0u8; 1_200_000];
    rand::rngs::StdRng::seed_from_u64(4421).fill_bytes(&mut input);
    let text = hex::encode(input);
    f.current
        .with_owner(&mut f.owner, |owner| {
            crate::owner::mutate_owned_record_for_test(owner, "so.tilo.chat", "", |doc| {
                doc.put(ui::automerge::ROOT, "onboarding_handoff_fixture", text)
                    .unwrap();
            })
        })
        .unwrap()
        .unwrap();
    assert!(
        f.owner
            .outgoing()
            .iter()
            .map(|item| item.bytes.len())
            .sum::<usize>()
            > 1024 * 1024
    );
}

#[tokio::test]
async fn onboarding_large_local_handoff_uses_actual_scoped_pin_and_exact_governance_stream() {
    use crate::{enrollment::EnrollmentAuthority, owner::resources::OwnerOperation};
    use qos_peer_state::enrollment::{ChallengeRequest, decode, sign};
    use tilo_relay::delivery_v3::onboarding_wire::{
        GovernanceBundle, HandoffManifest, HandoffPurpose,
    };
    let mut f = Fixture::new();
    large_publication(&mut f);
    let pool = f.ingress.host_bytes();
    f.owner.attach_data_resources(&pool).unwrap();
    let mut authority = EnrollmentAuthority::default();
    let challenge = authority
        .challenge(
            &f.owner,
            ChallengeRequest {
                device_public_key: hex::encode(f.device.verifying_key().to_bytes()),
                request_id: hex::encode([31; 32]),
            },
            f.now,
        )
        .unwrap();
    let challenge = decode(&challenge.challenge).unwrap();
    let complete = sign(&challenge, &f.device).unwrap();
    let digest = *blake3::hash(&postcard::to_stdvec(&complete).unwrap()).as_bytes();
    let mut scope = f
        .owner
        .prepare_owner_scope(
            &pool,
            OwnerOperation::Enrollment {
                input_len: postcard::to_stdvec(&complete).unwrap().len(),
            },
        )
        .unwrap();
    let (certificate, proof, pin) = f
        .owner
        .with_owner_scope(&mut scope, &f.current, |owner| {
            let (certificate, proof) = authority
                .complete_for_https(owner, complete, f.now)
                .unwrap();
            let pin = f.registry.pin_local(owner).unwrap();
            (certificate, proof, pin)
        })
        .unwrap();
    f.certificate = certificate.clone();
    let metadata = crate::enrollment::bootstrap_metadata(
        &f.owner,
        &certificate,
        serde_json::json!({"unused":true}),
        "http://127.0.0.1:1",
        f.now,
    )
    .unwrap();
    let view = f.current.read().unwrap();
    let encoded = f
        .registry
        .local_bootstrap(
            &f.owner,
            view.view(),
            &certificate,
            proof,
            pin,
            metadata,
            f.now,
        )
        .unwrap();
    drop(view);
    assert!(encoded.len() <= 1024 * 1024);
    let reply: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert!(
        reply["bootstrap"].get("governance_messages").is_none(),
        "oversized inline payload is never emitted first"
    );
    let manifest_bytes: Vec<u8> =
        serde_json::from_value(reply["bootstrap"]["governance_handoff"].clone()).unwrap();
    assert!(manifest_bytes.len() <= 8192);
    let manifest = HandoffManifest::decode(&manifest_bytes).unwrap();
    assert_eq!(manifest.purpose, HandoffPurpose::LocalGovernance);
    manifest
        .verify_host_signature(
            OnboardingScope {
                origin: "https://localhost:9443",
                host: f.owner.host_certificate().device_public_key,
                workspace: f.owner.workspace_id(),
                root: f.owner.peer().authority().user_public_key,
                room: f.owner.peer().room().parse().unwrap(),
            },
            certificate.device_public_key,
            digest,
            challenge.nonce,
            f.now,
        )
        .unwrap();
    let content = f.hydrate(&manifest).await;
    let bundle = GovernanceBundle::decode(&content).unwrap();
    assert_eq!(
        postcard::to_stdvec(&bundle).unwrap(),
        content,
        "range stream is exact canonical shared codec"
    );
    assert_eq!(bundle.items.len(), f.owner.outgoing().len());
    for (item, original) in bundle.items.iter().zip(f.owner.outgoing().iter()) {
        assert_eq!(item.bytes, original.bytes);
        assert_eq!(item.retention, original.kind);
    }
    assert_eq!(
        f.registry.pins.len(),
        1,
        "last-range retries retain bounded backing until timeout"
    );
    use tilo_relay::delivery_v3::onboarding_wire::{HANDOFF_CHUNKS_PATH, HandoffRequest};
    let (offset, length) = f.registry.pins[0].last_range.unwrap();
    let mut fetch = HandoffRequest {
        version: 1,
        manifest_digest: manifest.manifest_digest().unwrap(),
        device: certificate.device_public_key,
        offset,
        length,
        nonce: [61; 32],
        issued_at_ns: f.now,
        expires_at_ns: f.now + 20_000_000_000,
        signature: [[0; 32]; 2],
    };
    fetch.signature = split_signature(f.device.sign(&fetch.signing_digest().unwrap()).to_bytes());
    let before = pool.available_bytes();
    f.registry.maintain(f.now + 31_000_000_000).unwrap();
    assert!(
        f.registry.pins.is_empty(),
        "idle maintenance retires expired custody"
    );
    assert!(pool.available_bytes() > before);
    let (buffered, probe) = crate::https::onboarding_tests::request(
        &f.ingress,
        HANDOFF_CHUNKS_PATH,
        fetch.encode().unwrap(),
    );
    let view = f.current.read().unwrap();
    assert!(
        f.registry
            .chunk(&f.owner, view.view(), buffered, f.now + 31_000_000_000)
            .is_err()
    );
    drop(view);
    let (status, bytes) = probe.receive().await;
    assert_eq!(status, hyper::StatusCode::FORBIDDEN);
    drop(bytes);
    assert!(f.registry.pins.is_empty());
    assert!(
        pool.available_bytes() > before,
        "expired pin releases its own metadata/backing handles"
    );
}

#[tokio::test]
async fn onboarding_invocation_refuses_owner_change_after_possession() {
    use crate::collaboration::{CollaborationService, IssueInvitationRequest};
    let mut f = Fixture::new();
    let (buffered, _probe) = crate::https::onboarding_tests::request(
        &f.ingress,
        OnboardingOperation::Inspect.path(),
        f.wire([72; 32]),
    );
    let mut exchange = {
        let view = f.current.read().unwrap();
        f.registry
            .admit(&f.owner, view.view(), buffered, f.now)
            .unwrap()
    };
    let service = CollaborationService::new(vec![]);
    let administrator = f.owner.peer().authority().device_public_key;
    f.current
        .with_owner(&mut f.owner, |owner| {
            service.issue(
                owner,
                administrator,
                IssueInvitationRequest {
                    request_id: [73; 32],
                    role: qos_peer_state::join::MemberRole::Viewer,
                    recipient_user_public_key: None,
                    invitee_name: None,
                    expires_at_ns: f.now + 60_000_000_000,
                },
                f.now,
            )
        })
        .unwrap()
        .unwrap();
    let generation = f.owner.committed_generation();
    assert!(exchange.invoke(&mut f.owner, &service, f.now).is_err());
    assert_eq!(generation, f.owner.committed_generation());
}

#[tokio::test]
async fn onboarding_large_join_reply_preserves_frozen_response_bytes_through_chunks() {
    use crate::collaboration::{CollaborationService, IssueInvitationRequest};
    use qos_peer_state::join::*;
    let mut f = Fixture::new();
    large_publication(&mut f);
    let service = CollaborationService::new(vec![]);
    let administrator = f.owner.peer().authority().device_public_key;
    let invitation = f
        .current
        .with_owner(&mut f.owner, |owner| {
            service.issue(
                owner,
                administrator,
                IssueInvitationRequest {
                    request_id: [41; 32],
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(f.certificate.user_public_key),
                    invitee_name: None,
                    expires_at_ns: f.now + 120_000_000_000,
                },
                f.now,
            )
        })
        .unwrap()
        .unwrap();
    let JoinTransportResponse::Challenge(challenge) = f
        .call(
            &service,
            JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: invitation.signed_invitation,
                device_cert: f.certificate.clone(),
                request_id: [42; 32],
            }),
            42,
        )
        .await
    else {
        panic!("join challenge")
    };
    let proof = sign_join_proof(challenge.challenge, &f.device).unwrap();
    let complete = JoinTransportRequest::Complete(JoinCompleteRequest { proof });
    let JoinTransportResponse::Complete(bootstrap) = f.call(&service, complete.clone(), 43).await
    else {
        panic!("large actual completion")
    };
    assert_eq!(
        bootstrap.governance_messages,
        f.owner
            .outgoing()
            .iter()
            .map(|item| item.bytes.to_vec())
            .collect::<Vec<_>>()
    );
    assert_eq!(f.registry.pins.len(), 1);
    for nonce in 44..51 {
        assert!(matches!(
            f.call(&service, complete.clone(), nonce).await,
            JoinTransportResponse::Complete(_)
        ));
    }
    assert_eq!(f.registry.pins.len(), 8);
    let existing = f
        .registry
        .pins
        .iter()
        .map(|pin| pin.manifest.clone())
        .collect::<Vec<_>>();
    let wire = f.wire_for(&complete, [51; 32]);
    let (buffered, _probe) = crate::https::onboarding_tests::request(
        &f.ingress,
        OnboardingOperation::Complete.path(),
        wire,
    );
    let mut exchange = {
        let view = f.current.read().unwrap();
        f.registry
            .admit(&f.owner, view.view(), buffered, f.now)
            .unwrap()
    };
    let outcome = f
        .current
        .with_owner(&mut f.owner, |owner| {
            exchange.invoke(owner, &service, f.now)
        })
        .unwrap()
        .unwrap();
    let view = f.current.read().unwrap();
    assert!(
        f.registry
            .finish(exchange, &f.owner, view.view(), outcome.response, f.now)
            .unwrap_err()
            .is::<CapacityUnavailable>()
    );
    assert_eq!(
        f.registry
            .pins
            .iter()
            .map(|pin| pin.manifest.clone())
            .collect::<Vec<_>>(),
        existing,
        "capacity cannot evict a live pin"
    );
    drop(view);
    // A currently removed person cannot fetch even a still-retained signed pin.
    f.current
        .with_owner(&mut f.owner, |owner| {
            service.remove_member(
                owner,
                administrator,
                crate::collaboration::RemoveMemberRequest {
                    request_id: [52; 32],
                    user_public_key: f.certificate.user_public_key,
                },
                f.now,
            )
        })
        .unwrap()
        .unwrap();
    use tilo_relay::delivery_v3::onboarding_wire::{
        HANDOFF_CHUNKS_PATH, HandoffManifest, HandoffRequest,
    };
    let manifest = HandoffManifest::decode(&existing[0]).unwrap();
    let (offset, length) = f.registry.pins[0].last_range.unwrap();
    let mut fetch = HandoffRequest {
        version: 1,
        manifest_digest: manifest.manifest_digest().unwrap(),
        device: f.certificate.device_public_key,
        offset,
        length,
        nonce: [53; 32],
        issued_at_ns: f.now,
        expires_at_ns: f.now + 20_000_000_000,
        signature: [[0; 32]; 2],
    };
    fetch.signature = split_signature(f.device.sign(&fetch.signing_digest().unwrap()).to_bytes());
    let (buffered, probe) = crate::https::onboarding_tests::request(
        &f.ingress,
        HANDOFF_CHUNKS_PATH,
        fetch.encode().unwrap(),
    );
    let view = f.current.read().unwrap();
    assert!(
        f.registry
            .chunk(&f.owner, view.view(), buffered, f.now)
            .is_err()
    );
    drop(view);
    assert_eq!(probe.receive().await.0, hyper::StatusCode::FORBIDDEN);
}

/// An inline join reply between 512 KiB and the inline threshold used to be
/// refused after commit: postcard's growing Vec capacity exceeded the 1 MiB
/// response bound although the encoded length fit, so a returning member's
/// membership completion answered 503 on every retry.
#[tokio::test]
async fn onboarding_mid_size_inline_join_reply_fits_response_capacity() {
    use crate::collaboration::{CollaborationService, IssueInvitationRequest};
    use qos_peer_state::join::*;
    use rand::{RngCore, SeedableRng};
    use ui::automerge::transaction::Transactable;
    let mut f = Fixture::new();
    let mut input = vec![0u8; 700_000];
    rand::rngs::StdRng::seed_from_u64(4471).fill_bytes(&mut input);
    let text = hex::encode(input);
    f.current
        .with_owner(&mut f.owner, |owner| {
            crate::owner::mutate_owned_record_for_test(owner, "so.tilo.chat", "", |doc| {
                doc.put(ui::automerge::ROOT, "mid_size_fixture", text)
                    .unwrap();
            })
        })
        .unwrap()
        .unwrap();
    let service = CollaborationService::new(vec![]);
    let administrator = f.owner.peer().authority().device_public_key;
    let invitation = f
        .current
        .with_owner(&mut f.owner, |owner| {
            service.issue(
                owner,
                administrator,
                IssueInvitationRequest {
                    request_id: [71; 32],
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(f.certificate.user_public_key),
                    invitee_name: None,
                    expires_at_ns: f.now + 120_000_000_000,
                },
                f.now,
            )
        })
        .unwrap()
        .unwrap();
    let JoinTransportResponse::Challenge(challenge) = f
        .call(
            &service,
            JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: invitation.signed_invitation,
                device_cert: f.certificate.clone(),
                request_id: [72; 32],
            }),
            72,
        )
        .await
    else {
        panic!("join challenge")
    };
    let proof = sign_join_proof(challenge.challenge, &f.device).unwrap();
    let response = f
        .call(
            &service,
            JoinTransportRequest::Complete(JoinCompleteRequest { proof }),
            73,
        )
        .await;
    let length = postcard::experimental::serialized_size(&response).unwrap();
    assert!(
        (600 * 1024..1024 * 1024 - 8192).contains(&length),
        "fixture must produce a mid-size inline reply: {length}"
    );
    assert!(matches!(response, JoinTransportResponse::Complete(_)));
    assert!(f.registry.pins.is_empty(), "stays inline, no handoff pin");
}
