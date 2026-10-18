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

/// Names the model key file that moderates this relay's public workspaces;
/// unset, they are moderated by their own moderators alone.
pub(crate) const PUBLIC_MODERATION_KEY_ENV: &str = "TILO_RELAY_MODERATION_KEY_FILE";
/// Names the model that moderates them, when not [`tilo_moderation::DEFAULT_MODEL`].
pub(crate) const PUBLIC_MODERATION_MODEL_ENV: &str = "TILO_RELAY_MODERATION_MODEL";

/// Run the moderation agent in every room this relay lists as a public
/// workspace, for as long as it is listed, when the operator mounted a
/// model key ([`PUBLIC_MODERATION_KEY_ENV`]): a public room takes in
/// strangers, so its reports are answered even while its moderators are
/// away. Each room's agent is the persistent room's
/// ([`tilo_relay::room_bots::ModerationBot`]), its verdicts enforced
/// through that room's governance.
///
/// # Errors
/// The key is named but unusable; the relay refuses to start rather than
/// run its public rooms unmoderated by mistake.
pub(crate) fn follow_published(
    relay: &tilo_relay::daemon::Relay,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    let Some(key_file) = std::env::var_os(PUBLIC_MODERATION_KEY_ENV).map(std::path::PathBuf::from)
    else {
        return Ok(None);
    };
    let model = std::env::var(PUBLIC_MODERATION_MODEL_ENV).ok();
    let Some(rooms) = relay.rooms().cloned() else {
        return Ok(None);
    };
    let store = tilo_secrets::SecretStore::for_app(crate::persistent_room_cli::SECRET_APP);
    let agent = load(&store, &key_file, model.as_deref())?;
    tracing::info!(model = agent.model(), "public workspaces are moderated");
    Ok(Some(tokio::spawn(async move {
        let mut changes = rooms.listing_changes();
        let mut running: std::collections::HashMap<String, tokio::task::JoinHandle<()>> =
            std::collections::HashMap::new();
        loop {
            let published: std::collections::BTreeSet<String> =
                rooms.published_rooms().into_iter().collect();
            running.retain(|room, bot| {
                let listed = published.contains(room);
                if !listed {
                    bot.abort();
                }
                listed
            });
            for room in published {
                if running.contains_key(&room) {
                    continue;
                }
                match load(&store, &key_file, model.as_deref()) {
                    Ok(agent) => {
                        let bot = tilo_relay::room_bots::ModerationBot::new(
                            rooms.clone(),
                            room.clone(),
                            rooms.governor().clone(),
                            agent,
                        );
                        running.insert(room, tokio::spawn(bot.run()));
                    }
                    Err(error) => {
                        tracing::warn!(%room, error = %format!("{error:#}"), "a public workspace runs without its moderation agent");
                    }
                }
            }
            match changes.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        for bot in running.into_values() {
            bot.abort();
        }
    })))
}
