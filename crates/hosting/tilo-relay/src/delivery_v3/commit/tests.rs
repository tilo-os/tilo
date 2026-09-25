use super::*;

fn ledger() -> DeliveryLedgerV1 {
    DeliveryLedgerV1 {
        version: 1,
        store_generation: [1; 16],
        resolved_through: 0,
        replay: BTreeMap::default(),
        admissions: BTreeMap::default(),
        consumers: BTreeMap::default(),
        pins: BTreeMap::default(),
    }
}
fn publication(bytes: &[u8]) -> PublicationRef {
    PublicationRef {
        payload: *blake3::hash(bytes).as_bytes(),
        kind: Retention::Delta,
    }
}
fn replay(reference: PublicationRef) -> ReplayCommit {
    ReplayCommit {
        owner_generation: 9_007_199_254_740_993,
        previous: None,
        digest: [2; 32],
        publications: vec![reference],
        admitted_operations: vec![[3; 32]],
        committed_at_ns: 100,
    }
}
#[test]
fn queue_drain_and_baseline_replacement_do_not_erase_replay_or_pin_payloads() {
    let mut pool = PublicationPool::new(1024).unwrap();
    let id = pool.intern(b"original canonical bytes").unwrap();
    assert_eq!(id, pool.intern(b"original canonical bytes").unwrap());
    let reference = publication(b"original canonical bytes");
    let mut value = ledger();
    let event = replay(reference.clone());
    value.resolved_through = event.owner_generation;
    value.replay.insert(event.owner_generation, event.clone());
    value.pins.insert(
        [4; 16],
        SnapshotPin {
            consumer: [5; 16],
            authorization_view: [6; 32],
            watermark: CommitRef {
                sequence: event.owner_generation,
                digest: event.digest,
            },
            publications: vec![reference.clone()],
            expires_at_ns: 200,
        },
    );
    value.consumers.insert(
        [7; 32],
        ConsumerState {
            id: [5; 16],
            device: [7; 32],
            root: [8; 32],
            authorization_view: [6; 32],
            acknowledged: None,
        },
    );
    let roots = vec![reference.clone(), reference];
    assert_eq!(pool.reference_counts(&roots, &value).unwrap()[&id], 4);
    assert_eq!(pool.reference_counts(&[], &value).unwrap()[&id], 2);
    let reopened = PublicationPool::decode(&pool.encode(2048).unwrap(), 1024).unwrap();
    assert_eq!(reopened.resolve(&id).unwrap(), b"original canonical bytes");
    assert_eq!(reopened.len(), 1);
    assert_eq!(
        DeliveryLedgerV1::decode(&value.encode().unwrap()).unwrap(),
        value
    );
}
#[test]
fn pool_pressure_does_not_evict_existing_payload_and_invalid_limits_fail() {
    let mut pool = PublicationPool::new(3).unwrap();
    let id = pool.intern(b"one").unwrap();
    assert!(pool.intern(b"two").is_err());
    assert_eq!(pool.resolve(&id).unwrap(), b"one");
    assert_eq!(pool.intern(b"one").unwrap(), id);
    assert!(pool.intern(b"").is_err());
    assert!(PublicationPool::new(0).is_err());
    assert!(PublicationPool::new(MAX_HOST_BYTES + 1).is_err());
    assert!(pool.encode(1).is_err());
    assert!(PublicationPool::decode(&pool.encode(1024).unwrap(), 2).is_err());
}
#[test]
fn unresolved_references_fail_even_if_the_queue_is_empty() {
    let pool = PublicationPool::new(1024).unwrap();
    let reference = publication(b"missing");
    assert!(
        pool.reference_counts(std::slice::from_ref(&reference), &ledger())
            .is_err()
    );
    let mut value = ledger();
    let event = replay(reference);
    value.resolved_through = event.owner_generation;
    value.replay.insert(event.owner_generation, event);
    assert!(pool.reference_counts(&[], &value).is_err());
}
#[test]
fn metadata_rejects_future_positions_wrong_scope_and_broken_order() {
    let mut value = ledger();
    let event = replay(publication(b"a"));
    let sequence = event.owner_generation;
    value.replay.insert(sequence, event);
    assert!(value.encode().is_err());
    value.resolved_through = sequence;
    assert!(value.encode().is_ok());
    value.replay.get_mut(&sequence).unwrap().previous = Some(CommitRef {
        sequence,
        digest: [9; 32],
    });
    assert!(value.encode().is_err());
    value.replay.get_mut(&sequence).unwrap().previous = None;
    value.consumers.insert(
        [1; 32],
        ConsumerState {
            id: [1; 16],
            device: [2; 32],
            root: [3; 32],
            authorization_view: [4; 32],
            acknowledged: None,
        },
    );
    assert!(value.encode().is_err());
    value.consumers.clear();
    value.admissions.insert(
        ReceiptKey {
            device: [1; 32],
            delivery: [1; 16],
        },
        AdmissionRecord {
            digest: [4; 32],
            operations: vec![[5; 32]],
            committed: CommitRef {
                sequence,
                digest: [9; 32],
            },
            result: DurableAdmissionKind::Accepted,
        },
    );
    assert!(value.encode().is_err());
}
#[test]
fn decoder_rejects_unknown_truncated_trailing_and_noncanonical_metadata() {
    let encoded = ledger().encode().unwrap();
    let mut unknown = encoded.clone();
    unknown[0] = 2;
    assert!(DeliveryLedgerV1::decode(&unknown).is_err());
    for end in 0..encoded.len() {
        assert!(DeliveryLedgerV1::decode(&encoded[..end]).is_err());
    }
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(DeliveryLedgerV1::decode(&trailing).is_err());
    let mut noncanonical = encoded;
    noncanonical.splice(17..18, [0x80, 0]);
    assert!(DeliveryLedgerV1::decode(&noncanonical).is_err());
    assert!(DeliveryLedgerV1::decode(&vec![0; MAX_LEDGER_BYTES + 1]).is_err());
}
#[test]
fn forged_collection_lengths_fail_before_element_allocation() {
    let encoded = ledger().encode().unwrap();
    // The first map follows version, 16-byte incarnation and zero high-water.
    let mut huge = encoded[..18].to_vec();
    huge.extend(postcard::to_allocvec(&u64::MAX).unwrap());
    assert!(DeliveryLedgerV1::decode(&huge).is_err());
    // A real over-limit consumer map must fail both encode and decode.
    let mut value = ledger();
    for n in 0..=MAX_CONSUMERS {
        let mut device = [0; 32];
        device[..8].copy_from_slice(&(n as u64).to_le_bytes());
        let mut id = [0; 16];
        id[..8].copy_from_slice(&(n as u64 + 1).to_le_bytes());
        value.consumers.insert(
            device,
            ConsumerState {
                id,
                device,
                root: [1; 32],
                authorization_view: [2; 32],
                acknowledged: None,
            },
        );
    }
    assert!(value.encode().is_err());
    assert!(DeliveryLedgerV1::decode(&postcard::to_allocvec(&value).unwrap()).is_err());
}
#[test]
fn corrupted_pool_digest_duplicate_map_key_and_noncanonical_bytes_fail() {
    let mut pool = PublicationPool::new(1024).unwrap();
    let id = pool.intern(b"abc").unwrap();
    let encoded = pool.encode(2048).unwrap();
    let mut corrupt = encoded.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(PublicationPool::decode(&corrupt, 1024).is_err());
    // pool version, one-entry map, digest, bytes; duplicate full entries.
    let mut duplicate = vec![1, 2];
    duplicate.extend_from_slice(&encoded[2..]);
    duplicate.extend_from_slice(&encoded[2..]);
    assert!(PublicationPool::decode(&duplicate, 1024).is_err());
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(PublicationPool::decode(&trailing, 1024).is_err());
    assert_eq!(pool.resolve(&id).unwrap(), b"abc");
}
#[test]
fn exact_metadata_budget_and_binding_capacity_reject_without_mutating_caller() {
    let mut value = ledger();
    for n in 0..=MAX_ADMISSIONS {
        let mut delivery = [0; 16];
        delivery[..8].copy_from_slice(&(n as u64 + 1).to_le_bytes());
        value.admissions.insert(
            ReceiptKey {
                device: [1; 32],
                delivery,
            },
            AdmissionRecord {
                digest: [2; 32],
                operations: vec![[3; 32]],
                committed: CommitRef {
                    sequence: 1,
                    digest: [4; 32],
                },
                result: DurableAdmissionKind::Duplicate,
            },
        );
    }
    value.resolved_through = 1;
    assert!(value.encode().is_err());
    assert_eq!(value.admissions.len(), MAX_ADMISSIONS + 1);
    value.admissions.clear();
    let mut event = replay(publication(b"a"));
    event.admitted_operations = (0..MAX_REFERENCES)
        .map(|n| {
            let mut x = [0; 32];
            x[..8].copy_from_slice(&(n as u64).to_le_bytes());
            x
        })
        .collect();
    value.resolved_through = event.owner_generation + 1;
    value.replay.insert(event.owner_generation, event.clone());
    let previous = CommitRef {
        sequence: event.owner_generation,
        digest: event.digest,
    };
    event.owner_generation += 1;
    event.previous = Some(previous);
    event.digest = [5; 32];
    value.replay.insert(event.owner_generation, event);
    assert!(postcard::to_allocvec(&value).unwrap().len() > MAX_LEDGER_BYTES);
    assert!(value.encode().is_err());
}

#[test]
fn fork_shares_original_allocations_but_isolates_candidate_growth_and_quota_failure() {
    #[derive(Serialize)]
    struct V1 {
        version: u8,
        payloads: BTreeMap<Digest, Vec<u8>>,
    }
    let mut original = PublicationPool::new(7).unwrap();
    let id = original.intern(b"old").unwrap();
    let encoded = original.encode(1024).unwrap();
    let mut candidate = original.fork();
    assert_eq!(
        original.resolve(&id).unwrap().as_ptr(),
        candidate.resolve(&id).unwrap().as_ptr()
    );
    assert_eq!(candidate.encode(1024).unwrap(), encoded);
    let new_id = candidate.intern(b"new").unwrap();
    assert!(original.resolve(&new_id).is_err());
    assert!(candidate.intern(b"full").is_err());
    assert_eq!(original.len(), 1);
    assert_eq!(original.encode(1024).unwrap(), encoded);
    drop(original);
    assert_eq!(candidate.resolve(&id).unwrap(), b"old");
    // Independent frozen v1 wire DTO: Arc backing cannot alter persisted ABI.
    let fixture = V1 {
        version: 1,
        payloads: BTreeMap::from([(id, b"old".to_vec())]),
    };
    assert_eq!(postcard::to_stdvec(&fixture).unwrap(), encoded);
}

#[test]
fn encoded_lengths_match_exact_outputs_and_reject_insufficient_budget() {
    let mut pool = PublicationPool::new(256).unwrap();
    pool.intern(&[9; 128]).unwrap();
    let length = pool.encoded_len(1024).unwrap();
    assert_eq!(length, pool.encode(1024).unwrap().len());
    assert!(pool.encoded_len(length - 1).is_err());
    assert_eq!(pool.encoded_len(length).unwrap(), length);
    assert!(pool.encoded_len(usize::MAX).is_err());
    let metadata = ledger();
    assert_eq!(
        metadata.encoded_len().unwrap(),
        metadata.encode().unwrap().len()
    );
}

#[test]
fn resource_capacity_counts_unique_retained_backing_including_spare_capacity() {
    let mut pool = PublicationPool::new(1024).unwrap();
    assert_eq!(pool.retained_payload_capacity().unwrap(), 0);
    let id = pool.intern(b"abc").unwrap();
    // Preserve the digest/unique-allocation invariant while giving the retained
    // Vec realistic spare capacity. Length-only accounting would undercharge it.
    let mut backing = Vec::with_capacity(97);
    backing.extend_from_slice(b"abc");
    let capacity = backing.capacity();
    pool.wire.payloads.insert(id, Arc::new(backing));
    assert_eq!(pool.retained_payload_capacity().unwrap(), capacity);
    assert_eq!(pool.intern(b"abc").unwrap(), id);
    assert_eq!(pool.retained_payload_capacity().unwrap(), capacity);

    let mut fork = pool.fork();
    assert_eq!(
        pool.resolve(&id).unwrap().as_ptr(),
        fork.resolve(&id).unwrap().as_ptr()
    );
    // Strong-reference multiplicity is not additional payload backing.
    assert_eq!(pool.retained_payload_capacity().unwrap(), capacity);
    assert_eq!(fork.retained_payload_capacity().unwrap(), capacity);
    fork.intern(b"other").unwrap();
    assert_eq!(fork.retained_payload_capacity().unwrap(), capacity + 5);
    assert_eq!(pool.retained_payload_capacity().unwrap(), capacity);
    let reopened = PublicationPool::decode(&fork.encode(1024).unwrap(), 1024).unwrap();
    assert_eq!(reopened.retained_payload_capacity().unwrap(), 8);
}

#[test]
fn resource_encoders_preserve_literal_wire_with_exact_output_capacity() {
    let mut pool = PublicationPool::new(1024).unwrap();
    pool.intern(b"abc").unwrap();
    // Frozen pool v1 wire: version, map count, BLAKE3("abc"), byte count, bytes.
    let mut pool_golden = vec![1, 1];
    pool_golden.extend_from_slice(&[
        0x64, 0x37, 0xb3, 0xac, 0x38, 0x46, 0x51, 0x33, 0xff, 0xb6, 0x3b, 0x75, 0x27, 0x3a, 0x8d,
        0xb5, 0x48, 0xc5, 0x58, 0x46, 0x5d, 0x79, 0xdb, 0x03, 0xfd, 0x35, 0x9c, 0x6c, 0xd5, 0xbd,
        0x9d, 0x85,
    ]);
    pool_golden.extend_from_slice(&[3, b'a', b'b', b'c']);
    let encoded = pool.encode(pool_golden.len()).unwrap();
    assert_eq!(encoded, pool_golden);
    assert_eq!(encoded.capacity(), encoded.len());
    assert!(matches!(
        pool.encode(pool_golden.len() - 1),
        Err(CommitError::Quota)
    ));

    // Frozen empty ledger v1: version, incarnation, high-water and four maps.
    let ledger_golden = [
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0,
    ];
    let encoded = ledger().encode().unwrap();
    assert_eq!(encoded, ledger_golden);
    assert_eq!(encoded.capacity(), encoded.len());

    // Crossing a Postcard length-varint boundary must not grow the destination.
    for length in [127, 128, 255, 256] {
        let mut pool = PublicationPool::new(1024).unwrap();
        pool.intern(&vec![9; length]).unwrap();
        let encoded_len = pool.encoded_len(2048).unwrap();
        let encoded = pool.encode(encoded_len).unwrap();
        assert_eq!(encoded.len(), encoded_len);
        assert_eq!(encoded.capacity(), encoded_len);
        assert_eq!(PublicationPool::decode(&encoded, 1024).unwrap().len(), 1);
    }
}

#[test]
fn resource_encode_budget_rejects_before_output_pass_and_count_cannot_wrap() {
    struct Observed<'a>(&'a std::cell::Cell<usize>);
    impl Serialize for Observed<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.0.set(self.0.get() + 1);
            serializer.serialize_bytes(&[9; 128])
        }
    }
    let calls = std::cell::Cell::new(0);
    assert!(matches!(
        encode_bounded(&Observed(&calls), 129),
        Err(CommitError::Quota)
    ));
    assert_eq!(calls.get(), 1);
    calls.set(0);
    let encoded = encode_bounded(&Observed(&calls), 130).unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(encoded.len(), 130);
    assert_eq!(encoded.capacity(), 130);
    let mut count = Count {
        bytes: usize::MAX,
        limit: usize::MAX,
    };
    assert!(count.add(1).is_err());
    assert_eq!(count.bytes, usize::MAX);
}

#[test]
fn literal_empty_wire_and_maximum_vector_decode_while_one_more_is_refused() {
    let literal = [
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0,
    ];
    let mut ledger = DeliveryLedgerV1::decode(&literal).unwrap();
    assert_eq!(ledger.encode().unwrap(), literal);
    let literal_pool = [1, 0];
    let pool = PublicationPool::decode(&literal_pool, 1).unwrap();
    assert_eq!(pool.encode(2).unwrap(), literal_pool);

    ledger.resolved_through = u64::MAX;
    ledger.replay.insert(
        u64::MAX,
        ReplayCommit {
            owner_generation: u64::MAX,
            previous: None,
            digest: [2; 32],
            publications: vec![PublicationRef {
                payload: [3; 32],
                kind: Retention::Delta,
            }],
            admitted_operations: vec![[4; 32]; MAX_REFERENCES],
            committed_at_ns: u64::MAX,
        },
    );
    assert_eq!(
        DeliveryLedgerV1::decode(&ledger.encode().unwrap()).unwrap(),
        ledger
    );
    ledger
        .replay
        .get_mut(&u64::MAX)
        .unwrap()
        .admitted_operations
        .push([4; 32]);
    let oversized = postcard::to_allocvec(&ledger).unwrap();
    assert!(matches!(
        DeliveryLedgerV1::decode(&oversized),
        Err(CommitError::Codec)
    ));
}
