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

use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use tilo_actions::remote::{
    ActionHostError, CONTENT_GETS_UNAVAILABLE, Caller, ContentGetAnswer, ContentPiece, ContentPut,
    ContentPutStep, ContentPuts, MAX_CONTENT_FRAME_BYTES, Pending, RemoteActionMessage, pending,
};
use tilo_content::ContentStore;
use tilo_lock::MutexRecover as _;

use super::{Job, Worker, WorkspaceActions};
use crate::worker::ConfinedReplica;
use tilo_relay_protocol::WorkspaceId;
use ui::permissions::policy::ActionKind;

/// The most bytes one upload into a workspace's store takes. A file goes up
/// in pieces whatever its size; this bounds one file so a runaway agent
/// cannot fill a host's disk with a single upload. The bytes wait on disk,
/// never in memory ([`StorePut`]), so a file of hundreds of megabytes takes
/// no more of the host's memory than a small one.
pub const MAX_PUT_BYTES: u64 = 1024 * 1024 * 1024;

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
    // Only the piece is read: a caller downloading a file of hundreds of
    // megabytes asks for it a piece at a time, and reading the whole file
    // for each piece would read it hundreds of times.
    let (size, bytes) = if let Ok((size, bytes)) = store.read_range(hash, offset, length) {
        (size, bytes)
    } else {
        // Sealed content keeps its length only in the seal, so it is read
        // whole and sliced here.
        let bytes = store
            .read(hash)
            .ok_or_else(|| ActionHostError::InvalidRequest {
                reason: format!("this workspace's store holds no {content}"),
            })?;
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let from = usize::try_from(offset).unwrap_or(usize::MAX);
        let to = usize::try_from(offset.saturating_add(length).min(size)).unwrap_or(usize::MAX);
        (size, bytes.get(from..to).unwrap_or_default().to_vec())
    };
    if offset > size {
        return Err(ActionHostError::InvalidRequest {
            reason: format!("the content is {size} bytes: an offset of {offset} is past its end"),
        });
    }
    Ok(ContentPiece {
        size,
        offset,
        bytes,
    })
}

/// Puts into one workspace's store.
struct StorePuts(ContentStore);

impl ContentPuts for StorePuts {
    fn begin(&self, _caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionHostError> {
        if size > MAX_PUT_BYTES {
            return Err(ActionHostError::InvalidRequest {
                reason: format!(
                    "one file is at most {}: split the archive, or keep the piece the workspace needs",
                    ui::format::memory(MAX_PUT_BYTES)
                ),
            });
        }
        let spool = tempfile::tempfile().map_err(|error| ActionHostError::Unavailable {
            reason: format!("the host has nowhere to keep an upload: {error}"),
        })?;
        Ok(Box::new(StorePut {
            store: self.0.clone(),
            size,
            spool,
            written: 0,
        }))
    }
}

/// One upload: its bytes wait in a file of the host's until the last piece
/// arrives, so a file of hundreds of megabytes costs no memory. Dropped
/// unfinished, the file goes with it and nothing is stored.
struct StorePut {
    store: ContentStore,
    size: u64,
    spool: std::fs::File,
    written: u64,
}

impl ContentPut for StorePut {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        self.written
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionHostError> {
        if offset != self.written {
            return Err(ActionHostError::InvalidRequest {
                reason: format!(
                    "the upload has {} bytes: append at offset {}",
                    self.written, self.written
                ),
            });
        }
        let after = self
            .written
            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if after > self.size {
            return Err(ActionHostError::InvalidRequest {
                reason: format!("the upload began with {} bytes", self.size),
            });
        }
        self.spool
            .write_all(chunk)
            .map_err(|error| ActionHostError::Refused {
                reason: format!("the host could not keep the upload's bytes: {error}"),
            })?;
        self.written = after;
        Ok(after)
    }

    fn finish(mut self: Box<Self>) -> Result<String, ActionHostError> {
        if self.written != self.size {
            return Err(ActionHostError::InvalidRequest {
                reason: format!(
                    "the upload has {} of {} bytes: append the rest at offset {}",
                    self.written, self.size, self.written
                ),
            });
        }
        let kept = |error: String| ActionHostError::Refused {
            reason: format!("the workspace could not keep the file: {error}"),
        };
        // What the bytes are decides the media type the store keeps them
        // under: it is read from their first kilobyte, never from a name.
        let mut head = vec![0_u8; tilo_content_types::HEAD_BYTES];
        self.spool
            .seek(SeekFrom::Start(0))
            .map_err(|error| kept(error.to_string()))?;
        let read =
            read_up_to(&mut self.spool, &mut head).map_err(|error| kept(error.to_string()))?;
        head.truncate(read);
        let mime = tilo_content_types::sniff(&head).mime;
        self.spool
            .seek(SeekFrom::Start(0))
            .map_err(|error| kept(error.to_string()))?;
        let stored = self
            .store
            .put_reader(&mut self.spool, &mime)
            .map_err(|error| kept(error.to_string()))?;
        tilo_actions::content_resource(&stored.hash).ok_or_else(|| ActionHostError::Refused {
            reason: "the store named no content".into(),
        })
    }
}

/// Fill `into` from `file` until it is full or the file ends: how many bytes
/// were read.
fn read_up_to(file: &mut std::fs::File, into: &mut [u8]) -> std::io::Result<usize> {
    let mut read = 0;
    while let Some(room) = into.get_mut(read..) {
        if room.is_empty() {
            break;
        }
        match file.read(room) {
            Ok(0) => break,
            Ok(bytes) => read = read.saturating_add(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(read)
}

impl WorkspaceActions {
    /// Answer `step` of an upload `caller` asks of the host in
    /// `workspace_id`. The host's own person and their agents put files; a
    /// member, or an agent of theirs, puts them where their standing lets
    /// them change the workspace's records, which the service's thread reads
    /// when the upload begins. Its later steps go to the upload they began.
    pub(super) fn content_put(
        &self,
        workspace_id: WorkspaceId,
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
        workspace_id: WorkspaceId,
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
    fn known_store(&self, workspace_id: WorkspaceId) -> Option<ContentStore> {
        self.shared
            .stores
            .lock_recover()
            .get(&workspace_id)
            .cloned()
    }
}

impl Worker {
    /// `workspace_id`'s content store on this host, found once.
    pub(super) fn workspace_content(&self, workspace_id: WorkspaceId) -> Option<ContentStore> {
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
        workspace_id: WorkspaceId,
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
        workspace_id: WorkspaceId,
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
        match WorkspaceId::parse(workspace).and_then(|id| self.workspace_content(id)) {
            Some(store) => replica.lending(store),
            None => replica,
        }
    }
}

/// Shared between the service's handle and its thread.
pub(super) type Stores = std::sync::Mutex<std::collections::HashMap<WorkspaceId, ContentStore>>;

#[cfg(test)]
#[path = "content_upload_tests.rs"]
mod upload_tests;
