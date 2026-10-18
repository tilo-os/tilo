//! Monitor's readings on Linux: the session's system information service.
//!
//! Monitor runs in a sandbox with a process namespace of its own, so its
//! `/proc` shows Monitor and nothing else. The session reads the machine for
//! it instead and pushes a snapshot at Monitor's cadence over the connection
//! the launcher opened (`tilo_system_stats`): processor, memory, disks,
//! network and the person's processes, each with the app it belongs to.
//! Command lines come only with their own grant, environments never.

use tilo_system_stats::client::{Client, ClientSubscription, Notify};
use tilo_system_stats::protocol::{ProcessStats, Snapshot};

use super::{StopKind, note_provider_read};
use crate::model::{MonitorModel, ProcessDataStatus, ProcessInfo};

/// The app this is, when it dials the session itself (an unconfined run).
const APP_ID: &str = "so.tilo.monitor";

/// The cadence before the person's preference is known.
const DEFAULT_INTERVAL_MS: u32 = 1500;

enum Source {
    /// This process's connection, found on first use.
    Shared,
    /// A connection handed in (a test's service).
    Given(Client),
}

struct Live {
    client: Client,
    subscription: ClientSubscription,
    interval_ms: u32,
}

/// Monitor's subscription to the session's readings.
pub struct Sampler {
    /// Where readings come from; `None` for a sample that reads nothing.
    source: Option<Source>,
    live: Option<Live>,
    waker: Option<ui::WakeHandle>,
    /// The cadence asked for; `None` while paused.
    interval_ms: Option<u32>,
}

impl Sampler {
    fn over(source: Option<Source>) -> Self {
        Self {
            source,
            live: None,
            waker: None,
            interval_ms: Some(DEFAULT_INTERVAL_MS),
        }
    }

    /// Readings from the session, subscribed on first use.
    pub fn new() -> Self {
        // A test process has no session: it serves its own, over this
        // machine, before anything could dial a real one.
        #[cfg(test)]
        super::install_test_session();
        Self::over(Some(Source::Shared))
    }

    /// Readings over `client`'s connection.
    pub fn with_client(client: Client) -> Self {
        Self::over(Some(Source::Given(client)))
    }

    /// Storage for a deterministic sample: never connects, never reads.
    pub(crate) fn inert() -> Self {
        Self::over(None)
    }

    fn client(&self) -> Option<Client> {
        match self.source.as_ref()? {
            Source::Shared => Client::shared(session_socket, APP_ID),
            Source::Given(client) => Some(client.clone()),
        }
    }

    /// Readings are pushed by the session, each one waking the window.
    pub fn is_pushed(&self) -> bool {
        self.source.is_some()
    }

    /// Wake the window with `waker` whenever a snapshot arrives.
    pub fn set_waker(&mut self, waker: ui::WakeHandle) {
        self.waker = Some(waker);
        // The current subscription was made without it.
        self.live = None;
    }

    /// Follow the cadence the person chose; `None` pauses, which ends the
    /// subscription: a paused Monitor costs the session nothing.
    pub fn set_interval(&mut self, interval_ms: Option<u32>) {
        if self.interval_ms != interval_ms {
            self.interval_ms = interval_ms;
            self.live = None;
        }
    }

    /// The live subscription at the current cadence, made when missing.
    fn live(&mut self) -> Option<&Live> {
        let interval_ms = self.interval_ms?;
        if self
            .live
            .as_ref()
            .is_some_and(|live| live.interval_ms != interval_ms || live.subscription.is_closed())
        {
            self.live = None;
        }
        if self.live.is_none() {
            let client = self.client()?;
            note_provider_read();
            let notify = self
                .waker
                .clone()
                .map(|waker| Box::new(move || waker.wake()) as Notify);
            let subscription = client.subscribe(interval_ms, true, notify);
            self.live = Some(Live {
                client,
                subscription,
                interval_ms,
            });
        }
        self.live.as_ref()
    }

    /// Fold the newest pushed snapshot into `model`. Returns whether the
    /// model changed.
    pub fn refresh(&mut self, model: &mut MonitorModel) -> bool {
        if self.source.is_none() || self.interval_ms.is_none() {
            // A sample, or paused: nothing is subscribed, nothing changes.
            return false;
        }
        let Some(live) = self.live() else {
            return unavailable(model);
        };
        if live.subscription.refusal().is_some() {
            return refused(model);
        }
        match live.subscription.take() {
            Some(snapshot) => {
                fold(&snapshot, model);
                true
            }
            None if live.subscription.is_closed() => unavailable(model),
            None => false,
        }
    }

    /// Read now, as the person asked: a new subscription, whose first
    /// snapshot the session sends at once, waited for.
    pub fn refresh_now(&mut self, model: &mut MonitorModel) -> bool {
        if self.source.is_none() {
            return false;
        }
        let paused = self.interval_ms.is_none();
        if paused {
            // One reading, then nothing again.
            self.interval_ms = Some(tilo_system_stats::protocol::MAX_INTERVAL_MS);
        }
        self.live = None;
        let changed = match self.live() {
            None => unavailable(model),
            Some(live) => match live.subscription.wait() {
                Some(snapshot) => {
                    fold(&snapshot, model);
                    true
                }
                None if live.subscription.refusal().is_some() => refused(model),
                None => unavailable(model),
            },
        };
        if paused {
            self.interval_ms = None;
            self.live = None;
        }
        changed
    }

    fn connection(&self) -> Result<Client, String> {
        if let Some(live) = &self.live {
            return Ok(live.client.clone());
        }
        self.client()
            .ok_or_else(|| "the system information service is not running".to_owned())
    }

    /// End the process `pid` of the lifetime `started_at` through the
    /// session, which allows it only for the person's own apps.
    pub fn signal_if_current(&mut self, pid: u32, started_at: u64, kind: StopKind) -> bool {
        self.connection().is_ok_and(|client| {
            client
                .end_process(pid, started_at, kind == StopKind::Kill)
                .is_ok()
        })
    }

    /// End every process of `app`.
    ///
    /// # Errors
    /// Why the session refused.
    pub fn end_app(&mut self, app: &str) -> Result<u32, String> {
        self.connection()?
            .end_app(app, false)
            .map_err(|refusal| refusal.to_string())
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

/// The session's socket, where an unconfined copy finds the service.
fn session_socket() -> Option<std::path::PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())?;
    Some(
        std::path::PathBuf::from(runtime)
            .join("tilo/services")
            .join(format!("{}.sock", tilo_system_stats::SERVICE)),
    )
}

fn unavailable(model: &mut MonitorModel) -> bool {
    let changed = model.process_status != ProcessDataStatus::Unavailable;
    model.process_status = ProcessDataStatus::Unavailable;
    changed
}

fn refused(model: &mut MonitorModel) -> bool {
    let changed = model.process_status != ProcessDataStatus::PermissionDenied;
    model.processes.clear();
    model.process_status = ProcessDataStatus::PermissionDenied;
    changed
}

/// Fold one snapshot into the model the panes draw.
pub(crate) fn fold(snapshot: &Snapshot, model: &mut MonitorModel) {
    if let Some(system) = &snapshot.system {
        let m = &mut model.metrics;
        m.cores = system.cpu.as_ref().map_or_else(
            || vec![0.0; system.host.logical_cpus as usize],
            |cpu| cpu.per_core.clone(),
        );
        if let Some(model_name) = &system.host.cpu_model {
            m.cpu_brand.clone_from(model_name);
        }
        if !system.host.hostname.is_empty() {
            m.host.clone_from(&system.host.hostname);
        }
        if !system.host.kernel.is_empty() {
            m.kernel = format!("Linux {}", system.host.kernel);
        }
        m.mem_total = system.memory.total;
        m.mem_used = system.memory.used();
        m.swap_total = system.memory.swap_total;
        m.swap_used = system.memory.swap_used();
        m.uptime_secs = system.host.uptime_secs;
        m.load_avg = system.load_average;
        m.net_rx = system.network.rx_bytes_per_sec;
        m.net_tx = system.network.tx_bytes_per_sec;
        m.disk_read = system.disks.iter().map(|d| d.read_bytes_per_sec).sum();
        m.disk_write = system.disks.iter().map(|d| d.write_bytes_per_sec).sum();
        m.cpu_hist.push(m.cpu_overall());
        m.mem_hist.push(m.mem_frac());
        m.net_rx_hist.push(m.net_rx);
        m.net_tx_hist.push(m.net_tx);
        m.disk_read_hist.push(m.disk_read);
        m.disk_write_hist.push(m.disk_write);
    }
    if snapshot.processes_included {
        model.processes = snapshot.processes.iter().map(process).collect();
        model.process_status = if model.processes.is_empty() {
            ProcessDataStatus::Empty
        } else {
            ProcessDataStatus::Ready
        };
    } else {
        // Readings without the list: the person has not allowed it.
        model.processes.clear();
        model.process_status = ProcessDataStatus::PermissionDenied;
    }
}

fn process(row: &ProcessStats) -> ProcessInfo {
    ProcessInfo {
        pid: row.pid,
        started_at: row.started_at,
        ppid: row.ppid,
        name: row.name.clone(),
        user: row.user.clone(),
        cpu: row.cpu,
        mem_bytes: row.memory_bytes,
        virt_bytes: row.virtual_bytes,
        threads: row.threads as usize,
        status: row.state.label().to_owned(),
        cmd: row.command.clone().unwrap_or_default(),
        run_secs: row.run_secs,
        open_files: None,
        kernel: row.kernel_thread,
        app: row.app.clone(),
    }
}
