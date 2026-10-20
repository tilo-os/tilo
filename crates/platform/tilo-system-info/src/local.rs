//! The system information service in this process, for a platform whose
//! apps may read the machine themselves and where no Tilo session serves
//! readings (macOS).
//!
//! [`SysinfoProbe`] reads the platform's interfaces through `sysinfo` into
//! the service's raw counters, so the service's own collector, grants and
//! end policy apply unchanged; [`connection`] serves it over a socket pair
//! to this process's client. Every process the person runs is theirs to end
//! here: the platform has no app slice to confine that to.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::OnceLock;

use sysinfo::{
    Disks, Networks, Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, Users,
};
use tilo_system_stats::apps::EndPolicy;
use tilo_system_stats::client::Client;
use tilo_system_stats::end::Signaller;
use tilo_system_stats::hub::Hub;
use tilo_system_stats::probe::{
    CpuTicks, Probe, RawDisk, RawInterface, RawProcess, RawSample, RawSystem, USER_HZ, Want,
};
use tilo_system_stats::protocol::{Grants, Handoff, MemoryStats, PROTOCOL, Refusal};

/// The cgroup every process reads as here, and the app root the end policy
/// allows: the platform does not group processes into apps.
const ALL_PROCESSES: &str = "/";

/// This process's connection to a service in this process, started on
/// first use.
pub fn connection(app_id: &str) -> Option<Client> {
    static LOCAL: OnceLock<Option<Client>> = OnceLock::new();
    LOCAL
        .get_or_init(|| {
            let client = serve(app_id)
                .inspect_err(|error| {
                    tracing::warn!(%error, "no system information service in this process");
                })
                .ok()?;
            Client::install_shared(client.clone());
            Some(client)
        })
        .clone()
        .filter(|client| !client.is_closed())
}

/// Start the service over the platform's interfaces and connect to it.
fn serve(app_id: &str) -> std::io::Result<Client> {
    let hub = Hub::new(
        Box::new(SysinfoProbe::new()),
        EndPolicy {
            uid: rustix::process::geteuid().as_raw(),
            app_root: ALL_PROCESSES.to_owned(),
            own_pid: std::process::id(),
        },
        Box::new(KillSignaller),
    );
    let (app, service) = UnixStream::pair()?;
    let reader = service.try_clone()?;
    // The app reads its own machine: everything is granted.
    let handoff = Handoff {
        protocol: PROTOCOL,
        app_id: app_id.to_owned(),
        workspace_id: None,
        grants: Grants::ALL,
        ceiling: Grants::ALL,
    };
    std::thread::Builder::new()
        .name("tilo-system-info".into())
        .spawn(move || {
            tilo_system_stats::server::serve(
                reader,
                service,
                &handoff,
                &hub,
                &tilo_system_stats::server::NoPrompter,
            );
        })?;
    Client::from_stream(app)
}

/// Ends a process with a signal once its identity was checked again.
struct KillSignaller;

impl Signaller for KillSignaller {
    fn signal_checked(
        &self,
        pid: u32,
        force: bool,
        check: &mut dyn FnMut() -> Result<(), Refusal>,
    ) -> Result<(), Refusal> {
        use rustix::process::{Pid, Signal, kill_process};
        let pid = i32::try_from(pid)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or(Refusal::Gone)?;
        check()?;
        let signal = if force { Signal::KILL } else { Signal::TERM };
        kill_process(pid, signal).map_err(|error| {
            if error == rustix::io::Errno::SRCH {
                Refusal::Gone
            } else {
                Refusal::Failed {
                    detail: error.to_string(),
                }
            }
        })
    }
}

/// The machine as the platform reports it, as the service's raw counters.
///
/// The platform reports processor use as a share since the last reading;
/// the service computes shares from cumulative counters, so the probe keeps
/// counters that advance by each share of the time between readings.
pub struct SysinfoProbe {
    system: System,
    networks: Networks,
    disks: Disks,
    users: Users,
    boot_time: u64,
    /// When the processor counters last advanced.
    last: Option<std::time::Instant>,
    cpu_total: CpuTicks,
    cpu_cores: Vec<CpuTicks>,
    /// Bytes every process read and wrote, summed over the readings.
    disk_io: (u64, u64),
}

impl Default for SysinfoProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl SysinfoProbe {
    /// A probe primed with one reading, so the first one the service takes
    /// has a share to report.
    #[must_use]
    pub fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, process_detail());
        Self {
            system,
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            users: Users::new_with_refreshed_list(),
            boot_time: System::boot_time(),
            last: Some(std::time::Instant::now()),
            cpu_total: CpuTicks::default(),
            cpu_cores: Vec::new(),
            disk_io: (0, 0),
        }
    }

    /// Advance the processor counters by the shares since the last reading.
    fn advance_cpu(&mut self) {
        self.system.refresh_cpu_usage();
        let now = std::time::Instant::now();
        let elapsed = self.last.map_or(0, |last| {
            u64::try_from(now.duration_since(last).as_millis()).unwrap_or(0)
        });
        self.last = Some(now);
        let advance = |ticks: &mut CpuTicks, usage: f32| {
            let share = f64::from(usage.clamp(0.0, 100.0)) / 100.0;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "a share of a few seconds in milliseconds"
            )]
            let busy = (elapsed as f64 * share) as u64;
            ticks.busy = ticks.busy.saturating_add(busy);
            ticks.total = ticks.total.saturating_add(elapsed);
        };
        let cpus = self.system.cpus();
        self.cpu_cores.resize(cpus.len(), CpuTicks::default());
        for (ticks, cpu) in self.cpu_cores.iter_mut().zip(cpus) {
            advance(ticks, cpu.cpu_usage());
        }
        advance(&mut self.cpu_total, self.system.global_cpu_usage());
    }

    fn system(&mut self) -> RawSystem {
        self.advance_cpu();
        self.system.refresh_memory();
        self.networks.refresh(true);
        self.disks.refresh(true);
        let load = System::load_average();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "load averages are small numbers"
        )]
        let load_average = [load.one as f32, load.five as f32, load.fifteen as f32];
        RawSystem {
            hostname: System::host_name().unwrap_or_default(),
            kernel: System::kernel_version().unwrap_or_default(),
            cpu_model: self
                .system
                .cpus()
                .first()
                .map(|cpu| cpu.brand().trim().to_owned())
                .filter(|brand| !brand.is_empty()),
            uptime_secs: System::uptime(),
            cpu_total: Some(self.cpu_total),
            cpu_cores: self.cpu_cores.clone(),
            load_average,
            memory: MemoryStats {
                total: self.system.total_memory(),
                available: self.system.available_memory(),
                swap_total: self.system.total_swap(),
                swap_free: self.system.free_swap(),
            },
            pressure: None,
            disks: self.raw_disks(),
            interfaces: self
                .networks
                .iter()
                .map(|(name, data)| RawInterface {
                    name: name.clone(),
                    rx_bytes: data.total_received(),
                    tx_bytes: data.total_transmitted(),
                })
                .collect(),
            gpus: Vec::new(),
            batteries: Vec::new(),
            thermal: Vec::new(),
        }
    }

    /// Each mounted disk's space, and the I/O every process did as one
    /// device: the platform reports I/O per process.
    fn raw_disks(&self) -> Vec<RawDisk> {
        let mut disks: Vec<RawDisk> = self
            .disks
            .iter()
            .map(|disk| RawDisk {
                device: disk.name().to_string_lossy().into_owned(),
                mount: Some(disk.mount_point().to_string_lossy().into_owned()),
                total_bytes: disk.total_space(),
                used_bytes: disk.total_space().saturating_sub(disk.available_space()),
                read_bytes: 0,
                written_bytes: 0,
            })
            .collect();
        disks.push(RawDisk {
            device: "all".to_owned(),
            mount: None,
            total_bytes: 0,
            used_bytes: 0,
            read_bytes: self.disk_io.0,
            written_bytes: self.disk_io.1,
        });
        disks
    }

    fn refresh_processes(&mut self) {
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::All, true, process_detail());
        let (read, written) = self
            .system
            .processes()
            .values()
            .map(sysinfo::Process::disk_usage)
            .fold((0_u64, 0_u64), |(read, written), usage| {
                (
                    read.saturating_add(usage.read_bytes),
                    written.saturating_add(usage.written_bytes),
                )
            });
        self.disk_io = (
            self.disk_io.0.saturating_add(read),
            self.disk_io.1.saturating_add(written),
        );
    }

    fn raw_process(&self, pid: Pid, process: &sysinfo::Process, command_line: bool) -> RawProcess {
        let threads = process
            .tasks()
            .map_or(1, std::collections::HashSet::len)
            .max(1);
        let command = command_line
            .then(|| {
                process
                    .cmd()
                    .iter()
                    .map(|arg| arg.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|command| !command.trim().is_empty());
        RawProcess {
            pid: pid.as_u32(),
            ppid: process.parent().map(Pid::as_u32),
            started_at: process
                .start_time()
                .saturating_sub(self.boot_time)
                .saturating_mul(USER_HZ),
            name: process.name().to_string_lossy().into_owned(),
            state: state_code(process.status()),
            cpu_ticks: process.accumulated_cpu_time() / (1000 / USER_HZ),
            virtual_bytes: process.virtual_memory(),
            resident_bytes: process.memory(),
            threads: u32::try_from(threads).unwrap_or(u32::MAX),
            uid: process.user_id().map_or(u32::MAX, |uid| **uid),
            // Kernel work has no resident memory.
            kernel_thread: process.memory() == 0,
            cgroup: Some(ALL_PROCESSES.to_owned()),
            command,
        }
    }

    fn all_processes(&self, command_lines: bool) -> Vec<RawProcess> {
        self.system
            .processes()
            .iter()
            .map(|(pid, process)| self.raw_process(*pid, process, command_lines))
            .collect()
    }
}

/// What is read of each process: everything but its environment, which is
/// never read.
fn process_detail() -> ProcessRefreshKind {
    ProcessRefreshKind::everything().without_environ()
}

/// The `/proc` state letter the service reads for `status`.
fn state_code(status: ProcessStatus) -> char {
    match status {
        ProcessStatus::Run => 'R',
        ProcessStatus::Idle => 'I',
        ProcessStatus::Stop => 'T',
        ProcessStatus::Zombie => 'Z',
        ProcessStatus::Tracing => 't',
        ProcessStatus::Dead => 'X',
        ProcessStatus::UninterruptibleDiskSleep => 'D',
        _ => 'S',
    }
}

impl Probe for SysinfoProbe {
    fn read(&mut self, want: Want) -> RawSample {
        if want.processes || want.system {
            self.refresh_processes();
        }
        RawSample {
            system: want.system.then(|| self.system()),
            processes: want
                .processes
                .then(|| self.all_processes(want.command_lines)),
        }
    }

    fn process(&mut self, pid: u32) -> Option<RawProcess> {
        let pid = Pid::from_u32(pid);
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            process_detail(),
        );
        let process = self.system.process(pid)?;
        Some(self.raw_process(pid, process, false))
    }

    fn processes(&mut self) -> Vec<RawProcess> {
        self.refresh_processes();
        self.all_processes(false)
    }

    fn boot_time(&self) -> Option<u64> {
        Some(self.boot_time)
    }

    fn user_name(&mut self, uid: u32) -> String {
        let names: HashMap<u32, &str> = self
            .users
            .iter()
            .map(|user| (**user.id(), user.name()))
            .collect();
        names
            .get(&uid)
            .map_or_else(|| uid.to_string(), |name| (*name).to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_machine_reads_as_the_services_counters() {
        let mut probe = SysinfoProbe::new();
        let sample = probe.read(Want {
            system: true,
            processes: true,
            command_lines: true,
        });
        let system = sample.system.expect("a system reading");
        assert!(system.memory.total > 0, "this machine's memory");
        assert!(!system.cpu_cores.is_empty(), "this machine's cores");
        let processes = sample.processes.expect("a process list");
        let own = std::process::id();
        let me = processes
            .iter()
            .find(|process| process.pid == own)
            .expect("this test process is listed");
        assert_eq!(me.uid, rustix::process::geteuid().as_raw());
        assert_eq!(
            probe.process(own).map(|process| process.started_at),
            Some(me.started_at),
            "a second reading names the same lifetime"
        );
    }

    #[test]
    fn the_in_process_service_answers_with_this_machines_readings() {
        let client = serve("so.tilo.test").expect("the in-process service");
        let subscription = client.subscribe(250, true, None);
        let snapshot = subscription.wait().expect("a snapshot");
        assert!(snapshot.processes_included);
        assert!(
            snapshot
                .processes
                .iter()
                .any(|process| process.pid == std::process::id()),
            "this test process is listed"
        );
        assert!(
            client.end_process(u32::MAX, 0, false).is_err(),
            "no process of that number"
        );
    }
}
