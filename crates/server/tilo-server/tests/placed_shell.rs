//! A workspace's shell command runs on the VM host that holds the workspace's
//! VM: the server asks the placer which host runs it, waits while none has
//! workspace, and asks that VM host's member device, a device of the server's own
//! person, to run it over the device action service. The member holds the workspace as
//! any member does — entering it the first time the server asks about it, as a member
//! copy held from where the server's request says it serves the workspace — and its command works
//! on the member's copy of the workspace's files, its output coming back as
//! the run.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host, the member and their routers stay up until the test ends"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use iroh::{EndpointAddr, EndpointId};
use linux_vm::hosted::control::{self, Reply, Request};
use linux_vm::hosted::size::VmSize;
use tilo_actions::InvocationId;
use tilo_actions::remote::environment_commands::{self, CommandOutput, CommandRequest};
use tilo_actions::remote::environment_programs::{self, ServeProgram, TellProgram};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_agent_session::workspace_actions::{ShellRan, Watching};
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_runtime::device::take_work;
use tilo_server::placed_programs::{Machine, serve_placed};
use tilo_server::placed_shells::{Member, Members, PlacedShells};
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::DeviceCapabilities;
use ush::environment_programs::{Heard, Start};

use crate::common::{Hosted, TestUser, device_endpoint, serve_on};

/// The VM host the placer names, as the operator's `StatefulSet` calls it.
const VM_HOST: &str = "tilo-vm-host-0";

/// A placer that has no room for the first ask and names [`VM_HOST`] after.
fn placer() -> (String, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let Ok(Some(Request::Place { vm })) =
                control::FRAMING.read_value::<_, Request>(&mut stream)
            else {
                continue;
            };
            assert_eq!(vm.account, "acct-1");
            let reply = if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::Busy(linux_vm::hosted::service::BUSY.into())
            } else {
                Reply::Placed {
                    host: VM_HOST.into(),
                }
            };
            control::FRAMING.write_value(&mut stream, &reply).unwrap();
        }
    });
    (address, asked)
}

/// The `host:port` an endpoint of this process listens on, as a server's
/// members file names a member.
pub(super) fn address_of(endpoint: &iroh::Endpoint) -> String {
    let socket = endpoint
        .bound_sockets()
        .into_iter()
        .find(std::net::SocketAddr::is_ipv4)
        .expect("the endpoint listens on IPv4");
    format!("127.0.0.1:{}", socket.port())
}

/// `device`'s address, as the server reaches the member it asks.
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

/// A workspace whose VM host is a member device that takes its work, with the
/// server's placed shells asking it.
pub(super) struct Placed {
    pub(super) workspace: String,
    pub(super) shells: Arc<PlacedShells>,
    /// The member device that runs the workspace's shells.
    member: take_work::Member,
    /// That member's device key.
    member_device: [u8; 32],
    /// Where that member is reached.
    member_address: String,
    /// How often the placer was asked.
    asked: Arc<AtomicUsize>,
    hosted: Hosted,
    /// The member's profile, up for as long as the member is.
    _profile: tempfile::TempDir,
}

/// Arrange a workspace with a VM host that takes its work, as the operator's
/// fleet does.
pub(super) async fn placed() -> Arc<Placed> {
    let operator = TestUser::fresh("operator");
    let shell = operator.enrol("host", DeviceCapabilities::primary());
    let vm_host = operator.enrol(VM_HOST, DeviceCapabilities::member());
    let hosted = serve_on(&operator, &shell, WorkSetting::own_person()).await;
    let workspace = hosted
        .host
        .workspaces()
        .create("Build")
        .await
        .unwrap()
        .workspace;

    // The VM host is a member device that takes work: it holds the workspace
    // the server places on it as any member does, entering it the first time
    // the server asks about it, and answers the server's runs with the
    // workspace's shells on its own copy of the files.
    let profile = tempfile::tempdir().unwrap();
    let member_endpoint = device_endpoint(&vm_host).await;
    let member_address = address_of(&member_endpoint);
    let member = take_work::Member::open(
        profile.path(),
        (vm_host.sk.clone(), vm_host.cert.clone()),
        member_endpoint,
        take_work::Server::new(&hex::encode(shell.device_pubkey())).unwrap(),
        None,
    )
    .unwrap();

    // The server knows its VM hosts' members, and asks the placer.
    let (placer, asked) = placer();
    let members = Members::from([(
        VM_HOST.to_owned(),
        Member {
            device: hex::encode(vm_host.device_pubkey()),
            address: member_address.clone(),
        },
    )]);
    let shells = Arc::new(PlacedShells::new(
        placer,
        members,
        "acct-1".into(),
        VmSize::Small,
    ));
    shells.ask_from(
        hosted.host.workspaces().action_service(),
        hosted.router.endpoint().clone(),
        hosted.host.workspaces().clone(),
        tokio::runtime::Handle::current(),
    );
    Arc::new(Placed {
        workspace,
        shells,
        member,
        member_device: vm_host.device_pubkey(),
        member_address,
        asked,
        hosted,
        _profile: profile,
    })
}

/// Run `command` in `session` on the member, off this thread.
async fn run(placed: &Arc<Placed>, session: &str, command: &str) -> Result<ShellRan, String> {
    let (shells, workspace) = (Arc::clone(&placed.shells), placed.workspace.clone());
    let (session, command) = (session.to_owned(), command.to_owned());
    tokio::task::spawn_blocking(move || {
        shells.run(
            &workspace,
            &session,
            &command,
            true,
            None,
            Vec::new(),
            Watching::default(),
        )
    })
    .await
    .unwrap()
}

/// Stop the host, once nothing else holds the layout.
pub(super) async fn stop(placed: Arc<Placed>) {
    let Ok(placed) = Arc::try_unwrap(placed) else {
        panic!("nothing else holds the layout");
    };
    placed.hosted.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_runs_on_the_vm_host_holding_the_workspace() {
    let placed = placed().await;

    // Nothing in this test admitted the member to the workspace: the server's
    // first run is what makes it enter, and that run is answered.
    let ran = run(
        &placed,
        "run:1",
        "mkdir out && echo built > out/artifact && cat out/artifact",
    )
    .await
    .unwrap();
    let output = String::from_utf8(ran.output).unwrap();
    assert_eq!(ran.exit, 0, "{output}");
    assert!(
        output.starts_with(
            "ush: Starting another machine for it, about 3 minutes. Waiting; Ctrl+C cancels.\n"
        ),
        "the wait for room was said once, first: {output}"
    );
    assert!(output.ends_with("built\n"), "{output}");
    assert_eq!(
        placed.asked.load(Ordering::SeqCst),
        2,
        "asked again once there was workspace"
    );
    assert_eq!(
        std::fs::read_to_string(placed.member.files(&placed.workspace).join("out/artifact"))
            .unwrap(),
        "built\n",
        "the command worked on the VM host's own copy of the workspace's files"
    );

    // The session's shell lives on there: the next command starts where
    // the last one left off.
    let again = run(&placed, "run:1", "cd out && pwd").await.unwrap();
    let output = String::from_utf8(again.output).unwrap();
    assert!(output.trim_end().ends_with("/out"), "{output}");

    // A device of another person asking the member for the workspace's shell
    // is refused there: it takes the work of its own person's workspaces only.
    let stranger = TestUser::fresh("stranger").enrol("laptop", DeviceCapabilities::member());
    let stranger_endpoint = device_endpoint(&stranger).await;
    let asking = ActionService::new(stranger.sk.clone(), Arc::new(NoActions));
    let member_device = asking.remote(
        &stranger_endpoint,
        &member_at(placed.member_device, &placed.member_address),
        &placed.workspace,
    );
    let Ok(Some(answered)) = member_device
        .invoke(
            &Caller::local_app("tilo.server"),
            run_request("run:2", "echo stranger > stranger.txt"),
        )
        .wait_timeout(Duration::from_secs(30))
    else {
        panic!("the member's action service answers, and refuses it");
    };
    let refused = match answered {
        Ok(receipt) => panic!(
            "a device of another person runs nothing here: {:?}",
            receipt.status
        ),
        Err(error) => error,
    };
    assert!(
        matches!(refused, ActionCallError::NotPermitted),
        "the member refuses a device of another person: {refused:?}"
    );
    assert!(
        !placed
            .member
            .files(&placed.workspace)
            .join("stranger.txt")
            .exists(),
        "nothing ran on the member for another person"
    );

    // A workspace the server places here later is entered the same way: the
    // member takes the work of every workspace the server governs, not only
    // the first it heard of.
    let later = placed
        .hosted
        .host
        .workspaces()
        .create("Second")
        .await
        .unwrap()
        .workspace;
    let (shells, asked_later) = (Arc::clone(&placed.shells), later.clone());
    let ran = tokio::task::spawn_blocking(move || {
        shells.run(
            &asked_later,
            "run:1",
            "pwd",
            true,
            None,
            Vec::new(),
            Watching::default(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ran.exit, 0);
    assert_eq!(
        String::from_utf8(ran.output).unwrap().trim_end(),
        placed.member.files(&later).to_string_lossy(),
        "the second workspace's shell runs in its own files here"
    );

    stop(placed).await;
}

/// `asked` at `target`, as the server asks a member about a served program.
fn program_request(target: RemoteActionTarget, asked: &impl serde::Serialize) -> InvokeRequest {
    let serde_json::Value::Object(argument) = serde_json::to_value(asked).unwrap() else {
        panic!("what a member is asked is an object");
    };
    InvokeRequest {
        target,
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

#[tokio::test(flavor = "multi_thread")]
async fn a_served_program_is_asked_of_the_member_holding_the_workspaces_machine() {
    let placed = placed().await;

    // The start crosses to the member as the server's own device, asking as the workspace,
    // once the placer names it. This member has no machines, so what comes back from it is
    // the program's end and the member's own words for why: nothing is refused on the way.
    let (shells, workspace) = (Arc::clone(&placed.shells), placed.workspace.clone());
    let over = tokio::task::spawn_blocking(move || {
        let start = Start {
            argv: vec!["true".into()],
            env: Vec::new(),
            directory: std::path::PathBuf::new(),
            endpoints: Vec::new(),
            stop_grace: Duration::from_secs(1),
            agent_program: "/bin/true".into(),
            secrets: Vec::new(),
        };
        let program = serve_placed(&shells, &workspace, start).unwrap();
        tilo_test_env::wait_for(
            "the member to answer for the program",
            || match Machine::hear(program.as_ref(), Duration::from_millis(100)) {
                Some(Heard::Over(over)) => Some(over),
                _ => None,
            },
        )
    })
    .await
    .unwrap();
    assert!(
        over.words.contains("no Linux machines"),
        "the member's own answer is how the program ended: {over:?}"
    );
    assert_eq!(
        placed.asked.load(Ordering::SeqCst),
        2,
        "the placer was asked again until it named the member"
    );

    // A device of another person starts nothing on the member and tells nothing there.
    let stranger = TestUser::fresh("stranger").enrol("laptop", DeviceCapabilities::member());
    let stranger_endpoint = device_endpoint(&stranger).await;
    let asking = ActionService::new(stranger.sk.clone(), Arc::new(NoActions));
    let member_device = asking.remote(
        &stranger_endpoint,
        &member_at(placed.member_device, &placed.member_address),
        &placed.workspace,
    );
    let starting = ServeProgram {
        argv: vec!["true".into()],
        env: Vec::new(),
        directory: String::new(),
        endpoints: Vec::new(),
        stop_grace_ms: 1000,
        agent_program: "/bin/true".into(),
        account: "acct-1".into(),
        size: "small".into(),
        secrets: Vec::new(),
    };
    let telling = TellProgram {
        program: InvocationId::new(),
        first: 0,
        frames: Vec::new(),
        taken: 0,
    };
    for request in [
        program_request(environment_programs::serve_target(), &starting),
        program_request(environment_programs::tell_target(), &telling),
    ] {
        let Ok(Some(answered)) = member_device
            .invoke(&Caller::local_app("tilo.server"), request)
            .wait_timeout(Duration::from_secs(30))
        else {
            panic!("the member's action service answers, and refuses it");
        };
        assert!(
            matches!(answered, Err(ActionCallError::NotPermitted)),
            "the member refuses a device of another person: {answered:?}"
        );
    }

    stop(placed).await;
}

/// The workspace's governance generation: it moves when a device is admitted.
async fn generation(placed: &Placed, workspace: &str) -> u64 {
    placed
        .hosted
        .host
        .workspaces()
        .membership(workspace)
        .await
        .unwrap()
        .0
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_server_makes_the_member_enter_a_workspace() {
    let placed = placed().await;
    let later = placed
        .hosted
        .host
        .workspaces()
        .create("Second")
        .await
        .unwrap()
        .workspace;
    let before = generation(&placed, &later).await;

    // Another device naming a workspace the member does not hold is refused,
    // and the server admits nothing for it.
    let stranger = TestUser::fresh("stranger").enrol("laptop", DeviceCapabilities::member());
    let stranger_endpoint = device_endpoint(&stranger).await;
    let asking = ActionService::new(stranger.sk.clone(), Arc::new(NoActions));
    let answered = asking
        .remote(
            &stranger_endpoint,
            &member_at(placed.member_device, &placed.member_address),
            &later,
        )
        .invoke(
            &Caller::local_app("tilo.server"),
            run_request("run:1", "echo stranger > stranger.txt"),
        )
        .wait_timeout(Duration::from_secs(30));
    assert!(
        matches!(answered, Ok(Some(Err(_)))),
        "a device that is not the server is refused a workspace the member does not hold"
    );
    assert_eq!(
        generation(&placed, &later).await,
        before,
        "a stranger's request made the member enter a workspace"
    );

    // The server's own run is what enters it.
    let (shells, asked) = (Arc::clone(&placed.shells), later.clone());
    let ran = tokio::task::spawn_blocking(move || {
        shells.run(
            &asked,
            "run:1",
            "pwd",
            true,
            None,
            Vec::new(),
            Watching::default(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ran.exit, 0);
    assert_ne!(
        generation(&placed, &later).await,
        before,
        "the server's run admitted the member"
    );

    stop(placed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_runs_output_reaches_the_caller_before_it_ends() {
    let placed = placed().await;
    let seen = Arc::new(std::sync::Mutex::new(Vec::<CommandOutput>::new()));
    let ended = Arc::new(AtomicBool::new(false));
    let (recorded, running) = (Arc::clone(&seen), Arc::clone(&ended));
    let (shells, workspace) = (Arc::clone(&placed.shells), placed.workspace.clone());

    let ran = tokio::task::spawn_blocking(move || {
        let watch = |printed: &CommandOutput| {
            if !running.load(Ordering::SeqCst) {
                recorded.lock().unwrap().push(printed.clone());
            }
        };
        let ran = shells.run(
            &workspace,
            "run:1",
            "echo first && sleep 3 && echo second",
            true,
            None,
            Vec::new(),
            Watching {
                seen: Some(&watch),
                stop: None,
            },
        );
        ended.store(true, Ordering::SeqCst);
        ran
    })
    .await
    .unwrap()
    .unwrap();
    let output = String::from_utf8(ran.output).unwrap();
    assert_eq!(ran.exit, 0, "{output}");
    assert!(output.ends_with("first\nsecond\n"), "{output}");
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .any(|printed| printed.tail == "first\n" && printed.total == 6),
        "the first line reached the caller while the command ran: {seen:?}"
    );
    stop(placed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_ends_the_command_on_the_member() {
    let placed = placed().await;
    let cancel = Arc::new(AtomicBool::new(false));
    let printed = Arc::new(AtomicBool::new(false));
    let (shells, workspace) = (Arc::clone(&placed.shells), placed.workspace.clone());
    let (stopping, seen) = (Arc::clone(&cancel), Arc::clone(&printed));

    let run = tokio::task::spawn_blocking(move || {
        let stop = || stopping.load(Ordering::SeqCst);
        let watch = |printed: &CommandOutput| {
            if printed.tail.contains("started") {
                seen.store(true, Ordering::SeqCst);
            }
        };
        shells.run(
            &workspace,
            "run:1",
            "echo started && sleep 120 && echo never",
            true,
            None,
            Vec::new(),
            Watching {
                seen: Some(&watch),
                stop: Some(&stop),
            },
        )
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    while !printed.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "the command's first line never reached the caller"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The person's Ctrl+C in the session, or the agent's cancel of the
    // invocation that asked for the run.
    let asked_at = Instant::now();
    cancel.store(true, Ordering::SeqCst);
    let ran = tokio::time::timeout(Duration::from_secs(30), run)
        .await
        .expect("a cancelled run ends")
        .unwrap()
        .unwrap();
    assert!(
        asked_at.elapsed() < Duration::from_secs(20),
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
