//! What this holder offers an agent of a person's of that person's mail: the
//! actions of `tilo.accounts` it may call, as rows of the catalogue it
//! reads. A page asks the holder directly and is offered no row; an agent
//! finds what it may do here, once its person named it for an account
//! ([`person_accounts::ASSISTANT`]).
//!
//! It reads, searches and drafts. Sending is offered too, and is put to its
//! person before anything leaves. What only the person does (adding and
//! removing accounts, marking mail, saying who may read) is no row.

use tilo_actions::remote::{CatalogueRow, person_accounts};
#[cfg(feature = "agents")]
use tilo_actions::{ActionContract, contract::Audience};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::{Publisher, Verb};

/// A person's mail accounts, as their holder publishes them.
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
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: person_accounts::ARRIVED,
        label: "List arrived mail",
        check: "",
        kind: ActionKind::Read,
        params: arrived_params,
    },
    Verb {
        id: person_accounts::FETCH,
        label: "Read mail folder",
        check: "",
        kind: ActionKind::Read,
        params: fetch_params,
    },
    Verb {
        id: person_accounts::READ,
        label: "Read message",
        check: "",
        kind: ActionKind::Read,
        params: read_params,
    },
    Verb {
        id: person_accounts::SEARCH,
        label: "Search mail",
        check: "",
        kind: ActionKind::Read,
        params: search_params,
    },
    Verb {
        id: person_accounts::BEGIN_SEND,
        label: "Begin message",
        check: "",
        kind: ActionKind::Undoable,
        params: begin_params,
    },
    Verb {
        id: person_accounts::SEND_PIECE,
        label: "Add file to message",
        check: "",
        kind: ActionKind::Undoable,
        params: piece_params,
    },
    Verb {
        id: person_accounts::SAVE_DRAFT,
        label: "Keep message as draft",
        check: "",
        kind: ActionKind::Undoable,
        params: begun_params,
    },
    Verb {
        id: person_accounts::SEND,
        label: "Send message",
        check: "",
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

/// What signing a mail account in says of itself to whoever lists it.
#[cfg(feature = "agents")]
const CONNECT_ABOUT: &str = "Sign your person in to a mail account of theirs from its address \
     alone: the way to add a mail account. Pass the address, and nothing they did not tell \
     you. This server works out the account's servers itself and asks your person for the \
     password in a protected field of your conversation with them, which you cannot read: \
     never ask for a password and never take one. The call ends at once, told that they \
     are asked and which servers were found: tell them in one line to type the password \
     there, and do not make the call again. Once they typed it the account is signed in, \
     you are named for it and you are told. A password that is not accepted is asked for \
     again by this server itself.";

#[cfg(feature = "agents")]
fn connect_params() -> Vec<ActionParam> {
    let said = |name: &'static str, about: &'static str| {
        ActionParam::optional(name, ParamType::Text, about)
    };
    vec![
        ActionParam::required(
            "address",
            ParamType::Text,
            "The account's mail address, as your person said it.",
        ),
        said(
            "username",
            "The name it signs in with, only where your person said one that is not the address.",
        ),
        said(
            "name",
            "The name mail from it carries, where they said one.",
        ),
        said(
            "incomingServer",
            "The name of the server its mail is read from (IMAP): only after this call answered \
             that none was found and your person told you.",
        ),
        said(
            "outgoingServer",
            "The name of the server its mail is handed to (SMTP): only together with \
             incomingServer.",
        ),
    ]
}

impl super::super::Workspaces {
    /// The row a turn this holder carries out for its agent's keeper is offered for signing
    /// a mail account in from its address alone
    /// ([`crate::workspaces::person_settings::mail_connect`]). It is for agents alone: a
    /// person signs in by saying so, and types nothing but the password, where no agent
    /// reads it. What it says of itself is all a model needs to use it.
    #[cfg(feature = "agents")]
    pub(in crate::workspaces) fn mail_connect_row() -> CatalogueRow {
        let mut row = Verb {
            id: crate::workspaces::person_settings::mail_connect::CONNECT,
            label: "Sign in to mail account",
            check: "",
            kind: ActionKind::Outside,
            params: connect_params,
        }
        .row(MAIL, person_accounts::RESOURCE, "Mail");
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

    /// The rows an agent its person named for an account is offered, for the
    /// entry a turn this holder carries out reaches that person's mail
    /// through: such a turn has no device behind it for the catalogue to
    /// offer them by.
    pub(in crate::workspaces) fn assistant_mail_rows() -> Vec<CatalogueRow> {
        assistant_rows()
    }
}
