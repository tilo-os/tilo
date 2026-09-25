use qos_agent_control_protocol::{
    AgentBinding, AgentResponseKind, ExpectedResponse, MAX_REQUEST_FRAME_BYTES,
    MAX_RESPONSE_FRAME_BYTES, ValidationError, canonical_request, compare_revisions,
    decode_request, decode_response,
};
use serde_json::Value;
use std::cmp::Ordering;

fn fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/v1.json")).unwrap()
}

fn materialize(entry: &Value) -> Vec<u8> {
    if let Some(raw) = entry.get("raw").and_then(Value::as_str) {
        return raw.as_bytes().to_vec();
    }
    let mut value = entry["value"].clone();
    if let Some(repeat) = entry.get("repeat") {
        let field = repeat["field"].as_str().unwrap();
        let part = repeat["value"].as_str().unwrap();
        let count = repeat["count"].as_u64().unwrap() as usize;
        value[field] = Value::String(part.repeat(count));
    }
    if let Some(count) = entry.get("repeatAgent").and_then(Value::as_u64) {
        value["agents"] = Value::Array(
            (0..count)
                .map(|index| {
                    serde_json::json!({
                        "id": format!("agent-{index}"),
                        "name": format!("Agent {index}"),
                        "description": "Work",
                        "status": "running",
                        "conversation": []
                    })
                })
                .collect(),
        );
    }
    if let Some(count) = entry.get("repeatMessage").and_then(Value::as_u64) {
        value["messages"] = Value::Array(
            (0..count)
                .map(|index| {
                    serde_json::json!({
                        "id": format!("message-{index}"),
                        "role": "agent",
                        "text": "Done",
                        "status": "sent",
                        "timestamp": "2026-09-20T19:30:00Z"
                    })
                })
                .collect(),
        );
    }
    serde_json::to_vec(&value).unwrap()
}

fn expected(fixture: &Value, entry: &Value) -> ExpectedResponse {
    let binding = serde_json::from_value::<AgentBinding>(fixture["binding"].clone()).unwrap();
    let kind = entry
        .get("expectedKind")
        .and_then(Value::as_str)
        .map(|value| {
            serde_json::from_value::<AgentResponseKind>(Value::String(value.into())).unwrap()
        });
    ExpectedResponse {
        invocation_id: fixture["invocationId"].as_str().unwrap().into(),
        binding,
        kind,
    }
}

#[test]
fn shared_v1_request_fixtures_have_declared_acceptance_and_canonical_bytes() {
    let fixture = fixture();
    for entry in fixture["cases"].as_array().unwrap() {
        if entry["target"] != "request" {
            continue;
        }
        let decoded = decode_request(&materialize(entry));
        assert_eq!(
            decoded.is_ok(),
            entry["accept"].as_bool().unwrap(),
            "{}",
            entry["id"]
        );
        if let Ok(request) = decoded {
            let canonical = String::from_utf8(canonical_request(&request).unwrap()).unwrap();
            if let Some(expected) = entry.get("canonical").and_then(Value::as_str) {
                assert_eq!(canonical, expected, "{}", entry["id"]);
            }
        }
    }
}

#[test]
fn shared_v1_response_fixtures_enforce_bounds_shape_correlation_and_binding() {
    let fixture = fixture();
    for entry in fixture["cases"].as_array().unwrap() {
        if entry["target"] != "response" {
            continue;
        }
        let decoded = decode_response(&materialize(entry), &expected(&fixture, entry));
        assert_eq!(
            decoded.is_ok(),
            entry["accept"].as_bool().unwrap(),
            "{}",
            entry["id"]
        );
    }
}

#[test]
fn rejects_frames_beyond_the_request_and_response_byte_limits() {
    let mut request = br#"{"version":1,"kind":"agent_capabilities"}"#.to_vec();
    request.resize(MAX_REQUEST_FRAME_BYTES + 1, b' ');
    assert_eq!(
        decode_request(&request),
        Err(ValidationError::FrameTooLarge)
    );

    let fixture = fixture();
    let entry = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "valid-list-with-unicode")
        .unwrap();
    let mut response = materialize(entry);
    response.resize(MAX_RESPONSE_FRAME_BYTES + 1, b' ');
    assert_eq!(
        decode_response(&response, &expected(&fixture, entry)),
        Err(ValidationError::FrameTooLarge)
    );
}

#[test]
fn revision_comparison_is_numeric_not_lexical() {
    assert_eq!(compare_revisions("2", "10"), Ok(Ordering::Less));
    assert_eq!(compare_revisions("10", "2"), Ok(Ordering::Greater));
    assert_eq!(
        compare_revisions("18446744073709551615", "18446744073709551615"),
        Ok(Ordering::Equal)
    );
    assert_eq!(
        compare_revisions("01", "1"),
        Err(ValidationError::InvalidField("revision"))
    );
}

fn grant_resource_fixture(resource: &str) -> (Value, ExpectedResponse) {
    let fixture = fixture();
    let entry = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "valid-capabilities")
        .unwrap();
    let mut value = entry["value"].clone();
    value["capabilities"]["grantOffers"][0]["actions"][0]["resource"] = resource.into();
    (value, expected(&fixture, entry))
}

#[test]
fn grant_resource_preserves_real_bound_alias_and_exact_4096_byte_limit() {
    let alias = format!(
        "tilo-ui-bound:v1:00000000-0000-0000-0000-000000000001:00000000-0000-0000-0000-000000000002:{}",
        "776f72642d70726f636573736f722e646f63756d656e74"
    );
    assert!(alias.len() > 128);
    for resource in [alias, "a".repeat(4096)] {
        let (value, expected) = grant_resource_fixture(&resource);
        let bytes = serde_json::to_vec(&value).unwrap();
        let decoded = decode_response(&bytes, &expected)
            .expect("component-qualified resource must retain its complete identity");
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            value,
            "wire layout or resource bytes changed"
        );
    }
    let (value, expected) = grant_resource_fixture(&"a".repeat(4097));
    assert_eq!(
        decode_response(&serde_json::to_vec(&value).unwrap(), &expected),
        Err(ValidationError::InvalidField("grantAction.resource"))
    );
}

#[test]
fn grant_resource_rejects_empty_non_ascii_trim_and_control_bytes() {
    for resource in [
        "", "é", " padded", "padded ", "a\tb", "a\nb", "a\0b", "a\u{7f}b",
    ] {
        let (value, expected) = grant_resource_fixture(resource);
        assert_eq!(
            decode_response(&serde_json::to_vec(&value).unwrap(), &expected),
            Err(ValidationError::InvalidField("grantAction.resource")),
            "accepted {resource:?}"
        );
    }
}

#[test]
fn grant_resource_expansion_keeps_other_ids_action_count_and_total_frame_caps() {
    for (field, error) in [
        ("appId", "grantAction.appId"),
        ("recordId", "grantAction.recordId"),
        ("action", "grantAction.action"),
    ] {
        let (mut value, expected) = grant_resource_fixture("document");
        value["capabilities"]["grantOffers"][0]["actions"][0][field] = "a".repeat(128).into();
        assert!(decode_response(&serde_json::to_vec(&value).unwrap(), &expected).is_ok());
        value["capabilities"]["grantOffers"][0]["actions"][0][field] = "a".repeat(129).into();
        assert_eq!(
            decode_response(&serde_json::to_vec(&value).unwrap(), &expected),
            Err(ValidationError::InvalidField(error))
        );
    }
    let (mut value, expected) = grant_resource_fixture(&"a".repeat(4096));
    let action = value["capabilities"]["grantOffers"][0]["actions"][0].clone();
    value["capabilities"]["grantOffers"][0]["actions"] = Value::Array(
        (0..64)
            .map(|i| {
                let mut action = action.clone();
                action["action"] = format!("action-{i}").into();
                action
            })
            .collect(),
    );
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() < MAX_RESPONSE_FRAME_BYTES);
    assert!(decode_response(&bytes, &expected).is_ok());
    let mut too_many = value.clone();
    too_many["capabilities"]["grantOffers"][0]["actions"]
        .as_array_mut()
        .unwrap()
        .push(action);
    assert_eq!(
        decode_response(&serde_json::to_vec(&too_many).unwrap(), &expected),
        Err(ValidationError::InvalidField("grantOffer.actions"))
    );
    let mut second = value["capabilities"]["grantOffers"][0].clone();
    second["id"] = "second-offer".into();
    value["capabilities"]["grantOffers"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() > MAX_RESPONSE_FRAME_BYTES);
    assert_eq!(
        decode_response(&bytes, &expected),
        Err(ValidationError::FrameTooLarge)
    );
}
