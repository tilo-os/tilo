//! The engine jobs this device runs, kept as its own records: `devices/<device>/jobs/<job>`.
//!
//! An engine job is work an app started and the device does: a cut rendered, a file encoded, a
//! page painted. The process that runs the engine reports each move of a job here, and the daemon
//! keeps it as the device's record, read and followed through the one record API
//! ([`tilo_actions::remote::ResourceReads`]) rather than a second path of its own: the address's
//! workspace is [`tilo_actions::remote::device_record_space`], which no workspace is, its app is
//! [`tilo_actions::remote::DEVICE_JOBS`], and its record is the job's id. A view, a script and an
//! agent read a job as they read a workspace's record, and a held read wakes when it moves.
//!
//! A job is in memory only, because it does not outlive the process that runs it: when that
//! process ends, every job of it that had not ended reads failed. A job that did end stays
//! readable for as long as the device runs, so the answer a person waited for is never taken
//! away.
//!
//! The record is a JSON object, written by the process that runs the engine, with the daemon
//! naming its own fields: `{id, app, workspace, invocation, by, op, state, detail}`, its `state`
//! one of `queued`, `running`, `done`, `failed` or `cancelled`. `invocation` is the call that
//! started the job, and `done` and `progress` are the handlers the app asked to run when the job
//! ends and as it moves ([`JobEffects`]).
//!
//! Who reads a job: the device itself, and any caller who may read the app the job runs in the
//! workspace it runs in ([`DeviceJobs::decided_by`]): the app's own read rule decides, so a job
//! never widens what its app shows.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::remote::{
    ActionCallError, Caller, DEVICE_JOBS, Pending, RESOURCE_READ_REFUSED,
    RESOURCE_READS_UNAVAILABLE, RemoteActionMessage, ResourceAddress, ResourceCopy,
    ResourceReadRequest, ResourceReads, ResourceWake, device_record_space,
};
use tilo_lock::MutexRecover as _;

use super::workspaces::ServiceWorkspaces;

/// The device's engine jobs.
pub struct DeviceJobs {
    /// The device whose jobs these are.
    device: [u8; 32],
    /// The jobs as they stand, by id.
    records: Mutex<BTreeMap<String, JobRecord>>,
    /// Moves on with every job made or forgotten, waking a reader that followed one not yet made.
    revision: tokio::sync::watch::Sender<u64>,
    /// The workspaces this device holds, bound once the daemon runs: what decides whether a
    /// caller may read the app a job runs.
    workspaces: OnceLock<std::sync::Weak<ServiceWorkspaces>>,
    /// What the daemon does for a job's invocation, bound once the broker runs.
    effects: OnceLock<Arc<dyn JobEffects>>,
    /// The processes that run the jobs, bound by the daemon: what a cancelled job is told.
    processes: OnceLock<Arc<dyn JobProcesses>>,
}

/// One job as it stands, and the counter a reader following it wakes on.
struct JobRecord {
    value: Value,
    /// The process that reported it: which run of its app and workspace, so a later process of
    /// the same app never inherits its jobs.
    process: ProcessKey,
    revision: tokio::sync::watch::Sender<u64>,
}

/// The process a job runs in, as the socket that reported it names us: the app, the workspace
/// and which run of them, never what the job's own record says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessKey {
    pub workspace: String,
    pub app: String,
    /// The process's start generation: two runs of one app in one workspace are two processes.
    pub generation: u64,
}

pub use tilo_workspace_service::apps::collab_app_broker::JobEffects;

/// The processes that run this device's jobs: what a job cancelled with its invocation is told.
pub trait JobProcesses: Send + Sync {
    /// Stop running job `job` in `process`: the process that reported it, which has not ended.
    fn cancel(&self, process: &ProcessKey, job: &str);
}

/// Whether `state` is one a job does not leave: it ended.
#[must_use]
pub fn ended_state(state: &str) -> bool {
    matches!(state, "done" | "failed" | "cancelled")
}

/// The state `record` says a job is in, `queued` where it names none.
fn state_of(record: &Value) -> &str {
    record
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("queued")
}

/// The invocation `record` names as the one that started its job, when it names one.
fn invocation_of(record: &Value) -> Option<&str> {
    record
        .get("invocation")
        .and_then(Value::as_str)
        .filter(|invocation| !invocation.is_empty())
}

fn refused() -> ActionCallError {
    ActionCallError::Refused {
        reason: RESOURCE_READ_REFUSED.into(),
    }
}

fn unavailable() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: RESOURCE_READS_UNAVAILABLE.into(),
    }
}

impl DeviceJobs {
    /// The jobs of the device whose key is `device`, none yet.
    #[must_use]
    pub fn new(device: [u8; 32]) -> Arc<Self> {
        Arc::new(Self {
            device,
            records: Mutex::new(BTreeMap::new()),
            revision: tokio::sync::watch::Sender::new(0),
            workspaces: OnceLock::new(),
            effects: OnceLock::new(),
            processes: OnceLock::new(),
        })
    }

    /// The device whose jobs these are.
    #[must_use]
    pub const fn device(&self) -> [u8; 32] {
        self.device
    }

    /// The workspaces this device holds, bound once the daemon runs: what decides whether a
    /// caller may read a job's app.
    pub(crate) fn bind(&self, workspaces: &Arc<ServiceWorkspaces>) {
        let _ = self.workspaces.set(Arc::downgrade(workspaces));
    }

    /// What the daemon does for a job's invocation, bound once the broker runs.
    pub fn serve_effects(&self, effects: Arc<dyn JobEffects>) {
        let _ = self.effects.set(effects);
    }

    /// The processes that run the jobs, bound by the daemon: what a cancelled job is told.
    pub fn serve_processes(&self, processes: Arc<dyn JobProcesses>) {
        let _ = self.processes.set(processes);
    }

    /// The job `job` moved, as `process` reported it. A job never leaves an ended state, so a
    /// move that arrives late cannot take back an answer a person already saw.
    pub fn moved(&self, process: &ProcessKey, job: &str, record: Value) {
        if job.is_empty() {
            tracing::warn!(
                workspace = %process.workspace,
                app = %process.app,
                "a job says no id and is forgotten"
            );
            return;
        }
        let (value, first, moved, ended) = {
            let mut records = self.records.lock_recover();
            let before = records.get(job).map(|held| held.value.clone());
            if before
                .as_ref()
                .is_some_and(|before| ended_state(state_of(before)))
            {
                return;
            }
            // A job is one record whose fields move: a move that names only what changed keeps
            // the id, the invocation and the handlers the first word said.
            let mut fields = match before.clone() {
                Some(Value::Object(fields)) => fields,
                _ => Map::new(),
            };
            match record {
                Value::Object(record) => fields.extend(record),
                Value::Null => {}
                other => {
                    fields.insert("detail".to_owned(), other);
                }
            }
            let mut record = fields;
            // Whose job it is comes from the process that runs it, never from what it says of
            // itself.
            record.insert("id".to_owned(), Value::String(job.to_owned()));
            record.insert("app".to_owned(), Value::String(process.app.clone()));
            record.insert(
                "workspace".to_owned(),
                Value::String(process.workspace.clone()),
            );
            let value = Value::Object(record);
            let state = state_of(&value).to_owned();
            let ended = ended_state(&state);
            let first = before.is_none();
            let moved = before
                .as_ref()
                .is_some_and(|before| state_of(before) != state);
            match records.get_mut(job) {
                Some(held) => {
                    held.value = value.clone();
                    held.revision
                        .send_modify(|revision| *revision = revision.wrapping_add(1));
                }
                None => {
                    records.insert(
                        job.to_owned(),
                        JobRecord {
                            value: value.clone(),
                            process: process.clone(),
                            revision: tokio::sync::watch::Sender::new(0),
                        },
                    );
                }
            }
            drop(records);
            (value, first, moved, ended)
        };
        self.created();
        let Some(effects) = self.effects.get() else {
            return;
        };
        let invocation = invocation_of(&value).map(str::to_owned);
        if ended {
            effects.ended(&value);
            if let Some(invocation) = &invocation
                && !self.invocation_live(invocation)
            {
                effects.release(invocation);
            }
        } else {
            if let Some(invocation) = &invocation {
                effects.hold(invocation);
            }
            // A job's first word is its start; every move after it is progress.
            if moved && !first {
                effects.progress(&value);
            }
        }
    }

    /// The process `process` ended: every job of it that had not ended reads failed, because a
    /// job does not outlive the process that runs it.
    pub fn ended_process(&self, process: &ProcessKey) {
        let ended: Vec<Value> = {
            let mut records = self.records.lock_recover();
            let mut ended = Vec::new();
            for held in records.values_mut() {
                if held.process != *process || ended_state(state_of(&held.value)) {
                    continue;
                }
                let mut record = match held.value.clone() {
                    Value::Object(record) => record,
                    _ => Map::new(),
                };
                record.insert("state".to_owned(), Value::String("failed".to_owned()));
                record.insert(
                    "detail".to_owned(),
                    serde_json::json!({ "why": "the process running the job ended" }),
                );
                held.value = Value::Object(record);
                held.revision
                    .send_modify(|revision| *revision = revision.wrapping_add(1));
                ended.push(held.value.clone());
            }
            drop(records);
            ended
        };
        if let Some(effects) = self.effects.get() {
            for record in &ended {
                effects.ended(record);
                if let Some(invocation) = invocation_of(record)
                    && !self.invocation_live(invocation)
                {
                    effects.release(invocation);
                }
            }
        }
        self.created();
    }

    /// How many jobs of `process` have not ended.
    #[must_use]
    pub fn live(&self, process: &ProcessKey) -> usize {
        self.records
            .lock_recover()
            .values()
            .filter(|held| held.process == *process && !ended_state(state_of(&held.value)))
            .count()
    }

    /// Whether `invocation` still runs a job that has not ended.
    #[must_use]
    pub fn invocation_live(&self, invocation: &str) -> bool {
        self.records.lock_recover().values().any(|held| {
            invocation_of(&held.value) == Some(invocation) && !ended_state(state_of(&held.value))
        })
    }

    /// Cancel every job `invocation` runs that has not ended, answering how many: what the
    /// daemon does when the call that started them is cancelled. Each ends as `cancelled`, its
    /// `done` handler running once, and the invocation answers cancelled.
    pub fn cancel_invocation(&self, invocation: &str) -> usize {
        let (ended, processes_running): (Vec<Value>, Vec<(ProcessKey, String)>) = {
            let mut records = self.records.lock_recover();
            let mut ended = Vec::new();
            let mut running = Vec::new();
            for held in records.values_mut() {
                if invocation_of(&held.value) != Some(invocation)
                    || ended_state(state_of(&held.value))
                {
                    continue;
                }
                let mut record = match held.value.clone() {
                    Value::Object(record) => record,
                    _ => Map::new(),
                };
                record.insert("state".to_owned(), Value::String("cancelled".to_owned()));
                record.insert(
                    "detail".to_owned(),
                    serde_json::json!({ "why": "the invocation that started the job was cancelled" }),
                );
                held.value = Value::Object(record);
                held.revision
                    .send_modify(|revision| *revision = revision.wrapping_add(1));
                if let Some(job) = held.value.get("id").and_then(Value::as_str) {
                    running.push((held.process.clone(), job.to_owned()));
                }
                ended.push(held.value.clone());
            }
            drop(records);
            (ended, running)
        };
        if let Some(processes) = self.processes.get() {
            for (process, job) in &processes_running {
                processes.cancel(process, job);
            }
        }
        if let Some(effects) = self.effects.get() {
            for record in &ended {
                effects.ended(record);
            }
        }
        if !ended.is_empty() {
            if let Some(effects) = self.effects.get() {
                effects.release(invocation);
            }
            self.created();
        }
        ended.len()
    }

    /// The app whose read rule decides who reads job `job`, and where: the app's record of the
    /// workspace it runs in. `None` when the device runs no such job.
    #[must_use]
    pub fn decided_by(&self, job: &str) -> Option<ResourceAddress> {
        let records = self.records.lock_recover();
        let record = &records.get(job)?.value;
        let workspace = record.get("workspace")?.as_str()?.to_owned();
        let app = record.get("app")?.as_str()?.to_owned();
        drop(records);
        Some(ResourceAddress {
            workspace,
            app,
            record: String::new(),
        })
    }

    /// The job `job` as it stands, for a reader the daemon has already decided may read it.
    #[must_use]
    pub fn copy(&self, job: &str) -> ResourceCopy {
        let records = self.records.lock_recover();
        match records.get(job) {
            Some(held) => ResourceCopy::reading(
                held.value.clone(),
                Some(revision_of(&held.revision).to_string()),
            ),
            None => ResourceCopy::Absent,
        }
    }

    /// Whether `address` names one of this device's jobs.
    fn names(&self, address: &ResourceAddress) -> bool {
        address.app == DEVICE_JOBS && address.workspace == device_record_space(&self.device)
    }

    /// Whether `caller` may read job `job`: the device itself, and any caller who may read the
    /// app the job runs in its workspace, decided as a read of that app's record is.
    fn admits(&self, caller: &Caller, job: &str) -> Result<(), ActionCallError> {
        if caller.is_system() || caller.device_public_key() == Some(&self.device) {
            return Ok(());
        }
        let address = self.decided_by(job).ok_or_else(refused)?;
        let workspaces = self
            .workspaces
            .get()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(refused)?;
        match workspaces
            .copies()
            .resource_reads()
            .may_read(caller, &address)
            .wait_timeout(Duration::ZERO)
        {
            Ok(Some(Ok(()))) => Ok(()),
            Ok(Some(Err(error))) => Err(error),
            Ok(None) | Err(_) => Err(refused()),
        }
    }

    /// Note that the set of job ids moved, waking a reader that followed one not yet made.
    fn created(&self) {
        self.revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

/// The revision a job stands at.
fn revision_of(revision: &tokio::sync::watch::Sender<u64>) -> u64 {
    *revision.borrow()
}

impl ResourceReads for DeviceJobs {
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        if !self.names(address) {
            return Err(unavailable());
        }
        self.admits(caller, &address.record)?;
        Ok(self.copy(&address.record))
    }

    fn may_read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(if self.names(address) {
            self.admits(caller, &address.record)
        } else {
            Err(unavailable())
        })
    }

    fn list(
        &self,
        caller: &Caller,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<ResourceAddress>, ActionCallError> {
        if app != DEVICE_JOBS || workspace != device_record_space(&self.device) {
            return Err(unavailable());
        }
        // The ids are taken before each is decided: deciding reads the job's record.
        let jobs: Vec<String> = {
            let records = self.records.lock_recover();
            records.keys().cloned().collect()
        };
        Ok(jobs
            .into_iter()
            .filter(|job| self.admits(caller, job).is_ok())
            .map(|job| ResourceAddress {
                workspace: workspace.to_owned(),
                app: DEVICE_JOBS.to_owned(),
                record: job,
            })
            .collect())
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        if !self.names(address) {
            return Err(unavailable());
        }
        self.admits(caller, &address.record)?;
        let mut seen = {
            let records = self.records.lock_recover();
            records.get(&address.record).map_or_else(
                || self.revision.subscribe(),
                |held| held.revision.subscribe(),
            )
        };
        let wake = wake.clone();
        tilo_rt::app().spawn(async move {
            if seen.changed().await.is_ok() {
                wake.wake();
            }
        });
        Ok(())
    }
}

impl tilo_workspace_service::workspaces::DeviceJobReads for DeviceJobs {
    fn device(&self) -> [u8; 32] {
        self.device
    }

    fn decided_by(&self, job: &str) -> Option<ResourceAddress> {
        self.decided_by(job)
    }

    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        ResourceReads::read(self, caller, address)
    }

    fn may_read(&self, caller: &Caller, address: &ResourceAddress) -> Result<(), ActionCallError> {
        if !self.names(address) {
            return Err(unavailable());
        }
        self.admits(caller, &address.record)
    }

    fn list(
        &self,
        caller: &Caller,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<ResourceAddress>, ActionCallError> {
        ResourceReads::list(self, caller, workspace, app)
    }

    fn watch(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        ResourceReads::watch(self, caller, address, wake)
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
