//! Which host runs a workspace's VM, when a Tilo host has more than one machine for
//! VMs, and how many machines it wants. A VM stays where it was placed: its disk,
//! memory and snapshot live there, so while that host is down the VM waits for it.
//! Hosts are added when the awake VMs take more than three quarters of the budget,
//! when a VM waits for room, or to bring back a host that holds a waiting VM; they
//! are never removed here, since that is an operator's choice.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::control::{self, HostLoad, Reply, Request, VmRequest};
use super::size::VmSize;

/// The share of the hosts' budget the awake VMs may take before another
/// host is wanted.
const HEADROOM_PERCENT: u64 = 75;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostView {
    pub name: String,
    pub ready: bool,
    pub load: HostLoad,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Placement {
    Host(String),
    /// No host up has room for it, or the host holding it is down.
    Wait,
}

/// Where the VM runs: the host `placed` names for it, or, placed nowhere yet, the first host up
/// with room. A new VM needs one clone's worth ([`memory::CLONE_MIB`]) whatever its size, which
/// bounds the CPU and memory it may grow into; the host's own admission is the authority, and a
/// host with no room answers Busy, which is what starts another machine.
#[must_use]
pub fn place(hosts: &[HostView], placed: Option<&str>, _size: VmSize) -> Placement {
    if let Some(holder) = placed {
        return match hosts.iter().find(|host| host.name == holder) {
            Some(host) if host.ready => Placement::Host(host.name.clone()),
            _ => Placement::Wait,
        };
    }
    hosts
        .iter()
        .find(|host| host.ready && host.load.free_mib() >= super::memory::CLONE_MIB)
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

/// A host as the placer reaches it: the name a placement answers with and a members file lists, and
/// the address its Status answers at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Host {
    pub name: String,
    pub status: String,
}

impl Host {
    #[must_use]
    pub fn new(name: &str, status: &str) -> Self {
        Self {
            name: name.to_owned(),
            status: status.to_owned(),
        }
    }
}

/// The hosts there are, and how to start more of them.
pub trait Scale: Send + Sync {
    /// The hosts up, in the order new VMs fill them, each with where it
    /// answers.
    fn hosts(&self) -> Result<Vec<Host>, String>;
    /// Start hosts up to `count`, the first `count` of [`Scale::hosts`].
    fn set_hosts(&self, count: usize) -> Result<(), String>;
}

/// The placer's record: which host each VM was placed on, kept on disk so a
/// VM never moves while its host is away.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
struct Record {
    placed: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug)]
struct Waiting {
    mib: u64,
    at_ms: u64,
}

/// Reads a host's load at an address, `None` when it does not answer.
pub type ReadLoad = Box<dyn Fn(&str) -> Option<HostLoad> + Send + Sync>;

/// How long a waiting VM counts as demand after it last asked.
const WAITING_FOR_MS: u64 = 15_000;

/// Places VMs on the hosts [`Scale`] names, and scales them.
pub struct Placer {
    record_path: PathBuf,
    record: std::sync::Mutex<Record>,
    waiting: std::sync::Mutex<BTreeMap<String, Waiting>>,
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
    /// A placer keeping its record at `record_path`, on the hosts `scale`
    /// names, whose loads `read` reads, starting at most `max` of them.
    pub fn new(
        record_path: &Path,
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
            max,
            scale,
            read,
            clock,
        })
    }

    fn views(&self) -> Result<Vec<HostView>, String> {
        Ok(self
            .scale
            .hosts()?
            .into_iter()
            .map(|host| {
                let load = (self.read)(&host.status);
                HostView {
                    ready: load.is_some(),
                    load: load.unwrap_or_default(),
                    name: host.name,
                }
            })
            .collect())
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
                        mib: super::memory::CLONE_MIB,
                        at_ms: (self.clock)(),
                    },
                );
                self.rescale_with(&views);
                Reply::Busy(super::service::BUSY.to_owned())
            }
        }
    }

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
                .filter_map(|host| views.iter().position(|view| view.name == *host))
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

    pub fn handle(&self, request: Request) -> Reply {
        match request {
            Request::Place { vm } => self.place(&vm),
            _ => Reply::Error("a placer answers Place only".to_owned()),
        }
    }
}

/// The hosts a stack runs, named by hand with where each answers: a stack that knows its machines
/// places on them and starts none. A stateful set starts hosts instead.
pub struct FixedHosts(Vec<Host>);

impl FixedHosts {
    /// The hosts `text` names, written `NAME=ADDR[,NAME=ADDR]`.
    ///
    /// # Errors
    ///
    /// A host is not `NAME=ADDR`, or none is named.
    pub fn parse(text: &str) -> Result<Self, String> {
        let hosts = text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| {
                let (name, status) = part
                    .split_once('=')
                    .ok_or_else(|| format!("{part:?} is not NAME=ADDR"))?;
                let (name, status) = (name.trim(), status.trim());
                if name.is_empty() || status.is_empty() {
                    return Err(format!("{part:?} is not NAME=ADDR"));
                }
                Ok(Host::new(name, status))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if hosts.is_empty() {
            return Err("no host is named".to_owned());
        }
        Ok(Self(hosts))
    }
}

impl Scale for FixedHosts {
    fn hosts(&self) -> Result<Vec<Host>, String> {
        Ok(self.0.clone())
    }

    fn set_hosts(&self, count: usize) -> Result<(), String> {
        if count > self.0.len() {
            eprintln!(
                "tilo-legacy hosted place: {} hosts are named here; not starting {count}",
                self.0.len()
            );
        }
        Ok(())
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

    use super::super::memory::CLONE_MIB;
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
            Placement::Host("vm-host-0".into()),
            "a VM is placed by what it takes now, not by what it may grow into"
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
            Placement::Wait,
            "a host whose VMs take all its memory places nothing more"
        );
        let nearly = [HostView {
            load: HostLoad {
                budget_mib: Some(12 * GIB),
                awake_mib: 12 * GIB - CLONE_MIB,
                waiting_mib: 0,
            },
            ..host("vm-host-0", 12, 0)
        }];
        assert_eq!(
            place(&nearly, None, VmSize::Xlarge),
            Placement::Host("vm-host-0".into())
        );
    }

    #[test]
    fn hundreds_of_workspaces_fill_a_fleet_by_what_they_take() {
        // The customer's scale: 300 workspaces, each holding a clone's worth,
        // placed across the nodes a stack runs them on.
        let mut hosts: Vec<HostView> = (0..8)
            .map(|n| HostView {
                name: format!("vm-host-{n}"),
                ready: true,
                load: HostLoad {
                    budget_mib: Some(62 * GIB),
                    awake_mib: 0,
                    waiting_mib: 0,
                },
            })
            .collect();
        for workspace in 0..300 {
            let Placement::Host(name) = place(&hosts, None, VmSize::Small) else {
                panic!("workspace {workspace} waits with eight nodes up");
            };
            let host = hosts
                .iter_mut()
                .find(|host| host.name == name)
                .expect("the host it was placed on");
            host.load.awake_mib = host.load.awake_mib.saturating_add(CLONE_MIB);
        }
        let used = hosts.iter().filter(|host| host.load.awake_mib > 0).count();
        assert_eq!(used, 2, "300 workspaces take two nodes, not eighty");
        for host in &hosts {
            assert!(host.load.awake_mib <= 62 * GIB, "{}", host.name);
        }
        assert_eq!(
            hosts.first().map(|host| host.load.free_mib()),
            Some(62 * GIB - 195 * CLONE_MIB),
            "the placer reads what a host really has left"
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
        fn hosts(&self) -> Result<Vec<Host>, String> {
            Ok((0..*self.0.count.lock().unwrap())
                .map(|index| Host::new(&format!("vm-host-{index}"), &format!("vm-host-{index}")))
                .collect())
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

    #[test]
    fn a_stack_of_named_hosts_is_placed_on_and_never_grown() {
        let dir = tempfile::tempdir().unwrap();
        let loads = Arc::new(Mutex::new(BTreeMap::from([(
            "127.0.0.1:7701".to_owned(),
            HostLoad {
                budget_mib: Some(4 * GIB),
                awake_mib: 0,
                waiting_mib: 0,
            },
        )])));
        let reader = Arc::clone(&loads);
        let placer = Placer::new(
            &dir.path().join("placements.json"),
            1,
            Box::new(FixedHosts::parse("vm-host-0=127.0.0.1:7701").unwrap()),
            Box::new(move |address| reader.lock().unwrap().get(address).copied()),
            Arc::new(|| 1_000),
        )
        .unwrap();
        let request = |workspace: &str, size| VmRequest {
            workspace: workspace.into(),
            environment: "default".into(),
            account: "acct-1".into(),
            size,
        };
        assert_eq!(
            placer.place(&request("ws1", VmSize::Small)),
            Reply::Placed {
                host: "vm-host-0".into()
            },
            "the host the stack names answers, at the address it named"
        );
        loads
            .lock()
            .unwrap()
            .get_mut("127.0.0.1:7701")
            .unwrap()
            .awake_mib = 4 * GIB;
        assert_eq!(
            placer.place(&request("ws2", VmSize::Small)),
            Reply::Busy("Starting another machine for it, about 3 minutes.".into())
        );
        assert_eq!(
            FixedHosts::parse("vm-host-0=127.0.0.1:7701")
                .unwrap()
                .hosts()
                .unwrap(),
            [Host::new("vm-host-0", "127.0.0.1:7701")]
        );
        for wrong in ["vm-host-0", "=127.0.0.1:7701", "vm-host-0=", ""] {
            assert!(FixedHosts::parse(wrong).is_err(), "{wrong:?} names a host");
        }
    }
}
