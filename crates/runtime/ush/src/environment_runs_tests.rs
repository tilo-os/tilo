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

/// What a pending answer settled to, within a few seconds.
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

    // The session's shell carries its directory to the next command.
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

    // Once the command is under way, the host cancels it, as a person's
    // Ctrl+C in the session or an agent's cancel of its invocation does.
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

    // A run that ended is no longer one a cancel ends.
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
