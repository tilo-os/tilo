//! The bots installed in a workspace this host governs, run on the host.
//!
//! The host follows the workspace's records through its own copy: each member
//! write it takes in ([`Workspaces::document_writes`]) wakes the bots that watch
//! that record. What a bot changed reaches the workspace as an ordinary change, so
//! members merge it like anyone's and history names the bot.
//!
//! The workspace's moderation agent is the first bot run this way: a report filed in
//! the workspace's Chat wakes it, and it looks at every person with a report it has not
//! looked at, so a report filed while the host was down is still answered at the next
//! start.

use std::collections::BTreeSet;

use tilo_action_policy::bot::{moderation_grant, moderation_grants};
use tilo_action_policy::{Change, Context, Grant};
use tilo_moderation::{Judge, ModerationAgent};

use crate::moderation::{ChatApi, Governance};
use crate::tl_chat;
use crate::workspaces::{ResourceWrite, Workspaces};

/// The workspace's moderation agent, installed by the workspace's root and woken by
/// reports filed in the workspace's Chat.
pub struct ModerationBot<J> {
    workspaces: Workspaces,
    workspace: String,
    agent: std::sync::Arc<ModerationAgent<J>>,
    /// The reports it has looked at, by id: one the model gave no verdict on waits for a
    /// human Moderator instead of being put to the model again on every write.
    looked: BTreeSet<String>,
}

impl<J: Judge + Sync + Send + 'static> ModerationBot<J> {
    /// The agent for `workspace`, governed here.
    #[must_use]
    pub fn new(workspaces: Workspaces, workspace: String, agent: ModerationAgent<J>) -> Self {
        Self {
            workspaces,
            workspace,
            agent: std::sync::Arc::new(agent),
            looked: BTreeSet::new(),
        }
    }

    /// Answer the workspace's reports at start, then each time a write to its Chat
    /// arrives, and every half minute for a person turning it on, until the workspace is no
    /// longer governed here.
    pub async fn run(mut self) {
        let mut writes = self.workspaces.document_writes();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        // The first tick is now; the first pass waits a whole period, as later ones do.
        tick.tick().await;
        loop {
            if let Err(error) = self.wake().await {
                tracing::warn!(workspace = %self.workspace, %error, "moderation agent did not finish a look");
            }
            loop {
                let next = tokio::select! {
                    _ = tick.tick() => break,
                    next = writes.recv() => next,
                };
                match next {
                    Ok(write) if self.watches(&write) => break,
                    Ok(_) => {}
                    // Missed writes are read from the record itself.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }

    fn watches(&self, write: &ResourceWrite) -> bool {
        write.workspace == self.workspace && write.app == tl_chat::app()
    }

    /// Decide the open reports of every person with one it has not looked at yet: while a
    /// person who governs the workspace has it turned on.
    ///
    /// # Errors
    /// The workspace's grants or its Chat could not be read, opened or written.
    pub async fn wake(&mut self) -> Result<(), String> {
        // Turned off, it looks at nothing: the reports wait until a person turns it on
        // again.
        if !enabled(&self.workspaces, &self.workspace).await? {
            return Ok(());
        }
        let packages = tl_chat::packages()?;
        let moderation = self
            .workspaces
            .with_tl_record(&self.workspace, &packages, tl_chat::viewer(), |opened| {
                crate::moderation::moderation(opened)
            })
            .await?;
        let fresh: Vec<String> = moderation
            .open_reports()
            .filter(|report| !self.looked.contains(&report.id))
            .map(|report| report.id.clone())
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        let mut people: Vec<_> = moderation
            .open_reports()
            .filter(|report| fresh.contains(&report.id))
            .map(|report| report.person.clone())
            .collect();
        people.sort();
        people.dedup();
        let (workspaces, governed) = (self.workspaces.clone(), self.workspace.clone());
        let agent = std::sync::Arc::clone(&self.agent);
        let handle = tokio::runtime::Handle::current();
        // The record is changed on a blocking thread, so the model's answer and the
        // workspace's grants are waited for there.
        self.workspaces
            .with_tl_record(
                &self.workspace,
                &packages,
                tl_chat::viewer(),
                move |opened| {
                    let mut governance = HostGovernance {
                        handle: handle.clone(),
                        workspaces: workspaces.clone(),
                        workspace: governed.clone(),
                        bot: tilo_action_policy::bot::MODERATION,
                    };
                    let mut api = ChatApi::with_governance(
                        opened,
                        tilo_clock::unix_now_ms_i64(),
                        Some(&mut governance),
                    );
                    let moderation = api.moderation()?;
                    for person in people {
                        let look = ModerationAgent::<J>::look(&mut api, &moderation, &person);
                        // A model that gives no verdict leaves the report open for a human
                        // Moderator.
                        let (verdict, model) = match handle.block_on(agent.verdict(&look)) {
                            Ok(answer) => answer,
                            Err(error) => {
                                tracing::warn!(%person, %error, "moderation model gave no verdict");
                                continue;
                            }
                        };
                        // A person who turned it off while the model was thinking has the
                        // last word: nothing is enforced on what it decided.
                        if !handle.block_on(enabled(&workspaces, &governed))? {
                            return Ok(());
                        }
                        let enforced =
                            ModerationAgent::<J>::enforce(&mut api, look, verdict, model);
                        for failure in enforced.failures {
                            tracing::warn!(%person, %failure, "moderation step failed");
                        }
                    }
                    Ok(())
                },
            )
            .await?;
        self.looked.extend(fresh);
        Ok(())
    }
}

/// The workspace's own membership actions, made as its moderation bot: the agent decides, and
/// the host that governs the workspace carries it out under the grants it holds for the bot.
struct HostGovernance {
    handle: tokio::runtime::Handle,
    workspaces: Workspaces,
    workspace: String,
    bot: &'static str,
}

impl Governance for HostGovernance {
    fn change(&mut self, change: Change) -> Result<(), String> {
        self.handle
            .block_on(
                self.workspaces
                    .moderate_member(&self.workspace, self.bot, change),
            )
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

/// Whether `recorded` is the moderation grant `expected` as this build writes it, on
/// `expected`'s node or, without `node`, on any.
fn is_moderation_grant(recorded: &Grant, expected: &Grant, node: bool) -> bool {
    recorded.id == expected.id
        && recorded.grantee == expected.grantee
        && (!node || recorded.node == expected.node)
        && recorded.kinds == expected.kinds
        && recorded.answer == expected.answer
}

/// Give `workspace` its moderation grants as the workspace's own person
/// ([`moderation_grants`]). A grant already recorded under an id is a workspace policy decision
/// and stays, unless it is this build's grant on another node: the grant from when reports were
/// filed in another app, which moves to the Chat they are filed in now.
///
/// # Errors
/// The workspace is not governed here, or a grant could not be written.
pub async fn enable(workspaces: &Workspaces, workspace: &str) -> Result<(), String> {
    let cert = workspaces.governor();
    let expected = moderation_grants(workspace, &hex::encode(cert.user_public_key));
    let recorded = workspaces
        .workspace_grants(workspace)
        .await
        .map_err(|error| error.to_string())?;
    let mut write = Vec::new();
    for grant in expected {
        let held = recorded.iter().find(|recorded| recorded.id == grant.id);
        match held {
            // As this build writes it, on its node.
            Some(held) if is_moderation_grant(held, &grant, true) => {}
            // Any other grant under the id is a workspace policy decision: this build does not
            // overwrite it, save a grant from before its node moved.
            Some(held) if !is_moderation_grant(held, &grant, false) => {}
            _ => write.push(grant),
        }
    }
    if write.is_empty() {
        return Ok(());
    }
    // The request is the grants as this build writes them: settling the same enable again
    // changes nothing.
    let written = serde_json::to_vec(&write)
        .map_err(|error| format!("the moderation bot's grants as written: {error}"))?;
    let request_id = *blake3::Hasher::new()
        .update(b"enable-moderation\0")
        .update(&written)
        .finalize()
        .as_bytes();
    workspaces
        .put_workspace_grants(workspace, cert, request_id, write)
        .await
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Whether a person who governs `workspace` turned its moderation agent on:
/// whether its governance holds the grant the agent moderates Chat by
/// ([`moderation_grant`]). The membership verbs it may take are granted beside it
/// ([`moderation_grants`]); one of those a person took away, or a workspace enabled before they
/// were given, leaves the bot moderating Chat and refused the membership change it asks for, so
/// turning moderation on never silently widens what the bot may do to the workspace's members.
///
/// # Errors
/// The workspace is not governed here.
pub async fn enabled(workspaces: &Workspaces, workspace: &str) -> Result<bool, String> {
    let expected = moderation_grant(
        workspace,
        &hex::encode(workspaces.device_cert().user_public_key),
    );
    let now = Context::at_time(tilo_clock::wall());
    Ok(workspaces
        .workspace_grants(workspace)
        .await
        .map_err(|error| error.to_string())?
        .iter()
        .any(|grant| is_moderation_grant(grant, &expected, true) && grant.is_live(&now)))
}

pub mod discord;
pub mod discord_voice;
pub mod installed;
pub mod membership;
pub mod requested;

#[cfg(test)]
mod tests;
