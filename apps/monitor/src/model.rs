//! The monitor's data model: metrics with their rolling histories, the process
//! rows, and the sample machine captures render. No IO.

pub use ui::charts::RollingSeries;

#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    /// The kernel start tick of this PID lifetime: a delayed signal re-checks it,
    /// so it never reaches a process that took the PID.
    pub started_at: u64,
    /// The parent's id, when known; `None` for a forest root.
    pub ppid: Option<u32>,
    pub name: String,
    pub user: String,
    /// CPU use as a share of the whole machine, in percent: one of eight cores busy
    /// reads 12.5.
    pub cpu: f32,
    /// Resident memory in bytes.
    pub mem_bytes: u64,
    /// Virtual memory in bytes, typically several times RSS.
    pub virt_bytes: u64,
    pub threads: usize,
    pub status: String,
    /// The command line, or the bare name when the kernel exposes no argv.
    pub cmd: String,
    pub run_secs: u64,
    /// Open files, when the platform exposes them; `None` rather than a fabricated
    /// zero.
    pub open_files: Option<usize>,
    /// A kernel worker thread, hidden unless the preference is on.
    pub kernel: bool,
    /// The app the process belongs to; what "End app" ends.
    pub app: Option<String>,
}

/// Whole-system gauges with their rolling histories.
#[derive(Clone, Debug)]
pub struct Metrics {
    pub cpu_brand: String,
    pub host: String,
    pub kernel: String,
    /// Per-core use in percent.
    pub cores: Vec<f32>,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// Throughput in bytes per second.
    pub net_rx: f32,
    pub net_tx: f32,
    pub disk_read: f32,
    pub disk_write: f32,
    pub uptime_secs: u64,
    pub load_avg: [f32; 3],
    pub sampled_at: Option<ui::time::SystemTime>,
    /// The cadence the histories were collected at, in milliseconds; `0` when it is
    /// not known.
    pub sample_interval_ms: u32,

    pub cpu_hist: RollingSeries,
    pub mem_hist: RollingSeries,
    pub net_rx_hist: RollingSeries,
    pub net_tx_hist: RollingSeries,
    pub disk_read_hist: RollingSeries,
    pub disk_write_hist: RollingSeries,
}

impl Metrics {
    /// The span the histories cover, in the person's words: `last 2 min`.
    pub fn history_period(&self) -> String {
        let secs = (HISTORY as u64).saturating_mul(u64::from(self.sample_interval_ms)) / 1000;
        if secs < 60 {
            format!("last {secs} s")
        } else {
            format!("last {} min", secs / 60)
        }
    }

    pub fn measured(&self) -> bool {
        !self.cores.is_empty() || self.mem_total > 0
    }

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

/// Availability of the process source, which distinguishes an empty snapshot from a
/// source not yet loaded or not readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessDataStatus {
    Loading,
    Ready,
    Empty,
    PermissionDenied,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct MonitorModel {
    pub metrics: Metrics,
    pub processes: Vec<ProcessInfo>,
    pub process_status: ProcessDataStatus,
}

/// History depth for every time series: two minutes at a one-second cadence.
pub const HISTORY: usize = 120;

impl MonitorModel {
    /// A fully populated snapshot with no IO, which captures render and the live app
    /// starts from.
    #[allow(
        clippy::similar_names,
        reason = "net_rx_hist/net_tx_hist are the standard receive/transmit pair; renaming either would read worse"
    )]
    pub fn sample() -> Self {
        let cores: Vec<f32> = SAMPLE_CORE_LOAD.to_vec();

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
            sampled_at: Some(ui::time::wall()),
            sample_interval_ms: 1_000,
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

    pub fn scenario(scenario: Option<&str>) -> Self {
        let mut model = Self::sample();
        let status = match scenario {
            Some("empty") => ProcessDataStatus::Empty,
            Some("process-loading") => ProcessDataStatus::Loading,
            Some("process-permission-error") => ProcessDataStatus::PermissionDenied,
            Some("resources-many") => {
                // 24 cores so the grid wraps and the page must scroll on a short
                // shot.
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

    /// The live sampler's state before its first reading, histories pre-filled with
    /// zeros so the charts do not grow in from the left.
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
            sampled_at: None,
            sample_interval_ms: 1_500,
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

const SAMPLE_CORE_LOAD: [f32; 8] = [62.0, 18.0, 47.0, 9.0, 88.0, 33.0, 14.0, 71.0];

/// A sample row as `(name, parent, user, cpu, mem_MiB, threads, status, kernel)`;
/// `cpu` is per core, `parent` is `""` for a forest root.
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

/// The sample process table: each row names its parent so the tree reads as a real
/// session, and a name with several instances attaches to the first match.
const SAMPLE_ROWS: &[SampleRow] = &[
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

fn sample_pids(rows: &[SampleRow]) -> Vec<u32> {
    let mut pid: u32 = 1;
    let mut pids = Vec::with_capacity(rows.len());
    for (name, ..) in rows {
        pid += 37 + (name.len() as u32 * 11) % 113;
        pids.push(pid);
    }
    pids
}

/// Turn sample row `i` into a [`ProcessInfo`] with believable details.
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
    // Virtual size runs a few times RSS.
    let virt_bytes = if mem_bytes == 0 {
        0
    } else {
        mem_bytes.saturating_mul(3) + 220 * 1024 * 1024
    };
    let pid = pids.get(i).copied().unwrap_or(0);
    let run_secs = 37 + (u64::from(pid) * 7) % 90_000;
    let open_files = if kernel {
        None
    } else {
        Some(threads * 2 + (pid as usize % 17) + 4)
    };
    ProcessInfo {
        pid,
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

/// The app a sample process belongs to: the person's apps, never the system's
/// services.
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

/// Throughput in the toolkit's rate style: `0 B/s`, `84 kB/s`, `2.4 MB/s`.
pub fn fmt_rate(bytes_per_sec: f32) -> String {
    ui::format::rate(f64::from(bytes_per_sec))
}

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
    #[test]
    fn model_sample_and_formatters() {
        {
            let m = MonitorModel::sample();
            assert!(!m.processes.is_empty());
            assert_eq!(m.metrics.cores.len(), 8);
            assert_eq!(m.metrics.cpu_hist.len(), HISTORY);
            assert!(m.metrics.mem_used > 0 && m.metrics.mem_used < m.metrics.mem_total);
        }

        {
            assert_eq!(fmt_rate(0.0), "0 B/s");
            assert_eq!(fmt_rate(2_400_000.0), "2.4 MB/s");
            assert_eq!(fmt_uptime(3 * 3600 + 5 * 60 + 9), "3h 5m 9s");
        }
    }
}
