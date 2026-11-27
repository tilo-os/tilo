//! The content put api: a session client uploads bytes, chunk by chunk,
//! into its workspace's content store and gets back `tilo.content:<hash>`.
//!
//! Actions carry intent and references; bytes travel by session transport
//! into the content store.
//!
//! A put is decided at its own node, `tilo.content/…/put` in the workspace
//! ([`put_node`]): workspace rules reach it as they reach any node, an app's
//! rules do not. Puts are invisible to everyone until an action at its own
//! node references them; this node decides storage and tagging only, and a
//! successful put is never authority over any document.

use std::sync::{Arc, Weak};

use tilo_action_policy::{ActionKind, TreePath};
use tilo_actions::remote::{
    ActionCallError, CONTENT_PUTS_UNAVAILABLE, Caller, ContentPut, ContentPuts,
};
use tilo_desktop_model::Workspace;

use super::AppBrokerState;
use crate::services::{CONTENT_PUT_SCOPE, ContentSink, ContentStoreServices};
use tilo_invocation_journal::ContentPutRecord;

/// What a caller the workspace does not let add content is told: what it may
/// not do, and the one rule that would let it.
pub const CONTENT_PUT_REFUSED: &str = "not allowed to add content to this workspace: allow content uploads in this workspace to let it";

/// The node a put into `workspace` is decided at.
fn put_node(workspace: &Workspace) -> TreePath {
    tilo_actions::action_node(
        &workspace.policy.node(),
        CONTENT_PUT_SCOPE,
        CONTENT_PUT_SCOPE,
        "content",
        None,
        "put",
    )
}

impl AppBrokerState {
    /// The content put api, when this host has a content store.
    pub fn content_puts(self: &Arc<Self>) -> Option<Arc<dyn ContentPuts>> {
        let store = self.services.content_store.clone()?;
        Some(Arc::new(BrokerContentPuts {
            state: Arc::downgrade(self),
            store,
        }))
    }

    /// Whether `caller` may add content to `workspace` now, with the policy
    /// time it was decided at. An answer that would ask first refuses: a
    /// put asks nobody and spends no once-grant.
    fn may_put_content(&self, caller: &Caller, workspace: &str) -> Result<u64, ActionCallError> {
        super::action_provider::check_execution(caller)?;
        match self.decide_for(caller, workspace, |workspace, actor, now| {
            actor
                .may_in(workspace, &put_node(workspace), ActionKind::Undoable, now)
                .then_some(now)
        }) {
            Some(Some(now)) => Ok(now),
            Some(None) => Err(ActionCallError::Refused {
                reason: CONTENT_PUT_REFUSED.into(),
            }),
            None => Err(ActionCallError::NotPermitted),
        }
    }

    /// Note who put `hash` into `workspace`, durably, before answering its
    /// reference. Attribution only: it is never read for access.
    fn attribute_content_put(
        &self,
        caller: &Caller,
        workspace: &str,
        at: u64,
        (hash, len): (&str, u64),
    ) -> Result<(), ActionCallError> {
        let unavailable = |reason: String| ActionCallError::Unavailable { reason };
        let actor = self.caller_actor(caller).actor().clone();
        let store = self.services.content_store.clone();
        let durable = {
            let mut journal = self
                .journal
                .lock()
                .map_err(|_| unavailable("invocation journal unavailable".into()))?;
            match &mut *journal {
                Err(error) => return Err(unavailable(error.clone())),
                // A broker keeping no durable records keeps none of this either.
                Ok(None) => return Ok(()),
                Ok(Some(journal)) => {
                    let mut holds = StoredContent(store.as_deref());
                    journal
                        .record_content_put(
                            &ContentPutRecord {
                                at,
                                actor,
                                workspace: workspace.to_owned(),
                                hash: hash.to_owned(),
                                len,
                            },
                            &mut holds,
                        )
                        .map_err(unavailable)?
                }
            }
        };
        // The journal is free for others while the line reaches the disk;
        // an upload finishes on a blocking thread, never the broker's loop.
        durable
            .wait_blocking()
            .map_err(|error| unavailable(format!("content put attribution not durable: {error}")))
    }
}

impl AppBrokerState {
    /// `update`, once the content its answer names is the workspace's to read: an app keeps what
    /// it makes in its own content, which no one else reads, so a file its action answers is
    /// copied from there into the workspace's put content, where the caller reads it by its
    /// reference. Only content that app keeps and its own answer names moves; an answer whose
    /// content could not be handed over fails, saying why, rather than naming content no one
    /// can read.
    pub(super) async fn hand_over_answered(
        &self,
        app: &str,
        workspace: &str,
        update: tilo_actions::InvocationUpdate,
    ) -> tilo_actions::InvocationUpdate {
        let tilo_actions::InvocationUpdate::Succeeded { invocation, output } = &update else {
            return update;
        };
        let mut hashes = Vec::new();
        if let Some(value) = &output.value {
            answered_content(value, &mut hashes);
        }
        let Some(store) = self
            .services
            .content_store
            .clone()
            .filter(|_| !hashes.is_empty())
        else {
            return update;
        };
        let (app, workspace) = (app.to_owned(), workspace.to_owned());
        // The store reads and writes the disk: off the broker's loop.
        let handed = tokio::task::spawn_blocking(move || {
            hashes
                .iter()
                .try_for_each(|hash| store.hand_over(&workspace, &app, hash).map(drop))
        })
        .await
        .unwrap_or_else(|error| Err(error.to_string()));
        match handed {
            Ok(()) => update,
            Err(why) => tilo_actions::InvocationUpdate::Failed {
                invocation: *invocation,
                error: tilo_actions::ActionFailure {
                    code: "action_failed".into(),
                    message: format!(
                        "the file this action answered did not reach the workspace: {why}"
                    ),
                    details: None,
                    retryable: false,
                },
            },
        }
    }
}

/// Each content hash an answer names as a file (`{"content": "tilo.content:<hash>", ...}`),
/// wherever in the answer it is.
fn answered_content(value: &serde_json::Value, hashes: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(hash) = fields
                .get("content")
                .and_then(serde_json::Value::as_str)
                .and_then(tilo_actions::content_hash)
                .filter(|hash| is_content_hash(hash))
                && !hashes.iter().any(|seen| seen == hash)
            {
                hashes.push(hash.to_owned());
            }
            for field in fields.values() {
                answered_content(field, hashes);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                answered_content(item, hashes);
            }
        }
        _ => {}
    }
}

/// The device content store, asked whether a workspace still holds what a put
/// left there when the attributions are compacted.
struct StoredContent<'a>(Option<&'a dyn ContentStoreServices>);

impl tilo_invocation_journal::ContentHolds for StoredContent<'_> {
    fn holds(&mut self, workspace: &str, hash: &str) -> bool {
        self.0.is_none_or(|store| store.holds(workspace, hash))
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
fn broker(state: &Weak<AppBrokerState>) -> Result<Arc<AppBrokerState>, ActionCallError> {
    state.upgrade().ok_or_else(|| ActionCallError::Unavailable {
        reason: CONTENT_PUTS_UNAVAILABLE.into(),
    })
}

struct BrokerContentPuts {
    state: Weak<AppBrokerState>,
    store: Arc<dyn ContentStoreServices>,
}

impl ContentPuts for BrokerContentPuts {
    fn begin(&self, caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError> {
        let workspace = caller
            .workspace()
            .ok_or_else(|| ActionCallError::InvalidRequest {
                reason: "content goes to a workspace, and this session is in none".into(),
            })?
            .to_owned();
        let max = self.store.max_bytes();
        if size > max {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("an upload is 0 to {max} bytes, not {size}"),
            });
        }
        broker(&self.state)?.may_put_content(caller, &workspace)?;
        let sink = self
            .store
            .begin(&workspace)
            .map_err(|reason| ActionCallError::Unavailable { reason })?;
        Ok(Box::new(BrokerContentPut {
            state: Weak::clone(&self.state),
            caller: caller.clone(),
            workspace,
            size,
            written: 0,
            hasher: blake3::Hasher::new(),
            sink: Some(sink),
        }))
    }

    /// Which of `hashes` the caller's workspace has: asked by whoever may put content there,
    /// so a file it has is not sent again.
    fn have(&self, caller: &Caller, hashes: &[String]) -> Result<Vec<bool>, ActionCallError> {
        let workspace = caller
            .workspace()
            .ok_or_else(|| ActionCallError::InvalidRequest {
                reason: "content goes to a workspace, and this session is in none".into(),
            })?;
        broker(&self.state)?.may_put_content(caller, workspace)?;
        Ok(hashes
            .iter()
            .map(|address| {
                tilo_actions::remote::content_hash(address)
                    .is_some_and(|hash| self.store.holds(workspace, hash))
            })
            .collect())
    }
}

/// One upload. Dropping it drops the store's upload, which stores nothing.
struct BrokerContentPut {
    state: Weak<AppBrokerState>,
    caller: Caller,
    workspace: String,
    size: u64,
    written: u64,
    /// The digest of what was appended, known before the store keeps it, so
    /// the put is recorded before the workspace references the content.
    hasher: blake3::Hasher,
    /// `None` once the store refused a chunk: the upload is over.
    sink: Option<Box<dyn ContentSink>>,
}

impl BrokerContentPut {
    fn ended() -> ActionCallError {
        ActionCallError::Refused {
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

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
        let written = self.written;
        if offset != written {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("the upload has {written} bytes: append at offset {written}"),
            });
        }
        let end = written.saturating_add(chunk.len() as u64);
        if end > self.size {
            return Err(ActionCallError::InvalidRequest {
                reason: format!(
                    "the chunk ends at {end}, past the {} bytes the upload began with",
                    self.size
                ),
            });
        }
        let sink = self.sink.as_mut().ok_or_else(Self::ended)?;
        if let Err(reason) = sink.append(chunk) {
            self.sink = None;
            return Err(ActionCallError::Refused { reason });
        }
        self.hasher.update(chunk);
        self.written = end;
        Ok(end)
    }

    /// A connection that ended takes its run's execution with it: the upload it began is
    /// finished, and decided again, as the same caller on the connection that resumed it.
    fn finish_as(mut self: Box<Self>, caller: &Caller) -> Result<String, ActionCallError> {
        if caller.acting() != self.caller.acting()
            || caller.workspace() != Some(self.workspace.as_str())
        {
            return Err(ActionCallError::NotPermitted);
        }
        self.caller = caller.clone();
        self.finish()
    }

    fn finish(mut self: Box<Self>) -> Result<String, ActionCallError> {
        if self.written != self.size {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("the upload has {} of {} bytes", self.written, self.size),
            });
        }
        let sink = self.sink.take().ok_or_else(Self::ended)?;
        // Decided again: a grant withdrawn while the bytes arrived stores
        // nothing.
        let hash = self.hasher.finalize().to_hex().to_string();
        let broker = broker(&self.state)?;
        let at = broker.may_put_content(&self.caller, &self.workspace)?;
        // Recorded before the store keeps the bytes and tags them for the
        // workspace: content the workspace references is never unrecorded. A record
        // whose store then fails names content the workspace never got, which is
        // invisible; a failed record drops the upload, which keeps nothing.
        broker.attribute_content_put(&self.caller, &self.workspace, at, (&hash, self.size))?;
        drop(broker);
        let (stored, len) = sink
            .finish()
            .map_err(|reason| ActionCallError::Unavailable { reason })?;
        tilo_actions::content_resource(&hash)
            .filter(|_| stored == hash && is_content_hash(&stored) && len == self.size)
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "the content store answered a reference for other content".into(),
            })
    }
}

#[cfg(test)]
#[path = "content_put_tests.rs"]
mod tests;
