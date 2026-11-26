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
use tilo_app_host::automation::{ShortcutChange, unzoned, validate_trigger};
use tilo_app_host::services::{ShortcutLibraryRequest, ShortcutLibraryServices, ShortcutStore};
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::Workspaces;

/// The person's saved shortcuts, as a device's broker keeps and runs them.
pub struct Shortcuts {
    /// The workspaces this device holds: where a saved shortcut is committed.
    held: Workspaces,
    /// The device this library signs as: the one its workspace has admitted.
    device: DeviceCert,
    /// Signs as `device`: the shortcut's own proof of who saved it.
    signer: Arc<dyn DeviceSigner>,
    /// Where a shortcut's run executes and the change that saves one is committed.
    runs: tokio::runtime::Handle,
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
        Ok(Self {
            held: workspaces,
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
    held: Workspaces,
    device: DeviceCert,
    signer: Arc<dyn DeviceSigner>,
    request: ShortcutLibraryRequest,
) -> Result<(), String> {
    let person = hex::encode(device.user_public_key);
    // An agent saves for the person it acts for, signed with its own key; a person saves from
    // a device of their own.
    let agent = match (request.actor.agent_id(), request.acting_for.as_deref()) {
        (Some(agent), Some(acts_for)) => {
            let root: [u8; 32] = hex::decode(acts_for)
                .ok()
                .and_then(|root| root.try_into().ok())
                .ok_or("the agent acts for nobody this device knows")?;
            Some((agent.to_owned(), root))
        }
        (Some(_), None) => return Err("an agent saves a shortcut only for its person".into()),
        (None, _) if request.actor.person_id() != Some(person.as_str()) => {
            return Err("a shortcut is saved from its saver's own device".into());
        }
        (None, _) => None,
    };
    let change = match &request.change {
        ShortcutChange::Save(shortcut) => {
            let mut shortcut = shortcut.as_ref().clone();
            // A time of day is on the clock of the person it is saved for, whichever device
            // keeps it later.
            let saved_for = agent
                .as_ref()
                .map_or(device.user_public_key, |(_, root)| *root);
            let mut zone = None;
            for trigger in &mut shortcut.triggers {
                let Some(unzoned) = unzoned(trigger) else {
                    continue;
                };
                if zone.is_none() {
                    zone = Some(held.zone_of(&saved_for).await.ok_or(
                        "this device does not know which time zone the shortcut's person lives \
                         in: name it after the time, as `every day at 08:00 in Europe/Berlin`",
                    )?);
                }
                unzoned.clone_from(&zone);
            }
            // A trigger this device cannot watch would be saved into a shortcut that never
            // runs: the person is told now.
            for trigger in &shortcut.triggers {
                validate_trigger(trigger)?;
            }
            // A change the saver's proof does not cover (a pause) keeps that proof, and so
            // whose shortcut it is; any other change is signed again here.
            let revocations = held.state(&request.workspace).await.and_then(|state| {
                state
                    .admitted_workspace()
                    .map(tilo_peer_state::workspace::workspace_revocations)
            });
            let still_proven = shortcut.saved_proof.is_some()
                && revocations.is_some_and(|revocations| {
                    tilo_peer_state::workspace::proven_saver(
                        &request.workspace,
                        &shortcut,
                        &revocations,
                        tilo_clock::unix_now_ns(),
                    )
                    .is_some()
                });
            match (&agent, still_proven) {
                (_, true) => {}
                (Some((agent, root)), false) => {
                    tilo_app_host::agents::agent_keys::sign_shortcut_as_agent(
                        &held,
                        &request.workspace,
                        agent,
                        root,
                        &mut shortcut,
                    )
                    .await?;
                }
                (None, false) => tilo_peer_state::workspace::sign_saved_shortcut(
                    &request.workspace,
                    &mut shortcut,
                    &device,
                    signer.as_ref(),
                )?,
            }
            ShortcutChange::Save(Box::new(shortcut))
        }
        delete @ ShortcutChange::Delete(_) => delete.clone(),
    };
    let command = WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(change));
    // A workspace another device serves takes the change in this device's member copy and sends
    // it there; the workspace founded here is the one this device hosts, so it commits here.
    held.commit_own_change(&request.workspace, command, None)
        .await
}

#[cfg(test)]
#[path = "shortcuts_tests.rs"]
mod tests;
