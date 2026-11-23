//! The person's saved shortcuts, as a device's broker keeps them.
//!
//! A shortcut a person saves is one of its workspace's settings: signed here by this device as
//! its saver's own, then committed through the workspace service that holds the workspace, where
//! every device of the workspace reads it back from and this device's trigger runner arms it. The
//! broker publishes the library and each saved shortcut's `run` only where this service is
//! installed, so a computer keeps the person's shortcuts wherever its broker runs, the daemon's
//! and a desktop session's alike.

use std::sync::Arc;

use collab_types::signer::DeviceSigner;
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::apps::automation::{ShortcutChange, validate_trigger};
use tilo_workspace_service::apps::services::{
    ShortcutLibraryRequest, ShortcutLibraryServices, ShortcutStore,
};
use tilo_workspace_service::service::ServiceSlot;
use tilo_workspace_service::workspaces::Workspaces;

/// The person's saved shortcuts, as a device's broker keeps and runs them.
pub struct Shortcuts {
    /// The workspaces this device holds: where a saved shortcut is committed.
    held: Source,
    /// The device this library signs as: the one its workspace has admitted.
    device: DeviceCert,
    /// Signs as `device`: the shortcut's own proof of who saved it.
    signer: Arc<dyn DeviceSigner>,
    /// Where a shortcut's run executes and the change that saves one is committed.
    runs: tokio::runtime::Handle,
}

/// Where the workspaces this library saves in are.
#[derive(Clone)]
enum Source {
    /// Held now, as a daemon's workspace service is when it starts its broker.
    Held(Workspaces),
    /// Installed when this device's own workspace service opens, as a desktop session's is after
    /// it starts its broker: a save waits for the service rather than failing on that order.
    Opening(ServiceSlot),
}

impl Source {
    /// The workspaces to commit in, waiting for this device's service to open when it has not.
    async fn workspaces(&self) -> Workspaces {
        match self {
            Self::Held(workspaces) => workspaces.clone(),
            Self::Opening(service) => service.installed().await.workspaces().clone(),
        }
    }
}

impl Shortcuts {
    /// A library over the workspaces `workspaces` holds, signing as `device` with `signer`.
    ///
    /// # Errors
    /// The runtime a shortcut's run executes on could not be started.
    pub fn new(
        workspaces: Workspaces,
        device: DeviceCert,
        signer: Arc<dyn DeviceSigner>,
    ) -> std::io::Result<Self> {
        Self::over(Source::Held(workspaces), device, signer)
    }

    /// A library over the workspaces the service `service` installs, signing as `device` with
    /// `signer`: what a session builds before its own workspace service opens.
    ///
    /// # Errors
    /// The runtime a shortcut's run executes on could not be started.
    pub fn when_installed(
        service: ServiceSlot,
        device: DeviceCert,
        signer: Arc<dyn DeviceSigner>,
    ) -> std::io::Result<Self> {
        Self::over(Source::Opening(service), device, signer)
    }

    fn over(
        held: Source,
        device: DeviceCert,
        signer: Arc<dyn DeviceSigner>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            held,
            device,
            signer,
            runs: automation_runtime()?,
        })
    }
}

/// Where a shortcut's run executes and the change that saves one is committed: a thread of its
/// own, started once for this process, so a run waiting on a step keeps the rest of the device
/// answering.
fn automation_runtime() -> std::io::Result<tokio::runtime::Handle> {
    static RUNS: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> =
        std::sync::OnceLock::new();
    let runs = RUNS.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("tilo-automation")
            .enable_all()
            .build()
            .map_err(|error| error.to_string())
    });
    match runs {
        Ok(runs) => Ok(runs.handle().clone()),
        Err(error) => Err(std::io::Error::other(error.clone())),
    }
}

impl ShortcutLibraryServices for Shortcuts {
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig {
        settings_store::hardware::ShortcutConfig::load()
    }

    fn store(&self, request: ShortcutLibraryRequest) -> ShortcutStore {
        let held = self.held.clone();
        let device = self.device.clone();
        let signer = Arc::clone(&self.signer);
        let runtime = self.runs.clone();
        Box::pin(async move {
            runtime
                .spawn(store_shortcut(held, device, signer, request))
                .await
                .map_err(|_| "this device stopped before it saved the shortcut".to_owned())?
        })
    }

    fn runtime(&self) -> Result<tokio::runtime::Handle, String> {
        Ok(self.runs.clone())
    }
}

/// Store `request`'s change in its workspace: checked against the triggers this device can
/// watch, signed as its saver's own device, then committed through the service that holds the
/// workspace.
async fn store_shortcut(
    held: Source,
    device: DeviceCert,
    signer: Arc<dyn DeviceSigner>,
    request: ShortcutLibraryRequest,
) -> Result<(), String> {
    let person = hex::encode(device.user_public_key);
    if request.actor.person_id() != Some(person.as_str()) {
        return Err("a shortcut is saved from its saver's own device".into());
    }
    let change = match &request.change {
        ShortcutChange::Save(shortcut) => {
            let mut shortcut = shortcut.as_ref().clone();
            // A trigger this device cannot watch would be saved into a shortcut that never
            // runs: the person is told now.
            for trigger in &shortcut.triggers {
                validate_trigger(trigger)?;
            }
            tilo_peer_state::workspace::sign_saved_shortcut(
                &request.workspace,
                &mut shortcut,
                &device,
                signer.as_ref(),
            )?;
            ShortcutChange::Save(Box::new(shortcut))
        }
        delete @ ShortcutChange::Delete(_) => delete.clone(),
    };
    let workspaces = held.workspaces().await;
    let command = WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(change));
    // A workspace another device serves takes the change in this device's member copy and sends
    // it there; the workspace founded here is committed as its governor.
    if workspaces
        .is_member(&request.workspace)
        .await
        .unwrap_or(false)
    {
        let commit = workspaces
            .member_membership_write(&request.workspace, command)
            .await?;
        workspaces
            .commit_member_document(&request.workspace, commit)
            .await?;
    } else {
        workspaces
            .commit_own_membership(&request.workspace, command, None)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "shortcuts_tests.rs"]
mod tests;
