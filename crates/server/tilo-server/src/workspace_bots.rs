//! The bots installed in a workspace this host governs, run on the host.
//!
//! The host follows the workspace's records through its own copy: each member
//! write it takes in ([`Workspaces::document_writes`]) wakes the bots that watch
//! that record, which read the changes after their cursor as record events
//! ([`tilo_agent_session::bot::events_since`]). A bot acts through its
//! own confined replica of the record, as the installation for its
//! installer, and what it changed reaches the workspace as an ordinary change
//! the governor publishes, so members merge it like anyone's and history
//! names the bot.
//!
//! The workspace's moderation agent is the first bot run this way: a filed
//! report wakes it, and it looks at every person with open reports, so a
//! report filed while the host was down is still answered at the next
//! start.

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_action_policy::bot::{MODERATION, MODERATION_GRANT, moderation_grant};
use tilo_action_policy::{ActionKind, Grant, MemberAccess};
use tilo_actions::MemberId;
use tilo_agent_session::{AppCatalogue, Replica, ReplicaActor, Seed};
use tilo_moderation::{Judge, ModerationAgent};
use tilo_resource::Resource as _;
use tilo_user_identity::DeviceCert;

use crate::moderation::{ReplicaApi, files_a_report};
use crate::workspaces::{ResourceWrite, Workspaces};
use chat::{MODERATION_DOCUMENT, Moderation};

/// The app whose record holds the workspace's reports and decisions.
use tilo_app_id::apps::CHAT;

/// The workspace's moderation agent, installed by the workspace's root and woken by
/// filed reports.
pub struct ModerationBot<J> {
    workspaces: Workspaces,
    workspace: String,
    host: DeviceCert,
    agent: std::sync::Arc<ModerationAgent<J>>,
    certifier: membership::HostRootCertifier,
    /// The moderation record's heads the agent has read up to, hex; `None` until
    /// its first look, which answers whatever reports are open.
    cursor: Option<Vec<String>>,
}

impl<J: Judge + Sync + Send + 'static> ModerationBot<J> {
    /// The agent for `workspace`, governed here by `host` on the root's behalf.
    #[must_use]
    pub fn new(
        workspaces: Workspaces,
        workspace: String,
        host: DeviceCert,
        owner_root: ed25519_dalek::SigningKey,
        agent: ModerationAgent<J>,
    ) -> Self {
        Self {
            workspaces,
            workspace,
            host,
            certifier: membership::HostRootCertifier::new(owner_root),
            agent: std::sync::Arc::new(agent),
            cursor: None,
        }
    }

    /// Answer the workspace's reports at start, then each time a member's write
    /// to the moderation record files one, until the workspace is no longer governed here.
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
        write.workspace == self.workspace
            && write.app == CHAT
            && write.record == MODERATION_DOCUMENT
    }

    /// Read the moderation record's changes after the cursor and, when one of them filed
    /// a report (or on the first look), decide every open report: while a
    /// person who governs the workspace has it turned on.
    ///
    /// # Errors
    /// The workspace's grants or records could not be read, opened or written.
    pub async fn wake(&mut self) -> Result<(), String> {
        // Turned off, it looks at nothing: the reports wait from where it
        // left off until a person turns it on again.
        if !enabled(&self.workspaces, &self.workspace).await? {
            return Ok(());
        }
        let Some(bytes) = self.document(MODERATION_DOCUMENT).await? else {
            return Ok(());
        };
        let (reported, heads) = reports_since(&self.workspace, &bytes, self.cursor.as_deref())?;
        if !reported {
            self.cursor = Some(heads);
            return Ok(());
        }
        self.keep_bot().await?;
        let bot = self.certifier.device_key(MODERATION);
        let device = bot.verifying_key().to_bytes();
        let access = self
            .workspaces
            .bot_access(&self.workspace, device)
            .await
            .map_err(|error| error.to_string())?;
        let decision = tilo_actions::action_node(
            &access.policy.node(),
            CHAT,
            Moderation::KIND,
            MODERATION_DOCUMENT,
            Some(tilo_action_policy::roles::MODERATION_FIELD),
            "decide-reports",
        );
        if !reads(&access, MODERATION_DOCUMENT)
            || !access.allows(&decision, ActionKind::Undoable, tilo_clock::wall())
        {
            return Err("the moderation bot may not read and decide reports".into());
        }
        let chat = if reads(&access, "") {
            self.document("").await?
        } else {
            None
        };
        let parts = self.parts(&access).await?;
        let workspaces = self.workspaces.clone();
        let governed = self.workspace.clone();
        let reports = moderation_id()?;
        let agent = std::sync::Arc::clone(&self.agent);
        let handle = tokio::runtime::Handle::current();
        // Ask the judge off the workspace lock. The replica keeps only the
        // report IDs sent for judgment, even if new reports arrive meanwhile.
        let (mutation, mut beside) = tokio::task::spawn_blocking(move || {
            let mut replica = bot_replica(&governed, access, chat, bytes, parts)?;
            let mut api = ReplicaApi::new(&mut replica, reports, tilo_clock::unix_now_ms_i64());
            let moderation = api
                .moderation()
                .ok_or("the workspace moderation record did not read")?;
            for person in ModerationAgent::<J>::reported(&moderation) {
                let look = ModerationAgent::<J>::look(&mut api, &moderation, &person);
                let (verdict, model) = match handle.block_on(agent.verdict(&look)) {
                    Ok(answer) => answer,
                    Err(error) => {
                        tracing::warn!(%person, %error, "moderation model gave no verdict");
                        continue;
                    }
                };
                // A governance change while the model was thinking takes
                // effect before it can enforce this verdict.
                let current = handle
                    .block_on(workspaces.bot_access(&governed, device))
                    .map_err(|error| error.to_string())?;
                if !reads(&current, MODERATION_DOCUMENT)
                    || !current.allows(&decision, ActionKind::Undoable, tilo_clock::wall())
                {
                    return Err("the moderation bot's grant no longer reaches this decision".into());
                }
                let enforced = ModerationAgent::<J>::enforce(&mut api, look, verdict, model);
                for failure in enforced.failures {
                    tracing::warn!(%person, %failure, "moderation step failed");
                }
            }
            let taken = |error: tilo_agent_session::AppError| error.to_string();
            Ok::<_, String>((
                replica.take_mutation().map_err(taken)?,
                replica.take_document_mutations().map_err(taken)?,
            ))
        })
        .await
        .map_err(|error| error.to_string())??;
        if let Some(mutation) = mutation {
            self.publish(&bot, "", mutation).await?;
        }
        // Carry out Chat's undos before publishing the decision that names them.
        beside.sort_by_key(|(record, _)| record.as_str() == MODERATION_DOCUMENT);
        for (record, mutation) in beside {
            self.publish(&bot, record.as_str(), mutation).await?;
        }
        // Remember only what the bot looked at, not reports filed during judgment.
        self.cursor = Some(heads);
        Ok(())
    }

    async fn keep_bot(&self) -> Result<(), String> {
        let installed_at_ms = self
            .workspaces
            .installed_bot(&self.workspace, MODERATION)
            .await
            .map_err(|error| error.to_string())?
            .map_or_else(tilo_clock::unix_now_ms_i64, |bot| {
                bot.installation.installed_at_ms
            });
        let shortcut = tilo_automation::Shortcut {
            id: MODERATION.into(),
            name: "Moderation".into(),
            inputs: Vec::new(),
            steps: Vec::new(),
            step_timeout_secs: None,
            output: None,
            grant: Vec::new(),
            triggers: Vec::new(),
            saved_by: None,
            saved_proof: None,
        };
        // The built-in runner uses the installed Chat-only grant. The
        // governed identity adds no program grants.
        let program = tilo_automation::program::canonical_bytes(&shortcut)
            .map_err(|error| error.to_string())?;
        let bot = installed::InstalledBot {
            installation: MODERATION.into(),
            entry: MODERATION.into(),
            name: shortcut.name,
            program: String::from_utf8(program).map_err(|error| error.to_string())?,
            inputs: serde_json::Map::new(),
            installed_at_ms,
            release: String::new(),
        };
        membership::keep_member(
            &self.workspaces,
            &self.workspace,
            &self.host,
            &self.certifier,
            &bot,
        )
        .await?;
        Ok(())
    }

    /// The parts of the chat's messages the workspace holds, each whole.
    async fn parts(
        &self,
        access: &MemberAccess,
    ) -> Result<Vec<(collab_types::document::DocumentId, Vec<u8>)>, String> {
        let mut parts = Vec::new();
        for (app, record) in self
            .workspaces
            .document_routes(&self.workspace)
            .await
            .map_err(|error| error.to_string())?
        {
            let Some(part) = collab_types::document::DocumentId::parse(&record).filter(|record| {
                app == CHAT
                    && chat::parts::part_index(record).is_some()
                    && reads(access, record.as_str())
            }) else {
                continue;
            };
            if let Some(bytes) = self
                .workspaces
                .resource_document(&self.workspace, CHAT, &record)
                .await
                .map_err(|error| error.to_string())?
            {
                parts.push((part, bytes));
            }
        }
        Ok(parts)
    }

    async fn document(&self, record: &str) -> Result<Option<Vec<u8>>, String> {
        self.workspaces
            .resource_document(&self.workspace, CHAT, record)
            .await
            .map_err(|error| error.to_string())
    }

    async fn publish(
        &self,
        bot: &ed25519_dalek::SigningKey,
        record: &str,
        mutation: DocumentMutation,
    ) -> Result<(), String> {
        let bytes = match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
        };
        self.workspaces
            .publish_as_bot(
                &self.workspace,
                &self.host,
                bot,
                &AppCatalogue::native(),
                CHAT,
                record,
                &bytes,
            )
            .await
            .map_err(|error| error.to_string())
    }
}

fn reads(access: &MemberAccess, record: &str) -> bool {
    let node = collab_types::document_node(access.workspace(), CHAT, record);
    access.allows(&node, ActionKind::Read, tilo_clock::wall())
}

/// Whether a change after `since` filed a report (always, with no cursor
/// yet), and the record's heads, hex.
fn reports_since(
    workspace: &str,
    bytes: &[u8],
    since: Option<&[String]>,
) -> Result<(bool, Vec<String>), String> {
    let mut doc = AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    let heads: Vec<String> = doc.get_heads().iter().map(ToString::to_string).collect();
    let Some(since) = since else {
        return Ok((true, heads));
    };
    let events = tilo_agent_session::bot::events_since(
        workspace,
        MODERATION_DOCUMENT,
        Moderation::KIND,
        doc.document(),
        since,
    );
    Ok((
        events.iter().any(|(event, _)| files_a_report(&event.did)),
        heads,
    ))
}

/// Give `workspace` its initial moderation grant as the workspace's own person.
/// A grant already recorded under that id is a workspace policy decision.
///
/// # Errors
/// The workspace is not governed here, or the grant could not be written.
pub async fn enable(workspaces: &Workspaces, workspace: &str) -> Result<(), String> {
    if workspaces
        .workspace_grants(workspace)
        .await
        .map_err(|error| error.to_string())?
        .iter()
        .any(|grant| grant.id.as_str() == MODERATION_GRANT)
    {
        return Ok(());
    }
    let cert = workspaces.device_cert();
    let grant = moderation_grant(workspace, &hex::encode(cert.user_public_key));
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
        &hex::encode(workspaces.device_cert().user_public_key),
    );
    let grants = workspaces
        .workspace_grants(workspace)
        .await
        .map_err(|error| error.to_string())?;
    Ok(grants.into_iter().find(|grant| {
        grant.id == expected.id
            && grant.grantee == expected.grantee
            && grant.node == expected.node
            && grant.kinds == expected.kinds
            && grant.answer == expected.answer
            && grant.is_live(&tilo_action_policy::Context::at_time(tilo_clock::wall()))
    }))
}

fn moderation_id() -> Result<collab_types::document::DocumentId, String> {
    collab_types::document::DocumentId::parse(MODERATION_DOCUMENT)
        .ok_or_else(|| "the moderation record has no document id".to_owned())
}

fn bot_replica(
    workspace: &str,
    access: MemberAccess,
    chat: Option<Vec<u8>>,
    reports: Vec<u8>,
    parts: Vec<(collab_types::document::DocumentId, Vec<u8>)>,
) -> Result<Replica, String> {
    let installer = access
        .person_id()
        .ok_or("the moderation bot has no installer")?
        .to_owned();
    let seed = chat.map_or(Seed::New, |bytes| Seed::Existing {
        record: collab_types::document::DocumentId::package_primary(),
        changes: vec![bytes],
    });
    let mut replica = Replica::open(
        &AppCatalogue::native(),
        CHAT,
        workspace,
        &MemberId::bot(MODERATION),
        ReplicaActor::Bot {
            installation: MODERATION.into(),
            installer,
            access,
        },
        seed,
    )
    .map_err(|error| error.to_string())?;
    replica
        .apply_document(&moderation_id()?, &DocumentMutation::Snapshot(reports))
        .map_err(|error| error.to_string())?;
    // The bot opens moderation and message history. Answer the other
    // workspace routes so the replica can publish the records it holds.
    for resource in collab_types::WORKSPACE_DOCUMENTS
        .iter()
        .filter(|resource| resource.app == CHAT && resource.kind != Moderation::KIND)
    {
        let id = collab_types::document::DocumentId::parse(&collab_types::workspace_document_id(
            resource.kind,
        ))
        .ok_or("a Chat workspace resource has no document id")?;
        let answer = if resource.kind == "tilo.workspace.emoji" {
            collaboration::resource::WorkspaceAnswer::HoldsNone
        } else {
            collaboration::resource::WorkspaceAnswer::NotYours
        };
        replica.workspace_answered(id, answer);
    }
    for (part, bytes) in parts {
        if let Err(error) = replica.apply_document(&part, &DocumentMutation::Snapshot(bytes)) {
            tracing::warn!(record = %part.as_str(), %error, "moderation agent leaves out a part of the chat");
        }
    }
    Ok(replica)
}

pub mod attachments;
pub mod discord;
pub mod discord_voice;
pub mod installed;
pub mod membership;
pub mod requested;

#[cfg(test)]
mod tests;
