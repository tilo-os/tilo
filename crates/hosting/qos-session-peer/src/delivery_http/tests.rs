use super::*;
use crate::owner::delivery::NativeDeliveryAuthority;
use tilo_relay::delivery_v3::{auth, descriptor::GenerationExpectation};
use tilo_user_identity::RevocationList;
use web_time::{Duration, SystemTime};

pub(crate) struct Fixture {
    _dir: tempfile::TempDir,
    pub(crate) owner: SessionOwner,
    pub(crate) current: CommittedAuthority,
    pub(crate) now: u64,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
        owner.activate_delivery().unwrap();
        let now = tilo_relay::clock::now_ns();
        owner
            .install_delivery_transport("https://localhost:9443", 0, at(now))
            .unwrap();
        let current = CommittedAuthority::new(&owner);
        Self {
            _dir: dir,
            owner,
            current,
            now,
        }
    }
    pub(crate) fn request(&self, nonce: [u8; 32]) -> Vec<u8> {
        postcard::to_stdvec(&(
            3_u16,
            self.owner.identity.native_cert.device_public_key,
            nonce,
            0_u64,
            0_u64,
        ))
        .unwrap()
    }
    pub(crate) fn client(&self, proof: &auth::ServerProof, nonce: [u8; 32]) -> auth::ClientProof {
        let guard = self.current.read().unwrap();
        let descriptor = self
            .owner
            .verified_delivery_transport(
                guard.view(),
                at(self.now),
                crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
            )
            .unwrap();
        let authority = self.owner.transport_authority(
            guard.peer(),
            GenerationExpectation::Current {
                generation: descriptor.generation(),
                descriptor_digest: descriptor.descriptor_digest(),
            },
            at(self.now),
            crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
        );
        let scope = descriptor
            .client_scope(
                &authority,
                &self.owner.identity.native_cert,
                auth::Carrier::Https,
                0,
                0,
            )
            .unwrap();
        let native = NativeDeliveryAuthority {
            view: guard.view(),
            workspace: self.owner.workspace_id(),
            host: self.owner.host_certificate(),
        };
        let revocations = RevocationList::new();
        auth::verify_server(
            &auth::ClientExpectations {
                scope,
                capabilities: auth::CapabilityPolicy {
                    supported: 0,
                    allowed: 0,
                },
                credentials: auth::Credentials {
                    device: &self.owner.identity.native_cert,
                    host: self.owner.host_certificate(),
                    device_revocations: &revocations,
                    host_revocations: &revocations,
                },
                authority: &native,
                now_ns: self.now,
            },
            nonce,
            proof,
        )
        .unwrap()
        .client_proof(&self.owner.identity.native)
        .unwrap()
    }
    pub(crate) fn handshake(
        &self,
        registry: &mut HttpSessions,
        nonce: [u8; 32],
    ) -> ([u8; 16], [u8; 32]) {
        let server = auth::ServerProof::decode(
            &registry
                .challenge(&self.owner, &self.current, &self.request(nonce), self.now)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            server.transcript.binding.host,
            self.owner.host_certificate().device_public_key
        );
        assert_ne!(
            server.transcript.binding.host,
            self.owner.identity.native_cert.device_public_key
        );
        let client = self.client(&server, nonce);
        let bytes = registry
            .finish(
                &self.owner,
                &self.current,
                &client.encode().unwrap(),
                self.now,
            )
            .unwrap();
        let ready: (u16, [u8; 16], [u8; 32], u64, u64) = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(ready.0, 3);
        assert_eq!(ready.1, server.transcript.session_id);
        assert_eq!(ready.3, server.transcript.expires_at_ns);
        assert_eq!(ready.4, 0);
        assert!(
            registry
                .finish(
                    &self.owner,
                    &self.current,
                    &client.encode().unwrap(),
                    self.now
                )
                .is_err()
        );
        (ready.1, ready.2)
    }
}
fn at(ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_nanos(ns)
}
#[test]
fn installed_registry_uses_real_host_custody_and_one_atomic_replay_owner() {
    let f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [3; 32]);
    let request = Request::new("POST", "/peer/v3/media/grant/challenge", b"exact body").unwrap();
    let proof = RequestProof::sign(
        id,
        digest,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let wrong = Request::new("POST", "/peer/v3/media/grant/challenge", b"changed body").unwrap();
    assert!(
        registry
            .admit(&f.owner, &f.current, &proof, &wrong, f.now)
            .is_err()
    );
    assert_eq!(
        registry
            .admit(&f.owner, &f.current, &proof, &request, f.now)
            .unwrap()
            .session()
            .device_certificate(),
        &f.owner.identity.native_cert
    );
    assert!(
        registry
            .admit(&f.owner, &f.current, &proof, &request, f.now)
            .is_err()
    );
    let mut restarted = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    assert!(
        restarted
            .admit(&f.owner, &f.current, &proof, &request, f.now)
            .is_err()
    );
}
#[test]
fn registry_bounds_pending_live_expiry_and_clock_rollback() {
    let f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    for n in 1..=4 {
        registry
            .challenge(&f.owner, &f.current, &f.request([n; 32]), f.now)
            .unwrap();
    }
    assert!(
        registry
            .challenge(&f.owner, &f.current, &f.request([5; 32]), f.now)
            .is_err()
    );
    assert!(
        registry
            .challenge(
                &f.owner,
                &f.current,
                &f.request([5; 32]),
                f.now + 30_000_000_000
            )
            .is_ok()
    );
    assert!(
        registry
            .challenge(&f.owner, &f.current, &f.request([6; 32]), f.now)
            .is_err()
    );
    let mut live = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    f.handshake(&mut live, [11; 32]);
    f.handshake(&mut live, [12; 32]);
    assert!(
        live.challenge(&f.owner, &f.current, &f.request([13; 32]), f.now)
            .is_err()
    );
    let mut forged = f.request([8; 32]);
    forged.push(0);
    assert!(
        registry
            .challenge(&f.owner, &f.current, &forged, f.now + 30_000_000_000)
            .is_err()
    );
}
#[test]
fn registry_rechecks_committed_authority_and_fences_descriptor_replacement() {
    let mut f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [3; 32]);
    let request = Request::new("POST", "/peer/v3/media/presence", b"presence").unwrap();
    let proof = RequestProof::sign(
        id,
        digest,
        &request,
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    f.current
        .with_owner(&mut f.owner, |owner| {
            owner.install_delivery_transport("https://localhost:9444", 0, at(f.now))
        })
        .unwrap()
        .unwrap();
    assert!(
        registry
            .admit(&f.owner, &f.current, &proof, &request, f.now)
            .is_err()
    );
    assert!(
        registry
            .challenge(&f.owner, &f.current, &f.request([4; 32]), f.now)
            .is_err()
    );
}

#[test]
fn bootstrap_requires_enrolled_device_and_attests_exact_store_under_governor_custody() {
    use ed25519_dalek::{Signer, VerifyingKey};
    use tilo_relay::delivery_v3::media_wire::{
        BootstrapRequest, BootstrapResponse, join_signature, split_signature,
    };
    let f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let descriptor = {
        let guard = f.current.read().unwrap();
        f.owner
            .verified_delivery_transport(
                guard.view(),
                at(f.now),
                crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
            )
            .unwrap()
    };
    let mut request = BootstrapRequest {
        version: 1,
        device: f.owner.identity.native_cert.device_public_key,
        nonce: [3; 32],
        workspace: f.owner.workspace_id(),
        root: f.owner.peer().authority().user_public_key,
        room: f.owner.peer().room().into(),
        candidate_digest: descriptor.descriptor_digest(),
        signature: [[0; 32]; 2],
    };
    request.signature = split_signature(
        f.owner
            .identity
            .native
            .sign(&request.signing_digest().unwrap())
            .to_bytes(),
    );
    let before = f.owner.committed_generation();
    let bytes = registry
        .bootstrap(&f.owner, &f.current, &request.encode().unwrap(), f.now)
        .unwrap();
    let response = BootstrapResponse::decode(&bytes).unwrap();
    assert_eq!(response.request, request);
    assert_eq!(response.descriptor_digest, descriptor.descriptor_digest());
    assert_eq!(response.generation, descriptor.generation());
    assert_eq!(
        response.host_certificate,
        postcard::to_stdvec(f.owner.host_certificate()).unwrap()
    );
    assert_eq!(
        response.hint,
        postcard::to_stdvec(f.owner.stored_delivery_hint().unwrap()).unwrap()
    );
    VerifyingKey::from_bytes(&f.owner.identity.native_cert.device_public_key)
        .unwrap()
        .verify_strict(
            &response.signing_digest().unwrap(),
            &ed25519_dalek::Signature::from_bytes(&join_signature(response.signature)),
        )
        .unwrap();
    assert_eq!(f.owner.committed_generation(), before);
    for mutate in 0..4 {
        let mut bad = request.clone();
        match mutate {
            0 => bad.device = [42; 32],
            1 => bad.nonce = [8; 32],
            2 => bad.workspace += 1,
            _ => bad.candidate_digest = [5; 32],
        };
        assert!(
            registry
                .bootstrap(&f.owner, &f.current, &bad.encode().unwrap(), f.now)
                .is_err()
        );
    }
}

#[cfg(feature = "media")]
#[test]
fn authenticated_native_control_grant_setup_presence_refresh_close_and_leave() {
    use crate::{
        media_service::MediaService,
        room_media_policy::tests::{CALL, activate},
    };
    use ed25519_dalek::{Signer, VerifyingKey};
    use qos_moq::relay::RelayAuthority;
    use tilo_relay::delivery_v3::media_wire::*;
    let mut f = Fixture::new();
    f.current
        .with_owner(&mut f.owner, |owner| {
            activate(owner);
            Ok::<(), anyhow::Error>(())
        })
        .unwrap()
        .unwrap();
    let service = MediaService::for_owner(
        &f.owner,
        f.current.clone(),
        "https://localhost:9443/moq/v21/voice",
    )
    .unwrap();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [6; 32]);
    let invoke = |registry: &mut HttpSessions, command: ControlCommand| {
        let body = ControlRequest {
            version: 1,
            command: command.clone(),
        }
        .encode()
        .unwrap();
        let request =
            Request::new("POST", crate::media_control::route(&command).path(), &body).unwrap();
        let proof = RequestProof::sign(
            id,
            digest,
            &request,
            f.now,
            f.now + 30_000_000_000,
            &f.owner.identity.native,
        )
        .unwrap();
        let verified = registry
            .admit(&f.owner, &f.current, &proof, &request, f.now)
            .unwrap();
        service.control(&f.owner, &verified)
    };
    let check = |result: ControlResult| {
        let ControlResult::Challenge(c) = result else {
            panic!("expected challenge")
        };
        VerifyingKey::from_bytes(&f.owner.identity.native_cert.device_public_key)
            .unwrap()
            .verify_strict(
                &c.signing_digest().unwrap(),
                &ed25519_dalek::Signature::from_bytes(&join_signature(c.signature)),
            )
            .unwrap();
        assert_eq!(c.context.delivery_session_id, id);
        assert_eq!(
            c.context.delivery_host,
            f.owner.host_certificate().device_public_key
        );
        c
    };
    let mut intent = GrantRequest {
        device: f.owner.identity.native_cert.device_public_key,
        room: f.owner.peer().room().into(),
        call_id: hex::encode(CALL),
        participation_id: [9; 32],
        generation: 0,
        directions: MediaDirections {
            publish: true,
            subscribe: true,
        },
    };
    let first = check(
        invoke(
            &mut registry,
            ControlCommand::GrantChallenge {
                request: intent.clone(),
                operation: GrantOperation::Join,
            },
        )
        .unwrap(),
    );
    let repeat = check(
        invoke(
            &mut registry,
            ControlCommand::GrantChallenge {
                request: intent.clone(),
                operation: GrantOperation::Join,
            },
        )
        .unwrap(),
    );
    assert_eq!(first.context.request, repeat.context.request);
    assert_eq!(first.context.request.generation, 1);
    let complete = |registry: &mut HttpSessions, c: &SignedChallenge| {
        let ChallengeTranscript::Native(inner) = &c.transcript else {
            panic!()
        };
        invoke(
            registry,
            ControlCommand::GrantComplete {
                nonce: inner.nonce,
                signature: split_signature(
                    f.owner
                        .identity
                        .native
                        .sign(&c.transcript.signing_digest().unwrap())
                        .to_bytes(),
                ),
            },
        )
        .unwrap()
    };
    let ControlResult::Granted { grant } = complete(&mut registry, &first) else {
        panic!("expected native grant")
    };
    let relay = check(invoke(&mut registry, ControlCommand::RelayChallenge { grant }).unwrap());
    let ChallengeTranscript::Relay(inner) = &relay.transcript else {
        panic!()
    };
    let sid = inner.session_id;
    let setup = crate::media_service::encode_relay_proof(
        sid,
        inner.nonce,
        f.owner
            .identity
            .native
            .sign(&relay.transcript.signing_digest().unwrap())
            .to_bytes(),
    )
    .unwrap();
    let permit = service.admit(&setup).unwrap();
    assert!(permit.publication().is_some());
    assert!(permit.subscriptions().is_empty());
    let ControlResult::Present(presence) =
        invoke(&mut registry, ControlCommand::Presence { session_id: sid }).unwrap()
    else {
        panic!()
    };
    assert!(presence.context.inventory.namespaces.is_empty());
    VerifyingKey::from_bytes(&f.owner.identity.native_cert.device_public_key)
        .unwrap()
        .verify_strict(
            &presence.signing_digest().unwrap(),
            &ed25519_dalek::Signature::from_bytes(&join_signature(presence.signature)),
        )
        .unwrap();
    intent.generation = 1;
    let renewal = check(
        invoke(
            &mut registry,
            ControlCommand::GrantChallenge {
                request: intent.clone(),
                operation: GrantOperation::Renew,
            },
        )
        .unwrap(),
    );
    let ControlResult::Granted { grant } = complete(&mut registry, &renewal) else {
        panic!()
    };
    let refresh = check(
        invoke(
            &mut registry,
            ControlCommand::RefreshChallenge {
                session_id: sid,
                grant,
            },
        )
        .unwrap(),
    );
    let ChallengeTranscript::Refresh(inner) = &refresh.transcript else {
        panic!()
    };
    assert!(
        matches!(invoke(&mut registry,ControlCommand::RefreshComplete{session_id:sid,nonce:inner.nonce,signature:split_signature(f.owner.identity.native.sign(&refresh.transcript.signing_digest().unwrap()).to_bytes())}).unwrap(),ControlResult::Refreshed{session_id,..} if session_id==sid)
    );
    assert!(matches!(
        invoke(
            &mut registry,
            ControlCommand::RelayClose { session_id: sid }
        )
        .unwrap(),
        ControlResult::Closed
    ));
    assert!(invoke(&mut registry, ControlCommand::Presence { session_id: sid }).is_err());
    let leave = check(
        invoke(
            &mut registry,
            ControlCommand::GrantChallenge {
                request: intent.clone(),
                operation: GrantOperation::Leave,
            },
        )
        .unwrap(),
    );
    assert!(matches!(
        complete(&mut registry, &leave),
        ControlResult::Left
    ));
    intent.generation = 0;
    assert!(
        invoke(
            &mut registry,
            ControlCommand::GrantChallenge {
                request: intent.clone(),
                operation: GrantOperation::Join
            }
        )
        .is_err()
    );
    intent.participation_id = [10; 32];
    let next = check(
        invoke(
            &mut registry,
            ControlCommand::GrantChallenge {
                request: intent,
                operation: GrantOperation::Join,
            },
        )
        .unwrap(),
    );
    assert_eq!(next.context.request.generation, 2);
}

/// A descriptor renewal keeps live sessions admitted under their original
/// binding (the native host previously answered "unknown session" and forced
/// every browser through a fresh bootstrap every renewal period).
#[test]
fn renewal_keeps_live_sessions_under_their_original_binding() {
    let mut f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, digest) = f.handshake(&mut registry, [3; 32]);
    let old_descriptor = registry.descriptor_digest();
    let old_generation = registry.descriptor_generation;
    // Renew: less than a fifth of the 15-minute lifetime is left.
    let later = f.now + 13 * 60 * 1_000_000_000;
    f.current
        .with_owner(&mut f.owner, |owner| {
            owner.install_delivery_transport("https://localhost:9443", 0, at(later))
        })
        .unwrap()
        .unwrap();
    let mut renewed = HttpSessions::new(&f.owner, &f.current, later).unwrap();
    assert_ne!(renewed.descriptor_digest(), old_descriptor);
    assert!(renewed.descriptor_generation > old_generation);
    renewed.inherit_renewed(registry);
    let request = Request::new("POST", "/peer/v3/media/presence", b"presence").unwrap();
    let proof = RequestProof::sign(
        id,
        digest,
        &request,
        later,
        later + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    let admitted = renewed
        .admit(&f.owner, &f.current, &proof, &request, later)
        .unwrap();
    assert_eq!(
        admitted.session().binding().descriptor_digest,
        old_descriptor
    );
    assert!(
        renewed
            .admit(&f.owner, &f.current, &proof, &request, later)
            .is_err(),
        "replay stays rejected across the renewal"
    );
    // New sessions bind the renewed descriptor, and the retained sessions do
    // not consume the device's current-generation capacity.
    f.now = later;
    let (fresh, _) = f.handshake(&mut renewed, [4; 32]);
    let (second, _) = f.handshake(&mut renewed, [5; 32]);
    assert!(renewed.live.contains_key(&id));
    assert!(renewed.live[&fresh].identity.superseded.is_none());
    assert!(renewed.live[&second].identity.superseded.is_none());
    // The retained session still expires on its own schedule.
    let expired = f.now + 3 * 60 * 1_000_000_000;
    let proof = RequestProof::sign(
        id,
        digest,
        &request,
        expired,
        expired + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap();
    assert!(
        renewed
            .admit(&f.owner, &f.current, &proof, &request, expired)
            .is_err()
    );
    assert!(!renewed.live.contains_key(&id));
}

#[test]
fn renewal_retains_at_most_two_superseded_generations() {
    let f = Fixture::new();
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    let (id, _) = f.handshake(&mut registry, [3; 32]);
    let base = registry.descriptor_generation;
    for (step, retained) in [(1, true), (2, true), (3, false)] {
        let mut next = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
        next.descriptor_generation = base + step;
        next.inherit_renewed(registry);
        assert_eq!(next.live.contains_key(&id), retained, "renewal {step}");
        registry = next;
    }
    // A replacement that is not a renewal of the same installation (e.g. an
    // unchanged or older generation) never carries sessions over.
    let mut registry = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    f.handshake(&mut registry, [6; 32]);
    let mut same = HttpSessions::new(&f.owner, &f.current, f.now).unwrap();
    same.inherit_renewed(registry);
    assert!(same.live.is_empty());
}
