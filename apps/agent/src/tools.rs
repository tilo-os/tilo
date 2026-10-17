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
//! | [`READ_RESULT`](crate::pages::READ_RESULT) | read on in a result too long for one observation |
//! | [`PLAN`] | the answer: the steps to perform in order (plan mode), or the reply that finishes the task (loop mode) |
//!
//! Two tool sets are built from these. The *plan* set ([`definitions`]) is
//! discovery plus a `plan` naming every step; the caller dispatches the steps
//! afterwards. The *loop* set ([`loop_definitions`]) is discovery plus
//! [`INVOKE`], paging through long results, and a `plan` that only carries
//! the reply: the agent
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

use agent_provider_config::protocol::{ToolCall, ToolDefinition};
use serde_json::{Value, json};

use crate::plan;
use crate::surface::{self, APPLICATION_KIND, Catalogue, Offer};

pub const LIST_APPLICATIONS: &str = "list_applications";
pub const LIST_OPERATIONS: &str = "list_operations";
pub const DESCRIBE_OPERATION: &str = "describe_operation";
pub const INVOKE: &str = "invoke";
/// The [`INVOKE`] property holding the argument object.
pub const INVOKE_PARAMETERS: &str = "parameters";
/// The optional [`INVOKE`] property carrying the task's final reply.
pub const INVOKE_REPLY: &str = "reply";
/// The optional [`INVOKE`] property by which the model says a step stands
/// alone: it neither needs nor reports what the steps before it in the turn
/// do, so it may run alongside them. Named to sort after `operation`, which
/// stays the first property on the wire.
pub const INVOKE_STANDALONE: &str = "standalone";
pub const PLAN: &str = "plan";

/// Whether an [`INVOKE`] call declared itself independent of the steps
/// before it ([`INVOKE_STANDALONE`]). Anything but an explicit `true` keeps
/// it in order.
pub fn invoke_independent(input: &Value) -> bool {
    input.get(INVOKE_STANDALONE).and_then(Value::as_bool) == Some(true)
}

/// Validate a whole plan-mode batch before any discovery or control dispatch.
/// A terminal plan must be alone: accepting the first plan in a mixed batch
/// would silently discard calls and change the model's declared order.
///
/// # Errors
///
/// Rejects malformed or duplicate calls, invalid discovery arguments, unknown
/// planning tools, and batches mixing a terminal plan with other calls.
pub fn validate_plan_calls(calls: &[ToolCall]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for call in calls {
        call.validate().map_err(|error| error.to_string())?;
        if !ids.insert(&call.id) {
            return Err("duplicate tool call id".into());
        }
        validate_discovery_input(&call.name, &call.input)?;
        if !matches!(
            call.name.as_str(),
            LIST_APPLICATIONS | LIST_OPERATIONS | DESCRIBE_OPERATION | PLAN
        ) {
            return Err(format!("There is no planning tool called {}.", call.name));
        }
    }
    if calls.len() > 1 && calls.iter().any(|call| call.name == PLAN) {
        return Err("plan must be the only tool call in its turn".into());
    }
    Ok(())
}

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

/// Read the object and array arguments of `offer` that a model wrote as
/// JSON text back into objects and arrays. The invoke tool takes its
/// parameters as an open object, so a model cannot see that a parameter
/// (a selection's place, say) is an object, and some (Gemini) then pass it
/// as its JSON encoding. Text that is not the declared kind of JSON stays
/// as it is, to be refused with what the parameter takes. A place
/// parameter whose fields were written beside it instead of inside it
/// (`"item"`, `"start"`, `"end"` next to the other arguments) is gathered
/// back into the place.
pub fn decode_structured_arguments(offer: &Offer, arguments: &mut serde_json::Map<String, Value>) {
    gather_flat_place(offer, arguments);
    for param in &offer.descriptor.params {
        let Some(Value::String(text)) = arguments.get(param.name.as_ref()) else {
            continue;
        };
        let ((tilo_actions::ParamType::Object, Ok(decoded @ Value::Object(_)))
        | (tilo_actions::ParamType::Array, Ok(decoded @ Value::Array(_)))) =
            (param.ty, serde_json::from_str::<Value>(text.trim()))
        else {
            continue;
        };
        arguments.insert(param.name.to_string(), decoded);
    }
}

/// The place parameter of `offer` rebuilt from its fields written at the
/// top level: `item` with `start` and `end`, or `item` with the place's own
/// name holding an insertion offset. Only names the operation does not
/// declare are taken, and only while the place itself is not an object.
fn gather_flat_place(offer: &Offer, arguments: &mut serde_json::Map<String, Value>) {
    let params = &offer.descriptor.params;
    let declared = |name: &str| params.iter().any(|param| param.name.as_ref() == name);
    let Some(place) = params.iter().find(|param| param.is_place()) else {
        return;
    };
    let name = place.name.as_ref();
    if arguments.get(name).is_some_and(Value::is_object)
        || declared("item")
        || !arguments.get("item").is_some_and(Value::is_string)
    {
        return;
    }
    let mut gathered = serde_json::Map::new();
    for field in ["item", "start", "end"] {
        if !declared(field)
            && let Some(value) = arguments.remove(field)
        {
            gathered.insert(field.to_owned(), value);
        }
    }
    let offset = arguments.remove(name).filter(Value::is_u64);
    if !gathered.contains_key("start")
        && let Some(offset) = offset
    {
        gathered.insert("at".to_owned(), offset);
    }
    arguments.insert(name.to_owned(), Value::Object(gathered));
}

/// One operation's parameters as a compact signature and how to pass them,
/// for an invoke that was refused for its arguments.
pub fn parameter_hint(offer: &Offer) -> String {
    if takes_only_content(offer) {
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
        .filter(|p| p.required && !p.is_content())
        .map(|p| format!("\"{}\": …", p.name))
        .collect::<Vec<_>>()
        .join(", ");
    let carried = offer.content_param().map_or_else(String::new, |param| {
        format!(" `{param}` is filled in from what the step before it produced.")
    });
    format!(
        "{} takes: {params}. Pass them inside `parameters`, e.g. \"parameters\": {{{example}}}.{carried}",
        offer.descriptor.action_label
    )
}

/// Whether `offer`'s one input is content an earlier step produced, so it
/// takes no words at all.
fn takes_only_content(offer: &Offer) -> bool {
    offer.descriptor.input.wants_content()
        || (offer.wants_content()
            && offer
                .descriptor
                .params
                .iter()
                .all(tilo_actions::ActionParam::is_content))
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
pub const PLAN_REPLY_DESCRIPTION: &str = "Your answer to the person who asked, written as you \
     would answer them in chat: one or two sentences after doing something, the whole answer \
     when they asked a question (an explanation, a translation, a summary for them).";

/// The most characters of a plan's `reply` that are kept: room for a
/// paragraph-length answer to a question about a selection.
pub const MAX_PLAN_REPLY_CHARS: usize = 4000;

/// The most bytes of a reply, under the task outcome's retained-text bound
/// (`tilo_desktop_model::MAX_WORKSPACE_TASK_SUMMARY_BYTES`) with room to
/// spare, so a reply in a script that takes several bytes a character is
/// never refused for its length.
pub const MAX_REPLY_BYTES: usize = 6 * 1024;

/// `reply` as it is kept: trimmed, at most [`MAX_PLAN_REPLY_CHARS`]
/// characters and [`MAX_REPLY_BYTES`] bytes, cut at a character boundary.
pub fn bound_reply(reply: &str) -> String {
    let mut kept = String::new();
    for character in reply.trim().chars().take(MAX_PLAN_REPLY_CHARS) {
        if kept.len() + character.len_utf8() > MAX_REPLY_BYTES {
            break;
        }
        kept.push(character);
    }
    kept
}

// ---------------------------------------------------------------------------
// The definitions
// ---------------------------------------------------------------------------

/// The [`LIST_APPLICATIONS`] tool definition.
fn list_applications_definition() -> ToolDefinition {
    ToolDefinition {
        name: LIST_APPLICATIONS.into(),
        description: ("Every application in this room. Says which are open and which are not, \
             and how many operations each is exporting right now. An application \
             exporting nothing has nothing open in it yet — opening it onto \
             something is an ordinary step, and what it does is put that \
             application's own operations on the list. Call this first when the \
             request might involve an application you have not seen.")
            .into(),
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    }
}

/// The [`LIST_OPERATIONS`] tool definition.
fn list_operations_definition() -> ToolDefinition {
    ToolDefinition {
        name: LIST_OPERATIONS.into(),
        description: ("What can be done in this room, right now. Every filter is optional and \
             they combine; with none of them you get everything, capped. Use `kind` \
             to ask what may be done to a kind of thing in any application at all \
             (`files.entry`, `word_processor.section`), `resource` for one named \
             thing, `application` for one application, and `search` to match the \
             applications' own words. The ids that come back, like those already \
             listed with the request, are the ones `plan` and `invoke` accept.")
            .into(),
        input_schema: json!({
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
        }),
    }
}

/// The [`DESCRIBE_OPERATION`] tool definition.
fn describe_operation_definition() -> ToolDefinition {
    ToolDefinition {
        name: DESCRIBE_OPERATION.into(),
        description: ("One operation in full: what the application says it does, what it does \
             it to, whether performing it needs an argument, and — when it does — \
             the application's own prompt for that argument and its own example of \
             one. Also what the publishing resource says about itself, which is \
             where an application states a fact it has no operation to hang on. \
             Call this before planning an operation whose arguments its listed \
             signature leaves unclear — the example and the facts are what make the \
             value computable rather than guessed.")
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "The id of an operation a listing returned."
                }
            },
            "required": ["operation"],
            "additionalProperties": false
        }),
    }
}

/// The [`PLAN`] tool definition. Byte-stable across turns and tasks (no
/// per-conversation enum), so the provider's prompt cache keeps the whole
/// tool prefix; [`read_plan`] refuses an id nothing in the conversation
/// showed, and the refusal goes back to the model.
fn plan_definition(most_steps: usize) -> ToolDefinition {
    ToolDefinition {
        name: PLAN.into(),
        description: format!(
            "The answer. The operations to perform, in the order the request asks \
             for them — one for a request that asks for one thing, which is most of \
             them, and at most {most_steps}. Call this exactly once, when you know \
             what to do — straight away when the operations listed with the request \
             already do it. Every `operation` must be an id exactly as that list or a \
             listing showed it; any other id is refused."
        ),
        input_schema: json!({
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
                                "description": ARGUMENTS_DESCRIPTION,
                                // Open by design; see `invoke_definition`.
                                "additionalProperties": true
                            }
                        },
                        "required": ["operation", "arguments"],
                        "additionalProperties": false
                    }
                },
                "why": { "type": "string", "description": "One short clause." },
                // A plain string, not a nullable union: every provider's
                // schema dialect accepts it (Gemini's refuses `anyOf` with
                // `null` in places), and an empty string reads as none.
                "then": {
                    "type": "string",
                    "description":
                        "What the request still asks for that nothing you were shown can \
                         express yet, in the request's own words — the rest of the \
                         sentence, after the steps you named. Use it when opening an \
                         application is what would put the needed operation on the list, \
                         since that operation cannot be named until it is open. Empty when \
                         the steps you named already do all of it."
                }
            },
            "required": ["steps", "why", "then"],
            "additionalProperties": false
        }),
    }
}

/// The plan tool set as the API is told about it. A function of `most_steps`
/// alone, so it is byte-identical on every turn of a conversation.
pub fn definitions(most_steps: usize) -> Value {
    json!(typed_definitions(most_steps))
}

/// Provider-neutral, byte-stable plan schemas.
pub fn typed_definitions(most_steps: usize) -> Vec<ToolDefinition> {
    vec![
        list_applications_definition(),
        list_operations_definition(),
        describe_operation_definition(),
        plan_definition(most_steps),
    ]
}

/// The mark of an operation the person is asked about before it runs: its
/// contract confirms (it sends, publishes, deletes for good), or the
/// person's answers for this agent ask first. `None` for one that runs at
/// once.
pub(crate) fn confirmation_note(offer: &Offer) -> Option<&'static str> {
    (offer.descriptor.contract.advertised_confirmation() != tilo_actions::ConfirmationPolicy::None)
        .then_some("needs confirmation")
}

/// The description shared by `plan` steps and `invoke` of an argument object.
const ARGUMENTS_DESCRIPTION: &str = "The complete JSON argument object described by \
    `describe_operation`, keyed by the application's parameter names. Use {} for a nullary \
    operation. Values retain their declared JSON types; never stringify numbers, booleans, \
    arrays, objects or null. Never add `confirm` or `confirmation_phrase`: an operation marked \
    needs confirmation asks the person when you invoke it, and only they answer.";

/// The [`INVOKE`] tool definition.
fn invoke_definition() -> ToolDefinition {
    // Byte-stable across turns and tasks (no per-conversation enum), so the
    // provider's prompt cache keeps the whole tool prefix; `read_invoke`
    // refuses an id no tool showed.
    ToolDefinition {
        name: INVOKE.into(),
        description: ("Perform one listed operation now, as a person using the application would. \
             Returns the result, how your own view changed and which operations became \
             available. Several invokes in one turn run in order as soon as each is \
             written; after a failure the rest of the turn is skipped, except invokes \
             marked `standalone`, which may also run alongside the ones before them. \
             Put your reply on the last invoke of the task to finish in the same turn.")
            .into(),
        input_schema: json!({
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
                    "description": ARGUMENTS_DESCRIPTION,
                    // Each operation names its own arguments, so this object
                    // is open; saying so explicitly matters because Gemini
                    // reads an object schema without properties as an empty
                    // object and would then never pass a required argument.
                    "additionalProperties": true
                },
                PARTS: parts_schema(),
                INVOKE_REPLY: {
                    "type": "string",
                    "description":
                        "Only on the last invoke of the task: your short reply to the person \
                         (shown in your agent panel only). The task finishes when this invoke \
                         succeeds and every part is done; no plan call is needed."
                },
                INVOKE_STANDALONE: {
                    "type": "boolean",
                    "description":
                        "true only when this step stands alone: it neither needs nor reports \
                         the result of the steps before it in this turn (for example two \
                         unrelated messages in different applications); it may then run \
                         alongside them."
                },
                UNABLE: unable_schema()
            },
            "required": ["operation", INVOKE_PARAMETERS],
            "additionalProperties": false
        }),
    }
}

/// The loop's terminal [`PLAN`]: the reply that finishes the task.
fn finish_definition() -> ToolDefinition {
    ToolDefinition {
        name: PLAN.into(),
        description: ("Finish the task with your reply to the person (shown in your agent panel \
             only) when no invoke carried it: alone, or in the same turn as your last \
             invokes (it takes effect only if they all succeed and every part is done). \
             `done` is false only when the request cannot be completed with the \
             operations available; say why in the reply and list those parts under \
             `unable`.")
            .into(),
        input_schema: json!({
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
        }),
    }
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
    json!(typed_loop_definitions())
}

/// Provider-neutral loop schemas; the legacy JSON API is an adapter only.
pub fn typed_loop_definitions() -> Vec<ToolDefinition> {
    vec![
        list_applications_definition(),
        list_operations_definition(),
        describe_operation_definition(),
        invoke_definition(),
        crate::pages::read_result_definition(),
        finish_definition(),
    ]
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
            .map(bound_reply),
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
            .map(bound_reply),
    })
}

/// How many operations the first turn's catalogue shows.
pub const MOST_SUMMARISED: usize = 60;

/// An operation's parameters as a compact signature (`{text: text, n?:
/// integer [a|b]}`), or what it takes instead; empty for none.
pub fn parameter_signature(offer: &Offer) -> String {
    if takes_only_content(offer) {
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
            if param.is_content() {
                return format!("{}: what the previous step produced", param.name);
            }
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

/// The prefix of a host (non-Tilo) application's id in the catalogue.
const HOST_APP_PREFIX: &str = "host:";

/// The Tilo applications `request` (lowercased) names by the title their
/// open publishes ("Writer"), with that title: each needs a successful
/// operation in it before the task is done. A host application is never
/// one of them: opening it is all an agent can do there, and its title is
/// often an everyday word ("Web", "Files") the request uses for something
/// else.
pub fn named_applications<'a>(catalogue: &'a Catalogue, request: &str) -> Vec<(&'a str, &'a str)> {
    catalogue
        .offers
        .iter()
        .filter_map(|offer| {
            opened_app(offer).map(|app| (app, offer.descriptor.resource_label.as_str()))
        })
        .filter(|(app, _)| !app.starts_with(HOST_APP_PREFIX))
        .filter(|(_, title)| {
            !title.is_empty()
                && if title.contains(' ') {
                    request.contains(&title.to_lowercase())
                } else {
                    request
                        .split(|c: char| !c.is_alphanumeric())
                        .any(|word| word.eq_ignore_ascii_case(title))
                }
        })
        .collect()
}

/// The published operations, compactly, for the first turn (of the task loop
/// and of a plan decision): enough to invoke or plan the common ones directly
/// without a discovery round trip. Every id shown is revealed. Every
/// running application gets its share (document operations first, then
/// view, then the rest) of the first half; the opens — how a closed
/// application is started — follow, Tilo's applications first; a cut
/// summary says how to reach what it left out.
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
    // Round-robin over the applications' own operations (opens aside),
    // `depth` deep so far, until `shown` holds `until`.
    let shown = summarised(catalogue, &per_app, most);
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
            if let Some(note) = confirmation_note(offer) {
                let _ = write!(s, " ({note})");
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
             search) to reach them, or `{LIST_APPLICATIONS}` for the applications there are."
        );
    }
    s
}

/// The offers [`catalogue_summary`] shows, at most `most`: the running
/// applications' own operations take the first half, so the opens of
/// everything installed (a desktop lists every host application too) cannot
/// crowd them out; the opens follow, Tilo's applications before the host's,
/// whose windows publish nothing once open; what is left goes to more of
/// the running applications.
fn summarised<'a>(
    catalogue: &'a Catalogue,
    per_app: &std::collections::BTreeMap<&str, Vec<&'a Offer>>,
    most: usize,
) -> Vec<&'a Offer> {
    fn fill<'a>(
        per_app: &std::collections::BTreeMap<&str, Vec<&'a Offer>>,
        shown: &mut Vec<&'a Offer>,
        depth: &mut usize,
        until: usize,
    ) {
        while shown.len() < until {
            let mut any = false;
            for offers in per_app.values() {
                if let Some(offer) = offers.get(*depth) {
                    any = true;
                    if opened_app(offer).is_none() && shown.len() < until {
                        shown.push(offer);
                    }
                }
            }
            if !any {
                break;
            }
            *depth += 1;
        }
    }
    let mut shown: Vec<&'a Offer> = Vec::new();
    let mut depth = 0;
    fill(per_app, &mut shown, &mut depth, most.div_ceil(2));
    let mut opens: Vec<&Offer> = catalogue
        .offers
        .iter()
        .filter(|offer| opened_app(offer).is_some())
        .collect();
    opens
        .sort_by_key(|offer| opened_app(offer).is_some_and(|app| app.starts_with(HOST_APP_PREFIX)));
    let room = most.saturating_sub(shown.len());
    shown.extend(opens.into_iter().take(room));
    fill(per_app, &mut shown, &mut depth, most);
    shown
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

/// The most characters of a changed view shown to the model. A returned
/// value is never cut: a long one is shown a page at a time
/// ([`crate::pages`]).
pub const MOST_RESULT_CHARS: usize = 2_000;

/// A compact JSON observation for one [`INVOKE`]. A value too long for one
/// observation is kept in `pages`, its first page shown and `continues`
/// saying how to read on.
pub fn invoke_result(report: &InvokeReport, pages: &mut crate::pages::Pages) -> String {
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
        let (page, continues) = pages.first_page(value);
        out.insert("value".into(), page);
        if let Some(continues) = continues {
            out.insert("continues".into(), continues);
        }
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
    if let Err(error) = validate_discovery_input(name, input) {
        return error;
    }
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

/// One structural gate for discovery, regardless of transport or dispatcher.
fn validate_discovery_input(name: &str, input: &Value) -> Result<(), String> {
    let definition = match name {
        LIST_APPLICATIONS => list_applications_definition(),
        LIST_OPERATIONS => list_operations_definition(),
        DESCRIBE_OPERATION => describe_operation_definition(),
        _ => return Ok(()),
    };
    definition
        .validate_input(input)
        .map_err(|error| error.to_string())
}

/// Validate a whole task-loop batch before any action runs. Terminal `plan`
/// may follow invokes, but no call after it may be silently discarded.
///
/// # Errors
///
/// Rejects malformed or duplicate calls, invalid tool arguments, unknown tools,
/// and batches containing calls after a terminal plan.
pub fn validate_loop_calls(calls: &[ToolCall]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for (index, call) in calls.iter().enumerate() {
        call.validate().map_err(|error| error.to_string())?;
        if !ids.insert(&call.id) {
            return Err("duplicate tool call id".into());
        }
        validate_discovery_input(&call.name, &call.input)?;
        match call.name.as_str() {
            LIST_APPLICATIONS | LIST_OPERATIONS | DESCRIBE_OPERATION => {}
            INVOKE => {
                // Preserve the documented legacy alias and nullary form.
                let mut input = call.input.clone();
                let object = input.as_object_mut().ok_or("invoke needs an object")?;
                let arguments = object.remove("arguments");
                let parameters = object
                    .entry(INVOKE_PARAMETERS)
                    .or_insert_with(|| arguments.unwrap_or_else(|| json!({})));
                if parameters.is_null() {
                    *parameters = json!({});
                }
                invoke_definition()
                    .validate_input(&input)
                    .map_err(|error| error.to_string())?;
            }
            PLAN if index + 1 == calls.len() => {}
            PLAN => return Err("plan must be the last call in its turn".into()),
            _ => return Err(format!("There is no tool called {}.", call.name)),
        }
    }
    Ok(())
}

/// What is running, and what could be opened.
fn applications(catalogue: &Catalogue, revealed: &mut Revealed) -> String {
    let mut s = String::new();
    let applications = catalogue
        .offers
        .iter()
        .map(|offer| offer.app.as_str())
        .chain(catalogue.held.iter().copied())
        .collect::<BTreeSet<_>>();
    for application in applications {
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

        let _ = write!(s, "{application}");
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
        app_lower == asked || app_lower.ends_with(&format!(".{asked}"))
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
        if let Some(note) = confirmation_note(o) {
            let _ = write!(s, "; {note}");
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
        "  contract: kind={:?}; risk={:?}; confirmation={:?}; retry={:?}; \
         reversible={}; execution={:?}; result={:?}",
        c.kind, c.risk, c.confirmation, c.retry, c.reversible, c.execution, c.result
    );
    if let Some(note) = confirmation_note(offer) {
        let _ = writeln!(s, "  {note}: your step waits until the person answers it");
    }
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
    describe_arguments(&mut s, offer, &id);
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

/// How to pass `offer`'s arguments, for [`describe`].
fn describe_arguments(s: &mut String, offer: &Offer, id: &str) {
    let d = &offer.descriptor;
    if d.params.is_empty() {
        s.push_str("  arguments: none. Pass {}.\n");
    } else if takes_only_content(offer) {
        s.push_str(
            "  arguments: produced content, not words. It must be bound from the prior step.\n",
        );
    } else {
        if let Some(param) = offer.content_param() {
            let _ = writeln!(
                s,
                "  `{param}` is content, not words: invoke this right after the step that \
                 produced it (a file handed over, a picture made), and it is filled in from \
                 that step."
            );
        }
        let example = d
            .params
            .iter()
            .filter(|p| p.required && !p.is_content())
            .map(|p| format!("\"{}\": …", p.name))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            s,
            "  Pass one JSON object containing exactly the parameters above: \
             {{\"operation\": \"{id}\", \"parameters\": {{{example}}}}}"
        );
    }
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
            .map(bound_reply),
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

    #[test]
    fn typed_tools_are_stable_and_validate_whole_batches() {
        let definitions = typed_definitions(STEPS);
        assert_eq!(definitions[3].name, PLAN);
        assert!(
            definitions[3].input_schema["properties"]["steps"]["items"]["properties"]["operation"]
                .get("enum")
                .is_none()
        );
        assert_eq!(
            serde_json::to_value(&definitions).unwrap(),
            super::definitions(STEPS)
        );
        let loops = typed_loop_definitions();
        assert_eq!(serde_json::to_value(&loops).unwrap(), loop_definitions());
        assert_eq!(
            loops[3].input_schema["properties"][INVOKE_STANDALONE]["type"],
            "boolean"
        );
        let call = |id: &str, name: &str, input: Value| ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            extra_content: None,
        };
        assert!(
            validate_plan_calls(&[
                call("a", LIST_APPLICATIONS, json!({})),
                call("b", PLAN, json!({}))
            ])
            .is_err()
        );
        assert!(validate_plan_calls(&[call("a", PLAN, json!({}))]).is_ok());
        assert!(
            validate_loop_calls(&[
                call("a", LIST_APPLICATIONS, json!({})),
                call("a", LIST_OPERATIONS, json!({}))
            ])
            .is_err()
        );
        assert!(
            validate_loop_calls(&[call(
                "a",
                INVOKE,
                json!({"operation": "shown", "parameters": {}, "standalone": true})
            )])
            .is_ok()
        );
        assert!(
            validate_loop_calls(&[
                call("a", INVOKE, json!({"operation": "shown", "parameters": {}})),
                call("b", LIST_OPERATIONS, json!({"application": false}))
            ])
            .is_err()
        );
    }

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
            broker_provider: None,
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

    /// The opens of everything installed, host applications included, do
    /// not crowd what a running application can do out of the summary:
    /// a web search started beside the agent stays listed however many
    /// applications could be opened, and Tilo's opens come before the
    /// host's.
    #[test]
    fn opens_do_not_crowd_out_what_is_running() {
        let mut catalogue = Catalogue {
            offers: vec![offer(
                "so.tilo.web-search",
                "web.search:default",
                "web.search",
                "search",
                "Search the web",
                InputSpec::None,
            )],
            facts: Vec::new(),
            held: Vec::new(),
        };
        let open = |app: &str| {
            let mut open = offer(
                "desktop",
                &format!("desktop:app:{app}"),
                "desktop.application",
                "open",
                "Open",
                InputSpec::None,
            );
            open.descriptor.kind = tilo_actions::OperationKind::Effect;
            open
        };
        for n in 0..40 {
            catalogue.offers.push(open(&format!("host:app-{n}")));
        }
        catalogue.offers.push(open("so.tilo.notes"));
        let summary = catalogue_summary(&catalogue, &mut Revealed::new(), 10);
        assert!(summary.contains("web.search:default#search"), "{summary}");
        assert!(
            summary.contains("desktop:app:so.tilo.notes#open"),
            "{summary}"
        );
        assert!(summary.contains("list_applications"), "{summary}");
    }

    /// A request naming a Tilo application by its title names it; a host
    /// application's everyday title ("Web") does not.
    #[test]
    fn a_host_application_is_never_named_by_a_request() {
        let open = |app: &str, title: &str| {
            let mut open = offer(
                "desktop",
                &format!("desktop:app:{app}"),
                "desktop.application",
                "open",
                "Open",
                InputSpec::None,
            );
            open.descriptor.resource_label = title.to_owned();
            open
        };
        let catalogue = Catalogue {
            offers: vec![
                open("host:org.gnome.Epiphany", "Web"),
                open("so.tilo.word-processor", "Writer"),
            ],
            facts: Vec::new(),
            held: Vec::new(),
        };
        let request = "find it by web search and put it in writer";
        assert_eq!(
            named_applications(&catalogue, request),
            vec![("so.tilo.word-processor", "Writer")]
        );
    }

    /// An object or array argument a model wrote as its JSON text is read
    /// back as the object or array; text parameters, and text that is not
    /// the declared kind of JSON, stay text.
    #[test]
    fn json_text_for_an_object_or_array_argument_is_decoded() {
        let mut replace = offer(
            "notes",
            "notes.page:1/body",
            "tilo.rich-text",
            "replace-range",
            "Replace text at a place",
            InputSpec::None,
        );
        replace.descriptor.params = vec![
            tilo_actions::ActionParam::required("at", tilo_actions::ParamType::Object, "where")
                .as_place(),
            tilo_actions::ActionParam::optional("marks", tilo_actions::ParamType::Array, "marks"),
            tilo_actions::ActionParam::required("text", tilo_actions::ParamType::Text, "text"),
        ];
        let mut arguments = json!({
            "at": "{\"item\":\"b1\",\"start\":0,\"end\":39}",
            "marks": "[\"link\"]",
            "text": "{\"looks\":\"like json\"}",
        })
        .as_object()
        .cloned()
        .unwrap();
        decode_structured_arguments(&replace, &mut arguments);
        assert_eq!(
            arguments["at"],
            json!({"item": "b1", "start": 0, "end": 39})
        );
        assert_eq!(arguments["marks"], json!(["link"]));
        assert_eq!(arguments["text"], json!("{\"looks\":\"like json\"}"));
        let mut wrong = json!({"at": "[1, 2]"}).as_object().cloned().unwrap();
        decode_structured_arguments(&replace, &mut wrong);
        assert_eq!(wrong["at"], json!("[1, 2]"));
    }

    /// A place whose fields a model wrote beside the other arguments is
    /// gathered back into the place parameter, a range or an insertion.
    #[test]
    fn a_place_written_flat_is_gathered_into_its_parameter() {
        let mut replace = offer(
            "notes",
            "notes.page:1/body",
            "tilo.rich-text",
            "replace-range",
            "Replace text at a place",
            InputSpec::None,
        );
        replace.descriptor.params = vec![
            tilo_actions::ActionParam::required("at", tilo_actions::ParamType::Object, "where")
                .as_place(),
            tilo_actions::ActionParam::required("text", tilo_actions::ParamType::Text, "text"),
        ];
        let flat = |value: Value| value.as_object().cloned().unwrap();
        let mut range = flat(json!({"at": 1, "item": "b1", "start": 0, "end": 39, "text": "x"}));
        decode_structured_arguments(&replace, &mut range);
        assert_eq!(
            Value::Object(range),
            json!({"at": {"item": "b1", "start": 0, "end": 39}, "text": "x"})
        );
        let mut insert = flat(json!({"at": 4, "item": "b1", "text": "x"}));
        decode_structured_arguments(&replace, &mut insert);
        assert_eq!(
            Value::Object(insert),
            json!({"at": {"item": "b1", "at": 4}, "text": "x"})
        );
        let mut whole = flat(json!({"at": {"item": "b1", "at": 4}, "item": "b2", "text": "x"}));
        decode_structured_arguments(&replace, &mut whole);
        assert_eq!(
            whole["at"],
            json!({"item": "b1", "at": 4}),
            "a given place stays"
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

    /// An operation the person is asked about first is listed, marked as
    /// such everywhere the model meets it, and the model is told it waits.
    #[test]
    fn an_operation_needing_confirmation_is_listed_and_marked() {
        let mut catalogue = room();
        let mut send = offer(
            "mail",
            "mail.inbox",
            "mail.mailbox",
            "send_message",
            "Send message",
            InputSpec::None,
        );
        send.descriptor.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
        catalogue.offers.push(send.clone());
        let mut revealed = Revealed::new();
        let summary = catalogue_summary(&catalogue, &mut revealed, MOST_SUMMARISED);
        let line = summary
            .lines()
            .find(|line| line.contains("Send message"))
            .unwrap_or_default();
        assert!(line.contains("needs confirmation"), "{summary}");
        let insert = summary
            .lines()
            .find(|line| line.contains("Insert table"))
            .unwrap_or_default();
        assert!(!insert.contains("needs confirmation"), "{summary}");
        let listed = operations(&json!({"application": "mail"}), &catalogue, &mut revealed);
        assert!(listed.contains("needs confirmation"), "{listed}");
        let described = describe(&json!({"operation": send.id()}), &catalogue, &mut revealed);
        assert!(
            described.contains("needs confirmation: your step waits"),
            "{described}"
        );
        assert!(!ARGUMENTS_DESCRIPTION.contains("include `confirm: true`"));
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
    fn discovery_answers_list_and_describe_only_what_the_room_offers() {
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
        let schema = &definition.input_schema;
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
        let wide = "語".repeat(MAX_PLAN_REPLY_CHARS);
        let kept = bound_reply(&wide);
        assert!(kept.len() <= MAX_REPLY_BYTES, "{}", kept.len());
        assert_eq!(kept.chars().count(), MAX_REPLY_BYTES / "語".len());
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
                crate::pages::READ_RESULT,
                PLAN
            ]
        );
        let invoke = &definitions[3]["input_schema"];
        // Sorted maps on the wire: the operation still comes first.
        let keys: Vec<&String> = invoke["properties"].as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "operation",
                INVOKE_PARAMETERS,
                PARTS,
                INVOKE_REPLY,
                INVOKE_STANDALONE,
                UNABLE
            ]
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

    /// An observation is compact JSON and bounded; a long value is not cut
    /// but continues on a later page.
    #[test]
    fn an_invoke_observation_is_compact_and_bounded() {
        let mut pages = crate::pages::Pages::new();
        let long = "x".repeat(crate::pages::PAGE_CHARS * 2);
        let report = InvokeReport {
            succeeded: true,
            value: Some(Value::String(long.clone())),
            changed_view: Some(json!({"caret": {"block": "b1", "offset": 3}})),
            newly_available: vec!["a#b — B".into()],
            ..InvokeReport::default()
        };
        let text = invoke_result(&report, &mut pages);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["result"], "succeeded");
        assert_eq!(value["changed_view"]["caret"]["offset"], 3);
        assert_eq!(value["newly_available"][0], "a#b — B");
        assert!(
            text.len() < crate::pages::PAGE_CHARS + 600,
            "{}",
            text.len()
        );
        let from = value["continues"]["from"].as_u64().unwrap();
        let rest: Value = serde_json::from_str(
            &pages.read(&json!({"result": value["continues"]["result"], "from": from})),
        )
        .unwrap();
        assert_eq!(rest["next"], Value::Null);
        assert_eq!(
            format!(
                "{}{}",
                value["value"].as_str().unwrap(),
                rest["text"].as_str().unwrap()
            ),
            long
        );
        let skipped = invoke_result(
            &InvokeReport {
                skipped: Some("an earlier invoke failed".into()),
                ..InvokeReport::default()
            },
            &mut pages,
        );
        assert_eq!(
            serde_json::from_str::<Value>(&skipped).unwrap()["result"],
            "not run"
        );
    }

    /// Every open object in the tool schemas says it is open. Gemini reads an
    /// object schema without properties as an empty object, so an argument
    /// map left implicit would reach every operation empty and a required
    /// argument (a new note's title, say) could never be passed.
    #[test]
    fn every_open_argument_object_declares_itself_open() {
        fn walk(path: &str, node: &Value, open: &mut Vec<String>) {
            match node {
                Value::Object(map) => {
                    if map.get("type").and_then(Value::as_str) == Some("object")
                        && !map.contains_key("properties")
                        && map.get("additionalProperties") != Some(&Value::Bool(true))
                    {
                        open.push(path.to_owned());
                    }
                    for (key, child) in map {
                        walk(&format!("{path}/{key}"), child, open);
                    }
                }
                Value::Array(items) => {
                    for (index, child) in items.iter().enumerate() {
                        walk(&format!("{path}/{index}"), child, open);
                    }
                }
                _ => {}
            }
        }
        let mut undeclared = Vec::new();
        walk("", &definitions(4), &mut undeclared);
        assert!(
            undeclared.is_empty(),
            "implicitly open objects: {undeclared:?}"
        );
    }

    /// Every tool schema stays inside the JSON Schema subset every provider
    /// dialect the agent speaks accepts as function parameters, Gemini's
    /// included: plain types, no unions, no `null` type, no references.
    #[test]
    fn tool_schemas_use_only_keywords_every_provider_accepts() {
        const ACCEPTED: &[&str] = &[
            "type",
            "description",
            "properties",
            "required",
            "items",
            "enum",
            "minimum",
            "maximum",
            "minItems",
            "maxItems",
            "additionalProperties",
        ];
        fn check(schema: &Value, at: &str) {
            let Some(object) = schema.as_object() else {
                panic!("{at}: a schema is an object");
            };
            for (key, value) in object {
                assert!(ACCEPTED.contains(&key.as_str()), "{at}: `{key}`");
                match key.as_str() {
                    "type" => assert!(
                        value.as_str().is_some_and(|ty| ty != "null"),
                        "{at}: one non-null type"
                    ),
                    "properties" => {
                        for (name, property) in value.as_object().unwrap() {
                            check(property, &format!("{at}.{name}"));
                        }
                    }
                    "items" => check(value, &format!("{at}[]")),
                    _ => {}
                }
            }
        }
        for set in [definitions(8), loop_definitions()] {
            for tool in set.as_array().unwrap() {
                check(&tool["input_schema"], tool["name"].as_str().unwrap());
            }
        }
    }

    /// An operation taking content beside words lists its other parameters
    /// and says the content one is carried in from the step before it.
    #[test]
    fn content_beside_words_shows_the_words_and_carries_the_content() {
        let mut attach = offer(
            "mail",
            "mail.mailbox",
            "mail.mailbox",
            "attach",
            "Attach to draft",
            InputSpec::None,
        );
        attach.descriptor.params = vec![
            tilo_actions::ActionParam::required("content", tilo_actions::ParamType::Text, "file")
                .as_content(),
            tilo_actions::ActionParam::required("name", tilo_actions::ParamType::Text, "name"),
        ];
        assert_eq!(
            parameter_signature(&attach),
            "{content: what the previous step produced, name: text}"
        );
        let hint = parameter_hint(&attach);
        assert!(hint.contains("\"name\": …"), "{hint}");
        assert!(!hint.contains("\"content\": …"), "{hint}");
        assert!(hint.contains("`content` is filled in"), "{hint}");
        let place = &room().offers[1];
        assert_eq!(
            parameter_signature(place),
            "{takes what the previous step produced}"
        );
    }
}
