//! Who waits for whom while workspaces are created.

use std::collections::HashMap;
use std::sync::Arc;

use tilo_lock::MutexRecover as _;
use tokio::sync::Mutex as AsyncMutex;

use super::{GovernedWorkspace, RELEASED, delete};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::workspaces::WorkspaceSummary;

/// How many workspaces are made at once: each writes and syncs its files on
/// the runtime's threads, and the rest of the host's traffic needs some free.
const CREATING_AT_ONCE: usize = 4;

/// Who waits for whom while workspaces are created: one identity at a time,
/// and [`CREATING_AT_ONCE`] workspaces in all.
pub(super) struct Creating {
    identities: std::sync::Mutex<HashMap<String, std::sync::Weak<AsyncMutex<()>>>>,
    turns: Arc<tokio::sync::Semaphore>,
}

impl Default for Creating {
    fn default() -> Self {
        Self {
            identities: std::sync::Mutex::default(),
            turns: Arc::new(tokio::sync::Semaphore::new(CREATING_AT_ONCE)),
        }
    }
}

impl Creating {
    /// Held while the workspace `identity` is created.
    pub(super) async fn one(&self, identity: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let held = {
            let mut identities = self.identities.lock_recover();
            identities.retain(|_, held| held.strong_count() > 0);
            if let Some(held) = identities.get(identity).and_then(std::sync::Weak::upgrade) {
                held
            } else {
                let held = Arc::new(AsyncMutex::new(()));
                identities.insert(identity.to_owned(), Arc::downgrade(&held));
                held
            }
        };
        held.lock_owned().await
    }

    /// A turn to create one workspace.
    pub(super) async fn turn(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        Arc::clone(&self.turns).acquire_owned().await.ok()
    }
}

impl super::Workspaces {
    /// Create a new governed workspace named `name`.
    ///
    /// # Errors
    ///
    /// Returns why the workspace could not be created, persisted or bound.
    pub async fn create(&self, name: &str) -> Result<WorkspaceSummary, String> {
        self.create_workspace(name, None).await
    }

    /// Create a governed workspace named `name`, under the identity `workspace` when
    /// given: a workspace moving here keeps the workspace it is. Refused when
    /// this host holds that workspace already.
    ///
    /// # Errors
    ///
    /// The name is not 1–80 characters, `workspace` is no UUID or held here
    /// already, or the workspace could not be written.
    pub async fn create_workspace(
        &self,
        name: &str,
        workspace: Option<&str>,
    ) -> Result<WorkspaceSummary, String> {
        // Making a workspace is a large future; whoever awaits it holds a pointer.
        Box::pin(self.make_workspace(name, workspace, None)).await
    }

    /// [`Self::create_workspace`] for `person`: the workspace is theirs from
    /// the step that makes it. This host founds it, hands it to them as its
    /// owner, and stays in it as one of its administrators, which every
    /// member sees and its owner may withdraw. Made for this host's own
    /// person, it stays this host's.
    ///
    /// # Errors
    ///
    /// As [`Self::create_workspace`], and when the workspace could not be
    /// handed to `person`.
    pub async fn create_for(
        &self,
        name: &str,
        workspace: Option<&str>,
        person: [u8; 32],
    ) -> Result<WorkspaceSummary, String> {
        Box::pin(self.make_workspace(name, workspace, Some(person))).await
    }

    async fn make_workspace(
        &self,
        name: &str,
        workspace: Option<&str>,
        person: Option<[u8; 32]>,
    ) -> Result<WorkspaceSummary, String> {
        // A workspace placed under an identity waits for another placing the
        // same one, so two devices placing it at once create it once. Others
        // are made side by side, a few at a time: making one writes and syncs
        // its files on this runtime's threads, which every other request needs
        // too.
        let _same = match workspace {
            Some(workspace) => Some(self.inner.creating.one(workspace).await),
            None => None,
        };
        let _turn = self.inner.creating.turn().await;
        if let Some(workspace) = workspace
            && self.inner.workspaces.lock().await.contains_key(workspace)
        {
            return Err("this host has that workspace already".into());
        }
        if let Some(workspace) = workspace
            && self
                .inner
                .dir
                .join(workspace)
                .with_extension(RELEASED)
                .exists()
        {
            return Err("this host released that workspace; its files stay here".into());
        }
        if let Some(workspace) = workspace
            && self
                .inner
                .dir
                .join(workspace)
                .with_extension(delete::DELETED)
                .exists()
        {
            return Err("that workspace was deleted here".into());
        }
        // A workspace this host serves already is the one it serves: founding
        // it here as well would fork it.
        if let Some(workspace_id) = workspace.and_then(WorkspaceId::parse) {
            let bound = self
                .inner
                .host
                .workspaces
                .lock()
                .await
                .get(&workspace_id)
                .cloned();
            if let Some(bound) = bound
                && bound.lock().await.workspace_authority.is_some()
            {
                return Err("this host has that workspace already".into());
            }
        }
        let device = self.device_keys();
        let workspace = match workspace {
            Some(workspace) => GovernedWorkspace::create_in(
                &self.inner.dir,
                workspace,
                name,
                (&self.inner.host.cert, &self.inner.host.key),
                device.as_ref(),
                tilo_clock::wall(),
            )?,
            None => GovernedWorkspace::create(
                &self.inner.dir,
                name,
                &self.inner.host.cert,
                &self.inner.host.key,
                device.as_ref(),
                tilo_clock::wall(),
            )?,
        };
        self.bind(&workspace).await?;
        let summary = self.people(&workspace);
        let reading = workspace.reading();
        let workspace = Arc::new(AsyncMutex::new(workspace));
        self.inner
            .readings
            .hold(&summary.workspace, &workspace, reading);
        self.inner
            .workspaces
            .lock()
            .await
            .insert(summary.workspace.clone(), Arc::clone(&workspace));
        let mut governed = workspace.lock().await;
        self.settle(&mut governed).await?;
        self.name_own_host(&mut governed).await?;
        if let Some(person) = person {
            self.give_ownership(&mut governed, person, None).await?;
        }
        self.serve_direct(&governed)?;
        drop(governed);
        self.keep_reading(&workspace).await;
        tracing::info!(workspace = %summary.workspace, workspace_id = %summary.workspace_id, "governed workspace created");
        Ok(summary)
    }
}
