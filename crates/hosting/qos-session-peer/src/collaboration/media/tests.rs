use super::*;
use qos_action_broker::collaboration_admin::*;
use qos_peer_state::{NeighbourMsg, TransportOrigin, room::media};
use ui::automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};

fn fixture() -> (tempfile::TempDir, SessionOwner) {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    owner.activate_delivery().unwrap();
    add_channel(&mut owner, "voice");
    (dir, owner)
}
fn add_channel(owner: &mut SessionOwner, channel: &str) {
    let mut peer = owner.peer().clone();
    let mut chat = AutoCommit::load(&peer.document("dev.qos.chat", "").unwrap()).unwrap();
    let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
    let object = chat.put_object(&channels, channel, ObjType::Map).unwrap();
    chat.put(&object, "kind", "voice").unwrap();
    chat.put(&object, "workspace", "home").unwrap();
    peer.publish_document(
        "dev.qos.chat",
        "",
        chat.save(),
        &owner.identity.native,
        SystemTime::now(),
    )
    .unwrap();
    owner
        .commit_mutation_candidate(peer, owner.directory().to_vec())
        .unwrap();
}
fn request(owner: &SessionOwner, operation: Operation) -> CollaborationAdminRequest {
    CollaborationAdminRequest {
        room: owner.peer().room().into(),
        invocation: tilo_actions::InvocationId(uuid::Uuid::new_v4()),
        caller: owner.peer().authority().clone(),
        operation,
    }
}
fn create(owner: &SessionOwner) -> CollaborationAdminRequest {
    request(
        owner,
        Operation::MediaCreate(MediaCreateInput {
            app_id: "dev.qos.chat".into(),
            record_id: "".into(),
            channel_id: "voice".into(),
            expected_authorization_revision: None,
            allowed: MediaDirectionsInput {
                publish: true,
                subscribe: true,
            },
        }),
    )
}
fn run(
    owner: &mut SessionOwner,
    request: &CollaborationAdminRequest,
) -> Result<MediaCallResultV1, Error> {
    CollaborationService::new(vec![]).media_from_action(owner, request, current_time_ns())
}
fn call_input(result: &MediaCallResultV1) -> MediaCallInput {
    MediaCallInput {
        call_id: hex::encode(result.call_id),
        expected_authorization_revision: result.authorization_revision,
        expected_epoch: result.epoch,
    }
}
fn policy(owner: &SessionOwner) -> media::RoomMediaV1 {
    owner.peer().validated_media_policy().unwrap().unwrap()
}

#[test]
fn room_media_owner_reconciles_independent_verified_revocation_before_publication() {
    let (_dir, mut owner) = fixture();
    let (root, _, cert) = crate::room_media_policy::tests::member(
        &mut owner,
        qos_peer_state::join::MemberRole::Editor,
        None,
    );
    let r = create(&owner);
    let created = run(&mut owner, &r).unwrap();
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: [88; 32],
        revoked_at_ns: 2,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    let before = owner.status().generation;
    owner
        .receive(
            TransportOrigin::UntrustedPresence(cert.device_public_key),
            NeighbourMsg::IdentityHello {
                cert: Box::new(cert),
                revocations: vec![entry],
            },
        )
        .unwrap();
    let current = owner
        .peer()
        .validated_media_policy()
        .expect("owner must reconcile before publication")
        .unwrap();
    assert_eq!(
        current.authorization_revision,
        created.authorization_revision + 1
    );
    assert_eq!(current.calls[&created.call_id].epoch, created.epoch);
    assert_eq!(owner.status().generation, before + 1);
}

#[test]
fn room_media_owner_actions_retry_reopen_and_tombstone_lifecycle() {
    let (dir, mut owner) = fixture();
    let r = create(&owner);
    let created = run(&mut owner, &r).unwrap();
    assert_eq!(created.epoch, 1);
    assert_eq!(created.authorization_revision, 1);
    let generation = owner.status().generation;
    assert_eq!(run(&mut owner, &r).unwrap(), created);
    assert_eq!(owner.status().generation, generation);
    let end = request(&owner, Operation::MediaEnd(call_input(&created)));
    let ended = run(&mut owner, &end).unwrap();
    assert_eq!(ended.epoch, 2);
    assert_eq!(ended.phase, media::CallPhaseV1::Ended);
    assert_eq!(run(&mut owner, &r).unwrap(), created);
    let mut duplicate = create(&owner);
    if let Operation::MediaCreate(v) = &mut duplicate.operation {
        v.expected_authorization_revision = Some(1);
    }
    assert!(matches!(
        run(&mut owner, &duplicate),
        Err(Error::Semantic {
            code: MediaRejectionCode::AlreadyExists,
            ..
        })
    ));
    drop(owner);
    let mut owner = SessionOwner::open(&dir.path().join("owner")).unwrap();
    assert_eq!(run(&mut owner, &r).unwrap(), created);
    let restart = request(&owner, Operation::MediaRestart(call_input(&ended)));
    let restarted = run(&mut owner, &restart).unwrap();
    assert_eq!(restarted.epoch, 3);
    assert_eq!(restarted.binding, created.binding);
    assert_eq!(policy(&owner).calls.len(), 1);
    let mut reused = r;
    reused.operation = Operation::MediaEnd(call_input(&restarted));
    assert!(matches!(run(&mut owner, &reused), Err(Error::Rejected(_))));
}

#[test]
fn room_media_owner_moderation_noops_and_denial_reversal_preserve_exact_counters() {
    let (_dir, mut owner) = fixture();
    let r = create(&owner);
    let mut result = run(&mut owner, &r).unwrap();
    for (publish, subscribe, expected) in [
        (true, true, 1),
        (false, true, 2),
        (false, true, 2),
        (true, true, 3),
    ] {
        let r = request(
            &owner,
            Operation::MediaSetDirections(MediaDirectionsChangeInput {
                call_id: hex::encode(result.call_id),
                expected_authorization_revision: 1,
                expected_epoch: result.epoch,
                allowed: MediaDirectionsInput { publish, subscribe },
            }),
        );
        result = run(&mut owner, &r).unwrap();
        assert_eq!(result.epoch, expected);
        assert_eq!(result.authorization_revision, 1);
    }
    for (denied, expected) in [(true, 4), (true, 4), (false, 5), (false, 5)] {
        let r = request(
            &owner,
            Operation::MediaSetParticipantDenial(MediaDenialInput {
                call_id: hex::encode(result.call_id),
                expected_authorization_revision: 1,
                expected_epoch: result.epoch,
                person_root: hex::encode([3; 32]),
                denied: MediaDirectionsInput {
                    publish: denied,
                    subscribe: false,
                },
            }),
        );
        result = run(&mut owner, &r).unwrap();
        assert_eq!(result.epoch, expected);
    }
    assert!(policy(&owner).calls[&result.call_id].denied.is_empty());
}

#[test]
fn room_media_owner_requires_operational_v5_and_exact_current_owner_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let mut legacy = SessionOwner::initialize(&dir.path().join("legacy")).unwrap();
    let r = create(&legacy);
    let before = legacy.status().generation;
    assert!(matches!(run(&mut legacy, &r), Err(Error::Rejected(_))));
    assert_eq!(legacy.status().generation, before);
    assert!(!legacy.peer().admission_v5_selected());
    let (_dir, mut owner) = fixture();
    let mut r = create(&owner);
    r.caller = owner.host_certificate().clone();
    assert!(matches!(run(&mut owner, &r), Err(Error::Rejected(_))));
    r = create(&owner);
    r.caller.device_label.push('x');
    assert!(matches!(run(&mut owner, &r), Err(Error::Rejected(_))));
    let (_, _, member) = crate::room_media_policy::tests::member(
        &mut owner,
        qos_peer_state::join::MemberRole::Editor,
        None,
    );
    r = create(&owner);
    r.caller = member;
    assert!(matches!(run(&mut owner, &r), Err(Error::Rejected(_))));
    assert!(owner.peer().validated_media_policy().unwrap().is_none());
}

#[test]
fn room_media_owner_preconditions_binding_and_uncertain_barrier_are_atomic() {
    let (_dir, mut owner) = fixture();
    let mut r = create(&owner);
    if let Operation::MediaCreate(v) = &mut r.operation {
        v.record_id = "other".into();
    }
    let generation = owner.status().generation;
    assert!(matches!(
        run(&mut owner, &r),
        Err(Error::Semantic {
            code: MediaRejectionCode::Binding,
            ..
        })
    ));
    r = create(&owner);
    if let Operation::MediaCreate(v) = &mut r.operation {
        v.expected_authorization_revision = Some(1);
    }
    assert!(matches!(
        run(&mut owner, &r),
        Err(Error::Semantic {
            code: MediaRejectionCode::Stale,
            ..
        })
    ));
    assert_eq!(owner.status().generation, generation);
    let root = owner.root().to_owned();
    let r = create(&owner);
    let result = crate::store::test_fault::with_fault(
        &root,
        crate::store::test_fault::Point::BeforeWrite,
        || run(&mut owner, &r),
    );
    assert!(matches!(result, Err(Error::Uncertain(_))));
    assert!(!owner.durability_healthy());
    assert!(owner.peer().validated_media_policy().unwrap().is_none());
}

#[test]
fn room_media_owner_capacity_and_epoch_exhaustion_do_not_delete_tombstones() {
    let (_dir, mut owner) = fixture();
    let r = create(&owner);
    let created = run(&mut owner, &r).unwrap();
    crate::room_media_policy::tests::edit_policy(&mut owner, |p| {
        p.calls.get_mut(&created.call_id).unwrap().epoch = u64::MAX;
        for n in 1..64u8 {
            let mut call = p.calls[&created.call_id].clone();
            call.phase = media::CallPhaseV1::Ended;
            call.epoch = 2;
            call.binding.channel_id = format!("ended-{n}");
            call.binding.channel_object = format!("{}@ab", n + 1);
            p.calls.insert([n; 32], call);
        }
    });
    let generation = owner.status().generation;
    let current = policy(&owner);
    assert_eq!(current.calls.len(), 64);
    let end = request(
        &owner,
        Operation::MediaEnd(MediaCallInput {
            call_id: hex::encode(created.call_id),
            expected_authorization_revision: 1,
            expected_epoch: u64::MAX,
        }),
    );
    assert!(matches!(
        run(&mut owner, &end),
        Err(Error::Semantic {
            code: MediaRejectionCode::Exhausted,
            ..
        })
    ));
    assert_eq!(owner.status().generation, generation);
    assert_eq!(policy(&owner), current);
    add_channel(&mut owner, "additional");
    let mut r = create(&owner);
    if let Operation::MediaCreate(v) = &mut r.operation {
        v.channel_id = "additional".into();
        v.expected_authorization_revision = Some(1);
    }
    assert!(matches!(
        run(&mut owner, &r),
        Err(Error::Semantic {
            code: MediaRejectionCode::Capacity,
            ..
        })
    ));
    assert_eq!(policy(&owner).calls.len(), 64);
}

#[test]
fn room_media_owner_exact_record_binding_and_end_before_archive() {
    let (_dir, mut owner) = fixture();
    let mut peer = owner.peer().clone();
    let bytes = peer.document("dev.qos.chat", "").unwrap();
    peer.publish_document(
        "dev.qos.chat",
        "second",
        bytes,
        &owner.identity.native,
        SystemTime::now(),
    )
    .unwrap();
    owner
        .commit_mutation_candidate(peer, owner.directory().to_vec())
        .unwrap();
    let r = create(&owner);
    let first = run(&mut owner, &r).unwrap();
    let mut r = create(&owner);
    if let Operation::MediaCreate(v) = &mut r.operation {
        v.record_id = "second".into();
        v.expected_authorization_revision = Some(1);
    }
    let second = run(&mut owner, &r).unwrap();
    assert_ne!(first.call_id, second.call_id);
    assert_ne!(first.binding.record_id, second.binding.record_id);
    let mut peer = owner.peer().clone();
    let mut chat = AutoCommit::load(&peer.document("dev.qos.chat", "").unwrap()).unwrap();
    let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
    let object = chat.get(&channels, "voice").unwrap().unwrap().1;
    chat.put(&object, "archived", true).unwrap();
    let bytes = chat.save();
    assert!(
        peer.publish_document(
            "dev.qos.chat",
            "",
            bytes.clone(),
            &owner.identity.native,
            SystemTime::now()
        )
        .is_err()
    );
    let r = request(&owner, Operation::MediaEnd(call_input(&first)));
    let ended = run(&mut owner, &r).unwrap();
    let mut peer = owner.peer().clone();
    peer.publish_document(
        "dev.qos.chat",
        "",
        bytes,
        &owner.identity.native,
        SystemTime::now(),
    )
    .unwrap();
    owner
        .commit_mutation_candidate(peer, owner.directory().to_vec())
        .unwrap();
    let r = request(&owner, Operation::MediaRestart(call_input(&ended)));
    assert!(matches!(
        run(&mut owner, &r),
        Err(Error::Semantic {
            code: MediaRejectionCode::Binding,
            ..
        })
    ));
    assert_eq!(policy(&owner).calls[&second.call_id].epoch, 1);
}

#[test]
fn room_media_owner_max_revision_suspends_durably_without_new_replay_or_receipt() {
    use crate::{
        committed_authority::CommittedAuthority,
        media_authority::{GrantRequest, MediaCallPolicy, MediaDirections},
    };
    let (dir, mut owner) = fixture();
    let (root, _, cert) = crate::room_media_policy::tests::member(
        &mut owner,
        qos_peer_state::join::MemberRole::Editor,
        None,
    );
    let r = create(&owner);
    let created = run(&mut owner, &r).unwrap();
    crate::room_media_policy::tests::edit_policy(&mut owner, |p| {
        p.authorization_revision = u64::MAX
    });
    let committed = CommittedAuthority::new(&owner);
    let request = GrantRequest {
        device: owner.peer().authority().device_public_key,
        room: owner.peer().room().into(),
        call_id: hex::encode(created.call_id),
        participation_id: [1; 32],
        generation: 1,
        directions: MediaDirections {
            publish: true,
            subscribe: true,
        },
    };
    {
        let view = committed.read().unwrap();
        assert!(
            view.media_policy()
                .current(view.peer(), &request, SystemTime::now())
                .is_ok()
        );
    }
    let ledger = owner.delivery_ledger().unwrap().clone();
    let outgoing = owner
        .outgoing()
        .iter()
        .map(|p| (p.bytes.to_vec(), p.kind))
        .collect::<Vec<_>>();
    let before = owner.status().generation;
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: [88; 32],
        revoked_at_ns: 2,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    committed
        .with_owner(&mut owner, |owner| {
            owner.receive(
                TransportOrigin::UntrustedPresence(cert.device_public_key),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(cert),
                    revocations: vec![entry],
                },
            )
        })
        .unwrap()
        .unwrap();
    assert_eq!(owner.status().generation, before + 1);
    let after = owner.delivery_ledger().unwrap();
    assert_eq!(after.resolved_through, before + 1);
    assert_eq!(after.replay, ledger.replay);
    assert_eq!(after.admissions, ledger.admissions);
    assert_eq!(
        owner
            .outgoing()
            .iter()
            .map(|p| (p.bytes.to_vec(), p.kind))
            .collect::<Vec<_>>(),
        outgoing
    );
    {
        let view = committed.read().unwrap();
        assert!(
            view.media_policy()
                .current(view.peer(), &request, SystemTime::now())
                .is_err()
        );
        assert!(
            view.peer()
                .permissions(request.device, SystemTime::now())
                .is_ok()
        );
    }
    let suspended_generation = owner.status().generation;
    let source = owner.peer().authority().device_public_key;
    let room = owner.peer().room().to_string();
    assert!(
        owner
            .receive(
                TransportOrigin::RelayDelta(source),
                NeighbourMsg::SyncReq {
                    since: vec![],
                    room,
                    invite_id: None
                }
            )
            .is_err(),
        "suspended basis must not mint a new bootstrap"
    );
    assert_eq!(owner.status().generation, suspended_generation);
    assert_eq!(
        owner
            .outgoing()
            .iter()
            .map(|p| (p.bytes.to_vec(), p.kind))
            .collect::<Vec<_>>(),
        outgoing
    );
    drop(owner);
    let owner = SessionOwner::open(&dir.path().join("owner")).unwrap();
    assert!(owner.peer().validated_media_policy().is_err());
    assert_eq!(
        media::parse(owner.peer().admitted_room().unwrap())
            .unwrap()
            .unwrap()
            .authorization_revision,
        u64::MAX
    );
}

#[test]
fn room_media_owner_suspended_metadata_barrier_failure_poison_denies_old_view() {
    use crate::committed_authority::{CommittedAuthority, Observation};
    let (_dir, mut owner) = fixture();
    let (root, _, cert) = crate::room_media_policy::tests::member(
        &mut owner,
        qos_peer_state::join::MemberRole::Editor,
        None,
    );
    let r = create(&owner);
    run(&mut owner, &r).unwrap();
    crate::room_media_policy::tests::edit_policy(&mut owner, |p| {
        p.authorization_revision = u64::MAX
    });
    let committed = CommittedAuthority::new(&owner);
    let path = owner.root().to_owned();
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: [88; 32],
        revoked_at_ns: 2,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    let result = crate::store::test_fault::with_fault(
        &path,
        crate::store::test_fault::Point::BeforeWrite,
        || {
            committed.with_owner(&mut owner, |owner| {
                owner.receive(
                    TransportOrigin::UntrustedPresence(cert.device_public_key),
                    NeighbourMsg::IdentityHello {
                        cert: Box::new(cert),
                        revocations: vec![entry],
                    },
                )
            })
        },
    );
    assert!(result.is_err());
    assert!(!owner.durability_healthy());
    assert!(matches!(committed.observe(), Observation::Denied));
}

#[test]
fn room_media_owner_denial_limits_and_removal_allow_bounded_replacement() {
    for total in [false, true] {
        let (_dir, mut owner) = fixture();
        let r = create(&owner);
        let created = run(&mut owner, &r).unwrap();
        crate::room_media_policy::tests::edit_policy(&mut owner, |p| {
            let denial: std::collections::BTreeMap<_, _> = (1..=128u8)
                .map(|n| {
                    (
                        [n; 32],
                        media::DirectionsV1 {
                            publish: true,
                            subscribe: false,
                        },
                    )
                })
                .collect();
            if total {
                for n in 1..=4u8 {
                    let mut call = p.calls[&created.call_id].clone();
                    call.phase = media::CallPhaseV1::Ended;
                    call.epoch = 2;
                    call.binding.channel_id = format!("ended-{n}");
                    call.binding.channel_object = format!("{}@ab", n + 1);
                    call.denied = denial.clone();
                    p.calls.insert([n; 32], call);
                }
            } else {
                let call = p.calls.get_mut(&created.call_id).unwrap();
                call.denied = denial;
                call.epoch = 2;
            }
        });
        let mut result = MediaCallResultV1 {
            epoch: if total { 1 } else { 2 },
            ..created
        };
        let operation = |result: &MediaCallResultV1, root: u8, denied: bool| {
            Operation::MediaSetParticipantDenial(MediaDenialInput {
                call_id: hex::encode(result.call_id),
                expected_authorization_revision: 1,
                expected_epoch: result.epoch,
                person_root: hex::encode([root; 32]),
                denied: MediaDirectionsInput {
                    publish: denied,
                    subscribe: false,
                },
            })
        };
        let r = request(&owner, operation(&result, 200, true));
        let before = owner.status().generation;
        assert!(matches!(
            run(&mut owner, &r),
            Err(Error::Semantic {
                code: MediaRejectionCode::Capacity,
                ..
            })
        ));
        assert_eq!(owner.status().generation, before);
        if !total {
            let r = request(&owner, operation(&result, 1, false));
            result = run(&mut owner, &r).unwrap();
            let r = request(&owner, operation(&result, 200, true));
            run(&mut owner, &r).unwrap();
            assert_eq!(policy(&owner).calls[&result.call_id].denied.len(), 128);
        }
    }
}

#[test]
fn room_media_owner_current_room_admin_restriction_blocks_even_exact_retry() {
    use collab_types::{
        Ed25519PublicKey,
        caps::{CapOverride, Effect, Principal, Scope, Subject},
    };
    let (_dir, mut owner) = fixture();
    let r = create(&owner);
    run(&mut owner, &r).unwrap();
    let mut room = owner.peer().admitted_room().unwrap().clone();
    let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    let restriction = CapOverride::new(
        Subject::Principal(Principal::person_from_key(Ed25519PublicKey::from_bytes(
            owner.peer().authority().user_public_key,
        ))),
        Scope::Room(owner.peer().room().into()),
        Effect::Restrict,
        Verb::Read,
    );
    room.put(
        &caps,
        "owner-admin-restriction",
        ui::automerge::ScalarValue::Bytes(serde_json::to_vec(&restriction).unwrap()),
    )
    .unwrap();
    media::refresh_authorization(owner.peer().admitted_room().unwrap(), &mut room).unwrap();
    crate::room_media_policy::tests::commit_room(&mut owner, room);
    assert!(matches!(run(&mut owner, &r), Err(Error::Rejected(_))));
}

fn enroll_owner_browser(owner: &mut SessionOwner, seed: u8) -> tilo_user_identity::DeviceCert {
    use qos_peer_state::enrollment;
    let now_ns = current_time_ns();
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut authority = crate::enrollment::EnrollmentAuthority::default();
    let challenge = authority
        .challenge(
            owner,
            enrollment::ChallengeRequest {
                device_public_key: hex::encode(key.verifying_key().to_bytes()),
                request_id: hex::encode([seed; 32]),
            },
            now_ns,
        )
        .unwrap();
    let proof = enrollment::sign(&enrollment::decode(&challenge.challenge).unwrap(), &key).unwrap();
    authority.complete(owner, proof, now_ns).unwrap()
}

#[test]
fn room_media_owner_enrolled_browser_device_administers_calls_but_unenrolled_one_cannot() {
    let (_dir, mut owner) = fixture();
    let browser = enroll_owner_browser(&mut owner, 61);
    assert_eq!(
        browser.user_public_key,
        owner.peer().authority().user_public_key
    );
    let mut r = create(&owner);
    r.caller = browser.clone();
    let created = run(&mut owner, &r).unwrap();
    assert_eq!(created.phase, media::CallPhaseV1::Active);
    // The same enrolled device moderates (participant denial) the call.
    let mut denial = request(
        &owner,
        Operation::MediaSetParticipantDenial(MediaDenialInput {
            call_id: hex::encode(created.call_id),
            expected_authorization_revision: created.authorization_revision,
            expected_epoch: created.epoch,
            person_root: hex::encode([3; 32]),
            denied: MediaDirectionsInput {
                publish: true,
                subscribe: true,
            },
        }),
    );
    denial.caller = browser.clone();
    let denied = run(&mut owner, &denial).unwrap();
    assert_eq!(denied.call_id, created.call_id);
    // A same-root certificate without this host's enrollment issuance stays refused.
    let mut forged = browser.clone();
    forged.device_label.push('x');
    let mut r = create(&owner);
    r.caller = forged;
    assert!(matches!(run(&mut owner, &r), Err(Error::Rejected(_))));
}
