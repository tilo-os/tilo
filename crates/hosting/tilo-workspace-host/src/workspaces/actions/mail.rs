//! What this host offers an agent of a person's of that person's mail: the
//! actions of `tilo.accounts` it may call, as rows of the catalogue it
//! reads. A page asks the host directly and is offered no row; an agent
//! finds what it may do here, once its person named it for an account
//! ([`person_accounts::ASSISTANT`]).
//!
//! It reads, searches and drafts. Sending is offered too, and is put to its
//! person before anything leaves. What only the person does (adding and
//! removing accounts, marking mail, saying who may read) is no row.

use tilo_actions::remote::{CatalogueRow, person_accounts};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::{Publisher, Verb};

/// A person's mail accounts, as their host publishes them.
pub(super) const MAIL: Publisher = Publisher {
    provider: person_accounts::PROVIDER,
    kind: "tilo.accounts",
};

/// What an agent its person named for an account may ask, in catalogue
/// order.
const VERBS: [Verb; 9] = [
    Verb {
        id: person_accounts::LIST,
        label: "List mail accounts",
        gate: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: person_accounts::ARRIVED,
        label: "List arrived mail",
        gate: "",
        kind: ActionKind::Read,
        params: arrived_params,
    },
    Verb {
        id: person_accounts::FETCH,
        label: "Read mail folder",
        gate: "",
        kind: ActionKind::Read,
        params: fetch_params,
    },
    Verb {
        id: person_accounts::READ,
        label: "Read message",
        gate: "",
        kind: ActionKind::Read,
        params: read_params,
    },
    Verb {
        id: person_accounts::SEARCH,
        label: "Search mail",
        gate: "",
        kind: ActionKind::Read,
        params: search_params,
    },
    Verb {
        id: person_accounts::BEGIN_SEND,
        label: "Begin message",
        gate: "",
        kind: ActionKind::Undoable,
        params: begin_params,
    },
    Verb {
        id: person_accounts::SEND_PIECE,
        label: "Add file to message",
        gate: "",
        kind: ActionKind::Undoable,
        params: piece_params,
    },
    Verb {
        id: person_accounts::SAVE_DRAFT,
        label: "Keep message as draft",
        gate: "",
        kind: ActionKind::Undoable,
        params: begun_params,
    },
    Verb {
        id: person_accounts::SEND,
        label: "Send message",
        gate: "",
        kind: ActionKind::Outside,
        params: begun_params,
    },
];

/// The rows an agent its person named for an account is offered.
pub(super) fn assistant_rows() -> Vec<CatalogueRow> {
    VERBS
        .iter()
        .map(|verb| verb.row(MAIL, person_accounts::RESOURCE, "Mail"))
        .collect()
}

fn account_param() -> ActionParam {
    ActionParam::required(
        "account",
        ParamType::Text,
        "The account, by its id as the list of accounts names it.",
    )
}

fn folder_param() -> ActionParam {
    ActionParam::required(
        "folder",
        ParamType::Text,
        "The folder: inbox, sent, drafts, archive, trash or junk.",
    )
}

fn arrived_params() -> Vec<ActionParam> {
    vec![ActionParam::optional(
        "after",
        ParamType::Integer,
        "The place in the arrival log to read on from; everything the log still keeps when omitted.",
    )]
}

fn fetch_params() -> Vec<ActionParam> {
    vec![
        account_param(),
        folder_param(),
        ActionParam::optional(
            "knownUid",
            ParamType::Integer,
            "The newest message number already held of the folder, to be answered only what arrived since.",
        ),
        ActionParam::optional(
            "knownUidValidity",
            ParamType::Integer,
            "The numbering the held numbers were read under.",
        ),
        ActionParam::optional(
            "beforeUid",
            ParamType::Integer,
            "The oldest message number already held, to be answered the page before it.",
        ),
    ]
}

fn read_params() -> Vec<ActionParam> {
    vec![
        account_param(),
        folder_param(),
        ActionParam::required(
            "uid",
            ParamType::Integer,
            "The message's number in its folder.",
        ),
        ActionParam::required(
            "uidValidity",
            ParamType::Integer,
            "The numbering that number was read under.",
        ),
    ]
}

fn search_params() -> Vec<ActionParam> {
    fn part(name: &'static str, about: &'static str) -> ActionParam {
        ActionParam::optional(name, ParamType::Text, about)
    }
    vec![
        account_param(),
        ActionParam::optional(
            "folders",
            ParamType::Array,
            "The folders to look in; the inbox and the archive when none is named.",
        ),
        part("text", "Words the message holds anywhere."),
        part("from", "A name or address it came from."),
        part("to", "A name or address it went to."),
        part("subject", "Words of its subject."),
        ActionParam::optional(
            "limit",
            ParamType::Integer,
            "The most messages answered of each folder, newest first; 25 at most.",
        ),
    ]
}

fn begin_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "message",
            ParamType::Object,
            "The message: account, fromAddress, to, subject and body, with cc, bcc, htmlBody, \
             messageId, inReplyTo and references where it has them.",
        ),
        ActionParam::optional(
            "files",
            ParamType::Array,
            "The files it carries, each with filename, mime and size; their bytes follow in pieces.",
        ),
        ActionParam::optional(
            "calendarIcs",
            ParamType::Text,
            "A calendar invitation it brings.",
        ),
    ]
}

fn piece_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("send", ParamType::Text, "The begun message's id."),
        ActionParam::required(
            "file",
            ParamType::Integer,
            "The file's place among the message's files.",
        ),
        ActionParam::required(
            "offset",
            ParamType::Integer,
            "Where in the file this piece starts.",
        ),
        ActionParam::required(
            "bytes",
            ParamType::Text,
            "The piece, hex-encoded; 256 KiB of the file at most.",
        ),
    ]
}

fn begun_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "send",
        ParamType::Text,
        "The begun message's id.",
    )]
}
