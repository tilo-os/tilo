//! What an agent tool answers typed: one answer for a call that ran, one
//! for a call that did not, and the rows a listing and a refusal carry.
use super::render::render_value;
use super::{ActionDescriptor, Failure, Reply, Value, json};
use crate::UNTRUSTED;
use std::fmt::Write as _;

/// The MCP result of an agent tool: its text alone, or the structured copy
/// the caller asked for (its typed answer) as JSON, which is the text then: a
/// second rendering would be read twice. A typed value answered to a caller
/// that asked for none carries the words in its text as `text`.
/// A call the host refused for a reason
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
            text,
        }) => {
            // A caller that asked for no typed answer reads the words beside the typed fields; the
            // typed answer itself holds none, and a typed caller's text is that answer exactly.
            let mut said = answer.clone();
            if !structured
                && !text.is_empty()
                && let Some(fields) = said.as_object_mut()
            {
                fields.entry("text").or_insert_with(|| json!(text));
            }
            json!({
                "content":content(&said.to_string(), images),
                "structuredContent":answer,
                "isError":failed
            })
        }
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
pub(super) fn refused(failure: &Failure, retry_safe: bool) -> Value {
    let message = failure.message.as_str();
    // A failure with no host error of its own may name the code itself
    // (`unknown_action: …`): that is what a caller acts on, and the words
    // after it are what it reads.
    let (named, said) = coded(message);
    let message = match failure.host {
        None => said,
        Some(_) => message,
    };
    // A key its workspace keeps as revoked is told so by every surface, with the one command
    // that approves another: the caller knows what to do, not only that it may not.
    if let Some(at) = message.find("key_revoked:") {
        let said = message
            .get(at.saturating_add("key_revoked:".len())..)
            .unwrap_or_default()
            .trim();
        return json!({"status":"failed","error":{
            "code":"key_revoked",
            "message":said,
            "next_steps":[tilo_actions::remote::APPROVE_AGAIN],
        }});
    }
    // A refusal that came as words (a lookup the workspace did not answer, said on its way
    // through the path's resolution) keeps the code it starts with, and is told what to do
    // the same as the typed refusal of that code.
    let code = match &failure.host {
        Some(host) => host.code(),
        None => named,
    };
    let next = match code {
        "not_ready" if retry_safe => {
            "retry the same call once what it waits for arrived: tilo_watch says when the \
             workspace serves this session"
        }
        "unanswered" => {
            "call again with the same invocation_id: it answers what the call did, and runs it \
             once if it never arrived"
        }
        _ => "",
    };
    let mut error = json!({"code": code, "message": message});
    if let Some(fields) = error.as_object_mut()
        && !next.is_empty()
    {
        fields.insert("next_steps".into(), json!([next]));
    }
    json!({"status":"failed","error":error})
}

/// Whether `failure` is answered by asking the same call again under the same invocation id:
/// the workspace was not serving this session yet (`not_ready`), or did not answer in time
/// (`unanswered`), whether the host's typed refusal said so or the words it came as do.
pub(super) fn asks_again(failure: &Failure) -> bool {
    match &failure.host {
        Some(host) => host.ask_again(),
        None => matches!(coded(&failure.message).0, "not_ready" | "unanswered"),
    }
}

/// The typed answer of a call under invocation `id` that `failure` says to ask again
/// ([`asks_again`]): the refusal, with the id the same call is asked again under.
pub(super) fn refused_under(failure: &Failure, id: &str, retry_safe: bool) -> Value {
    let mut answer = refused(failure, retry_safe);
    if let Some(fields) = answer.as_object_mut() {
        fields.insert("invocation_id".into(), json!(id));
    }
    answer
}

/// The codes a refusal is documented with (`docs/tilo-sh/errors.md`), and the two a caller
/// waits on and asks again after (`not_ready`, `unanswered`): a lookup refused with one of those
/// keeps its code, rather than reading as a path that names nothing.
const CODES: [&str; 16] = [
    "invalid_arguments",
    "unknown_action",
    "not_offered",
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
    "not_ready",
    "unanswered",
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
        _ if says_asked(value) => {
            // Waits in its asked record for the person's answer: no failure, and nothing
            // runs until they give it.
            answer.insert("status".into(), "asked".into());
            if let Some(record) = asked_record(value) {
                answer.insert("record".into(), record.into());
            }
        }
        _ => {
            // Still running: the invocation id follows it, and the same call
            // with the same id asks again where it went.
            answer.insert("status".into(), "pending".into());
            // What it waits on, when the host says: the person's answer, say, also where
            // the call ended as it asked them.
            if let Some(message) = value
                .pointer("/update/message")
                .or_else(|| value.pointer("/update/error/message"))
                .and_then(Value::as_str)
                .filter(|message| !message.trim().is_empty())
            {
                answer.insert("waiting_for".into(), message.into());
            }
        }
    }
    if let Some(untaken) = value.get(UNTRUSTED) {
        answer.insert(UNTRUSTED.into(), untaken.clone());
    }
    Value::Object(answer)
}

/// One action of a listing, typed: what it is called, what it takes, what
/// running it may reach, and the schema of its result where it declares one.
pub(super) fn action_row(action: &ActionDescriptor) -> Value {
    let mut row = json!({
        "id": action.address.action,
        "label": action.action_label,
        "params": action.params,
        "kind": action.contract.kind,
    });
    if let Some(fields) = row.as_object_mut() {
        if let Some(description) = action
            .description
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            fields.insert("description".into(), json!(description.trim()));
        }
        if let tilo_actions::ResultSpec::Json { schema } = &action.contract.result {
            fields.insert("result".into(), schema.clone());
        }
    }
    row
}

/// One action of a listing of what a thing holds, typed: what it is called, what it may reach,
/// and each thing it takes by its name and type with whether it must be given. What a
/// parameter means, its choices and its schema are answered where that one action is asked for
/// ([`action_row`]): said of every action of every kind on a page they were nine tenths of a
/// listing, and its rows stood behind them.
pub(super) fn action_named(action: &ActionDescriptor) -> Value {
    let takes: Vec<Value> = action
        .params
        .iter()
        .map(|param| {
            json!({
                "name": param.name,
                "ty": param.ty,
                "required": param.required,
            })
        })
        .collect();
    json!({
        "id": action.address.action,
        "label": action.action_label,
        "params": takes,
        "kind": action.contract.kind,
    })
}

pub(super) fn actions_named(actions: &[ActionDescriptor]) -> Vec<Value> {
    actions.iter().map(action_named).collect()
}

/// What an invocation whose person is asked says of it.
const PERSON_ASKED: &str = "Its person is asked to allow this. Nothing happened yet.";
/// What whoever made such a call does meanwhile.
const ASKED_MEANWHILE: &str = "Go on with what does not need it, tell them it waits for their \
     answer, and do not make this call again.";

/// Whether the answer `value` says that its invocation waits in its asked record for its
/// person's answer: a progress of the asked phase.
fn says_asked(value: &Value) -> bool {
    value.pointer("/update/state").and_then(Value::as_str) == Some("progress")
        && value.pointer("/update/phase").and_then(Value::as_str)
            == Some(tilo_actions::remote::asked::ASKED_PHASE)
}

/// The asked record the answer `value` waits in (`<workspace>/<record>`).
fn asked_record(value: &Value) -> Option<&str> {
    value
        .pointer("/update/message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|record| !record.is_empty())
}

/// What an agent reads of an invocation whose person is asked to allow it: the record it
/// waits in, what to do meanwhile, and the id that says how it went.
fn asked_words(value: &Value) -> String {
    let id = value
        .get("invocation_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let record = asked_record(value)
        .map(|record| format!(" It waits in {record}."))
        .unwrap_or_default();
    format!(
        "asked: {PERSON_ASKED}{record} {ASKED_MEANWHILE}\ntilo_status {{\"invocation_id\":\"{id}\"}} \
         says how it went"
    )
}

/// What a resource an invocation's answer names is called in a tool's text: one a tilolang app
/// publishes (`tl:<app>:<id>`) by the path `tilo_read` takes it at (`<app>/<id>`), and a
/// package's own (`tl:<app>`) by its app (`<app>`), so an agent goes on with the line alone;
/// any other address as it is.
fn shown_resource(resource: &str) -> String {
    resource
        .strip_prefix("tl:")
        .map_or_else(|| resource.to_owned(), |rest| rest.replacen(':', "/", 1))
}

/// What an invocation's final update says, as an agent reads it.
pub(super) fn outcome(value: &Value) -> Reply {
    let state = value
        .pointer("/update/state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let asked = says_asked(value);
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
                let _ = write!(text, "\n-> {}", shown_resource(resource));
            }
            text
        }
        // Its person is asked to allow it, which is no failure and nothing to wait for.
        "progress" if asked => asked_words(value),
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
        failed: state != "succeeded" && !asked,
        text,
        structured: None,
        images: Vec::new(),
    }
}
