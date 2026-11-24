//! The `processes` resource: what [`tilo_system_resources::processes`] says it is, read from the
//! session's system information service.
//!
//! The service answers a subscription with a snapshot every interval; each one is mapped here
//! into the reading, and the histories the window draws are kept as they arrive. Ending a
//! process is the same service's, by the lifetime the row names.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use tilo_actions::ResourceSpec;
use tilo_service::{offer, unavailable};
use tilo_system_resources::processes::{
    AppUsage, Battery, Cpu, Disk, Gpu, Interface, Machine, Memory, Network, Pressure, PressureLine,
    Process, ProcessAction, ProcessState, Processes, Thermal,
};
use tilo_system_stats::client::{Client, ClientSubscription};
use tilo_system_stats::protocol::{ProcessState as StatsState, Snapshot, SystemStats};

use crate::device::ProcessChange;
use crate::i18n::name;

/// The resource's id.
pub use tilo_system_resources::processes::PROCESSES;

/// How many samples each history keeps: a minute at a second and a half.
const HISTORY: usize = 40;
/// The interval a fresh reading starts at, in milliseconds.
pub const DEFAULT_INTERVAL_MS: u32 = 1500;

/// The `processes` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.processes", label = "Processes")]
pub struct ProcessesReading;

/// The `processes` resource reading `processes`.
#[must_use]
pub fn resource(processes: &Processes) -> ResourceSpec {
    let mut resource = offer::<ProcessAction>(tilo_resource::typed_resource::<ProcessesReading>(
        PROCESSES,
        name("so.tilo.system.processes", "Processes"),
    ));
    if !processes.serving {
        for action in ["refresh", "end", "kill", "end-app", "set-interval"] {
            unavailable(
                &mut resource,
                action,
                "no_system_info",
                "This computer's system information service is not answering",
            );
        }
    }
    resource
}

fn held<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The connection and its subscription, kept for as long as the service runs.
struct Sampler {
    client: Client,
    subscription: ClientSubscription,
}

fn sampler() -> &'static Mutex<Option<Sampler>> {
    static SAMPLER: OnceLock<Mutex<Option<Sampler>>> = OnceLock::new();
    SAMPLER.get_or_init(|| Mutex::new(None))
}

fn notify() -> &'static Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>> {
    static NOTIFY: OnceLock<Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>> = OnceLock::new();
    NOTIFY.get_or_init(|| Mutex::new(None))
}

fn interval() -> &'static AtomicU32 {
    static INTERVAL: AtomicU32 = AtomicU32::new(DEFAULT_INTERVAL_MS);
    &INTERVAL
}

/// The histories the reading carries, oldest first.
#[derive(Default)]
struct Histories {
    cpu: VecDeque<f64>,
    memory: VecDeque<f64>,
    net_rx: VecDeque<f64>,
    net_tx: VecDeque<f64>,
    disk_read: VecDeque<f64>,
    disk_write: VecDeque<f64>,
    processes: HashMap<(u32, u64), VecDeque<f64>>,
}

fn histories() -> &'static Mutex<Histories> {
    static HISTORIES: OnceLock<Mutex<Histories>> = OnceLock::new();
    HISTORIES.get_or_init(|| Mutex::new(Histories::default()))
}

fn latest() -> &'static Mutex<Option<Snapshot>> {
    static LATEST: OnceLock<Mutex<Option<Snapshot>>> = OnceLock::new();
    LATEST.get_or_init(|| Mutex::new(None))
}

fn push(ring: &mut VecDeque<f64>, sample: f64) {
    ring.push_back(sample);
    if ring.len() > HISTORY {
        ring.pop_front();
    }
}

fn socket() -> Option<std::path::PathBuf> {
    Some(
        tilo_paths::runtime_dir()?
            .join("tilo/services")
            .join(format!("{}.sock", tilo_system_stats::SERVICE)),
    )
}

fn connect() -> Option<Client> {
    Client::connect_unconfined(&socket()?, "so.tilo.system-service").ok()
}

/// Start reading the machine every interval, calling `changed` as each snapshot arrives. The
/// subscription is kept here, so the reading takes the newest snapshot it left.
pub fn follow(changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn Send>> {
    let client = connect()?;
    let changed: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::from(changed);
    *held(notify()) = Some(std::sync::Arc::clone(&changed));
    let subscription = client.subscribe(
        interval().load(Ordering::Relaxed).max(250),
        true,
        Some(Box::new(move || changed())),
    );
    *held(sampler()) = Some(Sampler {
        client,
        subscription,
    });
    Some(Box::new(()))
}

/// Read at another interval, or stop reading while `ms` is 0.
///
/// # Errors
/// Why the interval could not be changed.
pub fn set_interval(ms: u32) -> Result<(), String> {
    if ms != 0 && ms < tilo_system_stats::protocol::MIN_INTERVAL_MS {
        return Err(format!(
            "an interval is at least {} milliseconds, or 0",
            tilo_system_stats::protocol::MIN_INTERVAL_MS
        ));
    }
    if ms > tilo_system_stats::protocol::MAX_INTERVAL_MS {
        return Err(format!(
            "an interval is at most {} milliseconds, or 0",
            tilo_system_stats::protocol::MAX_INTERVAL_MS
        ));
    }
    interval().store(ms, Ordering::Relaxed);
    let mut slot = held(sampler());
    let Some(sampler) = slot.as_mut() else {
        return Ok(());
    };
    if ms == 0 {
        *slot = None;
        return Ok(());
    }
    let changed = held(notify()).clone();
    sampler.subscription = sampler.client.subscribe(
        ms,
        true,
        changed.map(|changed| Box::new(move || changed()) as Box<dyn Fn() + Send + Sync>),
    );
    Ok(())
}

/// What this computer reads as now.
#[must_use]
pub fn reading() -> Processes {
    let interval_ms = interval().load(Ordering::Relaxed);
    let suspended = interval_ms == 0;
    let mut slot = held(sampler());
    let Some(sampler) = slot.as_mut() else {
        let last = held(latest()).clone();
        return last.map_or_else(
            || Processes {
                read_error: Some("the system information service is not answering".to_owned()),
                paused: suspended,
                interval_ms,
                ..Processes::default()
            },
            |snapshot| mapped(snapshot, interval_ms),
        );
    };
    if let Some(snapshot) = sampler.subscription.take() {
        *held(latest()) = Some(snapshot);
    }
    let Some(snapshot) = held(latest()).clone() else {
        return Processes {
            serving: true,
            interval_ms,
            paused: suspended,
            machine: machine_of(&SystemStats::default()),
            ..Processes::default()
        };
    };
    let refusal = sampler.subscription.refusal();
    let mut processes = mapped(snapshot, interval_ms);
    processes.serving = refusal.is_none();
    processes.read_error = refusal.map(|refusal| refusal.to_string());
    processes
}

/// The reading `snapshot` describes, with this service's histories.
fn mapped(snapshot: Snapshot, interval_ms: u32) -> Processes {
    let mut rings = held(histories());
    let machine = snapshot.system.as_ref().map_or_else(
        || Machine::default(),
        |system| machine_of_with(system, &mut rings),
    );
    let mut processes: Vec<Process> = snapshot
        .processes
        .iter()
        .map(|process| {
            let key = (process.pid, process.started_at);
            let ring = rings.processes.entry(key).or_default();
            push(ring, f64::from(process.cpu));
            Process {
                pid: process.pid,
                ppid: process.ppid,
                started_at: process.started_at,
                name: process.name.clone(),
                app_id: process.app.clone(),
                user: process.user.clone(),
                cpu: f64::from(process.cpu),
                memory_bytes: process.memory_bytes,
                virtual_bytes: process.virtual_bytes,
                threads: process.threads,
                state: state_of(process.state),
                run_secs: process.run_secs,
                kernel_thread: process.kernel_thread,
                endable: process.endable,
                cpu_history: ring.iter().copied().collect(),
            }
        })
        .collect();
    processes.sort_by(|a, b| {
        b.cpu
            .total_cmp(&a.cpu)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.pid.cmp(&b.pid))
    });
    let keep: BTreeSet<(u32, u64)> = snapshot
        .processes
        .iter()
        .map(|process| (process.pid, process.started_at))
        .collect();
    rings.processes.retain(|key, _| keep.contains(key));
    let apps: Vec<AppUsage> = snapshot
        .apps
        .iter()
        .map(|app| AppUsage {
            app_id: app.app.clone(),
            processes: app.processes,
            cpu: f64::from(app.cpu),
            memory_bytes: app.memory_bytes,
            endable: app.endable,
        })
        .collect();
    Processes {
        serving: true,
        read_error: None,
        sampled_ms: snapshot.taken_ms,
        interval_ms,
        paused: interval_ms == 0,
        machine,
        processes,
        apps,
        pending: Vec::new(),
        problem: None,
    }
}

/// The share of memory in use, 0 to 100, or 0 when the machine reported none.
#[allow(
    clippy::cast_precision_loss,
    reason = "a share of memory read to size a chart, where an exact byte count is not needed"
)]
fn memory_share(system: &SystemStats) -> f64 {
    if system.memory.total == 0 {
        0.0
    } else {
        (system.memory.total.saturating_sub(system.memory.available) as f64
            / system.memory.total as f64)
            * 100.0
    }
}

fn machine_of(system: &SystemStats) -> Machine {
    let mut rings = Histories::default();
    machine_of_with(system, &mut rings)
}

fn machine_of_with(system: &SystemStats, rings: &mut Histories) -> Machine {
    let cpu = system.cpu.as_ref().map_or_else(Cpu::default, |cpu| {
        push(&mut rings.cpu, f64::from(cpu.total));
        Cpu {
            total: f64::from(cpu.total),
            per_core: cpu.per_core.iter().map(|core| f64::from(*core)).collect(),
            history: rings.cpu.iter().copied().collect(),
        }
    });
    let share = memory_share(system);
    push(&mut rings.memory, share);
    let memory = Memory {
        total: system.memory.total,
        available: system.memory.available,
        swap_total: system.memory.swap_total,
        swap_free: system.memory.swap_free,
        history: rings.memory.iter().copied().collect(),
    };
    push(&mut rings.net_rx, f64::from(system.network.rx_bytes_per_sec));
    push(&mut rings.net_tx, f64::from(system.network.tx_bytes_per_sec));
    let read = system
        .disks
        .iter()
        .map(|disk| disk.read_bytes_per_sec)
        .sum::<f32>();
    let written = system
        .disks
        .iter()
        .map(|disk| disk.write_bytes_per_sec)
        .sum::<f32>();
    push(&mut rings.disk_read, f64::from(read));
    push(&mut rings.disk_write, f64::from(written));
    Machine {
        hostname: system.host.hostname.clone(),
        kernel: system.host.kernel.clone(),
        cpu_model: system.host.cpu_model.clone(),
        logical_cpus: system.host.logical_cpus,
        uptime_secs: system.host.uptime_secs,
        cpu,
        load_average: system
            .load_average
            .iter()
            .map(|load| f64::from(*load))
            .collect(),
        memory,
        pressure: system.pressure.map(|pressure| Pressure {
            cpu: line_of(pressure.cpu),
            memory: line_of(pressure.memory),
            io: line_of(pressure.io),
        }),
        disks: system
            .disks
            .iter()
            .map(|disk| Disk {
                device: disk.device.clone(),
                mount: disk.mount.clone(),
                total_bytes: disk.total_bytes,
                used_bytes: disk.used_bytes,
                read_per_sec: f64::from(disk.read_bytes_per_sec),
                write_per_sec: f64::from(disk.write_bytes_per_sec),
            })
            .collect(),
        disk_read_history: rings.disk_read.iter().copied().collect(),
        disk_write_history: rings.disk_write.iter().copied().collect(),
        network: Network {
            rx_per_sec: f64::from(system.network.rx_bytes_per_sec),
            tx_per_sec: f64::from(system.network.tx_bytes_per_sec),
            rx_history: rings.net_rx.iter().copied().collect(),
            tx_history: rings.net_tx.iter().copied().collect(),
            interfaces: system
                .network
                .interfaces
                .iter()
                .map(|interface| Interface {
                    name: interface.name.clone(),
                    rx_per_sec: f64::from(interface.rx_bytes_per_sec),
                    tx_per_sec: f64::from(interface.tx_bytes_per_sec),
                })
                .collect(),
        },
        gpus: system
            .gpus
            .iter()
            .map(|gpu| Gpu {
                driver: gpu.driver.clone(),
                busy_percent: gpu.busy_percent.map(f64::from),
                memory_used: gpu.memory_used,
                memory_total: gpu.memory_total,
            })
            .collect(),
        batteries: system
            .batteries
            .iter()
            .map(|battery| Battery {
                name: battery.name.clone(),
                percent: battery.percent.map(f64::from),
                status: battery.status.clone(),
                power_watts: battery.power_watts.map(f64::from),
            })
            .collect(),
        thermal: system
            .thermal
            .iter()
            .map(|zone| Thermal {
                name: zone.name.clone(),
                celsius: f64::from(zone.celsius),
            })
            .collect(),
    }
}

fn line_of(line: tilo_system_stats::protocol::PressureLine) -> PressureLine {
    PressureLine {
        some_avg10: f64::from(line.some_avg10),
        full_avg10: line.full_avg10.map(f64::from),
    }
}

fn state_of(state: StatsState) -> ProcessState {
    match state {
        StatsState::Running => ProcessState::Running,
        StatsState::Sleeping => ProcessState::Sleeping,
        StatsState::Waiting => ProcessState::Waiting,
        StatsState::Stopped => ProcessState::Stopped,
        StatsState::Zombie => ProcessState::Zombie,
        StatsState::Idle => ProcessState::Idle,
        StatsState::Dead => ProcessState::Dead,
    }
}

/// The pid and lifetime a row id names: `1234:5678`.
fn lifetime_of(row: &str) -> Result<(u32, u64), String> {
    let (pid, started) = row
        .split_once(':')
        .ok_or_else(|| format!("`{row}` names no process lifetime"))?;
    Ok((
        pid.trim()
            .parse()
            .map_err(|_| format!("`{pid}` is no process id"))?,
        started
            .trim()
            .parse()
            .map_err(|_| format!("`{started}` is no start time"))?,
    ))
}

/// Make `change` on the process list.
///
/// # Errors
/// The system information service's refusal, in its own words.
pub fn change(change: &ProcessChange) -> Result<(), String> {
    let slot = held(sampler());
    let Some(sampler) = slot.as_ref() else {
        return Err("the system information service is not answering".to_owned());
    };
    let client = sampler.client.clone();
    drop(slot);
    match change {
        ProcessChange::End(process) => {
            let (pid, started_at) = lifetime_of(process)?;
            client
                .end_process(pid, started_at, false)
                .map(drop)
                .map_err(|refusal| refusal.to_string())
        }
        ProcessChange::Kill(process) => {
            let (pid, started_at) = lifetime_of(process)?;
            client
                .end_process(pid, started_at, true)
                .map(drop)
                .map_err(|refusal| refusal.to_string())
        }
        ProcessChange::EndApp(app) => client
            .end_app(app, false)
            .map(drop)
            .map_err(|refusal| refusal.to_string()),
        ProcessChange::SetInterval(ms) => set_interval(*ms),
    }
}
