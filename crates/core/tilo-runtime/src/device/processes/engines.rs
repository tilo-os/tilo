//! Engine jobs: the work an app asks of its device (`engine.start`), run where its op declared it
//! may run, and recorded as the device's ([`super::super::jobs::DeviceJobs`]).
//!
//! A job is named by its record, `devices/<device>/jobs/<job>`; every process this device starts
//! is told where those are (`ToProcess::JobsUnder`) before anything else, and names the jobs it
//! starts beneath it. Each move a job makes is said by the process running it
//! (`FromProcess::Job`) and kept as its record, whose handlers the daemon runs.
//!
//! Work that needs nothing of the app's own (no released secret, no network, nothing the app's
//! process keeps) runs in the device's one engine host: a tilolang process started alone
//! ([`Alone::Engines`]), in the sandbox every tilolang process here runs in, granted none of what
//! any app keeps, holding no network and receiving no secret. The app's process asks for it
//! (`FromProcess::EngineStart`), and the device admits it as that process's app and workspace and
//! no other's, refusing work the shared host may not run. The host's moves are recorded as that
//! app's, never as the host's. The host lives while any job runs, then idles out like every
//! process here ([`super::IDLE`]). The app's process may end while its job runs: the job
//! finishes, and its record says how.
//!
//! Work that needs what only the app's own process may hold runs there, in its own sandbox under
//! its own grants; the device holds that process while the job runs, as it holds one running a
//! call.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use runtime_portal::process_link::{EngineJob, Packet as _, ToProcess};
use serde_json::{Value, json};

use super::super::jobs::{DeviceJobs, ProcessKey, ended_state};
use super::{Alone, HostKey, Processes};

/// A job the device admitted and has not seen end.
struct Known {
    /// The process that started it, and which start of it.
    starter: (HostKey, u64),
    /// The process that runs it, and which start of it.
    runs_in: (HostKey, u64),
    /// Whose job it is, as the device admitted it.
    app: String,
    workspace: String,
}

/// Where each engine job this device admitted runs, by its path, until it ends.
#[derive(Default)]
pub(super) struct Engines {
    known: HashMap<String, Known>,
}

impl Engines {
    /// The process `key` (its start `generation`) ended: the jobs it ran are no longer known, and
    /// the device's records say they failed.
    pub(super) fn ended_process(&mut self, key: &HostKey, generation: u64) {
        self.known
            .retain(|_, known| !(known.runs_in.0 == *key && known.runs_in.1 == generation));
    }
}

/// The storage the engine host keeps what it compiles in, beside every workspace's processes: no
/// app id starts with a dot.
pub(super) const ENGINE_STORAGE: &str = ".engines";

/// The key the device's job records name the process `key` (its start `generation`) by.
fn process_key(key: &HostKey, generation: u64) -> ProcessKey {
    ProcessKey {
        workspace: key.workspace.clone(),
        app: key.app.clone(),
        generation,
    }
}

/// The path the jobs of the device `device` are kept under: `devices/<device hex>/jobs`.
pub(super) fn jobs_under(device: &[u8; 32]) -> String {
    tilo_actions::remote::device_job_path(device, "")
        .trim_end_matches('/')
        .to_owned()
}

/// The job id `path` names among `jobs`' records, when it names one of this device's.
fn job_id(jobs: &DeviceJobs, path: &str) -> Option<String> {
    tilo_actions::remote::parse_job_path(path)
        .filter(|(device, _)| *device == jobs.device())
        .map(|(_, id)| id)
}

impl Processes {
    pub(super) fn engines(&self) -> std::sync::MutexGuard<'_, Engines> {
        self.engines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Tell the process `key`, just started on `socket`, where its jobs are kept: a process that
    /// runs apps' code or their engines; one that runs a task or a page starts none.
    pub(super) fn tell_jobs_under(
        &self,
        key: &HostKey,
        socket: &runtime_portal::process_link::Descriptor,
    ) {
        if !matches!(key.alone, None | Some(Alone::Engines)) {
            return;
        }
        let Some(under) = self.jobs().map(|jobs| jobs_under(&jobs.device())) else {
            return;
        };
        if let Err(error) = ToProcess::JobsUnder(under).send(socket) {
            tracing::warn!(%error, app = %key.app, "a process was not told where its jobs are kept");
        }
    }

    /// The process `from` (its start `generation`) starts the engine job `job` as its own app in
    /// its own workspace: run on the device's engine host, or, for work that needs what only the
    /// app's own process may hold, run there while the device holds that process.
    pub(super) fn engine_start(self: &Arc<Self>, from: &HostKey, generation: u64, job: &EngineJob) {
        let Some(jobs) = self.jobs().cloned() else {
            tracing::warn!(job = job.job, app = %from.app, "this device keeps no record of jobs, so it runs none");
            return;
        };
        let starter = process_key(from, generation);
        // What the job's record says from its first word: whose call started it, for whom, and
        // what its app asked to run as it moves and once it ends.
        let first = |state: &str, detail: Value| {
            let mut record = json!({
                "invocation": job.invocation,
                "by": job.by,
                "op": job.op,
                "state": state,
                "detail": detail,
            });
            if let Some(fields) = record.as_object_mut() {
                for kind in ["done", "progress"] {
                    if let Some(handler) =
                        job.handlers.get(kind).filter(|handler| !handler.is_null())
                    {
                        fields.insert(kind.to_owned(), handler.clone());
                    }
                }
            }
            record
        };
        let Some(id) = job_id(&jobs, &job.job) else {
            tracing::warn!(job = job.job, app = %from.app, "a job is named beneath the device's records");
            return;
        };
        let refuse = |why: String, runs_in: &ProcessKey| {
            tracing::warn!(
                job = job.job,
                app = job.app,
                op = job.op,
                why,
                "an engine job was refused"
            );
            jobs.moved(runs_in, &id, first("failed", json!({ "why": why })));
        };
        if from.alone.is_some() || job.app != from.app || job.workspace != from.workspace {
            // Recorded as the process's own app, never as the one it named.
            return refuse(
                format!(
                    "a process starts jobs only for its own app: this one runs {} in {}",
                    from.app, from.workspace
                ),
                &starter,
            );
        }
        if self.engines().known.contains_key(&job.job) {
            return refuse("a job of that name runs already".to_owned(), &starter);
        }
        let known = |runs_in: (HostKey, u64)| Known {
            starter: (from.clone(), generation),
            runs_in,
            app: job.app.clone(),
            workspace: job.workspace.clone(),
        };
        if job.here {
            // Its own process runs it, under its own grants: the device records it, which holds
            // the process until it ends.
            self.engines()
                .known
                .insert(job.job.clone(), known((from.clone(), generation)));
            jobs.moved(&starter, &id, first("queued", json!({})));
            return;
        }
        if !job.needs.shared() {
            return refuse(
                format!(
                    "`{}` needs what only {}'s own process may hold, which a process several apps \
                     share never receives",
                    job.op, job.app
                ),
                &starter,
            );
        }
        let host = HostKey {
            workspace: String::new(),
            app: String::new(),
            alone: Some(Alone::Engines),
            runtime: from.runtime.clone(),
            app_runtime: from.app_runtime.clone(),
        };
        // Recorded before the host hears of it, as the app's job the host runs: the record holds
        // the host from then on, and the host's first move lands on it.
        let recorded = |runs: u64| {
            self.engines()
                .known
                .insert(job.job.clone(), known((host.clone(), runs)));
            jobs.moved_for(
                &process_key(&host, runs),
                (&job.workspace, &job.app),
                &id,
                first("queued", json!({})),
            );
        };
        if let Err(error) = self.hand_engine(&host, job.clone(), recorded) {
            self.engines().known.remove(&job.job);
            refuse(
                format!("the device's engine host did not start: {error}"),
                &starter,
            );
        }
    }

    /// Hand `job` to the engine host `key`, starting it first when none runs, once `recorded`
    /// noted which start of the host takes it.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the host is chosen, started and handed the job in one hold, so it never idles out in between"
    )]
    fn hand_engine(
        self: &Arc<Self>,
        key: &HostKey,
        job: EngineJob,
        recorded: impl FnOnce(u64),
    ) -> std::io::Result<()> {
        let message = ToProcess::Engine(Box::new(job));
        let mut running = self.running();
        if let Some(host) = running.get_mut(key) {
            recorded(host.generation);
            match message.send(&host.socket) {
                Ok(()) => {
                    host.idle_since = None;
                    return Ok(());
                }
                Err(error) => {
                    tracing::info!(%error, "the engine host's socket is closed");
                    running.remove(key);
                    return Err(error);
                }
            }
        }
        let host = self.start(key, None)?;
        recorded(host.generation);
        message.send(&host.socket)?;
        running.insert(key.clone(), host);
        tracing::info!(runtime = %key.runtime, "the device's engine host started");
        Ok(())
    }

    /// The process `from` (its start `generation`) said the job `job` moved, to `record`. A process
    /// tells only of jobs the device admitted for it to run, each recorded as the app it runs it
    /// for: the device's engine host of every app's shared work, an app's process of its own.
    pub(super) fn job_moved(
        self: &Arc<Self>,
        from: &HostKey,
        generation: u64,
        job: &str,
        record: Value,
    ) {
        let Some(jobs) = self.jobs().cloned() else {
            return;
        };
        let Some(id) = job_id(&jobs, job) else {
            tracing::warn!(job, app = %from.app, "a process told of a job beneath no record of this device");
            return;
        };
        let ended = record
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(ended_state);
        let owner = self.engines().known.get(job).and_then(|known| {
            (known.runs_in.0 == *from && known.runs_in.1 == generation)
                .then(|| (known.workspace.clone(), known.app.clone()))
        });
        let Some((workspace, app)) = owner else {
            tracing::warn!(job, app = %from.app, "a process told of a job it does not run");
            return;
        };
        jobs.moved_for(
            &process_key(from, generation),
            (&workspace, &app),
            &id,
            record,
        );
        if ended {
            self.engines().known.remove(job);
            self.idle_when_unheld(from, generation);
        }
    }

    /// The process `key` (its start `generation`) ran a job that ended: once it runs nothing, it
    /// idles out like every process here.
    fn idle_when_unheld(self: &Arc<Self>, key: &HostKey, generation: u64) {
        if self.holds_jobs(key, generation) {
            return;
        }
        let idle = self
            .running()
            .get_mut(key)
            .filter(|process| process.generation == generation && process.instances.is_empty())
            .is_some_and(|process| {
                process.idle_since = Some(Instant::now());
                true
            });
        if idle {
            self.idle_later();
        }
    }

    /// The process `from` asks to stop the job `job` it started: the process running it is told.
    pub(super) fn engine_cancel(&self, from: &HostKey, job: &str) {
        let runs_in = self
            .engines()
            .known
            .get(job)
            .filter(|known| known.starter.0 == *from)
            .map(|known| known.runs_in.clone());
        let Some((key, generation)) = runs_in else {
            tracing::warn!(job, app = %from.app, "a stop was asked of a job its process did not start");
            return;
        };
        let socket = self
            .running()
            .get(&key)
            .filter(|process| process.generation == generation)
            .map(|process| Arc::clone(&process.socket));
        let told = socket.map(|socket| {
            ToProcess::CancelJob {
                job: job.to_owned(),
            }
            .send(&socket)
        });
        if !matches!(told, Some(Ok(()))) {
            tracing::warn!(job, "the process running a job was not told to stop it");
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
