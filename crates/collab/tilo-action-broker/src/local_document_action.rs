//! Typed reports of speculative local document actions, not execution proofs.
//!
//! Admission does not supply `DocumentAuthorization` or permit a signed document
//! change. The final owner must still refuse publication without its own evidence.
use serde::{Deserialize, Serialize};

pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_HASHES: usize = 256;

/// The authenticated channel supplies the original actor, app and record.
/// Encoded input, context, hashes and grouping are the application's report,
/// not verified execution. Context never supplies the authenticated actor.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u8,
    pub invocation: uuid::Uuid,
    pub revision: String,
    pub resource: String,
    pub action: String,
    pub argument: Option<String>,
    /// Exact canonical executor input, privately committed without re-encoding.
    /// The journal retains a request digest, never its plaintext.
    pub encoded: tilo_actions::EncodedAction,
    /// Exact original context, including parent resource and source invocation.
    /// This is correlation only, not caller authority or a delivery instruction.
    pub context: Option<tilo_actions::ActionContext>,
    pub original_heads: Vec<[u8; 32]>,
    pub mutation_hashes: Vec<[u8; 32]>,
    pub group: Option<tilo_actions::ActionGroup>,
}

impl Request {
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
                } else if params.len() == 1 {
                    let mut values = serde_json::Map::new();
                    values.insert(
                        params[0].name.to_string(),
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
        if value.version != 1 || value.invocation.is_nil() {
            return Err("invalid local document report version or invocation".into());
        }
        for field in [&value.revision, &value.resource, &value.action] {
            if field.is_empty() || field.len() > 4096 || field.chars().any(char::is_control) {
                return Err("invalid local document report address".into());
            }
        }
        for hashes in [&value.original_heads, &value.mutation_hashes] {
            if hashes.len() > MAX_HASHES || hashes.windows(2).any(|pair| pair[0] >= pair[1]) {
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
            "revision": "revision", "resource": "document", "action": "edit",
            "argument": "text", "original_heads": vec![[0u8; 32]],
            "mutation_hashes": vec![[1u8; 32]], "group": null,
            "encoded": {"type_id": "test.edit", "payload": b"{}".to_vec()},
            "context": null
        })
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
