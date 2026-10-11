use super::*;
use crate::state::{GovernedIdentity, RedemptionReceipt, json_resource_tests::*};

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
    let decoded = decode::<GovernedIdentity>(wire.as_bytes(), limits()).unwrap();
    assert_eq!(
        postcard::to_allocvec(&decoded).unwrap(),
        postcard::to_allocvec(&identity).unwrap()
    );
    assert_eq!(decoded.device_cert, identity.device_cert);
}

#[test]
fn guarded_json_grant_round_trips_to_identical_bytes() {
    let capability = grant_fixture();
    let wire = serde_json::to_vec(&capability).unwrap();
    let decoded = decode::<tilo_action_policy::Grant>(&wire, limits()).unwrap();
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), wire);
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
    let exact = JsonLimits {
        collection_entries: 32,
        ..limits()
    };
    assert_eq!(decode::<RedemptionReceipt>(&wire, exact).unwrap(), receipt);
    let over = String::from_utf8(wire)
        .unwrap()
        .replacen("[1,", "[null,", 1);
    // At zero entries the actual seed refuses before malformed null can be decoded.
    assert_eq!(
        error(decode::<RedemptionReceipt>(
            over.as_bytes(),
            JsonLimits {
                collection_entries: 0,
                ..limits()
            },
        )),
        PeerResourceError::Capacity
    );
}

#[test]
fn guarded_json_numeric_policy_covers_ignored_extensions_without_rewriting_strings() {
    for number in [
        "0.1000000000000000001",
        "1e-64",
        "1e64",
        "18446744073709551615",
    ] {
        let wire = identity_wire(&format!(",\"future\":{number}"));
        assert!(
            decode::<GovernedIdentity>(&wire, limits()).is_ok(),
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
            error(decode::<GovernedIdentity>(&wire, limits())),
            PeerResourceError::Capacity,
            "{number}"
        );
    }
    let wire = identity_wire(",\"123456789012345678901e999\":\"123456789012345678901e999\"");
    assert!(decode::<GovernedIdentity>(&wire, limits()).is_ok());
}

#[test]
fn guarded_json_bad_strings_signature_lengths_and_trailing_input_are_malformed() {
    for bytes in [
        b"{\"device_cert\":\"\\uD800\"}".as_slice(),
        b"{\"device_cert\":\"\\q\"}",
        b"{",
        b"{} {}",
    ] {
        assert_eq!(
            error(decode::<GovernedIdentity>(bytes, limits())),
            PeerResourceError::Malformed
        );
    }
    let mut wire: serde_json::Value = serde_json::from_slice(&identity_wire("")).unwrap();
    wire["device_cert"]["signature"] = serde_json::json!([1, 2, 3]);
    assert_eq!(
        error(decode::<GovernedIdentity>(
            &serde_json::to_vec(&wire).unwrap(),
            limits(),
        )),
        PeerResourceError::Malformed
    );
}

#[test]
fn guarded_json_small_allocation_limit_refuses() {
    let wire = identity_wire("");
    assert!(decode::<GovernedIdentity>(&wire, limits()).is_ok());
    assert_eq!(
        error(decode::<GovernedIdentity>(
            &wire,
            JsonLimits {
                allocation_bytes: 64,
                ..limits()
            },
        )),
        PeerResourceError::Capacity
    );
}

#[test]
fn guarded_json_signature_is_exactly_64_entries() {
    let wire = identity_wire("");
    let exact = JsonLimits {
        collection_entries: 64,
        ..limits()
    };
    assert!(decode::<GovernedIdentity>(&wire, exact).is_ok());
    assert_eq!(
        error(decode::<GovernedIdentity>(
            &wire,
            JsonLimits {
                collection_entries: 63,
                ..limits()
            },
        )),
        PeerResourceError::Capacity
    );

    // The signature is a fixed array: it reads its 64 entries and stops, so
    // an extra element is malformed at any quota.
    let mut extra: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    extra["device_cert"]["signature"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::Value::Null);
    let extra = serde_json::to_vec(&extra).unwrap();
    assert_eq!(
        error(decode::<GovernedIdentity>(&extra, exact)),
        PeerResourceError::Malformed
    );
    assert_eq!(
        error(decode::<GovernedIdentity>(&extra, limits())),
        PeerResourceError::Malformed
    );
}

#[test]
fn guarded_json_ignored_nesting_limit_and_malformed_numbers_are_closed() {
    // The enclosing identity object consumes one of the 64 lexical levels.
    let exact = identity_wire(&format!(
        ",\"future\":{}0{}",
        "[".repeat(63),
        "]".repeat(63)
    ));
    assert!(decode::<GovernedIdentity>(&exact, limits()).is_ok());
    let over = identity_wire(&format!(
        ",\"future\":{}0{}",
        "[".repeat(64),
        "]".repeat(64)
    ));
    assert_eq!(
        error(decode::<GovernedIdentity>(&over, limits())),
        PeerResourceError::Capacity
    );
    for value in ["1e", "1e+", "0.1.2", "--1"] {
        assert_eq!(
            error(decode::<GovernedIdentity>(
                &identity_wire(&format!(",\"future\":{value}")),
                limits(),
            )),
            PeerResourceError::Malformed
        );
    }
}

#[test]
fn guarded_json_long_escaped_input_round_trips_and_invalid_forms_are_malformed() {
    let (mut identity, _) = identity_fixture();
    // Valid JSON value allocation fixture; signature admission is intentionally
    // separate. The signed positive fixture above proves unchanged identity.
    identity.device_cert.device_label = "quoted\"\\\n🦀".repeat(2048);
    let wire = serde_json::to_vec(&identity).unwrap();
    let decoded = decode::<GovernedIdentity>(&wire, limits()).unwrap();
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), wire);

    let long_key = format!("{{\"{}\\q\":0}}", "\\u0001".repeat(8192));
    let unexpected_string = format!("\"{}\"", "\\u0001".repeat(8192));
    for invalid in [long_key.as_bytes(), unexpected_string.as_bytes()] {
        assert_eq!(
            error(decode::<GovernedIdentity>(invalid, limits())),
            PeerResourceError::Malformed
        );
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
        assert_eq!(
            error(decode::<GovernedIdentity>(invalid.as_bytes(), limits())),
            PeerResourceError::Malformed
        );
    }
}

#[test]
fn guarded_json_owned_string_limit_aggregates_across_grant_fields() {
    let mut one = grant_fixture();
    one.id = tilo_action_policy::GrantId::new("x".repeat(4096));
    let mut many = one.clone();
    many.subject = tilo_action_policy::Subject::Person("x".repeat(4096));
    many.granted_by = tilo_action_policy::Subject::Person("x".repeat(4096));
    let limit = JsonLimits {
        allocation_bytes: 12 * 1024,
        ..limits()
    };
    assert!(decode::<tilo_action_policy::Grant>(&serde_json::to_vec(&one).unwrap(), limit).is_ok());
    let wire = serde_json::to_vec(&many).unwrap();
    assert_eq!(
        error(decode::<tilo_action_policy::Grant>(&wire, limit)),
        PeerResourceError::Capacity
    );
}
