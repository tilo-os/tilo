use super::super::{CallPhaseV1, CallV1, ChatCallBindingV1, DirectionsV1, FIELD};
use super::*;
use crate::resources::native::{NativeBatch, NativeDocument};
use crate::room::read::LegacyRead;
use automerge::{ActorId, AutoCommit, ROOT, ScalarValue, transaction::Transactable};
use std::collections::BTreeMap;
use tilo_automerge::ApplyIncoming;

fn policy() -> RoomMediaV1 {
    RoomMediaV1 {
        version: 1,
        room: [1; 16],
        authorization_revision: 1,
        authorization_basis: [3; 32],
        calls: BTreeMap::from([(
            [7; 32],
            CallV1 {
                binding: ChatCallBindingV1 {
                    app_id: "so.tilo.chat".into(),
                    record_id: String::new(),
                    channel_id: "voice".into(),
                    channel_object: "1@01".into(),
                    chat_workspace_id: "main".into(),
                },
                epoch: 1,
                phase: CallPhaseV1::Active,
                allowed: DirectionsV1 {
                    publish: true,
                    subscribe: true,
                },
                denied: BTreeMap::from([(
                    [8; 32],
                    DirectionsV1 {
                        publish: false,
                        subscribe: true,
                    },
                )]),
            },
        )]),
    }
}
fn room(bytes: Option<Vec<u8>>) -> AutoCommit {
    let mut room = AutoCommit::new();
    room.put(ROOT, "room", uuid::Uuid::from_bytes([1; 16]).to_string())
        .unwrap();
    if let Some(bytes) = bytes {
        room.put(ROOT, FIELD, ScalarValue::Bytes(bytes)).unwrap();
    }
    room.commit();
    room
}
fn limits() -> OuterLimits {
    OuterLimits {
        allocation_bytes: 2 * 1024 * 1024,
        collection_entries: 4096,
    }
}
fn admitted(room: &mut AutoCommit) -> NativeDocument {
    NativeBatch::new(8 * 1024 * 1024)
        .load(&room.save())
        .unwrap()
}
fn parse_within(
    native: &NativeDocument,
    limits: OuterLimits,
) -> Result<Option<BoundedPolicy>, Error> {
    parse(&native.read(), &BoundedDecode { limits })
}
fn bounded(room: &mut AutoCommit) -> Result<Option<RoomMediaV1>, Error> {
    let held = parse_within(&admitted(room), limits())?;
    Ok(held.map(|held| held.policy().clone()))
}

#[test]
fn media_reader_retains_decoded_policy_after_source_drop() {
    let expected = policy();
    let mut room = room(Some(super::super::encode(&expected).unwrap()));
    let native = admitted(&mut room);
    let held = parse_within(&native, limits()).unwrap().unwrap();
    assert_eq!(held.policy(), &expected);
    drop(native);
    drop(room);
    assert_eq!(held.policy(), &expected);
}

#[test]
fn media_reader_matches_legacy_absence_malformed_binding_and_canonical_rejection() {
    let bytes = super::super::encode(&policy()).unwrap();
    for mode in 0..6 {
        let mut doc = room(Some(bytes.clone()));
        match mode {
            0 => {
                doc.delete(ROOT, FIELD).unwrap();
            }
            1 => {
                doc.put(ROOT, FIELD, "wrong scalar").unwrap();
            }
            2 => {
                doc.put(ROOT, FIELD, ScalarValue::Bytes(vec![0xff]))
                    .unwrap();
            }
            3 => {
                doc.put(ROOT, "room", "wrong room").unwrap();
            }
            4 => {
                let mut tail = bytes.clone();
                tail.push(0);
                doc.put(ROOT, FIELD, ScalarValue::Bytes(tail)).unwrap();
            }
            _ => {}
        }
        doc.commit();
        let legacy = historical_parse(&doc);
        let native = bounded(&mut doc);
        match legacy {
            Ok(value) => assert_eq!(native.unwrap(), value),
            Err(error) => assert_eq!(native.err(), Some(Error::Policy(error))),
        }
    }
}

#[test]
fn media_reader_conflict_precedes_decoding_and_equal_conflicts_remain_valid() {
    for equal in [false, true] {
        let bytes = super::super::encode(&policy()).unwrap();
        let mut base = room(None);
        base.set_actor(ActorId::from(vec![1]));
        let mut fork = base.fork();
        fork.set_actor(ActorId::from(vec![2]));
        base.put(ROOT, FIELD, ScalarValue::Bytes(bytes.clone()))
            .unwrap();
        base.commit();
        fork.put(
            ROOT,
            FIELD,
            ScalarValue::Bytes(if equal { bytes } else { vec![0xff] }),
        )
        .unwrap();
        fork.commit();
        base.merge_incoming(&mut fork).unwrap();
        let legacy = historical_parse(&base);
        assert_eq!(
            legacy.as_ref().err(),
            if equal {
                None
            } else {
                Some(&MediaPolicyError::Conflict)
            }
        );
        let native = bounded(&mut base);
        if equal {
            assert_eq!(native.unwrap(), legacy.unwrap());
        } else {
            assert_eq!(
                native.err(),
                Some(Error::Policy(MediaPolicyError::Conflict))
            );
        }
    }
}

#[test]
fn media_reader_validation_refuses_noncanonical_counters_and_duplicate_objects() {
    let mut value = policy();
    let call = value.calls.values().next().unwrap().clone();
    value.calls.clear();
    for id in 1..=64 {
        let mut call = call.clone();
        call.binding.channel_object = format!("{id}@01");
        value.calls.insert([id; 32], call);
    }
    super::super::validate(&value).unwrap();
    for counter in ["0", "00", "01", "+1", "-1", "18446744073709551616", ""] {
        let mut invalid = call.binding.clone();
        invalid.channel_object = format!("{counter}@01");
        assert_eq!(invalid.validate(), Err(MediaPolicyError::Binding));
    }
    let mut valid = call.binding;
    valid.channel_object = "18446744073709551615@01".into();
    valid.validate().unwrap();
    value
        .calls
        .get_mut(&[64; 32])
        .unwrap()
        .binding
        .channel_object = "1@01".into();
    assert_eq!(
        super::super::validate(&value),
        Err(MediaPolicyError::Binding)
    );
}

#[test]
fn media_reader_limit_refusal_does_not_replace_disabled_rooms_with_invalid_policy() {
    let mut doc = room(Some(super::super::encode(&policy()).unwrap()));
    let result = parse_within(
        &admitted(&mut doc),
        OuterLimits {
            allocation_bytes: 16,
            ..limits()
        },
    );
    assert!(matches!(
        result,
        Err(Error::Decode(PeerResourceError::Capacity))
    ));
    let absent = room(None);
    assert!(
        parse(&LegacyRead(&absent), &LegacyPolicy)
            .unwrap()
            .is_none()
    );
}

#[test]
fn media_reader_checks_maximum_maps_and_noncanonical_varints_against_original_parser() {
    let mut value = policy();
    let call = value.calls.values().next().unwrap().clone();
    value.calls.clear();
    for id in 1..=64 {
        let mut call = call.clone();
        call.binding.channel_object = format!("{id}@01");
        call.denied.clear();
        for root in 1..=8 {
            call.denied.insert(
                [root; 32],
                DirectionsV1 {
                    publish: true,
                    subscribe: false,
                },
            );
        }
        value.calls.insert([id; 32], call);
    }
    let bytes = historical_encode(&value).unwrap();
    let mut doc = room(Some(bytes));
    let parsed = parse_within(
        &admitted(&mut doc),
        OuterLimits {
            allocation_bytes: 8 * 1024 * 1024,
            ..limits()
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(parsed.policy(), &value);
    assert_eq!(
        historical_parse(&doc).unwrap().as_ref(),
        Some(parsed.policy())
    );
    let mut single = historical_encode(&policy()).unwrap();
    // Revision1 at offset17: valid Postcard non-minimal varint, invalid canonical bytes.
    assert_eq!(single[17], 1);
    single.splice(17..18, [0x81, 0]);
    let mut noncanonical = room(Some(single));
    assert_eq!(
        historical_parse(&noncanonical),
        Err(MediaPolicyError::Malformed)
    );
    assert_eq!(
        bounded(&mut noncanonical).err(),
        Some(Error::Policy(MediaPolicyError::Malformed))
    );
}

#[test]
fn media_reader_preserves_validation_error_order_for_binding_counter_and_capacity() {
    for mode in 0..8 {
        let mut value = policy();
        match mode {
            0 => value.version = 2,
            1 => value.room = [0; 16],
            2 => value.authorization_revision = 0,
            3 => {
                value.calls.values_mut().next().unwrap().epoch = 0;
            }
            4 => {
                value
                    .calls
                    .values_mut()
                    .next()
                    .unwrap()
                    .binding
                    .channel_object = "01@01".into();
            }
            5 => {
                value.calls.values_mut().next().unwrap().denied.insert(
                    [0; 32],
                    DirectionsV1 {
                        publish: true,
                        subscribe: false,
                    },
                );
            }
            6 => {
                let call = value.calls.values().next().unwrap().clone();
                value.calls.insert([9; 32], call);
            }
            _ => {
                value.calls.values_mut().next().unwrap().denied.insert(
                    [9; 32],
                    DirectionsV1 {
                        publish: false,
                        subscribe: false,
                    },
                );
            }
        }
        let mut doc = room(Some(postcard::to_allocvec(&value).unwrap()));
        let original = historical_parse(&doc).unwrap_err();
        assert_eq!(bounded(&mut doc).err(), Some(Error::Policy(original)));
        assert_eq!(super::super::validate(&value), historical_validate(&value));
    }
}

// Literal pre-extraction oracle, retained independently of the production helper.
use super::super::{
    MAX_CALLS, MAX_CHANNEL_BYTES, MAX_DENIED_PER_CALL, MAX_DENIED_TOTAL, MAX_OBJECT_BYTES,
    MAX_POLICY_BYTES, MAX_RECORD_BYTES, lower_hex, preflight_policy, text_valid,
};
use automerge::{ReadDoc, Value};
use std::collections::BTreeSet;
use tilo_user_identity::canonical::{canonical_bytes, from_canonical_bytes};
fn historical_parse(doc: &AutoCommit) -> Result<Option<RoomMediaV1>, MediaPolicyError> {
    let values = doc
        .get_all(ROOT, FIELD)
        .map_err(|_| MediaPolicyError::Malformed)?;
    let mut bytes: Option<&[u8]> = None;
    for (value, _) in &values {
        let Value::Scalar(value) = value else {
            return Err(MediaPolicyError::Malformed);
        };
        let ScalarValue::Bytes(value) = value.as_ref() else {
            return Err(MediaPolicyError::Malformed);
        };
        if value.len() > MAX_POLICY_BYTES {
            return Err(MediaPolicyError::Capacity);
        }
        if bytes.is_some_and(|prior| prior != value.as_slice()) {
            return Err(MediaPolicyError::Conflict);
        }
        bytes = Some(value);
    }
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    preflight_policy(bytes)?;
    let policy: RoomMediaV1 =
        from_canonical_bytes(bytes).map_err(|_| MediaPolicyError::Malformed)?;
    if historical_encode(&policy)? != bytes {
        return Err(MediaPolicyError::Malformed);
    }
    let rooms = doc
        .get_all(ROOT, "room")
        .map_err(|_| MediaPolicyError::Malformed)?;
    let expected = uuid::Uuid::from_bytes(policy.room).to_string();
    if rooms.is_empty() || rooms.iter().any(|(value, _)| !matches!(value, Value::Scalar(value) if matches!(value.as_ref(), ScalarValue::Str(room) if room.as_str() == expected))) {
        return Err(MediaPolicyError::Binding);
    }
    Ok(Some(policy))
}

fn historical_validate(value: &RoomMediaV1) -> Result<(), MediaPolicyError> {
    if value.version != 1 {
        return Err(MediaPolicyError::UnsupportedVersion);
    }
    if value.room == [0; 16] {
        return Err(MediaPolicyError::Binding);
    }
    if value.authorization_revision == 0 {
        return Err(MediaPolicyError::Counter);
    }
    if value.calls.len() > MAX_CALLS {
        return Err(MediaPolicyError::Capacity);
    }
    let mut bindings = BTreeSet::new();
    let mut denied_total = 0usize;
    for (id, call) in &value.calls {
        if *id == [0; 32] {
            return Err(MediaPolicyError::Binding);
        }
        historical_binding(&call.binding)?;
        if !bindings.insert((
            &call.binding.app_id,
            &call.binding.record_id,
            &call.binding.channel_object,
        )) {
            return Err(MediaPolicyError::Binding);
        }
        if call.epoch == 0 {
            return Err(MediaPolicyError::Counter);
        }
        denied_total = denied_total
            .checked_add(call.denied.len())
            .ok_or(MediaPolicyError::Capacity)?;
        if call.denied.len() > MAX_DENIED_PER_CALL || denied_total > MAX_DENIED_TOTAL {
            return Err(MediaPolicyError::Capacity);
        }
        if call
            .denied
            .iter()
            .any(|(root, denied)| *root == [0; 32] || !(denied.publish || denied.subscribe))
        {
            return Err(MediaPolicyError::Binding);
        }
    }
    Ok(())
}

fn historical_binding(value: &ChatCallBindingV1) -> Result<(), MediaPolicyError> {
    if value.app_id != "so.tilo.chat"
        || !text_valid(&value.record_id, MAX_RECORD_BYTES, true)
        || !text_valid(&value.channel_id, MAX_CHANNEL_BYTES, false)
        || !text_valid(&value.chat_workspace_id, MAX_CHANNEL_BYTES, false)
        || !text_valid(&value.channel_object, MAX_OBJECT_BYTES, false)
    {
        return Err(MediaPolicyError::Binding);
    }
    let (counter, actor) = value
        .channel_object
        .split_once('@')
        .ok_or(MediaPolicyError::Binding)?;
    if counter
        .parse::<u64>()
        .ok()
        .is_none_or(|value| value == 0 || value.to_string() != counter)
        || actor.is_empty()
        || !actor.len().is_multiple_of(2)
        || !lower_hex(actor)
    {
        return Err(MediaPolicyError::Binding);
    }
    Ok(())
}
fn historical_encode(value: &RoomMediaV1) -> Result<Vec<u8>, MediaPolicyError> {
    historical_validate(value)?;
    let bytes = canonical_bytes(value).map_err(|_| MediaPolicyError::Malformed)?;
    if bytes.len() > MAX_POLICY_BYTES {
        return Err(MediaPolicyError::Capacity);
    }
    Ok(bytes)
}
