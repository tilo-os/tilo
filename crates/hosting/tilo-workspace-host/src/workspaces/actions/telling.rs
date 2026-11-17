//! What this host offers an agent of a person's of that person's rule for
//! what is important: the actions of `tilo.telling` it may call, as rows
//! of the catalogue it reads. A page asks the host directly and is offered
//! no row.
//!
//! The agent reads the rule wherever it works for its person. It changes
//! the rule, when its person tells it to, only in a workspace that person
//! owns. What only the person says (what breaks do-not-disturb, which
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
    gate: "",
    kind: ActionKind::Read,
    params: Vec::new,
}];

/// What an agent changes of its person's rule, in a workspace of theirs.
const CHANGES: [Verb; 5] = [
    Verb {
        id: person_telling::SAY,
        label: "Add a line to what is important",
        gate: "",
        kind: ActionKind::Undoable,
        params: say_params,
    },
    Verb {
        id: person_telling::UNSAY,
        label: "Remove a line from what is important",
        gate: "",
        kind: ActionKind::Undoable,
        params: unsay_params,
    },
    Verb {
        id: person_telling::SET_FACT,
        label: "Set a situation",
        gate: "",
        kind: ActionKind::Undoable,
        params: set_fact_params,
    },
    Verb {
        id: person_telling::FORGET_FACT,
        label: "Forget a situation",
        gate: "",
        kind: ActionKind::Undoable,
        params: forget_fact_params,
    },
    Verb {
        id: person_telling::SET_INSISTING,
        label: "Set how a reminder repeats",
        gate: "",
        kind: ActionKind::Undoable,
        params: insisting_params,
    },
];

/// The rows an agent of a person's is offered: the read, and the changes
/// where it asks through a workspace its person owns (`own`).
pub(super) fn agent_rows(own: bool) -> Vec<CatalogueRow> {
    let row = |verb: &Verb| verb.row(TELLING, person_telling::IMPORTANCE, "What is important");
    let mut rows: Vec<CatalogueRow> = READS.iter().map(row).collect();
    if own {
        rows.extend(CHANGES.iter().map(row));
    }
    rows
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
