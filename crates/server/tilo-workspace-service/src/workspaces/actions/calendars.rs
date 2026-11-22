//! What this holder offers an agent of a person's, of that person's calendars:
//! the actions of `tilo.accounts` it may call on them, as rows of the
//! catalogue it reads, once its person named it for a calendar account.
//!
//! It reads what the calendars hold and when their person is free, files an
//! entry under categories and puts an invitation in. None of it sends
//! anything to anybody. Adding and removing accounts stay the person's.

use tilo_actions::contract::Audience;
use tilo_actions::remote::{CatalogueRow, person_accounts};
use tilo_actions::{ActionContract, ActionKind, ActionParam, ParamType};

use super::{Publisher, Verb};
use crate::workspaces::person_settings::calendars as held;

/// A person's calendars, as their holder publishes them.
const CALENDARS: Publisher = Publisher {
    provider: person_accounts::PROVIDER,
    kind: "tilo.accounts",
};

/// One of the rows: whom the person named for the account decides who is
/// offered it, and nothing of a workspace's governance does.
const fn verb(
    id: &'static str,
    label: &'static str,
    kind: ActionKind,
    params: fn() -> Vec<ActionParam>,
) -> Verb {
    Verb {
        id,
        label,
        check: "",
        kind,
        params,
    }
}

const VERBS: [Verb; 6] = [
    verb(held::LIST, "List calendars", ActionKind::Read, Vec::new),
    verb(
        held::SYNC,
        "Ask calendar servers now",
        ActionKind::Read,
        sync_params,
    ),
    verb(
        held::ENTRIES,
        "Read calendar entries",
        ActionKind::Read,
        entries_params,
    ),
    verb(held::FREE, "Find free time", ActionKind::Read, free_params),
    verb(
        held::ADD_INVITATION,
        "Add invitation to calendar",
        ActionKind::Undoable,
        invitation_params,
    ),
    verb(
        held::SET_CATEGORIES,
        "Set entry categories",
        ActionKind::Undoable,
        categories_params,
    ),
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
             invitation again changes nothing. Only an entry this agent put in itself is \
             changed or removed by it: one its person entered themselves, or that reached \
             the calendar another way, is not. Text that names people without inviting its \
             person is put only once the person allowed it. An account connected for reading \
             only takes none: nothing is written to its calendar server.",
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
            "The entry, by its id as a read answers it. An entry this agent did not put in \
             itself is filed only once its person allowed it, and one whose lines leave \
             writing to the people it names to its calendar server is its person's to file. \
             An entry of an account connected for reading only is filed by nobody.",
        ),
        ActionParam::required(
            "categories",
            ParamType::Array,
            "The categories, each a few words; none to take them all off.",
        ),
    ]
}

impl super::super::Workspaces {
    /// The rows a turn this holder carries out is offered of its person's
    /// calendars: the reads, since such a turn has no device behind it for
    /// the catalogue to offer them by, and it writes nothing.
    pub(in crate::workspaces) fn calendar_read_rows() -> Vec<CatalogueRow> {
        VERBS
            .iter()
            .filter(|verb| held::turn::READS.contains(&verb.id))
            .map(|verb| verb.row(CALENDARS, held::RESOURCE, "Calendars"))
            .collect()
    }

    /// The row a turn this holder carries out for the person who asked its agent is
    /// offered for connecting a calendar its person tells it of
    /// ([`held::connect`]). It is for agents alone: a person connects by
    /// saying so, and types nothing but the password, where no agent reads
    /// it. What it says of itself is all a model needs to use it.
    pub(in crate::workspaces) fn calendar_connect_row() -> CatalogueRow {
        let mut row = verb(
            held::CONNECT,
            "Connect calendar for reading",
            ActionKind::Outside,
            connect_params,
        )
        .row(CALENDARS, held::RESOURCE, "Calendars");
        if let Some(action) = &mut row.action {
            // It signs in somewhere outside the workspace, once its person typed the password.
            let mut contract = ActionContract {
                kind: ActionKind::Outside,
                ..ActionContract::default()
            };
            contract.set_audience(Audience::Agents);
            action.contract = contract;
            action.description = Some(CONNECT_ABOUT.to_owned());
        }
        row
    }
}

/// What connecting a calendar says of itself to whoever lists it.
const CONNECT_ABOUT: &str = "Connect a calendar account of your person's on a calendar server \
     (CalDAV) at an https address, for reading only. Pass what they told you: the server's \
     address, their user name there and a name for the account. A server's bare name is \
     taken as its https address, and where all they said is their mail address, pass that \
     as the address: the calendar is looked for at the server its name ends in. Never ask \
     for the password and never take one: this server asks your person for it in a \
     protected field of your conversation with them, which you cannot read. The call ends \
     at once, told that they are asked: tell them to type the password there, and do not \
     make the call again. Once they typed it the account is connected, you are named for \
     it and you are told which calendars it has. Nothing is ever written to that calendar \
     server.";

fn connect_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "serverUrl",
            ParamType::Text,
            "Where the calendar is, as your person said it: the server's https address, its \
             bare name, or their mail address there when that is all they said.",
        ),
        ActionParam::required(
            "username",
            ParamType::Text,
            "The name their account signs in with there.",
        ),
        text(
            "name",
            "What they call the account; the user name when left out.",
        ),
    ]
}
