//! The line protocol: one JSON object per line in, one per line out, with
//! the same `op` names and response envelopes the compositor's
//! `TILO_TEST_CONTROL` socket uses (`{"answer":…}`, `{"state":…}`,
//! `{"err":{"code","message"}}`), so `tilo-control` and the Python example
//! client need no second dialect.

use serde_json::{Map, Value, json};

/// Longest request line accepted. Descriptor-sized arguments fit easily; a
/// peer that streams more is refused rather than buffered without bound.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// One parsed request. Deliberately not `Debug`: invocation arguments can
/// carry secrets (passphrases), and a derived `Debug` would print them.
pub enum Request {
    Enumerate {
        query: String,
    },
    Invoke {
        provider: String,
        id: String,
        args: Map<String, Value>,
    },
    StateDump,
    /// A well-formed op this surface does not serve (input injection,
    /// snapshots, capability administration). Answered `not_implemented`
    /// rather than `parse_error`, because the caller spoke the protocol
    /// correctly and asked the wrong kind of endpoint.
    Unsupported {
        op: String,
    },
}

/// Why a line is not a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError(pub String);

/// Parse one request line.
///
/// # Errors
///
/// Returns [`WireError`] for malformed JSON, a non-object, a missing `op`,
/// or an ill-typed field of a served op.
pub fn parse(line: &str) -> Result<Request, WireError> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| WireError(format!("invalid JSON: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| WireError("a request must be a JSON object".into()))?;
    let op = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| WireError("a request needs a string `op`".into()))?;
    match op {
        "actions_enumerate" => Ok(Request::Enumerate {
            query: optional_string(object, "query")?.unwrap_or_default(),
        }),
        "actions_invoke" => Ok(Request::Invoke {
            provider: required_string(object, "provider")?,
            id: required_string(object, "id")?,
            args: match object.get("args") {
                None | Some(Value::Null) => Map::new(),
                Some(Value::Object(args)) => args.clone(),
                Some(_) => return Err(WireError("`args` must be a JSON object".into())),
            },
        }),
        "state_dump" => Ok(Request::StateDump),
        other => Ok(Request::Unsupported { op: other.into() }),
    }
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, WireError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| WireError(format!("`{key}` must be a non-empty string")))
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>, WireError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(WireError(format!("`{key}` must be a string"))),
    }
}

/// `{"answer": value}` — enumeration and invocation results.
pub fn answer(value: &Value) -> String {
    json!({ "answer": value }).to_string()
}

/// `{"state": value}` — what `tilo-control state-dump` prints.
pub fn state(value: &Value) -> String {
    json!({ "state": value }).to_string()
}

/// `{"err": {"code", "message"}}`.
pub fn error(code: &str, message: &str) -> String {
    json!({ "err": { "code": code, "message": message } }).to_string()
}

/// Correlation field a long-lived client may attach to a request.
pub const SEQ: &str = "seq";

/// The optional [`SEQ`] a caller attached to its request (a string or a
/// number). `tilo-control` never sends one; a long-lived client on the
/// virtio channel does, because a request written while the guest was not
/// yet listening can be answered after the client gave up on it, and the
/// sequence value is how the next exchange recognises and skips that stale
/// answer. (`id` is taken: it is the action address of `actions_invoke`.)
pub fn request_seq(line: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(line).ok()?;
    value
        .get(SEQ)
        .filter(|seq| seq.is_string() || seq.is_number())
        .cloned()
}

/// `reply` with the request's [`SEQ`] added, when it had one.
pub fn with_seq(reply: String, seq: Option<Value>) -> String {
    let Some(seq) = seq else {
        return reply;
    };
    match serde_json::from_str::<Value>(&reply) {
        Ok(Value::Object(mut object)) => {
            object.insert(SEQ.into(), seq);
            Value::Object(object).to_string()
        }
        _ => reply,
    }
}

/// The answer for a served-elsewhere op.
pub fn unsupported(op: &str) -> String {
    error(
        "not_implemented",
        &format!(
            "`{op}` is not served here: a console app answers only actions_enumerate, actions_invoke and state_dump (no synthetic input, no snapshots)"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_requests_parse_or_are_refused_and_envelopes_match_the_compositor() {
        // Phase `served_ops_parse_and_foreign_ops_are_unsupported_not_malformed`.
        {
            assert!(matches!(
                parse(r#"{"op":"actions_enumerate"}"#),
                Ok(Request::Enumerate { query }) if query.is_empty()
            ));
            assert!(matches!(
                parse(r#"{"op":"actions_enumerate","query":"disk"}"#),
                Ok(Request::Enumerate { query }) if query == "disk"
            ));
            let Ok(Request::Invoke { provider, id, args }) = parse(
                r#"{"op":"actions_invoke","provider":"installer","id":"next@installer","args":{"a":1}}"#,
            ) else {
                panic!("invoke did not parse");
            };
            assert_eq!(
                (provider.as_str(), id.as_str()),
                ("installer", "next@installer")
            );
            assert_eq!(args["a"], 1);
            assert!(matches!(
                parse(r#"{"op":"actions_invoke","provider":"p","id":"i"}"#),
                Ok(Request::Invoke { args, .. }) if args.is_empty()
            ));
            assert!(matches!(
                parse(r#"{"op":"state_dump"}"#),
                Ok(Request::StateDump)
            ));
            assert!(matches!(
                parse(r#"{"op":"click_named","name":"x"}"#),
                Ok(Request::Unsupported { op }) if op == "click_named"
            ));
        }
        // Phase `malformed_requests_are_refused_with_a_reason`.
        {
            for line in [
                "not json",
                "[1]",
                r#"{"name":"x"}"#,
                r#"{"op":3}"#,
                r#"{"op":"actions_invoke","id":"x"}"#,
                r#"{"op":"actions_invoke","provider":"p","id":""}"#,
                r#"{"op":"actions_invoke","provider":"p","id":"i","args":[]}"#,
                r#"{"op":"actions_enumerate","query":5}"#,
            ] {
                assert!(parse(line).is_err(), "accepted {line}");
            }
        }
        // Phase `a_request_seq_is_echoed_and_its_absence_changes_nothing`.
        {
            assert_eq!(
                request_seq(r#"{"op":"state_dump","seq":7}"#),
                Some(json!(7))
            );
            assert_eq!(
                request_seq(r#"{"op":"actions_invoke","provider":"p","id":"a@r","seq":"s"}"#),
                Some(json!("s"))
            );
            assert_eq!(request_seq(r#"{"op":"state_dump","seq":[1]}"#), None);
            assert_eq!(request_seq(r#"{"op":"state_dump","id":"x"}"#), None);
            assert_eq!(with_seq(state(&json!({})), None), r#"{"state":{}}"#);
            let tagged: Value =
                serde_json::from_str(&with_seq(state(&json!({})), Some(json!(7)))).unwrap();
            assert_eq!(tagged["seq"], 7);
            assert!(tagged.get("state").is_some());
        }
        // Phase `envelopes_match_the_compositor_socket`.
        {
            assert_eq!(answer(&json!({"n": 1})), r#"{"answer":{"n":1}}"#);
            assert_eq!(state(&json!({})), r#"{"state":{}}"#);
            let err: Value = serde_json::from_str(&error("unknown_action", "no")).unwrap();
            assert_eq!(err["err"]["code"], "unknown_action");
            let unsupported: Value = serde_json::from_str(&unsupported("key")).unwrap();
            assert_eq!(unsupported["err"]["code"], "not_implemented");
        }
    }
}
