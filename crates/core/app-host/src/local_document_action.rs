//! Typed reports of speculative local document actions, not execution proofs.
//!
//! The wire type, its content-bound identity and its shared structural
//! validation live in [`runtime_collab::LocalDocumentActionReport`]. The broker
//! remains the authority: this module enforces its wire byte budget before
//! deserialization, holds its bounds and checks published action arguments.
//!
//! Admission does not supply `DocumentAuthorization` or permit a signed document
//! change. The final owner must still refuse publication without its own evidence.

use runtime_collab::LocalDocumentActionReport;

pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_FIELD_BYTES: usize = 4096;
const MAX_HASHES: usize = 256;

const _: () = assert!(LocalDocumentActionReport::MAX_BYTES <= MAX_REQUEST_BYTES);
const _: () = assert!(LocalDocumentActionReport::MAX_FIELD_BYTES <= MAX_FIELD_BYTES);
const _: () = assert!(LocalDocumentActionReport::MAX_HASHES <= MAX_HASHES);

/// Resolve the existing action argument transport into its declared types.
/// The resulting object, not merely an opaque transport string, is committed.
pub(crate) fn typed_arguments(
    report: &LocalDocumentActionReport,
    params: &[tilo_actions::ActionParam],
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let supplied = match report.argument.as_deref() {
        None => serde_json::Map::new(),
        Some(argument) => {
            if let Some(json) = argument.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX) {
                serde_json::from_str(json).map_err(|_| "invalid typed action arguments")?
            } else if let [param] = params {
                let mut values = serde_json::Map::new();
                values.insert(
                    param.name.to_string(),
                    serde_json::Value::String(argument.into()),
                );
                values
            } else {
                return Err("local document report requires typed action arguments".into());
            }
        }
    };
    tilo_actions::ActionArgs::bind_json(params, &supplied)
        .map_err(|error| format!("invalid local document action arguments: {error}"))?;
    Ok(supplied)
}

/// Parse a closed, bounded intent; head/hash sets must be canonical, so
/// reordering or duplicate entries cannot create ambiguous commitments.
pub fn decode(bytes: &[u8]) -> Result<LocalDocumentActionReport, String> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("local document report exceeds byte budget".into());
    }
    let value: LocalDocumentActionReport =
        serde_json::from_slice(bytes).map_err(|_| "invalid local document action report")?;
    value.validate()?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_INVOCATION: &str = "8eff53bb-6445-8d76-4cf7-fcab24a914d8";
    const FIXTURE_FULL_DIGEST: &str =
        "1d11a69502bd02defa082f5d0f497970369119472de96e2a9eb43c3768370cbe";
    const FIXTURE_CAPTURE_DIGEST: &str =
        "ccb81077a761cb2108f2dfa1882847e27f781ebc10bbc0ac3b4c75ffbca20956";
    const FIXTURE_BYTES: &[u8] = br#"{"ownership":{"root":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"device":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"agent":"agent-1","app_id":"so.tilo.notes","workspace":"ws-1","document":"doc-1"},"version":1,"invocation":"8eff53bb-6445-8d76-4cf7-fcab24a914d8","capture_id":"01020304-0506-0708-090a-0b0c0d0e0f10","revision":"rev-1","resource":"document:main","action":"edit","argument":"\u001etilo-args:{\"b\":2,\"a\":\"hello\"}","encoded":{"type_id":"notes.edit","payload":"{ \"z\": 1, \"a\": 2 }"},"context":{"resource":{"id":"parent-doc","kind":"document","label":"Parent"},"purpose":"edit body","group":{"id":"grp-1","label":"Edit","kind":"edit"},"source_invocation":"100f0e0d-0c0b-0a09-0807-060504030201"},"original_heads":[[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2]],"mutation_hashes":[[3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3],[4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4]],"group":{"id":"grp-1","label":"Edit","kind":"edit"}}"#;
    const MINIMAL_FIXTURE_INVOCATION: &str = "5284f72b-32ef-2009-3a74-3688335c4ba2";
    const MINIMAL_FIXTURE_BYTES: &[u8] = br#"{"ownership":null,"version":1,"invocation":"5284f72b-32ef-2009-3a74-3688335c4ba2","capture_id":"10203040-5060-7080-90a0-b0c0d0e0f001","revision":"rev-0","resource":"doc","action":"read","argument":null,"encoded":{"type_id":"notes.read","payload":"{}"},"context":null,"original_heads":[],"mutation_hashes":[],"group":null}"#;

    fn report() -> serde_json::Value {
        serde_json::json!({
            "version": 1, "invocation": uuid::Uuid::new_v4(),
            "capture_id": uuid::Uuid::new_v4(),
            "revision": "revision", "resource": "document", "action": "edit",
            "argument": "text", "original_heads": vec![[0u8; 32]],
            "mutation_hashes": vec![[1u8; 32]], "group": null,
            "encoded": {"type_id": "test.edit", "payload": "{}"},
            "context": null,
            "ownership": null
        })
    }

    tilo_test_env::scenario! {
        fn a_local_document_report_binds_its_whole_capture_and_refuses_the_rest;

        phase fixed_wire_fixture_matches_main_bytes_identifier_and_digest;
        {
            let decoded = decode(FIXTURE_BYTES).unwrap();
            let expected: uuid::Uuid = FIXTURE_INVOCATION.parse().unwrap();
            assert_eq!(decoded.invocation, expected);
            assert_eq!(decoded.bound_invocation().unwrap(), expected);
            assert_eq!(decoded.encode().unwrap(), FIXTURE_BYTES);

            let full = serde_json::to_value(&decoded).unwrap();
            assert_eq!(
                hex::encode(crate::collab_app_broker::admission_request_digest(
                    &full
                )),
                FIXTURE_FULL_DIGEST
            );
            let mut capture = full;
            capture.as_object_mut().unwrap().remove("invocation");
            assert_eq!(
                hex::encode(crate::collab_app_broker::admission_request_digest(
                    &capture
                )),
                FIXTURE_CAPTURE_DIGEST
            );

            let minimal = decode(MINIMAL_FIXTURE_BYTES).unwrap();
            let expected_minimal: uuid::Uuid = MINIMAL_FIXTURE_INVOCATION.parse().unwrap();
            assert_eq!(minimal.invocation, expected_minimal);
            assert_eq!(minimal.bound_invocation().unwrap(), expected_minimal);
            assert_eq!(minimal.encode().unwrap(), MINIMAL_FIXTURE_BYTES);
        }

        phase capture_identity_binds_all_report_fields_and_preserves_distinct_noops;
        {
            let request = decode(&serde_json::to_vec(&report()).unwrap()).unwrap();
            let expected = request.bound_invocation().unwrap();
            let original = serde_json::to_value(&request).unwrap();
            for field in original.as_object().unwrap().keys() {
                if field == "invocation" {
                    continue;
                }
                let mut changed = original.clone();
                changed[field] = match field.as_str() {
                    "capture_id" => serde_json::json!(uuid::Uuid::new_v4()),
                    "version" => serde_json::json!(2),
                    "original_heads" | "mutation_hashes" => serde_json::json!([]),
                    "encoded" => {
                        serde_json::json!({"type_id": "test.edit", "payload": "null"})
                    }
                    "ownership" => serde_json::json!({
                        "root": vec![1; 32], "device": vec![2; 32], "agent": null,
                        "app_id": "app", "workspace": "workspace", "document": "document"
                    }),
                    "context" => serde_json::json!(tilo_actions::ActionContext::default()),
                    "group" => serde_json::json!({"id": "g", "label": "g", "kind": "edit"}),
                    _ => serde_json::json!("different"),
                };
                let changed: LocalDocumentActionReport = serde_json::from_value(changed).unwrap();
                assert_ne!(changed.bound_invocation().unwrap(), expected, "{field}");
            }
            let mut rebound = request;
            rebound.invocation = expected;
            assert_eq!(rebound.bound_invocation().unwrap(), expected);
            rebound.mutation_hashes.clear();
            let noop = rebound.bound_invocation().unwrap();
            rebound.capture_id = uuid::Uuid::new_v4();
            assert_ne!(rebound.bound_invocation().unwrap(), noop);
        }

        phase report_roundtrips_through_encode_and_decode_and_binds_every_field;
        {
            let original = report();
            for field in original.as_object().unwrap().keys() {
                let mut changed = original.clone();
                changed[field] = match field.as_str() {
                    "capture_id" | "invocation" => serde_json::json!(uuid::Uuid::new_v4()),
                    "version" => serde_json::json!(1),
                    "original_heads" | "mutation_hashes" => serde_json::json!([]),
                    "encoded" => serde_json::json!({
                        "payload": "{ \"z\": 1, \"a\": 2 }",
                        "type_id": "test.other"
                    }),
                    "context" => serde_json::json!(
                        tilo_actions::ActionContext::default()
                            .with_purpose("private purpose")
                            .with_group("gesture", "Edit", "edit")
                    ),
                    "ownership" => serde_json::json!({
                        "root": vec![1; 32], "device": vec![2; 32], "agent": null,
                        "app_id": "app", "workspace": "workspace", "document": "document"
                    }),
                    "group" => serde_json::json!({"kind": "edit", "label": "Edit", "id": "g"}),
                    _ => serde_json::json!("different"),
                };
                let mut sdk: LocalDocumentActionReport = serde_json::from_value(changed).unwrap();
                sdk.invocation = sdk.bound_invocation().unwrap();
                let bytes = sdk.encode().unwrap();
                let broker = decode(&bytes).unwrap();
                assert_eq!(broker.capture_id, sdk.capture_id, "{field}");
                assert_eq!(
                    broker.bound_invocation().unwrap(),
                    sdk.invocation,
                    "{field}"
                );
                assert_eq!(serde_json::to_vec(&broker).unwrap(), bytes, "{field}");
                assert_eq!(broker.encode().unwrap(), bytes, "{field}");
            }
        }

        phase typed_report_refuses_undeclared_and_untyped_arguments;
        {
            let mut request = decode(&serde_json::to_vec(&report()).unwrap()).unwrap();
            assert!(typed_arguments(&request, &[]).is_err());
            request.argument = Some(format!(
                "{}{{\"unknown\": 1}}",
                tilo_actions::ARGUMENTS_JSON_PREFIX
            ));
            assert!(typed_arguments(&request, &[]).is_err());
            request.argument = None;
            assert!(typed_arguments(&request, &[]).unwrap().is_empty());
        }

        phase read_or_noop_retains_exact_encoded_input_and_original_context;
        {
            let mut value = report();
            value["mutation_hashes"] = serde_json::json!([]);
            let source = tilo_actions::InvocationId(uuid::Uuid::new_v4());
            let context = tilo_actions::ActionContext {
                source_invocation: Some(source),
                resource: Some(tilo_actions::ActionResource {
                    id: "parent-document".into(),
                    kind: "document".into(),
                    label: "Private parent".into(),
                }),
                ..tilo_actions::ActionContext::default()
            };
            value["context"] = serde_json::to_value(&context).unwrap();
            // Formatting is part of the executor input; never normalize the bytes.
            value["encoded"]["payload"] = serde_json::json!("{ \"value\": 1 }");
            let decoded = decode(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(decoded.mutation_hashes.is_empty());
            assert_eq!(decoded.context, Some(context));
            assert_eq!(decoded.encoded.payload, b"{ \"value\": 1 }");
        }

        phase report_refuses_missing_or_malformed_executor_input;
        {
            let mut value = report();
            value.as_object_mut().unwrap().remove("encoded");
            assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
            value = report();
            value["encoded"]["payload"] = serde_json::json!("arbitrary bytes");
            assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
            value = report();
            value["encoded"]["type_id"] = serde_json::json!("");
            assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }

        phase closed_report_rejects_actor_and_noncanonical_hashes;
        {
            let mut value = report();
            assert!(decode(&serde_json::to_vec(&value).unwrap()).is_ok());
            value["actor"] = serde_json::json!("forged");
            assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
            value.as_object_mut().unwrap().remove("actor");
            value["mutation_hashes"] = serde_json::json!(vec![[1u8; 32], [1u8; 32]]);
            assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
            value = report();
            value["revision"] = serde_json::json!("a".repeat(MAX_FIELD_BYTES + 1));
            assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
            value = report();
            value["original_heads"] = serde_json::json!(
                (0..=MAX_HASHES)
                    .map(|i| {
                        let [hi, lo] = u16::try_from(i).unwrap().to_be_bytes();
                        [
                            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                            0, 0, 0, 0, 0, 0, hi, lo,
                        ]
                    })
                    .collect::<Vec<_>>()
            );
            assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
            assert!(decode(&vec![b' '; MAX_REQUEST_BYTES + 1]).is_err());
        }
    }
}
