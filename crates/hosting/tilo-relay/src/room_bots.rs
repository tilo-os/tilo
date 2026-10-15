//! The bots installed in a room this host governs, run on the host.
//!
//! The host follows the room's records through its own copy: each member
//! write it takes in ([`Rooms::record_writes`]) wakes the bots that watch
//! that record, which read the changes after their cursor as record events
//! ([`tilo_agent_participant::bot::events_since`]). A bot acts through its
//! own confined replica of the record, as the installation for its
//! installer, and what it changed reaches the room as an ordinary change
//! the governor publishes, so members merge it like anyone's and history
//! names the bot.
//!
//! The room's moderation agent is the first bot run this way: a filed
//! report wakes it, and it looks at every person with open reports, so a
//! report filed while the host was down is still answered at the next
//! start.

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_action_policy::{Answer, Change, Grant, KindSet, RoomPolicy, Standing, Subject, TreePath};
use tilo_agent_participant::{AppCatalogue, Replica, ReplicaActor, Seed};
use tilo_moderation::{Judge, ModerationAgent};
use tilo_relay_session::rooms::RemoveMemberRequest;
use tilo_resource::Resource as _;
use tilo_user_identity::DeviceCert;

use crate::moderation::{ReplicaDoor, files_a_report};
use crate::rooms::{RecordWrite, Rooms};

/// The app whose record holds the room's reports and decisions.
const CHAT: &str = "so.tilo.chat";
/// The installation id the room's moderation agent acts as.
pub const MODERATION_INSTALLATION: &str = "moderation";

/// Enforces one of the moderation agent's governance changes on `room` as
/// the room's root, held to the room's policy like a Moderator's own.
///
/// # Errors
/// The change is not one the agent makes, names no valid person, or the
/// room refused it.
pub async fn govern(
    rooms: &Rooms,
    room: &str,
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
            rooms.restrict_member(room, host, request, *until_ms).await
        }
        Change::Unrestrict { .. } => rooms.unrestrict_member(room, host, request).await,
        Change::Ban { .. } => rooms.ban_member(room, host, request).await,
        _ => rooms.unban_member(room, host, request).await,
    };
    done.map(drop).map_err(|error| error.to_string())
}

/// The room's moderation agent, installed by the room's root and woken by
/// filed reports.
pub struct ModerationBot<J> {
    rooms: Rooms,
    room: String,
    host: DeviceCert,
    agent: std::sync::Arc<ModerationAgent<J>>,
    /// The chat record's heads the agent has read up to, hex; `None` until
    /// its first look, which answers whatever reports are open.
    cursor: Option<Vec<String>>,
}

impl<J: Judge + Sync + Send + 'static> ModerationBot<J> {
    /// The agent for `room`, governed here by `host` on the root's behalf.
    #[must_use]
    pub fn new(rooms: Rooms, room: String, host: DeviceCert, agent: ModerationAgent<J>) -> Self {
        Self {
            rooms,
            room,
            host,
            agent: std::sync::Arc::new(agent),
            cursor: None,
        }
    }

    /// Answer the room's reports at start, then each time a member's write
    /// to the chat files one, until the room is no longer governed here.
    pub async fn run(mut self) {
        let mut writes = self.rooms.record_writes();
        loop {
            if let Err(error) = self.wake().await {
                tracing::warn!(room = %self.room, %error, "moderation agent did not finish a look");
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
        write.room == self.room && write.app == CHAT && write.record.is_empty()
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
        let (reported, heads) = reports_since(&self.room, &bytes, self.cursor.as_deref())?;
        if !reported {
            self.cursor = Some(heads);
            return Ok(());
        }
        let root = hex::encode(self.host.user_public_key);
        let (rooms, governed, host) = (self.rooms.clone(), self.room.clone(), self.host.clone());
        let agent = std::sync::Arc::clone(&self.agent);
        let handle = tokio::runtime::Handle::current();
        // The replica runs app code in a confined worker and is driven
        // synchronously from one thread, so the whole look is held on a
        // blocking thread; the room's locks are only taken per call.
        let mutation = tokio::task::spawn_blocking(move || {
            let mut replica = bot_replica(&governed, &root, bytes)?;
            let mut govern =
                |change: Change| handle.block_on(govern(&rooms, &governed, &host, &change));
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
            Ok::<_, String>(replica.take_mutation())
        })
        .await
        .map_err(|error| error.to_string())??;
        if let Some(mutation) = mutation {
            self.publish(mutation).await?;
        }
        let bytes = self.record().await?.unwrap_or_default();
        self.cursor = Some(reports_since(&self.room, &bytes, None)?.1);
        Ok(())
    }

    async fn record(&self) -> Result<Option<Vec<u8>>, String> {
        self.rooms
            .record_document(&self.room, CHAT, "")
            .await
            .map_err(|error| error.to_string())
    }

    /// Put the bot's changes into the room's chat, as the governor
    /// publishes any change: they keep their own entries, so history names
    /// the bot for its installer.
    async fn publish(&self, mutation: DocumentMutation) -> Result<(), String> {
        let bytes = match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
        };
        self.rooms
            .author_application(
                &self.room,
                &self.host,
                &AppCatalogue::native(),
                CHAT,
                "",
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
    room: &str,
    bytes: &[u8],
    since: Option<&[String]>,
) -> Result<(bool, Vec<String>), String> {
    let mut doc = AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    let heads: Vec<String> = doc.get_heads().iter().map(ToString::to_string).collect();
    let Some(since) = since else {
        return Ok((true, heads));
    };
    let events = tilo_agent_participant::bot::events_since(
        room,
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

/// The moderation agent's replica of the room's chat: the root's
/// installation, granted reading and undoable changes in the room.
fn bot_replica(room: &str, installer: &str, bytes: Vec<u8>) -> Result<Replica, String> {
    let bot = Subject::bot(MODERATION_INSTALLATION);
    let grant = Grant::new(
        format!("bot:{MODERATION_INSTALLATION}"),
        bot.clone(),
        TreePath::room(room),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Subject::person(installer),
    );
    let standing =
        Standing::person(RoomPolicy::new(room, installer), installer).bounded_by(bot, vec![grant]);
    Replica::open(
        &AppCatalogue::native(),
        CHAT,
        room,
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
    .map_err(|error| error.to_string())
}

fn chat_door<'a>(
    replica: &'a mut Replica,
    govern: &'a mut dyn FnMut(Change) -> Result<(), String>,
) -> ReplicaDoor<'a, chat::Chat> {
    let now = tilo_clock::unix_now_ms_i64();
    ReplicaDoor::new(replica, |chat: &chat::Chat| &chat.moderation, govern, now)
}

/// The record of `app` a room keeps and seeds: the primary of an app that
/// keeps one per room (chat), else the room's default record, which every
/// member's first window of the app opens.
#[must_use]
pub fn room_record(catalogue: &tilo_agent_participant::AppCatalogue, app: &str) -> String {
    match catalogue.get(app) {
        Some(definition) if !definition.keeps_room_record() => {
            ui::record::RecordId::room_default().as_str().to_owned()
        }
        _ => String::new(),
    }
}

pub mod discord;
pub mod installed;
pub mod membership;

#[cfg(test)]
mod tests;
