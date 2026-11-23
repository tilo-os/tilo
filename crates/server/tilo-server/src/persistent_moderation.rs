//! The persistent workspace's moderation agent, when the operator mounts a model
//! key (`--moderation-key-file`).
//!
//! The key is kept in the host's secret store. The workspace's moderation bot
//! ([`tilo_server::workspace_bots::ModerationBot`]) reads reports and records decisions
//! with its Chat grant. A person with moderation authority handles membership changes.

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
    let store = tilo_secrets::SecretStore::for_app(crate::persistent_workspace_cli::SECRET_APP);
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
                let host = workspaces.device_cert();
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
                // The hosted workspace explicitly stored its root when
                // provisioned. A host without that authority cannot enroll a bot.
                let root = match store.item("workspace-root", &workspace).get_blocking() {
                    Ok(Some(secret)) => <[u8; 32]>::try_from(secret.expose())
                        .ok()
                        .map(|bytes| ed25519_dalek::SigningKey::from_bytes(&bytes)),
                    _ => None,
                };
                let Some(root) =
                    root.filter(|root| root.verifying_key().to_bytes() == host.user_public_key)
                else {
                    tracing::warn!(%workspace, "no enrolled root for the workspace's moderation bot");
                    continue;
                };
                match load(&store, &key_file, model.as_deref()) {
                    Ok(agent) => {
                        let bot = tilo_server::workspace_bots::ModerationBot::new(
                            workspaces.clone(),
                            workspace.clone(),
                            host.clone(),
                            root,
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

/// How often one person may have the workspace's model look over a review, and
/// how often everyone together may: each review is one model call the
/// operator pays for, and a report is reviewed once per change to its words.
const REVIEWS_PER_PERSON: usize = 12;
const REVIEWS_PER_WORKSPACE: usize = 240;
const REVIEW_WINDOW: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The workspace's moderation model, lent to the workspace's reporters to look over
/// their reports before they send them: the reporter's page never holds the
/// key, and it asks through its feedback admission, so only people the workspace
/// admits to report may ask, and only the review contract is asked
/// (`tilo_agent_session::FeedbackReviewer`).
pub(crate) struct WorkspaceReviewer {
    judge: std::sync::Arc<AnthropicJudge>,
    runtime: tokio::runtime::Handle,
    asked: std::sync::Mutex<Asked>,
}

#[derive(Default)]
struct Asked {
    /// Every ask, whoever made it, within the window.
    all: std::collections::VecDeque<std::time::Instant>,
    /// Each person's asks, within the window.
    people: std::collections::HashMap<[u8; 32], std::collections::VecDeque<std::time::Instant>>,
}

impl Asked {
    /// Whether `person` may ask now; counts the ask when they may.
    fn admit(&mut self, person: [u8; 32], now: std::time::Instant) -> bool {
        let recent = |times: &mut std::collections::VecDeque<std::time::Instant>| {
            while times
                .front()
                .is_some_and(|at| now.duration_since(*at) >= REVIEW_WINDOW)
            {
                times.pop_front();
            }
        };
        recent(&mut self.all);
        self.people.retain(|_, times| {
            recent(times);
            !times.is_empty()
        });
        let theirs = self.people.entry(person).or_default();
        if self.all.len() >= REVIEWS_PER_WORKSPACE || theirs.len() >= REVIEWS_PER_PERSON {
            return false;
        }
        theirs.push_back(now);
        self.all.push_back(now);
        true
    }
}

impl WorkspaceReviewer {
    /// The reviewer asking the model whose key [`load`] kept in `store`.
    ///
    /// # Errors
    /// The store holds no usable key for it.
    pub(crate) fn from_store(
        store: &tilo_secrets::SecretStore,
        model: Option<&str>,
    ) -> anyhow::Result<Self> {
        let profile =
            tilo_moderation::anthropic::profile(tilo_moderation::anthropic::HOST_PROFILE, model);
        let judge = AnthropicJudge::from_profile(&profile, store)
            .map_err(|error| anyhow::anyhow!("{error}"))
            .context("load the workspace's review model")?;
        Ok(Self {
            judge: std::sync::Arc::new(judge),
            runtime: tokio::runtime::Handle::current(),
            asked: std::sync::Mutex::default(),
        })
    }
}

impl tilo_agent_session::FeedbackReviewer for WorkspaceReviewer {
    fn name(&self) -> String {
        format!(
            "Anthropic {}",
            tilo_moderation::Judge::model(self.judge.as_ref())
        )
    }

    fn review(
        &self,
        caller: &tilo_actions::remote::Caller,
        prompt: String,
        schema: serde_json::Value,
        done: Box<dyn FnOnce(Result<String, String>) + Send>,
    ) {
        let Some(person) = caller.user_public_key().copied() else {
            return done(Err("an unnamed caller".into()));
        };
        let admitted = self
            .asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .admit(person, std::time::Instant::now());
        if !admitted {
            return done(Err("reviews are asked too often".into()));
        }
        let judge = std::sync::Arc::clone(&self.judge);
        self.runtime.spawn(async move {
            let answer = judge.complete(&prompt, &schema).await;
            if let Err(error) = &answer {
                tracing::warn!(%error, "the workspace's model did not review a report");
            }
            done(answer.map_err(|error| error.to_string()));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One person cannot spend the workspace's model on reviews faster than a
    /// person writes reports, and a window later they may again.
    #[test]
    fn a_person_may_ask_only_so_often() {
        let mut asked = Asked::default();
        let start = std::time::Instant::now();
        for _ in 0..REVIEWS_PER_PERSON {
            assert!(asked.admit([1; 32], start));
        }
        assert!(!asked.admit([1; 32], start));
        assert!(asked.admit([2; 32], start), "others still may");
        assert!(asked.admit([1; 32], start + REVIEW_WINDOW));
    }
}
