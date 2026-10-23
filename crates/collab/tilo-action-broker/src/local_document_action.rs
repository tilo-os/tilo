//! Typed reports of speculative local document actions, not execution proofs.
//!
//! Admission does not supply `DocumentAuthorization` or permit a signed document
//! change. The final owner must still refuse publication without its own evidence.
use serde::{Deserialize, Serialize};

pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_HASHES: usize = 256;

/// The authenticated channel verifies the original actor, app and record custody.
/// Encoded input, context, hashes and grouping are the application's report,
/// not verified execution. Context never supplies the authenticated actor.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Original authenticated channel provenance, compared with live custody at admission.
    #[serde(default)]
    pub custody: Option<runtime_collab::committed_document::ConnectionSnapshot>,
    pub version: u8,
    pub invocation: uuid::Uuid,
    /// Random identity of this capture, before content binding. Keeps identical
    /// reads/no-ops distinct without an unbounded host invocation ledger.
    pub capture_id: uuid::Uuid,
    pub revision: String,
    pub resource: String,
    pub action: String,
    pub argument: Option<String>,
    /// Exact canonical executor input, privately committed without re-encoding.
    /// The bounded host retry cache retains a digest, never its plaintext.
    pub encoded: tilo_actions::EncodedAction,
    /// Exact original context, including parent resource and source invocation.
    /// This is correlation only, not caller authority or a delivery instruction.
    pub context: Option<tilo_actions::ActionContext>,
    pub original_heads: Vec<[u8; 32]>,
    pub mutation_hashes: Vec<[u8; 32]>,
    pub group: Option<tilo_actions::ActionGroup>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalDocumentActionRequest")
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

impl Request {
    /// Content-address the complete immutable capture, except the address itself.
    /// Callers generate `capture_id` once and preserve it on every retry.
    ///
    /// # Errors
    ///
    /// When the capture cannot be encoded or digested.
    pub fn bound_invocation(&self) -> Result<uuid::Uuid, String> {
        let mut value = serde_json::to_value(self).map_err(|_| "local report encoding failed")?;
        value
            .as_object_mut()
            .ok_or("invalid local report")?
            .remove("invocation");
        let digest = crate::collab_broker_capnp::admission_request_digest(&value);
        let mut hash = blake3::Hasher::new_derive_key("tilo local document capture identity v1");
        hash.update(&digest);
        let bytes = hash
            .finalize()
            .as_bytes()
            .first_chunk::<16>()
            .copied()
            .ok_or("invalid local report digest")?;
        Ok(uuid::Uuid::from_bytes(bytes))
    }

    /// Resolve the existing action argument transport into its declared types.
    /// The resulting object, not merely an opaque transport string, is committed.
    pub(crate) fn typed_arguments(
        &self,
        params: &[tilo_actions::ActionParam],
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let supplied = match self.argument.as_deref() {
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
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("local document report exceeds byte budget".into());
        }
        let value: Self =
            serde_json::from_slice(bytes).map_err(|_| "invalid local document action report")?;
        if value.version != 1 || value.invocation.is_nil() || value.capture_id.is_nil() {
            return Err("invalid local document report version or invocation".into());
        }
        for field in [&value.revision, &value.resource, &value.action] {
            if field.is_empty() || field.len() > 4096 || field.chars().any(char::is_control) {
                return Err("invalid local document report address".into());
            }
        }
        for hashes in [&value.original_heads, &value.mutation_hashes] {
            if hashes.len() > MAX_HASHES
                || hashes.windows(2).any(|pair| pair.first() >= pair.last())
            {
                return Err("local document report hashes must be bounded sorted sets".into());
            }
        }
        if value.encoded.type_id.is_empty()
            || value.encoded.type_id.len() > 4096
            || value.encoded.type_id.chars().any(char::is_control)
            || serde_json::from_slice::<serde_json::Value>(&value.encoded.payload).is_err()
        {
            return Err("invalid encoded local action report".into());
        }
        // An empty mutation set is a read/no-op claim, never execution evidence.
        // The broker has no application decoder: published parameters are checked
        // at admission, but correspondence to this encoded input remains a claim.
        if let Some(group) = &value.group {
            for field in [&group.id, &group.label, &group.kind] {
                if field.is_empty() || field.len() > 4096 || field.chars().any(char::is_control) {
                    return Err("invalid local document report group".into());
                }
            }
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> serde_json::Value {
        serde_json::json!({
            "version": 1, "invocation": uuid::Uuid::new_v4(),
            "capture_id": uuid::Uuid::new_v4(),
            "revision": "revision", "resource": "document", "action": "edit",
            "argument": "text", "original_heads": vec![[0u8; 32]],
            "mutation_hashes": vec![[1u8; 32]], "group": null,
            "encoded": {"type_id": "test.edit", "payload": b"{}".to_vec()},
            "context": null,
            "custody": null
        })
    }

    #[test]
    fn capture_identity_binds_all_report_fields_and_preserves_distinct_noops() {
        let request = Request::decode(&serde_json::to_vec(&report()).unwrap()).unwrap();
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
                    serde_json::json!({"type_id": "test.edit", "payload": b"null".to_vec()})
                }
                "custody" => serde_json::json!({
                    "root": vec![1; 32], "device": vec![2; 32], "agent": null,
                    "app_id": "app", "room": "room", "document": "document"
                }),
                "context" => serde_json::json!(tilo_actions::ActionContext::default()),
                "group" => serde_json::json!({"id": "g", "label": "g", "kind": "edit"}),
                _ => serde_json::json!("different"),
            };
            let changed: Request = serde_json::from_value(changed).unwrap();
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

    #[test]
    fn sdk_report_and_broker_request_bind_the_same_complete_capture() {
        let original = report();
        for field in original.as_object().unwrap().keys() {
            let mut changed = original.clone();
            changed[field] = match field.as_str() {
                "capture_id" | "invocation" => serde_json::json!(uuid::Uuid::new_v4()),
                "version" => serde_json::json!(1),
                "original_heads" | "mutation_hashes" => serde_json::json!([]),
                "encoded" => serde_json::json!({
                    "payload": b"{ \"z\": 1, \"a\": 2 }".to_vec(),
                    "type_id": "test.other"
                }),
                "context" => serde_json::json!(
                    tilo_actions::ActionContext::default()
                        .with_purpose("private purpose")
                        .with_group("gesture", "Edit", "edit")
                ),
                "custody" => serde_json::json!({
                    "root": vec![1; 32], "device": vec![2; 32], "agent": null,
                    "app_id": "app", "room": "room", "document": "document"
                }),
                "group" => serde_json::json!({"kind": "edit", "label": "Edit", "id": "g"}),
                _ => serde_json::json!("different"),
            };
            let mut sdk: runtime_collab::LocalDocumentActionReport =
                serde_json::from_value(changed).unwrap();
            sdk.invocation = sdk.bound_invocation().unwrap();
            let bytes = sdk.encode().unwrap();
            let broker = Request::decode(&bytes).unwrap();
            assert_eq!(broker.capture_id, sdk.capture_id, "{field}");
            assert_eq!(
                broker.bound_invocation().unwrap(),
                sdk.invocation,
                "{field}"
            );
            assert_eq!(serde_json::to_vec(&broker).unwrap(), bytes, "{field}");
            let roundtrip: runtime_collab::LocalDocumentActionReport =
                serde_json::from_slice(&serde_json::to_vec(&broker).unwrap()).unwrap();
            assert_eq!(roundtrip.bound_invocation().unwrap(), sdk.invocation);
            assert_eq!(roundtrip.encode().unwrap(), bytes);
        }
    }

    #[test]
    fn typed_report_refuses_undeclared_and_untyped_arguments() {
        let mut request = Request::decode(&serde_json::to_vec(&report()).unwrap()).unwrap();
        assert!(request.typed_arguments(&[]).is_err());
        request.argument = Some(format!(
            "{}{{\"unknown\": 1}}",
            tilo_actions::ARGUMENTS_JSON_PREFIX
        ));
        assert!(request.typed_arguments(&[]).is_err());
        request.argument = None;
        assert!(request.typed_arguments(&[]).unwrap().is_empty());
    }

    #[test]
    fn read_or_noop_retains_exact_encoded_input_and_original_context() {
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
        value["encoded"]["payload"] = serde_json::json!(b"{ \"value\": 1 }".to_vec());
        let decoded = Request::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(decoded.mutation_hashes.is_empty());
        assert_eq!(decoded.context, Some(context));
        assert_eq!(decoded.encoded.payload, b"{ \"value\": 1 }");
    }

    #[test]
    fn report_refuses_missing_or_malformed_executor_input() {
        let mut value = report();
        value.as_object_mut().unwrap().remove("encoded");
        assert!(Request::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        value = report();
        value["encoded"]["payload"] = serde_json::json!(b"arbitrary bytes".to_vec());
        assert!(Request::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        value = report();
        value["encoded"]["type_id"] = serde_json::json!("");
        assert!(Request::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn closed_report_rejects_actor_and_noncanonical_hashes() {
        let mut value = report();
        assert!(Request::decode(&serde_json::to_vec(&value).unwrap()).is_ok());
        value["actor"] = serde_json::json!("forged");
        assert!(Request::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        value.as_object_mut().unwrap().remove("actor");
        value["mutation_hashes"] = serde_json::json!(vec![[1u8; 32], [1u8; 32]]);
        assert!(Request::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(Request::decode(&vec![b' '; MAX_REQUEST_BYTES + 1]).is_err());
    }
}
