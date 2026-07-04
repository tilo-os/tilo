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
        let dt = self
            .last
            .map(|l| (now - l).as_secs_f32())
            .unwrap_or(1.0)
            .max(0.05);

        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        self.sys.refresh_processes(ProcessesToUpdate::All, true);
        self.networks.refresh();

        let m = &mut model.metrics;
        m.cores = self.sys.cpus().iter().map(|c| c.cpu_usage()).collect();
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
                .map(|u| u.name().to_string())
                .unwrap_or_else(|| "—".to_string());
            let threads = p.tasks().map(|t| t.len()).unwrap_or(1).max(1);
            let mem = p.memory();
            procs.push(ProcessInfo {
                pid: pid.as_u32(),
                ppid: p.parent().map(|pp| pp.as_u32()),
                name,
                user,
                cpu: p.cpu_usage(),
                mem_bytes: mem,
                threads,
                status: p.status().to_string(),
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
