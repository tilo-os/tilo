//! The platform's own sampler, where Tilo runs on another operating system
//! (macOS): an app there is confined by the platform, which lets it read the
//! machine's process table itself. Wraps `sysinfo` and folds a fresh reading
//! into a [`MonitorModel`] on the app's cadence.

use ui::time::Instant;

use sysinfo::{Networks, Pid, ProcessesToUpdate, Signal, System, Users};

use super::{StopKind, note_provider_read};
use crate::model::{MonitorModel, ProcessDataStatus, ProcessInfo};

fn signal(kind: StopKind) -> Signal {
    match kind {
        StopKind::Term => Signal::Term,
        StopKind::Kill => Signal::Kill,
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
        note_provider_read();
        sys.refresh_cpu_all();
        sys.refresh_memory();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let networks = Networks::new_with_refreshed_list();
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
    pub fn refresh(&mut self, model: &mut MonitorModel) -> bool {
        note_provider_read();
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
                app: None,
            });
        }
        model.processes = procs;
        model.process_status =
            // `sysinfo`'s refresh is infallible, so an empty listing is paired
            // with a probe of the process table: readable plus no rows is a
            // genuine empty snapshot, an access failure is surfaced.
            process_snapshot_status(model.processes.len(), host_info::process::probe_process_table());

        self.last = Some(now);
        true
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
            .and_then(|p| p.kill_with(signal(kind)))
            .unwrap_or(false)
    }
}

impl Sampler {
    /// Read now, as the person asked: here every reading is taken on demand.
    pub fn refresh_now(&mut self, model: &mut MonitorModel) -> bool {
        self.refresh(model)
    }

    /// Readings are taken on the app's cadence, not pushed.
    pub fn is_pushed(&self) -> bool {
        false
    }

    /// The cadence is the app's own here.
    pub fn set_interval(&mut self, _interval_ms: Option<u32>) {}

    /// Nothing arrives from another thread here.
    pub fn set_waker(&mut self, _waker: ui::WakeHandle) {}

    /// Apps are not told apart on this platform.
    ///
    /// # Errors
    /// Always: there is no app to end.
    pub fn end_app(&mut self, _app: &str) -> Result<u32, String> {
        Err("apps are ended from the system's own task manager here".into())
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
