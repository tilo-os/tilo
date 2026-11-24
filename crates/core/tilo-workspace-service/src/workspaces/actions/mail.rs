//! What this holder offers an agent of a workspace's mail: the actions of
//! `tilo.accounts` it may call, as rows of the catalogue it reads. A page
//! asks the holder directly and is offered no row; an agent is offered these
//! where the workspace's policy lets it read the mail.
//!
//! It reads, searches and drafts. Sending is offered too, and the policy
//! decides whether its person is asked before anything leaves. Adding and
//! removing accounts and marking mail are no row of the assistant's own
//! catalogue: an agent adds an account only through the workspace's
//! governance (`workspace/mail-accounts#add`), and there the policy asks its
//! person first, as any change an agent makes is. Never silently allowed and
//! never silently denied: the account is kept only once they answer yes, and
//! a no leaves it unconnected.

use tilo_actions::remote::{
    CatalogueRow, WORKSPACE_MAIL_ACCOUNTS, person_accounts, person_vault,
    workspace_governance_child, workspace_governance_resource,
};
#[cfg(feature = "agents")]
use tilo_actions::{ActionContract, contract::Audience};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::{Publisher, Verb};
#[cfg(feature = "agents")]
use crate::workspaces::person_settings::calendars::LIST as CALENDARS_LIST;
use crate::workspaces::workspace_stores::Store;

/// A workspace's mail accounts, as their holder publishes them.
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

/// The rows an agent the workspace's policy lets read its mail is offered.
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
     there, and do not make the call again. Once they typed it the account is signed in \
     for the workspace and you are told. A password that is not accepted is asked for \
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
            "The name of the server its mail is read from (IMAP), with its port after a colon \
             where that is not 993, as imap.example.org:1993: only after this call answered \
             that none was found and your person told you.",
        ),
        said(
            "outgoingServer",
            "The name of the server its mail is handed to (SMTP), with its port after a colon \
             where that is not 465: only together with incomingServer.",
        ),
    ]
}

impl super::super::Workspaces {
    /// The row a turn this holder carries out for the person who asked its agent is offered for signing
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

    /// The rows an agent the workspace's policy lets read its mail is
    /// offered, for the entry a turn this holder carries out reaches the
    /// workspace's mail through: such a turn has no device behind it for the
    /// catalogue to offer them by.
    pub(in crate::workspaces) fn assistant_mail_rows() -> Vec<CatalogueRow> {
        assistant_rows()
    }

    /// The rows `asked`, an agent, is offered of the workspace's mail and its
    /// calendars: those the workspace's policy lets it read.
    pub(super) async fn mail_rows(&self, asked: super::Asked<'_>) -> Vec<CatalogueRow> {
        let mut rows = Vec::new();
        if self
            .reaching(asked, Store::MAIL, person_accounts::LIST, ActionKind::Read)
            .await
            .is_ok()
        {
            rows.extend(assistant_rows());
        }
        #[cfg(feature = "agents")]
        if self
            .reaching(asked, Store::CALENDARS, CALENDARS_LIST, ActionKind::Read)
            .await
            .is_ok()
        {
            rows.extend(super::calendars::assistant_rows());
        }
        rows
    }
}

/// A workspace's mail accounts in its governance tree
/// (`workspace/mail-accounts`): every `tilo.accounts` action an agent may ask (listing them,
/// reading and sending their mail), adding one, and adding one whose sign-in the vault keeps
/// by reference. Each is decided by the workspace's policy on its mail as any call of it is.
const ACCOUNT_VERBS: [(&Verb, &str); 11] = [
    (&VERBS[0], person_accounts::LIST),
    (&VERBS[1], person_accounts::ARRIVED),
    (&VERBS[2], person_accounts::FETCH),
    (&VERBS[3], person_accounts::READ),
    (&VERBS[4], person_accounts::SEARCH),
    (&VERBS[5], person_accounts::BEGIN_SEND),
    (&VERBS[6], person_accounts::SEND_PIECE),
    (&VERBS[7], person_accounts::SAVE_DRAFT),
    (&VERBS[8], person_accounts::SEND),
    (
        &Verb {
            id: "add",
            label: "Add a mail account",
            check: "",
            kind: ActionKind::Outside,
            params: add_account_params,
        },
        person_accounts::ADD_MAIL,
    ),
    (
        &Verb {
            id: person_accounts::ADD_MAIL_WITH_ITEM,
            label: "Add a mail account from the vault",
            check: "",
            kind: ActionKind::Outside,
            params: add_account_with_item_params,
        },
        person_accounts::ADD_MAIL_WITH_ITEM,
    ),
];

fn add_account_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("address", ParamType::Text, "The account's address."),
        ActionParam::optional(
            "displayName",
            ParamType::Text,
            "The name its mail is sent under.",
        ),
        ActionParam::optional(
            "username",
            ParamType::Text,
            "The sign-in name, when it is not the address.",
        ),
        ActionParam::optional(
            "incomingHost",
            ParamType::Text,
            "The IMAP server; left out, the server finds both from the address.",
        ),
        ActionParam::optional("incomingPort", ParamType::Integer, "The IMAP port."),
        ActionParam::optional("outgoingHost", ParamType::Text, "The SMTP server."),
        ActionParam::optional("outgoingPort", ParamType::Integer, "The SMTP port."),
        ActionParam::required(
            "password",
            ParamType::Secret,
            "The account's password or app password: sealed in the workspace's vault, never \
             answered back.",
        ),
        ActionParam::optional(
            "identities",
            ParamType::Array,
            "Other names and addresses it sends as, each `Name <address>`.",
        ),
    ]
}

fn add_account_with_item_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("address", ParamType::Text, "The account's address."),
        ActionParam::optional(
            "displayName",
            ParamType::Text,
            "The name its mail is sent under.",
        ),
        ActionParam::optional(
            "username",
            ParamType::Text,
            "The sign-in name, when it is neither the item's own nor the address.",
        ),
        ActionParam::required("incomingHost", ParamType::Text, "The IMAP server."),
        ActionParam::optional("incomingPort", ParamType::Integer, "The IMAP port."),
        ActionParam::required("outgoingHost", ParamType::Text, "The SMTP server."),
        ActionParam::optional("outgoingPort", ParamType::Integer, "The SMTP port."),
        person_vault::item_param(
            "password",
            "The vault item that is the account's sign-in, by its reference.",
        ),
        ActionParam::optional(
            "reason",
            ParamType::Text,
            "Why whoever asks needs it, in one sentence their person reads when they are asked \
             whether they may.",
        ),
    ]
}

/// The `tilo.accounts` action `address` asks of the workspace's mail
/// accounts in its governance tree, and the workspace it names.
pub(super) fn account_action(address: &tilo_actions::ActionAddress) -> Option<(&str, &str)> {
    let workspace = address
        .resource
        .strip_prefix(tilo_actions::remote::workspace_governance_resource("").as_str())?
        .strip_suffix(WORKSPACE_MAIL_ACCOUNTS)?
        .strip_suffix('/')?;
    let action = ACCOUNT_VERBS
        .iter()
        .find(|(verb, _)| verb.id == address.action)
        .map(|(_, action)| *action)?;
    Some((workspace, action))
}

impl crate::workspaces::Workspaces {
    /// The rows of `workspace`'s mail accounts (its UUID) `asked` is offered:
    /// those the workspace's policy on its mail lets it ask.
    pub(super) async fn mail_account_rows(
        &self,
        asked: super::Asked<'_>,
        workspace: &str,
    ) -> Vec<CatalogueRow> {
        let root = workspace_governance_resource(workspace);
        let resource = workspace_governance_child(workspace, Some(WORKSPACE_MAIL_ACCOUNTS));
        let mut rows = Vec::new();
        for (verb, action) in ACCOUNT_VERBS {
            let effect = crate::workspaces::person_settings::mail_effect(action);
            if self
                .reaching(asked, Store::MAIL, action, effect)
                .await
                .is_ok()
            {
                let mut row = verb.row(super::GOVERNANCE, &resource, "Mail accounts");
                row.parent_resource.clone_from(&root);
                // Reading `workspace/mail-accounts` lists them.
                if action == person_accounts::LIST
                    && let Some(listing) = row.action.as_mut()
                {
                    listing
                        .contract
                        .tags
                        .push(tilo_actions::DOCUMENT_READ_TAG.to_owned());
                }
                rows.push(row);
            }
        }
        rows
    }

    /// Run `request`, one of the workspace's mail account actions in its
    /// governance tree, as the `tilo.accounts` action `action` it is, in the
    /// workspace `workspace` (its UUID) the request came through.
    pub(super) async fn mail_account_invoke(
        &self,
        asked: super::Asked<'_>,
        (workspace, action): (&str, &str),
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
        let (workspace_id, caller, certificate) = asked;
        let here = match workspace_id {
            Some(id) => self.workspace_identity(id).await,
            None => None,
        };
        if here.as_deref() != Some(workspace) {
            return Err(tilo_actions::remote::ActionCallError::UnknownAction);
        }
        let mut asking = request.clone();
        asking.target = person_accounts::target(action);
        // It takes its sign-in from the vault by reference, which the provider
        // resolves on this host.
        if action == person_accounts::ADD_MAIL_WITH_ITEM {
            return Box::pin(self.vault_use_invoke(workspace_id, caller, certificate, &asking))
                .await;
        }
        Box::pin(self.person_accounts_invoke(workspace_id, caller, certificate, &asking)).await
    }
}
