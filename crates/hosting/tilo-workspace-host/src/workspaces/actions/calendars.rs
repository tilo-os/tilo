//! What this host offers an agent of a person's of that person's calendars:
//! the actions of `tilo.accounts` it may call on them, as rows of the
//! catalogue it reads, once its person named it for a calendar account.
//!
//! It reads what the calendars hold and when their person is free, files an
//! entry under categories and puts an invitation in. None of it sends
//! anything to anybody. Adding and removing accounts stay the person's.

use tilo_actions::remote::{CatalogueRow, person_accounts};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::{Publisher, Verb};
use crate::workspaces::person_settings::calendars as held;

/// A person's calendars, as their host publishes them.
const CALENDARS: Publisher = Publisher {
    provider: person_accounts::PROVIDER,
    kind: "tilo.accounts",
};

const VERBS: [Verb; 6] = [
    Verb {
        id: held::LIST,
        label: "List calendars",
        gate: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: held::SYNC,
        label: "Ask calendar servers now",
        gate: "",
        kind: ActionKind::Read,
        params: sync_params,
    },
    Verb {
        id: held::ENTRIES,
        label: "Read calendar entries",
        gate: "",
        kind: ActionKind::Read,
        params: entries_params,
    },
    Verb {
        id: held::FREE,
        label: "Find free time",
        gate: "",
        kind: ActionKind::Read,
        params: free_params,
    },
    Verb {
        id: held::ADD_INVITATION,
        label: "Add invitation to calendar",
        gate: "",
        kind: ActionKind::Undoable,
        params: invitation_params,
    },
    Verb {
        id: held::SET_CATEGORIES,
        label: "Set entry categories",
        gate: "",
        kind: ActionKind::Undoable,
        params: categories_params,
    },
];

/// The rows an agent its person named for a calendar account is offered.
pub(super) fn assistant_rows() -> Vec<CatalogueRow> {
    VERBS
        .iter()
        .map(|verb| verb.row(CALENDARS, held::RESOURCE, "Calendars"))
        .collect()
}

fn text(name: &'static str, about: &'static str) -> ActionParam {
    ActionParam::optional(name, ParamType::Text, about)
}

fn span_params() -> Vec<ActionParam> {
    const MOMENT: &str = "a date (2026-11-20), a date and time (2026-11-20T09:00) or one with its \
                          offset from UTC";
    vec![
        ActionParam::required("from", ParamType::Text, MOMENT),
        ActionParam::required("to", ParamType::Text, MOMENT),
        text(
            "zone",
            "The IANA time zone the times are read and answered in, such as Europe/Berlin; the \
             zone most entries are in when left out. The answer names it.",
        ),
        ActionParam::optional(
            "calendars",
            ParamType::Array,
            "Only these calendars, by their ids as the list names them.",
        ),
    ]
}

fn sync_params() -> Vec<ActionParam> {
    vec![text(
        "account",
        "Only this account; every account when left out.",
    )]
}

fn entries_params() -> Vec<ActionParam> {
    let mut params = span_params();
    params.push(text(
        "category",
        "Only entries filed under this category, a word of their person's own.",
    ));
    params.push(text(
        "text",
        "Only entries whose title or place holds these words.",
    ));
    params
}

fn free_params() -> Vec<ActionParam> {
    let mut params = span_params();
    params.extend([
        text("dayStart", "When a day opens, such as 09:00 (the default)."),
        text("dayEnd", "When a day closes, such as 17:00 (the default); 24:00 is its end."),
        ActionParam::optional(
            "days",
            ParamType::Array,
            "The days that count: mon, tue, wed, thu, fri, sat, sun. Monday to Friday when left out.",
        ),
        ActionParam::optional(
            "atLeastMinutes",
            ParamType::Integer,
            "The shortest gap worth answering; 30 when left out.",
        ),
        ActionParam::optional(
            "allDayBusy",
            ParamType::Boolean,
            "Whether an all-day entry takes its days.",
        ),
    ]);
    params
}

fn invitation_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "ics",
            ParamType::Text,
            "The invitation: the iCalendar text (text/calendar) a message carried. The same \
             invitation again changes nothing.",
        ),
        text(
            "calendar",
            "The calendar it goes into, by id or name; the first that can be written when left out.",
        ),
    ]
}

fn categories_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "entry",
            ParamType::Text,
            "The entry, by its id as a read answers it.",
        ),
        ActionParam::required(
            "categories",
            ParamType::Array,
            "The categories, each a few words; none to take them all off.",
        ),
    ]
}
