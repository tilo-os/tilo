//! Reading the kernel's reporting interfaces: `/proc` and `/sys`.
//!
//! A [`Probe`] reads cumulative counters; [`crate::collect`] turns two
//! readings into rates. Everything is read under a root directory, so a test
//! can lay out a machine of its own. What is read, per process, is exactly
//! `stat`, `status` and `cgroup`, and `cmdline` only when a subscriber holds
//! the command-line grant. `environ`, `fd`, `maps`, `mem` and every other
//! per-process file are never opened.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "readings are shown to a few significant digits"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Clock ticks per second in `/proc`'s process times. Linux reports them in
/// `USER_HZ`, which is 100 on every architecture whatever the kernel's own
/// tick rate.
pub const USER_HZ: u64 = 100;

/// `PF_KTHREAD` in a process's `stat` flags.
const PF_KTHREAD: u64 = 0x0020_0000;

/// What one reading should include.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Want {
    pub system: bool,
    pub processes: bool,
    pub command_lines: bool,
}

/// Cumulative counters and instant values, as read.
#[derive(Clone, Debug, Default)]
pub struct RawSample {
    pub system: Option<RawSystem>,
    /// `None` when the reading did not include processes.
    pub processes: Option<Vec<RawProcess>>,
}

/// Machine-wide counters.
#[derive(Clone, Debug, Default)]
pub struct RawSystem {
    pub hostname: String,
    pub kernel: String,
    pub cpu_model: Option<String>,
    pub uptime_secs: u64,
    /// The aggregate `cpu` line, then one per core.
    pub cpu_total: Option<CpuTicks>,
    pub cpu_cores: Vec<CpuTicks>,
    pub load_average: [f32; 3],
    pub memory: crate::protocol::MemoryStats,
    pub pressure: Option<crate::protocol::Pressure>,
    pub disks: Vec<RawDisk>,
    pub interfaces: Vec<RawInterface>,
    pub gpus: Vec<crate::protocol::GpuStats>,
    pub batteries: Vec<crate::protocol::BatteryStats>,
    pub thermal: Vec<crate::protocol::ThermalZone>,
}

/// One `cpu` line of `/proc/stat`, in ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuTicks {
    pub busy: u64,
    pub total: u64,
}

/// One whole disk: its I/O counters and, when mounted, its space.
#[derive(Clone, Debug, Default)]
pub struct RawDisk {
    pub device: String,
    pub mount: Option<String>,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub read_bytes: u64,
    pub written_bytes: u64,
}

/// One interface's byte counters.
#[derive(Clone, Debug, Default)]
pub struct RawInterface {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// One process, as read.
#[derive(Clone, Debug, Default)]
pub struct RawProcess {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub started_at: u64,
    pub name: String,
    pub state: char,
    /// User plus system time, in ticks.
    pub cpu_ticks: u64,
    pub virtual_bytes: u64,
    pub resident_bytes: u64,
    pub threads: u32,
    pub uid: u32,
    pub kernel_thread: bool,
    pub cgroup: Option<String>,
    pub command: Option<String>,
}

/// Where readings come from.
pub trait Probe: Send {
    /// Read what `want` asks for.
    fn read(&mut self, want: Want) -> RawSample;

    /// Read one process's identity, owner and cgroup again, right before
    /// ending it; `None` when it is gone.
    fn process(&mut self, pid: u32) -> Option<RawProcess>;

    /// Every process, for finding an app's processes to end.
    fn processes(&mut self) -> Vec<RawProcess>;

    /// The boot time in seconds since the epoch, for run times.
    fn boot_time(&self) -> Option<u64>;

    /// The account name of `uid`.
    fn user_name(&mut self, uid: u32) -> String;
}

/// The machine under `root` (`/` in production).
pub struct KernelProbe {
    root: PathBuf,
    users: Option<HashMap<u32, String>>,
    boot_time: Option<u64>,
}

impl KernelProbe {
    /// The running machine.
    #[must_use]
    pub fn new() -> Self {
        Self::at("/")
    }

    /// A machine laid out under `root`.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let boot_time = read(&root, "proc/stat").and_then(|stat| {
            stat.lines()
                .find_map(|line| line.strip_prefix("btime "))
                .and_then(|value| value.trim().parse().ok())
        });
        Self {
            root,
            users: None,
            boot_time,
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn system(&self) -> RawSystem {
        let stat = read(&self.root, "proc/stat").unwrap_or_default();
        let mut cpu_total = None;
        let mut cpu_cores = Vec::new();
        for line in stat.lines() {
            if let Some(rest) = line.strip_prefix("cpu ") {
                cpu_total = parse_cpu_ticks(rest);
            } else if let Some(rest) = line.strip_prefix("cpu")
                && rest.starts_with(|c: char| c.is_ascii_digit())
                && let Some((_, fields)) = rest.split_once(' ')
                && let Some(ticks) = parse_cpu_ticks(fields)
            {
                cpu_cores.push(ticks);
            }
        }
        RawSystem {
            hostname: read_trimmed(&self.root, "proc/sys/kernel/hostname").unwrap_or_default(),
            kernel: read_trimmed(&self.root, "proc/sys/kernel/osrelease").unwrap_or_default(),
            cpu_model: read(&self.root, "proc/cpuinfo").and_then(|info| cpu_model(&info)),
            uptime_secs: read(&self.root, "proc/uptime")
                .and_then(|text| {
                    text.split_whitespace()
                        .next()
                        .and_then(|s| s.split('.').next())
                        .and_then(|s| s.parse().ok())
                })
                .unwrap_or(0),
            cpu_total,
            cpu_cores,
            load_average: read(&self.root, "proc/loadavg")
                .map(|text| parse_loadavg(&text))
                .unwrap_or_default(),
            memory: read(&self.root, "proc/meminfo")
                .map(|text| parse_meminfo(&text))
                .unwrap_or_default(),
            pressure: self.pressure(),
            disks: self.disks(),
            interfaces: read(&self.root, "proc/net/dev")
                .map(|text| parse_net_dev(&text))
                .unwrap_or_default(),
            gpus: self.gpus(),
            batteries: self.batteries(),
            thermal: self.thermal(),
        }
    }

    fn pressure(&self) -> Option<crate::protocol::Pressure> {
        let line = |name: &str| {
            read(&self.root, &format!("proc/pressure/{name}")).map(|t| parse_pressure(&t))
        };
        Some(crate::protocol::Pressure {
            cpu: line("cpu")?,
            memory: line("memory")?,
            io: line("io")?,
        })
    }

    /// Whole physical disks, with the space of the file system mounted from
    /// each (the largest, where a disk holds several).
    fn disks(&self) -> Vec<RawDisk> {
        let stats = read(&self.root, "proc/diskstats").unwrap_or_default();
        let mounts = read(&self.root, "proc/self/mountinfo").unwrap_or_default();
        let mut disks: Vec<RawDisk> = Vec::new();
        for line in stats.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (Some(name), Some(sectors_read), Some(sectors_written)) =
                (fields.get(2), fields.get(5), fields.get(9))
            else {
                continue;
            };
            // A whole disk backed by a device; partitions, loop, zram and
            // device-mapper volumes are counted by the disk beneath them.
            if !self.path(&format!("sys/block/{name}/device")).exists() {
                continue;
            }
            let sector = |value: &str| value.parse::<u64>().unwrap_or(0).saturating_mul(512);
            disks.push(RawDisk {
                device: (*name).to_owned(),
                read_bytes: sector(sectors_read),
                written_bytes: sector(sectors_written),
                ..RawDisk::default()
            });
        }
        for (source, mount) in parse_mounts(&mounts) {
            let Some(device) = source.strip_prefix("/dev/") else {
                continue;
            };
            let Some(disk) = disks
                .iter_mut()
                .find(|disk| device == disk.device || is_partition_of(device, &disk.device))
            else {
                continue;
            };
            let Some((total, used)) = self.space(&mount) else {
                continue;
            };
            if total > disk.total_bytes {
                disk.total_bytes = total;
                disk.used_bytes = used;
                disk.mount = Some(mount);
            }
        }
        disks
    }

    #[cfg(unix)]
    fn space(&self, mount: &str) -> Option<(u64, u64)> {
        let path = self.root.join(mount.trim_start_matches('/'));
        let stat = rustix::fs::statvfs(&path).ok()?;
        let total = stat.f_blocks.saturating_mul(stat.f_frsize);
        let free = stat.f_bfree.saturating_mul(stat.f_frsize);
        Some((total, total.saturating_sub(free)))
    }

    #[cfg(not(unix))]
    fn space(&self, _mount: &str) -> Option<(u64, u64)> {
        None
    }

    fn gpus(&self) -> Vec<crate::protocol::GpuStats> {
        let mut gpus = Vec::new();
        for card in entries(&self.path("sys/class/drm")) {
            let name = file_name(&card);
            let is_card = name
                .strip_prefix("card")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            if !is_card {
                continue;
            }
            let device = card.join("device");
            let driver = std::fs::read_to_string(device.join("uevent"))
                .ok()
                .and_then(|uevent| {
                    uevent
                        .lines()
                        .find_map(|line| line.strip_prefix("DRIVER="))
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            let number = |file: &str| {
                std::fs::read_to_string(device.join(file))
                    .ok()
                    .and_then(|text| text.trim().parse::<u64>().ok())
            };
            let busy = number("gpu_busy_percent")
                .map(|v| v as f32)
                .or_else(|| devfreq_load(&device));
            gpus.push(crate::protocol::GpuStats {
                driver,
                busy_percent: busy,
                memory_used: number("mem_info_vram_used"),
                memory_total: number("mem_info_vram_total"),
            });
        }
        gpus
    }

    fn batteries(&self) -> Vec<crate::protocol::BatteryStats> {
        let mut batteries = Vec::new();
        for supply in entries(&self.path("sys/class/power_supply")) {
            let text = |file: &str| {
                std::fs::read_to_string(supply.join(file))
                    .ok()
                    .map(|t| t.trim().to_owned())
            };
            if text("type").as_deref() != Some("Battery") {
                continue;
            }
            let micro = |file: &str| text(file).and_then(|t| t.parse::<f64>().ok());
            let power = micro("power_now").or_else(|| {
                let current = micro("current_now")?;
                let voltage = micro("voltage_now")?;
                Some(current * voltage / 1e6)
            });
            batteries.push(crate::protocol::BatteryStats {
                name: file_name(&supply),
                percent: text("capacity").and_then(|t| t.parse::<f32>().ok()),
                status: text("status").unwrap_or_else(|| "Unknown".to_owned()),
                power_watts: power.map(|microwatts| (microwatts.abs() / 1e6) as f32),
            });
        }
        batteries
    }

    fn thermal(&self) -> Vec<crate::protocol::ThermalZone> {
        let mut zones = Vec::new();
        for zone in entries(&self.path("sys/class/thermal")) {
            if !file_name(&zone).starts_with("thermal_zone") {
                continue;
            }
            let Some(milli) = std::fs::read_to_string(zone.join("temp"))
                .ok()
                .and_then(|t| t.trim().parse::<i64>().ok())
            else {
                continue;
            };
            let celsius = milli as f32 / 1000.0;
            // A broken sensor reports absolute zero or a nonsense value.
            if !(-40.0..=150.0).contains(&celsius) {
                continue;
            }
            let name = std::fs::read_to_string(zone.join("type"))
                .map_or_else(|_| file_name(&zone), |t| t.trim().to_owned());
            zones.push(crate::protocol::ThermalZone { name, celsius });
        }
        zones
    }

    fn read_process(&self, pid: u32, command_line: bool) -> Option<RawProcess> {
        let dir = self.path(&format!("proc/{pid}"));
        let stat = std::fs::read_to_string(dir.join("stat")).ok()?;
        let status = std::fs::read_to_string(dir.join("status")).ok()?;
        let mut process = parse_stat(pid, &stat)?;
        apply_status(&mut process, &status);
        process.cgroup = std::fs::read_to_string(dir.join("cgroup"))
            .ok()
            .and_then(|text| crate::apps::unified_path(&text).map(str::to_owned));
        if command_line {
            process.command = std::fs::read(dir.join("cmdline")).ok().and_then(|bytes| {
                let joined = bytes
                    .split(|byte| *byte == 0)
                    .filter(|arg| !arg.is_empty())
                    .map(String::from_utf8_lossy)
                    .collect::<Vec<_>>()
                    .join(" ");
                (!joined.is_empty()).then_some(joined)
            });
        }
        Some(process)
    }

    fn pids(&self) -> Vec<u32> {
        let Ok(dir) = std::fs::read_dir(self.path("proc")) else {
            return Vec::new();
        };
        dir.filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .collect()
    }

    fn all_processes(&self, command_lines: bool) -> Vec<RawProcess> {
        self.pids()
            .into_iter()
            .filter_map(|pid| self.read_process(pid, command_lines))
            .collect()
    }
}

impl Default for KernelProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for KernelProbe {
    fn read(&mut self, want: Want) -> RawSample {
        RawSample {
            system: want.system.then(|| self.system()),
            processes: want
                .processes
                .then(|| self.all_processes(want.command_lines)),
        }
    }

    fn process(&mut self, pid: u32) -> Option<RawProcess> {
        self.read_process(pid, false)
    }

    fn processes(&mut self) -> Vec<RawProcess> {
        self.all_processes(false)
    }

    fn boot_time(&self) -> Option<u64> {
        self.boot_time
    }

    fn user_name(&mut self, uid: u32) -> String {
        let root = &self.root;
        let users = self.users.get_or_insert_with(|| {
            read(root, "etc/passwd")
                .map(|text| parse_passwd(&text))
                .unwrap_or_default()
        });
        users.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
    }
}

fn read(root: &Path, relative: &str) -> Option<String> {
    std::fs::read_to_string(root.join(relative)).ok()
}

fn read_trimmed(root: &Path, relative: &str) -> Option<String> {
    read(root, relative).map(|text| text.trim().to_owned())
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|it| it.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    paths.sort();
    paths
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A devfreq-managed GPU's `load` (`37@500000000Hz`), for drivers that
/// report no busy percentage of their own.
fn devfreq_load(device: &Path) -> Option<f32> {
    let dir = entries(&device.join("devfreq")).into_iter().next()?;
    let text = std::fs::read_to_string(dir.join("load")).ok()?;
    text.trim().split('@').next()?.parse().ok()
}

/// `user nice system idle iowait irq softirq steal …`: idle and iowait are
/// not busy, and guest time is already counted in user time.
#[must_use]
pub fn parse_cpu_ticks(fields: &str) -> Option<CpuTicks> {
    let values: Vec<u64> = fields
        .split_whitespace()
        .filter_map(|field| field.parse().ok())
        .collect();
    let idle = values
        .get(3)?
        .saturating_add(values.get(4).copied().unwrap_or(0));
    let total = values
        .iter()
        .take(8)
        .fold(0_u64, |sum, value| sum.saturating_add(*value));
    Some(CpuTicks {
        busy: total.saturating_sub(idle),
        total,
    })
}

fn cpu_model(cpuinfo: &str) -> Option<String> {
    cpuinfo
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| matches!(key.trim(), "model name" | "Model" | "Hardware"))
        .map(|(_, value)| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_loadavg(text: &str) -> [f32; 3] {
    let mut values = text.split_whitespace().map(|v| v.parse().unwrap_or(0.0));
    [
        values.next().unwrap_or(0.0),
        values.next().unwrap_or(0.0),
        values.next().unwrap_or(0.0),
    ]
}

/// `/proc/meminfo`, whose values are in KiB.
#[must_use]
pub fn parse_meminfo(text: &str) -> crate::protocol::MemoryStats {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kib| kib.parse::<u64>().ok())
            .unwrap_or(0)
            .saturating_mul(1024)
    };
    crate::protocol::MemoryStats {
        total: field("MemTotal:"),
        available: field("MemAvailable:"),
        swap_total: field("SwapTotal:"),
        swap_free: field("SwapFree:"),
    }
}

fn parse_pressure(text: &str) -> crate::protocol::PressureLine {
    let avg10 = |prefix: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .and_then(|rest| {
                rest.split_whitespace()
                    .find_map(|field| field.strip_prefix("avg10="))
            })
            .and_then(|value| value.parse::<f32>().ok())
    };
    crate::protocol::PressureLine {
        some_avg10: avg10("some ").unwrap_or(0.0),
        full_avg10: avg10("full "),
    }
}

fn parse_net_dev(text: &str) -> Vec<RawInterface> {
    text.lines()
        .skip(2)
        .filter_map(|line| {
            let (name, counters) = line.split_once(':')?;
            let name = name.trim();
            if name == "lo" {
                return None;
            }
            let values: Vec<u64> = counters
                .split_whitespace()
                .filter_map(|v| v.parse().ok())
                .collect();
            Some(RawInterface {
                name: name.to_owned(),
                rx_bytes: *values.first()?,
                tx_bytes: *values.get(8)?,
            })
        })
        .collect()
}

/// `(source, mount point)` of every mount in a `mountinfo`.
fn parse_mounts(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (before, after) = line.split_once(" - ")?;
            let mount = before.split_whitespace().nth(4)?;
            let source = after.split_whitespace().nth(1)?;
            Some((source.to_owned(), unescape_mount(mount)))
        })
        .collect()
}

/// `mountinfo` escapes space, tab, newline and backslash as octal.
fn unescape_mount(path: &str) -> String {
    path.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

/// `nvme0n1p2` of `nvme0n1`, `sda1` of `sda`, `mmcblk0p1` of `mmcblk0`.
fn is_partition_of(device: &str, disk: &str) -> bool {
    device.strip_prefix(disk).is_some_and(|rest| {
        let digits = rest.strip_prefix('p').unwrap_or(rest);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

/// `/proc/<pid>/stat`. The name sits in parentheses and may itself hold
/// spaces and parentheses, so the fields are counted from the last `)`.
#[must_use]
pub fn parse_stat(pid: u32, stat: &str) -> Option<RawProcess> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open.saturating_add(1)..close)?.to_owned();
    let rest: Vec<&str> = stat
        .get(close.saturating_add(1)..)?
        .split_whitespace()
        .collect();
    // `rest[0]` is field 3 (state); field n is `rest[n - 3]`.
    let field = |n: usize| rest.get(n.saturating_sub(3)).copied();
    let number = |n: usize| field(n).and_then(|v| v.parse::<u64>().ok());
    let state = field(3)?.chars().next()?;
    let parent = number(4)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v != 0);
    let flags = number(9).unwrap_or(0);
    let utime = number(14).unwrap_or(0);
    let stime = number(15).unwrap_or(0);
    let threads = number(20).and_then(|v| u32::try_from(v).ok()).unwrap_or(1);
    let started_at = number(22)?;
    let virtual_bytes = number(23).unwrap_or(0);
    Some(RawProcess {
        pid,
        ppid: parent,
        started_at,
        name,
        state,
        cpu_ticks: utime.saturating_add(stime),
        virtual_bytes,
        threads,
        kernel_thread: flags & PF_KTHREAD != 0,
        ..RawProcess::default()
    })
}

/// The owner and resident memory from `/proc/<pid>/status`.
fn apply_status(process: &mut RawProcess, status: &str) {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            // Real, effective, saved, file system: the real owner is whose
            // process it is.
            if let Some(uid) = rest.split_whitespace().next().and_then(|v| v.parse().ok()) {
                process.uid = uid;
            }
        } else if let Some(rest) = line.strip_prefix("VmRSS:") {
            process.resident_bytes = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
                .saturating_mul(1024);
        }
    }
}

fn parse_passwd(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let uid = fields.nth(1)?.parse().ok()?;
            Some((uid, name.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_name_with_parentheses_and_spaces_reads_whole() {
        let stat = "4242 (Web (Content) 1) S 4000 4242 4242 0 -1 4194560 1 0 0 0 \
                    150 50 0 0 20 0 7 0 98765 123456789 2000 18446744073709551615";
        let process = parse_stat(4242, stat).unwrap();
        assert_eq!(process.name, "Web (Content) 1");
        assert_eq!(process.state, 'S');
        assert_eq!(process.ppid, Some(4000));
        assert_eq!(process.cpu_ticks, 200);
        assert_eq!(process.threads, 7);
        assert_eq!(process.started_at, 98765);
        assert_eq!(process.virtual_bytes, 123_456_789);
        assert!(!process.kernel_thread);
        let kthread = "2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 0 0 0 20 0 1 0 5 0 0";
        let kthread = parse_stat(2, kthread).unwrap();
        assert!(kthread.kernel_thread);
        assert_eq!(kthread.ppid, None);
    }

    #[test]
    fn partitions_belong_to_their_disk() {
        assert!(is_partition_of("nvme0n1p2", "nvme0n1"));
        assert!(is_partition_of("sda1", "sda"));
        assert!(!is_partition_of("sdab", "sda"));
        assert!(!is_partition_of("nvme0n10", "nvme0n1p"));
        assert_eq!(unescape_mount("/run/media/a\\040b"), "/run/media/a b");
    }
}
