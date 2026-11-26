//! What this host offers an agent of a person's on that person's log of
//! what they were told: the actions of `tilo.telling` it may call, as rows
//! of the catalogue it reads. A page asks the host directly and is offered
//! no row.
//!
//! The agent tells its person wherever it works for them, and says whether
//! what it told must not be missed. Reading what they were told, and
//! acknowledging it, is theirs alone, and no row.

use tilo_actions::remote::{CatalogueRow, person_telling};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use tilo_workspace_service::workspaces::actions::{Publisher, Verb};

/// A person's log of what they were told, as their host publishes it.
pub(super) const TELLING: Publisher = Publisher {
    provider: person_telling::PROVIDER,
    kind: "tilo.telling",
};

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

/// The rows an agent of a person's is offered: telling its person and
/// marking what it told as not to be missed.
pub(super) fn agent_rows() -> Vec<CatalogueRow> {
    TELLS
        .iter()
        .map(|verb| verb.row(TELLING, person_telling::TOLD, "Told"))
        .collect()
}

fn insist_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("told", ParamType::Text, "The thing told, by its id."),
        ActionParam::required(
            "insists",
            ParamType::Boolean,
            "Whether it must not be missed: raised again, until they acknowledge it.",
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
            "What caused it: { kind, label, key }, in your own words; the key is your own id for it, and one cause is told once. What you tell is news whatever kind you name.",
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
