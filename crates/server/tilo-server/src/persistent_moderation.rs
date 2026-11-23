//! The persistent workspace's moderation agent, when the operator mounts a model
//! key (`--moderation-key-file`).
//!
//! The key is kept in the host's secret store. The workspace's moderation bot
//! ([`tilo_server::workspace_bots::ModerationBot`]) reads reports, takes messages back and
//! settles reports through Chat's own actions under its Chat grant, and restricts or bans a
//! member through the workspace's own membership actions under the grants a person gave it when
//! they turned it on.

use std::path::Path;

use anyhow::Context as _;
use tilo_moderation::ModerationAgent;
use tilo_moderation::anthropic::{AnthropicJudge, import_mounted_key};

/// The moderation agent, its model's key imported from `key_file`.
///
/// # Errors
/// The key file is unreadable or empty, or the store refused it; the workspace
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

/// The secret store app a host keeps its public workspaces' roots in.
const SECRET_APP: &str = "so.tilo.server";

/// Names the model key file that moderates this server's public workspaces;
/// unset, they are moderated by their own moderators alone.
pub(crate) const PUBLIC_MODERATION_KEY_ENV: &str = "TILO_SERVER_MODERATION_KEY_FILE";
/// Names the model that moderates them, when not [`tilo_moderation::DEFAULT_MODEL`].
pub(crate) const PUBLIC_MODERATION_MODEL_ENV: &str = "TILO_SERVER_MODERATION_MODEL";

/// Run the moderation agent in every workspace this server lists as a public
/// workspace, for as long as it is listed, when the operator mounted a
/// model key ([`PUBLIC_MODERATION_KEY_ENV`]): a public workspace takes in
/// strangers, so its reports are answered even while its moderators are
/// away. Each workspace's agent is the persistent workspace's
/// ([`tilo_server::workspace_bots::ModerationBot`]).
///
/// # Errors
/// The key is named but unusable; the server refuses to start rather than
/// run its public workspaces unmoderated by mistake.
pub(crate) fn follow_published(
    server: &tilo_server::daemon::Server,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    let Some(key_file) = std::env::var_os(PUBLIC_MODERATION_KEY_ENV).map(std::path::PathBuf::from)
    else {
        return Ok(None);
    };
    let model = std::env::var(PUBLIC_MODERATION_MODEL_ENV).ok();
    let Some(workspaces) = server.workspaces().cloned() else {
        return Ok(None);
    };
    let store = tilo_secrets::SecretStore::for_app(SECRET_APP);
    let agent = load(&store, &key_file, model.as_deref())?;
    tracing::info!(model = agent.model(), "public workspaces are moderated");
    Ok(Some(tokio::spawn(async move {
        let mut changes = workspaces.listing_changes();
        let mut running: std::collections::HashMap<String, tokio::task::JoinHandle<()>> =
            std::collections::HashMap::new();
        loop {
            let published: std::collections::BTreeSet<String> = workspaces
                .published_workspaces()
                .await
                .into_iter()
                .collect();
            running.retain(|workspace, bot| {
                let listed = published.contains(workspace);
                if !listed {
                    bot.abort();
                }
                listed
            });
            for workspace in published {
                if running.contains_key(&workspace) {
                    continue;
                }
                // A person decides whether their workspace has a moderation
                // bot: without their enable it holds no grant and stays off.
                match tilo_server::workspace_bots::enabled(&workspaces, &workspace).await {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        tracing::warn!(%workspace, %error, "a public workspace's moderation enable could not be read");
                        continue;
                    }
                }
                match load(&store, &key_file, model.as_deref()) {
                    Ok(agent) => {
                        let bot = tilo_server::workspace_bots::ModerationBot::new(
                            workspaces.clone(),
                            workspace.clone(),
                            agent,
                        );
                        // A person who governs the workspace turns it on.
                        workspaces.offer_moderation(&workspace).await;
                        running.insert(workspace, tokio::spawn(bot.run()));
                    }
                    Err(error) => {
                        tracing::warn!(%workspace, error = %format!("{error:#}"), "a public workspace runs without its moderation agent");
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
