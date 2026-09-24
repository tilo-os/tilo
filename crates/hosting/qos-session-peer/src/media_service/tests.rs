use super::*;
use ed25519_dalek::{Signer, SigningKey};
use qos_moq::relay::Direction;
use web_time::SystemTime;

use crate::room_media_policy::tests::{CALL, activate, edit_policy};

fn assert_local_scope_denials(permit: &dyn SessionAuthorization) {
    let own = permit.publication().unwrap();
    let mut foreign = own.to_owned();
    foreign.replace_range(0..1, if &own[..1] == "0" { "1" } else { "0" });
    for (direction, name) in [
        (Direction::Subscribe, own),
        (Direction::Publish, "malformed"),
        (Direction::Publish, foreign.as_str()),
    ] {
        assert!(
            matches!(permit.status(direction, name), AuthorizationStatus::Denied),
            "a definite local scope denial must not become temporary authority contention"
        );
    }
}

#[test]
fn local_scope_denial_precedes_actual_owner_lock_contention() {
    let f = Fixture::new();
    let grant = f.grant(GrantOperation::Join);
    let (_, proof) = f.proof(&grant);
    let permit = f.service.admit(&proof).unwrap();
    let update = f.service.0.source.committed.begin(&f.owner).unwrap();
    assert!(matches!(
        permit.status(Direction::Publish, permit.publication().unwrap()),
        AuthorizationStatus::Unavailable { .. }
    ));
    assert_local_scope_denials(permit.as_ref());
    update.publish(&f.owner).unwrap();
}

#[test]
fn local_scope_denial_precedes_actual_media_core_lock_contention() {
    let f = Fixture::new();
    let grant = f.grant(GrantOperation::Join);
    let (_, proof) = f.proof(&grant);
    let permit = f.service.admit(&proof).unwrap();
    let _core = f.service.0.core.lock().unwrap();
    assert!(matches!(
        permit.status(Direction::Publish, permit.publication().unwrap()),
        AuthorizationStatus::Unavailable { .. }
    ));
    assert_local_scope_denials(permit.as_ref());
}

#[test]
fn native_media_offered_scopes_share_one_current_turn_and_remote_leave_lapses_only_that_scope() {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let (_, guest, certificate) = crate::room_media_policy::tests::member(
        &mut owner,
        qos_peer_state::join::MemberRole::Editor,
        None,
    );
    activate(&mut owner);
    let service = MediaService::for_owner(
        &owner,
        CommittedAuthority::new(&owner),
        "https://relay.test/moq/v21/voice",
    )
    .unwrap();
    let f = Fixture {
        _dir: dir,
        owner,
        service,
    };
    let mut guest_request = f.request();
    guest_request.device = certificate.device_public_key;
    guest_request.participation_id = [19; 32];
    let challenge = f
        .service
        .challenge(guest_request.clone(), GrantOperation::Join)
        .unwrap();
    let GrantCompletion::Granted(guest_grant) = f
        .service
        .complete(
            &f.owner,
            challenge.nonce,
            guest.sign(&challenge.signing_digest).to_bytes(),
        )
        .unwrap()
    else {
        panic!("guest grant")
    };
    let guest_name = namespace(guest_grant.metadata.namespace);
    let grant = f.grant(GrantOperation::Join);
    let (_, proof) = f.proof(&grant);
    let permit = f.service.admit(&proof).unwrap();
    let own = permit.publication().unwrap().to_owned();
    assert!(permit.subscriptions().is_empty());
    assert_eq!(permit.subscription_prefix(), own.split('/').next());
    let offered = [guest_name.as_str()];
    let mut lapsed = Vec::new();
    let before = CURRENT_TURNS.with(std::cell::Cell::get);
    assert!(matches!(
        permit.status_offered(&offered, &mut lapsed),
        AuthorizationStatus::Current
    ));
    assert!(lapsed.is_empty());
    assert_eq!(
        CURRENT_TURNS.with(std::cell::Cell::get) - before,
        1,
        "own, publication and every offered scope share one committed-owner guard"
    );
    // A namespace outside this call, or this session's own, is never offered.
    let mut foreign = guest_name.clone();
    foreign.replace_range(0..1, if &guest_name[..1] == "0" { "1" } else { "0" });
    for name in [foreign.as_str(), own.as_str()] {
        assert!(matches!(
            permit.status_offered(&[name], &mut lapsed),
            AuthorizationStatus::Denied
        ));
    }
    assert!(lapsed.is_empty());

    let leave = f
        .service
        .challenge(guest_request, GrantOperation::Leave)
        .unwrap();
    assert!(matches!(
        f.service
            .complete(
                &f.owner,
                leave.nonce,
                guest.sign(&leave.signing_digest).to_bytes(),
            )
            .unwrap(),
        GrantCompletion::Left
    ));
    assert!(permit.current(Direction::Publish, permit.publication().unwrap()));
    // The remote leave withdraws only that offered scope; this session's own
    // authority is still current, so it is not denied.
    let before = CURRENT_TURNS.with(std::cell::Cell::get);
    assert!(matches!(
        permit.status_offered(&offered, &mut lapsed),
        AuthorizationStatus::Current
    ));
    assert_eq!(lapsed, vec![guest_name.clone()]);
    assert_eq!(CURRENT_TURNS.with(std::cell::Cell::get) - before, 1);
    // A strict single-scope observation still fails closed.
    assert!(matches!(
        permit.status(Direction::Subscribe, &guest_name),
        AuthorizationStatus::Denied
    ));
    assert!(matches!(permit.status_all(), AuthorizationStatus::Current));
}

struct Fixture {
    _dir: tempfile::TempDir,
    owner: SessionOwner,
    service: MediaService,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
        activate(&mut owner);
        let authority = CommittedAuthority::new(&owner);
        let service =
            MediaService::for_owner(&owner, authority, "https://relay.test/moq/v21/voice").unwrap();
        Self {
            _dir: dir,
            owner,
            service,
        }
    }
    fn request(&self) -> GrantRequest {
        GrantRequest {
            device: self.owner.identity.native.verifying_key().to_bytes(),
            room: self.owner.peer().room().into(),
            call_id: hex::encode(CALL),
            participation_id: [7; 32],
            generation: 1,
            directions: MediaDirections {
                publish: true,
                subscribe: true,
            },
        }
    }
    fn grant(&self, operation: GrantOperation) -> MediaGrant {
        let c = self.service.challenge(self.request(), operation).unwrap();
        match self
            .service
            .complete(
                &self.owner,
                c.nonce,
                self.owner
                    .identity
                    .native
                    .sign(&c.signing_digest)
                    .to_bytes(),
            )
            .unwrap()
        {
            GrantCompletion::Granted(grant) => grant,
            _ => panic!("expected grant"),
        }
    }
    fn proof(&self, grant: &MediaGrant) -> (RelayChallenge, Vec<u8>) {
        let c = self.service.relay_challenge(grant.as_bytes()).unwrap();
        let bytes = encode_relay_proof(
            c.session_id,
            c.challenge.nonce,
            self.owner
                .identity
                .native
                .sign(&c.challenge.signing_digest)
                .to_bytes(),
        )
        .unwrap();
        (c, bytes)
    }
}
#[test]
fn native_custody_grant_and_fresh_device_proof_admit_exact_scopes() {
    let f = Fixture::new();
    let grant = f.grant(GrantOperation::Join);
    let (_, proof) = f.proof(&grant);
    assert_eq!(proof.len(), 129);
    let permit = f.service.admit(&proof).unwrap();
    let own = format!(
        "{}/{}",
        hex::encode(grant.metadata.namespace.call),
        hex::encode(grant.metadata.namespace.publisher)
    );
    assert_eq!(permit.publication(), Some(own.as_str()));
    assert!(permit.current(Direction::Publish, &own));
    assert!(permit.subscriptions().is_empty());
    assert!(!permit.current(Direction::Subscribe, &own));
    assert!(!permit.current(Direction::Publish, &format!("{own}/child")));
    assert!(f.service.admit(&proof).is_err());
}
#[test]
fn forged_wrong_session_and_noncanonical_proofs_are_denied() {
    let f = Fixture::new();
    let grant = f.grant(GrantOperation::Join);
    let c = f.service.relay_challenge(grant.as_bytes()).unwrap();
    let fake = SigningKey::from_bytes(&[31; 32])
        .sign(&c.challenge.signing_digest)
        .to_bytes();
    assert!(
        f.service
            .admit(&encode_relay_proof(c.session_id, c.challenge.nonce, fake).unwrap())
            .is_err()
    );
    let (c, _) = f.proof(&grant);
    let sig = f
        .owner
        .identity
        .native
        .sign(&c.challenge.signing_digest)
        .to_bytes();
    assert!(
        f.service
            .admit(&encode_relay_proof([42; 32], c.challenge.nonce, sig).unwrap())
            .is_err()
    );
    let (_, proof) = f.proof(&grant);
    for bytes in [
        vec![],
        vec![0; 8193],
        proof[..128].to_vec(),
        [proof.as_slice(), &[0]].concat(),
    ] {
        assert!(f.service.admit(&bytes).is_err());
    }
    let permit = f.service.admit(&proof).unwrap();
    assert!(permit.current(Direction::Publish, permit.publication().unwrap()));
}

#[test]
fn renewal_replaces_private_handle_and_close_cannot_be_replayed() {
    let f = Fixture::new();
    let grant = f.grant(GrantOperation::Join);
    let (c, proof) = f.proof(&grant);
    let permit = f.service.admit(&proof).unwrap();
    let name = permit.publication().unwrap();
    let renewed = f.grant(GrantOperation::Renew);
    let refresh = f
        .service
        .refresh_challenge(c.session_id, renewed.as_bytes())
        .unwrap();
    let signature = f
        .owner
        .identity
        .native
        .sign(&refresh.signing_digest)
        .to_bytes();
    assert!(
        f.service
            .refresh([99; 32], refresh.nonce, signature)
            .is_err()
    );
    f.service
        .refresh(c.session_id, refresh.nonce, signature)
        .unwrap();
    assert!(permit.current(Direction::Publish, name));
    assert!(
        f.service
            .refresh(c.session_id, refresh.nonce, signature)
            .is_err()
    );
    let pending = f
        .service
        .refresh_challenge(c.session_id, renewed.as_bytes())
        .unwrap();
    f.service.close(c.session_id).unwrap();
    assert!(!permit.current(Direction::Publish, name));
    assert!(
        f.service
            .refresh(
                c.session_id,
                pending.nonce,
                f.owner
                    .identity
                    .native
                    .sign(&pending.signing_digest)
                    .to_bytes()
            )
            .is_err()
    );
    assert!(f.service.admit(&proof).is_err());
}

#[test]
fn current_authority_contention_is_distinct_from_explicit_invalidation() {
    use qos_moq::relay::AuthorizationStatus;
    let f = Fixture::new();
    let grant = f.grant(GrantOperation::Join);
    let (_, proof) = f.proof(&grant);
    let permit = f.service.admit(&proof).unwrap();
    let name = permit.publication().unwrap();
    let (_, new_proof) = f.proof(&grant);
    let update = f.service.0.source.committed.begin(&f.owner).unwrap();
    assert!(matches!(
        permit.status(Direction::Publish, name),
        AuthorizationStatus::Unavailable { .. }
    ));
    assert!(f.service.admit(&new_proof).is_err());
    update.publish(&f.owner).unwrap();
    assert!(permit.current(Direction::Publish, name));
    f.service.invalidate();
    assert!(matches!(
        permit.status(Direction::Publish, name),
        AuthorizationStatus::Denied
    ));
}

#[test]
fn durable_owner_update_rechecks_current_committed_policy() {
    let mut f = Fixture::new();
    let grant = f.grant(GrantOperation::Join);
    let (_, proof) = f.proof(&grant);
    let permit = f.service.admit(&proof).unwrap();
    let name = permit.publication().unwrap();
    assert!(permit.current(Direction::Publish, name));
    let before = f.owner.committed_generation();
    f.service
        .owner_update(&mut f.owner, |owner| {
            edit_policy(owner, |policy| {
                let call = policy.calls.get_mut(&CALL).unwrap();
                call.epoch = 2;
                call.allowed.publish = false;
            });
            Ok(())
        })
        .unwrap();
    assert!(f.owner.committed_generation() > before);
    assert!(!permit.current(Direction::Publish, name));
    assert!(f.service.relay_challenge(grant.as_bytes()).is_err());
}

#[test]
fn live_handle_capacity_is_bounded_and_dropped_handles_are_reclaimed() {
    let f = Fixture::new();
    let grant = f.grant(GrantOperation::Join);
    let mut permits = Vec::new();
    for _ in 0..MAX_SESSIONS {
        let (_, proof) = f.proof(&grant);
        permits.push(f.service.admit(&proof).unwrap());
    }
    let (_, proof) = f.proof(&grant);
    assert!(f.service.admit(&proof).is_err());
    permits.pop();
    assert!(f.service.admit(&proof).is_ok());
    drop(permits);
    for _ in 0..32 {
        let (_, proof) = f.proof(&grant);
        assert!(f.service.admit(&proof).is_ok());
    }
    assert!(f.service.0.core.lock().unwrap().sessions.len() <= MAX_SESSIONS);
}

#[tokio::test]
async fn real_webtransport_uses_native_signed_proof_and_current_owner_revocation() {
    use qos_moq::{
        relay::{Relay, VOICE_PATH},
        voice::{FRAME_SAMPLES, VoiceObject},
    };
    let mut f = Fixture::new();
    let subscriber_key = SigningKey::from_bytes(&[97; 32]);
    let mut enrollment = crate::enrollment::EnrollmentAuthority::default();
    let now = tilo_relay::clock::now_ns();
    let enrolled = enrollment
        .challenge(
            &f.owner,
            qos_peer_state::enrollment::ChallengeRequest {
                device_public_key: hex::encode(subscriber_key.verifying_key().to_bytes()),
                request_id: hex::encode([96; 32]),
            },
            now,
        )
        .unwrap();
    let proof = qos_peer_state::enrollment::sign(
        &qos_peer_state::enrollment::decode(&enrolled.challenge).unwrap(),
        &subscriber_key,
    )
    .unwrap();
    f.service
        .owner_update(&mut f.owner, |owner| {
            enrollment
                .complete(owner, proof, now)
                .map_err(anyhow::Error::from)
        })
        .unwrap();
    let grant = f.grant(GrantOperation::Join);
    let mut request = f.request();
    request.device = subscriber_key.verifying_key().to_bytes();
    request.participation_id = [98; 32];
    request.directions.publish = false;
    let challenge = f.service.challenge(request, GrantOperation::Join).unwrap();
    let GrantCompletion::Granted(subscriber_grant) = f
        .service
        .complete(
            &f.owner,
            challenge.nonce,
            subscriber_key.sign(&challenge.signing_digest).to_bytes(),
        )
        .unwrap()
    else {
        panic!("expected listener grant")
    };
    let relay = f
        .service
        .relay_challenge(subscriber_grant.as_bytes())
        .unwrap();
    let subscriber_proof = encode_relay_proof(
        relay.session_id,
        relay.challenge.nonce,
        subscriber_key
            .sign(&relay.challenge.signing_digest)
            .to_bytes(),
    )
    .unwrap();
    let (_, publisher_proof) = f.proof(&grant);
    let namespace = namespace(grant.metadata.namespace);
    let draft: moq_net::Version = "moq-transport-21".parse().unwrap();
    let mut config = moq_native::ServerConfig::default();
    config.bind = Some("127.0.0.1:0".into());
    config.version = vec![draft];
    config.tls.generate = vec!["localhost".into()];
    let server = config.init().unwrap();
    let url: url::Url = format!(
        "https://localhost:{}{VOICE_PATH}",
        server.local_addr().unwrap().port()
    )
    .parse()
    .unwrap();
    let mut config = moq_native::ClientConfig::default();
    config.version = vec![draft];
    config.tls.fingerprint = server.certificates().fingerprints();
    let client = config.init().unwrap();
    let relay = Relay::new(Arc::new(f.service.clone()));
    let task = tokio::spawn(relay.serve(server));
    let incoming = moq_net::Origin::random().produce();
    let outgoing = moq_net::Origin::random().produce();
    let subscriber = client
        .clone()
        .with_authorization(subscriber_proof)
        .unwrap()
        .with_subscriber(incoming.clone())
        .connect(url.clone())
        .await
        .unwrap();
    let mut broadcast = outgoing
        .create_broadcast(
            namespace.as_str(),
            moq_net::broadcast::Route::new().with_announce(true),
        )
        .unwrap();
    let mut track = broadcast.create_track("audio", None).unwrap();
    let expected = VoiceObject {
        sequence: 0,
        timestamp: 0,
        duration: FRAME_SAMPLES,
        payload: vec![0xf8, 0xff, 0xfe],
    }
    .encode()
    .unwrap();
    track
        .write_frame(moq_net::Timestamp::ZERO, expected.clone())
        .unwrap();
    let publisher = client
        .clone()
        .with_authorization(publisher_proof.clone())
        .unwrap()
        .with_publisher(&outgoing)
        .connect(url.clone())
        .await
        .unwrap();
    let actual = tokio::time::timeout(Duration::from_secs(5), async {
        let b = incoming
            .consume()
            .announced_broadcast(namespace.as_str())
            .await
            .unwrap();
        let mut t = b.track("audio").unwrap().subscribe(None).await.unwrap();
        let mut g = t.recv_group().await.unwrap().unwrap();
        g.read_frame().await.unwrap().unwrap().payload.to_vec()
    })
    .await
    .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(publisher.version().as_str(), "moq-transport-21");
    if let Ok(replayed) = client
        .with_authorization(publisher_proof)
        .unwrap()
        .connect(url)
        .await
    {
        let _ = tokio::time::timeout(Duration::from_secs(1), replayed.closed())
            .await
            .expect("proof replay closes");
    }
    f.service.invalidate();
    let _ = tokio::time::timeout(Duration::from_secs(1), publisher.closed())
        .await
        .expect("publisher revocation deadline");
    let _ = tokio::time::timeout(Duration::from_secs(1), subscriber.closed())
        .await
        .expect("subscriber revocation deadline");
    task.abort();
}

#[test]
fn monotonic_grant_deadline_does_not_gain_time_spent_validating() {
    let f = Fixture::new();
    let sampled = SystemTime::now();
    let expected = Instant::now() + Duration::from_secs(1);
    let expires: u64 = (sampled.duration_since(SystemTime::UNIX_EPOCH).unwrap()
        + Duration::from_secs(1))
    .as_nanos()
    .try_into()
    .unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let offset = f.service.0.deadline(expires, sampled).unwrap();
    assert!(
        f.service.0.clock + Duration::from_nanos(offset) <= expected + Duration::from_millis(5),
        "signature verification time must not extend the grant"
    );
}

#[test]
fn committed_authority_rejected_signed_input_preserves_media_but_failed_barrier_denies() {
    use qos_peer_state::{NeighbourMsg, TransportOrigin};
    use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};
    let mut f = Fixture::new();
    let grant = f.grant(GrantOperation::Join);
    let (_, proof) = f.proof(&grant);
    let permit = f.service.admit(&proof).unwrap();
    let name = permit.publication().unwrap();
    let actions = crate::action_host::Authority::new(f.service.0.source.committed.clone());
    actions.set_ready(true);
    actions.set_ready(false);
    assert!(permit.current(Direction::Publish, name));
    let app = "dev.qos.word-processor";
    let record = "tab:0000000000000001";
    let mut candidate = f.owner.peer().clone();
    let mut document = AutoCommit::load(&candidate.document(app, record).unwrap()).unwrap();
    document.put(ROOT, "rejected", true).unwrap();
    let mut message = candidate
        .publish_document(
            app,
            record,
            document.save(),
            &f.owner.identity.native,
            SystemTime::now(),
        )
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut message else {
        panic!("expected signed mutation")
    };
    envelope.payload.push(0xff); // Break the actual native signature after signing.
    let origin = TransportOrigin::RelayDelta(f.owner.identity.native.verifying_key().to_bytes());
    let before = f.owner.committed_generation();
    let rejected = f
        .service
        .owner_update(&mut f.owner, |owner| owner.receive(origin, message));
    assert!(rejected.is_err());
    assert_eq!(f.owner.committed_generation(), before);
    assert!(permit.current(Direction::Publish, name));
    let root = f.owner.root().to_owned();
    std::fs::rename(root.join("session.bin"), root.join("saved.bin")).unwrap();
    std::fs::create_dir(root.join("session.bin")).unwrap();
    assert!(
        f.service
            .owner_update(&mut f.owner, SessionOwner::flush)
            .is_err()
    );
    assert!(!f.owner.permits_publication());
    assert!(matches!(
        permit.status(Direction::Publish, name),
        qos_moq::relay::AuthorizationStatus::Denied
    ));
}

#[test]
fn room_media_policy_legacy_voice_rows_cannot_authorize_join() {
    let dir = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let service =
        MediaService::for_owner(&owner, CommittedAuthority::new(&owner), "relay-test").unwrap();
    let f = Fixture {
        _dir: dir,
        owner,
        service,
    };
    assert!(f.owner.peer().document("dev.qos.chat", "").is_some());
    assert!(f.owner.peer().validated_media_policy().unwrap().is_none());
    assert!(
        matches!(
            f.service.challenge(f.request(), GrantOperation::Join),
            Err(MediaError::Unauthorized)
        ),
        "a Chat voice row and independent policy callback cannot authorize native media"
    );
}

#[test]
fn room_media_policy_reader_can_subscribe_but_cannot_publish_or_downgrade() {
    use crate::room_media_policy::tests::member;
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let (_, key, cert) = member(&mut owner, qos_peer_state::join::MemberRole::Viewer, None);
    activate(&mut owner);
    let service =
        MediaService::for_owner(&owner, CommittedAuthority::new(&owner), "relay-test").unwrap();
    let f = Fixture {
        _dir: dir,
        owner,
        service,
    };
    let mut request = f.request();
    request.device = cert.device_public_key;
    for directions in [
        MediaDirections {
            publish: true,
            subscribe: false,
        },
        MediaDirections {
            publish: true,
            subscribe: true,
        },
    ] {
        request.directions = directions;
        assert!(matches!(
            f.service.challenge(request.clone(), GrantOperation::Join),
            Err(MediaError::Unauthorized)
        ));
    }
    request.directions = MediaDirections {
        publish: false,
        subscribe: true,
    };
    let challenge = f.service.challenge(request, GrantOperation::Join).unwrap();
    let GrantCompletion::Granted(grant) = f
        .service
        .complete(
            &f.owner,
            challenge.nonce,
            key.sign(&challenge.signing_digest).to_bytes(),
        )
        .unwrap()
    else {
        panic!("reader grant")
    };
    let challenge = f.service.relay_challenge(grant.as_bytes()).unwrap();
    let proof = encode_relay_proof(
        challenge.session_id,
        challenge.challenge.nonce,
        key.sign(&challenge.challenge.signing_digest).to_bytes(),
    )
    .unwrap();
    let permit = f.service.admit(&proof).unwrap();
    assert!(permit.publication().is_none());
    assert!(!grant.metadata.request.directions.publish);
    assert!(grant.metadata.request.directions.subscribe);
}

#[test]
fn room_media_policy_committed_end_and_restart_never_revive_old_sessions() {
    use qos_peer_state::room::media::CallPhaseV1;
    let mut f = Fixture::new();
    let old = f.grant(GrantOperation::Join);
    let (_, proof) = f.proof(&old);
    let permit = f.service.admit(&proof).unwrap();
    let name = permit.publication().unwrap();
    let pending = f
        .service
        .challenge(f.request(), GrantOperation::Renew)
        .unwrap();
    for (epoch, phase) in [(2, CallPhaseV1::Ended), (3, CallPhaseV1::Active)] {
        f.service
            .owner_update(&mut f.owner, |owner| {
                edit_policy(owner, |policy| {
                    let call = policy.calls.get_mut(&CALL).unwrap();
                    call.epoch = epoch;
                    call.phase = phase;
                });
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            permit.status(Direction::Publish, name),
            AuthorizationStatus::Denied
        ));
        assert!(f.service.relay_challenge(old.as_bytes()).is_err());
        assert!(
            f.service
                .challenge(f.request(), GrantOperation::Renew)
                .is_err()
        );
    }
    assert!(
        f.service
            .complete(
                &f.owner,
                pending.nonce,
                f.owner
                    .identity
                    .native
                    .sign(&pending.signing_digest)
                    .to_bytes()
            )
            .is_err()
    );
    let mut request = f.request();
    request.generation = 2;
    let challenge = f.service.challenge(request, GrantOperation::Join).unwrap();
    let GrantCompletion::Granted(fresh) = f
        .service
        .complete(
            &f.owner,
            challenge.nonce,
            f.owner
                .identity
                .native
                .sign(&challenge.signing_digest)
                .to_bytes(),
        )
        .unwrap()
    else {
        panic!("fresh grant")
    };
    assert_eq!(fresh.metadata.epoch, 3);
    assert_ne!(old.metadata.namespace.call, fresh.metadata.namespace.call);
    assert!(!permit.current(Direction::Publish, name));
}

#[test]
fn room_media_policy_denial_intersects_directions_and_lifting_keeps_epoch_fence() {
    use qos_peer_state::room::media::DirectionsV1;
    let mut f = Fixture::new();
    let grant = f.grant(GrantOperation::Join);
    let (_, proof) = f.proof(&grant);
    let permit = f.service.admit(&proof).unwrap();
    let root = f.owner.peer().authority().user_public_key;
    f.service
        .owner_update(&mut f.owner, |owner| {
            edit_policy(owner, |policy| {
                let call = policy.calls.get_mut(&CALL).unwrap();
                call.epoch = 2;
                call.denied.insert(
                    root,
                    DirectionsV1 {
                        publish: true,
                        subscribe: false,
                    },
                );
            });
            Ok(())
        })
        .unwrap();
    assert!(!permit.current(Direction::Publish, permit.publication().unwrap()));
    let mut request = f.request();
    request.generation = 2;
    assert!(
        f.service
            .challenge(request.clone(), GrantOperation::Join)
            .is_err()
    );
    request.directions.publish = false;
    assert!(f.service.challenge(request, GrantOperation::Join).is_ok());
    f.service
        .owner_update(&mut f.owner, |owner| {
            edit_policy(owner, |policy| {
                let call = policy.calls.get_mut(&CALL).unwrap();
                call.epoch = 3;
                call.denied.clear();
            });
            Ok(())
        })
        .unwrap();
    assert!(!permit.current(Direction::Publish, permit.publication().unwrap()));
    assert!(f.service.relay_challenge(grant.as_bytes()).is_err());
}

#[test]
fn room_media_policy_rejected_turn_keeps_pair_and_forwarding_reuses_cached_projection() {
    use crate::room_media_policy::tests::projection_count;
    let mut f = Fixture::new();
    let grant = f.grant(GrantOperation::Join);
    let (relay, proof) = f.proof(&grant);
    let permit = f.service.admit(&proof).unwrap();
    let count = projection_count();
    let generation = f.owner.committed_generation();
    assert!(
        f.service
            .owner_update(&mut f.owner, |_| -> anyhow::Result<()> {
                anyhow::bail!("rejected input")
            })
            .is_err()
    );
    assert_eq!(f.owner.committed_generation(), generation);
    for _ in 0..20 {
        assert!(permit.current(Direction::Publish, permit.publication().unwrap()));
        f.service
            .turn(|peer, policy, core, now| {
                let session = &core.sessions[&relay.session_id].session;
                assert_eq!(
                    core.authority
                        .presence_snapshot(peer, policy, session, now)?
                        .len(),
                    1
                );
                assert_eq!(
                    core.authority
                        .subscription_namespaces(peer, policy, session, now)?
                        .len(),
                    0
                );
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(
        projection_count(),
        count,
        "periodic checks must reuse the committed room/Chat validation"
    );
}

#[test]
fn room_media_policy_publication_pairs_peer_and_policy_after_real_barrier() {
    let mut f = Fixture::new();
    let before = f.owner.committed_generation();
    let committed = f.service.0.source.committed.clone();
    let update = committed.begin(&f.owner).unwrap();
    edit_policy(&mut f.owner, |policy| {
        let call = policy.calls.get_mut(&CALL).unwrap();
        call.epoch = 2;
        call.allowed.publish = false;
    });
    assert!(f.owner.committed_generation() > before);
    std::thread::scope(|scope| {
        let service = f.service.clone();
        let request = f.request();
        scope
            .spawn(move || assert!(service.challenge(request, GrantOperation::Join).is_err()))
            .join()
            .unwrap();
    });
    assert!(matches!(committed.observe(), Observation::Updating));
    update.publish(&f.owner).unwrap();
    let current = committed.read().unwrap();
    let projected = current
        .media_policy()
        .current(current.peer(), &f.request(), SystemTime::now())
        .unwrap();
    assert_eq!(current.generation(), f.owner.committed_generation());
    assert_eq!(projected.epoch, 2);
    assert!(!projected.directions.publish);
    assert_eq!(
        current
            .peer()
            .validated_media_policy()
            .unwrap()
            .unwrap()
            .calls[&CALL]
            .epoch,
        projected.epoch
    );
}

#[test]
fn room_media_policy_exact_missing_record_denies_until_that_record_is_committed() {
    use crate::room_media_policy::tests::{APP, activate_with_record};
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let record = "uuid:other-chat";
    activate_with_record(&mut owner, record);
    let service =
        MediaService::for_owner(&owner, CommittedAuthority::new(&owner), "relay-test").unwrap();
    let mut f = Fixture {
        _dir: dir,
        owner,
        service,
    };
    assert!(f.owner.peer().document(APP, "").is_some());
    assert!(f.owner.peer().document(APP, record).is_none());
    assert!(matches!(
        f.service.challenge(f.request(), GrantOperation::Join),
        Err(MediaError::Unauthorized)
    ));
    let current = f.service.0.source.committed.read().unwrap();
    assert!(
        current
            .peer()
            .permissions(f.request().device, SystemTime::now())
            .is_ok(),
        "media prerequisites do not deny unrelated room data"
    );
    drop(current);
    f.service
        .owner_update(&mut f.owner, |owner| {
            let mut candidate = owner.peer().clone();
            let bytes = candidate.document(APP, "").unwrap();
            candidate
                .publish_document(
                    APP,
                    record,
                    bytes,
                    &owner.identity.native,
                    SystemTime::now(),
                )
                .map_err(anyhow::Error::msg)?;
            owner.commit_mutation_candidate(candidate, owner.directory().to_vec())
        })
        .unwrap();
    assert_eq!(f.grant(GrantOperation::Join).metadata.epoch, 1);
}

#[test]
fn room_media_policy_room_call_host_and_member_bindings_are_exact() {
    let f = Fixture::new();
    for call in ["voice".to_owned(), "AB".repeat(32), hex::encode([8; 32])] {
        let mut request = f.request();
        request.call_id = call;
        assert!(matches!(
            f.service.challenge(request, GrantOperation::Join),
            Err(MediaError::Unauthorized)
        ));
    }
    let mut request = f.request();
    request.room = uuid::Uuid::from_bytes([5; 16]).to_string();
    assert!(matches!(
        f.service.challenge(request, GrantOperation::Join),
        Err(MediaError::Unauthorized)
    ));
    let mut request = f.request();
    request.device = SigningKey::from_bytes(&[90; 32]).verifying_key().to_bytes();
    assert!(matches!(
        f.service.challenge(request, GrantOperation::Join),
        Err(MediaError::Unauthorized)
    ));
    let dir = tempfile::tempdir().unwrap();
    let other = SessionOwner::initialize(&dir.path().join("other")).unwrap();
    let current = f.service.0.source.committed.read().unwrap();
    assert!(
        current
            .media_policy()
            .current(other.peer(), &f.request(), SystemTime::now())
            .is_err()
    );
}

#[test]
fn room_media_policy_verified_revocation_reconciles_and_invalidates_old_grant() {
    use crate::room_media_policy::tests::member;
    use qos_peer_state::{NeighbourMsg, TransportOrigin};
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let (root, _, cert) = member(&mut owner, qos_peer_state::join::MemberRole::Editor, None);
    activate(&mut owner);
    let service =
        MediaService::for_owner(&owner, CommittedAuthority::new(&owner), "relay-test").unwrap();
    let mut f = Fixture {
        _dir: dir,
        owner,
        service,
    };
    let grant = f.grant(GrantOperation::Join);
    let (_, proof) = f.proof(&grant);
    let permit = f.service.admit(&proof).unwrap();
    // An admitted root revokes another device of its own root. The current
    // native requester stays authorized; reconciliation advances the governed basis.
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: [88; 32],
        revoked_at_ns: 2,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &root).unwrap();
    f.service
        .owner_update(&mut f.owner, |owner| {
            owner.receive(
                TransportOrigin::UntrustedPresence(cert.device_public_key),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(cert),
                    revocations: vec![revoked],
                },
            )
        })
        .unwrap();
    assert!(matches!(
        permit.status(Direction::Publish, permit.publication().unwrap()),
        AuthorizationStatus::Denied
    ));
    assert!(
        f.service
            .challenge(f.request(), GrantOperation::Renew)
            .is_ok()
    );
    let current = f.service.0.source.committed.read().unwrap();
    let policy = current.peer().validated_media_policy().unwrap().unwrap();
    assert_eq!(policy.authorization_revision, 2);
    assert_eq!(policy.calls[&CALL].epoch, 1);
    assert!(
        current
            .peer()
            .permissions(f.request().device, SystemTime::now())
            .is_ok()
    );
}

#[test]
fn room_media_policy_removed_or_revoked_member_cannot_renew_or_forward() {
    use crate::collaboration::{CollaborationService, RemoveMemberRequest};
    use crate::room_media_policy::tests::member;
    for revoke in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
        let (root, key, cert) = member(&mut owner, qos_peer_state::join::MemberRole::Editor, None);
        activate(&mut owner);
        let service =
            MediaService::for_owner(&owner, CommittedAuthority::new(&owner), "relay-test").unwrap();
        let mut f = Fixture {
            _dir: dir,
            owner,
            service,
        };
        let host_grant = f.grant(GrantOperation::Join);
        let mut request = f.request();
        request.device = cert.device_public_key;
        let challenge = f
            .service
            .challenge(request.clone(), GrantOperation::Join)
            .unwrap();
        let GrantCompletion::Granted(grant) = f
            .service
            .complete(
                &f.owner,
                challenge.nonce,
                key.sign(&challenge.signing_digest).to_bytes(),
            )
            .unwrap()
        else {
            panic!("member grant")
        };
        let relay = f.service.relay_challenge(grant.as_bytes()).unwrap();
        let permit = f
            .service
            .admit(
                &encode_relay_proof(
                    relay.session_id,
                    relay.challenge.nonce,
                    key.sign(&relay.challenge.signing_digest).to_bytes(),
                )
                .unwrap(),
            )
            .unwrap();
        let name = permit.publication().unwrap();
        assert!(permit.current(Direction::Publish, name));
        f.service
            .owner_update(&mut f.owner, |owner| {
                if revoke {
                    let mut revoked = tilo_user_identity::RevocationEntry {
                        user_public_key: cert.user_public_key,
                        revoked_device_public_key: cert.device_public_key,
                        revoked_at_ns: 2,
                        reason: tilo_user_identity::RevocationReason::Retired,
                        signature: [0; 64],
                    };
                    tilo_user_identity::sign_revocation_entry(&mut revoked, &root).unwrap();
                    use ui::automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
                    let before = owner.peer().admitted_room().unwrap().clone();
                    let mut room = before.clone();
                    let identities = room.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
                    let device = hex::encode(cert.device_public_key);
                    let (row, _) = room.get(&identities, &device).unwrap().unwrap();
                    let mut row: serde_json::Value =
                        serde_json::from_slice(row.to_bytes().unwrap()).unwrap();
                    row["revocations"] = serde_json::json!([revoked]);
                    room.put(
                        &identities,
                        &device,
                        ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
                    )
                    .unwrap();
                    qos_peer_state::room::media::refresh_authorization(&before, &mut room).unwrap();
                    crate::room_media_policy::tests::commit_room(owner, room);
                    Ok(())
                } else {
                    let administrator = owner.peer().authority().device_public_key;
                    let now: u64 = SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                        .try_into()
                        .unwrap();
                    CollaborationService::new(vec![])
                        .remove_member(
                            owner,
                            administrator,
                            RemoveMemberRequest {
                                request_id: [83; 32],
                                user_public_key: cert.user_public_key,
                            },
                            now,
                        )
                        .map_err(anyhow::Error::msg)
                        .map(|_| ())
                }
            })
            .unwrap();
        assert!(matches!(
            permit.status(Direction::Publish, name),
            AuthorizationStatus::Denied
        ));
        assert!(matches!(
            f.service.challenge(request, GrantOperation::Renew),
            Err(MediaError::Unauthorized)
        ));
        assert!(
            f.service
                .refresh_challenge(relay.session_id, grant.as_bytes())
                .is_err()
        );
        assert!(
            f.service.relay_challenge(host_grant.as_bytes()).is_err(),
            "room authorization changes also fence unaffected host grants"
        );
        if !revoke {
            let current = f.service.0.source.committed.read().unwrap();
            let policy = current
                .media_policy()
                .current(current.peer(), &f.request(), SystemTime::now())
                .unwrap();
            assert_eq!(policy.membership_generation, 2);
            assert_eq!(policy.epoch, 1);
        }
    }
}

#[test]
fn room_media_policy_cached_membership_still_checks_current_certificate_expiry() {
    use crate::room_media_policy::tests::member;
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let now = SystemTime::now();
    let expiry: u64 = (now.duration_since(SystemTime::UNIX_EPOCH).unwrap()
        + Duration::from_secs(60))
    .as_nanos()
    .try_into()
    .unwrap();
    let (_, _, cert) = member(
        &mut owner,
        qos_peer_state::join::MemberRole::Viewer,
        Some(expiry),
    );
    activate(&mut owner);
    let service =
        MediaService::for_owner(&owner, CommittedAuthority::new(&owner), "relay-test").unwrap();
    let f = Fixture {
        _dir: dir,
        owner,
        service,
    };
    let mut request = f.request();
    request.device = cert.device_public_key;
    request.directions.publish = false;
    let current = f.service.0.source.committed.read().unwrap();
    assert!(
        current
            .media_policy()
            .current(current.peer(), &request, SystemTime::now())
            .is_ok()
    );
    assert!(matches!(
        current
            .media_policy()
            .current(current.peer(), &request, now + Duration::from_secs(61)),
        Err(MediaError::Unauthorized)
    ));
}

#[test]
fn room_media_policy_signed_owner_restart_rebuilds_policy_and_fences_old_boot() {
    let f = Fixture::new();
    let old = f.grant(GrantOperation::Join);
    let root = f.owner.root().to_owned();
    let request = f.request();
    let Fixture {
        _dir: dir,
        owner,
        service,
    } = f;
    drop(service);
    drop(owner);
    let owner = SessionOwner::open(&root).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let service =
        MediaService::for_owner(&owner, committed, "https://relay.test/moq/v21/voice").unwrap();
    assert!(service.relay_challenge(old.as_bytes()).is_err());
    let challenge = service.challenge(request, GrantOperation::Join).unwrap();
    let GrantCompletion::Granted(fresh) = service
        .complete(
            &owner,
            challenge.nonce,
            owner
                .identity
                .native
                .sign(&challenge.signing_digest)
                .to_bytes(),
        )
        .unwrap()
    else {
        panic!("restart grant")
    };
    assert_eq!(fresh.metadata.epoch, 1);
    assert_eq!(fresh.metadata.membership_generation, 1);
    assert_ne!(fresh.metadata.host_boot, old.metadata.host_boot);
    drop(owner);
    drop(dir);
}
