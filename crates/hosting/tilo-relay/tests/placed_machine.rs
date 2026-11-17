//! A workspace's Linux machine is branched and deleted on the VM host that
//! holds it: the relay asks the placer where, then that host's member device,
//! over the device action service as the relay's own device and in the workspace's name,
//! and the member asks its hosted-VM service on the service's own socket. A
//! deletion asks the placer whether there is a machine at all, and forgets it
//! there only once the member deleted it.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host, the member and their routers stay up until the test ends"
)]

use std::sync::{Arc, Mutex};

use legacy_vm::hosted::control::{self, Reply, Request, VmRequest};
use legacy_vm::hosted::size::VmSize;
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_relay::placed_machines::PlacedMachines;
use tilo_relay::placed_shells::{Member, Members, PlacedShells};
use tilo_relay::workspaces::WorkspaceMachines as _;
use tilo_relay_protocol::WorkSetting;
use tilo_runtime::standalone::take_work;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{Hosted, TestUser, device_endpoint, host_on};

/// The VM host the placer names.
const VM_HOST: &str = "tilo-vm-host-0";

/// Who was asked what, in order: `placer` or `service`, and the request.
type Asked = Arc<Mutex<Vec<(&'static str, Request)>>>;

/// How a fake answers a request.
type Answer = Arc<Mutex<Box<dyn FnMut(&Request) -> Reply + Send>>>;

fn answering(answer: impl FnMut(&Request) -> Reply + Send + 'static) -> Answer {
    Arc::new(Mutex::new(Box::new(answer)))
}

/// Serve the connections `accept` takes one request each, noting each as
/// `who`'s and answering it as `answer` says.
fn serve<S: std::io::Read + std::io::Write>(
    mut accept: impl FnMut() -> std::io::Result<S> + Send + 'static,
    who: &'static str,
    asked: Asked,
    answer: Answer,
) {
    std::thread::spawn(move || {
        loop {
            let Ok(mut stream) = accept() else { continue };
            let Ok(Some(request)) = control::FRAMING.read_value::<_, Request>(&mut stream) else {
                continue;
            };
            asked.lock().unwrap().push((who, request.clone()));
            let reply = (answer.lock().unwrap())(&request);
            let _ = control::FRAMING.write_value(&mut stream, &reply);
        }
    });
}

/// The `host:port` an endpoint of this process listens on.
fn address_of(endpoint: &iroh::Endpoint) -> String {
    let socket = endpoint
        .bound_sockets()
        .into_iter()
        .find(std::net::SocketAddr::is_ipv4)
        .expect("the endpoint listens on IPv4");
    format!("127.0.0.1:{}", socket.port())
}

/// A relay, the member device of its one VM host with a hosted-VM service
/// beside it, and a placer: the two fakes answer as the test says.
struct Fleet {
    hosted: Hosted,
    machines: Arc<PlacedMachines>,
    placer_address: String,
    members: Members,
    asked: Asked,
    placer: Answer,
    service: Answer,
    _member: take_work::Member,
    _dir: tempfile::TempDir,
}

async fn fleet() -> Fleet {
    let operator = TestUser::fresh("operator");
    let shell = operator.enrol("host", DeviceCapabilities::primary());
    let vm_host = operator.enrol(VM_HOST, DeviceCapabilities::member());
    let hosted = host_on(&operator, &shell, WorkSetting::own_person()).await;
    let dir = tempfile::tempdir().unwrap();
    let asked = Asked::default();

    let placer = answering(|_| Reply::Error("the test did not say".into()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let placer_address = listener.local_addr().unwrap().to_string();
    serve(
        move || listener.accept().map(|(stream, _)| stream),
        "placer",
        Arc::clone(&asked),
        Arc::clone(&placer),
    );

    let service = answering(|_| Reply::Error("the test did not say".into()));
    let socket = dir.path().join("hosted.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    serve(
        move || listener.accept().map(|(stream, _)| stream),
        "service",
        Arc::clone(&asked),
        Arc::clone(&service),
    );

    let member_endpoint = device_endpoint(&vm_host).await;
    let member_address = address_of(&member_endpoint);
    let member = take_work::Member::open(
        &dir.path().join("member"),
        (vm_host.sk.clone(), vm_host.cert.clone()),
        member_endpoint,
        take_work::Relay::new(
            &hex::encode(shell.device_pubkey()),
            &address_of(hosted.router.endpoint()),
        )
        .unwrap(),
        Some(socket),
    )
    .unwrap();
    let members = Members::from([(
        VM_HOST.to_owned(),
        Member {
            device: hex::encode(vm_host.device_pubkey()),
            address: member_address,
        },
    )]);
    let shells = Arc::new(PlacedShells::new(
        placer_address.clone(),
        members.clone(),
        "acct-1".into(),
        VmSize::Small,
    ));
    shells.ask_from(
        hosted.host.workspaces().action_service(),
        hosted.router.endpoint().clone(),
    );
    let machines = Arc::new(PlacedMachines::new(Arc::clone(&shells)));
    hosted.host.workspaces().machines(machines.clone());
    Fleet {
        hosted,
        machines,
        placer_address,
        members,
        asked,
        placer,
        service,
        _member: member,
        _dir: dir,
    }
}

impl Fleet {
    fn asked(&self) -> Vec<(&'static str, Request)> {
        self.asked.lock().unwrap().clone()
    }

    async fn workspace(&self, name: &str) -> String {
        self.hosted
            .host
            .workspaces()
            .create(name)
            .await
            .unwrap()
            .workspace
    }

    /// The workspace's governance generation: it moves when a device enters.
    async fn generation(&self, workspace: &str) -> u64 {
        self.hosted
            .host
            .workspaces()
            .membership(workspace)
            .await
            .unwrap()
            .0
    }

    async fn branch(&self, source: &str, branch: &str) -> Result<bool, String> {
        let (machines, source, branch) = (
            Arc::clone(&self.machines),
            source.to_owned(),
            branch.to_owned(),
        );
        tokio::task::spawn_blocking(move || machines.branch(&source, &branch))
            .await
            .unwrap()
    }

    async fn stop(self) {
        self.hosted.stop().await;
    }
}

fn vm(workspace: &str) -> VmRequest {
    VmRequest {
        workspace: workspace.into(),
        environment: "default".into(),
        account: "acct-1".into(),
        size: VmSize::Small,
    }
}

fn on_host() -> Reply {
    Reply::Placed {
        host: VM_HOST.into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_branch_asks_the_placer_and_then_the_member_holding_the_source() {
    let fleet = fleet().await;
    let source = fleet.workspace("Template").await;
    let branch = fleet.workspace("Task").await;
    *fleet.placer.lock().unwrap() = Box::new(|_| on_host());
    *fleet.service.lock().unwrap() = Box::new(|_| Reply::Done);
    let before = fleet.generation(&branch).await;

    assert_eq!(fleet.branch(&source, &branch).await, Ok(true));
    let asked = Request::Branch {
        vm: vm(&branch),
        from: source.clone(),
    };
    assert_eq!(
        fleet.asked(),
        [("placer", asked.clone()), ("service", asked)],
        "the placer named the source's host, and that host's member made the branch there, \
         for the relay's account at its size"
    );
    assert_ne!(
        fleet.generation(&branch).await,
        before,
        "the member entered the branch, asked in its name"
    );
    fleet.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_branch_the_member_refuses_fails_in_its_words() {
    let fleet = fleet().await;
    let source = fleet.workspace("Template").await;
    let branch = fleet.workspace("Task").await;
    *fleet.placer.lock().unwrap() = Box::new(|request| match request {
        // The host made nothing, so it lists nothing to keep the placement.
        Request::Delete { .. } => Reply::Done,
        _ => on_host(),
    });
    *fleet.service.lock().unwrap() = Box::new(|request| {
        let Request::Branch { from, .. } = request else {
            return Reply::Error("only a branch is asked".into());
        };
        Reply::Error(format!(
            "the VM of workspace {from} is xlarge, and a branch of it is no smaller"
        ))
    });
    let refused = fleet.branch(&source, &branch).await.unwrap_err();
    assert!(
        refused.starts_with(&format!("{VM_HOST}: ")) && refused.ends_with("is no smaller"),
        "the refusal names the host and says why: {refused}"
    );
    let asked = Request::Branch {
        vm: vm(&branch),
        from: source.clone(),
    };
    assert_eq!(
        fleet.asked(),
        [
            ("placer", asked.clone()),
            ("service", asked),
            (
                "placer",
                Request::Delete {
                    workspace: branch.clone(),
                    environment: "default".into(),
                }
            ),
        ],
        "the placement made for the machine that did not come is forgotten"
    );

    // The member takes the work of its own person's relay alone: asked by a
    // device of another person, it refuses, enters nothing and asks its
    // service nothing.
    let stranger = TestUser::fresh("stranger").enrol("laptop", DeviceCapabilities::member());
    let stranger_endpoint = device_endpoint(&stranger).await;
    let posing = Arc::new(PlacedShells::new(
        fleet.placer_address.clone(),
        fleet.members.clone(),
        "acct-1".into(),
        VmSize::Small,
    ));
    posing.ask_from(
        ActionService::new(stranger.sk.clone(), Arc::new(NoActions)),
        stranger_endpoint,
    );
    let other = fleet.workspace("Other").await;
    let before = (fleet.generation(&other).await, fleet.asked().len());
    let machines = PlacedMachines::new(posing);
    let (asked_source, asked_other) = (source.clone(), other.clone());
    let refused = tokio::task::spawn_blocking(move || machines.branch(&asked_source, &asked_other))
        .await
        .unwrap()
        .unwrap_err();
    assert!(refused.contains("refused"), "{refused}");
    let asked = fleet.asked();
    assert_eq!(
        fleet.generation(&other).await,
        before.0,
        "the stranger's ask made no member enter"
    );
    assert!(
        asked[before.1..].iter().all(|(who, _)| *who == "placer"),
        "and reached the placer alone, which placed it and forgot it again: no machine was made \
         ({asked:?})"
    );
    fleet.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_without_a_machine_branches_with_nothing_asked_of_a_member() {
    let fleet = fleet().await;
    let source = fleet.workspace("Template").await;
    let branch = fleet.workspace("Task").await;
    let none = source.clone();
    *fleet.placer.lock().unwrap() = Box::new(move |_| Reply::NoVm {
        workspace: none.clone(),
    });
    assert_eq!(fleet.branch(&source, &branch).await, Ok(false));
    assert_eq!(
        fleet
            .asked()
            .iter()
            .map(|(who, _)| *who)
            .collect::<Vec<_>>(),
        ["placer"],
        "nothing to copy, so no member was asked"
    );
    fleet.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_is_deleted_after_its_machine_on_the_host_that_holds_it() {
    let fleet = fleet().await;
    let workspace = fleet.workspace("Task").await;
    let deleted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gone = Arc::clone(&deleted);
    *fleet.placer.lock().unwrap() = Box::new(move |request| match request {
        Request::Delete { .. } if gone.load(std::sync::atomic::Ordering::SeqCst) => Reply::Done,
        Request::Delete { workspace, .. } => Reply::Error(format!(
            "{VM_HOST} still holds the VM of workspace {workspace}: delete it there first"
        )),
        _ => on_host(),
    });
    let deleting = Arc::clone(&deleted);
    *fleet.service.lock().unwrap() = Box::new(move |_| {
        deleting.store(true, std::sync::atomic::Ordering::SeqCst);
        Reply::Done
    });

    fleet
        .hosted
        .host
        .workspaces()
        .delete(&workspace)
        .await
        .unwrap();
    let forget = Request::Delete {
        workspace: workspace.clone(),
        environment: "default".into(),
    };
    assert_eq!(
        fleet.asked(),
        [
            ("placer", forget.clone()),
            ("placer", Request::Place { vm: vm(&workspace) }),
            ("service", forget.clone()),
            ("placer", forget),
        ],
        "the placer kept the machine its host held, the member deleted it there, and only then \
         did the placer forget it"
    );
    assert!(
        fleet
            .hosted
            .host
            .workspaces()
            .summary(&workspace)
            .await
            .is_err(),
        "and then the workspace went"
    );
    fleet.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_whose_machines_host_does_not_answer_stays() {
    let fleet = fleet().await;
    let workspace = fleet.workspace("Task").await;
    *fleet.placer.lock().unwrap() = Box::new(move |request| match request {
        Request::Delete { workspace, .. } => Reply::Busy(format!(
            "{VM_HOST}, which holds the VM of workspace {workspace}, does not answer; ask again \
             once it is up"
        )),
        _ => on_host(),
    });
    let refused = fleet
        .hosted
        .host
        .workspaces()
        .delete(&workspace)
        .await
        .unwrap_err();
    assert!(
        refused.contains("Linux machine was not deleted") && refused.contains("does not answer"),
        "{refused}"
    );
    assert!(
        fleet
            .hosted
            .host
            .workspaces()
            .summary(&workspace)
            .await
            .is_ok(),
        "the workspace stays, to be deleted again"
    );
    assert!(
        fleet.asked().iter().all(|(who, _)| *who == "placer"),
        "no disk was deleted on a guess"
    );

    // Once its host answers again and holds no machine for it, it is deleted.
    *fleet.placer.lock().unwrap() = Box::new(|_| Reply::Done);
    fleet
        .hosted
        .host
        .workspaces()
        .delete(&workspace)
        .await
        .unwrap();
    fleet.stop().await;
}
