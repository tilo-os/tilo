//! The person's workspaces as the desktop reads them: how each one stands, as the device's daemon
//! publishes it (`tilo_peer_state::workspace_view`), and the asks the desktop makes of them
//! through the daemon.
//!
//! The desktop keeps no copy of any workspace. It watches the daemon's readings and holds the
//! last of each here, so its loop reads them without waiting; every change to a workspace is an
//! action the daemon takes ([`ShellAsks`]). The methods keep the names the desktop read its own
//! workspaces' state by, so a reader asks the same question of the daemon's answer.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tilo_action_policy::WorkspacePolicy;
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::join::Role;
pub use tilo_peer_state::workspace_view::{
    Holder, Hosting, Listed, NotDeletable, WorkspaceReading,
};
use tilo_peer_state::workspace_view::{LIST, workspace_of};
use tilo_peer_state::{Member, MemberList};
use tilo_server_session::workspaces::{HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE};

use crate::engine::ServiceAnswer;

/// The daemon's actions, as the desktop asks them: what changes a workspace, founds, joins or
/// leaves one, and what its governance and the host answer.
pub trait ShellAsks: Send + Sync {
    /// Invoke `action` of the account resource `resource` (`account/workspaces#create` and the
    /// rest) with `arguments`, answered with its output.
    fn account(
        &self,
        resource: &str,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> ServiceAnswer;

    /// Invoke `request` as the person on this device, answered with its output once it settled:
    /// a workspace's governance action, or one of the host's own operations.
    fn invoke(&self, request: tilo_actions::remote::InvokeRequest) -> ServiceAnswer;
}

/// How the person's workspaces stand, as the daemon last said, and the asks that change them.
pub struct WorkspaceView {
    person: [u8; 32],
    device: [u8; 32],
    listed: std::sync::Mutex<Vec<Listed>>,
    readings: std::sync::Mutex<BTreeMap<WorkspaceId, WorkspaceReading>>,
    /// The workspaces the desktop is founding now, and those whose founding failed until the
    /// next one ends.
    founding: std::sync::Mutex<BTreeSet<WorkspaceId>>,
    failed: std::sync::Mutex<BTreeSet<WorkspaceId>>,
    asks: Arc<dyn ShellAsks>,
}

impl std::fmt::Debug for WorkspaceView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceView")
            .field("workspaces", &self.workspaces())
            .finish_non_exhaustive()
    }
}

impl WorkspaceView {
    /// The view of `person`'s workspaces on the device `device`, asking `asks`.
    #[must_use]
    pub fn new(person: [u8; 32], device: [u8; 32], asks: Arc<dyn ShellAsks>) -> Self {
        Self {
            person,
            device,
            listed: std::sync::Mutex::default(),
            readings: std::sync::Mutex::default(),
            founding: std::sync::Mutex::default(),
            failed: std::sync::Mutex::default(),
            asks,
        }
    }

    /// Take a reading the daemon published of `resource`: the list, or one workspace's standing;
    /// `None` once it publishes it no more. Answers whether it changed what the view says.
    ///
    /// # Errors
    /// The reading does not read as the resource's.
    pub fn apply(&self, resource: &str, reading: Option<&str>) -> Result<bool, String> {
        if resource == LIST {
            let listed: Vec<Listed> = match reading {
                Some(reading) => serde_json::from_str(reading)
                    .map_err(|error| format!("the workspace list does not read: {error}"))?,
                None => Vec::new(),
            };
            let mut current = self.listed.lock_recover();
            let changed = *current != listed;
            *current = listed;
            return Ok(changed);
        }
        let workspace = workspace_of(resource)
            .and_then(WorkspaceId::parse)
            .ok_or_else(|| format!("{resource} names no workspace"))?;
        let mut readings = self.readings.lock_recover();
        let Some(reading) = reading else {
            return Ok(readings.remove(&workspace).is_some());
        };
        let reading: WorkspaceReading = serde_json::from_str(reading)
            .map_err(|error| format!("{resource} does not read: {error}"))?;
        Ok(readings.insert(workspace, reading.clone()).as_ref() != Some(&reading))
    }

    fn with<T>(
        &self,
        workspace: WorkspaceId,
        read: impl FnOnce(&WorkspaceReading) -> T,
    ) -> Option<T> {
        self.readings.lock_recover().get(&workspace).map(read)
    }

    fn with_named<T>(
        &self,
        workspace: &str,
        read: impl FnOnce(&WorkspaceReading) -> T,
    ) -> Result<T, String> {
        let id = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.with(id, read)
            .ok_or_else(|| format!("this device holds no workspace {workspace}"))
    }

    /// The person's workspaces, as the daemon lists them.
    #[must_use]
    pub fn listed(&self) -> Vec<Listed> {
        self.listed.lock_recover().clone()
    }

    /// The workspaces this device holds.
    #[must_use]
    pub fn workspaces(&self) -> Vec<WorkspaceId> {
        self.readings.lock_recover().keys().copied().collect()
    }

    #[must_use]
    pub fn holds(&self, workspace: WorkspaceId) -> bool {
        self.readings.lock_recover().contains_key(&workspace)
    }

    #[must_use]
    pub fn is_founding(&self, workspace: WorkspaceId) -> bool {
        self.founding.lock_recover().contains(&workspace)
    }

    #[must_use]
    pub fn founding_failed(&self, workspace: WorkspaceId) -> bool {
        self.failed.lock_recover().contains(&workspace)
    }

    #[must_use]
    pub fn workspace_name(&self, workspace: WorkspaceId) -> Option<String> {
        self.with(workspace, |reading| reading.name.clone())
            .flatten()
    }

    #[must_use]
    pub fn awaited_host(&self, workspace: WorkspaceId) -> Option<String> {
        self.hosting(workspace)
            .filter(|hosting| !hosting.admitted)
            .map(|hosting| hosting.host)
    }

    #[must_use]
    pub fn hosting(&self, workspace: WorkspaceId) -> Option<Hosting> {
        self.with(workspace, |reading| reading.hosting.clone())
            .flatten()
    }

    #[must_use]
    pub fn is_holder(&self, workspace: &str) -> bool {
        WorkspaceId::parse(workspace)
            .is_some_and(|workspace| self.holder_of(workspace) == Some(self.device))
    }

    #[must_use]
    pub fn reads_whole(&self, workspace: WorkspaceId) -> bool {
        self.with(workspace, |reading| reading.reads_whole)
            .unwrap_or(false)
    }

    /// # Errors
    /// `workspace` names no workspace this device holds.
    pub fn roster(&self, workspace: &str) -> Result<Vec<String>, String> {
        self.with_named(workspace, |reading| reading.roster.clone())
    }

    /// # Errors
    /// `workspace` names no workspace this device holds.
    pub fn roster_devices(&self, workspace: &str) -> Result<Vec<[u8; 32]>, String> {
        self.with_named(workspace, |reading| reading.roster_devices.clone())
    }

    /// # Errors
    /// `workspace` names no workspace this device holds.
    pub fn people(&self, workspace: &str) -> Result<Vec<Member>, String> {
        self.with_named(workspace, |reading| reading.people.clone())
    }

    /// # Errors
    /// `workspace` names no workspace this device holds.
    pub fn members(&self, workspace: &str) -> Result<MemberList, String> {
        self.with_named(workspace, |reading| reading.members.clone())
    }

    /// # Errors
    /// `workspace` names no workspace this device holds, or its policy has not arrived.
    pub fn policy(&self, workspace: &str) -> Result<WorkspacePolicy, String> {
        self.with_named(workspace, |reading| reading.policy.clone())?
            .ok_or_else(|| "the workspace's policy has not arrived".to_owned())
    }

    #[must_use]
    pub fn workspace_document(&self, workspace: &str) -> Option<Vec<u8>> {
        self.with_named(workspace, |reading| reading.document.clone())
            .ok()
            .flatten()
    }

    #[must_use]
    pub fn administers(&self, workspace: &str) -> bool {
        self.with_named(workspace, |reading| reading.administers)
            .unwrap_or(false)
    }

    #[must_use]
    pub fn name_of(&self, workspace: &str, person: [u8; 32]) -> Option<String> {
        self.with_named(workspace, |reading| reading.name_of(person))
            .ok()
            .flatten()
    }

    #[must_use]
    pub fn may_invite(&self, workspace: &str) -> bool {
        self.with_named(workspace, |reading| reading.may_invite)
            .unwrap_or(false)
    }

    #[must_use]
    pub fn invite_refused(&self, workspace: &str) -> bool {
        self.with_named(workspace, |reading| reading.invite_refused)
            .unwrap_or(false)
    }

    #[must_use]
    pub fn may_invite_administrator(&self, workspace: &str) -> bool {
        self.with_named(workspace, |reading| reading.may_invite_administrator)
            .unwrap_or(false)
    }

    #[must_use]
    pub fn may_delete(&self, workspace: WorkspaceId) -> bool {
        self.not_deletable(workspace).is_none()
    }

    #[must_use]
    pub fn not_deletable(&self, workspace: WorkspaceId) -> Option<NotDeletable> {
        self.with(workspace, |reading| reading.not_deletable)
            .unwrap_or(Some(NotDeletable::NotHeld))
    }

    #[must_use]
    pub fn invitable_roles(&self, workspace: &str) -> Vec<Role> {
        self.with_named(workspace, |reading| reading.invitable_roles.clone())
            .unwrap_or_default()
    }

    /// # Errors
    /// `workspace` names no workspace this device holds, or its governance names no owner.
    pub fn owner_of(&self, workspace: &str) -> Result<String, String> {
        self.with_named(workspace, |reading| reading.owner.clone())?
            .ok_or_else(|| "the workspace's governance names no owner".to_owned())
    }

    #[must_use]
    pub fn write_refusals(&self, workspace: &str) -> Vec<neighbour_wire::WriteRefusal> {
        self.with_named(workspace, |reading| reading.write_refusals.clone())
            .unwrap_or_default()
    }

    /// The verdicts other devices recorded on this device's writes, where waiting changes
    /// nothing, with the workspace each was filed in.
    #[must_use]
    pub fn lasting_refusals(&self) -> Vec<(String, neighbour_wire::WriteRefusal)> {
        self.readings
            .lock_recover()
            .values()
            .flat_map(|reading| {
                reading
                    .write_refusals
                    .iter()
                    .filter(|refusal| {
                        refusal.author == self.device
                            && refusal.by != self.device
                            && refusal.reason.permanent()
                    })
                    .map(|refusal| (reading.id.clone(), refusal.clone()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[must_use]
    pub fn device_labels(&self, workspace: &str) -> BTreeMap<[u8; 32], String> {
        self.with_named(workspace, |reading| reading.device_labels.clone())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn holder_of(&self, workspace: WorkspaceId) -> Option<[u8; 32]> {
        self.with(workspace, |reading| reading.holder).flatten()
    }

    #[must_use]
    pub fn holders(&self, workspace: &str) -> Vec<Holder> {
        self.with_named(workspace, |reading| reading.holders.clone())
            .unwrap_or_default()
    }

    /// Whether a link out of a workspace this device keeps still admits someone: a person who
    /// redeems it dials this device.
    #[must_use]
    pub fn invitations_open(&self, now: tilo_clock::time::SystemTime) -> bool {
        let now = now
            .duration_since(tilo_clock::time::SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        self.readings.lock_recover().values().any(|reading| {
            reading
                .open_links
                .iter()
                .any(|until| u128::from(*until) > now)
        })
    }

    /// The person this view is of.
    #[must_use]
    pub const fn person(&self) -> [u8; 32] {
        self.person
    }

    /// Found `workspace` as `name` on this device: the daemon makes it and keeps it, and it is
    /// here once its reading arrives.
    ///
    /// # Errors
    /// The daemon refused it.
    pub async fn found(&self, workspace: WorkspaceId, name: &str) -> Result<(), String> {
        if self.holds(workspace) || !self.founding.lock_recover().insert(workspace) {
            return Ok(());
        }
        let founded = self
            .asks
            .account(
                LIST,
                "create",
                serde_json::Map::from_iter([
                    ("name".to_owned(), name.into()),
                    ("id".to_owned(), workspace.to_string().into()),
                ]),
            )
            .await
            .map(drop);
        self.founding.lock_recover().remove(&workspace);
        let mut failed = self.failed.lock_recover();
        if founded.is_ok() {
            failed.clear();
        } else {
            failed.insert(workspace);
        }
        founded
    }

    /// Ask `workspace`'s governance action `action` with `arguments`, as the person on this
    /// device: the daemon answers it where it keeps the workspace, and forwards it to the device
    /// keeping it otherwise.
    #[must_use]
    pub fn ask_holder(
        &self,
        workspace: WorkspaceId,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<ServiceAnswer> {
        Some(self.asks.invoke(request(
            tilo_actions::remote::RemoteActionTarget::workspace_governance(
                &workspace.to_string(),
                action,
            ),
            arguments,
        )))
    }

    /// Ask the host's own operation `action` (`machine.plan` and the rest) with `arguments`.
    #[must_use]
    pub fn ask_own_host(
        &self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<ServiceAnswer> {
        Some(self.asks.invoke(request(
            tilo_actions::remote::RemoteActionTarget {
                provider: HOST_ACTIONS_PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    HOST_WORKSPACES_RESOURCE.to_owned(),
                    action,
                ),
            },
            arguments,
        )))
    }

    /// Ask the daemon the account action `action` of `resource` (`account/workspaces#join`,
    /// `#enter`, `#leave`, `#delete` and the rest).
    #[must_use]
    pub fn ask_account(
        &self,
        resource: &str,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> ServiceAnswer {
        self.asks.account(resource, action, arguments)
    }
}

/// `target` invoked with `arguments`, as the person's press.
fn request(
    target: tilo_actions::remote::RemoteActionTarget,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> tilo_actions::remote::InvokeRequest {
    tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target,
        argument: tilo_actions::remote::InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}
