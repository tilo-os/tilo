//! Who waits for whom while workspaces are created.

use std::collections::HashMap;
use std::sync::Arc;

use tilo_lock::MutexRecover as _;
use tokio::sync::Mutex as AsyncMutex;

use super::{HostedWorkspace, RELEASED, delete};
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
    /// Create a new hosted workspace named `name`.
    ///
    /// # Errors
    ///
    /// Returns why the workspace could not be created, persisted or bound.
    pub async fn create(&self, name: &str) -> Result<WorkspaceSummary, String> {
        let summary = self.create_workspace(name, None).await?;
        let person = self.device_cert().user_public_key;
        self.list_for(&person, &summary.workspace, &summary.name)
            .await;
        Ok(summary)
    }

    /// Found a workspace called `name`, under the identity `workspace` when given, on this device
    /// for its person, place it at their server and list it in their own list as kept there: what
    /// a person's device does when they make a workspace, so their other devices find it at the
    /// server. Where no server of theirs holds their own workspace yet, it is kept here alone.
    ///
    /// # Errors
    ///
    /// As [`Self::create_for`], or their server did not take the workspace in: it stays here.
    pub async fn found(
        &self,
        name: &str,
        workspace: Option<&str>,
    ) -> Result<WorkspaceSummary, String> {
        let person = self.device_cert().user_public_key;
        let server = self
            .persons_server(&person)
            .await
            .map(|(server, addr)| (server.host.device_public_key, addr));
        self.found_at(name, workspace, server)
            .await
            .map(|(summary, _)| summary)
    }

    /// [`Self::found`], placing the workspace at the device `at` names, at its address: their
    /// server, or another site that keeps their workspaces. Kept here alone with none. Answers
    /// the address that device signed for the workspace, where it took it.
    ///
    /// # Errors
    ///
    /// As [`Self::found`].
    pub(super) async fn found_at(
        &self,
        name: &str,
        workspace: Option<&str>,
        at: Option<([u8; 32], iroh_base::EndpointAddr)>,
    ) -> Result<
        (
            WorkspaceSummary,
            Option<tilo_peer_state::join::TransportHint>,
        ),
        String,
    > {
        let person = self.device_cert().user_public_key;
        let summary = self.create_for(name, workspace, person).await?;
        let Some((device, addr)) = at else {
            return Ok((summary, None));
        };
        let hint = self
            .attach_server(
                &summary.workspace,
                device,
                addr,
                tilo_peer_state::CopyRole::KeyHolder,
            )
            .await?;
        let kept = tilo_server_protocol::workspaces::KeptWorkspace {
            workspace: summary.workspace.clone(),
            name: summary.name.clone(),
            host_hint: hex::encode(
                postcard::to_allocvec(&hint).map_err(|error| error.to_string())?,
            ),
        };
        if !self.list_kept_by(&person, device, &[kept]).await {
            tracing::warn!(workspace = %summary.workspace, "a workspace placed for the person was not listed as kept there");
        }
        Ok((summary, Some(hint)))
    }

    /// Create a hosted workspace named `name`, under the identity `workspace` when
    /// given: a workspace moving here keeps the workspace it is. Refused when
    /// this host holds that workspace already.
    ///
    /// A workspace made this way starts with the default apps this device's registries offer,
    /// recorded as the versions it runs. A workspace that comes here with apps of its own is
    /// created with [`Self::create_workspace_keeping_its_apps`] instead, so it keeps them.
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
        Box::pin(self.make_workspace(name, workspace, None, true)).await
    }

    /// Create a hosted workspace under the identity `workspace` that keeps the apps it
    /// already has: a branch, or a workspace placed here from another device. It records no
    /// default apps, so what it runs stays what its source runs.
    ///
    /// # Errors
    ///
    /// As [`Self::create_workspace`].
    pub(super) async fn create_workspace_keeping_its_apps(
        &self,
        name: &str,
        workspace: &str,
    ) -> Result<WorkspaceSummary, String> {
        Box::pin(self.make_workspace(name, Some(workspace), None, false)).await
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
        Box::pin(self.make_workspace(name, workspace, Some(person), true)).await
    }

    async fn make_workspace(
        &self,
        name: &str,
        workspace: Option<&str>,
        person: Option<[u8; 32]>,
        seed_defaults: bool,
    ) -> Result<WorkspaceSummary, String> {
        // A device confined to one workspace makes none: it would hold the new one's keys.
        super::member::holds_copies(&self.signing_device().cert)?;
        if self.stopped() {
            return Err(
                "this device's workspace service has stopped; its next part holds it".to_owned(),
            );
        }
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
        self.refuse_held_workspace(workspace).await?;
        let device = self.device_keys();
        // The apps this workspace starts with, as `workspace/apps#install` would record them.
        // Made here so the lookup runs once, on the device that makes the workspace.
        // The registries come first: the default apps are the releases they offer.
        let (registries, defaults) = if seed_defaults {
            let registries = super::default_apps::registries();
            let defaults = super::default_apps::on_this_device_awaited(&registries).await;
            (super::default_apps::registry_rows(&registries), defaults)
        } else {
            (Vec::new(), Vec::new())
        };
        let workspace = match workspace {
            Some(workspace) => HostedWorkspace::create_in(
                &self.inner.dir,
                workspace,
                name,
                (&self.inner.host.cert, &self.inner.host.key),
                device.as_ref(),
                (&registries, &defaults),
                tilo_clock::wall(),
            )?,
            None => HostedWorkspace::create(
                &self.inner.dir,
                name,
                &self.inner.host.cert,
                &self.inner.host.key,
                device.as_ref(),
                (&registries, &defaults),
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
        // Opened before it is held: a call admitted in the moment between holding it and the
        // settle below signs for a workspace this host serves.
        tilo_invocation_journal::opened(&summary.workspace);
        self.keep_held(&summary.workspace, &workspace).await;
        let mut hosted = workspace.lock().await;
        self.settle(&mut hosted).await?;
        self.name_own_host(&mut hosted).await?;
        if let Some(person) = person {
            self.give_ownership(&mut hosted, person, None).await?;
        }
        self.serve_direct(&hosted).await?;
        // Everything this create made is written through before it answers as done: the next
        // part opens the store as this one leaves it, whatever moment this one ends at.
        hosted.keep_followed();
        drop(hosted);
        self.keep_reading(&workspace).await;
        tracing::info!(workspace = %summary.workspace, workspace_id = %summary.workspace_id, "hosted workspace created");
        // The person's list names the workspace they now have, where this device holds their own
        // list: a daemon that makes one for them writes it without a desktop session.
        if let Some(person) = person {
            self.list_for(&person, &summary.workspace, &summary.name)
                .await;
        }
        Ok(summary)
    }

    /// Refuse a workspace `workspace` names that this host already holds, has released, has
    /// deleted, or serves.
    async fn refuse_held_workspace(&self, workspace: Option<&str>) -> Result<(), String> {
        if let Some(workspace) = workspace
            && self.inner.workspaces.lock().await.contains_key(workspace)
        {
            return Err("this host has that workspace already".into());
        }
        // A workspace whose record did not read as this host opened is not absent: its files are
        // there, and making it here would write a fresh record over them.
        if let Some(workspace) = workspace
            && let Some(refusal) = self.closed_refusal(workspace)
        {
            return Err(refusal);
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
        Ok(())
    }
}
