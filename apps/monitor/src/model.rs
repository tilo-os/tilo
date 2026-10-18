//! Pure data model for the activity monitor.
//!
//! Nothing in here touches `sysinfo`, the CRDT, or the renderer — it is
//! the shared vocabulary the live sampler ([`crate::sampler`]) writes into
//! and the widgets ([`crate::app`]) read out of. Keeping it free of IO is
//! what lets `--shots` seed a [`MonitorModel::sample`] snapshot offline that
//! renders identically to a live frame.

pub use ui::charts::RollingSeries;

/// One process row in the table.
#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    /// Kernel start-time tick for this particular PID lifetime. A PID alone is
    /// reusable; signal operations carry this value and re-check it immediately
    /// before dispatch so a stale action can never affect a replacement process.
    pub started_at: u64,
    /// Parent process id, when known. `None` for the synthetic forest roots
    /// (pid 0/1 or a process whose parent isn't in the sampled set). Drives
    /// the parent→child tree in [`crate::tree`].
    pub ppid: Option<u32>,
    pub name: String,
    pub user: String,
    /// CPU usage as a share of the whole machine, in percent: a process
    /// keeping one of eight cores busy reads 12.5. The processes' shares add
    /// up to the machine's total load, so a summed subtree never reads more
    /// than the whole machine.
    pub cpu: f32,
    /// Resident memory (RSS) in bytes — physical pages the process holds.
    pub mem_bytes: u64,
    /// Virtual memory size in bytes — the process's whole address space,
    /// typically several times RSS. Surfaced in the detail pane.
    pub virt_bytes: u64,
    pub threads: usize,
    pub status: String,
    /// Full command line (argv joined with spaces), or the bare name when the
    /// kernel exposes no argv (kernel threads, zombies).
    pub cmd: String,
    /// Wall-clock seconds the process has been running (from its start time).
    pub run_secs: u64,
    /// Open file-descriptor count, when the platform exposes it (`/proc/<pid>/fd`
    /// on Linux). `None` when unavailable rather than a fabricated zero.
    pub open_files: Option<usize>,
    /// Linux kernel worker thread (pid-2 descendant). Hidden unless the
    /// "show kernel threads" preference is on.
    pub kernel: bool,
    /// The app the process belongs to (`so.tilo.notes`), when it belongs to
    /// one: what "End app" ends.
    pub app: Option<String>,
}

/// Whole-system gauges + rolling histories. One instance per app.
#[derive(Clone, Debug)]
pub struct Metrics {
    pub cpu_brand: String,
    pub host: String,
    pub kernel: String,
    /// Per-core instantaneous usage (0..=100).
    pub cores: Vec<f32>,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// Current throughput, bytes/sec.
    pub net_rx: f32,
    pub net_tx: f32,
    pub disk_read: f32,
    pub disk_write: f32,
    pub uptime_secs: u64,
    pub load_avg: [f32; 3],

    // Rolling histories (newest at the back).
    pub cpu_hist: RollingSeries,
    pub mem_hist: RollingSeries,
    pub net_rx_hist: RollingSeries,
    pub net_tx_hist: RollingSeries,
    pub disk_read_hist: RollingSeries,
    pub disk_write_hist: RollingSeries,
}

impl Metrics {
    /// Mean of every core's current load (0..=100).
    pub fn cpu_overall(&self) -> f32 {
        if self.cores.is_empty() {
            0.0
        } else {
            self.cores.iter().sum::<f32>() / self.cores.len() as f32
        }
    }

    pub fn mem_frac(&self) -> f32 {
        if self.mem_total == 0 {
            0.0
        } else {
            (self.mem_used as f64 / self.mem_total as f64) as f32
        }
    }

    pub fn swap_frac(&self) -> f32 {
        if self.swap_total == 0 {
            0.0
        } else {
            (self.swap_used as f64 / self.swap_total as f64) as f32
        }
    }
}

/// Availability of the operating-system process source. This deliberately
/// distinguishes an empty but valid snapshot from a source that has not yet
/// loaded or cannot be read, so the UI never mistakes a permission problem for
/// an idle machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessDataStatus {
    /// The first live snapshot is still being collected.
    Loading,
    /// The operating system returned one or more processes.
    Ready,
    /// The operating system successfully returned a genuinely empty list.
    Empty,
    /// The process source exists but this user is not allowed to inspect it.
    PermissionDenied,
    /// The platform could not provide a process snapshot for another reason.
    Unavailable,
}

/// Top-level model: system metrics + process list + source availability.
#[derive(Clone, Debug)]
pub struct MonitorModel {
    pub metrics: Metrics,
    pub processes: Vec<ProcessInfo>,
    pub process_status: ProcessDataStatus,
}

/// History depth for every time-series. ~2 minutes at a 1s cadence.
pub const HISTORY: usize = 120;

impl MonitorModel {
    /// Build a representative, fully-populated snapshot with no IO. This
    /// is what `--shots` renders, and what the live app starts from before
    /// the first real `sysinfo` sample lands a second later.
    #[allow(
        clippy::similar_names,
        reason = "net_rx_hist/net_tx_hist are the standard receive/transmit pair; renaming either would read worse"
    )]
    pub fn sample() -> Self {
        // ---- per-core history: a few cores hot, most idle-ish -------
        let cores: Vec<f32> = SAMPLE_CORE_LOAD.to_vec();

        // Synthesise wavy histories so the charts read as "alive".
        let cpu_hist = RollingSeries::from_samples(
            HISTORY,
            (0..HISTORY).map(|i| {
                let t = i as f32;
                let v = 41.0
                    + 22.0 * (t * 0.13).sin()
                    + 9.0 * (t * 0.37).sin()
                    + 6.0 * (t * 0.91).cos();
                v.clamp(2.0, 99.0)
            }),
        );
        let mem_total: u64 = 16 * 1024 * 1024 * 1024;
        let mem_hist = RollingSeries::from_samples(
            HISTORY,
            (0..HISTORY).map(|i| {
                let t = i as f32;
                (0.54 + 0.06 * (t * 0.05).sin() + 0.02 * (t * 0.21).cos()).clamp(0.30, 0.95)
            }),
        );
        let mem_used = (mem_total as f32 * mem_hist.last()) as u64;

        let net_rx_hist = RollingSeries::from_samples(
            HISTORY,
            (0..HISTORY).map(|i| {
                let t = i as f32;
                let mb = 2.4 + 2.0 * (t * 0.17).sin().max(0.0) + 0.8 * (t * 0.6).sin();
                mb.max(0.05) * 1_000_000.0
            }),
        );
        let net_tx_hist = RollingSeries::from_samples(
            HISTORY,
            (0..HISTORY).map(|i| {
                let t = i as f32;
                let mb = 0.8 + 0.9 * (t * 0.11 + 1.0).sin().max(0.0);
                mb.max(0.02) * 1_000_000.0
            }),
        );
        let disk_read_hist = RollingSeries::from_samples(
            HISTORY,
            (0..HISTORY).map(|i| {
                let t = i as f32;
                let mb = (3.0 * (t * 0.27).sin()).max(0.0) + 0.4 * (t * 0.9).cos().abs();
                mb.max(0.0) * 1_000_000.0
            }),
        );
        let disk_write_hist = RollingSeries::from_samples(
            HISTORY,
            (0..HISTORY).map(|i| {
                let t = i as f32;
                let mb = (2.0 * (t * 0.19 + 2.0).sin()).max(0.0);
                mb.max(0.0) * 1_000_000.0
            }),
        );

        let metrics = Metrics {
            cpu_brand: "AMD Ryzen 7 7840U w/ Radeon 780M".into(),
            host: "tilo-workstation".into(),
            kernel: "Linux 7.0.9".into(),
            cores,
            mem_total,
            mem_used,
            swap_total: 4 * 1024 * 1024 * 1024,
            swap_used: 612 * 1024 * 1024,
            net_rx: net_rx_hist.last(),
            net_tx: net_tx_hist.last(),
            disk_read: disk_read_hist.last(),
            disk_write: disk_write_hist.last(),
            uptime_secs: 5 * 3600 + 42 * 60 + 17,
            load_avg: [1.84, 1.42, 1.09],
            cpu_hist,
            mem_hist,
            net_rx_hist,
            net_tx_hist,
            disk_read_hist,
            disk_write_hist,
        };

        Self {
            metrics,
            processes: sample_processes(),
            process_status: ProcessDataStatus::Ready,
        }
    }

    /// The sample machine a screenshot `scenario` shows: the sample, with no
    /// processes for the empty, loading and refused process lists, and with
    /// many cores for `resources-many`.
    pub fn scenario(scenario: Option<&str>) -> Self {
        let mut model = Self::sample();
        let status = match scenario {
            Some("empty") => ProcessDataStatus::Empty,
            Some("process-loading") => ProcessDataStatus::Loading,
            Some("process-permission-error") => ProcessDataStatus::PermissionDenied,
            Some("resources-many") => {
                // 24 cores so the grid wraps to several rows — the card must
                // grow to hold them and the page must scroll on a short shot.
                model.metrics.cores = (0..24)
                    .map(|i| {
                        let t = i as f32;
                        (30.0 + 55.0 * (t * 0.7).sin() + 20.0 * (t * 0.3).cos()).clamp(2.0, 99.0)
                    })
                    .collect();
                return model;
            }
            _ => return model,
        };
        model.processes.clear();
        model.process_status = status;
        model
    }

    /// Empty model used as the initial state for the live sampler before
    /// the first refresh. Histories are pre-filled with zeros so the
    /// charts don't visibly "grow in" from the left edge.
    pub fn empty() -> Self {
        let z = || RollingSeries::from_samples(HISTORY, std::iter::repeat_n(0.0, HISTORY));
        let metrics = Metrics {
            cpu_brand: "CPU".into(),
            host: "localhost".into(),
            kernel: "Linux".into(),
            cores: Vec::new(),
            mem_total: 0,
            mem_used: 0,
            swap_total: 0,
            swap_used: 0,
            net_rx: 0.0,
            net_tx: 0.0,
            disk_read: 0.0,
            disk_write: 0.0,
            uptime_secs: 0,
            load_avg: [0.0; 3],
            cpu_hist: z(),
            mem_hist: z(),
            net_rx_hist: z(),
            net_tx_hist: z(),
            disk_read_hist: z(),
            disk_write_hist: z(),
        };
        Self {
            metrics,
            processes: Vec::new(),
            process_status: ProcessDataStatus::Loading,
        }
    }
}

/// The sample machine's per-core load, in percent.
const SAMPLE_CORE_LOAD: [f32; 8] = [62.0, 18.0, 47.0, 9.0, 88.0, 33.0, 14.0, 71.0];

/// One sample-data process row, as `(name, parent, user, cpu, mem_MiB,
/// threads, status, kernel)`; `cpu` is per core, as `top` reads it. `parent` is `""` for a forest root.
type SampleRow = (
    &'static str,
    &'static str,
    &'static str,
    f32,
    u64,
    usize,
    &'static str,
    bool,
);

/// Hand-tuned sample process table. Values are picked so the sort columns
/// and the CPU/MEM ranking read realistically in the screenshot. The
/// `parent` column names each row's parent process (by name) so the tree
/// view ([`crate::tree`]) reads as a real session hierarchy: `systemd`
/// roots the system services, `shell` roots the user session, and the heavy
/// apps (`browser`, `code`) fan out into the children that account for most
/// of "what they're doing" — exactly the subtree-sum story. Multi-instance
/// names (e.g. two "Web Content") attach to the *first* matching parent,
/// which is what a real renderer fan-out looks like.
const SAMPLE_ROWS: &[SampleRow] = &[
    // ---- the user session, rooted at the compositor ----------------
    ("shell", "", "ada", 14.6, 412, 24, "Running", false),
    ("browser", "shell", "ada", 12.3, 643, 14, "Running", false),
    (
        "Web Content",
        "browser",
        "ada",
        31.8,
        988,
        22,
        "Running",
        false,
    ),
    (
        "Web Content",
        "browser",
        "ada",
        48.1,
        1210,
        19,
        "Running",
        false,
    ),
    (
        "GPU Process",
        "browser",
        "ada",
        9.4,
        512,
        8,
        "Running",
        false,
    ),
    ("code", "shell", "ada", 6.4, 420, 18, "Sleeping", false),
    (
        "rust-analyzer",
        "code",
        "ada",
        63.9,
        2680,
        12,
        "Running",
        false,
    ),
    ("cargo", "code", "ada", 11.2, 140, 4, "Running", false),
    ("rustc", "cargo", "ada", 88.7, 760, 1, "Running", false),
    ("blender", "shell", "ada", 47.1, 2210, 51, "Running", false),
    ("terminal", "shell", "ada", 3.2, 142, 6, "Sleeping", false),
    ("fish", "terminal", "ada", 0.4, 28, 1, "Sleeping", false),
    ("monitor", "shell", "ada", 5.7, 96, 5, "Running", false),
    (
        "file-explorer",
        "shell",
        "ada",
        1.1,
        188,
        8,
        "Sleeping",
        false,
    ),
    ("Xwayland", "shell", "ada", 4.3, 176, 5, "Running", false),
    // ---- the system service tree, rooted at systemd ----------------
    ("systemd", "", "root", 0.2, 18, 1, "Sleeping", false),
    ("tilo-fs", "systemd", "root", 8.9, 224, 14, "Running", false),
    (
        "iroh-node",
        "tilo-fs",
        "ada",
        6.1,
        310,
        16,
        "Running",
        false,
    ),
    ("gossip", "iroh-node", "ada", 2.2, 88, 6, "Running", false),
    (
        "systemd-journald",
        "systemd",
        "root",
        1.7,
        96,
        1,
        "Sleeping",
        false,
    ),
    (
        "systemd-logind",
        "systemd",
        "root",
        0.1,
        12,
        1,
        "Sleeping",
        false,
    ),
    (
        "rtkit-daemon",
        "systemd",
        "rtkit",
        0.1,
        4,
        3,
        "Sleeping",
        false,
    ),
    (
        "NetworkManager",
        "systemd",
        "root",
        0.6,
        38,
        3,
        "Sleeping",
        false,
    ),
    (
        "dbus-broker",
        "systemd",
        "dbus",
        0.8,
        16,
        1,
        "Running",
        false,
    ),
    ("greetd", "systemd", "root", 0.0, 8, 1, "Sleeping", false),
    ("chronyd", "systemd", "chrony", 0.1, 6, 1, "Sleeping", false),
    ("sshd", "systemd", "root", 0.0, 11, 1, "Sleeping", false),
    (
        "avahi-daemon",
        "systemd",
        "avahi",
        0.2,
        9,
        2,
        "Sleeping",
        false,
    ),
    ("pipewire", "systemd", "ada", 2.4, 64, 4, "Sleeping", false),
    (
        "wireplumber",
        "pipewire",
        "ada",
        1.3,
        52,
        4,
        "Sleeping",
        false,
    ),
    (
        "pulseaudio",
        "systemd",
        "ada",
        0.9,
        41,
        3,
        "Sleeping",
        false,
    ),
    ("podman", "systemd", "ada", 0.5, 64, 7, "Sleeping", false),
    ("conmon", "podman", "ada", 0.1, 14, 1, "Sleeping", false),
    ("swaybg", "shell", "ada", 0.2, 44, 2, "Sleeping", false),
    (
        "gnome-keyring-d",
        "systemd",
        "ada",
        0.1,
        19,
        3,
        "Sleeping",
        false,
    ),
    ("upowerd", "systemd", "root", 0.1, 10, 1, "Sleeping", false),
    (
        "bluetoothd",
        "systemd",
        "root",
        0.2,
        13,
        1,
        "Sleeping",
        false,
    ),
    ("cupsd", "systemd", "root", 0.0, 15, 1, "Sleeping", false),
    // ---- kernel worker threads (own forest; hidden by default) -----
    ("kworker/0:1", "", "root", 1.4, 0, 1, "Idle", true),
    ("kworker/u32:3", "", "root", 2.1, 0, 1, "Idle", true),
    ("ksoftirqd/4", "", "root", 0.6, 0, 1, "Idle", true),
    ("kcompactd0", "", "root", 0.0, 0, 1, "Idle", true),
    ("kswapd0", "", "root", 0.3, 0, 1, "Idle", true),
    ("migration/2", "", "root", 0.0, 0, 1, "Idle", true),
    ("jbd2/nvme0n1p3", "", "root", 0.5, 0, 1, "Idle", true),
    ("irq/142-nvme0", "", "root", 0.1, 0, 1, "Idle", true),
];

/// Spread synthetic but stable PIDs across `rows`, one per row in order.
fn sample_pids(rows: &[SampleRow]) -> Vec<u32> {
    let mut pid: u32 = 1;
    let mut pids = Vec::with_capacity(rows.len());
    for (name, ..) in rows {
        pid += 37 + (name.len() as u32 * 11) % 113;
        pids.push(pid);
    }
    pids
}

/// Turn sample row `i` into a full [`ProcessInfo`]: resolve its parent's pid
/// by name (the first matching row above it), and synthesise a believable
/// command line, virtual size, run time and fd count from the row's stable
/// inputs so the detail pane reads as "alive" in the hermetic `--shots`
/// frame (which never samples).
#[allow(
    clippy::similar_names,
    reason = "pid/ppid are the standard process-table terms; renaming either would read worse"
)]
fn build_sample_process(rows: &[SampleRow], pids: &[u32], i: usize, row: SampleRow) -> ProcessInfo {
    let (name, parent, user, cpu, mem_mib, threads, status, kernel) = row;
    let ppid = if parent.is_empty() {
        None
    } else {
        rows.iter()
            .position(|r| r.0 == parent)
            .and_then(|j| pids.get(j).copied())
    };
    let mem_bytes = mem_mib * 1024 * 1024;
    let cmd = if kernel {
        format!("[{name}]")
    } else {
        sample_cmdline(name, user)
    };
    // Virtual size runs a few× RSS (shared libs + reserved heap/stack).
    let virt_bytes = if mem_bytes == 0 {
        0
    } else {
        mem_bytes.saturating_mul(3) + 220 * 1024 * 1024
    };
    let pid = pids.get(i).copied().unwrap_or(0);
    // Deterministic run time from the pid so rows differ but are stable.
    let run_secs = 37 + (u64::from(pid) * 7) % 90_000;
    let open_files = if kernel {
        None
    } else {
        Some(threads * 2 + (pid as usize % 17) + 4)
    };
    ProcessInfo {
        pid,
        // Synthetic snapshots never reach the operating system, but retaining
        // a deterministic generation lets the same action wire shape be used
        // by screenshots, tests, and the live app.
        started_at: 10_000 + u64::from(pid) * 13,
        ppid,
        name: name.to_string(),
        user: user.to_string(),
        cpu: cpu / SAMPLE_CORE_LOAD.len() as f32,
        mem_bytes,
        virt_bytes,
        threads,
        status: status.to_string(),
        cmd,
        run_secs,
        open_files,
        kernel,
        app: sample_app(name).map(str::to_owned),
    }
}

/// The app a sample process belongs to: the person's apps and the programs
/// they started, never the system's services.
fn sample_app(name: &str) -> Option<&'static str> {
    Some(match name {
        "browser" | "Web Content" | "GPU Process" => "so.tilo.browser",
        "code" | "rust-analyzer" | "cargo" | "rustc" => "so.tilo.code-editor",
        "terminal" | "fish" => "so.tilo.terminal",
        "monitor" => "so.tilo.monitor",
        "file-explorer" => "so.tilo.file-explorer",
        "blender" => "org.blender.Blender",
        _ => return None,
    })
}

fn sample_processes() -> Vec<ProcessInfo> {
    let pids = sample_pids(SAMPLE_ROWS);
    SAMPLE_ROWS
        .iter()
        .enumerate()
        .map(|(i, row)| build_sample_process(SAMPLE_ROWS, &pids, i, *row))
        .collect()
}

/// A believable argv for a sample process — a real-looking binary path plus a
/// couple of flags, so the detail pane's command-line row isn't just the name.
fn sample_cmdline(name: &str, user: &str) -> String {
    match name {
        "shell" => "/usr/bin/shell --session wayland --workspace 0".into(),
        "browser" => "/usr/bin/browser --profile default --enable-gpu".into(),
        "Web Content" => "/usr/lib/browser/content --type=renderer --field-trial-handle=3".into(),
        "GPU Process" => "/usr/lib/browser/content --type=gpu-process".into(),
        "code" => format!("/usr/bin/code --unity-launch /home/{user}/Development"),
        "rust-analyzer" => "/usr/bin/rust-analyzer --log-file /tmp/ra.log".into(),
        "cargo" => "/usr/bin/cargo build --release -p ui".into(),
        "rustc" => "/usr/bin/rustc --edition 2024 --crate-type lib src/lib.rs".into(),
        "blender" => format!("/usr/bin/blender /home/{user}/Projects/studio.blend"),
        "terminal" => "/usr/bin/terminal".into(),
        "monitor" => "/usr/bin/monitor".into(),
        "systemd" => "/usr/lib/systemd/systemd --system --deserialize 34".into(),
        "tilo-fs" => "/usr/bin/tilo-fs --mount /var/workspace --cache /var/cache/tilo-fs".into(),
        "iroh-node" => "/usr/bin/iroh-node --relay code.tilo.so".into(),
        "pipewire" => "/usr/bin/pipewire".into(),
        _ => format!("/usr/bin/{name}"),
    }
}

/// Human-readable throughput: 0 / 84 KB/s / 2.4 MB/s (decimal, like most
/// network tools).
pub fn fmt_rate(bytes_per_sec: f32) -> String {
    let bps = f64::from(bytes_per_sec.max(0.0));
    if bps < 1000.0 {
        format!("{bps:.0} B/s")
    } else if bps < 1_000_000.0 {
        format!("{:.0} KB/s", bps / 1000.0)
    } else if bps < 1_000_000_000.0 {
        format!("{:.1} MB/s", bps / 1_000_000.0)
    } else {
        format!("{:.2} GB/s", bps / 1_000_000_000.0)
    }
}

/// `5h 42m 17s` style duration.
pub fn fmt_uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m {s}s")
    } else {
        format!("{m}m {s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Model primitives: the sample model is fully populated, and the rate/uptime
    /// formatters.
    #[test]
    fn model_sample_and_formatters() {
        // Phase `sample_is_fully_populated`.
        {
            let m = MonitorModel::sample();
            assert!(!m.processes.is_empty());
            assert_eq!(m.metrics.cores.len(), 8);
            assert_eq!(m.metrics.cpu_hist.len(), HISTORY);
            assert!(m.metrics.mem_used > 0 && m.metrics.mem_used < m.metrics.mem_total);
        }

        // Phase `fmt_helpers`.
        {
            assert_eq!(fmt_rate(0.0), "0 B/s");
            assert_eq!(fmt_rate(2_400_000.0), "2.4 MB/s");
            assert_eq!(fmt_uptime(3 * 3600 + 5 * 60 + 9), "3h 5m 9s");
        }
    }
}
