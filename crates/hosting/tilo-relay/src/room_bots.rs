//! The bots installed in a workspace this host governs, run on the host.
//!
//! The host follows the workspace's records through its own copy: each member
//! write it takes in ([`Workspaces::record_writes`]) wakes the bots that watch
//! that record, which read the changes after their cursor as record events
//! ([`tilo_agent_participant::bot::events_since`]). A bot acts through its
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
use tilo_action_policy::{
    Actor, Answer, Change, Grant, KindSet, MemberId, Standing, Subject, TreePath, WorkspacePolicy,
};
use tilo_agent_participant::{AppCatalogue, Replica, ReplicaActor, Seed};
use tilo_moderation::{Judge, ModerationAgent};
use tilo_relay_session::workspaces::RemoveMemberRequest;
use tilo_resource::Resource as _;
use tilo_user_identity::DeviceCert;

use crate::moderation::{ReplicaDoor, files_a_report};
use crate::workspaces::{RecordWrite, Workspaces};

/// The app whose record holds the workspace's reports and decisions.
const CHAT: &str = "so.tilo.chat";
/// The installation id the workspace's moderation agent acts as.
pub const MODERATION_INSTALLATION: &str = "moderation";

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
/// filed reports.
pub struct ModerationBot<J> {
    workspaces: Workspaces,
    workspace: String,
    host: DeviceCert,
    agent: std::sync::Arc<ModerationAgent<J>>,
    /// The chat record's heads the agent has read up to, hex; `None` until
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
        agent: ModerationAgent<J>,
    ) -> Self {
        Self {
            workspaces,
            workspace,
            host,
            agent: std::sync::Arc::new(agent),
            cursor: None,
        }
    }

    /// Answer the workspace's reports at start, then each time a member's write
    /// to the chat files one, until the workspace is no longer governed here.
    pub async fn run(mut self) {
        let mut writes = self.workspaces.record_writes();
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

    fn watches(&self, write: &RecordWrite) -> bool {
        write.workspace == self.workspace && write.app == CHAT && write.record.is_empty()
    }

    /// Read the chat's changes after the cursor and, when one of them filed
    /// a report (or on the first look), decide every open report.
    ///
    /// # Errors
    /// The chat record could not be read, opened or written.
    pub async fn wake(&mut self) -> Result<(), String> {
        let Some(bytes) = self.record().await? else {
            return Ok(());
        };
        let (reported, heads) = reports_since(&self.workspace, &bytes, self.cursor.as_deref())?;
        if !reported {
            self.cursor = Some(heads);
            return Ok(());
        }
        let root = hex::encode(self.host.user_public_key);
        let parts = self.parts().await?;
        let (workspaces, governed, host) = (
            self.workspaces.clone(),
            self.workspace.clone(),
            self.host.clone(),
        );
        let agent = std::sync::Arc::clone(&self.agent);
        let handle = tokio::runtime::Handle::current();
        // The replica runs app code in a confined worker and is driven
        // synchronously from one thread, so the whole look is held on a
        // blocking thread; the workspace's locks are only taken per call.
        let (mutation, beside) = tokio::task::spawn_blocking(move || {
            let mut replica = bot_replica(&governed, &root, bytes, parts)?;
            let mut govern =
                |change: Change| handle.block_on(govern(&workspaces, &governed, &host, &change));
            let mut door = chat_door(&mut replica, &mut govern);
            let moderation = door.moderation().unwrap_or_default();
            for person in ModerationAgent::<J>::reported(&moderation) {
                let look = ModerationAgent::<J>::look(&mut door, &moderation, &person);
                // A model that gives no verdict leaves the report open for
                // a human Moderator.
                let (verdict, model) = match handle.block_on(agent.verdict(&look)) {
                    Ok(answer) => answer,
                    Err(error) => {
                        tracing::warn!(%person, %error, "moderation model gave no verdict");
                        continue;
                    }
                };
                let enforced = ModerationAgent::<J>::enforce(&mut door, look, verdict, model);
                for failure in enforced.failures {
                    tracing::warn!(%person, %failure, "moderation step failed");
                }
            }
            let taken = |error: tilo_agent_participant::AppError| error.to_string();
            Ok::<_, String>((
                replica.take_mutation().map_err(taken)?,
                replica.take_record_mutations().map_err(taken)?,
            ))
        })
        .await
        .map_err(|error| error.to_string())??;
        if let Some(mutation) = mutation {
            self.publish("", mutation).await?;
        }
        // What it took back of the messages is in the chat's parts.
        for (record, mutation) in beside {
            self.publish(record.as_str(), mutation).await?;
        }
        let bytes = self.record().await?.unwrap_or_default();
        self.cursor = Some(reports_since(&self.workspace, &bytes, None)?.1);
        Ok(())
    }

    /// The parts of the chat's messages the workspace holds, each whole.
    async fn parts(&self) -> Result<Vec<(ui::record::RecordId, Vec<u8>)>, String> {
        let mut parts = Vec::new();
        for (app, record) in self
            .workspaces
            .record_routes(&self.workspace)
            .await
            .map_err(|error| error.to_string())?
        {
            let Some(part) = ui::record::RecordId::parse(&record)
                .filter(|record| app == CHAT && chat::parts::part_index(record).is_some())
            else {
                continue;
            };
            if let Some(bytes) = self
                .workspaces
                .record_document(&self.workspace, CHAT, &record)
                .await
                .map_err(|error| error.to_string())?
            {
                parts.push((part, bytes));
            }
        }
        Ok(parts)
    }

    async fn record(&self) -> Result<Option<Vec<u8>>, String> {
        self.workspaces
            .record_document(&self.workspace, CHAT, "")
            .await
            .map_err(|error| error.to_string())
    }

    /// Put the bot's changes into the workspace's chat record (`""`) or one of
    /// its parts, as the governor publishes any change: they keep their own
    /// entries, so history names the bot for its installer.
    async fn publish(&self, record: &str, mutation: DocumentMutation) -> Result<(), String> {
        let bytes = match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
        };
        self.workspaces
            .author_application(
                &self.workspace,
                &self.host,
                &AppCatalogue::native(),
                CHAT,
                record,
                |doc| {
                    tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                        .map(drop)
                        .map_err(|error| error.to_string())
                },
            )
            .await
            .map_err(|error| error.to_string())
    }
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
    let events = tilo_agent_participant::bot::events_since(
        workspace,
        "",
        chat::Chat::KIND,
        doc.document(),
        since,
    );
    Ok((
        events.iter().any(|(event, _)| files_a_report(&event.did)),
        heads,
    ))
}

/// The moderation agent's replica of the workspace's chat, holding the parts of
/// its messages beside it: the root's installation, granted reading and
/// undoable changes in the workspace.
fn bot_replica(
    workspace: &str,
    installer: &str,
    bytes: Vec<u8>,
    parts: Vec<(ui::record::RecordId, Vec<u8>)>,
) -> Result<Replica, String> {
    let bot = Subject::bot(MODERATION_INSTALLATION);
    let grant = Grant::new(
        format!("bot:{MODERATION_INSTALLATION}"),
        bot,
        TreePath::workspace(workspace),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Subject::person(installer),
    );
    let standing = Standing::person(WorkspacePolicy::new(workspace, installer), installer)
        .acting_as(
            Actor::Member(MemberId::bot(MODERATION_INSTALLATION)),
            [grant],
        );
    let mut replica = Replica::open(
        &AppCatalogue::native(),
        CHAT,
        workspace,
        &format!("bot:{MODERATION_INSTALLATION}"),
        ReplicaActor::Bot {
            installation: MODERATION_INSTALLATION.into(),
            installer: installer.into(),
            standing,
        },
        Seed::Existing {
            record: ui::record::RecordId::package_primary(),
            changes: vec![bytes],
        },
    )
    .map_err(|error| error.to_string())?;
    for (part, bytes) in parts {
        // A part it cannot hold leaves only that part's posts out of its
        // look, not every report.
        if let Err(error) = replica.apply_record(&part, &DocumentMutation::Snapshot(bytes)) {
            tracing::warn!(record = %part.as_str(), %error, "moderation agent leaves out a part of the chat");
        }
    }
    Ok(replica)
}

fn chat_door<'a>(
    replica: &'a mut Replica,
    govern: &'a mut dyn FnMut(Change) -> Result<(), String>,
) -> ReplicaDoor<'a, chat::Chat> {
    let now = tilo_clock::unix_now_ms_i64();
    ReplicaDoor::new(replica, |chat: &chat::Chat| &chat.moderation, govern, now)
}

/// The record `doc` holds, read as this build's `R` once it is brought up to
/// `R`'s storage version by the app's own migration
/// ([`tilo_resource::evolution::migrate`]). A workspace an older relay or older
/// devices wrote is read, never refused: a newer relay starts on an older
/// relay's state. The migration deletes nothing and rewrites no id, and it
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
pub fn workspace_record(catalogue: &tilo_agent_participant::AppCatalogue, app: &str) -> String {
    match catalogue.get(app) {
        Some(definition) if !definition.keeps_workspace_record() => {
            ui::record::RecordId::workspace_default()
                .as_str()
                .to_owned()
        }
        _ => String::new(),
    }
}

pub mod attachments;
pub mod discord;
pub mod installed;
pub mod membership;
pub mod requested;

#[cfg(test)]
mod tests;
