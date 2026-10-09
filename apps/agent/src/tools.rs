//! What the agent can be *asked*: named tools over the room's own surface.
//!
//! Discovery is queried through separate tools instead of a single flat list.
//! This keeps the set of operations independent from response size, preserves
//! operation details and examples, and includes resources that publish facts
//! without actions. The agent asks for the scope it needs:
//!
//! | Tool | The question it answers |
//! |---|---|
//! | [`LIST_APPLICATIONS`] | what is running, and what could be opened |
//! | [`LIST_OPERATIONS`] | what can be done — to one resource, to a kind of resource in any application, in one application, or anywhere |
//! | [`DESCRIBE_OPERATION`] | what one operation does, what it takes, the application's own example of it, and what the resource says about itself |
//! | [`INVOKE`] | perform one published operation now, and see what it changed |
//! | [`PLAN`] | the answer: the steps to perform in order (plan mode), or the reply that finishes the task (loop mode) |
//!
//! Two tool sets are built from these. The *plan* set ([`definitions`]) is
//! discovery plus a `plan` naming every step; the caller dispatches the steps
//! afterwards. The *loop* set ([`loop_definitions`]) is discovery plus
//! [`INVOKE`] plus a `plan` that only carries the reply: the agent
//! discovers, invokes and observes step by step, the same way a person uses
//! the palette, and a step that changes its view (a selection, a find) can
//! make later operations available.
//!
//! ## Nothing here can perform anything
//!
//! The discovery tools are read-only in the shape of what they return rather
//! than by promise: a listing is labels and addresses, with no callback,
//! token or handle anywhere in one — "what can I do here" must be free to
//! ask, so that a caller exploring an unfamiliar room can explore it safely.
//!
//! [`PLAN`] performs nothing either: it ends the conversation with an answer,
//! which the caller validates against a freshly taken survey before anything
//! is dispatched. [`INVOKE`] is the one tool that acts, and only through the
//! same broker path and checks as any other step: the id must be one a tool
//! showed, the operation must still be published, and the broker authorizes
//! it again.
//!
//! ## An id cannot be invented
//!
//! What [`PLAN`] and [`INVOKE`] accept is **what this conversation has
//! actually shown the model** ([`Revealed`]): the operations summarised in
//! its first turn ([`catalogue_summary`]) and whatever the discovery tools
//! returned since — plus "none", which is the agent's own. The model does not
//! pick from a list somebody guessed it would need; it picks from what it was
//! shown. An id nothing showed is refused by [`read_plan`] / [`read_invoke`]
//! with the ids it may have meant, answered to the model so it can correct
//! itself, and would still not survive the survey check.
//!
//! The check lives there rather than in a per-conversation schema `enum` so
//! the tool definitions stay byte-identical across turns and tasks: they
//! come first in every request, and a provider's prompt cache keeps only an
//! unchanged prefix.
//!
//! ## Authority is upstream and unchanged
//!
//! Every listing is built from a [`Catalogue`] that was already gated: a
//! denied operation is **absent** from `offers`, so it is absent from every
//! tool result, absent from the enum, and unnameable. Nothing in this module
//! asks a permission question and nothing in it can widen one.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::plan;
use crate::surface::{self, APPLICATION_KIND, CONTRACTS, Catalogue, Offer};

pub const LIST_APPLICATIONS: &str = "list_applications";
pub const LIST_OPERATIONS: &str = "list_operations";
pub const DESCRIBE_OPERATION: &str = "describe_operation";
pub const INVOKE: &str = "invoke";
/// The [`INVOKE`] property holding the argument object.
pub const INVOKE_PARAMETERS: &str = "parameters";
/// The optional [`INVOKE`] property carrying the task's final reply.
pub const INVOKE_REPLY: &str = "reply";
pub const PLAN: &str = "plan";

/// How many operations one listing returns.
///
/// A cap on a **reply**, not on reach: a listing cut short says so and says
/// how to narrow it, and the narrowed listing reaches whatever was cut.
pub const MOST_LISTED: usize = 60;

// ---------------------------------------------------------------------------
// What has been shown
// ---------------------------------------------------------------------------

/// The addresses the discovery tools have returned so far in one conversation.
///
/// The whole of what [`PLAN`] will accept, which is why a made-up id cannot
/// be *answered* rather than merely refused afterwards. It starts as the
/// agent's own finite vocabulary — the editorial changes and "none" — and
/// grows only when a tool actually put an address in front of the model.
#[derive(Clone, Debug, Default)]
pub struct Revealed(BTreeSet<String>);

impl Revealed {
    pub fn new() -> Self {
        let mut set = BTreeSet::new();
        set.insert(plan::NOTHING_FITS.to_string());
        Revealed(set)
    }

    pub fn reveal(&mut self, id: impl Into<String>) {
        self.0.insert(id.into());
    }

    pub fn contains(&self, id: &str) -> bool {
        self.0.contains(id)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The revealed id `given` names: itself, or — when a model copied an id
    /// without its `broker:<peer>:` routing prefix, or with another peer's —
    /// the one revealed id with the same address. Only what was shown can be
    /// named this way, and only unambiguously.
    ///
    /// # Errors
    ///
    /// In words for the model, with the revealed ids for the same action as
    /// suggestions.
    pub fn resolve(&self, given: &str) -> Result<String, String> {
        self.lookup(given).map_err(|similar| {
            format!(
                "{:?} is not an operation any listing in this conversation showed. {}",
                given.trim(),
                copy_advice(&similar)
            )
        })
    }

    /// [`Self::resolve`], refusing with up to three shown ids for the same
    /// action instead of words.
    fn lookup(&self, given: &str) -> Result<String, Vec<&str>> {
        let given = given.trim();
        if self.contains(given) && given != plan::NOTHING_FITS {
            return Ok(given.to_owned());
        }
        let address = address_part(given);
        let same: Vec<&str> = self
            .0
            .iter()
            .map(String::as_str)
            .filter(|id| *id != plan::NOTHING_FITS && address_part(id) == address)
            .collect();
        if let [only] = same.as_slice() {
            return Ok((*only).to_owned());
        }
        if !same.is_empty() {
            return Err(same.into_iter().take(3).collect());
        }
        let action = address.rsplit_once(surface::ADDRESS_SEP).map(|(_, a)| a);
        Err(self
            .0
            .iter()
            .map(String::as_str)
            .filter(|id| {
                action.is_some_and(|action| {
                    id.rsplit_once(surface::ADDRESS_SEP)
                        .is_some_and(|(_, a)| a == action)
                })
            })
            .take(3)
            .collect())
    }
}

/// How to name a shown operation instead, with the shown ids that look
/// like what was meant.
fn copy_advice(similar: &[&str]) -> String {
    let mut advice = format!(
        "Copy an id exactly as listed (with its `broker:<n>:` prefix), or ask \
         `{LIST_OPERATIONS}` first"
    );
    if !similar.is_empty() {
        let _ = write!(
            advice,
            ". Listed ids for that action: {}",
            similar.join(", ")
        );
    }
    advice
}

/// An operation id without its `broker:<peer>:` routing prefix.
fn address_part(id: &str) -> &str {
    id.strip_prefix("broker:")
        .and_then(|rest| rest.split_once(':'))
        .filter(|(peer, _)| !peer.is_empty() && peer.bytes().all(|b| b.is_ascii_digit()))
        .map_or(id, |(_, address)| address)
}

/// The application an operation opens in a new window, when it is an open:
/// an application's own `open`, or the launcher's open that the desktop
/// publishes for every application it can start (`desktop:app:<app>#open`).
pub fn opened_app(offer: &Offer) -> Option<&str> {
    let address = offer.address();
    surface::opens(address).or_else(|| {
        (address.action == surface::OPEN_ACTION)
            .then(|| address.resource.strip_prefix(LAUNCHER_OPEN_PREFIX))
            .flatten()
            .filter(|app| !app.is_empty())
    })
}

/// What an open does, said next to it: start a closed application, or —
/// when the application already publishes operations — a second window, as
/// the launcher does for a person.
fn open_note(catalogue: &Catalogue, app: &str) -> String {
    if catalogue
        .offers
        .iter()
        .any(|o| o.app == app && opened_app(o).is_none())
    {
        format!(
            " ({app} is already open and its operations are listed: use those; this would \
             open a second window)"
        )
    } else {
        format!(" (opens {app} in a new window; its operations then appear)")
    }
}

/// The resource prefix of the launcher's published open.
pub const LAUNCHER_OPEN_PREFIX: &str = "desktop:app:";

/// One operation's parameters as a compact signature and how to pass them,
/// for an invoke that was refused for its arguments.
pub fn parameter_hint(offer: &Offer) -> String {
    if offer.wants_content() {
        return "It takes what an earlier step produced, not words.".into();
    }
    if offer.descriptor.params.is_empty() {
        return "It takes no parameters: pass `parameters: {}`.".into();
    }
    let params = offer
        .descriptor
        .params
        .iter()
        .map(|p| {
            let mut line = format!(
                "`{}`: {} ({}) — {}",
                p.name,
                p.ty.label(),
                if p.required { "required" } else { "optional" },
                p.description
            );
            if !p.choices.is_empty() {
                let choices = p
                    .choices
                    .iter()
                    .map(std::convert::AsRef::as_ref)
                    .collect::<Vec<_>>()
                    .join("|");
                let _ = write!(line, " [{choices}]");
            }
            line
        })
        .collect::<Vec<_>>()
        .join("; ");
    let example = offer
        .descriptor
        .params
        .iter()
        .filter(|p| p.required)
        .map(|p| format!("\"{}\": …", p.name))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{} takes: {params}. Pass them inside `parameters`, e.g. \"parameters\": {{{example}}}.",
        offer.descriptor.action_label
    )
}

/// What a model answers [`PLAN`] with, before it has been checked against a
/// freshly taken survey.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answer {
    pub actions: Vec<String>,
    pub arguments: Vec<serde_json::Map<String, Value>>,
    pub why: String,
    /// What the request still asks for that nothing shown could express yet
    /// — the words after the steps named, kept for when an earlier step
    /// opens the application that would answer it.
    pub then: Option<String>,
    /// The optional chat reply to the person who asked, bounded; the task's
    /// terminal message when present.
    pub reply: Option<String>,
}

/// The `plan` tool's optional `reply` property, as the model is told it.
pub const PLAN_REPLY_DESCRIPTION: &str =
    "One or two sentences to the person who asked, written as you would answer them in chat.";

/// The most characters of a plan's `reply` that are kept.
pub const MAX_PLAN_REPLY_CHARS: usize = 1000;

// ---------------------------------------------------------------------------
// The definitions
// ---------------------------------------------------------------------------

/// The [`LIST_APPLICATIONS`] tool definition.
fn list_applications_definition() -> Value {
    json!({
        "name": LIST_APPLICATIONS,
        "description":
            "Every application in this room. Says which are open and which are not, \
             and how many operations each is exporting right now. An application \
             exporting nothing has nothing open in it yet — opening it onto \
             something is an ordinary step, and what it does is put that \
             application's own operations on the list. Call this first when the \
             request might involve an application you have not seen.",
        "input_schema": { "type": "object", "properties": {}, "additionalProperties": false }
    })
}

/// The [`LIST_OPERATIONS`] tool definition.
fn list_operations_definition() -> Value {
    json!({
        "name": LIST_OPERATIONS,
        "description":
            "What can be done in this room, right now. Every filter is optional and \
             they combine; with none of them you get everything, capped. Use `kind` \
             to ask what may be done to a kind of thing in any application at all \
             (`files.entry`, `word_processor.section`), `resource` for one named \
             thing, `application` for one application, and `search` to match the \
             applications' own words. The ids that come back, like those already \
             listed with the request, are the ones `plan` and `invoke` accept.",
        "input_schema": {
            "type": "object",
            "properties": {
                "application": {
                    "type": "string",
                    "description": "One application's namespace, as `list_applications` gives it."
                },
                "resource": {
                    "type": "string",
                    "description": "One resource id, for everything that may be done to that one thing."
                },
                "kind": {
                    "type": "string",
                    "description": "A resource kind, for everything that may be done to that kind of thing in any application."
                },
                "search": {
                    "type": "string",
                    "description": "Words to match against the applications' own labels and descriptions."
                }
            },
            "additionalProperties": false
        }
    })
}

/// The [`DESCRIBE_OPERATION`] tool definition.
fn describe_operation_definition() -> Value {
    json!({
        "name": DESCRIBE_OPERATION,
        "description":
            "One operation in full: what the application says it does, what it does \
             it to, whether performing it needs an argument, and — when it does — \
             the application's own prompt for that argument and its own example of \
             one. Also what the publishing resource says about itself, which is \
             where an application states a fact it has no operation to hang on. \
             Call this before planning an operation whose arguments its listed \
             signature leaves unclear — the example and the facts are what make the \
             value computable rather than guessed.",
        "input_schema": {
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "The id of an operation a listing returned."
                }
            },
            "required": ["operation"],
            "additionalProperties": false
        }
    })
}

/// The [`PLAN`] tool definition. Byte-stable across turns and tasks (no
/// per-conversation enum), so the provider's prompt cache keeps the whole
/// tool prefix; [`read_plan`] refuses an id nothing in the conversation
/// showed, and the refusal goes back to the model.
fn plan_definition(most_steps: usize) -> Value {
    json!({
        "name": PLAN,
        "description": format!(
            "The answer. The operations to perform, in the order the request asks \
             for them — one for a request that asks for one thing, which is most of \
             them, and at most {most_steps}. Call this exactly once, when you know \
             what to do — straight away when the operations listed with the request \
             already do it. Every `operation` must be an id exactly as that list or a \
             listing showed it; any other id is refused."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                // First, so it streams before the steps and a live reply can
                // grow while the rest of the plan is still being written.
                "reply": {
                    "type": "string",
                    "description": PLAN_REPLY_DESCRIPTION
                },
                "steps": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "operation": {
                                "type": "string",
                                "description": format!(
                                    "The operation to perform, or {:?} for a step whose \
                                     operation cannot be named yet because the application \
                                     that would export it is not open — that step is asked \
                                     again after the open, against what that application \
                                     then publishes.",
                                    plan::NOTHING_FITS
                                )
                            },
                            "arguments": {
                                "type": "object",
                                "description": ARGUMENTS_DESCRIPTION
                            }
                        },
                        "required": ["operation", "arguments"],
                        "additionalProperties": false
                    }
                },
                "why": { "type": "string", "description": "One short clause." },
                "then": {
                    "anyOf": [{ "type": "string" }, { "type": "null" }],
                    "description":
                        "What the request still asks for that nothing you were shown can \
                         express yet, in the request's own words — the rest of the \
                         sentence, after the steps you named. Use it when opening an \
                         application is what would put the needed operation on the list, \
                         since that operation cannot be named until it is open. Null when \
                         the steps you named already do all of it."
                }
            },
            "required": ["steps", "why", "then"],
            "additionalProperties": false
        }
    })
}

/// The plan tool set as the API is told about it. A function of `most_steps`
/// alone, so it is byte-identical on every turn of a conversation.
pub fn definitions(most_steps: usize) -> Value {
    json!([
        list_applications_definition(),
        list_operations_definition(),
        describe_operation_definition(),
        plan_definition(most_steps),
    ])
}

/// The description shared by `plan` steps and `invoke` of an argument object.
const ARGUMENTS_DESCRIPTION: &str = "The complete JSON argument object described by \
    `describe_operation`, keyed by the application's parameter names. Use {} for a nullary \
    operation. Values retain their declared JSON types; never stringify numbers, booleans, \
    arrays, objects or null. When the descriptor's confirmation policy is Confirm, also \
    include `confirm: true`; for Phrase include `confirmation_phrase` with the exact phrase. \
    Those two names are invocation metadata, not app parameters.";

/// The [`INVOKE`] tool definition.
fn invoke_definition() -> Value {
    // Byte-stable across turns and tasks (no per-conversation enum), so the
    // provider's prompt cache keeps the whole tool prefix; `read_invoke`
    // refuses an id no tool showed.
    json!({
        "name": INVOKE,
        "description":
            "Perform one listed operation now, as a person using the application would. \
             Returns the result, how your own view changed and which operations became \
             available. Several invokes in one turn run in order as soon as each is \
             written; after a failure the rest of the turn is skipped. Put your reply \
             on the last invoke of the task to finish in the same turn.",
        "input_schema": {
            "type": "object",
            // Sorted maps on the wire: `operation` precedes `parameters` (its
            // text is attributable while it streams), and `reply` comes last.
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "An operation id exactly as listed."
                },
                INVOKE_PARAMETERS: {
                    "type": "object",
                    "description": ARGUMENTS_DESCRIPTION
                },
                PARTS: parts_schema(),
                INVOKE_REPLY: {
                    "type": "string",
                    "description":
                        "Only on the last invoke of the task: your short reply to the person \
                         (shown in your agent panel only). The task finishes when this invoke \
                         succeeds and every part is done; no plan call is needed."
                },
                UNABLE: unable_schema()
            },
            "required": ["operation", INVOKE_PARAMETERS],
            "additionalProperties": false
        }
    })
}

/// The loop's terminal [`PLAN`]: the reply that finishes the task.
fn finish_definition() -> Value {
    json!({
        "name": PLAN,
        "description":
            "Finish the task with your reply to the person (shown in your agent panel \
             only) when no invoke carried it: alone, or in the same turn as your last \
             invokes (it takes effect only if they all succeed and every part is done). \
             `done` is false only when the request cannot be completed with the \
             operations available; say why in the reply and list those parts under \
             `unable`.",
        "input_schema": {
            "type": "object",
            "properties": {
                // First, so the reply streams as it is written.
                "reply": {
                    "type": "string",
                    "description": PLAN_REPLY_DESCRIPTION
                },
                "done": {
                    "type": "boolean",
                    "description": "Whether the request was completed."
                },
                PARTS: parts_schema(),
                UNABLE: unable_schema()
            },
            "required": ["reply", "done"],
            "additionalProperties": false
        }
    })
}

/// The property listing the request's parts (first invoke or plan).
pub const PARTS: &str = "parts";
/// The property naming parts that cannot be done, with the reason.
pub const UNABLE: &str = "unable";

fn parts_schema() -> Value {
    json!({
        "type": "array",
        "description":
            "On your FIRST invoke (or plan): every separate thing the request asks for, in \
             order, each with the application it happens in (its id, e.g. `so.tilo.chat`). \
             Each message to post is its own part. Opening an application is a step, not a \
             part: list what the request wants done in it. The task cannot finish until \
             each part has a successful operation in its application or is listed under \
             `unable`.",
        "items": {
            "type": "object",
            "properties": {
                "part": { "type": "string" },
                "app": { "type": "string" }
            },
            "required": ["part", "app"],
            "additionalProperties": false
        }
    })
}

fn unable_schema() -> Value {
    json!({
        "type": "array",
        "description": "Parts you cannot do with the operations available, each with the reason.",
        "items": { "type": "string" }
    })
}

/// One part of a request, as the model listed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub text: String,
    /// The application it happens in, as the model named it.
    pub app: String,
}

/// The `parts` of a call, bounded.
pub fn read_parts(input: &Value) -> Vec<Part> {
    input
        .get(PARTS)
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .take(12)
                .filter_map(|part| {
                    let text = part.get("part")?.as_str()?.trim();
                    let app = part.get("app").and_then(Value::as_str).unwrap_or("").trim();
                    (!text.is_empty()).then(|| Part {
                        text: text.chars().take(200).collect(),
                        app: app.chars().take(120).collect(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `unable` explanations of a call, bounded.
pub fn read_unable(input: &Value) -> Vec<String> {
    input
        .get(UNABLE)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(12)
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| text.chars().take(300).collect())
                .collect()
        })
        .unwrap_or_default()
}

/// The loop tool set: discovery, [`INVOKE`], and the terminal [`PLAN`].
pub fn loop_definitions() -> Value {
    json!([
        list_applications_definition(),
        list_operations_definition(),
        describe_operation_definition(),
        invoke_definition(),
        finish_definition(),
    ])
}

/// What the loop's terminal [`PLAN`] said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finish {
    pub reply: Option<String>,
    pub done: bool,
}

/// Read the loop's terminal [`PLAN`] call. Lenient by design: a missing
/// `done` is read as done, a blank reply as none.
pub fn read_finish(input: &Value) -> Finish {
    Finish {
        reply: input
            .get("reply")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|reply| !reply.is_empty())
            .map(|reply| reply.chars().take(MAX_PLAN_REPLY_CHARS).collect()),
        done: input.get("done").and_then(Value::as_bool).unwrap_or(true),
    }
}

/// An [`INVOKE`] call's input, before it is checked against the catalogue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub operation: String,
    pub arguments: serde_json::Map<String, Value>,
    /// The task's final reply, when this is its last invoke.
    pub reply: Option<String>,
}

/// Read an [`INVOKE`] call.
///
/// # Errors
///
/// In words for the model: no operation, an operation no tool in this
/// conversation showed, or arguments that are not an object.
pub fn read_invoke(input: &Value, revealed: &Revealed) -> Result<Invocation, String> {
    let operation = input
        .get("operation")
        .and_then(Value::as_str)
        .ok_or_else(|| "invoke needs an `operation`".to_string())?;
    let operation = revealed.resolve(operation)?;
    let operation = operation.as_str();
    // `arguments` is accepted too: it is what a model trained on the common
    // name may write anyway.
    let given = input
        .get(INVOKE_PARAMETERS)
        .or_else(|| input.get("arguments"));
    let arguments = match given {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => return Err(format!("`{INVOKE_PARAMETERS}` must be a JSON object")),
    };
    Ok(Invocation {
        operation: operation.to_owned(),
        arguments,
        reply: input
            .get(INVOKE_REPLY)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|reply| !reply.is_empty())
            .map(|reply| reply.chars().take(MAX_PLAN_REPLY_CHARS).collect()),
    })
}

/// How many operations the first turn's catalogue shows.
pub const MOST_SUMMARISED: usize = 60;

/// An operation's parameters as a compact signature (`{text: text, n?:
/// integer [a|b]}`), or what it takes instead; empty for none.
pub fn parameter_signature(offer: &Offer) -> String {
    if offer.wants_content() {
        return "{takes what the previous step produced}".into();
    }
    if offer.descriptor.params.is_empty() {
        return String::new();
    }
    let params = offer
        .descriptor
        .params
        .iter()
        .map(|param| {
            let mut p = format!(
                "{}: {}{}",
                param.name,
                param.ty.label(),
                if param.required { "" } else { "?" }
            );
            if !param.choices.is_empty() {
                let choices = param
                    .choices
                    .iter()
                    .map(std::convert::AsRef::as_ref)
                    .collect::<Vec<_>>()
                    .join("|");
                let _ = write!(p, " [{choices}]");
            }
            p
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{params}}}")
}

/// The published operations, compactly, for the first turn (of the task loop
/// and of a plan decision): enough to invoke or plan the common ones directly
/// without a discovery round trip. Every id shown is revealed. Every
/// application gets its share (document operations first,
/// then view, then the rest), and every open — how a closed application is
/// started — is always shown; a cut summary says how to reach what it left
/// out.
pub fn catalogue_summary(catalogue: &Catalogue, revealed: &mut Revealed, most: usize) -> String {
    let rank = |offer: &&Offer| match offer.descriptor.kind {
        _ if opened_app(offer).is_some() => 0,
        tilo_actions::OperationKind::Document => 1,
        tilo_actions::OperationKind::View => 2,
        tilo_actions::OperationKind::Effect => 3,
    };
    // Per application, best first; then round-robin so a large application
    // cannot crowd the others (or the opens) out of the summary.
    let mut per_app: std::collections::BTreeMap<&str, Vec<&Offer>> =
        std::collections::BTreeMap::default();
    for offer in &catalogue.offers {
        per_app.entry(offer.app.as_str()).or_default().push(offer);
    }
    for offers in per_app.values_mut() {
        offers.sort_by_key(rank);
    }
    let mut shown: Vec<&Offer> = catalogue
        .offers
        .iter()
        .filter(|offer| opened_app(offer).is_some())
        .collect();
    let mut depth = 0;
    while shown.len() < most {
        let mut any = false;
        for offers in per_app.values() {
            if let Some(offer) = offers.get(depth) {
                any = true;
                if opened_app(offer).is_none() && shown.len() < most {
                    shown.push(offer);
                }
            }
        }
        if !any {
            break;
        }
        depth += 1;
    }
    if shown.is_empty() {
        return "No application publishes an operation you may perform right now.\n".into();
    }
    // Grouped by application, then by resource in first-appearance order.
    let mut groups: Vec<(&str, &str)> = Vec::new();
    for app in per_app.keys() {
        for offer in shown.iter().filter(|o| o.app == *app) {
            let resource = offer.address().resource.as_str();
            if !groups.contains(&(app, resource)) {
                groups.push((app, resource));
            }
        }
    }
    let mut s = String::new();
    for (app, resource) in groups {
        let mut first = true;
        for offer in shown
            .iter()
            .filter(|o| o.app == app && o.address().resource == resource)
        {
            if first {
                let _ = writeln!(
                    s,
                    "In {app}: {} ({}) — resource {resource}",
                    offer.descriptor.resource_label, offer.descriptor.resource_kind
                );
                first = false;
            }
            revealed.reveal(offer.id());
            let _ = write!(s, "  {} — {}", offer.id(), offer.descriptor.action_label);
            if let Some(opened) = opened_app(offer) {
                s.push_str(&open_note(catalogue, opened));
            }
            if let tilo_actions::Availability::Unavailable { reason, .. } =
                &offer.descriptor.contract.availability
            {
                let _ = write!(s, " (currently unavailable: {reason})");
            }
            let signature = parameter_signature(offer);
            if !signature.is_empty() {
                let _ = write!(s, " {signature}");
            }
            s.push('\n');
        }
    }
    let rest = catalogue.offers.len().saturating_sub(shown.len());
    if rest > 0 {
        let _ = writeln!(
            s,
            "… and {rest} more; ask `{LIST_OPERATIONS}` (by application, kind, resource or \
             search) to reach them."
        );
    }
    s
}

/// What one [`INVOKE`] did, as the model reads it.
#[derive(Clone, Debug, Default)]
pub struct InvokeReport {
    pub succeeded: bool,
    pub error: Option<String>,
    /// The application's declared result value.
    pub value: Option<Value>,
    /// Resources the operation produced or revealed.
    pub resources: Vec<String>,
    /// How the invoker's own view changed, when the operation is a view
    /// operation that says.
    pub changed_view: Option<Value>,
    /// Operations that became available because of this step, as
    /// `"<id> — <label>"`.
    pub newly_available: Vec<String>,
    /// Set when the step did not run at all.
    pub skipped: Option<String>,
    /// What to do next, when the loop can tell (e.g. after an open).
    pub note: Option<String>,
}

/// The most characters of a returned value shown to the model.
pub const MOST_RESULT_CHARS: usize = 2_000;

/// A compact JSON observation for one [`INVOKE`].
pub fn invoke_result(report: &InvokeReport) -> String {
    let bounded = |value: &Value| {
        let text = value.to_string();
        if text.chars().count() <= MOST_RESULT_CHARS {
            value.clone()
        } else {
            Value::String(format!(
                "{}… (cut)",
                text.chars().take(MOST_RESULT_CHARS).collect::<String>()
            ))
        }
    };
    let mut out = serde_json::Map::new();
    if let Some(skipped) = &report.skipped {
        out.insert("result".into(), Value::String("not run".into()));
        out.insert("why".into(), Value::String(skipped.clone()));
        return Value::Object(out).to_string();
    }
    out.insert(
        "result".into(),
        Value::String(
            if report.succeeded {
                "succeeded"
            } else {
                "failed"
            }
            .into(),
        ),
    );
    if let Some(error) = &report.error {
        out.insert("error".into(), Value::String(error.clone()));
    }
    if let Some(value) = &report.value {
        out.insert("value".into(), bounded(value));
    }
    if !report.resources.is_empty() {
        out.insert("resources".into(), json!(report.resources));
    }
    out.insert(
        "changed_view".into(),
        report.changed_view.as_ref().map_or(Value::Null, bounded),
    );
    out.insert("newly_available".into(), json!(report.newly_available));
    if let Some(note) = &report.note {
        out.insert("note".into(), Value::String(note.clone()));
    }
    Value::Object(out).to_string()
}

// ---------------------------------------------------------------------------
// Answering them
// ---------------------------------------------------------------------------

/// What a tool call is answered with when it names nothing.
///
/// An ordinary tool result rather than an error: the model can read it and
/// ask a better question, which is the whole point of a conversation.
fn nothing_found(what: &str) -> String {
    format!("Nothing matches {what}.")
}

/// Answer one discovery tool call against the catalogue as it stands.
///
/// `revealed` grows with every address this puts in front of the model, and
/// only with those.
pub fn answer(name: &str, input: &Value, catalogue: &Catalogue, revealed: &mut Revealed) -> String {
    match name {
        LIST_APPLICATIONS => applications(catalogue, revealed),
        LIST_OPERATIONS => operations(input, catalogue, revealed),
        DESCRIBE_OPERATION => describe(input, catalogue, revealed),
        // Not reachable through a well-formed call — the API only ever names
        // a tool that was defined — and answered rather than panicked on,
        // because a name is untrusted input like everything else here.
        other => format!("There is no tool called {other}."),
    }
}

/// What is running, and what could be opened.
fn applications(catalogue: &Catalogue, revealed: &mut Revealed) -> String {
    let mut s = String::new();
    let applications = catalogue
        .offers
        .iter()
        .map(|offer| offer.app.as_str())
        .chain(catalogue.held.iter().copied())
        .chain(CONTRACTS.iter().map(|contract| contract.namespace))
        .collect::<BTreeSet<_>>();
    for application in applications {
        let contract = CONTRACTS
            .iter()
            .find(|contract| contract.namespace == application);
        let title = contract.map_or(application, |contract| contract.title);
        let open = catalogue.held.contains(&application)
            || catalogue.offers.iter().any(|offer| {
                offer.app == application && offer.descriptor.resource_kind != APPLICATION_KIND
            });
        let count = catalogue
            .offers
            .iter()
            .filter(|o| o.app == application && o.descriptor.resource_kind != APPLICATION_KIND)
            .count();
        // The operation that would open it — if the room granted one. A
        // denied open is *absent* from the catalogue, so it is absent here
        // too: the application is named, no route to opening it is given.
        let opener = catalogue
            .offers
            .iter()
            .find(|o| o.app == application && surface::opens(o.address()).is_some());

        let _ = write!(s, "{title} ({application})");
        s.push_str(if open { " — open" } else { " — not open" });
        let _ = write!(
            s,
            ", exporting {count} operation{}",
            if count == 1 { "" } else { "s" }
        );
        if count > 0 {
            let _ = write!(
                s,
                "; ask `{LIST_OPERATIONS}` with application=\"{application}\" for them"
            );
        }
        match opener {
            Some(o) => {
                revealed.reveal(o.id());
                // Two different sentences, because being here and having
                // something to work on are two different facts.
                s.push_str(if open {
                    ". Nothing is open in it, so none of what it can do is here to be \
                     named yet"
                } else {
                    ". None of what it can do is here to be named yet"
                });
                let _ = write!(
                    s,
                    " — opening it is what puts its operations on the list: `{}`",
                    o.id()
                );
                if o.wants_content() {
                    s.push_str(
                        ", which opens onto the content this room already denotes. Leave \
                         its argument null: that content is filled in from what is here",
                    );
                }
            }
            None if !open => s.push_str(", and there is no opening it from here"),
            None => {}
        }
        s.push_str(".\n");
    }
    s
}

/// Whether `asked` (an application id, its last segment, its title, or one
/// of its resources, as a model may write it) names `o`'s application — or
/// the application `o` opens, so a closed application's listing shows how
/// to open it.
fn names_application(asked: &str, o: &Offer) -> bool {
    let names = |app: &str| {
        let asked = asked.to_lowercase();
        let app_lower = app.to_lowercase();
        app_lower == asked
            || app_lower.ends_with(&format!(".{asked}"))
            || CONTRACTS
                .iter()
                .any(|c| c.namespace == app && c.title.eq_ignore_ascii_case(&asked))
    };
    names(&o.app) || o.address().resource == asked || opened_app(o).is_some_and(names)
}

/// The words a search is matched against: everything the app itself wrote.
fn haystack(o: &Offer) -> String {
    format!(
        "{} {} {} {} {}",
        o.id(),
        o.descriptor.action_label,
        o.descriptor.resource_label,
        o.descriptor.resource_kind,
        o.descriptor.description.as_deref().unwrap_or("")
    )
    .to_lowercase()
}

/// What can be done, filtered the way the caller asked.
fn operations(input: &Value, catalogue: &Catalogue, revealed: &mut Revealed) -> String {
    let field = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let application = field("application");
    let resource = field("resource");
    let kind = field("kind");
    let search = field("search").map(str::to_lowercase);

    let matching: Vec<&Offer> = catalogue
        .offers
        .iter()
        .filter(|o| application.is_none_or(|a| names_application(a, o)))
        .filter(|o| resource.is_none_or(|r| o.address().resource == r))
        .filter(|o| kind.is_none_or(|k| o.descriptor.resource_kind == k))
        .filter(|o| {
            search
                .as_deref()
                .is_none_or(|q| q.split_whitespace().all(|w| haystack(o).contains(w)))
        })
        .collect();

    if matching.is_empty() {
        let asked: Vec<String> = [
            application.map(|a| format!("application={a}")),
            resource.map(|r| format!("resource={r}")),
            kind.map(|k| format!("kind={k}")),
            search.as_deref().map(|q| format!("search={q}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        let what = if asked.is_empty() {
            "anything — this room exports nothing the agent may perform".to_string()
        } else {
            asked.join(", ")
        };
        let mut s = nothing_found(&what);
        if application.is_some() {
            let apps = catalogue
                .offers
                .iter()
                .map(|o| o.app.as_str())
                .collect::<BTreeSet<_>>();
            let _ = write!(
                s,
                " `application` is an application id: one of {}.",
                apps.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
        return s;
    }

    // Grouped by the resource that publishes them, so the reply says what
    // the operations are *on* as well as what they are, and a second,
    // narrower question is obvious.
    let mut s = String::new();
    let mut last: Option<&str> = None;
    for o in matching.iter().take(MOST_LISTED) {
        let r = o.address().resource.as_str();
        if last != Some(r) {
            let _ = writeln!(
                s,
                "\n{} ({}) in {} — resource {}",
                o.descriptor.resource_label, o.descriptor.resource_kind, o.app, r
            );
            last = Some(r);
        }
        revealed.reveal(o.id());
        let _ = write!(s, "  {} — {}", o.id(), o.descriptor.action_label);
        if let Some(app) = opened_app(o) {
            s.push_str(&open_note(catalogue, app));
        }
        if let tilo_actions::Availability::Unavailable { reason, .. } =
            &o.descriptor.contract.availability
        {
            let _ = write!(s, "; currently unavailable: {reason}");
        }
        if o.wants_content() {
            s.push_str("; takes what an earlier step produced, not words");
        } else if !o.descriptor.params.is_empty() {
            let _ = write!(s, "; parameters {}", parameter_signature(o));
        }
        s.push('\n');
    }
    let rest = matching.len().saturating_sub(MOST_LISTED);
    if rest > 0 {
        let _ = write!(
            s,
            "\n… and {rest} more not listed. Ask again with a narrower \
             application=, kind=, resource= or search= to reach them — they are \
             not out of reach, only out of this reply.\n"
        );
    }
    s
}

/// One operation, in the app's own words.
fn describe(input: &Value, catalogue: &Catalogue, revealed: &mut Revealed) -> String {
    let Some(given) = input.get("operation").and_then(Value::as_str) else {
        return format!("{DESCRIBE_OPERATION} needs an `operation`.");
    };
    let given = given.trim();
    // An id copied without its `broker:<n>:` prefix names the one offer with
    // that address, as long as only one has it.
    let offer = surface::find(&catalogue.offers, given).or_else(|| {
        let mut same = catalogue
            .offers
            .iter()
            .filter(|o| address_part(&o.id()) == address_part(given));
        match (same.next(), same.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    });
    let Some(offer) = offer else {
        // Unknown and not-offered are deliberately one answer, as they are at
        // the door: a caller must not be able to map what it may not see by
        // getting a different refusal for it.
        return nothing_found(&format!("the operation {given}"));
    };
    let id = offer.id();
    revealed.reveal(id.clone());
    let d = &offer.descriptor;
    let mut s = format!(
        "{id}\n  does: {}\n  to: {} ({})\n  in: {}\n",
        d.action_label, d.resource_label, d.resource_kind, offer.app
    );
    if let Some(description) = &d.description {
        let _ = writeln!(s, "  the application says: {description}");
    }
    let c = &d.contract;
    match &c.availability {
        tilo_actions::Availability::Available => s.push_str("  availability: available\n"),
        tilo_actions::Availability::Unavailable { code, reason } => {
            let _ = writeln!(s, "  availability: unavailable ({code}) — {reason}");
        }
    }
    let _ = writeln!(
        s,
        "  contract: permission={:?}; risk={:?}; confirmation={:?}; retry={:?}; \
         reversible={}; execution={:?}; result={:?}",
        c.permission, c.risk, c.confirmation, c.retry, c.reversible, c.execution, c.result
    );
    if !d.params.is_empty() {
        s.push_str("  structured arguments:\n");
        for p in &d.params {
            let _ = write!(
                s,
                "    {}: {}{} — {}",
                p.name,
                p.ty.label(),
                if p.required {
                    " (required)"
                } else {
                    " (optional)"
                },
                p.description
            );
            if !p.choices.is_empty() {
                let choices = p
                    .choices
                    .iter()
                    .map(std::convert::AsRef::as_ref)
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = write!(s, "; choices: {choices}");
            }
            if let Some(schema) = &p.schema {
                let _ = write!(s, "; constraints: {schema}");
            }
            s.push('\n');
        }
    }
    if d.params.is_empty() {
        s.push_str("  arguments: none. Pass {}.\n");
    } else if offer.wants_content() {
        s.push_str(
            "  arguments: produced content, not words. It must be bound from the prior step.\n",
        );
    } else {
        let example = d
            .params
            .iter()
            .filter(|p| p.required)
            .map(|p| format!("\"{}\": …", p.name))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            s,
            "  Pass one JSON object containing exactly the parameters above: \
             {{\"operation\": \"{id}\", \"parameters\": {{{example}}}}}"
        );
    }
    // What the resource says about itself. An application with a fact and no
    // operation to hang it on publishes a child resource that exports
    // nothing, and a flat survey walks straight past it.
    if let Some(facts) = catalogue.about(&d.address.resource) {
        for note in &facts.notes {
            let _ = writeln!(s, "  about {}: {note}", facts.label);
        }
    }
    s
}

// ---------------------------------------------------------------------------
// The answer
// ---------------------------------------------------------------------------

/// Read a [`PLAN`] call's input into the answer the gate checks.
///
/// `Err` names what was wrong with it, in words that go in a log line and
/// back to the model ([`plan_refused`]); an unshown id comes with the shown
/// ids it may have meant.
///
/// # Errors
///
/// Returns an error when `input` has no `steps` array, when a step names no
/// `operation`, when an `operation` is not one a listing in this
/// conversation actually returned, when `steps` leaves nothing to do, or
/// when it names more operations than `most_steps` allows.
pub fn read_plan(input: &Value, revealed: &Revealed, most_steps: usize) -> Result<Answer, String> {
    let steps = input
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| "the plan named no steps".to_string())?;

    let mut actions: Vec<String> = Vec::new();
    let mut arguments: Vec<serde_json::Map<String, Value>> = Vec::new();
    for step in steps {
        // Only strings, and never coerced. Anything else means the answer
        // did not have the shape it was asked for, and quietly stringifying
        // it would hide that.
        let Some(operation) = step.get("operation").and_then(Value::as_str) else {
            return Err("a step named no operation".to_string());
        };
        // The schema names no ids (so it stays byte-stable for the prompt
        // cache); this is the check. An id nothing in this conversation
        // showed is refused here — before the survey ever sees it — so "the
        // model picks from what it was shown" holds whatever it wrote. An id
        // copied without its routing prefix names the one shown operation
        // with that address, as for `invoke`.
        let operation = if operation == plan::NOTHING_FITS {
            operation.to_owned()
        } else {
            revealed.lookup(operation).map_err(|similar| {
                format!(
                    "named {operation:?}, which no listing in this conversation returned. {}",
                    copy_advice(&similar)
                )
            })?
        };
        let arguments_object = step
            .get("arguments")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("step {operation:?} supplied no argument object"))?;
        actions.push(operation);
        arguments.push(arguments_object.clone());
    }
    if actions.is_empty() {
        return Err("the plan named nothing to do".to_string());
    }
    // The cap, enforced rather than asked for, and refused rather than
    // truncated: half of a misread sequence is still a run of edits to
    // somebody's document that nobody asked for.
    if actions.len() > most_steps {
        return Err(format!(
            "named {} operations, more than the {most_steps} one instruction may \
             be read as asking for",
            actions.len()
        ));
    }
    Ok(Answer {
        actions,
        arguments,
        why: input
            .get("why")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(200)
            .collect(),
        // Blank is null. A model that writes `""` has said the same thing as
        // one that wrote nothing, and turning an empty string into a clause
        // would put a step in the plan whose words are no words.
        then: input
            .get("then")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| t.chars().take(240).collect()),
        reply: input
            .get("reply")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|reply| !reply.is_empty())
            .map(|reply| reply.chars().take(MAX_PLAN_REPLY_CHARS).collect()),
    })
}

/// The tool result answering a [`PLAN`] call [`read_plan`] refused: nothing
/// was performed, and the model may correct it and call [`PLAN`] again.
pub fn plan_refused(error: &str) -> String {
    format!("Plan refused, nothing was performed: {error}. Correct it and call `{PLAN}` again.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::ResourceFacts;
    use tilo_actions::door::{ActionAddress, ActionDescriptor};
    use tilo_actions::{Collects, ExecutionLocus, InputSpec};

    const STEPS: usize = 4;

    fn offer(
        app: &'static str,
        resource: &str,
        kind: &str,
        action: &str,
        label: &str,
        input: InputSpec,
    ) -> Offer {
        let params = matches!(&input, InputSpec::Text { .. })
            .then(|| {
                vec![tilo_actions::ActionParam::required(
                    "value",
                    tilo_actions::ParamType::Text,
                    "value",
                )]
            })
            .unwrap_or_default();
        Offer {
            app: app.to_string(),
            broker_peer_id: None,
            descriptor: ActionDescriptor {
                semantic_identity: None,
                address: ActionAddress::new(resource, action),
                resource_kind: kind.to_string(),
                resource_label: "a thing".to_string(),
                action_label: label.to_string(),
                description: None,
                locus: ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Document,
                contract: tilo_actions::ActionContract::default(),
                params,
                input,
                view: None,
            },
        }
    }

    /// A room with the word processor open and holding operations, plus a
    /// resource `describe_operation` has a fact but no action for.
    fn room() -> Catalogue {
        Catalogue {
            held: vec!["word-processor", "files"],
            offers: vec![
                offer(
                    "word-processor",
                    "word-processor.document",
                    "word_processor.document",
                    "insert-table",
                    "Insert table",
                    InputSpec::None,
                ),
                offer(
                    "word-processor",
                    "word-processor.document",
                    "word_processor.document",
                    "place-image",
                    "Place image",
                    InputSpec::Text {
                        prompt: "The picture to place".to_string(),
                        placeholder: String::new(),
                        collects: Collects::Content,
                    },
                ),
                offer(
                    "files",
                    "files.entry:/home/q/harbour.png",
                    "files.entry",
                    "rename",
                    "Rename",
                    InputSpec::Text {
                        prompt: "The new name".to_string(),
                        placeholder: "notes.md".to_string(),
                        collects: Collects::Words,
                    },
                ),
            ],
            facts: vec![ResourceFacts {
                id: "files.entry:/home/q/harbour.png".to_string(),
                kind: "files.entry".to_string(),
                label: "harbour.png".to_string(),
                notes: vec!["512 x 320 pixels".to_string()],
            }],
        }
    }

    /// An id copied without its `broker:<n>:` routing prefix names the one
    /// shown operation with that address; an ambiguous or unshown one is
    /// refused with the listed ids for the same action.
    #[test]
    fn an_id_without_its_routing_prefix_names_the_one_shown_operation() {
        let mut revealed = Revealed::new();
        revealed.reveal("broker:3:word-processor.document#append-paragraph");
        revealed.reveal("broker:4:chat.channel:general#post");
        assert_eq!(
            revealed.resolve("word-processor.document#append-paragraph"),
            Ok("broker:3:word-processor.document#append-paragraph".to_owned())
        );
        assert_eq!(
            revealed.resolve("broker:9:chat.channel:general#post"),
            Ok("broker:4:chat.channel:general#post".to_owned())
        );
        let unshown = revealed.resolve("word-processor.other#append-paragraph");
        let error = unshown.unwrap_err();
        assert!(error.contains("is not an operation any listing"), "{error}");
        assert!(
            error.contains("broker:3:word-processor.document#append-paragraph"),
            "suggests the shown id for the same action: {error}"
        );
        revealed.reveal("broker:5:word-processor.document#append-paragraph");
        assert!(
            revealed
                .resolve("word-processor.document#append-paragraph")
                .is_err(),
            "two peers publish that address: ambiguous"
        );
        assert!(revealed.resolve(plan::NOTHING_FITS).is_err());
    }

    /// The first turn's summary always shows how to open a closed
    /// application, and gives every application a share however many
    /// operations one of them publishes.
    #[test]
    fn the_summary_keeps_opens_and_every_application_in_view() {
        let mut catalogue = room();
        for n in 0..50 {
            catalogue.offers.push(offer(
                "word-processor",
                "word-processor.document",
                "word_processor.document",
                &format!("op-{n}"),
                "Something",
                InputSpec::None,
            ));
        }
        let mut open = offer(
            "desktop",
            "desktop:app:so.tilo.notes",
            "desktop.application",
            "open",
            "Open Notes",
            InputSpec::None,
        );
        open.descriptor.kind = tilo_actions::OperationKind::Effect;
        catalogue.offers.push(open);
        let mut revealed = Revealed::new();
        let summary = catalogue_summary(&catalogue, &mut revealed, 10);
        assert!(
            summary.contains("desktop:app:so.tilo.notes#open — Open Notes (opens so.tilo.notes"),
            "{summary}"
        );
        assert!(
            summary.contains("files.entry:/home/q/harbour.png#rename"),
            "{summary}"
        );
        assert!(summary.contains("{value: text}"), "{summary}");
        assert!(summary.contains("In word-processor:"), "{summary}");
        // A listing by the closed application's id reaches its open.
        let listed = operations(
            &json!({"application": "so.tilo.notes"}),
            &catalogue,
            &mut revealed,
        );
        assert!(
            listed.contains("desktop:app:so.tilo.notes#open"),
            "{listed}"
        );
        let none = operations(
            &json!({"application": "nowhere"}),
            &catalogue,
            &mut revealed,
        );
        assert!(
            none.contains("one of desktop, files, word-processor"),
            "{none}"
        );
    }

    /// An open next to an application that already publishes operations
    /// says so: opening again makes a second window.
    #[test]
    fn an_open_of_an_already_open_application_says_so() {
        let mut catalogue = room();
        let mut open = offer(
            "desktop",
            "desktop:app:word-processor",
            "desktop.application",
            "open",
            "Open Writer",
            InputSpec::None,
        );
        open.descriptor.kind = tilo_actions::OperationKind::Effect;
        catalogue.offers.push(open);
        let summary = catalogue_summary(&catalogue, &mut Revealed::new(), MOST_SUMMARISED);
        assert!(
            summary.contains("word-processor is already open"),
            "{summary}"
        );
    }

    /// A refused argument says what the operation takes and how to pass it.
    #[test]
    fn a_parameter_hint_names_each_parameter_and_an_example() {
        let catalogue = room();
        let rename = &catalogue.offers[2];
        let hint = parameter_hint(rename);
        assert!(hint.contains("`value`: text (required)"), "{hint}");
        assert!(hint.contains("\"parameters\": {\"value\": …}"), "{hint}");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn discovery_answers_list_and_describe_only_what_the_room_offers() {
        // Case `the_application_listing_separates_what_is_open_from_what_could_be`.
        {
            let word_processor_open = Catalogue {
                held: vec!["word-processor"],
                offers: vec![Offer {
                    app: "chat".to_string(),
                    broker_peer_id: None,
                    descriptor: ActionDescriptor {
                        semantic_identity: None,
                        address: surface::open_address("chat"),
                        resource_kind: APPLICATION_KIND.to_string(),
                        resource_label: "the conversation".to_string(),
                        action_label: "open the conversation".to_string(),
                        description: None,
                        locus: ExecutionLocus::ServerPreferred,
                        kind: tilo_actions::OperationKind::Effect,
                        contract: tilo_actions::ActionContract::default(),
                        params: Vec::new(),
                        input: InputSpec::None,
                        view: None,
                    },
                }],
                facts: vec![],
            };
            let mut revealed = Revealed::new();
            let out = applications(&word_processor_open, &mut revealed);
            assert!(
                out.contains("the word processor (word-processor) — open"),
                "{out}"
            );
            assert!(out.contains("the conversation (chat) — not open"), "{out}");
            assert!(out.contains("chat.app#open"), "{out}");

            // An app named in `CONTRACTS` but granted no route to opening it is
            // still listed, with no route — the shape a denied open takes.
            let mut revealed = Revealed::new();
            let bare = applications(&Catalogue::default(), &mut revealed);
            assert!(
                bare.contains("and there is no opening it from here"),
                "{bare}"
            );
            assert!(!bare.contains(".app#open"), "{bare}");
        }

        // Case `a_kind_is_asked_about_across_the_room_and_not_within_one_app`.
        {
            let mut revealed = Revealed::new();
            let out = operations(
                &json!({"kind": "word_processor.document"}),
                &room(),
                &mut revealed,
            );
            assert!(out.contains("insert-table"), "{out}");
            assert!(!out.contains("harbour.png#rename"), "{out}");
        }

        // Case `a_cut_listing_says_how_to_reach_what_it_left_out`.
        {
            let mut room = room();
            for i in 0..MOST_LISTED + 5 {
                room.offers.push(offer(
                    "files",
                    &format!("files.entry:/home/q/f{i}.md"),
                    "files.entry",
                    "rename",
                    "Rename",
                    InputSpec::None,
                ));
            }
            let mut revealed = Revealed::new();
            let out = operations(&json!({}), &room, &mut revealed);
            assert!(out.contains("more not listed"), "{out}");
            assert!(out.contains("narrower"), "{out}");
            // And the narrower question reaches what the wide one cut.
            let narrowed = operations(
                &json!({"resource": format!("files.entry:/home/q/f{}.md", MOST_LISTED + 4)}),
                &room,
                &mut revealed,
            );
            assert!(
                narrowed.contains(&format!("f{}.md#rename", MOST_LISTED + 4)),
                "{narrowed}"
            );
        }

        // Case `describing_an_operation_carries_its_prompt_its_example_and_the_resources_own_facts`.
        {
            let out = describe(
                &json!({"operation": "files.entry:/home/q/harbour.png#rename"}),
                &room(),
                &mut Revealed::new(),
            );
            assert!(out.contains("value: text (required)"), "{out}");
            assert!(out.contains("512 x 320 pixels"), "{out}");
        }

        // Case `an_operation_that_collects_content_is_not_asked_for_a_value`.
        {
            let out = describe(
                &json!({"operation": "word-processor.document#place-image"}),
                &room(),
                &mut Revealed::new(),
            );
            assert!(out.contains("content, not words"), "{out}");
            assert!(out.contains("bound from the prior step"), "{out}");
        }

        // Case `an_operation_that_is_not_offered_is_answered_as_though_it_did_not_exist`.
        {
            let hidden_id = "word-processor.document#imaginary-crop";
            let absent_id = "nothing.at#all";
            let hidden = describe(
                &json!({"operation": hidden_id}),
                &room(),
                &mut Revealed::new(),
            );
            let absent = describe(
                &json!({"operation": absent_id}),
                &room(),
                &mut Revealed::new(),
            );

            assert_eq!(hidden, nothing_found(&format!("the operation {hidden_id}")));
            assert_eq!(absent, nothing_found(&format!("the operation {absent_id}")));
            assert_eq!(
                hidden.replace(hidden_id, "<id>"),
                absent.replace(absent_id, "<id>"),
                "the two must be indistinguishable once each stops quoting the \
                 caller's own words back at it"
            );
            for reply in [&hidden, &absent] {
                let lower = reply.to_lowercase();
                for leak in [
                    "permission",
                    "denied",
                    "allowed",
                    "not for you",
                    "capabilit",
                ] {
                    assert!(!lower.contains(leak), "{reply} leaks {leak:?}");
                }
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn a_plan_is_read_only_from_revealed_operations_within_the_step_cap() {
        // Case `only_what_a_listing_returned_can_be_planned`.
        {
            let mut revealed = Revealed::new();
            assert!(!revealed.contains("files.entry:/home/q/harbour.png#rename"));
            // The editorial changes and "none" are the agent's own and are there
            // from the start; nothing an application exports is.
            assert!(revealed.contains(plan::NOTHING_FITS));

            let call = json!({
                "steps": [{"operation": "files.entry:/home/q/harbour.png#rename", "arguments": {"value": "a.png"}}],
                "why": "", "then": null
            });
            let refused = read_plan(&call, &revealed, STEPS).expect_err("nothing revealed it yet");
            assert!(
                refused.contains("no listing in this conversation returned"),
                "{refused}"
            );

            let listed = operations(&json!({"kind": "files.entry"}), &room(), &mut revealed);
            assert!(
                listed.contains("files.entry:/home/q/harbour.png#rename"),
                "{listed}"
            );
            let answer = read_plan(&call, &revealed, STEPS).expect("now it has been shown");
            assert_eq!(
                answer.arguments,
                vec![serde_json::Map::from_iter([(
                    "value".into(),
                    Value::String("a.png".into())
                )])]
            );
        }

        // Case `one_published_operation_can_be_planned_repeatedly_with_distinct_arguments`.
        {
            let mut revealed = Revealed::new();
            let _ = operations(&json!({"kind": "files.entry"}), &room(), &mut revealed);
            let operation = "files.entry:/home/q/harbour.png#rename";
            let call = json!({
                "steps": [
                    {"operation": operation, "arguments": {"value": "first.png"}},
                    {"operation": operation, "arguments": {"value": "second.png"}}
                ],
                "why": "the same canonical operation applies to two requested values",
                "then": null
            });
            let answer = read_plan(&call, &revealed, STEPS).expect("both uses were listed");
            assert_eq!(answer.actions, vec![operation, operation]);
            assert_ne!(answer.arguments.first(), answer.arguments.last());
        }

        // Case `a_plan_longer_than_a_sentence_is_refused_rather_than_truncated`.
        {
            let mut revealed = Revealed::new();
            revealed.reveal("a#b");
            let steps: Vec<Value> = (0..=STEPS)
                .map(|_| json!({"operation": "a#b", "arguments": {}}))
                .collect();
            let err = read_plan(
                &json!({"steps": steps, "why": "", "then": null}),
                &revealed,
                STEPS,
            )
            .expect_err("over the cap");
            assert!(err.contains("more than the"), "{err}");
        }

        // Case `what_the_room_cannot_name_yet_is_carried_as_words`.
        {
            let mut revealed = Revealed::new();
            revealed.reveal("chat.app#open");
            let answer = read_plan(
                &json!({
                    "steps": [{"operation": "chat.app#open", "arguments": {}}],
                    "why": "the conversation is not open",
                    "then": "post it there"
                }),
                &revealed,
                STEPS,
            )
            .expect("read");
            assert_eq!(answer.then.as_deref(), Some("post it there"));
            // A blank is nothing, not a clause whose words are no words.
            let blank = read_plan(
                &json!({
                    "steps": [{"operation": "chat.app#open", "arguments": {}}],
                    "why": "", "then": "   "
                }),
                &revealed,
                STEPS,
            )
            .expect("read");
            assert_eq!(blank.then, None);
        }

        // Case `a_plan_naming_an_address_never_revealed_is_refused_not_attempted`.
        {
            let mut revealed = Revealed::new();
            // Revealed by one call, so the check below is genuinely about the
            // *other* address rather than an empty `Revealed` refusing anything.
            revealed.reveal("word-processor.document#insert-table");

            let call = json!({
                "steps": [{"operation": "files.entry:/home/q/harbour.png#rename", "arguments": {"value": "a.png"}}],
                "why": "", "then": null
            });
            let err = read_plan(&call, &revealed, STEPS).expect_err("never revealed");
            assert!(
                err.contains("no listing in this conversation returned"),
                "{err}"
            );
            assert!(
                err.contains("files.entry:/home/q/harbour.png#rename"),
                "{err}"
            );
        }
    }

    /// `reply` is optional, first in the schema, and kept (trimmed, bounded)
    /// when the model writes one.
    #[test]
    fn a_plan_reply_is_optional_first_and_kept() {
        let definition = plan_definition(STEPS);
        let schema = &definition["input_schema"];
        let first = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        assert_eq!(first, "reply");
        assert_eq!(schema["properties"]["reply"]["type"], "string");
        assert_eq!(
            schema["properties"]["reply"]["description"],
            PLAN_REPLY_DESCRIPTION
        );
        assert_eq!(schema["required"], json!(["steps", "why", "then"]));

        let mut revealed = Revealed::new();
        revealed.reveal("a#b");
        let step = json!([{"operation": "a#b", "arguments": {}}]);
        let with = read_plan(
            &json!({"reply": "  Sure, writing it now.  ", "steps": step, "why": "", "then": null}),
            &revealed,
            STEPS,
        )
        .expect("read");
        assert_eq!(with.reply.as_deref(), Some("Sure, writing it now."));
        let without = read_plan(
            &json!({"steps": step, "why": "", "then": null}),
            &revealed,
            STEPS,
        )
        .expect("read");
        assert_eq!(without.reply, None);
        let long = "x".repeat(MAX_PLAN_REPLY_CHARS + 50);
        let bounded = read_plan(
            &json!({"reply": long, "steps": step, "why": "", "then": null}),
            &revealed,
            STEPS,
        )
        .expect("read");
        assert_eq!(bounded.reply.unwrap().chars().count(), MAX_PLAN_REPLY_CHARS);
    }

    /// The plan tool set names no ids, so it is the same bytes on every turn
    /// however much the conversation has revealed; what the first turn's
    /// summary showed can be planned with no listing, a prefix-less copy
    /// names the one shown operation, and anything else is refused with
    /// what to copy instead.
    #[test]
    fn the_plan_tool_set_is_byte_stable_and_a_plan_names_what_was_shown() {
        let before = serde_json::to_string(&definitions(STEPS)).unwrap();
        let mut revealed = Revealed::new();
        let summary = catalogue_summary(&room(), &mut revealed, MOST_SUMMARISED);
        let _ = operations(&json!({}), &room(), &mut revealed);
        assert_eq!(
            serde_json::to_string(&definitions(STEPS)).unwrap(),
            before,
            "revealing operations must not change the tool bytes"
        );
        let set = definitions(STEPS);
        let plan = &set[3];
        assert_eq!(plan["name"], PLAN);
        let operation =
            &plan["input_schema"]["properties"]["steps"]["items"]["properties"]["operation"];
        assert_eq!(operation["type"], "string");
        assert!(operation.get("enum").is_none(), "{operation}");
        assert!(
            !before.contains("harbour.png"),
            "no conversation ids in the tools"
        );

        // Shown by the summary alone: planned with no discovery call.
        assert!(
            summary.contains("files.entry:/home/q/harbour.png#rename"),
            "{summary}"
        );
        let mut first_turn = Revealed::new();
        let _ = catalogue_summary(&room(), &mut first_turn, MOST_SUMMARISED);
        let answer = read_plan(
            &json!({
                "steps": [{"operation": "files.entry:/home/q/harbour.png#rename", "arguments": {"value": "a.png"}}],
                "why": "", "then": null
            }),
            &first_turn,
            STEPS,
        )
        .expect("the summary showed it");
        assert_eq!(answer.actions, ["files.entry:/home/q/harbour.png#rename"]);
        assert_eq!(
            read_plan(
                &json!({"steps": [{"operation": plan::NOTHING_FITS, "arguments": {}}], "why": "", "then": null}),
                &first_turn,
                STEPS,
            )
            .expect("none is the agent's own")
            .actions,
            [plan::NOTHING_FITS]
        );

        // A prefix-less copy of a routed id names the one shown operation.
        let mut routed = Revealed::new();
        routed.reveal("broker:3:word-processor.document#insert-table");
        let answer = read_plan(
            &json!({"steps": [{"operation": "word-processor.document#insert-table", "arguments": {}}], "why": "", "then": null}),
            &routed,
            STEPS,
        )
        .expect("one shown operation has that address");
        assert_eq!(
            answer.actions,
            ["broker:3:word-processor.document#insert-table"]
        );

        // An unshown id is refused with what to do instead, in words the
        // model can act on.
        let refused = read_plan(
            &json!({"steps": [{"operation": "word-processor.other#insert-table", "arguments": {}}], "why": "", "then": null}),
            &routed,
            STEPS,
        )
        .expect_err("never shown");
        assert!(
            refused.contains("no listing in this conversation returned"),
            "{refused}"
        );
        assert!(refused.contains(LIST_OPERATIONS), "{refused}");
        assert!(
            refused.contains("broker:3:word-processor.document#insert-table"),
            "suggests the shown id for the same action: {refused}"
        );
        let answered = plan_refused(&refused);
        assert!(answered.contains(&refused), "{answered}");
        assert!(answered.contains("nothing was performed"), "{answered}");
    }

    /// The loop offers only the generic tools; `invoke` can name only what a
    /// tool showed, and its operation comes before its arguments so text is
    /// attributable while it streams.
    #[test]
    fn the_loop_tool_set_is_generic_and_invoke_names_only_what_was_shown() {
        let mut revealed = Revealed::new();
        let summary = catalogue_summary(&room(), &mut revealed, MOST_SUMMARISED);
        assert!(
            summary.contains("word-processor.document#insert-table"),
            "{summary}"
        );
        assert!(summary.contains("{value: text}"), "{summary}");
        let definitions = loop_definitions();
        assert_eq!(
            definitions,
            loop_definitions(),
            "byte-stable for prompt caching"
        );
        let names: Vec<&str> = definitions
            .as_array()
            .unwrap()
            .iter()
            .map(|definition| definition["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                LIST_APPLICATIONS,
                LIST_OPERATIONS,
                DESCRIBE_OPERATION,
                INVOKE,
                PLAN
            ]
        );
        let invoke = &definitions[3]["input_schema"];
        // Sorted maps on the wire: the operation still comes first.
        let keys: Vec<&String> = invoke["properties"].as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["operation", INVOKE_PARAMETERS, PARTS, INVOKE_REPLY, UNABLE]
        );
        let wire = serde_json::to_string(invoke).unwrap();
        assert!(wire.find("\"operation\"") < wire.find(&format!("\"{INVOKE_PARAMETERS}\"")));
        assert!(invoke["properties"]["operation"].get("enum").is_none());

        let ok = read_invoke(
            &json!({"operation": "files.entry:/home/q/harbour.png#rename", "parameters": {"value": "a.png"}}),
            &revealed,
        )
        .expect("shown in the summary");
        assert_eq!(ok.arguments["value"], "a.png");
        let refused = read_invoke(&json!({"operation": "x#y", "arguments": {}}), &revealed)
            .expect_err("never shown");
        assert!(refused.contains("list_operations"), "{refused}");
        assert!(
            read_invoke(
                &json!({"operation": "files.entry:/home/q/harbour.png#rename", "arguments": 3}),
                &revealed
            )
            .is_err()
        );
        assert_eq!(
            read_finish(&json!({"reply": "  Done.  ", "done": false})),
            Finish {
                reply: Some("Done.".into()),
                done: false
            }
        );
    }

    /// An observation is compact JSON and bounded.
    #[test]
    fn an_invoke_observation_is_compact_and_bounded() {
        let report = InvokeReport {
            succeeded: true,
            value: Some(Value::String("x".repeat(MOST_RESULT_CHARS * 2))),
            changed_view: Some(json!({"caret": {"block": "b1", "offset": 3}})),
            newly_available: vec!["a#b — B".into()],
            ..InvokeReport::default()
        };
        let text = invoke_result(&report);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["result"], "succeeded");
        assert_eq!(value["changed_view"]["caret"]["offset"], 3);
        assert_eq!(value["newly_available"][0], "a#b — B");
        assert!(text.len() < MOST_RESULT_CHARS + 400, "{}", text.len());
        let skipped = invoke_result(&InvokeReport {
            skipped: Some("an earlier invoke failed".into()),
            ..InvokeReport::default()
        });
        assert_eq!(
            serde_json::from_str::<Value>(&skipped).unwrap()["result"],
            "not run"
        );
    }
}
