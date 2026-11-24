//! A workspace's shell command runs on a device of the workspace that runs
//! Linux: the server asks the turn (`tilo.work#take`) of the workspace's
//! devices and of its person's servers, and the one
//! that takes it runs the command, its output coming back as it prints. The
//! VM host is such a server of the operator: it enters the workspace the first
//! time it is asked about it, from where the server's request says it serves
//! the workspace, and its command works on its own copy of the workspace's
//! files. With no such device online the run waits and says so; a device that
//! goes quiet loses the turn to another once its claim lapsed.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host, the members and their routers stay up until the test ends"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::{EndpointAddr, EndpointId};
use linux_vm::hosted::size::VmSize;
use tilo_actions::InvocationId;
use tilo_actions::remote::environment_commands::{self, CommandOutput, CommandRequest};
use tilo_actions::remote::{ActionProvider as _, Caller, InvokeArgument, InvokeRequest};
use tilo_agent_session::shell::{ShellRan, Watching};
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_runtime::device::environments::{Environments, Paid, Timing, WAITING};
use tilo_runtime::device::take_work;
use tilo_server::servers::{self, person_workspace_id};
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{Hosted, TestUser, device_endpoint, serve_on};

/// How the server waits on turns here: a device that took one and went quiet
/// is given up on at once, so a test never waits a real minute.
const QUICK: Timing = Timing {
    claim: Duration::from_secs(5),
    retry: Duration::from_millis(50),
    quiet: Duration::from_millis(300),
};

/// The `host:port` an endpoint of this process listens on.
pub(super) fn address_of(endpoint: &iroh::Endpoint) -> String {
    let socket = endpoint
        .bound_sockets()
        .into_iter()
        .find(std::net::SocketAddr::is_ipv4)
        .expect("the endpoint listens on IPv4");
    format!("127.0.0.1:{}", socket.port())
}

/// `device`'s address, as its entry among its person's servers names it.
pub(super) fn member_at(device: [u8; 32], address: &str) -> EndpointAddr {
    let id = EndpointId::from_bytes(&device).unwrap();
    EndpointAddr::from_parts(
        id,
        std::net::ToSocketAddrs::to_socket_addrs(address)
            .unwrap()
            .map(iroh::TransportAddr::Ip),
    )
}

/// A run of `command` in a session, as the server asks it.
fn run_request(session: &str, command: &str) -> InvokeRequest {
    let argument = serde_json::to_value(CommandRequest {
        session: session.into(),
        command: command.into(),
        agent: true,
        account: "acct-1".into(),
        size: "small".into(),
        secrets: Vec::new(),
        person: None,
    })
    .unwrap();
    let serde_json::Value::Object(argument) = argument else {
        panic!("a run's argument is an object");
    };
    InvokeRequest {
        target: environment_commands::target(),
        argument: InvokeArgument::Json(argument),
        invocation: InvocationId::new(),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// A VM host: a server of the operator that runs Linux machines and takes the
/// workspaces' Linux turns.
pub(super) struct VmHost {
    member: Mutex<Option<take_work::Member>>,
    pub(super) device: [u8; 32],
    pub(super) address: String,
    profile: tempfile::TempDir,
}

impl VmHost {
    /// Where `workspace`'s files are on this host.
    pub(super) fn files(&self, workspace: &str) -> std::path::PathBuf {
        take_work::files(self.profile.path(), workspace)
    }

    /// The host stops answering, as a machine that went away does.
    fn go_quiet(&self) {
        drop(self.member.lock().unwrap().take());
    }
}

/// A workspace served by a server whose person's VM hosts take its Linux work.
pub(super) struct Placed {
    pub(super) workspace: String,
    pub(super) shells: Arc<Environments>,
    pub(super) vm_hosts: Vec<VmHost>,
    pub(super) hosted: Hosted,
}

/// Arrange a workspace with `count` VM hosts, as the operator's fleet does,
/// each serving Browser on `browsers` when it names a hosted-VM service.
pub(super) async fn placed_with(count: usize, browsers: Option<std::path::PathBuf>) -> Arc<Placed> {
    let operator = TestUser::fresh("operator");
    let shell = operator.enrol("host", DeviceCapabilities::primary());
    let hosted = serve_on(&operator, &shell, WorkSetting::own_person()).await;
    let workspaces = hosted.host.workspaces();
    let workspace = workspaces.create("Build").await.unwrap().workspace;
    workspaces
        .create_workspace("Servers", Some(&person_workspace_id(&operator.user_pk)))
        .await
        .unwrap();

    let mut vm_hosts = Vec::new();
    for at in 0..count {
        let vm_host = operator.enrol(&format!("tilo-vm-host-{at}"), DeviceCapabilities::member());
        let profile = tempfile::tempdir().unwrap();
        let endpoint = device_endpoint(&vm_host).await;
        let address = address_of(&endpoint);
        let member = take_work::Member::open(
            profile.path(),
            (vm_host.sk.clone(), vm_host.cert.clone()),
            endpoint,
            take_work::Server::new(&hex::encode(shell.device_pubkey())).unwrap(),
            browsers.clone(),
        )
        .unwrap();
        // It logs in as the ticket it started with leaves it, and enters
        // itself among the operator's servers at the address it answers on.
        let login = tilo_device_identity::login::Device {
            site: "https://tilo.test/".into(),
            host_origin: "https://tilo.test".into(),
            workspace: person_workspace_id(&operator.user_pk),
            workspace_name: None,
            agent: String::new(),
            cert: vm_host.cert.clone(),
            server_cert: shell.cert.clone(),
            server_address: Some(tilo_server_protocol::web::ServerAddress {
                endpoint_addr: hosted.host.node_addr(),
                relay_url: None,
                web: None,
            }),
        };
        member.log_in(&vm_host.sk, &login).await.unwrap();
        let device = vm_host.device_pubkey();
        vm_hosts.push(VmHost {
            member: Mutex::new(Some(member)),
            device,
            address,
            profile,
        });
    }

    // The server's environments, with no runtime beside it: its Linux work runs on the
    // devices that take it.
    let paid = Paid {
        account: "acct-1".into(),
        size: VmSize::Small,
    };
    let shells = Arc::new(Environments::new(PathBuf::new(), Some(paid)).with_timing(QUICK));
    shells.ask_from(
        workspaces.action_service(),
        hosted.router.endpoint().clone(),
        workspaces.clone(),
        tokio::runtime::Handle::current(),
    );
    Arc::new(Placed {
        workspace,
        shells,
        vm_hosts,
        hosted,
    })
}

/// A workspace with one VM host.
pub(super) async fn placed() -> Arc<Placed> {
    placed_with(1, None).await
}

/// What a test sees of a run's output as it prints.
type Seen = Arc<dyn Fn(&CommandOutput) + Send + Sync>;

/// Run `command` in `session`, off this thread, watched as `watching` says.
async fn run_watched(
    placed: &Arc<Placed>,
    workspace: &str,
    command: &str,
    seen: Option<Seen>,
    stop: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
) -> Result<ShellRan, String> {
    let (shells, workspace, command) = (
        Arc::clone(&placed.shells),
        workspace.to_owned(),
        command.to_owned(),
    );
    tokio::task::spawn_blocking(move || {
        let seen = seen
            .as_deref()
            .map(|seen| move |printed: &CommandOutput| seen(printed));
        let stop = stop.as_deref().map(|stop| move || stop());
        shells.run_command(
            &workspace,
            CommandRequest {
                session: "run:1".into(),
                command,
                agent: true,
                account: String::new(),
                size: String::new(),
                secrets: Vec::new(),
                person: None,
            },
            Watching {
                seen: seen
                    .as_ref()
                    .map(|seen| seen as &(dyn Fn(&CommandOutput) + Sync)),
                stop: stop.as_ref().map(|stop| stop as &(dyn Fn() -> bool + Sync)),
            },
        )
    })
    .await
    .unwrap()
}

async fn run(placed: &Arc<Placed>, command: &str) -> Result<ShellRan, String> {
    run_watched(placed, &placed.workspace.clone(), command, None, None).await
}

/// Stop the host, once nothing else uses the layout.
pub(super) async fn stop(placed: Arc<Placed>) {
    let Ok(placed) = Arc::try_unwrap(placed) else {
        panic!("nothing else uses the layout");
    };
    placed.hosted.stop().await;
}

/// Wait until `done` is true, or fail saying `what`.
async fn until(what: &str, done: impl Fn() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(60), "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A VM host started with its ticket enters itself among the operator's
/// servers, at the address it answers on: nothing else writes its entry, and
/// the server finds it there for the workspace's Linux turn.
#[tokio::test(flavor = "multi_thread")]
async fn a_vm_host_started_with_a_ticket_is_found_for_a_linux_turn() {
    let placed = placed().await;
    let vm_host = &placed.vm_hosts[0];
    let servers = servers::read(placed.hosted.host.workspaces())
        .await
        .unwrap();
    let entry = servers.servers.get(&servers::device_id(&vm_host.device));
    let endpoint: EndpointAddr = serde_json::from_str(&entry.unwrap().endpoint).unwrap();
    assert_eq!(
        endpoint.id.as_bytes(),
        &vm_host.device,
        "its entry is its own"
    );
    let ran = run(&placed, "echo found").await.unwrap();
    assert_eq!(String::from_utf8(ran.output).unwrap(), "found\n");
    stop(placed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_runs_on_the_device_that_takes_its_linux_turn() {
    let placed = placed().await;
    let vm_host = &placed.vm_hosts[0];

    // Nothing in this test admitted the VM host to the workspace: the server's
    // first ask is what makes it enter, and the run is answered.
    let ran = run(
        &placed,
        "mkdir out && echo built > out/artifact && cat out/artifact",
    )
    .await
    .unwrap();
    let output = String::from_utf8(ran.output).unwrap();
    assert_eq!(ran.exit, 0, "{output}");
    // ush reports its own file commands (`mkdir`) on a line before the
    // command's own output.
    assert!(
        output.ends_with("\nbuilt\n") && !output.contains(WAITING),
        "nothing waited, and it printed what it made: {output}"
    );
    assert_eq!(
        std::fs::read_to_string(vm_host.files(&placed.workspace).join("out/artifact")).unwrap(),
        "built\n",
        "the command worked on the VM host's own copy of the workspace's files"
    );

    // The session's shell lives on there: the next command starts where
    // the last one left off.
    let again = run(&placed, "cd out && pwd").await.unwrap();
    let output = String::from_utf8(again.output).unwrap();
    assert!(output.trim_end().ends_with("/out"), "{output}");

    // A device of another person asking the VM host for the workspace's turn
    // is refused there, and nothing runs for it.
    let stranger = TestUser::fresh("stranger").enrol("laptop", DeviceCapabilities::member());
    let stranger_endpoint = device_endpoint(&stranger).await;
    let asking = ActionService::new(stranger.sk.clone(), Arc::new(NoActions));
    let answered = asking
        .remote(
            &stranger_endpoint,
            &member_at(vm_host.device, &vm_host.address),
            &placed.workspace,
        )
        .invoke(
            &Caller::local_app("stranger"),
            tilo_workspace_service::work::linux_take_request(&run_request(
                "run:2",
                "echo stranger > stranger.txt",
            )),
        )
        .wait_timeout(Duration::from_secs(30));
    assert!(
        matches!(answered, Ok(Some(Err(_)))),
        "a device of another person is refused"
    );
    assert!(
        !vm_host
            .files(&placed.workspace)
            .join("stranger.txt")
            .exists(),
        "nothing ran on the VM host for another person"
    );
    stop(placed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_runs_output_reaches_the_caller_before_it_ends() {
    let placed = placed().await;
    let seen = Arc::new(Mutex::new(Vec::<CommandOutput>::new()));
    let ended = Arc::new(AtomicBool::new(false));
    let (recorded, running) = (Arc::clone(&seen), Arc::clone(&ended));
    let watch: Seen = Arc::new(move |printed| {
        if !running.load(Ordering::SeqCst) {
            recorded.lock().unwrap().push(printed.clone());
        }
    });
    let ran = run_watched(
        &placed,
        &placed.workspace.clone(),
        "echo first && sleep 0.5 && echo second",
        Some(watch),
        None,
    )
    .await
    .unwrap();
    ended.store(true, Ordering::SeqCst);
    let output = String::from_utf8(ran.output).unwrap();
    assert_eq!(ran.exit, 0, "{output}");
    assert_eq!(output, "first\nsecond\n");
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter().any(|printed| printed.tail == "first\n"),
        "the first line reached the caller while the command ran: {seen:?}"
    );
    stop(placed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_reaches_the_command_on_the_device_running_it() {
    let placed = placed().await;
    let cancel = Arc::new(AtomicBool::new(false));
    let printed = Arc::new(AtomicBool::new(false));
    let (stopping, seen) = (Arc::clone(&cancel), Arc::clone(&printed));
    let watch: Seen = Arc::new(move |printed| {
        if printed.tail.contains("started") {
            seen.store(true, Ordering::SeqCst);
        }
    });
    let stop_asked: Arc<dyn Fn() -> bool + Send + Sync> =
        Arc::new(move || stopping.load(Ordering::SeqCst));
    let running = {
        let placed = Arc::clone(&placed);
        tokio::spawn(async move {
            let workspace = placed.workspace.clone();
            run_watched(
                &placed,
                &workspace,
                "echo started && sleep 120 && echo never",
                Some(watch),
                Some(stop_asked),
            )
            .await
        })
    };
    until("the command's first line never reached the caller", || {
        printed.load(Ordering::SeqCst)
    })
    .await;

    // The person's Ctrl+C in the session, or the agent's cancel of the
    // invocation that asked for the run.
    let asked_at = Instant::now();
    cancel.store(true, Ordering::SeqCst);
    let ran = tokio::time::timeout(Duration::from_secs(30), running)
        .await
        .expect("a cancelled run ends")
        .unwrap()
        .unwrap();
    assert!(
        asked_at.elapsed() < Duration::from_secs(10),
        "the run ended promptly: {:?}",
        asked_at.elapsed()
    );
    assert!(ran.cancelled, "the run says it was cancelled: {ran:?}");
    assert_eq!(ran.exit, 130, "{ran:?}");
    let output = String::from_utf8(ran.output).unwrap();
    assert!(
        output.trim_end().ends_with("started") && !output.contains("never"),
        "it printed what it had and stopped: {output}"
    );
    stop(placed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_no_device_that_runs_linux_the_run_waits_and_says_so() {
    let placed = placed_with(0, None).await;
    let asked_at = Instant::now();
    let stop_asked: Arc<dyn Fn() -> bool + Send + Sync> =
        Arc::new(move || asked_at.elapsed() > Duration::from_millis(300));
    let ran = run_watched(
        &placed,
        &placed.workspace.clone(),
        "echo never",
        None,
        Some(stop_asked),
    )
    .await
    .unwrap();
    assert!(ran.cancelled && ran.exit == 130, "{ran:?}");
    let output = String::from_utf8(ran.output).unwrap();
    assert_eq!(output, format!("ush: {WAITING} Ctrl+C cancels.\n"));
    assert!(WAITING.contains("a device that runs Linux"), "{WAITING}");
    stop(placed).await;
}

/// The device running a turn goes quiet; once its claim lapsed, a minute
/// later by the workspace's clock, another device takes the turn over and
/// runs it to its end.
#[test]
fn a_device_that_goes_quiet_loses_the_turn_to_another() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_device_that_goes_quiet_loses_the_turn_to_another",
        || {
            let process = tilo_clock::testing::OwnProcess::of_a_test_in_its_own_process();
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .expect("a runtime for this test alone")
                .block_on(another_device_takes_over(process));
        },
    );
}

async fn another_device_takes_over(process: tilo_clock::testing::OwnProcess) {
    let started = tilo_clock::wall();
    let pinned = tilo_clock::testing::hold_wall_everywhere(process, started);
    let placed = placed_with(2, None).await;
    let workspace = placed.workspace.clone();
    let running = {
        let placed = Arc::clone(&placed);
        let workspace = workspace.clone();
        tokio::spawn(async move {
            run_watched(
                &placed,
                &workspace,
                "touch began && echo began && sleep 1 && echo ended",
                None,
                None,
            )
            .await
        })
    };
    let began = |vm_host: &VmHost| vm_host.files(&workspace).join("began").exists();
    until("no VM host took the run", || {
        placed.vm_hosts.iter().any(began)
    })
    .await;
    let (first, other) = if began(&placed.vm_hosts[0]) {
        (&placed.vm_hosts[0], &placed.vm_hosts[1])
    } else {
        (&placed.vm_hosts[1], &placed.vm_hosts[0])
    };
    first.go_quiet();
    // A minute passes, and the claim of the device that went quiet with it.
    drop(pinned);
    let later = started
        .checked_add(Duration::from_secs(2 * 60))
        .expect("a wall time past the claim");
    let _later = tilo_clock::testing::hold_wall_everywhere(process, later);

    let ran = tokio::time::timeout(Duration::from_secs(60), running)
        .await
        .expect("the run ends on the other device")
        .unwrap()
        .unwrap();
    let output = String::from_utf8(ran.output).unwrap();
    assert_eq!(ran.exit, 0, "{output}");
    assert!(
        output.ends_with("\nbegan\nended\n"),
        "the other device ran it to its end: {output}"
    );
    assert!(began(other), "the other device took the turn over");
    stop(placed).await;
}
