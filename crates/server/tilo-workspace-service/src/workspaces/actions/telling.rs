//! What this host offers an agent of a person's of that person's rule for
//! what is important: the actions of `tilo.telling` it may call, as rows
//! of the catalogue it reads. A page asks the host directly and is offered
//! no row.
//!
//! The agent reads the rule wherever it works for its person, and tells
//! its person there. It changes the rule, when its person tells it to,
//! only in a workspace that person owns. What only the person does (reading
//! what they were told, saying what breaks do-not-disturb and which
//! workspace's words stay on the host) is no row.

use tilo_actions::remote::{CatalogueRow, person_telling};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::{Publisher, Verb};

/// A person's rule for what is important, as their host publishes it.
pub(super) const TELLING: Publisher = Publisher {
    provider: person_telling::PROVIDER,
    kind: "tilo.telling",
};

/// What an agent reads of its person's rule.
const READS: [Verb; 1] = [Verb {
    id: person_telling::READ,
    label: "Read what is important",
    check: "",
    kind: ActionKind::Read,
    params: Vec::new,
}];

/// What an agent changes of its person's rule, in a workspace of theirs.
const CHANGES: [Verb; 5] = [
    Verb {
        id: person_telling::SAY,
        label: "Add a line to what is important",
        check: "",
        kind: ActionKind::Undoable,
        params: say_params,
    },
    Verb {
        id: person_telling::UNSAY,
        label: "Remove a line from what is important",
        check: "",
        kind: ActionKind::Undoable,
        params: unsay_params,
    },
    Verb {
        id: person_telling::SET_FACT,
        label: "Set a situation",
        check: "",
        kind: ActionKind::Undoable,
        params: set_fact_params,
    },
    Verb {
        id: person_telling::FORGET_FACT,
        label: "Forget a situation",
        check: "",
        kind: ActionKind::Undoable,
        params: forget_fact_params,
    },
    Verb {
        id: person_telling::SET_INSISTING,
        label: "Set how a reminder repeats",
        check: "",
        kind: ActionKind::Undoable,
        params: insisting_params,
    },
];

/// What an agent adds to the log of what its person was told.
const TELLS: [Verb; 2] = [
    Verb {
        id: person_telling::TELL,
        label: "Tell the person",
        check: "",
        kind: ActionKind::Undoable,
        params: tell_params,
    },
    Verb {
        id: person_telling::INSIST,
        label: "Mark as not to be missed",
        check: "",
        kind: ActionKind::Undoable,
        params: insist_params,
    },
];

/// The rows an agent of a person's is offered: the read of the rule, its
/// changes where it asks through a workspace its person owns (`own`), and
/// telling its person.
pub(super) fn agent_rows(own: bool) -> Vec<CatalogueRow> {
    let row = |verb: &Verb| verb.row(TELLING, person_telling::IMPORTANCE, "What is important");
    let mut rows: Vec<CatalogueRow> = READS.iter().map(row).collect();
    if own {
        rows.extend(CHANGES.iter().map(row));
    }
    rows.extend(
        TELLS
            .iter()
            .map(|verb| verb.row(TELLING, person_telling::TOLD, "Told")),
    );
    rows
}

fn insist_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("told", ParamType::Text, "The thing told, by its id."),
        ActionParam::required(
            "insists",
            ParamType::Boolean,
            "Whether it must not be missed: raised again, by the person's own rhythm, until they acknowledge it.",
        ),
    ]
}

fn tell_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "words",
            ParamType::Text,
            "What to tell the person, as plain text, in a sentence or two.",
        ),
        ActionParam::optional("title", ParamType::Text, "A few words over it."),
        ActionParam::optional(
            "cause",
            ParamType::Object,
            "What caused it: { kind, label, key }, in your own words; the key is your own id for it, and one cause is told once. What you tell is news whatever kind you name: your person's own rule marks it.",
        ),
        ActionParam::optional(
            "about",
            ParamType::Object,
            "Where it is: { workspace, app, record, resource }, what opening it shows.",
        ),
        ActionParam::optional(
            "task",
            ParamType::Object,
            "The task it belongs to: { record, issue }.",
        ),
        ActionParam::optional(
            "insists",
            ParamType::Boolean,
            "Whether it must not be missed, when its person said so of this thing.",
        ),
        ActionParam::optional(
            "actions",
            ParamType::Array,
            "Its buttons: [{ id, label, main, target: { app, record, resource, action }, arguments }], each a typed action the person's device invokes as the person. Offer only an action you may invoke yourself here: anything else is refused, and a workspace's governance always.",
        ),
    ]
}

fn say_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "words",
            ParamType::Text,
            "What is important, in the person's own words, kept as they said them.",
        ),
        ActionParam::optional(
            "onlyWhile",
            ParamType::Object,
            "When the line depends on a situation: { fact, is }, the situation's name and the value it must have for the line to count.",
        ),
    ]
}

fn unsay_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "line",
        ParamType::Text,
        "The line, by its id as the rule lists it.",
    )]
}

fn set_fact_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "name",
            ParamType::Text,
            "What the person calls the situation.",
        ),
        ActionParam::required(
            "value",
            ParamType::Text,
            "How it stands now, in the person's words.",
        ),
    ]
}

fn forget_fact_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "name",
        ParamType::Text,
        "The situation, by its name.",
    )]
}

fn insisting_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "everySecs",
            ParamType::Integer,
            "The time between two reminders of a thing that must not be missed, in seconds.",
        ),
        ActionParam::required("times", ParamType::Integer, "How often it reminds at most."),
    ]
}
