//! The person's saved shortcuts, as this device's broker keeps them.
//!
//! A shortcut a person saves is one of its workspace's settings: signed here by this device as
//! its saver's own, then committed through the service that holds the workspace, where every
//! device of the workspace reads it back from and this device's trigger runner arms it. The
//! broker publishes the library and each saved shortcut's `run` only where this service is
//! installed, so a computer keeps the person's shortcuts without a desktop session open on it.

use std::sync::Arc;

use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};
use tilo_workspace_service::apps::automation::{ShortcutChange, validate_trigger};
use tilo_workspace_service::apps::services::{
    ShortcutLibraryRequest, ShortcutLibraryServices, ShortcutStore,
};

use super::workspaces::ServiceWorkspaces;

/// The person's saved shortcuts, as this device's broker keeps and runs them.
pub(crate) struct Shortcuts {
    /// The workspaces this device holds: where a saved shortcut is committed.
    workspaces: Arc<ServiceWorkspaces>,
    /// Where a shortcut's run executes and the change that saves one is committed.
    runs: tokio::runtime::Handle,
}

impl Shortcuts {
    /// A library over the workspaces `workspaces` holds, whose runs execute on a runtime of
    /// their own.
    ///
    /// # Errors
    /// The runtime could not be started.
    pub(crate) fn new(workspaces: Arc<ServiceWorkspaces>) -> std::io::Result<Self> {
        Ok(Self {
            workspaces,
            runs: automation_runtime()?,
        })
    }
}

/// Where a shortcut's run executes and the change that saves one is committed: a thread of its
/// own, started once for this process, so a run waiting on a step keeps the rest of the daemon
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
        let workspaces = Arc::clone(&self.workspaces);
        let runtime = self.runs.clone();
        Box::pin(async move {
            runtime
                .spawn(store_shortcut(workspaces, request))
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
    workspaces: Arc<ServiceWorkspaces>,
    request: ShortcutLibraryRequest,
) -> Result<(), String> {
    let device = workspaces.device_cert();
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
                device,
                workspaces.signer().as_ref(),
            )?;
            ShortcutChange::Save(Box::new(shortcut))
        }
        delete @ ShortcutChange::Delete(_) => delete.clone(),
    };
    let command = WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(change));
    let copies = workspaces.copies();
    // A workspace another device serves takes the change in this device's member copy and sends
    // it there; the workspace founded here is committed as its governor.
    if copies.is_member(&request.workspace).await.unwrap_or(false) {
        let commit = copies
            .member_membership_write(&request.workspace, command)
            .await?;
        copies
            .commit_member_document(&request.workspace, commit)
            .await?;
    } else {
        copies
            .commit_own_membership(&request.workspace, command, None)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "shortcuts_tests.rs"]
mod tests;
