use super::*;
use crate::delivery_v3::commit::CommitRef;

#[test]
fn canonical_item_digest_matches_literal_blake3_and_exact_slice() {
    assert_eq!(
        hex::encode(canonical_item_digest(b"")),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
    assert_eq!(
        hex::encode(canonical_item_digest(b"abc")),
        "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
    let framed = b"xabc\0";
    assert_eq!(
        canonical_item_digest(&framed[1..4]),
        canonical_item_digest(b"abc")
    );
    assert_ne!(
        canonical_item_digest(&framed[1..]),
        canonical_item_digest(b"abc")
    );
}

fn commit() -> CommitRef {
    CommitRef {
        sequence: 3,
        digest: [4; 32],
    }
}

#[test]
fn admission_round_trip_and_scope_are_explicit() {
    let value = StateAdmission::accepted(
        [1; 16],
        [2; 32],
        CommitScope::NativeCommitted,
        vec![[3; 32]],
        commit(),
    )
    .unwrap();
    assert_eq!(
        StateAdmission::decode(&value.encode().unwrap()).unwrap(),
        value
    );
    assert!(
        StateAdmission::accepted([1; 16], [2; 32], CommitScope::RelayStored, vec![], commit())
            .is_err()
    );
}

#[test]
fn event_chunk_checks_offsets_and_encoded_frame_cap() {
    let ok = EventChunk::new(
        commit(),
        0,
        [5; 32],
        EventSource::Native,
        EventKind::State,
        3,
        0,
        vec![1, 2, 3],
    )
    .unwrap();
    assert_eq!(EventChunk::decode(&ok.encode().unwrap()).unwrap(), ok);
    assert!(
        EventChunk::new(
            commit(),
            0,
            [5; 32],
            EventSource::Native,
            EventKind::State,
            2,
            1,
            vec![1, 2]
        )
        .is_err()
    );
    assert!(
        EventChunk::new(
            commit(),
            0,
            [5; 32],
            EventSource::Native,
            EventKind::State,
            MAX_FRAME_PAYLOAD_BYTES as u64,
            0,
            vec![0; MAX_FRAME_PAYLOAD_BYTES]
        )
        .is_err()
    );
}

#[test]
fn boundary_ack_and_reset_are_exact_and_bounded() {
    let token = crate::delivery_v3::cursor::CursorToken::from_bytes(&[1, 2, 3]).unwrap();
    let boundary = CommitBoundary::new(
        [1; 16],
        [2; 32],
        [3; 16],
        commit(),
        0,
        visible_order_digest(&[]).unwrap(),
        token.clone(),
    )
    .unwrap();
    assert_eq!(
        CommitBoundary::decode(&boundary.encode().unwrap()).unwrap(),
        boundary
    );
    let ack = CursorAck::new(token);
    assert_eq!(CursorAck::decode(&ack.encode().unwrap()).unwrap(), ack);
    let reset = SnapshotRequired::new(ResetReason::StoreRotated, [3; 16]);
    assert_eq!(
        SnapshotRequired::decode(&reset.encode().unwrap()).unwrap(),
        reset
    );
}

#[test]
fn admission_rejects_duplicate_and_excess_operation_ids() {
    assert!(
        StateAdmission::accepted(
            [1; 16],
            [2; 32],
            CommitScope::NativeCommitted,
            vec![[3; 32], [3; 32]],
            commit()
        )
        .is_err()
    );
    assert!(
        StateAdmission::accepted(
            [1; 16],
            [2; 32],
            CommitScope::NativeCommitted,
            vec![[3; 32]; MAX_ADMITTED_OPERATIONS + 1],
            commit()
        )
        .is_err()
    );
}

#[test]
fn state_decoders_reject_truncation_trailing_and_forged_counts() {
    let value = StateAdmission::accepted(
        [1; 16],
        [2; 32],
        CommitScope::NativeCommitted,
        vec![[3; 32]],
        commit(),
    )
    .unwrap();
    let bytes = value.encode().unwrap();
    for end in 0..bytes.len() {
        assert!(StateAdmission::decode(&bytes[..end]).is_err());
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(StateAdmission::decode(&trailing).is_err());

    let forged = StateAdmission {
        version: VERSION,
        delivery: [1; 16],
        input_digest: [2; 32],
        outcome: AdmissionOutcome::Accepted {
            scope: CommitScope::NativeCommitted,
            operations: vec![[3; 32]; MAX_ADMITTED_OPERATIONS + 1],
            committed: commit(),
        },
    };
    let forged = postcard::to_stdvec(&forged).unwrap();
    assert!(StateAdmission::decode(&forged).is_err());
}

#[test]
fn visible_order_commitment_is_order_sensitive_and_bounded() {
    let a = visible_order_digest(&[(0, [1; 32]), (1, [2; 32])]).unwrap();
    let b = visible_order_digest(&[(1, [2; 32]), (0, [1; 32])]).unwrap();
    assert_ne!(a, b);
    assert!(visible_order_digest(&vec![(0, [0; 32]); MAX_ADMITTED_OPERATIONS + 1]).is_err());
}

#[test]
fn decoded_payloads_expose_read_only_inert_fields() {
    let admission = StateAdmission::accepted(
        [1; 16],
        [2; 32],
        CommitScope::NativeCommitted,
        vec![[3; 32]],
        commit(),
    )
    .unwrap();
    assert_eq!(admission.delivery(), [1; 16]);
    assert_eq!(admission.input_digest(), [2; 32]);
    let event = EventChunk::new(
        commit(),
        7,
        [5; 32],
        EventSource::Native,
        EventKind::State,
        3,
        0,
        vec![1, 2, 3],
    )
    .unwrap();
    assert_eq!((event.ordinal(), event.bytes()), (7, &[1, 2, 3][..]));
    assert_eq!(
        (event.committed().clone(), event.event_digest()),
        (commit(), [5; 32])
    );
    assert_eq!(
        (event.source(), event.kind()),
        (EventSource::Native, EventKind::State)
    );
    assert_eq!((event.canonical_size(), event.offset()), (3, 0));
    let token = crate::delivery_v3::cursor::CursorToken::from_bytes(&[1]).unwrap();
    let boundary = CommitBoundary::new(
        [1; 16],
        [2; 32],
        [3; 16],
        commit(),
        0,
        visible_order_digest(&[]).unwrap(),
        token,
    )
    .unwrap();
    assert_eq!(
        (boundary.consumer(), boundary.authorization_view()),
        ([1; 16], [2; 32])
    );
    assert_eq!(
        (boundary.committed().clone(), boundary.store_generation()),
        (commit(), [3; 16])
    );
    assert_eq!(boundary.visible_count(), 0);
    assert_eq!(
        boundary.visible_order_digest(),
        visible_order_digest(&[]).unwrap()
    );
    assert_eq!(boundary.cursor().as_bytes(), &[1]);
    let reset = SnapshotRequired::new(ResetReason::StoreRotated, [3; 16]);
    assert_eq!(
        (reset.reason(), reset.store_generation()),
        (ResetReason::StoreRotated, [3; 16])
    );
}

#[test]
fn oversized_cursor_field_rejects_before_owned_copy() {
    #[derive(serde::Serialize)]
    struct AckWire<'a> {
        version: u8,
        #[serde(with = "serde_bytes")]
        cursor: &'a [u8],
    }
    reset_token_owned_copies();
    let bytes = postcard::to_stdvec(&AckWire {
        version: VERSION,
        cursor: &vec![0; crate::delivery_v3::cursor::MAX_CURSOR_BYTES + 1],
    })
    .unwrap();
    assert!(CursorAck::decode(&bytes).is_err());
    assert_eq!(token_owned_copies(), 0);
}

#[test]
fn upload_chunk_literal_wire_is_canonical_and_borrowed() {
    let mut golden = vec![1];
    golden.extend_from_slice(&[2; 16]);
    golden.extend_from_slice(&[3; 32]);
    golden.extend_from_slice(&[3, 0, 3, 7, 8, 9]);
    let value = StateItemChunk::new([2; 16], [3; 32], 3, 0, &[7, 8, 9]).unwrap();
    assert_eq!(value.encoded_len().unwrap(), 55);
    assert_eq!(value.encode().unwrap(), golden);
    let decoded = StateItemChunk::decode(&golden).unwrap();
    assert_eq!(decoded.delivery(), [2; 16]);
    assert_eq!(decoded.input_digest(), [3; 32]);
    assert_eq!((decoded.canonical_size(), decoded.offset()), (3, 0));
    assert_eq!(decoded.bytes(), &[7, 8, 9]);
    assert_eq!(decoded.bytes().as_ptr(), golden[52..].as_ptr());

    // The common codec retains legitimate partial ranges; the complete-item
    // HTTP profile checks zero offset, complete length and the full digest.
    let range = StateItemChunk::new([2; 16], [3; 32], 130, 128, &[7, 8]).unwrap();
    let bytes = range.encode().unwrap();
    assert_eq!(&bytes[49..], &[130, 1, 128, 1, 2, 7, 8]);
    assert_eq!(StateItemChunk::decode(&bytes).unwrap(), range);
}

#[test]
fn upload_chunk_rejects_invalid_ranges_and_metadata_overflow() {
    assert!(StateItemChunk::new([0; 16], [3; 32], 1, 0, &[7]).is_err());
    assert!(StateItemChunk::new([2; 16], [3; 32], 0, 0, &[]).is_err());
    assert!(StateItemChunk::new([2; 16], [3; 32], 3, 3, &[7]).is_err());
    assert!(StateItemChunk::new([2; 16], [3; 32], u64::MAX, u64::MAX, &[7]).is_err());
    assert!(StateItemChunk::new([2; 16], [3; 32], u64::MAX, u64::MAX - 1, &[7]).is_ok());
    // 49 fixed bytes + three-byte total + one-byte offset + three-byte length.
    let exact = vec![7; MAX_FRAME_PAYLOAD_BYTES - 56];
    let chunk = StateItemChunk::new([2; 16], [3; 32], exact.len() as u64, 0, &exact).unwrap();
    assert_eq!(chunk.encoded_len().unwrap(), MAX_FRAME_PAYLOAD_BYTES);
    let encoded = chunk.encode().unwrap();
    assert_eq!(
        StateItemChunk::decode(&encoded).unwrap().bytes().len(),
        exact.len()
    );
    let oversized = vec![7; exact.len() + 1];
    assert!(matches!(
        StateItemChunk::new([2; 16], [3; 32], oversized.len() as u64, 0, &oversized),
        Err(StateError::Oversized)
    ));
    assert!(matches!(
        StateItemChunk::decode(&vec![0; MAX_FRAME_PAYLOAD_BYTES + 1]),
        Err(StateError::Oversized)
    ));
}

#[test]
fn upload_chunk_rejects_every_truncation_trailing_and_noncanonical_length() {
    let mut wire = vec![1];
    wire.extend_from_slice(&[2; 16]);
    wire.extend_from_slice(&[3; 32]);
    wire.extend_from_slice(&[3, 0, 3, 7, 8, 9]);
    for end in 0..wire.len() {
        assert!(
            StateItemChunk::decode(&wire[..end]).is_err(),
            "accepted prefix {end}"
        );
    }
    let mut invalid = wire.clone();
    invalid.push(0);
    assert!(StateItemChunk::decode(&invalid).is_err());
    let mut invalid = wire.clone();
    invalid[0] = 2;
    assert!(StateItemChunk::decode(&invalid).is_err());
    let mut invalid = wire.clone();
    invalid[1..17].fill(0);
    assert!(StateItemChunk::decode(&invalid).is_err());
    for field in [49, 50, 51] {
        let mut invalid = wire.clone();
        invalid[field] |= 0x80;
        invalid.insert(field + 1, 0);
        assert!(
            StateItemChunk::decode(&invalid).is_err(),
            "accepted overlong field {field}"
        );
    }
    let mut invalid = wire[..51].to_vec();
    invalid.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1]);
    assert!(
        StateItemChunk::decode(&invalid).is_err(),
        "forged allocation length admitted"
    );
}

#[test]
fn cursor_url_hex_is_lowercase_exact_and_bounded_before_copy() {
    assert_eq!(
        encode_cursor_url(&[0, 1, 15, 16, 255]).unwrap(),
        "00010f10ff"
    );
    assert_eq!(
        decode_cursor_url("00010f10ff").unwrap().as_bytes(),
        &[0, 1, 15, 16, 255]
    );
    let raw = vec![255; MAX_CURSOR_BYTES];
    let url = encode_cursor_url(&raw).unwrap();
    assert_eq!(url.len(), 8192);
    assert_eq!(MAX_CURSOR_URL_BYTES, 8192);
    assert_eq!(decode_cursor_url(&url).unwrap().as_bytes(), raw);
    assert!(encode_cursor_url(&[]).is_err());
    assert!(encode_cursor_url(&vec![0; MAX_CURSOR_BYTES + 1]).is_err());
    for invalid in [
        "", "0", "000", "0F", "FF", "gg", "%00", "00%20", "0é", " 00", "00\n",
    ] {
        reset_token_owned_copies();
        assert!(decode_cursor_url(invalid).is_err(), "accepted {invalid:?}");
        assert_eq!(
            token_owned_copies(),
            0,
            "invalid input copied before validation"
        );
    }
    reset_token_owned_copies();
    assert!(decode_cursor_url(&"00".repeat(MAX_CURSOR_BYTES + 1)).is_err());
    assert_eq!(token_owned_copies(), 0);
    assert!(decode_cursor_url("00").is_ok());
    assert_eq!(token_owned_copies(), 1);
}
