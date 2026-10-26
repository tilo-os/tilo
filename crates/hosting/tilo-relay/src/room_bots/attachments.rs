//! The files members post in a room's Chat, kept by the room's host so
//! they stay in the room after the member who posted them leaves.
//!
//! A member's device keeps what it posts and answers whoever asks the room
//! for it, as long as it is online. The host asks too: each time a member's
//! write reaches a part of the Chat's messages, it reads the attachments
//! there, and each time one reaches the Chat's own record, the pictures of
//! the feedback reports filed there ([`reported`]); it asks the room for
//! every one it does not keep yet, on its own presence, as any member asks.
//! A report's receipt waits for the host to keep its picture. The pieces members answer it with are
//! assembled and verified as a page assembles them
//! ([`tilo_browser_peer::content::ContentWants`]) and kept in the host's one
//! blob store, beside what its bots fetched ([`super::installed::FsBlobs`]),
//! which already answers members' requests for content the host keeps
//! ([`crate::rooms::RoomContent`]).
//!
//! What a host keeps is bounded ([`AttachmentLimits`]): a file larger than
//! the cap is not asked for or kept, and past a member's or the room's
//! quota the oldest files that member posted, or the room's oldest, go
//! first. A member's quota bounds what the host takes in of their files in
//! any hour too: past it, their next file is asked for once the hour has
//! room again. The pieces answering the host's requests count against these
//! quotas alone, not a member's general content budget
//! ([`crate::rooms::ContentKeeper::expects`]), so a file as large as the cap
//! reaches the host. A file the host kept before for another reason (its bots fetched
//! it) is neither counted nor ever let go of here. Which files it keeps,
//! and for whom is kept in a ledger beside the room's state, so
//! a restart keeps counting.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_browser_peer::content::ContentWants;
use tilo_lock::MutexRecover as _;

use super::CHAT;
use super::installed::FsBlobs;
use crate::rooms::{ContentKeeper, RecordWrite, Rooms};

/// How long the host waits before it asks again for files nobody answered.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(90);
/// The window a member's quota bounds what the host takes in of their
/// files over.
const HOUR_MS: u64 = 3_600_000;
/// How often the parts members wrote to are read for their files: a busy
/// Chat takes many writes a second, and one read finds all their files.
const READ_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// How much of members' posted files a host keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttachmentLimits {
    /// The largest file kept; a larger one stays with its poster.
    pub file_bytes: u64,
    /// What one member's files may take together; past it, that member's
    /// oldest go first. The host takes in no more of a member's files in
    /// any hour either.
    pub member_bytes: u64,
    /// What the room's files may take together; past it, the room's oldest
    /// go first.
    pub room_bytes: u64,
}

impl Default for AttachmentLimits {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            file_bytes: 25 * MIB,
            member_bytes: 250 * MIB,
            room_bytes: 2048 * MIB,
        }
    }
}

/// One file the host keeps for a member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    /// The participant id of the member who posted it.
    member: String,
    bytes: u64,
    /// The order it was kept in: the oldest goes first.
    order: u64,
}

/// The files kept for members, by hash.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    kept: BTreeMap<String, Kept>,
    next: u64,
    /// The files let go of past a quota: never asked for again, or every
    /// new file past the quota would bring the last one back.
    #[serde(default)]
    let_go: BTreeSet<String>,
}

impl Ledger {
    fn total(&self, member: Option<&str>) -> u64 {
        self.kept
            .values()
            .filter(|kept| member.is_none_or(|member| kept.member == member))
            .map(|kept| kept.bytes)
            .fold(0, u64::saturating_add)
    }

    /// The oldest file of `member`, or of anyone.
    fn oldest(&self, member: Option<&str>) -> Option<String> {
        self.kept
            .iter()
            .filter(|(_, kept)| member.is_none_or(|member| kept.member == member))
            .min_by_key(|(_, kept)| kept.order)
            .map(|(hash, _)| hash.clone())
    }
}

struct State {
    ledger: Ledger,
    /// The files asked for and not whole yet, named by who posted them.
    wants: ContentWants<String>,
    /// Parts that held files nobody answered for yet: read again later.
    unanswered: BTreeSet<String>,
    /// Parts members wrote to since they were last read.
    written: BTreeSet<String>,
    /// The files asked for within the last hour, by hash: who posted them,
    /// when and how large, which their poster's quota bounds.
    taken: BTreeMap<String, Taken>,
}

/// A file the host asked a member for.
struct Taken {
    member: String,
    at_ms: u64,
    bytes: u64,
}

/// The host's copy of the files a room's members post in its Chat.
pub struct RoomAttachments {
    rooms: Rooms,
    room: String,
    /// This host's device: what its requests name, and what answers to
    /// them are addressed to.
    host: [u8; 32],
    blobs: Arc<FsBlobs>,
    ledger: PathBuf,
    limits: AttachmentLimits,
    state: Mutex<State>,
}

/// A file a message of a part references: its hash, who posted it and the
/// size the message says it has, when it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Referenced {
    /// The content's hash, lowercase BLAKE3 hex.
    pub hash: String,
    /// The participant id of its poster.
    pub member: String,
    /// The size the message names, if any.
    pub size: Option<u64>,
}

/// The files the messages of `part`, a saved part of the Chat's messages,
/// reference.
#[must_use]
pub fn referenced(part: &[u8]) -> Vec<Referenced> {
    use chat::model::Attachment;
    let Ok(doc) = AutoCommit::load(part) else {
        return Vec::new();
    };
    chat::messages::read_part(&doc)
        .messages
        .values()
        .filter_map(|message| {
            let attachment = message.attachment.as_ref()?;
            let size = match attachment {
                Attachment::Image { .. } => None,
                Attachment::Video { size, .. } | Attachment::File { size, .. } => Some(*size),
            };
            Some(Referenced {
                hash: attachment.blob().to_ascii_lowercase(),
                member: message.author.0.clone(),
                size,
            })
        })
        .collect()
}

/// The pictures of the feedback reports filed in `record`, the saved Chat
/// record, each by whoever filed it.
#[must_use]
pub fn reported(record: &[u8]) -> Vec<Referenced> {
    let Ok(doc) = AutoCommit::load(record) else {
        return Vec::new();
    };
    let Some(chat) = chat::projection::read(&doc) else {
        // A record that does not read names no files; asking for any would
        // invent references, and there is nothing here to remove.
        return Vec::new();
    };
    chat.feedback
        .iter()
        .filter_map(|(_, report)| {
            let picture = report.submission.as_ref()?.feedback.screenshot.as_ref()?;
            Some(Referenced {
                hash: picture.hash.to_ascii_lowercase(),
                member: report.author.0.clone(),
                size: None,
            })
        })
        .collect()
}

/// The Chat's own record on the wire: its package primary.
const CHAT_RECORD: &str = "";

fn now_ms() -> u64 {
    tilo_clock::unix_now_ns() / 1_000_000
}

impl RoomAttachments {
    /// The host `host` of `room` keeping its members' files in `blobs`
    /// within `limits`, counted in the ledger at `ledger`.
    #[must_use]
    pub fn new(
        rooms: Rooms,
        room: String,
        host: [u8; 32],
        blobs: Arc<FsBlobs>,
        ledger: PathBuf,
        limits: AttachmentLimits,
    ) -> Arc<Self> {
        let kept = std::fs::read(&ledger)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Arc::new(Self {
            rooms,
            room,
            host,
            blobs,
            ledger,
            limits,
            state: Mutex::new(State {
                ledger: kept,
                wants: ContentWants::default(),
                unanswered: BTreeSet::new(),
                written: BTreeSet::new(),
                taken: BTreeMap::new(),
            }),
        })
    }

    /// Keep the room's members' files from now on: the pieces members send
    /// the host come here, every part of the Chat is read once, and each
    /// member write to a part after, until the room is no longer governed
    /// here.
    pub async fn run(self: Arc<Self>) {
        self.rooms.keep_content(self.clone());
        let mut writes = self.rooms.record_writes();
        let mut again = tokio::time::interval(ASK_AGAIN);
        again.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut read = tokio::time::interval(READ_EVERY);
        read.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        self.read_all().await;
        loop {
            tokio::select! {
                write = writes.recv() => match write {
                    Ok(write) => {
                        if let Some(part) = self.part_of(&write) {
                            self.state.lock_recover().written.insert(part);
                        }
                    }
                    // Missed writes are read from the parts themselves.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => self.read_all().await,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                _ = read.tick() => {
                    let parts = std::mem::take(&mut self.state.lock_recover().written);
                    for part in parts {
                        self.read_part(&part).await;
                    }
                }
                _ = again.tick() => {
                    let parts = std::mem::take(&mut self.state.lock_recover().unanswered);
                    for part in parts {
                        self.read_part(&part).await;
                    }
                }
            }
        }
    }

    /// The record of this room's Chat `write` went to, if it went to one
    /// that references files: a part of its messages or its own record,
    /// where feedback reports are filed.
    fn part_of(&self, write: &RecordWrite) -> Option<String> {
        if write.room != self.room || write.app != CHAT {
            return None;
        }
        if write.record == CHAT_RECORD {
            return Some(write.record.clone());
        }
        let id = ui::record::RecordId::parse(&write.record)?;
        chat::parts::part_index(&id)
            .is_some()
            .then(|| write.record.clone())
    }

    async fn read_all(&self) {
        let Ok(routes) = self.rooms.record_routes(&self.room).await else {
            return;
        };
        for (app, record) in routes {
            let write = RecordWrite {
                room: self.room.clone(),
                app,
                record,
            };
            if let Some(part) = self.part_of(&write) {
                self.read_part(&part).await;
            }
        }
    }

    /// Ask the room for the files `part` references that the host does not
    /// keep yet.
    async fn read_part(&self, part: &str) {
        let bytes = match self.rooms.record_document(&self.room, CHAT, part).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(room = %self.room, part, %error, "chat part not readable for its files");
                return;
            }
        };
        let files = if part == CHAT_RECORD {
            reported(&bytes)
        } else {
            referenced(&bytes)
        };
        let asks = self.asks(&files, now_ms());
        {
            let mut state = self.state.lock_recover();
            if files.iter().any(|file| self.lacks(&state, file)) {
                state.unanswered.insert(part.to_owned());
            }
        }
        for ask in asks {
            if let Err(error) = self.rooms.ask_members(&self.room, ask).await {
                tracing::debug!(room = %self.room, %error, "could not ask the room for a file");
            }
        }
    }

    /// The requests the host sends for the files of `referenced` it does
    /// not keep, within the cap and their poster's hourly quota, and not
    /// asked for lately.
    #[must_use]
    pub fn asks(&self, referenced: &[Referenced], now_ms: u64) -> Vec<Vec<u8>> {
        let mut state = self.state.lock_recover();
        state
            .taken
            .retain(|_, taken| now_ms.saturating_sub(taken.at_ms) < HOUR_MS);
        let mut asks = Vec::new();
        for file in referenced {
            if let Some(ask) = (self.lacks(&state, file) && self.within_hour(&state, file))
                .then(|| {
                    state.wants.ask(
                        &self.host,
                        file.member.clone(),
                        (CHAT, &self.room),
                        &file.hash,
                        now_ms,
                    )
                })
                .flatten()
            {
                state.taken.insert(
                    file.hash.clone(),
                    Taken {
                        member: file.member.clone(),
                        at_ms: now_ms,
                        bytes: file.size.unwrap_or(0),
                    },
                );
                asks.push(ask);
            }
        }
        drop(state);
        asks
    }

    /// Whether `file` fits what its poster's quota leaves of this hour.
    fn within_hour(&self, state: &State, file: &Referenced) -> bool {
        let taken = state
            .taken
            .iter()
            .filter(|(hash, taken)| taken.member == file.member && **hash != file.hash)
            .map(|(_, taken)| taken.bytes)
            .fold(0, u64::saturating_add);
        taken.saturating_add(file.size.unwrap_or(0)) <= self.limits.member_bytes
    }

    /// Whether the host lacks `file` and would keep it.
    fn lacks(&self, state: &State, file: &Referenced) -> bool {
        tilo_content::valid_hash(&file.hash)
            && file.size.is_none_or(|size| size <= self.limits.file_bytes)
            && !state.ledger.let_go.contains(&file.hash)
            && !self.blobs.contains(&file.hash)
    }

    /// Keep `bytes`, the whole and verified file `hash` `member` posted,
    /// and let the oldest files go past the member's and the room's quota.
    fn keep(&self, state: &mut State, member: &str, hash: &str, bytes: &[u8]) {
        let Ok(size) = u64::try_from(bytes.len()) else {
            return;
        };
        if size > self.limits.file_bytes || self.blobs.contains(hash) {
            return;
        }
        if let Err(error) = tilo_connector::BlobAccess::store(&*self.blobs, bytes) {
            tracing::warn!(room = %self.room, %error, "could not keep a member's file");
            return;
        }
        // A picture's message names no size: what came counts.
        if let Some(taken) = state.taken.get_mut(hash) {
            taken.bytes = size;
        }
        let order = state.ledger.next;
        state.ledger.next = order.saturating_add(1);
        state.ledger.kept.insert(
            hash.to_owned(),
            Kept {
                member: member.to_owned(),
                bytes: size,
                order,
            },
        );
        while state.ledger.total(Some(member)) > self.limits.member_bytes {
            let Some(oldest) = state.ledger.oldest(Some(member)) else {
                break;
            };
            self.let_go(state, &oldest);
        }
        while state.ledger.total(None) > self.limits.room_bytes {
            let Some(oldest) = state.ledger.oldest(None) else {
                break;
            };
            self.let_go(state, &oldest);
        }
        self.save(state);
    }

    fn let_go(&self, state: &mut State, hash: &str) {
        state.ledger.kept.remove(hash);
        state.ledger.let_go.insert(hash.to_owned());
        if let Err(error) = self.blobs.remove(hash) {
            tracing::warn!(room = %self.room, %error, "could not let a member's file go");
        }
    }

    fn save(&self, state: &State) {
        let saved = serde_json::to_vec(&state.ledger)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                tilo_files::native::atomic::write(&self.ledger, &bytes)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = saved {
            tracing::warn!(room = %self.room, %error, "could not save the ledger of kept files");
        }
    }

    /// Whether the host keeps file `hash` for a member, and for whom.
    #[must_use]
    pub fn kept_for(&self, hash: &str) -> Option<String> {
        self.state
            .lock_recover()
            .ledger
            .kept
            .get(hash)
            .map(|kept| kept.member.clone())
    }
}

impl ContentKeeper for RoomAttachments {
    fn piece(&self, room: &str, _from: [u8; 32], frame: &[u8]) {
        if room != self.room {
            return;
        }
        // A file over the cap is not taken in piece by piece either.
        if let Ok(neighbour_wire::NeighbourMsg::RecordContentPiece { total, .. }) =
            postcard::from_bytes(frame)
            && total > self.limits.file_bytes
        {
            return;
        }
        let mut state = self.state.lock_recover();
        if let Some(arrived) = state.wants.piece(&self.host, frame) {
            self.keep(&mut state, &arrived.tag, &arrived.hash, &arrived.bytes);
        }
    }

    fn expects(&self, room: &str, frame: &[u8]) -> bool {
        room == self.room
            && !matches!(
                postcard::from_bytes(frame),
                Ok(neighbour_wire::NeighbourMsg::RecordContentPiece { total, .. })
                    if total > self.limits.file_bytes
            )
            && self.state.lock_recover().wants.expects(&self.host, frame)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    /// The pictures of the reports filed in the Chat's record are the
    /// files the host keeps for them, each for whoever filed it; a report
    /// without one references nothing.
    #[test]
    fn a_filed_reports_picture_is_kept_for_its_reporter() {
        let mut doc = AutoCommit::new();
        chat::demo::seed(&mut doc).unwrap();
        let picture = "ab".repeat(32);
        let file = |id: &str, screenshot: Option<serde_json::Value>| {
            let mut feedback = serde_json::json!({
                "text": "The picker misses a row",
                "context": [],
            });
            if let Some(screenshot) = screenshot {
                feedback["screenshot"] = screenshot;
            }
            serde_json::json!({
                "id": id.repeat(32),
                "destination": {
                    "room": "room",
                    "provider": "record:so.tilo.chat:",
                    "channel": "c_general",
                },
                "feedback": feedback,
            })
        };
        let with_picture = serde_json::json!({
            "hash": picture.to_ascii_uppercase(),
            "width": 4,
            "height": 3,
        });
        for (author, submission) in [
            ("visitor:one", file("a", Some(with_picture))),
            ("visitor:two", file("b", None)),
        ] {
            chat::demo::apply_as(
                &mut doc,
                author,
                chat::Chat::SUBMIT_FEEDBACK.with(chat::record::chat::SubmitFeedback {
                    channel: tilo_resource::Id::from_raw("c_general"),
                    submission,
                }),
            )
            .unwrap();
        }
        assert_eq!(
            reported(&doc.save()),
            [Referenced {
                hash: picture,
                member: "visitor:one".into(),
                size: None,
            }]
        );
        assert!(reported(b"not a record").is_empty());
    }

    #[test]
    fn the_oldest_files_go_first_past_a_quota() {
        let mut ledger = Ledger::default();
        for (hash, member, order) in [("a", "ada", 0), ("b", "bo", 1), ("c", "ada", 2)] {
            ledger.kept.insert(
                hash.into(),
                Kept {
                    member: member.into(),
                    bytes: 10,
                    order,
                },
            );
        }
        assert_eq!(ledger.total(Some("ada")), 20);
        assert_eq!(ledger.total(None), 30);
        assert_eq!(ledger.oldest(Some("ada")).as_deref(), Some("a"));
        assert_eq!(ledger.oldest(Some("bo")).as_deref(), Some("b"));
        assert_eq!(ledger.oldest(None).as_deref(), Some("a"));
        let saved = serde_json::to_vec(&ledger).unwrap();
        let read: Ledger = serde_json::from_slice(&saved).unwrap();
        assert_eq!(read.kept, ledger.kept);
    }
}
