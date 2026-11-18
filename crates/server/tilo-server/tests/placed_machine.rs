//! A workspace's Linux machine is branched and deleted on the VM machine that
//! keeps it: the server reads which in the VM's lease, which that machine's
//! member device holds in the workspace, and asks that member device, over
//! the device action service as the server's own device and in the
//! workspace's name; the member asks its hosted-VM service on the service's
//! own socket. A workspace whose VM nobody keeps has no machine.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host, the member and their routers stay up until the test ends"
)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use linux_vm::hosted::control::{self, HostLoad, Reply, Request, VmRequest, VmStatus};
use linux_vm::hosted::size::VmSize;
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_runtime::standalone::take_work;
use tilo_server::placed_machines::PlacedMachines;
use tilo_server::placed_shells::{Member, Members, PlacedShells};
use tilo_server::workspaces::WorkspaceMachines as _;
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{Hosted, TestUser, device_endpoint, serve_on};

/// The VM machine of the test.
const VM_HOST: &str = "tilo-vm-host-0";

/// Who was asked what, in order: `placer` or `service`, and the request.
type Asked = Arc<Mutex<Vec<(&'static str, Request)>>>;

/// How a fake answers a request.
type Answer = Arc<Mutex<Box<dyn FnMut(&Request) -> Reply + Send>>>;

/// The VMs a fake hosted-VM service keeps, `workspace/environment`.
type Kept = Arc<Mutex<BTreeSet<String>>>;

fn answering(answer: impl FnMut(&Request) -> Reply + Send + 'static) -> Answer {
    Arc::new(Mutex::new(Box::new(answer)))
}

/// Serve the connections `accept` takes one request each, noting each as
/// `who`'s and answering it as `answer` says, and a Status with the VMs
/// `kept` lists.
fn serve<S: std::io::Read + std::io::Write>(
    mut accept: impl FnMut() -> std::io::Result<S> + Send + 'static,
    who: &'static str,
    asked: Asked,
    answer: Answer,
    kept: Kept,
) {
    std::thread::spawn(move || {
        loop {
            let Ok(mut stream) = accept() else { continue };
            let Ok(Some(request)) = control::FRAMING.read_value::<_, Request>(&mut stream) else {
                continue;
            };
            asked.lock().unwrap().push((who, request.clone()));
            let reply = if request == Request::Status {
                status(&kept.lock().unwrap())
            } else {
                (answer.lock().unwrap())(&request)
            };
            let _ = control::FRAMING.write_value(&mut stream, &reply);
        }
    });
}

/// A service's Status listing `kept`.
fn status(kept: &BTreeSet<String>) -> Reply {
    Reply::Status {
        vms: kept
            .iter()
            .map(|vm| VmStatus {
                vm: vm.clone(),
                account: "acct-1".into(),
                size: VmSize::Small,
                phase: "Off".into(),
                last_wake_ms: None,
                last_wake_restored: None,
                snapshot_bytes: None,
                resident_mib: None,
                balloon_mib: 0,
            })
            .collect(),
        load: HostLoad::default(),
    }
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

/// A server, the member device of its one VM machine with a hosted-VM
/// service beside it, and a placer: the two fakes answer as the test says.
struct Fleet {
    hosted: Hosted,
    machines: Arc<PlacedMachines>,
    placer_address: String,
    members: Members,
    asked: Asked,
    service: Answer,
    kept: Kept,
    _member: take_work::Member,
    _dir: tempfile::TempDir,
}

async fn fleet() -> Fleet {
    let operator = TestUser::fresh("operator");
    let shell = operator.enrol("host", DeviceCapabilities::primary());
    let vm_host = operator.enrol(VM_HOST, DeviceCapabilities::member());
    let hosted = serve_on(&operator, &shell, WorkSetting::own_person()).await;
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
        Kept::default(),
    );

    let kept = Kept::default();
    let service = answering(|_| Reply::Error("the test did not say".into()));
    let socket = dir.path().join("hosted.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    serve(
        move || listener.accept().map(|(stream, _)| stream),
        "service",
        Arc::clone(&asked),
        Arc::clone(&service),
        Arc::clone(&kept),
    );

    let member_endpoint = device_endpoint(&vm_host).await;
    let member_address = address_of(&member_endpoint);
    let member = take_work::Member::open(
        &dir.path().join("member"),
        (vm_host.sk.clone(), vm_host.cert.clone()),
        member_endpoint,
        take_work::Server::new(
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
    shells.read_leases(
        hosted.host.workspaces().clone(),
        tokio::runtime::Handle::current(),
    );
    let machines = Arc::new(PlacedMachines::new(Arc::clone(&shells)));
    hosted.host.workspaces().machines(machines.clone());
    Fleet {
        hosted,
        machines,
        placer_address,
        members,
        asked,
        service,
        kept,
        _member: member,
        _dir: dir,
    }
}

impl Fleet {
    /// What was asked besides the member's look at which VMs its service
    /// keeps.
    fn asked(&self) -> Vec<(&'static str, Request)> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, request)| *request != Request::Status)
            .cloned()
            .collect()
    }

    /// Have the member's hosted-VM service keep `workspace`'s VM, as one it
    /// made for it does, and wait until the member holds the VM's lease in
    /// the workspace, read by the server.
    async fn keep(&self, workspace: &str) {
        self.kept
            .lock()
            .unwrap()
            .insert(format!("{workspace}/default"));
        // Any request the server signs in the workspace's name makes the
        // member enter it, and then it looks at which VMs it keeps.
        let member = &self.members[VM_HOST];
        let address = tilo_server::placed_shells::member_address(member).unwrap();
        let device = self.hosted.host.workspaces().action_service().remote(
            self.hosted.router.endpoint(),
            &address,
            workspace,
        );
        let probe = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: "tilo.test".into(),
                address: ActionAddress::new("nothing", "nothing"),
            },
            argument: InvokeArgument::Json(serde_json::Map::new()),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        let caller = Caller::local_app("tilo.server");
        let _ = tokio::task::spawn_blocking(move || {
            device
                .invoke(&caller, probe)
                .wait_timeout(std::time::Duration::from_secs(30))
        })
        .await;
        let lease = tilo_peer_state::vm_lease("default");
        let until = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(30))
            .unwrap();
        loop {
            let holder = self
                .hosted
                .host
                .workspaces()
                .lease_holder(workspace, &lease)
                .await
                .unwrap();
            if holder.map(hex::encode).as_deref() == Some(member.device.as_str()) {
                return;
            }
            assert!(
                std::time::Instant::now() < until,
                "the member holds the lease of the VM it keeps"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
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

#[tokio::test(flavor = "multi_thread")]
async fn a_branch_is_made_by_the_member_keeping_the_source() {
    let fleet = fleet().await;
    let source = fleet.workspace("Template").await;
    let branch = fleet.workspace("Task").await;
    *fleet.service.lock().unwrap() = Box::new(|_| Reply::Done);
    fleet.keep(&source).await;
    let before = fleet.generation(&branch).await;

    assert_eq!(fleet.branch(&source, &branch).await, Ok(true));
    assert_eq!(
        fleet.asked(),
        [(
            "service",
            Request::Branch {
                vm: vm(&branch),
                from: source.clone(),
            }
        )],
        "the member keeping the source's VM made the branch there, for the server's account at \
         its size, and no placer was asked"
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
    *fleet.service.lock().unwrap() = Box::new(|request| match request {
        Request::Branch { from, .. } => Reply::Error(format!(
            "the VM of workspace {from} is xlarge, and a branch of it is no smaller"
        )),
        _ => Reply::Done,
    });
    fleet.keep(&source).await;
    let refused = fleet.branch(&source, &branch).await.unwrap_err();
    assert!(
        refused.starts_with(&format!("{VM_HOST}: ")) && refused.ends_with("is no smaller"),
        "the refusal names the machine and says why: {refused}"
    );
    assert_eq!(
        fleet.asked(),
        [
            (
                "service",
                Request::Branch {
                    vm: vm(&branch),
                    from: source.clone(),
                }
            ),
            (
                "service",
                Request::Delete {
                    workspace: branch.clone(),
                    environment: "default".into(),
                }
            ),
        ],
        "whatever the machine that did not come left is deleted there"
    );

    // The member takes the work of its own person's server alone: asked by a
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
    posing.read_leases(
        fleet.hosted.host.workspaces().clone(),
        tokio::runtime::Handle::current(),
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
    assert_eq!(
        fleet.generation(&other).await,
        before.0,
        "the stranger's ask made no member enter"
    );
    assert_eq!(fleet.asked().len(), before.1, "and no machine was made");
    fleet.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_without_a_machine_branches_with_nothing_asked() {
    let fleet = fleet().await;
    let source = fleet.workspace("Template").await;
    let branch = fleet.workspace("Task").await;
    assert_eq!(fleet.branch(&source, &branch).await, Ok(false));
    assert_eq!(fleet.asked(), [], "nobody keeps its VM: nothing to copy");
    fleet.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_is_deleted_after_its_machine_on_the_machine_keeping_it() {
    let fleet = fleet().await;
    let workspace = fleet.workspace("Task").await;
    *fleet.service.lock().unwrap() = Box::new(|_| Reply::Done);
    fleet.keep(&workspace).await;

    fleet
        .hosted
        .host
        .workspaces()
        .delete(&workspace)
        .await
        .unwrap();
    assert_eq!(
        fleet.asked(),
        [(
            "service",
            Request::Delete {
                workspace: workspace.clone(),
                environment: "default".into(),
            }
        )],
        "the member keeping the VM deleted it there"
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
async fn a_workspace_whose_machine_is_not_deleted_stays() {
    let fleet = fleet().await;
    let workspace = fleet.workspace("Task").await;
    *fleet.service.lock().unwrap() =
        Box::new(|_| Reply::Busy("the machine's disk does not answer; ask again".into()));
    fleet.keep(&workspace).await;
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

    // Once the machine deletes it, the workspace goes.
    *fleet.service.lock().unwrap() = Box::new(|_| Reply::Done);
    fleet
        .hosted
        .host
        .workspaces()
        .delete(&workspace)
        .await
        .unwrap();
    fleet.stop().await;
}
