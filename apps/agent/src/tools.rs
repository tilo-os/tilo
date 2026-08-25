//! What the agent can be *asked*: named tools over the room's own surface.
//!
//! The decision — which exported operation answers an instruction — was made
//! from a single flat list. Every operation every open app exported, plus the
//! editorial changes, plus one "open" entry per app that was not open, all
//! pasted into one turn and the answer pinned to an enum of all of it, capped
//! at a hundred and twenty.
//!
//! It works, and the shape of it is what a run of failures had in common:
//!
//! * "What may be named" and "what fits in one message" were the **same
//!   quantity**. Past the cap an operation could not be named at all, so a
//!   folder of a thousand files spent the whole budget on renames and put the
//!   rest of the room out of reach.
//! * A prompted operation's own words were one clause of one line among a
//!   hundred and twenty. The photo editor's `crop` says *"Region to keep, as
//!   x,y,w,h between 0 and 1"* and carries its own example, `0.25,0.25,0.5,0.5`
//!   — and the flat list showed the prompt and dropped the example.
//! * What an application says about a resource **with no operation on it** was
//!   dropped entirely. The photo editor publishes the pixel size of the open
//!   photograph as an action-less child resource, precisely so that a caller
//!   working from ids and titles knows the size it is working on; a flat
//!   survey walks past it. Which is the difference between computing a square
//!   crop and guessing one.
//!
//! None of that is fixed by a longer list. What they share is that discovery
//! was something the agent had to have been *handed*, whole, in advance of
//! knowing what it was looking for. So this module makes it something the
//! agent **asks for**:
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
//! than by promise: a listing is labels and addresses, and there is no
//! callback, token or handle anywhere in one. That is the separation
//! `docs/agent-action-api.md` states for the socket, for the same reason —
//! "what can I do here" must be free to ask, so that a caller exploring an
//! unfamiliar room can explore it safely.
//!
//! [`PLAN`] is the one that commits, and it performs nothing either: it ends
//! the conversation with an answer, which the caller then validates against a
//! freshly taken survey ([`crate::plan::Plan::from_answer`]) before anything is
//! dispatched.
//!
//! ## An id cannot be invented
//!
//! [`PLAN`]'s `operation` is an enum, and the enum is **what the discovery
//! tools have actually returned in this conversation** ([`Revealed`]) — plus
//! the editorial changes and "none", which are the agent's own and finite. The
//! model does not pick from a list somebody guessed it would need; it picks
//! from what it asked for and was shown. An id no tool returned is not in the
//! schema, is refused by [`read_plan`] if a malformed call carries one anyway,
//! and would still not survive the survey check.
//!
//! ## Authority is upstream and unchanged
//!
//! Every listing is built from a [`Catalogue`] that was already gated: a
//! denied operation is **absent** from `offers`, so it is absent from every
//! tool result, absent from the enum, and unnameable. Nothing in this module
//! asks a permission question and nothing in it can widen one — see the note
//! on authority at the top of [`crate::surface`].

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::model::Answer;
use crate::plan::{self, TableDraft};
use crate::surface::{self, Catalogue, Offer, APPLICATION_KIND, CONTRACTS};

pub const LIST_APPLICATIONS: &str = "list_applications";
pub const LIST_OPERATIONS: &str = "list_operations";
pub const DESCRIBE_OPERATION: &str = "describe_operation";
pub const PLAN: &str = "plan";

/// How many operations one listing returns.
///
/// A cap on a **reply**, which is what it always should have been. The flat
/// list's cap was a cap on *reach*: it pinned the answer's enum, so an
/// operation past the hundred and twentieth could not be named at all. Here a
/// listing that is cut short says so and says how to narrow it, and the
/// narrowed listing reaches whatever was cut.
pub const MOST_LISTED: usize = 60;

// ---------------------------------------------------------------------------
// What has been shown
// ---------------------------------------------------------------------------

/// The addresses the discovery tools have returned so far in one conversation.
///
/// The whole of what [`PLAN`] will accept, which is why a made-up id cannot be
/// *answered* rather than merely being refused afterwards. It starts as the
/// agent's own finite vocabulary — the editorial changes and "none" — and
/// grows only when a tool actually put an address in front of the model.
#[derive(Clone, Debug, Default)]
pub struct Revealed(BTreeSet<String>);

impl Revealed {
    /// The vocabulary before anything has been discovered: the compositions
    /// the agent performs itself, and the answer that means "nothing fits".
    pub fn new() -> Self {
        let mut set = BTreeSet::new();
        for r in plan::RECIPES {
            set.insert(r.id.to_string());
        }
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

// ---------------------------------------------------------------------------
// The definitions
// ---------------------------------------------------------------------------

/// The tools as the API is told about them.
///
/// [`PLAN`]'s enum is rebuilt from `revealed` on every turn, which is what
/// makes "pick from what a tool returned" a property of the request rather
/// than an instruction in a prompt.
pub fn definitions(revealed: &Revealed, most_steps: usize) -> Value {
    json!([
        {
            "name": LIST_APPLICATIONS,
            "description":
                "Every application in this room. Says which are open and which are not, \
                 and how many operations each is exporting right now. An application \
                 exporting nothing has nothing open in it yet — opening it onto \
                 something is an ordinary step, and what it does is put that \
                 application's own operations on the list. Call this first when the \
                 request might involve an application you have not seen.",
            "input_schema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": LIST_OPERATIONS,
            "description":
                "What can be done in this room, right now. Every filter is optional and \
                 they combine; with none of them you get everything, capped. Use `kind` \
                 to ask what may be done to a kind of thing in any application at all \
                 (`files.entry`, `photos.image`, `word_processor.section`), `resource` \
                 for one named thing, `application` for one application, and `search` to \
                 match the applications' own words. The ids that come back are the only \
                 ones `plan` will accept.",
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
        },
        {
            "name": DESCRIBE_OPERATION,
            "description":
                "One operation in full: what the application says it does, what it does \
                 it to, whether performing it needs an argument, and — when it does — \
                 the application's own prompt for that argument and its own example of \
                 one. Also what the publishing resource says about itself, which is \
                 where an application states a fact it has no operation to hang on: a \
                 photograph's pixel size, say. Call this before planning any operation \
                 that takes an argument — the example and the facts are what make the \
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
        },
        {
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
                                "argument": {
                                    "anyOf": [{ "type": "string" }, { "type": "null" }],
                                    "description":
                                        "The value this operation collects, in the form its own \
                                         prompt asks for — see `describe_operation`, which gives \
                                         you that prompt, the application's own example of a \
                                         value, and what the application says about the thing \
                                         being acted on. The example is the shape, not the \
                                         answer. Null for an operation that takes nothing, and \
                                         null for one that takes what an earlier step produced: \
                                         that is content, it is filled in from what actually \
                                         happened, and nothing you could write would name real \
                                         bytes."
                                }
                            },
                            "required": ["operation", "argument"],
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
                    },
                    "table": {
                        "anyOf": [
                            {
                                "type": "object",
                                "properties": {
                                    "headers": {
                                        "type": "array",
                                        "items": { "type": "string" },
                                        "description": "The column headings you chose."
                                    },
                                    "rows": {
                                        "type": "array",
                                        "items": { "type": "array", "items": { "type": "string" } },
                                        "description": "The body rows, each exactly as wide as `headers`."
                                    },
                                    "note": {
                                        "anyOf": [{ "type": "string" }, { "type": "null" }],
                                        "description": "One line to print under the table, naming \
                                            anything in the notes that contradicts itself, cannot \
                                            be right, or would not work in practice — the same \
                                            fact stated twice with different values, say, or \
                                            steps in an impossible order. Null ONLY if you \
                                            checked and it all genuinely holds together. Not for \
                                            describing what you did."
                                    }
                                },
                                "required": ["headers", "rows", "note"],
                                "additionalProperties": false
                            },
                            { "type": "null" }
                        ],
                        "description":
                            "Content for an editorial change that calls for a table. Null when \
                             the step you chose does not call for one — an editorial change \
                             that asks for a table and is given none cannot be carried out at \
                             all."
                    }
                },
                "required": ["steps", "why", "then", "table"],
                "additionalProperties": false
            }
        }
    ])
}

// ---------------------------------------------------------------------------
// Answering them
// ---------------------------------------------------------------------------

/// What a tool call is answered with when it names nothing.
///
/// An ordinary tool result rather than an error: the model can read it and ask
/// a better question, which is the whole point of a conversation. Nothing
/// about it is a failure of the run.
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
        // Not reachable through a well-formed call — the API only ever names a
        // tool that was defined — and answered rather than panicked on,
        // because a name is untrusted input like everything else here.
        other => format!("There is no tool called {other}."),
    }
}

/// What is running, and what could be opened.
fn applications(catalogue: &Catalogue, revealed: &mut Revealed) -> String {
    let mut s = String::new();
    for contract in CONTRACTS {
        let open = catalogue.held.iter().any(|h| *h == contract.namespace);
        let count = catalogue
            .offers
            .iter()
            .filter(|o| {
                o.app == contract.namespace && o.descriptor.resource_kind != APPLICATION_KIND
            })
            .count();
        // The operation that would open it — if the room granted one. A denied
        // open is *absent* from the catalogue, so it is absent here too: the
        // application is named, no route to opening it is given, and that is
        // the same thing a caller is told about any operation it may not
        // perform.
        let opener = catalogue
            .offers
            .iter()
            .find(|o| o.app == contract.namespace && surface::opens(o.address()).is_some());

        s.push_str(&format!("{} ({})", contract.title, contract.namespace));
        s.push_str(if open { " — open" } else { " — not open" });
        s.push_str(&format!(
            ", exporting {count} operation{}",
            if count == 1 { "" } else { "s" }
        ));
        if count > 0 {
            s.push_str(&format!(
                "; ask `{LIST_OPERATIONS}` with application=\"{}\" for them",
                contract.namespace
            ));
        }
        match opener {
            Some(o) => {
                revealed.reveal(o.id());
                // Two different sentences, because being here and having
                // something to work on are two different facts. An
                // application present but exporting nothing is not absent —
                // it is empty, and saying "not open" of it would be wrong in
                // the one direction that costs a request its answer.
                s.push_str(if open {
                    ". Nothing is open in it, so none of what it can do is here to be \
                     named yet"
                } else {
                    ". None of what it can do is here to be named yet"
                });
                s.push_str(&format!(
                    " — opening it is what puts its operations on the list: `{}`",
                    o.id()
                ));
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

    // Grouped by the resource that publishes them, so the reply says what the
    // operations are *on* as well as what they are — the shape a person would
    // read, and the shape that makes a second, narrower question obvious.
    let mut s = String::new();
    let mut last: Option<&str> = None;
    for o in matching.iter().take(MOST_LISTED) {
        let r = o.address().resource.as_str();
        if last != Some(r) {
            s.push_str(&format!(
                "\n{} — {} ({})\n",
                r, o.descriptor.resource_label, o.descriptor.resource_kind
            ));
            last = Some(r);
        }
        revealed.reveal(o.id());
        s.push_str(&format!("  {} — {}", o.id(), o.descriptor.action_label));
        match &o.descriptor.input {
            qos_actions::InputSpec::Text {
                collects: qos_actions::Collects::Words,
                prompt,
                ..
            } => s.push_str(&format!(
                "; takes an argument: {prompt} (ask `{DESCRIBE_OPERATION}` for its shape)"
            )),
            qos_actions::InputSpec::Text {
                collects: qos_actions::Collects::Content,
                ..
            } => s.push_str("; takes what an earlier step produced, not words"),
            qos_actions::InputSpec::None => {}
        }
        s.push('\n');
    }
    let rest = matching.len().saturating_sub(MOST_LISTED);
    if rest > 0 {
        s.push_str(&format!(
            "\n… and {rest} more not listed. Ask again with a narrower \
             application=, kind=, resource= or search= to reach them — they are \
             not out of reach, only out of this reply.\n"
        ));
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
        s.push_str(&format!("  the application says: {description}\n"));
    }
    match &d.input {
        qos_actions::InputSpec::None => {
            s.push_str("  argument: none. Pass null.\n");
        }
        qos_actions::InputSpec::Text {
            prompt,
            placeholder,
            collects: qos_actions::Collects::Words,
        } => {
            s.push_str(&format!("  argument: required — {prompt}\n"));
            if !placeholder.is_empty() {
                s.push_str(&format!(
                    "  the application's own example of one: {placeholder}\n"
                ));
            }
            s.push_str(
                "  Put the value in this step's `argument`, in exactly that form. The \
                 application parses it, and one it cannot read leaves the thing \
                 untouched rather than doing something else to it.\n",
            );
        }
        qos_actions::InputSpec::Text {
            prompt,
            collects: qos_actions::Collects::Content,
            ..
        } => {
            s.push_str(&format!(
                "  argument: content, not words — {prompt}. Leave it null: it is \
                 whatever the step before this one produced, filled in from what \
                 actually happened. Nothing you could write would name real bytes.\n"
            ));
        }
    }
    // What the resource says about itself. An application with a fact and no
    // operation to hang it on publishes a child resource that exports nothing,
    // and a flat survey walks straight past it.
    if let Some(facts) = catalogue.about(&d.address.resource) {
        for note in &facts.notes {
            s.push_str(&format!("  about {}: {note}\n", facts.label));
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
pub fn read_plan(input: &Value, revealed: &Revealed, most_steps: usize) -> Result<Answer, String> {
    let steps = input
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| "the plan named no steps".to_string())?;

    let mut actions: Vec<String> = Vec::new();
    let mut arguments: Vec<Option<String>> = Vec::new();
    for step in steps {
        // Only strings, and never coerced. Anything else means the answer did
        // not have the shape it was asked for, and quietly stringifying it
        // would hide that.
        let Some(operation) = step.get("operation").and_then(Value::as_str) else {
            return Err("a step named no operation".to_string());
        };
        // The enum is the schema's job; this is the check behind it. A
        // well-formed call cannot carry an id no tool returned, and a
        // malformed one is refused here — before the survey ever sees it — so
        // "the model picks from what it was shown" holds whether or not the
        // API enforced it.
        if !revealed.contains(operation) {
            return Err(format!(
                "named {operation:?}, which no listing in this conversation returned"
            ));
        }
        actions.push(operation.to_string());
        arguments.push(
            step.get("argument")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(str::to_string),
        );
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
        table: input.get("table").and_then(table_draft),
    })
}

/// Read a drafted table out of a plan, taking only strings.
///
/// Anything that is not a string is dropped rather than coerced: a number
/// where a cell should be means the answer did not have the shape it was asked
/// for, and quietly stringifying it would hide that. What survives here is
/// still unchecked — [`TableDraft::check`] is the gate.
fn table_draft(v: &Value) -> Option<TableDraft> {
    let obj = v.as_object()?;
    let strings = |val: Option<&Value>| -> Vec<String> {
        val.and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(TableDraft {
        headers: strings(obj.get("headers")),
        rows: obj
            .get("rows")
            .and_then(|r| r.as_array())
            .map(|rows| rows.iter().map(|r| strings(Some(r))).collect())
            .unwrap_or_default(),
        note: obj
            .get("note")
            .and_then(Value::as_str)
            .map(|n| n.chars().take(240).collect()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::ResourceFacts;
    use qos_actions::door::{ActionAddress, ActionDescriptor};
    use qos_actions::{Collects, ExecutionLocus, InputSpec};

    const STEPS: usize = 4;

    fn offer(
        app: &'static str,
        resource: &str,
        kind: &str,
        action: &str,
        label: &str,
        input: InputSpec,
    ) -> Offer {
        Offer {
            app,
            descriptor: ActionDescriptor {
                address: ActionAddress::new(resource, action),
                resource_kind: kind.to_string(),
                resource_label: "a thing".to_string(),
                action_label: label.to_string(),
                description: None,
                locus: ExecutionLocus::ClientLocal,
                input,
            },
        }
    }

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
                Offer {
                    app: "photos",
                    descriptor: ActionDescriptor {
                        address: surface::open_address("photos"),
                        resource_kind: APPLICATION_KIND.to_string(),
                        resource_label: "the photo editor".to_string(),
                        action_label: "open the photo editor".to_string(),
                        description: None,
                        locus: ExecutionLocus::ServerPreferred,
                        input: InputSpec::None,
                    },
                },
            ],
            facts: vec![ResourceFacts {
                id: "files.entry:/home/q/harbour.png".to_string(),
                kind: "files.entry".to_string(),
                label: "harbour.png".to_string(),
                notes: vec!["512 x 320 pixels".to_string()],
            }],
        }
    }

    /// The listing says which applications are here and which are not, and
    /// gives a route to opening only the ones the room granted.
    #[test]
    fn the_application_listing_separates_what_is_open_from_what_could_be() {
        let mut revealed = Revealed::new();
        let out = applications(&room(), &mut revealed);
        assert!(out.contains("the word processor (word-processor) — open"), "{out}");
        assert!(out.contains("the photo editor (photos) — not open"), "{out}");
        assert!(out.contains("photos.app#open"), "{out}");
        // Chat is neither held nor openable in this room, so it is named as
        // absent with no route — the shape a denied open has to take.
        assert!(out.contains("and there is no opening it from here"), "{out}");
        assert!(!out.contains("chat.app#open"), "{out}");
    }

    /// An operation is nameable exactly when a listing returned it.
    ///
    /// The property the whole restructure rests on: the model does not pick
    /// from a list somebody guessed it would need, it picks from what it asked
    /// for — and an id nothing showed it cannot be answered at all.
    #[test]
    fn only_what_a_listing_returned_can_be_planned() {
        let mut revealed = Revealed::new();
        assert!(!revealed.contains("files.entry:/home/q/harbour.png#rename"));
        // The editorial changes and "none" are the agent's own and are there
        // from the start; nothing an application exports is.
        assert!(revealed.contains(plan::NOTHING_FITS));

        let call = json!({
            "steps": [{"operation": "files.entry:/home/q/harbour.png#rename", "argument": "a.png"}],
            "why": "", "then": null, "table": null
        });
        let refused = read_plan(&call, &revealed, STEPS).expect_err("nothing revealed it yet");
        assert!(refused.contains("no listing in this conversation returned"), "{refused}");

        let listed = operations(&json!({"kind": "files.entry"}), &room(), &mut revealed);
        assert!(listed.contains("files.entry:/home/q/harbour.png#rename"), "{listed}");
        let answer = read_plan(&call, &revealed, STEPS).expect("now it has been shown");
        assert_eq!(answer.arguments, vec![Some("a.png".to_string())]);
    }

    /// A kind crosses applications by construction: the filter is the
    /// resource's own kind tag, which is shared across the OS.
    #[test]
    fn a_kind_is_asked_about_across_the_room_and_not_within_one_app() {
        let mut revealed = Revealed::new();
        let out = operations(&json!({"kind": "word_processor.document"}), &room(), &mut revealed);
        assert!(out.contains("insert-table"), "{out}");
        assert!(!out.contains("harbour.png#rename"), "{out}");
    }

    /// The description carries the application's own prompt, its own example,
    /// and the fact the publishing resource states with no operation on it.
    ///
    /// All three were unreachable from the flat list: it printed the prompt,
    /// dropped the example, and never walked an action-less child resource at
    /// all. The example and the size are what make a value computable rather
    /// than guessed.
    #[test]
    fn describing_an_operation_carries_its_prompt_its_example_and_the_resources_own_facts() {
        let out = describe(
            &json!({"operation": "files.entry:/home/q/harbour.png#rename"}),
            &room(),
        );
        assert!(out.contains("The new name"), "{out}");
        assert!(out.contains("example of one: notes.md"), "{out}");
        assert!(out.contains("512 x 320 pixels"), "{out}");
    }

    /// An operation whose argument is content says so, and says not to write
    /// one — the one thing a caller must never invent.
    #[test]
    fn an_operation_that_collects_content_is_not_asked_for_a_value() {
        let out = describe(&json!({"operation": "word-processor.document#place-image"}), &room());
        assert!(out.contains("content, not words"), "{out}");
        assert!(out.contains("Leave it null"), "{out}");
    }

    /// An address that is not on offer and one that does not exist are
    /// answered identically, so probing maps nothing.
    #[test]
    fn an_operation_that_is_not_offered_is_answered_as_though_it_did_not_exist() {
        let hidden = describe(&json!({"operation": "photos.image:/x#crop"}), &room());
        let absent = describe(&json!({"operation": "nothing.at#all"}), &room());
        assert_eq!(hidden, absent);
    }

    /// A listing that is cut short says how to reach the rest, because the cap
    /// is on the reply and not on what may be named.
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
        assert!(narrowed.contains(&format!("f{}.md#rename", MOST_LISTED + 4)), "{narrowed}");
    }

    #[test]
    fn a_plan_longer_than_a_sentence_is_refused_rather_than_truncated() {
        let mut revealed = Revealed::new();
        revealed.reveal("a#b");
        let steps: Vec<Value> = (0..STEPS + 1)
            .map(|_| json!({"operation": "a#b", "argument": null}))
            .collect();
        let err = read_plan(
            &json!({"steps": steps, "why": "", "then": null, "table": null}),
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
        revealed.reveal("photos.app#open");
        let answer = read_plan(
            &json!({
                "steps": [{"operation": "photos.app#open", "argument": null}],
                "why": "the editor is not open",
                "then": "make this square",
                "table": null
            }),
            &revealed,
            STEPS,
        )
        .expect("read");
        assert_eq!(answer.then.as_deref(), Some("make this square"));
        // A blank is nothing, not a clause whose words are no words.
        let blank = read_plan(
            &json!({
                "steps": [{"operation": "photos.app#open", "argument": null}],
                "why": "", "then": "   ", "table": null
            }),
            &revealed,
            STEPS,
        )
        .expect("read");
        assert_eq!(blank.then, None);
    }
}
