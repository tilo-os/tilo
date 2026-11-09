//! A workspace's shell command runs on the VM host that holds the workspace's
//! VM: the host asks the placer which one, waits while none has room, and
//! asks that VM host's member device, a device of the host's own operator
//! admitted to the workspace, to run it over the device plane. The command
//! works on the member's copy of the workspace's files, and its output comes
//! back as the run.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host and its routers stay up until the test ends"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use legacy_vm::hosted::control::{self, Reply, Request};
use legacy_vm::hosted::size::VmSize;
use tilo_clock::unix_now_ns;
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionPlane};
use tilo_peer_state::direct::Replica;
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_relay::host::HostMode;
use tilo_relay::placed_shells::{Member, Members, PlacedShells};
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::workspaces::IssueInvitationRequest;
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use ush::environment_runs::EnvironmentRuns;
use ush::workspace_shell::WorkspaceShells;

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

/// Admit `member` to `workspace` through the join exchange the host answers.
async fn admitted(
    hosted: &Hosted,
    workspace: &str,
    operator: &TestUser,
    member: &TestDevice,
    endpoint: &iroh::Endpoint,
) {
    let invitation = hosted
        .host
        .workspaces()
        .issue(
            workspace,
            hosted.host.device_cert(),
            IssueInvitationRequest {
                request_id: [5; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(operator.user_pk),
                invitee_name: None,
                expires_at_ns: unix_now_ns().checked_add(600_000_000_000).unwrap(),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        Some(operator.user_pk),
        &RevocationList::new(),
        unix_now_ns(),
    )
    .unwrap();
    WorkspaceJoinClient::new(endpoint.clone())
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation,
            member.cert.clone(),
            &member.sk,
            [8; 32],
        )
        .await
        .unwrap();
}

/// The member's replica of `workspace`, holding the host's device and its own.
fn replica(member: &TestDevice, host: &TestDevice, workspace: &str) -> Replica {
    let now = std::time::SystemTime::now();
    let roster = vec![member.cert.clone(), host.cert.clone()];
    let mut peer = StatePeer::new(
        workspace,
        member.cert.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(workspace, &member.cert, &roster).unwrap(),
            documents: Vec::new(),
        },
        &member.sk,
        now,
    )
    .unwrap();
    Replica::new(peer)
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

    // The VM host is a member device of the workspace, holding its files:
    // its plane answers the host's runs with the workspace's shells there.
    let files = tempfile::tempdir().unwrap();
    let member_endpoint = device_endpoint(&vm_host).await;
    admitted(&hosted, &workspace, &operator, &vm_host, &member_endpoint).await;
    let replicas = files.path().join("replicas");
    let runs = EnvironmentRuns::new(
        WorkspaceShells::new(files.path().join("shells"), None)
            .with_files(move |workspace| replicas.join(workspace)),
        operator.user_pk,
    );
    let plane = ActionPlane::new(vm_host.sk.clone(), Arc::new(runs));
    plane.hold(replica(&vm_host, &desk, &workspace));
    let member_router = iroh::protocol::Router::builder(member_endpoint.clone())
        .accept(ACTIONS_ALPN, plane)
        .spawn();
    let member_address = member_endpoint
        .bound_sockets()
        .into_iter()
        .find(std::net::SocketAddr::is_ipv4)
        .map(|socket| format!("127.0.0.1:{}", socket.port()))
        .unwrap();

    // The host knows its VM hosts' members, and asks the placer.
    let (placer, asked) = placer();
    let members = Members::from([(
        VM_HOST.to_owned(),
        Member {
            device: hex::encode(vm_host.device_pubkey()),
            address: member_address,
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
        std::fs::read_to_string(
            files
                .path()
                .join("replicas")
                .join(&workspace)
                .join("out/artifact")
        )
        .unwrap(),
        "built\n",
        "the command worked on the VM host's copy of the workspace's files"
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

    member_router.shutdown().await.unwrap();
    hosted.stop().await;
}
