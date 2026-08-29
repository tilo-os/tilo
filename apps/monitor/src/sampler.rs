//! Live system sampler. Wraps `sysinfo` and folds a fresh reading into a
//! [`MonitorModel`]. Constructed eagerly (cheap), but only *refreshed* on
//! the app's throttle — so the very first `--shots` frame, which never
//! ticks the throttle, renders the seeded sample data untouched.

use std::time::Instant;

use sysinfo::{Networks, Pid, ProcessesToUpdate, Signal, System, Users};

use crate::model::{MonitorModel, ProcessInfo};

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
        sys.refresh_cpu_all();
        sys.refresh_memory();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        Self {
            sys,
            networks: Networks::new_with_refreshed_list(),
            users: Users::new_with_refreshed_list(),
            last: None,
        }
    }

    /// Fold a fresh reading into `model`. Network and disk throughput are
    /// computed as byte deltas since the previous call divided by the
    /// elapsed wall time.
    pub fn refresh(&mut self, model: &mut MonitorModel) {
        let now = Instant::now();
        let dt = self.last.map_or(1.0, |l| (now - l).as_secs_f32()).max(0.05);

        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        self.sys.refresh_processes(ProcessesToUpdate::All, true);
        self.networks.refresh();

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

        // Rebuild the process table.
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
                ppid: p.parent().map(sysinfo::Pid::as_u32),
                name,
                user,
                cpu: p.cpu_usage(),
                mem_bytes: mem,
                virt_bytes: p.virtual_memory(),
                threads,
                status: p.status().to_string(),
                cmd,
                run_secs: p.run_time(),
                open_files: count_open_files(pid_u32),
                // Kernel worker threads have no resident memory.
                kernel: mem == 0,
            });
        }
        model.processes = procs;

        self.last = Some(now);
    }

    /// Send `kind`'s signal (SIGTERM or SIGKILL) to `pid`. Returns whether
    /// the signal was dispatched. `SIGTERM` is the polite "Stop"; `SIGKILL`
    /// is the hard "Kill". Both route through the real process handle.
    pub fn signal(&self, pid: u32, kind: StopKind) -> bool {
        self.sys
            .process(Pid::from_u32(pid))
            .and_then(|p| p.kill_with(kind.signal()))
            .unwrap_or(false)
    }

    /// Send SIGKILL to `pid`. Returns whether the signal was dispatched.
    /// Thin alias over [`Self::signal`] for the hard-stop path.
    pub fn kill(&self, pid: u32) -> bool {
        self.signal(pid, StopKind::Kill)
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

/// Count a process's open file descriptors by listing `/proc/<pid>/fd`.
/// `None` when the platform doesn't expose it or the directory can't be read
/// (permission denied, the process just exited) — never a fabricated zero.
#[cfg(target_os = "linux")]
fn count_open_files(pid: u32) -> Option<usize> {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .ok()
        .map(|it| it.filter(std::result::Result::is_ok).count())
}

#[cfg(not(target_os = "linux"))]
fn count_open_files(_pid: u32) -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MonitorModel;

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

    /// `signal`/`kill` route through the real process handle: an unknown pid
    /// dispatches nothing (returns false) rather than pretending to succeed.
    #[test]
    fn signal_and_kill_reject_unknown_pid() {
        let s = Sampler::new();
        assert!(
            !s.signal(u32::MAX, StopKind::Term),
            "no such process to SIGTERM"
        );
        assert!(
            !s.signal(u32::MAX, StopKind::Kill),
            "no such process to SIGKILL"
        );
        assert!(!s.kill(u32::MAX), "no such process to hard-kill");
    }
}
