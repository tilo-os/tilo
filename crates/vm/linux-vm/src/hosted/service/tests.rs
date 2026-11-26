//! The hosted-VM service's tests: fake VMs and their Firecracker APIs, and the
//! service's answers, in process.

use std::collections::BTreeSet;
use std::io::{Read as _, Write as _};

use super::super::firecracker::Machine;
use super::super::memory::{BALLOON_STEP_MIB, CLONE_MIB};
use super::super::meter::{OpenRun, RunEnd, UsageKind};
use super::*;
use tilo_server_protocol::hosted_vm::VmSize;

fn config(dir: &Path) -> ServiceConfig {
    ServiceConfig {
        state: dir.join("state"),
        run: dir.join("run"),
        plans: dir.join("plans.json"),
        settings: Settings {
            machine: Machine {
                // Never reached: every wake here is refused first.
                firecracker: dir.join("no-firecracker"),
                kernel: dir.join("no-kernel"),
                initramfs: None,
                cmdline: Machine::DEFAULT_CMDLINE.into(),
                base_disk: None,
            },
            idle_suspend: Duration::from_secs(30),
            self_exe: dir.join("tilo-linux-vm"),
            jail: None,
            server: None,
            templates: None,
        },
    }
}

fn request(workspace: &str, account: &str, size: VmSize) -> VmRequest {
    VmRequest {
        workspace: workspace.into(),
        environment: "default".into(),
        account: account.into(),
        size,
    }
}

fn clock(at: u64) -> Clock {
    Arc::new(move || at)
}

/// A host whose kernel measures nothing and has memory to spare: a test
/// that is not about admission says so, and does not depend on the
/// machine it runs on.
fn open_at(config: &ServiceConfig, at: u64) -> Service {
    Service::open(config.clone(), clock(at))
        .unwrap()
        .with_measure(Arc::new(|_: &HostedVm| None), Arc::new(|| Some(1 << 20)))
}

/// Fake VMs, and the host's memory that follows them: what each one's guest holds, what its
/// Firecracker API took for its balloon, and what the service asked for.
#[derive(Default)]
struct World {
    resident: Mutex<BTreeMap<String, u64>>,
    balloon: Mutex<BTreeMap<String, u64>>,
    asked: Mutex<Vec<(String, u64)>>,
    /// VMs whose Firecracker has no balloon device: their API answers as one booted or restored
    /// without the device does.
    no_balloon: Mutex<BTreeSet<String>>,
}

impl World {
    fn takes_mib(&self, id: &str) -> Option<u64> {
        let held = *self.resident.lock().unwrap().get(id)?;
        let back = *self.balloon.lock().unwrap().get(id).unwrap_or(&0);
        Some(held.saturating_sub(back))
    }

    fn used_mib(&self) -> u64 {
        let ids: Vec<String> = self.resident.lock().unwrap().keys().cloned().collect();
        ids.iter().filter_map(|id| self.takes_mib(id)).sum()
    }

    fn fill(&self, id: &str, mib: u64) {
        self.resident.lock().unwrap().insert(id.to_owned(), mib);
    }

    /// `id`'s VM sleeps: the host has its memory back (its snapshot is a file on disk).
    fn sleep(&self, id: &str) {
        self.resident.lock().unwrap().remove(id);
        self.balloon.lock().unwrap().remove(id);
    }

    fn asked(&self) -> Vec<(String, u64)> {
        self.asked.lock().unwrap().clone()
    }
}

fn fake_host(dir: &Path, node_mib: u64) -> (Service, Arc<World>) {
    let world = Arc::new(World::default());
    let weighed = Arc::clone(&world);
    let freed = Arc::clone(&world);
    let service = Service::open(config(dir), clock(0))
        .unwrap()
        .with_memory(Budget {
            node_mib: Some(node_mib),
            ..Budget::default()
        })
        .with_measure(
            Arc::new(move |vm: &HostedVm| weighed.takes_mib(&vm.identity().vm)),
            Arc::new(move || Some(node_mib.saturating_sub(freed.used_mib()))),
        );
    (service, world)
}

/// Admit `workspace`'s fake VM whose guest holds `mib`, with a Firecracker API at its socket.
fn fake_vm(
    service: &Service,
    world: &Arc<World>,
    config: &ServiceConfig,
    workspace: &str,
    mib: u64,
    balloon: bool,
) -> String {
    let entry = service
        .admit(
            &request(workspace, &format!("acct-{workspace}"), VmSize::Small),
            true,
        )
        .unwrap();
    let id = entry.id();
    world.fill(&id, mib);
    if !balloon {
        world.no_balloon.lock().unwrap().insert(id.clone());
    }
    let files = config.files(workspace, "default");
    std::fs::create_dir_all(&files.run).unwrap();
    serve_api(&files.api_socket(), Arc::clone(world), id.clone());
    id
}

/// A fake Firecracker's API at `socket`, answering the balloon calls the service makes. One
/// whole HTTP request: the client writes its head and its body in one go or not, so read until
/// the body is all there.
fn read_request(stream: &mut std::os::unix::net::UnixStream) -> String {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).unwrap_or(0);
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&request).into_owned();
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            continue;
        };
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        if body.len() >= length {
            return text;
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}

fn serve_api(socket: &Path, world: Arc<World>, id: String) {
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let request = read_request(&mut stream);
            let reply = if request.starts_with("PATCH /balloon") {
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or_default();
                let asked: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
                let target = asked["amount_mib"].as_u64().unwrap_or(0);
                world.asked.lock().unwrap().push((id.clone(), target));
                if world.no_balloon.lock().unwrap().contains(&id) {
                    "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n"
                } else {
                    world.balloon.lock().unwrap().insert(id.clone(), target);
                    "HTTP/1.1 204 No Content\r\n\r\n"
                }
            } else {
                "HTTP/1.1 204 No Content\r\n\r\n"
            };
            let _ = stream.write_all(reply.as_bytes());
        }
    });
}

/// A VM the account already has, as a meter on disk.
fn existing(config: &ServiceConfig, workspace: &str, account: &str, open: Option<OpenRun>) {
    let files = config.files(workspace, "default");
    let mut meter = Meter::new(VmIdentity {
        vm: format!("{workspace}/default"),
        workspace: workspace.into(),
        environment: "default".into(),
        account: account.into(),
        size: VmSize::Small,
    });
    meter.open = open;
    meter.save(&files.meter()).unwrap();
}

#[test]
fn a_restarted_service_closes_the_runs_it_lost() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    existing(
        &config,
        "ws1",
        "acct-1",
        Some(OpenRun {
            start_ms: 1_000,
            last_seen_ms: 61_500,
            billed_from_ms: Some(1_500),
        }),
    );
    let service = open_at(&config, 90_000);
    let records = service.log.records("acct-1").unwrap();
    assert_eq!(records.len(), 1);
    let record = records.first().unwrap();
    assert_eq!(
        (record.kind, record.ended, record.end_ms, record.seconds),
        (UsageKind::Running, Some(RunEnd::Recovered), 61_500, 60)
    );
    drop(service);
    let again = open_at(&config, 95_000);
    assert_eq!(
        again.log.records("acct-1").unwrap().len(),
        1,
        "a recovered run is closed once"
    );
    let Reply::Usage { by_size, .. } = again.handle(Request::Usage {
        account: "acct-1".into(),
        from_ms: 0,
        to_ms: u64::MAX,
    }) else {
        panic!("usage");
    };
    assert_eq!(by_size.first().map(|usage| usage.running_seconds), Some(60));
}

#[test]
fn a_host_being_taken_back_wakes_nothing_and_keeps_its_vms_for_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    existing(&config, "ws1", "acct-1", None);
    let service = open_at(&config, 0);
    assert!(!service.evicting());
    service.evict();
    assert!(service.evicting());
    let Reply::Error(message) = service.handle(Request::Ensure {
        vm: request("ws1", "acct-1", VmSize::Small),
        shares: Vec::new(),
    }) else {
        panic!("a host being taken back wakes nothing");
    };
    assert_eq!(
        message,
        "the workspace's Linux VM is moving to another machine, saved as it was; \
         run the command again in a minute"
    );
    assert!(
        config.files("ws1", "default").meter().is_file(),
        "the VM stays where the next host finds it"
    );
    let next = ServiceConfig {
        run: dir.path().join("run-of-the-next-host"),
        ..config
    };
    let elsewhere = open_at(&next, 10_000);
    assert!(!elsewhere.evicting());
    assert_eq!(
        elsewhere
            .status()
            .iter()
            .map(|vm| (vm.vm.as_str(), vm.account.as_str()))
            .collect::<Vec<_>>(),
        [("ws1/default", "acct-1")]
    );
}

#[test]
fn a_host_admits_by_what_its_vms_take_not_by_what_they_are_configured_at() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let (service, world) = fake_host(dir.path(), 4 * 1024);
    for n in 0..5 {
        fake_vm(
            &service,
            &world,
            &config,
            &format!("ws{n}"),
            CLONE_MIB,
            true,
        );
        service.tick();
    }
    assert_eq!(
        service.load(),
        HostLoad {
            budget_mib: Some(2 * 1024),
            awake_mib: 5 * CLONE_MIB,
            waiting_mib: 0,
        },
        "what the VMs take, not what they may grow into"
    );
    assert!(matches!(
        service.admit(&request("ws5", "acct-ws5", VmSize::Small), true),
        Err(EnsureError::HostFull)
    ));
    let Reply::Busy(words) = service.handle(Request::Ensure {
        vm: request("ws5", "acct-ws5", VmSize::Small),
        shares: Vec::new(),
    }) else {
        panic!("a full host makes the wake wait");
    };
    assert_eq!(words, "The machine is full until one of its VMs goes idle.");
    assert_eq!(
        service.load().waiting_mib,
        453,
        "what a VM is expected to take"
    );
    service
        .admit(&request("ws0", "acct-ws0", VmSize::Small), true)
        .unwrap();
    service
        .entry("ws1", "default")
        .unwrap()
        .awake
        .store(false, Ordering::SeqCst);
    world.sleep("ws1/default");
    service
        .admit(&request("ws5", "acct-ws5", VmSize::Small), true)
        .unwrap();
    assert_eq!(service.load().waiting_mib, 0, "a woken VM waits no more");
}

#[test]
fn a_host_short_of_memory_balloons_its_vms_before_it_refuses_one_and_kills_none() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let (service, world) = fake_host(dir.path(), 4 * 1024);
    let mut ids = Vec::new();
    for n in 0..5 {
        ids.push(fake_vm(
            &service,
            &world,
            &config,
            &format!("ws{n}"),
            CLONE_MIB,
            true,
        ));
        service.tick();
    }
    assert_eq!(
        world.asked(),
        vec![],
        "room to spare asks no guest for memory"
    );
    for id in &ids {
        world.fill(id, 900);
    }
    service.tick();
    assert_eq!(
        world.used_mib(),
        5 * (900 - BALLOON_STEP_MIB),
        "each guest gave a step of its memory back"
    );
    for id in &ids {
        world.fill(id, 4096);
    }
    for _ in 0..10 {
        service.tick();
    }
    assert!(
        world
            .balloon
            .lock()
            .unwrap()
            .values()
            .all(|held| *held == 2 * 1024),
        "every balloon holds half a small VM, and no more"
    );
    assert!(matches!(
        service.admit(&request("ws9", "acct-ws9", VmSize::Small), true),
        Err(EnsureError::HostFull)
    ));
    assert_eq!(service.status().len(), 5, "every workspace keeps its VM");
    for n in 0..5 {
        assert!(
            service
                .entry(&format!("ws{n}"), "default")
                .unwrap()
                .awake
                .load(Ordering::SeqCst),
            "ws{n}'s VM still runs"
        );
    }
    for id in &ids {
        world.fill(id, 2 * 1024);
    }
    service.tick();
    assert!(
        world.used_mib() < CLONE_MIB,
        "the balloons hold what the guests no longer need: {} MiB in use",
        world.used_mib()
    );
    service
        .admit(&request("ws9", "acct-ws9", VmSize::Small), true)
        .unwrap();
}

#[test]
fn a_template_is_counted_at_the_memory_its_guest_warms() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let (service, world) = fake_host(dir.path(), 4 * 1024);
    fake_vm(&service, &world, &config, "ws1", CLONE_MIB, true);
    service.tick();
    assert!(service.room_for(service.budget.expected_mib(), ""));
    assert!(!service.room_for(VmSize::Small.memory_mib(), ""));
    service
        .building_mib
        .fetch_add(VmSize::Small.memory_mib(), Ordering::SeqCst);
    assert_eq!(
        service.load().awake_mib,
        CLONE_MIB + VmSize::Small.memory_mib()
    );
    assert!(!service.room_for(service.budget.expected_mib(), ""));
}

#[test]
fn a_vm_whose_firecracker_has_no_balloon_is_asked_once_and_never_killed_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let (service, world) = fake_host(dir.path(), 4 * 1024);
    let plain = fake_vm(&service, &world, &config, "ws1", CLONE_MIB, true);
    let bare = fake_vm(&service, &world, &config, "ws2", CLONE_MIB, false);
    world.fill(&plain, 4096);
    world.fill(&bare, 4096);
    service.tick();
    let asked = world.asked();
    assert!(
        asked
            .iter()
            .any(|(id, mib)| id == &plain && *mib == BALLOON_STEP_MIB),
        "{asked:?}"
    );
    assert!(
        asked.iter().any(|(id, _)| id == &bare),
        "the VM without a balloon was asked once: {asked:?}"
    );
    for _ in 0..5 {
        service.tick();
    }
    let bare_asks = world.asked().iter().filter(|(id, _)| id == &bare).count();
    assert_eq!(
        bare_asks, 1,
        "a balloon that is not there is not asked twice"
    );
    assert_eq!(service.status().len(), 2);
}

#[test]
fn a_workspace_without_a_card_gets_its_vm_for_browser_and_no_linux_action() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    std::fs::write(
        &config.plans,
        r#"{"accounts":{"acct-card":{"max_running":2,"max_vms":2,"sizes":["small"]}},"sells":{"host":"tilo.sh","plans":[],
            "add_payment_method":"https://tilo.sh/card","browser_free_actions":300}}"#,
    )
    .unwrap();
    let service = open_at(&config, 0);
    service
        .admit(&request("ws1", "acct-free", VmSize::Small), false)
        .unwrap();
    let Reply::Refused(refusal) = service.handle(Request::Ensure {
        vm: request("ws1", "acct-free", VmSize::Small),
        shares: Vec::new(),
    }) else {
        panic!("a Linux action without a card is refused");
    };
    assert_eq!(refusal.reason, server_account::Reason::NoPaymentMethod);
    assert_eq!(
        service.status().len(),
        1,
        "one VM for the workspace, which Browser and Linux share"
    );
    service
        .admit(&request("ws2", "acct-card", VmSize::Small), true)
        .unwrap();
}

#[test]
fn a_linux_app_without_a_card_is_refused_before_anything_starts() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.settings.server =
        Some(super::super::network::Endpoint::parse("127.0.0.1:7704").unwrap());
    std::fs::write(
        &config.plans,
        r#"{"accounts":{},"sells":{"host":"tilo.sh","plans":[],
            "add_payment_method":"https://tilo.sh/card","browser_free_actions":300}}"#,
    )
    .unwrap();
    let service = open_at(&config, 0);
    let Reply::Refused(refusal) = service.handle(Request::OpenApp {
        app: OpenApp {
            workspace: "ws1".into(),
            account: "acct-free".into(),
            size: VmSize::Small,
            material: browser::test_material("127.0.0.1:7704"),
        },
    }) else {
        panic!("a Linux app without a card is refused");
    };
    assert_eq!(refusal.reason, server_account::Reason::NoPaymentMethod);
    assert!(
        refusal
            .next
            .iter()
            .any(|next| next.kind == server_account::NextStepKind::AddPaymentMethod),
        "the refusal says what lets it through: {refusal:?}"
    );
    assert!(
        service.status().iter().all(|vm| vm.phase != "Running"),
        "nothing woke for it"
    );
}

#[test]
fn a_workspace_is_handed_the_address_its_daemon_dials_the_server_at() {
    use super::super::network::Endpoint;

    let dir = tempfile::tempdir().unwrap();
    let with_server = |server: &str| {
        let mut config = config(dir.path());
        config.settings.server = Some(Endpoint::parse(server).unwrap());
        open_at(&config, 0)
    };
    let dialled = |material: &tilo_server_protocol::hosted::Material| -> std::net::SocketAddr {
        assert_eq!(
            material.ticket.issuer.relay, None,
            "a VM dials its host directly"
        );
        let [socket] = material.ticket.issuer.direct[..] else {
            panic!("one address: {:?}", material.ticket.issuer.direct);
        };
        socket
    };
    let local = with_server("127.0.0.1:7704");
    let line = local
        .hand_off(&browser::test_material("127.0.0.1:7704"))
        .unwrap();
    assert_eq!(
        dialled(&line),
        "10.0.2.4:7704".parse().unwrap(),
        "the VM dials the mapped loopback, not the host's own"
    );
    let hosted = with_server("10.0.0.9:7704");
    let line = hosted
        .hand_off(&browser::test_material("203.0.113.9:7704"))
        .unwrap();
    assert_eq!(dialled(&line), "10.0.0.9:7704".parse().unwrap());
    // and a placement the daemon could not log in with refuses the open
    assert!(matches!(
        local.hand_off("{}"),
        Err(BrowserError::Invalid(_))
    ));
}

#[test]
fn a_host_without_the_servers_endpoint_serves_no_browser() {
    let dir = tempfile::tempdir().unwrap();
    let service = open_at(&config(dir.path()), 0);
    let Reply::Error(message) = service.handle(Request::OpenBrowser {
        browser: OpenBrowser {
            workspace: "ws1".into(),
            account: "acct-1".into(),
            size: VmSize::Small,
            store: None,
            material: browser::test_material("127.0.0.1:7704"),
        },
    }) else {
        panic!("a host with no server endpoint opens nothing");
    };
    assert_eq!(
        message,
        "this host serves no Browser: it was started without the server's endpoint"
    );
    assert!(service.status().is_empty(), "and woke nothing");
    assert_eq!(
        service.handle(Request::StopBrowser {
            workspace: "ws1".into()
        }),
        Reply::Done,
        "stopping what is not open is nothing to do"
    );
}

#[test]
fn wakes_beyond_the_plan_are_refused_in_plain_words() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    std::fs::write(
        &config.plans,
        r#"{"default":{"max_running":1,"max_vms":2,"sizes":["small","default"]}}"#,
    )
    .unwrap();
    existing(&config, "ws1", "acct-1", None);
    existing(&config, "ws2", "acct-1", None);
    existing(&config, "ws9", "acct-2", None);
    let service = open_at(&config, 0);
    let refused = |request: &VmRequest| match service.handle(Request::Ensure {
        vm: request.clone(),
        shares: Vec::new(),
    }) {
        Reply::Refused(refusal) => refusal.message,
        other => panic!("expected a refusal, got {other:?}"),
    };
    {
        let message = refused(&request("ws1", "acct-1", VmSize::Large));
        assert!(
            message.starts_with("This workspace's Linux VM is large"),
            "{message}"
        );
    }
    {
        let message = refused(&request("ws3", "acct-1", VmSize::Small));
        assert!(
            message.starts_with("Your plan keeps at most 2 Linux VMs"),
            "{message}"
        );
        assert!(
            !dir.path().join("state/vms/ws3").exists(),
            "nothing was created"
        );
    }
    {
        lock(&service.vms)
            .get("ws1/default")
            .unwrap()
            .awake
            .store(true, Ordering::SeqCst);
        let message = refused(&request("ws2", "acct-1", VmSize::Small));
        assert!(
            message.starts_with(
                "Your plan runs at most 1 Linux VM at a time, and 1 is running now: ws1 (small)."
            ),
            "{message}"
        );
    }
    {
        let reply = service.handle(Request::Ensure {
            vm: request("ws9", "acct-1", VmSize::Small),
            shares: Vec::new(),
        });
        assert!(
            matches!(&reply, Reply::Error(message) if message.contains("belongs to another account")),
            "{reply:?}"
        );
    }
    {
        let reply = service.handle(Request::Ensure {
            vm: request("../etc", "acct-1", VmSize::Small),
            shares: Vec::new(),
        });
        assert!(matches!(reply, Reply::Error(_)), "{reply:?}");
    }
}

/// A VM `account` keeps at `size`, off: its disk holds `disk`, and its Firecracker directory
/// also holds each of `beside`.
fn with_disk(
    config: &ServiceConfig,
    workspace: &str,
    account: &str,
    size: VmSize,
    disk: &[u8],
    beside: &[&str],
) -> VmFiles {
    let files = config.files(workspace, "default");
    Meter::new(VmIdentity {
        vm: format!("{workspace}/default"),
        workspace: workspace.into(),
        environment: "default".into(),
        account: account.into(),
        size,
    })
    .save(&files.meter())
    .unwrap();
    std::fs::create_dir_all(files.vmm()).unwrap();
    std::fs::write(files.disk(), disk).unwrap();
    for name in beside {
        std::fs::write(files.vmm().join(name), name.as_bytes()).unwrap();
    }
    files
}

fn branch(service: &Service, workspace: &str, account: &str, size: VmSize, from: &str) -> Reply {
    service.handle(Request::Branch {
        vm: request(workspace, account, size),
        from: from.into(),
    })
}

fn delete(service: &Service, workspace: &str) -> Reply {
    service.handle(Request::Delete {
        workspace: workspace.into(),
        environment: "default".into(),
    })
}

fn head(path: &Path, len: usize) -> Vec<u8> {
    use std::os::unix::fs::FileExt as _;
    let mut bytes = vec![0_u8; len];
    std::fs::File::open(path)
        .unwrap()
        .read_exact_at(&mut bytes, 0)
        .unwrap();
    bytes
}

fn held(service: &Service) -> Vec<(String, String, String)> {
    service
        .status()
        .into_iter()
        .map(|vm| (vm.vm, vm.account, vm.phase))
        .collect()
}

fn error(reply: Reply) -> String {
    match reply {
        Reply::Error(message) => message,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn a_branch_is_its_sources_disk_in_a_vm_of_its_own_that_boots_cold() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let source = with_disk(
        &config,
        "ws1",
        "acct-1",
        VmSize::Small,
        b"the tenant's tools",
        &["vm.mem", "firecracker.json"],
    );
    let service = open_at(&config, 0);
    assert_eq!(
        branch(&service, "ws2", "acct-1", VmSize::Default, "ws1"),
        Reply::Done
    );

    let made = config.files("ws2", "default");
    assert_eq!(head(&made.disk(), 18), b"the tenant's tools");
    assert_eq!(
        std::fs::metadata(made.disk()).unwrap().len(),
        VmSize::Default.disk_bytes(),
        "grown to the size it was asked at"
    );
    assert!(!made.snapshot().memory.exists(), "no memory of the source");
    assert!(
        !made.snapshot().state.exists(),
        "no vCPU state of the source"
    );
    assert!(
        !made.config().exists(),
        "it boots from a configuration of its own"
    );
    assert!(
        made.branched().is_file(),
        "its guest forgets the source's sessions at its first wake"
    );
    let meter = Meter::load(&made.meter()).unwrap();
    assert_eq!(
        (meter.identity.account.as_str(), meter.identity.size),
        ("acct-1", VmSize::Default)
    );
    assert_eq!(
        held(&service),
        [
            ("ws1/default".into(), "acct-1".into(), "Off".into()),
            ("ws2/default".into(), "acct-1".into(), "Off".into()),
        ]
    );
    assert_eq!(std::fs::read(source.disk()).unwrap(), b"the tenant's tools");
    assert!(source.snapshot().memory.is_file() && source.config().is_file());
    drop(service);
    assert_eq!(
        held(&open_at(&config, 1_000)).len(),
        2,
        "the branch is a VM the host keeps"
    );
}

#[test]
fn a_branch_of_a_workspace_without_a_vm_makes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    existing(&config, "ws-unbooted", "acct-1", None);
    let service = open_at(&config, 0);
    for from in ["ws-none", "ws-unbooted"] {
        assert_eq!(
            branch(&service, "ws2", "acct-1", VmSize::Small, from),
            Reply::NoVm {
                workspace: from.into()
            },
            "said in a way the server tells apart"
        );
        assert!(
            !config.state.join("vms").join("ws2").exists(),
            "nothing was made"
        );
    }
    assert_eq!(
        held(&service),
        [(
            "ws-unbooted/default".into(),
            "acct-1".into(),
            "NotCreated".into()
        )]
    );
    let message = error(branch(&service, "ws2", "acct-1", VmSize::Small, "../ws1"));
    assert!(message.contains("is not a workspace id"), "{message}");
}

#[test]
fn a_branch_is_refused_onto_a_vm_for_another_account_or_smaller_than_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    with_disk(&config, "ws1", "acct-1", VmSize::Small, b"one", &[]);
    let kept = with_disk(&config, "ws2", "acct-1", VmSize::Small, b"two", &[]);
    with_disk(&config, "ws9", "acct-2", VmSize::Default, b"nine", &[]);
    let service = open_at(&config, 0);
    let message = error(branch(&service, "ws2", "acct-1", VmSize::Small, "ws1"));
    assert!(message.contains("ws2 already has a VM"), "{message}");
    assert_eq!(std::fs::read(kept.disk()).unwrap(), b"two");
    assert!(!kept.branched().exists());

    let message = error(branch(&service, "ws3", "acct-1", VmSize::Default, "ws9"));
    assert!(message.contains("belongs to another account"), "{message}");
    assert!(!config.files("ws3", "default").state.exists());

    let message = error(branch(&service, "ws3", "acct-2", VmSize::Small, "ws9"));
    assert!(
        message.contains("is default, and a branch of it is no smaller"),
        "{message}"
    );
    assert!(!config.files("ws3", "default").state.exists());
    assert_eq!(service.status().len(), 3);
}

#[test]
fn a_branch_counts_toward_the_accounts_vms_and_a_delete_gives_its_room_back() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    std::fs::write(
        &config.plans,
        r#"{"default":{"max_running":1,"max_vms":2,"sizes":["small","default"]}}"#,
    )
    .unwrap();
    with_disk(&config, "ws1", "acct-1", VmSize::Small, b"one", &[]);
    with_disk(&config, "ws2", "acct-1", VmSize::Small, b"two", &[]);
    let service = open_at(&config, 0);
    let Reply::Refused(refusal) = branch(&service, "ws3", "acct-1", VmSize::Small, "ws1") else {
        panic!("a branch past the plan's VMs is refused");
    };
    assert_eq!(
        refusal.reason,
        server_account::Reason::TooManyVms {
            limit: 2,
            existing: 2
        }
    );
    assert!(
        refusal
            .message
            .starts_with("Your plan keeps at most 2 Linux VMs"),
        "{}",
        refusal.message
    );
    assert!(!config.files("ws3", "default").state.exists());
    let Reply::Refused(refusal) = branch(&service, "ws3", "acct-1", VmSize::Large, "ws1") else {
        panic!("a size the plan does not include is refused");
    };
    assert!(matches!(
        refusal.reason,
        server_account::Reason::SizeNotInPlan { .. }
    ));

    assert_eq!(delete(&service, "ws2"), Reply::Done);
    assert_eq!(
        branch(&service, "ws3", "acct-1", VmSize::Small, "ws1"),
        Reply::Done
    );
    assert_eq!(
        held(&service)
            .into_iter()
            .map(|(vm, ..)| vm)
            .collect::<Vec<_>>(),
        ["ws1/default", "ws3/default"]
    );
}

#[test]
fn a_branch_whose_copy_failed_leaves_no_vm() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let source = with_disk(&config, "ws1", "acct-1", VmSize::Small, b"one", &[]);
    let service = open_at(&config, 0);
    // The disk goes after the host saw it: the copy fails half-way.
    std::fs::remove_file(source.disk()).unwrap();
    let message = error(branch(&service, "ws2", "acct-1", VmSize::Small, "ws1"));
    assert!(
        message.starts_with("the VM of workspace ws1 could not be branched"),
        "{message}"
    );
    assert!(
        !config.state.join("vms").join("ws2").exists(),
        "no disk, no part of one and no meter for a later wake to take for a VM"
    );
    assert_eq!(service.status().len(), 1);
    assert!(
        lock(&service.branching).is_empty(),
        "nothing stays reserved"
    );
    std::fs::write(source.disk(), b"one").unwrap();
    assert_eq!(
        branch(&service, "ws2", "acct-1", VmSize::Small, "ws1"),
        Reply::Done
    );
}

#[test]
fn a_deleted_vm_is_gone_from_the_host_and_deleting_it_again_is_done() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.settings.templates = Some(config.state.join("vms").join("template"));
    let files = with_disk(
        &config,
        "ws1",
        "acct-1",
        VmSize::Small,
        b"one",
        &["vm.mem", "vm.state"],
    );
    let template = with_disk(&config, "template", "", VmSize::Small, b"t", &[]);
    let service = open_at(&config, 0);
    assert_eq!(
        held(&service)
            .first()
            .map(|(vm, _, phase)| (vm.as_str(), phase.as_str())),
        Some(("template/default", "Off"))
    );
    assert_eq!(
        service
            .status()
            .iter()
            .find(|vm| vm.vm == "ws1/default")
            .map(|vm| vm.phase.as_str()),
        Some("Suspended")
    );
    assert_eq!(delete(&service, "ws1"), Reply::Done);
    assert!(
        !files.state.exists(),
        "its disk, snapshot and meter are gone"
    );
    assert!(
        !config.state.join("vms").join("ws1").exists(),
        "and its workspace's directory, which held no other VM"
    );
    assert!(service.entry("ws1", "default").is_none());
    assert_eq!(
        delete(&service, "ws1"),
        Reply::Done,
        "deleting it again is done"
    );
    assert_eq!(delete(&service, "ws-never"), Reply::Done);

    let message = error(delete(&service, "template"));
    assert!(message.contains("a template is the host's"), "{message}");
    assert!(template.disk().is_file());
    drop(service);
    assert!(
        open_at(&config, 1_000)
            .status()
            .iter()
            .all(|vm| vm.vm != "ws1/default"),
        "a restarted host does not find it"
    );
}
