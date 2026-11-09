//! A workspace's shell command runs on the VM host that holds the workspace's
//! VM: the relay asks the placer which host runs it, waits while none has
//! room, and asks that VM host's member device, a device of the relay's own
//! person, to run it over the device plane. The member holds the workspace as
//! any member does — entering it the first time the relay asks about it, with
//! the invitation the relay's own `room.enter` answers — and its command works
//! on the member's copy of the workspace's files, its output coming back as
//! the run.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host, the member and their routers stay up until the test ends"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iroh::{EndpointAddr, EndpointId};
use legacy_vm::hosted::control::{self, Reply, Request};
use legacy_vm::hosted::size::VmSize;
use tilo_actions::InvocationId;
use tilo_actions::remote::environment_runs::{self, RunCommand};
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, InvokeArgument, InvokeRequest,
};
use tilo_peer_state::actions::{ActionPlane, NoActions};
use tilo_relay::host::HostMode;
use tilo_relay::placed_shells::{Member, Members, PlacedShells};
use tilo_runtime::standalone::take_work;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{Hosted, TestDevice, TestUser, device_endpoint, host_on};

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
                Reply::Busy(legacy_vm::hosted::service::BUSY.into())
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

/// The `host:port` an endpoint of this process listens on, as a relay's
/// members file names a member.
fn address_of(endpoint: &iroh::Endpoint) -> String {
    let socket = endpoint
        .bound_sockets()
        .into_iter()
        .find(std::net::SocketAddr::is_ipv4)
        .expect("the endpoint listens on IPv4");
    format!("127.0.0.1:{}", socket.port())
}

/// The `host:port` a host of this process is reached at.
fn host_address(hosted: &Hosted) -> String {
    address_of(hosted.router.endpoint())
}

/// `device`'s address, as the relay reaches the member it asks.
fn member_at(device: &TestDevice, address: &str) -> EndpointAddr {
    let id = EndpointId::from_bytes(&device.device_pubkey()).unwrap();
    EndpointAddr::from_parts(
        id,
        std::net::ToSocketAddrs::to_socket_addrs(address)
            .unwrap()
            .map(iroh::TransportAddr::Ip),
    )
}

/// A run of `command` in a session, as the relay asks it.
fn run_request(session: &str, command: &str) -> InvokeRequest {
    let argument = serde_json::to_value(RunCommand {
        session: session.into(),
        command: command.into(),
        agent: true,
        account: "acct-1".into(),
        size: "small".into(),
    })
    .unwrap();
    let serde_json::Value::Object(argument) = argument else {
        panic!("a run's argument is an object");
    };
    InvokeRequest {
        target: environment_runs::target(),
        argument: InvokeArgument::Json(argument),
        invocation: InvocationId::new(),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_runs_on_the_vm_host_holding_the_workspace() {
    let operator = TestUser::fresh("operator");
    let desk = operator.enrol("host", DeviceCapabilities::primary());
    let vm_host = operator.enrol(VM_HOST, DeviceCapabilities::member());
    let hosted = host_on(&operator, &desk, HostMode::Device).await;
    let workspace = hosted
        .host
        .workspaces()
        .create("Build")
        .await
        .unwrap()
        .workspace;

    // The VM host is a member device that takes work: it holds the workspace
    // the relay places on it as any member does, entering it the first time
    // the relay asks about it, and answers the relay's runs with the
    // workspace's shells on its own copy of the files.
    let profile = tempfile::tempdir().unwrap();
    let member_endpoint = device_endpoint(&vm_host).await;
    let member_address = address_of(&member_endpoint);
    let member = take_work::Member::open(
        profile.path(),
        (vm_host.sk.clone(), vm_host.cert.clone()),
        member_endpoint.clone(),
        take_work::Relay::new(&hex::encode(desk.device_pubkey()), &host_address(&hosted)).unwrap(),
    )
    .unwrap();

    // The relay knows its VM hosts' members, and asks the placer.
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
        hosted.host.workspaces().plane(),
        hosted.router.endpoint().clone(),
    );

    // Nothing in this test admitted the member to the workspace: the relay's
    // first run is what makes it enter, and that run is answered.
    let ran = {
        let (shells, workspace) = (Arc::clone(&shells), workspace.clone());
        tokio::task::spawn_blocking(move || {
            shells.run(
                &workspace,
                "run:1",
                "mkdir out && echo built > out/artifact && cat out/artifact",
                true,
            )
        })
        .await
        .unwrap()
        .unwrap()
    };
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
        asked.load(Ordering::SeqCst),
        2,
        "asked again once there was room"
    );
    assert_eq!(
        std::fs::read_to_string(member.files(&workspace).join("out/artifact")).unwrap(),
        "built\n",
        "the command worked on the VM host's own copy of the workspace's files"
    );

    // The session's shell lives on there: the next command starts where
    // the last one left off.
    let again = {
        let (shells, workspace) = (Arc::clone(&shells), workspace.clone());
        tokio::task::spawn_blocking(move || shells.run(&workspace, "run:1", "cd out && pwd", true))
            .await
            .unwrap()
            .unwrap()
    };
    let output = String::from_utf8(again.output).unwrap();
    assert!(output.trim_end().ends_with("/out"), "{output}");

    // A device of another person asking the member for the workspace's shell
    // is refused there: it takes the work of its own person's workspaces only.
    let stranger = TestUser::fresh("stranger").enrol("laptop", DeviceCapabilities::member());
    let stranger_endpoint = device_endpoint(&stranger).await;
    let asking = ActionPlane::new(stranger.sk.clone(), Arc::new(NoActions));
    let member_device = asking.remote(
        &stranger_endpoint,
        &member_at(&vm_host, &member_address),
        &workspace,
    );
    let answered = match member_device
        .invoke(
            &Caller::local_app("tilo.relay"),
            run_request("run:2", "echo stranger > stranger.txt"),
        )
        .wait_timeout(Duration::from_secs(30))
    {
        Ok(Some(answered)) => answered,
        _ => panic!("the member's plane answers, and refuses it"),
    };
    let refused = match answered {
        Ok(receipt) => panic!(
            "a device of another person runs nothing here: {:?}",
            receipt.status
        ),
        Err(error) => error,
    };
    assert!(
        matches!(refused, ActionHostError::NotPermitted),
        "the member refuses a device of another person: {refused:?}"
    );
    assert!(
        !member.files(&workspace).join("stranger.txt").exists(),
        "nothing ran on the member for another person"
    );

    // A workspace the relay places here later is entered the same way: the
    // member takes the work of every workspace the relay governs, not only
    // the first it heard of.
    let later = hosted
        .host
        .workspaces()
        .create("Second")
        .await
        .unwrap()
        .workspace;
    let ran = {
        let (shells, later) = (Arc::clone(&shells), later.clone());
        tokio::task::spawn_blocking(move || shells.run(&later, "run:1", "pwd", true))
            .await
            .unwrap()
            .unwrap()
    };
    assert_eq!(ran.exit, 0);
    assert_eq!(
        String::from_utf8(ran.output).unwrap().trim_end(),
        member.files(&later).to_string_lossy(),
        "the second workspace's shell runs in its own files here"
    );

    hosted.stop().await;
}
