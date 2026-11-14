//! What an agent tool answers typed: one answer for a call that ran, one
//! for a call that did not, and the rows a listing and a refusal carry.
use super::render::render_value;
use super::{ActionDescriptor, Reply, Value, json};
use crate::mcp::UNTRUSTED;
use std::fmt::Write as _;

/// The MCP result of an agent tool: its text alone, or the structured copy
/// the caller asked for (its typed answer) as JSON, which is the text then: a
/// second rendering would be read twice. A call that ran nothing more than
/// what is safe to run again (`retry_safe`) and was refused by a host busy
/// with the person's changes answers `rate_limited`, with the wait.
pub(super) fn text_result(
    result: Result<Reply, String>,
    structured: bool,
    retry_safe: bool,
) -> Value {
    match result {
        Ok(Reply {
            structured: Some(answer),
            failed,
            ..
        }) => json!({
            "content":[{"type":"text","text":answer.to_string()}],
            "structuredContent":answer,
            "isError":failed
        }),
        Ok(reply) if structured => {
            // A tool that answered no typed value: what it said is the value.
            let answer = json!({
                "status": if reply.failed {"failed"} else {"ok"},
                "text": reply.text
            });
            json!({
                "content":[{"type":"text","text":answer.to_string()}],
                "structuredContent":answer,
                "isError":reply.failed
            })
        }
        Ok(reply) => json!({"content":[{"type":"text","text":reply.text}],"isError":reply.failed}),
        Err(message) if structured => {
            let answer = refused(&message, retry_safe);
            json!({
                "content":[{"type":"text","text":answer.to_string()}],
                "structuredContent":answer,
                "isError":true
            })
        }
        Err(message) => json!({"content":[{"type":"text","text":message}],"isError":true}),
    }
}

/// The typed answer of a call that did not run: what it was, and what would
/// work instead.
fn refused(message: &str, retry_safe: bool) -> Value {
    if retry_safe && let Some(wait) = tilo_relay_protocol::refusal::busy_wait(message) {
        let wait = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
        return json!({"status":"failed","error":{
            "code": "rate_limited",
            "message": message,
            "retry_after_ms": wait,
            "next_steps": [format!("wait {wait} ms and retry the same call")],
        }});
    }
    json!({"status":"failed","error":{"code":"failed","message":message}})
}

/// The typed answer of one invocation's answer (`{invocation_id, update,
/// pending, hint, …}`): what the call did, as the text says it, typed.
pub(super) fn answer_of(value: &Value) -> Value {
    let state = value
        .pointer("/update/state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut answer = serde_json::Map::new();
    if let Some(id) = value.get("invocation_id").and_then(Value::as_str) {
        answer.insert("invocation_id".into(), id.into());
    }
    match state {
        "succeeded" => {
            answer.insert("status".into(), "ok".into());
            if let Some(output) = value.pointer("/update/output").and_then(Value::as_object) {
                match output.get("value") {
                    Some(result) if !result.is_null() => {
                        answer.insert("result".into(), result.clone());
                    }
                    _ => {}
                }
                if let Some(Value::Array(resources)) = output
                    .get("resources")
                    .filter(|resources| !resources.is_null())
                {
                    answer.insert("resources".into(), Value::Array(resources.clone()));
                }
            }
        }
        "failed" => {
            answer.insert("status".into(), "failed".into());
            let code = value
                .pointer("/update/error/code")
                .and_then(Value::as_str)
                .filter(|code| !code.trim().is_empty())
                .unwrap_or("failed");
            let message = value
                .pointer("/update/error/message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut error = json!({"code": code, "message": message});
            if let (Some(fields), Some(steps)) = (
                error.as_object_mut(),
                value
                    .get("hint")
                    .and_then(Value::as_str)
                    .filter(|hint| !hint.trim().is_empty()),
            ) {
                fields.insert("next_steps".into(), json!([steps]));
            }
            answer.insert("error".into(), error);
        }
        "cancelled" => {
            answer.insert("status".into(), "failed".into());
            let message = value
                .pointer("/update/message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            answer.insert(
                "error".into(),
                json!({"code":"cancelled","message":message}),
            );
        }
        _ => {
            // Still running: the invocation id follows it, and the same call
            // with the same id asks again where it went.
            answer.insert("status".into(), "pending".into());
        }
    }
    if let Some(edited) = value.get("edited_by_person") {
        answer.insert("edited_by_person".into(), edited.clone());
    }
    if let Some(untaken) = value.get(UNTRUSTED) {
        answer.insert(UNTRUSTED.into(), untaken.clone());
    }
    Value::Object(answer)
}

/// One action of a listing, typed: what it is called, what it takes, and
/// what running it may reach.
pub(super) fn action_row(action: &ActionDescriptor) -> Value {
    let mut row = json!({
        "id": action.address.action,
        "label": action.action_label,
        "params": action.params,
        "kind": action.contract.kind,
    });
    if let (Some(fields), Some(description)) = (
        row.as_object_mut(),
        action
            .description
            .as_deref()
            .filter(|text| !text.trim().is_empty()),
    ) {
        fields.insert("description".into(), json!(description.trim()));
    }
    row
}

pub(super) fn action_rows(actions: &[ActionDescriptor]) -> Vec<Value> {
    actions.iter().map(action_row).collect()
}

/// What an invocation's final update says, as an agent reads it.
pub(super) fn outcome(value: &Value) -> Reply {
    let state = value
        .pointer("/update/state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut text = match state {
        "succeeded" => {
            let mut text = String::from("ok");
            let output = value.pointer("/update/output");
            if let Some(result) = output
                .and_then(|output| output.get("value"))
                .map(render_value)
                .filter(|result| !result.is_empty())
            {
                text.push('\n');
                text.push_str(&result);
            }
            for resource in output
                .and_then(|output| output.get("resources"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .take(8)
            {
                let _ = write!(text, "\n-> {resource}");
            }
            text
        }
        "failed" => {
            let code = value
                .pointer("/update/error/code")
                .and_then(Value::as_str)
                .unwrap_or("failed");
            let message = value
                .pointer("/update/error/message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!("failed: {code}: {message}")
        }
        "cancelled" => "cancelled".to_owned(),
        "item" | "progress" => {
            let id = value
                .get("invocation_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut text = format!("pending {id}: still running");
            // A run in a shell says what it printed so far as it prints it.
            if let Some(tail) = value
                .pointer("/update/value/tail")
                .and_then(Value::as_str)
                .filter(|tail| !tail.trim().is_empty())
            {
                let total = value
                    .pointer("/update/value/total")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let _ = write!(
                    text,
                    "; its output so far ({total} bytes), newest last:\n{}",
                    tail.trim_end()
                );
            }
            let _ = write!(
                text,
                "\ntilo_status {{invocation_id}} says how it went; {{\"cancel\": true}} stops it"
            );
            text
        }
        _ => {
            let id = value
                .get("invocation_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!(
                "pending {id}: not finished yet; tilo_status {{invocation_id}} says how it went"
            )
        }
    };
    if let Some(adjusted) = value.get("adjusted").and_then(Value::as_str) {
        let _ = write!(text, "\n(adjusted: {adjusted})");
    }
    Reply {
        failed: state != "succeeded",
        text,
        structured: None,
    }
}
