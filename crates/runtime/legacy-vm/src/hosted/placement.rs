//! Which host runs a workspace's VM, when a Tilo host has more than one
//! machine for VMs, and how many machines it wants.
//!
//! A VM's disk, memory and snapshot live on the host that first woke it, so
//! a VM stays where it was placed: the host holding it runs it, and while
//! that host is down the VM waits for it. A VM placed for the first time goes
//! to the first host, in the hosts' order, with room for its size; the first
//! host is the one always up, so the others fill only when it is full.
//!
//! The hosts wanted keep room ahead of demand: one more once the VMs awake
//! take more than three quarters of the budget of the hosts up, or as soon as
//! a VM waits for room, or to bring back a host that holds a waiting VM. They
//! never shrink here: taking a host away leaves its VMs waiting for it, so
//! that is an operator's choice, not a load's.
//!
//! [`place`] and [`hosts_wanted`] decide; [`Placer`] keeps the record of
//! placements, asks each host's [`Request::Status`](super::control::Request)
//! and scales the hosts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::control::{self, HostLoad, Reply, Request, VmRequest};
use super::size::VmSize;

/// The share of the hosts' budget the awake VMs may take before another
/// host is wanted.
const HEADROOM_PERCENT: u64 = 75;

/// A host as the placer last saw it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostView {
    pub name: String,
    /// Up and answering.
    pub ready: bool,
    pub load: HostLoad,
}

/// Where a VM runs, or that it waits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Placement {
    Host(String),
    /// No host up has room for it, or the host holding it is down.
    Wait,
}

/// Where the VM `vm` of `size` runs: the host `placed` names for it, or,
/// placed nowhere yet, the first host up with room for it.
#[must_use]
pub fn place(hosts: &[HostView], placed: Option<&str>, size: VmSize) -> Placement {
    if let Some(holder) = placed {
        return match hosts.iter().find(|host| host.name == holder) {
            Some(host) if host.ready => Placement::Host(host.name.clone()),
            _ => Placement::Wait,
        };
    }
    hosts
        .iter()
        .find(|host| host.ready && host.load.free_mib() >= size.memory_mib())
        .map_or(Placement::Wait, |host| Placement::Host(host.name.clone()))
}

/// How many hosts are wanted, of at most `max`, with `hosts` the ones there
/// are (up or starting), `waiting_mib` what VMs waiting for room ask for, and
/// `holders` the hosts' indices that hold a waiting VM.
#[must_use]
pub fn hosts_wanted(hosts: &[HostView], waiting_mib: u64, holders: &[usize], max: usize) -> usize {
    let there = hosts.len();
    let mut wanted = there;
    if hosts.iter().any(|host| !host.ready) {
        // One starting is the room on its way: want no more until it is up.
        wanted = there;
    } else {
        let budget: u64 = hosts.iter().filter_map(|host| host.load.budget_mib).sum();
        let awake: u64 = hosts.iter().map(|host| host.load.awake_mib).sum();
        let waiting =
            waiting_mib.saturating_add(hosts.iter().map(|host| host.load.waiting_mib).sum());
        if waiting > 0 || awake.saturating_mul(100) > budget.saturating_mul(HEADROOM_PERCENT) {
            wanted = there.saturating_add(1);
        }
    }
    if let Some(&furthest) = holders.iter().max() {
        wanted = wanted.max(furthest.saturating_add(1));
    }
    wanted.clamp(1, max.max(1))
}

/// The hosts' count, as whatever runs them keeps it.
pub trait Scale: Send + Sync {
    fn hosts(&self) -> Result<usize, String>;
    fn set_hosts(&self, count: usize) -> Result<(), String>;
}

/// The placer's record: which host each VM was placed on, kept on disk so a
/// VM never moves while its host is away.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
struct Record {
    placed: BTreeMap<String, String>,
}

/// A VM waiting for room, as last asked for.
#[derive(Clone, Copy, Debug)]
struct Waiting {
    mib: u64,
    at_ms: u64,
}

/// Reads a host's load at an address, `None` when it does not answer.
pub type ReadLoad = Box<dyn Fn(&str) -> Option<HostLoad> + Send + Sync>;

/// How long a waiting VM counts as demand after it last asked.
const WAITING_FOR_MS: u64 = 15_000;

/// Places VMs on hosts named `<prefix><n>`, reading each host's status at
/// `<host><status_suffix>`, and scales them.
pub struct Placer {
    record_path: PathBuf,
    record: std::sync::Mutex<Record>,
    waiting: std::sync::Mutex<BTreeMap<String, Waiting>>,
    prefix: String,
    status_suffix: String,
    max: usize,
    scale: Box<dyn Scale>,
    read: ReadLoad,
    clock: super::vm::Clock,
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Placer {
    /// A placer keeping its record at `record_path`, with hosts
    /// `<prefix>0`, `<prefix>1`, … up to `max`, whose loads `read` reads.
    pub fn new(
        record_path: &Path,
        prefix: &str,
        max: usize,
        scale: Box<dyn Scale>,
        read: ReadLoad,
        clock: super::vm::Clock,
    ) -> Result<Self, String> {
        let record = match std::fs::read(record_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("{}: {e}", record_path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Record::default(),
            Err(e) => return Err(format!("{}: {e}", record_path.display())),
        };
        Ok(Self {
            record_path: record_path.to_path_buf(),
            record: std::sync::Mutex::new(record),
            waiting: std::sync::Mutex::new(BTreeMap::new()),
            prefix: prefix.to_owned(),
            status_suffix: String::new(),
            max,
            scale,
            read,
            clock,
        })
    }

    /// Read hosts' status at `<host><suffix>` (the address `read` is given).
    #[must_use]
    pub fn with_status_suffix(mut self, suffix: &str) -> Self {
        suffix.clone_into(&mut self.status_suffix);
        self
    }

    fn views(&self) -> Result<Vec<HostView>, String> {
        let count = self.scale.hosts()?;
        Ok((0..count)
            .map(|index| {
                let name = format!("{}{index}", self.prefix);
                let load = (self.read)(&format!("{name}{}", self.status_suffix));
                HostView {
                    ready: load.is_some(),
                    load: load.unwrap_or_default(),
                    name,
                }
            })
            .collect())
    }

    fn index_of(&self, host: &str) -> Option<usize> {
        host.strip_prefix(&self.prefix)?.parse().ok()
    }

    /// Answer where `vm` runs: [`Reply::Placed`], or [`Reply::Busy`] while it
    /// waits, scaling the hosts for it.
    pub fn place(&self, vm: &VmRequest) -> Reply {
        let id = format!("{}/{}", vm.workspace, vm.environment);
        let views = match self.views() {
            Ok(views) => views,
            Err(error) => return Reply::Error(format!("the hosts cannot be counted: {error}")),
        };
        let placed = {
            let record = lock(&self.record);
            record.placed.get(&id).cloned()
        };
        match place(&views, placed.as_deref(), vm.size) {
            Placement::Host(host) => {
                lock(&self.waiting).remove(&id);
                if placed.is_none() {
                    let record = {
                        let mut record = lock(&self.record);
                        record.placed.insert(id, host.clone());
                        record.clone()
                    };
                    let saved = self.save(&record);
                    if let Err(error) = saved {
                        return Reply::Error(error);
                    }
                }
                Reply::Placed { host }
            }
            Placement::Wait => {
                lock(&self.waiting).insert(
                    id,
                    Waiting {
                        mib: vm.size.memory_mib(),
                        at_ms: (self.clock)(),
                    },
                );
                self.rescale_with(&views);
                Reply::Busy(super::service::BUSY.to_owned())
            }
        }
    }

    /// Look at the hosts' loads and want as many hosts as they need.
    pub fn rescale(&self) {
        if let Ok(views) = self.views() {
            self.rescale_with(&views);
        }
    }

    fn rescale_with(&self, views: &[HostView]) {
        let now = (self.clock)();
        let (waiting_mib, holders) = {
            let mut waiting = lock(&self.waiting);
            waiting.retain(|_, wait| now.saturating_sub(wait.at_ms) <= WAITING_FOR_MS);
            let waits: Vec<(String, u64)> = waiting
                .iter()
                .map(|(id, wait)| (id.clone(), wait.mib))
                .collect();
            drop(waiting);
            let record = lock(&self.record);
            let holders: Vec<usize> = waits
                .iter()
                .filter_map(|(id, _)| record.placed.get(id))
                .filter_map(|host| self.index_of(host))
                .collect();
            let unplaced: u64 = waits
                .iter()
                .filter(|(id, _)| !record.placed.contains_key(id))
                .map(|(_, mib)| mib)
                .sum();
            drop(record);
            (unplaced, holders)
        };
        let wanted = hosts_wanted(views, waiting_mib, &holders, self.max);
        if wanted > views.len()
            && let Err(error) = self.scale.set_hosts(wanted)
        {
            eprintln!("tilo-legacy hosted place: scaling to {wanted} hosts: {error}");
        }
    }

    fn save(&self, record: &Record) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
        tilo_files::native::atomic::write_creating_parent(&self.record_path, &bytes)
            .map_err(|e| format!("{}: {e}", self.record_path.display()))
    }

    /// Answer one request on the placer's listener.
    pub fn handle(&self, request: Request) -> Reply {
        match request {
            Request::Place { vm } => self.place(&vm),
            _ => Reply::Error("a placer answers Place only".to_owned()),
        }
    }
}

/// A host's load, read from its status listener at `address`, or `None`
/// when it does not answer.
#[must_use]
pub fn read_load(address: &str) -> Option<HostLoad> {
    use std::net::ToSocketAddrs as _;
    let socket = address.to_socket_addrs().ok()?.next()?;
    let mut stream = std::net::TcpStream::connect_timeout(&socket, Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    control::FRAMING
        .write_value(&mut stream, &Request::Status)
        .ok()?;
    match control::FRAMING
        .read_value::<_, Reply>(&mut stream)
        .ok()??
    {
        Reply::Status { load, .. } => Some(load),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, reason = "small test sizes")]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    const GIB: u64 = 1024;

    fn host(name: &str, budget_gib: u64, awake_gib: u64) -> HostView {
        HostView {
            name: name.into(),
            ready: true,
            load: HostLoad {
                budget_mib: Some(budget_gib * GIB),
                awake_mib: awake_gib * GIB,
                waiting_mib: 0,
            },
        }
    }

    #[test]
    fn a_new_vm_goes_to_the_first_host_with_room_and_a_placed_one_stays() {
        let hosts = [host("vm-host-0", 12, 8), host("vm-host-1", 59, 0)];
        assert_eq!(
            place(&hosts, None, VmSize::Small),
            Placement::Host("vm-host-0".into()),
            "the warm host first while it has room"
        );
        assert_eq!(
            place(&hosts, None, VmSize::Default),
            Placement::Host("vm-host-1".into())
        );
        assert_eq!(
            place(&hosts, Some("vm-host-0"), VmSize::Default),
            Placement::Host("vm-host-0".into()),
            "a VM runs where its disk is, room or not: its own host makes it wait"
        );
        let down = [
            host("vm-host-0", 12, 0),
            HostView {
                ready: false,
                ..host("vm-host-1", 59, 0)
            },
        ];
        assert_eq!(
            place(&down, Some("vm-host-1"), VmSize::Small),
            Placement::Wait
        );
        assert_eq!(
            place(&[host("vm-host-0", 12, 12)], None, VmSize::Small),
            Placement::Wait
        );
    }

    #[test]
    fn a_host_is_wanted_past_three_quarters_or_for_any_wait_and_never_fewer() {
        let max = 3;
        assert_eq!(
            hosts_wanted(&[host("a", 12, 8)], 0, &[], max),
            1,
            "8 of 12 is under 75%"
        );
        assert_eq!(hosts_wanted(&[host("a", 12, 12)], 0, &[], max), 2);
        assert_eq!(
            hosts_wanted(&[host("a", 12, 0)], 4 * GIB, &[], max),
            2,
            "a VM waits"
        );
        assert_eq!(
            hosts_wanted(&[host("a", 12, 12), host("b", 59, 59)], 0, &[], max),
            3
        );
        assert_eq!(
            hosts_wanted(
                &[host("a", 12, 12), host("b", 59, 59), host("c", 59, 59)],
                0,
                &[],
                max
            ),
            3,
            "never past the most"
        );
        let starting = [
            host("a", 12, 12),
            HostView {
                ready: false,
                ..host("b", 0, 0)
            },
        ];
        assert_eq!(
            hosts_wanted(&starting, 8 * GIB, &[], max),
            2,
            "the one starting is the room"
        );
        assert_eq!(
            hosts_wanted(&[host("a", 12, 0)], 0, &[2], max),
            3,
            "bring back a holder"
        );
    }

    struct Hosts {
        count: Mutex<usize>,
        loads: Mutex<BTreeMap<String, HostLoad>>,
    }

    struct ByCount(Arc<Hosts>);

    impl Scale for ByCount {
        fn hosts(&self) -> Result<usize, String> {
            Ok(*self.0.count.lock().unwrap())
        }
        fn set_hosts(&self, count: usize) -> Result<(), String> {
            *self.0.count.lock().unwrap() = count;
            Ok(())
        }
    }

    #[test]
    fn the_placer_keeps_each_placement_and_starts_a_host_for_a_vm_that_waits() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = Arc::new(Hosts {
            count: Mutex::new(1),
            loads: Mutex::new(BTreeMap::from([(
                "vm-host-0".to_owned(),
                HostLoad {
                    budget_mib: Some(12 * GIB),
                    awake_mib: 0,
                    waiting_mib: 0,
                },
            )])),
        });
        let reader = Arc::clone(&hosts);
        let placer = || {
            let reader = Arc::clone(&reader);
            Placer::new(
                &dir.path().join("placements.json"),
                "vm-host-",
                3,
                Box::new(ByCount(Arc::clone(&hosts))),
                Box::new(move |name| reader.loads.lock().unwrap().get(name).copied()),
                Arc::new(|| 1_000),
            )
            .unwrap()
        };
        let request = |workspace: &str, size| VmRequest {
            workspace: workspace.into(),
            environment: "default".into(),
            account: "acct-1".into(),
            size,
        };
        let first = placer();
        assert_eq!(
            first.place(&request("ws1", VmSize::Default)),
            Reply::Placed {
                host: "vm-host-0".into()
            }
        );
        // phase: the warm host fills; a new VM waits, and a host is wanted for it
        hosts
            .loads
            .lock()
            .unwrap()
            .get_mut("vm-host-0")
            .unwrap()
            .awake_mib = 12 * GIB;
        assert_eq!(
            first.place(&request("ws2", VmSize::Small)),
            Reply::Busy("Starting another machine for it, about 3 minutes.".into())
        );
        assert_eq!(*hosts.count.lock().unwrap(), 2);
        // phase: the new host is up; the waiting VM goes there
        hosts.loads.lock().unwrap().insert(
            "vm-host-1".into(),
            HostLoad {
                budget_mib: Some(59 * GIB),
                awake_mib: 0,
                waiting_mib: 0,
            },
        );
        assert_eq!(
            first.place(&request("ws2", VmSize::Small)),
            Reply::Placed {
                host: "vm-host-1".into()
            }
        );
        // phase: a placer started again keeps both placements, room or not
        drop(first);
        let again = placer();
        hosts
            .loads
            .lock()
            .unwrap()
            .get_mut("vm-host-0")
            .unwrap()
            .awake_mib = 0;
        assert_eq!(
            again.place(&request("ws2", VmSize::Small)),
            Reply::Placed {
                host: "vm-host-1".into()
            }
        );
        assert_eq!(
            again.place(&request("ws1", VmSize::Default)),
            Reply::Placed {
                host: "vm-host-0".into()
            }
        );
    }
}
