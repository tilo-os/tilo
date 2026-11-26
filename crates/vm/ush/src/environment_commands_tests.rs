#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "a test"
)]

use std::time::{Duration, Instant};

use tilo_actions::remote::environment_commands::{self, CommandOutput, CommandRan, CommandRequest};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvokeArgument, InvokeRequest,
};
use tilo_actions::{InvocationId, InvocationUpdate};

use super::EnvironmentCommands;
use crate::workspace_shell::WorkspaceShells;

const OPERATOR: [u8; 32] = [7; 32];

fn member(dir: &std::path::Path) -> EnvironmentCommands {
    let files = dir.join("replicas");
    EnvironmentCommands::new(
        WorkspaceShells::new(dir.join("shells"), None)
            .with_files(move |workspace| files.join(workspace)),
        OPERATOR,
    )
}

fn answered<T>(pending: tilo_actions::remote::Pending<T>) -> T {
    match pending.wait_timeout(Duration::from_secs(5)) {
        Ok(Some(value)) => value,
        _ => panic!("no answer"),
    }
}

fn host() -> Caller {
    Caller::authenticated(OPERATOR, [1; 32]).in_workspace("ws-1")
}

fn asking(command: &str) -> InvokeRequest {
    let asked = CommandRequest {
        session: "run:7".into(),
        command: command.into(),
        agent: true,
        account: "acct-1".into(),
        size: "small".into(),
        secrets: Vec::new(),
        person: None,
    };
    let serde_json::Value::Object(map) = serde_json::to_value(asked).unwrap() else {
        panic!("a command is an object");
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: environment_commands::target(),
        argument: InvokeArgument::Json(map),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// Follow `id` until it ends, keeping each live update's output.
fn followed(
    runs: &EnvironmentCommands,
    caller: &Caller,
    id: InvocationId,
) -> (CommandRan, Vec<CommandOutput>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut live = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "the run never ended");
        if let Some(change) = runs.next_change(caller, &id) {
            answered(change);
        }
        match runs.observe(caller, &id).unwrap().update {
            InvocationUpdate::Item { value, .. } => {
                live.push(serde_json::from_value(value).unwrap());
            }
            InvocationUpdate::Succeeded { output, .. } => {
                return (serde_json::from_value(output.value.unwrap()).unwrap(), live);
            }
            InvocationUpdate::Accepted { .. } | InvocationUpdate::Progress { .. } => {}
            other => panic!("the run did not succeed: {other:?}"),
        }
    }
}

tilo_test_env::scenario! {
    /// What a member's device does when the host asks: runs for the host's own devices, and nothing it cannot do.
    fn a_member_answers_what_the_host_may_ask_of_it;

    phase the_host_runs_a_command_in_the_members_copy_of_the_workspace;
    {
        let dir = tempfile::tempdir().unwrap();
        let runs = member(dir.path());
        let request = asking("mkdir src && echo fn main > src/main.rs && cd src && ls");
        let id = request.invocation;
        answered(runs.invoke(&host(), request.clone())).unwrap();
        let (ran, _) = followed(&runs, &host(), id);
        assert_eq!(ran.exit, 0, "{}", ran.output);
        assert!(ran.output.ends_with("main.rs\n"), "{}", ran.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("replicas/ws-1/src/main.rs")).unwrap(),
            "fn main\n",
            "the command worked on the member's copy of the workspace's files"
        );

        // The same invocation again is the same run, not a second one.
        answered(runs.invoke(&host(), request)).unwrap();

        let next = asking("pwd");
        let id = next.invocation;
        answered(runs.invoke(&host(), next)).unwrap();
        let (ran, _) = followed(&runs, &host(), id);
        assert!(
            ran.output.trim_end().ends_with("ws-1/src"),
            "{}",
            ran.output
        );
    }

    phase a_cancelled_run_ends_its_command_promptly;
    {
        let dir = tempfile::tempdir().unwrap();
        let runs = member(dir.path());
        let request = asking("echo started && sleep 120 && echo never");
        let id = request.invocation;
        answered(runs.invoke(&host(), request)).unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(state) = runs.observe(&host(), &id)
                && let InvocationUpdate::Item { value, .. } = state.update
            {
                let printed: CommandOutput = serde_json::from_value(value).unwrap();
                if printed.tail.contains("started") {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "the command never printed");
            std::thread::sleep(Duration::from_millis(20));
        }
        let asked_at = Instant::now();
        answered(runs.cancel(&host(), &id)).unwrap();
        let (ran, _) = followed(&runs, &host(), id);
        assert!(
            asked_at.elapsed() < Duration::from_secs(20),
            "the command ended promptly: {:?}",
            asked_at.elapsed()
        );
        assert!(ran.cancelled, "the run says it was cancelled: {ran:?}");
        assert_eq!(ran.exit, 130, "{}", ran.output);
        assert!(ran.output.starts_with("started\n"), "{}", ran.output);
        assert!(!ran.output.contains("never"), "{}", ran.output);

        let late = answered(runs.cancel(&host(), &id));
        assert!(
            matches!(late, Err(ActionCallError::NotCancellable { .. })),
            "{late:?}"
        );
    }

    phase only_the_hosts_own_devices_may_ask_and_only_for_runs;
    {
        let dir = tempfile::tempdir().unwrap();
        let runs = member(dir.path());
        let stranger = Caller::authenticated([9; 32], [2; 32]).in_workspace("ws-1");
        let refused = answered(runs.invoke(&stranger, asking("echo mine")));
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "a member who is not the host could bill another account: {refused:?}"
        );
        let mut other = asking("echo hi");
        other.target.address.action = "open".into();
        let refused = answered(runs.invoke(&host(), other));
        assert!(
            matches!(refused, Err(ActionCallError::Unavailable { .. })),
            "{refused:?}"
        );
    }

    phase a_member_that_runs_no_vms_opens_no_browser;
    {
        let dir = tempfile::tempdir().unwrap();
        let runs = member(dir.path());
        let refused = answered(runs.invoke(&host(), opening("{}")));
        assert!(
            matches!(refused, Err(ActionCallError::Unavailable { .. })),
            "a host with no workspace VMs has no Browser: {refused:?}"
        );
    }
}

#[test]
fn what_a_command_prints_is_shown_while_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let runs = member(dir.path());
    let request = asking("echo first && sleep 1 && echo second");
    let id = request.invocation;
    answered(runs.invoke(&host(), request)).unwrap();
    let (ran, live) = followed(&runs, &host(), id);
    assert_eq!(ran.output, "first\nsecond\n");
    assert!(
        live.iter()
            .any(|seen| seen.tail == "first\n" && seen.total == 6),
        "the first line was shown before the command ended: {live:?}"
    );
}

fn browser_member(dir: &std::path::Path, socket: std::path::PathBuf) -> EnvironmentCommands {
    let files = dir.join("replicas");
    EnvironmentCommands::new(
        WorkspaceShells::new(
            dir.join("shells"),
            Some(crate::hosted_vm::HostedTarget {
                socket,
                account: "acct-1".into(),
                size: tilo_server_protocol::hosted_vm::VmSize::Small,
                plans: None,
            }),
        )
        .with_files(move |workspace| files.join(workspace)),
        OPERATOR,
    )
}

fn fake_hosted(
    dir: &std::path::Path,
    replies: Vec<linux_vm::hosted::control::Reply>,
) -> (
    std::path::PathBuf,
    std::sync::Arc<std::sync::Mutex<Vec<linux_vm::hosted::control::Request>>>,
) {
    use linux_vm::hosted::control;
    let socket = dir.join("hosted.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&asked);
    std::thread::spawn(move || {
        let mut replies = replies.into_iter();
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Ok(Some(request)) = control::FRAMING.read_value(&mut stream) else {
                continue;
            };
            seen.lock().unwrap().push(request);
            if let Some(reply) = replies.next() {
                let _ = control::FRAMING.write_value(&mut stream, &reply);
            }
        }
    });
    (socket, asked)
}

fn opening(material: &str) -> InvokeRequest {
    use tilo_actions::remote::browser;
    let asked = browser::OpenBrowser {
        workspace: "ws-1".into(),
        account: "acct-1".into(),
        material: material.into(),
    };
    let serde_json::Value::Object(map) = serde_json::to_value(asked).unwrap() else {
        panic!("an open is an object");
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: browser::target(),
        argument: InvokeArgument::Json(map),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// Follow an invocation of the member's browser work until it ends, and what
/// it answered.
fn opened(
    runs: &EnvironmentCommands,
    caller: &Caller,
    id: InvocationId,
) -> Option<tilo_actions::remote::browser::OpenedBrowser> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "it never ended");
        if let Some(change) = runs.next_change(caller, &id) {
            answered(change);
        }
        match runs.observe(caller, &id).unwrap().update {
            InvocationUpdate::Succeeded { output, .. } => {
                return output
                    .value
                    .map(|value| serde_json::from_value(value).unwrap());
            }
            InvocationUpdate::Failed { error, .. } => panic!("it failed: {}", error.message),
            _ => {}
        }
    }
}

#[test]
fn opening_browser_asks_the_hosts_service_for_the_workspaces_vm() {
    use linux_vm::hosted::control::{OpenBrowser, Reply, Request};
    let dir = tempfile::tempdir().unwrap();
    let (socket, asked) = fake_hosted(
        dir.path(),
        vec![
            Reply::Busy("The machine is full until one of its VMs goes idle.".into()),
            Reply::Opened {
                opened_ms: Some(900),
                device: "ab".repeat(32),
            },
        ],
    );
    let runs = browser_member(dir.path(), socket);
    let request = opening("{\"record\":\"r1\"}");
    let receipt = answered(runs.invoke(&host(), request)).unwrap();
    let opened = opened(&runs, &host(), receipt.invocation).unwrap();
    assert_eq!(opened.opened_ms, Some(900), "the spare was handed over");
    assert_eq!(
        opened.device,
        "ab".repeat(32),
        "and with it the device its daemon logged in as"
    );
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "asked again once there was room");
    let Request::OpenBrowser {
        browser:
            OpenBrowser {
                workspace,
                account,
                size,
                store,
                material,
            },
    } = &asked[0]
    else {
        panic!("an open: {:?}", asked[0]);
    };
    assert_eq!(
        (workspace.as_str(), account.as_str(), material.as_str()),
        ("ws-1", "acct-1", "{\"record\":\"r1\"}"),
        "the caller's workspace, and the material word for word"
    );
    assert_eq!(
        *size,
        tilo_server_protocol::hosted_vm::VmSize::Small,
        "in the workspace's VM, at the size its shells run at"
    );
    assert!(
        store
            .as_ref()
            .is_some_and(|store| store.ends_with("ws-1/.tilo/browser")),
        "its profile kept among the workspace's own files: {store:?}"
    );
}

fn machine_request(
    target: tilo_actions::remote::RemoteActionTarget,
    argument: &impl serde::Serialize,
) -> InvokeRequest {
    let serde_json::Value::Object(map) = serde_json::to_value(argument).unwrap() else {
        panic!("a machine's request is an object");
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument: InvokeArgument::Json(map),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

fn branching(from: &str) -> InvokeRequest {
    use tilo_actions::remote::environment_machines::{self, BranchMachine};
    machine_request(
        environment_machines::branch_target(),
        &BranchMachine {
            from: from.into(),
            account: "acct-1".into(),
            size: "small".into(),
        },
    )
}

fn deleting() -> InvokeRequest {
    use tilo_actions::remote::environment_machines::{self, DeleteMachine};
    machine_request(environment_machines::delete_target(), &DeleteMachine {})
}

/// Follow an invocation until it ends: what it answered, or the words it failed with.
fn ended(
    runs: &EnvironmentCommands,
    caller: &Caller,
    id: InvocationId,
) -> Result<Option<serde_json::Value>, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() >= deadline {
            return Err("it never ended".into());
        }
        if let Some(change) = runs.next_change(caller, &id) {
            answered(change);
        }
        let Some(state) = runs.observe(caller, &id) else {
            return Err("the member forgot it".into());
        };
        match state.update {
            InvocationUpdate::Succeeded { output, .. } => return Ok(output.value),
            InvocationUpdate::Failed { error, .. } => return Err(error.message),
            _ => {}
        }
    }
}

#[test]
fn only_the_hosts_own_devices_branch_and_delete_a_workspaces_machine() {
    use linux_vm::hosted::control::{Reply, Request, VmRequest};
    use tilo_actions::remote::environment_machines::MachineBranched;
    let dir = tempfile::tempdir().unwrap();
    let (socket, asked) = fake_hosted(
        dir.path(),
        vec![
            Reply::Done,
            Reply::NoVm {
                workspace: "ws-new".into(),
            },
            Reply::Error("the VM of workspace ws-0 belongs to another account".into()),
            Reply::Done,
        ],
    );
    let runs = browser_member(dir.path(), socket);

    let stranger = Caller::authenticated([9; 32], [2; 32]).in_workspace("ws-1");
    for request in [branching("ws-0"), deleting()] {
        let refused = answered(runs.invoke(&stranger, request));
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "a device not the host's could copy or delete a workspace's machine: {refused:?}"
        );
    }
    assert!(
        asked.lock().unwrap().is_empty(),
        "nothing a stranger asked reached the hosted-VM service"
    );

    let branched = |from: &str| {
        let receipt = answered(runs.invoke(&host(), branching(from))).unwrap();
        ended(&runs, &host(), receipt.invocation)
    };
    let made: MachineBranched = serde_json::from_value(branched("ws-0").unwrap().unwrap()).unwrap();
    assert!(made.branched, "the branch's disk is there");
    let none: MachineBranched =
        serde_json::from_value(branched("ws-new").unwrap().unwrap()).unwrap();
    assert!(
        !none.branched,
        "a source without a machine is answered as one, not as a failure"
    );
    let failed = branched("ws-0").unwrap_err();
    assert!(
        failed.contains("belongs to another account"),
        "the service's refusal is the branch's failure, word for word: {failed}"
    );

    let receipt = answered(runs.invoke(&host(), deleting())).unwrap();
    assert_eq!(ended(&runs, &host(), receipt.invocation), Ok(None));

    let branch = Request::Branch {
        vm: VmRequest {
            workspace: "ws-1".into(),
            environment: "default".into(),
            account: "acct-1".into(),
            size: tilo_server_protocol::hosted_vm::VmSize::Small,
        },
        from: "ws-0".into(),
    };
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 4, "{asked:?}");
    assert_eq!(
        asked[0], branch,
        "the machine made is the caller's workspace's, for the account the host named"
    );
    assert_eq!(
        asked[3],
        Request::Delete {
            workspace: "ws-1".into(),
            environment: "default".into(),
        },
        "the machine deleted is the caller's workspace's"
    );
}

#[test]
fn a_member_that_runs_no_vms_has_no_machine_to_branch_or_delete() {
    let dir = tempfile::tempdir().unwrap();
    let runs = member(dir.path());
    for request in [branching("ws-0"), deleting()] {
        let refused = answered(runs.invoke(&host(), request));
        assert!(
            matches!(refused, Err(ActionCallError::Unavailable { .. })),
            "{refused:?}"
        );
    }
}

#[test]
fn a_member_whose_environment_names_no_account_runs_programs_for_the_account_asked() {
    use linux_vm::hosted::control::{Reply, Request};
    let dir = tempfile::tempdir().unwrap();
    let (socket, asked) = fake_hosted(
        dir.path(),
        vec![Reply::Error("this test runs no VM".into())],
    );
    let files = dir.path().join("replicas");
    // As a VM host's member is started: its environment names the hosted-VM
    // service and no account, since the server names one with each run.
    let runs = EnvironmentCommands::new(
        WorkspaceShells::new(dir.path().join("shells"), None)
            .with_files(move |workspace| files.join(workspace))
            .with_browsers_on(Some(socket)),
        OPERATOR,
    );
    let request = asking("uname -srm");
    let id = request.invocation;
    answered(runs.invoke(&host(), request)).unwrap();
    let (ran, _) = followed(&runs, &host(), id);
    assert_ne!(ran.exit, 0, "the service ran nothing: {}", ran.output);
    let asked = asked.lock().unwrap().clone();
    let Some(Request::Ensure { vm, .. }) = asked.first() else {
        panic!(
            "the program was never taken to the workspace's VM ({asked:?}): {}",
            ran.output
        );
    };
    assert_eq!(
        (vm.workspace.as_str(), vm.account.as_str(), vm.size),
        (
            "ws-1",
            "acct-1",
            tilo_server_protocol::hosted_vm::VmSize::Small
        ),
        "the caller's workspace's VM, paid for by the account the run named, at its size"
    );
}

#[test]
fn an_account_without_a_card_is_refused_before_anything_runs_in_its_vm() {
    use linux_vm::hosted::control::{Reply, Request};
    use tilo_actions::remote::server_account::{Reason, Refusal};
    let dir = tempfile::tempdir().unwrap();
    // The hosted-VM service reads its plans file for the account the request
    // names, and finds no card for it.
    let (socket, asked) = fake_hosted(
        dir.path(),
        vec![Reply::Refused(Refusal {
            reason: Reason::NoPaymentMethod,
            message: "Running this needs a Linux machine, and acct-1 has no card on file.".into(),
            next: Vec::new(),
        })],
    );
    let files = dir.path().join("replicas");
    let runs = EnvironmentCommands::new(
        WorkspaceShells::new(dir.path().join("shells"), None)
            .with_files(move |workspace| files.join(workspace))
            .with_browsers_on(Some(socket)),
        OPERATOR,
    );
    let request = asking("uname -srm");
    let id = request.invocation;
    answered(runs.invoke(&host(), request)).unwrap();
    let (ran, _) = followed(&runs, &host(), id);
    assert_eq!(
        ran.refusal.as_ref().map(|refusal| refusal.reason.clone()),
        Some(Reason::NoPaymentMethod),
        "the agent's run is refused, typed, at once: {ran:?}"
    );
    assert_eq!(ran.exit, 126, "{}", ran.output);
    assert!(ran.output.contains("no card on file"), "{}", ran.output);
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "asked once, and nothing more: {asked:?}");
    let Request::Ensure { vm, .. } = &asked[0] else {
        panic!("the VM was asked for: {asked:?}");
    };
    assert_eq!(
        vm.account, "acct-1",
        "the plans are read for the account the run named"
    );
}
