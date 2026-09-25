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
//! | [`PLAN`] | the answer: the steps to perform, in order |
//!
//! ## Nothing here can perform anything
//!
//! Three of the four are read-only in the shape of what they return rather
//! than by promise: a listing is labels and addresses, with no callback,
//! token or handle anywhere in one — "what can I do here" must be free to
//! ask, so that a caller exploring an unfamiliar room can explore it safely.
//!
//! [`PLAN`] is the one that commits, and it performs nothing either: it ends
//! the conversation with an answer, which the caller then validates against a
//! freshly taken survey before anything is dispatched.
//!
//! ## An id cannot be invented
//!
//! [`PLAN`]'s `operation` is an enum, and the enum is **what the discovery
//! tools have actually returned in this conversation** ([`Revealed`]) — plus
//! the editorial changes and "none", which are the agent's own and finite.
//! The model does not pick from a list somebody guessed it would need; it
//! picks from what it asked for and was shown. An id no tool returned is not
//! in the schema, is refused by [`read_plan`] if a malformed call carries one
//! anyway, and would still not survive the survey check.
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

    fn ids(&self) -> Vec<&str> {
        self.0.iter().map(String::as_str).collect()
    }
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
}

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
             applications' own words. The ids that come back are the only ones \
             `plan` will accept.",
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
             Call this before planning any operation that takes an argument — the \
             example and the facts are what make the value computable rather than \
             guessed.",
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

/// The [`PLAN`] tool definition. Rebuilt from `revealed` on every turn, which
/// is what makes "pick from what a tool returned" a property of the request
/// rather than an instruction in a prompt.
fn plan_definition(revealed: &Revealed, most_steps: usize) -> Value {
    json!({
        "name": PLAN,
        "description": format!(
            "The answer. The operations to perform, in the order the request asks \
             for them — one for a request that asks for one thing, which is most of \
             them, and at most {most_steps}. Call this exactly once, when you know \
             what to do. Every `operation` must be an id a listing actually \
             returned; nothing else can be named here."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "steps": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "operation": {
                                "type": "string",
                                "enum": revealed.ids(),
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
                                "description":
                                    "The complete JSON argument object described by \
                                     `describe_operation`, keyed by the application's \
                                     parameter names. Use {} for a nullary operation. Values \
                                     retain their declared JSON types; never stringify numbers, \
                                     booleans, arrays, objects or null. When the descriptor's \
                                     confirmation policy is Confirm, also include `confirm: true`; \
                                     for Phrase include `confirmation_phrase` with the exact phrase. \
                                     Those two names are invocation metadata, not app parameters."
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

/// The tools as the API is told about them.
pub fn definitions(revealed: &Revealed, most_steps: usize) -> Value {
    json!([
        list_applications_definition(),
        list_operations_definition(),
        describe_operation_definition(),
        plan_definition(revealed, most_steps),
    ])
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
        DESCRIBE_OPERATION => describe(input, catalogue),
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
        .filter(|o| application.is_none_or(|a| o.app == a))
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
        return nothing_found(&what);
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
                "\n{} — {} ({})",
                r, o.descriptor.resource_label, o.descriptor.resource_kind
            );
            last = Some(r);
        }
        revealed.reveal(o.id());
        let _ = write!(s, "  {} — {}", o.id(), o.descriptor.action_label);
        if let tilo_actions::Availability::Unavailable { reason, .. } =
            &o.descriptor.contract.availability
        {
            let _ = write!(s, "; currently unavailable: {reason}");
        }
        if o.wants_content() {
            s.push_str("; takes what an earlier step produced, not words");
        } else if !o.descriptor.params.is_empty() {
            let _ = write!(
                s,
                "; takes {} structured argument{} (ask `{DESCRIBE_OPERATION}` for the schema)",
                o.descriptor.params.len(),
                if o.descriptor.params.len() == 1 {
                    ""
                } else {
                    "s"
                }
            );
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
fn describe(input: &Value, catalogue: &Catalogue) -> String {
    let Some(id) = input.get("operation").and_then(Value::as_str) else {
        return format!("{DESCRIBE_OPERATION} needs an `operation`.");
    };
    let Some(offer) = surface::find(&catalogue.offers, id) else {
        // Unknown and not-offered are deliberately one answer, as they are at
        // the door: a caller must not be able to map what it may not see by
        // getting a different refusal for it.
        return nothing_found(&format!("the operation {id}"));
    };
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
        s.push_str("  Pass one JSON object containing exactly the parameters above.\n");
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
/// `Err` names what was wrong with it, in words that go in a log line.
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
        // The enum is the schema's job; this is the check behind it. A
        // well-formed call cannot carry an id no tool returned, and a
        // malformed one is refused here — before the survey ever sees it —
        // so "the model picks from what it was shown" holds whether or not
        // the API enforced it.
        if !revealed.contains(operation) {
            return Err(format!(
                "named {operation:?}, which no listing in this conversation returned"
            ));
        }
        actions.push(operation.to_string());
        let arguments_object = step
            .get("arguments")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("step {operation:?} supplied no argument object"))?;
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
    })
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

    /// The listing separates what the room holds from what could still be
    /// opened, using the real [`CONTRACTS`] rather than a hand-built one —
    /// so this proves the wiring, not a fixture that agrees with itself.
    #[test]
    fn the_application_listing_separates_what_is_open_from_what_could_be() {
        let word_processor_open = Catalogue {
            held: vec!["word-processor"],
            offers: vec![Offer {
                app: "chat".to_string(),
                broker_peer_id: None,
                descriptor: ActionDescriptor {
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

    /// An operation is nameable exactly when a listing returned it.
    ///
    /// The property the whole restructure rests on: the model does not pick
    /// from a list somebody guessed it would need, it picks from what it
    /// asked for — and an id nothing showed it cannot be answered at all.
    #[test]
    fn only_what_a_listing_returned_can_be_planned() {
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

    #[test]
    fn one_published_operation_can_be_planned_repeatedly_with_distinct_arguments() {
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

    /// A kind crosses applications by construction: the filter is the
    /// resource's own kind tag, which is shared across the OS.
    #[test]
    fn a_kind_is_asked_about_across_the_room_and_not_within_one_app() {
        let mut revealed = Revealed::new();
        let out = operations(
            &json!({"kind": "word_processor.document"}),
            &room(),
            &mut revealed,
        );
        assert!(out.contains("insert-table"), "{out}");
        assert!(!out.contains("harbour.png#rename"), "{out}");
    }

    /// The description carries the declared typed parameter and the fact the
    /// publishing resource states with no operation on it — neither of which
    /// a flat list reaches.
    #[test]
    fn describing_an_operation_carries_its_prompt_its_example_and_the_resources_own_facts() {
        let out = describe(
            &json!({"operation": "files.entry:/home/q/harbour.png#rename"}),
            &room(),
        );
        assert!(out.contains("value: text (required)"), "{out}");
        assert!(out.contains("512 x 320 pixels"), "{out}");
    }

    /// An operation whose argument is content says so, and says not to write
    /// one — the one thing a caller must never invent.
    #[test]
    fn an_operation_that_collects_content_is_not_asked_for_a_value() {
        let out = describe(
            &json!({"operation": "word-processor.document#place-image"}),
            &room(),
        );
        assert!(out.contains("content, not words"), "{out}");
        assert!(out.contains("bound from the prior step"), "{out}");
    }

    /// An address that is not on offer and one that does not exist are
    /// answered the same way, so probing maps nothing.
    ///
    /// Not byte-identical — each reply names the id it was *asked* about —
    /// but same sentence, no mention of permission, and nothing that would
    /// let a caller tell "no such operation" from "not for you" apart.
    #[test]
    fn an_operation_that_is_not_offered_is_answered_as_though_it_did_not_exist() {
        let hidden_id = "word-processor.document#imaginary-crop";
        let absent_id = "nothing.at#all";
        let hidden = describe(&json!({"operation": hidden_id}), &room());
        let absent = describe(&json!({"operation": absent_id}), &room());

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

    /// A listing that is cut short says how to reach the rest, because the
    /// cap is on the reply and not on what may be named.
    #[test]
    fn a_cut_listing_says_how_to_reach_what_it_left_out() {
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

    #[test]
    fn a_plan_longer_than_a_sentence_is_refused_rather_than_truncated() {
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

    /// The half of a request the room cannot name yet survives the read, in
    /// the words it was asked in.
    #[test]
    fn what_the_room_cannot_name_yet_is_carried_as_words() {
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

    /// An id no tool ever put in front of the model cannot be answered,
    /// whatever it names — the property [`Revealed`] exists to hold.
    #[test]
    fn a_plan_naming_an_address_never_revealed_is_refused_not_attempted() {
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
