//! Live system sampler. Wraps `sysinfo` and folds a fresh reading into a
//! [`MonitorModel`]. Constructed eagerly (cheap), but only *refreshed* on
//! the app's throttle — so the very first `--shots` frame, which never
//! ticks the throttle, renders the seeded sample data untouched.

use ui::time::Instant;

#[cfg(test)]
use std::cell::Cell;

use sysinfo::{Networks, Pid, ProcessesToUpdate, Signal, System, Users};

use crate::model::{MonitorModel, ProcessDataStatus, ProcessInfo};

#[cfg(test)]
thread_local! {
    static PROVIDER_READ_COUNT: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn note_provider_read() {
    PROVIDER_READ_COUNT.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(test)]
pub(crate) fn reset_provider_read_count() {
    PROVIDER_READ_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn provider_read_count() -> usize {
    PROVIDER_READ_COUNT.with(Cell::get)
}

/// Which POSIX signal a stop action dispatches. `Term` asks a process to
/// shut down cleanly (catchable); `Kill` is the un-catchable hard stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopKind {
    /// `SIGTERM` — polite, catchable termination request ("Stop").
    Term,
    /// `SIGKILL` — immediate, un-catchable kill ("Kill").
    Kill,
}

impl StopKind {
    fn signal(self) -> Signal {
        match self {
            StopKind::Term => Signal::Term,
            StopKind::Kill => Signal::Kill,
        }
    }
}

pub struct Sampler {
    sys: System,
    networks: Networks,
    users: Users,
    last: Option<Instant>,
}

impl Sampler {
    pub fn new() -> Self {
        let mut sys = System::new();
        // Prime CPU + process state so the first real `refresh` produces
        // meaningful deltas rather than a cold zero reading.
        #[cfg(test)]
        note_provider_read();
        sys.refresh_cpu_all();
        #[cfg(test)]
        note_provider_read();
        sys.refresh_memory();
        #[cfg(test)]
        note_provider_read();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        #[cfg(test)]
        note_provider_read();
        let networks = Networks::new_with_refreshed_list();
        #[cfg(test)]
        note_provider_read();
        let users = Users::new_with_refreshed_list();
        Self {
            sys,
            networks,
            users,
            last: None,
        }
    }

    /// Construct storage for a deterministic sample without refreshing any
    /// native provider. The live constructor remains eagerly primed so its
    /// first refresh has meaningful CPU and process deltas.
    pub(crate) fn inert() -> Self {
        Self {
            sys: System::new(),
            networks: Networks::new(),
            users: Users::new(),
            last: None,
        }
    }

    /// Fold a fresh reading into `model`. Network and disk throughput are
    /// computed as byte deltas since the previous call divided by the
    /// elapsed wall time.
    pub fn refresh(&mut self, model: &mut MonitorModel) {
        let now = ui::time::now();
        let dt = self.last.map_or(1.0, |l| (now - l).as_secs_f32()).max(0.05);

        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        self.sys.refresh_processes(ProcessesToUpdate::All, true);
        self.networks.refresh(true);

        let m = &mut model.metrics;
        m.cores = self
            .sys
            .cpus()
            .iter()
            .map(sysinfo::Cpu::cpu_usage)
            .collect();
        if let Some(c) = self.sys.cpus().first() {
            let brand = c.brand().trim().to_string();
            if !brand.is_empty() {
                m.cpu_brand = brand;
            }
        }
        m.mem_total = self.sys.total_memory();
        m.mem_used = self.sys.used_memory();
        m.swap_total = self.sys.total_swap();
        m.swap_used = self.sys.used_swap();
        m.uptime_secs = System::uptime();
        let la = System::load_average();
        m.load_avg = [la.one as f32, la.five as f32, la.fifteen as f32];
        if let Some(h) = System::host_name() {
            m.host = h;
        }
        if let Some(k) = System::kernel_version() {
            m.kernel = format!("Linux {k}");
        }

        // Network throughput: sum byte deltas across interfaces.
        let (mut rx, mut tx) = (0u64, 0u64);
        for (_name, data) in &self.networks {
            rx += data.received();
            tx += data.transmitted();
        }
        m.net_rx = rx as f32 / dt;
        m.net_tx = tx as f32 / dt;

        // Disk throughput: sum per-process read/written byte deltas.
        let (mut dr, mut dw) = (0u64, 0u64);
        for p in self.sys.processes().values() {
            let du = p.disk_usage();
            dr += du.read_bytes;
            dw += du.written_bytes;
        }
        m.disk_read = dr as f32 / dt;
        m.disk_write = dw as f32 / dt;

        // Push histories.
        m.cpu_hist.push(m.cpu_overall());
        m.mem_hist.push(m.mem_frac());
        m.net_rx_hist.push(m.net_rx);
        m.net_tx_hist.push(m.net_tx);
        m.disk_read_hist.push(m.disk_read);
        m.disk_write_hist.push(m.disk_write);

        // Rebuild the process table. sysinfo reads a process's CPU per core;
        // the table shows its share of the whole machine.
        let core_count = self.sys.cpus().len().max(1) as f32;
        let mut procs = Vec::with_capacity(self.sys.processes().len());
        for (pid, p) in self.sys.processes() {
            let name = p.name().to_string_lossy().to_string();
            let user = p
                .user_id()
                .and_then(|uid| self.users.get_user_by_id(uid))
                .map_or_else(|| "—".to_string(), |u| u.name().to_string());
            let threads = p.tasks().map_or(1, std::collections::HashSet::len).max(1);
            let mem = p.memory();
            // Full command line (argv joined); fall back to the bare name when
            // the kernel exposes no argv (kernel threads, zombies).
            let cmd = {
                let joined = p
                    .cmd()
                    .iter()
                    .map(|a| a.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ");
                if joined.trim().is_empty() {
                    format!("[{name}]")
                } else {
                    joined
                }
            };
            let pid_u32 = pid.as_u32();
            procs.push(ProcessInfo {
                pid: pid_u32,
                started_at: p.start_time(),
                ppid: p.parent().map(sysinfo::Pid::as_u32),
                name,
                user,
                cpu: p.cpu_usage() / core_count,
                mem_bytes: mem,
                virt_bytes: p.virtual_memory(),
                threads,
                status: p.status().to_string(),
                cmd,
                run_secs: p.run_time(),
                open_files: host_info::process::open_file_count(pid_u32),
                // Kernel worker threads have no resident memory.
                kernel: mem == 0,
            });
        }
        model.processes = procs;
        model.process_status =
            // `sysinfo`'s refresh is infallible, so an empty listing is paired
            // with a probe of the process table: readable plus no rows is a
            // genuine empty snapshot, an access failure is surfaced.
            process_snapshot_status(model.processes.len(), host_info::process::probe_process_table());

        self.last = Some(now);
    }

    /// Re-check a PID's kernel generation and then send `kind`'s signal.
    ///
    /// `pid` is deliberately not sufficient authorization: it can be reused
    /// between the table sample and a delayed click, palette invocation, or
    /// agent action. Refresh just this process and compare its start-time
    /// tick immediately before dispatch. A vanished or replaced process is a
    /// clean failure, never a signal delivered to its replacement.
    pub fn signal_if_current(&mut self, pid: u32, started_at: u64, kind: StopKind) -> bool {
        let target = Pid::from_u32(pid);
        self.sys
            .refresh_processes(ProcessesToUpdate::Some(&[target]), true);
        self.sys
            .process(target)
            .filter(|p| p.start_time() == started_at)
            .and_then(|p| p.kill_with(kind.signal()))
            .unwrap_or(false)
    }
}

fn process_snapshot_status(
    process_count: usize,
    source_probe: std::io::Result<()>,
) -> ProcessDataStatus {
    if process_count > 0 {
        return ProcessDataStatus::Ready;
    }
    match source_probe {
        Ok(()) => ProcessDataStatus::Empty,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            ProcessDataStatus::PermissionDenied
        }
        Err(_) => ProcessDataStatus::Unavailable,
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MonitorModel;

    /// The sampler reports empty, permission and failure states distinctly, and signal
    /// effects on an unknown or stale PID dispatch nothing.
    #[test]
    fn process_source_status_and_signals_refuse_to_pretend() {
        // Phase `process_source_status_distinguishes_empty_permission_and_failure`.
        {
            assert_eq!(process_snapshot_status(1, Ok(())), ProcessDataStatus::Ready);
            assert_eq!(process_snapshot_status(0, Ok(())), ProcessDataStatus::Empty);
            assert_eq!(
                process_snapshot_status(
                    0,
                    Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
                ),
                ProcessDataStatus::PermissionDenied
            );
            assert_eq!(
                process_snapshot_status(0, Err(std::io::Error::other("source offline"))),
                ProcessDataStatus::Unavailable
            );
        }

        // Phase `signal_and_kill_reject_unknown_or_stale_pid`.
        // Signal effects route through a generation-checked real process handle:
        // an unknown or stale PID dispatches nothing rather than pretending to
        // succeed.
        {
            let mut s = Sampler::new();
            assert!(
                !s.signal_if_current(u32::MAX, 0, StopKind::Term),
                "no such process to SIGTERM"
            );
            assert!(
                !s.signal_if_current(u32::MAX, 0, StopKind::Kill),
                "no such process to SIGKILL"
            );
        }
    }

    /// A live refresh folds real metrics into the model and ties the
    /// network/disk throughput calculations to their rolling histories — the
    /// delta path the live app draws from.
    #[test]
    fn refresh_populates_metrics_and_throughput_histories() {
        let mut s = Sampler::new();
        let mut model = MonitorModel::empty();
        s.refresh(&mut model);
        let m = &model.metrics;
        assert!(!m.cores.is_empty(), "real per-core readings sampled");
        assert!(m.mem_total > 0, "real memory total sampled");
        assert!(!model.processes.is_empty(), "real process list sampled");
        // Throughput deltas are non-negative, finite, and pushed to histories.
        assert!(
            m.net_rx >= 0.0 && m.net_rx.is_finite(),
            "net rx is a real rate"
        );
        assert!(
            m.disk_read >= 0.0 && m.disk_read.is_finite(),
            "disk read is a real rate"
        );
        assert_eq!(
            m.net_rx_hist.last(),
            m.net_rx,
            "net rx history tracks the throughput"
        );
        assert_eq!(
            m.disk_read_hist.last(),
            m.disk_read,
            "disk read history tracks the throughput"
        );
        // A second refresh recomputes deltas over real elapsed wall time.
        s.refresh(&mut model);
        assert!(model.metrics.net_tx >= 0.0 && model.metrics.net_tx.is_finite());
        assert_eq!(
            model.metrics.disk_write_hist.last(),
            model.metrics.disk_write
        );
    }
}
