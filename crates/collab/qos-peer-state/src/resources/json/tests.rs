use super::*;
use crate::state::{GovernedIdentity, RedemptionReceipt, json_resource_tests::*};
use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};

fn setup() -> (NativeAllowance, NativeWorkTurn) {
    let allowance = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    (allowance, turn)
}
fn limits() -> JsonLimits {
    JsonLimits {
        allocation_bytes: 256 * 1024,
        collection_entries: 4096,
    }
}
fn identity_wire(extra: &str) -> Vec<u8> {
    let mut wire = serde_json::to_string(&identity_fixture().0).unwrap();
    wire.pop();
    wire.push_str(extra);
    wire.push('}');
    wire.into_bytes()
}
fn error<T>(value: Result<T, PeerResourceError>) -> PeerResourceError {
    value.err().expect("expected refusal")
}

#[test]
fn guarded_json_signed_identity_escaped_keys_and_values_preserve_canonical_data() {
    let (identity, _) = identity_fixture();
    let wire = serde_json::to_string(&identity)
        .unwrap()
        .replace("\"device_cert\"", "\"device_\\u0063ert\"")
        .replace('🦀', "\\uD83E\\uDD80");
    let (allowance, turn) = setup();
    let before = allowance.available_bytes();
    let held = JsonHeld::<GovernedIdentity>::decode(wire.as_bytes(), limits(), &turn).unwrap();
    assert_eq!(
        postcard::to_allocvec(held.value()).unwrap(),
        postcard::to_allocvec(&identity).unwrap()
    );
    assert_eq!(
        allowance.available_bytes(),
        before - limits().allocation_bytes
    );
    drop(turn);
    assert_eq!(held.value().device_cert, identity.device_cert);
    drop(held);
    // Turn metadata may also have been released, but retained DTO credit must be back.
    assert!(allowance.available_bytes() >= before);
}

#[test]
fn guarded_json_exact_output_cap_escapes_and_holds_its_own_credit() {
    let capability = capability_fixture();
    let wire = serde_json::to_vec(&capability).unwrap();
    let (allowance, turn) = setup();
    let held = JsonHeld::<collab_types::caps::CapOverride>::decode(&wire, limits(), &turn).unwrap();
    let before = allowance.available_bytes();
    assert_eq!(
        error(held.encode(wire.len() - 1, &turn)),
        PeerResourceError::Capacity
    );
    assert_eq!(allowance.available_bytes(), before);
    let output = held.encode(wire.len(), &turn).unwrap();
    assert_eq!(output.as_bytes(), wire);
    assert_eq!(allowance.available_bytes(), before - wire.len());
    drop(held);
    assert_eq!(output.as_bytes(), wire);
    drop(output);
    assert_eq!(
        allowance.available_bytes(),
        before + limits().allocation_bytes
    );
}

#[test]
fn guarded_json_actual_receipt_array_exact_limit_and_next_seed_refusal() {
    let receipt = RedemptionReceipt {
        invite_id: [1; 32],
        user_public_key: [2; 32],
        device_public_key: [3; 32],
        request_id: [4; 32],
        role: crate::join::MemberRole::Editor,
    };
    let wire = serde_json::to_vec(&receipt).unwrap();
    let (_, turn) = setup();
    let exact = JsonLimits {
        collection_entries: 32,
        ..limits()
    };
    assert_eq!(
        JsonHeld::<RedemptionReceipt>::decode(&wire, exact, &turn)
            .unwrap()
            .value(),
        &receipt
    );
    let over = String::from_utf8(wire)
        .unwrap()
        .replacen("[1,", "[null,", 1);
    // At zero entries the actual seed refuses before malformed null can be decoded.
    assert_eq!(
        error(JsonHeld::<RedemptionReceipt>::decode(
            over.as_bytes(),
            JsonLimits {
                collection_entries: 0,
                ..limits()
            },
            &turn
        )),
        PeerResourceError::Capacity
    );
}

#[test]
fn guarded_json_numeric_policy_covers_ignored_extensions_without_rewriting_strings() {
    let (_, turn) = setup();
    for number in [
        "0.1000000000000000001",
        "1e-64",
        "1e64",
        "18446744073709551615",
    ] {
        let wire = identity_wire(&format!(",\"future\":{number}"));
        assert!(
            JsonHeld::<GovernedIdentity>::decode(&wire, limits(), &turn).is_ok(),
            "{number}"
        );
    }
    for number in [
        "0.10000000000000000001",
        "1e-65",
        "1e65",
        "123456789012345678901",
    ] {
        let wire = identity_wire(&format!(",\"future\":{number}"));
        assert_eq!(
            error(JsonHeld::<GovernedIdentity>::decode(&wire, limits(), &turn)),
            PeerResourceError::Capacity,
            "{number}"
        );
    }
    let wire = identity_wire(",\"123456789012345678901e999\":\"123456789012345678901e999\"");
    assert!(JsonHeld::<GovernedIdentity>::decode(&wire, limits(), &turn).is_ok());
}

#[test]
fn guarded_json_bad_strings_signature_lengths_and_trailing_input_release_credit() {
    let (allowance, turn) = setup();
    let before = allowance.available_bytes();
    for bytes in [
        b"{\"device_cert\":\"\\uD800\"}".as_slice(),
        b"{\"device_cert\":\"\\q\"}",
        b"{",
        b"{} {}",
    ] {
        assert_eq!(
            error(JsonHeld::<GovernedIdentity>::decode(bytes, limits(), &turn)),
            PeerResourceError::Malformed
        );
        assert_eq!(allowance.available_bytes(), before);
    }
    let mut wire: serde_json::Value = serde_json::from_slice(&identity_wire("")).unwrap();
    wire["device_cert"]["signature"] = serde_json::json!([1, 2, 3]);
    assert_eq!(
        error(JsonHeld::<GovernedIdentity>::decode(
            &serde_json::to_vec(&wire).unwrap(),
            limits(),
            &turn
        )),
        PeerResourceError::Malformed
    );
    assert_eq!(allowance.available_bytes(), before);
}

#[test]
fn guarded_json_low_credit_and_dto_capacity_refuse_without_leaking() {
    let wire = identity_wire("");
    let (allowance, turn) = setup();
    let before = allowance.available_bytes();
    let block = allowance
        .reserve(before - limits().allocation_bytes)
        .unwrap();
    assert_eq!(
        error(JsonHeld::<GovernedIdentity>::decode(&wire, limits(), &turn)),
        PeerResourceError::Capacity
    );
    drop(block);
    assert_eq!(allowance.available_bytes(), before);
    assert_eq!(
        error(JsonHeld::<GovernedIdentity>::decode(
            &wire,
            JsonLimits {
                allocation_bytes: 64,
                ..limits()
            },
            &turn
        )),
        PeerResourceError::Capacity
    );
    assert_eq!(allowance.available_bytes(), before);
}

#[test]
fn guarded_json_dynamic_signature_sequence_accepts_exact_end_probe() {
    let wire = identity_wire("");
    let (_, turn) = setup();
    let exact = JsonLimits {
        collection_entries: 64,
        ..limits()
    };
    assert!(JsonHeld::<GovernedIdentity>::decode(&wire, exact, &turn).is_ok());
    assert_eq!(
        error(JsonHeld::<GovernedIdentity>::decode(
            &wire,
            JsonLimits {
                collection_entries: 63,
                ..limits()
            },
            &turn
        )),
        PeerResourceError::Capacity
    );

    // A real ByteBuf Vec asks for the next element after its64-byte signature.
    // At the quota boundary the seed must refuse BEFORE decoding malformed null.
    let mut extra: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    extra["device_cert"]["signature"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::Value::Null);
    let extra = serde_json::to_vec(&extra).unwrap();
    assert_eq!(
        error(JsonHeld::<GovernedIdentity>::decode(&extra, exact, &turn)),
        PeerResourceError::Capacity
    );
    assert_eq!(
        error(JsonHeld::<GovernedIdentity>::decode(
            &extra,
            limits(),
            &turn
        )),
        PeerResourceError::Malformed
    );
}

#[test]
fn guarded_json_parser_plus_dto_exact_allowance_boundary_is_precharged() {
    let wire = identity_wire("");
    let (allowance, turn) = setup();
    let required = parser_bytes(wire.len()).unwrap() + limits().allocation_bytes;
    let before = allowance.available_bytes();
    let block = allowance.reserve(before - required + 1).unwrap();
    let result = crate::native_footprint_vendor_tests::assert_precharged(&allowance, || {
        JsonHeld::<GovernedIdentity>::decode(&wire, limits(), &turn)
    });
    assert_eq!(error(result), PeerResourceError::Capacity);
    drop(block);
    let block = allowance.reserve(before - required).unwrap();
    let held = crate::native_footprint_vendor_tests::assert_precharged(&allowance, || {
        JsonHeld::<GovernedIdentity>::decode(&wire, limits(), &turn)
    })
    .unwrap();
    assert_eq!(
        allowance.available_bytes(),
        required - limits().allocation_bytes
    );
    drop(held);
    drop(block);
    assert_eq!(allowance.available_bytes(), before);
}

#[test]
fn guarded_json_ignored_nesting_limit_and_malformed_numbers_are_closed() {
    let (_, turn) = setup();
    // The enclosing identity object consumes one of the64 lexical levels.
    let exact = identity_wire(&format!(
        ",\"future\":{}0{}",
        "[".repeat(63),
        "]".repeat(63)
    ));
    assert!(JsonHeld::<GovernedIdentity>::decode(&exact, limits(), &turn).is_ok());
    let over = identity_wire(&format!(
        ",\"future\":{}0{}",
        "[".repeat(64),
        "]".repeat(64)
    ));
    assert_eq!(
        error(JsonHeld::<GovernedIdentity>::decode(&over, limits(), &turn)),
        PeerResourceError::Capacity
    );
    for value in ["1e", "1e+", "0.1.2", "--1"] {
        assert_eq!(
            error(JsonHeld::<GovernedIdentity>::decode(
                &identity_wire(&format!(",\"future\":{value}")),
                limits(),
                &turn
            )),
            PeerResourceError::Malformed
        );
    }
}

#[test]
fn guarded_json_long_escaped_input_errors_and_output_are_precharged() {
    let (mut identity, _) = identity_fixture();
    // Valid JSON value allocation fixture; signature admission is intentionally
    // separate. The signed positive fixture above proves unchanged identity.
    identity.device_cert.device_label = "quoted\"\\\n🦀".repeat(2048);
    let wire = serde_json::to_vec(&identity).unwrap();
    let (allowance, turn) = setup();
    let before = allowance.available_bytes();
    let held = crate::native_footprint_vendor_tests::assert_precharged(&allowance, || {
        JsonHeld::<GovernedIdentity>::decode(&wire, limits(), &turn)
    })
    .unwrap();
    let output = crate::native_footprint_vendor_tests::assert_precharged(&allowance, || {
        held.encode(wire.len(), &turn)
    })
    .unwrap();
    assert_eq!(output.as_bytes(), wire);
    drop(output);
    drop(held);
    assert_eq!(allowance.available_bytes(), before);

    let long_key = format!("{{\"{}\\q\":0}}", "\\u0001".repeat(8192));
    let unexpected_string = format!("\"{}\"", "\\u0001".repeat(8192));
    for invalid in [long_key.as_bytes(), unexpected_string.as_bytes()] {
        let result = crate::native_footprint_vendor_tests::assert_precharged(&allowance, || {
            JsonHeld::<GovernedIdentity>::decode(invalid, limits(), &turn)
        });
        assert_eq!(error(result), PeerResourceError::Malformed);
        assert_eq!(allowance.available_bytes(), before);
    }
    // Exercise numeric parsing itself, rather than only ignore_value's scanner.
    for decimal in [
        "0.1000000000000000001",
        "9.999999999999999999e-64",
        "99999999999999999999",
    ] {
        let mut data: serde_json::Value = serde_json::from_slice(&identity_wire("")).unwrap();
        data["device_cert"]["expires_at_ns"] = serde_json::Value::Null;
        let invalid = serde_json::to_string(&data).unwrap().replace(
            "\"expires_at_ns\":null",
            &format!("\"expires_at_ns\":{decimal}"),
        );
        let result = crate::native_footprint_vendor_tests::assert_precharged(&allowance, || {
            JsonHeld::<GovernedIdentity>::decode(invalid.as_bytes(), limits(), &turn)
        });
        assert_eq!(error(result), PeerResourceError::Malformed);
        assert_eq!(allowance.available_bytes(), before);
    }
}

#[test]
fn guarded_json_owned_string_budget_aggregates_across_capability_fields() {
    let mut one = capability_fixture();
    one.id = "x".repeat(4096);
    let mut many = one.clone();
    many.subject = collab_types::caps::Subject::Principal(collab_types::caps::Principal::Person(
        "x".repeat(4096),
    ));
    many.scope = collab_types::caps::Scope::Action {
        app: "a".repeat(4096),
        action: "b".repeat(4096),
    };
    many.granted_by = Some(collab_types::caps::Principal::Person("x".repeat(4096)));
    let (allowance, turn) = setup();
    let limit = JsonLimits {
        allocation_bytes: 12 * 1024,
        ..limits()
    };
    assert!(
        JsonHeld::<collab_types::caps::CapOverride>::decode(
            &serde_json::to_vec(&one).unwrap(),
            limit,
            &turn
        )
        .is_ok()
    );
    let wire = serde_json::to_vec(&many).unwrap();
    let before = allowance.available_bytes();
    let result = crate::native_footprint_vendor_tests::assert_precharged(&allowance, || {
        JsonHeld::<collab_types::caps::CapOverride>::decode(&wire, limit, &turn)
    });
    assert_eq!(error(result), PeerResourceError::Capacity);
    assert_eq!(allowance.available_bytes(), before);
}

#[test]
fn guarded_json_dto_and_output_credit_survive_sealed_work_turn() {
    let wire = serde_json::to_vec(&capability_fixture()).unwrap();
    let (allowance, turn) = setup();
    let held = JsonHeld::<collab_types::caps::CapOverride>::decode(&wire, limits(), &turn).unwrap();
    let output = held.encode(wire.len(), &turn).unwrap();
    let retained = allowance.retained_bytes();
    allowance.finish_turn();
    assert_eq!(allowance.available_bytes(), 0);
    assert_eq!(
        error(held.encode(wire.len(), &turn)),
        PeerResourceError::Capacity
    );
    assert_eq!(output.as_bytes(), wire);
    assert_eq!(allowance.retained_bytes(), retained);
    drop(held);
    assert_eq!(
        allowance.retained_bytes(),
        retained - limits().allocation_bytes
    );
    drop(output);
    assert_eq!(
        allowance.retained_bytes(),
        retained - limits().allocation_bytes - wire.len()
    );
}
