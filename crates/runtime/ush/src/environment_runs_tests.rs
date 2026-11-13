#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "a test"
)]

use std::time::{Duration, Instant};

use tilo_actions::remote::environment_runs::{self, CommandRan, RunCommand, RunOutput};
use tilo_actions::remote::{ActionHost, ActionHostError, Caller, InvokeArgument, InvokeRequest};
use tilo_actions::{InvocationId, InvocationUpdate};

use super::EnvironmentRuns;
use crate::workspace_shell::WorkspaceShells;

const OPERATOR: [u8; 32] = [7; 32];

fn member(dir: &std::path::Path) -> EnvironmentRuns {
    let files = dir.join("replicas");
    EnvironmentRuns::new(
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
    let asked = RunCommand {
        session: "run:7".into(),
        command: command.into(),
        agent: true,
        account: "acct-1".into(),
        size: "small".into(),
    };
    let serde_json::Value::Object(map) = serde_json::to_value(asked).unwrap() else {
        panic!("a command is an object");
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: environment_runs::target(),
        argument: InvokeArgument::Json(map),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

/// Follow `id` until it ends, keeping each live update's output.
fn followed(
    runs: &EnvironmentRuns,
    caller: &Caller,
    id: InvocationId,
) -> (CommandRan, Vec<RunOutput>) {
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

#[test]
fn the_host_runs_a_command_in_the_members_copy_of_the_workspace() {
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

#[test]
fn a_cancelled_run_ends_its_command_promptly() {
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
            let printed: RunOutput = serde_json::from_value(value).unwrap();
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
        matches!(late, Err(ActionHostError::NotCancellable { .. })),
        "{late:?}"
    );
}

#[test]
fn only_the_hosts_own_devices_may_ask_and_only_for_runs() {
    let dir = tempfile::tempdir().unwrap();
    let runs = member(dir.path());
    let stranger = Caller::authenticated([9; 32], [2; 32]).in_workspace("ws-1");
    let refused = answered(runs.invoke(&stranger, asking("echo mine")));
    assert!(
        matches!(refused, Err(ActionHostError::NotPermitted)),
        "a member who is not the host could bill another account: {refused:?}"
    );
    let mut other = asking("echo hi");
    other.target.address.action = "open".into();
    let refused = answered(runs.invoke(&host(), other));
    assert!(
        matches!(refused, Err(ActionHostError::Unavailable { .. })),
        "{refused:?}"
    );
}

fn browser_member(dir: &std::path::Path, socket: std::path::PathBuf) -> EnvironmentRuns {
    let files = dir.join("replicas");
    EnvironmentRuns::new(
        WorkspaceShells::new(
            dir.join("shells"),
            Some(crate::hosted_vm::HostedTarget {
                socket,
                account: "acct-1".into(),
                size: legacy_vm::hosted::size::VmSize::Small,
                plans: None,
            }),
        )
        .with_files(move |workspace| files.join(workspace)),
        OPERATOR,
    )
}

fn fake_hosted(
    dir: &std::path::Path,
    replies: Vec<legacy_vm::hosted::control::Reply>,
) -> (
    std::path::PathBuf,
    std::sync::Arc<std::sync::Mutex<Vec<legacy_vm::hosted::control::Request>>>,
) {
    use legacy_vm::hosted::control;
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
    }
}

/// Follow an invocation of the member's browser work until it ends, and what
/// it answered.
fn opened(
    runs: &EnvironmentRuns,
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
    use legacy_vm::hosted::control::{OpenBrowser, Reply, Request};
    let dir = tempfile::tempdir().unwrap();
    let (socket, asked) = fake_hosted(
        dir.path(),
        vec![
            Reply::Busy("Starting another machine for it, about 3 minutes.".into()),
            Reply::Opened {
                opened_ms: Some(900),
                provider: "42".into(),
            },
        ],
    );
    let runs = browser_member(dir.path(), socket);
    let request = opening("{\"record\":\"r1\"}");
    let receipt = answered(runs.invoke(&host(), request)).unwrap();
    let opened = opened(&runs, &host(), receipt.invocation).unwrap();
    assert_eq!(opened.opened_ms, Some(900), "the spare was handed over");
    assert_eq!(
        opened.provider, "42",
        "and with it the window the session in it opened"
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
        legacy_vm::hosted::size::VmSize::Small,
        "in the workspace's VM, at the size its shells run at"
    );
    assert!(
        store
            .as_ref()
            .is_some_and(|store| store.ends_with("ws-1/.tilo/browser")),
        "its profile kept among the workspace's own files: {store:?}"
    );
}

#[test]
fn a_member_that_runs_no_vms_opens_no_browser() {
    let dir = tempfile::tempdir().unwrap();
    let runs = member(dir.path());
    let refused = answered(runs.invoke(&host(), opening("{}")));
    assert!(
        matches!(refused, Err(ActionHostError::Unavailable { .. })),
        "a host with no workspace VMs has no Browser: {refused:?}"
    );
}
