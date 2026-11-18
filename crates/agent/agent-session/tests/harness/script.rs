//! A task written out as the calls its turn makes.
//!
//! [`task`] turns calls into the task text [`tilo_agent_session::ModelChoice::Scripted`]
//! plays: one call a round, then the tools' own words as the answer.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these fixtures"
)]
#![allow(
    clippy::needless_pass_by_value,
    reason = "a call is written as the model reads it, its arguments whole"
)]

/// One tool call, as the scripted model makes it.
pub fn call(name: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"name": name, "input": input})
}

/// A `tilo_do` call: do the action `path` names with `args`.
pub fn doing(path: &str, args: serde_json::Value) -> serde_json::Value {
    call("tilo_do", serde_json::json!({"path": path, "args": args}))
}

/// [`doing`] with the arguments an action's own names made.
pub fn doing_args(
    path: &str,
    args: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    doing(path, serde_json::Value::Object(args))
}

/// The task asking for `calls`, in order.
pub fn task(calls: &[serde_json::Value]) -> String {
    serde_json::Value::Array(calls.to_vec()).to_string()
}
