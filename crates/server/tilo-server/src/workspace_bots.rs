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

use tilo_action_policy::bot::{MODERATION_GRANT, moderation_grant};
use tilo_action_policy::{Change, Grant};
use tilo_moderation::{Judge, ModerationAgent};
use tilo_server_session::workspaces::RemoveMemberRequest;
use tilo_user_identity::DeviceCert;

use crate::moderation::ChatApi;
use crate::tl_chat;
use crate::workspaces::{ResourceWrite, Workspaces};

/// Enforces one of the moderation agent's governance changes on `workspace` as
/// the workspace's root, held to the workspace's policy like a Moderator's own.
pub async fn govern(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    change: &Change,
) -> Result<(), String> {
    let person = match change {
        Change::Restrict { person, .. }
        | Change::Unrestrict { person }
        | Change::Ban { person }
        | Change::Unban { person } => person,
        other => return Err(format!("the moderation agent does not {}", other.action())),
    };
    let user_public_key: [u8; 32] = hex::decode(person)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| format!("{person} is not a person"))?;
    let request = RemoveMemberRequest {
        request_id: *blake3::hash(
            &serde_json::to_vec(&(change, tilo_clock::unix_now_ns())).unwrap_or_default(),
        )
        .as_bytes(),
        user_public_key,
    };
    let done = match change {
        Change::Restrict { until_ms, .. } => {
            workspaces
                .restrict_member(workspace, host, request, *until_ms)
                .await
        }
        Change::Unrestrict { .. } => workspaces.unrestrict_member(workspace, host, request).await,
        Change::Ban { .. } => workspaces.ban_member(workspace, host, request).await,
        _ => workspaces.unban_member(workspace, host, request).await,
    };
    done.map(drop).map_err(|error| error.to_string())
}

/// The workspace's moderation agent, installed by the workspace's root and woken by
/// reports filed in the workspace's Chat.
pub struct ModerationBot<J> {
    workspaces: Workspaces,
    workspace: String,
    host: DeviceCert,
    agent: std::sync::Arc<ModerationAgent<J>>,
    /// The reports it has looked at, by id: one the model gave no verdict on waits for a
    /// human Moderator instead of being put to the model again on every write.
    looked: BTreeSet<String>,
}

impl<J: Judge + Sync + Send + 'static> ModerationBot<J> {
    /// The agent for `workspace`, governed here by `host` on the root's behalf.
    #[must_use]
    pub fn new(
        workspaces: Workspaces,
        workspace: String,
        host: DeviceCert,
        agent: ModerationAgent<J>,
    ) -> Self {
        Self {
            workspaces,
            workspace,
            host,
            agent: std::sync::Arc::new(agent),
            looked: BTreeSet::new(),
        }
    }

    /// Answer the workspace's reports at start, then each time a write to its Chat
    /// arrives, until the workspace is no longer governed here.
    pub async fn run(mut self) {
        let mut writes = self.workspaces.document_writes();
        loop {
            if let Err(error) = self.wake().await {
                tracing::warn!(workspace = %self.workspace, %error, "moderation agent did not finish a look");
            }
            loop {
                match writes.recv().await {
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
        if installed_moderation_grant(&self.workspaces, &self.workspace)
            .await?
            .is_none()
        {
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
        let (workspaces, governed, host) = (
            self.workspaces.clone(),
            self.workspace.clone(),
            self.host.clone(),
        );
        let agent = std::sync::Arc::clone(&self.agent);
        let handle = tokio::runtime::Handle::current();
        // The record is changed on a blocking thread, so the model's answer and the
        // workspace's governance are waited for there.
        self.workspaces
            .with_tl_record(
                &self.workspace,
                &packages,
                tl_chat::viewer(),
                move |opened| {
                    let mut govern = |change: Change| {
                        handle.block_on(govern(&workspaces, &governed, &host, &change))
                    };
                    let mut api = ChatApi::new(opened, &mut govern, tilo_clock::unix_now_ms_i64());
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

/// Whether `recorded` is the moderation grant `expected` as this build writes it, on
/// `expected`'s node or, without `node`, on any.
fn is_moderation_grant(recorded: &Grant, expected: &Grant, node: bool) -> bool {
    recorded.id == expected.id
        && recorded.grantee == expected.grantee
        && (!node || recorded.node == expected.node)
        && recorded.kinds == expected.kinds
        && recorded.answer == expected.answer
        && recorded.lifetime == expected.lifetime
}

/// Give `workspace` its moderation grant as the workspace's own person. A grant already
/// recorded under that id is a workspace policy decision and stays, unless it is this
/// build's grant on another node: the grant from when reports were filed in another app,
/// which moves to the Chat they are filed in now.
///
/// # Errors
/// The workspace is not governed here, or the grant could not be written.
pub async fn enable(workspaces: &Workspaces, workspace: &str) -> Result<(), String> {
    let cert = workspaces.governor();
    let grant = moderation_grant(workspace, &hex::encode(cert.user_public_key));
    let recorded = workspaces
        .workspace_grants(workspace)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|recorded| recorded.id.as_str() == MODERATION_GRANT);
    if let Some(recorded) = recorded {
        let moved = is_moderation_grant(&recorded, &grant, false)
            && !is_moderation_grant(&recorded, &grant, true);
        if !moved {
            return Ok(());
        }
    }
    // The request is the grant as this build writes it: settling the same
    // enable again changes nothing.
    let written = serde_json::to_vec(&grant)
        .map_err(|error| format!("the moderation bot's grant as written: {error}"))?;
    let request_id = *blake3::Hasher::new()
        .update(b"enable-moderation\0")
        .update(&written)
        .finalize()
        .as_bytes();
    workspaces
        .put_workspace_grants(workspace, cert, request_id, vec![grant])
        .await
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Whether a person who governs `workspace` turned its moderation agent on:
/// whether its governance holds the grant the agent moderates Chat by
/// ([`moderation_grant`]).
///
/// # Errors
/// The workspace is not governed here.
pub async fn enabled(workspaces: &Workspaces, workspace: &str) -> Result<bool, String> {
    Ok(installed_moderation_grant(workspaces, workspace)
        .await?
        .is_some())
}

async fn installed_moderation_grant(
    workspaces: &Workspaces,
    workspace: &str,
) -> Result<Option<Grant>, String> {
    let expected = moderation_grant(
        workspace,
        &hex::encode(workspaces.governor().user_public_key),
    );
    let grants = workspaces
        .workspace_grants(workspace)
        .await
        .map_err(|error| error.to_string())?;
    Ok(grants
        .into_iter()
        .find(|grant| is_moderation_grant(grant, &expected, true)))
}

/// The record `doc` holds, read as this build's `R` once it is brought up to
/// `R`'s storage version by the app's own migration
/// ([`tilo_resource::evolution::migrate`]). A workspace an older server or older
/// devices wrote is read, never refused: a newer server starts on an older
/// server's state. The migration deletes nothing and rewrites no id, and it
/// is the same change every device migrating the record makes.
///
/// # Errors
/// The migration failed, or the migrated record does not read as `R`.
pub fn opened<R>(doc: &mut automerge::AutoCommit) -> Result<R::State, String>
where
    R: tilo_resource::Resource,
{
    tilo_resource::evolution::migrate::<R>(doc)
        .map_err(|error| format!("bring the record up to version {}: {error}", R::VERSION))?;
    R::read(doc).map_err(|error| error.to_string())
}

/// The record of `app` a workspace keeps and seeds: the primary of an app that
/// keeps one per workspace (chat), else the workspace's default record, which every
/// member's first window of the app opens.
#[must_use]
pub fn workspace_resource(catalogue: &tilo_agent_session::AppCatalogue, app: &str) -> String {
    match catalogue.get(app) {
        Some(definition) if !definition.keeps_workspace_resource() => {
            collab_types::document::DocumentId::workspace_default()
                .as_str()
                .to_owned()
        }
        _ => String::new(),
    }
}

pub mod assistant;
pub mod discord;
pub mod events;
pub mod initiative;
pub mod installed;
pub mod kept;
pub mod membership;
pub mod requested;
mod task_waits;

#[cfg(test)]
mod tests;
