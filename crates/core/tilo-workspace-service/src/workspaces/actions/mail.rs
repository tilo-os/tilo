//! What this holder offers an agent of a workspace's mail accounts: the
//! actions of `tilo.accounts` it may call, as rows of the catalogue it reads.
//! A page asks the holder directly and is offered no row; an agent is offered
//! these where the workspace's policy lets it read the accounts.
//!
//! Reading, searching, sending and marking mail are the Mail app's, reached
//! as any other app's actions are, over the connector that keeps them. Adding
//! and removing accounts are no row of the assistant's own catalogue: an
//! agent adds an account only through the workspace's governance
//! (`workspace/mail-accounts#add`), and there the policy asks its person
//! first, as any change an agent makes is. Never silently allowed and never
//! silently denied: the account is kept only once they answer yes, and a no
//! leaves it unconnected.

use tilo_actions::remote::{
    CatalogueRow, WORKSPACE_MAIL_ACCOUNTS, person_accounts, person_vault,
    workspace_governance_child, workspace_governance_resource,
};
#[cfg(feature = "agents")]
use tilo_actions::{ActionContract, contract::Audience};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::{Publisher, Verb};
use crate::workspaces::workspace_stores::Store;

/// A workspace's mail accounts, as their holder publishes them.
pub(super) const MAIL: Publisher = Publisher {
    provider: person_accounts::PROVIDER,
    kind: "tilo.accounts",
};

/// What an agent its person named for an account may ask, in catalogue
/// order: the accounts it may read and send from. Mail's own reading and
/// sending are the app's, reached as any other app's actions are.
const VERBS: [Verb; 1] = [Verb {
    id: person_accounts::LIST,
    label: "List mail accounts",
    check: "",
    kind: ActionKind::Read,
    params: Vec::new,
}];

/// The rows an agent the workspace's policy lets read its mail is offered.
pub(super) fn assistant_rows() -> Vec<CatalogueRow> {
    VERBS
        .iter()
        .map(|verb| verb.row(MAIL, person_accounts::RESOURCE, "Mail"))
        .collect()
}

impl super::super::Workspaces {
    /// The rows `asked`, an agent, is offered of the workspace's mail
    /// accounts: those the workspace's policy lets it read.
    pub(super) async fn mail_rows(&self, asked: super::Asked<'_>) -> Vec<CatalogueRow> {
        let mut rows = Vec::new();
        if self
            .reaching(asked, Store::MAIL, person_accounts::LIST, ActionKind::Read)
            .await
            .is_ok()
        {
            rows.extend(assistant_rows());
        }
        rows
    }
}

/// A workspace's mail accounts in its governance tree
/// (`workspace/mail-accounts`): listing them, adding one, and adding one whose sign-in the
/// vault keeps by reference. Each is decided by the workspace's policy on its mail as any call
/// of it is.
const ACCOUNT_VERBS: [(&Verb, &str); 3] = [
    (&VERBS[0], person_accounts::LIST),
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

pub(super) fn add_account_params() -> Vec<ActionParam> {
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

/// The parameters the mail accounts' action `action` (a `tilo.accounts` action, or a verb of
/// the mail accounts in a workspace's governance tree) is published with.
pub(super) fn account_params_of(action: &str) -> Option<Vec<ActionParam>> {
    ACCOUNT_VERBS
        .iter()
        .find(|(verb, named)| verb.id == action || *named == action)
        .map(|(verb, _)| (verb.params)())
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
