//! The persistent room's moderation agent, when the operator mounts a model
//! key (`--moderation-key-file`).
//!
//! The key is kept in this host's secret store like every provider key, and
//! the agent's verdicts are enforced through the room's own governance, as
//! the room's root: a restriction is the room's expiring `restrict:<person>`
//! Deny, a ban the room's ban. Reports, history, decisions and undos go
//! through the bot's replica of the chat record; the host runs it as the
//! room's moderation bot ([`tilo_relay::room_bots::ModerationBot`]).

use std::path::Path;

use anyhow::Context as _;
use tilo_moderation::ModerationAgent;
use tilo_moderation::anthropic::{AnthropicJudge, import_mounted_key};

/// The moderation agent, its model's key imported from `key_file`.
///
/// # Errors
/// The key file is unreadable or empty, or the store refused it; the room
/// refuses to start rather than run unmoderated by mistake.
pub(crate) fn load(
    store: &tilo_secrets::SecretStore,
    key_file: &Path,
    model: Option<&str>,
) -> anyhow::Result<ModerationAgent<AnthropicJudge>> {
    let judge = import_mounted_key(store, key_file, model)
        .map_err(|error| anyhow::anyhow!("{error}"))
        .context("load the moderation agent's model key")?;
    Ok(ModerationAgent::new(judge))
}
