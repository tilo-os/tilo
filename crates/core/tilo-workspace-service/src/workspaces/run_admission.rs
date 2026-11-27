//! What an in-process run admitted to a workspace is pinned to.
//!
//! A host that carries a person's agent runs admits each run to one exact
//! workspace, as one device: the caller it authenticated, its certificate and
//! the enrollment of the copy the workspace is held under. Every call the run
//! makes is checked against that material, so a run whose workspace or device
//! changed ends rather than acting for an identity it was not admitted for.
//!
//! A member copy's material moves when the copy is enrolled again: the
//! service fences the runs admitted under the enrollment it replaces. A
//! workspace this service hosts itself cannot change identity while it
//! holds it, so a run admitted there is pinned to the device alone.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

use tilo_actions::remote::Caller;
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::join::TransportHint;
use tilo_user_identity::DeviceCert;

use super::{Workspaces, system_time};

const MAX_EXECUTIONS: usize = 512;

/// Why an admitted run may not go on.
///
/// The client sees the refusal; only what it can act on is said, so a
/// changed ownership (the workspace or device this run was admitted for is gone)
/// and ownership that could not be read (a disk or identity store that failed
/// this once) are never the same answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The run ended: an ownership change this run was admitted for, or the
    /// caller's own request to end it.
    Ended,
    /// The workspace this run was admitted to moved to this device while the call waited:
    /// nothing was done, and the call run again is admitted to it here.
    Moved,
    /// This call could not be checked because the identity material could
    /// not be read. Nothing changed; the run stays live and the call may be
    /// retried.
    Unavailable { reason: String },
    /// The call came from a caller this run was not admitted for.
    Stranger,
    /// The key the run acts under is one its person's root revoked: it acts no more, however
    /// long ago this run was admitted, and the refusal says so and what to do about it.
    Revoked { reason: String },
}

impl Refusal {
    /// The message the client sees. It carries no ownership material, only
    /// what happened to the check.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Ended => "execution session ended".into(),
            Self::Moved => "this workspace moved while your call waited; it is kept on this device now. Nothing was done; run it again.".into(),
            Self::Unavailable { reason } => format!(
                "the caller's ownership could not be verified, so this call was not run: {reason}; nothing changed, retry the call"
            ),
            Self::Stranger => "the caller may not invoke this action".into(),
            Self::Revoked { reason } => reason.clone(),
        }
    }
}

impl From<Refusal> for tilo_actions::remote::ActionCallError {
    /// The client sees what it can act on: an ended run stays ended, and a
    /// check that could not be made names itself as retryable.
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Moved => Self::Unavailable {
                reason: refusal.message(),
            },
            Refusal::Unavailable { reason } => Self::Unavailable { reason },
            Refusal::Revoked { reason } => Self::Refused { reason },
            Refusal::Ended | Refusal::Stranger => Self::NotPermitted,
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for Refusal {}

/// What a run was admitted under, compared on every call.
#[allow(
    clippy::large_enum_variant,
    reason = "a member copy's pin is its serving device's signed address; the hosted case keeps nothing, and there is one of these per run"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pinned {
    /// A member copy: the device serving it and the enrollment that admitted
    /// this device there.
    Member {
        hint: TransportHint,
        continuity: u64,
    },
    /// A workspace this service hosts itself.
    Own,
}

/// One run admitted to a workspace, held for as long as the run is.
pub struct SessionAdmission {
    caller: Caller,
    workspace: WorkspaceId,
    certificate: DeviceCert,
    pinned: Pinned,
    presentation: Vec<u8>,
    _registration: ExecutionRegistration,
}

impl SessionAdmission {
    /// The caller this run acts as, as the broker admits it.
    #[must_use]
    pub fn certificate(&self) -> &DeviceCert {
        &self.certificate
    }

    /// The certified caller this run is.
    #[must_use]
    pub fn caller(&self) -> &Caller {
        &self.caller
    }

    /// The identity this run presents to the broker's app links.
    #[must_use]
    pub fn presentation(&self) -> &[u8] {
        &self.presentation
    }

    /// Whether this call may run as the run's caller, now.
    pub fn check(&self, workspaces: &Workspaces) -> Result<(), Refusal> {
        if !self.caller.execution_is_live() {
            return Err(self.ended(workspaces));
        }
        // A key its person revoked acts no more: the run ends with its key, however long it
        // was admitted before the revocation answered.
        if let Some(revoked) = workspaces.revoked(&self.caller) {
            return Err(revoked);
        }
        self.check_original_ownership(workspaces)
    }

    /// Compare the ownership this run was admitted with to the workspace's now.
    fn check_original_ownership(&self, workspaces: &Workspaces) -> Result<(), Refusal> {
        if self.current(workspaces) != self.pinned {
            tracing::warn!(
                workspace = %self.workspace,
                "an admitted run's workspace or device identity changed; ending the run"
            );
            self.caller.retire_execution();
            return Err(self.ended(workspaces));
        }
        Ok(())
    }

    /// Why this run ended: the workspace moved here from the copy it was admitted to, or not.
    fn ended(&self, workspaces: &Workspaces) -> Refusal {
        match (&self.pinned, self.current(workspaces)) {
            (Pinned::Member { .. }, Pinned::Own) => Refusal::Moved,
            _ => Refusal::Ended,
        }
    }

    /// What `workspace`'s ownership is now: the member copy's material, or a
    /// workspace hosted here. A run admitted to a member copy whose copy is gone
    /// (this device holds the workspace itself now, or no longer has it) is
    /// ended, and the next run is admitted to what this device has now.
    fn current(&self, workspaces: &Workspaces) -> Pinned {
        workspaces
            .member_material(self.workspace)
            .map_or(Pinned::Own, |material| Pinned::Member {
                hint: material.hint,
                continuity: material.continuity,
            })
    }
}

impl Workspaces {
    /// Admit a run of an agent, in `workspace`, as `actor`, on the device
    /// `own` certifies, presenting `presentation` (the identity the app links
    /// verify): the caller the run's calls are, and the material every later
    /// call is checked against. `own` is this embedder's device, which must be
    /// the device the member copy acts as for a workspace another device
    /// serves.
    ///
    /// # Errors
    ///
    /// This device does not have the workspace, the device is
    /// not the one its copy acts as, or the actor is not the person that
    /// device acts for.
    pub async fn admit_execution(
        &self,
        workspace: &str,
        actor: &str,
        own: &DeviceCert,
        presentation: Vec<u8>,
    ) -> Result<Arc<SessionAdmission>, String> {
        let selected = WorkspaceId::parse(workspace).ok_or("invalid exact MCP workspace")?;
        if selected.is_nil() || selected.to_string() != workspace {
            return Err("noncanonical MCP workspace".into());
        }
        // A device placed for records runs nothing as its person: what it asks is decided by its
        // host, from its certificate, as the records it reaches.
        if own.capabilities.records.is_some() {
            return Err("a device placed for records runs nothing as its person".into());
        }
        let pinned = if let Some(material) = self.member_material(selected) {
            if material.cert.device_public_key != own.device_public_key {
                return Err("the member copy acts as another device".into());
            }
            Pinned::Member {
                hint: material.hint,
                continuity: material.continuity,
            }
        } else {
            // Why this device does not have it (stopped for the part that starts next, its
            // record did not read, or never here) is the reason the run is refused.
            self.workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            Pinned::Own
        };
        // A run of this device's own person, whose root certifies this very device, is on their
        // own device. Any other certificate is a foreign device, as is one its root issued for a
        // foreign device.
        let on_this_device = own.device_public_key == self.device_cert().device_public_key
            && !own.capabilities.foreign
            && tilo_user_identity::verify_device_cert(own).is_ok();
        let caller = Caller::authenticated(own.user_public_key, own.device_public_key)
            .in_workspace(workspace)
            .from_foreign_device(!on_this_device)
            .in_new_execution();
        if caller.acting().person_id() != Some(actor) {
            caller.retire_execution();
            return Err(
                "selected workspace identity differs from the runtime profile actor".into(),
            );
        }
        self.admitted_run(selected, caller, own, pinned, presentation)
    }

    /// Admit a run of the key `key` certifies, in `workspace`, a workspace this service hosts,
    /// presenting `presentation` (the key's own proof for the workspace): the caller its calls
    /// are is the key, carried beside its person ([`Caller::own_key`]), so each listing names
    /// the key, while every call is decided by the person's grants, narrowed by the key's
    /// certificate ([`Self::enter`] keeps a key's grant as it admits it). An agent's grant is
    /// the agent alone, and its calls are decided by what the workspace grants that agent.
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here (a member copy's key enters where it is kept), or the
    /// key's grant does not reach it.
    pub async fn admit_key(
        &self,
        workspace: &str,
        key: &DeviceCert,
        presentation: Vec<u8>,
    ) -> Result<Arc<SessionAdmission>, String> {
        let selected = WorkspaceId::parse(workspace).ok_or("invalid exact workspace")?;
        if self.member_material(selected).is_some() {
            return Err("this workspace is kept on another device; its keys act there".into());
        }
        // A grant reaches the workspaces its person approved it for, and no others: a key for
        // one workspace acts in that one alone.
        if let Some(scope) = &key.capabilities.agent
            && !scope.reaches(workspace)
        {
            return Err("this key's grant does not reach that workspace".into());
        }
        let held = self
            .workspace(workspace)
            .await
            .map_err(|_| "this device does not keep that workspace")?;
        // A server key is the person at the key, as its certificate says
        // ([`tilo_user_identity::DeviceCapabilities::own_key_in`]), carrying the key beside them
        // so a listing names it. Any other grant is an agent's: it acts as its own member, never
        // as the person who delegated it, and every call of its run is decided by the grant its
        // person signed for it — the workspace's grants naming that agent
        // ([`tilo_peer_state::key_caller::caller_of_key`]).
        let caller = tilo_peer_state::key_caller::caller_of_key(key, async {
            let governed = held.lock().await;
            Ok(governed
                .peer()
                .permissions(
                    key.device_public_key,
                    system_time(tilo_clock::unix_now_ns()),
                )
                .map_err(|_| "the workspace holds no grant for this key")?
                .granted)
        })
        .await?
        .in_workspace(workspace)
        .from_foreign_device(true)
        .in_new_execution();
        self.admitted_run(selected, caller, key, Pinned::Own, presentation)
    }

    /// Register `caller`'s run in `selected`, pinned to `pinned`, and check it once.
    fn admitted_run(
        &self,
        selected: WorkspaceId,
        caller: Caller,
        cert: &DeviceCert,
        pinned: Pinned,
        presentation: Vec<u8>,
    ) -> Result<Arc<SessionAdmission>, String> {
        // The one owner observer is already installed. Register this exact
        // lease, then re-read the pinned material: a concurrent fence either
        // closes the lease or is detected by the changed enrollment.
        let service = self.clone();
        let expected = pinned.clone();
        let registration =
            self.inner
                .executions
                .register_checked(selected, &caller, move || {
                    let current = match service.member_material(selected) {
                        Some(material) => Pinned::Member {
                            hint: material.hint,
                            continuity: material.continuity,
                        },
                        None if matches!(expected, Pinned::Own) => Pinned::Own,
                        None => {
                            return Err("the member copy this run was admitted to is gone".into());
                        }
                    };
                    if current != expected {
                        return Err("workspace changed while admitting execution".into());
                    }
                    Ok(())
                })?;
        let admission = Arc::new(SessionAdmission {
            caller,
            workspace: selected,
            certificate: cert.clone(),
            pinned,
            presentation,
            _registration: registration,
        });
        self.inner.executions.attach(&admission)?;
        admission.check(self).map_err(|refusal| refusal.message())?;
        Ok(admission)
    }

    /// Re-check every run admitted to `workspace`: a run whose material
    /// changed ends.
    ///
    /// # Errors
    ///
    /// A run admitted to the workspace ended.
    pub fn recheck_executions(&self, workspace: WorkspaceId) -> Result<(), String> {
        // Baselines come only from host-admitted sessions, never a presented
        // certificate. Release the registry lock before material reads.
        let admissions = self.inner.executions.admissions(workspace)?;
        let mut result = Ok(());
        for admission in admissions {
            // An execution may end normally after the snapshot. Its lifetime
            // says nothing about the workspace's ownership, so only the pinned
            // material is compared; a retired run stays retired.
            if let Err(refusal @ (Refusal::Ended | Refusal::Moved)) =
                admission.check_original_ownership(self)
            {
                result = Err(refusal.message());
            }
        }
        result
    }
}

/// The runs admitted to this service's workspaces, and the signal that ends
/// those of a member copy whose enrollment moved.
pub(super) struct ExecutionPins {
    entries: Mutex<HashMap<uuid::Uuid, ExecutionEntry>>,
}

struct ExecutionEntry {
    workspace: WorkspaceId,
    lease: tilo_actions::remote::ExecutionLease,
    admission: Weak<SessionAdmission>,
}

impl ExecutionPins {
    pub(super) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// End every run admitted to `workspace`: its copy was enrolled again.
    pub(super) fn fence(&self, workspace: WorkspaceId) {
        // Called synchronously under the enrollment's own lock. Only atomic
        // lease closure happens here: no owner reread, SDK or user code.
        let entries = self.entries.lock_recover();
        for entry in entries.values() {
            if entry.workspace == workspace {
                entry.lease.close();
            }
        }
    }

    pub(super) fn register_checked(
        self: &Arc<Self>,
        workspace: WorkspaceId,
        caller: &Caller,
        recheck: impl FnOnce() -> Result<(), String>,
    ) -> Result<ExecutionRegistration, String> {
        let result = (|| {
            if caller.workspace() != Some(workspace.to_string().as_str())
                || !caller.execution_is_live()
            {
                return Err("execution audience or lifetime unavailable".into());
            }
            let id = caller
                .execution_id()
                .ok_or("execution identity unavailable")?;
            let lease = caller
                .execution_lease()
                .ok_or("execution lifetime unavailable")?;
            {
                let mut entries = self
                    .entries
                    .lock()
                    .map_err(|_| "execution registry unavailable")?;
                if entries.len() >= MAX_EXECUTIONS || entries.contains_key(&id) {
                    return Err("execution registration unavailable".into());
                }
                entries.insert(
                    id,
                    ExecutionEntry {
                        workspace,
                        lease,
                        admission: Weak::new(),
                    },
                );
            }
            let registration = ExecutionRegistration {
                caller: caller.clone(),
                pins: Arc::downgrade(self),
            };
            // A fence during recheck closes this very lease; no lock is held
            // while calling recheck.
            recheck()?;
            if !caller.execution_is_live() {
                return Err("execution fenced during admission".into());
            }
            Ok(registration)
        })();
        if result.is_err() {
            caller.retire_execution();
        }
        result
    }

    pub(super) fn attach(&self, admission: &Arc<SessionAdmission>) -> Result<(), String> {
        let id = admission
            .caller()
            .execution_id()
            .ok_or("execution identity unavailable")?;
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "execution registry unavailable")?;
        let entry = entries
            .get_mut(&id)
            .ok_or("execution registration unavailable")?;
        if !entry.lease.is_live() {
            return Err("execution fenced before attachment".into());
        }
        entry.admission = Arc::downgrade(admission);
        drop(entries);
        Ok(())
    }

    pub(super) fn admissions(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<Arc<SessionAdmission>>, String> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| "execution registry unavailable")?;
        Ok(entries
            .values()
            .filter(|entry| entry.workspace == workspace && entry.lease.is_live())
            .filter_map(|entry| entry.admission.upgrade())
            .collect())
    }
}

impl Drop for ExecutionPins {
    fn drop(&mut self) {
        for entry in self
            .entries
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            entry.lease.close();
        }
    }
}

pub(super) struct ExecutionRegistration {
    caller: Caller,
    pins: Weak<ExecutionPins>,
}

impl Drop for ExecutionRegistration {
    fn drop(&mut self) {
        self.caller.retire_execution();
        if let Some(pins) = self.pins.upgrade()
            && let Some(id) = self.caller.execution_id()
        {
            pins.entries.lock_recover().remove(&id);
        }
    }
}

#[cfg(test)]
#[path = "run_admission_tests.rs"]
mod tests;
