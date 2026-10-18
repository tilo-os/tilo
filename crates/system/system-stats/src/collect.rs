//! Two readings into rates, and one reading into what a subscriber may see.

#![allow(
    clippy::cast_precision_loss,
    reason = "rates and shares are shown to a few significant digits"
)]

use std::collections::HashMap;

use crate::apps::{EndPolicy, app_of_cgroup};
use crate::probe::{CpuTicks, Probe, RawSample, RawSystem, USER_HZ};
use crate::protocol::{
    AppUsage, CpuStats, DiskStats, Grants, HostInfo, InterfaceStats, NetworkStats, ProcessState,
    ProcessStats, Snapshot, SystemStats,
};

/// A full reading, rates included, before it is cut to a subscriber's
/// grants.
#[derive(Clone, Debug, Default)]
pub struct Reading {
    pub taken_ms: u64,
    pub window_ms: u64,
    pub system: Option<SystemStats>,
    /// `None` when the reading did not include processes.
    pub processes: Option<Vec<ProcessStats>>,
    pub apps: Vec<AppUsage>,
}

impl Reading {
    /// What a subscriber holding `grants` sees; `processes` says whether it
    /// asked for the process list.
    #[must_use]
    pub fn cut(&self, grants: Grants, processes: bool) -> Snapshot {
        let list = processes && grants.processes;
        let commands = grants.holds(crate::protocol::Grant::CommandLines);
        Snapshot {
            taken_ms: self.taken_ms,
            window_ms: self.window_ms,
            system: if grants.system_info {
                self.system.clone()
            } else {
                None
            },
            apps: if list { self.apps.clone() } else { Vec::new() },
            processes: match (&self.processes, list) {
                (Some(rows), true) => rows
                    .iter()
                    .map(|row| ProcessStats {
                        command: if commands { row.command.clone() } else { None },
                        ..row.clone()
                    })
                    .collect(),
                _ => Vec::new(),
            },
            processes_included: list && self.processes.is_some(),
        }
    }
}

/// The counters a rate is measured from.
#[derive(Default)]
struct Previous {
    at_ms: Option<u64>,
    cpu_total: Option<CpuTicks>,
    cpu_cores: Vec<CpuTicks>,
    disks: HashMap<String, (u64, u64)>,
    interfaces: HashMap<String, (u64, u64)>,
    /// `(pid, started_at)` → cumulative processor ticks.
    processes: HashMap<(u32, u64), u64>,
}

/// Turns readings into rates against the reading before.
pub struct Collector {
    probe: Box<dyn Probe>,
    policy: EndPolicy,
    previous: Previous,
}

impl Collector {
    #[must_use]
    pub fn new(probe: Box<dyn Probe>, policy: EndPolicy) -> Self {
        Self {
            probe,
            policy,
            previous: Previous::default(),
        }
    }

    pub fn probe_mut(&mut self) -> &mut dyn Probe {
        self.probe.as_mut()
    }

    #[must_use]
    pub fn policy(&self) -> &EndPolicy {
        &self.policy
    }

    /// Forget the counters: the next reading starts a new interval.
    pub fn reset(&mut self) {
        self.previous = Previous::default();
    }

    /// Take a reading at `now_ms` (milliseconds since the epoch).
    pub fn read(&mut self, want: crate::probe::Want, now_ms: u64) -> Reading {
        let raw: RawSample = self.probe.read(want);
        let window_ms = self
            .previous
            .at_ms
            .map_or(0, |at| now_ms.saturating_sub(at));
        let seconds = window_ms as f32 / 1000.0;
        let system = raw
            .system
            .map(|system| self.system(system, seconds, window_ms > 0));
        let cores = system
            .as_ref()
            .map(|s| s.host.logical_cpus)
            .filter(|n| *n > 0)
            .unwrap_or_else(|| self.previous.cpu_cores.len().try_into().unwrap_or(1))
            .max(1);
        let processes = raw
            .processes
            .map(|rows| self.processes(rows, seconds, cores, now_ms));
        let apps = processes.as_deref().map(group_apps).unwrap_or_default();
        self.previous.at_ms = Some(now_ms);
        Reading {
            taken_ms: now_ms,
            window_ms,
            system,
            processes,
            apps,
        }
    }

    fn system(&mut self, raw: RawSystem, seconds: f32, has_window: bool) -> SystemStats {
        let previous = &mut self.previous;
        let cpu = match (previous.cpu_total, raw.cpu_total) {
            (Some(before), Some(after)) if has_window => Some(CpuStats {
                total: load(before, after),
                per_core: raw
                    .cpu_cores
                    .iter()
                    .zip(
                        previous
                            .cpu_cores
                            .iter()
                            .chain(std::iter::repeat(&CpuTicks::default())),
                    )
                    .map(|(after, before)| load(*before, *after))
                    .collect(),
            }),
            _ => None,
        };
        previous.cpu_total = raw.cpu_total;
        previous.cpu_cores.clone_from(&raw.cpu_cores);

        let rate = |before: Option<u64>, after: u64| match before {
            Some(before) if seconds > 0.0 => after.saturating_sub(before) as f32 / seconds,
            _ => 0.0,
        };
        let disks = raw
            .disks
            .into_iter()
            .map(|disk| {
                let before = previous.disks.get(&disk.device).copied();
                previous
                    .disks
                    .insert(disk.device.clone(), (disk.read_bytes, disk.written_bytes));
                DiskStats {
                    read_bytes_per_sec: rate(before.map(|b| b.0), disk.read_bytes),
                    write_bytes_per_sec: rate(before.map(|b| b.1), disk.written_bytes),
                    device: disk.device,
                    mount: disk.mount,
                    total_bytes: disk.total_bytes,
                    used_bytes: disk.used_bytes,
                }
            })
            .collect();
        let interfaces: Vec<InterfaceStats> = raw
            .interfaces
            .into_iter()
            .map(|interface| {
                let before = previous.interfaces.get(&interface.name).copied();
                previous.interfaces.insert(
                    interface.name.clone(),
                    (interface.rx_bytes, interface.tx_bytes),
                );
                InterfaceStats {
                    rx_bytes_per_sec: rate(before.map(|b| b.0), interface.rx_bytes),
                    tx_bytes_per_sec: rate(before.map(|b| b.1), interface.tx_bytes),
                    name: interface.name,
                }
            })
            .collect();
        SystemStats {
            host: HostInfo {
                hostname: raw.hostname,
                kernel: raw.kernel,
                cpu_model: raw.cpu_model,
                logical_cpus: raw.cpu_cores.len().try_into().unwrap_or(u32::MAX),
                uptime_secs: raw.uptime_secs,
            },
            cpu,
            load_average: raw.load_average,
            memory: raw.memory,
            pressure: raw.pressure,
            disks,
            network: NetworkStats {
                rx_bytes_per_sec: interfaces.iter().map(|i| i.rx_bytes_per_sec).sum(),
                tx_bytes_per_sec: interfaces.iter().map(|i| i.tx_bytes_per_sec).sum(),
                interfaces,
            },
            gpus: raw.gpus,
            batteries: raw.batteries,
            thermal: raw.thermal,
        }
    }

    fn processes(
        &mut self,
        rows: Vec<crate::probe::RawProcess>,
        seconds: f32,
        cores: u32,
        now_ms: u64,
    ) -> Vec<ProcessStats> {
        let boot = self.probe.boot_time();
        let now_secs = now_ms / 1000;
        let mut seen = HashMap::with_capacity(rows.len());
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let key = (row.pid, row.started_at);
            let cpu = match self.previous.processes.get(&key) {
                Some(before) if seconds > 0.0 => {
                    let ticks = row.cpu_ticks.saturating_sub(*before) as f32;
                    // A share of the whole machine: one of eight cores busy
                    // reads 12.5.
                    (ticks / USER_HZ as f32 / seconds / cores as f32 * 100.0).clamp(0.0, 100.0)
                }
                _ => 0.0,
            };
            seen.insert(key, row.cpu_ticks);
            let run_secs = boot.map_or(0, |boot| {
                now_secs.saturating_sub(boot.saturating_add(row.started_at / USER_HZ))
            });
            let endable = self
                .policy
                .check(row.pid, row.uid, row.cgroup.as_deref(), row.kernel_thread)
                .is_ok();
            out.push(ProcessStats {
                pid: row.pid,
                ppid: row.ppid,
                started_at: row.started_at,
                app: row.cgroup.as_deref().and_then(app_of_cgroup),
                user: self.probe.user_name(row.uid),
                uid: row.uid,
                cpu,
                memory_bytes: row.resident_bytes,
                virtual_bytes: row.virtual_bytes,
                threads: row.threads,
                state: ProcessState::from_code(row.state),
                run_secs,
                kernel_thread: row.kernel_thread,
                command: row.command,
                endable,
                name: row.name,
            });
        }
        self.previous.processes = seen;
        out
    }
}

/// The share of ticks spent busy between two readings, in percent.
fn load(before: CpuTicks, after: CpuTicks) -> f32 {
    let total = after.total.saturating_sub(before.total);
    if total == 0 {
        return 0.0;
    }
    let busy = after.busy.saturating_sub(before.busy).min(total);
    busy as f32 / total as f32 * 100.0
}

/// The processes that belong to an app, taken together per app, busiest
/// first.
fn group_apps(processes: &[ProcessStats]) -> Vec<AppUsage> {
    let mut apps: Vec<AppUsage> = Vec::new();
    for process in processes {
        let Some(app) = &process.app else { continue };
        if let Some(usage) = apps.iter_mut().find(|usage| &usage.app == app) {
            usage.processes = usage.processes.saturating_add(1);
            usage.cpu += process.cpu;
            usage.memory_bytes = usage.memory_bytes.saturating_add(process.memory_bytes);
            usage.endable |= process.endable;
        } else {
            apps.push(AppUsage {
                app: app.clone(),
                processes: 1,
                cpu: process.cpu,
                memory_bytes: process.memory_bytes,
                endable: process.endable,
            });
        }
    }
    apps.sort_by(|a, b| {
        b.cpu
            .total_cmp(&a.cpu)
            .then(b.memory_bytes.cmp(&a.memory_bytes))
            .then_with(|| a.app.cmp(&b.app))
    });
    apps
}
