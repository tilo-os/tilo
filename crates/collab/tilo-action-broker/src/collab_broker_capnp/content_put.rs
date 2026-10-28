//! The content put door: a session client uploads bytes, chunk by chunk,
//! into its room's content store and gets back `tilo.content:<hash>`.
//!
//! Actions carry intent and references; bytes travel by session transport
//! into the content store.
//!
//! A put is decided at its own node, `tilo.content/…/put` in the room
//! ([`put_node`]): room rules reach it as they reach any node, an app's
//! rules do not. Puts are invisible to everyone until an action at its own
//! node references them; this node governs storage and tagging only, and a
//! successful put is never authority over any document.

use std::sync::{Arc, Weak};

use tilo_action_policy::{ActionKind, TreePath};
use tilo_actions::remote::{
    ActionHostError, CONTENT_PUTS_UNAVAILABLE, Caller, ContentPut, ContentPuts,
};
use tilo_desktop_model::Workspace;

use super::CapnpBrokerState;
use super::invocation_journal::ContentPutRecord;
use crate::services::{CONTENT_PUT_SCOPE, ContentSink, ContentStoreServices};

/// What a caller the room does not let add content is told: what it may
/// not do, and the one rule that would let it.
pub const CONTENT_PUT_REFUSED: &str =
    "not allowed to add content to this room: allow content uploads in this room to let it";

/// The node a put into `workspace` is decided at.
fn put_node(workspace: &Workspace) -> TreePath {
    tilo_actions::action_node(
        &workspace.node(),
        CONTENT_PUT_SCOPE,
        CONTENT_PUT_SCOPE,
        "content",
        None,
        "put",
    )
}

impl CapnpBrokerState {
    /// The content put door, when this host has a content store.
    pub fn content_puts(self: &Arc<Self>) -> Option<Arc<dyn ContentPuts>> {
        let store = self.services.content_store.clone()?;
        Some(Arc::new(BrokerContentPuts {
            state: Arc::downgrade(self),
            store,
        }))
    }

    /// Whether `caller` may add content to `room` now, with the policy
    /// time it was decided at. An answer that would ask first refuses: a
    /// put asks nobody and spends no once-grant.
    fn may_put_content(&self, caller: &Caller, room: &str) -> Result<u64, ActionHostError> {
        super::action_host::check_execution(caller)?;
        match self.decide_for(caller, room, |workspace, actor, now| {
            actor
                .may_in(workspace, &put_node(workspace), ActionKind::Undoable, now)
                .then_some(now)
        }) {
            Some(Some(now)) => Ok(now),
            Some(None) => Err(ActionHostError::Refused {
                reason: CONTENT_PUT_REFUSED.into(),
            }),
            None => Err(ActionHostError::NotPermitted),
        }
    }

    /// Note who put `hash` into `room`, durably, before answering its
    /// reference. Attribution only: it is never read for access.
    fn attribute_content_put(
        &self,
        caller: &Caller,
        room: &str,
        at: u64,
        (hash, len): (&str, u64),
    ) -> Result<(), ActionHostError> {
        let unavailable = |reason: String| ActionHostError::Unavailable { reason };
        let actor = self.caller_actor(caller, room).id;
        let durable = {
            let mut journal = self
                .journal
                .lock()
                .map_err(|_| unavailable("invocation journal unavailable".into()))?;
            match &mut *journal {
                Err(error) => return Err(unavailable(error.clone())),
                // A broker keeping no durable records keeps none of this either.
                Ok(None) => return Ok(()),
                Ok(Some(journal)) => journal
                    .record_content_put(&ContentPutRecord {
                        at,
                        actor,
                        room: room.to_owned(),
                        hash: hash.to_owned(),
                        len,
                    })
                    .map_err(unavailable)?,
            }
        };
        // The journal is free for others while the line reaches the disk;
        // an upload finishes on a blocking thread, never the broker's loop.
        durable
            .wait_blocking()
            .map_err(|error| unavailable(format!("content put attribution not durable: {error}")))
    }
}

/// A lowercase 64-character hex BLAKE3 digest, as the store answers.
fn is_content_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The broker, while it runs.
fn broker(state: &Weak<CapnpBrokerState>) -> Result<Arc<CapnpBrokerState>, ActionHostError> {
    state.upgrade().ok_or_else(|| ActionHostError::Unavailable {
        reason: CONTENT_PUTS_UNAVAILABLE.into(),
    })
}

struct BrokerContentPuts {
    state: Weak<CapnpBrokerState>,
    store: Arc<dyn ContentStoreServices>,
}

impl ContentPuts for BrokerContentPuts {
    fn begin(&self, caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionHostError> {
        let room = caller
            .room()
            .ok_or_else(|| ActionHostError::InvalidRequest {
                reason: "content goes to a room, and this session is in none".into(),
            })?
            .to_owned();
        let max = self.store.max_bytes();
        if size == 0 || size > max {
            return Err(ActionHostError::InvalidRequest {
                reason: format!("an upload is 1 to {max} bytes, not {size}"),
            });
        }
        broker(&self.state)?.may_put_content(caller, &room)?;
        let sink = self
            .store
            .begin(&room)
            .map_err(|reason| ActionHostError::Unavailable { reason })?;
        Ok(Box::new(BrokerContentPut {
            state: Weak::clone(&self.state),
            caller: caller.clone(),
            room,
            size,
            written: 0,
            hasher: blake3::Hasher::new(),
            sink: Some(sink),
        }))
    }
}

/// One upload. Dropping it drops the store's upload, which stores nothing.
struct BrokerContentPut {
    state: Weak<CapnpBrokerState>,
    caller: Caller,
    room: String,
    size: u64,
    written: u64,
    /// The digest of what was appended, known before the store keeps it, so
    /// the put is recorded before the room references the content.
    hasher: blake3::Hasher,
    /// `None` once the store refused a chunk: the upload is over.
    sink: Option<Box<dyn ContentSink>>,
}

impl BrokerContentPut {
    fn ended() -> ActionHostError {
        ActionHostError::Refused {
            reason: "this upload failed and is over: begin it again".into(),
        }
    }
}

impl ContentPut for BrokerContentPut {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        self.written
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionHostError> {
        let written = self.written;
        if offset != written {
            return Err(ActionHostError::InvalidRequest {
                reason: format!("the upload has {written} bytes: append at offset {written}"),
            });
        }
        let end = written.saturating_add(chunk.len() as u64);
        if end > self.size {
            return Err(ActionHostError::InvalidRequest {
                reason: format!(
                    "the chunk ends at {end}, past the {} bytes the upload began with",
                    self.size
                ),
            });
        }
        let sink = self.sink.as_mut().ok_or_else(Self::ended)?;
        if let Err(reason) = sink.append(chunk) {
            self.sink = None;
            return Err(ActionHostError::Refused { reason });
        }
        self.hasher.update(chunk);
        self.written = end;
        Ok(end)
    }

    fn finish(mut self: Box<Self>) -> Result<String, ActionHostError> {
        if self.written != self.size {
            return Err(ActionHostError::InvalidRequest {
                reason: format!("the upload has {} of {} bytes", self.written, self.size),
            });
        }
        let sink = self.sink.take().ok_or_else(Self::ended)?;
        // Decided again: a grant withdrawn while the bytes arrived stores
        // nothing.
        let hash = self.hasher.finalize().to_hex().to_string();
        let broker = broker(&self.state)?;
        let at = broker.may_put_content(&self.caller, &self.room)?;
        // Recorded before the store keeps the bytes and tags them for the
        // room: content the room references is never unrecorded. A record
        // whose store then fails names content the room never got, which is
        // invisible; a failed record drops the upload, which keeps nothing.
        broker.attribute_content_put(&self.caller, &self.room, at, (&hash, self.size))?;
        drop(broker);
        let (stored, len) = sink
            .finish()
            .map_err(|reason| ActionHostError::Unavailable { reason })?;
        tilo_actions::content_resource(&hash)
            .filter(|_| stored == hash && is_content_hash(&stored) && len == self.size)
            .ok_or_else(|| ActionHostError::Unavailable {
                reason: "the content store answered a reference for other content".into(),
            })
    }
}

#[cfg(test)]
#[path = "content_put_tests.rs"]
mod tests;
