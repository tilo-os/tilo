//! The `processes` resource: the process table of this computer and the machine's own readings —
//! processor, memory, pressure, disks, network, graphics, battery and temperature — with the
//! actions that read them again and end a process.
//!
//! What reads `/proc` is the session's system information service; this resource is the
//! vocabulary the Monitor window, an agent and a script read it by. Command lines are not part of
//! the reading: they are granted to one app at a time, and a reading is one value for the whole
//! session.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const PROCESSES: &str = "processes";

/// How a process stands.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessState {
    Running,
    #[default]
    Sleeping,
    /// Waiting on the disk.
    Waiting,
    Stopped,
    Zombie,
    Idle,
    Dead,
}

/// One process, as the table shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Process {
    pub pid: u32,
    pub ppid: Option<u32>,
    /// Its start time in clock ticks since boot: with the pid, the identity of this lifetime.
    pub started_at: u64,
    pub name: String,
    /// The app this process belongs to, when it belongs to one.
    pub app_id: Option<String>,
    pub user: String,
    /// Processor use in percent of the whole machine.
    pub cpu: f64,
    pub memory_bytes: u64,
    pub virtual_bytes: u64,
    pub threads: u32,
    pub state: ProcessState,
    /// Seconds since it started.
    pub run_secs: u64,
    pub kernel_thread: bool,
    /// Whether the person may end it: their own app or process, never the system's services.
    pub endable: bool,
    /// Its processor use over the last samples, oldest first.
    pub cpu_history: Vec<f64>,
}

/// A running app, its processes' use together.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct AppUsage {
    pub app_id: String,
    pub processes: u32,
    pub cpu: f64,
    pub memory_bytes: u64,
    pub endable: bool,
}

/// Processor load in percent of each core, and of all of them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Cpu {
    pub total: f64,
    pub per_core: Vec<f64>,
    /// The total over the last samples, oldest first.
    pub history: Vec<f64>,
}

/// Memory and swap, in bytes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
    /// The share in use over the last samples, oldest first.
    pub history: Vec<f64>,
}

/// Pressure stall information: the share of the last ten seconds in which work waited.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct PressureLine {
    pub some_avg10: f64,
    pub full_avg10: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Pressure {
    pub cpu: PressureLine,
    pub memory: PressureLine,
    pub io: PressureLine,
}

/// One disk and what it holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Disk {
    pub device: String,
    pub mount: Option<String>,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub read_per_sec: f64,
    pub write_per_sec: f64,
}

/// One network interface's throughput.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Interface {
    pub name: String,
    pub rx_per_sec: f64,
    pub tx_per_sec: f64,
}

/// The machine's network throughput, and its history.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Network {
    pub rx_per_sec: f64,
    pub tx_per_sec: f64,
    pub rx_history: Vec<f64>,
    pub tx_history: Vec<f64>,
    pub interfaces: Vec<Interface>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Gpu {
    pub driver: String,
    pub busy_percent: Option<f64>,
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Battery {
    pub name: String,
    pub percent: Option<f64>,
    /// As the kernel spells it: `Charging`, `Discharging`, `Full`, `Not charging`, `Unknown`.
    pub status: String,
    pub power_watts: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Thermal {
    pub name: String,
    pub celsius: f64,
}

/// The machine itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Machine {
    pub hostname: String,
    pub kernel: String,
    pub cpu_model: Option<String>,
    pub logical_cpus: u32,
    pub uptime_secs: u64,
    pub cpu: Cpu,
    /// The load averages over 1, 5 and 15 minutes.
    pub load_average: Vec<f64>,
    pub memory: Memory,
    pub pressure: Option<Pressure>,
    pub disks: Vec<Disk>,
    pub disk_read_history: Vec<f64>,
    pub disk_write_history: Vec<f64>,
    pub network: Network,
    pub gpus: Vec<Gpu>,
    pub batteries: Vec<Battery>,
    pub thermal: Vec<Thermal>,
}

/// What this computer reads as now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Processes {
    /// Whether the system information service answers here.
    #[serde(default)]
    pub serving: bool,
    /// Why the readings could not be read, until they can.
    #[serde(default)]
    pub read_error: Option<String>,
    /// When the last reading was taken, in milliseconds since the Unix epoch.
    pub sampled_ms: u64,
    /// How often a reading is taken, in milliseconds; 0 while paused.
    pub interval_ms: u32,
    pub paused: bool,
    pub machine: Machine,
    /// Running processes, busiest first.
    pub processes: Vec<Process>,
    /// Running apps, busiest first.
    pub apps: Vec<AppUsage>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
    /// Why the last action did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
}

/// The `processes` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.processes")]
pub enum ProcessAction {
    /// Read the machine and its processes again.
    #[effect(kind = "read")]
    #[effect(own_device, icon = "rotate-cw", keywords = "refresh processes, prozesse neu lesen")]
    Refresh,
    /// Ask a process to close, as quitting it would.
    #[effect(kind = "outside")]
    #[effect(own_device, destructive, icon = "square", keywords = "end process, prozess beenden")]
    End {
        #[param("The process's row id, `pid:started_at`")] process: String,
    },
    /// End a process at once.
    #[effect(kind = "outside")]
    #[effect(own_device, destructive, icon = "x", keywords = "kill process, prozess töten")]
    Kill {
        #[param("The process's row id, `pid:started_at`")] process: String,
    },
    /// Ask every process of an app to close.
    #[effect(kind = "outside")]
    #[effect(own_device, destructive, icon = "square", keywords = "end app, app beenden")]
    EndApp {
        #[param("The app's id")] application: String,
    },
    /// Read the machine every `ms` milliseconds, or stop reading while 0.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "clock", keywords = "update interval, aktualisierungsintervall")]
    SetInterval {
        #[param("The interval in milliseconds, or 0 to pause")] ms: u32,
    },
}
