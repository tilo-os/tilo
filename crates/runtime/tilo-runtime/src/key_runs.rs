//! What a device's own workspace host answers a key of its person, beside its
//! governance: the record actions of the workspaces it hosts.
//!
//! Every Tilo host has one external surface. A key of the person (another of
//! their devices, an agent's grant, a server key of theirs) opens a session
//! with the host, enters a workspace there, and runs that workspace's actions
//! addressed to the host's own device. A relay answers them with its app
//! workers; a person's device answers them with its own action broker, which
//! is what [`KeyRuns`] puts behind the host
//! ([`HostConfig::apps`](tilo_room_host::host::HostConfig::apps)).
//!
//! Each key works in a run of its own, which the device's broker admits as it
//! admits an agent at the device's agent door ([`Runs::open`]). Before every
//! message the key is checked against the person's own store of their devices
//! ([`PersonsDevices::admit`]: theirs, current, not revoked) and its grant's
//! scope against the workspace; a key that no longer holds loses its run.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use tilo_actions::remote::{ActionHost, ActionHostError, Caller, Pending, RemoteActionMessage};
use tilo_lock::MutexRecover as _;
use tilo_room_host::app_serving::AppServingPlane;
use tilo_user_identity::DeviceCert;

use crate::remote_door::PersonsDevices;

/// The most runs one device keeps for its person's keys at once.
const MAX_RUNS: usize = 256;

/// An action host every call of one run goes through, as the caller it
/// admitted.
pub type RunHost = Arc<dyn ActionHost + Send + Sync>;

/// A device's action broker as a key's runs reach it.
pub trait Runs: Send + Sync {
    /// The workspace (its UUID) the host's session calls `workspace_id`, when
    /// this device answers its actions.
    fn workspace(&self, workspace_id: u64) -> Option<String>;

    /// A run for `key` in `workspace`: the host its calls go through and the
    /// caller they go as. The broker admits it as it admits an agent at this
    /// device's agent door; a refusal says why.
    ///
    /// # Errors
    /// The broker does not admit a run for the key.
    fn open(&self, workspace: &str, key: &DeviceCert) -> Result<(RunHost, Caller), String>;
}

/// The record actions of the workspaces a device hosts, answered for its
/// person's keys by its own broker: the device's
/// [`AppServingPlane`](tilo_room_host::app_serving::AppServingPlane).
pub struct KeyRuns {
    /// The person's devices and the broker, once they run: a host opens
    /// before the store and the broker it serves.
    serving: OnceLock<Serving>,
    open: std::sync::Mutex<HashMap<(u64, [u8; 32]), Run>>,
}

struct Serving {
    devices: Arc<dyn PersonsDevices>,
    runs: Arc<dyn Runs>,
}

struct Run {
    cert: DeviceCert,
    host: RunHost,
    caller: Caller,
}

impl KeyRuns {
    /// Runs nobody answers yet: every message is refused as not running
    /// until [`Self::serve`].
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            serving: OnceLock::new(),
            open: std::sync::Mutex::default(),
        })
    }

    /// Answer the keys `devices` says are the person's through `runs` from
    /// now on. A second call is ignored.
    pub fn serve(&self, devices: Arc<dyn PersonsDevices>, runs: Arc<dyn Runs>) {
        let _ = self.serving.set(Serving { devices, runs });
    }

    /// The run `key` works in on `workspace_id`, opened now if it has none,
    /// once the key is the person's and its scope reaches the workspace.
    fn run(
        &self,
        workspace_id: u64,
        key: &DeviceCert,
    ) -> Result<(RunHost, Caller), ActionHostError> {
        let Serving { devices, runs } =
            self.serving
                .get()
                .ok_or_else(|| ActionHostError::Unavailable {
                    reason: "this device's actions are not running yet".into(),
                })?;
        let workspace = runs
            .workspace(workspace_id)
            .ok_or(ActionHostError::UnknownAction)?;
        if let Err(refusal) = devices.admit(key.device_public_key, key) {
            self.end(workspace_id, &key.device_public_key);
            return Err(ActionHostError::Refused {
                reason: refusal.reason(),
            });
        }
        if key
            .capabilities
            .agent
            .is_some_and(|scope| !scope.reaches(&workspace))
        {
            return Err(ActionHostError::Refused {
                reason: crate::remote_door::Refusal::OutOfScope.reason(),
            });
        }
        let mut open = self.open.lock_recover();
        if let Some(run) = open.get(&(workspace_id, key.device_public_key))
            && run.cert == *key
        {
            return Ok((Arc::clone(&run.host), run.caller.clone()));
        }
        // A key that renewed its certificate starts its run again under the
        // one it shows now.
        let renewed = open.remove(&(workspace_id, key.device_public_key));
        if let Some(run) = renewed {
            run.host.retire_execution(&run.caller);
        }
        if open.len() >= MAX_RUNS {
            return Err(ActionHostError::Unavailable {
                reason: "this device runs as many of its person's keys as it takes".into(),
            });
        }
        let (host, caller) = runs
            .open(&workspace, key)
            .map_err(|reason| ActionHostError::Refused { reason })?;
        open.insert(
            (workspace_id, key.device_public_key),
            Run {
                cert: key.clone(),
                host: Arc::clone(&host),
                caller: caller.clone(),
            },
        );
        drop(open);
        Ok((host, caller))
    }

    /// End the run `device` works in on `workspace_id`, if it has one.
    fn end(&self, workspace_id: u64, device: &[u8; 32]) {
        let ended = self.open.lock_recover().remove(&(workspace_id, *device));
        if let Some(run) = ended {
            run.host.retire_execution(&run.caller);
        }
    }
}

impl AppServingPlane for KeyRuns {
    /// Only a key's certified session reaches a run: without the certificate
    /// there is no key to hold to its scope.
    fn serve(
        &self,
        _workspace_id: u64,
        _caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        tilo_actions::remote::refuse(&message, ActionHostError::NotPermitted).map(Pending::ready)
    }

    fn serve_certified(
        &self,
        workspace_id: u64,
        _caller: &Caller,
        certificate: &DeviceCert,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        match self.run(workspace_id, certificate) {
            Ok((host, caller)) => tilo_actions::remote::serve(&host, &caller, message),
            Err(refusal) => tilo_actions::remote::refuse(&message, refusal).map(Pending::ready),
        }
    }

    fn forget_workspace(&self, workspace_id: u64) {
        let ended: Vec<Run> = {
            let mut open = self.open.lock_recover();
            let keys: Vec<_> = open
                .keys()
                .filter(|(workspace, _)| *workspace == workspace_id)
                .copied()
                .collect();
            let ended = keys.iter().filter_map(|key| open.remove(key)).collect();
            drop(open);
            ended
        };
        for run in ended {
            run.host.retire_execution(&run.caller);
        }
    }
}
