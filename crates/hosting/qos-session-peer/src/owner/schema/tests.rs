use super::*;
use tilo_relay::delivery_v3::commit::{DeliveryLedgerV1, PublicationPool, PublicationRef};

// Independent frozen DTO: this describes the approved wire ABI, not runtime fields.
#[derive(Serialize)]
struct FixtureV5<'a> {
    version: u32,
    identity_root: &'a PathBuf,
    host_identity_root: &'a PathBuf,
    room: &'a str,
    authority: &'a DeviceCert,
    directory: &'a [u8],
    peer: &'a [u8],
    publications: Vec<u8>,
    pending: &'a [PendingOperation],
    generation: u64,
    issuances: &'a [crate::enrollment::Issuance],
    publication: FixtureRoots,
    collaboration: &'a crate::collaboration::CollaborationState,
    delivery: Vec<u8>,
    transport_hint: Option<Vec<u8>>,
}
#[derive(Serialize)]
struct FixtureRoots {
    baseline_generation: u64,
    baseline: Vec<PublicationRef>,
    tail: Vec<PublicationRef>,
}
fn fixture(owner: &SessionOwner) -> FixtureV5<'_> {
    fixture_from(&owner.aggregate)
}
fn fixture_from(old: &Aggregate) -> FixtureV5<'_> {
    let mut pool = PublicationPool::new(crate::store::MAX_AGGREGATE_BYTES).unwrap();
    let roots = old
        .outgoing()
        .iter()
        .map(|p| PublicationRef {
            payload: pool.intern(p.bytes).unwrap(),
            kind: p.kind,
        })
        .collect::<Vec<_>>();
    FixtureV5 {
        version: 5,
        identity_root: &old.identity_root,
        host_identity_root: &old.host_identity_root,
        room: &old.room,
        authority: &old.authority,
        directory: &old.directory,
        peer: &old.peer,
        publications: pool.encode(crate::store::MAX_AGGREGATE_BYTES).unwrap(),
        pending: &old.pending,
        generation: old.generation,
        issuances: &old.issuances,
        publication: FixtureRoots {
            baseline_generation: old.baseline().0,
            baseline: roots[..old.baseline().1].to_vec(),
            tail: roots[old.baseline().1..].to_vec(),
        },
        collaboration: &old.collaboration,
        delivery: DeliveryLedgerV1 {
            version: 1,
            store_generation: [7; 16],
            resolved_through: old.generation,
            replay: Default::default(),
            admissions: Default::default(),
            consumers: Default::default(),
            pins: Default::default(),
        }
        .encode()
        .unwrap(),
        transport_hint: None,
    }
}
fn signed_fixture(owner: &SessionOwner, fixture: &FixtureV5<'_>) -> Vec<u8> {
    let payload = postcard::to_stdvec(fixture).unwrap();
    let signature = owner
        .identity
        .native
        .sign(
            blake3::Hasher::new_derive_key("qOS native session aggregate v1")
                .update(&payload)
                .finalize()
                .as_bytes(),
        )
        .to_bytes()
        .to_vec();
    postcard::to_stdvec(&SignedAggregate { payload, signature }).unwrap()
}
#[test]
fn session_v5_restores_exact_bytes_but_denies_mutation_and_publication() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    owner
        .queue_pending("legacy-request", b"exact private imported bytes", true)
        .unwrap();
    let before = owner.status();
    let bytes = signed_fixture(&owner, &fixture(&owner));
    owner.store.commit(&bytes).unwrap();
    drop(owner);
    let mut restored = SessionOwner::open(&path).expect("reader-first V5 restore");
    assert_eq!(restored.status(), before);
    assert!(!restored.permits_publication());
    assert!(
        restored
            .schema_read_only_reason()
            .unwrap()
            .contains("read-only")
    );
    assert!(
        crate::committed_authority::CommittedAuthority::new(&restored)
            .read()
            .is_err()
    );
    let problem = restored
        .queue_pending("new", b"no mutation", true)
        .unwrap_err()
        .to_string();
    assert!(problem.contains("read-only"), "{problem}");
    assert_eq!(restored.status(), before);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
    restored.flush().unwrap();
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
}
#[test]
fn session_v5_rejects_dangling_root_without_rewriting_original() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let mut value = fixture(&owner);
    value.publication.baseline[0].payload = [99; 32];
    let bytes = signed_fixture(&owner, &value);
    owner.store.commit(&bytes).unwrap();
    drop(owner);
    assert!(SessionOwner::open(&path).is_err());
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
}

#[test]
fn session_v5_literal_legacy_abi_and_administration_conversion() {
    let literals = [
        include_str!("fixtures/v1.hex"),
        include_str!("fixtures/v2.hex"),
        include_str!("fixtures/v3.hex"),
        include_str!("fixtures/v4.hex"),
    ];
    for (index, literal) in literals.iter().enumerate() {
        let bytes = hex::decode(literal.trim()).unwrap();
        let value = Aggregate::decode_payload(&bytes).unwrap();
        assert_eq!(value.identity_root, PathBuf::from("/i"));
        assert_eq!(value.peer, [6]);
        assert_eq!(value.pending[0].encoded_request, [9, 10]);
        assert!(value.pending[0].outcome_unknown);
        assert_eq!(value.outgoing().get(0).unwrap().bytes, [7, 8]);
        assert_eq!(value.generation, 11);
        let roundtrip =
            match index {
                0 => postcard::to_stdvec(&postcard::from_bytes::<LegacyAggregate>(&bytes).unwrap())
                    .unwrap(),
                1 => postcard::to_stdvec(&postcard::from_bytes::<AggregateV2>(&bytes).unwrap())
                    .unwrap(),
                2 => postcard::to_stdvec(&postcard::from_bytes::<AggregateV3>(&bytes).unwrap())
                    .unwrap(),
                _ => postcard::to_stdvec(&postcard::from_bytes::<AggregateV4>(&bytes).unwrap())
                    .unwrap(),
            };
        assert_eq!(roundtrip, bytes, "literal old ABI changed");
        let encoded = value.encode_payload().unwrap();
        assert_eq!(encoded[0], 4);
        if index == 3 {
            assert_eq!(encoded, bytes, "V4 writer must remain byte-exact");
            let receipt = &value.collaboration.administration[0];
            assert_eq!(receipt.request_id, [12; 32]);
            assert_eq!(receipt.request_digest, [13; 32]);
            assert_eq!(receipt.generation, 10);
            assert!(receipt.media_result.is_none());
        }
    }
}
fn media_result() -> qos_peer_state::room::media::MediaCallResultV1 {
    use qos_peer_state::room::media::*;
    MediaCallResultV1 {
        version: 1,
        call_id: [42; 32],
        epoch: 1,
        authorization_revision: 2,
        phase: CallPhaseV1::Ended,
        binding: ChatCallBindingV1 {
            // Frozen V5 literal written before the rename: the retired Chat id
            // must stay readable in persisted media results.
            app_id: "dev.qos.chat".into(),
            record_id: "".into(),
            channel_id: "channel".into(),
            channel_object: "1@aa".into(),
            chat_workspace_id: "chat".into(),
        },
    }
}
#[test]
fn session_v5_nested_media_requires_validation_and_v4_refuses_lossy_downgrade() {
    let temp = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let mut collaboration = owner.aggregate.collaboration.clone();
    collaboration
        .administration
        .push(crate::collaboration::AdministrationRecord {
            request_id: [1; 32],
            request_digest: [2; 32],
            generation: 1,
            media_result: Some(media_result()),
        });
    let mut value = fixture(&owner);
    value.collaboration = &collaboration;
    let bytes = postcard::to_stdvec(&value).unwrap();
    let decoded = Aggregate::decode_payload(&bytes).unwrap();
    assert_eq!(
        decoded.collaboration.administration[0].media_result,
        Some(media_result())
    );
    assert_eq!(decoded.encode_payload().unwrap(), bytes);
    let mut old = owner.aggregate.clone();
    old.collaboration = collaboration.clone();
    assert!(old.encode_payload().is_err(), "V4 cannot drop a V5 result");
    drop(value);
    collaboration.administration[0]
        .media_result
        .as_mut()
        .unwrap()
        .epoch = 0;
    let mut value = fixture(&owner);
    value.collaboration = &collaboration;
    assert!(Aggregate::decode_payload(&postcard::to_stdvec(&value).unwrap()).is_err());
}
#[test]
fn session_v5_hint_is_bounded_inert_storage_not_restore_time_authority() {
    let temp = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let mut value = fixture(&owner);
    // Deliberately unusable/unsigned descriptor. Storage parsing may not make it authoritative
    // or prevent restoring otherwise valid native data because current verification fails.
    let hint = TransportHint {
        kind: tilo_relay::delivery_v3::descriptor::KIND.into(),
        opaque_json: "expired inert descriptor".into(),
        host: owner.host_certificate().clone(),
        service_public_key: [0; 32],
        host_signature: [0; 64],
    };
    value.transport_hint = Some(postcard::to_stdvec(&hint).unwrap());
    let bytes = postcard::to_stdvec(&value).unwrap();
    assert_eq!(
        Aggregate::decode_payload(&bytes)
            .unwrap()
            .encode_payload()
            .unwrap(),
        bytes
    );
    value.transport_hint.as_mut().unwrap().push(0);
    assert!(Aggregate::decode_payload(&postcard::to_stdvec(&value).unwrap()).is_err());
    value.transport_hint = Some(vec![
        0;
        tilo_relay::delivery_v3::descriptor::MAX_HINT_BYTES + 1
    ]);
    assert!(Aggregate::decode_payload(&postcard::to_stdvec(&value).unwrap()).is_err());
}
#[test]
fn session_v5_codec_rejects_future_mismatched_and_noncanonical_storage() {
    let temp = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let value = fixture(&owner);
    let bytes = postcard::to_stdvec(&value).unwrap();
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(Aggregate::decode_payload(&trailing).is_err());
    let mut overlong = vec![0x85, 0];
    overlong.extend_from_slice(&bytes[1..]);
    assert!(Aggregate::decode_payload(&overlong).is_err());
    let mut value = fixture(&owner);
    value.generation += 1;
    assert!(Aggregate::decode_payload(&postcard::to_stdvec(&value).unwrap()).is_err());
    value.generation -= 1;
    value.delivery[0] = 2;
    assert!(Aggregate::decode_payload(&postcard::to_stdvec(&value).unwrap()).is_err());
    value.delivery = vec![0; tilo_relay::delivery_v3::commit::MAX_LEDGER_BYTES + 1];
    assert!(Aggregate::decode_payload(&postcard::to_stdvec(&value).unwrap()).is_err());
}
#[test]
fn session_v5_preflight_counts_signature_and_varints_before_output_allocation() {
    for n in [0, 1, 127, 128, 16383, 16384] {
        let bytes = vec![9u8; n];
        assert_eq!(
            blob_size(n).unwrap(),
            postcard::to_stdvec(&bytes).unwrap().len()
        );
    }
    assert!(blob_size(usize::MAX).is_err());
    let cap = crate::store::MAX_AGGREGATE_BYTES;
    assert!(check_signed_size(cap).is_err());
    assert!(check_signed_size(cap - 69).is_ok());
    assert!(check_signed_size(cap - 68).is_err());
    let encoded = SignedAggregate {
        payload: vec![0; 128],
        signature: vec![0; 64],
    };
    assert_eq!(
        postcard::to_stdvec(&encoded).unwrap().len(),
        blob_size(128).unwrap() + blob_size(64).unwrap()
    );
}
#[test]
fn session_v5_declared_root_count_is_rejected_before_elements_are_read() {
    let mut bytes = vec![1]; // baseline generation
    bytes.extend(postcard::to_stdvec(&(MAX_REFERENCES as u64 + 1)).unwrap());
    let problem = postcard::from_bytes::<PublicationRootsV1>(&bytes)
        .err()
        .unwrap();
    assert_eq!(
        problem,
        postcard::Error::SerdeDeCustom,
        "cardinality rejected before truncated elements"
    );
}
#[test]
fn session_v5_v4_open_flush_and_mutation_keep_writer_version_four() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    owner.queue_pending("first", b"retained", true).unwrap();
    let before = std::fs::read(path.join("session.bin")).unwrap();
    owner.flush().unwrap();
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), before);
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    owner.queue_pending("second", b"new", false).unwrap();
    let signed: SignedAggregate =
        postcard::from_bytes(&std::fs::read(path.join("session.bin")).unwrap()).unwrap();
    assert_eq!(signed.payload[0], 4);
    assert_eq!(owner.pending()[0].encoded_request, b"retained");
    assert!(owner.permits_publication());
}

fn complete_literal_fixture(old: &Aggregate) -> FixtureV5<'_> {
    use tilo_relay::delivery_v3::commit::*;
    let mut value = fixture_from(old);
    let r = value.publication.baseline[0].clone();
    value.publication.baseline.push(r.clone()); // two references, one retained payload allocation
    value.publication.tail.push(PublicationRef {
        payload: r.payload,
        kind: Retention::Delta,
    });
    let committed = CommitRef {
        sequence: old.generation,
        digest: [2; 32],
    };
    let mut ledger = DeliveryLedgerV1::decode(&value.delivery).unwrap();
    ledger.replay.insert(
        old.generation,
        ReplayCommit {
            owner_generation: old.generation,
            previous: None,
            digest: committed.digest,
            publications: vec![r.clone()],
            admitted_operations: vec![[3; 32]],
            committed_at_ns: 100,
        },
    );
    ledger.admissions.insert(
        ReceiptKey {
            device: [7; 32],
            delivery: [4; 16],
        },
        AdmissionRecord {
            digest: [5; 32],
            operations: vec![[3; 32]],
            committed: committed.clone(),
            result: DurableAdmissionKind::Accepted,
        },
    );
    ledger.consumers.insert(
        [7; 32],
        ConsumerState {
            id: [6; 16],
            device: [7; 32],
            root: [8; 32],
            authorization_view: [9; 32],
            acknowledged: Some(committed.clone()),
        },
    );
    ledger.pins.insert(
        [10; 16],
        SnapshotPin {
            consumer: [6; 16],
            authorization_view: [9; 32],
            watermark: committed,
            publications: vec![r],
            expires_at_ns: 200,
        },
    );
    value.delivery = ledger.encode().unwrap();
    value.transport_hint = Some(
        postcard::to_stdvec(&TransportHint {
            kind: "stored".into(),
            opaque_json: "inert".into(),
            host: old.authority.clone(),
            service_public_key: [14; 32],
            host_signature: [15; 64],
        })
        .unwrap(),
    );
    value
}
fn literal_base() -> Aggregate {
    let mut old =
        Aggregate::decode_payload(&hex::decode(include_str!("fixtures/v4.hex").trim()).unwrap())
            .unwrap();
    *old.legacy_mut().unwrap().0 = vec![
        postcard::to_stdvec(&NeighbourMsg::SyncResp {
            room: "r".into(),
            changes: vec![],
        })
        .unwrap(),
    ];
    old.collaboration.administration[0].media_result = Some(media_result());
    old
}
#[test]
fn session_v5_literal_complete_abi_and_replay_pin_roots_preserve_payload() {
    let old = literal_base();
    let value = complete_literal_fixture(&old);
    let bytes = postcard::to_stdvec(&value).unwrap();
    assert_eq!(
        hex::encode(&bytes),
        include_str!("fixtures/v5.hex").trim(),
        "approved complete V5 field order"
    );
    let mut decoded = Aggregate::decode_payload(&bytes).unwrap();
    assert_eq!(decoded.encode_payload().unwrap(), bytes);
    assert!(
        postcard::from_bytes::<AggregateV4>(&bytes).is_err(),
        "old reader cannot silently consume V5"
    );
    assert_eq!(decoded.outgoing().len(), 3);
    assert_eq!(
        decoded.outgoing().get(0).unwrap().bytes.as_ptr(),
        decoded.outgoing().get(1).unwrap().bytes.as_ptr()
    );
    let PublicationStorage::V5 {
        pool,
        roots,
        delivery,
        ..
    } = &mut decoded.publication
    else {
        unreachable!()
    };
    let payload = roots.baseline[0].payload;
    roots.baseline.clear();
    roots.tail.clear();
    assert_eq!(
        pool.reference_counts(&[], delivery).unwrap()[&payload],
        2,
        "replay and pin survive queue replacement"
    );
    assert_eq!(pool.len(), 1);
    decoded.validate().unwrap();
}
#[test]
fn session_v5_combined_reference_budget_rejects_tail_before_element_read() {
    // One baseline ref leaves MAX_REFERENCES-1 slots. A MAX_REFERENCES tail is invalid.
    let r = PublicationRef {
        payload: [1; 32],
        kind: Retention::Checkpoint,
    };
    let mut bytes = postcard::to_stdvec(&1u64).unwrap();
    bytes.extend(postcard::to_stdvec(&vec![r]).unwrap());
    bytes.extend(postcard::to_stdvec(&(MAX_REFERENCES as u64)).unwrap());
    bytes.resize(bytes.len() + MAX_REFERENCES, 0); // enough input for Postcard's count hint, no valid tail
    assert_eq!(
        postcard::from_bytes::<PublicationRootsV1>(&bytes)
            .err()
            .unwrap(),
        postcard::Error::SerdeDeCustom
    );
}

#[test]
fn session_v5_rejects_trailing_signed_envelope_without_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let mut bytes = signed_fixture(&owner, &fixture(&owner));
    bytes.push(0);
    owner.store.commit(&bytes).unwrap();
    drop(owner);
    assert!(
        SessionOwner::open(&path).is_err(),
        "V5 outer envelope must be exact too"
    );
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
}

fn completed_join_with_membership_challenge() -> (
    tempfile::TempDir,
    SessionOwner,
    crate::collaboration::CollaborationService,
    [u8; 32],
    qos_peer_state::join::JoinCompleteRequest,
    qos_peer_state::join::MembershipCompleteRequest,
    u64,
) {
    use qos_peer_state::join::*;
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let now = tilo_relay::clock::now_ns();
    let root = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
    let endpoint = device.verifying_key().to_bytes();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: endpoint,
        device_label: "guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now - 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let service = crate::collaboration::CollaborationService::new(vec![]);
    let administrator = owner.peer().authority().device_public_key;
    let issued = service
        .issue(
            &mut owner,
            administrator,
            crate::collaboration::IssueInvitationRequest {
                request_id: [41; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(cert.user_public_key),
                invitee_name: Some("guest".into()),
                expires_at_ns: now + 120_000_000_000,
            },
            now,
        )
        .unwrap();
    let challenge = service
        .challenge(
            &mut owner,
            endpoint,
            JoinChallengeRequest {
                signed_invitation: issued.signed_invitation,
                device_cert: cert.clone(),
                request_id: [42; 32],
            },
            now + 1,
        )
        .unwrap()
        .challenge;
    let complete = JoinCompleteRequest {
        proof: sign_join_proof(challenge, &device).unwrap(),
    };
    let joined = service
        .complete(&mut owner, endpoint, complete.clone(), now + 2)
        .unwrap();
    assert!(!joined.bootstrap.governance_messages.is_empty());
    let room = owner.peer().room().to_owned();
    let workspace_id = owner.workspace_id();
    let challenge = service
        .membership_challenge(
            &mut owner,
            endpoint,
            MembershipChallengeRequest {
                room,
                workspace_id,
                device_cert: cert,
                request_id: [43; 32],
            },
            now + 3,
        )
        .unwrap()
        .challenge;
    let membership = MembershipCompleteRequest {
        proof: sign_membership_proof(challenge, &device).unwrap(),
    };
    assert!(
        !service
            .membership_complete(&owner, endpoint, membership.clone(), now + 4)
            .unwrap()
            .bootstrap
            .governance_messages
            .is_empty()
    );
    (
        temp,
        owner,
        service,
        endpoint,
        complete,
        membership,
        now + 5,
    )
}
fn restore_fixture_v5(mut owner: SessionOwner) -> SessionOwner {
    let path = owner.root().to_owned();
    let bytes = signed_fixture(&owner, &fixture(&owner));
    owner.store.commit(&bytes).unwrap();
    drop(owner);
    SessionOwner::open(&path).unwrap()
}
#[test]
fn session_v5_completed_join_retry_cannot_publish_through_recovery_turn() {
    let (_temp, owner, service, endpoint, complete, _, now) =
        completed_join_with_membership_challenge();
    let mut owner = restore_fixture_v5(owner);
    let before = owner.status();
    let authority = crate::committed_authority::CommittedAuthority::new(&owner);
    let result = authority
        .with_owner(&mut owner, |owner| {
            service.complete(owner, endpoint, complete, now)
        })
        .unwrap();
    assert!(
        result.is_err(),
        "stored V4 completion must not bypass V5 read-only publication gate"
    );
    assert!(result.err().unwrap().to_string().contains("read-only"));
    assert_eq!(owner.status(), before);
    assert!(authority.read().is_err());
}
#[test]
fn session_v5_membership_refresh_cannot_publish_through_recovery_turn() {
    let (_temp, owner, service, endpoint, _, membership, now) =
        completed_join_with_membership_challenge();
    let mut owner = restore_fixture_v5(owner);
    let before = owner.status();
    let authority = crate::committed_authority::CommittedAuthority::new(&owner);
    let result = authority
        .with_owner(&mut owner, |owner| {
            service.membership_complete(owner, endpoint, membership, now)
        })
        .unwrap();
    assert!(
        result.is_err(),
        "persisted membership proof must not return current V5 bootstrap"
    );
    assert!(result.err().unwrap().to_string().contains("read-only"));
    assert_eq!(owner.status(), before);
}

fn enrolled_fixture() -> (
    tempfile::TempDir,
    SessionOwner,
    crate::enrollment::EnrollmentAuthority,
    DeviceCert,
    qos_peer_state::enrollment::CompleteRequest,
    qos_peer_state::enrollment::ChallengeRequest,
    u64,
) {
    use qos_peer_state::enrollment::*;
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[60; 32]);
    let request = ChallengeRequest {
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        request_id: hex::encode([61; 32]),
    };
    let now = tilo_relay::clock::now_ns();
    let mut service = crate::enrollment::EnrollmentAuthority::default();
    let first = service.challenge(&owner, request.clone(), now).unwrap();
    let certificate = service
        .complete(
            &mut owner,
            sign(&decode(&first.challenge).unwrap(), &key).unwrap(),
            now + 1,
        )
        .unwrap();
    let second = service.challenge(&owner, request.clone(), now + 2).unwrap();
    let proof = sign(&decode(&second.challenge).unwrap(), &key).unwrap();
    (
        temp,
        owner,
        service,
        certificate,
        proof,
        request,
        tilo_relay::clock::now_ns(),
    )
}
#[test]
fn session_v5_enrollment_duplicate_cannot_reply_through_recovery_turn() {
    let (_temp, owner, mut service, _, proof, _, now) = enrolled_fixture();
    let mut owner = restore_fixture_v5(owner);
    let before = owner.status();
    let authority = crate::committed_authority::CommittedAuthority::new(&owner);
    let result = authority
        .with_owner(&mut owner, |owner| service.complete(owner, proof, now))
        .unwrap();
    assert!(
        result.is_err(),
        "same-request enrollment must not return a V5 authority bootstrap"
    );
    assert!(result.err().unwrap().to_string().contains("read-only"));
    assert_eq!(owner.status(), before);
}
#[test]
fn session_v5_enrollment_public_bootstrap_is_not_an_inspection_escape() {
    let (_temp, owner, _, cert, _, _, now) = enrolled_fixture();
    crate::enrollment::public_bootstrap(&owner, &cert, serde_json::Value::Null, "packet", now)
        .expect("V4 bootstrap fixture must work");
    let owner = restore_fixture_v5(owner);
    let result =
        crate::enrollment::public_bootstrap(&owner, &cert, serde_json::Value::Null, "packet", now);
    assert!(
        result.is_err(),
        "V5 may not sign/disclose a new directory bootstrap"
    );
    assert!(result.err().unwrap().to_string().contains("read-only"));
}
#[test]
fn session_v5_enrollment_challenge_is_not_issued() {
    let (_temp, owner, mut service, _, _, request, now) = enrolled_fixture();
    let owner = restore_fixture_v5(owner);
    let result = service.challenge(&owner, request, now);
    assert!(result.is_err(), "V5 may not begin an active enrollment");
    assert!(result.err().unwrap().to_string().contains("read-only"));
}
