//! The call that works: what a refused call of an agent tool ends with.
//!
//! Every guess at what would have worked is one more ask of a model, and many seconds for the
//! person who waits. So a refusal ends with the call that works, written as its caller writes
//! one: with what the caller already gave, and a named blank in angle brackets where something
//! is missing. A refusal names its calls ([`Call`]); the words around them are written here
//! alone ([`said`]), so they are one mechanism's and not each refusal's own sentence.
//!
//! A call is only ever made of what the caller's own listing answered it: the path it named
//! and that was found for it, and the actions offered to it there. Nothing is named to a caller
//! that it could not list.

use super::{ActionDescriptor, Failure, Reply, Resolved, Value, close_matches, json, list_names};
use std::fmt::Write as _;
use tilo_actions::{ActionParam, ParamType};

/// The longest value of the caller's own, as JSON, that a refusal writes back into the call
/// that works; a longer one is named, not repeated. A refusal's words reach a server's log,
/// and what a person wrote at length does not belong there.
const GIVEN_BYTES: usize = 120;
/// What stands for a value of the caller's own that is not repeated.
const AS_GIVEN: &str = "<as you gave it>";
/// How many actions a refusal writes out as calls.
const CALLS: usize = 2;

/// A call of an agent tool, written as its caller writes one: `tilo_ls {"path":"notes"}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Call(String);

impl Call {
    /// The listing of what `path` holds and offers.
    pub(crate) fn lists(path: &str) -> Self {
        Self(format!("tilo_ls {}", json!({"path": path})))
    }

    /// How the invocation `id` went.
    pub(crate) fn status(id: &str) -> Self {
        Self(format!("tilo_status {}", json!({"invocation_id": id})))
    }

    /// `action` on `path` with what the caller `gave` of what the action takes, and a named
    /// blank for each argument it must be given and was not, and for each one that `why`, the
    /// words it was refused with, names between backticks.
    pub(crate) fn does(
        path: &str,
        action: &ActionDescriptor,
        gave: &serde_json::Map<String, Value>,
        why: &str,
    ) -> Self {
        let mut args = serde_json::Map::new();
        for param in &action.params {
            let name = param.name.to_string();
            let refused = why.contains(&format!("`{name}`"));
            let given = gave.get(&name);
            let value = if let Some(value) = given.filter(|_| !refused) {
                repeated(param, value)
            } else if given.is_some() || param.required || refused {
                Value::String(blank(param))
            } else {
                continue;
            };
            args.insert(name, value);
        }
        let path = json!(format!("{path}#{}", action.address.action));
        Self(format!(
            "tilo_do {{\"path\":{path},\"args\":{}}}",
            Value::Object(args)
        ))
    }

    /// The call as its caller writes it.
    pub(crate) fn written(&self) -> &str {
        &self.0
    }
}

/// What the caller gave for `param`, as the call that works repeats it: itself where it is
/// short and no secret, else the word that it stands as it was given.
fn repeated(param: &ActionParam, value: &Value) -> Value {
    if matches!(param.ty, ParamType::Secret) || value.to_string().len() > GIVEN_BYTES {
        Value::String(AS_GIVEN.to_owned())
    } else {
        value.clone()
    }
}

/// The named blank that stands where `param` must be given: its name, with the choices it
/// takes, or its type where that is no plain text.
fn blank(param: &ActionParam) -> String {
    if !param.choices.is_empty() {
        let choices: Vec<String> = param.choices.iter().map(ToString::to_string).collect();
        return format!("<{}: {}>", param.name, choices.join("|"));
    }
    match param.ty {
        ParamType::Text => format!("<{}>", param.name),
        other => format!("<{}: {}>", param.name, other.label()),
    }
}

/// What a refusal ends with: `calls`, the calls that work instead; nothing where there are
/// none.
pub(crate) fn said(calls: &[Call]) -> String {
    let own = if calls.iter().any(|call| call.0.contains('<')) {
        ", with your own value where one stands in angle brackets"
    } else {
        ""
    };
    match calls {
        [] => String::new(),
        [one] => format!("\nThe call that works{own}: {}", one.0),
        many => {
            let mut said = format!("\nCalls that work{own}:");
            for call in many {
                let _ = write!(said, "\n  {}", call.0);
            }
            said
        }
    }
}

/// `arguments` as `action` declares them: a text that holds the object or the list a parameter
/// takes is read as that object or list, as a caller writes one that was told `args` is JSON.
/// Whether the object is one the action takes stays the action's own to decide.
pub(crate) fn as_declared(
    action: &ActionDescriptor,
    mut arguments: serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    for param in &action.params {
        let object = matches!(param.ty, ParamType::Object);
        if !object && !matches!(param.ty, ParamType::Array) {
            continue;
        }
        let name = param.name.to_string();
        let held = match arguments.get(&name) {
            Some(Value::String(text)) => serde_json::from_str::<Value>(text).ok(),
            _ => None,
        };
        if let Some(held) = held.filter(|held| {
            if object {
                held.is_object()
            } else {
                held.is_array()
            }
        }) {
            arguments.insert(name, held);
        }
    }
    arguments
}

/// What a read of `path` answers where nothing reads it: the `actions` it offers, by name, and
/// the calls that work there: the listing of what it holds, where it `holds` anything or offers
/// nothing, and what it offers, as calls.
pub(crate) fn not_read(path: &str, actions: &[ActionDescriptor], holds: bool) -> Reply {
    let mut text = if actions.is_empty() {
        format!("{path} is not read as one text and offers no actions of its own")
    } else {
        let names: Vec<String> = actions
            .iter()
            .map(|action| action.address.action.clone())
            .collect();
        format!(
            "{path} has no read action; its actions: {}",
            list_names(&names)
        )
    };
    let mut calls = Vec::new();
    if holds || actions.is_empty() {
        calls.push(Call::lists(path));
    }
    let nothing = serde_json::Map::new();
    calls.extend(
        actions
            .iter()
            .take(CALLS)
            .map(|action| Call::does(path, action, &nothing, "")),
    );
    text.push_str(&said(&calls));
    Reply::failed(text)
}

/// The refusal of `named`, an action that what was found `at` a path does not offer, in the
/// words `why`: it ends with the actions offered there whose names come close, each as a call
/// with what the caller `gave`, and where none comes close with the listing of what the path
/// holds and offers.
pub(super) fn no_such_action(
    why: String,
    named: &str,
    at: &Resolved,
    gave: &serde_json::Map<String, Value>,
) -> Failure {
    let mut ids: Vec<String> = at
        .actions
        .iter()
        .map(|action| action.address.action.clone())
        .collect();
    ids.dedup();
    let mut calls: Vec<Call> = close_matches(named, &ids, CALLS)
        .iter()
        .filter_map(|id| {
            at.actions
                .iter()
                .find(|action| &action.address.action == id)
        })
        .map(|action| Call::does(&at.path, action, gave, ""))
        .collect();
    if calls.is_empty() {
        calls.push(Call::lists(&at.path));
    }
    Failure::from(why).ending_with(calls)
}
