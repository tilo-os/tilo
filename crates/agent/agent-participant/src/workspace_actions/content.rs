//! A workspace's content on its host: the store its records' files live in.
//!
//! A caller of the host's actions over a session (an agent's MCP session
//! through the person's own device) puts a file's bytes here, step by step
//! ([`ContentPutStep`]), and names the content `tilo.content:<hash>` in an
//! action. The confined app worker that runs the action is lent the same
//! store, so an app reads what was put (PDF Viewer's `import-pdf-content`,
//! Writer's `read-content`) and keeps what it makes (Writer's
//! `export-content`) exactly as it does on a device. The host answers the
//! workspace's members from it too, so a page that opens the record later
//! reads the same bytes.
//!
//! The same callers read those bytes back out, a piece at a time
//! ([`super::RemoteActionMessage::ContentGetRequest`]): only this
//! workspace's store answers, so a reference another workspace holds reads
//! as content this one does not have.
//!
//! Who may: the host's own person and their agents, and any member of the
//! workspace, or an agent of theirs, as their standing there is now: one who
//! may change its records (an editor, an agent of one) puts files, and one
//! who may read it gets them. A viewer only reads.

use tilo_actions::remote::{
    ActionHostError, CONTENT_GETS_UNAVAILABLE, Caller, ContentGetAnswer, ContentPiece, ContentPut,
    ContentPutStep, ContentPuts, MAX_CONTENT_FRAME_BYTES, Pending, RemoteActionMessage, pending,
};
use tilo_content::ContentStore;
use tilo_lock::MutexRecover as _;

use super::{Job, Worker, WorkspaceActions};
use crate::worker::ConfinedReplica;
use ui::permissions::policy::ActionKind;

/// The most bytes one upload into a workspace's store takes: as much as the
/// host answers a member's request for content with.
pub const MAX_PUT_BYTES: u64 = 64 * 1024 * 1024;

/// One piece of `content` read out of `store`: `length` bytes from `offset`,
/// or one piece's worth when `length` is zero.
///
/// # Errors
///
/// A reference that is not one, content this store does not hold, an offset
/// past its end, or a piece longer than a frame carries.
fn piece(
    store: &ContentStore,
    content: &str,
    offset: u64,
    length: u64,
) -> Result<ContentPiece, ActionHostError> {
    let length = if length == 0 {
        MAX_CONTENT_FRAME_BYTES as u64
    } else {
        length
    };
    if length > MAX_CONTENT_FRAME_BYTES as u64 {
        return Err(ActionHostError::InvalidRequest {
            reason: format!("a piece carries at most {MAX_CONTENT_FRAME_BYTES} bytes"),
        });
    }
    let hash =
        tilo_actions::content_hash(content).ok_or_else(|| ActionHostError::InvalidRequest {
            reason: format!("{content} is not a tilo.content:<hash> reference"),
        })?;
    let bytes = store
        .read(hash)
        .ok_or_else(|| ActionHostError::InvalidRequest {
            reason: format!("this workspace's store holds no {content}"),
        })?;
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if offset > size {
        return Err(ActionHostError::InvalidRequest {
            reason: format!("the content is {size} bytes: an offset of {offset} is past its end"),
        });
    }
    let from = usize::try_from(offset).unwrap_or(usize::MAX);
    let to = usize::try_from(offset.saturating_add(length).min(size)).unwrap_or(usize::MAX);
    Ok(ContentPiece {
        size,
        offset,
        bytes: bytes.get(from..to).unwrap_or_default().to_vec(),
    })
}

/// Puts into one workspace's store.
struct StorePuts(ContentStore);

impl ContentPuts for StorePuts {
    fn begin(&self, _caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionHostError> {
        if size > MAX_PUT_BYTES {
            return Err(ActionHostError::InvalidRequest {
                reason: format!(
                    "an upload takes at most {}",
                    ui::format::memory(MAX_PUT_BYTES)
                ),
            });
        }
        Ok(Box::new(StorePut {
            store: self.0.clone(),
            size,
            bytes: Vec::new(),
        }))
    }
}

/// One upload, held in memory until it is finished.
struct StorePut {
    store: ContentStore,
    size: u64,
    bytes: Vec<u8>,
}

impl StorePut {
    fn written_len(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }
}

impl ContentPut for StorePut {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        self.written_len()
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionHostError> {
        let written = self.written_len();
        if offset != written {
            return Err(ActionHostError::InvalidRequest {
                reason: format!("the upload has {written} bytes: append at offset {written}"),
            });
        }
        let after = written.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if after > self.size {
            return Err(ActionHostError::InvalidRequest {
                reason: format!("the upload began with {} bytes", self.size),
            });
        }
        self.bytes.extend_from_slice(chunk);
        Ok(after)
    }

    fn finish(self: Box<Self>) -> Result<String, ActionHostError> {
        let hash = self
            .store
            .put(&self.bytes)
            .map_err(|error| ActionHostError::Refused {
                reason: format!("the workspace could not keep the file: {error}"),
            })?;
        tilo_actions::content_resource(&hash).ok_or_else(|| ActionHostError::Refused {
            reason: "the store named no content".into(),
        })
    }
}

impl WorkspaceActions {
    /// Answer `step` of an upload `caller` asks of the host in
    /// `workspace_id`. The host's own person and their agents put files; a
    /// member, or an agent of theirs, puts them where their standing lets
    /// them change the workspace's records, which the service's thread reads
    /// when the upload begins. Its later steps go to the upload they began.
    pub(super) fn content_put(
        &self,
        workspace_id: u64,
        caller: &Caller,
        correlation: u64,
        step: ContentPutStep,
    ) -> Pending<RemoteActionMessage> {
        let respond = move |result| RemoteActionMessage::ContentPutResponse {
            correlation,
            result,
        };
        let checked = !self.authorized(caller) && matches!(step, ContentPutStep::Begin { .. });
        if checked && !super::speaks_for_a_member(caller) {
            return Pending::ready(respond(Err(ActionHostError::NotPermitted)));
        }
        if let Some(store) = self.known_store(workspace_id).filter(|_| !checked) {
            return Pending::ready(respond(self.shared.puts.step(
                &StorePuts(store),
                caller,
                step,
            )));
        }
        let (done, answer) = pending();
        let job = Job::Content {
            workspace_id,
            caller: caller.clone(),
            step,
            done,
        };
        if let Err(Job::Content { done, .. }) = self.submit(job) {
            done.complete(Err(ActionHostError::Unavailable {
                reason: "the host's workspace actions are not running".into(),
            }));
        }
        answer.map(respond)
    }

    /// Answer `caller`'s request for a piece of `content` from the content
    /// store of the host's workspace `workspace_id`: the host's own person
    /// and their agents, and anyone whose standing there lets them read it,
    /// which the service's thread reads.
    pub(super) fn content_get(
        &self,
        workspace_id: u64,
        caller: &Caller,
        correlation: u64,
        (content, offset, length): (String, u64, u64),
    ) -> Pending<RemoteActionMessage> {
        let respond = move |result| RemoteActionMessage::ContentGetResponse {
            correlation,
            result,
        };
        let owner = self.authorized(caller);
        if !owner && !super::speaks_for_a_member(caller) {
            return Pending::ready(respond(Err(ActionHostError::NotPermitted)));
        }
        if let Some(store) = self.known_store(workspace_id).filter(|_| owner) {
            return Pending::ready(respond(
                piece(&store, &content, offset, length).and_then(ContentGetAnswer::new),
            ));
        }
        let (done, answer) = pending();
        let job = Job::ContentGet {
            workspace_id,
            caller: caller.clone(),
            content,
            offset,
            length,
            done,
        };
        if let Err(Job::ContentGet { done, .. }) = self.submit(job) {
            done.complete(Err(ActionHostError::Unavailable {
                reason: "the host's workspace actions are not running".into(),
            }));
        }
        answer.map(respond)
    }

    /// `workspace_id`'s store, once the service's thread found it.
    fn known_store(&self, workspace_id: u64) -> Option<ContentStore> {
        self.shared
            .stores
            .lock_recover()
            .get(&workspace_id)
            .cloned()
    }
}

impl Worker {
    /// `workspace_id`'s content store on this host, found once.
    pub(super) fn workspace_content(&self, workspace_id: u64) -> Option<ContentStore> {
        if let Some(store) = self.shared.stores.lock_recover().get(&workspace_id) {
            return Some(store.clone());
        }
        let store = self.shared.link.get()?.workspace_content(workspace_id)?;
        self.shared
            .stores
            .lock_recover()
            .insert(workspace_id, store.clone());
        Some(store)
    }

    /// Answer `step` of `caller`'s upload into `workspace_id`'s store: one
    /// begins only where they may change the workspace's records.
    pub(super) fn content_put(
        &self,
        workspace_id: u64,
        caller: &Caller,
        step: ContentPutStep,
    ) -> Result<tilo_actions::remote::ContentPutAnswer, ActionHostError> {
        if matches!(step, ContentPutStep::Begin { .. })
            && !self.may_across(workspace_id, caller, ActionKind::Undoable)
        {
            return Err(ActionHostError::NotPermitted);
        }
        let store =
            self.workspace_content(workspace_id)
                .ok_or_else(|| ActionHostError::Unavailable {
                    reason: tilo_actions::remote::CONTENT_PUTS_UNAVAILABLE.into(),
                })?;
        self.shared.puts.step(&StorePuts(store), caller, step)
    }

    /// The piece of `content` `caller` asked of `workspace_id`'s store,
    /// where they may read the workspace.
    pub(super) fn content_get(
        &self,
        workspace_id: u64,
        caller: &Caller,
        content: &str,
        (offset, length): (u64, u64),
    ) -> Result<ContentGetAnswer, ActionHostError> {
        if !self.may_across(workspace_id, caller, ActionKind::Read) {
            return Err(ActionHostError::NotPermitted);
        }
        let store =
            self.workspace_content(workspace_id)
                .ok_or_else(|| ActionHostError::Unavailable {
                    reason: CONTENT_GETS_UNAVAILABLE.into(),
                })?;
        ContentGetAnswer::new(piece(&store, content, offset, length)?)
    }

    /// `replica`, a record of the workspace `workspace` names, lent the
    /// workspace's content store when this host keeps one.
    pub(super) fn lend(&self, replica: ConfinedReplica, workspace: &str) -> ConfinedReplica {
        match tilo_relay_protocol::room_workspace_id(workspace)
            .ok()
            .and_then(|id| self.workspace_content(id))
        {
            Some(store) => replica.lending(store),
            None => replica,
        }
    }
}

/// Shared between the service's handle and its thread.
pub(super) type Stores = std::sync::Mutex<std::collections::HashMap<u64, ContentStore>>;
