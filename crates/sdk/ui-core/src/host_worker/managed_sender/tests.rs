use super::*;

fn short(bytes: &mut Vec<u8>, text: &str) {
    bytes.extend_from_slice(&(text.len() as u16).to_le_bytes());
    bytes.extend_from_slice(text.as_bytes());
}

// Independent inert fixture encoder, not a constructor for an admitted row.
pub(super) fn pending_row(id: u8, argument: Option<&str>) -> Vec<u8> {
    let mut payload = vec![id; 16];
    payload.extend_from_slice(&[1; 32]);
    payload.extend_from_slice(&[2; 32]);
    short(&mut payload, "caller-app");
    short(&mut payload, "caller-record");
    payload.extend_from_slice(&u16::MAX.to_le_bytes()); // person, no agent alias
    payload.extend_from_slice(&[3; 16]);
    payload.extend_from_slice(&7u64.to_le_bytes());
    short(&mut payload, "target-app");
    short(&mut payload, "target-record");
    payload.extend_from_slice(&[4; 32]);
    payload.extend_from_slice(&[5; 32]);
    payload.extend_from_slice(&[6; 32]);
    short(&mut payload, "revision-original");
    short(&mut payload, "resource-original");
    short(&mut payload, "action-original");
    match argument {
        Some(text) => {
            payload.extend_from_slice(&(text.len() as u32).to_le_bytes());
            payload.extend_from_slice(text.as_bytes());
        }
        None => payload.extend_from_slice(&u32::MAX.to_le_bytes()),
    }
    payload.push(0); // no receiver observation
    let mut row = (payload.len() as u32).to_le_bytes().to_vec();
    row.extend_from_slice(&payload);
    row
}

pub(super) fn pending_payload(rows: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = b"NSO1\x01\x00".to_vec();
    bytes.extend_from_slice(&(rows.len() as u16).to_le_bytes());
    for row in rows {
        bytes.extend_from_slice(row);
    }
    bytes
}

#[test]
fn managed_sender_codec_empty_is_borrowed_and_strict() {
    // The fresh native layout is an inert schema payload, never a receipt.
    let bytes = *b"NSO1\x01\x00\x00\x00";
    let view = NativeSenderOutboxV1::decode(&bytes).unwrap();
    assert!(view.is_empty());
    assert_eq!(view.as_bytes().as_ptr(), bytes.as_ptr());
    for length in 0..bytes.len() {
        assert!(NativeSenderOutboxV1::decode(&bytes[..length]).is_err());
    }
    let mut trailing = bytes.to_vec();
    trailing.push(0);
    assert!(NativeSenderOutboxV1::decode(&trailing).is_err());
    let mut future = bytes;
    future[4] = 2;
    assert!(NativeSenderOutboxV1::decode(&future).is_err());
}

#[test]
fn managed_sender_codec_rejects_announced_and_byte_capacity_before_rows() {
    let mut announced = *b"NSO1\x01\x00\x00\x00";
    announced[6..8].copy_from_slice(&257u16.to_le_bytes());
    assert!(matches!(
        NativeSenderOutboxV1::decode(&announced),
        Err(OutboxFailure::Capacity)
    ));
    let oversized = vec![0u8; 4 * 1024 * 1024 + 1];
    assert!(matches!(
        NativeSenderOutboxV1::decode(&oversized),
        Err(OutboxFailure::Capacity)
    ));
}

#[test]
fn managed_sender_codec_retains_exact_identity_and_argument_presence() {
    let bytes = pending_payload(&[pending_row(8, None), pending_row(9, Some(""))]);
    let view = NativeSenderOutboxV1::decode(&bytes).unwrap();
    assert_eq!(view.len(), 2);
    let original = view.find(&[8; 16]).unwrap().unwrap();
    assert_eq!(original.invocation, [8; 16]);
    assert_eq!(original.caller.root, [1; 32]);
    assert_eq!(original.caller.agent, None);
    assert_eq!(original.target.host, [3; 16]);
    assert_eq!(original.target.peer, 7);
    assert_eq!(original.target.binding, [6; 32]);
    assert_eq!(original.revision, "revision-original");
    assert_eq!(original.argument, None);
    assert_eq!(view.find(&[9; 16]).unwrap().unwrap().argument, Some(""));
    assert_eq!(view.find(&[10; 16]).unwrap(), None);
    let range = bytes.as_ptr_range();
    assert!(range.contains(&original.resource.as_ptr()));
}

#[test]
fn managed_sender_codec_refuses_duplicate_or_reordered_business_ids() {
    for rows in [
        vec![pending_row(8, None), pending_row(8, Some("changed"))],
        vec![pending_row(9, None), pending_row(8, None)],
        vec![pending_row(0, None)],
    ] {
        assert!(NativeSenderOutboxV1::decode(&pending_payload(&rows)).is_err());
    }
    let bytes = pending_payload(&[pending_row(8, Some("original"))]);
    NativeSenderOutboxV1::decode(&bytes).unwrap();
    for length in 0..bytes.len() {
        assert!(NativeSenderOutboxV1::decode(&bytes[..length]).is_err());
    }
    let oversized = "x".repeat(32 * 1024 + 1);
    assert!(
        NativeSenderOutboxV1::decode(&pending_payload(&[pending_row(8, Some(&oversized))]))
            .is_err()
    );
}

#[test]
fn managed_sender_codec_insert_preserves_original_rows() {
    let original = pending_payload(&[pending_row(9, Some("old"))]);
    let incoming = pending_payload(&[pending_row(8, Some(""))]);
    let view = NativeSenderOutboxV1::decode(&original).unwrap();
    let input = NativeSenderOutboxV1::decode(&incoming).unwrap();
    let row = input.find(&[8; 16]).unwrap().unwrap();
    let needed = view.insertion_bytes(&row).unwrap();
    let encoded = view.insert(&row).unwrap();
    assert_eq!(encoded.bytes().len(), needed);
    assert_eq!(
        encoded.bytes(),
        pending_payload(&[pending_row(8, Some("")), pending_row(9, Some("old"))])
    );
    assert_eq!(
        NativeSenderOutboxV1::decode(encoded.bytes()).unwrap().len(),
        2
    );
    assert!(
        NativeSenderOutboxV1::decode(encoded.bytes())
            .unwrap()
            .insertion_bytes(&row)
            .is_err()
    );
}

#[test]
fn managed_sender_codec_reserves_future_terminal_bytes_before_admission() {
    // MAX_ROWS is simultaneous count only. Every row must also leave room for
    // its largest allowed observation; otherwise new work is refused early.
    let mut rows = Vec::new();
    for id in 1..=15 {
        rows.push(pending_row(id, None));
    }
    let original = pending_payload(&rows);
    let view = NativeSenderOutboxV1::decode(&original).unwrap();
    let incoming = pending_payload(&[pending_row(16, None)]);
    let input = NativeSenderOutboxV1::decode(&incoming).unwrap();
    assert!(matches!(
        view.insertion_bytes(&input.find(&[16; 16]).unwrap().unwrap()),
        Err(OutboxFailure::Capacity)
    ));
    rows.push(pending_row(16, None));
    assert!(matches!(
        NativeSenderOutboxV1::decode(&pending_payload(&rows)),
        Err(OutboxFailure::Capacity)
    ));
}

#[test]
fn managed_sender_codec_observation_barrier_payload_and_terminal_consumption() {
    use runtime_collab::PublishedInvocationResult;
    let bytes = pending_payload(&[pending_row(8, None)]);
    let view = NativeSenderOutboxV1::decode(&bytes).unwrap();
    assert!(view.consumption_bytes(&[8; 16]).is_err());
    let response=PublishedInvocationResult{ admission_json: None, terminal_json: None,id:4,update_json:r#"{"state":"succeeded","invocation":"08080808-0808-0808-0808-080808080808","output":{"value":{"secret":"retained"}}}"#.into(),uncertain:false,cancellation_requested:false};
    let needed = view.observation_bytes(&[8; 16], &response).unwrap();
    let recorded = view.with_observation(&[8; 16], &response).unwrap();
    assert_eq!(recorded.bytes().len(), needed);
    let checked = NativeSenderOutboxV1::decode(recorded.bytes()).unwrap();
    let removed_bytes = checked.consumption_bytes(&[8; 16]).unwrap();
    assert_eq!(removed_bytes, 8);
    let removed = checked.consume(&[8; 16]).unwrap();
    assert_eq!(removed.bytes(), NativeSenderOutboxV1::empty_bytes());
    assert!(checked.observation_bytes(&[9; 16], &response).is_err());
    let mut changed = response.clone();
    changed.update_json = changed.update_json.replace("retained", "changed");
    assert!(checked.observation_bytes(&[8; 16], &changed).is_err());
    let mut corrupt = recorded.bytes().to_vec();
    *corrupt.last_mut().unwrap() = b'!';
    assert!(NativeSenderOutboxV1::decode(&corrupt).is_err());
}
#[test]
fn managed_sender_codec_uncertain_and_sanitized_rows_cannot_be_consumed() {
    use runtime_collab::PublishedInvocationResult;
    let bytes = pending_payload(&[pending_row(8, None)]);
    let view = NativeSenderOutboxV1::decode(&bytes).unwrap();
    for (code, uncertain) in [
        ("action_outcome_unknown", true),
        ("action_read_payload_unavailable", false),
    ] {
        let response = PublishedInvocationResult {
            admission_json: None,
            terminal_json: None,
            id: 4,
            update_json: format!(
                r#"{{"state":"failed","invocation":"08080808-0808-0808-0808-080808080808","error":{{"code":"{code}","message":"unavailable"}}}}"#
            ),
            uncertain,
            cancellation_requested: false,
        };
        let recorded = view.with_observation(&[8; 16], &response).unwrap();
        let checked = NativeSenderOutboxV1::decode(recorded.bytes()).unwrap();
        assert!(checked.consumption_bytes(&[8; 16]).is_err());
        assert_eq!(checked.find(&[8; 16]).unwrap().unwrap().argument, None);
    }
}

#[test]
fn managed_sender_codec_preserves_opaque_runtime_revision_grammar() {
    let mut original = pending_payload(&[pending_row(8, None)]);
    let offset = original
        .windows(b"revision-original".len())
        .position(|s| s == b"revision-original")
        .unwrap();
    original[offset] = b'\n';
    let view = NativeSenderOutboxV1::decode(&original).unwrap();
    let row = view.find(&[8; 16]).unwrap().unwrap();
    assert_eq!(row.revision, "\nevision-original");
    let empty = NativeSenderOutboxV1::decode(NativeSenderOutboxV1::empty_bytes()).unwrap();
    let encoded = empty.insert(&row).unwrap();
    assert_eq!(encoded.bytes(), original);
    let mut invalid = row;
    invalid.revision = "opaque\0revision";
    assert!(empty.insertion_bytes(&invalid).is_err());
    invalid.revision = "";
    assert!(empty.insertion_bytes(&invalid).is_err());
    original[offset] = 0;
    assert!(NativeSenderOutboxV1::decode(&original).is_err());
}
