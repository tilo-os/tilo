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

use tilo_actions::remote::{
    ActionHostError, Caller, ContentPut, ContentPutStep, ContentPuts, Pending, RemoteActionMessage,
    pending,
};
use tilo_content::ContentStore;
use tilo_lock::MutexRecover as _;

use super::{Job, Worker, WorkspaceActions};
use crate::worker::ConfinedReplica;

/// The most bytes one upload into a workspace's store takes: as much as the
/// host answers a member's request for content with.
pub const MAX_PUT_BYTES: u64 = 64 * 1024 * 1024;

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
    /// `workspace_id`. Only the host's own person puts files, as only they
    /// run document actions through it.
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
        if !self.authorized(caller) {
            return Pending::ready(respond(Err(ActionHostError::NotPermitted)));
        }
        // The store is found once on the service's thread, then kept.
        let known = self
            .shared
            .stores
            .lock_recover()
            .get(&workspace_id)
            .cloned();
        if let Some(store) = known {
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

    /// Answer `step` of `caller`'s upload into `workspace_id`'s store.
    pub(super) fn content_put(
        &self,
        workspace_id: u64,
        caller: &Caller,
        step: ContentPutStep,
    ) -> Result<tilo_actions::remote::ContentPutAnswer, ActionHostError> {
        let store =
            self.workspace_content(workspace_id)
                .ok_or_else(|| ActionHostError::Unavailable {
                    reason: tilo_actions::remote::CONTENT_PUTS_UNAVAILABLE.into(),
                })?;
        self.shared.puts.step(&StorePuts(store), caller, step)
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

/// Each workspace's content store under one directory, by the workspace's
/// uuid: one workspace's store never answers for another's.
pub struct WorkspaceStores {
    dir: std::path::PathBuf,
    stores: std::sync::Mutex<std::collections::BTreeMap<String, ContentStore>>,
}

impl WorkspaceStores {
    /// The stores under `dir`.
    #[must_use]
    pub fn in_dir(dir: std::path::PathBuf) -> Self {
        Self {
            dir,
            stores: std::sync::Mutex::default(),
        }
    }

    /// The content store of `workspace`.
    ///
    /// # Errors
    ///
    /// `workspace` is not a workspace's uuid.
    pub fn get(&self, workspace: &str) -> Result<ContentStore, String> {
        let workspace = uuid::Uuid::parse_str(workspace)
            .map_err(|_| "the workspace is not a uuid".to_string())?
            .hyphenated()
            .to_string();
        let store = self
            .stores
            .lock_recover()
            .entry(workspace)
            .or_insert_with_key(|workspace| ContentStore::in_dir(self.dir.join(workspace)))
            .clone();
        Ok(store)
    }
}

/// Shared between the service's handle and its thread.
pub(super) type Stores = std::sync::Mutex<std::collections::HashMap<u64, ContentStore>>;
