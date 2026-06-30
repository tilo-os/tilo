//! Pure data model for the activity monitor.
//!
//! Nothing in here touches `sysinfo`, the CRDT, or the renderer — it is
//! the shared vocabulary the live sampler ([`crate::sampler`]) writes into
//! and the widgets ([`crate::app`]) read out of. Keeping it free of IO is
//! what lets `--shots` seed a fully-populated [`MonitorModel::sample`]
//! snapshot offline that renders identically to a live frame.

/// A fixed-capacity rolling history of `f32` samples — the backing store
/// for every time-series chart and sparkline. Oldest sample falls off the
/// front when the buffer is full. `cap` is small (≈120) so the O(n)
/// `remove(0)` is irrelevant.
#[derive(Clone, Debug)]
pub struct Ring {
    data: Vec<f32>,
    cap: usize,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self { data: Vec::with_capacity(cap), cap }
    }

    /// Seed a ring from an iterator, keeping only the last `cap` values.
    pub fn from_iter(cap: usize, it: impl IntoIterator<Item = f32>) -> Self {
        let mut r = Self::new(cap);
        for v in it {
            r.push(v);
        }
        r
    }

    pub fn push(&mut self, v: f32) {
        if self.data.len() >= self.cap && !self.data.is_empty() {
            self.data.remove(0);
        }
        self.data.push(v);
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    pub fn last(&self) -> f32 {
        self.data.last().copied().unwrap_or(0.0)
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Largest sample currently held, or `0.0` when empty. Used to pick a
    /// sensible y-axis ceiling for the network/disk charts whose units
    /// are unbounded.
    pub fn max(&self) -> f32 {
        self.data.iter().copied().fold(0.0_f32, f32::max)
    }

    /// Mean of the held samples (0.0 when empty).
    pub fn avg(&self) -> f32 {
        if self.data.is_empty() {
            0.0
        } else {
            self.data.iter().sum::<f32>() / self.data.len() as f32
        }
    }
}

/// One process row in the table.
#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub user: String,
    /// CPU usage as a percentage of a single core (so > 100 on a busy
    /// multi-threaded process — matches `top`/`htop` semantics).
    pub cpu: f32,
    pub mem_bytes: u64,
    pub threads: usize,
    pub status: String,
    /// Linux kernel worker thread (pid-2 descendant). Hidden unless the
    /// "show kernel threads" preference is on.
    pub kernel: bool,
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
    pub cpu_hist: Ring,
    pub mem_hist: Ring,
    pub net_rx_hist: Ring,
    pub net_tx_hist: Ring,
    pub disk_read_hist: Ring,
    pub disk_write_hist: Ring,
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

/// Top-level model: system metrics + process list.
#[derive(Clone, Debug)]
pub struct MonitorModel {
    pub metrics: Metrics,
    pub processes: Vec<ProcessInfo>,
}

/// History depth for every time-series. ~2 minutes at a 1s cadence.
pub const HISTORY: usize = 120;

impl MonitorModel {
    /// Build a representative, fully-populated snapshot with no IO. This
    /// is what `--shots` renders, and what the live app starts from before
    /// the first real `sysinfo` sample lands a second later.
    pub fn sample() -> Self {
        // ---- per-core history: a few cores hot, most idle-ish -------
        let core_base = [62.0, 18.0, 47.0, 9.0, 88.0, 33.0, 14.0, 71.0];
        let cores: Vec<f32> = core_base.to_vec();

        // Synthesise wavy histories so the charts read as "alive".
        let cpu_hist = Ring::from_iter(
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
        let mem_hist = Ring::from_iter(
            HISTORY,
            (0..HISTORY).map(|i| {
                let t = i as f32;
                (0.54 + 0.06 * (t * 0.05).sin() + 0.02 * (t * 0.21).cos())
                    .clamp(0.30, 0.95)
            }),
        );
        let mem_used = (mem_total as f32 * mem_hist.last()) as u64;

        let net_rx_hist = Ring::from_iter(
            HISTORY,
            (0..HISTORY).map(|i| {
                let t = i as f32;
                let mb = 2.4 + 2.0 * (t * 0.17).sin().max(0.0) + 0.8 * (t * 0.6).sin();
                (mb.max(0.05) * 1_000_000.0) as f32
            }),
        );
        let net_tx_hist = Ring::from_iter(
            HISTORY,
            (0..HISTORY).map(|i| {
                let t = i as f32;
                let mb = 0.8 + 0.9 * (t * 0.11 + 1.0).sin().max(0.0);
                (mb.max(0.02) * 1_000_000.0) as f32
            }),
        );
        let disk_read_hist = Ring::from_iter(
            HISTORY,
            (0..HISTORY).map(|i| {
                let t = i as f32;
                let mb = (3.0 * (t * 0.27).sin()).max(0.0) + 0.4 * (t * 0.9).cos().abs();
                (mb.max(0.0) * 1_000_000.0) as f32
            }),
        );
        let disk_write_hist = Ring::from_iter(
            HISTORY,
            (0..HISTORY).map(|i| {
                let t = i as f32;
                let mb = (2.0 * (t * 0.19 + 2.0).sin()).max(0.0);
                (mb.max(0.0) * 1_000_000.0) as f32
            }),
        );

        let metrics = Metrics {
            cpu_brand: "AMD Ryzen 7 7840U w/ Radeon 780M".into(),
            host: "quos-workstation".into(),
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

        Self { metrics, processes: sample_processes() }
    }

    /// Empty model used as the initial state for the live sampler before
    /// the first refresh. Histories are pre-filled with zeros so the
    /// charts don't visibly "grow in" from the left edge.
    pub fn empty() -> Self {
        let z = || Ring::from_iter(HISTORY, std::iter::repeat(0.0).take(HISTORY));
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
        Self { metrics, processes: Vec::new() }
    }
}

/// A believable process table. Values are hand-tuned so the sort columns
/// and the CPU/MEM ranking read realistically in the screenshot.
fn sample_processes() -> Vec<ProcessInfo> {
    // (name, user, cpu, mem_MiB, threads, status, kernel)
    let rows: &[(&str, &str, f32, u64, usize, &str, bool)] = &[
        ("shell", "quinten", 14.6, 412, 24, "Running", false),
        ("browser", "quinten", 92.3, 1843, 38, "Running", false),
        ("firefox", "quinten", 47.1, 2210, 51, "Running", false),
        ("Web Content", "quinten", 31.8, 988, 22, "Running", false),
        ("code", "quinten", 26.4, 1320, 33, "Sleeping", false),
        ("rust-analyzer", "quinten", 63.9, 2680, 12, "Running", false),
        ("cargo", "quinten", 71.2, 540, 9, "Running", false),
        ("rustc", "quinten", 88.7, 760, 1, "Running", false),
        ("terminal", "quinten", 3.2, 142, 6, "Sleeping", false),
        ("fish", "quinten", 0.4, 28, 1, "Sleeping", false),
        ("monitor", "quinten", 5.7, 96, 5, "Running", false),
        ("file-explorer", "quinten", 1.1, 188, 8, "Sleeping", false),
        ("qos-fs", "root", 8.9, 224, 14, "Running", false),
        ("pipewire", "quinten", 2.4, 64, 4, "Sleeping", false),
        ("wireplumber", "quinten", 1.3, 52, 4, "Sleeping", false),
        ("pulseaudio", "quinten", 0.9, 41, 3, "Sleeping", false),
        ("systemd", "root", 0.2, 18, 1, "Sleeping", false),
        ("systemd-journald", "root", 1.7, 96, 1, "Sleeping", false),
        ("systemd-logind", "root", 0.1, 12, 1, "Sleeping", false),
        ("systemd-resolved", "systemd+", 0.3, 22, 2, "Sleeping", false),
        ("NetworkManager", "root", 0.6, 38, 3, "Sleeping", false),
        ("dbus-broker", "dbus", 0.8, 16, 1, "Running", false),
        ("greetd", "root", 0.0, 8, 1, "Sleeping", false),
        ("chronyd", "chrony", 0.1, 6, 1, "Sleeping", false),
        ("sshd", "root", 0.0, 11, 1, "Sleeping", false),
        ("avahi-daemon", "avahi", 0.2, 9, 2, "Sleeping", false),
        ("iroh-node", "quinten", 6.1, 310, 16, "Running", false),
        ("gossipsub", "quinten", 2.2, 88, 6, "Running", false),
        ("podman", "quinten", 0.5, 64, 7, "Sleeping", false),
        ("conmon", "quinten", 0.1, 14, 1, "Sleeping", false),
        ("Xwayland", "quinten", 4.3, 176, 5, "Running", false),
        ("swaybg", "quinten", 0.2, 44, 2, "Sleeping", false),
        ("gnome-keyring-d", "quinten", 0.1, 19, 3, "Sleeping", false),
        ("upowerd", "root", 0.1, 10, 1, "Sleeping", false),
        ("bluetoothd", "root", 0.2, 13, 1, "Sleeping", false),
        ("cupsd", "root", 0.0, 15, 1, "Sleeping", false),
        ("kworker/0:1", "root", 1.4, 0, 1, "Idle", true),
        ("kworker/u32:3", "root", 2.1, 0, 1, "Idle", true),
        ("ksoftirqd/4", "root", 0.6, 0, 1, "Idle", true),
        ("kcompactd0", "root", 0.0, 0, 1, "Idle", true),
        ("kswapd0", "root", 0.3, 0, 1, "Idle", true),
        ("migration/2", "root", 0.0, 0, 1, "Idle", true),
        ("jbd2/nvme0n1p3", "root", 0.5, 0, 1, "Idle", true),
        ("irq/142-nvme0", "root", 0.1, 0, 1, "Idle", true),
    ];

    // Spread synthetic but stable PIDs.
    let mut pid: u32 = 1;
    rows.iter()
        .map(|(name, user, cpu, mem_mib, threads, status, kernel)| {
            pid += 37 + (name.len() as u32 * 11) % 113;
            ProcessInfo {
                pid,
                name: (*name).to_string(),
                user: (*user).to_string(),
                cpu: *cpu,
                mem_bytes: *mem_mib * 1024 * 1024,
                threads: *threads,
                status: (*status).to_string(),
                kernel: *kernel,
            }
        })
        .collect()
}

/// Human-readable byte size: 18 B / 4.2 KiB / 1.8 GiB.
pub fn fmt_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", b, U[0])
    } else {
        format!("{:.1} {}", v, U[i])
    }
}

/// Human-readable throughput: 0 / 84 KB/s / 2.4 MB/s (decimal, like most
/// network tools).
pub fn fmt_rate(bytes_per_sec: f32) -> String {
    let bps = bytes_per_sec.max(0.0) as f64;
    if bps < 1000.0 {
        format!("{:.0} B/s", bps)
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
        format!("{}d {}h {}m", d, h, m)
    } else if h > 0 {
        format!("{}h {}m {}s", h, m, s)
    } else {
        format!("{}m {}s", m, s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_caps_and_keeps_newest() {
        let mut r = Ring::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            r.push(v);
        }
        assert_eq!(r.as_slice(), &[2.0, 3.0, 4.0]);
        assert_eq!(r.last(), 4.0);
        assert_eq!(r.max(), 4.0);
    }

    #[test]
    fn sample_is_fully_populated() {
        let m = MonitorModel::sample();
        assert!(!m.processes.is_empty());
        assert_eq!(m.metrics.cores.len(), 8);
        assert_eq!(m.metrics.cpu_hist.len(), HISTORY);
        assert!(m.metrics.mem_used > 0 && m.metrics.mem_used < m.metrics.mem_total);
    }

    #[test]
    fn fmt_helpers() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2 * 1024 * 1024), "2.0 MiB");
        assert_eq!(fmt_rate(0.0), "0 B/s");
        assert_eq!(fmt_rate(2_400_000.0), "2.4 MB/s");
        assert_eq!(fmt_uptime(3 * 3600 + 5 * 60 + 9), "3h 5m 9s");
    }
}
