//! What an agent tool answers typed: one answer for a call that ran, one
//! for a call that did not, and the rows a listing and a refusal carry.
use super::render::render_value;
use super::{ActionDescriptor, Failure, Reply, Value, json};
use crate::UNTRUSTED;
use std::fmt::Write as _;

/// The MCP result of an agent tool: its text alone, or the structured copy
/// the caller asked for (its typed answer) as JSON, which is the text then: a
/// second rendering would be read twice. A call the host refused for a reason
/// it can act on is answered with the host's code: `not_ready` until the
/// workspace serves it, `unanswered` to ask again under the same invocation
/// id. A call that ran more than is safe to run again (`retry_safe` false) is
/// never told to.
pub(super) fn text_result(
    result: Result<Reply, Failure>,
    structured: bool,
    retry_safe: bool,
) -> Value {
    /// The answer's content: what it says, and the pictures it shows.
    fn content(text: &str, images: Vec<Value>) -> Vec<Value> {
        let mut content = vec![json!({"type":"text","text":text})];
        content.extend(images);
        content
    }
    match result {
        Ok(Reply {
            structured: Some(answer),
            failed,
            images,
            ..
        }) => json!({
            "content":content(&answer.to_string(), images),
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
                "content":content(&answer.to_string(), reply.images),
                "structuredContent":answer,
                "isError":reply.failed
            })
        }
        Ok(reply) => json!({
            "content":content(&reply.text, reply.images),
            "isError":reply.failed
        }),
        Err(failure) if structured => {
            let answer = refused(&failure, retry_safe);
            json!({
                "content":content(&answer.to_string(), Vec::new()),
                "structuredContent":answer,
                "isError":true
            })
        }
        // A host's refusal: the caller reads the words, and a program reads
        // the typed refusal beside them, so it never matches the words.
        Err(failure) if failure.host.is_some() => {
            let typed = refused(&failure, retry_safe);
            json!({
                "content":content(&String::from(failure), Vec::new()),
                "structuredContent":typed,
                "isError":true
            })
        }
        Err(failure) => json!({
            "content":content(&String::from(failure), Vec::new()),
            "isError":true
        }),
    }
}

/// The typed answer of a call that did not run: what it was, and what would
/// work instead.
fn refused(failure: &Failure, retry_safe: bool) -> Value {
    use tilo_actions::remote::ActionCallError;
    let message = failure.message.as_str();
    // A failure with no host error of its own may name the code itself
    // (`unknown_action: …`): that is what a caller acts on, and the words
    // after it are what it reads.
    let (named, said) = coded(message);
    let message = match failure.host {
        None => said,
        Some(_) => message,
    };
    let (code, next) = match &failure.host {
        Some(ActionCallError::NotReady) if retry_safe => (
            "not_ready",
            "retry the same call once what it waits for arrived: tilo_watch says when the \
             workspace serves this session"
                .to_owned(),
        ),
        Some(ActionCallError::Unanswered) => (
            "unanswered",
            "call again with the same invocation_id: it answers what the call did, and runs it \
             once if it never arrived"
                .to_owned(),
        ),
        Some(other) => (other.code(), String::new()),
        None => (named, String::new()),
    };
    let mut error = json!({"code": code, "message": message});
    if let Some(fields) = error.as_object_mut()
        && !next.is_empty()
    {
        fields.insert("next_steps".into(), json!([next]));
    }
    json!({"status":"failed","error":error})
}

/// The codes a refusal is documented with (`docs/tilo-sh/errors.md`).
const CODES: [&str; 13] = [
    "invalid_arguments",
    "unknown_action",
    "unknown_resource",
    "unknown_app",
    "not_found",
    "not_permitted",
    "beyond_scope",
    "no_payment_method",
    "surface_changed",
    "action_unavailable",
    "action_failed",
    "unavailable",
    "stale_revision",
];

/// `message` without the code it starts with: what a sentence embedding a
/// refusal says, so the code is not repeated mid-sentence.
pub(super) fn words(message: &str) -> &str {
    coded(message).1
}

/// `message` split into the code it starts with, when it starts with one of
/// the documented ones, and the words that follow it.
pub(super) fn coded(message: &str) -> (&str, &str) {
    for code in CODES {
        if let Some(said) = message
            .strip_prefix(code)
            .and_then(|rest| rest.strip_prefix(": "))
        {
            return (code, said);
        }
    }
    ("failed", message)
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
                // What it made, per item: its id and the address it is
                // published under, beside the action's own answer.
                if let Some(made) = output
                    .get("value")
                    .and_then(|value| value.get("made"))
                    .filter(|made| !made.is_null())
                {
                    answer.insert("made".into(), made.clone());
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
            // What it waits on, when the host says: the person's answer, say.
            if let Some(message) = value
                .pointer("/update/message")
                .and_then(Value::as_str)
                .filter(|message| !message.trim().is_empty())
            {
                answer.insert("waiting_for".into(), message.into());
            }
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
            // Content from outside the user's data is said to be so before it
            // is shown, so a model reads the label before what it labels.
            if let Some(note) = value.get(UNTRUSTED).and_then(Value::as_str) {
                let _ = write!(text, "\n[{UNTRUSTED}: {note}]");
            }
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
            // What it waits on, where the host said (the person's answer,
            // say), else that it runs.
            let doing = value
                .pointer("/update/message")
                .and_then(Value::as_str)
                .filter(|message| !message.trim().is_empty())
                .unwrap_or("still running");
            let mut text = format!("pending {id}: {doing}");
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
        images: Vec::new(),
    }
}
