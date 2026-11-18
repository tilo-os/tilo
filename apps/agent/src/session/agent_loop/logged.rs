//! What the agent's own log keeps of a tool call.
//!
//! The agent writes its log to stderr, which the launcher keeps: a file on the person's
//! device. A call's arguments are what the model wrote for its person: a sentence, a
//! document's text, where it errs a password. So no value of an argument stands in a line of
//! the log, at any level: a line names each argument and says how many bytes its value has.

use serde_json::Value;

/// A call's arguments as a line of the log names them: each by its name and the bytes of its
/// value as the call wrote it, `name:bytes` with a comma between, `-` for a call without any,
/// and `?` for a name that has not the form of one or an input that names nothing.
pub(super) fn arguments(input: &Value) -> String {
    let named = match input {
        Value::Null => return "-".to_owned(),
        Value::Object(named) if named.is_empty() => return "-".to_owned(),
        Value::Object(named) => named,
        unnamed => return format!("?:{}", unnamed.to_string().len()),
    };
    named
        .iter()
        .map(|(name, value)| {
            let plain = !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
            let name = if plain { name.as_str() } else { "?" };
            format!("{name}:{}", value.to_string().len())
        })
        .collect::<Vec<_>>()
        .join(",")
}
