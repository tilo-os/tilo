use super::*;
use crate::committed_authority::CommittedAuthority;
use error::{OwnerDataFailure, OwnerDataFailureKind as Kind};
use tilo_relay::delivery_v3::{cursor::CursorToken, state::ResetReason};
fn kind(error: impl Into<anyhow::Error>) -> Kind {
    let error = error.into();
    error
        .downcast_ref::<OwnerDataFailure>()
        .expect("actual owner branch must return a closed typed failure")
        .kind()
}
#[test]
fn delivery_resource_input_and_exact_identity_failures_are_typed() {
    let (_temp, mut owner) = tests::hosted_owner();
    let origin = TransportOrigin::RelayDelta(owner.peer.authority().device_public_key);
    let before = std::fs::read(owner.root().join("session.bin")).unwrap();
    assert_eq!(
        kind(
            owner
                .receive_delivery(
                    origin,
                    CompleteStateItem {
                        delivery_id: [1; 16],
                        canonical: &[255]
                    },
                    SystemTime::now()
                )
                .unwrap_err()
        ),
        Kind::InvalidNativeInput
    );
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        before
    );
    let mut author = owner.peer.clone();
    let bytes = tests::upload(&owner, &mut author, "typed-input");
    owner
        .receive_delivery(
            origin,
            CompleteStateItem {
                delivery_id: [2; 16],
                canonical: &bytes,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert_eq!(
        kind(
            owner
                .receive_delivery(
                    origin,
                    CompleteStateItem {
                        delivery_id: [2; 16],
                        canonical: &[254]
                    },
                    SystemTime::now()
                )
                .unwrap_err()
        ),
        Kind::ExactIdConflict
    );
    let mut conflicting: NeighbourMsg = postcard::from_bytes(&bytes).unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut conflicting else {
        panic!("wrong real native fixture")
    };
    envelope.timestamp += web_time::Duration::from_secs(1);
    collab_types::sign_op(envelope, &owner.identity.native).unwrap();
    let conflicting = postcard::to_stdvec(&conflicting).unwrap();
    assert_eq!(
        kind(
            owner
                .receive_delivery(
                    origin,
                    CompleteStateItem {
                        delivery_id: [3; 16],
                        canonical: &conflicting
                    },
                    SystemTime::now()
                )
                .unwrap_err()
        ),
        Kind::NativeSequenceConflict
    );
}
#[test]
fn delivery_resource_cursor_identity_precedes_typed_reset_lookup() {
    let (_temp, mut owner) = tests::hosted_owner();
    let committed = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &committed);
    let token = tests::register_and_cursor(&mut owner, &committed, &session);
    let good = token
        .authenticate(&owner.identity.host.verifying_key())
        .unwrap()
        .issue()
        .clone();
    let current = committed.read().unwrap();
    let source = owner
        .delivery_source(current.view(), &session, tests::now_ns())
        .unwrap();
    for case in 0..6 {
        let mut issue = good.clone();
        let expected = match case {
            0 => {
                issue.consumer_device = [91; 32];
                issue.committed.sequence = u64::MAX;
                issue.store_generation = [99; 16];
                Kind::CurrentDenied
            }
            1 => {
                issue.store_generation = [92; 16];
                Kind::Reset(ResetReason::StoreRotated)
            }
            2 => {
                issue.authorization_view = [93; 32];
                Kind::Reset(ResetReason::AuthorizationChanged)
            }
            3 => {
                issue.committed.sequence = u64::MAX;
                Kind::Reset(ResetReason::CursorAhead)
            }
            4 => {
                issue.committed.sequence = owner.aggregate.generation;
                Kind::Reset(ResetReason::CursorUnknown)
            }
            _ => {
                issue.committed.digest = [94; 32];
                Kind::Reset(ResetReason::CommitMismatch)
            }
        };
        let token = CursorToken::issue(issue, &owner.identity.host).unwrap();
        let error = match owner.verify_delivery_cursor(&source, &token) {
            Err(e) => e,
            Ok(_) => panic!("invalid cursor case {case} accepted"),
        };
        assert_eq!(kind(error), expected, "cursor case {case}");
    }
}

#[test]
fn delivery_resource_signed_buffers_have_exact_capacity() {
    let (_temp, owner) = tests::hosted_owner();
    let payload = owner.aggregate.encode_payload().unwrap();
    assert_eq!(
        payload.capacity(),
        payload.len(),
        "aggregate output must not grow unreserved spare capacity"
    );
    let envelope = encode(&owner.aggregate, &owner.identity).unwrap();
    assert_eq!(
        envelope.capacity(),
        envelope.len(),
        "signed envelope output must not grow unreserved spare capacity"
    );
    assert_eq!(
        decode(&envelope).unwrap().0.encode_payload().unwrap(),
        payload
    );
}

#[test]
fn delivery_resource_attachment_accounts_existing_bytes_and_rejects_other_pool() {
    let (_temp, mut owner) = tests::hosted_owner();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    let before = budget.available_bytes();
    let existing = owner.aggregate.peer.capacity() + owner.aggregate.directory.capacity();
    owner
        .attach_data_resources(&budget)
        .expect("actual existing owner must acquire serialized custody");
    assert!(budget.available_bytes() <= before - existing);
    let charged = budget.available_bytes();
    owner.attach_data_resources(&budget).unwrap();
    assert_eq!(
        budget.available_bytes(),
        charged,
        "same-pool attachment must not double charge Arc payloads"
    );
    let (other, _receiver) = crate::https::ingress_channel();
    assert!(owner.attach_data_resources(&other.host_bytes()).is_err());
    drop(owner);
    assert_eq!(budget.available_bytes(), before);
}
#[test]
fn delivery_resource_preflight_pressure_does_not_mutate_owner() {
    use crate::owner::resources::OwnerDataOperation;
    let (_temp, mut owner) = tests::hosted_owner();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    owner.attach_data_resources(&budget).unwrap();
    let original = std::fs::read(owner.root().join("session.bin")).unwrap();
    let generation = owner.status().generation;
    let held = budget
        .try_acquire_bulk(budget.available_bytes() - crate::host_bytes::CONTROL_RESERVE_BYTES)
        .unwrap();
    let error = match owner.prepare_data_turn(&budget, OwnerDataOperation::Ack) {
        Err(error) => error,
        Ok(_) => panic!("bulk pressure admitted owner allocation"),
    };
    assert_eq!(
        error.kind(),
        Kind::Capacity(error::OwnerCapacityKind::Bytes)
    );
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        original
    );
    assert_eq!(owner.status().generation, generation);
    drop(held);
    let root = owner.root().to_owned();
    let error = crate::store::test_space::with_space(
        &root,
        crate::store::test_space::Statistics {
            available: 0,
            total: 2 * 1024 * 1024 * 1024,
            unit: 4096,
        },
        || match owner.prepare_data_turn(&budget, OwnerDataOperation::Ack) {
            Err(e) => e,
            Ok(_) => panic!("low disk admitted owner turn"),
        },
    );
    assert_eq!(error.kind(), Kind::Capacity(error::OwnerCapacityKind::Disk));
    assert!(!owner.store.is_poisoned());
    owner
        .prepare_data_turn(&budget, OwnerDataOperation::Ack)
        .unwrap();
}

#[test]
fn delivery_resource_current_view_retains_owner_serialized_credit() {
    let (_temp, mut owner) = tests::hosted_owner();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    let before = budget.available_bytes();
    owner.attach_data_resources(&budget).unwrap();
    let authority = CommittedAuthority::new(&owner);
    let charged = budget.available_bytes();
    drop(owner);
    assert_eq!(
        budget.available_bytes(),
        charged,
        "retained Current must not free owner backing credits"
    );
    drop(authority);
    assert_eq!(budget.available_bytes(), before);
}
#[test]
fn delivery_resource_actual_turn_binds_owner_generation_and_covers_publish() {
    use crate::owner::resources::OwnerDataOperation;
    let (_temp, mut owner) = tests::hosted_owner();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    owner.attach_data_resources(&budget).unwrap();
    let authority = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &authority);
    let mut turn = owner
        .prepare_data_turn(&budget, OwnerDataOperation::RegisterConsumer)
        .unwrap();
    let reserved = budget.available_bytes();
    // The commit replaces the previous generation's custody (freed once no
    // pin holds it) with the next aggregate's; it never frees the turn.
    let previous_generation = owner.accounted_retained_bytes();
    authority
        .with_owner_view(&mut owner, |view, owner| {
            let source = owner.data_source(&mut turn, view, &session, tests::now_ns())?;
            owner.data_register_consumer(&mut turn, &source)
        })
        .unwrap()
        .unwrap();
    assert!(
        budget.available_bytes() <= reserved + previous_generation,
        "publish must not free the outer turn credit"
    );
    assert_eq!(
        owner.accounted_retained_bytes(),
        owner.actual_retained_bytes()
    );
    let current = authority.read().unwrap();
    assert!(
        owner
            .data_source(&mut turn, current.view(), &session, tests::now_ns())
            .is_err(),
        "completed/stale turn cannot be reused"
    );
    drop(current);
    drop(turn);
    let after = budget.available_bytes();
    assert!(
        after > reserved,
        "only unused transient credit returns after publication"
    );
    let mut author = owner.peer.clone();
    let bytes = tests::upload(&owner, &mut author, "budgeted-actual-upload");
    let mut turn = owner
        .prepare_data_turn(
            &budget,
            OwnerDataOperation::State {
                input_len: bytes.len(),
            },
        )
        .unwrap();
    let result = authority
        .with_owner_view(&mut owner, |view, owner| {
            let source = owner.data_source(&mut turn, view, &session, tests::now_ns())?;
            owner.data_admit(
                &mut turn,
                &source,
                CompleteStateItem {
                    delivery_id: [61; 16],
                    canonical: &bytes,
                },
                SystemTime::now(),
            )
        })
        .unwrap()
        .unwrap();
    let AdmissionOutcome::Durable(first) = result else {
        panic!("actual budgeted upload must be durable")
    };
    drop(turn);
    assert!(
        budget.available_bytes() < after,
        "new retained publication must keep a resource charge"
    );
    let retained_after_upload = budget.available_bytes();
    let upload_generation = owner.status().generation;
    let mut retry_turn = owner
        .prepare_data_turn(
            &budget,
            OwnerDataOperation::State {
                input_len: bytes.len(),
            },
        )
        .unwrap();
    let retry = authority
        .with_owner_view(&mut owner, |view, owner| {
            let source = owner.data_source(&mut retry_turn, view, &session, tests::now_ns())?;
            owner.data_admit(
                &mut retry_turn,
                &source,
                CompleteStateItem {
                    delivery_id: [61; 16],
                    canonical: &bytes,
                },
                SystemTime::now(),
            )
        })
        .unwrap()
        .unwrap();
    let AdmissionOutcome::Durable(retry) = retry else {
        panic!("lost reply retry must be durable")
    };
    assert_eq!(retry, first);
    drop(retry_turn);
    assert_eq!(owner.status().generation, upload_generation);
    assert_eq!(
        budget.available_bytes(),
        retained_after_upload,
        "exact retry must not retain another publication charge"
    );
    let token = {
        let current = authority.read().unwrap();
        let mut turn = owner
            .prepare_data_turn(&budget, OwnerDataOperation::Replay)
            .unwrap();
        let source = owner
            .data_source(&mut turn, current.view(), &session, tests::now_ns())
            .unwrap();
        let publications = owner
            .data_replay(&mut turn, &source, &first.committed)
            .unwrap();
        assert!(publications.len() > 0);
        let first_pass = publications
            .iter()
            .map(|item| item.unwrap().bytes.as_ptr())
            .collect::<Vec<_>>();
        let second_pass = publications
            .iter()
            .map(|item| item.unwrap().bytes.as_ptr())
            .collect::<Vec<_>>();
        assert_eq!(
            first_pass, second_pass,
            "both borrowed passes resolve original pool bytes"
        );
        owner
            .data_issue_cursor(&mut turn, &source, first.committed.clone())
            .unwrap()
    };
    let pool_before_ack = match &owner.aggregate.publication {
        PublicationStorage::V5 { pool, .. } => pool.retained_payload_capacity().unwrap(),
        _ => unreachable!(),
    };
    for repeated in [false, true] {
        let before = budget.available_bytes();
        let held_before = owner.accounted_retained_bytes();
        let generation = owner.status().generation;
        let mut turn = owner
            .prepare_data_turn(&budget, OwnerDataOperation::Ack)
            .unwrap();
        authority
            .with_owner_view(&mut owner, |view, owner| {
                let source = owner.data_source(&mut turn, view, &session, tests::now_ns())?;
                let cut = owner
                    .data_verify_cursor(&mut turn, &source, &token)?
                    .into_cut();
                owner.data_acknowledge(&mut turn, &source, cut)
            })
            .unwrap()
            .unwrap();
        drop(turn);
        let PublicationStorage::V5 { pool, .. } = &owner.aggregate.publication else {
            unreachable!()
        };
        assert_eq!(
            pool.retained_payload_capacity().unwrap(),
            pool_before_ack,
            "ACK cannot reclaim retained publication backing"
        );
        if repeated {
            assert_eq!(owner.status().generation, generation);
            assert_eq!(budget.available_bytes(), before);
        } else {
            assert_eq!(owner.status().generation, generation + 1);
            // The ACK's aggregate replaces (not adds to) the prior custody.
            let held_after = owner.accounted_retained_bytes();
            assert_eq!(held_after, owner.actual_retained_bytes());
            assert_eq!(
                budget.available_bytes() as isize,
                before as isize + held_before as isize - held_after as isize,
                "first ACK retains exactly its new aggregate backing"
            );
        }
    }
    let (_other_temp, mut other) = tests::hosted_owner();
    other.attach_data_resources(&budget).unwrap();
    let mut foreign = other
        .prepare_data_turn(&budget, OwnerDataOperation::Replay)
        .unwrap();
    let current = authority.read().unwrap();
    assert!(
        owner
            .data_source(&mut foreign, current.view(), &session, tests::now_ns())
            .is_err()
    );
}

#[test]
fn delivery_resource_attachment_publishes_credit_to_existing_current() {
    let (_temp, mut owner) = tests::hosted_owner();
    let authority = CommittedAuthority::new(&owner);
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    let before = budget.available_bytes();
    authority
        .with_owner(&mut owner, |owner| owner.attach_data_resources(&budget))
        .unwrap()
        .unwrap();
    let charged = budget.available_bytes();
    drop(owner);
    assert_eq!(
        budget.available_bytes(),
        charged,
        "same-generation publish must attach retained credits to previous Current"
    );
    drop(authority);
    assert_eq!(budget.available_bytes(), before);
}
#[test]
fn delivery_resource_second_disk_gate_and_barrier_failure_are_distinct() {
    use crate::owner::resources::OwnerDataOperation;
    for fault in [false, true] {
        let (_temp, mut owner) = tests::hosted_owner();
        let (ingress, _receiver) = crate::https::ingress_channel();
        let budget = ingress.host_bytes();
        owner.attach_data_resources(&budget).unwrap();
        let authority = CommittedAuthority::new(&owner);
        let session = tests::authenticated_session(&owner, &authority);
        let mut author = owner.peer.clone();
        let bytes = tests::upload(&owner, &mut author, "second-disk-gate");
        let before_credit = budget.available_bytes();
        let mut turn = owner
            .prepare_data_turn(
                &budget,
                OwnerDataOperation::State {
                    input_len: bytes.len(),
                },
            )
            .unwrap();
        let root = owner.root().to_owned();
        let file = std::fs::read(root.join("session.bin")).unwrap();
        let generation = owner.status().generation;
        let operation = || {
            authority.with_owner_view(&mut owner, |view, owner| {
                let source = owner.data_source(&mut turn, view, &session, tests::now_ns())?;
                owner.data_admit(
                    &mut turn,
                    &source,
                    CompleteStateItem {
                        delivery_id: [71; 16],
                        canonical: &bytes,
                    },
                    SystemTime::now(),
                )
            })
        };
        let result = if fault {
            crate::store::test_fault::with_fault(
                &root,
                crate::store::test_fault::Point::DirectoryBarrier,
                operation,
            )
        } else {
            crate::store::test_space::with_space(
                &root,
                crate::store::test_space::Statistics {
                    available: 0,
                    total: 2 * 1024 * 1024 * 1024,
                    unit: 4096,
                },
                operation,
            )
        };
        drop(turn);
        if fault {
            assert!(
                result.is_err()
                    || result.as_ref().is_ok_and(|result| result
                        .as_ref()
                        .err()
                        .is_some_and(|error| error.kind() == Kind::DurabilityUncertain))
            );
            assert!(owner.store.is_poisoned());
            assert!(authority.read().is_err());
        } else {
            assert_eq!(
                result.unwrap().unwrap_err().kind(),
                Kind::Capacity(error::OwnerCapacityKind::Disk)
            );
            assert_eq!(
                budget.available_bytes(),
                before_credit,
                "refused candidate must return unused retained allowance"
            );
            assert!(!owner.store.is_poisoned());
            assert!(authority.read().is_ok());
            assert_eq!(owner.status().generation, generation);
            assert_eq!(std::fs::read(root.join("session.bin")).unwrap(), file);
        }
    }
}

#[test]
fn delivery_resource_publication_buffers_use_exact_reserved_capacity() {
    let (_temp, owner) = tests::hosted_owner();
    let messages = owner
        .outgoing()
        .iter()
        .map(|publication| postcard::from_bytes::<NeighbourMsg>(publication.bytes).unwrap())
        .collect::<Vec<_>>();
    let bytes = owner.encode_publication_messages(&messages).unwrap();
    assert!(!bytes.is_empty());
    assert!(
        bytes.iter().all(|bytes| bytes.capacity() == bytes.len()),
        "publication encoder allocated uncounted spare capacity"
    );
}

#[test]
fn delivery_resource_policy_bytes_are_bounded_before_cursor_source() {
    use collab_types::caps::{CapOverride, Effect, Scope, Subject, Verb};
    use ui::automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
    let (_temp, mut owner) = tests::hosted_owner();
    let authority = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &authority);
    authority
        .with_owner(&mut owner, |owner| -> Result<()> {
            let mut room = owner.peer.admitted_room().unwrap().clone();
            let caps = match room.get(ROOT, "capabilities")? {
                Some((_, object)) => object,
                None => room.put_object(ROOT, "capabilities", ObjType::Map)?,
            };
            let cap = CapOverride::new(
                Subject::User(collab_types::Ed25519PublicKey::from_bytes(
                    owner.identity.native_cert.user_public_key,
                )),
                Scope::Workspace,
                Effect::Grant,
                Verb::Read,
            );
            room.put(caps, "x".repeat(1024 * 1024), serde_json::to_vec(&cap)?)?;
            room.commit();
            owner.receive(
                TransportOrigin::RelayDelta(owner.peer.authority().device_public_key),
                NeighbourMsg::SyncResp {
                    room: owner.peer.room().into(),
                    changes: room
                        .get_changes(&[])
                        .iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                },
            )?;
            Ok(())
        })
        .unwrap()
        .unwrap();
    let current = authority.read().unwrap();
    let result = owner.delivery_source(current.view(), &session, tests::now_ns());
    let failure = match result {
        Err(error) => kind(error),
        Ok(_) => panic!("oversized serialized policy acquired source"),
    };
    assert_eq!(failure, Kind::Capacity(error::OwnerCapacityKind::Bytes));
}
#[test]
fn delivery_resource_attachment_counts_spare_nested_capacity() {
    let (_temp, mut owner) = tests::hosted_owner();
    // Spare capacity changes no native bytes or authority, but consumes real RAM.
    owner.aggregate.room.reserve_exact(1024 * 1024);
    owner
        .aggregate
        .collaboration
        .join_completions
        .reserve_exact(4096);
    let minimum = owner.aggregate.room.capacity()
        + owner
            .aggregate
            .collaboration
            .retained_serialized_capacity()
            .unwrap();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    let before = budget.available_bytes();
    owner.attach_data_resources(&budget).unwrap();
    assert!(
        before - budget.available_bytes() >= minimum,
        "attachment ignored actual nested/spare backing capacity"
    );
}
#[test]
fn delivery_resource_existing_live_transition_requires_retained_custody() {
    let (_temp, mut owner) = tests::hosted_owner();
    let origin = TransportOrigin::RelayDelta(owner.peer.authority().device_public_key);
    for id in 1..=64u8 {
        // The real owner checkpoint also advances this native signing identity.
        let mut author = owner.peer.clone();
        let bytes = tests::upload(&owner, &mut author, &format!("live-before-attachment-{id}"));
        owner
            .receive_delivery(
                origin,
                CompleteStateItem {
                    delivery_id: [id; 16],
                    canonical: &bytes,
                },
                SystemTime::now(),
            )
            .unwrap();
        if owner.live_publication_transition.is_some() {
            break;
        }
    }
    let escaped = owner
        .live_publication_transition()
        .expect("actual native compaction transition")
        .clone();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    let before = budget.available_bytes();
    assert!(
        owner.attach_data_resources(&budget).is_err(),
        "cannot retrofit custody onto an already escaped uncharged live Arc"
    );
    assert_eq!(budget.available_bytes(), before);
    drop(escaped);
    let weak = Arc::downgrade(owner.live_publication_transition().unwrap());
    assert!(owner.attach_data_resources(&budget).is_err());
    assert_eq!(budget.available_bytes(), before);
    drop(weak);
    owner.attach_data_resources(&budget).unwrap();
    let escaped = owner.live_publication_transition().unwrap().clone();
    let charged = budget.available_bytes();
    drop(owner);
    assert_eq!(
        budget.available_bytes(),
        charged,
        "last live-publication owner must retain backing credits"
    );
    drop(escaped);
    assert_eq!(budget.available_bytes(), before);
}

#[test]
fn delivery_resource_output_cap_refusal_is_typed() {
    assert_eq!(
        kind(schema::encode_bounded(&[7u8; 32], 2).unwrap_err()),
        Kind::Capacity(error::OwnerCapacityKind::Bytes),
    );
    let (_temp, owner) = tests::hosted_owner();
    assert_eq!(
        kind(owner.aggregate.encode_payload_bounded(1).unwrap_err()),
        Kind::Capacity(error::OwnerCapacityKind::Bytes),
    );
}

#[test]
fn delivery_resource_streaming_replay_hash_matches_original_postcard_bytes() {
    let (_temp, owner) = tests::hosted_owner();
    let ledger = owner.delivery_ledger().unwrap();
    for entry in ledger.replay.values() {
        let encoded = postcard::to_stdvec(&(
            ledger.store_generation,
            owner.peer.room(),
            owner.workspace_id,
            entry.owner_generation,
            &entry.previous,
            &entry.publications,
            &entry.admitted_operations,
        ))
        .unwrap();
        let expected = *blake3::Hasher::new()
            .update(b"tilo-delivery-v3-native-commit-v1\0")
            .update(&encoded)
            .finalize()
            .as_bytes();
        assert_eq!(
            replay_digest(
                ledger.store_generation,
                owner.peer.room(),
                owner.workspace_id,
                entry
            )
            .unwrap(),
            expected
        );
        assert_eq!(entry.digest, expected);
    }
}

#[test]
fn private_context_turn_cannot_mint_data_source_or_mutation_permission() {
    use crate::owner::resources::OwnerDataOperation;
    let (_temp, mut owner) = tests::hosted_owner();
    let (ingress, _rx) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    owner.attach_data_resources(&pool).unwrap();
    let current = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &current);
    let mut turn = owner
        .prepare_data_turn(
            &pool,
            OwnerDataOperation::PrivateContext {
                input_len: 256 * 1024,
            },
        )
        .unwrap();
    let view = current.read().unwrap();
    assert!(
        owner
            .data_source(&mut turn, view.view(), &session, tests::now_ns())
            .is_err(),
        "private proof context is not data authorization"
    );
    assert!(owner.flush().is_err());
    drop(turn);
    assert!(
        owner
            .prepare_data_turn(&pool, OwnerDataOperation::PrivateContext { input_len: 0 })
            .is_ok()
    );
    assert!(
        owner
            .prepare_data_turn(
                &pool,
                OwnerDataOperation::PrivateContext {
                    input_len: 256 * 1024 + 1
                }
            )
            .is_err()
    );
}

#[test]
fn delivery_resource_receipt_lookup_returns_only_original_durable_outcome() {
    use crate::owner::resources::OwnerDataOperation;
    let (_temp, mut owner) = tests::hosted_owner();
    let mut author = owner.peer.clone();
    let bytes = tests::upload(&owner, &mut author, "lookup-original");
    let id = [177; 16];
    let digest = *blake3::hash(&bytes).as_bytes();
    let origin = TransportOrigin::RelayDelta(owner.peer.authority().device_public_key);
    let AdmissionOutcome::Durable(original) = owner
        .receive_delivery(
            origin,
            CompleteStateItem {
                delivery_id: id,
                canonical: &bytes,
            },
            SystemTime::now(),
        )
        .unwrap()
    else {
        panic!("real admission")
    };
    let (ingress, _rx) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    owner.attach_data_resources(&pool).unwrap();
    let current = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &current);
    let generation = owner.committed_generation();
    let disk = std::fs::read(owner.root().join("session.bin")).unwrap();
    let mut turn = owner
        .prepare_data_turn(&pool, OwnerDataOperation::ReceiptLookup)
        .unwrap();
    let view = current.read().unwrap();
    let source = owner
        .data_source(&mut turn, view.view(), &session, tests::now_ns())
        .unwrap();
    let receipt = owner
        .data_admission_receipt(&mut turn, &source, id, digest)
        .unwrap()
        .unwrap();
    assert_eq!(receipt, &original);
    assert!(
        owner
            .data_admission_receipt(&mut turn, &source, [178; 16], digest)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        owner
            .data_admission_receipt(&mut turn, &source, id, [1; 32])
            .unwrap_err()
            .kind(),
        Kind::ExactIdConflict
    );
    let mut wrong = owner
        .prepare_data_turn(&pool, OwnerDataOperation::Replay)
        .unwrap();
    assert_eq!(
        owner
            .data_admission_receipt(&mut wrong, &source, id, digest)
            .unwrap_err()
            .kind(),
        Kind::CurrentDenied
    );
    assert_eq!(owner.committed_generation(), generation);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        disk
    );
    let (_other_temp, mut other) = tests::hosted_owner();
    other.attach_data_resources(&pool).unwrap();
    let mut foreign = other
        .prepare_data_turn(&pool, OwnerDataOperation::ReceiptLookup)
        .unwrap();
    assert_eq!(
        other
            .data_admission_receipt(&mut foreign, &source, id, digest)
            .unwrap_err()
            .kind(),
        Kind::CurrentDenied
    );
    let root = owner.root().to_owned();
    drop(source);
    drop(view);
    drop(turn);
    drop(wrong);
    drop(current);
    drop(owner);
    let mut reopened = SessionOwner::open(&root).unwrap();
    reopened.attach_data_resources(&pool).unwrap();
    let current = CommittedAuthority::new(&reopened);
    let session = tests::authenticated_session(&reopened, &current);
    let mut turn = reopened
        .prepare_data_turn(&pool, OwnerDataOperation::ReceiptLookup)
        .unwrap();
    let view = current.read().unwrap();
    let source = reopened
        .data_source(&mut turn, view.view(), &session, tests::now_ns())
        .unwrap();
    assert_eq!(
        reopened
            .data_admission_receipt(&mut turn, &source, id, digest)
            .unwrap(),
        Some(&original)
    );
    assert_eq!(std::fs::read(root.join("session.bin")).unwrap(), disk);
}

/// Long-session custody: many real owner writes (with periodic ACK
/// commits) must never exhaust the ingress pool, and the retained serialized
/// credit must track what the owner actually still holds instead of
/// accumulating a whole-aggregate charge per commit.
#[test]
fn delivery_resource_long_run_retained_credit_tracks_actual_bytes() {
    use crate::owner::native_credit::allocations_only;
    use crate::owner::resources::OwnerDataOperation;
    let writes: usize = std::env::var("QOS_LONG_RUN_WRITES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    let (_temp, mut owner) = tests::hosted_owner();
    let authority = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &authority);
    let _ = tests::register_and_cursor(&mut owner, &authority, &session);
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    let pristine = budget.available_bytes();
    // Leave 32 MiB of bulk headroom. Measured on the former accumulate-
    // forever custody (whole raw aggregate charged per commit, ~19x turns),
    // accounted + turn crossed 32 MiB between writes 75 and 100; exact
    // custody plus measured-peak turns stays within it for this run.
    let ballast = budget
        .try_acquire_bulk(pristine - crate::host_bytes::CONTROL_RESERVE_BYTES - 32 * 1024 * 1024)
        .unwrap();
    let initial = budget.available_bytes();
    authority
        .with_owner(&mut owner, |owner| owner.attach_data_resources(&budget))
        .unwrap()
        .unwrap();
    let csv = std::env::var_os("QOS_LONG_RUN_CSV").is_some();
    let mut worst_excess = 0isize;
    let mut worst_reserved_ratio = 0f64;
    for index in 0..writes {
        // The owner signs its own commits with the same native identity, so
        // each authored write starts from the committed owner peer.
        let mut author = owner.peer.clone();
        let bytes = tests::upload(&owner, &mut author, &format!("long-run-{index}"));
        let current = owner.aggregate.encoded_payload_len().unwrap();
        let before_turn = budget.available_bytes();
        let mut turn = owner
            .prepare_data_turn(
                &budget,
                OwnerDataOperation::State {
                    input_len: bytes.len(),
                },
            )
            .unwrap_or_else(|e| {
                panic!(
                    "write {index} refused: {e:?} accounted={} pool_used={}",
                    owner.accounted_retained_bytes(),
                    initial - budget.available_bytes()
                )
            });
        let reserved = before_turn - budget.available_bytes();
        let (outcome, stats) = allocations_only(|| {
            authority
                .with_owner_view(&mut owner, |view, owner| {
                    let source = owner.data_source(&mut turn, view, &session, tests::now_ns())?;
                    owner.data_admit(
                        &mut turn,
                        &source,
                        CompleteStateItem {
                            delivery_id: *uuid::Uuid::new_v4().as_bytes(),
                            canonical: &bytes,
                        },
                        SystemTime::now(),
                    )
                })
                .unwrap()
                .unwrap()
        });
        drop(turn);
        let AdmissionOutcome::Durable(receipt) = outcome else {
            panic!("write {index} not durable")
        };
        let peak = usize::try_from(stats.peak).unwrap_or(0);
        if csv {
            eprintln!("long-run-csv,state,{index},{current},{reserved},{peak}",);
        }
        worst_reserved_ratio = worst_reserved_ratio.max(peak as f64 / reserved as f64);
        assert!(
            peak <= reserved,
            "write {index}: measured peak {peak} exceeds reservation {reserved}"
        );
        if index % 10 == 9 {
            let token = {
                let current = authority.read().unwrap();
                let mut turn = owner
                    .prepare_data_turn(&budget, OwnerDataOperation::Replay)
                    .unwrap();
                let source = owner
                    .data_source(&mut turn, current.view(), &session, tests::now_ns())
                    .unwrap();
                owner
                    .data_issue_cursor(&mut turn, &source, receipt.committed.clone())
                    .unwrap()
            };
            let before_ack = budget.available_bytes();
            let mut turn = owner
                .prepare_data_turn(&budget, OwnerDataOperation::Ack)
                .unwrap_or_else(|e| panic!("ack {index} refused: {e:?}"));
            let ack_reserved = before_ack - budget.available_bytes();
            let ((), ack) = allocations_only(|| {
                authority
                    .with_owner_view(&mut owner, |view, owner| {
                        let source =
                            owner.data_source(&mut turn, view, &session, tests::now_ns())?;
                        let cut = owner
                            .data_verify_cursor(&mut turn, &source, &token)?
                            .into_cut();
                        owner.data_acknowledge(&mut turn, &source, cut)
                    })
                    .unwrap()
                    .unwrap()
            });
            drop(turn);
            let ack_peak = usize::try_from(ack.peak).unwrap_or(0);
            worst_reserved_ratio = worst_reserved_ratio.max(ack_peak as f64 / ack_reserved as f64);
            assert!(
                ack_peak <= ack_reserved,
                "ack {index}: measured peak {ack_peak} exceeds reservation {ack_reserved}"
            );
            if csv {
                eprintln!(
                    "long-run-csv,ack,{index},{},{ack_reserved},{ack_peak}",
                    owner.aggregate.encoded_payload_len().unwrap()
                );
            }
        }
        let accounted = owner.accounted_retained_bytes();
        let actual = owner.actual_retained_bytes();
        let excess = accounted as isize - actual as isize;
        worst_excess = worst_excess.max(excess);
        if index % 50 == 0 || index + 1 == writes {
            eprintln!(
                "long-run write={index} input={} aggregate={} reserved={reserved} peak={peak} \
                 accounted={accounted} actual={actual} pool_used={}",
                bytes.len(),
                owner.aggregate.encoded_payload_len().unwrap(),
                initial - budget.available_bytes(),
            );
        }
        assert!(
            accounted >= actual,
            "write {index}: retained credit {accounted} underfunds actual {actual}"
        );
        assert_eq!(
            accounted, actual,
            "write {index}: retained credit must equal what the owner holds"
        );
        assert_eq!(
            initial - budget.available_bytes(),
            accounted,
            "write {index}: nothing but exact owner custody stays charged"
        );
    }
    eprintln!(
        "long-run done writes={writes} worst_excess={worst_excess} \
         worst_peak_over_reserved={worst_reserved_ratio:.3} pool_used={}",
        initial - budget.available_bytes()
    );
    drop(authority);
    drop(owner);
    drop(ballast);
    assert_eq!(budget.available_bytes(), pristine);
}

/// A pin taken before a commit keeps paying for the payloads it borrows
/// (its generation) after the owner moves on; dropping it releases exactly
/// that generation, leaving only the owner's current custody charged.
#[test]
fn delivery_resource_pin_holds_its_generation_until_dropped() {
    use crate::owner::resources::{OwnerDataOperation, OwnerOperation};
    let (_temp, mut owner) = tests::hosted_owner();
    let authority = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &authority);
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    let pristine = budget.available_bytes();
    authority
        .with_owner(&mut owner, |owner| owner.attach_data_resources(&budget))
        .unwrap()
        .unwrap();
    let mut scope = owner
        .prepare_owner_scope(&budget, OwnerOperation::Export)
        .unwrap();
    let pin = owner
        .with_owner_scope(&mut scope, &authority, |o| o.pin_publication(&budget))
        .unwrap()
        .unwrap();
    drop(scope);
    let pinned_generation = owner.accounted_retained_bytes();
    let mut author = owner.peer.clone();
    let bytes = tests::upload(&owner, &mut author, "after-pin");
    let mut turn = owner
        .prepare_data_turn(
            &budget,
            OwnerDataOperation::State {
                input_len: bytes.len(),
            },
        )
        .unwrap();
    authority
        .with_owner_view(&mut owner, |view, owner| {
            let source = owner.data_source(&mut turn, view, &session, tests::now_ns())?;
            owner.data_admit(
                &mut turn,
                &source,
                CompleteStateItem {
                    delivery_id: [71; 16],
                    canonical: &bytes,
                },
                SystemTime::now(),
            )
        })
        .unwrap()
        .unwrap();
    drop(turn);
    let actual = owner.actual_retained_bytes();
    assert_eq!(
        owner.accounted_retained_bytes(),
        actual + pinned_generation,
        "the pin keeps its own generation; the owner holds the new one"
    );
    assert!(pin.get(0).is_some());
    drop(pin);
    assert_eq!(owner.accounted_retained_bytes(), actual);
    drop(authority);
    drop(owner);
    assert_eq!(budget.available_bytes(), pristine);
}

/// Pool, disk and native-quota pressure are retryable (HTTP 503); other
/// owner failures stay opaque refusals.
#[test]
fn delivery_resource_transient_capacity_classification() {
    use crate::delivery_http::serve::transient_capacity;
    let (_temp, owner) = tests::hosted_owner();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let budget = ingress.host_bytes();
    let held = budget
        .try_acquire_bulk(budget.available_bytes() - crate::host_bytes::CONTROL_RESERVE_BYTES)
        .unwrap();
    let mut owner = owner;
    let error: anyhow::Error = owner.attach_data_resources(&budget).unwrap_err().into();
    assert!(transient_capacity(&error));
    assert!(transient_capacity(&error.context("route")));
    drop(held);
    let denied: anyhow::Error =
        OwnerDataFailure::new(Kind::CurrentDenied, anyhow::anyhow!("x")).into();
    assert!(!transient_capacity(&denied));
    assert!(!transient_capacity(&anyhow::anyhow!("plain")));
}
