//! What a turn this server carries out is offered of a person's calendar accounts: beginning a
//! sign-in on a provider's own page, finding an account's calendars, and choosing which of them
//! show. The rows are the accounts surface's three calendar verbs, published on the person's
//! accounts, as a Settings page reads them.

use crate::workspace_accounts;
use tilo_actions::remote::CatalogueRow;
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::{Publisher, Verb};

/// A person's calendar accounts, as the host publishes them.
const CALENDARS: Publisher = Publisher {
    provider: workspace_accounts::PROVIDER,
    kind: "tilo.accounts",
};

/// The rows, in the order a turn reads them.
pub(in crate::workspaces) fn rows() -> Vec<CatalogueRow> {
    [
        Verb {
            id: workspace_accounts::BEGIN_SIGN_IN,
            label: "Sign in to an account for its calendars",
            check: "",
            kind: ActionKind::Undoable,
            params: begin_sign_in_params,
        },
        Verb {
            id: workspace_accounts::DISCOVER_CALENDARS,
            label: "Find an account's calendars",
            check: "",
            kind: ActionKind::Undoable,
            params: account_params,
        },
        Verb {
            id: workspace_accounts::SET_CALENDARS,
            label: "Choose an account's calendars",
            check: "",
            kind: ActionKind::Undoable,
            params: calendar_params,
        },
    ]
    .iter()
    .map(|verb| verb.row(CALENDARS, workspace_accounts::RESOURCE, "Calendars"))
    .collect()
}

fn begin_sign_in_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "provider",
            ParamType::Text,
            "The provider: `nextcloud`, `google`, `microsoft`, `icloud` or `other`.",
        ),
        ActionParam::optional(
            "server",
            ParamType::Text,
            "The Nextcloud server's address; left out for the others.",
        ),
        ActionParam::optional(
            "name",
            ParamType::Text,
            "The name the account is shown under.",
        ),
        ActionParam::optional(
            "calendar",
            ParamType::Boolean,
            "Whether its calendars show in Calendar: a turn connects an account for its calendars \
             alone, so this is on and its mail and files are off.",
        ),
    ]
}

fn account_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "account",
        ParamType::Text,
        "The account, by its id as the accounts reading names it.",
    )]
}

fn calendar_params() -> Vec<ActionParam> {
    let mut params = account_params();
    params.push(ActionParam::required(
        "calendars",
        ParamType::Array,
        "The ids of the account's calendars that show; the others keep their events out.",
    ));
    params
}
