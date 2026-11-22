//! What the actions that choose a workspace's servers and copies take.

use tilo_actions::{ActionParam, ParamType};

pub(super) fn add_server_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "host",
            ParamType::Text,
            "The other server's own web-host.json line, which its operator hands over: its \
             certificate and where it is dialled. You must be let in there, and it may refuse by \
             its own rules.",
        ),
        ActionParam::optional(
            "role",
            ParamType::Text,
            "`key_holder` (the default): it holds the workspace's keys, reads it and takes its \
             work, without becoming a member. `store`: it keeps and forwards it encrypted and \
             reads none of it.",
        )
        .with_choices(HOLDER_ROLES),
    ]
}

/// How a holder may hold a workspace, as [`tilo_peer_state::CopyRole`] spells it.
const HOLDER_ROLES: [&str; 2] = ["key_holder", "store"];

pub(super) fn copies_params() -> Vec<ActionParam> {
    vec![
        ActionParam::optional(
            "key_holders",
            ParamType::Integer,
            "Holders that hold its keys and take its work. Leave both out for the default: one \
             of each once a server holds it, and the devices it has otherwise.",
        ),
        ActionParam::optional(
            "stores",
            ParamType::Integer,
            "Holders that keep it encrypted and read none of it.",
        ),
    ]
}

pub(super) fn remove_server_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "server",
        ParamType::Text,
        "The server's name as the server list shows it, or its device key (64 hex characters).",
    )]
}

pub(super) fn move_server_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "server",
            ParamType::Text,
            "The server it moves from: its name as the server list shows it, or its device key \
             (64 hex characters).",
        ),
        ActionParam::required(
            "host",
            ParamType::Text,
            "The moving-to server's own web-host.json line, which its operator hands over. You \
             must be let in there, and it may refuse by its own rules; the workspace then stays \
             where it was.",
        ),
    ]
}
