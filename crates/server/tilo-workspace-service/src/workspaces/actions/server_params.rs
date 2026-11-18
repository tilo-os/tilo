//! What the actions that choose a workspace's servers and copies take.

use tilo_actions::{ActionParam, ParamType};

pub(super) fn add_server_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "origin",
            ParamType::Text,
            "The other server's site, like https://tilo.sh. You must be let in there, and it may \
             refuse by its own rules.",
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
        "The server's site, like https://tilo.sh, or its device key (64 hex characters) as the \
         server list names it.",
    )]
}

pub(super) fn move_server_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "server",
            ParamType::Text,
            "The server it moves from: its site, like https://tilo.sh, or its device key (64 hex \
             characters) as the server list names it.",
        ),
        ActionParam::required(
            "origin",
            ParamType::Text,
            "The site of the server it moves to, like https://tilo.sh. You must be let in there, \
             and it may refuse by its own rules; the workspace then stays where it was.",
        ),
    ]
}
